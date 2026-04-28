//! Central relay state — thread-safe wrapper around RouteTable + ConnectionPool.
//!
//! Lock ordering (must be followed everywhere):
//!   1. route_table (RwLock)
//!   2. conn_pool (internal slot mutexes)

use std::collections::HashMap;
use std::sync::Arc;

use c2_config::RelayConfig;
use c2_ipc::IpcClient;
use parking_lot::RwLock;

use crate::relay::conn_pool::{AcquireError, CachedClient, ConnectionPool, UpstreamLease};
use crate::relay::route_table::RouteTable;
use crate::relay::types::*;

pub struct RelayState {
    route_table: RwLock<RouteTable>,
    conn_pool: ConnectionPool,
    config: Arc<RelayConfig>,
    disseminator: Arc<dyn crate::relay::disseminator::Disseminator>,
}

#[derive(Clone)]
pub enum LocalOwnerStatus {
    NoOwner,
    SameAddress,
    DifferentAddressReady {
        existing_address: String,
    },
    DifferentAddressNeedsProbe {
        existing_address: String,
        generation: u64,
    },
}

#[derive(Clone)]
pub struct OwnerReplacementToken {
    pub existing_address: String,
    pub generation: u64,
}

pub enum RegisterCommitResult {
    Registered { entry: RouteEntry },
    SameOwner { entry: RouteEntry },
    Duplicate { existing_address: String },
}

impl RelayState {
    pub fn new(
        config: Arc<RelayConfig>,
        disseminator: Arc<dyn crate::relay::disseminator::Disseminator>,
    ) -> Self {
        Self {
            route_table: RwLock::new(RouteTable::new(config.relay_id.clone())),
            conn_pool: ConnectionPool::new(),
            disseminator,
            config,
        }
    }

    pub fn disseminator(&self) -> &Arc<dyn crate::relay::disseminator::Disseminator> {
        &self.disseminator
    }

    pub fn config(&self) -> &RelayConfig {
        &self.config
    }
    pub fn relay_id(&self) -> &str {
        &self.config.relay_id
    }

    // -- Transactional: route + connection together --

    /// Register a LOCAL upstream CRM.
    pub fn register_upstream(
        &self,
        name: String,
        address: String,
        crm_ns: String,
        crm_ver: String,
        client: Arc<IpcClient>,
    ) -> RouteEntry {
        match self.commit_register_upstream(name, address, crm_ns, crm_ver, client, None) {
            RegisterCommitResult::Registered { entry }
            | RegisterCommitResult::SameOwner { entry } => entry,
            RegisterCommitResult::Duplicate { existing_address } => {
                panic!("duplicate upstream registration for existing address {existing_address}")
            }
        }
    }

    pub fn commit_register_upstream(
        &self,
        name: String,
        address: String,
        crm_ns: String,
        crm_ver: String,
        client: Arc<IpcClient>,
        replacement: Option<OwnerReplacementToken>,
    ) -> RegisterCommitResult {
        let mut route_table = self.route_table.write();
        if let Some(existing) = route_table.local_route(&name) {
            let existing_address = existing.ipc_address.clone().unwrap_or_default();
            if existing_address == address {
                if matches!(self.conn_pool.lookup(&name), CachedClient::Ready { .. }) {
                    return RegisterCommitResult::SameOwner { entry: existing };
                }
                self.conn_pool.insert(name, address, client);
                return RegisterCommitResult::Registered { entry: existing };
            }
            match replacement {
                Some(token) if token.existing_address == existing_address => {
                    match self.conn_pool.reconnect_candidate(&name) {
                        Some((slot_address, generation))
                            if slot_address == existing_address
                                && generation == token.generation => {}
                        _ => return RegisterCommitResult::Duplicate { existing_address },
                    }
                }
                _ => return RegisterCommitResult::Duplicate { existing_address },
            }
        }

        let entry = RouteEntry {
            name: name.clone(),
            relay_id: self.config.relay_id.clone(),
            relay_url: self.config.effective_advertise_url(),
            ipc_address: Some(address.clone()),
            crm_ns,
            crm_ver,
            locality: Locality::Local,
            registered_at: now_secs(),
        };
        route_table.register_route(entry.clone());
        self.conn_pool.insert(name, address, client);
        RegisterCommitResult::Registered { entry }
    }

    /// Unregister a LOCAL upstream CRM.
    pub fn unregister_upstream(&self, name: &str) -> Option<(RouteEntry, Option<Arc<IpcClient>>)> {
        let relay_id = self.config.relay_id.clone();
        let entry = self.route_table.write().unregister_route(name, &relay_id);
        let client = self.conn_pool.remove(name);
        entry.map(|e| (e, client))
    }

    // -- Route-only operations --

    pub fn resolve(&self, name: &str) -> Vec<RouteInfo> {
        self.route_table.read().resolve(name)
    }

    pub fn route_names(&self) -> Vec<String> {
        self.route_table.read().route_names()
    }

    pub fn list_routes(&self) -> Vec<RouteEntry> {
        self.route_table.read().list_routes()
    }

    // -- Connection-only operations --

    pub async fn acquire_upstream(&self, name: &str) -> Result<UpstreamLease, AcquireError> {
        let lease = self
            .conn_pool
            .acquire_with(name, |address| async move {
                let mut client = IpcClient::new(&address);
                client.connect().await?;
                Ok(Arc::new(client))
            })
            .await?;

        let route_matches_lease = self
            .route_table
            .read()
            .local_route(name)
            .and_then(|entry| entry.ipc_address)
            .is_some_and(|address| address == lease.address());

        if route_matches_lease {
            Ok(lease)
        } else {
            let client = lease.client();
            drop(lease);
            client.close_shared().await;
            Err(AcquireError::NotFound)
        }
    }

    pub fn get_address(&self, name: &str) -> Option<String> {
        self.conn_pool.get_address(name)
    }

    pub fn reconnect_candidate(&self, name: &str) -> Option<(String, u64)> {
        self.conn_pool.reconnect_candidate(name)
    }

    pub fn evict_idle(&self, idle_timeout_ms: u64) -> Vec<(String, Option<Arc<IpcClient>>)> {
        self.conn_pool.evict_idle(idle_timeout_ms)
    }

    pub fn evict_connection(&self, name: &str) -> Option<Arc<IpcClient>> {
        self.conn_pool.evict(name)
    }

    pub fn evict_connection_generation(
        &self,
        name: &str,
        generation: u64,
    ) -> Option<Arc<IpcClient>> {
        self.conn_pool.evict_generation(name, generation)
    }

    pub fn reconnect(&self, name: &str, client: Arc<IpcClient>) {
        self.conn_pool.reconnect(name, client);
    }

    pub fn check_local_owner(&self, name: &str, address: &str) -> LocalOwnerStatus {
        let local_route = self.route_table.read().local_route(name);
        match self.conn_pool.lookup(name) {
            CachedClient::Ready {
                address: existing_address,
                ..
            } => {
                if existing_address == address {
                    LocalOwnerStatus::SameAddress
                } else {
                    LocalOwnerStatus::DifferentAddressReady { existing_address }
                }
            }
            CachedClient::Evicted {
                address: existing_address,
                generation,
            }
            | CachedClient::Disconnected {
                address: existing_address,
                generation,
            } => {
                if existing_address == address {
                    LocalOwnerStatus::SameAddress
                } else {
                    LocalOwnerStatus::DifferentAddressNeedsProbe {
                        existing_address,
                        generation,
                    }
                }
            }
            CachedClient::Missing => {
                let Some(existing_address) = local_route.and_then(|entry| entry.ipc_address) else {
                    return LocalOwnerStatus::NoOwner;
                };
                if existing_address == address {
                    LocalOwnerStatus::SameAddress
                } else {
                    LocalOwnerStatus::DifferentAddressReady { existing_address }
                }
            }
        }
    }

    // -- PEER route operations (gossip) --

    pub fn register_peer_route(&self, entry: RouteEntry) {
        // Never overwrite a LOCAL route with a peer-sourced one. Anti-entropy
        // can echo our own routes back to us with `relay_id == our id`; if we
        // accepted those, we'd silently replace `Locality::Local` with
        // `Locality::Peer` and lose our own ipc_address.
        if entry.relay_id == self.route_table.read().relay_id() {
            return;
        }
        self.route_table.write().register_route(entry);
    }

    pub fn unregister_peer_route(&self, name: &str, relay_id: &str) {
        self.route_table.write().unregister_route(name, relay_id);
    }

    pub fn remove_routes_by_relay(&self, relay_id: &str) -> Vec<RouteEntry> {
        self.route_table.write().remove_routes_by_relay(relay_id)
    }

    // -- Peer management --

    pub fn register_peer(&self, info: PeerInfo) {
        self.route_table.write().register_peer(info);
    }

    pub fn unregister_peer(&self, relay_id: &str) -> Option<PeerInfo> {
        self.route_table.write().unregister_peer(relay_id)
    }

    pub fn list_peers(&self) -> Vec<PeerSnapshot> {
        self.route_table
            .read()
            .list_peers()
            .into_iter()
            .map(|p| PeerSnapshot {
                relay_id: p.relay_id.clone(),
                url: p.url.clone(),
                route_count: p.route_count,
                status: p.status,
            })
            .collect()
    }

    pub fn local_route_count(&self) -> u32 {
        self.route_table.read().local_route_count()
    }

    // -- Snapshot operations --

    pub fn full_snapshot(&self) -> FullSync {
        self.route_table.read().full_snapshot()
    }

    pub fn merge_snapshot(&self, sync: FullSync) {
        self.route_table.write().merge_snapshot(sync);
    }

    pub fn route_digest(&self) -> HashMap<(String, String), u64> {
        self.route_table.read().route_digest()
    }

    pub fn with_route_table<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&RouteTable) -> R,
    {
        f(&self.route_table.read())
    }

    pub fn with_route_table_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut RouteTable) -> R,
    {
        f(&mut self.route_table.write())
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullDisseminator;
    impl crate::relay::disseminator::Disseminator for NullDisseminator {
        fn broadcast(
            &self,
            _envelope: crate::relay::peer::PeerEnvelope,
            _peers: &[PeerSnapshot],
        ) -> Option<tokio::task::JoinHandle<()>> {
            None
        }
    }

    fn test_config() -> Arc<RelayConfig> {
        Arc::new(RelayConfig {
            relay_id: "test-relay".into(),
            advertise_url: "http://localhost:9999".into(),
            ..Default::default()
        })
    }

    fn null_disseminator() -> Arc<dyn crate::relay::disseminator::Disseminator> {
        Arc::new(NullDisseminator)
    }

    #[test]
    fn register_and_resolve_upstream() {
        let state = RelayState::new(test_config(), null_disseminator());
        let client = Arc::new(IpcClient::new("ipc://grid"));
        state.register_upstream(
            "grid".into(),
            "ipc://grid".into(),
            "test.ns".into(),
            "0.1.0".into(),
            client,
        );
        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].ipc_address.as_deref(), Some("ipc://grid"));
    }

    #[test]
    fn unregister_upstream() {
        let state = RelayState::new(test_config(), null_disseminator());
        let client = Arc::new(IpcClient::new("ipc://grid"));
        state.register_upstream(
            "grid".into(),
            "ipc://grid".into(),
            "test.ns".into(),
            "0.1.0".into(),
            client,
        );
        assert!(state.unregister_upstream("grid").is_some());
        assert!(state.resolve("grid").is_empty());
    }

    #[test]
    fn peer_route_operations() {
        let state = RelayState::new(test_config(), null_disseminator());
        state.register_peer_route(RouteEntry {
            name: "remote".into(),
            relay_id: "peer-1".into(),
            relay_url: "http://peer-1:8080".into(),
            ipc_address: None,
            crm_ns: "ns".into(),
            crm_ver: "0.1.0".into(),
            locality: Locality::Peer,
            registered_at: 1000.0,
        });
        assert_eq!(state.resolve("remote").len(), 1);
        state.unregister_peer_route("remote", "peer-1");
        assert!(state.resolve("remote").is_empty());
    }

    #[test]
    fn register_peer_route_does_not_overwrite_local() {
        // Anti-entropy can echo our own routes back to us. The peer-route
        // entry path MUST refuse anything carrying our own relay_id, or it
        // would silently demote a Local route (with ipc_address) to a Peer
        // route (without ipc_address) and break local IPC dispatch.
        let state = RelayState::new(test_config(), null_disseminator());
        let client = Arc::new(IpcClient::new("ipc://grid"));
        state.register_upstream(
            "grid".into(),
            "ipc://grid".into(),
            "test.ns".into(),
            "0.1.0".into(),
            client,
        );

        // Echo of our own route arriving via DigestDiff with our relay_id.
        state.register_peer_route(RouteEntry {
            name: "grid".into(),
            relay_id: "test-relay".into(),
            relay_url: "http://elsewhere:8080".into(),
            ipc_address: None,
            crm_ns: "test.ns".into(),
            crm_ver: "0.1.0".into(),
            locality: Locality::Peer,
            registered_at: 1000.0,
        });

        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(
            routes[0].ipc_address.as_deref(),
            Some("ipc://grid"),
            "echoed peer route must not overwrite our LOCAL route"
        );
    }

    #[test]
    fn local_owner_check_uses_route_table_when_connection_entry_is_missing() {
        let state = RelayState::new(test_config(), null_disseminator());
        state.with_route_table_mut(|rt| {
            rt.register_route(RouteEntry {
                name: "grid".into(),
                relay_id: "test-relay".into(),
                relay_url: "http://localhost:9999".into(),
                ipc_address: Some("ipc://grid-old".into()),
                crm_ns: String::new(),
                crm_ver: String::new(),
                locality: Locality::Local,
                registered_at: 1000.0,
            });
        });

        assert!(matches!(
            state.check_local_owner("grid", "ipc://grid-old"),
            LocalOwnerStatus::SameAddress
        ));
        assert!(matches!(
            state.check_local_owner("grid", "ipc://grid-new"),
            LocalOwnerStatus::DifferentAddressReady { .. }
        ));
    }

    #[test]
    fn register_commit_rechecks_owner_after_preflight_no_owner() {
        let state = RelayState::new(test_config(), null_disseminator());
        let first = Arc::new(IpcClient::new("ipc://first"));
        first.force_connected(true);
        let second = Arc::new(IpcClient::new("ipc://second"));
        second.force_connected(true);

        let first_result = state.commit_register_upstream(
            "grid".into(),
            "ipc://first".into(),
            String::new(),
            String::new(),
            first,
            None,
        );
        assert!(matches!(
            first_result,
            RegisterCommitResult::Registered { .. }
        ));

        let second_result = state.commit_register_upstream(
            "grid".into(),
            "ipc://second".into(),
            String::new(),
            String::new(),
            second,
            None,
        );
        assert!(matches!(
            second_result,
            RegisterCommitResult::Duplicate {
                existing_address
            } if existing_address == "ipc://first"
        ));
        assert_eq!(state.get_address("grid").as_deref(), Some("ipc://first"));
    }

    #[test]
    fn replacement_token_must_match_current_owner_generation() {
        let state = RelayState::new(test_config(), null_disseminator());
        let old = Arc::new(IpcClient::new("ipc://old"));
        old.force_connected(true);
        state.register_upstream(
            "grid".into(),
            "ipc://old".into(),
            String::new(),
            String::new(),
            old,
        );
        let (_, old_generation) = state.reconnect_candidate("grid").unwrap();

        let refreshed = Arc::new(IpcClient::new("ipc://old"));
        refreshed.force_connected(true);
        state.reconnect("grid", refreshed);

        let replacement = Arc::new(IpcClient::new("ipc://new"));
        replacement.force_connected(true);
        let result = state.commit_register_upstream(
            "grid".into(),
            "ipc://new".into(),
            String::new(),
            String::new(),
            replacement,
            Some(OwnerReplacementToken {
                existing_address: "ipc://old".into(),
                generation: old_generation,
            }),
        );

        assert!(matches!(
            result,
            RegisterCommitResult::Duplicate {
                existing_address
            } if existing_address == "ipc://old"
        ));
        assert_eq!(state.get_address("grid").as_deref(), Some("ipc://old"));
    }

    #[tokio::test]
    async fn acquire_after_unregister_reports_not_found() {
        let state = RelayState::new(test_config(), null_disseminator());
        let client = Arc::new(IpcClient::new("ipc://grid"));
        client.force_connected(true);
        state.register_upstream(
            "grid".into(),
            "ipc://grid".into(),
            String::new(),
            String::new(),
            client,
        );

        assert!(state.unregister_upstream("grid").is_some());

        assert!(matches!(
            state.acquire_upstream("grid").await,
            Err(AcquireError::NotFound)
        ));
    }

    #[tokio::test]
    async fn same_address_register_repairs_evicted_slot() {
        let state = RelayState::new(test_config(), null_disseminator());
        let original = Arc::new(IpcClient::new("ipc://grid"));
        original.force_connected(true);
        state.register_upstream(
            "grid".into(),
            "ipc://grid".into(),
            String::new(),
            String::new(),
            original,
        );
        state.evict_connection("grid");

        let replacement = Arc::new(IpcClient::new("ipc://grid"));
        replacement.force_connected(true);
        let result = state.commit_register_upstream(
            "grid".into(),
            "ipc://grid".into(),
            String::new(),
            String::new(),
            replacement,
            None,
        );

        assert!(matches!(result, RegisterCommitResult::Registered { .. }));
        assert!(state.acquire_upstream("grid").await.is_ok());
    }
}
