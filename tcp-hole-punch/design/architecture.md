# Libp2p ⇄ Tonic Bridge Architecture Notes

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
   - Owns a libp2p `Swarm<Behaviour>` configured with TCP + Noise + Yamux together with the identify, ping and `libp2p-stream` behaviours. DCUtR is enabled by default and can be toggled through `SwarmConfig::hole_punch`.
   - Runs in a dedicated task; communicates with application via async channels (`mpsc` for commands, `oneshot` for responses). The command/incoming channels are sized to 128 items so bursty dial attempts cannot starve the swarm.
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

## Default Configuration Overview
- **Transport stack**: TCP + Noise + Yamux with fixed 32 MiB receive/max buffers to match tonic’s enlarged HTTP/2 windows. QUIC support exists but is disabled unless `transport.enable_quic` is toggled.
- **Relay/DCUtR**: Relay client behaviour is enabled by default (`RelayConfig { enabled: true, max_reservations: 8, max_circuits_per_peer: 4 }`) and DCUtR hole punching stays on (`hole_punch.enable_dcutr = true`) so private peers attempt direct connectivity first.
- **Command & inbound queues**: Both `SwarmCommand` and incoming stream `mpsc` channels use 128-slot buffers, absorbing the 40-stream stress scenario and real-world burst dial attempts without backpressure.
- **HTTP/2 tuning**: `ConnectionHandler` applies a 1 MiB max frame, 8 MiB stream window, and 16 MiB connection window whenever it builds tonic channels over libp2p, removing the earlier `FRAME_SIZE_ERROR` failures.
- **Metadata synthesis**: When tonic lacks a remote socket, the handler synthesizes a deterministic IPv6 address from libp2p metadata and records the peer ID, multiaddr, and relay flag for downstream metrics/logging.

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

The demo binaries (`protocol/p2p/src/bin/{server,client}.rs`) wire this behaviour under the `libp2p-bridge` feature. Set `LIBP2P_LISTEN_MULTIADDRS=/ip4/0.0.0.0/tcp/16000;/ip4/<relay>/tcp/<port>/p2p/<relay-peer>/p2p-circuit` on the server to register local sockets and pre-reserve relay circuits (comma/semicolon are accepted separators). Optional `LIBP2P_RELAY_MULTIADDR(S)` entries keep backwards compatibility with the one-shot dial helper. The client accepts `LIBP2P_REMOTE_MULTIADDRS` (same separators, supports relay + direct addresses) together with `LIBP2P_REMOTE_PEER_ID` to drive the dialer; it tries each provided multiaddr until one succeeds.

## Operational Notes
- Keep both the local listen socket and the relay circuit in `LIBP2P_LISTEN_MULTIADDRS`. The server issues a follow-up reservation via `LIBP2P_RELAY_MULTIADDRS`; expect to see `Relay reservation requested` followed by a full `/p2p-circuit/p2p/<server>` listener before dialling from the client.
- The tonic request that surfaces an inbound libp2p stream often lacks a socket address. The connection handler now synthesizes one (based on the libp2p metadata) and records it in the log as `addr=/p2p/<peer>`, so downstream consumers still receive a stable identifier.
- Give the reservation a moment to settle before starting the remote client. In the Phase 4 VPS run the server was up for ~1 s before the Vultr dial succeeded; starting the client too early still triggers `Relay has no reservation for destination`.
- Reference logs for the mixed-NAT rehearsal live under `logs/phase4-{relay,server,client}-session.log` and show the complete reservation/DCUtR/handshake timeline.
- The Phase 5 hardened replay (`logs/phase5-hardened-run.md`) captures the deterministic synthetic socket addresses, relay metadata, and byte counters after the latest fixes.
- The Phase 6 remote validation (`logs/phase6-remote-validation.md`) reconfirms the hardened path against the real relay; use `logs/phase6-{server,client,relay}-session.log` when troubleshooting future rollouts.

## Test Coverage
- Core bridge smoke tests: `libp2p_dial_yields_stream` verifies two in-process swarms can open a substream, exchange bytes and shut down deterministically, while `tonic_server_accepts_libp2p_stream` feeds the stream through tonic using `serve_with_incoming`.
- Hardening regressions: `synthetic_socket_addr_is_stable`, `duplicate_libp2p_connection_is_rejected`, `relay_limits_are_enforced`, `relay_limit_recovers_after_failed_dial`, `libp2p_counters_capture_traffic`, and the new stress test `inbound_queue_handles_many_concurrent_streams` exercise the Phase 5 fixes.
- URI failure modes remain covered (`missing addr`, malformed multiaddr, missing `/p2p/`, mismatched peer`).

Core bridge smoke tests complete in ~20 ms; the full suite (including `inbound_queue_handles_many_concurrent_streams`) now finishes in under a second on a 6‑core laptop after widening the swarm command/incoming buffers.

## Future Enhancements (Optional)
- Integrate the libp2p byte counters into the wider Kaspa telemetry pipeline (dashboards/alerts).
- Monitor relay/DCUtR behaviour under real-world load and adjust defaults if reservations or retries need tightening.
- Extend automation (nightly integration harness, operator runbooks) beyond the PoC if/when the bridge is productised.
