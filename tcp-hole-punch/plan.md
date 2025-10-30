# TCP Hole Punch Integration Plan

## Progress Snapshot
- ✅ **Phase 1 – Baseline hole punching**  
  - Successful direct punch: see `research/test-scenarios.md` (VPS dialer scenario) and raw logs in `logs/dialer_vps_scenario.log`.  
  - Negative case (expected relay fallback): documented in `logs/hotspot-scenario-summary.md`.
- ✅ **Phase 2 – Build the libp2p ⇄ tonic bridge**
- ✅ **Phase 3 – Extend the Kaspa adaptor/router**
- ✅ **Phase 4 – Two-private-peer PoC**
- ✅ **Phase 5 – Libp2p hardening**
- ✅ **Phase 6 – Validation & rollout prep**

## Phase 1 – Reproduce Baseline Hole Punching
- ✅ Relay, listener, and dialer binaries exercised.  
  - Positive case (VPS dialer ↔ home listener) produced a direct TCP channel – evidence in `logs/dialer_vps_scenario.log` and summary in `research/test-scenarios.md`.  
  - Negative case (hotspot dialer) remained on the relay, captured in `logs/hotspot-scenario-summary.md` for reference.
- Timing, multiaddrs, and Swarm events recorded to guide Phase 2 expectations (`logs/phase1-summary.md`).

## Phase 2 – Build the libp2p ⇄ tonic Bridge
- [x] Document bridge usage and swarm lifecycle in `design/architecture.md`.
- [x] Implement swarm actor, dual-stack stream wrapper, and tonic integration helper APIs under `bridge/src/`.
- [x] Provide deterministic tests: `libp2p_dial_yields_stream`, `tonic_server_accepts_libp2p_stream`, and URI error coverage.
- [x] Ensure the bridge test suite runs cleanly (full run now completes in <1 s on a 6‑core dev laptop after enlarging swarm command/incoming buffers).
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
  _Remote success (2025-10-29):_ see `logs/phase4-relay-session.log`, `logs/phase4-server-session.log`, `logs/phase4-client-session.log`, and the summary in `final-report.md` for the coordinating relay ↔ local server ↔ remote client run; note the reservation/handshake timeline and the synthesized inbound addresses now surfaced in the server log.
- [x] Document operational knobs and open policy items: relay inventory, connection/relay limits, PeerId↔PeerKey unification, and peer-store schema updates for storing Multiaddrs/observed addresses.  
  _See_ `design/architecture.md` (Operational Notes) _and_ `logs/phase4-remote-success.md` for the mixed-NAT runbook.

## Outstanding Follow-up (Backlog)
These items were intentionally left outside the PoC scope and remain open for production hardening.

1. [ ] **Transport Policy & Fallback Strategy**  
   Draft per-peer relay quotas, retry cadence, TCP/QUIC toggles, and operator-facing configuration; implement adaptor policy + docs.

2. [ ] **Metrics & Observability**  
   Instrument reservation/punch success rates, latency, and relay load; surface dashboards/alerts.

3. [ ] **Automation & Testing**  
   Build an automated harness (containers/CI) that boots relay/server/client, asserts Version→Verack→Ready over libp2p, and publishes sanitized logs.

4. [ ] **Peer Identity & Persistence**  
   Persist libp2p PeerId↔PeerKey mapping and extend the peer-store schema for observed multiaddrs/relay provenance.


## Phase 5 – Libp2p Hardening (Complete)

- [x] **Stabilise synthetic socket addresses**
  - `ConnectionHandler::synthetic_socket_addr` now hashes stable metadata; `synthetic_socket_addr_is_stable` and `duplicate_libp2p_connection_is_rejected` cover determinism and duplicate suppression.
  - Hardened run captured in `logs/phase5-hardened-run.md` + `logs/phase5-server-session.log:376,488` confirms repeated inbound attempts reuse the same synthesized endpoint.

- [x] **Correct relay quota bookkeeping**
  - `PeerBook` tracks pending vs. active relay addresses and releases them on failures.
  - Regression suite exercises quota exhaustion/recovery (`relay_limits_are_enforced`, `relay_limit_recovers_after_failed_dial`).

- [x] **Improve libp2p metadata fidelity**
  - `PeerBook::info_for` propagates the latest dialed multiaddr and relay flag; adaptor and logs surface the context.
  - Phase 5 rehearsal (`logs/phase5-client-session.log:184,330`) shows metadata recorded for metrics/logging.

- [x] **Restore traffic accounting for libp2p channels**
  - `Libp2pSendRequest` is wrapped in byte counters; `libp2p_counters_capture_traffic` asserts TX/RX increment during a libp2p handshake.

- [x] **Handle inbound stream backpressure**
  - Incoming queue uses awaited `send`, and new stress test `inbound_queue_handles_many_concurrent_streams` proves >32 simultaneous streams succeed.
  - Phase 5 rehearsal logs show tonic draining the queue without dropped streams.

## Phase 6 – Validation & Rollout Prep

- [x] **Repeat end-to-end validation**
  - Reproduced the Phase 4 remote run with the hardened bridge; see `logs/phase6-server-session.log`, `logs/phase6-client-session.log`, `logs/phase6-relay-session.log`, and the summary in `logs/phase6-remote-validation.md`.
  - Captured the new behaviour (synthetic IPv6 socket, byte counters) in `final-report.md`.

- [x] **Update documentation and runbooks**
  - Reflected the hardening changes in `design/architecture.md`, `final-report.md`, and added `logs/phase6-remote-validation.md`.
  - Highlighted new metrics and policy knobs for operators.

- [ ] **Decide on merge readiness**
  - Review outstanding items from Phase 6 and Post-Phase 4 policy/automation tracks.
  - Prepare a merge-readiness checklist (open task; document will be added once policy/automation items land).

## Verification Snapshot
- ✅ `cargo test --manifest-path tcp-hole-punch/bridge/Cargo.toml`
- ✅ `cargo test -p kaspa-p2p-lib connect_with_stream_establishes_router -- --nocapture`
- Last run: 2025-10-30 (local 6‑core laptop, sub-second bridge suite after channel buffer enlargement). For environment and runbook context see `design/architecture.md` and `logs/phase6-remote-validation.md`.
