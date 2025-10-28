use std::str::FromStr;

use futures::{
    channel::{mpsc, oneshot},
    future::BoxFuture,
    SinkExt,
};
use http::Uri;
use libp2p::{multiaddr::{Multiaddr, Protocol}, PeerId};
use tower::Service;

use crate::{
    stream::{Libp2pStream, Libp2pStreamHandle},
    swarm::{SwarmCommand, SwarmCommandSender, SwarmHandle},
    BridgeError, Result,
};

/// Connector that yields libp2p streams to tonic clients.
pub struct Libp2pConnector {
    command_tx: SwarmCommandSender,
}

impl Libp2pConnector {
    pub fn new(command_tx: SwarmCommandSender) -> Self {
        Self { command_tx }
    }
}

impl Service<Uri> for Libp2pConnector {
    type Response = Libp2pStream;
    type Error = BridgeError;
    type Future = BoxFuture<'static, Result<Libp2pStream>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        let mut command_tx = self.command_tx.clone();
        Box::pin(async move {
            let target = parse_target(&req)?;
            let (response_tx, response_rx) = oneshot::channel();
            command_tx
                .send(SwarmCommand::Dial { peer: target.peer_id, addrs: target.addresses, response: response_tx })
                .await
                .map_err(|_| BridgeError::CommandChannelClosed)?;
            let handle = response_rx.await.map_err(|_| BridgeError::DialFailed("dial response dropped".into()))??;
            Ok(handle.into_stream())
        })
    }
}

/// Incoming stream wrapper to feed into `Server::serve_with_incoming`.
pub struct Libp2pIncoming {
    inner: mpsc::Receiver<Libp2pStreamHandle>,
}

impl Libp2pIncoming {
    pub fn new(inner: mpsc::Receiver<Libp2pStreamHandle>) -> Self {
        Self { inner }
    }
}

/// Detach the pending incoming stream receiver from a swarm handle.
pub fn incoming_from_handle(handle: &mut SwarmHandle) -> Result<Libp2pIncoming> {
    let rx = handle.take_incoming().ok_or(BridgeError::IncomingClosed)?;
    Ok(Libp2pIncoming::new(rx))
}

impl futures::Stream for Libp2pIncoming {
    type Item = std::result::Result<Libp2pStream, std::io::Error>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match futures::Stream::poll_next(std::pin::Pin::new(&mut this.inner), cx) {
            std::task::Poll::Ready(Some(handle)) => std::task::Poll::Ready(Some(Ok(handle.into_stream()))),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl Default for Libp2pIncoming {
    fn default() -> Self {
        let (_tx, rx) = mpsc::channel(1);
        Self { inner: rx }
    }
}

struct DialTarget {
    peer_id: PeerId,
    addresses: Vec<Multiaddr>,
}

fn parse_target(uri: &Uri) -> Result<DialTarget> {
    let scheme = uri.scheme_str().unwrap_or_default();
    if !scheme.is_empty() && scheme != "libp2p" {
        return Err(BridgeError::DialFailed(format!("unsupported scheme: {scheme}")));
    }

    let peer_part = if let Some(authority) = uri.authority() { authority.as_str() } else { uri.path().trim_start_matches('/') };

    if peer_part.is_empty() {
        return Err(BridgeError::DialFailed("missing peer id in uri".into()));
    }

    let peer_id = PeerId::from_str(peer_part).map_err(|e| BridgeError::DialFailed(format!("invalid peer id: {e}")))?;

    let mut addrs = Vec::new();
    if let Some(query) = uri.query() {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("");
            if matches!(key, "addr" | "multiaddr") {
                let decoded = percent_decode(value)?;
                let addr = Multiaddr::from_str(&decoded)
                    .map_err(|e| BridgeError::DialFailed(format!("invalid multiaddr '{decoded}': {e}")))?;
                addrs.push(sanitize_multiaddr(addr, peer_id)?);
            }
        }
    }

    if addrs.is_empty() {
        return Err(BridgeError::DialFailed("missing addr query parameters".into()));
    }

    Ok(DialTarget { peer_id, addresses: addrs })
}

fn percent_decode(input: &str) -> Result<String> {
    if input.contains('%') {
        let decoded = urlencoding::decode(input).map_err(|e| BridgeError::DialFailed(format!("invalid percent-encoding: {e}")))?;
        Ok(decoded.into_owned())
    } else {
        Ok(input.to_owned())
    }
}

fn sanitize_multiaddr(mut addr: Multiaddr, peer_id: PeerId) -> Result<Multiaddr> {
    match addr.iter().last() {
        Some(Protocol::P2p(id)) if id == peer_id => {
            addr.pop();
            Ok(addr)
        }
        Some(Protocol::P2p(id)) => Err(BridgeError::DialFailed(format!(
            "multiaddr '{addr}' targets peer {id} instead of {peer_id}"
        ))),
        _ => Err(BridgeError::DialFailed(format!("multiaddr '{addr}' missing terminal /p2p/ component"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_sanitizes_terminal_peer_component() {
        let peer = PeerId::random();
        let multiaddr = format!("/ip4/127.0.0.1/tcp/12345/p2p/{peer}");
        let uri: Uri = format!("libp2p://{peer}?addr={}", urlencoding::encode(&multiaddr))
            .parse()
            .expect("uri");

        let target = parse_target(&uri).expect("target");
        assert_eq!(target.peer_id, peer);
        assert_eq!(target.addresses.len(), 1);
        assert_eq!(target.addresses[0].to_string(), "/ip4/127.0.0.1/tcp/12345");
    }

    #[test]
    fn parse_target_rejects_missing_addresses() {
        let peer = PeerId::random();
        let uri: Uri = format!("libp2p://{peer}").parse().unwrap();
        assert!(matches!(parse_target(&uri), Err(BridgeError::DialFailed(msg)) if msg.contains("missing addr")));
    }

    #[test]
    fn parse_target_rejects_mismatched_peer() {
        let peer = PeerId::random();
        let other = PeerId::random();
        let multiaddr = format!("/ip4/10.0.0.5/tcp/5555/p2p/{other}");
        let uri: Uri = format!("libp2p://{peer}?addr={}", urlencoding::encode(&multiaddr))
            .parse()
            .expect("uri");

        assert!(matches!(parse_target(&uri), Err(BridgeError::DialFailed(msg)) if msg.contains("targets peer")));
    }
}
