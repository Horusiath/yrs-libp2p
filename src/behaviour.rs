use crate::handler::Handler;
use crate::protocol::ProtocolConfig;
use libp2p::core::Endpoint;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId};
use std::task::{Context, Poll};
use yrs::sync::{Awareness, DefaultProtocol, Protocol};

pub struct Behaviour<P> {
    awareness: Awareness,
    config: ProtocolConfig<P>,
}

impl<P: Protocol> Behaviour<P> {
    pub fn new(awareness: Awareness, protocol: P) -> Self {
        Behaviour {
            awareness,
            config: ProtocolConfig::new(protocol),
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
}

impl Default for Behaviour<DefaultProtocol> {
    fn default() -> Self {
        Self::new(Awareness::default(), DefaultProtocol)
    }
}

impl<P> NetworkBehaviour for Behaviour<P>
where
    P: Protocol + Clone + Send + 'static,
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
        Handler::new(
            connection_id,
            peer,
            &self.awareness,
            self.config.protocol.clone(),
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Handler::new(
            connection_id,
            peer,
            &self.awareness,
            self.config.protocol.clone(),
        )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(established) => {}
            FromSwarm::ConnectionClosed(closed) => {}
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
        _peer_id: PeerId,
        _connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        todo!()
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        todo!()
    }
}

#[derive(Debug)]
pub enum Event {}
