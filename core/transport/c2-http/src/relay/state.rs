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
use parking_lot::RwLockWriteGuard;

use crate::relay::authority::{
    ControlError, OwnerReplacement, RouteAuthority, RouteCommand, RouteCommandResult,
};
use crate::relay::conn_pool::{
    AcquireError, CachedClient, ConnectionPool, OwnerToken, UpstreamLease,
};
use crate::relay::route_table::RouteTable;
use crate::relay::types::*;

pub struct RelayState {
    route_table: RwLock<RouteTable>,
    conn_pool: ConnectionPool,
    config: Arc<RelayConfig>,
    disseminator: Arc<dyn crate::relay::disseminator::Disseminator>,
}

#[derive(Clone)]
pub struct OwnerReplacementToken {
    pub existing_address: String,
    pub token: OwnerToken,
}

pub enum RegisterCommitResult {
    Registered { entry: RouteEntry },
    SameOwner { entry: RouteEntry },
    Duplicate { existing_address: String },
    ConflictingOwner { existing_address: String },
}

pub enum UnregisterResult {
    Removed {
        entry: RouteEntry,
        removed_at: f64,
        client: Option<Arc<IpcClient>>,
    },
    AlreadyRemoved,
    NotFound,
    OwnerMismatch,
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

    pub fn commit_register_upstream(
        &self,
        name: String,
        server_id: String,
        address: String,
        crm_ns: String,
        crm_ver: String,
        client: Arc<IpcClient>,
        replacement: Option<OwnerReplacementToken>,
    ) -> RegisterCommitResult {
        let replacement = replacement.map(|token| OwnerReplacement {
            existing_address: token.existing_address,
            token: token.token,
        });
        match RouteAuthority::new(self).execute(RouteCommand::RegisterLocal {
            name,
            server_id,
            address,
            crm_ns,
            crm_ver,
            client,
            replacement,
        }) {
            Ok(RouteCommandResult::Registered { entry }) => {
                RegisterCommitResult::Registered { entry }
            }
            Ok(RouteCommandResult::SameOwner { entry }) => {
                RegisterCommitResult::SameOwner { entry }
            }
            Err(ControlError::AddressMismatch { existing_address }) => {
                RegisterCommitResult::ConflictingOwner { existing_address }
            }
            Err(ControlError::DuplicateRoute { existing_address }) => {
                RegisterCommitResult::Duplicate { existing_address }
            }
            Err(ControlError::InvalidServerId { .. }) | Err(ControlError::InvalidName { .. }) => {
                RegisterCommitResult::ConflictingOwner {
                    existing_address: "<invalid>".to_string(),
                }
            }
            Ok(
                RouteCommandResult::Unregistered { .. }
                | RouteCommandResult::AlreadyUnregistered
                | RouteCommandResult::PeerRouteChanged
                | RouteCommandResult::PeerRoutesRemoved,
            )
            | Err(ControlError::OwnerMismatch)
            | Err(ControlError::NotFound) => RegisterCommitResult::Duplicate {
                existing_address: "<unknown>".to_string(),
            },
        }
    }

    /// Unregister a LOCAL upstream CRM.
    pub fn unregister_upstream(&self, name: &str, server_id: &str) -> UnregisterResult {
        match RouteAuthority::new(self).execute(RouteCommand::UnregisterLocal {
            name: name.to_string(),
            server_id: server_id.to_string(),
        }) {
            Ok(RouteCommandResult::Unregistered {
                entry,
                removed_at,
                client,
            }) => UnregisterResult::Removed {
                entry,
                removed_at,
                client,
            },
            Ok(
                RouteCommandResult::Registered { .. }
                | RouteCommandResult::SameOwner { .. }
                | RouteCommandResult::PeerRouteChanged
                | RouteCommandResult::PeerRoutesRemoved,
            ) => UnregisterResult::OwnerMismatch,
            Ok(RouteCommandResult::AlreadyUnregistered) => UnregisterResult::AlreadyRemoved,
            Err(ControlError::NotFound) => UnregisterResult::NotFound,
            Err(ControlError::OwnerMismatch)
            | Err(ControlError::AddressMismatch { .. })
            | Err(ControlError::InvalidName { .. })
            | Err(ControlError::InvalidServerId { .. }) => UnregisterResult::OwnerMismatch,
            Err(ControlError::DuplicateRoute { .. }) => UnregisterResult::OwnerMismatch,
        }
    }

    pub fn remove_unreachable_local_upstream(
        &self,
        name: &str,
        address: &str,
    ) -> Option<(RouteEntry, f64, Option<Arc<IpcClient>>)> {
        let (entry, removed_at, client) = {
            let mut route_table = self.route_table.write();
            let (entry, removed_at) =
                route_table.unregister_local_route_if_address_matches(name, address);
            let client = if entry.is_some() {
                self.conn_pool.remove(name)
            } else {
                None
            };
            (entry, removed_at, client)
        };
        entry.map(|entry| (entry, removed_at, client))
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

    pub(crate) fn local_route(&self, name: &str) -> Option<RouteEntry> {
        self.route_table.read().local_route(name)
    }

    // -- Connection-only operations --

    pub async fn acquire_upstream(&self, name: &str) -> Result<UpstreamLease, AcquireError> {
        let lease = self
            .conn_pool
            .acquire_with(name, |address| async move {
                let mut client = IpcClient::new(&address);
                client.connect().await?;
                if !client.has_route(name) {
                    client.close().await;
                    return Err(c2_ipc::IpcError::RouteNotFound(name.to_string()));
                }
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

    #[cfg(test)]
    pub(crate) fn get_address(&self, name: &str) -> Option<String> {
        self.conn_pool.get_address(name)
    }

    pub(crate) fn owner_token(&self, name: &str) -> Option<OwnerToken> {
        self.conn_pool.owner_token(name)
    }

    pub(crate) fn matches_owner_token(&self, name: &str, token: &OwnerToken) -> bool {
        self.conn_pool.matches_owner_token(name, token)
    }

    pub(crate) fn can_replace_owner_token(&self, name: &str, token: &OwnerToken) -> bool {
        self.conn_pool.can_replace_owner_token(name, token)
    }

    pub(crate) fn connection_lookup(&self, name: &str) -> CachedClient {
        self.conn_pool.lookup(name)
    }

    pub(crate) fn insert_connection(&self, name: String, address: String, client: Arc<IpcClient>) {
        self.conn_pool.insert(name, address, client);
    }

    pub(crate) fn remove_connection(&self, name: &str) -> Option<Arc<IpcClient>> {
        self.conn_pool.remove(name)
    }

    pub(crate) fn route_table_write(&self) -> RwLockWriteGuard<'_, RouteTable> {
        self.route_table.write()
    }

    pub(crate) fn evict_idle(&self, idle_timeout_ms: u64) -> Vec<(String, Option<Arc<IpcClient>>)> {
        self.conn_pool.evict_idle(idle_timeout_ms)
    }

    #[cfg(test)]
    pub(crate) fn evict_connection(&self, name: &str) -> Option<Arc<IpcClient>> {
        self.conn_pool.evict(name)
    }

    #[cfg(test)]
    pub(crate) fn reconnect(&self, name: &str, client: Arc<IpcClient>) {
        self.conn_pool.reconnect(name, client);
    }

    // -- Peer management --

    #[cfg(test)]
    pub fn register_peer(&self, info: PeerInfo) {
        self.route_table.write().register_peer(info);
    }

    pub fn unregister_peer(&self, relay_id: &str) -> Option<PeerInfo> {
        self.route_table.write().unregister_peer(relay_id)
    }

    pub fn has_peer(&self, relay_id: &str) -> bool {
        self.route_table.read().has_peer(relay_id)
    }

    pub fn peer_is_alive(&self, relay_id: &str) -> bool {
        self.route_table.read().peer_is_alive(relay_id)
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

    pub fn route_digest(&self) -> HashMap<(String, String, bool), u64> {
        self.route_table.read().route_digest()
    }

    pub(crate) fn route_state_for_diff(
        &self,
        name: &str,
        relay_id: &str,
        deleted: bool,
    ) -> Option<crate::relay::peer::DigestDiffEntry> {
        self.route_table
            .read()
            .route_state_for_diff(name, relay_id, deleted)
    }

    pub(crate) fn authoritative_missing_tombstone(
        &self,
        name: &str,
        relay_id: &str,
    ) -> Option<RouteTombstone> {
        self.route_table
            .write()
            .authoritative_missing_tombstone(name, relay_id)
    }

    pub(crate) fn gc_tombstones(&self, retention: std::time::Duration) -> usize {
        self.route_table.write().gc_tombstones(retention)
    }

    pub(crate) fn with_route_table<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&RouteTable) -> R,
    {
        f(&self.route_table.read())
    }

    pub(crate) fn with_route_table_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut RouteTable) -> R,
    {
        f(&mut self.route_table.write())
    }
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

    fn register_local(
        state: &RelayState,
        name: &str,
        server_id: &str,
        address: &str,
        client: Arc<IpcClient>,
    ) -> RouteEntry {
        match state.commit_register_upstream(
            name.to_string(),
            server_id.to_string(),
            address.to_string(),
            String::new(),
            String::new(),
            client,
            None,
        ) {
            RegisterCommitResult::Registered { entry }
            | RegisterCommitResult::SameOwner { entry } => entry,
            RegisterCommitResult::Duplicate { existing_address }
            | RegisterCommitResult::ConflictingOwner { existing_address } => {
                panic!("unexpected duplicate route at {existing_address}")
            }
        }
    }

    fn announce_peer_route(state: &RelayState, entry: RouteEntry) {
        let sender_relay_id = entry.relay_id.clone();
        RouteAuthority::new(state)
            .execute(RouteCommand::AnnouncePeer {
                sender_relay_id,
                entry,
            })
            .unwrap();
    }

    fn withdraw_peer_route(state: &RelayState, name: &str, relay_id: &str) {
        RouteAuthority::new(state)
            .execute(RouteCommand::WithdrawPeer {
                sender_relay_id: relay_id.to_string(),
                name: name.to_string(),
                relay_id: relay_id.to_string(),
                removed_at: 1001.0,
            })
            .unwrap();
    }

    fn remove_peer_routes(state: &RelayState, relay_id: &str) {
        assert!(matches!(
            RouteAuthority::new(state)
                .execute(RouteCommand::RemovePeerRoutes {
                    relay_id: relay_id.to_string(),
                })
                .unwrap(),
            RouteCommandResult::PeerRoutesRemoved
        ));
    }

    #[test]
    fn register_and_resolve_upstream() {
        let state = RelayState::new(test_config(), null_disseminator());
        let client = Arc::new(IpcClient::new("ipc://grid"));
        register_local(&state, "grid", "server-grid", "ipc://grid", client);
        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].ipc_address.as_deref(), Some("ipc://grid"));
        assert_eq!(
            state.list_routes()[0].server_id.as_deref(),
            Some("server-grid")
        );
    }

    #[test]
    fn unregister_upstream() {
        let state = RelayState::new(test_config(), null_disseminator());
        let client = Arc::new(IpcClient::new("ipc://grid"));
        register_local(&state, "grid", "server-grid", "ipc://grid", client);
        assert!(matches!(
            state.unregister_upstream("grid", "server-grid"),
            UnregisterResult::Removed { .. }
        ));
        assert!(state.resolve("grid").is_empty());
    }

    #[test]
    fn unregister_upstream_rejects_wrong_server_id() {
        let state = RelayState::new(test_config(), null_disseminator());
        let client = Arc::new(IpcClient::new("ipc://grid"));
        client.force_connected(true);
        register_local(&state, "grid", "server-grid", "ipc://grid", client);

        assert!(matches!(
            state.unregister_upstream("grid", "server-other"),
            UnregisterResult::OwnerMismatch
        ));
        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].ipc_address.as_deref(), Some("ipc://grid"));
    }

    #[test]
    fn peer_route_operations() {
        let state = RelayState::new(test_config(), null_disseminator());
        state.register_peer(PeerInfo {
            relay_id: "peer-1".into(),
            url: "http://peer-1:8080".into(),
            route_count: 0,
            last_heartbeat: std::time::Instant::now(),
            status: PeerStatus::Alive,
        });
        announce_peer_route(
            &state,
            RouteEntry {
                name: "remote".into(),
                relay_id: "peer-1".into(),
                relay_url: "http://peer-1:8080".into(),
                server_id: None,
                ipc_address: None,
                crm_ns: "ns".into(),
                crm_ver: "0.1.0".into(),
                locality: Locality::Peer,
                registered_at: 1000.0,
            },
        );
        assert_eq!(state.resolve("remote").len(), 1);
        withdraw_peer_route(&state, "remote", "peer-1");
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
        register_local(&state, "grid", "server-grid", "ipc://grid", client);

        // Echo of our own route arriving via DigestDiff with our relay_id.
        let result = RouteAuthority::new(&state).execute(RouteCommand::AnnouncePeer {
            sender_relay_id: "test-relay".into(),
            entry: RouteEntry {
                name: "grid".into(),
                relay_id: "test-relay".into(),
                relay_url: "http://elsewhere:8080".into(),
                server_id: None,
                ipc_address: None,
                crm_ns: "test.ns".into(),
                crm_ver: "0.1.0".into(),
                locality: Locality::Peer,
                registered_at: 1000.0,
            },
        });
        assert!(matches!(result, Err(ControlError::OwnerMismatch)));

        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(
            routes[0].ipc_address.as_deref(),
            Some("ipc://grid"),
            "echoed peer route must not overwrite our LOCAL route"
        );
    }

    #[test]
    fn unregister_peer_route_does_not_remove_local_route() {
        let state = RelayState::new(test_config(), null_disseminator());
        let client = Arc::new(IpcClient::new("ipc://grid"));
        register_local(&state, "grid", "server-grid", "ipc://grid", client);

        let result = RouteAuthority::new(&state).execute(RouteCommand::WithdrawPeer {
            sender_relay_id: "test-relay".into(),
            name: "grid".into(),
            relay_id: "test-relay".into(),
            removed_at: 1001.0,
        });
        assert!(matches!(result, Err(ControlError::OwnerMismatch)));

        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].ipc_address.as_deref(), Some("ipc://grid"));
    }

    #[test]
    fn remove_routes_by_relay_does_not_remove_local_routes() {
        let state = RelayState::new(test_config(), null_disseminator());
        let client = Arc::new(IpcClient::new("ipc://grid"));
        register_local(&state, "grid", "server-grid", "ipc://grid", client);

        remove_peer_routes(&state, "test-relay");

        let routes = state.resolve("grid");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].ipc_address.as_deref(), Some("ipc://grid"));
    }

    #[test]
    fn route_authority_preflight_uses_route_table_when_connection_entry_is_missing() {
        let state = RelayState::new(test_config(), null_disseminator());
        state.with_route_table_mut(|rt| {
            rt.register_route(RouteEntry {
                name: "grid".into(),
                relay_id: "test-relay".into(),
                relay_url: "http://localhost:9999".into(),
                server_id: Some("server-old".into()),
                ipc_address: Some("ipc://grid-old".into()),
                crm_ns: String::new(),
                crm_ver: String::new(),
                locality: Locality::Local,
                registered_at: 1000.0,
            });
        });

        assert!(matches!(
            RouteAuthority::new(&state).register_local_preflight(
                "grid",
                "server-old",
                "ipc://grid-old",
            ),
            Ok(crate::relay::authority::RegisterPreflight::SameOwner)
        ));
        assert!(matches!(
            RouteAuthority::new(&state).register_local_preflight(
                "grid",
                "server-new",
                "ipc://grid-new",
            ),
            Err(ControlError::DuplicateRoute { .. })
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
            "server-first".into(),
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
            "server-second".into(),
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
    fn replacement_token_can_replace_same_slot_only_while_still_evicted() {
        let state = RelayState::new(test_config(), null_disseminator());
        let old = Arc::new(IpcClient::new("ipc://old"));
        old.force_connected(true);
        register_local(&state, "grid", "server-old", "ipc://old", old);
        let old_token = state.owner_token("grid").unwrap();
        state.evict_connection("grid");

        let replacement = Arc::new(IpcClient::new("ipc://new"));
        replacement.force_connected(true);
        let result = state.commit_register_upstream(
            "grid".into(),
            "server-new".into(),
            "ipc://new".into(),
            String::new(),
            String::new(),
            replacement,
            Some(OwnerReplacementToken {
                existing_address: "ipc://old".into(),
                token: old_token,
            }),
        );

        assert!(matches!(result, RegisterCommitResult::Registered { .. }));
        assert_eq!(state.get_address("grid").as_deref(), Some("ipc://new"));
    }

    #[test]
    fn replacement_token_does_not_match_re_registered_same_address_owner() {
        let state = RelayState::new(test_config(), null_disseminator());
        let old = Arc::new(IpcClient::new("ipc://same"));
        old.force_connected(true);
        register_local(&state, "grid", "server-old", "ipc://same", old);
        let old_token = state.owner_token("grid").unwrap();
        assert!(matches!(
            state.unregister_upstream("grid", "server-old"),
            UnregisterResult::Removed { .. }
        ));

        let new_same_address = Arc::new(IpcClient::new("ipc://same"));
        new_same_address.force_connected(true);
        register_local(
            &state,
            "grid",
            "server-new-same-address",
            "ipc://same",
            new_same_address,
        );

        let stale_replacement = Arc::new(IpcClient::new("ipc://replacement"));
        stale_replacement.force_connected(true);
        let result = state.commit_register_upstream(
            "grid".into(),
            "server-racer".into(),
            "ipc://replacement".into(),
            String::new(),
            String::new(),
            stale_replacement,
            Some(OwnerReplacementToken {
                existing_address: "ipc://same".into(),
                token: old_token,
            }),
        );

        assert!(matches!(
            result,
            RegisterCommitResult::Duplicate {
                existing_address
            } if existing_address == "ipc://same"
        ));
        assert_eq!(state.get_address("grid").as_deref(), Some("ipc://same"));
    }

    #[test]
    fn replacement_token_cannot_replace_reconnected_owner_slot() {
        let state = RelayState::new(test_config(), null_disseminator());
        let old = Arc::new(IpcClient::new("ipc://same-slot"));
        old.force_connected(true);
        register_local(&state, "grid", "server-old", "ipc://same-slot", old);
        let old_token = state.owner_token("grid").unwrap();
        state.evict_connection("grid");

        let reconnected_old = Arc::new(IpcClient::new("ipc://same-slot"));
        reconnected_old.force_connected(true);
        state.reconnect("grid", reconnected_old);

        let replacement = Arc::new(IpcClient::new("ipc://replacement"));
        replacement.force_connected(true);
        let result = state.commit_register_upstream(
            "grid".into(),
            "server-new".into(),
            "ipc://replacement".into(),
            String::new(),
            String::new(),
            replacement,
            Some(OwnerReplacementToken {
                existing_address: "ipc://same-slot".into(),
                token: old_token,
            }),
        );

        assert!(matches!(
            result,
            RegisterCommitResult::Duplicate {
                existing_address
            } if existing_address == "ipc://same-slot"
        ));
        assert_eq!(
            state.get_address("grid").as_deref(),
            Some("ipc://same-slot")
        );
    }

    #[test]
    fn same_server_registration_is_idempotent_without_repairing_evicted_client() {
        let state = RelayState::new(test_config(), null_disseminator());
        let original = Arc::new(IpcClient::new("ipc://grid"));
        original.force_connected(true);
        register_local(&state, "grid", "server-grid", "ipc://grid", original);
        state.evict_connection("grid");

        let ignored = Arc::new(IpcClient::new("ipc://grid"));
        ignored.force_connected(true);
        let result = state.commit_register_upstream(
            "grid".into(),
            "server-grid".into(),
            "ipc://grid".into(),
            String::new(),
            String::new(),
            ignored,
            None,
        );

        assert!(matches!(result, RegisterCommitResult::SameOwner { .. }));
        assert!(matches!(
            state.conn_pool.lookup("grid"),
            CachedClient::Evicted { .. }
        ));
    }

    #[test]
    fn same_server_registration_with_different_address_conflicts() {
        let state = RelayState::new(test_config(), null_disseminator());
        let original = Arc::new(IpcClient::new("ipc://old"));
        original.force_connected(true);
        register_local(&state, "grid", "server-grid", "ipc://old", original);

        let moved = Arc::new(IpcClient::new("ipc://new"));
        moved.force_connected(true);
        let result = state.commit_register_upstream(
            "grid".into(),
            "server-grid".into(),
            "ipc://new".into(),
            String::new(),
            String::new(),
            moved,
            None,
        );

        assert!(matches!(
            result,
            RegisterCommitResult::ConflictingOwner {
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
        register_local(&state, "grid", "server-grid", "ipc://grid", client);

        assert!(matches!(
            state.unregister_upstream("grid", "server-grid"),
            UnregisterResult::Removed { .. }
        ));

        assert!(matches!(
            state.acquire_upstream("grid").await,
            Err(AcquireError::NotFound)
        ));
    }

    #[tokio::test]
    async fn same_server_register_does_not_repair_evicted_slot() {
        let state = RelayState::new(test_config(), null_disseminator());
        let original = Arc::new(IpcClient::new("ipc://grid"));
        original.force_connected(true);
        register_local(&state, "grid", "server-grid", "ipc://grid", original);
        state.evict_connection("grid");

        let replacement = Arc::new(IpcClient::new("ipc://grid"));
        replacement.force_connected(true);
        let result = state.commit_register_upstream(
            "grid".into(),
            "server-grid".into(),
            "ipc://grid".into(),
            String::new(),
            String::new(),
            replacement,
            None,
        );

        assert!(matches!(result, RegisterCommitResult::SameOwner { .. }));
        assert!(matches!(
            state.acquire_upstream("grid").await,
            Err(AcquireError::Unreachable { .. })
        ));
    }
}
