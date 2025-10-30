# TCP Hole Punch PoC – Final Report

## Background & Objective

Kaspa’s P2P layer traditionally requires at least one publicly reachable node per connection. Michael asked for a proof-of-concept that demonstrates two **fully private** Kaspa peers establishing and maintaining the Kaspa gRPC flows (Version → Verack → Ready) by coordinating through a public relay and punching through both NATs via libp2p’s DCUtR protocol.

Success criteria:

1. Wrap libp2p substreams so that Kaspa’s tonic-based adaptor can communicate over them.
2. Use a libp2p relay only for coordination; once the circuit is punched, traffic must flow directly between the private peers.
3. Demonstrate the full Kaspa handshake between private peers located behind different NATs, with reproducible logs and documentation.

## Implementation Overview

### Bridge & Stream Wrapping
- Built a dedicated bridge crate (`tcp-hole-punch/bridge`) that spawns a libp2p swarm, handles DCUtR, and exposes inbound/outbound substreams.
- `Libp2pStream` wraps the libp2p substream via `tokio_util::compat` + `hyper_util::rt::TokioIo`, implementing both Tokio’s `AsyncRead/Write` and tonic’s `Connected` traits.
- Stream metadata (`Libp2pConnectInfo`) includes the peer ID, any known multiaddr, whether a relay was used, and an optional synthesized `SocketAddr`.

### Kaspa P2P Integration
- `ConnectionHandler::connect_with_stream` and the new `serve_with_incoming` path accept libp2p streams, build a tonic HTTP/2 channel with custom window/frame sizing, and feed the existing router logic.
- The adaptor (`protocol/p2p/src/core/adaptor.rs`) now exposes `serve_incoming_streams` so the binaries can hand in the bridge’s incoming queue directly.
- When tonic reports no remote socket on inbound requests (expected for libp2p), the handler synthesizes a deterministic IPv6 address derived from the libp2p metadata and logs the fact, ensuring router bookkeeping and metrics remain consistent.

### HTTP/2 & Transport Tuning
- Enlarged hyper/h2 window and frame limits (1 MiB frame, 8 MiB stream window, 16 MiB connection window) to match the yamux buffers and eliminate the `FRAME_SIZE_ERROR` failures we hit early on.
- Added diagnostic logging around handshake settings and gRPC requests to aid troubleshooting.

### Binaries & Environment Wiring
- `kaspa_p2p_server` and `kaspa_p2p_client` now read environment variables:
  - `LIBP2P_LISTEN_MULTIADDRS` (server) may include both local sockets (e.g. `/ip4/0.0.0.0/tcp/4012`) and relay circuits (`/ip4/<RELAY_IP>/tcp/4011/p2p/<relay-peer>/p2p-circuit`). The server automatically requests a reservation via `LIBP2P_RELAY_MULTIADDRS` and logs the resulting `/p2p-circuit/p2p/<server>` listener.
  - `LIBP2P_REMOTE_PEER_ID` and `LIBP2P_REMOTE_MULTIADDRS` (client) drive the dialer and allow multiple addresses to be tried in order.
- Both binaries keep tonic compression/flow settings synchronized to avoid head-of-line blocking or mismatched frame sizes.

## Investigation & Troubleshooting Timeline

1. **Initial bridge integration (local)**  
   - Early runs failed with `GoAway(FRAME_SIZE_ERROR)` immediately after SETTINGS exchange. Root cause: tonic’s default (16 KiB) max frame clashing with Kaspa’s larger messages. Fixed by raising max frame and window sizes and matching them across client and server.
   - Added logging to confirm settings and gRPC calls; ensured `configure_libp2p_server` is applied everywhere (tests, server binary, serve_with_incoming path).

2. **Metadata & Server Path**  
   - Discovered inbound libp2p stream path bypassed the custom tonic builder, leading to mismatched settings. Introduced `serve_with_incoming` helper and pushed it through the adaptor.
   - Needed to surface libp2p metadata in `message_stream`. Initially, tonic’s `Request` lacked a remote address, so router creation failed. Solution: synthesize a socket from the libp2p connect info and log the synthesized address.

3. **Yamux Buffer Constraints**  
   - Yamux default receive windows (256 KiB) were insufficient for Kaspa’s compressed frames. Explicitly configured 32 MiB buffers and turned off adaptive windowing to keep behaviour predictable.

4. **Remote Run Hurdles**  
   - Early remote attempts failed with “Relay has no reservation for destination.” Diagnosis: the server was trimming multiaddr transports before issuing `SwarmCommand::ListenOn`, so the relay never recognised our reservation. Fix: retain the full transport portion when requesting reservations and log success/failure.
   - Client initially fell back to HTTP/2 over TCP because the rustup toolchain wasn’t installed on the Vultr box (cross-compiled binary mismatch). Resolved by installing rustup/cargo on the VPS and building locally.
   - During the first real run, the client launched before the server logged the reservation; waiting a second resolved the issue (DCUtR requires the reservation to be in place before dial).

5. **Sanitised Logging**  
   - All final logs replace real IPs with `<HOME_IP>`, `<RELAY_IP>`, and `<CLIENT_VPS_IP>` to keep the repository clean while still documenting the flow.

## PoC Demonstration Summary

- **Topology**: local workstation (Kaspa server, private behind NAT) + remote Vultr VPS (Kaspa client, private behind VPS NAT) + remote relay (public).  
- **Runbook**: documented in `tcp-hole-punch/logs/phase4-remote-success.md`. Includes required env vars, commands, and the log checkpoints to verify success.
- **Key evidence**:
  - `phase4-client-session.log:253` – “P2P, Client connected via libp2p … relay=true”.
  - `phase4-server-session.log:333-360` – Server sends Version, receives Version/Ready, acknowledges inbound peer with synthesized libp2p address.
  - `phase4-relay-session.log` – Reservation acceptance and DCUtR coordination between the two private peers.

## Outcomes & Decisions

- **Feasibility**: Two private Kaspa nodes can maintain an ongoing P2P connection by coordinating through libp2p DCUtR; no fallback relay proxying is required after the punch.
- **Code Changes**:  
  - Stream handling and adaptor paths now seamlessly accept libp2p substreams.
  - HTTP/2 settings tuned for bridging over yamux/libp2p.
  - Binaries flexible through environment variables; no code changes needed for different topologies.
- **Operational Learnings**:
  - Relay reservation must complete before dial; logs make this visible.
  - Synthesised metadata is necessary for routers but should be clearly logged to avoid confusion.

## Phase 5 Hardening Highlights

- **Deterministic addressing & duplicate protection** – `ConnectionHandler::synthetic_socket_addr` now hashes stable metadata; the paired regressions (`synthetic_socket_addr_is_stable`, `duplicate_libp2p_connection_is_rejected`) ensure repeated libp2p arrivals reuse the same endpoint while still raising `PeerAlreadyExists` on duplicates.
- **Relay quota bookkeeping** – `PeerBook` tracks pending vs. confirmed relay addresses so failed dials release reservations; covered by `relay_limits_are_enforced` and `relay_limit_recovers_after_failed_dial`.
- **Metadata & metrics parity** – `PeerBook::info_for` feeds libp2p multiaddrs/relay flags into the adaptor, and `libp2p_counters_capture_traffic` validates byte counters for bridged traffic.
- **Inbound backpressure** – The swarm now awaits the incoming queue; the new stress test `inbound_queue_handles_many_concurrent_streams` runs 40 simultaneous substreams without drops while tonic consumes them.
- **Hardened rehearsal** – `logs/phase5-hardened-run.md` together with `logs/phase5-{relay,server,client}-session.log` documents the end-to-end replay using the updated bridge.

## Phase 6 Validation Snapshot

- Re-ran the mixed-NAT scenario against the production relay: the server reserved `/p2p-circuit` on `<RELAY_IP>` (`phase6-server-session.log:188`), the client established the libp2p circuit and Kaspa handshake (`phase6-client-session.log:251`), and the relay confirmed `CircuitReqAccepted` for the hardened peers (`phase6-relay-session.log:295`).
- `logs/phase6-remote-validation.md` captures the runbook (tmux sessions, env vars, and key log callouts) so future rollouts can replay the validation verbatim.

## Follow-up Opportunities (Post-PoC)

These are not required for the proof-of-concept but will matter for production hardening:

1. **Transport policy** – define connection limits, relay quotas, default retry cadence, optional QUIC toggles, and operator-facing configuration.
2. **Metrics & observability** – instrument reservation/dial success rates, punch latency, and relay load; surface metrics and dashboards.
3. **Automation** – add an integration test harness (containers or CI) that spins up relay/server/client and asserts the Kaspa handshake over libp2p.
4. **Peer identity persistence** – align PeerId ↔ PeerKey storage so the libp2p identity is durable across restarts.

## Artifacts

| Artifact | Description |
| --- | --- |
| `protocol/p2p/src/core/connection_handler.rs` | Stream integration, synthesized metadata handling, HTTP/2 tuning. |
| `protocol/p2p/src/bin/{server,client}.rs` | libp2p env wiring, reservation handling, stream hand-off. |
| `tcp-hole-punch/logs/phase4-remote-success.md` | Runbook + timeline for the remote PoC. |
| `tcp-hole-punch/logs/phase4-{relay,server,client}-session.log` | Captured logs (sanitised) from the successful mixed-NAT run. |
| `tcp-hole-punch/logs/phase5-hardened-run.md` | Phase 5 hardened replay summary with links to the new relay/server/client logs. |
| `tcp-hole-punch/bridge/tests/integration.rs` | Integration suite covering libp2p dial, tonic server acceptance, relay quota, counter instrumentation, and the >32 stream stress test. |
| `tcp-hole-punch/logs/phase6-remote-validation.md` | Phase 6 remote rehearsal notes with pointers to the latest server/client/relay logs. |
| `tcp-hole-punch/logs/phase6-{server,client,relay}-session.log` | Logs from the Phase 6 validation run (sanitised). |
| `tcp-hole-punch/design/architecture.md` | Updated architecture and operational notes. |
| `tcp-hole-punch/plan.md` | Phase tracking, now with post-Phase 4 follow-up tasks. |
| `tcp-hole-punch/final-verification.md` | One-page checklist linking the decisive evidence, tests, and reproduction steps. |
