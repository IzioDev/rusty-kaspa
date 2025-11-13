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
- ✅ `cargo test --manifest-path tcp-hole-punch/bridge/Cargo.toml`  
  _Includes the 40-stream stress case; passes in <1 s after widening swarm channels._
- ✅ `cargo test -p kaspa-p2p-lib connect_with_stream_establishes_router -- --nocapture`
- ✅ `kaspa-cli getlibpstatus` on bridging nodes to confirm role/peer ID/inbound caps before each rehearsal.

Both commands were rerun on 2025-10-30 (local 6‑core laptop); output is available on request.

## 4. Phase 10 Relay Metadata
- ✅ `kaspa-cli getpeeraddresses --json | jq '.addresses[] | select(.services == 1)'` now lists the relay-capable set together with the advertised `relayPort`. Versions `<9` leave `services=0`, so they are automatically filtered out.
- ✅ `kaspa-cli getconnectedpeerinfo --json` surfaces the same metadata for active peers, making it easy to confirm that private nodes keep at least two distinct relay links (look for `services: 1` across `isOutbound=true` entries).
- ✅ Connection-manager logs show the new rotation logic (`Connection manager: ... relay quota ...`), proving that private nodes refuse to funnel every hole punch through a single relay and immediately retry with alternates after a failure.

## 5. Reproducing the Proof
1. Follow the tmux/rsync/run instructions in `tcp-hole-punch/README.md` (mirrors the Phase 6 runbook).
2. Use the same environment variables as documented in `phase6-remote-validation.md`.
3. Capture fresh logs if desired and compare to the referenced line numbers above.

## 6. Supporting Documentation
- `tcp-hole-punch/final-report.md` – full narrative of the implementation and troubleshooting timeline.
- `tcp-hole-punch/design/architecture.md` – architecture & configuration details for the libp2p ⇄ tonic bridge.
- `tcp-hole-punch/plan.md` – completed phase tracker plus outstanding backlog for production hardening.

This document supersedes informal “rollout checklist” notes; point reviewers here when validating the PoC.
