//! Reference-counted pool of [`HttpClient`] instances.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::client::{HttpClient, HttpError};

// ── Pool entry ──────────────────────────────────────────────────────────

fn canonical_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_owned()
}

struct PoolEntry {
    client: Arc<HttpClient>,
    ref_count: usize,
    use_proxy: bool,
    timeout_secs: f64,
    /// Set to `Some(Instant::now())` when `ref_count` drops to 0.
    last_release: Option<Instant>,
}

// ── HttpClientPool ──────────────────────────────────────────────────────

/// Reference-counted pool of [`HttpClient`] instances.
///
/// Clients connecting to the same relay URL share a single
/// `HttpClient` (and its underlying connection pool).  When all
/// references are released, the client is kept for a grace period
/// before being destroyed.
pub struct HttpClientPool {
    entries: Mutex<HashMap<String, PoolEntry>>,
    grace_period: Duration,
    default_timeout: f64,
    default_max_connections: usize,
}

// Compile-time assertion: HttpClientPool must be Send + Sync.
const _: () = {
    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}
    fn _assertions() {
        _assert_send::<HttpClientPool>();
        _assert_sync::<HttpClientPool>();
    }
};

impl HttpClientPool {
    /// Create a new pool with the given grace period.
    pub fn new(grace_secs: f64) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            grace_period: Duration::from_secs_f64(grace_secs),
            default_timeout: 300.0,
            default_max_connections: 100,
        }
    }

    /// Acquire (or create) a client for `base_url` with an explicit proxy policy.
    pub fn acquire_with_proxy_policy(
        &self,
        base_url: &str,
        use_proxy: bool,
    ) -> Result<Arc<HttpClient>, HttpError> {
        self.acquire_with_options(base_url, use_proxy, self.default_timeout)
    }

    /// Acquire (or create) a client for `base_url` with explicit proxy and timeout policy.
    pub fn acquire_with_options(
        &self,
        base_url: &str,
        use_proxy: bool,
        timeout_secs: f64,
    ) -> Result<Arc<HttpClient>, HttpError> {
        self.sweep_expired();
        let key = canonical_base_url(base_url);

        let mut entries = self.entries.lock();

        if let Some(entry) = entries.get_mut(&key) {
            if entry.use_proxy == use_proxy && entry.timeout_secs == timeout_secs {
                entry.ref_count += 1;
                entry.last_release = None;
                return Ok(Arc::clone(&entry.client));
            }
            if entry.ref_count > 0 && entry.use_proxy != use_proxy {
                return Err(HttpError::Transport(format!(
                    "active pooled HTTP client for {base_url} has proxy policy mismatch"
                )));
            }
            if entry.ref_count > 0 {
                return Err(HttpError::Transport(format!(
                    "active pooled HTTP client for {base_url} has timeout policy mismatch"
                )));
            }
        }

        // Create a new client (lock held — HttpClient::new is fast).
        let client = Arc::new(HttpClient::new_with_proxy_policy(
            &key,
            timeout_secs,
            self.default_max_connections,
            use_proxy,
        )?);

        entries.insert(
            key,
            PoolEntry {
                client: Arc::clone(&client),
                ref_count: 1,
                use_proxy,
                timeout_secs,
                last_release: None,
            },
        );

        Ok(client)
    }

    /// Decrement reference count; mark for grace-period cleanup at 0.
    pub fn release(&self, base_url: &str) {
        let key = canonical_base_url(base_url);
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.get_mut(&key) {
            if entry.ref_count == 0 {
                eprintln!("HttpClientPool::release: ref_count already 0 for {base_url}");
                return;
            }
            entry.ref_count -= 1;
            if entry.ref_count == 0 {
                entry.last_release = Some(Instant::now());
            }
        }
    }

    /// Sweep entries past the grace period.
    pub fn sweep_expired(&self) {
        let mut entries = self.entries.lock();
        let grace = self.grace_period;
        entries.retain(|_url, entry| {
            if entry.ref_count == 0 {
                if let Some(released_at) = entry.last_release {
                    if released_at.elapsed() >= grace {
                        return false;
                    }
                }
            }
            true
        });
    }

    /// Destroy all clients immediately.
    pub fn shutdown_all(&self) {
        let mut entries = self.entries.lock();
        entries.clear();
    }

    /// Number of active entries.
    pub fn active_count(&self) -> usize {
        self.entries.lock().len()
    }

    /// Reference count for a specific URL.
    pub fn refcount(&self, base_url: &str) -> usize {
        self.entries
            .lock()
            .get(&canonical_base_url(base_url))
            .map_or(0, |e| e.ref_count)
    }
}

// ── Singleton ───────────────────────────────────────────────────────────

static GLOBAL_HTTP_POOL: OnceLock<HttpClientPool> = OnceLock::new();

impl HttpClientPool {
    /// Return the process-level singleton.
    pub fn instance() -> &'static HttpClientPool {
        GLOBAL_HTTP_POOL.get_or_init(|| HttpClientPool::new(60.0))
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_new() {
        let pool = HttpClientPool::new(30.0);
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn test_pool_singleton() {
        let p1 = HttpClientPool::instance() as *const HttpClientPool;
        let p2 = HttpClientPool::instance() as *const HttpClientPool;
        assert_eq!(p1, p2, "singleton must return the same instance");
    }

    #[test]
    fn test_pool_acquire_with_proxy_policy_release() {
        let pool = HttpClientPool::new(60.0);
        let url = "http://localhost:9999";

        let _client = pool.acquire_with_proxy_policy(url, false).unwrap();
        assert_eq!(pool.active_count(), 1);
        assert_eq!(pool.refcount(url), 1);

        let _client2 = pool.acquire_with_proxy_policy(url, false).unwrap();
        assert_eq!(pool.refcount(url), 2);

        pool.release(url);
        assert_eq!(pool.refcount(url), 1);

        pool.release(url);
        assert_eq!(pool.refcount(url), 0);
    }

    #[test]
    fn trailing_slash_variants_share_one_pool_entry() {
        let pool = HttpClientPool::new(60.0);

        let first = pool
            .acquire_with_proxy_policy("http://localhost:9998", false)
            .unwrap();
        let second = pool
            .acquire_with_proxy_policy("http://localhost:9998/", false)
            .unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(pool.active_count(), 1);
        assert_eq!(pool.refcount("http://localhost:9998"), 2);
        assert_eq!(pool.refcount("http://localhost:9998/"), 2);

        pool.release("http://localhost:9998/");
        assert_eq!(pool.refcount("http://localhost:9998"), 1);
        pool.release("http://localhost:9998");
        assert_eq!(pool.refcount("http://localhost:9998/"), 0);
    }

    #[test]
    fn active_proxy_policy_mismatch_is_rejected_without_refcount_change() {
        let pool = HttpClientPool::new(60.0);
        let url = "http://localhost:9995";

        let _client = pool.acquire_with_proxy_policy(url, false).unwrap();
        let err = match pool.acquire_with_proxy_policy(url, true) {
            Ok(_) => panic!("active client with different proxy policy must be rejected"),
            Err(err) => err,
        };

        assert!(
            err.to_string().contains("proxy policy mismatch"),
            "unexpected error: {err}"
        );
        assert_eq!(pool.refcount(url), 1);
    }

    #[test]
    fn released_proxy_policy_mismatch_replaces_idle_entry() {
        let pool = HttpClientPool::new(60.0);
        let url = "http://localhost:9994";

        let first = pool.acquire_with_proxy_policy(url, false).unwrap();
        pool.release(url);

        let second = pool.acquire_with_proxy_policy(url, true).unwrap();

        assert!(
            !Arc::ptr_eq(&first, &second),
            "idle entry with stale proxy policy should be replaced"
        );
        assert_eq!(pool.refcount(url), 1);
    }

    #[test]
    fn active_timeout_policy_mismatch_is_rejected_without_refcount_change() {
        let pool = HttpClientPool::new(60.0);
        let url = "http://localhost:9993";

        let _client = pool.acquire_with_options(url, false, 300.0).unwrap();
        let err = match pool.acquire_with_options(url, false, 900.0) {
            Ok(_) => panic!("active client with different timeout policy must be rejected"),
            Err(err) => err,
        };

        assert!(
            err.to_string().contains("timeout policy mismatch"),
            "unexpected error: {err}"
        );
        assert_eq!(pool.refcount(url), 1);
    }

    #[test]
    fn released_timeout_policy_mismatch_replaces_idle_entry() {
        let pool = HttpClientPool::new(60.0);
        let url = "http://localhost:9992";

        let first = pool.acquire_with_options(url, false, 300.0).unwrap();
        pool.release(url);

        let second = pool.acquire_with_options(url, false, 900.0).unwrap();

        assert!(
            !Arc::ptr_eq(&first, &second),
            "idle entry with stale timeout policy should be replaced"
        );
        assert_eq!(pool.refcount(url), 1);
    }

    #[test]
    fn test_pool_sweep_expired() {
        let pool = HttpClientPool::new(0.0); // zero grace

        let url = "http://localhost:9998";
        let _client = pool.acquire_with_proxy_policy(url, false).unwrap();
        pool.release(url);

        // After release with zero grace, sweep should remove it.
        std::thread::sleep(Duration::from_millis(10));
        pool.sweep_expired();
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn test_pool_shutdown_all() {
        let pool = HttpClientPool::new(60.0);
        let _c1 = pool
            .acquire_with_proxy_policy("http://localhost:9997", false)
            .unwrap();
        let _c2 = pool
            .acquire_with_proxy_policy("http://localhost:9996", false)
            .unwrap();
        assert_eq!(pool.active_count(), 2);

        pool.shutdown_all();
        assert_eq!(pool.active_count(), 0);
    }
}
