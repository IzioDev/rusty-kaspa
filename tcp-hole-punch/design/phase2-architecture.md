# Phase 2 – libp2p ⇄ tonic Bridge: Architecture Notes

## Objectives
- Accept libp2p DCUtR connections and surface them to Kaspa's existing tonic-based P2P adaptor without rewriting flow logic.
- Support both inbound and outbound Kaspa peers while keeping relay/DCUtR coordination inside libp2p.
- Preserve Kaspa metrics/logging expectations (peer identity, socket metadata) and expose fallbacks when hole punching fails.

## Relevant References
- `research/gpt.md` – sections *Focus 1* and *Focus 2* outline tonic APIs (`Endpoint::connect_with_connector`, `Server::serve_with_incoming`) and libp2p stream extraction strategies.
- `research/gemini.md` – “Bridging libp2p Streams to the Tonic Runtime” and “The libp2p Swarm as a Managed Service” describe wrapper traits, `Connected` metadata, and Swarm actor patterns.
- libp2p crate examples (not vendored here): `examples/dcutr` for DCUtR flow; `examples/relay-server` for relay coordination.

## Bridge Components
1. **Swarm Service (Actor)**
   - Owns libp2p `Swarm<Behaviour>` configured with TCP + Noise + Yamux, QUIC, relay client, identify, ping, DCUtR.
   - Runs in a dedicated task; communicates with application via async channels (`mpsc` for commands, `oneshot` for responses).
   - Responsibilities:
     - Dial peers via relay/DCUtR when instructed (`DialPeer` command).
     - Listen for inbound connections, perform DCUtR upgrades, and surface negotiated substreams.
     - Manage shutdown and relay reservations.

2. **Stream Wrapper**
   - Wraps `libp2p::Stream` (or `libp2p_stream::Stream`) implementing `tokio::io::AsyncRead + AsyncWrite`.
   - Implements tonic's `Connected` trait to provide metadata:
     ```rust
     impl Connected for Libp2pStreamWrapper {
         type ConnectInfo = Libp2pConnectInfo;
         fn connect_info(&self) -> Self::ConnectInfo { ... }
     }
     ```
   - `Libp2pConnectInfo` stores `PeerId`, `Multiaddr`, optional synthesized `SocketAddr` (fallback `HOMEIP:0` style if unknown), relay usage flag, observed addresses.

3. **Tonic Integration Points**
   - **Inbound**: feed wrappers into `Server::serve_with_incoming` by exposing an `mpsc::Receiver<Result<Libp2pStreamWrapper, _>>` as the incoming stream.
   - **Outbound**: pass wrappers via `Endpoint::connect_with_connector` by implementing `tower::Service<Uri, Response = Libp2pStreamWrapper>`.
   - Provide helper functions:
     ```rust
     async fn accept_stream(&self) -> Result<Libp2pStreamWrapper>;
     async fn dial_stream(&self, peer_id: PeerId) -> Result<Libp2pStreamWrapper>;
     ```

## Data Flow Overview
1. Kaspa adaptor requests connection (`dial_via_libp2p`):
   - Send `DialPeer { peer_id, relays }` command to Swarm actor.
   - Swarm performs relay reservation + DCUtR; upon success, returns `Libp2pStreamWrapper` via oneshot.
   - Kaspa uses tonic connector to wrap stream into `Router` handshake flow.
2. Libp2p receives inbound connection:
   - Swarm actor upgrades circuit to direct stream and pushes wrapper into incoming queue.
   - Kaspa server side consumes queue through `serve_with_incoming` and proceeds with handshake.
3. Failure cases:
   - If hole punch fails, wrapper indicates relay fallback and includes metrics for decision logic (optionally keep the relayed stream or retry later).

## Open Questions
- Should we rely on `libp2p-stream` crate for substream extraction or build a lightweight Behaviour ourselves? (Gemini recommends `libp2p_stream` for explicit accept/open APIs.)
- How do we expose relay statistics (e.g., connection duration, DCUtR retries) to Kaspa metrics? Possibly extend `Libp2pConnectInfo` with counters or pass a side-channel.
- Do we support QUIC fallback automatically? The swarm can register both TCP and QUIC; decision logic may live inside the behaviour.

## Next Steps
1. Prototype the Swarm actor interface in a separate module (no tonic integration yet).
2. Evaluate `libp2p-stream` vs. custom behaviour by coding minimal extraction.
3. Sketch the tonic connector/server wrappers (`bridge::client::Connector`, `bridge::server::Incoming`).
