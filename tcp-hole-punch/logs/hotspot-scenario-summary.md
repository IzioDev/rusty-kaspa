# Hotspot Dialer vs. Home NAT Listener – Observations

- **Relay**: `RELAYIP:4001`, peer id `12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X`, running `relay-server-example` on Hetzner VPS.
- **Listener (phone hotspot)**  
  - Command: `RUST_LOG=info cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode listen --secret-key-seed 2 --relay-address /ip4/RELAYIP/tcp/4001/p2p/12D3KooWPj...`  
  - Observed address reported by relay: `HOTSPOTIP:*` (carrier-grade NAT).  
  - Reservation confirmed; trace logs saved to `logs/listener_run_hotspot.log`.
- **Dialer (web01 on home LAN)**  
  - Command loop: `RUST_LOG=info cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode dial --secret-key-seed 3 ... --remote-peer-id 12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3`.  
  - Each attempt reached the relay, learned observed address `HOMEIP:*`, but the punch failed: `Failed to connect to destination: Relay has no reservation for destination`.  
  - Dialer log captured in `logs/dialer_run_web01.log`.

**Outcome**: All attempts fell back to the relayed connection; no direct TCP hole punch succeeded. This aligns with expectations for mobile 4G hotspots (typically symmetric/port-randomizing NATs). Scenario recorded for future reference; moving to alternate NAT environments next.
