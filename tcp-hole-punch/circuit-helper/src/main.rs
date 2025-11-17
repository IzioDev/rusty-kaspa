use std::{net::SocketAddr, time::Duration};

use anyhow::{Context, Result};
use clap::Parser;
use humantime::parse_duration;
use kaspa_libp2p_helper_protocol::{Libp2pDialRequest, Libp2pDialResponse};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    time::timeout,
};

#[derive(Parser, Debug)]
#[command(author, version, about = "Kaspa libp2p relay circuit helper")]
struct Args {
    /// One or more relay multiaddrs (comma separated or repeated flag) that include the /p2p/<relay>/p2p-circuit path.
    #[arg(long = "relay-ma", required = true, value_delimiter = ',')]
    relay_multiaddrs: Vec<String>,

    /// Optional relay peer id used to sanity-check the supplied multiaddrs.
    #[arg(long = "relay-peer")]
    relay_peer_id: Option<String>,

    /// The target peer id that should be reached via the relay circuit.
    #[arg(long = "target-peer", required = true)]
    target_peer_id: String,

    /// Control endpoint exposed by the local kaspad instance (format: host:port).
    #[arg(long = "control", env = "KASPA_LIBP2P_HELPER_ADDR", default_value = "127.0.0.1:38081")]
    control: SocketAddr,

    /// Timeout for the dial attempt ( accepts values such as 30s, 1m, 45s ).
    #[arg(long = "timeout", default_value = "30s")]
    timeout: String,
}

impl Args {
    fn timeout_duration(&self) -> Result<Duration> {
        parse_duration(&self.timeout).with_context(|| format!("invalid timeout value '{}'", self.timeout))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let request = Libp2pDialRequest {
        relay_multiaddrs: args.relay_multiaddrs.clone(),
        relay_peer_id: args.relay_peer_id.clone(),
        target_peer_id: args.target_peer_id.clone(),
        timeout_ms: args.timeout_duration()?.as_millis().try_into().ok(),
    };

    let payload = serde_json::to_vec(&request).context("serialize dial request")?;
    println!(
        "Connecting to helper control plane at {} (relay_multiaddrs={:?}, target_peer={})",
        args.control, request.relay_multiaddrs, request.target_peer_id
    );

    let stream = TcpStream::connect(args.control).await.context("connect to helper port")?;
    let (reader, mut writer) = stream.into_split();
    writer.write_all(&payload).await.context("send dial request")?;
    writer.write_all(b"\n").await.context("send delimiter")?;
    writer.flush().await.context("flush request")?;

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let wait_duration = args.timeout_duration()?;
    let read_result = timeout(wait_duration, reader.read_line(&mut line))
        .await
        .context("helper response timed out")?
        .context("read response line")?;
    if read_result == 0 {
        anyhow::bail!("helper closed the connection without responding");
    }

    let mut response: Libp2pDialResponse = serde_json::from_str(line.trim()).context("decode helper response")?;
    response = response.with_target(args.target_peer_id);

    if response.success {
        println!("✓ Circuit established: {}", response.message);
        if let Some(addr) = &response.libp2p_multiaddr {
            println!("  libp2p multiaddr: {addr}");
        }
        if let Some(relay) = response.relay_used {
            println!("  relay used: {relay}");
        }
        if let Some(peer_key) = &response.peer_key {
            println!("  peer key: {peer_key}");
        }
        Ok(())
    } else {
        println!("✗ Dial failed: {}", response.message);
        std::process::exit(1);
    }
}
