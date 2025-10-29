# Merge Readiness Checklist (Phase 6)

- [ ] `cargo fmt --all` (workspace)
- [ ] `cargo clippy --all --workspace --all-targets` (bridge + kaspa_p2p with `--features libp2p-bridge`)
- [ ] `cargo test --manifest-path tcp-hole-punch/bridge/Cargo.toml`
- [ ] `cargo test -p kaspa-p2p-lib connect_with_stream_establishes_router libp2p_counters_capture_traffic`
- [ ] Remote validation replayed (`logs/phase6-{server,client,relay}-session.log`)
- [ ] Docs updated (`design/phase2-architecture.md`, `final-report.md`, `logs/phase6-remote-validation.md`)
