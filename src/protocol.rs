use futures::{AsyncRead, AsyncWrite};
use libp2p::core::UpgradeInfo;
use libp2p::{InboundUpgrade, OutboundUpgrade, StreamProtocol};
use std::future::Future;
use std::pin::Pin;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};
use yrs::sync::DefaultProtocol;

pub const SIGNING_PREFIX: &[u8] = b"libp2p-ysync:";
pub const PROTOCOL_V1: StreamProtocol = StreamProtocol::new("/ysync/1.0.0");

#[derive(Debug, Clone)]
pub struct ProtocolConfig<P> {
    pub protocol: P,
}

impl<P> ProtocolConfig<P> {
    pub fn new(protocol: P) -> Self {
        ProtocolConfig { protocol }
    }
}

impl Default for ProtocolConfig<DefaultProtocol> {
    fn default() -> Self {
        ProtocolConfig::new(DefaultProtocol)
    }
}

impl<P> UpgradeInfo for ProtocolConfig<P> {
    type Info = StreamProtocol;
    type InfoIter = Vec<StreamProtocol>;

    fn protocol_info(&self) -> Self::InfoIter {
        vec![PROTOCOL_V1]
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
