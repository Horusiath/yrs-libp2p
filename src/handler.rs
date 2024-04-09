use crate::protocol::ProtocolConfig;
use libp2p::core::upgrade::DeniedUpgrade;
use libp2p::swarm::derive_prelude::Either;
use libp2p::swarm::handler::ConnectionEvent;
use libp2p::swarm::{
    ConnectionDenied, ConnectionHandler, ConnectionHandlerEvent, ConnectionId, SubstreamProtocol,
};
use libp2p::PeerId;
use std::task::{Context, Poll};
use yrs::sync::{Awareness, Protocol};
use yrs::Subscription;

pub struct Handler<P> {
    connection_id: ConnectionId,
    peer_id: PeerId,
    protocol: P,
    on_awareness_update: Subscription,
    on_update: Subscription,
}

impl<P> Handler<P> {
    pub fn new(
        connection_id: ConnectionId,
        peer_id: PeerId,
        awareness: &Awareness,
        protocol: P,
    ) -> Result<Self, ConnectionDenied> {
        let on_awareness_update = awareness.on_update(move |e| {
            todo!();
        });
        let on_update = awareness
            .doc()
            .observe_update_v1(move |txn, e| todo!())
            .map_err(|_| ConnectionDenied::new("couldn't subscribe for document updates"))?;
        Ok(Handler {
            connection_id,
            peer_id,
            protocol,
            on_awareness_update,
            on_update,
        })
    }
}

impl<P> ConnectionHandler for Handler<P>
where
    P: Protocol + Send + 'static,
{
    type FromBehaviour = HandlerIn;
    type ToBehaviour = HandlerEvent;
    type InboundProtocol = Either<ProtocolConfig<P>, DeniedUpgrade>;
    type OutboundProtocol = ProtocolConfig<P>;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        todo!()
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        todo!()
    }

    fn on_behaviour_event(&mut self, _event: Self::FromBehaviour) {
        todo!()
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
        todo!()
    }
}

#[derive(Debug)]
pub enum HandlerIn {}

#[derive(Debug)]
pub enum HandlerEvent {}
