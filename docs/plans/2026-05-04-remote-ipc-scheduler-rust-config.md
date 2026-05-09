# Remote IPC Scheduler Rust Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make remote IPC honor per-CRM `ConcurrencyConfig` through the Rust `c2-server` scheduler while keeping Python as a thin typed facade and preserving the current zero-copy SHM request path.

**Architecture:** Python keeps CRM discovery, route metadata, Python resource invocation, and thread-local direct-call guards. Rust owns remote IPC scheduler configuration and execution semantics by constructing each `c2_server::scheduler::Scheduler` from Python-provided typed config at route registration time. The data path remains `RequestData -> PyShmBuffer -> memoryview(request_buf)`; moving scheduler config into Rust must not materialize SHM or handle payloads into Python `bytes`.

**Tech Stack:** Rust `core/transport/c2-server`, PyO3 bindings in `sdk/python/native`, Python SDK transport bridge in `sdk/python/src/c_two/transport`, pytest integration tests, Cargo unit tests.

---

## 0.x Constraint

C-Two is in the 0.x line. Do not preserve compatibility for incorrect, experimental, or superseded internal scheduler APIs unless the user explicitly asks for a compatibility window. Once remote IPC no longer depends on Python scheduler state, remove obsolete remote-path behavior in the same change instead of leaving fallback branches, duplicate locks, or zombie tests.

## Non-Negotiable Runtime Constraints

1. **No additional bytes copy.** Remote scheduler migration changes only concurrency configuration and lock/permit ownership. It must not change request payload conversion in `sdk/python/native/src/server_ffi.rs::PyCrmCallback::invoke()` or the Python dispatcher’s `memoryview(request_buf)` path in `sdk/python/src/c_two/transport/server/native.py::_make_dispatcher()`.
2. **Thread-local remains separate.** Same-process `cc.connect()` passes Python objects directly through `CRMProxy.call_direct()` and must not be routed through Rust serialized dispatch for symmetry.
3. **Direct IPC remains relay-independent.** Explicit `cc.connect(..., address="ipc://...")` must continue to bypass relay. No step in this plan makes relay the authority for IPC scheduling, registration, or connection establishment.
4. **Rust scheduler is authoritative for remote IPC.** Python may expose `ConcurrencyConfig` as a typed SDK facade, but remote cross-process calls must be controlled by `core/transport/c2-server/src/scheduler.rs`.

## Current Problem

`NativeServerBridge.register_crm()` creates a Python `Scheduler(cc_config)` for every route, but `RustServer.register_route()` currently ignores that remote scheduler configuration and hardcodes `ConcurrencyMode::ReadParallel`:

```rust
let scheduler = Arc::new(Scheduler::new(ConcurrencyMode::ReadParallel, map));
```

This means remote IPC cannot reliably honor `ConcurrencyConfig(mode=ConcurrencyMode.PARALLEL)` or `ConcurrencyConfig(mode=ConcurrencyMode.EXCLUSIVE)`. The Python scheduler still matters for the thread-local fast path because `CRMProxy.call_direct()` uses `scheduler.execution_guard(access)`, but it should not be treated as the remote IPC execution authority.

## File Responsibility Map

- Modify `sdk/python/tests/integration/test_remote_scheduler_config.py`: new end-to-end remote IPC tests for `exclusive`, `parallel`, `read_parallel`, no-relay direct IPC, and SHM zero-copy guardrails.
- Modify `core/transport/c2-server/src/scheduler.rs`: keep existing lock semantics, add optional route-level capacity permits only if `max_pending` is included in this PR.
- Modify `sdk/python/native/src/server_ffi.rs`: extend `RustServer.register_route()` to accept concurrency mode, parse mode strings, and construct the Rust scheduler from that mode. Optional remote limits are added only in Task 5.
- Modify `sdk/python/src/c_two/transport/server/native.py`: pass `cc_config.mode.value` to native registration; pass explicit limits only if Task 5 implements them; keep Python `Scheduler` only for thread-local slot info.
- Modify `sdk/python/src/c_two/transport/server/scheduler.py`: reduce comments/API wording so it is described as thread-local direct-call support plus `ConcurrencyConfig`, not as the remote IPC executor.
- Modify `sdk/python/tests/unit/test_scheduler.py`: keep existing Python scheduler API tests unless the implementation removes those APIs in the same PR; the safe first pass only updates wording and preserves behavior.
- Modify `sdk/python/tests/unit/test_security.py`: keep or update tests for Python scheduler `begin()`/capacity behavior if the API remains exported.
- Modify `sdk/python/tests/unit/test_name_collision.py`: update fake `register_route()` assertions to match the new native registration signature.
- Modify `sdk/python/tests/unit/test_proxy_concurrency.py`: keep coverage proving thread-local semantics remain intact and object-direct.
- Modify `sdk/python/native/src/shm_buffer.rs`: do not change unless a test needs a read-only introspection property; prefer using existing `.is_inline` from Python tests.
- Modify `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: update the P1 entry after implementation so it points to the new Rust-owned remote scheduler behavior.
- Modify `AGENTS.md`: update only if implementation changes the durable architecture guidance.

## Implementation Tasks

### Task 1: Add failing remote IPC tests for mode propagation

**Files:**
- Create: `sdk/python/tests/integration/test_remote_scheduler_config.py`

- [ ] **Step 1: Create the test module with isolated registry cleanup and a remote-only connection helper**

Use a direct `ipc://` address so this test never depends on relay name resolution:

```python
from __future__ import annotations

import threading
import time

import pytest

import c_two as cc
from c_two.config import settings
from c_two.transport.server.scheduler import ConcurrencyConfig, ConcurrencyMode
from c_two.transport.registry import _ProcessRegistry


@pytest.fixture(autouse=True)
def _clean_env_and_registry(monkeypatch):
    previous_override_relay = settings._relay_anchor_address
    previous_override_shm_threshold = settings._shm_threshold
    settings.relay_anchor_address = None
    monkeypatch.delenv('C2_RELAY_ANCHOR_ADDRESS', raising=False)
    yield
    try:
        cc.shutdown()
    except Exception:
        pass
    _ProcessRegistry._instance = None
    settings._relay_anchor_address = previous_override_relay
    settings._shm_threshold = previous_override_shm_threshold


def _direct_client(crm_type: type, name: str):
    address = cc.server_address()
    assert address.startswith('ipc://')
    return cc.connect(crm_type, name=name, address=address)
```

- [ ] **Step 2: Add a CRM/resource pair that records max concurrent execution**

```python
@cc.transferable
class Delay:
    value: float

    def serialize(value: 'Delay') -> bytes:
        import pickle
        return pickle.dumps(value.value)

    def deserialize(buf: memoryview) -> 'Delay':
        import pickle
        return Delay(float(pickle.loads(bytes(buf))))


@cc.transferable
class Window:
    start: float
    end: float

    def serialize(value: 'Window') -> bytes:
        import pickle
        return pickle.dumps((value.start, value.end))

    def deserialize(buf: memoryview) -> 'Window':
        import pickle
        start, end = pickle.loads(bytes(buf))
        return Window(float(start), float(end))


@cc.transferable
class Count:
    value: int

    def serialize(value: 'Count') -> bytes:
        import pickle
        return pickle.dumps(value.value)

    def deserialize(buf: memoryview) -> 'Count':
        import pickle
        return Count(int(pickle.loads(bytes(buf))))


@cc.crm(namespace='test.remote.scheduler', version='0.1.0')
class RemoteSchedulerProbe:
    @cc.read
    @cc.transfer(input=Delay, output=Window)
    def read_op(self, delay: Delay) -> Window: ...

    @cc.write
    @cc.transfer(input=Delay, output=Window)
    def write_op(self, delay: Delay) -> Window: ...

    @cc.write
    @cc.transfer(output=Count)
    def max_active(self) -> Count: ...


class RemoteSchedulerProbeImpl:
    def __init__(self):
        self.active = 0
        self.max_seen = 0
        self.lock = threading.Lock()

    def _run(self, delay: Delay) -> Window:
        start = time.monotonic()
        with self.lock:
            self.active += 1
            self.max_seen = max(self.max_seen, self.active)
        try:
            time.sleep(delay.value)
            return Window(start, time.monotonic())
        finally:
            with self.lock:
                self.active -= 1

    def read_op(self, delay: Delay) -> Window:
        return self._run(delay)

    def write_op(self, delay: Delay) -> Window:
        return self._run(delay)

    def max_active(self) -> Count:
        return Count(self.max_seen)
```

- [ ] **Step 3: Add `exclusive` remote IPC serialization test**

This fails before implementation because native route registration hardcodes `ReadParallel` and can run read methods concurrently even when Python passed `Exclusive`.

```python
def test_remote_ipc_exclusive_serializes_reads():
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='remote_exclusive',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.EXCLUSIVE),
    )
    cc.serve(blocking=False)

    errors: list[BaseException] = []

    def worker():
        client = _direct_client(RemoteSchedulerProbe, 'remote_exclusive')
        try:
            client.read_op(Delay(0.05))
        except BaseException as exc:
            errors.append(exc)
        finally:
            cc.close(client)

    threads = [threading.Thread(target=worker) for _ in range(3)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=5)

    assert not errors
    probe = _direct_client(RemoteSchedulerProbe, 'remote_exclusive')
    try:
        assert probe.max_active().value == 1
    finally:
        cc.close(probe)
```

- [ ] **Step 4: Add `parallel` remote IPC concurrency test**

This fails before implementation because hardcoded `ReadParallel` treats write methods as exclusive, even when Python passed `Parallel`.

```python
def test_remote_ipc_parallel_allows_concurrent_writes():
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='remote_parallel',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.PARALLEL),
    )
    cc.serve(blocking=False)

    errors: list[BaseException] = []

    def worker():
        client = _direct_client(RemoteSchedulerProbe, 'remote_parallel')
        try:
            client.write_op(Delay(0.08))
        except BaseException as exc:
            errors.append(exc)
        finally:
            cc.close(client)

    threads = [threading.Thread(target=worker) for _ in range(3)]
    start = time.monotonic()
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=5)
    elapsed = time.monotonic() - start

    assert not errors
    probe = _direct_client(RemoteSchedulerProbe, 'remote_parallel')
    try:
        assert probe.max_active().value >= 2
    finally:
        cc.close(probe)
    assert elapsed < 0.20
```

- [ ] **Step 5: Add `read_parallel` remote IPC read/write behavior tests**

These preserve the current default semantics while proving the path still works after making mode explicit:

```python
def test_remote_ipc_read_parallel_allows_reads_and_serializes_writes():
    read_impl = RemoteSchedulerProbeImpl()
    cc.register(
        RemoteSchedulerProbe,
        read_impl,
        name='remote_read_parallel_reads',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.READ_PARALLEL),
    )
    cc.serve(blocking=False)

    read_errors: list[BaseException] = []

    def read_worker():
        client = _direct_client(RemoteSchedulerProbe, 'remote_read_parallel_reads')
        try:
            client.read_op(Delay(0.05))
        except BaseException as exc:
            read_errors.append(exc)
        finally:
            cc.close(client)

    readers = [threading.Thread(target=read_worker) for _ in range(3)]
    for thread in readers:
        thread.start()
    for thread in readers:
        thread.join(timeout=5)

    assert not read_errors
    client = _direct_client(RemoteSchedulerProbe, 'remote_read_parallel_reads')
    try:
        assert client.max_active().value >= 2
    finally:
        cc.close(client)

    cc.unregister('remote_read_parallel_reads')

    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='remote_read_parallel_writes',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.READ_PARALLEL),
    )

    write_errors: list[BaseException] = []

    def write_worker():
        client = _direct_client(RemoteSchedulerProbe, 'remote_read_parallel_writes')
        try:
            client.write_op(Delay(0.05))
        except BaseException as exc:
            write_errors.append(exc)
        finally:
            cc.close(client)

    writers = [threading.Thread(target=write_worker) for _ in range(3)]
    for thread in writers:
        thread.start()
    for thread in writers:
        thread.join(timeout=5)

    assert not write_errors
    client = _direct_client(RemoteSchedulerProbe, 'remote_read_parallel_writes')
    try:
        assert client.max_active().value == 1
    finally:
        cc.close(client)
```

- [ ] **Step 6: Run the new tests and verify expected failures**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_remote_scheduler_config.py -q --timeout=30
```

Expected before implementation: `test_remote_ipc_exclusive_serializes_reads` and `test_remote_ipc_parallel_allows_concurrent_writes` fail because the Rust route scheduler is still hardcoded to `ReadParallel`. The `read_parallel` test may pass before implementation.

- [ ] **Step 7: Commit the failing tests**

```bash
git add sdk/python/tests/integration/test_remote_scheduler_config.py
git commit -m "test: cover remote ipc scheduler modes"
```

### Task 2: Pass concurrency mode from Python bridge into native route registration

**Files:**
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Modify: `sdk/python/native/src/server_ffi.rs`

- [ ] **Step 1: Extend the PyO3 `register_route` signature**

In `sdk/python/native/src/server_ffi.rs`, replace the existing registration signature annotation and function parameters with a mode input:

```rust
#[pyo3(signature = (name, dispatcher, method_names, access_map, concurrency_mode))]
fn register_route(
    &self,
    py: Python<'_>,
    name: &str,
    dispatcher: Py<PyAny>,
    method_names: Vec<String>,
    access_map: &Bound<'_, PyDict>,
    concurrency_mode: &str,
) -> PyResult<()> {
```

- [ ] **Step 2: Parse mode strings into the Rust enum**

Add this helper near the top-level functions in `sdk/python/native/src/server_ffi.rs`:

```rust
fn parse_concurrency_mode(mode: &str) -> PyResult<ConcurrencyMode> {
    match mode {
        "parallel" => Ok(ConcurrencyMode::Parallel),
        "exclusive" => Ok(ConcurrencyMode::Exclusive),
        "read_parallel" => Ok(ConcurrencyMode::ReadParallel),
        other => Err(PyValueError::new_err(format!(
            "invalid concurrency mode '{other}', expected 'parallel', 'exclusive', or 'read_parallel'"
        ))),
    }
}
```

Use it inside `register_route()` before constructing the scheduler:

```rust
let mode = parse_concurrency_mode(concurrency_mode)?;
```

- [ ] **Step 3: Construct the Rust scheduler from the parsed mode**

For the first implementation pass, keep the existing `Scheduler::new(mode, map)` constructor and do not accept remote limit arguments:

```rust
let scheduler = Arc::new(Scheduler::new(mode, map));
```

This is intentionally minimal: `max_pending` and `max_workers` should not be silently accepted for remote IPC unless Rust actually enforces them. If this PR includes Task 5, extend the signature in that task and replace this block with the real Rust limit constructor.

- [ ] **Step 4: Pass mode from `NativeServerBridge.register_crm()`**

In `sdk/python/src/c_two/transport/server/native.py`, replace the native registration call:

```python
self._rust_server.register_route(
    routing_name, dispatcher, methods, access_map,
)
```

with this first-pass mode-only call:

```python
self._rust_server.register_route(
    routing_name,
    dispatcher,
    methods,
    access_map,
    cc_config.mode.value,
)
```

Do not pass `cc_config.max_pending` or `cc_config.max_workers` in the first pass. Passing them while Rust ignores them would make remote IPC silently accept configuration that is not enforced. Wire `max_pending` only if Task 5 is implemented in the same change, and do not wire `max_workers` until it has real Rust semantics.

Also update Python tests that fake `RustServer.register_route()` so they assert the new call shape. In `sdk/python/tests/unit/test_name_collision.py::test_register_rolls_back_python_slot_when_rust_registration_fails`, change the fake to record the mode argument:

```python
class FakeRustServer:
    def register_route(self, name, dispatcher, methods, access_map, concurrency_mode):  # noqa: ARG002
        events.append(f'rust_register:{concurrency_mode}')
        raise RuntimeError('boom')
```

and update the assertion to:

```python
assert events == ['rust_register:read_parallel', 'scheduler_shutdown']
```

- [ ] **Step 5: Rebuild native extension and run focused tests**

Run:

```bash
uv sync --reinstall-package c-two
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_remote_scheduler_config.py -q --timeout=30
```

Expected after this task: remote mode propagation tests pass for `exclusive`, `parallel`, and `read_parallel`. Tests involving remote `max_pending` or `max_workers` are not added until Task 5.

- [ ] **Step 6: Commit mode propagation**

```bash
git add sdk/python/src/c_two/transport/server/native.py sdk/python/native/src/server_ffi.rs sdk/python/tests/unit/test_name_collision.py
git commit -m "fix: pass remote scheduler mode to rust"
```

### Task 3: Add native validation tests for registration mode parsing

**Files:**
- Modify: `sdk/python/tests/integration/test_remote_scheduler_config.py`

- [ ] **Step 1: Add an invalid mode bridge test through `ConcurrencyConfig` is already covered**

`ConcurrencyConfig(mode='bogus')` already raises in `sdk/python/tests/unit/test_scheduler.py`. Do not duplicate that Python facade test here.

- [ ] **Step 2: Add a native registration smoke test for all valid mode strings through the public API**

Append this test to `test_remote_scheduler_config.py`:

```python
@pytest.mark.parametrize(
    'mode',
    [ConcurrencyMode.PARALLEL, ConcurrencyMode.EXCLUSIVE, ConcurrencyMode.READ_PARALLEL],
)
def test_remote_ipc_registers_all_public_concurrency_modes(mode):
    name = f'remote_mode_{mode.value}'
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name=name,
        concurrency=ConcurrencyConfig(mode=mode),
    )
    cc.serve(blocking=False)
    client = _direct_client(RemoteSchedulerProbe, name)
    try:
        assert client.max_active().value == 0
    finally:
        cc.close(client)
```

- [ ] **Step 3: Run the expanded focused test file**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_remote_scheduler_config.py -q --timeout=30
```

Expected: all tests pass.

- [ ] **Step 4: Commit the validation coverage**

```bash
git add sdk/python/tests/integration/test_remote_scheduler_config.py
git commit -m "test: validate remote scheduler mode registration"
```

### Task 4: Add zero-copy guard coverage for large remote IPC input

**Files:**
- Modify: `sdk/python/tests/integration/test_remote_scheduler_config.py`

- [ ] **Step 1: Add a CRM/resource pair that observes the received request buffer type**

Append this code to the test module:

```python
@cc.transferable
class LargePayload:
    data: bytes

    def serialize(value: 'LargePayload') -> bytes:
        return value.data

    def deserialize(buf: memoryview) -> 'LargePayload':
        return LargePayload(bytes(buf))

    def from_buffer(buf: memoryview) -> 'LargePayload':
        base = getattr(buf, 'obj', None)
        is_inline = getattr(base, 'is_inline', None)
        LargePayloadResource.last_buffer_info = (
            type(buf).__name__,
            type(base).__name__ if base is not None else None,
            is_inline,
            len(buf),
        )
        return LargePayload(bytes(buf[:8]))


@cc.crm(namespace='test.remote.scheduler.large', version='0.1.0')
class LargePayloadCRM:
    @cc.transfer(input=LargePayload, output=LargePayload, buffer='hold')
    def echo_large(self, payload: LargePayload) -> LargePayload: ...

    @cc.read
    def last_info(self) -> tuple[str | None, str | None, bool | None, int]: ...


class LargePayloadResource:
    last_buffer_info: tuple[str | None, str | None, bool | None, int] = (None, None, None, 0)

    def echo_large(self, payload: LargePayload) -> LargePayload:
        return payload

    def last_info(self) -> tuple[str | None, str | None, bool | None, int]:
        return self.last_buffer_info
```

- [ ] **Step 2: Add the SHM-backed remote input test**

Force a small process SHM threshold before registration so the IPC client uses SHM deterministically rather than depending on environment defaults:

```python
def test_remote_ipc_large_hold_input_reaches_python_as_shm_memoryview():
    settings.shm_threshold = 1024
    LargePayloadResource.last_buffer_info = (None, None, None, 0)
    cc.register(
        LargePayloadCRM,
        LargePayloadResource(),
        name='large_zero_copy',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.EXCLUSIVE),
    )
    cc.serve(blocking=False)

    client = _direct_client(LargePayloadCRM, 'large_zero_copy')
    try:
        payload = LargePayload(b'x' * 8192)
        result = client.echo_large(payload)
        assert result.data == b'x' * 8
        view_type, base_type, is_inline, size = client.last_info()
    finally:
        cc.close(client)

    assert view_type == 'memoryview'
    assert base_type == 'ShmBuffer'
    assert is_inline is False
    assert size == 8192
```

- [ ] **Step 3: Run the zero-copy guard test**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_remote_scheduler_config.py::test_remote_ipc_large_hold_input_reaches_python_as_shm_memoryview -q --timeout=30
```

Expected: pass. If this fails because `LargePayload.deserialize()` is used instead of `from_buffer`, inspect `sdk/python/src/c_two/crm/transfer.py` and adjust only the test annotations to trigger the existing hold path; do not change `PyCrmCallback::invoke()` to pass `bytes`.

- [ ] **Step 4: Commit the zero-copy guard test**

```bash
git add sdk/python/tests/integration/test_remote_scheduler_config.py
git commit -m "test: guard remote scheduler zero copy input path"
```

### Task 5: Optional — implement remote `max_pending` in Rust; reject or defer remote `max_workers`

**Files:**
- Modify: `core/transport/c2-server/src/scheduler.rs`
- Modify: `sdk/python/native/src/server_ffi.rs`
- Modify: `sdk/python/tests/integration/test_remote_scheduler_config.py`

This task is optional and should be a separate commit, or a separate PR if the mode propagation change is already large. The recommended first PR may skip remote `max_pending` and `max_workers`. If skipped, the public bridge must not pass those values into Rust and docs must not claim they are enforced for remote IPC. If this task is included, implement only `max_pending` as a Rust async semaphore/backpressure limit. Do not reintroduce a Python remote pending counter.

- [ ] **Step 1: Add Rust scheduler options**

In `core/transport/c2-server/src/scheduler.rs`, add `Semaphore` imports and options:

```rust
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SchedulerOptions {
    pub max_pending: Option<usize>,
}
```

Update the struct:

```rust
pub struct Scheduler {
    mode: ConcurrencyMode,
    rw_lock: RwLock<()>,
    access_map: HashMap<u16, AccessLevel>,
    pending_permits: Option<Arc<Semaphore>>,
}
```

- [ ] **Step 2: Add an options constructor while preserving `new()` for existing Rust tests**

```rust
impl Scheduler {
    pub fn new(mode: ConcurrencyMode, access_map: HashMap<u16, AccessLevel>) -> Self {
        Self::with_options(mode, access_map, SchedulerOptions::default())
    }

    pub fn with_options(
        mode: ConcurrencyMode,
        access_map: HashMap<u16, AccessLevel>,
        options: SchedulerOptions,
    ) -> Self {
        Self {
            mode,
            rw_lock: RwLock::new(()),
            access_map,
            pending_permits: options.max_pending.map(|n| Arc::new(Semaphore::new(n))),
        }
    }
```

- [ ] **Step 3: Acquire the pending permit before lock selection**

At the start of `execute()`:

```rust
let _pending_permit = match &self.pending_permits {
    Some(permits) => Some(permits.clone().acquire_owned().await.unwrap()),
    None => None,
};
```

This makes `max_pending` a backpressure limit rather than an immediate error. If immediate rejection is required, replace `acquire_owned().await` with `try_acquire_owned()` and return a typed scheduler-capacity error through `CrmError`; that requires changing `execute()` to return a scheduler error wrapper and is outside the minimal mode-propagation fix.

- [ ] **Step 4: Add a Rust unit test for permit-limited execution**

Append to the `#[cfg(test)]` module in `scheduler.rs`:

```rust
#[tokio::test]
async fn max_pending_limits_concurrent_execution() {
    let sched = Arc::new(Scheduler::with_options(
        ConcurrencyMode::Parallel,
        empty_map(),
        SchedulerOptions { max_pending: Some(1) },
    ));
    let max_concurrent = Arc::new(AtomicU32::new(0));
    let active = Arc::new(AtomicU32::new(0));

    let mut handles = Vec::new();
    for _ in 0..3 {
        let s = Arc::clone(&sched);
        let mc = Arc::clone(&max_concurrent);
        let ac = Arc::clone(&active);
        handles.push(tokio::spawn(async move {
            s.execute(0, move || {
                let cur = ac.fetch_add(1, Ordering::SeqCst) + 1;
                mc.fetch_max(cur, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(10));
                ac.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 5: Wire `max_pending` through PyO3 if Task 5 is included**

In `server_ffi.rs`, import `SchedulerOptions`, extend the PyO3 signature to accept `max_pending`, and replace scheduler construction:

```rust
use c2_server::scheduler::{AccessLevel, ConcurrencyMode, Scheduler, SchedulerOptions};
```

```rust
#[pyo3(signature = (name, dispatcher, method_names, access_map, concurrency_mode, max_pending=None))]
fn register_route(
    &self,
    py: Python<'_>,
    name: &str,
    dispatcher: Py<PyAny>,
    method_names: Vec<String>,
    access_map: &Bound<'_, PyDict>,
    concurrency_mode: &str,
    max_pending: Option<usize>,
) -> PyResult<()> {
    if matches!(max_pending, Some(0)) {
        return Err(PyValueError::new_err("max_pending must be at least 1 when provided"));
    }
    // existing access_map parsing and mode parsing stay here
    let scheduler = Arc::new(Scheduler::with_options(
        mode,
        map,
        SchedulerOptions { max_pending },
    ));
}
```

Then update `NativeServerBridge.register_crm()` to pass `cc_config.max_pending` only when this Rust implementation exists. Keep `max_workers` out of the native signature until there is a concrete Rust limit for it.

- [ ] **Step 6: Add a Python integration test for remote `max_pending` only if Task 5 is included**

```python
def test_remote_ipc_max_pending_limits_parallel_execution_when_enabled():
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='remote_max_pending',
        concurrency=ConcurrencyConfig(
            mode=ConcurrencyMode.PARALLEL,
            max_pending=1,
        ),
    )
    cc.serve(blocking=False)

    errors: list[BaseException] = []

    def worker():
        client = _direct_client(RemoteSchedulerProbe, 'remote_max_pending')
        try:
            client.write_op(Delay(0.04))
        except BaseException as exc:
            errors.append(exc)
        finally:
            cc.close(client)

    threads = [threading.Thread(target=worker) for _ in range(3)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=5)

    assert not errors
    client = _direct_client(RemoteSchedulerProbe, 'remote_max_pending')
    try:
        assert client.max_active().value == 1
    finally:
        cc.close(client)
```

- [ ] **Step 7: Run Rust and Python focused tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server scheduler
uv sync --reinstall-package c-two
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_remote_scheduler_config.py -q --timeout=30
```

Expected: Rust scheduler tests pass; Python remote tests pass.

- [ ] **Step 8: Commit optional capacity support**

```bash
git add core/transport/c2-server/src/scheduler.rs sdk/python/native/src/server_ffi.rs sdk/python/tests/integration/test_remote_scheduler_config.py
git commit -m "feat: enforce remote scheduler pending limit in rust"
```

### Task 6: Cleanly split Python scheduler documentation and tests from remote IPC authority

**Files:**
- Modify: `sdk/python/src/c_two/transport/server/scheduler.py`
- Modify: `sdk/python/tests/unit/test_scheduler.py`
- Modify: `sdk/python/tests/unit/test_proxy_concurrency.py`
- Modify: `sdk/python/tests/unit/test_security.py` if scheduler lifecycle APIs change

- [ ] **Step 1: Update `scheduler.py` module docstring to state its remaining authority**

Replace the current module docstring with:

```python
"""Thread-local read/write guards and concurrency config facade.

Remote IPC scheduling is enforced by the Rust ``c2-server`` scheduler. This
module remains in Python for two SDK-local responsibilities:

- expose ``ConcurrencyMode`` and ``ConcurrencyConfig`` as typed public SDK
  inputs to ``cc.register(..., concurrency=...)``;
- guard same-process thread-local ``CRMProxy.call_direct()`` calls, which pass
  Python objects directly and intentionally skip serialization.

Do not use this scheduler as the remote IPC execution authority.
"""
```

- [ ] **Step 2: Keep scheduler API behavior while adding direct guard coverage**

In `sdk/python/tests/unit/test_scheduler.py`, keep tests for:

- `ConcurrencyConfig` validation;
- `_WriterPriorityReadWriteLock` behavior;
- `Scheduler.execution_guard()` behavior if direct tests are added;
- `Scheduler.begin()` and `pending_count` because they remain public/exported in this first pass.

Do not remove `begin()`, `execute()`, `execute_fast()`, `pending_count`, or `shutdown()` in the mode-propagation PR. They are still exported, covered by `sdk/python/tests/unit/test_scheduler.py` and `sdk/python/tests/unit/test_security.py`, and may be used by downstream 0.x users. A later clean-cut PR may remove them after a dedicated repository-wide search and public export cleanup. For this PR, add direct `execution_guard()` tests and update docstrings without changing runtime behavior. If a later PR removes `execute()`/`execute_fast()`, replace tests with direct `execution_guard()` tests. A minimal replacement test looks like:

```python
def test_execution_guard_exclusive_serializes_thread_local_calls():
    sched = Scheduler(ConcurrencyConfig(mode='exclusive'))
    log: list[str] = []
    lock = threading.Lock()

    def worker():
        with sched.execution_guard(MethodAccess.WRITE):
            with lock:
                log.append('enter')
            time.sleep(0.02)
            with lock:
                log.append('exit')

    threads = [threading.Thread(target=worker) for _ in range(3)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=5)

    for idx in range(0, len(log), 2):
        assert log[idx] == 'enter'
        assert log[idx + 1] == 'exit'
```

- [ ] **Step 3: Keep `test_proxy_concurrency.py` as thread-local coverage**

Do not weaken `sdk/python/tests/unit/test_proxy_concurrency.py`; it proves the separate thread-local path still has language-specific guard semantics. Add this assertion to the no-scheduler test if it is not already present:

```python
assert proxy.supports_direct_call is True
```

- [ ] **Step 4: Run Python unit tests for the remaining thread-local scheduler**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_scheduler.py sdk/python/tests/unit/test_proxy_concurrency.py sdk/python/tests/unit/test_security.py -q --timeout=30
```

Expected: all tests pass and no test describes Python scheduler as the remote IPC authority.

- [ ] **Step 5: Commit Python scheduler cleanup**

```bash
git add sdk/python/src/c_two/transport/server/scheduler.py sdk/python/tests/unit/test_scheduler.py sdk/python/tests/unit/test_proxy_concurrency.py sdk/python/tests/unit/test_security.py
git commit -m "refactor: limit python scheduler to thread local guards"
```

### Task 7: Verify direct IPC still bypasses relay

**Files:**
- Modify: `sdk/python/tests/integration/test_remote_scheduler_config.py`

- [ ] **Step 1: Add a bad relay environment test with explicit IPC address**

Append this test:

```python
def test_remote_ipc_scheduler_direct_address_ignores_bad_relay(monkeypatch):
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='direct_no_relay_scheduler',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.EXCLUSIVE),
    )
    cc.serve(blocking=False)
    direct_address = cc.server_address()

    # Set a bad relay only after local registration. This isolates the invariant
    # under test: an explicit IPC address must bypass relay name resolution.
    monkeypatch.setenv('C2_RELAY_ANCHOR_ADDRESS', 'http://127.0.0.1:9')
    settings.relay_anchor_address = None

    client = cc.connect(
        RemoteSchedulerProbe,
        name='direct_no_relay_scheduler',
        address=direct_address,
    )
    try:
        client.write_op(Delay(0.001))
        assert client.max_active().value == 1
    finally:
        cc.close(client)
```

- [ ] **Step 2: Run the no-relay direct IPC test**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_remote_scheduler_config.py::test_remote_ipc_scheduler_direct_address_ignores_bad_relay -q --timeout=30
```

Expected: pass. Failure here means scheduler changes coupled direct IPC to relay or let relay environment override explicit IPC address.

- [ ] **Step 3: Commit the relay-independence regression test**

```bash
git add sdk/python/tests/integration/test_remote_scheduler_config.py
git commit -m "test: keep direct ipc scheduler independent of relay"
```

### Task 8: Update architecture docs after implementation

**Files:**
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify: `AGENTS.md` if durable guidance changed

- [ ] **Step 1: Update the P1 scheduler entry in the boundary document**

Change the P1 entry from a pending candidate to implemented behavior. Use this wording if the implementation matches Tasks 1-4:

```markdown
Remote IPC scheduler mode is owned by Rust `c2-server`. Python passes
`ConcurrencyConfig.mode.value` during route registration; PyO3 parses it into
`c2_server::scheduler::ConcurrencyMode`; Rust acquires the per-route locks
around callback invocation. Python keeps `ConcurrencyConfig` as the SDK typed
facade and keeps `Scheduler.execution_guard()` only for thread-local direct
calls.
```

If Task 5 is included, add:

```markdown
Remote `max_pending` is enforced by the Rust scheduler through per-route async
permits. `max_workers` is accepted as SDK configuration but is not documented as
a remote worker-pool guarantee unless Rust implements it as a real permit or
runtime limit.
```

- [ ] **Step 2: Update AGENTS.md only if implementation changed durable rules**

If the current `AGENTS.md` already says Rust owns transport scheduling and Python keeps thread-local as a language optimization, do not edit it. If it still implies Python owns remote resource scheduling, replace that wording with:

```markdown
Python owns CRM discovery, resource invocation, serialization orchestration, and
thread-local direct-call guards. Rust owns remote IPC scheduler mode enforcement
for cross-process calls.
```

- [ ] **Step 3: Run doc sanity checks**

Run:

```bash
rg -n "hardcoded|ReadParallel|Python owns.*remote.*scheduler|remote IPC scheduling is Python-owned" docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md AGENTS.md
```

Expected: no stale claims that remote IPC scheduling is Python-owned. The command may still find historical discussion in the new implementation plan; do not change historical problem statements there unless they read as current guidance.

- [ ] **Step 4: Commit docs updates**

```bash
git add docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md AGENTS.md
git commit -m "docs: record rust-owned remote ipc scheduler"
```

### Task 9: Full verification before PR update

**Files:**
- No code changes unless verification exposes a real bug.

- [ ] **Step 1: Rebuild the Python native extension**

Run:

```bash
uv sync --reinstall-package c-two
```

Expected: build succeeds.

- [ ] **Step 2: Run focused Python tests**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/integration/test_remote_scheduler_config.py \
  sdk/python/tests/unit/test_scheduler.py \
  sdk/python/tests/unit/test_proxy_concurrency.py \
  -q --timeout=30
```

Expected: all selected tests pass.

- [ ] **Step 3: Run Rust scheduler tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server scheduler
```

Expected: all `c2-server` scheduler tests pass.

- [ ] **Step 4: Run the full Python suite**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30
```

Expected: all Python SDK tests pass.

- [ ] **Step 5: Check for accidental data-path changes**

Run:

```bash
git diff -- sdk/python/native/src/server_ffi.rs sdk/python/src/c_two/transport/server/native.py core/transport/c2-server/src/server.rs | rg -n "from_peer_shm|from_handle|from_inline|memoryview\(request_buf\)|to_vec\(|bytes\(request_buf\)|RequestData::Shm|RequestData::Handle"
```

Expected: route registration changes are visible, but `RequestData::Shm`, `RequestData::Handle`, `PyShmBuffer::from_peer_shm`, `PyShmBuffer::from_handle`, and `memoryview(request_buf)` remain the dispatch path. There should be no new `bytes(request_buf)` or SHM-to-`Vec<u8>` conversion on the normal buddy/handle path.

- [ ] **Step 6: Inspect final status**

Run:

```bash
git status --short
```

Expected: only intentional files are modified, or the working tree is clean after commits.

## `max_workers` Decision Record

Do not implement remote `max_workers` by adding a Python thread pool or by trying to let Rust schedule Python functions directly from a type config in a way that requires converting SHM payloads to `bytes`. The earlier rejected design would add an extra payload copy because Rust and Python cannot share function-local bytes ownership for arbitrary SDK functions. If remote worker limiting is required, implement it in Rust as a route-level permit around `spawn_blocking`, and document that it limits concurrent blocking invocations rather than creating a Python-owned executor.

## Thread-Local Bypass Decision Record

Thread-local calls intentionally bypass serialization and Rust IPC. They should keep passing Python objects directly through `CRMProxy.call_direct()`. If the project later needs identical lock fairness between thread-local and remote IPC, the acceptable direction is a tiny native read/write permit guard exposed to SDKs, or a thin per-SDK guard that maps to language-native synchronization. Do not force thread-local through remote bytes dispatch just to share scheduler code.

## Success Criteria

- Remote IPC `ConcurrencyConfig(mode=EXCLUSIVE)` serializes all route calls in Rust.
- Remote IPC `ConcurrencyConfig(mode=PARALLEL)` allows concurrent write calls.
- Remote IPC `ConcurrencyConfig(mode=READ_PARALLEL)` allows concurrent reads and serializes writes.
- Large SHM-backed remote input still reaches Python as `memoryview` over `ShmBuffer`, not as Python `bytes`.
- Same-process thread-local calls still skip serialization and pass Python objects directly.
- Explicit direct IPC address still works with no relay and with a bad relay environment variable.
- Python SDK no longer presents its scheduler as the remote IPC execution authority.
