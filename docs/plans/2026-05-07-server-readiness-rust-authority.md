# Server Readiness Rust Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move server startup readiness and running-state authority from Python socket-file polling into Rust `c2-server` / PyO3 native server lifecycle APIs.

**Architecture:** `c2-server::Server` owns lifecycle state and readiness notification. PyO3 `RustServer` starts the Tokio runtime, waits for native readiness, and exposes native state projections. Python `NativeServerBridge` delegates start/readiness to native and stops maintaining `_started` or polling `socket_path`.

**Tech Stack:** Rust `core/transport/c2-server`, PyO3 bindings in `sdk/python/native/src/server_ffi.rs`, Python SDK server bridge in `sdk/python/src/c_two/transport/server/native.py`, pytest integration/unit tests, Cargo tests.

**Implementation status:** Implemented on `dev-feature`.

**Final implementation note:** Task 5 integration coverage exposed an additional
socket-ownership edge case: after a duplicate server failed to bind an active
IPC socket, PyO3 startup cleanup called `shutdown()` on the failed server and
could still unlink the first server's active socket path. The final
implementation therefore tracks whether each concrete `c2-server::Server`
instance successfully bound its socket before allowing that instance to unlink
the socket during shutdown. This is part of the issue6 readiness/lifecycle
boundary, not a separate compatibility path.

Follow-up review also clarified that direct IPC `shutdown("ipc://...")` is an
admin/control-plane helper. It may stop the native server before the owner
process calls `NativeServerBridge.shutdown()`, so bridge cleanup must always
call idempotent native shutdown to drain the PyO3 runtime handle; lifecycle
`is_running == false` is not a valid reason to skip runtime cleanup.

Final strict review found one more lifecycle edge: readiness waiters could see
the previous attempt's terminal `Stopped` / `Failed` state before the newly
spawned accept-loop task had a chance to set `Starting`. The implementation now
uses a Rust-side start-attempt fence before PyO3 spawns and waits: it resets the
one-shot shutdown signal, moves terminal state back to `Starting`, then waits
for readiness for the current attempt. This keeps restart after normal shutdown
and retry after a failed duplicate bind inside native lifecycle authority,
without adding Python polling or retry logic.

The shutdown-side fix is deliberately shared by explicit `RustServer.shutdown()`
and `start_and_wait()` failure cleanup. PyO3 signals native shutdown, waits for
`wait_until_stopped()`, drops the runtime, then calls a terminal cleanup fence
that converts stale runtime-owned `Starting` / `Ready` / `Stopping` states to
`Stopped` without overwriting `Failed(_)` diagnostics. This avoids solving
normal shutdown while leaving timeout or failed-start cleanup poisoned.

---

## 0.x Clean-Cut Constraint

C-Two is in the 0.x line. Do not preserve the Python socket polling path as a compatibility fallback. Remove `_started` as Python-owned lifecycle state once native readiness is wired. Public Python API names such as `Server.start()`, `Server.is_started()`, and `cc.serve()` may remain as facades, but the source of truth must be Rust.

## Corrected Scope For Issue 6

Issue6 is not about relay, route discovery, or Python CRM dispatch semantics. It is about the generic IPC server lifecycle:

```text
Server lifecycle = initialized -> starting -> ready -> stopping -> stopped/failed
Readiness = native UDS listener has bound successfully and the server can accept direct IPC connections
Route slot lifecycle = Python local slot and Rust route table must not diverge during start/shutdown
```

Python still owns CRM object slots and Python callback dispatch. Rust owns whether the IPC server is started, ready, failed, or stopped.

## Current Problem

Current Python startup logic treats socket-file existence as readiness:

```python
def start(self, timeout: float = 5.0) -> None:
    self._rust_server.start()
    socket_path = self._rust_server.socket_path
    while not os.path.exists(socket_path):
        ...
    self._started = True
```

This has three problems:

1. A socket path can exist before the accept loop is actually ready.
2. Every SDK would need to duplicate platform-specific socket polling.
3. Python `_started` can diverge from native runtime state if Rust startup fails after spawning.

There is also an adjacent native startup safety issue: `c2-server::Server::run()` unconditionally removes any existing socket path before binding. If another active C-Two server is already listening on that path, a second server can unlink the active socket path and break direct IPC routing. Issue6 should fix this as part of native lifecycle authority.

## Target Behavior

- `RustServer.start_and_wait(timeout_seconds)` returns only after native readiness is observed.
- `RustServer.start()` delegates to `start_and_wait(5.0)` for 0.x clean-cut behavior.
- `RustServer.is_running` and `RustServer.is_ready` project native lifecycle state, not merely “Tokio runtime object exists”.
- `NativeServerBridge.start(timeout)` calls `_rust_server.start_and_wait(timeout)` and does not import `os`, inspect `socket_path`, or keep `_started`.
- `NativeServerBridge.is_started()` returns native state.
- Runtime shutdown outcomes use native running/readiness state, not `socket_path().exists()`.
- Starting a second server on an active socket returns a useful error and does not unlink the first server's socket.
- Stale socket files are still removed before binding when no active listener exists.
- Explicit direct IPC remains relay-independent.

## File Responsibility Map

- Modify `core/transport/c2-server/src/server.rs`: add lifecycle state, readiness watch channel, active-socket guard, `wait_until_ready()`, `is_ready()`, and `is_running()`.
- Modify `core/transport/c2-server/src/lib.rs`: export `ServerLifecycleState`.
- Modify `sdk/python/native/src/server_ffi.rs`: expose `RustServer.start_and_wait(timeout_seconds)`, native `is_ready`, and native-backed `is_running`; make `start()` delegate to `start_and_wait(5.0)`.
- Modify `sdk/python/native/src/runtime_session_ffi.rs`: rely on native server state for `server_was_started` shutdown outcome.
- Modify `sdk/python/src/c_two/transport/server/native.py`: remove `os` socket polling and `_started`; use native readiness/running projection.
- Modify tests under `sdk/python/tests/unit/` and `sdk/python/tests/integration/`: add guards against Python readiness ownership, direct IPC readiness regressions, and active-socket unlinking.
- Modify `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: mark issue6 with this implementation plan path.
- Modify `AGENTS.md` after implementation: document that Rust owns server readiness/lifecycle state and Python must not reintroduce socket polling.

---

## Task 0: Baseline And Stale Symbol Audit

**Files:**
- Read: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Read: `core/transport/c2-server/src/server.rs`
- Read: `sdk/python/native/src/server_ffi.rs`
- Read: `sdk/python/src/c_two/transport/server/native.py`
- Read: `sdk/python/tests/integration/test_direct_ipc_control.py`
- Read: `sdk/python/tests/unit/test_sdk_boundary.py`

- [ ] **Step 1: Confirm worktree state**

Run:

```bash
git status --short --branch
```

Expected: branch is `dev-feature`; no uncommitted source changes. If this plan was just added, commit it before source implementation begins.

- [ ] **Step 2: Run baseline focused tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server
cargo check --manifest-path sdk/python/native/Cargo.toml -q
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/integration/test_direct_ipc_control.py \
  sdk/python/tests/integration/test_server.py \
  -q --timeout=30 -rs
```

Expected: baseline tests pass before implementation. Existing tests may still use helper polling; issue6 will add stricter tests before changing production code.

- [ ] **Step 3: Record current Python readiness ownership**

Run:

```bash
rg -n "_started|os\.path\.exists|socket_path|while not os\.path" \
  sdk/python/src/c_two/transport/server/native.py \
  sdk/python/native/src/server_ffi.rs \
  core/runtime/c2-runtime/src/session.rs
```

Expected: matches show Python `_started`, Python socket polling, and runtime socket-path inference that this plan removes.

- [ ] **Step 4: Commit plan-only changes if present**

Run only if this plan file is uncommitted:

```bash
git add docs/plans/2026-05-07-server-readiness-rust-authority.md

git commit -m "docs: plan server readiness rust authority"
```

Expected: implementation starts from a clean tree with the plan preserved.

---

## Task 1: Add Native Lifecycle State To `c2-server`

**Files:**
- Modify: `core/transport/c2-server/src/server.rs`
- Modify: `core/transport/c2-server/src/lib.rs`
- Test: inline unit tests in `core/transport/c2-server/src/server.rs`

- [ ] **Step 1: Write failing lifecycle tests in `server.rs`**

Append these tests inside the existing `#[cfg(test)] mod tests` in `core/transport/c2-server/src/server.rs`. If the module already imports these names, reuse the existing imports instead of duplicating them.

```rust
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::Arc;
use std::time::Duration;

fn unique_readiness_address(prefix: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("ipc://{prefix}_{}_{}", std::process::id(), n)
}

#[tokio::test]
async fn wait_until_ready_times_out_before_start() {
    let server = Arc::new(Server::new(
        &unique_readiness_address("ready_timeout"),
        ServerIpcConfig::default(),
    ).unwrap());

    let err = server
        .wait_until_ready(Duration::from_millis(1))
        .await
        .expect_err("server that was never started must time out");

    assert!(err.to_string().contains("did not become ready"));
    assert_eq!(server.lifecycle_state(), ServerLifecycleState::Initialized);
    assert!(!server.is_ready());
    assert!(!server.is_running());
}

#[tokio::test]
async fn run_sets_ready_then_shutdown_sets_stopped() {
    let server = Arc::new(Server::new(
        &unique_readiness_address("ready_state"),
        ServerIpcConfig::default(),
    ).unwrap());
    let runner = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.run().await })
    };

    server.wait_until_ready(Duration::from_secs(2)).await.unwrap();
    assert_eq!(server.lifecycle_state(), ServerLifecycleState::Ready);
    assert!(server.is_ready());
    assert!(server.is_running());
    assert!(StdUnixStream::connect(server.socket_path()).is_ok());

    server.shutdown();
    runner.await.unwrap().unwrap();
    assert_eq!(server.lifecycle_state(), ServerLifecycleState::Stopped);
    assert!(!server.is_ready());
    assert!(!server.is_running());
}

#[tokio::test]
async fn active_socket_is_not_unlinked_by_second_server() {
    let address = unique_readiness_address("active_socket");
    let first = Arc::new(Server::new(&address, ServerIpcConfig::default()).unwrap());
    let first_runner = {
        let first = Arc::clone(&first);
        tokio::spawn(async move { first.run().await })
    };
    first.wait_until_ready(Duration::from_secs(2)).await.unwrap();
    assert!(StdUnixStream::connect(first.socket_path()).is_ok());

    let second = Arc::new(Server::new(&address, ServerIpcConfig::default()).unwrap());
    let second_result = tokio::time::timeout(
        Duration::from_millis(200),
        {
            let second = Arc::clone(&second);
            async move { second.run().await }
        },
    )
    .await;

    match second_result {
        Ok(Err(err)) => {
            let message = err.to_string();
            assert!(
                message.contains("already has an active listener")
                    || message.contains("address already in use"),
                "unexpected error: {message}",
            );
        }
        Ok(Ok(())) => panic!("second server unexpectedly started and stopped cleanly"),
        Err(_) => {
            second.shutdown();
            panic!("second server hung instead of rejecting the active socket");
        }
    }

    assert!(first.is_ready());
    assert!(StdUnixStream::connect(first.socket_path()).is_ok());
    first.shutdown();
    first_runner.await.unwrap().unwrap();
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server wait_until_ready_times_out_before_start
cargo test --manifest-path core/Cargo.toml -p c2-server run_sets_ready_then_shutdown_sets_stopped
cargo test --manifest-path core/Cargo.toml -p c2-server active_socket_is_not_unlinked_by_second_server
```

Expected: compile failures for missing `ServerLifecycleState`, `wait_until_ready()`, `lifecycle_state()`, `is_ready()`, and `is_running()`.

- [ ] **Step 3: Add lifecycle state and readiness channel**

In `core/transport/c2-server/src/server.rs`, add imports near the existing imports:

```rust
use std::io::ErrorKind;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::time::Duration;
```

Add this enum above `pub struct Server`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerLifecycleState {
    Initialized,
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed(String),
}

impl ServerLifecycleState {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting | Self::Ready | Self::Stopping)
    }
}
```

Add this field to `Server`:

```rust
lifecycle_tx: watch::Sender<ServerLifecycleState>,
```

Initialize it in `Server::new()` immediately before `Ok(Self { ... })`:

```rust
let (lifecycle_tx, _lifecycle_rx) = watch::channel(ServerLifecycleState::Initialized);
```

Add the field inside `Ok(Self { ... })`:

```rust
lifecycle_tx,
```

- [ ] **Step 4: Implement lifecycle helper methods**

Add these methods inside `impl Server` after `socket_path()`:

```rust
fn set_lifecycle_state(&self, state: ServerLifecycleState) {
    let _ = self.lifecycle_tx.send(state);
}

pub fn lifecycle_state(&self) -> ServerLifecycleState {
    self.lifecycle_tx.subscribe().borrow().clone()
}

pub fn is_ready(&self) -> bool {
    self.lifecycle_state().is_ready()
}

pub fn is_running(&self) -> bool {
    self.lifecycle_state().is_running()
}

pub async fn wait_until_ready(&self, timeout: Duration) -> Result<(), ServerError> {
    let mut rx = self.lifecycle_tx.subscribe();
    let wait = async {
        loop {
            let state = rx.borrow().clone();
            match state {
                ServerLifecycleState::Ready => return Ok(()),
                ServerLifecycleState::Failed(message) => {
                    return Err(ServerError::Config(format!(
                        "server failed to start: {message}",
                    )));
                }
                ServerLifecycleState::Stopped => {
                    return Err(ServerError::Config(
                        "server stopped before becoming ready".to_string(),
                    ));
                }
                ServerLifecycleState::Initialized
                | ServerLifecycleState::Starting
                | ServerLifecycleState::Stopping => {}
            }
            rx.changed().await.map_err(|_| {
                ServerError::Config("server readiness channel closed".to_string())
            })?;
        }
    };

    tokio::time::timeout(timeout, wait).await.map_err(|_| {
        ServerError::Config(format!(
            "server did not become ready within {:.3}s",
            timeout.as_secs_f64(),
        ))
    })?
}
```

- [ ] **Step 5: Add active-socket stale-file guard**

Add this helper near `parse_socket_path()`:

```rust
fn remove_stale_socket_file(socket_path: &Path) -> Result<(), ServerError> {
    if !socket_path.exists() {
        return Ok(());
    }

    match StdUnixStream::connect(socket_path) {
        Ok(_) => Err(ServerError::Config(format!(
            "IPC socket {} already has an active listener",
            socket_path.display(),
        ))),
        Err(err) if matches!(err.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {
            let _ = std::fs::remove_file(socket_path);
            Ok(())
        }
        Err(err) => Err(ServerError::Io(err)),
    }
}
```

This deliberately refuses `PermissionDenied` and unknown connection errors rather than unlinking a possibly active socket owned by another process.

- [ ] **Step 6: Wire lifecycle state through `run()` and `shutdown()`**

Replace the first socket cleanup in `Server::run()`:

```rust
std::fs::create_dir_all(IPC_SOCK_DIR)?;
let _ = std::fs::remove_file(&self.socket_path);
let listener = UnixListener::bind(&self.socket_path)?;
```

with:

```rust
self.set_lifecycle_state(ServerLifecycleState::Starting);

let startup = async {
    std::fs::create_dir_all(IPC_SOCK_DIR)?;
    remove_stale_socket_file(&self.socket_path)?;
    let listener = UnixListener::bind(&self.socket_path)?;
    Ok::<UnixListener, ServerError>(listener)
}
.await;

let listener = match startup {
    Ok(listener) => listener,
    Err(err) => {
        self.set_lifecycle_state(ServerLifecycleState::Failed(err.to_string()));
        return Err(err);
    }
};
```

Immediately after the permissions block and before `info!(path = ...)`, add:

```rust
self.set_lifecycle_state(ServerLifecycleState::Ready);
```

Inside the shutdown branch before `break`, add:

```rust
self.set_lifecycle_state(ServerLifecycleState::Stopping);
```

After the final socket removal and before `Ok(())`, add:

```rust
self.set_lifecycle_state(ServerLifecycleState::Stopped);
```

In `Server::shutdown()`, add a state transition before sending shutdown:

```rust
if self.is_running() {
    self.set_lifecycle_state(ServerLifecycleState::Stopping);
}
let _ = self.shutdown_tx.send(true);
let _ = std::fs::remove_file(&self.socket_path);
```

- [ ] **Step 7: Export lifecycle state**

Modify `core/transport/c2-server/src/lib.rs`:

```rust
pub use server::{Server, ServerError, ServerLifecycleState};
```

- [ ] **Step 8: Run c2-server tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server wait_until_ready_times_out_before_start
cargo test --manifest-path core/Cargo.toml -p c2-server run_sets_ready_then_shutdown_sets_stopped
cargo test --manifest-path core/Cargo.toml -p c2-server active_socket_is_not_unlinked_by_second_server
cargo test --manifest-path core/Cargo.toml -p c2-server
```

Expected: all c2-server tests pass. The active-socket test must prove that a second server cannot unlink an active first server socket.

- [ ] **Step 9: Commit Task 1**

Run:

```bash
git add core/transport/c2-server/src/server.rs core/transport/c2-server/src/lib.rs

git commit -m "feat(server): own readiness lifecycle state"
```

Expected: commit contains Rust lifecycle state, active-socket guard, and tests.

---

## Task 2: Expose Native Readiness Through PyO3 `RustServer`

**Files:**
- Modify: `sdk/python/native/src/server_ffi.rs`
- Test: `sdk/python/tests/unit/test_native_server_readiness.py`

- [ ] **Step 1: Write failing Python FFI method tests**

Create `sdk/python/tests/unit/test_native_server_readiness.py`:

```python
from __future__ import annotations

import math

import pytest

from c_two import _native


def test_rust_server_exposes_native_readiness_api():
    assert hasattr(_native.RustServer, "start_and_wait")
    assert hasattr(_native.RustServer, "is_ready")
    assert hasattr(_native.RustServer, "is_running")


def test_start_and_wait_rejects_invalid_timeout(monkeypatch):
    # Construct through the public Python Server wrapper in implementation tests;
    # this low-level test only locks API validation shape after PyO3 exposes it.
    from c_two.transport import Server

    server = Server(bind_address="ipc://unit_bad_timeout_native")
    try:
        with pytest.raises((RuntimeError, ValueError), match="timeout"):
            server._rust_server.start_and_wait(math.nan)  # noqa: SLF001
    finally:
        try:
            server.shutdown()
        except Exception:
            pass
```

- [ ] **Step 2: Run test and verify RED**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_native_server_readiness.py -q --timeout=30
```

Expected: failure because `RustServer.start_and_wait` and `is_ready` do not exist yet.

- [ ] **Step 3: Implement `start_and_wait()` in `server_ffi.rs`**

Add imports near the top of `sdk/python/native/src/server_ffi.rs`:

```rust
use std::time::Duration;
use pyo3::exceptions::{PyTimeoutError, PyValueError};
```

If `PyValueError` is already imported from `pyo3::exceptions`, extend the existing import instead of adding a duplicate.

Add this private helper inside `impl PyServer`:

```rust
fn start_runtime_and_wait(&self, timeout: Duration) -> PyResult<()> {
    let server_for_run = Arc::clone(&self.inner);
    let server_for_wait = Arc::clone(&self.inner);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| PyRuntimeError::new_err(format!("failed to create runtime: {e}")))?;

    {
        let mut rt_guard = self.rt.lock();
        if rt_guard.is_some() {
            return Err(PyRuntimeError::new_err("server is already running"));
        }

        rt.spawn(async move {
            if let Err(e) = server_for_run.run().await {
                eprintln!("c2-server error: {e}");
            }
        });

        *rt_guard = Some(rt);
    }

    let readiness = {
        let rt_guard = self.rt.lock();
        let rt = rt_guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("server runtime missing after start"))?;
        rt.block_on(server_for_wait.wait_until_ready(timeout))
    };

    match readiness {
        Ok(()) => Ok(()),
        Err(err) => {
            self.inner.shutdown();
            let rt = self.rt.lock().take();
            if let Some(rt) = rt {
                rt.shutdown_background();
            }
            let message = err.to_string();
            if message.contains("did not become ready") {
                Err(PyTimeoutError::new_err(message))
            } else {
                Err(PyRuntimeError::new_err(message))
            }
        }
    }
}
```

- [ ] **Step 4: Expose `start_and_wait()` and update `start()`**

Replace the existing `start(&self, py)` body with a call to `start_and_wait(py, 5.0)`:

```rust
fn start(&self, py: Python<'_>) -> PyResult<()> {
    self.start_and_wait(py, 5.0)
}
```

Add this PyO3 method next to `start()`:

```rust
#[pyo3(signature = (timeout_seconds=5.0))]
fn start_and_wait(&self, py: Python<'_>, timeout_seconds: f64) -> PyResult<()> {
    if !timeout_seconds.is_finite() || timeout_seconds < 0.0 {
        return Err(PyValueError::new_err(
            "timeout_seconds must be a non-negative finite number",
        ));
    }
    let timeout = Duration::from_secs_f64(timeout_seconds);
    py.detach(|| self.start_runtime_and_wait(timeout))
}
```

Update `runtime_is_running()` in `impl PyServer` to project native lifecycle state:

```rust
pub(crate) fn runtime_is_running(&self) -> bool {
    self.inner.is_running()
}
```

Update getters:

```rust
#[getter]
fn is_running(&self) -> bool {
    self.inner.is_running()
}

#[getter]
fn is_ready(&self) -> bool {
    self.inner.is_ready()
}
```

- [ ] **Step 5: Run native build and FFI tests**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
uv sync --reinstall-package c-two
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_native_server_readiness.py -q --timeout=30
```

Expected: native crate checks, rebuild succeeds, and readiness API tests pass.

- [ ] **Step 6: Commit Task 2**

Run:

```bash
git add sdk/python/native/src/server_ffi.rs sdk/python/tests/unit/test_native_server_readiness.py

git commit -m "feat(python-native): expose server readiness wait"
```

Expected: commit contains PyO3 readiness API and tests.

---

## Task 3: Remove Python Socket Polling And `_started` Authority

**Files:**
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Modify: `sdk/python/tests/unit/test_sdk_boundary.py`
- Modify: `sdk/python/tests/unit/test_name_collision.py`

- [ ] **Step 1: Write failing SDK boundary test**

Append this test to `sdk/python/tests/unit/test_sdk_boundary.py`:

```python
def test_python_server_bridge_does_not_own_readiness_polling():
    import inspect
    from c_two.transport.server.native import NativeServerBridge

    source = inspect.getsource(NativeServerBridge)
    forbidden = [
        "os.path.exists",
        "while not os.path",
        "self._started",
        "_started =",
    ]
    offenders = [needle for needle in forbidden if needle in source]
    assert offenders == []

    start_source = inspect.getsource(NativeServerBridge.start)
    assert "start_and_wait" in start_source
```

- [ ] **Step 2: Run test and verify RED**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_sdk_boundary.py::test_python_server_bridge_does_not_own_readiness_polling -q --timeout=30
```

Expected: failure because `NativeServerBridge` still imports `os`, polls `os.path.exists`, and owns `_started`.

- [ ] **Step 3: Update `NativeServerBridge` lifecycle methods**

Modify `sdk/python/src/c_two/transport/server/native.py`:

1. Remove the unused import:

```python
import os
```

2. Remove this assignment from `__init__`:

```python
self._started = False
```

3. Replace `is_started()` with:

```python
def is_started(self) -> bool:
    return bool(getattr(self._rust_server, 'is_running', False))
```

4. Replace `start()` with:

```python
def start(self, timeout: float = 5.0) -> None:
    self._rust_server.start_and_wait(float(timeout))
```

5. Replace the shutdown block:

```python
if self._started:
    try:
        self._rust_server.shutdown()
    except Exception:
        logger.warning('Error shutting down RustServer', exc_info=True)
    self._started = False
```

with:

```python
if self.is_started():
    try:
        self._rust_server.shutdown()
    except Exception:
        logger.warning('Error shutting down RustServer', exc_info=True)
```

- [ ] **Step 4: Update object-construction fakes in `test_name_collision.py`**

Search:

```bash
rg -n "_started|class FakeRustServer|def is_started" sdk/python/tests/unit/test_name_collision.py
```

For each fake `NativeServerBridge` built with `object.__new__`, remove assignments like:

```python
bridge._started = False  # noqa: SLF001
```

Ensure each `FakeRustServer` used by `NativeServerBridge.is_started()` has an `is_running` property:

```python
class FakeRustServer:
    @property
    def is_running(self) -> bool:
        return False
```

If the fake represents a started server, return `True`.

- [ ] **Step 5: Run boundary and name-collision tests**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_sdk_boundary.py::test_python_server_bridge_does_not_own_readiness_polling \
  sdk/python/tests/unit/test_name_collision.py \
  -q --timeout=30
```

Expected: tests pass. No Python source should contain `_started` in `NativeServerBridge`.

- [ ] **Step 6: Commit Task 3**

Run:

```bash
git add sdk/python/src/c_two/transport/server/native.py sdk/python/tests/unit/test_sdk_boundary.py sdk/python/tests/unit/test_name_collision.py

git commit -m "refactor(python): delegate server readiness to native"
```

Expected: commit removes Python readiness polling and duplicate `_started` authority.

---

## Task 4: Use Native Running State In Runtime Shutdown Outcomes

**Files:**
- Modify: `core/runtime/c2-runtime/src/session.rs`
- Test: Rust unit tests in `core/runtime/c2-runtime/src/session.rs` or Python tests in `sdk/python/tests/unit/test_runtime_session.py`

- [ ] **Step 1: Write failing source boundary test against socket-path inference**

Append this test to `sdk/python/tests/unit/test_sdk_boundary.py`:

```python
def test_runtime_session_does_not_infer_started_from_socket_file():
    from pathlib import Path

    root = Path(__file__).resolve().parents[4]
    session_rs = root / "core" / "runtime" / "c2-runtime" / "src" / "session.rs"
    source = session_rs.read_text(encoding="utf-8")
    assert "socket_path().exists()" not in source
```

- [ ] **Step 2: Run test and verify RED**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_sdk_boundary.py::test_runtime_session_does_not_infer_started_from_socket_file -q --timeout=30
```

Expected: failure because `RuntimeSession::shutdown()` still uses `server.socket_path().exists()`.

- [ ] **Step 3: Add native state method to `RuntimeSession::shutdown()` path**

Modify `core/runtime/c2-runtime/src/session.rs` in `shutdown()`:

Replace:

```rust
let server_was_started = server
    .map(|server| server.socket_path().exists())
    .unwrap_or(false);
```

with:

```rust
let server_was_started = server
    .map(|server| server.is_running())
    .unwrap_or(false);
```

This works because Task 1 added `Server::is_running()` to `c2-server`.

- [ ] **Step 4: Run runtime and boundary tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-runtime
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_sdk_boundary.py::test_runtime_session_does_not_infer_started_from_socket_file -q --timeout=30
```

Expected: tests pass. Shutdown outcomes now use native lifecycle state.

- [ ] **Step 5: Commit Task 4**

Run:

```bash
git add core/runtime/c2-runtime/src/session.rs sdk/python/tests/unit/test_sdk_boundary.py

git commit -m "refactor(runtime): use native server lifecycle state"
```

Expected: commit removes socket-file inference from runtime shutdown.

---

## Task 5: Add Direct IPC Readiness And Active-Socket Integration Coverage

**Files:**
- Modify: `sdk/python/tests/integration/test_direct_ipc_control.py`
- Modify: `sdk/python/tests/integration/test_server.py`

- [ ] **Step 1: Add direct IPC ready-after-start test**

Append this test to `sdk/python/tests/integration/test_direct_ipc_control.py`:

```python
def test_server_start_returns_only_after_direct_ipc_ready(monkeypatch):
    monkeypatch.delenv('C2_RELAY_ANCHOR_ADDRESS', raising=False)
    address = f'ipc://{_unique_region("ready")}'
    server = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    try:
        server.start(timeout=5.0)
        assert ping(address, timeout=0.5) is True
    finally:
        server.shutdown()
```

- [ ] **Step 2: Add bad-relay direct IPC readiness test**

Append this test to `sdk/python/tests/integration/test_direct_ipc_control.py`:

```python
def test_server_start_readiness_ignores_bad_relay_env(monkeypatch):
    monkeypatch.setenv('C2_RELAY_ANCHOR_ADDRESS', 'http://127.0.0.1:9')
    address = f'ipc://{_unique_region("ready_bad_relay")}'
    server = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    try:
        server.start(timeout=5.0)
        assert ping(address, timeout=0.5) is True
    finally:
        server.shutdown()
```

- [ ] **Step 3: Add active-socket rejection integration test**

Append this test to `sdk/python/tests/integration/test_direct_ipc_control.py`:

```python
def test_starting_second_server_does_not_unlink_active_socket(monkeypatch):
    monkeypatch.delenv('C2_RELAY_ANCHOR_ADDRESS', raising=False)
    address = f'ipc://{_unique_region("active")}'
    first = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    second = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello2',
    )
    try:
        first.start(timeout=5.0)
        assert ping(address, timeout=0.5) is True
        with pytest.raises(RuntimeError, match='active listener|address already in use|failed to start'):
            second.start(timeout=1.0)
        assert ping(address, timeout=0.5) is True
    finally:
        try:
            second.shutdown()
        except Exception:
            pass
        first.shutdown()
```

- [ ] **Step 4: Remove redundant helper waits from new readiness-sensitive tests only**

Do not bulk-edit every integration fixture in this task. Only newly added tests should rely directly on `server.start(timeout=...)`. Existing tests may keep `_wait_for_server()` until a later cleanup pass; this avoids unnecessary churn.

- [ ] **Step 5: Run direct IPC integration tests**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_direct_ipc_control.py -q --timeout=30 -rs
```

Expected: direct IPC tests pass, including bad relay env and active-socket rejection.

- [ ] **Step 6: Commit Task 5**

Run:

```bash
git add sdk/python/tests/integration/test_direct_ipc_control.py sdk/python/tests/integration/test_server.py

git commit -m "test: cover native server readiness semantics"
```

Expected: commit contains integration coverage for native readiness and active-socket safety.

---

## Task 6: Update Docs And Guard Against Zombie Readiness Code

**Files:**
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify: `AGENTS.md`
- Test: source audits via `rg`

- [ ] **Step 1: Update issue6 status in thin-sdk boundary doc**

Replace the `### P2. Server route slot lifecycle and readiness` section in `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md` with:

```markdown
### P2. Server route slot lifecycle and readiness

**Implemented ownership**

Status: implemented on `dev-feature` by
`docs/plans/2026-05-07-server-readiness-rust-authority.md`.

Rust `c2-server` owns IPC server lifecycle state and readiness notification.
Native startup exposes `start_and_wait(timeout)` and state projection through
`is_running` / `is_ready`. Python `NativeServerBridge` keeps CRM slot objects
and Python callback dispatch, but it no longer polls socket files, maintains a
separate `_started` flag, or infers readiness from filesystem state.

**Verified coverage**

- `Server.start(timeout=...)` returns only after direct IPC ping/connect can
  succeed.
- Starting a second server on an active IPC socket fails without unlinking the
  first server's socket path.
- Runtime shutdown outcomes use native lifecycle state instead of
  `socket_path().exists()`.
- Direct IPC readiness remains relay-independent, including with a bad relay
  environment variable.
- Boundary tests prevent Python socket polling and `_started` lifecycle state
  from being reintroduced.
```

- [ ] **Step 2: Update AGENTS.md transport guidance**

In `AGENTS.md`, under the Transport Layer or Rust Native Layer guidance, add:

```markdown
Server lifecycle/readiness belongs to Rust. Python `NativeServerBridge` may
expose `start()`, `is_started()`, and shutdown facades, but must delegate
readiness to native `RustServer.start_and_wait()` and native state projection.
Do not reintroduce Python socket-file polling, `os.path.exists(socket_path)`
readiness checks, or Python-owned `_started` authority.
```

- [ ] **Step 3: Run stale-code audit**

Run:

```bash
rg -n "_started|os\.path\.exists\(|while not os\.path|socket_path\(\)\.exists\(\)" \
  sdk/python/src/c_two/transport/server/native.py \
  sdk/python/src/c_two \
  sdk/python/native/src \
  core/runtime/c2-runtime/src \
  docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md \
  AGENTS.md -S
```

Expected: no matches in active Python/native/runtime source. Matches in docs are acceptable only when explicitly saying not to reintroduce the old pattern.

- [ ] **Step 4: Commit Task 6**

Run:

```bash
git add docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md AGENTS.md

git commit -m "docs: document rust-owned server readiness"
```

Expected: active guidance documents point to the implemented issue6 ownership boundary.

---

## Task 7: Final Verification And Review

**Files:**
- Verify: Rust workspace
- Verify: Python native crate
- Verify: focused and full Python tests
- Verify: stale symbol audits

- [ ] **Step 1: Run Rust tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server
cargo test --manifest-path core/Cargo.toml -p c2-runtime
cargo test --manifest-path core/Cargo.toml --workspace
```

Expected: all Rust tests pass.

- [ ] **Step 2: Run native build and reinstall Python package**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
uv sync --reinstall-package c-two
```

Expected: native crate checks and package rebuild succeeds.

- [ ] **Step 3: Run focused Python tests**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_native_server_readiness.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/unit/test_name_collision.py \
  sdk/python/tests/unit/test_runtime_session.py \
  sdk/python/tests/integration/test_direct_ipc_control.py \
  sdk/python/tests/integration/test_server.py \
  sdk/python/tests/integration/test_crm_proxy.py \
  -q --timeout=30 -rs
```

Expected: focused tests pass.

- [ ] **Step 4: Run full Python tests**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: full Python SDK suite passes. Existing optional example skips may remain if optional dependencies are not installed.

- [ ] **Step 5: Run repo tests and whitespace check**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest tests/ -q --timeout=30
git diff --check
```

Expected: repo tests pass and whitespace check has no output.

- [ ] **Step 6: Audit readiness ownership boundary**

Run:

```bash
rg -n "_started|os\.path\.exists\(|while not os\.path|socket_path\(\)\.exists\(\)" \
  sdk/python/src/c_two \
  sdk/python/native/src \
  core/runtime/c2-runtime/src -S
```

Expected: no matches in active source. If a match appears, remove it unless it is in a test explicitly asserting absence.

- [ ] **Step 7: Audit relay independence**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS=http://127.0.0.1:9 uv run pytest \
  sdk/python/tests/integration/test_direct_ipc_control.py::test_server_start_readiness_ignores_bad_relay_env \
  sdk/python/tests/integration/test_direct_ipc_control.py::test_ping_ignores_bad_relay_env \
  -q --timeout=30 -rs
```

Expected: direct IPC readiness and ping work with a bad relay environment.

- [ ] **Step 8: Final strict review checklist**

Verify each item against code and tests:

- [ ] Rust `c2-server` owns lifecycle state and readiness notification.
- [ ] Python `NativeServerBridge` does not import `os` for readiness and does not poll socket files.
- [ ] Python `NativeServerBridge` does not maintain `_started`.
- [ ] `RustServer.start()` and `NativeServerBridge.start()` wait for native readiness.
- [ ] Runtime shutdown outcome uses native state, not filesystem existence.
- [ ] Active-socket startup does not unlink another running server's socket path.
- [ ] Explicit direct IPC readiness is relay-independent.
- [ ] No compatibility shim or zombie readiness path remains.

- [ ] **Step 9: Commit verification fixes if needed**

If final verification required fixes, commit them:

```bash
git add <changed-files>

git commit -m "fix: stabilize server readiness lifecycle"
```

Expected: branch is clean after final fixes.

- [ ] **Step 10: Final status check**

Run:

```bash
git status --short --branch
```

Expected: branch is clean and ahead of upstream by the issue6 implementation commits.
