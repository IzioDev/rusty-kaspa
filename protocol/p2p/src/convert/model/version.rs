use bitflags::bitflags;
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_core::{
    kaspad_env::{name, version},
    time::unix_now,
};
use kaspa_utils::networking::{NetAddress, PeerId, NET_ADDRESS_SERVICE_LIBP2P_RELAY};

bitflags! {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ServiceFlags: u64 {
        const LIBP2P_RELAY = NET_ADDRESS_SERVICE_LIBP2P_RELAY;
    }
}

/// Maximum allowed length for the user agent field in a version message `VersionMessage`.
pub const MAX_USER_AGENT_LEN: usize = 256;

pub struct Version {
    pub protocol_version: u32,
    pub network: String,
    pub services: ServiceFlags,
    pub timestamp: u64,
    pub address: Option<NetAddress>,
    pub id: PeerId,
    pub user_agent: String,
    pub disable_relay_tx: bool,
    pub subnetwork_id: Option<SubnetworkId>,
    pub relay_port: Option<u16>,
}

impl Version {
    pub fn new(
        address: Option<NetAddress>,
        id: PeerId,
        network: String,
        subnetwork_id: Option<SubnetworkId>,
        protocol_version: u32,
    ) -> Self {
        Self {
            protocol_version,
            network,
            services: ServiceFlags::empty(),
            timestamp: unix_now(),
            address,
            id,
            user_agent: format!("/{}:{}/", name(), version()),
            disable_relay_tx: false,
            subnetwork_id,
            relay_port: None,
        }
    }

    pub fn add_user_agent(&mut self, name: &str, version: &str, comments: &[String]) {
        let comments = if !comments.is_empty() { format!("({})", comments.join("; ")) } else { "".to_string() };
        let new_user_agent = format!("{}:{}{}", name, version, comments);
        self.user_agent = format!("{}{}/", self.user_agent, new_user_agent);
        self.user_agent.truncate(MAX_USER_AGENT_LEN);
    }

    pub fn enable_service(&mut self, flag: ServiceFlags, relay_port: Option<u16>) {
        self.services.insert(flag);
        if flag.contains(ServiceFlags::LIBP2P_RELAY) {
            self.relay_port = relay_port;
        }
    }

    pub fn disable_service(&mut self, flag: ServiceFlags) {
        self.services.remove(flag);
        if flag.contains(ServiceFlags::LIBP2P_RELAY) {
            self.relay_port = None;
        }
    }
}
