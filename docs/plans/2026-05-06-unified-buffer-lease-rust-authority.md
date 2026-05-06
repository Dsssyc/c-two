# Unified Buffer Lease Rust Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move SDK-visible buffer lease lifecycle accounting into Rust `c2-mem` / native runtime so hold-mode and view-mode buffer exposure share one memory-lifecycle model, with client-side `cc.hold()` as the primary retained-lease path.

**Architecture:** `c2-mem` owns a lightweight `BufferLeaseTracker` that records SDK-visible buffer exposure metadata without reading payload bytes or freeing memory. Inline, SHM, reassembly handle, and file-spill buffers are storage tiers; hold and view are retention policies. Python keeps `cc.hold()`, `HeldResult`, `from_buffer()` orchestration, and warning presentation, but it no longer owns weakref-based hold accounting or Python-side sweep state.

**Tech Stack:** Rust `core/foundation/c2-mem`, PyO3 bindings in `sdk/python/native`, Python SDK transfer/client/server glue in `sdk/python/src/c_two`, pytest unit/integration tests, Cargo tests.

---

## 0.x Clean-Cut Constraint

C-Two is in the 0.x line. Remove Python-owned hold accounting rather than keeping compatibility shims. Do not keep `HoldRegistry`, Python `_entries`, weakref lease cleanup, or Python sweep authority after native retained-buffer accounting is wired. Public names such as `cc.hold()` and `cc.hold_stats()` may remain as SDK facades, but their state source must be native.


## Post-Implementation Review Status

Issue5 is implemented on `dev-feature`. A post-review repair fixed the only
observed semantic drift: deserialize-only client outputs wrapped with
`cc.hold()` now count as retained client-response leases. This is required
because the Python `HeldResult` still holds both the native response owner and a
`memoryview` until `HeldResult.release()`, even when the output object itself was
constructed by `deserialize()` rather than `from_buffer()`.

The current repair baseline is therefore:

- Track every successful client `cc.hold()` response that keeps a native owner
  alive, independent of output construction hook.
- Track only after output construction succeeds; failed `from_buffer()` paths
  release the memoryview and response owner immediately and must not leave stale
  retained stats.
- Preserve storage transparency: inline, SHM, handle, and file-spill retained
  responses use the same stats semantics without forcing SHM promotion.
- Treat remaining issue5 work as hardening of the SDK/native seam, not a new
  Python-owned lifecycle mechanism.

## Corrected Scope For Issue 5

The original issue name says “Hold-mode lease tracking”, but the correct abstraction is broader:

```text
Lease = SDK/user code is holding a runtime-owned buffer view.
Storage tier = inline | shm | handle | file_spill.
Retention policy = transient | retained.
```

`cc.hold()` is a retained client-response lease. `@cc.transfer(..., buffer="hold")` on resource input is also a retained lease, but it is not the primary user-facing API. View/default mode is a transient lease: it borrows the same kind of runtime-owned buffer and releases it at the end of deserialization. This implementation models both policies in Rust, while default hot paths only account retained leases to avoid adding a per-call mutex/FFI cost to normal view calls.

## Non-Negotiable Runtime Constraints

1. **No second memory owner.** The lease tracker must not free SHM, drop buffers, read payload bytes, or participate in allocation decisions. Existing `MemPool::free_at()`, `MemPool::release_handle()`, `ResponseBuffer.release()`, `ShmBuffer.release()`, and Drop paths remain the memory-release authority.
2. **Inline is a valid retained lease.** Inline retained buffers are not SHM, but they still need owner lifetime tracking because `from_buffer(memoryview(...))` may produce a zero-copy object backed by native `Vec<u8>`.
3. **Storage tier is transparent to SDK users.** `cc.hold(grid.subdivide_grids)(...)` must behave consistently whether the result travels inline or through SHM/chunk/reassembly.
4. **No forced SHM promotion for hold.** Small inline retained results must stay inline; do not move them into SHM just to fit the lease model.
5. **No default view-mode regression.** Default transient/view calls must keep their current release behavior and must not add payload copies or mandatory global accounting overhead.
6. **No relay coupling.** Explicit direct IPC buffer leases must work without relay and with a bad relay environment.
7. **Cross-language viable.** The Rust model must be expressible as pointer/length/storage/retention/release-token semantics for Rust, Go, and Fortran SDKs.
8. **No Python weakref authority.** Python may trigger retained tracking at semantic boundaries and may log native stale snapshots, but it must not own retained-entry dictionaries, ages, byte totals, or stale detection.

## Current Problem

Current Python hold accounting only tracks server-side input hold through `sdk/python/src/c_two/transport/server/hold_registry.py`:

```text
NativeServerBridge hold input branch
  -> memoryview(request_buf)
  -> resource method
  -> Python HoldRegistry.track(request_buf)
  -> weakref cleanup
  -> Python stats/sweep
```

This misses the primary client-facing retained path:

```python
with cc.hold(proxy.method)(...) as held:
    value = held.value
```

It also treats inline buffers as non-leases even though inline `ResponseBufferInner::Inline(Vec<u8>)` and `ShmBufferInner::Inline(Vec<u8>)` can be exposed through `memoryview()` and retained by `from_buffer()` outputs. For a grid-style method whose output size varies, this creates a dangerous split: large retained results are SHM-backed, while small retained results are inline-backed, but both need the same owner lifetime semantics.

## Target Data Flows

### Client Response, Retained, Inline

```text
Rust IPC client receives inline response
  -> PyResponseBuffer { storage=inline, owner=Vec<u8> }
  -> memoryview(response)
  -> output.from_buffer(memoryview) or output.deserialize(memoryview)
  -> output construction succeeds
  -> Python cc.hold() path calls response.track_retained(route, method, "client_response")
  -> HeldResult holds value + memoryview + ResponseBuffer owner
  -> HeldResult.release()
       -> memoryview.release()
       -> response.release()
       -> lease guard drops
       -> inline Vec owner is released
```

### Client Response, Retained, SHM / Handle

```text
Rust IPC client receives SHM or reassembled handle response
  -> PyResponseBuffer { storage=shm|handle|file_spill }
  -> memoryview(response)
  -> output.from_buffer(memoryview) or output.deserialize(memoryview)
  -> output construction succeeds
  -> Python cc.hold() path calls response.track_retained(route, method, "client_response")
  -> HeldResult holds value + memoryview + ResponseBuffer owner
  -> HeldResult.release()
       -> memoryview.release()
       -> response.release()
       -> lease guard drops
       -> existing free_at() / release_handle() path releases memory
```

### Resource Input, Retained, Inline Or SHM

```text
Rust server creates PyShmBuffer for request payload
  -> Python dispatch sees contract buffer_mode == "hold"
  -> request_buf.track_retained(route, method, "resource_input")
  -> memoryview(request_buf)
  -> input.from_buffer(memoryview)
  -> resource method may retain view-backed object
  -> request_buf.release() / Drop eventually releases memory and lease guard
```

### Default View / Transient

```text
Rust creates PyResponseBuffer or PyShmBuffer
  -> Python view/default path creates memoryview
  -> deserialize(memoryview)
  -> memoryview.release()
  -> buffer.release()
```

Client output `from_buffer()` failures release `memoryview(response)` and the
`ResponseBuffer` before raising `ClientOutputFromBuffer`; resource input
`from_buffer()` failures release `memoryview(request_buf)` and `request_buf`
before returning `ResourceInputFromBuffer` over the normal C2 error wire. If a
resource successfully receives and retains a hold-mode input view before later
raising, the retained lease remains tracked until the resource drops the view;
this avoids invalidating buffer-backed objects that resource code already owns.

The core lease model includes `LeaseRetention::Transient`, but Python does not add retained-style global accounting to every default view call. This keeps current hot-path performance while leaving a Rust-native transient model available for focused diagnostics and cross-SDK ABI parity.

## File Responsibility Map

- Create `core/foundation/c2-mem/src/lease.rs`: define SDK-visible buffer lease metadata, storage tiers, retention policies, directions, RAII guard, stats, and stale snapshot logic.
- Modify `core/foundation/c2-mem/src/lib.rs`: export lease types.
- Modify `sdk/python/native/src/lease_ffi.rs`: expose a native `BufferLeaseTracker` pyclass plus dict-producing `stats()` and `sweep_retained()` methods.
- Modify `sdk/python/native/src/lib.rs`: register the lease FFI module.
- Modify `sdk/python/native/src/client_ffi.rs`: add optional retained lease state to `PyResponseBuffer`, expose `track_retained(route_name, method_name, direction="client_response")`, clear the guard on successful release/drop, and report storage tier without copying data.
- Modify `sdk/python/native/src/shm_buffer.rs`: add optional retained lease state to `PyShmBuffer`, expose `track_retained(route_name, method_name, direction="resource_input")`, clear the guard on successful release/drop, and report storage tier without copying data.
- Modify `sdk/python/native/src/runtime_session_ffi.rs`: make `RuntimeSession` own one `Arc<BufferLeaseTracker>`, expose `lease_tracker()`, `hold_stats()`, and `sweep_hold_leases(threshold_seconds)`.
- Modify `sdk/python/src/c_two/transport/registry.py`: route public `hold_stats()` to native `RuntimeSession.hold_stats()` even when no server is running.
- Modify `sdk/python/src/c_two/transport/client/proxy.py`: store the native lease tracker and expose route/mode metadata used by transfer wrappers.
- Modify `sdk/python/src/c_two/crm/transferable.py`: in client-side hold path, create a memoryview, construct the output through `from_buffer()` or `deserialize()`, then track retained response leases only after construction succeeds and before returning `HeldResult`; keep owner and memoryview alive inside the release closure.
- Modify `sdk/python/src/c_two/transport/server/native.py`: accept/pass a native lease tracker, track retained resource-input leases in the hold branch, log warnings from native stale snapshots, and remove Python `HoldRegistry` usage.
- Delete `sdk/python/src/c_two/transport/server/hold_registry.py`: remove Python-owned lease accounting.
- Replace `sdk/python/tests/unit/test_hold_registry.py`: move coverage to native/core lease tests and Python retained-buffer integration tests.
- Modify `sdk/python/tests/unit/test_name_collision.py`: remove `_hold_registry` fake state setup.
- Add or modify `sdk/python/tests/unit/test_sdk_boundary.py`: guard against reintroducing Python `HoldRegistry`, weakref lease accounting, or Python-owned sweep state.
- Add `sdk/python/tests/unit/test_buffer_lease.py`: unit coverage for Python facade semantics over native tracker.
- Add `sdk/python/tests/integration/test_buffer_lease_ipc.py`: direct IPC integration coverage for inline retained, SHM retained, release stats, bad-relay independence, and grid-style variable-size behavior.
- Modify `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: after implementation, replace issue5 wording with unified SDK-visible buffer lease ownership.
- Modify `AGENTS.md`: after implementation, update the Hold Mode Pattern and Memory subsystem guidance to state that Rust owns buffer lease accounting and SDKs only choose retention policy.

---

## Task 0: Baseline Guard And Worktree Hygiene

**Files:**
- Read: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Read: `sdk/python/src/c_two/transport/server/hold_registry.py`
- Read: `sdk/python/src/c_two/crm/transferable.py`
- Read: `sdk/python/native/src/client_ffi.rs`
- Read: `sdk/python/native/src/shm_buffer.rs`
- Read: `core/foundation/c2-mem/src/pool.rs`

- [ ] **Step 1: Confirm the branch is clean before implementation**

Run:

```bash
git status --short --branch
```

Expected: branch is `dev-feature`; no uncommitted code changes. If docs from this plan are dirty, commit or intentionally carry them before editing source.

- [ ] **Step 2: Run baseline focused tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-mem
cargo check --manifest-path sdk/python/native/Cargo.toml -q
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_hold_registry.py \
  sdk/python/tests/unit/test_held_result.py \
  sdk/python/tests/integration/test_transfer_hold.py \
  -q --timeout=30 -rs
```

Expected: current tests pass before the migration begins. If `test_hold_registry.py` passes, that confirms the Python-owned behavior being removed.

- [ ] **Step 3: Record current stale-symbol targets**

Run:

```bash
rg -n "HoldRegistry|hold_registry|_hold_registry|weakref|_entries|sweep\(" \
  sdk/python/src/c_two/transport/server \
  sdk/python/tests/unit/test_hold_registry.py \
  sdk/python/tests/unit/test_name_collision.py
```

Expected: matches identify the Python-owned hold accounting paths that must be deleted or rewritten by subsequent tasks.

- [ ] **Step 4: Commit only if there were preparatory doc changes**

If no files changed, do not commit. If this implementation plan was added in the same working session, commit it separately:

```bash
git add docs/plans/2026-05-06-unified-buffer-lease-rust-authority.md
git commit -m "docs: plan unified buffer lease rust authority"
```

Expected: source implementation starts from a clean tree with the plan preserved.

---

## Task 1: Add Unified Buffer Lease Model To `c2-mem`

**Files:**
- Create: `core/foundation/c2-mem/src/lease.rs`
- Modify: `core/foundation/c2-mem/src/lib.rs`
- Test: inline unit tests in `core/foundation/c2-mem/src/lease.rs`

- [ ] **Step 1: Write failing Rust tests for retained inline and SHM lease accounting**

Create `core/foundation/c2-mem/src/lease.rs` with the tests first. The implementation below is intentionally absent at this step, so the test module should fail to compile until the types are added in Step 3.

```rust
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
        assert_eq!(stats.by_storage.get(&BufferStorage::Inline).unwrap().active_holds, 1);
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
```

- [ ] **Step 2: Run the Rust test and verify it fails**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-mem lease::tests::retained_inline_lease_counts_until_guard_drop
```

Expected: compile failure naming missing `BufferLeaseTracker`, `BufferLeaseMeta`, `BufferStorage`, `LeaseDirection`, or `LeaseRetention`.

- [ ] **Step 3: Implement the minimal lease model**

Replace `core/foundation/c2-mem/src/lease.rs` with this implementation and keep the tests from Step 1 below it:

```rust
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
        self.inner.entries.lock().unwrap().insert(id, entry);
        BufferLeaseGuard {
            id: Some(id),
            tracker: Arc::downgrade(&self.inner),
        }
    }

    pub fn stats(&self) -> BufferLeaseStats {
        let now = Instant::now();
        let entries = self.inner.entries.lock().unwrap();
        let mut stats = BufferLeaseStats::default();
        for entry in entries.values() {
            stats.active_leases += 1;
            stats.total_leased_bytes += entry.meta.bytes;
            let storage = stats.by_storage.entry(entry.meta.storage).or_default();
            storage.active_leases += 1;
            storage.total_leased_bytes += entry.meta.bytes;
            if entry.meta.retention == LeaseRetention::Retained {
                stats.active_holds += 1;
                stats.total_held_bytes += entry.meta.bytes;
                storage.active_holds += 1;
                storage.total_held_bytes += entry.meta.bytes;
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
        let entries = self.inner.entries.lock().unwrap();
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
        inner.entries.lock().unwrap().remove(&id);
    }
}
```

- [ ] **Step 4: Export the lease module**

Modify `core/foundation/c2-mem/src/lib.rs`:

```rust
pub mod lease;
pub use lease::{
    BufferLeaseGuard, BufferLeaseMeta, BufferLeaseSnapshot, BufferLeaseStats,
    BufferLeaseTracker, BufferStorage, LeaseDirection, LeaseRetention,
    StorageLeaseStats,
};
```

Place these exports near the existing `pub use` lines for `MemHandle`, `MemPool`, and `PoolStats`.

- [ ] **Step 5: Run c2-mem tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-mem
```

Expected: all `c2-mem` tests pass, including the new lease tests.

- [ ] **Step 6: Review for overdesign and memory authority violations**

Run:

```bash
rg -n "free_at\(|release_handle\(|handle_slice\(|data_ptr_at\(|copy_from|copy_to|from_raw_parts" core/foundation/c2-mem/src/lease.rs
```

Expected: no matches. The lease module must not free memory, read payloads, copy payloads, or expose raw pointers.

- [ ] **Step 7: Commit Task 1**

Run:

```bash
git add core/foundation/c2-mem/src/lib.rs core/foundation/c2-mem/src/lease.rs
git commit -m "feat(mem): add buffer lease accounting primitives"
```

Expected: commit succeeds with only `c2-mem` lease model changes.

---

## Task 2: Expose Native Lease Tracker Through PyO3

**Files:**
- Create: `sdk/python/native/src/lease_ffi.rs`
- Modify: `sdk/python/native/src/lib.rs`
- Test: `sdk/python/tests/unit/test_native_buffer_lease.py`

- [ ] **Step 1: Write failing Python tests for native lease FFI**

Create `sdk/python/tests/unit/test_native_buffer_lease.py`:

```python
import time

from c_two import _native


def test_native_buffer_lease_tracker_counts_inline_retained():
    tracker = _native.BufferLeaseTracker()
    lease = tracker.track_retained(
        route_name="grid",
        method_name="subdivide_grids",
        direction="client_response",
        storage="inline",
        bytes=128,
    )
    stats = tracker.stats()
    assert stats["active_holds"] == 1
    assert stats["total_held_bytes"] == 128
    assert stats["by_storage"]["inline"]["active_holds"] == 1
    lease.release()
    assert tracker.stats()["active_holds"] == 0


def test_native_buffer_lease_tracker_sweeps_retained_snapshots():
    tracker = _native.BufferLeaseTracker()
    lease = tracker.track_retained(
        route_name="grid",
        method_name="subdivide_grids",
        direction="client_response",
        storage="shm",
        bytes=8192,
    )
    time.sleep(0.01)
    stale = tracker.sweep_retained(0.001)
    assert len(stale) == 1
    assert stale[0]["route_name"] == "grid"
    assert stale[0]["method_name"] == "subdivide_grids"
    assert stale[0]["direction"] == "client_response"
    assert stale[0]["storage"] == "shm"
    assert stale[0]["bytes"] == 8192
    assert stale[0]["age_seconds"] > 0
    lease.release()


def test_native_buffer_lease_tracker_rejects_invalid_labels():
    tracker = _native.BufferLeaseTracker()
    for kwargs in [
        {"direction": "bad", "storage": "inline"},
        {"direction": "client_response", "storage": "bad"},
    ]:
        try:
            tracker.track_retained(
                route_name="grid",
                method_name="subdivide_grids",
                bytes=1,
                **kwargs,
            )
        except ValueError:
            pass
        else:
            raise AssertionError(f"expected ValueError for {kwargs}")
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_native_buffer_lease.py -q --timeout=30
```

Expected: failure because `_native.BufferLeaseTracker` is not exported.

- [ ] **Step 3: Implement `lease_ffi.rs`**

Create `sdk/python/native/src/lease_ffi.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use c2_mem::{
    BufferLeaseGuard, BufferLeaseMeta, BufferLeaseTracker, BufferStorage,
    LeaseDirection, LeaseRetention,
};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

#[pyclass(name = "BufferLeaseTracker", frozen)]
#[derive(Clone)]
pub struct PyBufferLeaseTracker {
    pub(crate) inner: Arc<BufferLeaseTracker>,
}

#[pyclass(name = "BufferLease", frozen)]
pub struct PyBufferLease {
    guard: parking_lot::Mutex<Option<BufferLeaseGuard>>,
}

impl PyBufferLeaseTracker {
    pub(crate) fn from_arc(inner: Arc<BufferLeaseTracker>) -> Self {
        Self { inner }
    }

    pub(crate) fn arc(&self) -> Arc<BufferLeaseTracker> {
        Arc::clone(&self.inner)
    }

    pub(crate) fn stats_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let stats = self.inner.stats();
        let dict = PyDict::new(py);
        dict.set_item("active_leases", stats.active_leases)?;
        dict.set_item("active_holds", stats.active_holds)?;
        dict.set_item("total_leased_bytes", stats.total_leased_bytes)?;
        dict.set_item("total_held_bytes", stats.total_held_bytes)?;
        dict.set_item("oldest_hold_seconds", stats.oldest_hold_seconds)?;
        let by_storage = PyDict::new(py);
        for storage in [
            BufferStorage::Inline,
            BufferStorage::Shm,
            BufferStorage::Handle,
            BufferStorage::FileSpill,
        ] {
            let value = stats.by_storage.get(&storage).cloned().unwrap_or_default();
            by_storage.set_item(storage.as_str(), storage_stats_dict(py, &value)?)?;
        }
        dict.set_item("by_storage", by_storage)?;
        Ok(dict)
    }

    pub(crate) fn sweep_retained_list<'py>(
        &self,
        py: Python<'py>,
        threshold_seconds: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        if !threshold_seconds.is_finite() || threshold_seconds < 0.0 {
            return Err(PyValueError::new_err(
                "threshold_seconds must be a non-negative finite number",
            ));
        }
        let threshold = Duration::from_secs_f64(threshold_seconds);
        let list = PyList::empty(py);
        for snapshot in self.inner.sweep_retained(threshold) {
            let dict = PyDict::new(py);
            dict.set_item("id", snapshot.id)?;
            dict.set_item("route_name", snapshot.route_name)?;
            dict.set_item("method_name", snapshot.method_name)?;
            dict.set_item("direction", snapshot.direction.as_str())?;
            dict.set_item("retention", snapshot.retention.as_str())?;
            dict.set_item("storage", snapshot.storage.as_str())?;
            dict.set_item("bytes", snapshot.bytes)?;
            dict.set_item("age_seconds", snapshot.age_seconds)?;
            list.append(dict)?;
        }
        Ok(list)
    }
}

impl PyBufferLease {
    pub(crate) fn take_guard(&self) -> Option<BufferLeaseGuard> {
        self.guard.lock().take()
    }
}

fn parse_storage(storage: &str) -> PyResult<BufferStorage> {
    match storage {
        "inline" => Ok(BufferStorage::Inline),
        "shm" => Ok(BufferStorage::Shm),
        "handle" => Ok(BufferStorage::Handle),
        "file_spill" => Ok(BufferStorage::FileSpill),
        other => Err(PyValueError::new_err(format!(
            "invalid buffer storage {other:?}"
        ))),
    }
}

fn parse_direction(direction: &str) -> PyResult<LeaseDirection> {
    match direction {
        "client_response" => Ok(LeaseDirection::ClientResponse),
        "resource_input" => Ok(LeaseDirection::ResourceInput),
        other => Err(PyValueError::new_err(format!(
            "invalid lease direction {other:?}"
        ))),
    }
}

fn storage_stats_dict<'py>(
    py: Python<'py>,
    stats: &c2_mem::StorageLeaseStats,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("active_leases", stats.active_leases)?;
    dict.set_item("active_holds", stats.active_holds)?;
    dict.set_item("total_leased_bytes", stats.total_leased_bytes)?;
    dict.set_item("total_held_bytes", stats.total_held_bytes)?;
    Ok(dict)
}

#[pymethods]
impl PyBufferLeaseTracker {
    #[new]
    #[pyo3(signature = (track_transient=false))]
    fn new(track_transient: bool) -> Self {
        Self {
            inner: Arc::new(BufferLeaseTracker::new(track_transient)),
        }
    }

    #[pyo3(signature = (route_name, method_name, direction, storage, bytes))]
    fn track_retained(
        &self,
        route_name: &str,
        method_name: &str,
        direction: &str,
        storage: &str,
        bytes: usize,
    ) -> PyResult<PyBufferLease> {
        let guard = self.inner.track(BufferLeaseMeta {
            route_name: route_name.to_string(),
            method_name: method_name.to_string(),
            direction: parse_direction(direction)?,
            retention: LeaseRetention::Retained,
            storage: parse_storage(storage)?,
            bytes,
        });
        Ok(PyBufferLease {
            guard: parking_lot::Mutex::new(Some(guard)),
        })
    }

    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        self.stats_dict(py)
    }

    fn sweep_retained<'py>(
        &self,
        py: Python<'py>,
        threshold_seconds: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        self.sweep_retained_list(py, threshold_seconds)
    }
}

#[pymethods]
impl PyBufferLease {
    fn release(&self) {
        self.guard.lock().take();
    }

    fn __enter__<'py>(slf: PyRefMut<'py, Self>) -> PyResult<PyRefMut<'py, Self>> {
        Ok(slf)
    }

    fn __exit__(
        &self,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc: Option<&Bound<'_, PyAny>>,
        _tb: Option<&Bound<'_, PyAny>>,
    ) -> bool {
        self.release();
        false
    }
}

impl Drop for PyBufferLease {
    fn drop(&mut self) {
        self.guard.lock().take();
    }
}

pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyBufferLeaseTracker>()?;
    parent.add_class::<PyBufferLease>()?;
    Ok(())
}
```

- [ ] **Step 4: Register the module in native lib**

Modify `sdk/python/native/src/lib.rs` to declare and register the module:

```rust
mod lease_ffi;
```

and inside the PyO3 module registration function add:

```rust
lease_ffi::register_module(m)?;
```

Use the same style as existing `error_ffi`, `ipc_control_ffi`, and `route_concurrency_ffi` registration lines.

- [ ] **Step 5: Run native build and focused test**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_native_buffer_lease.py -q --timeout=30
```

Expected: cargo check passes and the new Python native lease tests pass.

- [ ] **Step 6: Commit Task 2**

Run:

```bash
git add sdk/python/native/src/lease_ffi.rs sdk/python/native/src/lib.rs sdk/python/tests/unit/test_native_buffer_lease.py
git commit -m "feat(python-native): expose buffer lease tracker"
```

Expected: commit contains only native tracker projection and tests.

---

## Task 3: Attach Retained Lease Guards To ResponseBuffer And ShmBuffer

**Files:**
- Modify: `sdk/python/native/src/client_ffi.rs`
- Modify: `sdk/python/native/src/shm_buffer.rs`
- Test: `sdk/python/tests/unit/test_native_buffer_lease.py`

- [ ] **Step 1: Add failing tests for inline and SHM-capable buffer tracking methods**

Append to `sdk/python/tests/unit/test_native_buffer_lease.py`:

```python
def test_response_buffer_inline_retained_lease_counts_until_release():
    tracker = _native.BufferLeaseTracker()
    # RustClient is not needed for this unit test because MemPool.read() returns bytes,
    # so this test uses the tracker object directly through a retained lease.
    lease = tracker.track_retained(
        route_name="grid",
        method_name="subdivide_grids",
        direction="client_response",
        storage="inline",
        bytes=3,
    )
    assert tracker.stats()["by_storage"]["inline"]["active_holds"] == 1
    lease.release()
    assert tracker.stats()["by_storage"]["inline"]["active_holds"] == 0


def test_native_buffer_classes_expose_track_retained_methods():
    assert hasattr(_native.ResponseBuffer, "track_retained")
    assert hasattr(_native.ShmBuffer, "track_retained")
```

- [ ] **Step 2: Run test and verify class method check fails**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_native_buffer_lease.py::test_native_buffer_classes_expose_track_retained_methods -q --timeout=30
```

Expected: failure because `ResponseBuffer.track_retained` and `ShmBuffer.track_retained` do not exist.

- [ ] **Step 3: Add lease state and storage labels to `PyResponseBuffer`**

Modify `sdk/python/native/src/client_ffi.rs`:

1. Add imports:

```rust
use crate::lease_ffi::PyBufferLeaseTracker;
use c2_mem::BufferLeaseGuard;
```

2. Add fields to `PyResponseBuffer`:

```rust
lease: Mutex<Option<BufferLeaseGuard>>,
```

3. Initialize `lease` to `None` in `PyResponseBuffer::from_response_data()`.

4. Add helper method inside `impl PyResponseBuffer`:

```rust
fn storage_label_for_inner(inner: &ResponseBufferInner) -> &'static str {
    match inner {
        ResponseBufferInner::Inline(_) => "inline",
        ResponseBufferInner::Shm { .. } => "shm",
        ResponseBufferInner::Handle { handle, .. } => {
            if handle.is_file_spill() {
                "file_spill"
            } else {
                "handle"
            }
        }
    }
}
```

5. Add PyO3 method inside `#[pymethods] impl PyResponseBuffer`:

```rust
#[pyo3(signature = (tracker, route_name, method_name, direction="client_response"))]
fn track_retained(
    &self,
    tracker: &PyBufferLeaseTracker,
    route_name: &str,
    method_name: &str,
    direction: &str,
) -> PyResult<()> {
    let guard = self.inner.lock();
    let Some(inner) = guard.as_ref() else {
        return Ok(());
    };
    let storage = Self::storage_label_for_inner(inner);
    drop(guard);
    let lease = tracker.track_retained(
        route_name,
        method_name,
        direction,
        storage,
        self.data_len,
    )?;
    let mut lease_guard = self.lease.lock();
    lease_guard.take();
    *lease_guard = lease.guard.lock().take();
    Ok(())
}
```

Use the crate-visible `PyBufferLease::take_guard()` helper defined in `lease_ffi.rs`:

```rust
*lease_guard = lease.take_guard();
```

6. In `PyResponseBuffer.release()`, only clear the lease guard after the active-export checks pass and immediately before or after `guard.take()` frees the storage:

```rust
self.lease.lock().take();
```

7. In `Drop for PyResponseBuffer`, clear the lease guard before taking `inner`:

```rust
self.lease.lock().take();
```

- [ ] **Step 4: Add the same retained tracking to `PyShmBuffer`**

Modify `sdk/python/native/src/shm_buffer.rs`:

1. Add imports:

```rust
use crate::lease_ffi::PyBufferLeaseTracker;
use c2_mem::BufferLeaseGuard;
```

2. Add field to `PyShmBuffer`:

```rust
lease: Mutex<Option<BufferLeaseGuard>>,
```

3. Initialize `lease` to `None` in `from_inline()`, `from_peer_shm()`, and `from_handle()`.

4. Add helper method:

```rust
fn storage_label_for_inner(inner: &ShmBufferInner) -> &'static str {
    match inner {
        ShmBufferInner::Inline(_) => "inline",
        ShmBufferInner::PeerShm { .. } => "shm",
        ShmBufferInner::Handle { handle, .. } => {
            if handle.is_file_spill() {
                "file_spill"
            } else {
                "handle"
            }
        }
    }
}
```

5. Add PyO3 method:

```rust
#[pyo3(signature = (tracker, route_name, method_name, direction="resource_input"))]
fn track_retained(
    &self,
    tracker: &PyBufferLeaseTracker,
    route_name: &str,
    method_name: &str,
    direction: &str,
) -> PyResult<()> {
    let guard = self.inner.lock();
    let Some(inner) = guard.as_ref() else {
        return Ok(());
    };
    let storage = Self::storage_label_for_inner(inner);
    drop(guard);
    let lease = tracker.track_retained(
        route_name,
        method_name,
        direction,
        storage,
        self.data_len,
    )?;
    let mut lease_guard = self.lease.lock();
    lease_guard.take();
    *lease_guard = lease.take_guard();
    Ok(())
}
```

6. Clear `self.lease.lock().take()` on successful `release()` after export checks pass, and in Drop before freeing storage.

- [ ] **Step 5: Run native build and method tests**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_native_buffer_lease.py -q --timeout=30
```

Expected: native build passes and tests pass.

- [ ] **Step 6: Review active-export semantics**

Open `sdk/python/native/src/client_ffi.rs` and `sdk/python/native/src/shm_buffer.rs` and verify:

- `release()` returns `BufferError` without clearing the lease when `exports > 0`.
- `release()` clears the lease only when the buffer is actually releasable or already released.
- Drop clears the lease and then frees storage through existing memory release logic.
- No `bytes`, `Vec` clone, `copy_to_slice`, or payload read was added for tracking.

- [ ] **Step 7: Commit Task 3**

Run:

```bash
git add sdk/python/native/src/client_ffi.rs sdk/python/native/src/shm_buffer.rs sdk/python/native/src/lease_ffi.rs sdk/python/tests/unit/test_native_buffer_lease.py
git commit -m "feat(native): attach leases to exposed buffers"
```

Expected: commit contains buffer lease attachment only.

---

## Task 4: Make RuntimeSession Own The Process Lease Tracker

**Files:**
- Modify: `sdk/python/native/src/runtime_session_ffi.rs`
- Modify: `sdk/python/src/c_two/transport/registry.py`
- Test: `sdk/python/tests/unit/test_runtime_session.py`
- Test: `sdk/python/tests/unit/test_buffer_lease.py`

- [ ] **Step 1: Write failing tests for RuntimeSession hold stats with no server**

Create `sdk/python/tests/unit/test_buffer_lease.py`:

```python
from c_two import _native


def test_runtime_session_hold_stats_are_native_and_available_without_server():
    session = _native.RuntimeSession()
    stats = session.hold_stats()
    assert stats["active_holds"] == 0
    assert stats["total_held_bytes"] == 0
    assert stats["oldest_hold_seconds"] == 0
    assert set(stats["by_storage"]) == {"inline", "shm", "handle", "file_spill"}


def test_runtime_session_lease_tracker_is_shared_by_stats():
    session = _native.RuntimeSession()
    tracker = session.lease_tracker()
    lease = tracker.track_retained(
        route_name="grid",
        method_name="subdivide_grids",
        direction="client_response",
        storage="inline",
        bytes=64,
    )
    assert session.hold_stats()["active_holds"] == 1
    lease.release()
    assert session.hold_stats()["active_holds"] == 0
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_buffer_lease.py -q --timeout=30
```

Expected: failure because `RuntimeSession.lease_tracker()` and `RuntimeSession.hold_stats()` do not exist.

- [ ] **Step 3: Add tracker ownership to `PyRuntimeSession`**

Modify `sdk/python/native/src/runtime_session_ffi.rs`:

1. Add imports:

```rust
use crate::lease_ffi::PyBufferLeaseTracker;
use c2_mem::BufferLeaseTracker;
use std::sync::Arc;
```

2. Add field to `PyRuntimeSession`:

```rust
lease_tracker: Arc<BufferLeaseTracker>,
```

3. Initialize it in `new()`:

```rust
lease_tracker: Arc::new(BufferLeaseTracker::default()),
```

4. Add methods to `#[pymethods] impl PyRuntimeSession`:

```rust
fn lease_tracker(&self) -> PyBufferLeaseTracker {
    PyBufferLeaseTracker::from_arc(Arc::clone(&self.lease_tracker))
}

fn hold_stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
    PyBufferLeaseTracker::from_arc(Arc::clone(&self.lease_tracker)).stats_dict(py)
}

fn sweep_hold_leases<'py>(
    &self,
    py: Python<'py>,
    threshold_seconds: f64,
) -> PyResult<Bound<'py, PyList>> {
    PyBufferLeaseTracker::from_arc(Arc::clone(&self.lease_tracker))
        .sweep_retained_list(py, threshold_seconds)
}
```

Add `PyList` to the existing imports from `pyo3::types` if it is not already imported.

- [ ] **Step 4: Route public Python `hold_stats()` to RuntimeSession**

Modify `sdk/python/src/c_two/transport/registry.py` so the module-level function is independent of server existence:

```python
def hold_stats() -> dict:
    """Return retained runtime buffer tracking statistics."""
    inst = _ProcessRegistry.get()
    return dict(inst._runtime_session.hold_stats())  # noqa: SLF001
```

Do not read `server._hold_registry` or return a separate Python-created zero dict.

- [ ] **Step 5: Run tests**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_buffer_lease.py \
  sdk/python/tests/unit/test_runtime_session.py \
  -q --timeout=30 -rs
```

Expected: tests pass.

- [ ] **Step 6: Commit Task 4**

Run:

```bash
git add sdk/python/native/src/runtime_session_ffi.rs sdk/python/src/c_two/transport/registry.py sdk/python/tests/unit/test_buffer_lease.py sdk/python/tests/unit/test_runtime_session.py
git commit -m "feat(runtime): own buffer lease tracker in session"
```

Expected: commit contains session ownership and public stats facade.

---

## Task 5: Track Client-Side `cc.hold()` Response Leases

**Files:**
- Modify: `sdk/python/src/c_two/transport/client/proxy.py`
- Modify: `sdk/python/src/c_two/transport/registry.py`
- Modify: `sdk/python/src/c_two/crm/transferable.py`
- Test: `sdk/python/tests/integration/test_buffer_lease_ipc.py`
- Test: `sdk/python/tests/unit/test_held_result.py`

- [ ] **Step 1: Write failing direct IPC tests for inline and SHM retained responses**

Create `sdk/python/tests/integration/test_buffer_lease_ipc.py`:

```python
from __future__ import annotations

import gc
import os

import pytest

import c_two as cc
from c_two.config import settings
from c_two.transport.registry import _ProcessRegistry


@pytest.fixture(autouse=True)
def _cleanup():
    old_threshold = settings._shm_threshold  # noqa: SLF001
    cc.shutdown()
    _ProcessRegistry._instance = None
    yield
    settings._shm_threshold = old_threshold  # noqa: SLF001
    cc.shutdown()
    _ProcessRegistry._instance = None


@cc.transferable
class BytesView:
    data: memoryview | bytes

    def serialize(value: "BytesView") -> bytes:
        return bytes(value.data)

    def deserialize(data: memoryview) -> "BytesView":
        return BytesView(bytes(data))

    def from_buffer(data: memoryview) -> "BytesView":
        return BytesView(data)


@cc.crm(namespace="cc.test.buffer_lease", version="0.1.0")
class PayloadCRM:
    @cc.transfer(output=BytesView)
    def payload(self, size: int) -> BytesView:
        ...


class PayloadResource:
    def payload(self, size: int) -> BytesView:
        return BytesView(b"x" * size)


def _connect_payload():
    address = cc.server_address()
    assert address is not None
    return cc.connect(PayloadCRM, name="payload", address=address)


def test_client_hold_inline_response_counts_and_releases_without_copy():
    settings.shm_threshold = 1024
    cc.register(PayloadCRM, PayloadResource(), name="payload")
    cc.serve(blocking=False)
    client = _connect_payload()
    try:
        held = cc.hold(client.payload)(16)
        try:
            stats = cc.hold_stats()
            assert stats["active_holds"] == 1
            assert stats["total_held_bytes"] == 16
            assert stats["by_storage"]["inline"]["active_holds"] == 1
            assert isinstance(held.value.data, memoryview)
            assert bytes(held.value.data[:4]) == b"xxxx"
        finally:
            held.release()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(client)


def test_client_hold_shm_response_counts_and_releases_without_relay(monkeypatch):
    settings.shm_threshold = 1024
    monkeypatch.setenv("C2_RELAY_ADDRESS", "http://127.0.0.1:9")
    cc.register(PayloadCRM, PayloadResource(), name="payload")
    cc.serve(blocking=False)
    client = _connect_payload()
    try:
        held = cc.hold(client.payload)(8192)
        try:
            stats = cc.hold_stats()
            assert stats["active_holds"] == 1
            assert stats["total_held_bytes"] == 8192
            assert stats["by_storage"]["shm"]["active_holds"] + stats["by_storage"]["handle"]["active_holds"] >= 1
            assert isinstance(held.value.data, memoryview)
            assert bytes(held.value.data[:4]) == b"xxxx"
        finally:
            held.release()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(client)


def test_client_hold_drop_releases_retained_response():
    settings.shm_threshold = 1024
    cc.register(PayloadCRM, PayloadResource(), name="payload")
    cc.serve(blocking=False)
    client = _connect_payload()
    try:
        held = cc.hold(client.payload)(4096)
        assert cc.hold_stats()["active_holds"] == 1
        del held
        gc.collect()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(client)
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/integration/test_buffer_lease_ipc.py -q --timeout=30 -rs
```

Expected: failures because `CRMProxy` does not expose a lease tracker and client-side hold does not call `ResponseBuffer.track_retained()`.

- [ ] **Step 3: Store route and tracker metadata on `CRMProxy`**

Modify `sdk/python/src/c_two/transport/client/proxy.py`:

1. Extend `__slots__`:

```python
'_lease_tracker',
```

2. Add optional `lease_tracker` to factory methods:

```python
@classmethod
def thread_local(cls, crm_instance: object, *, scheduler=None, on_terminate=None, lease_tracker=None) -> CRMProxy:
    ...
    proxy._lease_tracker = lease_tracker
    return proxy

@classmethod
def ipc(cls, client: Any, name: str, *, on_terminate=None, lease_tracker=None) -> CRMProxy:
    ...
    proxy._lease_tracker = lease_tracker
    return proxy

@classmethod
def http(cls, http_client: Any, name: str, *, on_terminate=None, lease_tracker=None) -> CRMProxy:
    ...
    proxy._lease_tracker = lease_tracker
    return proxy
```

3. Add properties:

```python
@property
def route_name(self) -> str:
    return self._name

@property
def lease_tracker(self):
    return self._lease_tracker
```

- [ ] **Step 4: Pass RuntimeSession tracker when creating proxies**

Modify `sdk/python/src/c_two/transport/registry.py` in `connect()`:

```python
lease_tracker = self._runtime_session.lease_tracker()
```

Pass it into each `CRMProxy.thread_local`, `CRMProxy.ipc`, and `CRMProxy.http` call:

```python
proxy = CRMProxy.ipc(
    client,
    name,
    on_terminate=lambda addr=address: self._runtime_session.release_ipc_client(addr),
    lease_tracker=lease_tracker,
)
```

Use the same keyword for thread-local and HTTP proxy creation. HTTP retained byte-buffer accounting remains no-op until HTTP returns native buffers; passing the tracker keeps the proxy shape consistent.

- [ ] **Step 5: Track retained client responses in `transferable.py`**

Modify `sdk/python/src/c_two/crm/transferable.py` in `com_to_crm()` inside the `if hasattr(response, 'release'):` hold branch so retained tracking happens only after output construction succeeds and before returning `HeldResult`:

```python
mv = memoryview(response)
if _c2_buffer == 'hold':
    try:
        result = output_fn(mv)
        if hasattr(response, 'track_retained'):
            tracker = getattr(client, 'lease_tracker', None)
            if tracker is not None:
                route_name = getattr(client, 'route_name', '')
                response.track_retained(
                    tracker,
                    route_name,
                    method_name,
                    'client_response',
                )
    except Exception:
        mv.release()
        try:
            response.release()
        except Exception:
            pass
        raise

    def release_cb():
        mv.release()
        try:
            response.release()
        except Exception:
            pass
    return HeldResult(result, release_cb)
```

This ordering is deliberate: failed `from_buffer()` paths must not leave stale retained stats, while deserialize-only successful hold outputs still count because `HeldResult` retains the native owner. The closure is required for inline retained zero-copy because `response` owns the native `Vec<u8>` backing the memoryview.

- [ ] **Step 6: Run client retained lease tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/integration/test_buffer_lease_ipc.py \
  sdk/python/tests/unit/test_held_result.py \
  -q --timeout=30 -rs
```

Expected: inline retained and SHM/handle retained tests pass. `HeldResult` behavior remains unchanged for release, context manager, and `__del__` fallback.

- [ ] **Step 7: Review for payload copy regressions**

Run:

```bash
rg -n "tobytes\(|bytes\(response\)|bytes\(mv\)|bytes\(data\)" sdk/python/src/c_two/crm/transferable.py sdk/python/src/c_two/transport/client/proxy.py
```

Expected: no new conversion in the hold path. Existing test-only or non-hold conversions must not be introduced by this task.

- [ ] **Step 8: Commit Task 5**

Run:

```bash
git add sdk/python/src/c_two/transport/client/proxy.py sdk/python/src/c_two/transport/registry.py sdk/python/src/c_two/crm/transferable.py sdk/python/tests/integration/test_buffer_lease_ipc.py sdk/python/tests/unit/test_held_result.py
git commit -m "feat(python): track client retained buffer leases"
```

Expected: commit covers the primary `cc.hold()` retained-response path.

---

## Task 6: Replace Server-Side HoldRegistry With Native Retained Leases

**Files:**
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Delete: `sdk/python/src/c_two/transport/server/hold_registry.py`
- Delete/replace: `sdk/python/tests/unit/test_hold_registry.py`
- Modify: `sdk/python/tests/unit/test_name_collision.py`
- Test: `sdk/python/tests/integration/test_buffer_lease_ipc.py`

- [ ] **Step 1: Add failing resource-input retained lease test**

Append to `sdk/python/tests/integration/test_buffer_lease_ipc.py`:

```python
@cc.crm(namespace="cc.test.buffer_lease_input", version="0.1.0")
class InputHoldCRM:
    @cc.transfer(input=BytesView, output=BytesView, buffer="hold")
    def echo(self, data: BytesView) -> BytesView:
        ...


class InputHoldResource:
    retained = []

    def echo(self, data: BytesView) -> BytesView:
        self.retained.append(data.data)
        return BytesView(data.data[:4])


def test_resource_input_hold_counts_inline_or_shm_until_request_buffer_release():
    settings.shm_threshold = 1024
    resource = InputHoldResource()
    cc.register(InputHoldCRM, resource, name="input_hold")
    cc.serve(blocking=False)
    address = cc.server_address()
    assert address is not None
    client = cc.connect(InputHoldCRM, name="input_hold", address=address)
    try:
        result = client.echo(BytesView(b"x" * 8192))
        assert bytes(result.data) == b"xxxx"
        stats = cc.hold_stats()
        assert stats["by_direction"]["resource_input"]["active_holds"] >= 1
        resource.retained.clear()
        gc.collect()
    finally:
        cc.close(client)
    # The retained input view was cleared; native Drop/release should remove the lease.
    assert cc.hold_stats()["by_direction"]["resource_input"]["active_holds"] == 0
```

This test expects `hold_stats()` to include `by_direction`. Task 6 adds that field to native stats if Task 2 did not already include it.

- [ ] **Step 2: Run test and verify failure**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/integration/test_buffer_lease_ipc.py::test_resource_input_hold_counts_inline_or_shm_until_request_buffer_release -q --timeout=30 -rs
```

Expected: failure because server-side hold still uses Python `HoldRegistry` and native stats do not include resource-input retained leases.

- [ ] **Step 3: Add `by_direction` to native stats output**

Modify `core/foundation/c2-mem/src/lease.rs` to add direction stats:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectionLeaseStats {
    pub active_leases: usize,
    pub active_holds: usize,
    pub total_leased_bytes: usize,
    pub total_held_bytes: usize,
}
```

Add to `BufferLeaseStats`:

```rust
pub by_direction: BTreeMap<LeaseDirection, DirectionLeaseStats>,
```

Update `stats()` to increment direction counters exactly as storage counters are incremented. Then modify `sdk/python/native/src/lease_ffi.rs` to emit:

```python
"by_direction": {
    "client_response": {...},
    "resource_input": {...},
}
```

Use the same field names as storage stats: `active_leases`, `active_holds`, `total_leased_bytes`, `total_held_bytes`.

- [ ] **Step 4: Pass the native tracker into `NativeServerBridge`**

Modify `sdk/python/native/src/runtime_session_ffi.rs` in `ensure_server_bridge()`:

```rust
kwargs.set_item("lease_tracker", self.lease_tracker())?;
```

Modify `sdk/python/src/c_two/transport/server/native.py` constructor signature:

```python
def __init__(..., hold_warn_seconds: float = 60.0, lease_tracker: object | None = None) -> None:
```

Store:

```python
self._lease_tracker = lease_tracker
self._hold_warn_seconds = float(hold_warn_seconds)
self._hold_sweep_interval = 10
```

Remove:

```python
from .hold_registry import HoldRegistry
self._hold_registry = HoldRegistry(warn_threshold=hold_warn_seconds)
```

- [ ] **Step 5: Track resource-input retained buffers through native tracker**

Modify the hold branch in `NativeServerBridge._make_dispatcher()`:

```python
else:  # hold
    if self._lease_tracker is not None and hasattr(request_buf, 'track_retained'):
        request_buf.track_retained(
            self._lease_tracker,
            route_name,
            method_name,
            'resource_input',
        )
    mv = memoryview(request_buf)
    result = method(mv)

    nonlocal hold_dispatch_count
    hold_dispatch_count += 1
    if self._lease_tracker is not None and hold_dispatch_count % hold_sweep_interval == 0:
        for stale in self._lease_tracker.sweep_retained(self._hold_warn_seconds):
            if stale.get('direction') != 'resource_input':
                continue
            logger.warning(
                "Retained resource input buffer pinned for %.1fs — "
                "route=%s method=%s storage=%s size=%d bytes. "
                "Resource may be storing buffer-backed views.",
                stale['age_seconds'], stale['route_name'], stale['method_name'],
                stale['storage'], stale['bytes'],
            )
```

Do not call `hold_registry.track()` or `hold_registry.sweep()`.

- [ ] **Step 6: Delete Python HoldRegistry and rewrite stale tests**

Delete:

```bash
git rm sdk/python/src/c_two/transport/server/hold_registry.py
git rm sdk/python/tests/unit/test_hold_registry.py
```

Modify `sdk/python/tests/unit/test_name_collision.py` to remove fake setup lines that assign `_hold_registry` or `_hold_sweep_interval` solely for the deleted registry. Keep `_hold_sweep_interval` only if the test still constructs a bridge object without calling the constructor and the dispatcher reads it.

- [ ] **Step 7: Add boundary guard against Python-owned hold accounting**

Modify `sdk/python/tests/unit/test_sdk_boundary.py`:

```python
def test_python_does_not_own_buffer_lease_accounting():
    from pathlib import Path

    root = Path(__file__).resolve().parents[2] / "src" / "c_two"
    offenders = []
    forbidden = [
        "class HoldRegistry",
        "weakref.ref(request_buf",
        "_hold_registry",
        "_entries",
        "total_held_bytes +=",
    ]
    for path in root.rglob("*.py"):
        text = path.read_text(encoding="utf-8")
        for needle in forbidden:
            if needle in text:
                offenders.append(f"{path.relative_to(root)}:{needle}")
    assert offenders == []
```

- [ ] **Step 8: Run server retained lease tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-mem
cargo check --manifest-path sdk/python/native/Cargo.toml -q
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/integration/test_buffer_lease_ipc.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/unit/test_name_collision.py \
  -q --timeout=30 -rs
```

Expected: tests pass and Python-owned hold accounting is absent.

- [ ] **Step 9: Commit Task 6**

Run:

```bash
git add core/foundation/c2-mem/src/lease.rs sdk/python/native/src/lease_ffi.rs sdk/python/native/src/runtime_session_ffi.rs sdk/python/src/c_two/transport/server/native.py sdk/python/tests/integration/test_buffer_lease_ipc.py sdk/python/tests/unit/test_sdk_boundary.py sdk/python/tests/unit/test_name_collision.py
git add -u sdk/python/src/c_two/transport/server/hold_registry.py sdk/python/tests/unit/test_hold_registry.py
git commit -m "refactor(python): replace hold registry with native buffer leases"
```

Expected: commit deletes Python `HoldRegistry` and routes resource-input retained accounting through native lease stats.

---

## Task 7: Add Grid-Style Variable-Size Retained Buffer Regression

**Files:**
- Modify: `sdk/python/tests/integration/test_buffer_lease_ipc.py`
- Optional modify: `examples/python/grid/transferables.py` if example dependencies are available in the implementation environment

- [ ] **Step 1: Add a grid-style variable-size regression using local test transferables**

Append to `sdk/python/tests/integration/test_buffer_lease_ipc.py`:

```python
@cc.crm(namespace="cc.test.grid_like_buffer_lease", version="0.1.0")
class GridLikeCRM:
    @cc.transfer(output=BytesView)
    def subdivide_grids(self, count: int) -> BytesView:
        ...


class GridLikeResource:
    def subdivide_grids(self, count: int) -> BytesView:
        # Simulates variable-size serialized Arrow/grid output without requiring pyarrow.
        return BytesView(("grid:" + ",".join(str(i) for i in range(count))).encode())


def test_grid_like_hold_is_storage_transparent_for_inline_and_shm():
    settings.shm_threshold = 256
    cc.register(GridLikeCRM, GridLikeResource(), name="grid_like")
    cc.serve(blocking=False)
    address = cc.server_address()
    assert address is not None
    grid = cc.connect(GridLikeCRM, name="grid_like", address=address)
    try:
        small = cc.hold(grid.subdivide_grids)(3)
        try:
            assert bytes(small.value.data).startswith(b"grid:0,1,2")
            small_stats = cc.hold_stats()
            assert small_stats["by_storage"]["inline"]["active_holds"] == 1
        finally:
            small.release()

        large = cc.hold(grid.subdivide_grids)(300)
        try:
            assert bytes(large.value.data[:6]) == b"grid:0"
            large_stats = cc.hold_stats()
            assert large_stats["active_holds"] == 1
            assert (
                large_stats["by_storage"]["shm"]["active_holds"]
                + large_stats["by_storage"]["handle"]["active_holds"]
                + large_stats["by_storage"]["file_spill"]["active_holds"]
            ) >= 1
        finally:
            large.release()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(grid)
```

This test covers the examples/grid shape without adding optional `pyarrow` to the default test path.

- [ ] **Step 2: Run the grid-style regression**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/integration/test_buffer_lease_ipc.py::test_grid_like_hold_is_storage_transparent_for_inline_and_shm -q --timeout=30 -rs
```

Expected: test passes, proving small inline and large SHM/handle retained results share one public `cc.hold()` lifecycle.

- [ ] **Step 3: If example dependencies are available, add `from_buffer` to grid transferables**

If `uv sync --group examples` has been run and `pyarrow` is available, update `examples/python/grid/transferables.py` so Arrow-backed deserialization can accept memoryviews without forcing `bytes()`:

```python
def deserialize_to_table(serialized_data: bytes | memoryview) -> pa.Table:
    buffer = pa.py_buffer(serialized_data)
    reader = pa.ipc.open_stream(buffer)
    return reader.read_all()
```

For each transferable where hold should be zero-copy, add:

```python
def from_buffer(arrow_bytes: memoryview):
    return GridAttribute.deserialize(arrow_bytes)
```

or for table/list-returning helpers, call the same `deserialize_to_table(memoryview)` helper. Keep `serialize()` unchanged.

- [ ] **Step 4: Run examples syntax tests and optional examples tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_python_examples_syntax.py -q --timeout=30 -rs
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/integration/test_python_examples.py -q --timeout=30 -rs
```

Expected: syntax tests pass. Integration examples pass if optional dependencies and local relay binary are available, or skip with explicit dependency/relay skip reasons.

- [ ] **Step 5: Commit Task 7**

Run:

```bash
git add sdk/python/tests/integration/test_buffer_lease_ipc.py examples/python/grid/transferables.py
git commit -m "test: cover storage-transparent retained buffers"
```

If `examples/python/grid/transferables.py` was not changed because optional dependencies were unavailable, omit it from `git add`.

Expected: commit captures the grid-style regression and any safe example transferable improvements.

---

## Task 8: Update Boundary Documentation And Agent Guidance

**Files:**
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify: `AGENTS.md`
- Test: `tests/repo`

- [ ] **Step 1: Update issue5 wording in thin-sdk boundary doc**

Replace the `### P2. Hold-mode lease tracking` section in `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md` with:

```markdown
### P2. SDK-visible buffer lease lifecycle

**Implemented ownership**

Status: implemented on `dev-feature` by
`docs/plans/2026-05-06-unified-buffer-lease-rust-authority.md`.

Rust `core/foundation/c2-mem` owns SDK-visible buffer lease accounting for
runtime-owned buffers exposed to language SDKs. Inline, SHM, handle, and
file-spill buffers are storage tiers; transient view mode and retained hold
mode are retention policies. Python keeps `cc.hold()`, `HeldResult`,
`from_buffer()` orchestration, and warning presentation, but retained entry
state, byte totals, ages, storage/direction aggregation, and stale snapshots
come from native accounting.

Client-side `cc.hold()` is the primary retained lease path. Resource-side
`@cc.transfer(..., buffer="hold")` input retention uses the same native tracker
as a diagnostic path. Inline retained buffers are counted because they can back
zero-copy `memoryview` outputs through native-owned `Vec<u8>`; they are reported
separately from SHM-backed retained buffers so memory pressure diagnostics stay
accurate.

**Verified coverage**

- Inline client-response `cc.hold()` counts as a retained lease and releases on
  `HeldResult.release()` / context manager / GC fallback.
- SHM or handle client-response `cc.hold()` uses the same API and releases
  through the existing `ResponseBuffer.release()` memory path.
- Resource-side input hold uses the same native retained tracker without
  Python weakref accounting.
- `hold_stats()` works without a Python server object and reports storage and
  direction breakdowns.
- Direct IPC retained buffers remain relay-independent.
- Default view mode still releases immediately and does not materialize payloads
  or add retained accounting overhead.
```

- [ ] **Step 2: Update AGENTS.md hold/memory guidance**

In `AGENTS.md`, update the Hold Mode Pattern section to add:

```markdown
Retained buffer accounting is Rust-owned. `cc.hold()` and `HeldResult` are
Python SDK facades over native SDK-visible buffer leases. Inline, SHM, handle,
and file-spill buffers can all be retained leases; do not special-case hold as
SHM-only and do not reintroduce Python weakref registries for held buffers.
```

In the Rust Native Layer or Memory subsystem section, add:

```markdown
`c2-mem` owns SDK-visible buffer lease accounting. Lease tracking records
metadata and retention state only; it must not read payload bytes, allocate a
second buffer, or replace `MemPool::free_at()` / `release_handle()` as the
memory release authority.
```

- [ ] **Step 3: Run docs/repo tests**

Run:

```bash
uv run pytest tests/repo -q --timeout=30
git diff --check
```

Expected: repo tests pass and whitespace check is clean.

- [ ] **Step 4: Commit Task 8**

Run:

```bash
git add docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md AGENTS.md
git commit -m "docs: document rust-owned buffer lease lifecycle"
```

Expected: docs reflect the implemented issue5 boundary and no longer describe Python `HoldRegistry` as the target state.

---

## Task 9: Full Verification And Stale-Code Audit

**Files:**
- Verify all files touched by Tasks 1-8.

- [ ] **Step 1: Run Rust workspace tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml --workspace
```

Expected: all Rust tests pass.

- [ ] **Step 2: Run native build**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
uv sync --reinstall-package c-two
```

Expected: native crate checks and Python package rebuild succeeds.

- [ ] **Step 3: Run focused Python tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_native_buffer_lease.py \
  sdk/python/tests/unit/test_buffer_lease.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/unit/test_runtime_session.py \
  sdk/python/tests/unit/test_held_result.py \
  sdk/python/tests/integration/test_buffer_lease_ipc.py \
  sdk/python/tests/integration/test_transfer_hold.py \
  sdk/python/tests/integration/test_remote_scheduler_config.py \
  -q --timeout=30 -rs
```

Expected: focused tests pass. Skips are acceptable only for pre-existing optional example dependency checks, not for buffer lease tests.

- [ ] **Step 4: Run full Python tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: full Python suite passes. Existing optional example skips may remain if examples dependencies are not installed.

- [ ] **Step 5: Run repo tests and diff check**

Run:

```bash
uv run pytest tests/repo -q --timeout=30
git diff --check
```

Expected: repo tests pass and whitespace check is clean.

- [ ] **Step 6: Audit for stale Python-owned hold accounting**

Run:

```bash
rg -n "HoldRegistry|hold_registry|_hold_registry|weakref\.ref\(|_entries|total_held_bytes \+=|oldest_hold_seconds.*time\.monotonic|sweep\(" \
  sdk/python/src/c_two sdk/python/tests -S
```

Expected: no matches for Python-owned retained lease accounting. Matches inside docs describing removed behavior are acceptable only in historical implementation plans, not active source or tests.

- [ ] **Step 7: Audit for payload-copy regressions in retained path**

Run:

```bash
rg -n "track_retained|_c2_buffer == 'hold'|memoryview\(|tobytes\(|bytes\(response\)|bytes\(mv\)|bytes\(data\)" \
  sdk/python/src/c_two/crm/transferable.py \
  sdk/python/native/src/client_ffi.rs \
  sdk/python/native/src/shm_buffer.rs -S
```

Expected: retained tracking appears after successful hold output construction and before returning `HeldResult`; no new `tobytes()` or `bytes(response)` conversion is used to support lease accounting. Existing `bytes(...)` in tests, user-defined deserialize hooks, or explicit value assertions are not part of the runtime retained-accounting path.

- [ ] **Step 8: Audit direct IPC relay independence**

Run:

```bash
C2_RELAY_ADDRESS=http://127.0.0.1:9 uv run pytest \
  sdk/python/tests/integration/test_buffer_lease_ipc.py::test_client_hold_shm_response_counts_and_releases_without_relay \
  sdk/python/tests/integration/test_direct_ipc_control.py \
  -q --timeout=30 -rs
```

Expected: tests pass with bad relay env, confirming direct IPC retained leases do not depend on relay.

- [ ] **Step 9: Final commit if verification required small fixes**

If verification required changes, commit them:

```bash
git add <changed-files>
git commit -m "fix: stabilize buffer lease lifecycle verification"
```

Expected: final tree is clean after the commit.

- [ ] **Step 10: Final status check**

Run:

```bash
git status --short --branch
```

Expected: branch is clean and ahead of upstream by the implementation commits.

---

## Design Review Checklist For Implementers

Before marking this issue complete, verify each statement with code or test evidence:

- [ ] `c2-mem` has one lease model with storage and retention dimensions; there is no `HoldTracker`-only Rust type.
- [ ] Lease tracking records metadata only and does not read or copy payload bytes.
- [ ] Inline retained buffers are counted and released through the same public `cc.hold()` lifecycle as SHM retained buffers.
- [ ] `ResponseBuffer.release()` and `ShmBuffer.release()` remain the memory release authority for retained buffers.
- [ ] Active memoryview exports prevent release and therefore must not clear the lease guard prematurely.
- [ ] `cc.hold_stats()` reads native `RuntimeSession` stats and works when only a client exists.
- [ ] Python `HoldRegistry` and Python weakref retained accounting are deleted.
- [ ] Resource-side input hold is tracked as `direction="resource_input"` but does not make `c2-server` the semantic owner of hold.
- [ ] Default view mode does not add a mandatory retained-accounting overhead or materialize bytes.
- [ ] Bad relay environment does not affect explicit direct IPC retained buffers.
- [ ] The design can be projected to Go/Rust/Fortran as pointer + length + storage + retention + release token.
