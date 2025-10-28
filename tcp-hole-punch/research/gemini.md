
A Strategic Plan for Integrating libp2p TCP Hole Punching into the Kaspa P2P Network

This report outlines a comprehensive, workable plan to enhance the Kaspa network by enabling direct peer-to-peer connections between nodes operating behind Network Address Translators (NATs). By integrating libp2p's Direct Connection Upgrade through Relay (DCUtR) protocol, we can achieve TCP hole punching with minimal disruption to Kaspa's existing tonic-based gRPC communication architecture. The strategic value lies in reducing reliance on public infrastructure, lowering latency, and increasing the overall robustness and decentralization of the network. This document provides a detailed technical blueprint, addressing the key challenges of transport abstraction, stream management, runtime integration, and security to guide the implementation process.

Bridging libp2p Streams to the Tonic Runtime

The foundational technical challenge in this integration is plumbing a raw, byte-oriented libp2p::Stream into Kaspa's application-level tonic gRPC stack. This requires a deep understanding of tonic's custom transport APIs and a clear analysis of the properties of the stream provided by libp2p. Successfully bridging these two distinct networking layers is paramount to achieving the project's goals without rewriting major components of the Kaspa node.

Analysis of Tonic's Custom Transport APIs

The tonic framework, while providing a high-level gRPC implementation, offers powerful extension points for operating over non-standard transports. These APIs are the primary enablers for this integration, allowing tonic to run on a connection established by libp2p rather than its default TCP listener and dialer.
For the server-side component of a P2P interaction (i.e., the node that accepts the incoming gRPC session), the primary API is tonic::transport::Server::serve_with_incoming. This method consumes the Server builder and executes it over a provided stream of I/O objects. The critical requirement is that each item yielded by the incoming stream must implement the AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static traits.1 This API is the designated entry point for accepting a successfully hole-punched connection from libp2p and handing it off to Kaspa's existing gRPC service implementation. A variant, serve_with_incoming_shutdown, extends this functionality by accepting a future that can trigger a graceful shutdown, a feature essential for clean integration into the Kaspa node's lifecycle management.1
For the client-side component (i.e., the node initiating the gRPC session after a connection is established), the key API is tonic::transport::Endpoint::connect_with_connector.3 This function allows the application to bypass tonic's standard TCP dialing logic and supply a custom "connector." This connector is a tower::Service that takes a Uri and returns a future that resolves to an object satisfying the AsyncRead + AsyncWrite traits. The tonic repository's Unix Domain Socket (UDS) example serves as the canonical reference for this pattern, demonstrating how to wrap a non-TCP stream for tonic's consumption.6 This is precisely the mechanism that will be used to feed a libp2p stream into a tonic client channel.
A catalog of the essential tonic APIs for this task includes:
Server::serve_with_incoming<I, IO, IE>(self, incoming: I): Runs the gRPC server on a provided stream of connection objects. This is the core function for the "listening" peer in a post-hole-punch scenario.
Server::serve_with_incoming_shutdown<...>(self, incoming: I, signal: F): An enhanced version of the above that integrates with a graceful shutdown signal.
Endpoint::connect_with_connector<C>(&self, connector: C): Creates a client Channel using a custom service (C) that is responsible for establishing the underlying I/O stream.3 This is the core function for the "dialing" peer.
Endpoint::connect_with_connector_lazy<C>(&self, connector: C): A variant that defers the creation of the underlying connection until the first gRPC call is made, which can be useful for managing resources.3

Extracting and Vending libp2p Streams

To use the tonic APIs, a compatible stream must first be obtained from the libp2p Swarm. A libp2p::Stream is a full-duplex byte stream that directly implements the futures::io::AsyncRead and futures::io::AsyncWrite traits.8 Because the tokio::io traits, which tonic depends on, provide blanket implementations for types that implement the futures::io equivalents, a libp2p::Stream is source-compatible with tonic's transport requirements. Furthermore, the stream satisfies the necessary marker traits: it is Send, allowing it to be transferred between threads, and Unpin, making it suitable for use in async contexts.8 It is notably !Sync, which reinforces the need for a clear ownership model where the stream is handled by a single task at a time.
The rust-libp2p framework is architected around the NetworkBehaviour and ConnectionHandler traits, which typically manage protocol logic and keep streams encapsulated within the Swarm's event loop.9 While a successful DCUtR event signals the creation of a direct connection via a ConnectionId, it does not directly expose the underlying stream to the application layer.12 Attempting to "hijack" a stream from deep within a custom ConnectionHandler would be complex and counter to the framework's design principles, which are geared toward protocol implementation rather than stream extraction.9
A more idiomatic and robust approach is to use the libp2p-stream crate.14 This crate is specifically designed to provide a cleaner, more direct API for application-level stream management. It introduces a Control object that allows an application to explicitly accept() inbound streams or open_stream() outbound ones for a specific, application-defined protocol. This avoids the need for deep, custom ConnectionHandler logic for the sole purpose of stream extraction.
The proposed workflow is as follows:
Define a custom protocol name for Kaspa's gRPC communication over libp2p, for example, /kaspa/grpc/1.0.
After the dcutr behavior signals a successful hole punch, one peer (the initiator) will use Control::open_stream() to open a new substream to the other peer using this protocol ID.
The other peer will have a pending future from Control::accept() for the same protocol ID, which will resolve with the newly opened stream.
The extracted libp2p_stream::Stream is then ready to be vended to the tonic layer. This handoff from the libp2p event loop to the Kaspa adaptor will be managed via tokio::sync::mpsc channels, decoupling the two subsystems.

Surfacing Peer Address Information in Tonic

A significant challenge highlighted in the initial query is the potential loss of the remote peer's address. Kaspa's existing router and logging infrastructure depend on tonic::Request::remote_addr(), which returns an Option<SocketAddr>.16 When using a custom transport, this method returns None unless the underlying I/O object implements the tonic::transport::server::Connected trait.18 A raw libp2p::Stream does not provide this information in a format tonic understands.
The solution is not to simply pass the raw stream but to create a wrapper that bridges the information gap. This wrapper struct, which can be named Libp2pStreamWrapper, will contain the libp2p::Stream and delegate the AsyncRead and AsyncWrite trait implementations to it. Its primary architectural role is to implement the Connected trait.
The Connected trait is the linchpin of this part of the integration. It requires an associated type, ConnectInfo, which can be a custom struct containing rich connection metadata.19

Rust


pub trait Connected {
    type ConnectInfo: Clone + Send + Sync + 'static;
    fn connect_info(&self) -> Self::ConnectInfo;
}


A custom KaspaP2pConnectInfo struct will be defined to serve as this ConnectInfo. This struct will store the libp2p::PeerId and the final Multiaddr of the connected peer, preserving the complete addressing information provided by libp2p. This information can then be accessed from a tonic service via request extensions, providing a path for future enhancements that leverage this richer data.
To maintain backward compatibility with existing Kaspa components, the KaspaP2pConnectInfo struct will contain a method to synthesize a SocketAddr. This can be achieved by iterating through the protocols in the Multiaddr and extracting the IP and TCP port components. In cases where a standard SocketAddr cannot be derived (e.g., for a pure relay address), a placeholder can be generated—for instance, by using the last few bytes of the PeerId's hash to create a private IP address—or the method can simply return None. This approach ensures that remote_addr() continues to function predictably while making the full, superior addressing information from libp2p available where needed. The tls-connect-info feature in tonic provides a reference for this pattern of implementing Connected on custom TLS connectors.21

Address and Identity Management

A successful integration requires a coherent strategy for managing peer identities and addresses, translating between the paradigms of libp2p and Kaspa's existing network model. This involves mapping cryptographic identities and handling the richer, more flexible addressing scheme of libp2p.

libp2p Identity and Addressing Primitives

libp2p is built on two fundamental concepts: identity and location.
Identity: The canonical identifier for any node in a libp2p network is its PeerId. A PeerId is a cryptographic hash (specifically, a multihash) of the node's public key, making it a secure, verifiable, and location-independent identifier.22
Addressing: A node's location is described by one or more Multiaddrs. A Multiaddr is a self-describing network address format that can encapsulate a stack of protocols, such as /ip4/192.0.2.1/tcp/4001 or a relayed address like /p2p/QmRelayPeer/p2p-circuit/p2p/QmTargetPeer.22
After a successful DCUtR-negotiated connection, several key pieces of information become available. The dcutr NetworkBehaviour emits an event that explicitly provides the PeerId of the remote peer.12 The connection itself is associated with a specific Multiaddr that was successfully used. Furthermore, the identify protocol, which typically runs automatically on new connections, allows peers to exchange metadata, including the address on which they were observed by the remote peer.25 This "observed address" is crucial for a node to learn about its public-facing IP and port, which is a foundational step in the NAT traversal process.26

Mapping to Kaspa Primitives

The primitives used in Kaspa's networking layer must be mapped to their libp2p equivalents. This mapping must be bidirectional where possible and must handle information loss gracefully.
Identity Mapping: Kaspa uses a PeerKey for peer identification, which is also derived from a public key. The convergence of both systems on public-key cryptography provides a strong foundation for a unified identity. The mapping can be achieved by ensuring that both the Kaspa PeerKey and the libp2p::PeerId are generated from the same underlying cryptographic keypair. The libp2p::PeerId can be converted to and from a raw byte slice using its to_bytes() and from_bytes() methods.23 These raw bytes (representing the public key's multihash) can be used to construct a Kaspa PeerKey, and vice-versa, establishing a consistent identity across both networking stacks.
Address Mapping: Kaspa's NetAddress is fundamentally a wrapper around an IP address and port, reflecting a client-server view of the network. In contrast, libp2p's Multiaddr is a superset, capable of describing much more complex routing information. The translation from Multiaddr to NetAddress is therefore inherently lossy. The recommended approach is to augment, not replace, Kaspa's peer data structures. The peer manager should be updated to store the full, original Multiaddr as the primary source of truth for connectivity. A NetAddress can then be synthesized from this Multiaddr by parsing it for IP and TCP components, serving as a compatibility layer for existing code.
The following table formalizes the proposed mapping logic.
libp2p Primitive
Kaspa Primitive
Proposed Mapping/Translation Logic
Notes/Challenges
libp2p::identity::PeerId
kaspa_core::PeerKey
Use PeerId::to_bytes() and construct PeerKey from the raw multihash bytes. A reverse function must be implemented.
Ensure both primitives use the same underlying cryptographic key type and hashing algorithm for seamless conversion.
libp2p::Multiaddr
kaspa_core::NetAddress
Parse the Multiaddr for /ip4/ or /ip6/ and /tcp/ components to construct a SocketAddr, then wrap in NetAddress.
This is a lossy conversion. Relay addresses (/p2p-circuit/) will not translate. A fallback or placeholder is needed.
Observed Multiaddr
N/A
Store as part of the peer's metadata in Kaspa's peer manager.
This is new information that Kaspa does not currently track but is highly valuable for future P2P optimizations.


Strategies for Stable Identifier Synthesis

In a dynamic P2P network where IP addresses are ephemeral, relying on stable, cryptographic identifiers is essential.
The libp2p::PeerId must be treated as the canonical identifier for any node. All peer management, reputation scoring, and routing logic should be keyed by PeerId. A Multiaddr may contain a PeerId encapsulated within a /p2p/ protocol component.22 A utility function can be implemented to reliably extract this PeerId by iterating over the Multiaddr components. While some documentation refers to a PeerId::try_from_multiaddr function, its availability can be inconsistent, making a manual iteration a more robust solution.28

Rust


use libp2p::{Multiaddr, PeerId, multiaddr::Protocol};

fn peer_id_from_multiaddr(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|protocol| {
        if let Protocol::P2p(peer_id) = protocol {
            Some(peer_id)
        } else {
            None
        }
    })
}


If a Multiaddr does not contain a PeerId, the identity must be obtained through other means. The most reliable method is to use the PeerId provided in the SwarmEvent::ConnectionEstablished event, which is emitted by the Swarm upon a successful connection and includes both the peer's ID and the endpoint address used.

Architectural Integration Strategy

Integrating the libp2p subsystem requires a carefully designed architecture that respects the ownership and runtime models of both libp2p and tokio. The goal is to encapsulate libp2p as a self-contained service within the Kaspa node, exposing its capabilities through a clean, asynchronous API.

The libp2p Swarm as a Managed Service (Actor Model)

The libp2p::Swarm is a stateful object that must be continuously polled to drive the network state machine forward (e.g., process incoming data, manage connections, and advance protocol states). It is also !Sync, meaning it cannot be safely shared across threads via an Arc. Community discussions and best practices strongly indicate that attempting to wrap the Swarm in a Mutex for shared access is an anti-pattern that frequently leads to deadlocks, especially when a lock is held across an .await boundary.29
The correct and robust architectural pattern is the Actor Model. The Swarm will be owned and managed exclusively by a single, dedicated tokio task. This "Swarm actor" will run a continuous event loop, polling the Swarm and handling all network events internally. It will communicate with the rest of the Kaspa application through asynchronous message-passing channels.
Initialization of this service will use the SwarmBuilder. The builder will be configured with SwarmBuilder::with_tokio() to ensure it uses the existing tokio runtime for spawning I/O and background tasks, preventing runtime conflicts.31 The transport stack will be configured to include TCP, security layers like noise, a stream multiplexer like yamux, and the necessary NetworkBehaviours for hole punching: dcutr and relay_client.

An Asynchronous API for the Kaspa Adaptor

To maintain clean separation of concerns, the Kaspa adaptor module should not have direct access to the Swarm. Instead, it will interact with the libp2p subsystem through a well-defined handle. This Libp2pServiceHandle will be a lightweight, cloneable struct that encapsulates the sender half of a channel for sending commands to the Swarm actor.
The proposed API on this handle will abstract away the complexities of the libp2p event loop. For example, initiating a hole-punched connection would be a single async method call:

Rust


use tokio::sync::{mpsc, oneshot};
use libp2p::PeerId;

// A command sent from the application to the Swarm actor
enum Libp2pCommand {
    Dial {
        peer_id: PeerId,
        response_sender: oneshot::Sender<Result<Libp2pStreamWrapper, DialError>>,
    },
    // Other commands like Shutdown, GetStats, etc.
}

#[derive(Clone)]
pub struct Libp2pServiceHandle {
    command_sender: mpsc::Sender<Libp2pCommand>,
}

impl Libp2pServiceHandle {
    pub async fn dial_dcutr(&self, peer_id: PeerId) -> Result<Libp2pStreamWrapper, DialError> {
        let (response_sender, response_receiver) = oneshot::channel();
        let cmd = Libp2pCommand::Dial { peer_id, response_sender };

        // Send the command and wait for the response
        self.command_sender.send(cmd).await
           .map_err(|_| DialError::ServiceShutdown)?; // Handle case where actor has shut down

        response_receiver.await
           .map_err(|_| DialError::RequestDropped)? // Handle case where actor drops the request
    }
}



Inter-Task Communication and State Management

The core of the Swarm actor is its event loop. This loop must be responsive to both commands from the application and events from the network. The tokio::select! macro is the canonical tool for this purpose, allowing the task to await multiple futures concurrently and process whichever one completes first.30
The actor's loop will look conceptually like this:

Rust


// Inside the Swarm actor task
loop {
    tokio::select! {
        // Branch 1: Handle a command from the application
        Some(command) = command_receiver.recv() => {
            // e.g., handle Libp2pCommand::Dial by calling swarm.dial()
            // and storing the oneshot::Sender in a map keyed by PeerId
        }

        // Branch 2: Handle a network event from the Swarm
        event = swarm.select_next_some() => {
            // e.g., handle SwarmEvent::Behaviour(DcutrEvent::HolePunchSucceeded)
            // by finding the corresponding oneshot::Sender and sending back the stream
        }

        // Branch 3: Handle shutdown signal
        _ = shutdown_signal.recv() => {
            break; // Exit the loop to terminate the actor
        }
    }
}


This structure ensures that the actor remains non-blocking and can manage all libp2p-related state internally. For instance, a HashMap<PeerId, oneshot::Sender<...>> can be used to track pending dial requests, allowing the actor to respond to the correct caller when a connection is finally established.

Graceful Shutdown and Resource Management

Proper lifecycle management is critical for a stable node. The Swarm actor must integrate with the Kaspa node's shutdown sequence. A shutdown signal (e.g., from a tokio::sync::watch channel, or a simple ctrl_c handler for testing) will be incorporated into the actor's select! loop.
Upon receiving this signal, the actor will break its loop. When the actor's main function returns, the Swarm object it owns will be dropped. The Swarm's Drop implementation handles the graceful closing of all active connections and substreams, releasing network resources. The main application thread, after broadcasting the shutdown signal, should await the JoinHandle of the Swarm actor's task to ensure that this cleanup process has completed before the program exits.

Failure Handling and Network Strategy

TCP hole punching is a probabilistic technique, not a guaranteed one. Its success depends heavily on the type and configuration of NAT devices involved. A robust system must therefore be designed with the explicit assumption that hole punching will frequently fail and must include a seamless, efficient fallback strategy.

Analysis of DCUtR Signals and Timelines

The libp2p-dcutr NetworkBehaviour provides clear signals for success and failure. It emits a dcutr::Event which is a Result. A successful outcome is indicated by Ok(ConnectionId), signifying that a new direct connection has been established. A failure is indicated by Err(ConnectionUpgradeError), which provides details about why the attempt failed, such as a timeout or a protocol negotiation error.12
The hole punching process can be time-consuming due to the necessary coordination through a relay. An application cannot wait indefinitely for a result. It is therefore critical to implement an application-level timeout on top of libp2p's internal timeouts. When the Kaspa adaptor calls dial_dcutr, it should wrap the call in a timeout (e.g., using tokio::time::timeout). A reasonable duration might be 5-10 seconds. If this timeout elapses before a response (either success or failure) is received from the Swarm actor, the attempt should be considered failed, allowing the application to proceed immediately to its fallback strategy.

NAT Traversal Efficacy and Limitations

Real-world studies on NAT traversal provide important context for setting expectations. Early academic research found that TCP hole punching has a success rate of approximately 64%, while UDP hole punching is more successful at around 82%.35 More recent measurement campaigns focused specifically on libp2p's implementation suggest a success rate that caps at around 70-80% under favorable conditions.37
The most significant limitation is the presence of symmetric NATs. A symmetric NAT assigns a different external port for each new destination peer, making it extremely difficult for an external party to predict which port to connect to. libp2p's current DCUtR implementation does not reliably establish connections when both peers are behind symmetric NATs.39 Since symmetric NATs are prevalent in some corporate environments and are often used by mobile carriers (Carrier-Grade NAT), this failure case is common and must be a primary consideration in the system's design.

Recommended Fallback Mechanisms: A Multi-Layered Strategy

Given that direct connectivity is not guaranteed, a multi-layered connection strategy is required to ensure reliability.
Primary Fallback: Circuit Relay: When a direct connection cannot be established, the only remaining option for communication between two NAT-ed peers within the libp2p framework is to use a circuit relay. The libp2p-relay client protocol allows a node to establish an end-to-end encrypted connection that is proxied through a third-party, publicly reachable relay node.40 While this introduces additional latency and depends on the availability of the relay, it provides a crucial fallback for connectivity.
Proposed Connection Strategy: A simple, sequential fallback (try direct -> if fail, try hole punch -> if fail, try relay) is suboptimal as it introduces significant latency at each step. A more effective approach is to race multiple connection methods concurrently to minimize the time-to-first-byte.
Step 1: Attempt Direct Dial. The application should first attempt a standard TCP dial to any known public Multiaddrs of the peer. This is the most efficient path and will succeed if the peer is publicly dialable.
Step 2: Race Hole Punching and Relaying. If the peer has no known public addresses or the initial direct dial fails, the Kaspa adaptor should simultaneously initiate two operations via the Libp2pServiceHandle:
A DCUtR hole punching attempt to the target PeerId.
A circuit relay connection attempt to the PeerId via one or more known, trusted relay nodes.
Step 3: Use First Success. The application should proceed with the first connection that is successfully established. If the relayed connection is established first, communication can begin immediately over the proxy. If the hole punching attempt subsequently succeeds, the Swarm will emit an event. The application can then decide to "upgrade" to the more efficient direct connection and terminate the relayed one. This "race-to-connect" strategy optimizes for connection establishment time in an environment with uncertain network topologies.

Heuristics for Efficient Resource Utilization

To avoid wasting resources on futile connection attempts, several heuristics can be employed.
Limit Punching Attempts: Hole punching attempts should be time-limited and not retried indefinitely. A single, short-lived attempt per connection request is generally sufficient.
Prioritize Based on NAT Type: The libp2p identify protocol allows a node to learn about its own NAT type by observing its address as reported by other peers.27 If this information is shared, a node could infer the likelihood of success. For example, if both peers discover they are behind "easy" NATs (e.g., Endpoint-Independent Mapping), hole punching can be prioritized. Conversely, if one peer is known to be behind a symmetric NAT, it may be more efficient to fall back to relaying immediately.

Security and Abuse Mitigation

The introduction of new networking capabilities, particularly reliance on public relay nodes, necessitates a thorough security review and the implementation of robust abuse mitigation strategies. libp2p provides the tools for building secure systems, but the application layer is responsible for defining and enforcing policy.

Relay Trust Model and Connection Security

A key security feature of libp2p's circuit relay protocol is that all relayed connections are end-to-end encrypted between the two connecting peers. The relay node acts as a packet forwarder but cannot decrypt or tamper with the contents of the communication.41 The primary threat posed by a malicious or compromised relay is a denial-of-service (DoS) attack, where it can selectively drop traffic or refuse to forward connections.
The trust model should therefore be one of "trust but verify," with redundancy.
Best Practice: A Kaspa node should not depend on a single relay. The network should maintain a curated list of multiple, geographically and administratively diverse public relays. When a relayed connection is needed, a client can select one randomly from this list. This distributes trust and makes the network more resilient to the failure or malice of a single relay operator. For applications requiring a higher degree of security or reliability, the Kaspa project could deploy its own set of private, trusted relay nodes.42
Identity and Accountability: The relay protocol is not anonymous. All three parties—the dialing peer, the listening peer, and the relay—are identified by their cryptographic PeerId.41 This provides a basis for accountability and allows for reputation systems to be built at the application layer.

Enforcing Connection and Rate Limits

The first line of defense against many network-based attacks is the enforcement of strict resource limits. rust-libp2p provides several mechanisms for this.
Connection Limits: The libp2p::swarm::ConnectionLimits struct is a powerful tool for preventing resource exhaustion from excessive connection attempts.43 It is essential that every Kaspa node be configured with sane defaults for:
with_max_pending_incoming: Limits the number of concurrent incoming connections that are in the handshake/negotiation phase. This is a critical defense against SYN flood-style attacks.
with_max_established_per_peer: Prevents a single remote peer from consuming an unreasonable number of connection slots.
with_max_established: A global cap on the total number of established connections to prevent memory and file descriptor exhaustion.
Rate Limiting: While a dedicated libp2p-ratelimit crate exists, it may be outdated.44 However, rate-limiting principles can be applied at different layers. For nodes that will act as public relays, the libp2p::relay::Behaviour's Config allows for setting limits on the number of active reservations, connection durations, and data transfer rates, which is crucial for preventing abuse of the relay service.47 At the application protocol level, gossipsub includes configuration options to limit the number of messages per RPC, which can mitigate spam.46 Furthermore, the allow-block-list crate can be used to implement a dynamic firewall, allowing the application to block peers that are identified as malicious.48

Mitigating Eclipse Attacks and Other Vectors

Beyond simple resource exhaustion, more sophisticated attacks must be considered.
Eclipse Attacks: In an eclipse attack, an adversary attempts to isolate a victim node from the rest of the network by surrounding it with malicious peers, thereby controlling its view of the network state.49 The primary defense against this is to ensure a node maintains a diverse set of connections to honest peers. This involves using a robust peer discovery mechanism (like the Kademlia DHT) and connecting to a diverse, hardcoded set of bootstrap nodes to ensure the initial routing table is not easily poisoned.
Relay Abuse: Malicious peers may attempt to exhaust a relay's resources by requesting an excessive number of reservations or by using relayed connections to launch attacks on other nodes. As mentioned, the relay Config provides knobs to limit reservations and data rates. Operators of public relays for the Kaspa network must configure these limits conservatively to ensure the stability of this critical infrastructure.
Ultimately, security in a decentralized system is a layered responsibility. libp2p provides the low-level cryptographic guarantees of authentication and encryption, but the Kaspa application layer must use the signals provided—such as the authenticated PeerId—to implement its own higher-level security policies regarding authorization, reputation, and abuse prevention.49

Conclusion: Recommendations and Policy Decisions

The integration of libp2p's DCUtR protocol presents a viable and powerful path toward enabling direct, peer-to-peer connections for NAT-ed Kaspa nodes. By carefully bridging the libp2p and tonic ecosystems, Kaspa can significantly enhance its network topology's decentralization and efficiency without requiring a disruptive rewrite of its core communication logic. The analysis in this report leads to a set of concrete recommendations and highlights key policy decisions that require attention from the Kaspa engineering and product teams.

Summary of Key Recommendations

Adopt the Actor Model for Swarm Management: The libp2p::Swarm should be encapsulated within a dedicated tokio task. This architectural pattern is essential for managing the Swarm's state and polling requirements safely in a concurrent application, preventing common pitfalls like deadlocks. All interaction with the libp2p subsystem should occur through a clean, channel-based ServiceHandle API.
Use libp2p-stream for Stream Extraction: Instead of implementing complex, low-level ConnectionHandler logic, the libp2p-stream crate should be used. It provides the correct level of abstraction for opening and accepting application-level streams over established libp2p connections.
Implement the Connected Trait for Compatibility: To ensure continued functionality of Kaspa's existing network logic, a wrapper around the libp2p::Stream must be created. This wrapper will implement tonic's Connected trait, allowing it to provide a synthesized SocketAddr while also making richer libp2p metadata like PeerId and Multiaddr available via request extensions.
Adopt a Concurrent Connection Strategy: A sequential fallback from direct dial to hole punching to relaying is inefficient. To minimize connection latency, the system should race hole punching and circuit relay attempts concurrently, using whichever connection method succeeds first.
Enforce Strict Configuration Limits: Security and stability must be prioritized from the outset. The implementation must include conservative, default configurations for ConnectionLimits and relay behavior limits to provide a strong baseline defense against resource exhaustion and other denial-of-service attacks.

Blockers and Open Questions for Kaspa Engineering

The following points require decisions and further investigation before or during implementation:
Official Relay Infrastructure: The network will need a set of public, trusted relay nodes to facilitate hole punching coordination and to serve as a connectivity fallback. What is the plan for deploying, maintaining, and publicizing the initial list of official Kaspa relay nodes?
Resource Limit Tuning: The specific numerical values for connection limits (e.g., max_pending_incoming, max_established_per_peer) are critical for security but also impact performance. These values must be determined through performance testing and modeling based on expected network size, node capacity, and adversarial assumptions.
Cryptographic Key Unification: A formal audit of Kaspa's PeerKey and libp2p's PeerId generation is required. The process must be verified to ensure that both identifiers can be derived from the same underlying cryptographic keypair to establish a unified node identity.
Peer Store Integration: Kaspa's peer management database must be extended. What is the schema and migration strategy for storing new peer metadata, such as full Multiaddrs and observed addresses obtained from the identify protocol?

Appendix


Annotated Links to Code and Documentation

tonic UDS Example: https://github.com/hyperium/tonic/tree/master/examples/src/uds
Annotation: This is the primary reference for implementing connect_with_connector and serve_with_incoming with non-TCP streams. It demonstrates the pattern of creating a stream wrapper and using tower::service_fn.6
libp2p Hole Punching Tutorial: https://docs.rs/libp2p/latest/libp2p/tutorials/hole_punching/index.html
Annotation: Provides a step-by-step guide for setting up a relay and using the dcutr-example binary. The example logs are invaluable for confirming the expected event flow and data available upon success.12
libp2p-stream Crate: https://crates.io/crates/libp2p-stream
Annotation: The documentation for the recommended high-level API for extracting streams from the Swarm for external application use, avoiding low-level ConnectionHandler complexity.14
tonic::transport::server::Connected Trait: https://docs.rs/tonic/latest/tonic/transport/server/trait.Connected.html
Annotation: The official documentation for the key trait that must be implemented on our stream wrapper to surface peer address information to tonic's request context.19
libp2p::swarm::ConnectionLimits Struct: https://tidelabs.github.io/tidechain/libp2p/swarm/struct.ConnectionLimits.html
Annotation: The primary API for configuring connection-based DoS protection. This documentation details the available limits that should be configured for the Kaspa node.43
Works cited
Router in tonic::transport::server - Rust, accessed October 27, 2025, https://wiki.enablingpersonalizedinterventions.nl/docs/tonic/transport/server/struct.Router.html
Router in tonic::transport::server - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/tonic/latest/tonic/transport/server/struct.Router.html
Endpoint in tonic::transport - Rust - UCSD CSE, accessed October 27, 2025, https://cseweb.ucsd.edu/classes/sp22/cse223B-a/tribbler/tonic/transport/struct.Endpoint.html
tonic::transport::channel::Endpoint - Rust, accessed October 27, 2025, https://gkkachi.github.io/gapi-grpc-rs/tonic/transport/channel/struct.Endpoint.html
Endpoint in tonic::transport::channel - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/tonic/latest/tonic/transport/channel/struct.Endpoint.html
Support non-static UDS client connection paths · Issue #1612 · hyperium/tonic - GitHub, accessed October 27, 2025, https://github.com/hyperium/tonic/issues/1612
Update UDS example with socket paths determined at runtime #1611 - GitHub, accessed October 27, 2025, https://github.com/hyperium/tonic/issues/1611
Stream in libp2p - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p/latest/libp2p/struct.Stream.html
Simple example with stream between two peers · libp2p rust-libp2p ..., accessed October 27, 2025, https://github.com/libp2p/rust-libp2p/discussions/2396
libp2p_swarm - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p-swarm
ConnectionHandler in libp2p::swarm - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p/latest/libp2p/swarm/trait.ConnectionHandler.html
libp2p::tutorials::hole_punching - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p/latest/libp2p/tutorials/hole_punching/index.html
libp2p::swarm - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p/latest/libp2p/swarm/index.html
libp2p-stream - crates.io: Rust Package Registry, accessed October 27, 2025, https://crates.io/crates/libp2p-stream
libp2p-stream - crates.io: Rust Package Registry, accessed October 27, 2025, https://crates.io/crates/libp2p-stream/dependencies
Request in tonic - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/tonic/latest/tonic/struct.Request.html
Request in tonic - Rust, accessed October 27, 2025, https://gkkachi.github.io/firestore-grpc/tonic/struct.Request.html
TcpConnectInfo in tonic::transport::server - Rust - UCSD CSE, accessed October 27, 2025, https://cseweb.ucsd.edu/classes/sp22/cse223B-a/tribbler/tonic/transport/server/struct.TcpConnectInfo.html
Connected in tonic::transport::server - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/tonic/latest/tonic/transport/server/trait.Connected.html
TcpConnectInfo in tonic::transport::server - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/tonic/latest/tonic/transport/server/struct.TcpConnectInfo.html
tonic - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/tonic
Peers - The libp2p docs, accessed October 27, 2025, https://docs.libp2p.io/concepts/fundamentals/peers/
PeerId in libp2p - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p/latest/libp2p/struct.PeerId.html
Multiaddr in libp2p - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p/latest/libp2p/struct.Multiaddr.html
Protocols - The libp2p docs, accessed October 27, 2025, https://docs.libp2p.io/concepts/fundamentals/protocols/
libp2p_observed_address - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p-observed-address
Decentralized Hole Punching - Protocol Labs Research, accessed October 27, 2025, https://research.protocol.ai/publications/decentralized-hole-punching/seemann2022.pdf
PeerId in libp2p - Rust, accessed October 27, 2025, https://tidelabs.github.io/tidechain/libp2p/struct.PeerId.html
Access swarm in a multi threading app - Users and Developers - libp2p, accessed October 27, 2025, https://discuss.libp2p.io/t/access-swarm-in-a-multi-threading-app/2433
How do I call into libp2p rather than having it call my functions in a loop? #2024 - GitHub, accessed October 27, 2025, https://github.com/libp2p/rust-libp2p/discussions/2024
SwarmBuilder in libp2p - Rust, accessed October 27, 2025, https://libp2p.github.io/rust-libp2p/libp2p/struct.SwarmBuilder.html
SwarmBuilder in libp2p - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p/latest/libp2p/struct.SwarmBuilder.html
Connection is not esthablished - rust - libp2p, accessed October 27, 2025, https://discuss.libp2p.io/t/connection-is-not-esthablished/2554
libp2p::tutorials::hole_punching - Rust - Inria, accessed October 27, 2025, https://wide.gitlabpages.inria.fr/data-wallet-prototype/libp2p/tutorials/hole_punching/index.html
What's so hard about p2p Hole Punching? [closed] - Stack Overflow, accessed October 27, 2025, https://stackoverflow.com/questions/23176800/whats-so-hard-about-p2p-hole-punching
Peer-to-Peer Communication Across Network Address Translators - Bryan Ford, accessed October 27, 2025, https://bford.info/pub/net/p2pnat/
Comparing Iroh & Libp2p: Simplifying P2P Connectivity, accessed October 27, 2025, https://www.iroh.computer/blog/comparing-iroh-and-libp2p
Decentralized NAT Hole-Punching Measurement Campaign - libp2p, accessed October 27, 2025, https://discuss.libp2p.io/t/decentralized-nat-hole-punching-measurement-campaign/1616
Symmetric NAT Holepunching - Research and paper discussions - libp2p, accessed October 27, 2025, https://discuss.libp2p.io/t/symmetric-nat-holepunching/1335
DCUtR - libp2p, accessed October 27, 2025, https://docs.libp2p.io/concepts/nat/dcutr/
Circuit Relay - The libp2p docs, accessed October 27, 2025, https://docs.libp2p.io/concepts/nat/circuit-relay/
threefoldtech/libp2p-relay - GitHub, accessed October 27, 2025, https://github.com/threefoldtech/libp2p-relay
ConnectionLimits in libp2p::swarm - Rust, accessed October 27, 2025, https://tidelabs.github.io/tidechain/libp2p/swarm/struct.ConnectionLimits.html
libp2p_ratelimit - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p-ratelimit
libp2p-ratelimit — Rust network library // Lib.rs, accessed October 27, 2025, https://lib.rs/crates/libp2p-ratelimit
Rate limiting for rust libp2p GossipSub #5632 - GitHub, accessed October 27, 2025, https://github.com/libp2p/rust-libp2p/discussions/5632
libp2p::relay - Rust - Docs.rs, accessed October 27, 2025, https://docs.rs/libp2p/latest/libp2p/relay/index.html
Correct way to implement private relay? · libp2p rust-libp2p · Discussion #5135 - GitHub, accessed October 27, 2025, https://github.com/libp2p/rust-libp2p/discussions/5135
Security Considerations - The libp2p docs, accessed October 27, 2025, https://docs.libp2p.io/concepts/security/security-considerations/
