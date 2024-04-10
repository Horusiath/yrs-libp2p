use crate::handler::{Handler, HandlerEvent, HandlerIn};
use crate::protocol::ProtocolConfig;
use bytes::Bytes;
use libp2p::core::Endpoint;
use libp2p::swarm::behaviour::ConnectionEstablished;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{
    CloseConnection, ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour,
    NotifyHandler, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt::Debug;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use yrs::sync::{Awareness, AwarenessUpdate, DefaultProtocol, Message, Protocol, SyncMessage};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{ReadTxn, Subscription, Transact, Update};

pub struct Behaviour<P> {
    awareness: Awareness,
    config: ProtocolConfig<P>,
    events_rx: UnboundedReceiver<ToSwarm<Event, HandlerIn>>,
    events_tx: UnboundedSender<ToSwarm<Event, HandlerIn>>,
    peers: HashSet<PeerId>,
    on_awareness_update: Subscription,
    on_doc_update: Subscription,
}

impl<P> Behaviour<P>
where
    P: Protocol + Clone,
{
    pub fn new(awareness: Awareness, protocol: P) -> Self {
        let (events_tx, events_rx) = unbounded_channel();
        let sender = events_tx.clone();
        let on_awareness_update = awareness.on_update(move |e| {
            let _ = sender.send(ToSwarm::GenerateEvent(Event::AwarenessUpdate(e.clone())));
        });
        let sender = events_tx.clone();
        let on_doc_update = awareness
            .doc()
            .observe_update_v1(move |txn, e| {
                let _ = sender.send(ToSwarm::GenerateEvent(Event::DocUpdate(e.update.clone())));
            })
            .unwrap();
        Behaviour {
            events_rx,
            events_tx,
            awareness,
            on_doc_update,
            on_awareness_update,
            config: ProtocolConfig::new(protocol),
            peers: Default::default(),
        }
    }

    #[inline]
    pub fn awareness(&self) -> &Awareness {
        &self.awareness
    }

    #[inline]
    pub fn awareness_mut(&mut self) -> &mut Awareness {
        &mut self.awareness
    }

    pub fn add_peer(&mut self, peer_id: PeerId) {
        if self.peers.insert(peer_id) {
            let state_vector = self.awareness.doc().transact().state_vector();
            self.send_message(peer_id, Message::Sync(SyncMessage::SyncStep1(state_vector)));
        }
    }

    pub fn remove_peer(&mut self, peer_id: PeerId) {
        if self.peers.remove(&peer_id) {
            todo!()
        }
    }

    fn on_connection_closed(&mut self, peer_id: PeerId, remaining_connections: usize) {
        if remaining_connections > 0 {
            return; // only disconnect when all are disconnected
        }
        self.remove_peer(peer_id);
    }

    fn on_connection_established(&mut self, peer_id: PeerId, other_connections: usize) {
        if other_connections > 0 {
            return; // only connect to first one
        }
        self.add_peer(peer_id);
    }

    fn handle(&mut self, message: Message, sender: PeerId) {
        match handle_message(&self.config.protocol, &mut self.awareness, message) {
            Ok(None) => { /* do nothing */ }
            Ok(Some(reply)) => self.send_message(sender, reply),
            Err(error) => {
                tracing::error!("error while handling incoming message from {sender}: {error}")
            }
        }
    }

    fn send_message(&self, peer_id: PeerId, message: Message) {
        let data = Bytes::from(message.encode_v1());
        let _ = self.events_tx.send(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::Any,
            event: HandlerIn::Send(data),
        });
    }
}

impl Default for Behaviour<DefaultProtocol> {
    fn default() -> Self {
        Self::new(Awareness::default(), DefaultProtocol)
    }
}

impl<P> NetworkBehaviour for Behaviour<P>
where
    P: Protocol + Debug + Clone + Send + 'static,
{
    type ConnectionHandler = Handler<P>;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        tracing::trace!("established inbound connection {connection_id} to peer {peer}");
        Handler::new(connection_id, peer, self.config.clone())
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        tracing::trace!("established outbound connection {connection_id} to peer {peer}");
        Handler::new(connection_id, peer, self.config.clone())
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        tracing::trace!("swarm event: {event:?}");
        match event {
            FromSwarm::ConnectionEstablished(e) => {
                self.on_connection_established(e.peer_id, e.other_established)
            }
            FromSwarm::ConnectionClosed(e) => {
                self.on_connection_closed(e.peer_id, e.remaining_established)
            }
            FromSwarm::AddressChange(change) => {}
            FromSwarm::DialFailure(fail) => {}
            FromSwarm::ListenFailure(fail) => {}
            FromSwarm::NewListener(listener) => {}
            FromSwarm::NewListenAddr(addr) => {}
            FromSwarm::ExpiredListenAddr(addr) => {}
            FromSwarm::ListenerError(err) => {}
            FromSwarm::ListenerClosed(closed) => {}
            FromSwarm::NewExternalAddrCandidate(candidate) => {}
            FromSwarm::ExternalAddrConfirmed(confirmed) => {}
            FromSwarm::ExternalAddrExpired(expired) => {}
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        tracing::trace!("connection handler event: {event:?}");
        match event {
            HandlerEvent::Received(message) => self.handle(message, peer_id),
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        loop {
            match self.events_rx.poll_recv(cx) {
                Poll::Ready(Some(msg)) => match msg {
                    ToSwarm::GenerateEvent(Event::DocUpdate(update)) => {
                        let msg: Bytes = Message::Sync(SyncMessage::Update(update))
                            .encode_v1()
                            .into();
                        for &peer_id in self.peers.iter() {
                            let _ = self.events_tx.send(ToSwarm::NotifyHandler {
                                peer_id,
                                handler: NotifyHandler::Any,
                                event: HandlerIn::Send(msg.clone()),
                            });
                        }
                    }
                    ToSwarm::GenerateEvent(Event::AwarenessUpdate(e)) => {
                        let mut clients = Vec::with_capacity(
                            e.added().len() + e.updated().len() + e.removed().len(),
                        );
                        clients.extend_from_slice(e.added());
                        clients.extend_from_slice(e.updated());
                        clients.extend_from_slice(e.removed());
                        match self.awareness.update_with_clients(clients) {
                            Ok(update) => {
                                let msg: Bytes = Message::Awareness(update).encode_v1().into();
                                for &peer_id in self.peers.iter() {
                                    let _ = self.events_tx.send(ToSwarm::NotifyHandler {
                                        peer_id,
                                        handler: NotifyHandler::Any,
                                        event: HandlerIn::Send(msg.clone()),
                                    });
                                }
                            }
                            Err(e) => {
                                tracing::error!("failed to broadcast awareness update: {e}");
                            }
                        }
                    }
                    other => return Poll::Ready(other),
                },
                Poll::Ready(None) => return Poll::Pending,
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Message { sender: PeerId, message: Message },
    DocUpdate(Vec<u8>),
    AwarenessUpdate(yrs::sync::awareness::Event),
}

fn handle_message<P: Protocol>(
    protocol: &P,
    awareness: &mut Awareness,
    msg: Message,
) -> Result<Option<Message>, Box<dyn std::error::Error>> {
    match msg {
        Message::Sync(sync_msg) => match sync_msg {
            SyncMessage::SyncStep1(state_vector) => {
                protocol.handle_sync_step1(awareness, state_vector)
            }
            SyncMessage::SyncStep2(update) => {
                protocol.handle_sync_step2(awareness, Update::decode_v1(&*update)?)
            }
            SyncMessage::Update(update) => {
                protocol.handle_sync_step2(awareness, Update::decode_v1(&*update)?)
            }
        },
        Message::Auth(deny_reason) => protocol.handle_auth(awareness, deny_reason),
        Message::AwarenessQuery => protocol.handle_awareness_query(awareness),
        Message::Awareness(update) => protocol.handle_awareness_update(awareness, update),
        Message::Custom(tag, data) => protocol.missing_handle(awareness, tag, data),
    }
    .map_err(|e| e.into())
}
