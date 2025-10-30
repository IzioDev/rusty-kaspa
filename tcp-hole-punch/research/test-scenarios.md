# Archived libp2p Baseline Test Scenarios

These notes capture the exploratory Phase 1 experiments we ran with the upstream `rust-libp2p/examples/dcutr` binaries before wiring the Kaspa bridge. They’re a historical reference only; the Proof‑of‑Concept validation in Phase 4/6 supersedes them. Each section lists the commands used, the observed behaviour, and the relevant logs so anyone can reproduce the baseline libp2p behaviour if needed.

---

## 1. Mobile Hotspot Dialer → Home LAN Listener

- **Goal**: Stress-test hole punching with an extreme NAT combination (phone 4G hotspot + residential router).
- **Relay**: `RELAYIP:4001` (peer `12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X`).
- **Listener (hotspot)**  
  - Command: `RUST_LOG=info cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode listen --secret-key-seed 2 --relay-address /ip4/RELAYIP/tcp/4001/p2p/12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X`  
  - Relay observed address: carrier WAN `/ip4/HOMEIP/tcp/<dynamic>`  
  - Logs: `logs/listener_run_hotspot.log`
- **Dialer (web01 @ home)**  
  - Command loop: `RUST_LOG=info cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode dial --secret-key-seed 3 ... --remote-peer-id 12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3`
  - Outcome: Relay coordination succeeds, but every punch attempt fails with `Relay has no reservation for destination`—the direct TCP session never forms.
  - Primary cause: the hotspot’s symmetric/port-randomizing NAT (typical for mobile carriers).
- **Summary**: Documented in `logs/hotspot-scenario-summary.md`. Expect relay fallback for similar NAT combos.

---

## 2. VPS Dialer (No Port Forward) → Home LAN Listener

- **Goal**: Emulate two “private” peers without explicit port forwarding.
- **Relay**: Same Hetzner VPS (`RELAYIP`).
- **Listener (home LAN)**  
  - Command: `RUST_LOG=info cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode listen --secret-key-seed 6 ...`  
  - PeerId: `12D3KooWDMCQbZZvLgHiHntG1KwcHoqHPAxL37KvhgibWqFtpqUY`  
  - Relay observed address: `/ip4/HOMEIP/tcp/52346` (home ISP WAN)  
  - Logs: see tmux session output in `logs/vps-dialer-scenario.md`
- **Dialer (VPS VPSIP)**  
  - Command: `RUST_LOG=info cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode dial --secret-key-seed 7 ... --remote-peer-id 12D3KooWDMCQbZZvLgHiHntG1KwcHoqHPAxL37KvhgibWqFtpqUY`  
  - Firewall: only ports 22 and 16111 open; the libp2p listener port remained closed.
- **Outcome**: Hole punching succeeds. Both sides log `Established new connection` with direct TCP endpoints (`HOMEIP:52346 ↔ VPSIP:41803`).  
  - Dialer log: `logs/dialer_vps_scenario.log`  
  - Summary: `logs/vps-dialer-scenario.md`

---

## Notes

- Keep this file under `tcp-hole-punch/research/` as an archive. Append new NAT permutations here only if you run the raw libp2p example binaries again and want a historical record.
