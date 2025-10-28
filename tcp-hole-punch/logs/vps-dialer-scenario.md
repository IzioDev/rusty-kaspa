# VPS Dialer (no port forwarding) vs. Home Listener

- **Relay**: `RELAYIP:4001` (peer `12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X`).
- **Listener (home LAN)**
  - Command: `RUST_LOG=info cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode listen --secret-key-seed 6 --relay-address /ip4/RELAYIP/tcp/4001/p2p/12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X`
  - PeerId: `12D3KooWDMCQbZZvLgHiHntG1KwcHoqHPAxL37KvhgibWqFtpqUY`
  - Observed address from relay: `/ip4/HOMEIP/tcp/52346` (home router WAN, no port forwards).
- **Dialer (VPS VPSIP, no inbound ports exposed beyond 22/16111)**
  - Command: `RUST_LOG=info cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode dial --secret-key-seed 7 ... --remote-peer-id 12D3KooWDMCQbZZvLgHiHntG1KwcHoqHPAxL37KvhgibWqFtpqUY`
  - `ufw` allows 22 & 16111 only; random listener port 52346 remained closed to inbound dials.

**Result**
- DCUtR successfully upgraded the relay circuit to a direct TCP connection:
  - Dialer log (`logs/dialer_vps_scenario.log`):
    - `Established new connection ... address: /ip4/HOMEIP/tcp/52346`
  - Listener log (captured live):
    - `Established new connection ... address: /ip4/VPSIP/tcp/41803`
- This confirms hole punching works when both peers are non-public (VPS with closed inbound port + home NAT) as long as NATs are compatible.

