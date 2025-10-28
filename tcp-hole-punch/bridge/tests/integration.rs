use std::time::Duration;

use libp2p::identity;
use tokio::time::timeout;

use hole_punch_bridge::{
    swarm::spawn_swarm,
    tonic_integration::incoming_from_handle,
    BridgeError,
};

#[tokio::test]
async fn incoming_helper_returns_once() {
    let mut handle = spawn_swarm(identity::Keypair::generate_ed25519()).expect("spawn");

    let incoming = incoming_from_handle(&mut handle).expect("incoming available");
    drop(incoming);

    assert!(matches!(incoming_from_handle(&mut handle), Err(BridgeError::IncomingClosed)));

    timeout(Duration::from_secs(5), handle.shutdown()).await.expect("shutdown timed out").expect("shutdown");
}
