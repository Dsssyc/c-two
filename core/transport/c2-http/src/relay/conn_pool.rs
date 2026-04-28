use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use c2_ipc::IpcClient;
use parking_lot::Mutex;
use tokio::sync::Notify;

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// A single route/address upstream slot.
struct UpstreamSlot {
    inner: Mutex<SlotInner>,
    notify: Notify,
}

struct SlotInner {
    address: String,
    client: Option<Arc<IpcClient>>,
    generation: u64,
    last_activity: u64,
    active_requests: usize,
    state: SlotState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotState {
    Ready,
    Evicted,
    Disconnected,
    Reconnecting,
    Retired,
}

#[derive(Debug)]
pub enum AcquireError {
    NotFound,
    Unreachable(c2_ipc::IpcError),
}

pub struct UpstreamLease {
    slot: Arc<UpstreamSlot>,
    client: Arc<IpcClient>,
    generation: u64,
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
    entries: Mutex<HashMap<String, Arc<UpstreamSlot>>>,
    next_generation: AtomicU64,
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
        }
    }

    fn allocate_generation(&self) -> u64 {
        self.next_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .expect("connection generation exhausted")
    }

    fn slot(&self, name: &str) -> Option<Arc<UpstreamSlot>> {
        self.entries.lock().get(name).cloned()
    }

    pub async fn acquire_with<C, Fut>(
        &self,
        name: &str,
        connector: C,
    ) -> Result<UpstreamLease, AcquireError>
    where
        C: Fn(String) -> Fut,
        Fut: Future<Output = Result<Arc<IpcClient>, c2_ipc::IpcError>>,
    {
        let Some(slot) = self.slot(name) else {
            return Err(AcquireError::NotFound);
        };
        slot.acquire_with(connector).await
    }

    /// Insert a pre-connected client for a route name.
    pub fn insert(&self, name: String, address: String, client: Arc<IpcClient>) {
        let generation = self.allocate_generation();
        let old_slot = self.entries.lock().insert(
            name,
            Arc::new(UpstreamSlot::new(
                address,
                Some(client),
                generation,
                SlotState::Ready,
            )),
        );
        if let Some(old_slot) = old_slot {
            if let Some(client) = old_slot.retire() {
                close_replaced_client(client);
            }
        }
    }

    pub fn lookup(&self, name: &str) -> CachedClient {
        let Some(slot) = self.slot(name) else {
            return CachedClient::Missing;
        };
        slot.lookup()
    }

    /// Begin an upstream request against a connected client.
    pub fn begin_request(&self, name: &str) -> Option<(Arc<IpcClient>, u64)> {
        self.slot(name)?.begin_request()
    }

    /// End an upstream request and refresh activity.
    pub fn end_request(&self, name: &str, generation: u64) {
        if let Some(slot) = self.slot(name) {
            slot.end_request(generation);
        }
    }

    /// Get stored address for reconnection.
    pub fn get_address(&self, name: &str) -> Option<String> {
        self.slot(name).map(|slot| slot.address())
    }

    /// Capture the current reconnect identity before awaiting a new connection.
    pub fn reconnect_candidate(&self, name: &str) -> Option<(String, u64)> {
        self.slot(name).map(|slot| slot.reconnect_candidate())
    }

    /// Touch activity timestamp.
    pub fn touch(&self, name: &str) {
        if let Some(slot) = self.slot(name) {
            slot.touch();
        }
    }

    /// Names of entries to evict (dead or idle beyond timeout_ms).
    pub fn idle_entries(&self, idle_timeout_ms: u64) -> Vec<String> {
        self.entries
            .lock()
            .iter()
            .filter(|(_, slot)| slot.is_idle_candidate(idle_timeout_ms))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Evict idle clients with a slot-local recheck before removing them.
    pub fn evict_idle(&self, idle_timeout_ms: u64) -> Vec<(String, Option<Arc<IpcClient>>)> {
        let names = self.idle_entries(idle_timeout_ms);
        names
            .into_iter()
            .map(|name| {
                let client = self
                    .slot(&name)
                    .and_then(|slot| slot.evict_if_idle(idle_timeout_ms));
                (name, client)
            })
            .collect()
    }

    /// Evict a client — returns old Arc for async close.
    pub fn evict(&self, name: &str) -> Option<Arc<IpcClient>> {
        self.slot(name)?.evict()
    }

    /// Evict a client only if the current entry matches the request generation.
    pub fn evict_generation(&self, name: &str, generation: u64) -> Option<Arc<IpcClient>> {
        self.slot(name)?.evict_generation(generation)
    }

    /// Re-attach a freshly connected client.
    pub fn reconnect(&self, name: &str, client: Arc<IpcClient>) {
        if let Some(slot) = self.slot(name) {
            slot.reconnect(client);
        }
    }

    /// Re-attach a freshly connected client only if the entry still matches.
    pub fn reconnect_generation(
        &self,
        name: &str,
        generation: u64,
        address: &str,
        client: Arc<IpcClient>,
    ) -> bool {
        self.slot(name)
            .map(|slot| slot.reconnect_generation(generation, address, client))
            .unwrap_or(false)
    }

    /// Remove entry entirely.
    pub fn remove(&self, name: &str) -> Option<Arc<IpcClient>> {
        self.entries
            .lock()
            .remove(name)
            .and_then(|slot| slot.retire())
    }

    /// List route names with addresses.
    pub fn list_connections(&self) -> Vec<(String, String)> {
        self.entries
            .lock()
            .iter()
            .map(|(n, slot)| (n.clone(), slot.address()))
            .collect()
    }
}

fn close_replaced_client(client: Arc<IpcClient>) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move { client.close_shared().await });
    }
}

impl UpstreamSlot {
    fn new(
        address: String,
        client: Option<Arc<IpcClient>>,
        generation: u64,
        state: SlotState,
    ) -> Self {
        Self {
            inner: Mutex::new(SlotInner {
                address,
                client,
                generation,
                last_activity: now_millis(),
                active_requests: 0,
                state,
            }),
            notify: Notify::new(),
        }
    }

    async fn acquire_with<C, Fut>(
        self: Arc<Self>,
        connector: C,
    ) -> Result<UpstreamLease, AcquireError>
    where
        C: Fn(String) -> Fut,
        Fut: Future<Output = Result<Arc<IpcClient>, c2_ipc::IpcError>>,
    {
        loop {
            let notified = self.notify.notified();
            let mut notified = pin!(notified);
            notified.as_mut().enable();
            let address = {
                let mut inner = self.inner.lock();
                match inner.state {
                    SlotState::Retired => return Err(AcquireError::NotFound),
                    SlotState::Ready => {
                        if let Some(client) = inner.client.clone() {
                            if client.is_connected() {
                                inner.active_requests += 1;
                                inner.last_activity = now_millis();
                                return Ok(UpstreamLease {
                                    slot: self.clone(),
                                    client,
                                    generation: inner.generation,
                                });
                            }
                        }
                        inner.client = None;
                        inner.state = SlotState::Reconnecting;
                        Some(inner.address.clone())
                    }
                    SlotState::Evicted | SlotState::Disconnected => {
                        inner.state = SlotState::Reconnecting;
                        Some(inner.address.clone())
                    }
                    SlotState::Reconnecting => None,
                }
            };

            match address {
                Some(address) => match connector(address).await {
                    Ok(client) => {
                        let acquire = {
                            let mut inner = self.inner.lock();
                            if inner.state == SlotState::Retired {
                                None
                            } else {
                                inner.generation = inner
                                    .generation
                                    .checked_add(1)
                                    .expect("connection generation exhausted");
                                inner.client = Some(client.clone());
                                inner.state = SlotState::Ready;
                                inner.active_requests += 1;
                                inner.last_activity = now_millis();
                                Some(inner.generation)
                            }
                        };
                        let Some(generation) = acquire else {
                            client.close_shared().await;
                            return Err(AcquireError::NotFound);
                        };
                        self.notify.notify_waiters();
                        return Ok(UpstreamLease {
                            slot: self.clone(),
                            client,
                            generation,
                        });
                    }
                    Err(err) => {
                        let mut inner = self.inner.lock();
                        if inner.state == SlotState::Retired {
                            drop(inner);
                            return Err(AcquireError::NotFound);
                        }
                        inner.client = None;
                        inner.state = SlotState::Disconnected;
                        inner.last_activity = now_millis();
                        drop(inner);
                        self.notify.notify_waiters();
                        return Err(AcquireError::Unreachable(err));
                    }
                },
                None => {
                    notified.as_mut().await;
                }
            }
        }
    }

    fn lookup(&self) -> CachedClient {
        let inner = self.inner.lock();
        match inner.state {
            SlotState::Ready => match &inner.client {
                Some(client) if client.is_connected() => CachedClient::Ready {
                    client: client.clone(),
                    address: inner.address.clone(),
                    generation: inner.generation,
                },
                Some(_) => CachedClient::Disconnected {
                    address: inner.address.clone(),
                    generation: inner.generation,
                },
                None => CachedClient::Evicted {
                    address: inner.address.clone(),
                    generation: inner.generation,
                },
            },
            SlotState::Evicted => CachedClient::Evicted {
                address: inner.address.clone(),
                generation: inner.generation,
            },
            SlotState::Disconnected | SlotState::Reconnecting => CachedClient::Disconnected {
                address: inner.address.clone(),
                generation: inner.generation,
            },
            SlotState::Retired => CachedClient::Missing,
        }
    }

    fn begin_request(&self) -> Option<(Arc<IpcClient>, u64)> {
        let mut inner = self.inner.lock();
        if inner.state != SlotState::Ready {
            return None;
        }
        let client = inner.client.as_ref()?.clone();
        if !client.is_connected() {
            inner.state = SlotState::Disconnected;
            inner.client = None;
            return None;
        }
        inner.active_requests += 1;
        inner.last_activity = now_millis();
        Some((client, inner.generation))
    }

    fn end_request(&self, generation: u64) {
        let mut inner = self.inner.lock();
        if inner.generation != generation {
            return;
        }
        if inner.active_requests == 0 {
            debug_assert!(false, "end_request called without begin_request");
        } else {
            inner.active_requests -= 1;
        }
        inner.last_activity = now_millis();
    }

    fn address(&self) -> String {
        self.inner.lock().address.clone()
    }

    fn reconnect_candidate(&self) -> (String, u64) {
        let inner = self.inner.lock();
        (inner.address.clone(), inner.generation)
    }

    fn touch(&self) {
        self.inner.lock().last_activity = now_millis();
    }

    fn is_idle_candidate(&self, idle_timeout_ms: u64) -> bool {
        let cutoff = now_millis().saturating_sub(idle_timeout_ms);
        let inner = self.inner.lock();
        match &inner.client {
            Some(client) if !client.is_connected() => true,
            Some(_) => {
                inner.state == SlotState::Ready
                    && inner.active_requests == 0
                    && inner.last_activity <= cutoff
            }
            None => false,
        }
    }

    fn evict_if_idle(&self, idle_timeout_ms: u64) -> Option<Arc<IpcClient>> {
        let cutoff = now_millis().saturating_sub(idle_timeout_ms);
        let mut inner = self.inner.lock();
        if inner.state == SlotState::Retired {
            return None;
        }
        let should_evict = match &inner.client {
            Some(client) if !client.is_connected() => true,
            Some(_) => {
                inner.state == SlotState::Ready
                    && inner.active_requests == 0
                    && inner.last_activity <= cutoff
            }
            None => false,
        };
        if !should_evict {
            return None;
        }
        let client = inner.client.take();
        if client.is_some() {
            inner.state = SlotState::Evicted;
        }
        client
    }

    fn evict(&self) -> Option<Arc<IpcClient>> {
        let mut inner = self.inner.lock();
        if inner.state == SlotState::Retired {
            return None;
        }
        let client = inner.client.take();
        if client.is_some() {
            inner.state = SlotState::Evicted;
        }
        client
    }

    fn evict_generation(&self, generation: u64) -> Option<Arc<IpcClient>> {
        let mut inner = self.inner.lock();
        if inner.state == SlotState::Retired {
            return None;
        }
        if inner.generation != generation {
            return None;
        }
        let client = inner.client.take();
        if client.is_some() {
            inner.state = SlotState::Evicted;
        }
        client
    }

    fn reconnect(&self, client: Arc<IpcClient>) {
        let mut inner = self.inner.lock();
        if inner.state == SlotState::Retired {
            return;
        }
        inner.generation = inner
            .generation
            .checked_add(1)
            .expect("connection generation exhausted");
        inner.client = Some(client);
        inner.state = SlotState::Ready;
        inner.active_requests = 0;
        inner.last_activity = now_millis();
        self.notify.notify_waiters();
    }

    fn reconnect_generation(&self, generation: u64, address: &str, client: Arc<IpcClient>) -> bool {
        let mut inner = self.inner.lock();
        if inner.state == SlotState::Retired {
            return false;
        }
        if inner.generation != generation || inner.address != address {
            return false;
        }
        inner.generation = inner
            .generation
            .checked_add(1)
            .expect("connection generation exhausted");
        inner.client = Some(client);
        inner.state = SlotState::Ready;
        inner.active_requests = 0;
        inner.last_activity = now_millis();
        drop(inner);
        self.notify.notify_waiters();
        true
    }

    fn retire(&self) -> Option<Arc<IpcClient>> {
        let mut inner = self.inner.lock();
        inner.state = SlotState::Retired;
        let client = inner.client.take();
        drop(inner);
        self.notify.notify_waiters();
        client
    }
}

impl UpstreamLease {
    pub fn client(&self) -> Arc<IpcClient> {
        self.client.clone()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn address(&self) -> String {
        self.slot.address()
    }
}

impl Drop for UpstreamLease {
    fn drop(&mut self) {
        self.slot.end_request(self.generation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn new_pool_is_empty() {
        let pool = ConnectionPool::new();
        assert!(pool.list_connections().is_empty());
    }

    #[test]
    fn insert_and_lookup_ready() {
        let pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://test"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://test".into(), client);
        assert!(matches!(pool.lookup("grid"), CachedClient::Ready { .. }));
    }

    #[test]
    fn lookup_distinguishes_ready_evicted_disconnected_and_missing() {
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://test"));
        pool.insert("grid".into(), "ipc://test".into(), client);
        pool.remove("grid");
        assert!(matches!(pool.lookup("grid"), CachedClient::Missing));
    }

    #[test]
    fn idle_entries_detects_disconnected() {
        let pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://dead"));
        pool.insert("d".into(), "ipc://dead".into(), client);
        assert_eq!(pool.idle_entries(u64::MAX).len(), 1);
    }

    #[test]
    fn idle_entries_do_not_evict_active_connected_client() {
        let pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://active"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://active".into(), client);

        assert!(pool.begin_request("grid").is_some());

        assert!(pool.idle_entries(0).is_empty());
    }

    #[test]
    fn idle_entries_can_evict_inactive_connected_client() {
        let pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://idle"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://idle".into(), client);

        assert_eq!(pool.idle_entries(0), vec!["grid".to_string()]);
    }

    #[test]
    fn end_request_makes_client_idle_candidate_again() {
        let pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://active"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://active".into(), client);

        let (_, generation) = pool.begin_request("grid").unwrap();
        pool.end_request("grid", generation);

        assert_eq!(pool.idle_entries(0), vec!["grid".to_string()]);
    }

    #[test]
    fn disconnected_client_is_evicted_even_when_not_idle_by_time() {
        let pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://dead"));
        client.force_connected(false);
        pool.insert("grid".into(), "ipc://dead".into(), client);

        assert_eq!(pool.idle_entries(u64::MAX), vec!["grid".to_string()]);
    }

    #[test]
    fn stale_generation_end_after_reinsert_leaves_new_entry_idle() {
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
        let client = Arc::new(IpcClient::new("ipc://current"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://current".into(), client);
        let (_, generation) = pool.begin_request("grid").unwrap();

        assert!(pool.evict_generation("grid", generation).is_some());
        assert!(matches!(pool.lookup("grid"), CachedClient::Evicted { .. }));
    }

    #[test]
    fn stale_generation_evict_after_reconnect_does_not_evict_reconnected_client() {
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
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
        let pool = ConnectionPool::new();
        pool.next_generation.store(u64::MAX, Ordering::Release);

        let c1 = Arc::new(IpcClient::new("ipc://max"));
        pool.insert("max".into(), "ipc://max".into(), c1);

        let c2 = Arc::new(IpcClient::new("ipc://overflow"));
        pool.insert("overflow".into(), "ipc://overflow".into(), c2);
    }

    #[tokio::test]
    async fn concurrent_acquire_after_eviction_shares_one_reconnect() {
        let pool = Arc::new(ConnectionPool::new());
        let client = Arc::new(IpcClient::new("ipc://shared"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://shared".into(), client);
        pool.evict("grid");

        let connect_count = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();

        for _ in 0..16 {
            let pool = pool.clone();
            let connect_count = connect_count.clone();
            tasks.push(tokio::spawn(async move {
                pool.acquire_with("grid", move |address| {
                    let connect_count = connect_count.clone();
                    async move {
                        connect_count.fetch_add(1, Ordering::SeqCst);
                        let client = Arc::new(IpcClient::new(&address));
                        client.force_connected(true);
                        Ok(client)
                    }
                })
                .await
                .expect("acquire should reconnect")
            }));
        }

        let mut leases = Vec::new();
        for task in tasks {
            leases.push(task.await.unwrap());
        }

        assert_eq!(connect_count.load(Ordering::SeqCst), 1);
        let first = leases[0].client();
        assert!(
            leases
                .iter()
                .all(|lease| Arc::ptr_eq(&first, &lease.client()))
        );
    }

    #[tokio::test]
    async fn remove_during_reconnect_makes_waiting_acquire_not_found() {
        let pool = Arc::new(ConnectionPool::new());
        let client = Arc::new(IpcClient::new("ipc://removed"));
        client.force_connected(true);
        pool.insert("grid".into(), "ipc://removed".into(), client);
        pool.evict("grid");

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started_tx = Arc::new(std::sync::Mutex::new(Some(started_tx)));
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let finish_rx = Arc::new(tokio::sync::Mutex::new(Some(finish_rx)));

        let acquire = {
            let pool = pool.clone();
            let finish_rx = finish_rx.clone();
            let started_tx = started_tx.clone();
            tokio::spawn(async move {
                pool.acquire_with("grid", move |address| {
                    let finish_rx = finish_rx.clone();
                    let started_tx = started_tx.clone();
                    async move {
                        if let Some(started_tx) = started_tx.lock().unwrap().take() {
                            let _ = started_tx.send(());
                        }
                        let rx = finish_rx.lock().await.take().unwrap();
                        rx.await.unwrap();
                        let client = Arc::new(IpcClient::new(&address));
                        client.force_connected(true);
                        Ok(client)
                    }
                })
                .await
            })
        };

        started_rx.await.unwrap();
        pool.remove("grid");
        finish_tx.send(()).unwrap();

        assert!(matches!(
            acquire.await.unwrap(),
            Err(AcquireError::NotFound)
        ));
    }

    #[tokio::test]
    async fn insert_retires_replaced_slot_before_waiting_reconnect_completes() {
        let pool = Arc::new(ConnectionPool::new());
        let old_client = Arc::new(IpcClient::new("ipc://old"));
        old_client.force_connected(true);
        pool.insert("grid".into(), "ipc://old".into(), old_client);
        pool.evict("grid");

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started_tx = Arc::new(std::sync::Mutex::new(Some(started_tx)));
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let finish_rx = Arc::new(tokio::sync::Mutex::new(Some(finish_rx)));

        let acquire = {
            let pool = pool.clone();
            let finish_rx = finish_rx.clone();
            let started_tx = started_tx.clone();
            tokio::spawn(async move {
                pool.acquire_with("grid", move |address| {
                    let finish_rx = finish_rx.clone();
                    let started_tx = started_tx.clone();
                    async move {
                        if let Some(started_tx) = started_tx.lock().unwrap().take() {
                            let _ = started_tx.send(());
                        }
                        let rx = finish_rx.lock().await.take().unwrap();
                        rx.await.unwrap();
                        let client = Arc::new(IpcClient::new(&address));
                        client.force_connected(true);
                        Ok(client)
                    }
                })
                .await
            })
        };

        started_rx.await.unwrap();
        let replacement = Arc::new(IpcClient::new("ipc://new"));
        replacement.force_connected(true);
        pool.insert("grid".into(), "ipc://new".into(), replacement.clone());
        finish_tx.send(()).unwrap();

        assert!(matches!(
            acquire.await.unwrap(),
            Err(AcquireError::NotFound)
        ));
        let (current, _) = pool.begin_request("grid").unwrap();
        assert!(Arc::ptr_eq(&current, &replacement));
    }
}
