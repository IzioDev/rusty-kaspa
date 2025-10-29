use clap::Parser;
use futures::StreamExt;
use libp2p::{
    core::{multiaddr::Protocol, transport::ListenerId, Multiaddr},
    identify, identity, noise, ping, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
use std::{
    net::{Ipv4Addr, Ipv6Addr},
    time::Duration,
};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "hole-punch-relay", about = "Minimal libp2p relay server used for Kaspa hole-punch PoC.")]
struct Opt {
    /// TCP port to listen on (both IPv4 and IPv6 if enabled).
    #[arg(long, default_value_t = 4011)]
    port: u16,

    /// Also listen on IPv6 (in addition to IPv4 unspecified address).
    #[arg(long, default_value_t = false)]
    ipv6: bool,

    /// Deterministic peer-id seed (0-255). When omitted a random identity is used.
    #[arg(long)]
    secret_key_seed: Option<u8>,
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay: relay::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).try_init();

    let opts = Opt::parse();
    let local_key = if let Some(seed) = opts.secret_key_seed { generate_ed25519(seed) } else { identity::Keypair::generate_ed25519() };
    let local_peer_id = local_key.public().to_peer_id();

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(tcp::Config::default(), noise::Config::new, yamux::Config::default)?
        .with_quic()
        .with_behaviour(|key| Behaviour {
            relay: relay::Behaviour::new(key.public().to_peer_id(), Default::default()),
            ping: ping::Behaviour::new(ping::Config::new().with_interval(Duration::from_secs(30))),
            identify: identify::Behaviour::new(identify::Config::new("/kaspa/relay/1.0.0".to_string(), key.public())),
        })?
        .build();

    let listeners = build_listeners(opts.port, opts.ipv6);
    for addr in listeners {
        let id: ListenerId = swarm.listen_on(addr.clone())?;
        println!("Listening on {addr} (id={id:?})");
    }
    println!("Relay peer id: {local_peer_id}");

    loop {
        tokio::select! {
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(event) => {
                    if let BehaviourEvent::Identify(identify::Event::Received { info: identify::Info { observed_addr, .. }, .. }) = &event {
                        println!("Observed addr: {observed_addr}");
                        swarm.add_external_address(observed_addr.clone());
                    }
                    println!("{event:?}");
                }
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("New listen address: {address}");
                }
                other => {
                    println!("Swarm event: {other:?}");
                }
            },
            _ = tokio::signal::ctrl_c() => {
                println!("Received CTRL+C, shutting down relay");
                break;
            }
        }
    }

    Ok(())
}

fn build_listeners(port: u16, ipv6: bool) -> Vec<Multiaddr> {
    let mut addrs = Vec::new();
    let ipv4_addr = Multiaddr::empty().with(Protocol::from(Ipv4Addr::UNSPECIFIED)).with(Protocol::Tcp(port));
    addrs.push(ipv4_addr);

    if ipv6 {
        let ipv6_addr = Multiaddr::empty().with(Protocol::from(Ipv6Addr::UNSPECIFIED)).with(Protocol::Tcp(port));
        addrs.push(ipv6_addr);
    }

    // Optionally also expose QUIC for clients that support it.
    let ipv4_udp = Multiaddr::empty().with(Protocol::from(Ipv4Addr::UNSPECIFIED)).with(Protocol::Udp(port)).with(Protocol::QuicV1);
    addrs.push(ipv4_udp);

    if ipv6 {
        let ipv6_udp = Multiaddr::empty().with(Protocol::from(Ipv6Addr::UNSPECIFIED)).with(Protocol::Udp(port)).with(Protocol::QuicV1);
        addrs.push(ipv6_udp);
    }

    addrs
}

fn generate_ed25519(seed: u8) -> identity::Keypair {
    let mut bytes = [0u8; 32];
    bytes[0] = seed;
    identity::Keypair::ed25519_from_bytes(bytes).expect("valid length")
}
