# Route Concurrency Rust Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move route concurrency state, limits, lifecycle, and fast-path projection into Rust so the same `ConcurrencyConfig` governs thread-local, direct IPC, and relay-backed HTTP without a Python-owned duplicate scheduler or hidden default fallback.

**Architecture:** Rust owns one shared per-route concurrency handle that resolves mode and limits once, exposes a read-only snapshot, and enforces admission/execution control for both Rust-dispatched remote calls and Python thread-local direct calls. Python keeps `ConcurrencyConfig` as input plus a thin adapter for same-process calls, but it no longer owns pending counters, a worker executor, or the default policy. The direct-call hot path stays zero-copy and continues to invoke the Python resource object; only the permit / mode decision moves into Rust, and only when the route is actually constrained.

**Tech Stack:** Rust `core/transport/c2-server`, PyO3 bindings in `sdk/python/native`, Python bridge in `sdk/python/src/c_two/transport`, pytest unit/integration tests, Cargo unit tests.

---

## 0.x Constraint: Prefer Clean Cuts Over Compatibility Debt

C-Two is in the 0.x line. Do not preserve compatibility shims for stale scheduler ownership, hidden `max_workers=4` defaults, or dual-state execution paths. Remove obsolete code paths, tests, docs, and module surfaces in the same change that replaces them.

## Non-Negotiable Runtime Constraints

1. **Rust is the single authority for route concurrency state.** One shared handle must own mode, `max_pending`, `max_workers`, and closing state for both local and remote execution.
2. **Direct IPC remains relay-independent.** Explicit `cc.connect(CRMClass, name="route", address="ipc://server")` must continue to bypass relay discovery and remain usable when relay is unset or unreachable.
3. **No extra bytes copy.** Concurrency migration must not change `RequestData -> PyShmBuffer -> memoryview(request_buf)` on the remote path, and must not route same-process thread-local calls through remote bytes dispatch.
4. **No Python-owned scheduler authority.** `NativeServerBridge` must not synthesize `ConcurrencyConfig(max_workers=4)` or any other hidden fallback before Rust sees the route config.
5. **No silent acceptance of unenforced limits.** If a concurrency field cannot be enforced by the native handle, registration must fail rather than accept and ignore it.

## Current Problem

Today the route concurrency boundary is split across languages:

- Python creates and owns `ConcurrencyConfig` plus a full `Scheduler` implementation with pending counters, an executor, and `begin()` / `execute()` / `shutdown()` state.
- `NativeServerBridge.__init__()` currently injects a Python-side default `ConcurrencyConfig(max_workers=max_workers)` when the caller omits a config.
- `NativeServerBridge.register_crm()` forwards only part of the concurrency state into Rust.
- The Python same-process path still relies on Python scheduler state, while Rust remote dispatch enforces only a subset of route behavior.

That split means the same registration input can still diverge by transport or by language-default behavior.

## File Responsibility Map

- Modify `core/transport/c2-server/src/scheduler.rs`: extend the route scheduler into the single Rust authority for mode, `max_pending`, `max_workers`, snapshot projection, close state, and guard acquisition.
- Modify `core/transport/c2-server/src/dispatcher.rs`: store the shared concurrency handle on each route and preserve a single `Arc` across local and remote execution paths.
- Modify `core/transport/c2-server/src/server.rs`: keep route registration / unregister sequencing aligned with the new close semantics and route-table ownership.
- Create or modify `sdk/python/native/src/route_concurrency_ffi.rs`: expose the native route-concurrency handle to Python as a small pyclass wrapper.
- Modify `sdk/python/native/src/server_ffi.rs`: return the native concurrency handle from `register_route()` and stop accepting Python-owned scheduler defaults as authoritative.
- Modify `sdk/python/native/src/lib.rs`: register the new route-concurrency pyclass module.
- Modify `sdk/python/src/c_two/transport/server/native.py`: stop injecting the `max_workers=4` default, store the native handle in each slot, and coordinate unregister/shutdown against the shared handle.
- Modify `sdk/python/src/c_two/transport/server/scheduler.py`: reduce the class to a thin adapter over the native handle; remove owning state.
- Modify `sdk/python/src/c_two/transport/client/proxy.py`: keep the thread-local fast path direct for unconstrained routes and use the native guard only when the route is actually constrained.
- Modify `sdk/python/src/c_two/transport/registry.py`: pass route concurrency through unchanged and wire local proxy creation from the native handle snapshot.
- Modify `sdk/python/tests/unit/test_scheduler.py`: rewrite legacy Python-scheduler tests around the thin adapter and remove ownership-only expectations.
- Modify `sdk/python/tests/unit/test_security.py`: remove direct assertions on Python-owned pending state and executor state.
- Modify `sdk/python/tests/unit/test_proxy_concurrency.py`: add coverage for the unconstrained no-op direct-call fast path and the constrained guard path.
- Modify `sdk/python/tests/unit/test_name_collision.py`: update fake `register_route()` signatures and teardown-order assertions.
- Modify `sdk/python/tests/integration/test_remote_scheduler_config.py`: add mixed local/remote limit coverage and retain the bad-relay direct-address regression.
- Modify `sdk/python/tests/integration/test_concurrency_safety.py`: add shutdown / unregister drain coverage for the shared handle.
- Modify `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: after implementation, update the boundary note to describe the Rust-owned route concurrency handle instead of Python-owned scheduler state.
- Modify `AGENTS.md` only if the durable SDK boundary guidance changes after this work.

---

## Design

### 1) One native route-concurrency handle

The Rust `Scheduler` should grow the missing state rather than leaving Python to own it:

- `mode`
- `max_pending`
- `max_workers`
- `closed` / `closing`
- read-only `snapshot()` data for SDK projection
- a cheap `is_unconstrained()` fast-path check
- a guard acquisition API that can fail fast when the route is closed or at capacity

The handle must be cloneable into both:

- the Rust route stored by `c2-server`
- the Python slot used by same-process direct calls

Concrete Rust names to use in `core/transport/c2-server/src/scheduler.rs`:

```rust
pub struct SchedulerLimits {
    pub max_pending: Option<std::num::NonZeroUsize>,
    pub max_workers: Option<std::num::NonZeroUsize>,
}

impl Default for SchedulerLimits {
    fn default() -> Self {
        Self {
            max_pending: None,
            max_workers: None,
        }
    }
}

impl SchedulerLimits {
    pub fn try_from_usize(
        max_pending: Option<usize>,
        max_workers: Option<usize>,
    ) -> Result<Self, String> {
        let max_pending = max_pending
            .map(|value| {
                std::num::NonZeroUsize::new(value)
                    .ok_or_else(|| "max_pending must be at least 1".to_string())
            })
            .transpose()?;
        let max_workers = max_workers
            .map(|value| {
                std::num::NonZeroUsize::new(value)
                    .ok_or_else(|| "max_workers must be at least 1".to_string())
            })
            .transpose()?;
        Ok(Self {
            max_pending,
            max_workers,
        })
    }
}

pub struct SchedulerSnapshot {
    pub mode: ConcurrencyMode,
    pub max_pending: Option<usize>,
    pub max_workers: Option<usize>,
    pub pending: usize,
    pub active_workers: usize,
    pub closed: bool,
    pub is_unconstrained: bool,
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
}

struct SchedulerInner {
    mode: ConcurrencyMode,
    access_map: HashMap<u16, AccessLevel>,
    limits: SchedulerLimits,
    state: Mutex<SchedulerState>,
    cvar: Condvar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerAcquireError {
    Closed,
    Capacity {
        field: &'static str,
        limit: usize,
    },
}

impl ConcurrencyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ConcurrencyMode::Parallel => "parallel",
            ConcurrencyMode::Exclusive => "exclusive",
            ConcurrencyMode::ReadParallel => "read_parallel",
        }
    }
}

pub struct SchedulerGuard {
    // Owned RAII guard; no borrowed Mutex/RwLock guard is stored here.
}
```

`Scheduler` may remain the Rust type name to minimize churn inside
`c2-server`, but it is the route-concurrency authority. The PyO3 class should
be named `RouteConcurrency` because that is the SDK-facing concept. Prefer
passing `Scheduler` clones across PyO3 because each clone points to the same
`Arc<SchedulerInner>`; avoid wrapping an extra `Arc<Scheduler>` unless an
existing `CrmRoute` field still requires it.

Do not hold a Tokio async lock guard inside Python. Implement the shared
authority with runtime-independent state (`std::sync::Mutex` + `Condvar`, or an
equivalent owned-state primitive) where `SchedulerGuard` owns only an `Arc` and
releases counters / read-write state in `Drop`. Remote execution must acquire
the guard inside `tokio::task::spawn_blocking`; Python `__enter__` must release
the GIL while waiting for read/write serialization. This keeps mode waiting out
of Tokio core threads and avoids Python context-manager lifetimes around async
guards.

Capacity semantics:

- `max_pending` is a fail-fast admission cap for route calls that have entered
  the scheduler, including calls waiting for read/write mode or worker permits.
- `max_workers` is a fail-fast cap for concurrent callback execution permits.
- `exclusive` and `read_parallel` mode serialization may wait; capacity
  exhaustion must not wait indefinitely.
- If both caps are set, both are enforced. On any failed acquisition, no method
  body is entered and any already-materialized request resource must be
  released.

### 2) Python keeps only a thin adapter

`Scheduler` in `sdk/python/src/c_two/transport/server/scheduler.py` becomes a small adapter around the native handle. It may expose:

- `method_idx(method_name)`
- `execution_guard(method_idx)`
- `snapshot()`
- `close()` / `shutdown()` as forwarding methods
- a cached fast-path flag derived from the snapshot

It must not own `ThreadPoolExecutor`, `_pending_count`, or any other policy state.

### 3) Route registration returns the native handle

`RustServer.register_route()` should return the native concurrency handle (or a small wrapper around it) instead of `()`. `NativeServerBridge.register_crm()` stores that handle in the route slot and exposes it to `CRMProxy.thread_local()`. The same call must pass `mode`, `max_pending`, and `max_workers`; omitting either limit from the FFI call is a correctness failure.

### 4) Constrained routes use the native guard; unconstrained routes stay zero-overhead

`CRMProxy.call_direct()` should keep a pure Python direct call path when the
route snapshot is truly unconstrained (`mode=parallel`,
`max_pending=None`, `max_workers=None`, and not closed). When the route is
constrained, it should enter the native guard before invoking the Python method
body. `read_parallel` and `exclusive` are constrained even when both capacity
fields are `None`.

This preserves the hot path while still keeping Rust authoritative for real limits.

### 5) Lifecycle / teardown is explicit

The shared handle needs deterministic shutdown semantics so local and remote calls do not race:

1. Rust atomically marks the route handle closing and removes the route from
   the dispatch table under the same dispatcher write lock.
2. New local guards fail immediately because the Python slot observes the same
   already-closed handle; new remote lookups fail because the route is gone.
3. Python invokes the CRM shutdown callback.
4. Python drops the slot and releases its clone of the native handle.
5. In-flight calls are allowed to drain; only new calls are rejected.

Shutdown must be idempotent. Unregister and global shutdown must be safe in either order.

---

## Implementation Tasks

### Task 1: Add failing Rust tests for the shared route handle and remote cleanup

**Files:**
- Modify: `core/transport/c2-server/src/scheduler.rs`
- Modify: `core/transport/c2-server/src/dispatcher.rs`
- Modify: `core/transport/c2-server/src/server.rs`

- [ ] **Step 1: Add tests for snapshot, close, and constrained / unconstrained behavior**

Add Rust tests that exercise the intended API before the implementation exists:

```rust
use std::num::NonZeroUsize;
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::time::Duration;

#[test]
fn unconstrained_snapshot_reports_fast_path() {
    let sched = Scheduler::with_limits(
        ConcurrencyMode::Parallel,
        HashMap::new(),
        SchedulerLimits::default(),
    );
    let snap = sched.snapshot();
    assert!(snap.is_unconstrained);
    assert!(!snap.closed);
    assert_eq!(snap.max_pending, None);
    assert_eq!(snap.max_workers, None);
}

#[test]
fn close_rejects_new_acquires_but_keeps_snapshot() {
    let sched = Scheduler::with_limits(
        ConcurrencyMode::Exclusive,
        HashMap::new(),
        SchedulerLimits::default(),
    );
    sched.close();
    let snap = sched.snapshot();
    assert!(snap.closed);
    assert_eq!(
        sched.try_acquire(0).unwrap_err(),
        SchedulerAcquireError::Closed,
    );
}
```

- [ ] **Step 2: Add a limit test that exercises both `max_pending` and `max_workers`**

Add a concurrency test that proves the handle rejects extra calls instead of silently accepting them:

```rust
#[test]
fn route_limits_fail_fast_when_capacity_is_exhausted() {
    let sched = Arc::new(Scheduler::with_limits(
        ConcurrencyMode::Parallel,
        HashMap::new(),
        SchedulerLimits {
            max_pending: Some(NonZeroUsize::new(1).unwrap()),
            max_workers: Some(NonZeroUsize::new(1).unwrap()),
        },
    ));

    let first = sched.try_acquire(0).expect("first acquire should enter");
    let err = sched.try_acquire(0).unwrap_err();
    assert_eq!(
        err,
        SchedulerAcquireError::Capacity {
            field: "max_pending",
            limit: 1,
        },
    );

    drop(first);
    let second = sched.try_acquire(0).expect("capacity releases on drop");
    drop(second);
    assert_eq!(sched.snapshot().pending, 0);
    assert_eq!(sched.snapshot().active_workers, 0);
}

#[test]
fn exclusive_waits_without_consuming_extra_worker_permits() {
    let sched = Arc::new(Scheduler::with_limits(
        ConcurrencyMode::Exclusive,
        HashMap::new(),
        SchedulerLimits {
            max_pending: Some(NonZeroUsize::new(2).unwrap()),
            max_workers: Some(NonZeroUsize::new(2).unwrap()),
        },
    ));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (tx, rx) = mpsc::channel();

    let s1 = Arc::clone(&sched);
    let entered1 = Arc::clone(&entered);
    let release1 = Arc::clone(&release);
    let first = std::thread::spawn(move || {
        let _guard = s1.blocking_acquire(0).expect("first enters");
        entered1.wait();
        release1.wait();
    });

    entered.wait();

    let s2 = Arc::clone(&sched);
    let second = std::thread::spawn(move || {
        let _guard = s2.blocking_acquire(0).expect("second waits then enters");
        tx.send(()).unwrap();
    });

    assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
    release.wait();
    first.join().unwrap();
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    second.join().unwrap();
}
```

- [ ] **Step 3: Add remote acquisition-failure cleanup coverage**

Add a focused helper test in `core/transport/c2-server/src/server.rs` that
proves a materialized request is cleaned up when the scheduler rejects
admission. Use a real `MemPool` allocation so the test verifies the actual
`cleanup_request()` path rather than a mock:

```rust
#[test]
fn rejected_acquire_cleans_materialized_request() {
    let pool = Arc::new(parking_lot::RwLock::new(MemPool::new(PoolConfig {
        segment_size: 4096,
        min_block_size: 128,
        max_segments: 1,
        ..PoolConfig::default()
    })));
    pool.write().ensure_ready().unwrap();
    let alloc = pool.write().alloc(128).unwrap();
    let request = RequestData::Shm {
        pool: Arc::clone(&pool),
        seg_idx: alloc.seg_idx as u16,
        offset: alloc.offset,
        data_size: 128,
        is_dedicated: alloc.is_dedicated,
    };
    let sched = Arc::new(Scheduler::with_limits(
        ConcurrencyMode::Parallel,
        HashMap::new(),
        SchedulerLimits {
            max_pending: Some(NonZeroUsize::new(1).unwrap()),
            max_workers: Some(NonZeroUsize::new(1).unwrap()),
        },
    ));
    let _first = sched.try_acquire(0).unwrap();

    let err = execute_request_for_test((*sched).clone(), 0, request, |_request| {
        panic!("callback must not run when acquire fails")
    })
    .unwrap_err();

    assert!(matches!(
        err,
        RouteExecutionError::Acquire(SchedulerAcquireError::Capacity {
            field: "max_pending",
            limit: 1,
        })
    ));
    let reused = pool.write().alloc(128).unwrap();
    assert_eq!(reused.offset, alloc.offset);
}
```

The important invariant is concrete: `cleanup_request(request)` runs on every
scheduler-acquisition failure after request materialization.

- [ ] **Step 4: Run the failing Rust tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server scheduler -q
```

Expected before implementation: compilation fails or the new tests fail because `snapshot()`, `close()`, `try_acquire()`, `with_limits()`, and acquisition-failure cleanup do not exist yet.

- [ ] **Step 5: Implement the native route handle and route storage**

Update `core/transport/c2-server/src/scheduler.rs` so the scheduler owns the full route-concurrency state and can be shared by both Rust dispatch and Python direct calls. Keep the existing read/write semantics, but add:

- `SchedulerLimits`, `SchedulerSnapshot`, `SchedulerAcquireError`, and `SchedulerGuard`
- `Scheduler::with_limits(mode, access_map, limits)`
- `Scheduler::snapshot()`
- `Scheduler::close()`
- `Scheduler::try_acquire(method_idx)` for fail-fast tests and already-available permits
- `Scheduler::blocking_acquire(method_idx)` for remote `spawn_blocking` and Python `with` paths
- `Scheduler::is_unconstrained()` derived from `mode=Parallel`, no caps, and not closed

`Scheduler::execute()` or `execute_request()` must remain the only
remote-dispatch wrappers used by `server.rs`. They should run
`blocking_acquire(method_idx)` inside `tokio::task::spawn_blocking` and hold the
`SchedulerGuard` until the callback returns:

```rust
pub async fn execute<F, R>(&self, method_idx: u16, f: F) -> Result<R, SchedulerAcquireError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let sched = self.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = sched.blocking_acquire(method_idx)?;
        Ok(f())
    })
    .await
    .expect("scheduler task panicked")
}
```

Do not acquire a blocking read/write wait on a Tokio worker thread before
`spawn_blocking`.

Update `dispatcher.rs` and `server.rs` only as needed so each `CrmRoute` stores
the same shared scheduler authority that Python receives. It is acceptable for
`CrmRoute` to continue storing `Arc<Scheduler>` for compatibility, where each
`Scheduler` clone internally points to the same `Arc<SchedulerInner>`.

- [ ] **Step 6: Map scheduler failures and clean request resources in remote dispatch**

Update each remote dispatch branch in `core/transport/c2-server/src/server.rs`
that currently calls `route.scheduler.execute(...)`:

- inline request path;
- buddy SHM request path;
- chunked/reassembled request path;
- any relay-forwarded path that ultimately enters the same server dispatch.

For paths where `RequestData` has already been built before scheduler
acquisition, move cleanup into the same blocking closure that owns the request.
Do not clone or convert SHM payload bytes to make cleanup easier.

Use this shape so the request is either consumed by the callback or cleaned in
the acquisition-error branch:

```rust
let result = route
    .scheduler
    .as_ref()
    .clone()
    .execute_request(idx, request, move |request| {
        callback.invoke(&name, idx, request, resp_pool)
    })
    .await;

match result {
    Ok(meta) => {
        send_response_meta(
            &server.response_pool,
            writer,
            request_id,
            meta,
            server.config.shm_threshold,
            server.config.chunk_size as usize,
        )
        .await
    }
    Err(RouteExecutionError::Crm(CrmError::UserError(b))) => {
        write_reply(writer, request_id, &ReplyControl::Error(b)).await;
    }
    Err(RouteExecutionError::Crm(CrmError::InternalError(s))) => {
        write_reply(writer, request_id, &ReplyControl::Error(s.into_bytes())).await;
    }
    Err(RouteExecutionError::Acquire(SchedulerAcquireError::Closed)) => {
        write_reply(writer, request_id, &ReplyControl::Error(b"route closed".to_vec())).await;
    }
    Err(RouteExecutionError::Acquire(SchedulerAcquireError::Capacity { field, limit })) => {
        let msg = format!("route concurrency capacity exceeded: {field}={limit}");
        write_reply(writer, request_id, &ReplyControl::Error(msg.into_bytes())).await;
    }
}
```

Implement the helper in `scheduler.rs` or `server.rs` with this invariant:

```rust
pub enum RouteExecutionError {
    Acquire(SchedulerAcquireError),
    Crm(CrmError),
}

pub async fn execute_request<F>(
    scheduler: Scheduler,
    method_idx: u16,
    request: RequestData,
    f: F,
) -> Result<ResponseMeta, RouteExecutionError>
where
    F: FnOnce(RequestData) -> Result<ResponseMeta, CrmError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let guard = match scheduler.blocking_acquire(method_idx) {
            Ok(guard) => guard,
            Err(err) => {
                cleanup_request(request);
                return Err(RouteExecutionError::Acquire(err));
            }
        };
        let _guard = guard;
        f(request).map_err(RouteExecutionError::Crm)
    })
    .await
    .expect("route execution task panicked")
}
```

Inline requests do not own SHM resources, but should use the same error mapping
for consistent route-closed / route-capacity replies.

- [ ] **Step 7: Re-run the Rust tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server scheduler -q
```

Expected: the new handle tests pass, and the existing remote-mode tests still pass.

- [ ] **Step 8: Commit the Rust core handle work**

```bash
git add core/transport/c2-server/src/scheduler.rs core/transport/c2-server/src/dispatcher.rs core/transport/c2-server/src/server.rs
git commit -m "feat: make route concurrency rust-owned"
```

### Task 2: Expose the native handle through PyO3 and remove Python default injection

**Files:**
- Create: `sdk/python/native/src/route_concurrency_ffi.rs`
- Modify: `sdk/python/native/src/server_ffi.rs`
- Modify: `sdk/python/native/src/lib.rs`
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Modify: `sdk/python/tests/unit/test_name_collision.py`
- Modify: `sdk/python/tests/unit/test_ipc_config.py` if constructor arguments change

- [ ] **Step 1: Add a PyO3 wrapper for the native route handle**

Create a small pyclass wrapper that exposes the native handle to Python:

```rust
use std::sync::Arc;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use c2_server::scheduler::{
    Scheduler,
    SchedulerAcquireError,
    SchedulerSnapshot,
};

#[pyclass(name = "RouteConcurrency", frozen)]
pub struct PyRouteConcurrency {
    pub(crate) inner: Scheduler,
}

#[pyclass(name = "RouteConcurrencySnapshot", frozen)]
#[derive(Clone)]
pub struct PyRouteConcurrencySnapshot {
    #[pyo3(get)]
    pub mode: String,
    #[pyo3(get)]
    pub max_pending: Option<usize>,
    #[pyo3(get)]
    pub max_workers: Option<usize>,
    #[pyo3(get)]
    pub pending: usize,
    #[pyo3(get)]
    pub active_workers: usize,
    #[pyo3(get)]
    pub closed: bool,
    #[pyo3(get)]
    pub is_unconstrained: bool,
}

#[pyclass(name = "RouteConcurrencyGuard")]
pub struct PyRouteConcurrencyGuard {
    inner: Option<c2_server::scheduler::SchedulerGuard>,
}

impl From<SchedulerSnapshot> for PyRouteConcurrencySnapshot {
    fn from(snapshot: SchedulerSnapshot) -> Self {
        Self {
            mode: snapshot.mode.as_str().to_string(),
            max_pending: snapshot.max_pending,
            max_workers: snapshot.max_workers,
            pending: snapshot.pending,
            active_workers: snapshot.active_workers,
            closed: snapshot.closed,
            is_unconstrained: snapshot.is_unconstrained,
        }
    }
}

fn acquire_error_to_py(err: SchedulerAcquireError) -> PyErr {
    match err {
        SchedulerAcquireError::Closed => PyRuntimeError::new_err("route closed"),
        SchedulerAcquireError::Capacity { field, limit } => {
            PyRuntimeError::new_err(format!(
                "route concurrency capacity exceeded: {field}={limit}"
            ))
        }
    }
}

#[pymethods]
impl PyRouteConcurrency {
    fn snapshot(&self) -> PyRouteConcurrencySnapshot {
        PyRouteConcurrencySnapshot::from(self.inner.snapshot())
    }

    fn execution_guard(&self, py: Python<'_>, method_idx: u16) -> PyResult<PyRouteConcurrencyGuard> {
        let sched = self.inner.clone();
        let guard = py.detach(move || sched.blocking_acquire(method_idx))
            .map_err(acquire_error_to_py)?;
        Ok(PyRouteConcurrencyGuard { inner: Some(guard) })
    }

    fn close(&self) {
        self.inner.close();
    }

    fn shutdown(&self) {
        self.inner.close();
    }

    #[getter]
    fn is_unconstrained(&self) -> bool {
        self.inner.snapshot().is_unconstrained
    }
}

#[pymethods]
impl PyRouteConcurrencyGuard {
    fn __enter__<'py>(slf: PyRefMut<'py, Self>) -> PyResult<PyRefMut<'py, Self>> {
        if slf.inner.is_none() {
            return Err(PyRuntimeError::new_err("route concurrency guard already released"));
        }
        Ok(slf)
    }

    fn __exit__(
        mut slf: PyRefMut<'_, Self>,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc: Option<&Bound<'_, PyAny>>,
        _tb: Option<&Bound<'_, PyAny>>,
    ) -> bool {
        slf.inner.take();
        false
    }
}

impl Drop for PyRouteConcurrencyGuard {
    fn drop(&mut self) {
        self.inner.take();
    }
}
```

The Python adapter passes a method index, not `"read"` / `"write"`, into
`execution_guard()`. Rust already owns the method-index access map, so Python
must not duplicate read/write policy decisions beyond optional debug
projection.

- [ ] **Step 2: Return the native handle from `register_route()`**

Change `sdk/python/native/src/server_ffi.rs` so `register_route()` accepts the
full concurrency config and returns the new pyclass instead of `()`.

Use this signature shape:

```rust
use c2_server::scheduler::SchedulerLimits;

#[pyo3(signature = (
    name,
    dispatcher,
    method_names,
    access_map,
    concurrency_mode,
    max_pending=None,
    max_workers=None,
))]
fn register_route(
    &self,
    py: Python<'_>,
    name: &str,
    dispatcher: Py<PyAny>,
    method_names: Vec<String>,
    access_map: &Bound<'_, PyDict>,
    concurrency_mode: &str,
    max_pending: Option<usize>,
    max_workers: Option<usize>,
) -> PyResult<PyRouteConcurrency> {
    let mode = parse_concurrency_mode(concurrency_mode)?;
    let limits = SchedulerLimits::try_from_usize(max_pending, max_workers)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    let scheduler = Scheduler::with_limits(mode, map, limits);
    let route_handle = PyRouteConcurrency {
        inner: scheduler.clone(),
    };
    let callback = Arc::new(PyCrmCallback {
        py_callable: dispatcher,
        response_pool_obj,
    });
    let route = CrmRoute {
        name: name.to_string(),
        scheduler: Arc::new(scheduler),
        callback,
        method_names,
    };
    py.detach(move || register_route_on_runtime(server, route))?;
    Ok(route_handle)
}
```

`None` means no cap. `0` must fail registration in Rust even if Python already
validated it, because Rust is the final authority. Do not keep a fallback path
that accepts `max_pending` / `max_workers` in Python but omits them from this
call.

Add or update native bridge tests to assert the exact argument propagation:

```python
server.register_route(
    'grid',
    dispatcher,
    ['op'],
    {0: 'write'},
    'parallel',
    3,
    2,
)
assert fake_native.calls[-1].max_pending == 3
assert fake_native.calls[-1].max_workers == 2
```

Also add a registration-failure test where Rust rejects `max_workers=0` through
the FFI path, even if a direct Python object construction path is bypassed.

- [ ] **Step 3: Stop injecting a Python default `ConcurrencyConfig(max_workers=4)`**

In `sdk/python/src/c_two/transport/server/native.py`, remove the `max_workers=4`
constructor default and stop manufacturing
`_default_concurrency = ConcurrencyConfig(max_workers=max_workers)` when the
caller did not provide a config. When no explicit config is passed, pass
`ConcurrencyConfig()` only as the public input object and let Rust resolve /
validate the final policy, then project it back from the returned native
handle.

`register_crm()` must pass all three fields:

```python
native_concurrency = self._rust_server.register_route(
    routing_name,
    dispatcher,
    methods,
    access_map,
    cc_config.mode.value,
    cc_config.max_pending,
    cc_config.max_workers,
)
```

The returned `native_concurrency` is stored in the slot before the route can be
used for thread-local proxy creation. No Python field should retain an
independent pending count, worker count, executor, or closing flag.

- [ ] **Step 4: Store the native handle in the route slot**

Update `CRMSlot` so it stores the native route-concurrency handle (or a thin
Python `Scheduler` adapter around it) rather than owning separate scheduler
state. The slot should also store a method-name to method-index map for
thread-local guard acquisition:

```python
method_index: dict[str, int]
concurrency: RouteConcurrency
scheduler: Scheduler
```

The adapter may project a read-only snapshot into `CRMProxy.thread_local()`,
but the snapshot is never used as mutable state.

- [ ] **Step 5: Update the fake-native-server tests to the new signature**

In `sdk/python/tests/unit/test_name_collision.py`, update the fake `register_route()` signature to record the new handle / snapshot return value and verify the registration order still rolls back correctly when Rust registration fails.

- [ ] **Step 6: Run the focused Python tests**

Run:

```bash
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_name_collision.py sdk/python/tests/unit/test_ipc_config.py -q --timeout=30
```

Expected: the constructor no longer injects a Python-owned `max_workers` default, and the native handle is visible to the slot.

- [ ] **Step 7: Commit the PyO3 / bridge work**

```bash
git add sdk/python/native/src/route_concurrency_ffi.rs sdk/python/native/src/server_ffi.rs sdk/python/native/src/lib.rs sdk/python/src/c_two/transport/server/native.py sdk/python/tests/unit/test_name_collision.py sdk/python/tests/unit/test_ipc_config.py
git commit -m "fix: project route concurrency from rust"
```

### Task 3: Remove Python-owned scheduler state and keep the hot path fast

**Files:**
- Modify: `sdk/python/src/c_two/transport/server/scheduler.py`
- Modify: `sdk/python/src/c_two/transport/client/proxy.py`
- Modify: `sdk/python/tests/unit/test_scheduler.py`
- Modify: `sdk/python/tests/unit/test_security.py`
- Modify: `sdk/python/tests/unit/test_proxy_concurrency.py`

- [ ] **Step 1: Write the failing adapter tests first**

Add tests that assert the new contract:

```python
class RecordingResource:
    def __init__(self):
        self.calls = []

    def op(self):
        self.calls.append('op')
        return 'ok'


class Resource:
    def op(self):
        return 'ok'


def test_unconstrained_call_direct_skips_guard():
    crm = Resource()
    scheduler = FakeScheduler(
        snapshot=SimpleNamespace(
            mode=ConcurrencyMode.PARALLEL,
            max_pending=None,
            max_workers=None,
            closed=False,
            is_unconstrained=True,
        ),
        method_index={'op': 0},
    )
    proxy = CRMProxy.thread_local(crm, scheduler=scheduler)
    assert proxy.call_direct('op', ()) == 'ok'
    assert scheduler.guard_calls == []
```

Also add the constrained counterpart:

```python
def test_constrained_call_direct_enters_native_guard_by_method_index():
    crm = Resource()
    scheduler = FakeScheduler(
        snapshot=SimpleNamespace(
            mode=ConcurrencyMode.EXCLUSIVE,
            max_pending=None,
            max_workers=None,
            closed=False,
            is_unconstrained=False,
        ),
        method_index={'op': 7},
    )
    proxy = CRMProxy.thread_local(crm, scheduler=scheduler)
    assert proxy.call_direct('op', ()) == 'ok'
    assert scheduler.guard_calls == [7]
```

Use this fake shape so the tests enforce the new native-handle contract:

```python
class FakeScheduler:
    def __init__(self, *, snapshot, method_index):
        self._snapshot = snapshot
        self._method_index = method_index
        self.guard_calls = []

    @property
    def is_unconstrained(self):
        return self._snapshot.is_unconstrained

    def method_idx(self, method_name):
        return self._method_index[method_name]

    def execution_guard(self, method_idx):
        self.guard_calls.append(method_idx)
        return nullcontext()
```

Add the closed-route counterpart:

```python
def test_closed_call_direct_fails_before_entering_resource():
    resource = RecordingResource()
    scheduler = FakeScheduler(
        snapshot=SimpleNamespace(
            mode=ConcurrencyMode.PARALLEL,
            max_pending=None,
            max_workers=None,
            closed=True,
            is_unconstrained=False,
        ),
        method_index={'op': 0},
    )
    scheduler.execution_guard = Mock(side_effect=RuntimeError('route closed'))
    proxy = CRMProxy.thread_local(resource, scheduler=scheduler)

    with pytest.raises(RuntimeError, match='route closed'):
        proxy.call_direct('op', ())
    assert resource.calls == []
```

Remove older tests that pass `access_map` into `CRMProxy.thread_local()` or
assert Python-owned read/write lock behavior. Those tests are intentionally
obsolete because Rust owns the method-index access map.

- [ ] **Step 2: Rewrite `scheduler.py` into a thin adapter**

Remove Python-owned `ThreadPoolExecutor`, `_pending_count`, `begin()`, `execute()`, `execute_fast()`, and the pending-drain logic. Keep only the adapter surface needed by `CRMProxy.call_direct()` and tests:

- `method_idx(method_name)`
- `execution_guard(method_idx)`
- `snapshot()`
- `close()` / `shutdown()` forwarding
- `is_unconstrained` or equivalent cached fast-path projection

The adapter shape should be:

```python
class Scheduler:
    def __init__(self, native, method_index: dict[str, int]):
        self._native = native
        self._method_index = dict(method_index)

    def method_idx(self, method_name: str) -> int:
        return self._method_index[method_name]

    @property
    def is_unconstrained(self) -> bool:
        return bool(self._native.is_unconstrained)

    def snapshot(self):
        return self._native.snapshot()

    def execution_guard(self, method_idx: int):
        return self._native.execution_guard(method_idx)

    def close(self) -> None:
        self._native.close()

    shutdown = close
```

Keep `ConcurrencyMode` and `ConcurrencyConfig` in this module as public SDK
input types. Remove `MethodAccess`-driven locking from this class; Rust owns the
method-index access map.

If tests need to compare the native snapshot mode against `ConcurrencyMode`,
convert the native string at assertion time:

```python
assert ConcurrencyMode(snapshot.mode) is ConcurrencyMode.READ_PARALLEL
```

- [ ] **Step 3: Update `CRMProxy.call_direct()` to use the fast path**

If the slot snapshot reports an unconstrained route, call the Python method directly. Otherwise, enter the native guard before the method body. Do not route the thread-local path through serialized bytes just to share code with remote IPC.

The direct-call shape should be:

```python
if self._scheduler is None or self._scheduler.is_unconstrained:
    return method(*args)
method_idx = self._scheduler.method_idx(method_name)
with self._scheduler.execution_guard(method_idx):
    return method(*args)
```

Do not keep `access_map` as an input to `CRMProxy.thread_local()` for scheduling
authority. If tests still need method access for assertions, expose it as a
read-only debug projection from the slot, not as a scheduler input.

- [ ] **Step 4: Rewrite the legacy scheduler tests around the new adapter**

Replace `begin()` / `execute()` / `pending_count` assertions in `sdk/python/tests/unit/test_scheduler.py` and `sdk/python/tests/unit/test_security.py` with:

- snapshot shape checks
- `execution_guard()` concurrency checks
- close / closed-state checks
- the new no-op fast path for unconstrained routes

- [ ] **Step 5: Run the focused thread-local tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_scheduler.py sdk/python/tests/unit/test_security.py sdk/python/tests/unit/test_proxy_concurrency.py -q --timeout=30
```

Expected: the tests now describe the thin adapter and the direct-call hot path, not Python-owned scheduler state.

- [ ] **Step 6: Commit the Python scheduler cleanup**

```bash
git add sdk/python/src/c_two/transport/server/scheduler.py sdk/python/src/c_two/transport/client/proxy.py sdk/python/tests/unit/test_scheduler.py sdk/python/tests/unit/test_security.py sdk/python/tests/unit/test_proxy_concurrency.py
git commit -m "refactor: make python scheduler a thin adapter"
```

### Task 4: Wire mixed local/remote limits, lifecycle, and teardown ownership explicitly

**Files:**
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Modify: `sdk/python/tests/unit/test_name_collision.py`
- Modify: `sdk/python/tests/integration/test_concurrency_safety.py`
- Modify: `sdk/python/tests/integration/test_remote_scheduler_config.py`
- Modify: `core/transport/c2-server/src/server.rs`

- [ ] **Step 1: Add teardown-order tests before changing the implementation**

Update the existing unregister test to assert the new order. The Rust
unregister event must mean "closed handle and removed route under the same
dispatcher write lock", not merely "removed from table":

```python
assert events == [
    'rust_unregister:grid',
    'scheduler_close',
    'crm_shutdown',
]
```

Also add a test that a thread-local call after unregister sees the closed route and fails immediately instead of entering the resource method.

- [ ] **Step 2: Add mixed local/direct-IPC limit coverage**

In `sdk/python/tests/integration/test_remote_scheduler_config.py`, add a test
that proves one route handle is shared by thread-local and direct IPC calls:

```python
def test_thread_local_and_direct_ipc_share_max_workers_limit():
    impl = RemoteSchedulerProbeImpl()
    cc.register(
        RemoteSchedulerProbe,
        impl,
        name='mixed_limit',
        concurrency=ConcurrencyConfig(
            mode=ConcurrencyMode.PARALLEL,
            max_workers=1,
            max_pending=2,
        ),
    )
    cc.serve(blocking=False)

    local = cc.connect(RemoteSchedulerProbe, name='mixed_limit')
    remote = _direct_client(RemoteSchedulerProbe, 'mixed_limit')
    errors: list[BaseException] = []

    def local_worker():
        try:
            local.write_op(Delay(0.08))
        except BaseException as exc:
            errors.append(exc)

    def remote_worker():
        try:
            remote.write_op(Delay(0.08))
        except BaseException as exc:
            errors.append(exc)

    threads = [
        threading.Thread(target=local_worker, name='local-worker'),
        threading.Thread(target=remote_worker, name='remote-worker'),
    ]
    for thread in threads:
        thread.start()
    _join_all(threads)
    cc.close(local)
    cc.close(remote)

    assert not errors
    assert impl.max_seen == 1
```

Also add the fail-fast admission variant:

```python
def test_thread_local_and_direct_ipc_share_max_pending_rejection():
    impl = RemoteSchedulerProbeImpl()
    cc.register(
        RemoteSchedulerProbe,
        impl,
        name='mixed_pending_limit',
        concurrency=ConcurrencyConfig(
            mode=ConcurrencyMode.PARALLEL,
            max_pending=1,
        ),
    )
    cc.serve(blocking=False)

    local = cc.connect(RemoteSchedulerProbe, name='mixed_pending_limit')
    remote = _direct_client(RemoteSchedulerProbe, 'mixed_pending_limit')
    t1 = threading.Thread(target=lambda: local.write_op(Delay(0.12)))
    t1.start()
    time.sleep(0.03)
    with pytest.raises(Exception, match='capacity|pending|route concurrency'):
        remote.write_op(Delay(0.01))
    t1.join(timeout=5)
    assert not t1.is_alive()
    assert impl.max_seen == 1
    cc.close(local)
    cc.close(remote)
```

If route-capacity errors are serialized as `CCError` rather than plain runtime
exceptions by the time this test is implemented, keep the assertion on the
public error message/category but do not weaken the behavioral assertion:
the second call must not enter the resource and `impl.max_seen` must remain `1`.

- [ ] **Step 3: Implement atomic close / unregister sequencing**

In `NativeServerBridge.unregister_crm()`:

1. call Rust `unregister_route(name)`;
2. Rust must mark the shared handle closing and remove the route under the same
   dispatcher write lock before returning;
3. invoke the Python shutdown callback;
4. drop the Python slot and release the handle.

The Rust shape should be:

```rust
pub async fn unregister_route(&self, name: &str) -> Option<Arc<Scheduler>> {
    let mut dispatcher = self.dispatcher.write().await;
    let removed = dispatcher.unregister(name);
    if let Some(route) = removed.as_ref() {
        route.scheduler.close();
        return Some(Arc::clone(&route.scheduler));
    }
    None
}
```

Adjust `Dispatcher::unregister()` to return `Option<Arc<CrmRoute>>`, not
`bool`, so close and removal happen while the route is still available under
the write lock. The PyO3 `unregister_route()` may continue returning `bool` to
Python after it has performed the atomic Rust close+remove.

In `NativeServerBridge.shutdown()`, close all route handles first, then call the Rust server shutdown path, then drain the remaining Python-side slots idempotently.

- [ ] **Step 4: Keep in-flight work draining, but reject new work immediately**

The shared handle must allow existing guards to complete after closure, but any new guard acquisition after closure must raise a route-closed error. Do not add a second queue or a Python drain counter to simulate this.

- [ ] **Step 5: Verify direct IPC still bypasses relay**

Keep and extend the existing regression in
`sdk/python/tests/integration/test_remote_scheduler_config.py` so an explicit
direct IPC address still works when `C2_RELAY_ADDRESS` points at an invalid
host.

Use an explicit address:

```python
monkeypatch.setenv('C2_RELAY_ADDRESS', 'http://127.0.0.1:9')
client = cc.connect(RemoteSchedulerProbe, name='direct_without_relay', address=cc.server_address())
assert client.max_active().value == 0
```

- [ ] **Step 6: Run the lifecycle regression tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_name_collision.py sdk/python/tests/integration/test_concurrency_safety.py sdk/python/tests/integration/test_remote_scheduler_config.py -q --timeout=30
```

Expected: unregister/shutdown remain idempotent, local calls fail after close, and direct IPC remains relay-independent.

- [ ] **Step 7: Commit the lifecycle work**

```bash
git add sdk/python/src/c_two/transport/server/native.py sdk/python/tests/unit/test_name_collision.py sdk/python/tests/integration/test_concurrency_safety.py sdk/python/tests/integration/test_remote_scheduler_config.py core/transport/c2-server/src/server.rs
git commit -m "fix: make route concurrency teardown explicit"
```

### Task 5: Update boundary docs and run the full verification matrix

**Files:**
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify: `AGENTS.md` only if durable guidance changed

- [ ] **Step 1: Update the thin-sdk boundary note after the code lands**

Replace the stale route-concurrency note with the implementation result: Rust owns the shared route handle, Python only keeps the thin adapter, and no Python-owned pending/executor state remains.

- [ ] **Step 2: Add an explicit zero-copy regression if no existing test asserts buffer backing**

If no existing test already proves server-side large input arrives as a
SHM-backed `PyShmBuffer`, add this focused integration test to
`sdk/python/tests/integration/test_zero_copy_ipc.py` or
`sdk/python/tests/integration/test_remote_scheduler_config.py`. The least
invasive implementation is to monkeypatch `NativeServerBridge._make_dispatcher`
in the test and record `request_buf.is_inline` before the normal dispatcher
converts it to `memoryview`:

```python
@cc.crm(namespace='test.zero_copy.inspect', version='0.1.0')
class IEchoZC:
    def echo(self, data: bytes) -> bytes:
        raise NotImplementedError


class EchoZC:
    def echo(self, data: bytes) -> bytes:
        return data


def test_remote_ipc_large_input_reaches_resource_as_shm_memoryview(monkeypatch):
    from c_two.transport.server.native import NativeServerBridge

    seen: list[tuple[bool, int]] = []
    original_make_dispatcher = NativeServerBridge._make_dispatcher

    def recording_make_dispatcher(self, route_name, slot):
        dispatch = original_make_dispatcher(self, route_name, slot)

        def wrapped(route_name_arg, method_idx, request_buf, response_pool):
            seen.append((getattr(request_buf, 'is_inline', True), len(request_buf)))
            return dispatch(route_name_arg, method_idx, request_buf, response_pool)

        return wrapped

    monkeypatch.setattr(
        NativeServerBridge,
        '_make_dispatcher',
        recording_make_dispatcher,
    )

    cc.register(IEchoZC, EchoZC(), name='zero_copy_inspect')
    cc.serve(blocking=False)
    client = cc.connect(IEchoZC, name='zero_copy_inspect', address=cc.server_address())

    payload = b'Z' * (2 * 1024 * 1024)
    assert client.echo(payload) == payload

    assert seen[-1] == (False, 2 * 1024 * 1024)
    cc.close(client)
```

Do not replace this with a byte equality round-trip; equality does not prove the
zero-copy path.

If `len(request_buf)` is not exposed by the native `PyShmBuffer`, compute the
length with `len(memoryview(request_buf))` and release that temporary
memoryview before calling the original dispatcher.

- [ ] **Step 3: Run the focused native and Python tests**

Run:

```bash
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_scheduler.py \
  sdk/python/tests/unit/test_proxy_concurrency.py \
  sdk/python/tests/unit/test_security.py \
  sdk/python/tests/unit/test_name_collision.py \
  sdk/python/tests/integration/test_remote_scheduler_config.py \
  sdk/python/tests/integration/test_concurrency_safety.py \
  -q --timeout=30
cargo test --manifest-path core/Cargo.toml -p c2-server scheduler -q
```

Expected: all selected tests pass and the direct-call hot path still skips serialization.

The focused Python matrix must include:

- `test_thread_local_and_direct_ipc_share_max_workers_limit`
- `test_thread_local_and_direct_ipc_share_max_pending_rejection`
- the bad-relay explicit `ipc://` regression
- a large-input hold/view regression that proves the callback still receives a
  SHM-backed `memoryview` rather than copied bytes

- [ ] **Step 4: Run the full Python SDK suite if the focused matrix passes**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30
```

Expected: the suite passes without any test depending on Python-owned scheduler internals.

- [ ] **Step 5: Commit the docs update**

```bash
git add docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md AGENTS.md
git commit -m "docs: record rust-owned route concurrency boundary"
```

## Review Checklist

Before implementation starts, verify that this plan covers every current risk:

1. **Lifecycle ownership** — the plan names the close / unregister order and says who rejects new calls.
2. **Default resolution** — the plan removes the Python-side `max_workers=4` fallback and lets Rust resolve defaults.
3. **Legacy API removal** — the plan explicitly retires Python-owned `begin()` / `execute()` / `pending_count` / `executor` behavior.
4. **Fast path** — the plan preserves an unconstrained direct-call branch so thread-local calls do not pay guard overhead when no limits apply.
5. **Relay independence** — the plan keeps explicit `ipc://` direct-address regression coverage.
6. **Zero-copy guardrail** — the plan never changes the `RequestData -> PyShmBuffer -> memoryview` remote data path.
7. **Remote cleanup** — every scheduler acquisition failure after `RequestData`
   materialization releases SHM/chunk resources before replying with an error.
8. **Mixed local/remote limits** — one integration test proves thread-local and
   direct IPC calls consume the same route-level permits.

The desired end state is simple:

- Rust owns the policy and execution guard.
- Python only passes configuration in and projects immutable state out.
- Relay remains discovery and forwarding only.
- Same limits apply regardless of transport.
