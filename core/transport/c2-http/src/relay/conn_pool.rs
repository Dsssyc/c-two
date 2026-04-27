use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use c2_ipc::IpcClient;

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// A managed IPC connection with activity tracking.
struct ConnectionEntry {
    address: String,
    client: Option<Arc<IpcClient>>,
    generation: u64,
    last_activity: AtomicU64,
    active_requests: AtomicUsize,
}

#[derive(Clone)]
pub enum CachedClient {
    Ready {
        client: Arc<IpcClient>,
        address: String,
        generation: u64,
    },
    Evicted {
        address: String,
        generation: u64,
    },
    Disconnected {
        address: String,
        generation: u64,
    },
    Missing,
}

/// Pool of IPC connections keyed by route name.
///
/// Separated from RouteTable to keep route metadata independent of
/// connection lifecycle. Supports lazy reconnection and idle eviction.
pub struct ConnectionPool {
    entries: HashMap<String, ConnectionEntry>,
    next_generation: u64,
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            next_generation: 1,
        }
    }

    fn allocate_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("connection generation exhausted");
        generation
    }

    /// Insert a pre-connected client for a route name.
    pub fn insert(&mut self, name: String, address: String, client: Arc<IpcClient>) {
        let generation = self.allocate_generation();
        self.entries.insert(
            name,
            ConnectionEntry {
                address,
                client: Some(client),
                generation,
                last_activity: AtomicU64::new(now_millis()),
                active_requests: AtomicUsize::new(0),
            },
        );
    }

    pub fn lookup(&self, name: &str) -> CachedClient {
        let Some(entry) = self.entries.get(name) else {
            return CachedClient::Missing;
        };
        match &entry.client {
            Some(client) if client.is_connected() => CachedClient::Ready {
                client: client.clone(),
                address: entry.address.clone(),
                generation: entry.generation,
            },
            Some(_) => CachedClient::Disconnected {
                address: entry.address.clone(),
                generation: entry.generation,
            },
            None => CachedClient::Evicted {
                address: entry.address.clone(),
                generation: entry.generation,
            },
        }
    }

    /// Begin an upstream request against a connected client.
    pub fn begin_request(&self, name: &str) -> Option<(Arc<IpcClient>, u64)> {
        let entry = self.entries.get(name)?;
        let client = entry.client.as_ref()?;
        if !client.is_connected() {
            return None;
        }
        entry.active_requests.fetch_add(1, Ordering::AcqRel);
        entry.last_activity.store(now_millis(), Ordering::Release);
        Some((client.clone(), entry.generation))
    }

    /// End an upstream request and refresh activity.
    pub fn end_request(&self, name: &str, generation: u64) {
        if let Some(entry) = self.entries.get(name) {
            if entry.generation != generation {
                return;
            }
            let mut current = entry.active_requests.load(Ordering::Acquire);
            loop {
                if current == 0 {
                    debug_assert!(false, "end_request called without begin_request");
                    break;
                }
                match entry.active_requests.compare_exchange_weak(
                    current,
                    current - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
            entry.last_activity.store(now_millis(), Ordering::Release);
        }
    }

    /// Get stored address for reconnection.
    pub fn get_address(&self, name: &str) -> Option<String> {
        self.entries.get(name).map(|e| e.address.clone())
    }

    /// Capture the current reconnect identity before awaiting a new connection.
    pub fn reconnect_candidate(&self, name: &str) -> Option<(String, u64)> {
        self.entries
            .get(name)
            .map(|e| (e.address.clone(), e.generation))
    }

    /// Touch activity timestamp (lock-free, Relaxed).
    pub fn touch(&self, name: &str) {
        if let Some(entry) = self.entries.get(name) {
            entry.last_activity.store(now_millis(), Ordering::Relaxed);
        }
    }

    /// Names of entries to evict (dead or idle beyond timeout_ms).
    pub fn idle_entries(&self, idle_timeout_ms: u64) -> Vec<String> {
        let cutoff = now_millis().saturating_sub(idle_timeout_ms);
        self.entries
            .iter()
            .filter(|(_, e)| {
                if let Some(ref client) = e.client {
                    if !client.is_connected() {
                        return true;
                    }
                    e.active_requests.load(Ordering::Acquire) == 0
                        && e.last_activity.load(Ordering::Acquire) <= cutoff
                } else {
                    false
                }
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Evict a client — returns old Arc for async close.
    pub fn evict(&mut self, name: &str) -> Option<Arc<IpcClient>> {
        self.entries.get_mut(name).and_then(|e| e.client.take())
    }

    /// Evict a client only if the current entry matches the request generation.
    pub fn evict_generation(&mut self, name: &str, generation: u64) -> Option<Arc<IpcClient>> {
        self.entries.get_mut(name).and_then(|e| {
            if e.generation == generation {
                e.client.take()
            } else {
                None
            }
        })
    }

    /// Re-attach a freshly connected client.
    pub fn reconnect(&mut self, name: &str, client: Arc<IpcClient>) {
        if !self.entries.contains_key(name) {
            return;
        }
        let generation = self.allocate_generation();
        if let Some(entry) = self.entries.get_mut(name) {
            entry.client = Some(client);
            entry.generation = generation;
            entry.active_requests.store(0, Ordering::Release);
            entry.last_activity.store(now_millis(), Ordering::Release);
        }
    }

    /// Re-attach a freshly connected client only if the entry still matches.
    pub fn reconnect_generation(
        &mut self,
        name: &str,
        generation: u64,
        address: &str,
        client: Arc<IpcClient>,
    ) -> bool {
        if !matches!(
            self.entries.get(name),
            Some(entry) if entry.generation == generation && entry.address == address
        ) {
            return false;
        }
        let new_generation = self.allocate_generation();
        if let Some(entry) = self.entries.get_mut(name) {
            entry.client = Some(client);
            entry.generation = new_generation;
            entry.active_requests.store(0, Ordering::Release);
            entry.last_activity.store(now_millis(), Ordering::Release);
            return true;
        }
        false
    }

    /// Remove entry entirely.
    pub fn remove(&mut self, name: &str) -> Option<Arc<IpcClient>> {
        self.entries.remove(name).and_then(|e| e.client)
    }

    /// List route names with addresses.
    pub fn list_connections(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .map(|(n, e)| (n.clone(), e.address.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_pool_is_empty() {
        let pool = ConnectionPool::new();
        assert!(pool.list_connections().is_empty());
    }

    #[test]
    fn insert_and_lookup_ready() {
        let mut pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://test"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://test".into(), client);
        assert!(matches!(pool.lookup("grid"), CachedClient::Ready { .. }));
    }

    #[test]
    fn lookup_distinguishes_ready_evicted_disconnected_and_missing() {
        let mut pool = ConnectionPool::new();
        let ready = Arc::new(IpcClient::new("ipc://ready"));
        ready.force_connected(true);
        pool.insert("ready".into(), "ipc://ready".into(), ready);
        let disconnected = Arc::new(IpcClient::new("ipc://disconnected"));
        pool.insert(
            "disconnected".into(),
            "ipc://disconnected".into(),
            disconnected,
        );
        let evicted = Arc::new(IpcClient::new("ipc://evicted"));
        evicted.force_connected(true);
        pool.insert("evicted".into(), "ipc://evicted".into(), evicted);
        pool.evict("evicted");

        match pool.lookup("ready") {
            CachedClient::Ready {
                address,
                generation,
                ..
            } => {
                assert_eq!(address, "ipc://ready");
                assert!(generation > 0);
            }
            _ => panic!("ready connection should be explicit"),
        }
        match pool.lookup("evicted") {
            CachedClient::Evicted {
                address,
                generation,
            } => {
                assert_eq!(address, "ipc://evicted");
                assert!(generation > 0);
            }
            _ => panic!("evicted connection should be explicit"),
        }
        match pool.lookup("disconnected") {
            CachedClient::Disconnected {
                address,
                generation,
            } => {
                assert_eq!(address, "ipc://disconnected");
                assert!(generation > 0);
            }
            _ => panic!("disconnected connection should be explicit"),
        }
        assert!(matches!(pool.lookup("missing"), CachedClient::Missing));
    }

    #[test]
    fn lookup_returns_disconnected_for_disconnected() {
        let mut pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://test"));
        // Client starts disconnected.
        pool.insert("grid".into(), "ipc://test".into(), client);
        assert!(matches!(
            pool.lookup("grid"),
            CachedClient::Disconnected { .. }
        ));
    }

    #[test]
    fn evict_and_reconnect() {
        let mut pool = ConnectionPool::new();
        let c1 = Arc::new(IpcClient::new("ipc://test"));
        c1.force_connected(true);
        pool.insert("grid".into(), "ipc://test".into(), c1);
        pool.evict("grid");
        assert!(matches!(pool.lookup("grid"), CachedClient::Evicted { .. }));

        let c2 = Arc::new(IpcClient::new("ipc://test"));
        c2.force_connected(true);
        pool.reconnect("grid", c2);
        assert!(matches!(pool.lookup("grid"), CachedClient::Ready { .. }));
    }

    #[test]
    fn remove_deletes_entry() {
        let mut pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://test"));
        pool.insert("grid".into(), "ipc://test".into(), client);
        pool.remove("grid");
        assert!(matches!(pool.lookup("grid"), CachedClient::Missing));
    }

    #[test]
    fn idle_entries_detects_disconnected() {
        let mut pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://dead"));
        pool.insert("d".into(), "ipc://dead".into(), client);
        assert_eq!(pool.idle_entries(u64::MAX).len(), 1);
    }

    #[test]
    fn idle_entries_do_not_evict_active_connected_client() {
        let mut pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://active"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://active".into(), client);

        assert!(pool.begin_request("grid").is_some());

        assert!(pool.idle_entries(0).is_empty());
    }

    #[test]
    fn idle_entries_can_evict_inactive_connected_client() {
        let mut pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://idle"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://idle".into(), client);

        assert_eq!(pool.idle_entries(0), vec!["grid".to_string()]);
    }

    #[test]
    fn end_request_makes_client_idle_candidate_again() {
        let mut pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://active"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://active".into(), client);

        let (_, generation) = pool.begin_request("grid").unwrap();
        pool.end_request("grid", generation);

        assert_eq!(pool.idle_entries(0), vec!["grid".to_string()]);
    }

    #[test]
    fn disconnected_client_is_evicted_even_when_not_idle_by_time() {
        let mut pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://dead"));
        client.force_connected(false);
        pool.insert("grid".into(), "ipc://dead".into(), client);

        assert_eq!(pool.idle_entries(u64::MAX), vec!["grid".to_string()]);
    }

    #[test]
    fn stale_generation_end_after_reinsert_leaves_new_entry_idle() {
        let mut pool = ConnectionPool::new();
        let old_client = Arc::new(IpcClient::new("ipc://old"));
        old_client.force_connected(true);
        pool.insert("grid".into(), "ipc://old".into(), old_client);
        let (_, old_generation) = pool.begin_request("grid").unwrap();

        pool.remove("grid");

        let new_client = Arc::new(IpcClient::new("ipc://new"));
        new_client.force_connected(true);
        pool.insert("grid".into(), "ipc://new".into(), new_client);
        pool.end_request("grid", old_generation);

        assert_eq!(pool.idle_entries(0), vec!["grid".to_string()]);
    }

    #[test]
    fn stale_generation_end_after_reinsert_does_not_release_new_active_request() {
        let mut pool = ConnectionPool::new();
        let old_client = Arc::new(IpcClient::new("ipc://old"));
        old_client.force_connected(true);
        pool.insert("grid".into(), "ipc://old".into(), old_client);
        let (_, old_generation) = pool.begin_request("grid").unwrap();

        pool.remove("grid");

        let new_client = Arc::new(IpcClient::new("ipc://new"));
        new_client.force_connected(true);
        pool.insert("grid".into(), "ipc://new".into(), new_client);
        let (_, new_generation) = pool.begin_request("grid").unwrap();

        pool.end_request("grid", old_generation);
        assert!(pool.idle_entries(0).is_empty());

        pool.end_request("grid", new_generation);
        assert_eq!(pool.idle_entries(0), vec!["grid".to_string()]);
    }

    #[test]
    fn stale_generation_evict_after_reinsert_does_not_evict_new_entry() {
        let mut pool = ConnectionPool::new();
        let old_client = Arc::new(IpcClient::new("ipc://old"));
        old_client.force_connected(true);
        pool.insert("grid".into(), "ipc://old".into(), old_client);
        let (_, old_generation) = pool.begin_request("grid").unwrap();

        pool.remove("grid");

        let new_client = Arc::new(IpcClient::new("ipc://new"));
        new_client.force_connected(true);
        pool.insert("grid".into(), "ipc://new".into(), new_client);

        assert!(pool.evict_generation("grid", old_generation).is_none());
        assert!(matches!(pool.lookup("grid"), CachedClient::Ready { .. }));
    }

    #[test]
    fn matching_generation_evict_removes_current_entry_client() {
        let mut pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://current"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://current".into(), client);
        let (_, generation) = pool.begin_request("grid").unwrap();

        assert!(pool.evict_generation("grid", generation).is_some());
        assert!(matches!(pool.lookup("grid"), CachedClient::Evicted { .. }));
    }

    #[test]
    fn stale_generation_evict_after_reconnect_does_not_evict_reconnected_client() {
        let mut pool = ConnectionPool::new();
        let old_client = Arc::new(IpcClient::new("ipc://old"));
        old_client.force_connected(true);
        pool.insert("grid".into(), "ipc://old".into(), old_client);
        let (_, old_generation) = pool.begin_request("grid").unwrap();

        assert!(pool.evict_generation("grid", old_generation).is_some());

        let new_client = Arc::new(IpcClient::new("ipc://new"));
        new_client.force_connected(true);
        pool.reconnect("grid", new_client);

        assert!(pool.evict_generation("grid", old_generation).is_none());
        assert!(matches!(pool.lookup("grid"), CachedClient::Ready { .. }));
    }

    #[test]
    fn stale_generation_end_after_reconnect_does_not_release_new_active_request() {
        let mut pool = ConnectionPool::new();
        let old_client = Arc::new(IpcClient::new("ipc://old"));
        old_client.force_connected(true);
        pool.insert("grid".into(), "ipc://old".into(), old_client);
        let (_, old_generation) = pool.begin_request("grid").unwrap();

        let new_client = Arc::new(IpcClient::new("ipc://new"));
        new_client.force_connected(true);
        pool.reconnect("grid", new_client);
        assert_eq!(pool.idle_entries(0), vec!["grid".to_string()]);

        let (_, new_generation) = pool.begin_request("grid").unwrap();

        pool.end_request("grid", old_generation);
        assert!(pool.idle_entries(0).is_empty());

        pool.end_request("grid", new_generation);
        assert_eq!(pool.idle_entries(0), vec!["grid".to_string()]);
    }

    #[test]
    fn stale_generation_reconnect_after_reinsert_does_not_replace_new_entry() {
        let mut pool = ConnectionPool::new();
        let old_client = Arc::new(IpcClient::new("ipc://old"));
        old_client.force_connected(true);
        pool.insert("grid".into(), "ipc://old".into(), old_client);
        let (address, old_generation) = pool.reconnect_candidate("grid").unwrap();

        pool.remove("grid");

        let current_client = Arc::new(IpcClient::new("ipc://current"));
        current_client.force_connected(true);
        pool.insert(
            "grid".into(),
            "ipc://current".into(),
            current_client.clone(),
        );

        let stale_client = Arc::new(IpcClient::new("ipc://stale"));
        stale_client.force_connected(true);
        assert!(!pool.reconnect_generation("grid", old_generation, &address, stale_client));

        let (client, current_generation) = pool.begin_request("grid").unwrap();
        assert!(Arc::ptr_eq(&client, &current_client));
        assert_ne!(current_generation, old_generation);
        assert!(matches!(pool.lookup("grid"), CachedClient::Ready { .. }));
    }

    #[test]
    fn matching_generation_reconnect_attaches_client_and_advances_generation() {
        let mut pool = ConnectionPool::new();
        let old_client = Arc::new(IpcClient::new("ipc://old"));
        old_client.force_connected(true);
        pool.insert("grid".into(), "ipc://old".into(), old_client);
        let (address, old_generation) = pool.reconnect_candidate("grid").unwrap();

        let new_client = Arc::new(IpcClient::new("ipc://new"));
        new_client.force_connected(true);
        assert!(pool.reconnect_generation("grid", old_generation, &address, new_client.clone()));

        let (client, new_generation) = pool.begin_request("grid").unwrap();
        assert!(Arc::ptr_eq(&client, &new_client));
        assert_ne!(new_generation, old_generation);

        assert!(pool.evict_generation("grid", old_generation).is_none());
        pool.end_request("grid", old_generation);
        assert!(pool.idle_entries(0).is_empty());

        pool.end_request("grid", new_generation);
        assert_eq!(pool.idle_entries(0), vec!["grid".to_string()]);
    }

    #[test]
    #[should_panic(expected = "connection generation exhausted")]
    fn generation_allocator_panics_instead_of_reusing_on_overflow() {
        let mut pool = ConnectionPool::new();
        pool.next_generation = u64::MAX;

        let c1 = Arc::new(IpcClient::new("ipc://max"));
        pool.insert("max".into(), "ipc://max".into(), c1);

        let c2 = Arc::new(IpcClient::new("ipc://overflow"));
        pool.insert("overflow".into(), "ipc://overflow".into(), c2);
    }
}
