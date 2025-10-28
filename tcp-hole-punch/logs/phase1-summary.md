# Phase 1 – libp2p Baseline Execution Notes

## Relay server
- Command: `cargo run --release --manifest-path examples/relay-server/Cargo.toml -- --secret-key-seed 1 --port 4001`
- PeerId (`seed=1` via `peer-id` helper): `12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X`
- Listening multiaddrs captured in `logs/relay-server.log` (tcp + quic on loopback/private interfaces).

## Listener client
- Command: `cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode listen --secret-key-seed 2 --relay-address /ip4/127.0.0.1/tcp/4001/p2p/12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X`
- Relay reservation accepted; observed address `/ip4/127.0.0.1/tcp/64681`.
- Listener PeerId (`seed=2`): `12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3`.
- Full connection log in `logs/dcutr-listen.log`.

## Dialer client
- Command: `cargo run --release --manifest-path examples/dcutr/Cargo.toml -- --mode dial --secret-key-seed 3 --relay-address /ip4/127.0.0.1/tcp/4001/p2p/12D3KooWPjceQrSwdWXPyLLeABRXmuqt69Rg3sBYbU1Nft9HyQ6X --remote-peer-id 12D3KooWH3uVF6wv47WnArKHk5p6cvgCJEb74UTmxztmQDc298L3`
- Dialer PeerId (`seed=3`): `12D3KooWQYhTNQdmr3ArTeUHRYzFg94BKyTkoWBDWez9kSCVe2Xo`.
- Result: relay reservation succeeded; direct connection attempt failed locally (`Address already in use (os error 48)`) when dialing the listener’s observed address, as both peers run on the same host without distinct NAT mappings.
- Raw event trace stored in `logs/dcutr-dial.log`.

### Takeaways
- Relay infrastructure and DCUtR example binaries build and run with deterministic peer identities derived from `peer-id` helper (added under `libp2p/src/bin/peer-id.rs`).
- On single-host testing, hole punching fails with `AddrInUse` because the listener’s observed socket is already bound locally; reproducing a successful punch will require running peers behind distinct NATs as per libp2p tutorial guidance.
