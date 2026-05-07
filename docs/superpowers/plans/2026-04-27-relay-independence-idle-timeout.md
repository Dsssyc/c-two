# Relay Independence And Idle Timeout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve issue 2 by making relay idle eviction safe and canonical, and by removing Python SDK embedded relay ownership.

**Architecture:** The relay is a standalone Rust runtime started by `c3 relay`, Docker Compose, or orchestration systems; language SDKs connect to a relay but do not host one. Python keeps the Rust HTTP client binding because HTTP is the cross-node data path, but removes `NativeRelay` and all relay-server lifecycle FFI. Relay idle eviction becomes in-flight-safe before changing the canonical default to `60s`.

**Tech Stack:** Rust 2024, `c2-http`, `c2-config`, `c2-cli`, PyO3/maturin, Python 3.10+, pytest, `tools/dev/c3_tool.py`.

---

## Scope And Decisions

This plan covers issue 2 from `docs/plans/2026-04-27-cross-language-residue-review.md`.

Decisions already made:

- Canonical relay idle timeout default: `60` seconds.
- `0` remains a valid explicit value meaning "disable time-based idle eviction".
- Idle eviction must not close an upstream connection while an HTTP request is in flight.
- Python SDK must not expose or manage a relay server through `NativeRelay`.
- Python native extension keeps `c2-http` client bindings such as `RustHttpClientPool`.
- Relay-dependent tests and examples must use the standalone `c3` binary.
- Source checkouts must build `c3` with `python tools/dev/c3_tool.py --build --link` before running relay-dependent Python tests, examples, or language-SDK development flows.

The user referred to `/dev/tools/c3_tool.py`; the repository path is `tools/dev/c3_tool.py`. Documentation should use the repository path unless the tool is moved in a separate task.

## File Map

- `core/transport/c2-http/src/relay/conn_pool.rs`: add active request accounting and in-flight-safe idle candidate selection.
- `core/transport/c2-http/src/relay/state.rs`: expose acquire/release or begin/end request operations around connection use.
- `core/transport/c2-http/src/relay/router.rs`: wrap data-plane calls in a request guard.
- `core/foundation/c2-config/src/relay.rs`: set canonical `RelayConfig::default().idle_timeout_secs` to `60`.
- `core/foundation/c2-config/src/resolver.rs`: update resolver tests and expected defaults.
- `core/foundation/c2-config/src/resolver.rs`: add `skip_ipc_validation` to relay overrides so CLI tests can start control-plane-only relays.
- `cli/src/relay.rs`: add a hidden `--skip-ipc-validation` relay flag for test-only mesh control-plane tests, and pass it as a relay override.
- `cli/tests/relay_args.rs`: assert the new default and hidden skip-validation behavior.
- `sdk/python/native/src/lib.rs`: stop registering `relay_ffi`.
- `sdk/python/native/src/relay_ffi.rs`: delete after tests stop importing `NativeRelay`.
- `sdk/python/src/c_two/relay/__init__.py`: delete the public Python relay-server module.
- `sdk/python/tests/conftest.py`: add fixtures that locate/start/stop standalone `c3 relay` subprocesses.
- `sdk/python/tests/unit/test_native_relay.py`: delete or replace with CLI-backed relay tests.
- `sdk/python/tests/integration/test_http_relay.py`: replace `NativeRelay` fixture with `c3 relay` fixture.
- `sdk/python/tests/integration/test_relay_mesh.py`: replace `NativeRelay` instances with `c3 relay` subprocesses using hidden `--skip-ipc-validation`.
- `sdk/python/tests/integration/test_registry.py`: replace duplicate-route relay test setup with `c3 relay`.
- `sdk/python/tests/integration/test_python_examples.py`: start `c3 relay` subprocess instead of `NativeRelay`.
- `sdk/python/benchmarks/*.py`: remove `NativeRelay` startup helpers from Python benchmark scripts that host relay in-process; require external `c3 relay`.
- `README.md`, `README.zh-CN.md`, `CONTRIBUTING.md`, `.github/copilot-instructions.md`, `sdk/python/README.md`, `sdk/python/tests/README.md`, `cli/README.md`, `.env.example`: document standalone relay ownership, `60s` default, and `tools/dev/c3_tool.py --build --link` prerequisite.

## Task 1: Make Relay Idle Eviction In-Flight Safe

**Files:**
- Modify: `core/transport/c2-http/src/relay/conn_pool.rs`
- Modify: `core/transport/c2-http/src/relay/state.rs`
- Modify: `core/transport/c2-http/src/relay/router.rs`

- [ ] **Step 1: Add failing connection-pool tests for active request protection**

Append these tests to `core/transport/c2-http/src/relay/conn_pool.rs` inside the existing `#[cfg(test)] mod tests`:

```rust
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

    assert!(pool.begin_request("grid").is_some());
    pool.end_request("grid");

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
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test --manifest-path core/transport/c2-http/Cargo.toml --features relay conn_pool
```

Expected: fail to compile because `ConnectionPool::begin_request` and `ConnectionPool::end_request` do not exist.

- [ ] **Step 3: Implement active request accounting in `ConnectionPool`**

In `core/transport/c2-http/src/relay/conn_pool.rs`, add `AtomicUsize` to the existing atomic imports:

```rust
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
```

Change `ConnectionEntry` to include an active request counter:

```rust
struct ConnectionEntry {
    address: String,
    client: Option<Arc<IpcClient>>,
    last_activity: AtomicU64,
    active_requests: AtomicUsize,
}
```

Update `insert()`:

```rust
self.entries.insert(
    name,
    ConnectionEntry {
        address,
        client: Some(client),
        last_activity: AtomicU64::new(now_millis()),
        active_requests: AtomicUsize::new(0),
    },
);
```

Add these methods to `impl ConnectionPool`:

```rust
pub fn begin_request(&self, name: &str) -> Option<Arc<IpcClient>> {
    let entry = self.entries.get(name)?;
    let client = entry.client.as_ref()?;
    if !client.is_connected() {
        return None;
    }
    entry.active_requests.fetch_add(1, Ordering::AcqRel);
    entry.last_activity.store(now_millis(), Ordering::Release);
    Some(client.clone())
}

pub fn end_request(&self, name: &str) {
    if let Some(entry) = self.entries.get(name) {
        let previous = entry.active_requests.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "end_request called without begin_request");
        entry.last_activity.store(now_millis(), Ordering::Release);
    }
}
```

Change `idle_entries()` so connected clients are only time-evicted when `active_requests == 0`:

```rust
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
                    && e.last_activity.load(Ordering::Acquire) < cutoff
            } else {
                false
            }
        })
        .map(|(name, _)| name.clone())
        .collect()
}
```

- [ ] **Step 4: Expose request acquire/release through `RelayState`**

In `core/transport/c2-http/src/relay/state.rs`, replace the current `get_client()` method with:

```rust
pub fn begin_request(&self, name: &str) -> Option<Arc<IpcClient>> {
    self.conn_pool.read().begin_request(name)
}

pub fn end_request(&self, name: &str) {
    self.conn_pool.read().end_request(name);
}
```

Keep `touch_connection()` for non-guard paths if current code still calls it elsewhere.

- [ ] **Step 5: Use a request guard in the relay HTTP data-plane handler**

In `core/transport/c2-http/src/relay/router.rs`, add this local guard type near `call_handler()`:

```rust
struct RelayRequestGuard {
    state: Arc<RelayState>,
    route_name: String,
}

impl RelayRequestGuard {
    fn new(state: Arc<RelayState>, route_name: String) -> Self {
        Self { state, route_name }
    }
}

impl Drop for RelayRequestGuard {
    fn drop(&mut self) {
        self.state.end_request(&self.route_name);
    }
}
```

In `call_handler()`, replace:

```rust
let client = state.get_client(&route_name);
```

with:

```rust
let client = state.begin_request(&route_name);
let mut request_guard: Option<RelayRequestGuard> = None;
```

After matching `Some(c) => c`, set the guard:

```rust
Some(c) => {
    request_guard = Some(RelayRequestGuard::new(state.clone(), route_name.clone()));
    c
}
```

After successful `try_reconnect`, call `state.begin_request()` for the reconnected entry rather than using the raw reconnected client without accounting:

```rust
None => match try_reconnect(&state, &route_name).await {
    Some(_) => match state.begin_request(&route_name) {
        Some(c) => {
            request_guard = Some(RelayRequestGuard::new(state.clone(), route_name.clone()));
            c
        }
        None => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("Upstream '{route_name}' is registered but unreachable")
                })),
            )
                .into_response();
        }
    },
    None => {
        let has = state.has_connection(&route_name);
        if has {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("Upstream '{route_name}' is registered but unreachable")
                })),
            )
                .into_response();
        }
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("No upstream registered for route: '{route_name}'")
            })),
        )
            .into_response();
    }
},
```

Immediately after the `let client = match client { ... };` block and before `match client.call(...)`, keep this binding alive for the whole upstream call:

```rust
let _request_guard = request_guard;
```

Remove `state.touch_connection(&route_name)` calls from success and CRM error branches because guard drop updates last activity at the end of the request.

- [ ] **Step 6: Run relay tests**

Run:

```bash
cargo test --manifest-path core/transport/c2-http/Cargo.toml --features relay
```

Expected: all `c2-http` relay tests pass.

- [ ] **Step 7: Commit**

```bash
git add core/transport/c2-http/src/relay/conn_pool.rs core/transport/c2-http/src/relay/state.rs core/transport/c2-http/src/relay/router.rs
git commit -m "fix(relay): protect active upstream requests from idle eviction"
```

## Task 2: Set Canonical Relay Idle Timeout To 60 Seconds

**Files:**
- Modify: `core/foundation/c2-config/src/relay.rs`
- Modify: `core/foundation/c2-config/src/resolver.rs`
- Modify: `cli/tests/relay_args.rs`
- Modify: `.env.example`
- Modify: `cli/README.md`

- [ ] **Step 1: Add failing tests for the 60-second default**

In `core/foundation/c2-config/src/resolver.rs`, update the resolver default assertion from:

```rust
assert_eq!(resolved.relay.idle_timeout_secs, 0);
```

to:

```rust
assert_eq!(resolved.relay.idle_timeout_secs, 60);
```

In `cli/tests/relay_args.rs`, append:

```rust
#[test]
fn relay_dry_run_uses_canonical_idle_timeout_default() {
    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.env("C2_ENV_FILE", "")
        .env_remove("C2_RELAY_IDLE_TIMEOUT")
        .args(["relay", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("idle_timeout=60"));
}
```

- [ ] **Step 2: Run tests to verify the old default fails**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config
cargo test --manifest-path cli/Cargo.toml relay_dry_run_uses_canonical_idle_timeout_default
```

Expected: at least one test fails while the default is still `0`.

- [ ] **Step 3: Change the canonical default**

In `core/foundation/c2-config/src/relay.rs`, change:

```rust
idle_timeout_secs: 0,
```

to:

```rust
idle_timeout_secs: 60,
```

- [ ] **Step 4: Update docs for the new default**

In `.env.example`, change:

```dotenv
# C2_RELAY_IDLE_TIMEOUT=0
```

to:

```dotenv
# C2_RELAY_IDLE_TIMEOUT=60
```

In `cli/README.md`, change the relay options table row to:

```markdown
| `--idle-timeout` | `C2_RELAY_IDLE_TIMEOUT` | `60` | Seconds before idle upstream IPC connections are evicted. Use `0` to disable time-based eviction. |
```

- [ ] **Step 5: Run config and CLI tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config
cargo test --manifest-path cli/Cargo.toml
```

Expected: both commands pass.

- [ ] **Step 6: Commit**

```bash
git add core/foundation/c2-config/src/relay.rs core/foundation/c2-config/src/resolver.rs cli/tests/relay_args.rs .env.example cli/README.md
git commit -m "fix(relay): use sixty second idle timeout by default"
```

## Task 3: Add CLI Support Needed By Relay Process Tests

**Files:**
- Modify: `core/foundation/c2-config/src/resolver.rs`
- Modify: `cli/src/relay.rs`
- Modify: `cli/tests/relay_args.rs`

- [ ] **Step 1: Add failing CLI tests for hidden skip-validation**

Append this to `cli/tests/relay_args.rs`:

```rust
#[test]
fn relay_help_hides_skip_ipc_validation() {
    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.args(["relay", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--skip-ipc-validation").not());
}

#[test]
fn relay_dry_run_accepts_hidden_skip_ipc_validation_for_tests() {
    let mut cmd = Command::cargo_bin("c3").unwrap();
    cmd.env("C2_ENV_FILE", "")
        .args(["relay", "--skip-ipc-validation", "--dry-run"])
        .assert()
        .success();
}
```

- [ ] **Step 2: Run the failing test**

Run:

```bash
cargo test --manifest-path cli/Cargo.toml relay_dry_run_accepts_hidden_skip_ipc_validation_for_tests
```

Expected: fail because the CLI does not accept `--skip-ipc-validation`.

- [ ] **Step 3: Add the resolver override field**

In `core/foundation/c2-config/src/resolver.rs`, add this field to `RelayConfigOverrides`:

```rust
pub skip_ipc_validation: Option<bool>,
```

In `resolve_relay_config()`, after applying `overrides.idle_timeout_secs`, add:

```rust
if let Some(v) = overrides.skip_ipc_validation {
    cfg.skip_ipc_validation = v;
}
```

- [ ] **Step 4: Add the hidden CLI flag and pass it into the resolver override**

In `cli/src/relay.rs`, add to `RelayArgs`:

```rust
/// Skip IPC validation in /_register. Intended for relay control-plane tests only.
#[arg(long = "skip-ipc-validation", hide = true)]
pub skip_ipc_validation: bool,
```

In the `RelayConfigOverrides` literal, add:

```rust
skip_ipc_validation: if args.skip_ipc_validation {
    Some(true)
} else {
    None
},
```

- [ ] **Step 5: Run config and CLI tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config
cargo test --manifest-path cli/Cargo.toml
```

Expected: all config and CLI tests pass.

- [ ] **Step 6: Commit**

```bash
git add core/foundation/c2-config/src/resolver.rs cli/src/relay.rs cli/tests/relay_args.rs
git commit -m "test(cli): expose hidden relay validation bypass"
```

## Task 4: Remove Python `NativeRelay` FFI And Public Module

**Files:**
- Modify: `sdk/python/native/src/lib.rs`
- Delete: `sdk/python/native/src/relay_ffi.rs`
- Delete: `sdk/python/src/c_two/relay/__init__.py`
- Modify: `sdk/python/tests/unit/test_python_examples_syntax.py`

- [ ] **Step 1: Add failing public API test**

Append this test to `sdk/python/tests/unit/test_python_examples_syntax.py`:

```python
def test_python_sdk_does_not_export_embedded_native_relay():
    import importlib.util
    import c_two._native as native

    assert not hasattr(native, "NativeRelay")
    assert importlib.util.find_spec("c_two.relay") is None
```

- [ ] **Step 2: Run the failing test**

Run:

```bash
C2_ENV_FILE= uv run pytest sdk/python/tests/unit/test_python_examples_syntax.py::test_python_sdk_does_not_export_embedded_native_relay -q
```

Expected: fail because `c_two._native.NativeRelay` and `c_two.relay` still exist.

- [ ] **Step 3: Stop registering the relay FFI module**

In `sdk/python/native/src/lib.rs`, remove:

```rust
#[cfg(feature = "python")]
mod relay_ffi;
```

and remove this call from `c2_native()`:

```rust
relay_ffi::register_module(m)?;
```

- [ ] **Step 4: Delete the embedded relay FFI files**

Run:

```bash
git rm sdk/python/native/src/relay_ffi.rs sdk/python/src/c_two/relay/__init__.py
```

- [ ] **Step 5: Run native build check**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml --lib
```

Expected: the Python native crate builds without `relay_ffi`.

- [ ] **Step 6: Commit**

```bash
git add sdk/python/native/src/lib.rs sdk/python/tests/unit/test_python_examples_syntax.py
git commit -m "refactor(python): remove embedded NativeRelay binding"
```

## Task 5: Add Python Test Fixtures For Standalone `c3 relay`

**Files:**
- Modify: `sdk/python/tests/conftest.py`

- [ ] **Step 1: Add fixture code**

Add these imports near the top of `sdk/python/tests/conftest.py`:

```python
import signal
import socket
import subprocess
import sys
from pathlib import Path
from collections.abc import Iterator
import urllib.request
```

Add these helpers and fixtures after the existing `_wait_for_server()` helper:

```python
def repo_root() -> Path:
    return Path(__file__).resolve().parents[3]


def free_tcp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def c3_binary() -> Path:
    root = repo_root()
    name = "c3.exe" if os.name == "nt" else "c3"
    candidates = [
        root / "cli" / "target" / "debug" / name,
        root / "cli" / "target" / "release" / name,
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    pytest.skip(
        "c3 binary is required for relay tests. "
        "Run `python tools/dev/c3_tool.py --build --link` from the repository root."
    )


def wait_for_relay(url: str, timeout: float = 5.0) -> None:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with opener.open(f"{url}/health", timeout=0.5) as resp:
                if resp.status == 200:
                    return
        except Exception:
            pass
        time.sleep(0.1)
    raise TimeoutError(f"Relay at {url} not ready after {timeout}s")


def stop_process(proc: subprocess.Popen[str]) -> None:
    if proc.poll() is not None:
        return
    proc.send_signal(signal.SIGINT if hasattr(signal, "SIGINT") else signal.SIGTERM)
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


class RelayProcess:
    def __init__(self, proc: subprocess.Popen[str], url: str, bind: str):
        self.proc = proc
        self.url = url
        self.bind = bind

    def stop(self) -> None:
        stop_process(self.proc)


@pytest.fixture
def start_c3_relay() -> Iterator:
    processes: list[RelayProcess] = []

    def _start(
        *,
        port: int | None = None,
        relay_id: str | None = None,
        seeds: list[str] | None = None,
        skip_ipc_validation: bool = False,
        idle_timeout: int | None = None,
    ) -> RelayProcess:
        actual_port = port if port is not None else free_tcp_port()
        bind = f"127.0.0.1:{actual_port}"
        url = f"http://127.0.0.1:{actual_port}"
        args = [str(c3_binary()), "relay", "--bind", bind, "--advertise-url", url]
        if relay_id is not None:
            args.extend(["--relay-id", relay_id])
        if seeds:
            args.extend(["--seeds", ",".join(seeds)])
        if skip_ipc_validation:
            args.append("--skip-ipc-validation")
        if idle_timeout is not None:
            args.extend(["--idle-timeout", str(idle_timeout)])

        env = os.environ.copy()
        env["C2_ENV_FILE"] = ""
        env["NO_PROXY"] = "127.0.0.1,localhost"
        env["no_proxy"] = "127.0.0.1,localhost"
        proc = subprocess.Popen(
            args,
            cwd=repo_root(),
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        relay = RelayProcess(proc=proc, url=url, bind=bind)
        try:
            wait_for_relay(url)
        except Exception:
            stop_process(proc)
            stdout, stderr = proc.communicate(timeout=1)
            raise AssertionError(f"c3 relay failed to start\nstdout:\n{stdout}\nstderr:\n{stderr}")
        processes.append(relay)
        return relay

    yield _start

    for relay in reversed(processes):
        relay.stop()
```

- [ ] **Step 2: Run fixture import check**

Run:

```bash
C2_ENV_FILE= uv run pytest sdk/python/tests/unit/test_python_examples_syntax.py -q
```

Expected: syntax/import tests pass, or only fail because Task 4 has not removed `NativeRelay` yet if tasks are run out of order.

- [ ] **Step 3: Commit**

```bash
git add sdk/python/tests/conftest.py
git commit -m "test(python): add standalone c3 relay fixture"
```

## Task 6: Rewrite Python Relay Tests To Use `c3 relay`

**Files:**
- Delete: `sdk/python/tests/unit/test_native_relay.py`
- Modify: `sdk/python/tests/integration/test_http_relay.py`
- Modify: `sdk/python/tests/integration/test_relay_mesh.py`
- Modify: `sdk/python/tests/integration/test_registry.py`
- Modify: `sdk/python/tests/integration/test_python_examples.py`

- [ ] **Step 1: Remove direct `NativeRelay` unit tests**

Run:

```bash
git rm sdk/python/tests/unit/test_native_relay.py
```

These lifecycle tests belong to Rust/CLI relay tests after relay becomes standalone. Keep duplicate-route and data-plane coverage by moving them to CLI-backed integration tests below.

- [ ] **Step 2: Rewrite `test_http_relay.py` imports**

In `sdk/python/tests/integration/test_http_relay.py`, change:

```python
from c_two._native import NativeRelay, RustHttpClientPool
```

to:

```python
from c_two._native import RustHttpClientPool
```

Update the module docstring first line to:

```python
"""Integration tests for the HTTP relay chain through standalone c3 relay.
```

- [ ] **Step 3: Rewrite the HTTP relay fixture**

Replace the existing `relay_stack()` fixture in `test_http_relay.py` with:

```python
@pytest.fixture
def relay_stack(start_c3_relay):
    """Start Server + standalone c3 relay and return (relay_url, ipc_address)."""
    cc.register(Hello, HelloImpl(), name='hello')
    cc.register(Counter, CounterImpl(), name='counter')
    ipc_addr = cc.server_address()

    relay = start_c3_relay()
    relay_url = relay.url

    with httpx.Client(trust_env=False, timeout=5.0) as client:
        for name in ("hello", "counter"):
            resp = client.post(f"{relay_url}/_register", json={"name": name, "address": ipc_addr})
            assert resp.status_code == 201, resp.text

    yield relay_url, ipc_addr
```

Remove `relay.stop()` from this fixture; `start_c3_relay` owns cleanup.

- [ ] **Step 4: Replace late inline `NativeRelay` usage in slash/proxy tests**

Run:

```bash
rg -n "NativeRelay|relay\\.stop\\(|relay\\.start\\(|register_upstream" sdk/python/tests/integration/test_http_relay.py
```

For `test_connect_http_with_slash_in_name`, add `start_c3_relay` to the test parameters and replace the relay setup block with:

```python
relay = start_c3_relay()
relay_url = relay.url
with httpx.Client(trust_env=False, timeout=5.0) as http:
    resp = http.post(f"{relay_url}/_register", json={"name": slashed_name, "address": ipc_addr})
    assert resp.status_code == 201, resp.text
```

For `test_relay_traffic_bypasses_system_proxy`, add `start_c3_relay` to the test parameters and replace the relay setup block with:

```python
relay = start_c3_relay()
relay_url = relay.url
with httpx.Client(trust_env=False, timeout=5.0) as http:
    resp = http.post(f"{relay_url}/_register", json={"name": name, "address": ipc_addr})
    assert resp.status_code == 201, resp.text
```

Remove the old `_wait_for_relay(relay_url)`, `relay.start()`, `relay.stop()`, and local `from c_two._native import NativeRelay` lines from both tests.

- [ ] **Step 5: Replace control-plane `NativeRelay` usage in `test_http_relay.py`**

In class `TestRelayControlPlane`, add `start_c3_relay` to each test that starts a relay and use one of these patterns.

For tests that register a real upstream:

```python
relay = start_c3_relay()
relay_url = relay.url
transport = httpx.HTTPTransport()
with httpx.Client(transport=transport) as http:
    resp = http.post(
        f'{relay_url}/_register',
        json={'name': 'hello', 'address': ipc_addr},
    )
    assert resp.status_code == 201
```

For tests that do not need a real upstream connection, such as unknown route or missing unregister checks:

```python
relay = start_c3_relay(skip_ipc_validation=True)
relay_url = relay.url
transport = httpx.HTTPTransport()
with httpx.Client(transport=transport) as http:
    resp = http.post(
        f'{relay_url}/nonexistent/method',
        content=b'data',
    )
    assert resp.status_code == 404
```

For `test_health_shows_registered_routes`, replace direct `relay.register_upstream(...)` calls with HTTP control-plane registration:

```python
with httpx.Client(trust_env=False, timeout=5.0) as http:
    for name in ("hello", "counter"):
        resp = http.post(f"{relay_url}/_register", json={"name": name, "address": ipc_addr})
        assert resp.status_code == 201, resp.text
```

Remove every `relay.stop()` call from these tests because `start_c3_relay` owns cleanup.

- [ ] **Step 6: Rewrite `test_relay_mesh.py` to start CLI relay processes**

In `sdk/python/tests/integration/test_relay_mesh.py`, remove:

```python
import c_two as cc
from c_two._native import NativeRelay

pytestmark = pytest.mark.skipif(
    not hasattr(cc._native, "NativeRelay"),
    reason="relay feature not compiled",
)
```

For each test, add `start_c3_relay` as a parameter and replace relay creation:

```python
relay_a = start_c3_relay(
    port=port_a,
    relay_id="relay-a",
    skip_ipc_validation=True,
)
relay_b = start_c3_relay(
    port=port_b,
    relay_id="relay-b",
    seeds=[url_a],
    skip_ipc_validation=True,
)
```

Remove all `relay_a.stop()` and `relay_b.stop()` calls. The fixture cleans up.

For single-relay tests, use:

```python
relay = start_c3_relay(port=port, skip_ipc_validation=True)
```

- [ ] **Step 7: Rewrite duplicate-route registry integration test**

In `sdk/python/tests/integration/test_registry.py`, replace:

```python
relay = NativeRelay(f'127.0.0.1:{_next_relay_port()}')
relay.start()
relay_url = f'http://{relay.bind_address}'
```

with:

```python
relay = start_c3_relay(port=_next_relay_port())
relay_url = relay.url
```

Add `start_c3_relay` to `test_relay_rejects_duplicate_name_from_second_registry` parameters and remove `relay.stop()` from the `finally` block.

Remove any `from c_two._native import NativeRelay` import from this file.

- [ ] **Step 8: Rewrite Python example integration tests**

In `sdk/python/tests/integration/test_python_examples.py`, remove:

```python
import c_two as cc
from c_two._native import NativeRelay

pytestmark = pytest.mark.skipif(
    not hasattr(cc._native, "NativeRelay"),
    reason="relay feature not compiled",
)
```

In `test_relay_client_workflow_uses_explicit_relay_url(start_c3_relay)`, replace:

```python
port = _free_port()
relay_url = f"http://127.0.0.1:{port}"
relay = NativeRelay(f"127.0.0.1:{port}")
relay.start()
_wait_for_relay(relay_url)
```

with:

```python
relay = start_c3_relay()
relay_url = relay.url
```

Remove `relay.stop()` from the `finally` block.

Apply the same replacement to `test_general_client_uses_relay_without_ipc_address(start_c3_relay)`.

- [ ] **Step 9: Verify there are no direct Python test imports of `NativeRelay`**

Run:

```bash
rg -n "NativeRelay|c_two\\.relay" sdk/python/tests sdk/python/src
```

Expected: no results under `sdk/python/tests` or `sdk/python/src`.

- [ ] **Step 10: Build c3 before Python relay tests**

Run:

```bash
python tools/dev/c3_tool.py --build --link
```

Expected: prints a `c3 binary:` path and a linked path.

- [ ] **Step 11: Run focused Python relay tests**

Run:

```bash
C2_ENV_FILE= uv run pytest \
  sdk/python/tests/integration/test_http_relay.py \
  sdk/python/tests/integration/test_relay_mesh.py \
  sdk/python/tests/integration/test_registry.py::TestRelayRegistration::test_relay_rejects_duplicate_name_from_second_registry \
  sdk/python/tests/integration/test_python_examples.py \
  -q --timeout=60
```

Expected: tests pass. If examples dependencies such as `pandas` or `pyarrow` are absent, example tests may skip by existing `pytest.importorskip()` logic.

- [ ] **Step 12: Commit**

```bash
git add sdk/python/tests/conftest.py sdk/python/tests/integration/test_http_relay.py sdk/python/tests/integration/test_relay_mesh.py sdk/python/tests/integration/test_registry.py sdk/python/tests/integration/test_python_examples.py
git commit -m "test(python): run relay coverage through standalone c3"
```

## Task 7: Update Benchmarks And Examples To Require External Relay

**Files:**
- Modify: `sdk/python/benchmarks/_relay_server.py`
- Modify: `sdk/python/benchmarks/_relay_full_server.py`
- Modify: `sdk/python/benchmarks/three_mode_benchmark.py`
- Modify: `sdk/python/benchmarks/relay_qps_benchmark.py`
- Modify: `examples/python/relay_mesh/README.md`
- Modify: `examples/README.md`

- [ ] **Step 1: Delete relay-host helper benchmark scripts**

These scripts only exist to host `NativeRelay` inside Python:

```bash
git rm sdk/python/benchmarks/_relay_server.py sdk/python/benchmarks/_relay_full_server.py
```

- [ ] **Step 2: Rewrite `relay_qps_benchmark.py` to require external relay**

In `sdk/python/benchmarks/relay_qps_benchmark.py`, remove:

```python
from c_two._native import NativeRelay
```

Change the module docstring to:

```python
"""Relay QPS benchmark - measures requests/second through an external c3 relay.

Start the relay before running this benchmark:

    python tools/dev/c3_tool.py --build --link
    c3 relay --bind 127.0.0.1:<port>

Set RELAY_PORT to match the relay port when needed.
"""
```

Add:

```python
import urllib.request
```

Add this helper after `cleanup_stale()`:

```python
def require_external_relay(relay_url: str) -> None:
    try:
        with urllib.request.urlopen(f"{relay_url}/health", timeout=5) as resp:
            if resp.status == 200:
                return
    except Exception as exc:
        raise SystemExit(
            f"Relay is not reachable at {relay_url}. "
            "Start it first with `c3 relay --bind 127.0.0.1:<port>` after running "
            "`python tools/dev/c3_tool.py --build --link`."
        ) from exc
```

Inside `main()`, replace `NativeRelay` startup, `relay.register_upstream(...)`, and `relay.stop()` with:

```python
relay_url = f"http://127.0.0.1:{RELAY_PORT}"
require_external_relay(relay_url)

with urllib.request.urlopen(
    urllib.request.Request(
        f"{relay_url}/_register",
        data=b'{"name":"hello","address":"' + ipc_addr.encode() + b'"}',
        headers={"Content-Type": "application/json"},
        method="POST",
    ),
    timeout=5,
) as resp:
    if resp.status != 201:
        raise RuntimeError(f"relay registration failed: HTTP {resp.status}")
```

Keep the `hey` URLs pointed at `relay_url`.

- [ ] **Step 3: Rewrite `three_mode_benchmark.py` to require external relay for relay mode**

In `sdk/python/benchmarks/three_mode_benchmark.py`, remove:

```python
from c_two._native import NativeRelay
```

Update the relay mode docstring bullet to:

```text
  - Relay: HTTP -> external c3 relay -> IPC -> CRM -> reverse
```

Add:

```python
import json
import urllib.request
```

Add this helper before `bench_relay()`:

```python
def _require_external_relay(relay_url: str) -> None:
    try:
        with urllib.request.urlopen(f"{relay_url}/health", timeout=5) as resp:
            if resp.status == 200:
                return
    except Exception as exc:
        raise RuntimeError(
            f"Relay is not reachable at {relay_url}. "
            "Start it first with `c3 relay --bind 127.0.0.1:<port>` after running "
            "`python tools/dev/c3_tool.py --build --link`."
        ) from exc


def _register_relay_upstream(relay_url: str, name: str, address: str) -> None:
    req = urllib.request.Request(
        f"{relay_url}/_register",
        data=json.dumps({"name": name, "address": address}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5) as resp:
        if resp.status != 201:
            raise RuntimeError(f"relay registration failed: HTTP {resp.status}")
```

In `bench_relay()`, replace the `NativeRelay` block with:

```python
relay_url = f"http://127.0.0.1:{_relay_port}"
_require_external_relay(relay_url)
_register_relay_upstream(relay_url, "echo_relay", address)
```

Use `relay_url` in `cc.connect(...)`:

```python
crm = cc.connect(Echo, name='echo_relay', address=relay_url)
```

Remove `relay.stop()` from the `finally` block.

- [ ] **Step 4: Verify benchmark imports are gone**

Run:

```bash
rg -n "NativeRelay" sdk/python/benchmarks examples
```

- [ ] **Step 5: Update example docs**

In `examples/python/relay_mesh/README.md`, make the first command:

```bash
python tools/dev/c3_tool.py --build --link
c3 relay --bind 127.0.0.1:8080
```

Then keep the Python resource and client commands as separate terminals.

- [ ] **Step 6: Verify no benchmark/example imports remain**

Run:

```bash
rg -n "NativeRelay|c_two\\.relay" sdk/python/benchmarks examples
```

Expected: no results.

- [ ] **Step 7: Commit**

```bash
git add sdk/python/benchmarks examples
git commit -m "docs(examples): require standalone c3 relay"
```

## Task 8: Update Documentation For Standalone Relay Ownership

**Files:**
- Modify: `README.md`
- Modify: `README.zh-CN.md`
- Modify: `CONTRIBUTING.md`
- Modify: `.github/copilot-instructions.md`
- Modify: `sdk/python/README.md`
- Modify: `sdk/python/tests/README.md`
- Modify: `cli/README.md`
- Modify: `docs/plans/2026-04-27-cross-language-residue-review.md`
- Modify: `docs/superpowers/specs/2026-04-27-rust-config-resolver-design.md`

- [ ] **Step 1: Update Python SDK README test instructions**

In `sdk/python/README.md`, add this before any command that runs all Python tests, relay tests, or examples:

```markdown
Relay-dependent tests and examples require a standalone `c3` binary in a source checkout:

```bash
python tools/dev/c3_tool.py --build --link
```

The Python SDK does not embed or start the relay server. Start relay processes with `c3 relay` or through Docker Compose/Kubernetes, then point SDK code at them with `C2_RELAY_ANCHOR_ADDRESS` or `cc.set_relay_anchor()`.
```
```

- [ ] **Step 2: Update Python tests README**

In `sdk/python/tests/README.md`, remove the row for `test_native_relay.py` and add:

```markdown
Relay integration tests start standalone `c3 relay` subprocesses. Build `c3` first:

```bash
python tools/dev/c3_tool.py --build --link
```
```

- [ ] **Step 3: Update root docs and contributor docs**

In `README.md`, `README.zh-CN.md`, `CONTRIBUTING.md`, and `.github/copilot-instructions.md`, ensure relay example/test setup uses:

```bash
python tools/dev/c3_tool.py --build --link
c3 relay --bind 127.0.0.1:8080
```

Remove wording that implies Python can start a relay server with `NativeRelay`.

- [ ] **Step 4: Update issue/spec history**

In `docs/plans/2026-04-27-cross-language-residue-review.md`, update issue 2 acceptance criteria to include:

```markdown
- Python SDK no longer exposes embedded relay server lifecycle APIs.
- Relay server config, including `C2_RELAY_IDLE_TIMEOUT`, belongs to the standalone Rust relay runtime.
- Relay idle eviction is in-flight-safe.
- Canonical idle timeout default is `60s`; `0` disables time-based eviction.
```

In `docs/superpowers/specs/2026-04-27-rust-config-resolver-design.md`, update relay default rows and open question text so they no longer say `0` or `300` are the unresolved choices.

- [ ] **Step 5: Verify docs no longer mention removed API**

Run:

```bash
rg -n "NativeRelay|c_two\\.relay|idle_timeout=0|C2_RELAY_IDLE_TIMEOUT=0" README.md README.zh-CN.md CONTRIBUTING.md .github/copilot-instructions.md sdk/python/README.md sdk/python/tests/README.md cli/README.md docs/plans/2026-04-27-cross-language-residue-review.md docs/superpowers/specs/2026-04-27-rust-config-resolver-design.md
```

Expected: no stale public API references. Historical docs outside this command can remain until their own cleanup tasks.

- [ ] **Step 6: Commit**

```bash
git add README.md README.zh-CN.md CONTRIBUTING.md .github/copilot-instructions.md sdk/python/README.md sdk/python/tests/README.md cli/README.md docs/plans/2026-04-27-cross-language-residue-review.md docs/superpowers/specs/2026-04-27-rust-config-resolver-design.md
git commit -m "docs: document standalone relay workflow"
```

## Task 9: Final Verification

**Files:**
- No code changes unless verification reveals a bug.

- [ ] **Step 1: Format Rust code**

Run:

```bash
cargo fmt --check --manifest-path core/transport/c2-http/Cargo.toml
cargo fmt --check --manifest-path core/Cargo.toml
cargo fmt --check --manifest-path cli/Cargo.toml
cargo fmt --check --manifest-path sdk/python/native/Cargo.toml
```

Expected: all commands pass. If any fail, run `cargo fmt` on the relevant manifest and commit the formatting-only diff.

- [ ] **Step 2: Run Rust test suites**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config
cargo test --manifest-path core/transport/c2-http/Cargo.toml --features relay
cargo test --manifest-path cli/Cargo.toml
cargo check --manifest-path sdk/python/native/Cargo.toml --lib
```

Expected: all commands pass.

- [ ] **Step 3: Build and link c3**

Run:

```bash
python tools/dev/c3_tool.py --build --link
```

Expected: prints the local `c3` binary path and link location.

- [ ] **Step 4: Run focused Python tests**

Run:

```bash
C2_ENV_FILE= uv run pytest \
  sdk/python/tests/unit/test_python_examples_syntax.py \
  sdk/python/tests/integration/test_http_relay.py \
  sdk/python/tests/integration/test_relay_mesh.py \
  sdk/python/tests/integration/test_registry.py::TestRelayRegistration::test_relay_rejects_duplicate_name_from_second_registry \
  sdk/python/tests/integration/test_python_examples.py \
  -q --timeout=60
```

Expected: tests pass, with example tests skipped only if optional example dependencies are not installed.

- [ ] **Step 5: Run full Python SDK tests**

Run:

```bash
C2_ENV_FILE= C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests -q --timeout=60
```

Expected: tests pass.

- [ ] **Step 6: Check stale references and whitespace**

Run:

```bash
rg -n "NativeRelay|c_two\\.relay" sdk/python/src sdk/python/tests sdk/python/benchmarks examples README.md README.zh-CN.md CONTRIBUTING.md .github/copilot-instructions.md
git diff --check
```

Expected: no `NativeRelay` or `c_two.relay` references remain in active SDK/docs surfaces, and `git diff --check` reports no whitespace errors.

- [ ] **Step 7: Final commit if needed**

If verification fixes touch concrete files, stage those files explicitly. For example:

```bash
git add core/transport/c2-http/src/relay/conn_pool.rs
git commit -m "chore: finalize relay independence cleanup"
```

## Addendum: Relay Ownership Hardening

The initial idle-timeout work exposed a deeper ownership problem: route
ownership had been coupled to the presence of a cached IPC client. After idle
eviction, a live but evicted owner could be overwritten by a duplicate
registration.

The approved follow-up design is documented in
`docs/superpowers/plans/2026-04-28-relay-ownership-hardening.md` and uses one
per-route upstream slot. `RouteTable` remains the ownership source of truth,
while each slot serializes lazy reconnect, tracks active requests, supports
retirement on unregister/replacement, and prevents orphan reconnect results
from serving routes that no longer exist.
