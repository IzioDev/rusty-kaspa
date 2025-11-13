use std::{
    cmp::min,
    collections::{HashMap, HashSet, VecDeque},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::Arc,
    time::{Duration, SystemTime},
};

use duration_string::DurationString;
use futures_util::future::{join_all, try_join_all};
use itertools::Itertools;
use kaspa_addressmanager::{AddressManager, NetAddress};
use kaspa_core::{debug, info, warn};
use kaspa_p2p_lib::{common::ProtocolError, convert::model::version::ServiceFlags, ConnectionError, Libp2pConnectInfo, Peer, PeerKey};
use kaspa_utils::triggers::SingleTrigger;
use parking_lot::Mutex as ParkingLotMutex;
use rand::{seq::SliceRandom, thread_rng};
use tokio::{
    select,
    sync::{
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
        Mutex as TokioMutex,
    },
    time::{interval, MissedTickBehavior},
};

const MIN_PRIVATE_RELAY_CONNECTIONS: usize = 2;

pub struct ConnectionManager {
    p2p_adaptor: Arc<kaspa_p2p_lib::Adaptor>,
    outbound_target: usize,
    inbound_limit: usize,
    dns_seeders: &'static [&'static str],
    default_port: u16,
    address_manager: Arc<ParkingLotMutex<AddressManager>>,
    connection_requests: TokioMutex<HashMap<SocketAddr, ConnectionRequest>>,
    force_next_iteration: UnboundedSender<()>,
    shutdown_signal: SingleTrigger,
    libp2p_limits: Option<Libp2pLimits>,
}

#[derive(Clone, Debug)]
pub struct Libp2pLimits {
    pub total_inbound: usize,
    pub per_relay: usize,
}

#[derive(Clone, Debug)]
struct ConnectionRequest {
    next_attempt: SystemTime,
    is_permanent: bool,
    attempts: u32,
}

impl ConnectionRequest {
    fn new(is_permanent: bool) -> Self {
        Self { next_attempt: SystemTime::now(), is_permanent, attempts: 0 }
    }
}

impl ConnectionManager {
    pub fn new(
        p2p_adaptor: Arc<kaspa_p2p_lib::Adaptor>,
        outbound_target: usize,
        inbound_limit: usize,
        dns_seeders: &'static [&'static str],
        default_port: u16,
        address_manager: Arc<ParkingLotMutex<AddressManager>>,
        libp2p_limits: Option<Libp2pLimits>,
    ) -> Arc<Self> {
        let (tx, rx) = unbounded_channel::<()>();
        let manager = Arc::new(Self {
            p2p_adaptor,
            outbound_target,
            inbound_limit,
            address_manager,
            connection_requests: Default::default(),
            force_next_iteration: tx,
            shutdown_signal: SingleTrigger::new(),
            dns_seeders,
            default_port,
            libp2p_limits,
        });
        manager.clone().start_event_loop(rx);
        manager.force_next_iteration.send(()).unwrap();
        manager
    }

    fn start_event_loop(self: Arc<Self>, mut rx: UnboundedReceiver<()>) {
        let mut ticker = interval(Duration::from_secs(30));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        tokio::spawn(async move {
            loop {
                if self.shutdown_signal.trigger.is_triggered() {
                    break;
                }
                select! {
                    _ = rx.recv() => self.clone().handle_event().await,
                    _ = ticker.tick() => self.clone().handle_event().await,
                    _ = self.shutdown_signal.listener.clone() => break,
                }
            }
            debug!("Connection manager event loop exiting");
        });
    }

    async fn handle_event(self: Arc<Self>) {
        debug!("Starting connection loop iteration");
        let peers = self.p2p_adaptor.active_peers();
        let peer_by_address: HashMap<SocketAddr, Peer> = peers.into_iter().map(|peer| (peer.net_address(), peer)).collect();

        self.handle_connection_requests(&peer_by_address).await;
        self.handle_outbound_connections(&peer_by_address).await;
        self.handle_inbound_connections(&peer_by_address).await;
    }

    pub async fn add_connection_request(&self, address: SocketAddr, is_permanent: bool) {
        // If the request already exists, it resets the attempts count and overrides the `is_permanent` setting.
        self.connection_requests.lock().await.insert(address, ConnectionRequest::new(is_permanent));
        self.force_next_iteration.send(()).unwrap(); // We force the next iteration of the connection loop.
    }

    pub async fn stop(&self) {
        self.shutdown_signal.trigger.trigger()
    }

    async fn handle_connection_requests(self: &Arc<Self>, peer_by_address: &HashMap<SocketAddr, Peer>) {
        let mut requests = self.connection_requests.lock().await;
        let mut new_requests = HashMap::with_capacity(requests.len());
        for (address, request) in requests.iter() {
            let address = *address;
            let request = request.clone();
            let is_connected = peer_by_address.contains_key(&address);
            if is_connected && !request.is_permanent {
                // The peer is connected and the request is not permanent - no need to keep the request
                continue;
            }

            if !is_connected && request.next_attempt <= SystemTime::now() {
                debug!("Connecting to peer request {}", address);
                match self.p2p_adaptor.connect_peer(address.to_string()).await {
                    Err(err) => {
                        debug!("Failed connecting to peer request: {}, {}", address, err);
                        if request.is_permanent {
                            const MAX_ACCOUNTABLE_ATTEMPTS: u32 = 4;
                            let retry_duration =
                                Duration::from_secs(30u64 * 2u64.pow(min(request.attempts, MAX_ACCOUNTABLE_ATTEMPTS)));
                            debug!("Will retry peer request {} in {}", address, DurationString::from(retry_duration));
                            new_requests.insert(
                                address,
                                ConnectionRequest {
                                    next_attempt: SystemTime::now() + retry_duration,
                                    attempts: request.attempts + 1,
                                    is_permanent: true,
                                },
                            );
                        }
                    }
                    Ok(_) if request.is_permanent => {
                        // Permanent requests are kept forever
                        new_requests.insert(address, ConnectionRequest::new(true));
                    }
                    Ok(_) => {}
                }
            } else {
                new_requests.insert(address, request);
            }
        }

        *requests = new_requests;
    }

    async fn handle_outbound_connections(self: &Arc<Self>, peer_by_address: &HashMap<SocketAddr, Peer>) {
        let active_outbound: HashSet<kaspa_addressmanager::NetAddress> =
            peer_by_address.values().filter(|peer| peer.is_outbound()).map(|peer| peer.net_address().into()).collect();
        if active_outbound.len() >= self.outbound_target {
            return;
        }

        let mut missing_connections = self.outbound_target - active_outbound.len();
        let relay_outbound_target =
            if self.libp2p_limits.is_some() { MIN_PRIVATE_RELAY_CONNECTIONS.min(self.outbound_target) } else { 0 };
        let active_relay_connections = peer_by_address
            .values()
            .filter(|peer| peer.is_outbound() && peer.properties().services.contains(ServiceFlags::LIBP2P_RELAY))
            .count();
        let mut remaining_relay_quota = relay_outbound_target.saturating_sub(active_relay_connections);

        let prioritized_iter = self.address_manager.lock().iterate_prioritized_random_addresses(active_outbound);
        let mut relay_candidates = VecDeque::new();
        let mut regular_candidates = VecDeque::new();
        for addr in prioritized_iter {
            if addr.is_libp2p_relay() {
                relay_candidates.push_back(addr);
            } else {
                regular_candidates.push_back(addr);
            }
        }

        let mut progressing = true;
        while missing_connections > 0 {
            if self.shutdown_signal.trigger.is_triggered() {
                return;
            }
            let addrs_to_connect =
                Self::plan_outbound_batch(&mut relay_candidates, &mut regular_candidates, remaining_relay_quota, missing_connections);
            if addrs_to_connect.is_empty() {
                break;
            }
            let mut jobs = Vec::with_capacity(addrs_to_connect.len());
            for net_addr in &addrs_to_connect {
                let socket_addr = SocketAddr::new(net_addr.ip.into(), net_addr.port).to_string();
                debug!("Connecting to {}", &socket_addr);
                jobs.push(self.p2p_adaptor.connect_peer(socket_addr));
            }

            if progressing && !jobs.is_empty() {
                // Log only if progress was made
                info!(
                    "Connection manager: has {}/{} outgoing P2P connections, trying to obtain {} additional connection(s)...",
                    self.outbound_target - missing_connections,
                    self.outbound_target,
                    jobs.len(),
                );
                progressing = false;
            } else {
                debug!(
                    "Connection manager: outgoing: {}/{} , connecting: {}, iterator: {}",
                    self.outbound_target - missing_connections,
                    self.outbound_target,
                    jobs.len(),
                    relay_candidates.len() + regular_candidates.len(),
                );
            }

            for (res, net_addr) in (join_all(jobs).await).into_iter().zip(addrs_to_connect) {
                match res {
                    Ok(_) => {
                        self.address_manager.lock().mark_connection_success(net_addr);
                        if net_addr.is_libp2p_relay() && remaining_relay_quota > 0 {
                            remaining_relay_quota = remaining_relay_quota.saturating_sub(1);
                        }
                        missing_connections -= 1;
                        progressing = true;
                    }
                    Err(ConnectionError::ProtocolError(ProtocolError::PeerAlreadyExists(_))) => {
                        // We avoid marking the existing connection as connection failure
                        debug!("Failed connecting to {:?}, peer already exists", net_addr);
                    }
                    Err(err) => {
                        debug!("Failed connecting to {:?}, err: {}", net_addr, err);
                        self.address_manager.lock().mark_connection_failure(net_addr);
                    }
                }
            }
        }

        if missing_connections > 0 && !self.dns_seeders.is_empty() {
            if missing_connections > self.outbound_target / 2 {
                // If we are missing more than half of our target, query all in parallel.
                // This will always be the case on new node start-up and is the most resilient strategy in such a case.
                self.dns_seed_many(self.dns_seeders.len()).await;
            } else {
                // Try to obtain at least twice the number of missing connections
                self.dns_seed_with_address_target(2 * missing_connections).await;
            }
        }
    }

    fn plan_outbound_batch(
        relay_candidates: &mut VecDeque<NetAddress>,
        regular_candidates: &mut VecDeque<NetAddress>,
        relay_quota: usize,
        missing_connections: usize,
    ) -> Vec<NetAddress> {
        if missing_connections == 0 {
            return Vec::new();
        }

        let mut batch = Vec::with_capacity(missing_connections);
        let mut relays_planned = 0;
        while relays_planned < relay_quota && batch.len() < missing_connections {
            if let Some(addr) = relay_candidates.pop_front() {
                batch.push(addr);
                relays_planned += 1;
            } else {
                break;
            }
        }

        while batch.len() < missing_connections {
            if let Some(addr) = regular_candidates.pop_front() {
                batch.push(addr);
            } else if let Some(addr) = relay_candidates.pop_front() {
                batch.push(addr);
            } else {
                break;
            }
        }

        batch
    }

    async fn handle_inbound_connections(self: &Arc<Self>, peer_by_address: &HashMap<SocketAddr, Peer>) {
        let active_inbound = peer_by_address.values().filter(|peer| !peer.is_outbound()).collect_vec();
        if let Some(limits) = &self.libp2p_limits {
            self.enforce_libp2p_limits(&active_inbound, limits).await;
        }
        let active_inbound_len = active_inbound.len();
        if self.inbound_limit >= active_inbound_len {
            return;
        }

        let mut futures = Vec::with_capacity(active_inbound_len - self.inbound_limit);
        for peer in active_inbound.choose_multiple(&mut thread_rng(), active_inbound_len - self.inbound_limit) {
            debug!("Disconnecting from {} because we're above the inbound limit", peer.net_address());
            futures.push(self.p2p_adaptor.terminate(peer.key()));
        }
        join_all(futures).await;
    }

    async fn enforce_libp2p_limits(&self, peers: &[&Peer], limits: &Libp2pLimits) {
        let peers_to_drop = Self::select_libp2p_peers_to_drop(peers, limits);
        if peers_to_drop.is_empty() {
            return;
        }
        info!(
            "libp2p inbound limits triggered: removing {} peers (total cap: {}, per relay cap: {})",
            peers_to_drop.len(),
            limits.total_inbound,
            limits.per_relay,
        );
        for key in peers_to_drop {
            debug!("Disconnecting libp2p peer {} due to inbound hole-punch limits", key);
            self.p2p_adaptor.terminate(key).await;
        }
    }

    fn select_libp2p_peers_to_drop(peers: &[&Peer], limits: &Libp2pLimits) -> HashSet<PeerKey> {
        let mut peers_to_drop = HashSet::new();

        if limits.total_inbound > 0 {
            let mut libp2p_peers: Vec<PeerKey> =
                peers.iter().filter(|peer| Self::libp2p_metadata(peer).is_some()).map(|peer| peer.key()).collect();
            if libp2p_peers.len() > limits.total_inbound {
                let mut rng = thread_rng();
                libp2p_peers.shuffle(&mut rng);
                for key in libp2p_peers.into_iter().skip(limits.total_inbound) {
                    peers_to_drop.insert(key);
                }
            }
        }

        if limits.per_relay > 0 {
            let mut per_relay: HashMap<String, usize> = HashMap::new();
            for peer in peers.iter().filter(|peer| Self::libp2p_metadata(peer).map(|info| info.relay_used).unwrap_or(false)) {
                if let Some(relay_id) = Self::relay_peer_id(peer) {
                    let counter = per_relay.entry(relay_id.clone()).or_insert(0);
                    if *counter >= limits.per_relay {
                        peers_to_drop.insert(peer.key());
                    } else {
                        *counter += 1;
                    }
                }
            }
        }

        peers_to_drop
    }

    fn libp2p_metadata(peer: &Peer) -> Option<&Libp2pConnectInfo> {
        peer.connection_metadata().and_then(|metadata| metadata.libp2p.as_ref())
    }

    fn relay_peer_id(peer: &Peer) -> Option<String> {
        let info = Self::libp2p_metadata(peer)?;
        if !info.relay_used {
            return None;
        }
        let multiaddr = info.remote_multiaddr.as_ref()?;
        let mut parts = multiaddr.split("/p2p/").skip(1);
        let relay_section = parts.next()?;
        let relay = relay_section.split('/').next()?.to_string();
        Some(relay)
    }

    /// Queries DNS seeders in random order, one after the other, until obtaining `min_addresses_to_fetch` addresses
    async fn dns_seed_with_address_target(self: &Arc<Self>, min_addresses_to_fetch: usize) {
        let cmgr = self.clone();
        tokio::task::spawn_blocking(move || cmgr.dns_seed_with_address_target_blocking(min_addresses_to_fetch)).await.unwrap();
    }

    fn dns_seed_with_address_target_blocking(self: &Arc<Self>, mut min_addresses_to_fetch: usize) {
        let shuffled_dns_seeders = self.dns_seeders.choose_multiple(&mut thread_rng(), self.dns_seeders.len());
        for &seeder in shuffled_dns_seeders {
            // Query seeders sequentially until reaching the desired number of addresses
            let addrs_len = self.dns_seed_single(seeder);
            if addrs_len >= min_addresses_to_fetch {
                break;
            } else {
                min_addresses_to_fetch -= addrs_len;
            }
        }
    }

    /// Queries `num_seeders_to_query` random DNS seeders in parallel
    async fn dns_seed_many(self: &Arc<Self>, num_seeders_to_query: usize) -> usize {
        info!("Querying {} DNS seeders", num_seeders_to_query);
        let shuffled_dns_seeders = self.dns_seeders.choose_multiple(&mut thread_rng(), num_seeders_to_query);
        let jobs = shuffled_dns_seeders.map(|seeder| {
            let cmgr = self.clone();
            tokio::task::spawn_blocking(move || cmgr.dns_seed_single(seeder))
        });
        try_join_all(jobs).await.unwrap().into_iter().sum()
    }

    /// Query a single DNS seeder and add the obtained addresses to the address manager.
    ///
    /// DNS lookup is a blocking i/o operation so this function is assumed to be called
    /// from a blocking execution context.
    fn dns_seed_single(self: &Arc<Self>, seeder: &str) -> usize {
        info!("Querying DNS seeder {}", seeder);
        // Since the DNS lookup protocol doesn't come with a port, we must assume that the default port is used.
        let addrs = match (seeder, self.default_port).to_socket_addrs() {
            Ok(addrs) => addrs,
            Err(e) => {
                warn!("Error connecting to DNS seeder {}: {}", seeder, e);
                return 0;
            }
        };

        let addrs_len = addrs.len();
        info!("Retrieved {} addresses from DNS seeder {}", addrs_len, seeder);
        let mut amgr_lock = self.address_manager.lock();
        for addr in addrs {
            amgr_lock.add_address(NetAddress::new(addr.ip().into(), addr.port()));
        }

        addrs_len
    }

    /// Bans the given IP and disconnects from all the peers with that IP.
    ///
    /// _GO-KASPAD: BanByIP_
    pub async fn ban(&self, ip: IpAddr) {
        if self.ip_has_permanent_connection(ip).await {
            return;
        }
        for peer in self.p2p_adaptor.active_peers() {
            if peer.net_address().ip() == ip {
                self.p2p_adaptor.terminate(peer.key()).await;
            }
        }
        self.address_manager.lock().ban(ip.into());
    }

    /// Returns whether the given address is banned.
    pub async fn is_banned(&self, address: &SocketAddr) -> bool {
        !self.is_permanent(address).await && self.address_manager.lock().is_banned(address.ip().into())
    }

    /// Returns whether the given address is a permanent request.
    pub async fn is_permanent(&self, address: &SocketAddr) -> bool {
        self.connection_requests.lock().await.contains_key(address)
    }

    /// Returns whether the given IP has some permanent request.
    pub async fn ip_has_permanent_connection(&self, ip: IpAddr) -> bool {
        self.connection_requests.lock().await.iter().any(|(address, request)| request.is_permanent && address.ip() == ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_consensus_core::config::{params::SIMNET_PARAMS, Config};
    use kaspa_core::task::tick::TickService;
    use kaspa_database::create_temp_db;
    use kaspa_database::prelude::ConnBuilder;
    use kaspa_p2p_lib::{ConnectionMetadata, PeerProperties};
    use kaspa_utils::networking::{IpAddress, NetAddress, PeerId, NET_ADDRESS_SERVICE_LIBP2P_RELAY};
    use std::{net::{IpAddr, Ipv4Addr, SocketAddr}, sync::Arc, time::Instant};
    fn make_peer(index: u8, relay: Option<&str>) -> Peer {
        let mut bytes = [0u8; 16];
        bytes[15] = index;
        let peer_id = PeerId::from_slice(&bytes).unwrap();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, index)), 1000 + index as u16);
        let metadata = relay.map(|relay_id| {
            let remote_multiaddr = format!("/ip4/1.1.1.{}/tcp/4001/p2p/{}/p2p-circuit/p2p/{}", index, relay_id, peer_id);
            ConnectionMetadata::new(
                Some(addr),
                Some(Libp2pConnectInfo::with_address(peer_id.to_string(), Some(remote_multiaddr), true)),
            )
        });
        Peer::new(peer_id, addr, false, Instant::now(), Arc::new(PeerProperties::default()), 0, metadata)
    }

    #[test]
    fn total_limit_trims_excess_peers() {
        let peers = vec![make_peer(1, Some("relay-a")), make_peer(2, Some("relay-b")), make_peer(3, Some("relay-c"))];
        let refs: Vec<&Peer> = peers.iter().collect();
        let limits = Libp2pLimits { total_inbound: 2, per_relay: usize::MAX };
        let drop = ConnectionManager::select_libp2p_peers_to_drop(&refs, &limits);
        assert_eq!(drop.len(), 1);
    }

    #[test]
    fn per_relay_limit_enforced() {
        let peers = vec![
            make_peer(10, Some("relay-a")),
            make_peer(11, Some("relay-a")),
            make_peer(12, Some("relay-a")),
            make_peer(13, Some("relay-b")),
        ];
        let refs: Vec<&Peer> = peers.iter().collect();
        let limits = Libp2pLimits { total_inbound: usize::MAX, per_relay: 1 };
        let drop = ConnectionManager::select_libp2p_peers_to_drop(&refs, &limits);
        assert_eq!(drop.len(), 2);
    }

    #[test]
    fn no_drops_when_under_limits() {
        let peers = vec![make_peer(20, Some("relay-a")), make_peer(21, None)];
        let refs: Vec<&Peer> = peers.iter().collect();
        let limits = Libp2pLimits { total_inbound: 5, per_relay: 3 };
        let drop = ConnectionManager::select_libp2p_peers_to_drop(&refs, &limits);
        assert!(drop.is_empty());
    }

    #[test]
    fn relay_candidates_used_until_quota_met() {
        let mut relays = VecDeque::from(vec![relay_addr(1001), relay_addr(1002)]);
        let mut regular = VecDeque::from(vec![plain_addr(2001), plain_addr(2002)]);
        let batch = ConnectionManager::plan_outbound_batch(&mut relays, &mut regular, 2, 3);
        assert_eq!(batch.len(), 3);
        assert!(batch[0].is_libp2p_relay());
        assert!(batch[1].is_libp2p_relay());
        assert!(!batch[2].is_libp2p_relay());
    }

    #[test]
    fn regular_addresses_take_precedence_once_relays_met() {
        let mut relays = VecDeque::from(vec![relay_addr(1010)]);
        let mut regular = VecDeque::from(vec![plain_addr(2010), plain_addr(2011)]);
        let batch = ConnectionManager::plan_outbound_batch(&mut relays, &mut regular, 1, 3);
        assert!(batch[0].is_libp2p_relay());
        assert!(!batch[1].is_libp2p_relay());
        assert!(!batch[2].is_libp2p_relay());
    }

    #[test]
    fn relays_fill_remaining_slots_when_no_regulars() {
        let mut relays = VecDeque::from(vec![relay_addr(3000), relay_addr(3001), relay_addr(3002)]);
        let mut regular = VecDeque::new();
        let batch = ConnectionManager::plan_outbound_batch(&mut relays, &mut regular, 1, 3);
        assert_eq!(batch.iter().filter(|addr| addr.is_libp2p_relay()).count(), 3);
    }

    #[test]
    fn address_manager_metadata_reaches_planner() {
        let (_db_lifetime, db) = create_temp_db!(ConnBuilder::default().with_files_limit(10));
        let config = Arc::new(Config::new(SIMNET_PARAMS));
        let (address_manager, _) = AddressManager::new(config, db, Arc::new(TickService::default()));
        {
            let mut guard = address_manager.lock();
            guard.add_address(relay_addr(4001));
            guard.add_address(relay_addr(4002));
            guard.add_address(plain_addr(5001));
        }

        let scoped_addresses = {
            let guard = address_manager.lock();
            guard
                .iterate_addresses()
                .filter(|addr| (4000..6000).contains(&addr.port))
                .collect_vec()
        };
        assert_eq!(scoped_addresses.len(), 3);

        let mut relay_candidates = VecDeque::new();
        let mut regular_candidates = VecDeque::new();
        for addr in scoped_addresses {
            if addr.is_libp2p_relay() {
                relay_candidates.push_back(addr);
            } else {
                regular_candidates.push_back(addr);
            }
        }

        let batch = ConnectionManager::plan_outbound_batch(&mut relay_candidates, &mut regular_candidates, 2, 3);
        assert_eq!(batch.iter().filter(|addr| addr.is_libp2p_relay()).count(), 2);
    }

    fn plain_addr(port: u16) -> NetAddress {
        NetAddress::new(test_ip(port).into(), port)
    }

    fn relay_addr(port: u16) -> NetAddress {
        plain_addr(port).with_services(NET_ADDRESS_SERVICE_LIBP2P_RELAY)
    }

    fn test_ip(offset: u16) -> IpAddress {
        IpAddress::from(IpAddr::V4(Ipv4Addr::new(198, 51, (offset % 200) as u8, (offset % 250) as u8)))
    }
}
