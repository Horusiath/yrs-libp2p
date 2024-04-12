use bytes::{Bytes, BytesMut};
use futures::{AsyncRead, AsyncWrite, SinkExt, StreamExt};
use libp2p::core::{Endpoint, UpgradeInfo};
use libp2p::swarm::behaviour::ConnectionEstablished;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{
    CloseConnection, ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour,
    NotifyHandler, OneShotHandler, SubstreamProtocol, THandler, THandlerInEvent, THandlerOutEvent,
    ToSwarm,
};
use libp2p::{InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, StreamProtocol};
use std::collections::hash_map::{Entry, HashMap};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::{io, iter};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_util::codec::{Decoder, Framed, LengthDelimitedCodec};
use yrs::block::ClientID;
use yrs::encoding::read::{Cursor, Read};
use yrs::encoding::write::Write;
use yrs::sync::{Awareness, Message, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{Doc, Options, ReadTxn, Subscription, Transact, Update};

pub type Topic = std::sync::Arc<str>;

/// Network behaviour capable of supporting [ysync protocol](https://github.com/yjs/y-protocols/blob/master/PROTOCOL.md) - used by Yrs/Yjs - with multiplexing multiple documents over a single peer.
/// Documents are uniquely identified by [yrs::Doc::guid], which becomes a topic for pub/sub mechanism
/// used by this behaviour.
pub struct Behaviour {
    events: VecDeque<ToSwarm<Event, DocMessage>>,
    update_receiver: UnboundedReceiver<InternalEvent>,
    update_sender: UnboundedSender<InternalEvent>,
    config: Config,
    target_peers: HashSet<PeerId>,
    connected_peers: HashSet<PeerId>,
    subscribed_topics: HashMap<Topic, TopicHandler>,
}

impl Behaviour {
    /// Creates a `` with default configuration.
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }

    /// Creates a `` with the given configuration.
    pub fn with_config(config: Config) -> Self {
        let (events_sender, events_receiver) = unbounded_channel();
        Self {
            events: VecDeque::new(),
            update_receiver: events_receiver,
            update_sender: events_sender,
            config,
            target_peers: HashSet::new(),
            connected_peers: HashSet::new(),
            subscribed_topics: HashMap::new(),
        }
    }

    fn handler(&mut self, topic: Topic) -> &mut TopicHandler {
        match self.subscribed_topics.entry(topic.clone()) {
            Entry::Vacant(e) => {
                let mut options = self.config.doc_options.clone();
                options.guid = topic.clone();
                let handler = e.insert(TopicHandler::new(options, self.update_sender.clone()));

                let sv = handler.awareness.doc().transact().state_vector();
                // this awareness might not yet exist locally, but other peers may already have some state in it
                for &peer_id in self.connected_peers.iter() {
                    tracing::trace!("new handler for `{topic}` init msg to `{peer_id}`: {sv:?}");
                    self.events.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: NotifyHandler::Any,
                        event: DocMessage {
                            source: None,
                            doc_name: topic.clone(),
                            message: Message::Sync(SyncMessage::SyncStep1(sv.clone())),
                        },
                    });
                }

                handler
            }
            Entry::Occupied(e) => e.into_mut(),
        }
    }

    /// Return or create a [yrs::sync::Awareness] instance for a document identifier by a given
    /// topic.
    pub fn awareness(&mut self, topic: Topic) -> &mut Awareness {
        &mut self.handler(topic).awareness
    }

    /// Add a node to the list of nodes to propagate messages to.
    #[inline]
    pub fn add_peer(&mut self, peer_id: PeerId) {
        if self.connected_peers.contains(&peer_id) {
            self.welcome(peer_id);
        }

        if self.target_peers.insert(peer_id) {
            self.events.push_back(ToSwarm::Dial {
                opts: DialOpts::peer_id(peer_id).build(),
            });
        }
    }

    fn welcome(&mut self, peer_id: PeerId) {
        for (topic, handle) in self.subscribed_topics.iter() {
            let sv = handle.awareness.doc().transact().state_vector();
            tracing::trace!("new handler for `{topic}` init msg to `{peer_id}`: {sv:?}");
            self.events.push_back(ToSwarm::NotifyHandler {
                peer_id,
                handler: NotifyHandler::Any,
                event: DocMessage {
                    source: None,
                    doc_name: topic.clone(),
                    message: Message::Sync(SyncMessage::SyncStep1(sv)),
                },
            });
        }
    }

    /// Remove a node from the list of nodes to propagate messages to.
    #[inline]
    pub fn remove_peer(&mut self, peer_id: &PeerId) {
        self.target_peers.remove(peer_id);
    }

    fn broadcast(&mut self, topic: Topic, msg: Message) {
        for &peer_id in self.connected_peers.iter() {
            tracing::trace!("`{topic}` broadcasting to `{peer_id}`: {msg:?}");
            self.events.push_back(ToSwarm::NotifyHandler {
                peer_id,
                handler: NotifyHandler::Any,
                event: DocMessage {
                    doc_name: topic.clone(),
                    message: msg.clone(),
                    source: None,
                },
            })
        }
    }

    fn on_connection_established(&mut self, conn: ConnectionEstablished) {
        if conn.other_established > 0 {
            // We only care about the first time a peer connects.
            return;
        }

        self.connected_peers.insert(conn.peer_id);
        self.welcome(conn.peer_id);
    }

    fn on_connection_closed(
        &mut self,
        ConnectionClosed {
            peer_id,
            remaining_established,
            ..
        }: ConnectionClosed,
    ) {
        if remaining_established > 0 {
            // we only care about peer disconnections
            return;
        }

        let was_in = self.connected_peers.remove(&peer_id);
        debug_assert!(was_in);

        // We can be disconnected by the remote in case of inactivity for example, so we always
        // try to reconnect.
        if self.target_peers.contains(&peer_id) {
            self.events.push_back(ToSwarm::Dial {
                opts: DialOpts::peer_id(peer_id).build(),
            });
        }
    }

    fn handle_message(&mut self, msg: DocMessage, sender: PeerId) {
        if msg.source.is_none() {
            // ignore messages generated locally
            return;
        }

        let handle = self.handler(msg.doc_name.clone());
        match handle.handle(&msg.message, &sender) {
            Ok(None) => { /* do nothing */ }
            Ok(Some(reply)) => {
                tracing::trace!("sending reply to `{sender}`: {reply:?}");
                self.events.push_back(ToSwarm::NotifyHandler {
                    peer_id: sender,
                    handler: NotifyHandler::Any,
                    event: DocMessage {
                        source: None,
                        doc_name: msg.doc_name.clone(),
                        message: reply.into(),
                    },
                });
            }
            Err(e) => {
                tracing::warn!("failed to handle message from {sender}: {e}")
            }
        }

        self.events
            .push_back(ToSwarm::GenerateEvent(Event::Message {
                source: msg.source,
                topic: msg.doc_name,
                message: msg.message,
            }));
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = OneShotHandler<Protocol, DocMessage, InnerMessage>;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer_id: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let listen_protocol = SubstreamProtocol::new(Protocol::new(peer_id), Default::default());
        Ok(OneShotHandler::new(listen_protocol, Default::default()))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer_id: PeerId,
        _: &Multiaddr,
        _: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let listen_protocol = SubstreamProtocol::new(Protocol::new(peer_id), Default::default());
        Ok(OneShotHandler::new(listen_protocol, Default::default()))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(connection_established) => {
                self.on_connection_established(connection_established)
            }
            FromSwarm::ConnectionClosed(connection_closed) => {
                self.on_connection_closed(connection_closed)
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        propagation_source: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        // We ignore successful sends or timeouts.
        match event {
            Ok(InnerMessage::Rx(msg)) => self.handle_message(msg, propagation_source),
            Ok(InnerMessage::Sent) => return,
            Err(e) => {
                tracing::warn!("Failed to send message: {e}");
                self.events.push_back(ToSwarm::CloseConnection {
                    peer_id: propagation_source,
                    connection: CloseConnection::One(connection_id),
                });
                return;
            }
        };
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        loop {
            if let Some(e) = self.events.pop_front() {
                return Poll::Ready(e);
            } else {
                match self.update_receiver.poll_recv(cx) {
                    Poll::Ready(Some(e)) => {
                        match e {
                            InternalEvent::Update { topic, update } => {
                                self.broadcast(topic, Message::Sync(SyncMessage::Update(update)));
                            }
                            InternalEvent::AwarenessUpdate {
                                topic,
                                added,
                                updated,
                                removed,
                            } => {
                                let mut clients =
                                    Vec::with_capacity(added.len() + updated.len() + removed.len());
                                clients.extend_from_slice(&added);
                                clients.extend_from_slice(&updated);
                                clients.extend_from_slice(&removed);
                                let awareness = self.awareness(topic.clone());
                                match awareness.update_with_clients(clients) {
                                    Ok(update) => {
                                        self.broadcast(topic, Message::Awareness(update));
                                    }
                                    Err(e) => {
                                        tracing::warn!("failed to generate awareness update for topic `{}`: {}", topic, e);
                                    }
                                }
                            }
                        }
                    }
                    Poll::Ready(None) | Poll::Pending => return Poll::Pending,
                }
            }
        }
    }
}

/// Transmission between the `OneShotHandler` and the `Handler`.
#[derive(Debug)]
pub enum InnerMessage {
    /// We received an RPC from a remote.
    Rx(DocMessage),
    /// We successfully sent an RPC request.
    Sent,
}

impl From<DocMessage> for InnerMessage {
    #[inline]
    fn from(rpc: DocMessage) -> InnerMessage {
        InnerMessage::Rx(rpc)
    }
}

impl From<()> for InnerMessage {
    #[inline]
    fn from(_: ()) -> InnerMessage {
        InnerMessage::Sent
    }
}

/// Event that can happen on the floodsub behaviour.
#[derive(Debug)]
pub enum Event {
    /// A message has been received.
    Message {
        /// Unique document identifier.
        topic: Topic,
        /// YSync protocol message.
        message: Message,
        /// Unique peer identifier of the message sender.
        /// `None` if message was generated by current peer.
        source: Option<PeerId>,
    },
}

const PROTOCOL_NAME: StreamProtocol = StreamProtocol::new("/ysync");

/// Implementation of `ConnectionUpgrade` for the floodsub protocol.
#[derive(Debug, Clone)]
pub struct Protocol {
    peer_id: PeerId,
}

impl Protocol {
    /// Builds a new `Protocol`.
    pub fn new(peer_id: PeerId) -> Protocol {
        Protocol { peer_id }
    }
}

impl UpgradeInfo for Protocol {
    type Info = StreamProtocol;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once(PROTOCOL_NAME)
    }
}

impl<TSocket> InboundUpgrade<TSocket> for Protocol
where
    TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = DocMessage;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(self, socket: TSocket, _: Self::Info) -> Self::Future {
        use tokio_util::compat::FuturesAsyncReadCompatExt;
        let peer_id = self.peer_id.clone();
        Box::pin(async move {
            let mut framed = Framed::new(socket.compat(), Codec::new(Some(peer_id)));

            let msg = framed
                .next()
                .await
                .ok_or_else(|| Error::ReadError(io::ErrorKind::UnexpectedEof.into()))??;

            Ok(msg)
        })
    }
}

/// Reach attempt interrupt errors.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Error when reading the packet from the socket.
    #[error("Failed to read from socket: {0}")]
    ReadError(#[from] io::Error),
    #[error("Failed to decode message: {0}")]
    DecodeError(#[from] yrs::encoding::read::Error),
    #[error("failed to sync message: {0}")]
    SyncError(#[from] yrs::sync::Error),
}

impl From<yrs::sync::awareness::Error> for Error {
    fn from(e: yrs::sync::awareness::Error) -> Self {
        Error::SyncError(yrs::sync::Error::AwarenessEncoding(e))
    }
}

/// An RPC received by the floodsub system.
#[derive(Debug, Clone)]
pub struct DocMessage {
    /// Name of the document, this message is related to.
    pub doc_name: Topic,
    /// Message content.
    pub message: Message,
    /// Has this message been generated by the current peer.
    pub source: Option<PeerId>,
}

impl UpgradeInfo for DocMessage {
    type Info = StreamProtocol;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once(PROTOCOL_NAME)
    }
}

impl<TSocket> OutboundUpgrade<TSocket> for DocMessage
where
    TSocket: AsyncWrite + AsyncRead + Send + Unpin + 'static,
{
    type Output = ();
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_outbound(self, socket: TSocket, _: Self::Info) -> Self::Future {
        use tokio_util::compat::FuturesAsyncReadCompatExt;
        Box::pin(async move {
            let mut framed = Framed::new(socket.compat(), Codec::new(None));
            framed.send(self).await?;
            framed.close().await?;
            Ok(())
        })
    }
}

/// Configuration options for the  protocol.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Template used for options used to create new documents.
    pub doc_options: Options,
}

impl Config {
    pub fn new(doc_options: Options) -> Self {
        Self { doc_options }
    }
}

struct Codec {
    peer_id: Option<PeerId>,
    inner: LengthDelimitedCodec,
}

impl Codec {
    fn new(peer_id: Option<PeerId>) -> Self {
        Codec {
            peer_id,
            inner: LengthDelimitedCodec::new(),
        }
    }
}

impl tokio_util::codec::Encoder<DocMessage> for Codec {
    type Error = Error;

    fn encode(&mut self, item: DocMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let mut encoder = EncoderV1::new();
        encoder.write_string(&*item.doc_name);
        item.message.encode(&mut encoder);
        self.inner.encode(Bytes::from(encoder.to_vec()), dst)?;
        Ok(())
    }
}

impl Decoder for Codec {
    type Item = DocMessage;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(src) {
            Ok(Some(data)) => {
                let mut decoder = DecoderV1::new(Cursor::new(data.as_ref()));
                let topic: Topic = decoder.read_string()?.into();
                let message = Message::decode(&mut decoder)?;
                let message = DocMessage {
                    source: self.peer_id,
                    doc_name: topic,
                    message,
                };
                tracing::trace!("received message: {message:?}");
                Ok(Some(message))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

struct TopicHandler {
    awareness: Awareness,
    _on_update: Subscription,
    _on_awareness_update: Subscription,
}

impl TopicHandler {
    fn new(options: Options, mailbox: UnboundedSender<InternalEvent>) -> Self {
        let topic = options.guid.clone();
        let doc = Doc::with_options(options);
        let on_update = {
            let topic = topic.clone();
            let mailbox = mailbox.clone();
            doc.observe_update_v1(move |_, e| {
                // we only propagate local non-empty updates
                if has_data(&e.update) {
                    let _ = mailbox.send(InternalEvent::Update {
                        topic: topic.clone(),
                        update: e.update.clone(),
                    });
                }
            })
            .unwrap()
        };
        let awareness = Awareness::new(doc);
        let on_awareness_update = awareness.on_update(move |e| {
            if !(e.updated().is_empty() && e.added().is_empty() && e.removed().is_empty()) {
                let _ = mailbox.send(InternalEvent::AwarenessUpdate {
                    topic: topic.clone(),
                    added: e.added().into(),
                    updated: e.updated().into(),
                    removed: e.removed().into(),
                });
            }
        });

        TopicHandler {
            awareness,
            _on_update: on_update,
            _on_awareness_update: on_awareness_update,
        }
    }

    fn handle(&mut self, msg: &Message, sender: &PeerId) -> Result<Option<Message>, Error> {
        let a = &mut self.awareness;
        match msg {
            Message::Sync(msg) => match msg {
                SyncMessage::SyncStep1(sv) => {
                    let update = a.doc().transact().encode_state_as_update_v1(sv);
                    if has_data(&*update) {
                        Ok(Some(Message::Sync(SyncMessage::SyncStep2(update))))
                    } else {
                        Ok(None)
                    }
                }
                SyncMessage::SyncStep2(u) | SyncMessage::Update(u) => {
                    let u = Update::decode_v1(&*u)?;
                    a.doc_mut()
                        .transact_mut_with(&*sender.to_bytes())
                        .apply_update(u);
                    Ok(None)
                }
            },
            Message::Auth(deny_reason) => {
                if let Some(reason) = deny_reason.clone() {
                    Err(yrs::sync::Error::PermissionDenied { reason })
                } else {
                    Ok(None)
                }
            }
            Message::AwarenessQuery => {
                let u = a.update()?;
                Ok(Some(Message::Awareness(u)))
            }
            Message::Awareness(u) => {
                a.apply_update(u.clone())?;
                Ok(None)
            }
            Message::Custom(_tag, _data) => Ok(None),
        }
        .map_err(|e| e.into())
    }
}

#[derive(Debug)]
enum InternalEvent {
    Update {
        topic: Topic,
        update: Vec<u8>,
    },
    AwarenessUpdate {
        topic: Topic,
        added: Vec<ClientID>,
        updated: Vec<ClientID>,
        removed: Vec<ClientID>,
    },
}
#[inline]
fn has_data(update: &[u8]) -> bool {
    update != &[0, 0]
}
