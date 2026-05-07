# Relay IPC Endpoint Identity Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make relay-discovered direct IPC safe by accepting an IPC fast-path target only when the IPC handshake proves it is the exact server instance described by relay resolution.

**Architecture:** Rust owns endpoint identity. `c2-wire` carries server identity in the IPC handshake, `c2-server` publishes that identity, `c2-ipc` projects it to clients, `c2-http` relay registration/resolution carries local route identity, and `c2-runtime` verifies identity before exposing a relay-resolved IPC client to Python. Python remains a thin routing facade and never decides IPC endpoint authenticity from route names.

**Tech Stack:** Rust `c2-wire`, `c2-server`, `c2-ipc`, `c2-http`, `c2-runtime`; PyO3 native bindings in `sdk/python/native`; Python pytest integration tests; Cargo workspace tests.

**Implementation status:** Planned. Do not start production code changes until this plan has been reviewed in the current dirty worktree.

---

## 0.x Clean-Cut Constraint

C-Two is in the 0.x line. This plan intentionally breaks stale internal wire/control shapes instead of preserving compatibility aliases. Bump the IPC handshake version and update tests/fixtures in one change; do not keep a v6/v7 dual decoder, old relay response aliases, Python fallback checks, or route-name-only relay IPC acceptance.

## Problem Statement

Current relay name resolution can return an `ipc_address` for local fast-path use. The native runtime then connects to that IPC server and accepts it if the server handshake contains the requested route name:

```rust
if !client.route_names().into_iter().any(|registered| registered == route_name) {
    pool.release(&addr);
    return Err(RelayIpcConnectError::Unavailable);
}
```

This proves only that the connected server has a route with the same user-chosen name. It does not prove the connected server is the route owner described by the relay response. Stale relay state, reused IPC addresses, duplicate route names in unrelated local processes, or loopback port-forwarded remote relays can make the SDK connect to the wrong local IPC server. Route name is a business routing key, not an endpoint identity.

## Target Behavior

- Every IPC server instance has two identities:
  - `server_id`: stable logical server identity already used by runtime/relay registration.
  - `server_instance_id`: unpredictable per-process/per-server-incarnation nonce generated when the server instance is created.
- IPC server handshake ACK includes both identity fields.
- Relay registration sends both identity fields for local routes.
- Relay `/_resolve` returns `ipc_address`, `server_id`, and `server_instance_id` only for loopback callers and only for routes local to that relay.
- Non-loopback callers and peer routes do not receive IPC-only identity fields.
- Relay-aware client selects an IPC candidate only when all three local fields are present: `ipc_address`, `server_id`, and `server_instance_id`.
- Runtime accepts a relay-discovered IPC client only when the IPC handshake identity exactly matches the relay candidate identity and the route table contains the requested route name.
- Identity mismatch, missing identity, stale route, or failed IPC connect falls back to HTTP route selection. It must not invoke a CRM method over the mismatched IPC client.
- Explicit direct IPC remains unchanged: `cc.connect(..., address="ipc://...")` bypasses relay and does not require relay response metadata.
- Thread-local same-process calls remain zero-serialization Python object calls.
- Remote HTTP data-plane calls still use the selected `route.relay_url` directly, not `client -> anchor relay -> remote relay` forwarding.

## Security And Locality Model

`C2_RELAY_ANCHOR_ADDRESS` is the SDK process's relay anchor for control-plane registration and name resolution. A loopback URL is an optimization signal, not cryptographic proof that a relay is physically local; loopback port-forwarding can make a remote relay appear local. Therefore the correctness boundary cannot rely on URL host checks alone.

This plan makes the IPC fast path safe by requiring endpoint identity proof from the IPC handshake before any CRM request is sent. A malicious or stale relay response may cause a handshake attempt to an address under `/tmp/c_two_ipc`, but the client must release the connection and fall back to HTTP unless the handshake proves the exact expected server instance. There is no CRM data-plane call before identity validation.

## File Responsibility Map

- `core/protocol/c2-wire/src/handshake.rs`: canonical IPC handshake version, `ServerIdentity`, server ACK encode/decode, handshake tests.
- `core/protocol/c2-wire/src/tests.rs`: canonical handshake fixture updates and decode/encode regression tests.
- `core/transport/c2-server/src/server.rs`: server identity storage, identity-aware constructors, server handshake ACK identity, lifecycle tests using identity.
- `core/transport/c2-server/src/lib.rs`: public re-export of the server identity type used by runtime and tests.
- `core/transport/c2-ipc/src/client.rs`: store server identity from handshake, expose identity getters, reject missing server identity on connected server ACK.
- `core/transport/c2-ipc/src/sync_client.rs`: sync client identity projection.
- `core/transport/c2-ipc/src/tests.rs`: IPC handshake identity integration tests.
- `core/transport/c2-http/src/relay/types.rs`: local route identity fields on relay entries and resolution results.
- `core/transport/c2-http/src/relay/authority.rs`: registration validation and local route entry construction with identity.
- `core/transport/c2-http/src/relay/state.rs` and `route_table.rs`: route table storage, local-only identity projection, merge sanitization, and digest stability.
- `core/transport/c2-http/src/relay/peer_handlers.rs`: scrub IPC-only identity from peer sync snapshots.
- `core/transport/c2-http/src/relay/router.rs`: `/_register` accepts instance identity; `/_resolve` strips IPC-only fields for non-loopback callers.
- `core/transport/c2-http/src/client/control.rs`: relay control registration request and resolution response structs include identity fields.
- `core/transport/c2-http/src/client/relay_aware.rs`: local IPC candidate selection carries expected identity and never returns address-only IPC targets.
- `core/runtime/c2-runtime/src/identity.rs`: generate `server_instance_id` with `uuid::Uuid::new_v4().simple()`.
- `core/runtime/c2-runtime/src/session.rs`: runtime identity includes server instance identity; relay registration and relay-resolved IPC carry expected identity.
- `core/runtime/c2-runtime/src/outcome.rs`: registration outcomes can project server instance identity for tests/debugging.
- `sdk/python/native/src/server_ffi.rs`: `RustServer` accepts/project server identity, passing it into `c2-server`.
- `sdk/python/native/src/runtime_session_ffi.rs`: `ensure_server_bridge()` passes identity to Python bridge; `connect_via_relay()` verifies relay IPC candidate identity before accepting IPC.
- `sdk/python/src/c_two/transport/server/native.py`: `NativeServerBridge` accepts private identity kwargs from runtime session and forwards them to `RustServer`.
- `sdk/python/tests/integration/test_http_relay.py`: local relay IPC fast path and identity-mismatch fallback coverage.
- `sdk/python/tests/unit/test_ipc_config.py` and `test_runtime_session.py`: boundary/source tests for native-owned identity validation and absence of route-name-only acceptance.
- `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: update issue status and relay IPC identity invariant.
- `AGENTS.md`: document that relay-discovered IPC requires native identity verification; Python must not reintroduce route-name-only trust.

---

## Task 0: Baseline And Dirty-Tree Guard

**Files:**
- Read: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Read: `core/protocol/c2-wire/src/handshake.rs`
- Read: `core/transport/c2-http/src/client/relay_aware.rs`
- Read: `sdk/python/native/src/runtime_session_ffi.rs`
- Read: `sdk/python/tests/integration/test_http_relay.py`

- [ ] **Step 1: Record current worktree**

Run:

```bash
git status --short --branch
```

Expected: branch is `dev-feature`. If there are unrelated user edits, do not revert them. If this plan is the only new file added by this step, keep it staged or committed separately before source implementation begins.

- [ ] **Step 2: Run focused baseline checks**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay
cargo check --manifest-path sdk/python/native/Cargo.toml -q
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/integration/test_http_relay.py \
  sdk/python/tests/unit/test_runtime_session.py \
  -q --timeout=30 -rs
```

Expected: existing tests pass before the new failing tests are added. If a failure is unrelated to this plan, record the failing command and stop before changing production code.

- [ ] **Step 3: Confirm the route-name-only trust site still exists**

Run:

```bash
rg -n "route_names\(\)|select_local_ipc_address|RelayResolvedConnection::Ipc|ipc_address" \
  sdk/python/native/src/runtime_session_ffi.rs \
  core/transport/c2-http/src/client/relay_aware.rs \
  core/runtime/c2-runtime/src/session.rs
```

Expected before implementation: matches include `acquire_relay_ipc_client()` checking only `route_names()` and `RelayResolvedConnection::Ipc { address }` carrying only an address.

---

## Task 1: Add Red Tests For Relay IPC Identity Requirements

**Files:**
- Modify: `core/transport/c2-http/src/client/relay_aware.rs`
- Modify: `sdk/python/tests/unit/test_runtime_session.py`
- Modify: `sdk/python/tests/integration/test_http_relay.py`

- [ ] **Step 1: Add Rust unit tests requiring identity-complete IPC candidates**

Add these tests inside `#[cfg(all(test, feature = "relay"))] mod tests` in `core/transport/c2-http/src/client/relay_aware.rs`:

```rust
#[test]
fn local_ipc_selection_requires_identity_complete_route() {
    let complete = RelayRouteInfo {
        name: "grid".to_string(),
        relay_url: "http://127.0.0.1:8080".to_string(),
        ipc_address: Some("ipc://grid-server".to_string()),
        server_id: Some("grid-server".to_string()),
        server_instance_id: Some("inst-a".to_string()),
        crm_ns: String::new(),
        crm_ver: String::new(),
    };
    let missing_instance = RelayRouteInfo {
        server_instance_id: None,
        ..complete.clone()
    };
    let missing_server_id = RelayRouteInfo {
        server_id: None,
        ..complete.clone()
    };

    assert_eq!(
        select_local_ipc_candidate(true, true, &[missing_instance]),
        None,
    );
    assert_eq!(
        select_local_ipc_candidate(true, true, &[missing_server_id]),
        None,
    );
    assert_eq!(
        select_local_ipc_candidate(true, true, &[complete.clone()]),
        Some(RelayLocalIpcCandidate {
            address: "ipc://grid-server".to_string(),
            server_id: "grid-server".to_string(),
            server_instance_id: "inst-a".to_string(),
        }),
    );
}

#[test]
fn relay_resolved_ipc_target_carries_expected_identity() {
    let route = RelayRouteInfo {
        name: "grid".to_string(),
        relay_url: "http://127.0.0.1:8080".to_string(),
        ipc_address: Some("ipc://grid-server".to_string()),
        server_id: Some("grid-server".to_string()),
        server_instance_id: Some("inst-a".to_string()),
        crm_ns: String::new(),
        crm_ver: String::new(),
    };

    let candidate = select_local_ipc_candidate(true, true, &[route]).expect("candidate");
    assert_eq!(candidate.address, "ipc://grid-server");
    assert_eq!(candidate.server_id, "grid-server");
    assert_eq!(candidate.server_instance_id, "inst-a");
}
```

Expected before implementation: compilation fails because `RelayRouteInfo` has no identity fields and `select_local_ipc_candidate` / `RelayLocalIpcCandidate` do not exist.

- [ ] **Step 2: Add Python source guard against route-name-only relay IPC acceptance**

Add this test to `sdk/python/tests/unit/test_runtime_session.py`:

```python
def test_relay_ipc_acceptance_does_not_trust_route_name_only() -> None:
    repo_root = Path(__file__).resolve().parents[4]
    source = (
        repo_root / 'sdk/python/native/src/runtime_session_ffi.rs'
    ).read_text(encoding='utf-8')
    acquire_body = source.split('fn acquire_relay_ipc_client(', 1)[1].split(
        'fn acquire_relay_http_client(',
        1,
    )[0]

    assert 'expected_server_id' in acquire_body
    assert 'expected_server_instance_id' in acquire_body
    assert 'server_identity()' in acquire_body or 'server_instance_id()' in acquire_body
    assert acquire_body.find('server_instance_id') < acquire_body.find('route_names()')
```

Expected before implementation: FAIL because the native code checks route names without server identity.

- [ ] **Step 3: Add a native source guard for fallback on identity mismatch**

Add this test to `sdk/python/tests/unit/test_runtime_session.py` next to `test_relay_ipc_acceptance_does_not_trust_route_name_only`:

```python
def test_relay_ipc_identity_mismatch_falls_back_to_http_not_hard_error() -> None:
    repo_root = Path(__file__).resolve().parents[4]
    source = (
        repo_root / 'sdk/python/native/src/runtime_session_ffi.rs'
    ).read_text(encoding='utf-8')
    connect_body = source.split('fn connect_via_relay(', 1)[1].split(
        'fn shutdown_ipc_clients(',
        1,
    )[0]

    assert 'RelayIpcConnectError::Unavailable' in connect_body
    assert 'acquire_relay_http_client' in connect_body
    assert connect_body.find('RelayIpcConnectError::Unavailable') < connect_body.find('acquire_relay_http_client')
```

Expected before implementation: PASS for the existing fallback branch, then Task 6 tightens the branch so identity mismatch maps to `Unavailable` instead of being accepted as IPC. This test prevents a future hard failure when a stale local IPC candidate is encountered and an HTTP route is still available.

- [ ] **Step 4: Verify red tests fail for the intended reason**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay local_ipc_selection_requires_identity_complete_route
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_runtime_session.py::test_relay_ipc_acceptance_does_not_trust_route_name_only \
  -q --timeout=30
```

Expected before implementation: Rust compile failure for missing identity fields/candidate type and Python assertion failure for missing identity verification.

---

## Task 2: Make IPC Handshake Carry Server Identity

**Files:**
- Modify: `core/protocol/c2-wire/src/handshake.rs`
- Modify: `core/protocol/c2-wire/src/tests.rs`

- [ ] **Step 1: Add failing handshake codec tests**

Add tests near existing handshake tests in `core/protocol/c2-wire/src/tests.rs`:

```rust
#[test]
fn server_handshake_roundtrip_includes_server_identity() {
    use crate::handshake::*;

    let routes = vec![RouteInfo {
        name: "grid".to_string(),
        methods: vec![MethodEntry {
            name: "ping".to_string(),
            index: 0,
        }],
    }];
    let identity = ServerIdentity {
        server_id: "grid-server".to_string(),
        server_instance_id: "inst-001".to_string(),
    };

    let encoded = encode_server_handshake(
        &[],
        CAP_CALL_V2 | CAP_METHOD_IDX,
        &routes,
        "/cc3rtest",
        &identity,
    )
    .expect("server handshake encodes");
    let decoded = decode_handshake(&encoded).expect("server handshake decodes");

    assert_eq!(decoded.server_identity.as_ref(), Some(&identity));
    assert_eq!(decoded.routes, routes);
}

#[test]
fn client_handshake_has_no_server_identity() {
    use crate::handshake::*;

    let encoded = encode_client_handshake(&[], CAP_CALL_V2, "/cc3ctest")
        .expect("client handshake encodes");
    let decoded = decode_handshake(&encoded).expect("client handshake decodes");

    assert_eq!(decoded.server_identity, None);
    assert!(decoded.routes.is_empty());
}
```

- [ ] **Step 2: Implement handshake v7 identity shape**

In `core/protocol/c2-wire/src/handshake.rs`, make these structural changes:

```rust
pub const HANDSHAKE_VERSION: u8 = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    pub server_id: String,
    pub server_instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    pub prefix: String,
    pub segments: Vec<(String, u32)>,
    pub capability_flags: u16,
    pub server_identity: Option<ServerIdentity>,
    pub routes: Vec<RouteInfo>,
}
```

Change the server encoder signature:

```rust
pub fn encode_server_handshake(
    segments: &[(String, u32)],
    capability_flags: u16,
    routes: &[RouteInfo],
    prefix: &str,
    identity: &ServerIdentity,
) -> Result<Vec<u8>, EncodeError>
```

After `encode_client_handshake(...)`, append identity fields before route count:

```rust
validate_name_len("server_id", &identity.server_id)?;
validate_name_len("server_instance_id", &identity.server_instance_id)?;
let server_id_b = identity.server_id.as_bytes();
buf.push(server_id_b.len() as u8);
buf.extend_from_slice(server_id_b);
let instance_b = identity.server_instance_id.as_bytes();
buf.push(instance_b.len() as u8);
buf.extend_from_slice(instance_b);
buf.extend_from_slice(&(routes.len() as u16).to_le_bytes());
```

In `decode_handshake`, after reading `capability_flags`, treat no remaining bytes as a client handshake:

```rust
if off == buf.len() {
    return Ok(Handshake {
        prefix,
        segments,
        capability_flags,
        server_identity: None,
        routes: Vec::new(),
    });
}
```

Then decode `server_id`, `server_instance_id`, and routes. Return `server_identity: Some(ServerIdentity { ... })`.

- [ ] **Step 3: Update all Rust call sites to pass identity**

Run:

```bash
rg -n "encode_server_handshake\(" core
```

Update each call. In tests that do not care about identity, use:

```rust
let identity = ServerIdentity {
    server_id: "test-server".to_string(),
    server_instance_id: "test-instance".to_string(),
};
```

- [ ] **Step 4: Run c2-wire verification**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire
```

Expected: all c2-wire tests pass after fixture updates. No decoder support for handshake v6 remains.

---

## Task 3: Publish Server Identity Through c2-server And c2-ipc

**Files:**
- Modify: `core/transport/c2-server/src/server.rs`
- Modify: `core/transport/c2-server/src/lib.rs`
- Modify: `core/transport/c2-ipc/src/client.rs`
- Modify: `core/transport/c2-ipc/src/sync_client.rs`
- Modify: `core/transport/c2-ipc/src/tests.rs`

- [ ] **Step 1: Add client identity projection tests**

Add a unit test in `core/transport/c2-ipc/src/client.rs` inside a `#[cfg(test)] mod tests` block at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_projects_server_identity() {
        let identity = c2_wire::handshake::ServerIdentity {
            server_id: "identity-server".to_string(),
            server_instance_id: "identity-instance".to_string(),
        };
        let mut client = IpcClient::new("ipc://identity_projection");
        client.server_identity = Some(identity.clone());

        assert_eq!(client.server_identity(), Some(&identity));
        assert_eq!(client.server_id(), Some("identity-server"));
        assert_eq!(client.server_instance_id(), Some("identity-instance"));
    }
}
```

Add the equivalent sync projection test in `core/transport/c2-ipc/src/sync_client.rs` inside its existing `#[cfg(test)] pub(crate) mod tests`:

```rust
#[test]
fn sync_client_projects_server_identity() {
    let identity = c2_wire::handshake::ServerIdentity {
        server_id: "identity-server".to_string(),
        server_instance_id: "identity-instance".to_string(),
    };
    let mut client = SyncClient::new_unconnected("ipc://identity_projection_sync");
    client.inner.server_identity = Some(identity.clone());

    assert_eq!(client.server_identity(), Some(&identity));
    assert_eq!(client.server_id(), Some("identity-server"));
    assert_eq!(client.server_instance_id(), Some("identity-instance"));
}
```

Make `IpcClient.server_identity` `pub(crate)` so `sync_client.rs` tests can set it without introducing production setters.

Expected before implementation: compilation fails because `server_identity` storage and getters do not exist.

- [ ] **Step 2: Add server identity storage and constructors**

In `core/transport/c2-server/src/server.rs`, add a field to `Server`:

```rust
identity: c2_wire::handshake::ServerIdentity,
```

Add constructors:

```rust
impl Server {
    pub fn new(address: &str, config: ServerIpcConfig) -> Result<Self, ServerError> {
        let server_id = server_id_from_ipc_address(address)?;
        let identity = c2_wire::handshake::ServerIdentity {
            server_id,
            server_instance_id: uuid::Uuid::new_v4().simple().to_string(),
        };
        Self::new_with_identity(address, config, identity)
    }

    pub fn new_with_identity(
        address: &str,
        config: ServerIpcConfig,
        identity: c2_wire::handshake::ServerIdentity,
    ) -> Result<Self, ServerError> {
        validate_server_identity(&identity)?;
        // Keep the current allocation, socket-path, lifecycle, dispatcher, and pool initialization logic here, then assign the validated `identity` field in the returned `Server`.
    }

    pub fn identity(&self) -> &c2_wire::handshake::ServerIdentity {
        &self.identity
    }

    pub fn server_id(&self) -> &str {
        &self.identity.server_id
    }

    pub fn server_instance_id(&self) -> &str {
        &self.identity.server_instance_id
    }
}
```

Add `uuid = { version = "1", features = ["v4"] }` to `core/transport/c2-server/Cargo.toml`. Validate both identity fields with existing server-id/region-id constraints where possible; `server_instance_id` must be non-empty ASCII without `/`, `\0`, or path separators.

- [ ] **Step 3: Include identity in server handshake ACK**

In `handle_handshake` in `core/transport/c2-server/src/server.rs`, change:

```rust
let hs_bytes = encode_server_handshake(&server_segments, cap, &routes, &server_prefix)
```

to:

```rust
let hs_bytes = encode_server_handshake(
    &server_segments,
    cap,
    &routes,
    &server_prefix,
    server.identity(),
)
```

- [ ] **Step 4: Store handshake identity in IPC clients**

In `core/transport/c2-ipc/src/client.rs`, add:

```rust
server_identity: Option<c2_wire::handshake::ServerIdentity>,
```

to `Client`, populate it after `do_handshake`, and expose:

```rust
pub fn server_identity(&self) -> Option<&c2_wire::handshake::ServerIdentity> {
    self.server_identity.as_ref()
}

pub fn server_id(&self) -> Option<&str> {
    self.server_identity.as_ref().map(|identity| identity.server_id.as_str())
}

pub fn server_instance_id(&self) -> Option<&str> {
    self.server_identity
        .as_ref()
        .map(|identity| identity.server_instance_id.as_str())
}
```

In `SyncClient`, mirror these projections by delegating to `self.inner`.

- [ ] **Step 5: Run server and IPC tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server
cargo test --manifest-path core/Cargo.toml -p c2-ipc
```

Expected: all tests pass. Handshake fixture failures must be updated through the v7 shape, not by adding v6 fallback logic.

---

## Task 4: Carry Identity Through Relay Registration And Resolution

**Files:**
- Modify: `core/transport/c2-http/src/relay/types.rs`
- Modify: `core/transport/c2-http/src/relay/authority.rs`
- Modify: `core/transport/c2-http/src/relay/route_table.rs`
- Modify: `core/transport/c2-http/src/relay/state.rs`
- Modify: `core/transport/c2-http/src/relay/peer_handlers.rs`
- Modify: `core/transport/c2-http/src/relay/router.rs`
- Modify: `core/transport/c2-http/src/client/control.rs`
- Modify: `core/runtime/c2-runtime/src/session.rs`

- [ ] **Step 1: Add relay identity projection tests**

In `core/transport/c2-http/src/relay/router.rs`, extend the existing `resolve_exposes_ipc_address_only_to_loopback_clients` test assertions:

```rust
assert_eq!(routes[0].server_id.as_deref(), Some("server-grid"));
assert_eq!(routes[0].server_instance_id.as_deref(), Some("inst-grid"));
```

For the non-loopback response, assert:

```rust
assert_eq!(routes[0].ipc_address, None);
assert_eq!(routes[0].server_id, None);
assert_eq!(routes[0].server_instance_id, None);
```

Update the test registration call so it passes `server_instance_id: "inst-grid"` through the same API production registration uses.

- [ ] **Step 2: Extend relay route structs**

In `core/transport/c2-http/src/relay/types.rs`, add fields:

```rust
pub struct RouteEntry {
    pub name: String,
    pub relay_id: String,
    pub relay_url: String,
    pub server_id: Option<String>,
    pub server_instance_id: Option<String>,
    pub ipc_address: Option<String>,
    pub crm_ns: String,
    pub crm_ver: String,
    pub locality: Locality,
    pub registered_at: f64,
}

pub struct RouteInfo {
    pub name: String,
    pub relay_url: String,
    pub ipc_address: Option<String>,
    pub server_id: Option<String>,
    pub server_instance_id: Option<String>,
    pub crm_ns: String,
    pub crm_ver: String,
}
```

In `RouteEntry::to_route_info()`, expose all IPC-only fields only for `Locality::Local`:

```rust
let local = self.locality == Locality::Local;
let ipc_address = if local { self.ipc_address.clone() } else { None };
let server_id = if local { self.server_id.clone() } else { None };
let server_instance_id = if local {
    self.server_instance_id.clone()
} else {
    None
};
```

- [ ] **Step 3: Extend relay register request**

In `core/transport/c2-http/src/client/control.rs`, extend `RegisterRequest` and `RelayControlClient::register`:

```rust
struct RegisterRequest<'a> {
    name: &'a str,
    server_id: &'a str,
    server_instance_id: &'a str,
    address: &'a str,
    crm_ns: &'a str,
    crm_ver: &'a str,
}

pub fn register(
    &self,
    name: &str,
    server_id: &str,
    server_instance_id: &str,
    address: &str,
    crm_ns: &str,
    crm_ver: &str,
) -> Result<(), HttpError>
```

Also extend `RelayRouteInfo`:

```rust
pub struct RelayRouteInfo {
    pub name: String,
    pub relay_url: String,
    pub ipc_address: Option<String>,
    pub server_id: Option<String>,
    pub server_instance_id: Option<String>,
    pub crm_ns: String,
    pub crm_ver: String,
}
```

- [ ] **Step 4: Validate and store server instance identity in relay authority**

In relay register preparation/commit code, require non-empty `server_instance_id` for local registrations. The error should be `400 BAD_REQUEST` with a message containing `invalid server_instance_id` when the field is empty or contains path separators. Local route entries must store:

```rust
server_id: Some(server_id.clone()),
server_instance_id: Some(server_instance_id.clone()),
ipc_address: Some(address.clone()),
```

Peer routes created by gossip/anti-entropy must keep:

```rust
server_id: None,
server_instance_id: None,
ipc_address: None,
```

- [ ] **Step 5: Strip IPC-only identity for non-loopback resolve callers**

In `handle_resolve` in `core/transport/c2-http/src/relay/router.rs`, extend the existing strip loop:

```rust
if !expose_ipc_address {
    for route in &mut routes {
        route.ipc_address = None;
        route.server_id = None;
        route.server_instance_id = None;
    }
}
```

- [ ] **Step 6: Scrub IPC-only identity from relay mesh snapshots and merges**

In `core/transport/c2-http/src/relay/peer_handlers.rs`, extend `scrub_peer_snapshot`:

```rust
fn scrub_peer_snapshot(mut snapshot: FullSync) -> FullSync {
    for entry in snapshot.routes.iter_mut() {
        entry.ipc_address = None;
        entry.server_id = None;
        entry.server_instance_id = None;
    }
    snapshot
        .tombstones
        .retain(|tombstone| tombstone.relay_id != "");
    snapshot
}
```

In `core/transport/c2-http/src/relay/route_table.rs`, extend `merge_snapshot` sanitization:

```rust
entry.ipc_address = None;
entry.server_id = None;
entry.server_instance_id = None;
```

Add assertions to the existing peer snapshot and merge tests:

```rust
assert_eq!(snapshot.routes[0].server_instance_id, None);
assert_eq!(rt_b.list_routes()[0].server_instance_id, None);
```

Add a digest stability assertion beside the existing `route_digest_stable_when_only_ipc_address_changes` coverage:

```rust
let before = rt.route_digest();
let mut entry = rt.local_route("grid").expect("local route");
entry.server_instance_id = Some("changed-instance".into());
rt.register_route(entry);
assert_eq!(rt.route_digest(), before);
```

The route digest must not include `server_id`, `server_instance_id`, or `ipc_address`, because those fields differ between the owning relay and peer relays.

- [ ] **Step 7: Pass runtime identity during relay registration**

In `core/runtime/c2-runtime/src/session.rs`, add `server_instance_id` to `RuntimeIdentity` and pass it to `RelayControlClient::register(...)`:

```rust
projection.control.register(
    &spec.name,
    &identity.server_id,
    &identity.server_instance_id,
    &identity.ipc_address,
    &spec.crm_ns,
    &spec.crm_ver,
)
```

- [ ] **Step 8: Run relay tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay
cargo test --manifest-path core/Cargo.toml -p c2-runtime
```

Expected: local loopback resolve returns identity fields; non-loopback resolve, peer snapshots, peer merges, and peer routes do not expose IPC-only identity.

---

## Task 5: Make Relay-Aware Target Selection Identity-Aware

**Files:**
- Modify: `core/transport/c2-http/src/client/relay_aware.rs`
- Modify: `core/runtime/c2-runtime/src/session.rs`
- Modify: `core/runtime/c2-runtime/src/lib.rs`

- [ ] **Step 1: Replace address-only IPC target with identity candidate**

In `core/transport/c2-http/src/client/relay_aware.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayLocalIpcCandidate {
    pub address: String,
    pub server_id: String,
    pub server_instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayResolvedTarget {
    Ipc { candidate: RelayLocalIpcCandidate },
    Http { relay_url: String },
}
```

Replace `select_local_ipc_address(...) -> Option<String>` with:

```rust
fn select_local_ipc_candidate(
    prefer_local_ipc: bool,
    anchor_allows_local_ipc: bool,
    routes: &[RelayRouteInfo],
) -> Option<RelayLocalIpcCandidate> {
    if !prefer_local_ipc || !anchor_allows_local_ipc {
        return None;
    }
    routes.iter().find_map(|route| {
        Some(RelayLocalIpcCandidate {
            address: route.ipc_address.clone()?,
            server_id: route.server_id.clone()?,
            server_instance_id: route.server_instance_id.clone()?,
        })
    })
}
```

- [ ] **Step 2: Update target selection logic**

Change the local IPC branch in `select_target_async`:

```rust
if let Some(candidate) =
    select_local_ipc_candidate(prefer_local_ipc, self.anchor_allows_local_ipc, &routes)
{
    return Ok(RelayResolvedTarget::Ipc { candidate });
}
```

The HTTP route ordering and `route.relay_url` path must remain unchanged.

- [ ] **Step 3: Extend runtime resolved connection enum**

In `core/runtime/c2-runtime/src/session.rs`, change:

```rust
pub enum RelayResolvedConnection {
    Ipc { address: String },
    Http { client: RelayAwareHttpClient, relay_url: String },
}
```

to:

```rust
pub enum RelayResolvedConnection {
    Ipc {
        address: String,
        server_id: String,
        server_instance_id: String,
    },
    Http {
        client: RelayAwareHttpClient,
        relay_url: String,
    },
}
```

Map `RelayResolvedTarget::Ipc { candidate }` into the expanded enum.

- [ ] **Step 4: Run focused target-selection tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay local_ipc_selection_requires_identity_complete_route
cargo test --manifest-path core/Cargo.toml -p c2-runtime
```

Expected: Rust unit tests pass and runtime compiles with the expanded IPC candidate shape.

---

## Task 6: Verify IPC Handshake Identity Before Accepting Relay IPC

**Files:**
- Modify: `sdk/python/native/src/runtime_session_ffi.rs`
- Modify: `sdk/python/native/src/client_ffi.rs`
- Test: `sdk/python/tests/unit/test_runtime_session.py`
- Test: `sdk/python/tests/integration/test_http_relay.py`

- [ ] **Step 1: Project server identity from native IPC clients if missing**

If `PyRustClient` in `sdk/python/native/src/client_ffi.rs` does not already expose identity, add getters:

```rust
#[getter]
fn server_id(&self) -> Option<String> {
    self.inner.server_id().map(ToOwned::to_owned)
}

#[getter]
fn server_instance_id(&self) -> Option<String> {
    self.inner.server_instance_id().map(ToOwned::to_owned)
}
```

These getters are for tests/debugging and are not used as Python-owned authority.

- [ ] **Step 2: Expand relay IPC acquisition signature**

In `sdk/python/native/src/runtime_session_ffi.rs`, change:

```rust
fn acquire_relay_ipc_client(
    &self,
    py: Python<'_>,
    route_name: &str,
    address: &str,
) -> Result<Arc<SyncClient>, RelayIpcConnectError>
```

to:

```rust
fn acquire_relay_ipc_client(
    &self,
    py: Python<'_>,
    route_name: &str,
    address: &str,
    expected_server_id: &str,
    expected_server_instance_id: &str,
) -> Result<Arc<SyncClient>, RelayIpcConnectError>
```

Update the `RelayResolvedConnection::Ipc` match to pass the expected fields.

- [ ] **Step 3: Validate identity before route-name check and before freezing config**

Replace the acceptance block with this order:

```rust
let identity_matches = client
    .server_identity()
    .is_some_and(|identity| {
        identity.server_id == expected_server_id
            && identity.server_instance_id == expected_server_instance_id
    });
if !identity_matches {
    pool.release(&addr);
    return Err(RelayIpcConnectError::Unavailable);
}

if !client
    .route_names()
    .into_iter()
    .any(|registered| registered == route_name)
{
    pool.release(&addr);
    return Err(RelayIpcConnectError::Unavailable);
}

self.inner.mark_client_config_frozen();
Ok(client)
```

The order matters:

1. Acquire IPC client.
2. Check identity.
3. Check route name.
4. Freeze client config only after accepting an IPC client.
5. Return IPC client.

A mismatch must not call `mark_client_config_frozen()` and must not perform a CRM request over the IPC connection.

- [ ] **Step 4: Keep fallback behavior HTTP-safe**

For `RelayIpcConnectError::Unavailable`, keep the existing fallback:

```rust
Err(RelayIpcConnectError::Unavailable) => self.acquire_relay_http_client(
    py,
    route_name,
    use_proxy,
    max_attempts,
    call_timeout_secs,
),
```

Do not turn identity mismatch into a user-visible hard failure; stale local IPC candidates are expected to degrade to HTTP when an HTTP route exists.

- [ ] **Step 5: Run Python source guard**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_runtime_session.py::test_relay_ipc_acceptance_does_not_trust_route_name_only \
  -q --timeout=30
```

Expected: PASS after identity validation is present before `route_names()`.

---

## Task 7: Wire Runtime Identity Into Python Server Construction

**Files:**
- Modify: `core/runtime/c2-runtime/src/identity.rs`
- Modify: `core/runtime/c2-runtime/src/session.rs`
- Modify: `core/runtime/c2-runtime/src/outcome.rs`
- Modify: `sdk/python/native/src/runtime_session_ffi.rs`
- Modify: `sdk/python/native/src/server_ffi.rs`
- Modify: `sdk/python/src/c_two/transport/server/native.py`

- [ ] **Step 1: Generate runtime server instance identity**

In `core/runtime/c2-runtime/src/identity.rs`, add:

```rust
pub fn auto_server_instance_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
```

In `RuntimeIdentity`, add:

```rust
pub server_instance_id: String,
```

In `RuntimeSession::ensure_server()`, assign:

```rust
let identity = RuntimeIdentity {
    ipc_address: ipc_address_for_server_id(&server_id),
    server_id,
    server_instance_id: auto_server_instance_id(),
};
```

In `core/runtime/c2-runtime/src/outcome.rs`, add the same field to `RegisterOutcome`:

```rust
pub server_instance_id: String,
```

When `RuntimeSession::register_route(...)` builds `RegisterOutcome`, set it from the same `RuntimeIdentity` used for relay registration. In `sdk/python/native/src/runtime_session_ffi.rs`, project it for tests/debugging:

```rust
dict.set_item("server_instance_id", outcome.server_instance_id)?;
```

Ensure repeated `ensure_server()` calls return the same instance ID for the same runtime session.

- [ ] **Step 2: Pass identity from RuntimeSession to NativeServerBridge**

In `sdk/python/native/src/runtime_session_ffi.rs`, inside `ensure_server_bridge()`, add:

```rust
kwargs.set_item("server_id", identity.server_id)?;
kwargs.set_item("server_instance_id", identity.server_instance_id)?;
```

- [ ] **Step 3: Forward identity through Python bridge**

In `sdk/python/src/c_two/transport/server/native.py`, extend `NativeServerBridge.__init__` with keyword-only args:

```python
server_id: str | None = None,
server_instance_id: str | None = None,
```

Forward them to `RustServer`:

```python
self._rust_server = RustServer(
    address=bind_address,
    server_id=server_id,
    server_instance_id=server_instance_id,
    shm_threshold=int(self._config['shm_threshold']),
    ...
)
```

These kwargs are internal runtime-session plumbing. Do not document them as general SDK user configuration.

- [ ] **Step 4: Accept identity in RustServer constructor**

In `sdk/python/native/src/server_ffi.rs`, extend the PyO3 constructor signature:

```rust
#[pyo3(signature = (
    address,
    shm_threshold,
    pool_enabled,
    pool_segment_size,
    max_pool_segments,
    reassembly_segment_size,
    reassembly_max_segments,
    max_frame_size,
    max_payload_size,
    max_pending_requests,
    pool_decay_seconds,
    heartbeat_interval,
    heartbeat_timeout,
    max_total_chunks,
    chunk_gc_interval,
    chunk_threshold_ratio,
    chunk_assembler_timeout,
    max_reassembly_bytes,
    chunk_size,
    server_id=None,
    server_instance_id=None,
))]
```

Build the server as:

```rust
let server = match (server_id, server_instance_id) {
    (Some(server_id), Some(server_instance_id)) => Server::new_with_identity(
        address,
        config,
        c2_server::ServerIdentity {
            server_id,
            server_instance_id,
        },
    ),
    (None, None) => Server::new(address, config),
    _ => Err(c2_server::ServerError::Config(
        "server_id and server_instance_id must be provided together".to_string(),
    )),
}
.map_err(|e| PyRuntimeError::new_err(format!("{e}")))?;
```

- [ ] **Step 5: Add runtime identity tests**

Add to `sdk/python/tests/unit/test_runtime_session.py`:

```python
def test_runtime_session_passes_server_instance_identity_to_bridge(monkeypatch) -> None:
    captured: dict[str, object] = {}

    class FakeBridge:
        def __init__(self, **kwargs: object) -> None:
            captured.update(kwargs)

    monkeypatch.setattr(
        'c_two.transport.server.native.NativeServerBridge',
        FakeBridge,
    )

    from c_two._native import RuntimeSession

    session = RuntimeSession(shm_threshold=4096)
    session.ensure_server_bridge()

    assert isinstance(captured['server_id'], str)
    assert isinstance(captured['server_instance_id'], str)
    assert captured['server_id']
    assert captured['server_instance_id']
    assert captured['server_id'] != captured['server_instance_id']
```

If monkeypatching `NativeServerBridge` through import does not intercept native import semantics, use the existing test pattern in this file for runtime bridge construction guards.

- [ ] **Step 6: Run native and runtime tests**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_runtime_session.py -q --timeout=30
```

Expected: native bindings compile and runtime identity tests pass.

---

## Task 8: End-To-End Relay Fast Path And Fallback Verification

**Files:**
- Modify: `sdk/python/tests/integration/test_http_relay.py`
- Modify: `sdk/python/tests/integration/test_direct_ipc_control.py`
- Modify: `sdk/python/tests/unit/test_ipc_config.py`

- [ ] **Step 1: Assert local relay name-only connect can still use verified IPC**

Add this integration test to `sdk/python/tests/integration/test_http_relay.py`:

```python
def test_name_only_connect_uses_verified_ipc_for_local_anchor(start_c3_relay):
    name = 'identity/verified/local-ipc'
    relay = start_c3_relay()
    relay_url = relay.url
    previous_relay = settings.relay_anchor_address
    registrar = _ProcessRegistry()
    resolver = _ProcessRegistry()
    crm = None
    try:
        registrar.set_relay_anchor(relay_url)
        registrar.register(Hello, HelloImpl(), name=name)
        resolver.set_relay_anchor(relay_url)

        crm = resolver.connect(Hello, name=name)

        assert crm.client._mode == 'ipc'  # noqa: SLF001
        assert crm.greeting('Identity') == 'Hello, Identity!'
    finally:
        if crm is not None:
            resolver.close(crm)
        resolver.shutdown()
        registrar.shutdown()
        settings.relay_anchor_address = previous_relay
```

This test uses the existing `_ProcessRegistry()` isolation pattern in `test_http_relay.py`. It validates the fast path remains available after identity verification; Task 6 source guards validate that mismatched identity falls back before a CRM call.

- [ ] **Step 2: Assert explicit IPC remains relay-independent**

Keep or add this coverage in `sdk/python/tests/integration/test_direct_ipc_control.py`:

```python
def test_explicit_ipc_connect_ignores_bad_relay_anchor(monkeypatch):
    import c_two as cc

    monkeypatch.setenv('C2_RELAY_ANCHOR_ADDRESS', 'http://127.0.0.1:9')

    @cc.crm(namespace='test.identity.direct', version='0.1.0')
    class Direct:
        def ping(self) -> str:
            ...

    class DirectResource:
        def ping(self) -> str:
            return 'direct-ipc'

    cc.register(Direct, DirectResource(), name='direct-identity')
    address = cc.server_address()
    client = cc.connect(Direct, name='direct-identity', address=address)

    assert client.ping() == 'direct-ipc'
```

Expected: explicit IPC path does not consult relay resolve or relay identity metadata.

- [ ] **Step 3: Run integration tests with relay binary present**

Run:

```bash
python tools/dev/c3_tool.py --build --link
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/integration/test_http_relay.py \
  sdk/python/tests/integration/test_direct_ipc_control.py \
  -q --timeout=30 -rs
```

Expected: relay tests run instead of skipping due missing `c3`; local fast path and direct IPC independence pass.

---

## Task 9: Documentation And Boundary Guards

**Files:**
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
- Modify: `AGENTS.md`
- Modify: `sdk/python/tests/unit/test_ipc_config.py`
- Modify: `sdk/python/tests/unit/test_runtime_session.py`

- [ ] **Step 1: Update thin SDK boundary document**

In `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`, update the runtime-session / relay projection verified coverage section with:

```markdown
- Relay-discovered direct IPC is accepted only after native IPC handshake identity
  matches the relay route's `server_id` and `server_instance_id`; route-name-only
  acceptance is forbidden.
- Relay `/_resolve` exposes `ipc_address`, `server_id`, and `server_instance_id`
  only to loopback callers for local routes. Non-loopback callers and peer routes
  receive HTTP route targets without IPC-only fields.
```

- [ ] **Step 2: Update AGENTS.md**

Add to the Transport Layer / Direct IPC section:

```markdown
Relay-discovered IPC fast paths must validate endpoint identity in Rust before
accepting the IPC client. The relay response identity (`server_id` and
`server_instance_id`) must match the identity returned by the IPC handshake, and
then the requested route name must exist. Do not reintroduce Python-side or
route-name-only trust decisions for relay IPC.
```

- [ ] **Step 3: Add source boundary test preventing regression**

Add to `sdk/python/tests/unit/test_runtime_session.py`:

```python
def test_relay_ipc_identity_boundary_is_native_owned() -> None:
    repo_root = Path(__file__).resolve().parents[4]
    registry_source = (
        repo_root / 'sdk/python/src/c_two/transport/registry.py'
    ).read_text(encoding='utf-8')
    native_source = (
        repo_root / 'sdk/python/native/src/runtime_session_ffi.rs'
    ).read_text(encoding='utf-8')

    assert 'server_instance_id' not in registry_source
    assert 'expected_server_instance_id' in native_source
    assert 'route_names()' in native_source
```

This test allows Python to route by native `mode`, but prevents Python from owning endpoint identity checks.

- [ ] **Step 4: Run docs/source guards**

Run:

```bash
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_runtime_session.py \
  sdk/python/tests/unit/test_ipc_config.py \
  -q --timeout=30
rg -n "route-name-only|server_instance_id|relay-discovered" AGENTS.md docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md
```

Expected: tests pass and docs contain the new invariant.

---

## Task 10: Full Verification And Strict Review

**Files:**
- Read all modified files from the File Responsibility Map.

- [ ] **Step 1: Run Rust verification**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire
cargo test --manifest-path core/Cargo.toml -p c2-server
cargo test --manifest-path core/Cargo.toml -p c2-ipc
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay
cargo test --manifest-path core/Cargo.toml -p c2-runtime
cargo test --manifest-path core/Cargo.toml --workspace
```

Expected: all commands exit 0.

- [ ] **Step 2: Run native/Python verification**

Run:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
uv sync --reinstall-package c-two
python tools/dev/c3_tool.py --build --link
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: native extension builds and Python tests pass. Relay-dependent tests must run if `c3` was built and linked.

- [ ] **Step 3: Run stale-symbol audit**

Run:

```bash
rg -n "HANDSHAKE_VERSION: u8 = 6|encode_server_handshake\([^\n]*&routes,\s*&server_prefix\)|RelayResolvedConnection::Ipc \{ address \}|select_local_ipc_address|route-name-only trust|expected_server_instance_id" \
  core/protocol/c2-wire \
  core/transport/c2-server \
  core/transport/c2-ipc \
  core/transport/c2-http \
  core/runtime/c2-runtime \
  sdk/python/native \
  sdk/python/src \
  sdk/python/tests
```

Expected:

- No `HANDSHAKE_VERSION: u8 = 6`.
- No address-only `RelayResolvedConnection::Ipc { address }`.
- No `select_local_ipc_address`.
- `expected_server_instance_id` appears in native runtime verification code and tests.

- [ ] **Step 4: Run formatting and diff checks**

Run:

```bash
cargo fmt --manifest-path core/Cargo.toml --all --check
cargo fmt --manifest-path sdk/python/native/Cargo.toml --all --check
git diff --check
```

Expected: all commands exit 0.

- [ ] **Step 5: Strict review checklist before committing**

Manually verify each statement against the final diff:

- `c2-wire` server handshake identity is mandatory for server ACK; no v6 compatibility parser remains.
- `c2-server` generated instance IDs are unpredictable and per server instance.
- `c2-ipc` stores identity before exposing a connected client.
- Relay local routes store instance identity; peer routes, peer snapshots, peer merges, route digests, and non-loopback resolve responses do not expose IPC-only identity.
- Relay-aware target selection refuses IPC candidates missing address, server ID, or instance ID.
- PyO3 runtime validates identity before route name and before `mark_client_config_frozen()`.
- Identity mismatch falls back to HTTP without a CRM method call over the mismatched IPC client.
- Explicit `ipc://` connect path remains relay-independent.
- Python does not parse or compare relay IPC identity.
- No zombie `route_names()`-only acceptance path remains.

- [ ] **Step 6: Commit implementation after verification**

Run:

```bash
git add \
  core/protocol/c2-wire \
  core/transport/c2-server \
  core/transport/c2-ipc \
  core/transport/c2-http \
  core/runtime/c2-runtime \
  sdk/python/native \
  sdk/python/src/c_two/transport/server/native.py \
  sdk/python/tests \
  docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md \
  AGENTS.md

git commit -m "fix: verify relay-discovered ipc endpoint identity"
```

Expected: commit succeeds only after all verification commands above have passed.

---

## Strict Self-Review Of This Plan

### Coverage Against The Review Finding

- The finding said route-name-only acceptance can accept the wrong IPC server. Tasks 2, 3, 5, and 6 replace route-name-only acceptance with relay identity plus IPC handshake identity.
- The finding required actionable protection for stale/wrong local IPC endpoints. Task 6 releases mismatched clients and falls back to HTTP before any CRM call.
- The finding required Rust/native ownership. Tasks 3 through 7 keep endpoint identity in Rust crates and PyO3; Python only forwards private construction kwargs and routes by native mode.

### Regression Risk Review

- Direct IPC independence is guarded by Task 8 Step 2.
- HTTP relay performance is preserved because Task 5 leaves `route.relay_url` data-plane selection unchanged.
- Relay mesh privacy is preserved because Task 4 strips identity for peer routes and non-loopback callers.
- Zero-copy payload paths are unaffected because identity is exchanged only during handshake and control-plane resolution.
- Config-freeze behavior is tightened in Task 6 so failed relay IPC probes do not freeze client IPC config.
- 0.x clean cut is explicit: no v6 handshake fallback and no old relay response aliases.

### Known Hard Boundary

A loopback port-forwarded remote relay is indistinguishable from a local TCP relay at URL-parse time. This plan does not pretend to solve physical locality detection through `127.0.0.1`. It solves the correctness and safety boundary that matters for C-Two dispatch: the SDK must not accept or invoke a CRM method on an IPC endpoint unless the endpoint proves the exact relay-advertised server identity. If future deployment requires blocking even the initial local IPC handshake attempt for port-forwarded relays, that needs an explicit relay-local proof channel rather than more URL heuristics.
