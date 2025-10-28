# TCP Hole Punching Test Scenarios

This document records the environments we exercised while validating libp2p’s DCUtR flow against Kaspa-style peers. Each section summarizes what we tried, key observations, and where to find the detailed logs.

---

## 1. Mobile Hotspot Dialer → Home LAN Listener

- **Goal**: Stress-test hole punching with an extreme NAT combination (phone 4G hotspot + residential router).
- **Relay**: `RELAYIP:4001` (peer `12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X`).
- **Listener (hotspot)**  
  - Command: `RUST_LOG=info cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode listen --secret-key-seed 2 --relay-address /ip4/RELAYIP/tcp/4001/p2p/12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X`  
  - Relay observed address: carrier WAN `/ip4/HOTSPOTIP/tcp/<dynamic>`  
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

## Next Steps

- Use these references when extending Kaspa’s adaptor to accept libp2p streams.
- If new NAT combinations are tested, append a section here with commands, logs, and conclusions so future engineers can reproduce the findings quickly.
