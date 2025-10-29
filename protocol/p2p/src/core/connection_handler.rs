use crate::common::ProtocolError;
use crate::core::connection_info::ConnectionMetadata;
use crate::core::hub::HubEvent;
use crate::pb::{
    p2p_client::P2pClient as ProtoP2pClient, p2p_server::P2p as ProtoP2p, p2p_server::P2pServer as ProtoP2pServer, KaspadMessage,
};
use crate::{ConnectionInitializer, Router};
use futures::{future::poll_fn, FutureExt, Stream};
use http::Uri;
use hyper_util::rt::TokioIo;
use kaspa_core::{debug, info};
use kaspa_utils::networking::NetAddress;
use kaspa_utils_tower::{
    counters::TowerConnectionCounters,
    middleware::{BodyExt, CountBytesBody, MapRequestBodyLayer, MapResponseBodyLayer, ServiceBuilder},
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{channel as mpsc_channel, Sender as MpscSender};
use tokio::sync::oneshot::{channel as oneshot_channel, Sender as OneshotSender};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::body::BoxBody;
use tonic::transport::{Error as TonicError, Server as TonicServer};
use tonic::{Request, Response, Status as TonicStatus, Streaming};
use tower::Service;

#[cfg(feature = "libp2p-bridge")]
use hole_punch_bridge::stream::Libp2pConnectInfo as BridgeLibp2pInfo;

#[derive(Error, Debug)]
pub enum ConnectionError {
    #[error("missing socket address")]
    NoAddress,

    #[error("{0}")]
    IoError(#[from] std::io::Error),

    #[error("{0}")]
    TonicError(#[from] TonicError),

    #[error("{0}")]
    TonicStatus(#[from] TonicStatus),

    #[error("{0}")]
    ProtocolError(#[from] ProtocolError),
}

/// Maximum P2P decoded gRPC message size to send and receive
const P2P_MAX_MESSAGE_SIZE: usize = 1024 * 1024 * 1024; // 1GB
const LIBP2P_HTTP2_STREAM_WINDOW: u32 = 8 * 1024 * 1024; // 8 MiB
const LIBP2P_HTTP2_CONNECTION_WINDOW: u32 = 16 * 1024 * 1024; // 16 MiB
const LIBP2P_HTTP2_MAX_FRAME_SIZE: u32 = 1024 * 1024; // 1 MiB
const LIBP2P_HTTP2_MAX_HEADER_LIST_SIZE: u32 = 64 * 1024; // 64 KiB

static SYNTHETIC_ADDR_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Handles Router creation for both server and client-side new connections
#[derive(Clone)]
pub struct ConnectionHandler {
    /// Cloned on each new connection so that routers can communicate with a central hub
    hub_sender: MpscSender<HubEvent>,
    initializer: Arc<dyn ConnectionInitializer>,
    counters: Arc<TowerConnectionCounters>,
}

impl ConnectionHandler {
    pub(crate) fn new(
        hub_sender: MpscSender<HubEvent>,
        initializer: Arc<dyn ConnectionInitializer>,
        counters: Arc<TowerConnectionCounters>,
    ) -> Self {
        Self { hub_sender, initializer, counters }
    }

    /// Connect to a peer using a pre-established async stream instead of dialing by address.
    pub(crate) async fn connect_with_stream<S>(&self, stream: S, metadata: ConnectionMetadata) -> Result<Arc<Router>, ConnectionError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let connector = StreamConnector::new(stream);
        let channel =
            build_libp2p_channel(connector, Duration::from_millis(Self::connect_timeout())).await.map_err(ConnectionError::IoError)?;

        let mut client = ProtoP2pClient::with_origin(channel, Uri::from_static("http://kaspa.libp2p"))
            .send_compressed(tonic::codec::CompressionEncoding::Gzip)
            .accept_compressed(tonic::codec::CompressionEncoding::Gzip)
            .max_decoding_message_size(P2P_MAX_MESSAGE_SIZE);

        let (outgoing_route, outgoing_receiver) = mpsc_channel(Self::outgoing_network_channel_size());
        let incoming_stream = client.message_stream(ReceiverStream::new(outgoing_receiver)).await?.into_inner();

        let socket_address = Self::synthetic_socket_addr(&metadata);
        let router =
            Router::new(socket_address, Some(metadata.clone()), true, self.hub_sender.clone(), incoming_stream, outgoing_route).await;

        match self.initializer.initialize_connection(router.clone()).await {
            Ok(()) => {
                if let Some(summary) = metadata.summary() {
                    info!("P2P, Client connected via libp2p: {}", summary);
                }
                self.hub_sender.send(HubEvent::NewPeer(router.clone())).await.expect("hub receiver should never drop before senders");
            }

            Err(err) => {
                router.try_sending_reject_message(&err).await;
                router.close().await;
                debug!("P2P, handshake failed for outbound libp2p peer {}: {}", router, err);
                return Err(ConnectionError::ProtocolError(err));
            }
        }

        Ok(router)
    }

    /// Launches a P2P server listener loop
    pub(crate) fn serve(&self, serve_address: NetAddress) -> Result<OneshotSender<()>, ConnectionError> {
        let (termination_sender, termination_receiver) = oneshot_channel::<()>();
        let connection_handler = self.clone();
        info!("P2P Server starting on: {}", serve_address);

        tokio::spawn(async move {
            let proto_server = ProtoP2pServer::new(connection_handler).max_decoding_message_size(P2P_MAX_MESSAGE_SIZE);
            let proto_server = proto_server
                .send_compressed(tonic::codec::CompressionEncoding::Gzip)
                .accept_compressed(tonic::codec::CompressionEncoding::Gzip);

            // TODO: check whether we should set tcp_keepalive
            let serve_result = configure_libp2p_server(TonicServer::builder())
                .add_service(proto_server)
                .serve_with_shutdown(serve_address.into(), termination_receiver.map(drop))
                .await;

            match serve_result {
                Ok(_) => info!("P2P Server stopped: {}", serve_address),
                Err(err) => panic!("P2P, Server {serve_address} stopped with error: {err:?}"),
            }
        });
        Ok(termination_sender)
    }

    pub(crate) fn serve_with_incoming<S, I>(&self, incoming: I)
    where
        S: AsyncRead + AsyncWrite + tonic::transport::server::Connected + Send + Unpin + 'static,
        I: Stream<Item = Result<S, std::io::Error>> + Send + 'static,
    {
        let connection_handler = self.clone();
        tokio::spawn(async move {
            let proto_server = ProtoP2pServer::new(connection_handler.clone()).max_decoding_message_size(P2P_MAX_MESSAGE_SIZE);
            let proto_server = proto_server
                .send_compressed(tonic::codec::CompressionEncoding::Gzip)
                .accept_compressed(tonic::codec::CompressionEncoding::Gzip);

            let result = configure_libp2p_server(TonicServer::builder()).add_service(proto_server).serve_with_incoming(incoming).await;

            match result {
                Ok(_) => debug!("P2P, libp2p incoming server stopped"),
                Err(err) => debug!("P2P, libp2p incoming server stopped with error: {err:?}"),
            }
        });
    }

    /// Connect to a new peer
    pub(crate) async fn connect(&self, peer_address: String) -> Result<Arc<Router>, ConnectionError> {
        let Some(socket_address) = peer_address.to_socket_addrs()?.next() else {
            return Err(ConnectionError::NoAddress);
        };
        let peer_address = format!("http://{}", peer_address); // Add scheme prefix as required by Tonic

        let channel = tonic::transport::Endpoint::new(peer_address)?
            .timeout(Duration::from_millis(Self::communication_timeout()))
            .connect_timeout(Duration::from_millis(Self::connect_timeout()))
            .tcp_keepalive(Some(Duration::from_millis(Self::keep_alive())))
            .connect()
            .await?;

        let channel = ServiceBuilder::new()
            .layer(MapResponseBodyLayer::new(move |body| CountBytesBody::new(body, self.counters.bytes_rx.clone())))
            .layer(MapRequestBodyLayer::new(move |body| CountBytesBody::new(body, self.counters.bytes_tx.clone()).boxed_unsync()))
            .service(channel);

        let mut client = ProtoP2pClient::new(channel)
            .send_compressed(tonic::codec::CompressionEncoding::Gzip)
            .accept_compressed(tonic::codec::CompressionEncoding::Gzip)
            .max_decoding_message_size(P2P_MAX_MESSAGE_SIZE);

        let (outgoing_route, outgoing_receiver) = mpsc_channel(Self::outgoing_network_channel_size());
        let incoming_stream = client.message_stream(ReceiverStream::new(outgoing_receiver)).await?.into_inner();

        let router = Router::new(socket_address, None, true, self.hub_sender.clone(), incoming_stream, outgoing_route).await;

        // For outbound peers, we perform the initialization as part of the connect logic
        match self.initializer.initialize_connection(router.clone()).await {
            Ok(()) => {
                // Notify the central Hub about the new peer
                self.hub_sender.send(HubEvent::NewPeer(router.clone())).await.expect("hub receiver should never drop before senders");
            }

            Err(err) => {
                router.try_sending_reject_message(&err).await;
                // Ignoring the new router
                router.close().await;
                debug!("P2P, handshake failed for outbound peer {}: {}", router, err);
                return Err(ConnectionError::ProtocolError(err));
            }
        }

        Ok(router)
    }

    /// Connect to a new peer with `retry_attempts` retries and `retry_interval` duration between each attempt
    pub(crate) async fn connect_with_retry(
        &self,
        address: String,
        retry_attempts: u8,
        retry_interval: Duration,
    ) -> Result<Arc<Router>, ConnectionError> {
        let mut counter = 0;
        loop {
            counter += 1;
            match self.connect(address.clone()).await {
                Ok(router) => {
                    debug!("P2P, Client connected, peer: {:?}", address);
                    return Ok(router);
                }
                Err(ConnectionError::ProtocolError(err)) => {
                    // On protocol errors we avoid retrying
                    debug!("P2P, connect retry #{} failed with error {:?}, peer: {:?}, aborting retries", counter, err, address);
                    return Err(ConnectionError::ProtocolError(err));
                }
                Err(err) => {
                    debug!("P2P, connect retry #{} failed with error {:?}, peer: {:?}", counter, err, address);
                    if counter < retry_attempts {
                        // Await `retry_interval` time before retrying
                        tokio::time::sleep(retry_interval).await;
                    } else {
                        debug!("P2P, Client connection retry #{} - all failed", retry_attempts);
                        return Err(err);
                    }
                }
            }
        }
    }

    // TODO: revisit the below constants
    fn outgoing_network_channel_size() -> usize {
        // TODO: this number is taken from go-kaspad and should be re-evaluated
        (1 << 17) + 256
    }

    fn communication_timeout() -> u64 {
        10_000
    }

    fn keep_alive() -> u64 {
        10_000
    }

    fn connect_timeout() -> u64 {
        1_000
    }

    fn synthetic_socket_addr(metadata: &ConnectionMetadata) -> SocketAddr {
        if let Some(addr) = metadata.socket_addr {
            return addr;
        }

        let mut hasher = DefaultHasher::new();
        metadata.hash(&mut hasher);
        hasher.write_u64(SYNTHETIC_ADDR_COUNTER.fetch_add(1, Ordering::Relaxed));
        let hash = hasher.finish();

        let mut octets = [0u8; 16];
        octets[0] = 0xfd;
        octets[1..9].copy_from_slice(&hash.to_be_bytes());

        let port = (((hash >> 16) as u16) | 0x8000).max(1025);
        SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), port)
    }
}

#[tonic::async_trait]
impl ProtoP2p for ConnectionHandler {
    type MessageStreamStream = Pin<Box<dyn futures::Stream<Item = Result<KaspadMessage, TonicStatus>> + Send + 'static>>;

    /// Handle the new arriving **server** connections
    async fn message_stream(
        &self,
        request: Request<Streaming<KaspadMessage>>,
    ) -> Result<Response<Self::MessageStreamStream>, TonicStatus> {
        let mut socket_addr = request.remote_addr();
        let mut libp2p_metadata: Option<crate::core::connection_info::Libp2pConnectInfo> = None;

        #[cfg(feature = "libp2p-bridge")]
        if let Some(connect_info) = request.extensions().get::<BridgeLibp2pInfo>() {
            if socket_addr.is_none() {
                socket_addr = connect_info.synthesized_socket;
            }

            libp2p_metadata = Some(crate::core::connection_info::Libp2pConnectInfo::with_address(
                connect_info.peer_id.to_string(),
                connect_info.remote_multiaddr.as_ref().map(|addr| addr.to_string()),
                connect_info.relay_used,
            ));
        }

        let Some(remote_address) = socket_addr else {
            return Err(TonicStatus::new(tonic::Code::InvalidArgument, "Incoming connection opening request has no remote address"));
        };

        let metadata = ConnectionMetadata::new(Some(remote_address), libp2p_metadata);

        if let Some(summary) = metadata.summary() {
            debug!("P2P, accepting incoming libp2p stream: {}", summary);
        } else {
            debug!("P2P, accepting incoming stream from {}", remote_address);
        }

        // Build the in/out pipes
        let (outgoing_route, outgoing_receiver) = mpsc_channel(Self::outgoing_network_channel_size());
        let incoming_stream = request.into_inner();

        // Build the router object
        let router =
            Router::new(remote_address, Some(metadata), false, self.hub_sender.clone(), incoming_stream, outgoing_route).await;

        // Notify the central Hub about the new peer
        self.hub_sender.send(HubEvent::NewPeer(router)).await.expect("hub receiver should never drop before senders");

        // Give tonic a receiver stream (messages sent to it will be forwarded to the network peer)
        Ok(Response::new(Box::pin(ReceiverStream::new(outgoing_receiver).map(Ok)) as Self::MessageStreamStream))
    }
}

struct StreamConnector<S> {
    stream: Option<TokioIo<S>>,
}

impl<S> StreamConnector<S> {
    fn new(stream: S) -> Self {
        Self { stream: Some(TokioIo::new(stream)) }
    }
}

impl<S> tower::Service<Uri> for StreamConnector<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Response = TokioIo<S>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<TokioIo<S>, std::io::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), std::io::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Uri) -> Self::Future {
        let stream = self.stream.take().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "libp2p connector reused"));
        Box::pin(async move { stream })
    }
}

fn configure_libp2p_server(builder: TonicServer) -> TonicServer {
    // Match yamux buffers (32 MiB) while keeping connection responsive
    builder
        .max_frame_size(Some(LIBP2P_HTTP2_MAX_FRAME_SIZE))
        .initial_stream_window_size(Some(LIBP2P_HTTP2_STREAM_WINDOW)) // 8 MiB stream window
        .initial_connection_window_size(Some(LIBP2P_HTTP2_CONNECTION_WINDOW)) // 16 MiB connection window
        .http2_keepalive_interval(Some(Duration::from_secs(30)))
        .http2_keepalive_timeout(Some(Duration::from_secs(10)))
        .http2_max_header_list_size(Some(LIBP2P_HTTP2_MAX_HEADER_LIST_SIZE))
        .http2_adaptive_window(Some(false))
}

type Libp2pGrpcService = Libp2pSendRequest;

struct Libp2pSendRequest {
    inner: hyper::client::conn::http2::SendRequest<BoxBody>,
}

impl From<hyper::client::conn::http2::SendRequest<BoxBody>> for Libp2pSendRequest {
    fn from(inner: hyper::client::conn::http2::SendRequest<BoxBody>) -> Self {
        Self { inner }
    }
}

impl tower::Service<http::Request<BoxBody>> for Libp2pSendRequest {
    type Response = http::Response<BoxBody>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<BoxBody>) -> Self::Future {
        debug!("libp2p gRPC send request: {}", req.uri());
        let fut = self.inner.send_request(req);
        Box::pin(async move {
            match fut.await {
                Ok(res) => Ok(res.map(tonic::body::boxed)),
                Err(err) => {
                    debug!("libp2p gRPC request failed: {err:?}");
                    Err(err)
                }
            }
        })
    }
}

async fn build_libp2p_channel<S>(
    mut connector: StreamConnector<S>,
    connect_timeout: Duration,
) -> Result<Libp2pGrpcService, std::io::Error>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    poll_fn(|cx| connector.poll_ready(cx)).await?;
    let stream = connector.call(Uri::from_static("http://kaspa.libp2p")).await?;

    let mut builder = hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new());
    // Match yamux buffers (32 MiB) while keeping connection responsive
    builder
        .initial_stream_window_size(LIBP2P_HTTP2_STREAM_WINDOW) // 8 MiB stream window
        .initial_connection_window_size(LIBP2P_HTTP2_CONNECTION_WINDOW) // 16 MiB connection window
        .keep_alive_interval(Some(Duration::from_secs(30)))
        .keep_alive_timeout(Duration::from_secs(10))
        .keep_alive_while_idle(true)
        .adaptive_window(false)
        .max_frame_size(LIBP2P_HTTP2_MAX_FRAME_SIZE)
        .max_header_list_size(LIBP2P_HTTP2_MAX_HEADER_LIST_SIZE);
    builder.timer(hyper_util::rt::TokioTimer::new());

    debug!(
        "libp2p HTTP/2 client: max_frame={} bytes, stream_window={} bytes, conn_window={} bytes",
        LIBP2P_HTTP2_MAX_FRAME_SIZE, LIBP2P_HTTP2_STREAM_WINDOW, LIBP2P_HTTP2_CONNECTION_WINDOW
    );

    let handshake = tokio::time::timeout(connect_timeout, builder.handshake(stream))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "libp2p grpc handshake timed out"))?;
    let (send_request, connection) = handshake.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;

    tokio::spawn(async move {
        if let Err(err) = connection.await {
            debug!("libp2p h2 connection error: {:?}", err);
        }
    });

    Ok(Libp2pSendRequest::from(send_request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        adaptor::Adaptor,
        connection_info::{ConnectionMetadata, Libp2pConnectInfo},
        hub::Hub,
    };
    use kaspa_utils::networking::PeerId;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::io::duplex;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;
    use uuid::Uuid;

    use tonic::transport::server::Connected;

    #[derive(Clone)]
    struct TestInitializer;

    #[tonic::async_trait]
    impl ConnectionInitializer for TestInitializer {
        async fn initialize_connection(&self, router: Arc<Router>) -> Result<(), ProtocolError> {
            router.set_identity(PeerId::new(Uuid::new_v4()));
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_with_stream_establishes_router() {
        let server_initializer = Arc::new(TestInitializer);
        let server_counters = Arc::new(TowerConnectionCounters::default());
        let server_hub = Hub::new();
        let (server_tx, server_rx) = mpsc::channel(Adaptor::hub_channel_size());
        server_hub.clone().start_event_loop(server_rx, server_initializer.clone());
        let server_handler = ConnectionHandler::new(server_tx, server_initializer.clone(), server_counters);

        let (client_half, server_half) = duplex(8 * 1024);

        let (incoming_tx, incoming_rx) = mpsc::channel(1);
        let remote_addr = SocketAddr::from(([127, 0, 0, 1], 4000));
        incoming_tx.send(TestServerIo::new(server_half, remote_addr)).await.expect("send server stream");
        drop(incoming_tx);
        let incoming_stream = tokio_stream::StreamExt::map(ReceiverStream::new(incoming_rx), |io| Ok::<_, std::io::Error>(io));

        let server_task = tokio::spawn(async move {
            configure_libp2p_server(TonicServer::builder())
                .add_service(
                    ProtoP2pServer::new(server_handler)
                        .accept_compressed(tonic::codec::CompressionEncoding::Gzip)
                        .send_compressed(tonic::codec::CompressionEncoding::Gzip)
                        .max_decoding_message_size(P2P_MAX_MESSAGE_SIZE),
                )
                .serve_with_incoming(incoming_stream)
                .await
        });

        let client_initializer = Arc::new(TestInitializer);
        let client_counters = Arc::new(TowerConnectionCounters::default());
        let client_hub = Hub::new();
        let adaptor = Adaptor::client_only(client_hub, client_initializer, client_counters);

        let expected_info =
            Libp2pConnectInfo::with_address(Uuid::new_v4().to_string(), Some("/ip4/192.0.2.1/tcp/12345".to_string()), false);
        let metadata = ConnectionMetadata::new(Some(SocketAddr::from(([192, 0, 2, 1], 12345))), Some(expected_info.clone()));

        let peer_key = adaptor.connect_peer_with_stream(client_half, metadata.clone()).await.expect("connect");
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(adaptor.active_peers_len(), 1);

        let peers = adaptor.active_peers();
        assert_eq!(peers.len(), 1);
        let peer = &peers[0];
        assert_eq!(peer.key(), peer_key);
        let stored = peer.connection_metadata().expect("metadata");
        assert_eq!(stored.socket_addr, metadata.socket_addr);
        let stored_info = stored.libp2p.as_ref().expect("libp2p info");
        assert_eq!(stored_info, metadata.libp2p.as_ref().unwrap());

        adaptor.close().await;
        server_hub.terminate_all_peers().await;
        server_task.abort();
        let _ = server_task.await;
    }

    struct TestServerIo<T> {
        inner: T,
        addr: SocketAddr,
    }

    impl<T> TestServerIo<T> {
        fn new(stream: T, addr: SocketAddr) -> Self {
            Self { inner: stream, addr }
        }
    }

    impl<T> Connected for TestServerIo<T>
    where
        T: AsyncRead + AsyncWrite + Unpin,
    {
        type ConnectInfo = tonic::transport::server::TcpConnectInfo;

        fn connect_info(&self) -> Self::ConnectInfo {
            tonic::transport::server::TcpConnectInfo { local_addr: None, remote_addr: Some(self.addr) }
        }
    }

    impl<T> std::ops::Deref for TestServerIo<T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl<T> std::ops::DerefMut for TestServerIo<T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }

    impl<T> AsyncRead for TestServerIo<T>
    where
        T: AsyncRead + Unpin,
    {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl<T> AsyncWrite for TestServerIo<T>
    where
        T: AsyncWrite + Unpin,
    {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }
}
