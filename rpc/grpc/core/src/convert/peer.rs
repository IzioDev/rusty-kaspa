use std::str::FromStr;

use crate::protowire;
use crate::{from, try_from};
use kaspa_rpc_core::{RpcError, RpcNodeId, RpcPeerAddress};

// ----------------------------------------------------------------------------
// rpc_core to protowire
// ----------------------------------------------------------------------------

from!(item: &kaspa_rpc_core::RpcPeerInfo, protowire::GetConnectedPeerInfoMessage, {
    Self {
        id: item.id.to_string(),
        address: item.address.to_string(),
        last_ping_duration: item.last_ping_duration as i64,
        is_outbound: item.is_outbound,
        time_offset: item.time_offset,
        user_agent: item.user_agent.clone(),
        advertised_protocol_version: item.advertised_protocol_version,
        time_connected: item.time_connected as i64,
        is_ibd_peer: item.is_ibd_peer,
        is_libp2p: item.is_libp2p,
        libp2p_peer_id: item.libp2p_peer_id.clone().unwrap_or_default(),
        libp2p_multiaddr: item.libp2p_multiaddr.clone().unwrap_or_default(),
        libp2p_relay_used: item.libp2p_relay_used.unwrap_or(false),
    }
});

from!(item: &kaspa_rpc_core::RpcPeerAddress, protowire::GetPeerAddressesKnownAddressMessage, { Self { addr: item.to_string() } });
from!(item: &kaspa_rpc_core::RpcIpAddress, protowire::GetPeerAddressesKnownAddressMessage, { Self { addr: item.to_string() } });

// ----------------------------------------------------------------------------
// protowire to rpc_core
// ----------------------------------------------------------------------------

try_from!(item: &protowire::GetConnectedPeerInfoMessage, kaspa_rpc_core::RpcPeerInfo, {
    Self {
        id: RpcNodeId::from_str(&item.id)?,
        address: RpcPeerAddress::from_str(&item.address)?,
        last_ping_duration: item.last_ping_duration as u64,
        is_outbound: item.is_outbound,
        time_offset: item.time_offset,
        user_agent: item.user_agent.clone(),
        advertised_protocol_version: item.advertised_protocol_version,
        time_connected: item.time_connected as u64,
        is_ibd_peer: item.is_ibd_peer,
        is_libp2p: item.is_libp2p,
        libp2p_peer_id: if item.libp2p_peer_id.is_empty() { None } else { Some(item.libp2p_peer_id.clone()) },
        libp2p_multiaddr: if item.libp2p_multiaddr.is_empty() { None } else { Some(item.libp2p_multiaddr.clone()) },
        libp2p_relay_used: if item.is_libp2p { Some(item.libp2p_relay_used) } else { None },
    }
});

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_rpc_core::RpcPeerInfo;
    use kaspa_utils::networking::{NetAddress, PeerId};
    use std::str::FromStr;
    use uuid::Uuid;

    #[test]
    fn libp2p_metadata_roundtrip() {
        let info = RpcPeerInfo {
            id: PeerId::new(Uuid::new_v4()),
            address: NetAddress::from_str("127.0.0.1:16111").unwrap(),
            last_ping_duration: 42,
            is_outbound: true,
            time_offset: 0,
            user_agent: "kaspa-libp2p-test".to_string(),
            advertised_protocol_version: 5,
            time_connected: 7,
            is_ibd_peer: false,
            is_libp2p: true,
            libp2p_peer_id: Some("12D3KooWTestPeer".to_string()),
            libp2p_multiaddr: Some("/ip4/127.0.0.1/tcp/4010".to_string()),
            libp2p_relay_used: Some(true),
        };

        let proto: protowire::GetConnectedPeerInfoMessage = (&info).into();
        assert!(proto.is_libp2p);
        assert_eq!(proto.libp2p_peer_id, "12D3KooWTestPeer");
        assert_eq!(proto.libp2p_multiaddr, "/ip4/127.0.0.1/tcp/4010");
        assert!(proto.libp2p_relay_used);

        let roundtrip = RpcPeerInfo::try_from(&proto).unwrap();
        assert!(roundtrip.is_libp2p);
        assert_eq!(roundtrip.libp2p_peer_id.as_deref(), Some("12D3KooWTestPeer"));
        assert_eq!(roundtrip.libp2p_multiaddr.as_deref(), Some("/ip4/127.0.0.1/tcp/4010"));
        assert_eq!(roundtrip.libp2p_relay_used, Some(true));
    }
}

try_from!(item: &protowire::GetPeerAddressesKnownAddressMessage, kaspa_rpc_core::RpcPeerAddress, { Self::from_str(&item.addr)? });
try_from!(item: &protowire::GetPeerAddressesKnownAddressMessage, kaspa_rpc_core::RpcIpAddress, { Self::from_str(&item.addr)? });
