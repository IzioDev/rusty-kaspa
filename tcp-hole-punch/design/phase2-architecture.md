# Phase 2 – libp2p ⇄ tonic Bridge: Architecture Notes

## Objectives
- Accept libp2p DCUtR connections and surface them to Kaspa's existing tonic-based P2P adaptor without rewriting flow logic.
- Support both inbound and outbound Kaspa peers while keeping relay/DCUtR coordination inside libp2p.
- Preserve Kaspa metrics/logging expectations (peer identity, socket metadata) and expose fallbacks when hole punching fails.

## Relevant References
- `research/gpt.md` – sections *Focus 1* and *Focus 2* outline tonic APIs (`Endpoint::connect_with_connector`, `Server::serve_with_incoming`) and libp2p stream extraction strategies.
- `research/gemini.md` – “Bridging libp2p Streams to the Tonic Runtime” and “The libp2p Swarm as a Managed Service” describe wrapper traits, `Connected` metadata, and Swarm actor patterns.
- libp2p crate examples (not vendored here): `examples/dcutr` for DCUtR flow; `examples/relay-server` for relay coordination.

## Bridge Components
1. **Swarm Service (Actor)**
   - Owns a libp2p `Swarm<Behaviour>` configured with TCP + Noise + Yamux together with the identify, ping and `libp2p-stream` behaviours. (Relay/DCUtR remain future work once Kaspa’s adaptor is wired in.)
   - Runs in a dedicated task; communicates with application via async channels (`mpsc` for commands, `oneshot` for responses).
   - Responsibilities:
     - Dial peers when instructed (`Dial` command) using the multiaddrs supplied over the channel (the swarm re-appends the `/p2p/<peer>` suffix before dialling).
     - Listen for inbound connections and surface negotiated substreams via a bounded `mpsc` queue.
     - Manage clean shutdowns and emit tracing around clone/drop counts so leaks are easy to spot in integration tests.

2. **Stream Wrapper**
   - Wraps a `libp2p_stream::Stream` with `tokio_util::compat::Compat` and `hyper_util::rt::tokio::WithHyperIo` so the same type implements both Tokio’s `AsyncRead/AsyncWrite` and Hyper’s `rt::Read/Write` traits.
   - Implements tonic's `Connected` trait to provide metadata:
     ```rust
     impl Connected for Libp2pStreamWrapper {
         type ConnectInfo = Libp2pConnectInfo;
         fn connect_info(&self) -> Self::ConnectInfo { ... }
     }
     ```
   - `Libp2pConnectInfo` stores `PeerId`, `Multiaddr`, optional synthesized `SocketAddr` (fallback `HOMEIP:0` style if unknown), relay usage flag, observed addresses.

3. **Tonic Integration Points**
   - **Inbound**: feed wrappers into `Server::serve_with_incoming` by exposing an `mpsc::Receiver<Result<Libp2pStreamWrapper, _>>` as the incoming stream.
   - **Outbound**: pass wrappers via `Endpoint::connect_with_connector` by implementing `tower::Service<Uri, Response = Libp2pStreamWrapper>`.
   - Provide helper functions:
     ```rust
     async fn accept_stream(&self) -> Result<Libp2pStreamWrapper>;
     async fn dial_stream(&self, peer_id: PeerId) -> Result<Libp2pStreamWrapper>;
     ```

## Data Flow Overview
1. Kaspa adaptor requests connection (`dial_via_libp2p`):
   - Send `DialPeer { peer_id, relays }` command to Swarm actor.
   - Swarm performs relay reservation + DCUtR; upon success, returns `Libp2pStreamWrapper` via oneshot.
   - Kaspa uses tonic connector to wrap stream into `Router` handshake flow.
2. Libp2p receives inbound connection:
   - Swarm actor upgrades circuit to direct stream and pushes wrapper into incoming queue.
   - Kaspa server side consumes queue through `serve_with_incoming` and proceeds with handshake.
3. Failure cases:
   - If hole punch fails, wrapper indicates relay fallback and includes metrics for decision logic (optionally keep the relayed stream or retry later).

## Usage Guide

1. **Spawn a swarm**
   ```rust
   let keypair = libp2p::identity::Keypair::generate_ed25519();
   let mut swarm_handle = spawn_swarm(keypair)?;
   ```
   Each call owns the background task. Use `SwarmHandle::shutdown()` to terminate gracefully.

2. **Accept inbound connections**
   ```rust
   let incoming = incoming_from_handle(&mut swarm_handle)?; // can only be called once
   Server::builder()
       .add_service(MyService::new())
       .serve_with_incoming(incoming)
       .await?;
   ```

3. **Dial outbound connections**
   ```rust
   let connector = Libp2pConnector::new(swarm_handle.command_tx());
   let uri = format!("libp2p://{peer_id}?addr={}", urlencoding::encode("/ip4/203.0.113.5/tcp/16111/p2p/…"));
   let channel = Endpoint::from_shared(uri)?.connect_with_connector(connector).await?;
   ```
   `parse_target` sanitises the multiaddr and the swarm reattaches the `/p2p/<peer>` suffix before dialling. If the URI is malformed the connector returns `BridgeError::DialFailed` immediately.

4. **Inspect metadata**
   Every accepted stream implements `Connected` and exposes `Libp2pConnectInfo` (peer id, first observed multiaddr, relay flag, synthesised socket address). This can be threaded into Kaspa’s logging/metrics without additional plumbing.

5. **Bridge directly into the Kaspa adaptor**
   ```rust
   use hole_punch_bridge::stream::Libp2pStreamHandle;

   fn metadata(info: &hole_punch_bridge::stream::Libp2pConnectInfo) -> kaspa_p2p_lib::ConnectionMetadata {
       let libp2p_info = kaspa_p2p_lib::Libp2pConnectInfo::with_address(
           info.peer_id.to_string(),
           info.remote_multiaddr.as_ref().map(|addr| addr.to_string()),
           info.relay_used,
       );
       kaspa_p2p_lib::ConnectionMetadata::new(info.synthesized_socket, Some(libp2p_info))
   }

   async fn connect_stream(adaptor: &kaspa_p2p_lib::Adaptor, handle: Libp2pStreamHandle) -> kaspa_p2p_lib::ConnectionResult {
       let meta = metadata(handle.info());
       adaptor.connect_peer_with_stream(handle.into_stream(), meta).await
   }
   ```
   `Libp2pStreamHandle` carries the metadata required for logging/metrics; the adaptor can be wired using the same pattern that the unit test `kaspa_p2p_lib::core::connection_handler::tests::connect_with_stream_establishes_router` employs for an in-memory duplex stream.

6. **Tune transport policy via `SwarmConfig`**
   ```rust
   use hole_punch_bridge::{spawn_swarm_with_config, SwarmConfig, RelayConfig, TransportConfig};

   let mut config = SwarmConfig::default();
   config.transport.enable_quic = true;
   config.relay = RelayConfig { enabled: true, max_reservations: 4, max_circuits_per_peer: 2 };
   let swarm = spawn_swarm_with_config(local_key, config)?;
   ```
   The configuration toggles QUIC alongside TCP and enforces conservative relay limits. DCUtR hole punching is enabled by default but can be disabled through `config.hole_punch.enable_dcutr`.

   The demo binaries (`protocol/p2p/src/bin/{server,client}.rs`) wire this behaviour under the `libp2p-bridge` feature. Set `LIBP2P_LISTEN_MULTIADDR=/ip4/0.0.0.0/tcp/16000` on the server to accept inbound streams, and supply `LIBP2P_REMOTE_MULTIADDR`/`LIBP2P_REMOTE_PEER_ID` on the client to dial via libp2p.

## Test Coverage
- `libp2p_dial_yields_stream` verifies two in-process swarms can open a substream, exchange bytes and shut down deterministically.
- `tonic_server_accepts_libp2p_stream` feeds the same stream through a tonic Echo service using `serve_with_incoming` + `Libp2pConnector`.
- Additional tests cover URI failure modes (`missing addr`, malformed multiaddr, missing `/p2p/`, mismatched peer`).

Core tests run in ~20 ms via `cargo test --manifest-path tcp-hole-punch/bridge/Cargo.toml`.

## Open Questions
- Extend metrics once Phase 3 threads `Libp2pConnectInfo` into Kaspa’s adaptor (e.g., relay/DCUtR counters).
- Monitor relay/DCUtR behaviour under real-world load and adjust defaults if reservations or retries need tightening.

## Next Steps
1. Extend Kaspa’s adaptor/router to consume `Libp2pStream` directly (Phase 3).
2. Record relay/DCUtR metrics once the adaptor exposes them downstream.
3. Wire the bridge into the Kaspa client/server binaries to smoke-test a punched Kaspa session (Phase 4).
