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
use yrs::encoding::read::{Cursor, Read};
use yrs::encoding::write::Write;
use yrs::sync::{Awareness, DefaultProtocol, Message, Protocol as _, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{Doc, Options, ReadTxn, Subscription, Transact, Update};

pub type Topic = std::sync::Arc<str>;

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
        Self::from_config(Config::default())
    }

    /// Creates a `` with the given configuration.
    pub fn from_config(config: Config) -> Self {
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

    pub fn awareness(&mut self, topic: Topic) -> &mut Awareness {
        match self.subscribed_topics.entry(topic.clone()) {
            Entry::Vacant(e) => {
                let mut options = Options::default();
                options.guid = topic;
                &mut e
                    .insert(TopicHandler::new(options, self.update_sender.clone()))
                    .awareness
            }
            Entry::Occupied(e) => &mut e.into_mut().awareness,
        }
    }

    /// Add a node to the list of nodes to propagate messages to.
    #[inline]
    pub fn add_peer(&mut self, peer_id: PeerId) {
        // Send our initial message
        if self.connected_peers.contains(&peer_id) {
            for (topic, handle) in self.subscribed_topics.iter() {
                let sv = handle.awareness.doc().transact().state_vector();
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

        if self.target_peers.insert(peer_id) {
            self.events.push_back(ToSwarm::Dial {
                opts: DialOpts::peer_id(peer_id).build(),
            });
        }
    }

    /// Remove a node from the list of nodes to propagate messages to.
    #[inline]
    pub fn remove_peer(&mut self, peer_id: &PeerId) {
        self.target_peers.remove(peer_id);
    }

    /// Subscribes to a topic.
    ///
    /// Returns true if the subscription worked. Returns false if we were already subscribed.
    pub fn subscribe(&mut self, topic: Topic) -> bool {
        let h = match self.subscribed_topics.entry(topic.clone()) {
            Entry::Occupied(_) => return false,
            Entry::Vacant(e) => {
                let mut options = self.config.doc_options.clone();
                options.guid = topic.clone();
                e.insert(TopicHandler::new(options, self.update_sender.clone()))
            }
        };
        let sv = h.awareness.doc().transact().state_vector();

        for peer in self.connected_peers.iter() {
            self.events.push_back(ToSwarm::NotifyHandler {
                peer_id: *peer,
                handler: NotifyHandler::Any,
                event: DocMessage {
                    source: None,
                    doc_name: topic.clone(),
                    message: Message::Sync(SyncMessage::SyncStep1(sv.clone())),
                },
            });
        }

        true
    }

    /// Unsubscribes from a topic.
    ///
    /// Note that this only requires the topic name.
    ///
    /// Returns true if we were subscribed to this topic.
    pub fn unsubscribe(&mut self, topic: &Topic) -> bool {
        self.subscribed_topics.remove(topic).is_some()
    }

    fn broadcast(&mut self, topic: Topic, msg: Message) {
        for &peer_id in self.connected_peers.iter() {
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

    fn on_connection_established(
        &mut self,
        ConnectionEstablished {
            peer_id,
            other_established,
            ..
        }: ConnectionEstablished,
    ) {
        if other_established > 0 {
            // We only care about the first time a peer connects.
            return;
        }

        // We need to send our subscriptions to the newly-connected node.
        if self.target_peers.contains(&peer_id) {
            for (topic, handle) in self.subscribed_topics.iter() {
                let sv = handle.awareness.doc().transact().state_vector();
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

        self.connected_peers.insert(peer_id);
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

        self.events
            .push_back(ToSwarm::GenerateEvent(Event::Message {
                source: msg.source.clone(),
                topic: msg.doc_name.clone(),
                message: msg.message.clone(),
            }));

        let handle = match self.subscribed_topics.entry(msg.doc_name.clone()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let mut options = self.config.doc_options.clone();
                options.guid = msg.doc_name.clone();
                e.insert(TopicHandler::new(options, self.update_sender.clone()))
            }
        };
        match handle.handle(msg.message) {
            Ok(None) => { /* do nothing */ }
            Ok(Some(reply)) => {
                self.events.push_back(ToSwarm::NotifyHandler {
                    peer_id: sender,
                    handler: NotifyHandler::Any,
                    event: DocMessage {
                        source: None,
                        doc_name: msg.doc_name,
                        message: reply.into(),
                    },
                });
            }
            Err(e) => {
                tracing::warn!("failed to handle message from {sender}: {e}")
            }
        }
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
                    Poll::Ready(Some(e)) => match e {
                        InternalEvent::Update { topic, update } => {
                            self.broadcast(topic, Message::Sync(SyncMessage::Update(update)));
                            continue;
                        }
                    },
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
        topic: Topic,
        message: Message,
        source: Option<PeerId>,
    },
}

const PROTOCOL_NAME: StreamProtocol = StreamProtocol::new("/ysync/1.0.0");

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
            tracing::trace!("sending message {self:?}");
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
    pub doc_options: Options,

    /// `true` if messages published by local node should be propagated as messages received from
    /// the network, `false` by default.
    pub subscribe_local_messages: bool,
}

impl Config {
    pub fn new(doc_options: Options) -> Self {
        Self {
            doc_options,
            subscribe_local_messages: false,
        }
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
                tracing::trace!("received message {message:?}");
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
}

impl TopicHandler {
    fn new(options: Options, mailbox: UnboundedSender<InternalEvent>) -> Self {
        let topic = options.guid.clone();
        let doc = Doc::with_options(options);
        let on_update = doc
            .observe_update_v1(move |_, e| {
                // check if update has any data
                if e.update.len() > 2 {
                    let _ = mailbox.send(InternalEvent::Update {
                        topic: topic.clone(),
                        update: e.update.clone(),
                    });
                }
            })
            .unwrap();
        let awareness = Awareness::new(doc);
        TopicHandler {
            awareness,
            _on_update: on_update,
        }
    }

    fn handle(&mut self, msg: Message) -> Result<Option<Message>, Error> {
        let protocol = DefaultProtocol;
        let awareness = &mut self.awareness;
        match msg {
            Message::Sync(msg) => match msg {
                SyncMessage::SyncStep1(sv) => protocol.handle_sync_step1(awareness, sv),
                SyncMessage::SyncStep2(u) => {
                    protocol.handle_sync_step2(awareness, Update::decode_v1(&*u)?)
                }
                SyncMessage::Update(u) => {
                    protocol.handle_update(awareness, Update::decode_v1(&*u)?)
                }
            },
            Message::Auth(deny_reason) => protocol.handle_auth(awareness, deny_reason),
            Message::AwarenessQuery => protocol.handle_awareness_query(awareness),
            Message::Awareness(u) => protocol.handle_awareness_update(awareness, u),
            Message::Custom(tag, data) => {
                protocol.missing_handle(awareness, tag.clone(), data.clone())
            }
        }
        .map_err(|e| e.into())
    }
}

#[derive(Debug)]
enum InternalEvent {
    Update { topic: Topic, update: Vec<u8> },
}
