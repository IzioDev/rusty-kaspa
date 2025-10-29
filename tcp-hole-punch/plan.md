# TCP Hole Punch Integration Plan

## Progress Snapshot
- ✅ **Phase 1 – Baseline hole punching**  
  - Successful direct punch: see `TEST_SCENARIOS.md` (VPS dialer scenario) and raw logs in `logs/dialer_vps_scenario.log`.  
  - Negative case (expected relay fallback): documented in `logs/hotspot-scenario-summary.md`.
- ✅ **Phase 2 – Build the libp2p ⇄ tonic bridge**
- 🔄 **Phase 3 – Extend the Kaspa adaptor/router**.
- ✅ **Phase 4 – Two-private-peer PoC**.

## Phase 1 – Reproduce Baseline Hole Punching
- ✅ Relay, listener, and dialer binaries exercised.  
  - Positive case (VPS dialer ↔ home listener) produced a direct TCP channel – evidence in `logs/dialer_vps_scenario.log` and summary in `TEST_SCENARIOS.md`.  
  - Negative case (hotspot dialer) remained on the relay, captured in `logs/hotspot-scenario-summary.md` for reference.
- Timing, multiaddrs, and Swarm events recorded to guide Phase 2 expectations (`logs/phase1-summary.md`).

## Phase 2 – Build the libp2p ⇄ tonic Bridge
- [x] Document bridge usage and swarm lifecycle in `design/phase2-architecture.md`.
- [x] Implement swarm actor, dual-stack stream wrapper, and tonic integration helper APIs under `bridge/src/`.
- [x] Provide deterministic tests: `libp2p_dial_yields_stream`, `tonic_server_accepts_libp2p_stream`, and URI error coverage.
- [x] Ensure the test suite (`cargo test --manifest-path tcp-hole-punch/bridge/Cargo.toml`) runs cleanly in <50 ms.
- [x] Prepare Phase 3 handoff notes (adaptor integration, metrics, relay/DCUtR re-enable plan).

## Phase 3 – Extend the Kaspa Adaptor/Router
- [x] Add an adaptor entrypoint that accepts an owned `AsyncRead + AsyncWrite + Send + 'static` stream (in addition to the existing URI dial path) and plumb it through `ConnectionHandler::connect`/`Router::new`.
- [x] Thread `Libp2pConnectInfo` (peer id, multiaddr, relay flag) into the router so logging/metrics can surface libp2p context.
- [x] Decide when to re-enable relay/DCUtR/QUIC behaviours and expose the necessary knobs (connection limits, retry cadence).
- [x] Update adaptor/unit tests to cover the new stream-based handshake path (see `kaspa_p2p_lib::core::connection_handler::tests::connect_with_stream_establishes_router`).

## Phase 4 – Deliver the Two-Private-Peer PoC
- [x] Modify `protocol/p2p/src/bin/{server.rs,client.rs}` to source their transport from the libp2p bridge while keeping the Kaspa handshake and flow registration intact.
- [x] libp2p env wiring: server accepts `LIBP2P_LISTEN_MULTIADDRS` (e.g., `/ip4/0.0.0.0/tcp/16000;/ip4/<relay>/tcp/<port>/p2p/<relay-peer>/p2p-circuit`) plus optional `LIBP2P_RELAY_MULTIADDR(S)` to issue manual reservations; client accepts `LIBP2P_REMOTE_MULTIADDR(S)` and `LIBP2P_REMOTE_PEER_ID`, attempting each address in order.
- [x] Harden the libp2p handshake channel (HTTP/2 window + frame sizing, keep-alives) so tonic runs reliably over bridged streams (`protocol/p2p/src/core/connection_handler.rs`).
- [x] Script the three-node test (public relay + two NATed Kaspa binaries) to demonstrate sustained Kaspa gossip over a hole-punched TCP connection, recording fallback behaviour (relay persistence, QUIC attempts, retry cadence).  
  _Local rehearsal:_ `logs/local-libp2p-bridge.md` plus `logs/local-relay-session.log`, `logs/local-server-session.log`, and `logs/local-client-session.log`.  
  _Remote success (2025-10-29):_ see `logs/phase4-relay-session.log`, `logs/phase4-server-session.log`, `logs/phase4-client-session.log`, and the summary in `final-report.md` for the coordinating relay ↔ local server ↔ remote client run; note the reservation/handshake timeline and the synthesized inbound addresses now surfaced in the server log.
- [x] Document operational knobs and open policy items: relay inventory, connection/relay limits, PeerId↔PeerKey unification, and peer-store schema updates for storing Multiaddrs/observed addresses.  
  _See_ `design/phase2-architecture.md` (Operational Notes) _and_ `logs/phase4-remote-success.md` for the mixed-NAT runbook.

## Post-Phase 4 – Follow-up Work

1. **Transport Policy & Fallback Strategy**
   - Draft requirements: per-peer relay quota, retry cadence, TCP/QUIC toggles, and UI/CLI exposure.
   - Implement adaptor-level policy (routing decisions, relay vs. direct fallback, connection limits) plus config serialization.
   - Add documentation for operators (new knobs, recommended defaults, troubleshooting playbook).

2. **Metrics & Observability**
   - Instrument handshake path (reservation timing, punch success/failure, relay usage) and expose counters/gauges via existing metrics pipeline.
   - Surface peer metadata (multiaddrs, relay flag, punch latency) to the hub and logging.
   - Produce dashboards/alerts for punch success rate and relay load.

3. **Automation & Testing**
   - Build an integration harness that spins up relay/server/client automatically (local containers or CI environment) and asserts Version/Verack/Ready over libp2p.
   - Add regression tests for metadata synthesis and policy enforcement.
   - Wire the harness into CI (nightly or targeted jobs) with artifacts (sanitized logs) for debugging.

4. **Peer Identity & Persistence**
   - Align PeerId↔PeerKey mapping to ensure libp2p identity persists across restarts.
   - Extend the peer-store schema to remember observed multiaddrs and relay provenance.

## Phase 3 – Status (handover)
- [x] Adopt `Libp2pStream` in the Kaspa adaptor/router
- [x] Thread bridge metadata into logging/metrics
- [x] Re-enable relay/DCUtR behaviours once adaptor path exists
- [x] Extend adaptor tests to cover libp2p streams (run `cargo test -p kaspa-p2p-lib connect_with_stream_establishes_router -- --nocapture`)
- Latest verification (2025-10-28): `cargo check -p kaspa-p2p-lib`, `cargo check -p kaspa-p2p-lib --no-default-features --features libp2p-bridge`, `cargo test -p kaspa-p2p-lib connect_with_stream_establishes_router -- --nocapture`, and `cargo test --manifest-path tcp-hole-punch/bridge/Cargo.toml` all pass; libp2p-enabled demo usage is captured in `design/phase2-architecture.md`.
