use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BufferStorage {
    Inline,
    Shm,
    Handle,
    FileSpill,
}

impl BufferStorage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Shm => "shm",
            Self::Handle => "handle",
            Self::FileSpill => "file_spill",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeaseRetention {
    Transient,
    Retained,
}

impl LeaseRetention {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Retained => "retained",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeaseDirection {
    ClientResponse,
    ResourceInput,
}

impl LeaseDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClientResponse => "client_response",
            Self::ResourceInput => "resource_input",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferLeaseMeta {
    pub route_name: String,
    pub method_name: String,
    pub direction: LeaseDirection,
    pub retention: LeaseRetention,
    pub storage: BufferStorage,
    pub bytes: usize,
}

#[derive(Debug, Clone)]
struct LeaseEntry {
    meta: BufferLeaseMeta,
    created_at: Instant,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageLeaseStats {
    pub active_leases: usize,
    pub active_holds: usize,
    pub total_leased_bytes: usize,
    pub total_held_bytes: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BufferLeaseStats {
    pub active_leases: usize,
    pub active_holds: usize,
    pub total_leased_bytes: usize,
    pub total_held_bytes: usize,
    pub oldest_hold_seconds: f64,
    pub by_storage: BTreeMap<BufferStorage, StorageLeaseStats>,
}

#[derive(Debug, Clone)]
pub struct BufferLeaseSnapshot {
    pub id: u64,
    pub route_name: String,
    pub method_name: String,
    pub direction: LeaseDirection,
    pub retention: LeaseRetention,
    pub storage: BufferStorage,
    pub bytes: usize,
    pub age_seconds: f64,
}

#[derive(Debug)]
struct BufferLeaseTrackerInner {
    track_transient: bool,
    next_id: AtomicU64,
    entries: Mutex<HashMap<u64, LeaseEntry>>,
}

impl BufferLeaseTrackerInner {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<u64, LeaseEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Debug, Clone)]
pub struct BufferLeaseTracker {
    inner: Arc<BufferLeaseTrackerInner>,
}

#[derive(Debug)]
pub struct BufferLeaseGuard {
    id: Option<u64>,
    tracker: Weak<BufferLeaseTrackerInner>,
}

impl BufferLeaseTracker {
    pub fn new(track_transient: bool) -> Self {
        Self {
            inner: Arc::new(BufferLeaseTrackerInner {
                track_transient,
                next_id: AtomicU64::new(1),
                entries: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn track(&self, meta: BufferLeaseMeta) -> BufferLeaseGuard {
        if meta.retention == LeaseRetention::Transient && !self.inner.track_transient {
            return BufferLeaseGuard {
                id: None,
                tracker: Weak::new(),
            };
        }

        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = LeaseEntry {
            meta,
            created_at: Instant::now(),
        };
        self.inner.entries().insert(id, entry);

        BufferLeaseGuard {
            id: Some(id),
            tracker: Arc::downgrade(&self.inner),
        }
    }

    pub fn stats(&self) -> BufferLeaseStats {
        let now = Instant::now();
        let entries = self.inner.entries();
        let mut stats = BufferLeaseStats::default();

        for entry in entries.values() {
            stats.active_leases = stats.active_leases.saturating_add(1);
            stats.total_leased_bytes = stats.total_leased_bytes.saturating_add(entry.meta.bytes);

            let storage = stats.by_storage.entry(entry.meta.storage).or_default();
            storage.active_leases = storage.active_leases.saturating_add(1);
            storage.total_leased_bytes =
                storage.total_leased_bytes.saturating_add(entry.meta.bytes);

            if entry.meta.retention == LeaseRetention::Retained {
                stats.active_holds = stats.active_holds.saturating_add(1);
                stats.total_held_bytes = stats.total_held_bytes.saturating_add(entry.meta.bytes);
                storage.active_holds = storage.active_holds.saturating_add(1);
                storage.total_held_bytes =
                    storage.total_held_bytes.saturating_add(entry.meta.bytes);

                let age = now.duration_since(entry.created_at).as_secs_f64();
                if age > stats.oldest_hold_seconds {
                    stats.oldest_hold_seconds = age;
                }
            }
        }

        stats
    }

    pub fn sweep_retained(&self, threshold: Duration) -> Vec<BufferLeaseSnapshot> {
        let now = Instant::now();
        let entries = self.inner.entries();
        let mut snapshots = Vec::new();

        for (id, entry) in entries.iter() {
            if entry.meta.retention != LeaseRetention::Retained {
                continue;
            }

            let age = now.duration_since(entry.created_at);
            if age >= threshold {
                snapshots.push(BufferLeaseSnapshot {
                    id: *id,
                    route_name: entry.meta.route_name.clone(),
                    method_name: entry.meta.method_name.clone(),
                    direction: entry.meta.direction,
                    retention: entry.meta.retention,
                    storage: entry.meta.storage,
                    bytes: entry.meta.bytes,
                    age_seconds: age.as_secs_f64(),
                });
            }
        }

        snapshots.sort_by_key(|snapshot| snapshot.id);
        snapshots
    }
}

impl Default for BufferLeaseTracker {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Drop for BufferLeaseGuard {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let Some(inner) = self.tracker.upgrade() else {
            return;
        };
        inner.entries().remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn retained_inline_meta() -> BufferLeaseMeta {
        BufferLeaseMeta {
            route_name: "grid".to_string(),
            method_name: "subdivide_grids".to_string(),
            direction: LeaseDirection::ClientResponse,
            retention: LeaseRetention::Retained,
            storage: BufferStorage::Inline,
            bytes: 128,
        }
    }

    #[test]
    fn retained_inline_lease_counts_until_guard_drop() {
        let tracker = BufferLeaseTracker::new(false);
        let guard = tracker.track(retained_inline_meta());
        let stats = tracker.stats();
        assert_eq!(stats.active_holds, 1);
        assert_eq!(stats.total_held_bytes, 128);
        assert_eq!(
            stats
                .by_storage
                .get(&BufferStorage::Inline)
                .unwrap()
                .active_holds,
            1
        );
        drop(guard);
        let stats = tracker.stats();
        assert_eq!(stats.active_holds, 0);
        assert_eq!(stats.total_held_bytes, 0);
    }

    #[test]
    fn retained_shm_snapshot_reports_route_method_storage_and_age() {
        let tracker = BufferLeaseTracker::new(false);
        let _guard = tracker.track(BufferLeaseMeta {
            route_name: "grid".to_string(),
            method_name: "subdivide_grids".to_string(),
            direction: LeaseDirection::ClientResponse,
            retention: LeaseRetention::Retained,
            storage: BufferStorage::Shm,
            bytes: 8192,
        });
        std::thread::sleep(Duration::from_millis(5));
        let stale = tracker.sweep_retained(Duration::from_millis(1));
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].route_name, "grid");
        assert_eq!(stale[0].method_name, "subdivide_grids");
        assert_eq!(stale[0].storage, BufferStorage::Shm);
        assert_eq!(stale[0].bytes, 8192);
        assert!(stale[0].age_seconds > 0.0);
    }

    #[test]
    fn transient_leases_are_noop_by_default_to_keep_view_path_cheap() {
        let tracker = BufferLeaseTracker::new(false);
        let _guard = tracker.track(BufferLeaseMeta {
            route_name: "grid".to_string(),
            method_name: "subdivide_grids".to_string(),
            direction: LeaseDirection::ClientResponse,
            retention: LeaseRetention::Transient,
            storage: BufferStorage::Shm,
            bytes: 4096,
        });
        let stats = tracker.stats();
        assert_eq!(stats.active_leases, 0);
        assert_eq!(stats.active_holds, 0);
    }

    #[test]
    fn transient_tracking_can_be_enabled_for_diagnostics() {
        let tracker = BufferLeaseTracker::new(true);
        let guard = tracker.track(BufferLeaseMeta {
            route_name: "grid".to_string(),
            method_name: "subdivide_grids".to_string(),
            direction: LeaseDirection::ClientResponse,
            retention: LeaseRetention::Transient,
            storage: BufferStorage::Shm,
            bytes: 4096,
        });
        let stats = tracker.stats();
        assert_eq!(stats.active_leases, 1);
        assert_eq!(stats.active_holds, 0);
        assert_eq!(stats.total_leased_bytes, 4096);
        drop(guard);
        assert_eq!(tracker.stats().active_leases, 0);
    }
}
