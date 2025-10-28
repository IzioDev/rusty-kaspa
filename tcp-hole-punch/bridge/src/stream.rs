use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    task::{Context, Poll},
};

use libp2p::{multiaddr::Protocol, swarm::Stream as Libp2pSubstream, Multiaddr, PeerId};
use tokio::io::{AsyncRead as TokioAsyncRead, AsyncWrite as TokioAsyncWrite, ReadBuf};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};

/// Handle returned when the swarm completes a dial or accepts an inbound stream.
pub struct Libp2pStreamHandle {
    inner: Libp2pSubstream,
    info: Libp2pConnectInfo,
}

impl Libp2pStreamHandle {
    pub fn new(inner: Libp2pSubstream, info: Libp2pConnectInfo) -> Self {
        Self { inner, info }
    }

    pub fn info(&self) -> &Libp2pConnectInfo {
        &self.info
    }

    pub fn into_stream(self) -> Libp2pStream {
        Libp2pStream::new(self.inner, self.info)
    }
}

/// Metadata describing the remote peer and connection context.
#[derive(Clone, Debug)]
pub struct Libp2pConnectInfo {
    pub peer_id: PeerId,
    pub remote_multiaddr: Option<Multiaddr>,
    pub relay_used: bool,
    pub synthesized_socket: Option<SocketAddr>,
}

impl Libp2pConnectInfo {
    pub fn new(peer_id: PeerId) -> Self {
        Self { peer_id, remote_multiaddr: None, relay_used: false, synthesized_socket: None }
    }

    pub fn with_address(peer_id: PeerId, remote_multiaddr: Multiaddr, relay_used: bool) -> Self {
        let synthesized_socket = multiaddr_to_socket(&remote_multiaddr);
        Self { peer_id, remote_multiaddr: Some(remote_multiaddr), relay_used, synthesized_socket }
    }
}

/// Wrapper that implements tonic's `Connected` trait and Tokio IO traits.
pub struct Libp2pStream {
    pub info: Libp2pConnectInfo,
    inner: Compat<Libp2pSubstream>,
}

impl Libp2pStream {
    fn new(inner: Libp2pSubstream, info: Libp2pConnectInfo) -> Self {
        Self { info, inner: inner.compat() }
    }
}

impl fmt::Debug for Libp2pStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut dbg = f.debug_struct("Libp2pStream");
        dbg.field("peer_id", &self.info.peer_id).field("relay_used", &self.info.relay_used);
        if let Some(addr) = &self.info.remote_multiaddr {
            dbg.field("remote_multiaddr", addr);
        }
        dbg.finish()
    }
}

impl TokioAsyncRead for Libp2pStream {
    fn poll_read(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl TokioAsyncWrite for Libp2pStream {
    fn poll_write(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl tonic::transport::server::Connected for Libp2pStream {
    type ConnectInfo = Libp2pConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.info.clone()
    }
}

fn multiaddr_to_socket(addr: &Multiaddr) -> Option<SocketAddr> {
    let mut ip: Option<IpAddr> = None;
    let mut port: Option<u16> = None;

    for component in addr.iter() {
        match component {
            Protocol::Ip4(v4) => ip = Some(IpAddr::V4(v4)),
            Protocol::Ip6(v6) => ip = Some(IpAddr::V6(v6)),
            Protocol::Tcp(p) => port = Some(p),
            _ => {}
        }
    }

    match (ip, port) {
        (Some(ip), Some(port)) => Some(SocketAddr::new(ip, port)),
        _ => None,
    }
}
