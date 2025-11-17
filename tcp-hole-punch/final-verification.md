# TCP Hole Punch PoC – Verification Checklist

_Last refreshed: 2025-11-13 • Branch `tcp-hole-punch` • Commit `HEAD`_

## 1. Summary
- Two private Kaspa peers (home workstation + Vultr VPS) established the full Version → Verack → Ready handshake over a libp2p DCUtR stream coordinated by a Hetzner relay.
- After the punch, traffic flowed directly peer↔peer; the relay only mediated the connection.
- Sanitised logs and the runbook are published in-repo; tests were rerun locally on 2025-10-30 with all suites green.

## 2. Evidence
| Proof Point | Log Reference |
| --- | --- |
| Relay reservation acknowledged and libp2p streams injected into tonic | `tcp-hole-punch/logs/phase6-server-session.log:177-190` |
| Kaspa server synthesises libp2p address, exchanges Version/Ready, reaches Ready | `tcp-hole-punch/logs/phase6-server-session.log:350-403` |
| Kaspa client reports “Client connected via libp2p … relay=true” | `tcp-hole-punch/logs/phase6-client-session.log:251-259` |
| Hetzner relay confirms DCUtR circuit between the two private peers | `tcp-hole-punch/logs/phase6-relay-session.log:295` |
| Step-by-step reproduction (tmux layout, env vars, rsync, commands) | `tcp-hole-punch/logs/phase6-remote-validation.md` |

All logs are sanitised (`<RELAY_IP>`, `<HOME_IP>`, `<CLIENT_VPS_IP>` placeholders) but otherwise unedited.

## 3. Tests & Tooling
- ✅ `cargo test -p kaspa-addressmanager` – covers the RocksDB migration harness and confirms schema upgrades write version markers exactly once.
- ✅ `cargo test -p kaspa-connectionmanager gossip_to_rotation_end_to_end rotation_recovers_after_unhealthy_relay relay_candidates_with_missing_ports_are_ignored_in_planner -- --nocapture` – exercises the gossip→store→planner flow, unhealthy relay rotation/backoff, and the zero-port guard on relay candidates.
- ✅ `cargo test --manifest-path tcp-hole-punch/bridge/Cargo.toml`
- ✅ `cargo test -p kaspa-p2p-lib connect_with_stream_establishes_router -- --nocapture`
- ✅ `kaspa-cli getlibpstatus`, `kaspa-cli getpeeraddresses --json`, `kaspa-cli getconnectedpeerinfo --json` on bridge/relay nodes to verify role detection and the v9 relay metadata.

All commands were rerun on 2025-11-13 (local 6‑core laptop); log snippets are available on request.

## 4. Relay Metadata Validation
- ✅ After upgrading to protocol v9, `getpeeraddresses --json | jq '.addresses[] | select(.services == 1)'` shows the correct `relayPort` for each public node.
- ✅ `getconnectedpeerinfo --json` lists `services == 1` on outbound private-peer connections, proving the connection manager rotates through multiple relays.
- ✅ Address-manager migration logs (“schema upgraded from v1 to v2”) appear once per node start; subsequent restarts skip the rewrite (verified via the new migration harness test).

## 5. Protocol v9 Compatibility Proof
- `p2p-compat-v8-tests::compat_v8_ignores_v9_relay_fields_on_version`
- `p2p-compat-v8-tests::compat_v9_handles_v8_version_without_relay_fields`
- `p2p-compat-v8-tests::compat_v8_address_gossip_survives_relay_metadata`
- `utils::networking::tests::compat_v8_net_address_legacy_deserialize_defaults`
- `protocol::p2p::convert::messages::tests::compat_v9_version_zero_relay_port_defaults_to_none`
- Older readers ignore the new relay metadata, and newer readers safely default missing fields away.

## 6. Reproducing the Proof
1. Follow the tmux/rsync/run instructions in `tcp-hole-punch/README.md` (mirrors the Phase 6 runbook).
2. Use the same environment variables as documented in `phase6-remote-validation.md`.
3. Capture fresh logs if desired and compare to the referenced line numbers above.

## 7. Supporting Documentation
- `tcp-hole-punch/final-report.md` – full narrative of the implementation and troubleshooting timeline.
- `tcp-hole-punch/design/architecture.md` – architecture & configuration details for the libp2p ⇄ tonic bridge.
- `tcp-hole-punch/plan.md` – completed phase tracker plus outstanding backlog for production hardening.

This document supersedes informal “rollout checklist” notes; point reviewers here when validating the PoC.
