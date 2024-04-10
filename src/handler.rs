use crate::protocol::ProtocolConfig;
use bytes::Bytes;
use futures::{Sink, StreamExt};
use libp2p::core::upgrade::DeniedUpgrade;
use libp2p::swarm::derive_prelude::Either;
use libp2p::swarm::handler::ConnectionEvent;
use libp2p::swarm::{
    ConnectionDenied, ConnectionHandler, ConnectionHandlerEvent, ConnectionId, SubstreamProtocol,
};
use libp2p::PeerId;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tokio_util::compat::Compat;
use yrs::encoding::read::Error;
use yrs::sync::{Awareness, Message, Protocol};
use yrs::updates::decoder::Decode;
use yrs::Subscription;

pub struct Handler<P> {
    connection_id: ConnectionId,
    peer_id: PeerId,
    protocol: ProtocolConfig<P>,
    inbound: Option<InboundState>,
    outbound: Option<OutboundState>,
    send_queue: VecDeque<Bytes>,
    establishing_outbound: bool,
}

impl<P> Handler<P>
where
    P: Protocol,
{
    pub fn new(
        connection_id: ConnectionId,
        peer_id: PeerId,
        protocol: ProtocolConfig<P>,
    ) -> Result<Self, ConnectionDenied> {
        Ok(Handler {
            connection_id,
            peer_id,
            protocol,
            send_queue: VecDeque::new(),
            inbound: None,
            outbound: None,
            establishing_outbound: false,
        })
    }

    fn on_negotiated_inbound(&mut self, stream: Stream) {
        self.inbound = Some(InboundState::WaitingInput(stream));
    }

    fn on_negotiated_outbound(&mut self, stream: Stream) {
        self.outbound = Some(OutboundState::WaitingOutput(stream));
    }
}

impl<P> ConnectionHandler for Handler<P>
where
    P: Protocol + Debug + Clone + Send + 'static,
{
    type FromBehaviour = HandlerIn;
    type ToBehaviour = HandlerEvent;
    type InboundProtocol = ProtocolConfig<P>;
    type OutboundProtocol = ProtocolConfig<P>;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(self.protocol.clone(), ())
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // determine if we need to create the outbound stream
        if !self.send_queue.is_empty() && self.outbound.is_none() && !self.establishing_outbound {
            self.establishing_outbound = true;
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(self.protocol.clone(), ()),
            });
        }

        // process outbound stream
        loop {
            match std::mem::replace(&mut self.outbound, Some(OutboundState::Poisoned)) {
                // outbound idle state
                Some(OutboundState::WaitingOutput(substream)) => {
                    if let Some(message) = self.send_queue.pop_front() {
                        self.outbound = Some(OutboundState::PendingSend(substream, message));
                        continue;
                    }

                    self.outbound = Some(OutboundState::WaitingOutput(substream));
                    break;
                }
                Some(OutboundState::PendingSend(mut substream, message)) => {
                    match Sink::poll_ready(Pin::new(&mut substream), cx) {
                        Poll::Ready(Ok(())) => {
                            match Sink::start_send(Pin::new(&mut substream), message) {
                                Ok(()) => {
                                    self.outbound = Some(OutboundState::PendingFlush(substream))
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "Failed to send message on outbound stream: {e}"
                                    );
                                    self.outbound = None;
                                    break;
                                }
                            }
                        }
                        Poll::Ready(Err(e)) => {
                            tracing::debug!("Failed to send message on outbound stream: {e}");
                            self.outbound = None;
                            break;
                        }
                        Poll::Pending => {
                            self.outbound = Some(OutboundState::PendingSend(substream, message));
                            break;
                        }
                    }
                }
                Some(OutboundState::PendingFlush(mut substream)) => {
                    match Sink::poll_flush(Pin::new(&mut substream), cx) {
                        Poll::Ready(Ok(())) => {
                            //self.last_io_activity = Instant::now();
                            self.outbound = Some(OutboundState::WaitingOutput(substream))
                        }
                        Poll::Ready(Err(e)) => {
                            tracing::debug!("Failed to flush outbound stream: {e}");
                            self.outbound = None;
                            break;
                        }
                        Poll::Pending => {
                            self.outbound = Some(OutboundState::PendingFlush(substream));
                            break;
                        }
                    }
                }
                None => {
                    self.outbound = None;
                    break;
                }
                Some(OutboundState::Poisoned) => {
                    unreachable!("Error occurred during outbound stream processing")
                }
            }
        }

        // process inbound stream
        loop {
            match std::mem::replace(&mut self.inbound, Some(InboundState::Poisoned)) {
                // inbound idle state
                Some(InboundState::WaitingInput(mut substream)) => {
                    match substream.poll_next_unpin(cx) {
                        Poll::Ready(Some(Ok(message))) => {
                            //self.last_io_activity = Instant::now();
                            self.inbound = Some(InboundState::WaitingInput(substream));
                            match Message::decode_v1(&*message) {
                                Ok(message) => {
                                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                                        HandlerEvent::Received(message),
                                    ))
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to parse inbound stream message: {e}");
                                    break;
                                }
                            }
                        }
                        Poll::Ready(Some(Err(error))) => {
                            tracing::debug!("Failed to read from inbound stream: {error}");
                            // Close this side of the stream. If the
                            // peer is still around, they will re-establish their
                            // outbound stream i.e. our inbound stream.
                            self.inbound = Some(InboundState::Closing(substream));
                        }
                        // peer closed the stream
                        Poll::Ready(None) => {
                            tracing::debug!("Inbound stream closed by remote");
                            self.inbound = Some(InboundState::Closing(substream));
                        }
                        Poll::Pending => {
                            self.inbound = Some(InboundState::WaitingInput(substream));
                            break;
                        }
                    }
                }
                Some(InboundState::Closing(mut substream)) => {
                    match Sink::poll_close(Pin::new(&mut substream), cx) {
                        Poll::Ready(res) => {
                            if let Err(e) = res {
                                // Don't close the connection but just drop the inbound substream.
                                // In case the remote has more to send, they will open up a new
                                // substream.
                                tracing::debug!("Inbound stream error while closing: {e}");
                            }
                            self.inbound = None;
                            break;
                        }
                        Poll::Pending => {
                            self.inbound = Some(InboundState::Closing(substream));
                            break;
                        }
                    }
                }
                None => {
                    self.inbound = None;
                    break;
                }
                Some(InboundState::Poisoned) => {
                    unreachable!("Error occurred during inbound stream processing")
                }
            }
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        tracing::trace!("behaviour event: {event:?}");
        match event {
            HandlerIn::Send(message) => {
                self.send_queue.push_back(message);
            }
        }
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        tracing::trace!("connection event: {event:?}");
        if event.is_outbound() {
            self.establishing_outbound = false;
        }
        match event {
            ConnectionEvent::FullyNegotiatedInbound(inbound) => {
                self.on_negotiated_inbound(inbound.protocol)
            }
            ConnectionEvent::FullyNegotiatedOutbound(outbound) => {
                self.on_negotiated_outbound(outbound.protocol)
            }
            ConnectionEvent::AddressChange(_) => {}
            ConnectionEvent::DialUpgradeError(_) => {}
            ConnectionEvent::ListenUpgradeError(_) => {}
            ConnectionEvent::LocalProtocolsChange(_) => {}
            ConnectionEvent::RemoteProtocolsChange(_) => {}
            _ => {}
        }
    }
}

#[derive(Debug)]
pub enum HandlerIn {
    Send(Bytes),
}

#[derive(Debug)]
pub enum HandlerEvent {
    Received(Message),
}

/// State of the inbound stream.
enum InboundState {
    /// Waiting for a message from the remote. The idle state for an inbound substream.
    WaitingInput(Stream),
    /// The substream is being closed.
    Closing(Stream),
    /// An error occurred during processing.
    Poisoned,
}

/// State of the outbound stream.
enum OutboundState {
    /// Waiting for the user to send a message. The idle state for an outbound substream.
    WaitingOutput(Stream),
    /// Waiting to send a message to the remote.
    PendingSend(Stream, Bytes),
    /// Waiting to flush the substream so that the data arrives to the remote.
    PendingFlush(Stream),
    /// An error occurred during processing.
    Poisoned,
}

type Stream = Framed<Compat<libp2p::Stream>, LengthDelimitedCodec>;
