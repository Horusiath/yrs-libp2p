use std::task::{Context, Poll};
use libp2p::gossipsub::{AllowAllSubscriptionFilter, Behaviour, IdentityTransform};
use libp2p::{gossipsub, mdns, Multiaddr, PeerId};
use libp2p::core::Endpoint;
use libp2p::mdns::tokio::Tokio;
use libp2p::swarm::{ConnectionDenied, ConnectionHandler, ConnectionHandlerEvent, ConnectionId, FromSwarm, NetworkBehaviour, SubstreamProtocol, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm};
use libp2p::swarm::handler::ConnectionEvent;

pub struct YSyncBehaviour {

}

impl YSyncBehaviour {
    pub fn new(gossipsub: Behaviour<IdentityTransform, AllowAllSubscriptionFilter>, mdns: Behaviour<Tokio>) -> Self {
        todo!()
    }
}

impl NetworkBehaviour for YSyncBehaviour {
    type ConnectionHandler = YSyncConnectionHandler;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(&mut self, _connection_id: ConnectionId, peer: PeerId, local_addr: &Multiaddr, remote_addr: &Multiaddr) -> Result<THandler<Self>, ConnectionDenied> {
        todo!()
    }

    fn handle_established_outbound_connection(&mut self, _connection_id: ConnectionId, peer: PeerId, addr: &Multiaddr, role_override: Endpoint) -> Result<THandler<Self>, ConnectionDenied> {
        todo!()
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        todo!()
    }

    fn on_connection_handler_event(&mut self, _peer_id: PeerId, _connection_id: ConnectionId, _event: THandlerOutEvent<Self>) {
        todo!()
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        todo!()
    }
}

pub enum Event {
    Mdns(mdns::Event),
    Gossipsub(gossipsub::Event),
}

pub struct YSyncConnectionHandler {

}

impl YSyncConnectionHandler {

}

impl ConnectionHandler for YSyncConnectionHandler {
    type FromBehaviour = ();
    type ToBehaviour = ();
    type InboundProtocol = ();
    type OutboundProtocol = ();
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        todo!()
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>> {
        todo!()
    }

    fn on_behaviour_event(&mut self, _event: Self::FromBehaviour) {
        todo!()
    }

    fn on_connection_event(&mut self, event: ConnectionEvent<Self::InboundProtocol, Self::OutboundProtocol, Self::InboundOpenInfo, Self::OutboundOpenInfo>) {
        todo!()
    }
}