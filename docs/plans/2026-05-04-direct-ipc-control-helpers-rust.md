# Direct IPC Control Helpers Rust Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented on `dev-feature`. Rust `c2-ipc` owns the direct IPC
socket-path, ping, and shutdown control helpers; Python `client/util.py` is now
a thin native-call facade.

**Goal:** Move direct IPC `ping()` and `shutdown()` control-plane helpers from Python raw-socket/frame construction into Rust `c2-ipc`, with Python left as a thin native-call facade.

**Architecture:** Rust `c2-ipc` becomes the language-neutral owner for IPC address validation, UDS socket-path derivation, signal frame encoding, timeout handling, and response validation. The Python SDK keeps the public `c_two.transport.client.util.ping()` and `shutdown()` functions for API continuity, but those functions call PyO3 wrappers instead of constructing frames or opening sockets themselves. Relay remains out of this path: all helpers operate on explicit `ipc://` addresses and never consult relay configuration.

**Tech Stack:** Rust `core/transport/c2-ipc`, canonical `core/protocol/c2-wire` frame/signal constants, PyO3 bindings in `sdk/python/native`, Python SDK facade in `sdk/python/src/c_two/transport/client/util.py`, pytest integration/unit tests, Cargo unit tests.

---

## 0.x Constraint

C-Two is in the 0.x line. Do not keep Python raw-socket fallback behavior after the Rust helper path works. Remove copied Python frame constants and socket-path derivation instead of leaving parallel implementations that can drift from `c2-wire` or `c2-ipc`.

## Non-Negotiable Runtime Constraints

1. **Direct IPC remains relay-independent.** `ping("ipc://...")` and `shutdown("ipc://...")` must use only direct UDS IPC. They must not consult `C2_RELAY_ADDRESS`, relay route resolution, `c2-http`, or relay control clients.
2. **Canonical wire constants only.** Signal bytes and frame flags must come from `c2-wire`; do not duplicate numeric constants in Python or in a second Rust table.
3. **Canonical address validation only.** IPC region validation and socket-path derivation must be owned by Rust. Python tests may keep a private helper only if it delegates to native validation and uses the native resolved path.
4. **No data-plane changes.** This work touches control probes only. Do not change CRM call, SHM, buddy, chunked, scheduler, or relay-aware data paths.
5. **Timeouts are bounded.** Failed probes must return or raise promptly according to the provided timeout. Do not add indefinite blocking reads or writes.

## Current Problem

At the start of this plan,
`sdk/python/src/c_two/transport/client/util.py` did all of the following in Python:

- derives `/tmp/c_two_ipc/{region}.sock` from `ipc://{region}`;
- validates IPC region IDs by calling native validation but still owns path construction;
- copies frame and signal constants such as `FLAG_SIGNAL`, PING, PONG, and SHUTDOWN_CLIENT;
- manually opens a Unix socket, writes a signal frame, reads a reply frame, and interprets the payload.

These are language-neutral IPC/wire control-plane mechanics. Future SDKs should not reimplement them independently.

## File Responsibility Map

- Modify `core/transport/c2-ipc/src/control.rs`: new Rust-owned direct IPC probe/control helpers. This module owns address validation, socket-path derivation, signal frame send/receive, `ping`, and `shutdown`.
- Modify `core/transport/c2-ipc/src/lib.rs`: export the new control helper functions and, only if useful for tests/native bindings, socket-path resolution.
- Modify `core/transport/c2-ipc/src/client.rs`: reuse the new address/path helper to eliminate duplicate client-side socket path logic while keeping `IpcClient::connect()` behavior identical.
- Modify `sdk/python/native/src/ipc_control_ffi.rs`: new PyO3 wrappers around `c2_ipc::ping`, `c2_ipc::shutdown`, and a test/support socket-path resolver.
- Modify `sdk/python/native/src/lib.rs`: register `ipc_control_ffi` in `c_two._native`.
- Modify `sdk/python/src/c_two/transport/client/util.py`: replace raw socket/frame code with thin calls to native helpers; keep public function names and return contracts.
- Modify `sdk/python/tests/unit/test_ipc_address_validation.py`: update tests to verify the Python facade delegates to native validation/path resolution rather than owning path construction.
- Modify `sdk/python/tests/integration/test_server.py`: keep existing `ping`/`shutdown` tests and add no-relay/bad-relay assertions if not covered elsewhere.
- Modify or create `sdk/python/tests/integration/test_direct_ipc_control.py`: focused integration coverage for native-backed direct IPC `ping()`/`shutdown()` and invalid addresses.
- Modify `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: after implementation, mark the direct IPC control helper P1 as implemented.
- Modify `AGENTS.md`: update only if durable guidance about direct IPC control-plane ownership changes.

## Design

### Rust helper API

Add `core/transport/c2-ipc/src/control.rs` with a small synchronous public API:

```rust
use std::path::PathBuf;
use std::time::Duration;

use crate::client::IpcError;

pub fn socket_path_from_ipc_address(address: &str) -> Result<PathBuf, IpcError>;
pub fn ping(address: &str, timeout: Duration) -> Result<bool, IpcError>;
pub fn shutdown(address: &str, timeout: Duration) -> Result<bool, IpcError>;
```

Behavior:

- `socket_path_from_ipc_address()` validates `ipc://` addresses through `c2_config::validate_ipc_region_id()` and derives the same `/tmp/c_two_ipc/{region}.sock` path used by `c2-server` and `IpcClient`.
- `ping()` returns `Ok(false)` when the socket path does not exist, connect/read/write times out, the peer closes, or the response is not a valid PONG signal. It returns `Err(IpcError::Config(_))` for invalid IPC addresses.
- `shutdown()` returns `Ok(true)` when the socket path does not exist, matching current Python idempotent behavior. When the socket exists, it sends `SHUTDOWN_CLIENT_BYTES`, waits for `SHUTDOWN_ACK_BYTES`, and returns `Ok(true)` only for that valid ACK. Timeout/closed/invalid response returns `Ok(false)`. Invalid IPC addresses return `Err(IpcError::Config(_))`.
- The helper uses `std::os::unix::net::UnixStream` plus `set_read_timeout()`/`set_write_timeout()` for a small synchronous control path. This avoids creating a Tokio runtime for one-shot probes and keeps PyO3 wrappers simple.

### Python facade API

Keep the existing public functions:

```python
def ping(server_address: str, timeout: float = 0.5) -> bool: ...
def shutdown(server_address: str, timeout: float = 0.5) -> bool: ...
```

Facade behavior:

- `ping()` returns `False` on invalid addresses and unavailable servers, preserving current public behavior.
- `shutdown()` returns `False` on invalid addresses and failed communication, and `True` when the socket is already absent, preserving current public behavior.
- `_socket_path_from_address()` remains only for tests/fixtures that inspect socket paths. It delegates to native `ipc_socket_path()` and raises `ValueError` for invalid addresses. It must not construct `/tmp/c_two_ipc/...` in Python.

### PyO3 wrappers

Expose flat native functions from `c_two._native`:

```rust
#[pyfunction]
fn ipc_socket_path(address: &str) -> PyResult<String>;

#[pyfunction]
#[pyo3(signature = (address, timeout_seconds=0.5))]
fn ipc_ping(address: &str, timeout_seconds: f64) -> PyResult<bool>;

#[pyfunction]
#[pyo3(signature = (address, timeout_seconds=0.5))]
fn ipc_shutdown(address: &str, timeout_seconds: f64) -> PyResult<bool>;
```

Wrapper mapping:

- `IpcError::Config(_)` maps to `PyValueError` for `ipc_socket_path()` and to `False` through the Python facade for `ping()`/`shutdown()`.
- Other Rust errors from the helper should normally be converted to `Ok(false)` by the Rust helper. If any unexpected `Err` remains, map it to `PyRuntimeError` so tests catch it.
- Reject negative or non-finite timeout values with `PyValueError`. A zero timeout is allowed but may immediately return `False` if the socket operation cannot complete.

## Implementation Tasks

### Task 1: Add failing Rust unit tests for direct IPC socket-path ownership

**Files:**
- Create: `core/transport/c2-ipc/src/control.rs`
- Modify: `core/transport/c2-ipc/src/lib.rs`

- [ ] **Step 1: Create `control.rs` with tests first**

Create `core/transport/c2-ipc/src/control.rs` with tests that reference the intended Rust API before it exists:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_rejects_invalid_ipc_addresses() {
        for address in [
            "tcp://not-ipc",
            "ipc://",
            "ipc://../escape",
            "ipc://bad/name",
            "ipc://bad\\name",
            "ipc://.",
            "ipc://..",
            "ipc:// leading",
            "ipc://trailing ",
            "ipc://bad\nname",
        ] {
            let err = socket_path_from_ipc_address(address)
                .expect_err("invalid address must fail");
            assert!(matches!(err, IpcError::Config(_)), "{address}: {err:?}");
        }
    }

    #[test]
    fn socket_path_accepts_plain_region() {
        let path = socket_path_from_ipc_address("ipc://unit-server").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/c_two_ipc/unit-server.sock"));
    }
}
```

- [ ] **Step 2: Export the module**

In `core/transport/c2-ipc/src/lib.rs`, add:

```rust
pub mod control;
```

Do not export `socket_path_from_ipc_address`, `ping`, or `shutdown` from `lib.rs` until the corresponding functions exist.

- [ ] **Step 3: Run the failing Rust tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc control::tests::socket_path -q
```

Expected: compilation fails with `cannot find function socket_path_from_ipc_address`, proving the new tests are active before implementation.

### Task 2: Implement canonical Rust address validation and socket-path derivation

**Files:**
- Modify: `core/transport/c2-ipc/src/control.rs`
- Modify: `core/transport/c2-ipc/src/lib.rs`
- Modify: `core/transport/c2-ipc/src/client.rs`

- [ ] **Step 1: Replace the placeholder path function**

In `core/transport/c2-ipc/src/control.rs`, replace `socket_path_from_ipc_address()` with:

```rust
const IPC_SOCK_DIR: &str = "/tmp/c_two_ipc";

pub fn socket_path_from_ipc_address(address: &str) -> Result<PathBuf, IpcError> {
    let region = address
        .strip_prefix("ipc://")
        .ok_or_else(|| IpcError::Config(format!("invalid IPC address: {address}")))?;
    c2_config::validate_ipc_region_id(region).map_err(IpcError::Config)?;
    Ok(PathBuf::from(IPC_SOCK_DIR).join(format!("{region}.sock")))
}
```

- [ ] **Step 2: Export the socket-path helper**

In `core/transport/c2-ipc/src/lib.rs`, add the public re-export below `pub mod control;`:

```rust
pub use control::socket_path_from_ipc_address;
```

- [ ] **Step 3: Run Rust path tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc control::tests::socket_path -q
```

Expected: the two socket path tests pass.

- [ ] **Step 4: Reuse the helper from `client.rs`**

Replace the existing private `socket_path_from_address()` / `region_from_address()` / `validate_region_id()` block in `core/transport/c2-ipc/src/client.rs` with a wrapper around the new helper:

```rust
fn socket_path_from_address(address: &str) -> (PathBuf, Option<String>) {
    match crate::control::socket_path_from_ipc_address(address) {
        Ok(path) => (path, None),
        Err(error) => (
            PathBuf::from("/tmp/c_two_ipc").join("invalid.sock"),
            Some(error.to_string()),
        ),
    }
}
```

Do not change `IpcClient::new()` or `IpcClient::connect()` behavior in this task. Existing invalid-address tests in `client.rs` must continue to produce `IpcError::Config(_)` before attempting UDS connect.

- [ ] **Step 5: Run client invalid-address tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc client::tests::invalid -q
```

Expected: invalid IPC address tests still pass. If the exact filter matches no tests, run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc rejects_invalid_ipc_addresses_before_connecting -q
```

Expected: pass.

### Task 3: Add failing Rust unit tests for absent-socket probe behavior

**Files:**
- Modify: `core/transport/c2-ipc/src/control.rs`

- [ ] **Step 1: Add tests for absent socket behavior**

Append these tests to the `#[cfg(test)]` module in `control.rs` before implementing `ping()` and `shutdown()`:

```rust
use std::time::Duration;

#[test]
fn ping_absent_socket_returns_false() {
    let address = "ipc://unit-control-absent";
    let path = socket_path_from_ipc_address(address).unwrap();
    let _ = std::fs::remove_file(path);
    let result = ping(address, Duration::from_millis(10)).unwrap();
    assert!(!result);
}

#[test]
fn shutdown_absent_socket_returns_true() {
    let address = "ipc://unit-control-absent-shutdown";
    let path = socket_path_from_ipc_address(address).unwrap();
    let _ = std::fs::remove_file(path);
    let result = shutdown(address, Duration::from_millis(10)).unwrap();
    assert!(result);
}
```

- [ ] **Step 2: Run the failing Rust control tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc control::tests -q
```

Expected: compilation fails with `cannot find function ping` and `cannot find function shutdown`, proving the new tests are active before implementation.

### Task 4: Implement Rust direct IPC signal probe helpers

**Files:**
- Modify: `core/transport/c2-ipc/src/control.rs`

- [ ] **Step 1: Add frame read/write helpers**

Add these imports and helpers to `control.rs`:

```rust
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use c2_wire::flags::{FLAG_RESPONSE, FLAG_SIGNAL};
use c2_wire::frame::{self, HEADER_SIZE};
use c2_wire::msg_type::{PING_BYTES, PONG_BYTES, SHUTDOWN_ACK_BYTES, SHUTDOWN_CLIENT_BYTES};

fn recv_exact(stream: &mut UnixStream, len: usize) -> Result<Vec<u8>, IpcError> {
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).map_err(IpcError::Io)?;
    Ok(buf)
}

fn send_signal_and_read_reply(
    address: &str,
    timeout: Duration,
    request_id: u64,
    signal: &[u8],
) -> Result<Option<(u32, Vec<u8>)>, IpcError> {
    let socket_path = socket_path_from_ipc_address(address)?;
    if !socket_path.exists() {
        return Ok(None);
    }

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(stream) => stream,
        Err(err) => {
            if matches!(err.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) {
                return Ok(None);
            }
            return Err(IpcError::Io(err));
        }
    };
    stream.set_read_timeout(Some(timeout)).map_err(IpcError::Io)?;
    stream.set_write_timeout(Some(timeout)).map_err(IpcError::Io)?;

    let frame_bytes = frame::encode_frame(request_id, FLAG_SIGNAL, signal);
    stream.write_all(&frame_bytes).map_err(IpcError::Io)?;

    let header = match recv_exact(&mut stream, HEADER_SIZE) {
        Ok(header) => header,
        Err(IpcError::Io(err))
            if matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
            ) => return Ok(None),
        Err(err) => return Err(err),
    };

    let (total_len, body_rest) = match frame::decode_total_len(&header) {
        Ok(value) => value,
        Err(err) => return Err(IpcError::Decode(err)),
    };
    let (frame_header, _) = frame::decode_frame_body(body_rest, total_len)?;
    let payload_len = frame_header.payload_len();
    let payload = if payload_len == 0 {
        Vec::new()
    } else {
        match recv_exact(&mut stream, payload_len) {
            Ok(payload) => payload,
            Err(IpcError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                ) => return Ok(None),
            Err(err) => return Err(err),
        }
    };

    Ok(Some((frame_header.flags, payload)))
}

fn is_signal_reply(flags: u32, payload: &[u8], expected: &[u8]) -> bool {
    (flags & FLAG_SIGNAL != 0) && (flags & FLAG_RESPONSE != 0) && payload == expected
}
```

- [ ] **Step 2: Replace `ping()` and `shutdown()` placeholders**

Replace the placeholder functions with:

```rust
pub fn ping(address: &str, timeout: Duration) -> Result<bool, IpcError> {
    match send_signal_and_read_reply(address, timeout, 0, &PING_BYTES)? {
        Some((flags, payload)) => Ok(is_signal_reply(flags, &payload, &PONG_BYTES)),
        None => Ok(false),
    }
}

pub fn shutdown(address: &str, timeout: Duration) -> Result<bool, IpcError> {
    let socket_path = socket_path_from_ipc_address(address)?;
    if !socket_path.exists() {
        return Ok(true);
    }
    match send_signal_and_read_reply(address, timeout, 0, &SHUTDOWN_CLIENT_BYTES)? {
        Some((flags, payload)) => Ok(is_signal_reply(flags, &payload, &SHUTDOWN_ACK_BYTES)),
        None => Ok(false),
    }
}
```

- [ ] **Step 3: Add unit tests for absent socket behavior**

Append to the `#[cfg(test)]` module:

```rust
#[test]
fn ping_absent_socket_returns_false() {
    let address = "ipc://unit-control-absent";
    let path = socket_path_from_ipc_address(address).unwrap();
    let _ = std::fs::remove_file(path);
    let result = ping(address, Duration::from_millis(10)).unwrap();
    assert!(!result);
}

#[test]
fn shutdown_absent_socket_returns_true() {
    let address = "ipc://unit-control-absent-shutdown";
    let path = socket_path_from_ipc_address(address).unwrap();
    let _ = std::fs::remove_file(path);
    let result = shutdown(address, Duration::from_millis(10)).unwrap();
    assert!(result);
}
```

- [ ] **Step 4: Run Rust control tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc control::tests -q
```

Expected: all control module unit tests pass.

### Task 5: Add PyO3 wrappers for Rust IPC control helpers

**Files:**
- Create: `sdk/python/native/src/ipc_control_ffi.rs`
- Modify: `sdk/python/native/src/lib.rs`

- [ ] **Step 1: Create native wrapper module**

Create `sdk/python/native/src/ipc_control_ffi.rs`:

```rust
//! PyO3 bindings for direct IPC control helpers from `c2-ipc`.

use std::time::Duration;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

fn timeout_from_seconds(timeout_seconds: f64) -> PyResult<Duration> {
    if !timeout_seconds.is_finite() || timeout_seconds < 0.0 {
        return Err(PyValueError::new_err(
            "timeout_seconds must be a non-negative finite number",
        ));
    }
    Ok(Duration::from_secs_f64(timeout_seconds))
}

#[pyfunction]
fn ipc_socket_path(address: &str) -> PyResult<String> {
    let path = c2_ipc::socket_path_from_ipc_address(address)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
}

#[pyfunction]
#[pyo3(signature = (address, timeout_seconds=0.5))]
fn ipc_ping(py: Python<'_>, address: &str, timeout_seconds: f64) -> PyResult<bool> {
    let timeout = timeout_from_seconds(timeout_seconds)?;
    let address = address.to_string();
    py.detach(move || {
        c2_ipc::ping(&address, timeout).map_err(|e| PyRuntimeError::new_err(e.to_string()))
    })
}

#[pyfunction]
#[pyo3(signature = (address, timeout_seconds=0.5))]
fn ipc_shutdown(py: Python<'_>, address: &str, timeout_seconds: f64) -> PyResult<bool> {
    let timeout = timeout_from_seconds(timeout_seconds)?;
    let address = address.to_string();
    py.detach(move || {
        c2_ipc::shutdown(&address, timeout).map_err(|e| PyRuntimeError::new_err(e.to_string()))
    })
}

pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(ipc_socket_path, m)?)?;
    m.add_function(wrap_pyfunction!(ipc_ping, m)?)?;
    m.add_function(wrap_pyfunction!(ipc_shutdown, m)?)?;
    Ok(())
}
```

- [ ] **Step 2: Register the module in native lib**

In `sdk/python/native/src/lib.rs`, add the module declaration near the other native modules:

```rust
#[cfg(feature = "python")]
mod ipc_control_ffi;
```

Then register it in `c2_native()` after `client_ffi::register_module(m)?;`:

```rust
ipc_control_ffi::register_module(m)?;
```

- [ ] **Step 3: Build the native crate**

Run:

```bash
cargo test --manifest-path sdk/python/native/Cargo.toml --lib
```

Expected: native crate compiles successfully.

### Task 6: Replace Python raw-socket implementation with thin native facade

**Files:**
- Modify: `sdk/python/src/c_two/transport/client/util.py`
- Modify: `sdk/python/tests/unit/test_ipc_address_validation.py`

- [ ] **Step 1: Replace `client/util.py` contents**

Replace `sdk/python/src/c_two/transport/client/util.py` with:

```python
"""IPC utility functions backed by Rust c2-ipc control helpers."""
from __future__ import annotations


def _socket_path_from_address(server_address: str) -> str:
    from c_two._native import ipc_socket_path

    return ipc_socket_path(server_address)


def ping(server_address: str, timeout: float = 0.5) -> bool:
    """Ping a direct IPC server to check whether it is alive.

    Invalid addresses and unavailable servers return ``False`` for backwards
    compatibility with the previous probe helper.
    """
    from c_two._native import ipc_ping

    try:
        return bool(ipc_ping(server_address, float(timeout)))
    except ValueError:
        return False


def shutdown(server_address: str, timeout: float = 0.5) -> bool:
    """Send a direct IPC shutdown signal to a server.

    Invalid addresses and failed communication return ``False``. A missing
    socket returns ``True`` because shutdown is idempotent for an already-stopped
    direct IPC server.
    """
    from c_two._native import ipc_shutdown

    try:
        return bool(ipc_shutdown(server_address, float(timeout)))
    except ValueError:
        return False
```

Do not retain imports of `os`, `socket`, or `struct`, and do not retain copied frame/signal constants.

- [ ] **Step 2: Update unit tests to assert native delegation**

In `sdk/python/tests/unit/test_ipc_address_validation.py`, replace the monkeypatch test with native-function delegation tests:

```python
def test_client_util_uses_native_socket_path(monkeypatch):
    calls = []

    def fake_socket_path(address: str) -> str:
        calls.append(address)
        return '/tmp/native.sock'

    import c_two._native as native

    monkeypatch.setattr(native, 'ipc_socket_path', fake_socket_path)

    assert util._socket_path_from_address('ipc://unit-server') == '/tmp/native.sock'
    assert calls == ['ipc://unit-server']


def test_ping_invalid_address_returns_false():
    assert util.ping('tcp://not-ipc') is False


def test_shutdown_invalid_address_returns_false():
    assert util.shutdown('tcp://not-ipc') is False
```

Keep the existing invalid-address parametrized test for `_socket_path_from_address()` because it now verifies the native wrapper raises `ValueError`.

- [ ] **Step 3: Rebuild native extension and run unit tests**

Run:

```bash
uv sync --reinstall-package c-two
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/unit/test_ipc_address_validation.py -q --timeout=30
```

Expected: all IPC address validation tests pass.

### Task 7: Add focused direct IPC control integration tests

**Files:**
- Create: `sdk/python/tests/integration/test_direct_ipc_control.py`

- [ ] **Step 1: Create the integration test file**

Create `sdk/python/tests/integration/test_direct_ipc_control.py`:

```python
"""Direct IPC control helpers are backed by Rust c2-ipc and do not use relay."""
from __future__ import annotations

import os
import time
import uuid

import pytest

from c_two.transport import Server
from c_two.transport.client.util import ping, shutdown

from tests.fixtures.hello import HelloImpl
from tests.fixtures.ihello import Hello


def _unique_region(prefix: str = 'direct_ctl') -> str:
    return f'{prefix}_{os.getpid()}_{uuid.uuid4().hex[:12]}'


def _wait_for_ping(address: str, timeout: float = 5.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if ping(address, timeout=0.2):
            return
        time.sleep(0.05)
    raise TimeoutError(f'{address} did not respond to ping')


@pytest.fixture
def direct_server(monkeypatch):
    monkeypatch.delenv('C2_RELAY_ADDRESS', raising=False)
    address = f'ipc://{_unique_region()}'
    server = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    server.start()
    _wait_for_ping(address)
    yield address, server
    server.shutdown()


def test_ping_returns_true_against_direct_ipc_without_relay(direct_server):
    address, _server = direct_server
    assert ping(address, timeout=0.5) is True


def test_ping_ignores_bad_relay_env(monkeypatch, direct_server):
    address, _server = direct_server
    monkeypatch.setenv('C2_RELAY_ADDRESS', 'http://127.0.0.1:9')
    assert ping(address, timeout=0.5) is True


def test_shutdown_stops_direct_ipc_without_relay(monkeypatch):
    monkeypatch.delenv('C2_RELAY_ADDRESS', raising=False)
    address = f'ipc://{_unique_region("shutdown")}'
    server = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    server.start()
    _wait_for_ping(address)

    assert shutdown(address, timeout=0.5) is True

    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        if not ping(address, timeout=0.2):
            break
        time.sleep(0.05)
    else:
        pytest.fail('server still responds to ping after shutdown signal')

    server.shutdown()


def test_control_helpers_reject_invalid_addresses():
    assert ping('tcp://not-ipc', timeout=0.01) is False
    assert shutdown('tcp://not-ipc', timeout=0.01) is False
```

- [ ] **Step 2: Run focused integration tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/integration/test_direct_ipc_control.py -q --timeout=30
```

Expected: all direct IPC control tests pass.

- [ ] **Step 3: Run existing server tests that use `ping()` and `shutdown()`**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/integration/test_server.py sdk/python/tests/integration/test_heartbeat.py -q --timeout=30
```

Expected: existing server and heartbeat tests pass with the native-backed facade.

### Task 8: Update thin-sdk boundary doc after implementation

**Files:**
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify: `AGENTS.md` only if durable architecture guidance changes

- [ ] **Step 1: Mark the direct IPC helper P1 as implemented**

After Tasks 1-6 pass, replace the `P1. Direct IPC probe/control helpers` section's `Current Python ownership` and `Implementation sketch` wording with this implemented-ownership wording:

```markdown
**Implemented ownership**

Status: implemented by `docs/plans/2026-05-04-direct-ipc-control-helpers-rust.md`.

Direct IPC probe/control helpers are owned by Rust `c2-ipc`. Python keeps
`c_two.transport.client.util.ping()` and `shutdown()` as thin facades over
native `ipc_ping()` and `ipc_shutdown()`. Rust owns IPC address validation,
socket-path derivation, signal frame encoding, timeout-bounded socket I/O, and
response validation using canonical `c2-wire` flags and signal bytes.
```

Keep the `Why this is core behavior`, `IPC/no-relay impact`, and required-test summary, but update it to past tense after implementation.

- [ ] **Step 2: Run doc sanity grep**

Run:

```bash
rg -n "manually builds UDS|copied frame constants|raw socket|Python owns.*ping|Python owns.*shutdown" docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md AGENTS.md sdk/python/src/c_two/transport/client/util.py
```

Expected: no stale current-state claims. Historical implementation plans may still mention the old problem; do not rewrite historical problem statements unless they read as current guidance.

### Task 9: Full verification and review

**Files:**
- No code changes unless verification exposes a real issue.

- [ ] **Step 1: Rebuild Python native extension**

Run:

```bash
uv sync --reinstall-package c-two
```

Expected: build succeeds.

- [ ] **Step 2: Run focused Python tests**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_ipc_address_validation.py \
  sdk/python/tests/integration/test_direct_ipc_control.py \
  sdk/python/tests/integration/test_server.py \
  sdk/python/tests/integration/test_heartbeat.py \
  -q --timeout=30
```

Expected: all selected tests pass.

- [ ] **Step 3: Run Rust IPC and native tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-ipc control
cargo test --manifest-path core/Cargo.toml -p c2-ipc
cargo test --manifest-path sdk/python/native/Cargo.toml --lib
```

Expected: all commands pass.

- [ ] **Step 4: Run full Python and Rust suites**

Run:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30
cargo test --manifest-path core/Cargo.toml --workspace
```

Expected: all Python SDK tests pass, and all Rust workspace tests pass.

- [ ] **Step 5: Inspect for stale Python raw control logic**

Run:

```bash
rg -n "socket|struct|FLAG_SIGNAL|PING_BYTES|PONG_BYTES|SHUTDOWN_CLIENT|SHUTDOWN_ACK|_encode_frame|_recv_exact|/tmp/c_two_ipc" sdk/python/src/c_two/transport/client/util.py
```

Expected: no Python raw socket/frame implementation remains. `_socket_path_from_address()` may remain, but it must call native `ipc_socket_path` and must not construct `/tmp/c_two_ipc` itself.

- [ ] **Step 6: Review for performance/security/logic regressions**

Check these points explicitly before completing the PR:

- Invalid IPC addresses fail through Rust validation and cannot escape `/tmp/c_two_ipc`.
- `ping()` and `shutdown()` use direct UDS only and do not import or consult relay settings.
- Timeouts apply to both reads and writes; unavailable servers return promptly.
- `shutdown()` still treats an absent socket as already stopped.
- Existing tests and fixtures that import `ping` continue to work.
- No CRM data-plane, SHM, scheduler, or relay-aware client paths changed.

- [ ] **Step 7: Inspect final status**

Run:

```bash
git status --short
```

Expected: only intentional files changed.

## Success Criteria

- `c2-ipc` owns direct IPC `ping`, `shutdown`, socket-path derivation, and signal response validation.
- Python `client/util.py` no longer builds frames, copies signal constants, or opens raw sockets.
- Python `ping()` / `shutdown()` keep their public bool-return behavior for invalid/unavailable addresses.
- Direct IPC probes work without relay and with a bad relay environment variable.
- Invalid IPC addresses are rejected through Rust validation.
- Existing server readiness fixtures continue to work.
- No CRM call, SHM, scheduler, HTTP, or relay route-fallback data path is changed.
