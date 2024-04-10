use futures::{AsyncRead, AsyncWrite};
use libp2p::core::UpgradeInfo;
use libp2p::{InboundUpgrade, OutboundUpgrade, StreamProtocol};
use std::future::Future;
use std::pin::Pin;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};
use yrs::sync::{DefaultProtocol, Protocol};

pub const SIGNING_PREFIX: &[u8] = b"libp2p-ysync:";
pub const PROTOCOL_INFO: StreamProtocol = StreamProtocol::new("/ysync");

#[derive(Debug, Clone, Copy, Default)]
pub struct ProtocolConfig<P> {
    pub protocol: P,
}

impl<P: Protocol> ProtocolConfig<P> {
    pub fn new(protocol: P) -> Self {
        ProtocolConfig { protocol }
    }
}

impl<P> UpgradeInfo for ProtocolConfig<P> {
    type Info = StreamProtocol;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL_INFO)
    }
}

impl<S, P> InboundUpgrade<S> for ProtocolConfig<P>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Output = Framed<Compat<S>, LengthDelimitedCodec>;
    type Error = ();
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(self, socket: S, _info: Self::Info) -> Self::Future {
        Box::pin(async { Ok(Framed::new(socket.compat(), LengthDelimitedCodec::new())) })
    }
}

impl<S, P> OutboundUpgrade<S> for ProtocolConfig<P>
where
    S: AsyncWrite + AsyncRead + Unpin + Send + 'static,
{
    type Output = Framed<Compat<S>, LengthDelimitedCodec>;
    type Error = ();
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_outbound(self, socket: S, info: Self::Info) -> Self::Future {
        Box::pin(async { Ok(Framed::new(socket.compat(), LengthDelimitedCodec::new())) })
    }
}
