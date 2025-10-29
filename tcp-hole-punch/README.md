# TCP Hole Punch Workspace

This directory collects everything built around the libp2p/DCUtR proof-of-concept for Kaspa.

| Path | Purpose |
| --- | --- |
| `plan.md` | Phase tracker with status of each milestone and the follow-up work queued after Phase 4. |
| `final-report.md` | End-to-end write‑up covering architecture changes, troubleshooting, the successful remote run, and future recommendations. |
| `design/phase2-architecture.md` | Detailed design notes for the bridge layer, stream wrapper, and operational guidance. |
| `logs/` | Sanitised captures from local and remote rehearsals plus the mixed-NAT runbook (`phase4-remote-success.md`). |
| `bridge/` | Libp2p⇄tonic bridge crate sources and tests. |
| `relay/` | Helper crate/scripts for running the standalone relay binary. |
| `scripts/` | Utilities for controlling the remote relay service. |
| `research/` | Scratch notes and reference material gathered during investigation. |
| `TEST_SCENARIOS.md` | Phase 1/2 scenario notes used when validating the initial libp2p experiments. |

For a quick orientation:
1. Start with `final-report.md` to understand what was built and how the PoC succeeded.
2. Review `design/phase2-architecture.md` for implementation specifics.
3. Use the runbook in `logs/phase4-remote-success.md` if you need to replay the mixed-NAT handshake.
