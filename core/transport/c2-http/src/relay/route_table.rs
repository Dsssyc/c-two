use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use c2_wire::control::MAX_CALL_ROUTE_NAME_BYTES;

use crate::relay::types::*;

/// Route table — owns all route entries and peer info.
///
/// Primary key for routes: (name, relay_id).
/// Resolve returns LOCAL first, then PEER sorted by (registered_at, relay_id).
pub struct RouteTable {
    /// Routes keyed by (name, relay_id) for O(1) upsert/delete.
    routes: HashMap<(String, String), RouteEntry>,
    /// Deleted routes keyed by (name, relay_id). Tombstones are short-lived
    /// control-plane state used by anti-entropy to converge deletions.
    tombstones: HashMap<(String, String), RouteTombstone>,
    /// Known peer relays keyed by relay_id.
    peers: HashMap<String, PeerInfo>,
    /// This relay's ID (for distinguishing LOCAL vs PEER).
    relay_id: String,
    /// Per-relay hybrid logical timestamp encoded as epoch milliseconds.
    /// Local events use max(current wall milliseconds, previous + 1), keeping
    /// process-local monotonicity while still outranking stale pre-restart
    /// logical counters seen through anti-entropy.
    next_timestamp: f64,
}

impl RouteTable {
    pub fn new(relay_id: String) -> Self {
        Self {
            routes: HashMap::new(),
            tombstones: HashMap::new(),
            peers: HashMap::new(),
            relay_id,
            next_timestamp: current_epoch_millis().saturating_sub(1) as f64,
        }
    }

    pub fn relay_id(&self) -> &str {
        &self.relay_id
    }

    // -- Route operations --

    /// Register or update a route (upsert semantics).
    pub fn register_route(&mut self, entry: RouteEntry) -> bool {
        if !entry.registered_at.is_finite() {
            return false;
        }
        let key = (entry.name.clone(), entry.relay_id.clone());
        if let Some(tombstone) = self.tombstones.get(&key) {
            if entry.registered_at <= tombstone.removed_at {
                return false;
            }
        }
        self.tombstones.remove(&key);
        self.routes.insert(key, entry);
        true
    }

    pub fn unregister_route_with_tombstone(
        &mut self,
        name: &str,
        relay_id: &str,
        removed_at: f64,
    ) -> Option<RouteEntry> {
        if !removed_at.is_finite() {
            return None;
        }
        let key = (name.to_string(), relay_id.to_string());
        let removed = match self.routes.get(&key) {
            Some(entry) if entry.registered_at > removed_at => None,
            _ => self.routes.remove(&key),
        };
        self.apply_tombstone(RouteTombstone {
            name: name.to_string(),
            relay_id: relay_id.to_string(),
            removed_at,
            server_id: None,
            observed_at: Instant::now(),
        });
        removed
    }

    pub fn unregister_local_route_with_tombstone(
        &mut self,
        name: &str,
        server_id: &str,
    ) -> (Option<RouteEntry>, f64) {
        let relay_id = self.relay_id.clone();
        let key = (name.to_string(), relay_id.clone());
        let removed_at = self.next_local_timestamp();
        let removed = match self.routes.get(&key) {
            Some(entry) if entry.registered_at > removed_at => None,
            _ => self.routes.remove(&key),
        };
        self.apply_tombstone(RouteTombstone {
            name: name.to_string(),
            relay_id,
            removed_at,
            server_id: Some(server_id.to_string()),
            observed_at: Instant::now(),
        });
        (removed, removed_at)
    }

    pub fn unregister_local_route_if_address_matches(
        &mut self,
        name: &str,
        address: &str,
    ) -> (Option<RouteEntry>, f64) {
        let relay_id = self.relay_id.clone();
        let key = (name.to_string(), relay_id.clone());
        let matches_address = self
            .routes
            .get(&key)
            .and_then(|entry| entry.ipc_address.as_deref())
            .is_some_and(|stored| stored == address);
        if !matches_address {
            return (None, self.next_local_timestamp());
        }
        let removed_at = self.next_local_timestamp();
        let removed = match self.routes.get(&key) {
            Some(entry) if entry.registered_at > removed_at => None,
            _ => self.routes.remove(&key),
        };
        let server_id = removed.as_ref().and_then(|entry| entry.server_id.clone());
        self.apply_tombstone(RouteTombstone {
            name: name.to_string(),
            relay_id,
            removed_at,
            server_id,
            observed_at: Instant::now(),
        });
        (removed, removed_at)
    }

    pub fn next_local_timestamp(&mut self) -> f64 {
        let wall = current_epoch_millis() as f64;
        self.next_timestamp = if self.next_timestamp.is_finite() {
            wall.max(self.next_timestamp + 1.0)
        } else {
            wall
        };
        self.next_timestamp
    }

    pub fn local_tombstone_matches_server(&self, name: &str, server_id: &str) -> bool {
        self.tombstones
            .get(&(name.to_string(), self.relay_id.clone()))
            .and_then(|tombstone| tombstone.server_id.as_deref())
            .is_some_and(|stored| stored == server_id)
    }

    pub fn apply_tombstone(&mut self, mut tombstone: RouteTombstone) -> bool {
        if !tombstone.removed_at.is_finite() {
            return false;
        }
        let key = (tombstone.name.clone(), tombstone.relay_id.clone());
        if let Some(entry) = self.routes.get(&key) {
            if entry.registered_at > tombstone.removed_at {
                return false;
            }
        }
        if let Some(existing) = self.tombstones.get(&key) {
            if existing.removed_at >= tombstone.removed_at {
                return false;
            }
            if tombstone.server_id.is_none() {
                tombstone.server_id = existing.server_id.clone();
            }
        }
        tombstone.observed_at = Instant::now();
        self.routes.remove(&key);
        self.tombstones.insert(key, tombstone);
        true
    }

    pub fn local_route(&self, name: &str) -> Option<RouteEntry> {
        self.routes
            .get(&(name.to_string(), self.relay_id.clone()))
            .cloned()
    }

    /// Resolve a name → ordered list of RouteInfo.
    /// LOCAL first, then PEER sorted by (registered_at, relay_id).
    pub fn resolve(&self, name: &str) -> Vec<RouteInfo> {
        let mut local = Vec::new();
        let mut peers = Vec::new();

        for ((n, _), entry) in &self.routes {
            if n != name {
                continue;
            }
            match entry.locality {
                Locality::Local => local.push(entry.to_route_info()),
                Locality::Peer if self.peer_is_alive(&entry.relay_id) => peers.push(entry),
                Locality::Peer => {}
            }
        }

        // Deterministic sort: (registered_at, relay_id) ascending.
        peers.sort_by(|a, b| {
            a.registered_at
                .partial_cmp(&b.registered_at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.relay_id.cmp(&b.relay_id))
        });

        let mut result = local;
        result.extend(peers.into_iter().map(|e| e.to_route_info()));
        result
    }

    /// List all routes.
    pub fn list_routes(&self) -> Vec<RouteEntry> {
        self.routes.values().cloned().collect()
    }

    pub fn list_tombstones(&self) -> Vec<RouteTombstone> {
        self.tombstones.values().cloned().collect()
    }

    /// List route names (unique).
    pub fn route_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.routes.values().map(|e| e.name.clone()).collect();
        names.sort();
        names.dedup();
        names
    }

    /// Count of LOCAL routes only (for heartbeat reporting).
    pub fn local_route_count(&self) -> u32 {
        self.routes
            .values()
            .filter(|e| e.locality == Locality::Local)
            .count() as u32
    }

    /// Remove all routes from a specific relay.
    pub fn remove_routes_by_relay(&mut self, relay_id: &str) -> Vec<RouteEntry> {
        let keys: Vec<_> = self
            .routes
            .keys()
            .filter(|(_, rid)| rid == relay_id)
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|k| self.routes.remove(&k))
            .collect()
    }

    pub fn gc_tombstones(&mut self, retention: Duration) -> usize {
        let now = Instant::now();
        let before = self.tombstones.len();
        self.tombstones.retain(|key, tombstone| {
            if self.routes.contains_key(key) {
                return false;
            }
            now.duration_since(tombstone.observed_at) < retention
        });
        before - self.tombstones.len()
    }

    // -- Peer operations --

    #[cfg(test)]
    pub fn register_peer(&mut self, info: PeerInfo) {
        let relay_id = info.relay_id.clone();
        let url = info.url.clone();
        self.peers.insert(relay_id.clone(), info);
        self.sync_peer_route_urls(&relay_id, &url);
    }

    pub fn record_peer_join(&mut self, relay_id: String, url: String) {
        let now = Instant::now();
        match self.peers.get_mut(&relay_id) {
            Some(peer) => {
                peer.url = url.clone();
                peer.last_heartbeat = now;
                peer.status = PeerStatus::Alive;
            }
            None => {
                self.peers.insert(
                    relay_id.clone(),
                    PeerInfo {
                        relay_id: relay_id.clone(),
                        url: url.clone(),
                        route_count: 0,
                        last_heartbeat: now,
                        status: PeerStatus::Alive,
                    },
                );
            }
        }
        self.sync_peer_route_urls(&relay_id, &url);
    }

    pub fn unregister_peer(&mut self, relay_id: &str) -> Option<PeerInfo> {
        self.peers.remove(relay_id)
    }

    pub fn get_peer(&self, relay_id: &str) -> Option<&PeerInfo> {
        self.peers.get(relay_id)
    }

    pub fn has_peer(&self, relay_id: &str) -> bool {
        self.peers.contains_key(relay_id)
    }

    pub fn peer_is_alive(&self, relay_id: &str) -> bool {
        self.peers
            .get(relay_id)
            .is_some_and(|peer| peer.status == PeerStatus::Alive)
    }

    pub fn get_peer_mut(&mut self, relay_id: &str) -> Option<&mut PeerInfo> {
        self.peers.get_mut(relay_id)
    }

    pub fn list_peers(&self) -> Vec<&PeerInfo> {
        self.peers.values().collect()
    }

    pub fn alive_peers(&self) -> Vec<&PeerInfo> {
        self.peers
            .values()
            .filter(|p| p.status == PeerStatus::Alive)
            .collect()
    }

    pub fn dead_peers(&self) -> Vec<&PeerInfo> {
        self.peers
            .values()
            .filter(|p| p.status == PeerStatus::Dead)
            .collect()
    }

    fn sync_peer_route_urls(&mut self, relay_id: &str, url: &str) {
        for entry in self.routes.values_mut() {
            if entry.locality == Locality::Peer && entry.relay_id == relay_id {
                entry.relay_url = url.to_string();
            }
        }
    }

    // -- Snapshot operations (for join protocol + anti-entropy) --

    pub fn full_snapshot(&self) -> FullSync {
        FullSync {
            routes: self.list_routes(),
            tombstones: self.list_tombstones(),
            peers: self
                .peers
                .values()
                .map(|p| PeerSnapshot {
                    relay_id: p.relay_id.clone(),
                    url: p.url.clone(),
                    route_count: p.route_count,
                    status: p.status,
                })
                .collect(),
        }
    }

    /// Merge a FULL_SYNC snapshot (join protocol).
    /// Replaces all PEER routes only after the incoming snapshot has been
    /// validated into a replacement set; preserves LOCAL routes.
    pub fn merge_snapshot(&mut self, sync: FullSync) {
        let FullSync {
            routes,
            tombstones,
            peers,
        } = sync;

        for peer in &peers {
            if peer.relay_id == self.relay_id {
                continue;
            }
            if !valid_relay_id(&peer.relay_id) {
                return;
            }
        }

        for tombstone in &tombstones {
            if tombstone.relay_id == self.relay_id {
                continue;
            }
            if !valid_route_name(&tombstone.name) || !valid_relay_id(&tombstone.relay_id) {
                return;
            }
        }

        let peer_snapshots: HashMap<_, _> = peers
            .iter()
            .filter(|ps| ps.relay_id != self.relay_id)
            .map(|ps| (ps.relay_id.clone(), ps.clone()))
            .collect();

        let mut replacement_routes = HashMap::new();
        for mut entry in routes {
            if entry.relay_id == self.relay_id {
                continue; // Don't overwrite our own LOCAL routes.
            }
            if !valid_route_name(&entry.name) || !valid_relay_id(&entry.relay_id) {
                return;
            }
            let Some(peer) = peer_snapshots.get(&entry.relay_id) else {
                return;
            };
            if !self.snapshot_route_owner_is_alive(peer) {
                continue;
            }
            entry.relay_url = peer.url.clone();
            entry.locality = Locality::Peer;
            // Snapshot sources must not carry owner-private fields. Keep this
            // invariant local so malformed or old inputs cannot poison peers.
            entry.ipc_address = None;
            entry.server_id = None;
            let key = (entry.name.clone(), entry.relay_id.clone());
            replacement_routes.insert(key, entry);
        }

        // Replace all existing PEER routes after the replacement set is ready.
        let peer_keys: Vec<_> = self
            .routes
            .iter()
            .filter(|(_, e)| e.locality == Locality::Peer)
            .map(|(k, _)| k.clone())
            .collect();
        for key in peer_keys {
            self.routes.remove(&key);
        }
        for tombstone in tombstones {
            if tombstone.relay_id == self.relay_id {
                continue;
            }
            self.apply_tombstone(tombstone);
        }

        for (key, entry) in replacement_routes {
            if let Some(tombstone) = self.tombstones.get(&key) {
                if entry.registered_at <= tombstone.removed_at {
                    continue;
                }
            }
            self.register_route(entry);
        }

        // Merge peers.
        for ps in peers {
            if ps.relay_id == self.relay_id {
                continue; // Don't add ourselves.
            }
            let relay_id = ps.relay_id.clone();
            let url = ps.url.clone();
            self.peers
                .entry(ps.relay_id.clone())
                .and_modify(|existing| {
                    existing.url = ps.url.clone();
                    existing.route_count = ps.route_count;
                    if existing.status != PeerStatus::Dead {
                        existing.status = ps.status;
                    }
                    existing.last_heartbeat = Instant::now();
                })
                .or_insert_with(|| PeerInfo {
                    relay_id: ps.relay_id,
                    url: ps.url,
                    route_count: ps.route_count,
                    last_heartbeat: Instant::now(),
                    status: ps.status,
                });
            self.sync_peer_route_urls(&relay_id, &url);
        }
    }

    fn snapshot_route_owner_is_alive(&self, peer: &PeerSnapshot) -> bool {
        self.peers
            .get(&peer.relay_id)
            .map(|local| local.status == PeerStatus::Alive)
            .unwrap_or(peer.status == PeerStatus::Alive)
    }

    /// Route digest for anti-entropy: (name, relay_id) → hash of fields that
    /// every relay sees the same way.
    ///
    /// **Must not include `ipc_address`**: the route owner stores the local
    /// UDS path, but peers (correctly) store `None`. Hashing the path would
    /// make local-vs-peer digests permanently disagree and cause anti-entropy
    /// to churn the same route forever.
    pub fn route_digest(&self) -> HashMap<(String, String, bool), u64> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut digest: HashMap<(String, String, bool), u64> = self
            .routes
            .iter()
            .map(|(key, entry)| {
                let mut hasher = DefaultHasher::new();
                entry.registered_at.to_bits().hash(&mut hasher);
                entry.relay_url.hash(&mut hasher);
                entry.crm_ns.hash(&mut hasher);
                entry.crm_ver.hash(&mut hasher);
                ((key.0.clone(), key.1.clone(), false), hasher.finish())
            })
            .collect();
        for (key, tombstone) in &self.tombstones {
            let mut hasher = DefaultHasher::new();
            tombstone.removed_at.to_bits().hash(&mut hasher);
            digest.insert((key.0.clone(), key.1.clone(), true), hasher.finish());
        }
        digest
    }

    pub fn route_state_for_diff(
        &self,
        name: &str,
        relay_id: &str,
        deleted: bool,
    ) -> Option<crate::relay::peer::DigestDiffEntry> {
        if deleted {
            return self
                .tombstones
                .get(&(name.to_string(), relay_id.to_string()))
                .map(|t| crate::relay::peer::DigestDiffEntry::Deleted {
                    name: t.name.clone(),
                    relay_id: t.relay_id.clone(),
                    removed_at: t.removed_at,
                });
        }
        self.routes
            .get(&(name.to_string(), relay_id.to_string()))
            .map(|entry| crate::relay::peer::DigestDiffEntry::Active {
                name: entry.name.clone(),
                relay_id: entry.relay_id.clone(),
                relay_url: entry.relay_url.clone(),
                crm_ns: entry.crm_ns.clone(),
                crm_ver: entry.crm_ver.clone(),
                registered_at: entry.registered_at,
            })
    }

    pub fn authoritative_missing_tombstone(
        &mut self,
        name: &str,
        relay_id: &str,
    ) -> Option<RouteTombstone> {
        if relay_id != self.relay_id {
            return None;
        }
        if !valid_route_name(name) || !valid_relay_id(relay_id) {
            return None;
        }
        let key = (name.to_string(), relay_id.to_string());
        if self.routes.contains_key(&key) {
            return None;
        }
        let removed_at = self.next_local_timestamp();
        let tombstone = RouteTombstone {
            name: name.to_string(),
            relay_id: relay_id.to_string(),
            removed_at,
            server_id: None,
            observed_at: Instant::now(),
        };
        self.apply_tombstone(tombstone);
        self.tombstones.get(&key).cloned()
    }
}

fn valid_route_name(name: &str) -> bool {
    !name.trim().is_empty() && name.as_bytes().len() <= MAX_CALL_ROUTE_NAME_BYTES
}

fn valid_relay_id(relay_id: &str) -> bool {
    !relay_id.trim().is_empty()
}

fn current_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn local_entry(name: &str, relay_id: &str) -> RouteEntry {
        RouteEntry {
            name: name.into(),
            relay_id: relay_id.into(),
            relay_url: format!("http://{relay_id}:8080"),
            server_id: Some(format!("server-{name}-{relay_id}")),
            ipc_address: Some(format!("ipc://{name}_{relay_id}")),
            crm_ns: "test.ns".into(),
            crm_ver: "0.1.0".into(),
            locality: Locality::Local,
            registered_at: 1000.0,
        }
    }

    fn peer_entry(name: &str, relay_id: &str, registered_at: f64) -> RouteEntry {
        RouteEntry {
            name: name.into(),
            relay_id: relay_id.into(),
            relay_url: format!("http://{relay_id}:8080"),
            server_id: None,
            ipc_address: None,
            crm_ns: "test.ns".into(),
            crm_ver: "0.1.0".into(),
            locality: Locality::Peer,
            registered_at,
        }
    }

    fn register_alive_peer(rt: &mut RouteTable, relay_id: &str) {
        rt.register_peer(PeerInfo {
            relay_id: relay_id.into(),
            url: format!("http://{relay_id}:8080"),
            route_count: 0,
            last_heartbeat: Instant::now(),
            status: PeerStatus::Alive,
        });
    }

    #[test]
    fn new_table_is_empty() {
        let rt = RouteTable::new("relay-a".into());
        assert!(rt.list_routes().is_empty());
        assert!(rt.list_peers().is_empty());
    }

    #[test]
    fn register_and_resolve_local() {
        let mut rt = RouteTable::new("relay-a".into());
        rt.register_route(local_entry("grid", "relay-a"));
        let resolved = rt.resolve("grid");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "grid");
        assert!(resolved[0].ipc_address.is_some());
    }

    #[test]
    fn resolve_local_before_peer() {
        let mut rt = RouteTable::new("relay-a".into());
        register_alive_peer(&mut rt, "relay-b");
        rt.register_route(local_entry("grid", "relay-a"));
        rt.register_route(peer_entry("grid", "relay-b", 500.0));
        let resolved = rt.resolve("grid");
        assert_eq!(resolved.len(), 2);
        assert!(resolved[0].ipc_address.is_some()); // LOCAL first
        assert!(resolved[1].ipc_address.is_none()); // PEER second
    }

    #[test]
    fn resolve_peers_sorted_deterministically() {
        let mut rt = RouteTable::new("relay-a".into());
        register_alive_peer(&mut rt, "relay-b");
        register_alive_peer(&mut rt, "relay-c");
        rt.register_route(peer_entry("grid", "relay-c", 1000.0));
        rt.register_route(peer_entry("grid", "relay-b", 1000.0));
        let resolved = rt.resolve("grid");
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].relay_url, "http://relay-b:8080");
        assert_eq!(resolved[1].relay_url, "http://relay-c:8080");
    }

    #[test]
    fn resolve_ignores_routes_owned_by_dead_peer() {
        let mut rt = RouteTable::new("relay-a".into());
        rt.register_peer(PeerInfo {
            relay_id: "relay-b".into(),
            url: "http://relay-b:8080".into(),
            route_count: 1,
            last_heartbeat: Instant::now(),
            status: PeerStatus::Dead,
        });
        rt.register_route(peer_entry("grid", "relay-b", 1000.0));

        assert!(rt.resolve("grid").is_empty());
    }

    #[test]
    fn resolve_includes_routes_owned_by_recovered_peer() {
        let mut rt = RouteTable::new("relay-a".into());
        rt.register_peer(PeerInfo {
            relay_id: "relay-b".into(),
            url: "http://relay-b:8080".into(),
            route_count: 1,
            last_heartbeat: Instant::now(),
            status: PeerStatus::Alive,
        });
        rt.register_route(peer_entry("grid", "relay-b", 1000.0));

        assert_eq!(rt.resolve("grid").len(), 1);
    }

    #[test]
    fn upsert_semantics() {
        let mut rt = RouteTable::new("relay-a".into());
        let mut entry = local_entry("grid", "relay-a");
        rt.register_route(entry.clone());
        entry.ipc_address = Some("ipc://new_addr".into());
        entry.registered_at = 2000.0;
        rt.register_route(entry);
        let routes = rt.list_routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].ipc_address.as_deref(), Some("ipc://new_addr"));
    }

    #[test]
    fn unregister_route_with_tombstone_removes_active_route() {
        let mut rt = RouteTable::new("relay-a".into());
        rt.register_route(local_entry("grid", "relay-a"));
        assert!(rt.local_route("grid").is_some());
        let removed = rt.unregister_route_with_tombstone("grid", "relay-a", 2000.0);
        assert!(removed.is_some());
        assert!(rt.local_route("grid").is_none());
        assert_eq!(rt.list_tombstones().len(), 1);
    }

    #[test]
    fn unregister_local_route_records_private_owner_tombstone() {
        let mut rt = RouteTable::new("relay-a".into());
        let registered_at = rt.next_local_timestamp();
        rt.register_route(RouteEntry {
            registered_at,
            ..local_entry("grid", "relay-a")
        });

        rt.unregister_local_route_with_tombstone("grid", "server-grid");

        assert!(rt.local_tombstone_matches_server("grid", "server-grid"));
        assert!(!rt.local_tombstone_matches_server("grid", "server-other"));
    }

    #[test]
    fn serialized_tombstone_does_not_expose_private_server_id() {
        let mut rt = RouteTable::new("relay-a".into());
        let registered_at = rt.next_local_timestamp();
        rt.register_route(RouteEntry {
            registered_at,
            ..local_entry("grid", "relay-a")
        });

        rt.unregister_local_route_with_tombstone("grid", "server-grid");
        let json = serde_json::to_string(&rt.list_tombstones()[0]).unwrap();

        assert!(!json.contains("server-grid"));
        assert!(!json.contains("server_id"));
    }

    #[test]
    fn newer_peer_tombstone_preserves_private_local_owner_id() {
        let mut rt = RouteTable::new("relay-a".into());
        let registered_at = rt.next_local_timestamp();
        rt.register_route(RouteEntry {
            registered_at,
            ..local_entry("grid", "relay-a")
        });
        rt.unregister_local_route_with_tombstone("grid", "server-grid");
        let newer_removed_at = rt.list_tombstones()[0].removed_at + 1.0;

        assert!(rt.apply_tombstone(RouteTombstone {
            name: "grid".into(),
            relay_id: "relay-a".into(),
            removed_at: newer_removed_at,
            server_id: None,
            observed_at: Instant::now(),
        }));

        assert!(rt.local_tombstone_matches_server("grid", "server-grid"));
    }

    #[test]
    fn local_unregister_uses_monotonic_timestamp() {
        let mut rt = RouteTable::new("relay-a".into());
        let first = rt.next_local_timestamp();
        rt.register_route(RouteEntry {
            registered_at: first,
            ..local_entry("grid", "relay-a")
        });

        let (_removed, removed_at) =
            rt.unregister_local_route_with_tombstone("grid", "server-grid");
        assert!(removed_at > first);
    }

    #[test]
    fn remove_routes_by_relay() {
        let mut rt = RouteTable::new("relay-a".into());
        register_alive_peer(&mut rt, "relay-b");
        rt.register_route(peer_entry("grid", "relay-b", 1000.0));
        rt.register_route(peer_entry("net", "relay-b", 1001.0));
        rt.register_route(local_entry("local", "relay-a"));
        let removed = rt.remove_routes_by_relay("relay-b");
        assert_eq!(removed.len(), 2);
        assert_eq!(rt.list_routes().len(), 1);
    }

    #[test]
    fn full_snapshot_and_merge() {
        let mut rt_a = RouteTable::new("relay-a".into());
        rt_a.register_route(local_entry("grid", "relay-a"));
        rt_a.register_route(peer_entry("net", "relay-b", 1000.0));
        rt_a.register_peer(PeerInfo {
            relay_id: "relay-b".into(),
            url: "http://relay-b:8080".into(),
            route_count: 1,
            last_heartbeat: Instant::now(),
            status: PeerStatus::Alive,
        });

        let mut snapshot = rt_a.full_snapshot();
        snapshot.peers.push(PeerSnapshot {
            relay_id: "relay-a".into(),
            url: "http://relay-a:8080".into(),
            route_count: 1,
            status: PeerStatus::Alive,
        });

        let mut rt_c = RouteTable::new("relay-c".into());
        rt_c.register_route(local_entry("local_c", "relay-c"));
        rt_c.merge_snapshot(snapshot);

        assert!(rt_c.local_route("local_c").is_some());
        let resolved = rt_c.resolve("grid");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].relay_url, "http://relay-a:8080");
        assert!(rt_c.get_peer("relay-b").is_some());
    }

    #[test]
    fn route_digest_stable_when_only_ipc_address_changes() {
        // Critical anti-entropy invariant: ipc_address must NOT contribute
        // to the digest, because the owning relay stores Some(...) but
        // peers store None — if it contributed, the two views would never
        // converge and anti-entropy would loop forever.
        let mut rt = RouteTable::new("relay-a".into());
        rt.register_route(local_entry("grid", "relay-a"));
        let d1 = rt.route_digest();
        let mut entry = local_entry("grid", "relay-a");
        entry.ipc_address = Some("ipc://changed".into());
        rt.register_route(entry);
        let d2 = rt.route_digest();
        let key = ("grid".to_string(), "relay-a".to_string(), false);
        assert_eq!(d1[&key], d2[&key]);
    }

    #[test]
    fn route_digest_changes_on_relay_url_update() {
        let mut rt = RouteTable::new("relay-a".into());
        rt.register_route(local_entry("grid", "relay-a"));
        let d1 = rt.route_digest();
        let mut entry = local_entry("grid", "relay-a");
        entry.relay_url = "http://changed:9090".into();
        rt.register_route(entry);
        let d2 = rt.route_digest();
        let key = ("grid".to_string(), "relay-a".to_string(), false);
        assert_ne!(d1[&key], d2[&key]);
    }

    #[test]
    fn resolve_missing_name_returns_empty() {
        let rt = RouteTable::new("relay-a".into());
        assert!(rt.resolve("nonexistent").is_empty());
    }

    #[test]
    fn merge_snapshot_strips_owner_private_fields_from_peer_routes() {
        // Sender's local route includes owner-private fields.
        let mut rt_a = RouteTable::new("relay-a".into());
        rt_a.register_route(local_entry("grid", "relay-a"));
        let mut snapshot = rt_a.full_snapshot();
        snapshot.peers.push(PeerSnapshot {
            relay_id: "relay-a".into(),
            url: "http://relay-a:8080".into(),
            route_count: 1,
            status: PeerStatus::Alive,
        });
        // Local snapshots can contain owner-private fields internally;
        // peer merge still scrubs them to keep the invariant local.

        let mut rt_b = RouteTable::new("relay-b".into());
        rt_b.merge_snapshot(snapshot);

        let resolved = rt_b.resolve("grid");
        assert_eq!(resolved.len(), 1);
        assert!(
            resolved[0].ipc_address.is_none(),
            "PEER routes must not expose sender's local UDS path",
        );
        assert_eq!(resolved[0].relay_url, "http://relay-a:8080");
        assert_eq!(rt_b.list_routes()[0].server_id, None);
    }

    #[test]
    fn merge_snapshot_does_not_clear_existing_peer_routes_when_snapshot_lacks_owner_peer() {
        let mut rt = RouteTable::new("relay-c".into());
        register_alive_peer(&mut rt, "relay-a");
        rt.register_route(peer_entry("grid", "relay-a", 1000.0));

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("cache", "relay-b", 1001.0)],
            tombstones: vec![],
            peers: vec![],
        });

        let resolved = rt.resolve("grid");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].relay_url, "http://relay-a:8080");
    }

    #[test]
    fn to_route_info_strips_ipc_address_for_peer() {
        // Even if a Peer entry somehow carries an ipc_address, to_route_info
        // must not leak it to clients (defense-in-depth).
        let mut entry = peer_entry("grid", "relay-b", 1000.0);
        entry.ipc_address = Some("ipc://leaked".into());
        let info = entry.to_route_info();
        assert!(info.ipc_address.is_none());
    }

    #[test]
    fn to_route_info_keeps_ipc_address_for_local() {
        let entry = local_entry("grid", "relay-a");
        let info = entry.to_route_info();
        assert!(info.ipc_address.is_some());
    }

    #[test]
    fn tombstone_blocks_stale_route_announce() {
        let mut rt = RouteTable::new("relay-b".into());
        rt.apply_tombstone(RouteTombstone {
            name: "grid".into(),
            relay_id: "relay-a".into(),
            removed_at: 2000.0,
            server_id: None,
            observed_at: Instant::now(),
        });

        assert!(!rt.register_route(peer_entry("grid", "relay-a", 1000.0)));
        assert!(rt.resolve("grid").is_empty());
    }

    #[test]
    fn register_route_rejects_non_finite_timestamp() {
        let mut rt = RouteTable::new("relay-b".into());
        register_alive_peer(&mut rt, "relay-a");

        assert!(!rt.register_route(peer_entry("grid", "relay-a", f64::NAN)));
        assert!(!rt.register_route(peer_entry("grid", "relay-a", f64::INFINITY)));

        assert!(rt.resolve("grid").is_empty());
    }

    #[test]
    fn apply_tombstone_rejects_non_finite_timestamp() {
        let mut rt = RouteTable::new("relay-b".into());
        register_alive_peer(&mut rt, "relay-a");
        assert!(rt.register_route(peer_entry("grid", "relay-a", 1000.0)));

        assert!(!rt.apply_tombstone(RouteTombstone {
            name: "grid".into(),
            relay_id: "relay-a".into(),
            removed_at: f64::NAN,
            server_id: None,
            observed_at: Instant::now(),
        }));

        assert_eq!(rt.resolve("grid").len(), 1);
        assert!(rt.list_tombstones().is_empty());
    }

    #[test]
    fn unregister_route_rejects_non_finite_tombstone_without_removing_route() {
        let mut rt = RouteTable::new("relay-b".into());
        register_alive_peer(&mut rt, "relay-a");
        assert!(rt.register_route(peer_entry("grid", "relay-a", 1000.0)));

        let removed = rt.unregister_route_with_tombstone("grid", "relay-a", f64::NAN);

        assert!(removed.is_none());
        assert_eq!(rt.resolve("grid").len(), 1);
        assert!(rt.list_tombstones().is_empty());
    }

    #[test]
    fn newer_route_replaces_tombstone() {
        let mut rt = RouteTable::new("relay-b".into());
        register_alive_peer(&mut rt, "relay-a");
        rt.apply_tombstone(RouteTombstone {
            name: "grid".into(),
            relay_id: "relay-a".into(),
            removed_at: 2000.0,
            server_id: None,
            observed_at: Instant::now(),
        });

        assert!(rt.register_route(peer_entry("grid", "relay-a", 2001.0)));
        assert_eq!(rt.resolve("grid").len(), 1);
        assert!(rt.list_tombstones().is_empty());
    }

    #[test]
    fn old_tombstone_cannot_delete_newer_route() {
        let mut rt = RouteTable::new("relay-b".into());
        register_alive_peer(&mut rt, "relay-a");
        rt.register_route(peer_entry("grid", "relay-a", 2001.0));

        assert!(!rt.apply_tombstone(RouteTombstone {
            name: "grid".into(),
            relay_id: "relay-a".into(),
            removed_at: 2000.0,
            server_id: None,
            observed_at: Instant::now(),
        }));

        assert_eq!(rt.resolve("grid").len(), 1);
        assert!(rt.list_tombstones().is_empty());
    }

    #[test]
    fn merge_snapshot_updates_known_peer_url() {
        let mut rt = RouteTable::new("relay-b".into());
        rt.register_peer(PeerInfo {
            relay_id: "relay-a".into(),
            url: "http://relay-a:8080".into(),
            route_count: 1,
            last_heartbeat: Instant::now(),
            status: PeerStatus::Alive,
        });

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("grid", "relay-a", 2001.0)],
            tombstones: vec![],
            peers: vec![PeerSnapshot {
                relay_id: "relay-a".into(),
                url: "http://relay-a-new:8080".into(),
                route_count: 99,
                status: PeerStatus::Alive,
            }],
        });

        assert_eq!(
            rt.get_peer("relay-a").unwrap().url,
            "http://relay-a-new:8080"
        );
        assert_eq!(rt.get_peer("relay-a").unwrap().route_count, 99);
        assert_eq!(rt.resolve("grid")[0].relay_url, "http://relay-a-new:8080");
    }

    #[test]
    fn merge_snapshot_keeps_url_for_new_peer() {
        let mut rt = RouteTable::new("relay-b".into());

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("grid", "relay-a", 2001.0)],
            tombstones: vec![],
            peers: vec![PeerSnapshot {
                relay_id: "relay-a".into(),
                url: "http://relay-a:8080".into(),
                route_count: 99,
                status: PeerStatus::Alive,
            }],
        });

        assert_eq!(rt.get_peer("relay-a").unwrap().url, "http://relay-a:8080");
        assert_eq!(rt.resolve("grid")[0].relay_url, "http://relay-a:8080");
    }

    #[test]
    fn monotonic_timestamp_clamps_backwards_steps() {
        let mut rt = RouteTable::new("relay-a".into());
        let first = rt.next_local_timestamp();
        rt.register_route(RouteEntry {
            registered_at: first,
            ..local_entry("grid", "relay-a")
        });

        let (_removed, removed_at) =
            rt.unregister_local_route_with_tombstone("grid", "server-grid");
        let updated = rt.register_route(RouteEntry {
            registered_at: removed_at + 1.0,
            ..local_entry("grid", "relay-a")
        });

        assert!(updated);
        assert!(removed_at > first);
        assert!(rt.local_route("grid").unwrap().registered_at > removed_at);
    }

    #[test]
    fn local_register_after_restart_outranks_old_tombstone() {
        let mut rt = RouteTable::new("relay-a".into());
        assert!(rt.apply_tombstone(RouteTombstone {
            name: "grid".into(),
            relay_id: "relay-a".into(),
            removed_at: 10.0,
            server_id: None,
            observed_at: Instant::now(),
        }));

        let registered_at = rt.next_local_timestamp();
        assert!(
            registered_at > 10.0,
            "fresh local events after restart must outrank old peer tombstones",
        );
        assert!(rt.register_route(RouteEntry {
            registered_at,
            ..local_entry("grid", "relay-a")
        }));

        assert!(rt.local_route("grid").is_some());
        assert!(rt.list_tombstones().is_empty());
    }

    #[test]
    fn merge_snapshot_applies_tombstone_before_old_route() {
        let mut rt = RouteTable::new("relay-b".into());

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("grid", "relay-a", 1000.0)],
            tombstones: vec![RouteTombstone {
                name: "grid".into(),
                relay_id: "relay-a".into(),
                removed_at: 2000.0,
                server_id: None,
                observed_at: Instant::now(),
            }],
            peers: vec![PeerSnapshot {
                relay_id: "relay-a".into(),
                url: "http://relay-a:8080".into(),
                route_count: 0,
                status: PeerStatus::Alive,
            }],
        });

        assert!(rt.resolve("grid").is_empty());
        assert_eq!(rt.list_tombstones().len(), 1);
    }

    #[test]
    fn merge_snapshot_newer_route_removes_older_tombstone() {
        let mut rt = RouteTable::new("relay-b".into());

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("grid", "relay-a", 2001.0)],
            tombstones: vec![RouteTombstone {
                name: "grid".into(),
                relay_id: "relay-a".into(),
                removed_at: 2000.0,
                server_id: None,
                observed_at: Instant::now(),
            }],
            peers: vec![PeerSnapshot {
                relay_id: "relay-a".into(),
                url: "http://relay-a:8080".into(),
                route_count: 1,
                status: PeerStatus::Alive,
            }],
        });

        assert_eq!(rt.resolve("grid").len(), 1);
        assert!(rt.list_tombstones().is_empty());
    }

    #[test]
    fn merge_snapshot_does_not_import_routes_for_dead_peer() {
        let mut rt = RouteTable::new("relay-c".into());

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("grid", "relay-a", 1000.0)],
            tombstones: vec![],
            peers: vec![PeerSnapshot {
                relay_id: "relay-a".into(),
                url: "http://relay-a:8080".into(),
                route_count: 1,
                status: PeerStatus::Dead,
            }],
        });

        assert!(rt.list_routes().is_empty());
        assert!(rt.resolve("grid").is_empty());
        assert_eq!(
            rt.get_peer("relay-a").map(|peer| peer.status),
            Some(PeerStatus::Dead)
        );
    }

    #[test]
    fn merge_snapshot_does_not_resurrect_locally_dead_peer_routes() {
        let mut rt = RouteTable::new("relay-c".into());
        rt.register_peer(PeerInfo {
            relay_id: "relay-a".into(),
            url: "http://relay-a:8080".into(),
            route_count: 0,
            last_heartbeat: Instant::now(),
            status: PeerStatus::Dead,
        });

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("grid", "relay-a", 1000.0)],
            tombstones: vec![],
            peers: vec![PeerSnapshot {
                relay_id: "relay-a".into(),
                url: "http://relay-a:8080".into(),
                route_count: 1,
                status: PeerStatus::Alive,
            }],
        });

        assert!(rt.list_routes().is_empty());
        assert!(rt.resolve("grid").is_empty());
        assert_eq!(
            rt.get_peer("relay-a").map(|peer| peer.status),
            Some(PeerStatus::Dead)
        );
    }

    #[test]
    fn merge_snapshot_accepts_routes_after_peer_status_recovers() {
        let mut rt = RouteTable::new("relay-c".into());

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("grid", "relay-a", 1000.0)],
            tombstones: vec![],
            peers: vec![PeerSnapshot {
                relay_id: "relay-a".into(),
                url: "http://relay-a:8080".into(),
                route_count: 1,
                status: PeerStatus::Alive,
            }],
        });

        assert_eq!(rt.resolve("grid").len(), 1);
        assert_eq!(
            rt.get_peer("relay-a").map(|peer| peer.status),
            Some(PeerStatus::Alive)
        );
    }

    #[test]
    fn merge_snapshot_rejects_routes_with_wire_invalid_names() {
        let mut rt = RouteTable::new("relay-b".into());

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry(
                &"x".repeat(MAX_CALL_ROUTE_NAME_BYTES + 1),
                "relay-a",
                1000.0,
            )],
            tombstones: vec![],
            peers: vec![PeerSnapshot {
                relay_id: "relay-a".into(),
                url: "http://relay-a:8080".into(),
                route_count: 1,
                status: PeerStatus::Alive,
            }],
        });

        assert!(rt.list_routes().is_empty());
    }

    #[test]
    fn merge_snapshot_rejects_tombstones_with_wire_invalid_names() {
        let mut rt = RouteTable::new("relay-b".into());
        register_alive_peer(&mut rt, "relay-a");
        rt.register_route(peer_entry("existing", "relay-a", 1000.0));

        rt.merge_snapshot(FullSync {
            routes: vec![],
            tombstones: vec![RouteTombstone {
                name: "x".repeat(MAX_CALL_ROUTE_NAME_BYTES + 1),
                relay_id: "relay-a".into(),
                removed_at: 2000.0,
                server_id: None,
                observed_at: Instant::now(),
            }],
            peers: vec![PeerSnapshot {
                relay_id: "relay-a".into(),
                url: "http://relay-a:8080".into(),
                route_count: 0,
                status: PeerStatus::Alive,
            }],
        });

        assert!(rt.list_tombstones().is_empty());
        assert_eq!(rt.resolve("existing").len(), 1);
    }

    #[test]
    fn merge_snapshot_rejects_empty_relay_ids_without_mutating_existing_routes() {
        let mut rt = RouteTable::new("relay-b".into());
        register_alive_peer(&mut rt, "relay-a");
        rt.register_route(peer_entry("existing", "relay-a", 1000.0));

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("grid", "", 1000.0)],
            tombstones: vec![],
            peers: vec![PeerSnapshot {
                relay_id: "".into(),
                url: "http://blank:8080".into(),
                route_count: 1,
                status: PeerStatus::Alive,
            }],
        });

        assert_eq!(rt.resolve("existing").len(), 1);
        assert!(rt.resolve("grid").is_empty());
    }

    #[test]
    fn merge_snapshot_rejects_invalid_peer_without_partial_mutation() {
        let mut rt = RouteTable::new("relay-b".into());
        register_alive_peer(&mut rt, "relay-a");
        rt.register_route(peer_entry("existing", "relay-a", 1000.0));

        rt.merge_snapshot(FullSync {
            routes: vec![peer_entry("grid", "relay-c", 1000.0)],
            tombstones: vec![RouteTombstone {
                name: "existing".into(),
                relay_id: "relay-a".into(),
                removed_at: 2000.0,
                server_id: None,
                observed_at: Instant::now(),
            }],
            peers: vec![
                PeerSnapshot {
                    relay_id: "relay-c".into(),
                    url: "http://relay-c:8080".into(),
                    route_count: 1,
                    status: PeerStatus::Alive,
                },
                PeerSnapshot {
                    relay_id: "".into(),
                    url: "http://blank:8080".into(),
                    route_count: 0,
                    status: PeerStatus::Alive,
                },
            ],
        });

        assert_eq!(rt.resolve("existing").len(), 1);
        assert!(rt.resolve("grid").is_empty());
        assert!(rt.list_tombstones().is_empty());
        assert!(rt.get_peer("relay-c").is_none());
    }

    #[test]
    fn tombstone_gc_removes_expired_negative_state() {
        let mut rt = RouteTable::new("relay-b".into());
        rt.apply_tombstone(RouteTombstone {
            name: "grid".into(),
            relay_id: "relay-a".into(),
            removed_at: 2000.0,
            server_id: None,
            observed_at: Instant::now(),
        });

        assert_eq!(rt.gc_tombstones(Duration::from_secs(0)), 1);
        assert!(rt.list_tombstones().is_empty());
    }
}
