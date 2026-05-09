# Runtime Session Rust Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented on `dev-feature`. Rust `c2-runtime::RuntimeSession`
owns process identity, route transactions, direct IPC and HTTP client pool
projection, relay projection, and unregister/shutdown outcomes. Python remains
the CRM/resource binding facade.

**Goal:** Move process runtime/session state from the Python SDK into a Rust-owned `RuntimeSession` while keeping Python as the CRM/resource binding layer and preserving relay-independent direct IPC.

**Architecture:** Add a language-neutral Rust runtime crate that owns the standalone IPC session, process server identity/address, server lifecycle, client pools, route transaction state, and optional relay projection. Python keeps CRM metadata discovery, Python callback dispatch, thread-local direct-call binding, proxy construction, and Python shutdown callbacks. The migration is intentionally phased, but phases are only reviewable engineering slices: every phase has full production-grade invariants and verification, and no phase is allowed to leave a documented no-op, compatibility shim, or split-brain runtime authority.

**Tech Stack:** Rust workspace crate `core/runtime/c2-runtime`, existing Rust crates `c2-config`, `c2-server`, `c2-ipc`, `c2-http`, PyO3 bindings in `sdk/python/native`, Python facade in `sdk/python/src/c_two/transport`, pytest integration/unit tests, Cargo unit tests.

---

## 0.x Constraint: Clean Cut, No Compatibility Debt

C-Two is in the 0.x line. This work must not preserve Python-owned runtime
fallbacks after Rust owns an equivalent mechanism. Remove obsolete Python
state, tests, docs, imports, and module surfaces in the same phase that
replaces them. Do not keep dormant compatibility paths such as a Python
`_pool_config_applied` fallback, Python server-id generation fallback, or
Python relay-control cache after `RuntimeSession` owns those behaviors.

## Industrial-Grade Phase Commitment

Phasing is allowed only to make review and rollback tractable. It is not a
license to land partial-quality behavior, silent no-op configuration, temporary
Python authority, or undocumented follow-up work. Each phase must be implemented
as production code for the runtime concern it claims to move:

- the Rust owner must enforce the behavior, not merely mirror Python state;
- Python must not keep a second source of truth for the moved concern;
- tests must cover success, failure, rollback, direct-IPC-without-relay, and
  bad-relay/direct-IPC regressions whenever the phase touches those paths;
- performance-sensitive paths must preserve thread-local zero-serialization and
  remote SHM zero-copy boundaries;
- every intentionally deferred concern must be named in this document, assigned
  to a later phase, and reflected in the traceability matrix and phase ledger.

Do not close a phase with verbal handoff only. The plan is the durable handoff
artifact; if a review discovers a gap, update this file before continuing.

## Non-Negotiable Runtime Constraints

1. **Direct IPC is a complete standalone mode.** `cc.register(...)` without a
   relay must create a usable local IPC server, expose `cc.server_address()`,
   and support explicit `cc.connect(..., address="ipc://...")`.
2. **Relay is only a projection.** Relay registration, unregistration, and
   name resolution are optional projections above the base IPC session. Relay
   must never own server identity, IPC server lifecycle, direct IPC client
   pooling, route handles, or base route transaction state.
3. **Explicit `ipc://` bypasses relay.** A bad `C2_RELAY_ANCHOR_ADDRESS` or bad
   `cc.set_relay_anchor(...)` value must not affect explicit direct IPC connections.
4. **Thread-local calls remain Python direct calls.** Same-process
   `cc.connect(CRMClass, name="route")` must still pass Python objects directly
   and must not be routed through serialized Rust bytes dispatch.
5. **Zero-copy SHM boundaries remain intact.** Remote IPC dispatch must keep
   the path `RequestData::Shm` / `RequestData::Handle` → `PyShmBuffer` →
   Python `memoryview`; the session migration must not materialize SHM or
   handle payloads into Python `bytes` before invoking resource code.
6. **Rust is the runtime transaction authority.** Once a phase moves a runtime
   concern into `RuntimeSession`, Python must not keep an independent copy that
   can disagree with Rust.
7. **Structured outcomes, not ambiguous exceptions.** Native unregister and
   shutdown APIs must return enough information for Python to perform
   language-specific cleanup exactly once while preserving existing control
   semantics.
8. **Industrial-grade verification for every phase.** Every phase must include
   Rust unit tests, Python unit tests, Python integration tests, relay tests
   when relay behavior is touched, bad-relay/direct-IPC regressions, and
   full-suite verification before the phase is considered complete.

## Current Problem

At the start of this plan, `sdk/python/src/c_two/transport/registry.py` still
acted as the process runtime authority. It owned:

- HTTP pool handles;
- relay control client cache;
- unregister ordering and relay cleanup error propagation;
- shutdown ordering and best-effort relay cleanup.

These are language-neutral runtime/session mechanics. Future SDKs should not
have to reimplement them. Python should stay as the language binding facade and
resource invocation layer.

## Target Ownership

```text
Rust RuntimeSession
  ├── StandaloneIpcSession
  │   ├── server identity/address
  │   ├── lazy direct IPC server lifecycle
  │   ├── route registration transactions
  │   ├── direct IPC route handles
  │   ├── direct IPC client pool and default client config
  │   └── local unregister/shutdown transaction state
  └── Optional RelayProjection
      ├── relay address resolution / explicit override
      ├── relay control client cache
      ├── relay register/unregister projection
      └── relay name-resolution projection

Python ProcessRegistry facade
  ├── CRM class/method discovery
  ├── Python dispatcher/callback construction
  ├── Python direct-instance table for thread-local fast path
  ├── Python proxy construction
  ├── Python @on_shutdown callback invocation
  └── cc.serve() banner/signal UX
```

The base `StandaloneIpcSession` must be usable when `RelayProjection` is absent,
misconfigured, or unreachable.

## Traceability Matrix

This matrix is the follow-up guardrail. Do not close the RuntimeSession work
until every current Python-owned runtime concern has a Rust owner, tests, and a
documented deletion of the stale Python authority.

| Current Python-owned concern | Rust target owner | Phase | Python deletion / reduction |
| --- | --- | --- | --- |
| generated server id | `c2-runtime::identity` | 1 | delete `_auto_server_id()` and Python `uuid` / pid-based generation |
| `ipc://` server address derivation | `RuntimeSession::ensure_server()` / identity helper | 1 | delete `_address_for_server_id()` and `_server_address` authority |
| lazy direct IPC server creation | `RuntimeSession` bridge acquisition now; `StandaloneIpcSession` later | 1, 3 | Phase 1 removes registry-side bridge construction; Phase 3 moves route/server transaction authority |
| server IPC override freeze | `RuntimeSessionOptions` / session state | 1 | Python forwards typed overrides and reads native projection only |
| direct IPC client pool | `RuntimeSession` owned `c2_ipc::ClientPool` projection | 2 | delete direct `_pool = RustClientPool.instance()` ownership |
| client config application/freeze | `RuntimeSession` client config projection/freeze | 2 | delete `_client_config`, `_client_ipc_overrides`, `_pool_config_applied` |
| route registration transaction | `RuntimeSession::register_route()` | 3 | delete `_rollback_registration()` and insert Python binding only after native success |
| relay register rollback | `RuntimeSession` + optional `RelayProjection` | 3 | Python maps native error only; no local rollback logic |
| route unregister transaction | `RuntimeSession::unregister_route()` outcome | 4 | Python removes binding/calls callback from structured native outcome |
| shutdown ordering | `RuntimeSession::shutdown()` outcome | 4 | Python executes language callbacks once from native removed-route list |
| relay control client cache | `RelayProjection` | 5 | delete `_relay_control_client` and `_relay_control_address` |
| relay name resolution | `RelayProjection::connect_via_relay()` | 5 | Python only chooses local thread binding before asking native relay path |
| docs/agent boundary guidance | `docs/plans`, `AGENTS.md`, Copilot guidance | 6 | remove stale Python-runtime-authority instructions |

## File Responsibility Map

- Create `core/runtime/c2-runtime/Cargo.toml`: new language-neutral runtime crate.
- Create `core/runtime/c2-runtime/src/lib.rs`: public exports for runtime session types.
- Create `core/runtime/c2-runtime/src/identity.rs`: server-id generation, validation, and address derivation.
- Create `core/runtime/c2-runtime/src/session.rs`: `RuntimeSession`, lifecycle state machine, route transaction API, pool ownership, and shutdown orchestration.
- Create `core/runtime/c2-runtime/src/relay_projection.rs`: optional relay projection using `c2-http` control and relay-aware client primitives.
- Create `core/runtime/c2-runtime/src/outcome.rs`: structured `RegisterOutcome`, `UnregisterOutcome`, `ShutdownOutcome`, and error/outcome enums.
- Modify `core/Cargo.toml`: include `runtime/c2-runtime`.
- Modify `sdk/python/native/Cargo.toml`: depend on `c2-runtime`.
- Create `sdk/python/native/src/runtime_session_ffi.rs`: PyO3 `RuntimeSession` wrapper, route-registration bridge, client acquire/release wrappers, relay projection wrappers, and outcome conversion.
- Modify `sdk/python/native/src/server_ffi.rs`: expose any reusable server-route construction pieces needed by runtime session without duplicating callback or SHM conversion logic.
- Modify `sdk/python/native/src/client_ffi.rs`: share or wrap native client/response types so `RuntimeSession.acquire_ipc_client()` returns the same Python-visible call surface as `RustClientPool.acquire()`.
- Modify `sdk/python/native/src/http_ffi.rs` and `runtime_session_ffi.rs`: share or wrap HTTP clients and relay-selected clients so low-level `RuntimeSession.acquire_http_client()` plus SOTA `RuntimeSession.connect_via_relay()` / `connect_explicit_relay_http()` return Python-visible call surfaces without exposing a separate Python-owned relay-aware HTTP client surface.
- Modify `sdk/python/native/src/lib.rs`: register `runtime_session_ffi`.
- Modify `sdk/python/src/c_two/transport/registry.py`: reduce `_ProcessRegistry` to a facade over native `RuntimeSession` plus Python local binding table.
- Modify `sdk/python/src/c_two/transport/server/native.py`: keep CRM slot and dispatcher construction, but receive native route handles/session route outcomes from `RuntimeSession`.
- Modify `sdk/python/src/c_two/transport/client/proxy.py`: keep proxy behavior stable while accepting clients acquired through `RuntimeSession`.
- Modify `sdk/python/src/c_two/config/settings.py`: keep code-level facade, but do not become session authority; ensure relay and `shm_threshold` overrides are passed into `RuntimeSession`.
- Modify `sdk/python/src/c_two/config/ipc.py`: keep typed override schemas, but do not retain resolved config dictionaries in registry state after the relevant phase.
- Modify `sdk/python/tests/unit/test_ipc_config.py`: update assertions from Python private state to native session projections.
- Modify `sdk/python/tests/unit/test_name_collision.py`: update rollback/unregister tests to verify native structured outcomes and Python binding cleanup.
- Modify `sdk/python/tests/unit/test_relay_graceful_shutdown.py`: preserve explicit unregister vs shutdown relay failure semantics through `RuntimeSession`.
- Modify `sdk/python/tests/integration/test_registry.py`: add full runtime-session lifecycle coverage.
- Modify `sdk/python/tests/integration/test_http_relay.py`: retain relay-backed duplicate/rollback coverage through the session API.
- Modify `sdk/python/tests/integration/test_direct_ipc_control.py`: add bad-relay/direct-IPC assertions for session-owned client/server paths.
- Create `sdk/python/tests/integration/test_runtime_session.py`: focused integration tests for identity, direct IPC, client pool, route transaction, and shutdown behavior.
- Modify `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`: point the third P1 item to this plan and keep the target boundary concise.
- Modify `AGENTS.md` and `.github/copilot-instructions.md` only after implementation changes durable agent guidance.

## Phased Execution Model

The work is phased to control blast radius, not to lower the quality bar. Each
phase must leave the repository in a coherent, tested, production-grade state.
No phase may document a behavior as supported unless the Rust owner enforces it.
No phase may keep a stale Python fallback for behavior already moved into Rust.

### Phase Discipline And Tracking

Phases are sequencing checkpoints only. Each checkpoint must meet the same
production invariants as any other runtime change in this repository.

Every phase must be specified in this document with:

- an objective that states the exact runtime concern being moved;
- the files and APIs it touches;
- explicit exit criteria that prove the new Rust owner is authoritative;
- deferred items that remain for later phases, with the exact later phase named;
- the verification commands that must pass before the phase can close.

If implementation uncovers additional follow-up work, record it in this plan
before proceeding so the next phase does not inherit an implicit gap.

### Required Phase Handoff Record

Before starting a phase, confirm the previous rows in the ledger below still
match the code. Before closing a phase, update the phase section and this ledger
with:

- the Rust APIs and Python APIs that changed;
- the Python runtime authority deleted or intentionally left for a named later
  phase;
- focused tests added or changed;
- exact verification commands and results;
- review findings fixed before proceeding;
- remaining work, if any, linked to a later phase rather than left as prose.

### Industrial Phase Ledger

| Phase | Status | Rust-owned at phase close | Explicit later-phase work |
| --- | --- | --- | --- |
| 1. Identity/session skeleton | Implemented in current worktree | `RuntimeSession` owns server id, canonical `ipc://` address, server IPC override storage/projection, and native server bridge acquisition | Route registration transactions, unregister/shutdown transactions, direct IPC client pool/config, and relay projection were not closed by Phase 1; client pool/config moved in Phase 2 |
| 2. Direct IPC client pool/config | Implemented in current worktree | `RuntimeSession` owns client IPC overrides, resolved client config snapshot, config freeze, `shm_threshold` propagation, direct IPC acquire/release, and client shutdown drain | Unregister/shutdown remains Phase 4; HTTP/relay projection remains Phase 5 |
| 3. Route registration transactions | Implemented in current worktree | `RuntimeSession.register_route(...)` owns local route registration, relay rollback on failure, and route success/failure outcomes | Relay connect/resolve/cache remains Phase 5; final stale Python deletion remains Phase 6 |
| 4. Unregister/shutdown transactions | Implemented in current worktree | `RuntimeSession.unregister_route(...)` and `RuntimeSession.shutdown(...)` own local route removal, structured relay cleanup outcomes, idempotence, native server shutdown signaling, and direct IPC client drain through the PyO3 session wrapper | Relay connect/resolve/cache and explicit HTTP client projection remain Phase 5; final stale Python deletion remains Phase 6 |
| 5. Relay projection and HTTP client projection | Implemented in current worktree | `RuntimeSession` owns relay address override/effective resolution, relay register/unregister projection, relay-backed `connect_via_relay()`, explicit HTTP relay validation via `connect_explicit_relay_http()`, and low-level HTTP address acquire/release/refcount/shutdown projection; Python local binding lookup still wins before relay | Final stale Python deletion and docs guidance remain Phase 6 |
| 6. Final deletion/docs cleanup | Implemented in current worktree | Durable docs and agent guidance reflect the final Rust runtime boundary; obsolete Python private state and docs were removed or rewritten | No runtime-session follow-up remains unassigned |

### Phase 1: RuntimeSession Skeleton And Identity Authority

**Objective:** Introduce native `RuntimeSession` as the owner of process server
identity, `ipc://` address derivation, base session state, server IPC override
storage, and lazy direct IPC server bridge acquisition. Python must stop
generating server ids and addresses and must not keep a separate server IPC
override authority.

**Implementation status:** implemented in the current worktree. Rust
`c2-runtime` owns identity/address and server IPC override storage. The PyO3
`RuntimeSession` projects `server_id`, `server_address`,
`server_ipc_overrides`, and `ensure_server_bridge()`. At Phase 1 close, Python
still owned route registration, unregister, shutdown ordering, and client pool
config; Phase 2 has since moved client pool/config into Rust. The remaining
Python-owned transaction concerns are tracked under Phases 3 and 4.

**Rust API shape:**

```rust
pub struct RuntimeSession {
    inner: parking_lot::Mutex<RuntimeSessionState>,
}

pub struct RuntimeSessionOptions {
    pub server_id: Option<String>,
    pub server_ipc_overrides: Option<c2_config::ServerIpcConfigOverrides>,
}

pub struct RuntimeIdentity {
    pub server_id: String,
    pub ipc_address: String,
}

impl RuntimeSession {
    pub fn new(options: RuntimeSessionOptions) -> Result<Self, RuntimeSessionError>;
    pub fn ensure_server(&self) -> Result<RuntimeIdentity, RuntimeSessionError>;
    pub fn server_id(&self) -> Option<String>;
    pub fn server_address(&self) -> Option<String>;
    pub fn server_ipc_overrides(&self) -> Option<c2_config::ServerIpcConfigOverrides>;
    pub fn clear_server_identity(&self);
}
```

**Identity rules:**

- Explicit `server_id` is validated with `c2_config::validate_server_id`.
- Auto ids are generated in Rust using the existing `uuid` dependency already
  available to `c2-config`.
- Auto id format must remain path-safe and valid under `validate_server_id`.
  Use a stable prefix and pid/uuid entropy, for example
  `cc{pid_hex}{uuid_simple_hex}`.
- `ipc_address` is always `ipc://{server_id}` and is derived in Rust.
- Python must remove `_auto_server_id()` and `_address_for_server_id()` once
  `RuntimeSession` exposes identity.
- Python forwards normalized server IPC overrides into `RuntimeSession` and
  reads them back only as a native projection when constructing the current
  bridge. It does not keep a separate `_server_ipc_overrides` authority.

**Python facade after phase:**

- `_ProcessRegistry.__init__()` creates one native `RuntimeSession`.
- `set_server(server_id=..., ipc_overrides=...)` forwards normalized typed
  overrides to the native session before server start.
- `get_server_id()` and `get_server_address()` read from native session.
- `register()` asks native `RuntimeSession.ensure_server_bridge()` for the
  server bridge instead of constructing the bridge directly from Python-owned
  identity/address state.
- Python local binding table may still be owned by `_ProcessRegistry` in this
  phase, but identity must not be duplicated.

**Required verification for this phase:**

- Rust tests in `c2-runtime`:
  - explicit valid server id produces `ipc://{id}`;
  - invalid id is rejected;
  - auto id validates and derives a matching address;
  - `server_id()` and `server_address()` return `None` before lazy start and
    concrete values after `ensure_server()`.
  - `server_ipc_overrides()` returns the Rust-owned typed override projection.
- Python tests:
  - `cc.set_server(server_id="unit-server")` then `cc.register(...)` returns
    `cc.server_id() == "unit-server"` and `cc.server_address() == "ipc://unit-server"`;
  - Python no longer imports `uuid` or `os` for server-id generation in
    `registry.py`;
  - registry does not directly construct `Server(...)` from Python-generated
    identity state;
  - `set_server(ipc_overrides=...)` is copied into native session projection;
  - no relay configured: `register -> server_address -> connect(address)` works;
  - bad relay configured: explicit `ipc://` connect still works.

**Phase exit criteria:**

- Rust owns process identity and `ipc://` address derivation.
- Python no longer generates server ids or addresses.
- Rust owns server IPC override storage; Python only forwards and projects it.
- Explicit `ipc://` remains usable with relay unset or unavailable.
- The plan's traceability matrix reflects the new ownership and any deferred
  follow-up is written down before the next phase starts.

### Phase 2: RuntimeSession Client Pool And Config Authority

**Objective:** Move direct IPC client pool ownership and client default config
freezing into `RuntimeSession`. Python must stop storing `_client_config` and
`_pool_config_applied`.

**Implementation status:** implemented in the current worktree. Python
`registry.py` no longer owns `RustClientPool`, `_client_config`,
`_client_ipc_overrides`, `_pool_config_applied`, or `_ensure_pool_config`.
Native `RuntimeSession` owns client IPC override storage/projection, resolves
client IPC config with `c2-config`, acquires/releases IPC clients through the
Rust pool, and marks client config frozen after first direct IPC acquire.
HTTP pool and relay-aware HTTP clients remain outside this phase and are
tracked under Phase 5.

**Rust API additions:**

```rust
impl RuntimeSession {
    pub fn acquire_ipc_client(
        &self,
        address: &str,
    ) -> Result<RuntimeIpcClientHandle, RuntimeSessionError>;

    pub fn release_ipc_client(&self, address: &str);
    pub fn client_ipc_overrides(&self) -> Option<c2_config::ClientIpcConfigOverrides>;
    pub fn client_config_frozen(&self) -> bool;
}
```

**Config rules:**

- Rust resolves client IPC config through `c2-config`.
- The session freezes client config on first direct IPC acquire.
- Calling `cc.set_client(...)` after a direct IPC client exists preserves the
  current public warning behavior, but the freeze decision comes from Rust.
- Python must not keep a resolved config dict for authority. If tests need
  introspection, expose a read-only native snapshot.
- Process transport policy `shm_threshold` is forwarded into `RuntimeSession`
  and participates in native client config resolution; it is also frozen after
  first direct IPC acquire.

**Python facade after phase:**

- `connect(..., address="ipc://...")` calls `RuntimeSession.acquire_ipc_client`.
- `close(crm)` releases via session-owned pool.
- `_ProcessRegistry._pool`, `_client_config`, `_client_ipc_overrides`, and
  `_pool_config_applied` are removed.

**Required verification for this phase:**

- Rust tests:
  - explicit client overrides beat env/defaults in session-acquired clients;
  - config freezes after acquire;
  - release decrements refcount and shutdown drains pool.
- Python tests:
  - `cc.set_client(ipc_overrides=...)` affects direct IPC client behavior through
    native snapshot, not `_client_config`;
  - `cc.set_client(...)` after direct IPC connect emits the existing warning;
  - `cc.set_transport_policy(...)` after direct IPC connect emits the existing
    active-connection warning even when the process is client-only;
  - explicit direct IPC connect does not instantiate relay-aware HTTP client
    even when relay env is set;
  - full direct IPC request/response still passes large payload response as
    `PyResponseBuffer` / `memoryview` when hold/view mode requests it.

**Phase exit criteria:**

- Rust owns the direct IPC client pool and config freeze point.
- Python no longer stores `_client_config` / `_pool_config_applied`.
- The remaining phases are still clearly documented after the change lands.

### Phase 3: Route Registration Transaction Authority

**Objective:** Move route registration transaction rules into
`RuntimeSession`. Python still builds route callback metadata and owns local
thread binding, but Rust decides whether the route is fully registered or must
be rolled back.

**Implementation status:** implemented in the current worktree; verification
is recorded below.

**Rust API additions:**

```rust
pub struct RuntimeRouteSpec {
    pub name: String,
    pub crm_ns: String,
    pub crm_ver: String,
    pub method_names: Vec<String>,
    pub access_map: std::collections::HashMap<u16, c2_server::scheduler::AccessLevel>,
    pub concurrency_mode: c2_server::scheduler::ConcurrencyMode,
    pub max_pending: Option<usize>,
    pub max_workers: Option<usize>,
}

pub struct RegisterOutcome {
    pub route_name: String,
    pub server_id: String,
    pub ipc_address: String,
    pub relay_registered: bool,
}

impl RuntimeSession {
    pub fn register_route(
        &self,
        spec: RuntimeRouteSpec,
        callback: Arc<dyn c2_server::dispatcher::CrmCallback>,
    ) -> Result<(RegisterOutcome, RouteConcurrencyHandle), RuntimeSessionError>;
}
```

**Transaction rules:**

1. Ensure base IPC server exists.
2. Register route with `c2-server`.
3. If relay projection is configured, register route with relay.
4. If relay registration fails, unregister the Rust route before returning the
   error.
5. Return success only after local Rust route state and relay projection state
   are coherent.
6. Python inserts the thread-local direct binding only after native
   `register_route` returns success.

**Python facade after phase:**

- `NativeServerBridge.register_crm(...)` builds `RuntimeRouteSpec` and Python
  callback, calls native `RuntimeSession.register_route`, and only then inserts
  `CRMSlot` into `_slots`.
- `_ProcessRegistry._rollback_registration()` is removed. Rollback is native.
- Same-process duplicate name remains a preflight error before relay contact;
  the authoritative duplicate check for Rust route state is native.

**Required verification for this phase:**

- Rust tests:
  - register route creates route in server;
  - duplicate local route is rejected;
  - simulated relay failure unregisters the Rust route;
  - successful relay registration returns `relay_registered=true`.
- Python tests:
  - relay duplicate registration raises `ResourceAlreadyRegistered`;
  - unreachable relay registration leaves no Python binding, no Rust route, and
    no server address when the failed route was the only route;
  - registering route A then failing route B leaves route A usable;
  - same-process thread-local connect remains direct Python object dispatch;
  - remote IPC connect to the same route still uses Rust-dispatched callback
    and preserves SHM request memoryview behavior.

**Verification recorded in this worktree:**

- `cargo check --manifest-path core/Cargo.toml -p c2-runtime -q`
- `cargo check --manifest-path sdk/python/native/Cargo.toml -q`
- `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_runtime_session.py sdk/python/tests/unit/test_name_collision.py sdk/python/tests/unit/test_serve.py sdk/python/tests/unit/test_relay_graceful_shutdown.py sdk/python/tests/integration/test_registry.py -q --timeout=30 -rs`
  → `77 passed`
- `cargo test --manifest-path core/Cargo.toml -p c2-server -q`
- `cargo test --manifest-path core/Cargo.toml -p c2-runtime -q`
- `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_direct_ipc_control.py sdk/python/tests/integration/test_zero_copy_ipc.py sdk/python/tests/integration/test_remote_scheduler_config.py -q --timeout=30 -rs`
  → `22 passed`

**Phase exit criteria:**

- Rust owns route-registration transaction success/failure.
- Python inserts local bindings only after native success.
- Relay rollback behavior is documented and verified, not implied.
- Any remaining unregister/shutdown or relay-resolution work is explicitly
  still assigned to Phases 4 and 5 in the ledger.

### Phase 4: Unregister And Shutdown Transaction Authority

**Objective:** Move unregister and shutdown transaction state into Rust while
allowing Python to execute language-specific cleanup exactly once.

**Implementation status:** implemented in the current worktree. Rust
`c2-runtime` now exposes `RelayCleanupError`, `UnregisterOutcome`, and
`ShutdownOutcome`. The PyO3 `RuntimeSession` wrapper exposes
`unregister_route(...)` and `shutdown(...)` outcome dictionaries. Python
`NativeServerBridge` removes local CRM slots and invokes `@on_shutdown`
callbacks only from native `local_removed` / `removed_routes` outcomes.
`_ProcessRegistry.unregister()` raises explicit relay cleanup failures only
after local cleanup, and `_ProcessRegistry.shutdown()` logs relay cleanup
failures without raising. Relay connect/resolve cache work remains Phase 5.
Explicit HTTP client projection remains Phase 5 and is not documented as
complete until `registry.py` no longer owns `_http_pool` or
`RustHttpClientPool.instance()`.

**Rust API additions:**

```rust
pub struct UnregisterOutcome {
    pub route_name: String,
    pub local_removed: bool,
    pub relay_error: Option<RelayCleanupError>,
}

pub struct ShutdownOutcome {
    pub removed_routes: Vec<String>,
    pub relay_errors: Vec<RelayCleanupError>,
    pub server_was_started: bool,
    pub ipc_clients_drained: bool,
    pub http_clients_drained: bool,
}

impl RuntimeSession {
    pub fn unregister_route(
        &self,
        server: &Arc<c2_server::Server>,
        name: &str,
        relay_address: Option<&str>,
        relay_use_proxy: bool,
    ) -> Result<UnregisterOutcome, RuntimeSessionError>;

    pub fn shutdown(
        &self,
        server: Option<&Arc<c2_server::Server>>,
        route_names: Vec<String>,
        relay_address: Option<&str>,
        relay_use_proxy: bool,
    ) -> ShutdownOutcome;
}
```

The current signatures accept the native server handle and route-name snapshot
from the Python bridge because Phase 1 kept Python responsible for constructing
the PyO3 `NativeServerBridge`. This is not a Python transaction fallback: Rust
performs the local route removal, relay cleanup attempt, native server shutdown
signal, and structured outcome construction. Python only supplies the
language-specific bridge handle and then mutates Python local bindings from the
outcome.

**Unregister rules:**

- If the route is absent locally, return an error equivalent to `KeyError`.
- Remove the local Rust route before relay cleanup.
- If relay cleanup fails, return `UnregisterOutcome { local_removed: true,
  relay_error: Some(...) }`.
- Python removes its local binding and invokes `@on_shutdown` if
  `local_removed` is true, then raises the relay error for explicit
  `cc.unregister(...)`.

**Shutdown rules:**

- Shutdown uses the route-name snapshot supplied by the Python bridge for its
  Python-owned CRM slots, then Rust performs the authoritative native route
  removals and returns the actual removed-route list.
- Rust removes all supplied local routes, attempts best-effort relay unregister
  for each configured projection, signals native server shutdown, drains direct
  IPC clients through the PyO3 session wrapper, and returns all relay cleanup
  errors in the outcome. HTTP pool drain remains a Phase 5 projection item until
  the PyO3 `RuntimeSession` wrapper drains the native HTTP pool and reports
  `http_clients_drained=true`.
- Python removes any matching local bindings and invokes each `@on_shutdown`
  callback exactly once.
- Python logs relay cleanup failures during shutdown and does not raise.
- Calling `shutdown()` twice is safe and does not double-call Python shutdown
  callbacks.

**Required verification for this phase:**

**Verification recorded for this phase:**

- Rust tests:
  - unregister absent route reports `MissingRoute`;
  - unregister local success plus relay failure returns a structured
    `relay_error` after local removal;
  - unregister and shutdown without relay do not lazily publish a new identity;
  - shutdown is idempotent and reports removed routes only once.
- Python tests:
  - explicit unregister with relay failure removes local route, invokes
    shutdown callback once, then raises;
  - shutdown with relay failure logs and does not raise;
  - shutdown after unregister does not double-call callback;
  - reregister after unregister works;
  - `registry.names` reflects native-outcome-driven Python local binding state;
  - if native shutdown does not report a route as removed, Python does not
    invoke that route's shutdown callback as if removal had succeeded.

Commands run during implementation:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-runtime -q
cargo check --manifest-path sdk/python/native/Cargo.toml -q
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_name_collision.py \
  sdk/python/tests/unit/test_relay_graceful_shutdown.py \
  sdk/python/tests/unit/test_runtime_session.py \
  sdk/python/tests/unit/test_serve.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/integration/test_p0_fixes.py::TestOnShutdownLifecycle \
  sdk/python/tests/integration/test_registry.py::TestErrors::test_register_then_unregister_then_reregister \
  -q --timeout=30 -rs

C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_runtime_session.py \
  sdk/python/tests/unit/test_name_collision.py \
  sdk/python/tests/unit/test_serve.py \
  sdk/python/tests/unit/test_relay_graceful_shutdown.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/integration/test_registry.py \
  sdk/python/tests/integration/test_direct_ipc_control.py \
  sdk/python/tests/integration/test_zero_copy_ipc.py \
  sdk/python/tests/integration/test_remote_scheduler_config.py \
  sdk/python/tests/integration/test_p0_fixes.py \
  -q --timeout=30 -rs
```

**Phase exit criteria:**

- Rust owns unregister/shutdown transaction outcomes.
- Python cleanup executes exactly once from structured native outcomes.
- Remaining relay projection work is still tracked explicitly.
- Any public behavior changes are documented in `AGENTS.md`,
  `.github/copilot-instructions.md`, and SDK test docs before proceeding.

### Phase 5: RelayProjection And Explicit HTTP Client Authority

**Objective:** Move relay-aware name resolution, relay-control cache, and HTTP
client projection into `RuntimeSession` without coupling relay to direct IPC.
Later Phase 10 work moves explicit SOTA HTTP connects onto the relay-aware
contract-validation path instead of returning a bare pooled HTTP client.

**Implementation status:** implemented in the current worktree. Relay remains an
optional projection above the standalone IPC session. `RuntimeSession` now stores
the explicit relay address override, resolves the effective relay address from
that override or `C2_RELAY_ANCHOR_ADDRESS`, owns the reusable relay control-client
projection/cache, performs relay registration/unregistration through `c2-http`,
exposes `connect_via_relay()` for name-only relay connections, exposes
`connect_explicit_relay_http_client()` for explicit HTTP relay connects, and
keeps low-level HTTP address acquire/release/refcount/shutdown methods over the
native `c2-http` pool. Python still checks the same-process local binding first
and still uses explicit `ipc://` connections directly; relay does not own server
identity, server lifecycle, direct IPC clients, route handles, or scheduling.

**Rust/PyO3 API additions:**

```rust
impl RuntimeSession {
    pub fn set_relay_anchor_address(&self, relay_address: Option<String>);
    pub fn relay_anchor_address_override(&self) -> Option<String>;
    pub fn effective_relay_anchor_address(&self) -> Result<Option<String>, RuntimeSessionError>;
    pub fn resolve_relay_connection(
        &self,
        route_name: &str,
        relay_use_proxy: bool,
        max_attempts: usize,
        call_timeout_secs: f64,
        expected_crm_ns: &str,
        expected_crm_ver: &str,
    ) -> Result<RelayResolvedConnection, RuntimeSessionError>;
    pub fn connect_relay_http_client(
        &self,
        route_name: &str,
        relay_use_proxy: bool,
        max_attempts: usize,
        call_timeout_secs: f64,
        expected_crm_ns: &str,
        expected_crm_ver: &str,
    ) -> Result<(RelayAwareHttpClient, String), RuntimeSessionError>;
    pub fn connect_explicit_relay_http_client(
        &self,
        relay_url: &str,
        route_name: &str,
        relay_use_proxy: bool,
        max_attempts: usize,
        call_timeout_secs: f64,
        expected_crm_ns: &str,
        expected_crm_ver: &str,
    ) -> Result<(RelayAwareHttpClient, String), RuntimeSessionError>;
    pub fn clear_relay_projection_cache(&self);
}

// PyO3 RuntimeSession projection over the native c2-http pool.
impl PyRuntimeSession {
    pub fn acquire_http_client(&self, address: &str) -> Result<PyRustHttpClient, PyErr>;
    pub fn release_http_client(&self, address: &str);
    pub fn http_client_refcount(&self, address: &str) -> usize;
    pub fn shutdown_http_clients(&self);
}
```

PyO3 exposes `RuntimeSession.set_relay_anchor_address(...)`,
`relay_anchor_address_override`, `effective_relay_anchor_address`,
`connect_via_relay(name, expected_crm_ns, expected_crm_ver) ->
RelayConnectedClient`, `connect_explicit_relay_http(...) ->
RelayConnectedClient`, and low-level HTTP client pool projection methods so
`registry.py` does not construct or cache relay-aware/control clients or own
`RustHttpClientPool.instance()` itself.

**Relay rules:**

- `connect(..., address=None)` checks Python thread-local binding first.
- If no local binding exists and no relay address resolves, Python raises the
  existing lookup error.
- If relay address resolves, the native session resolves a concrete relay target:
  local `ipc://` routes become IPC clients, and HTTP routes are probed before
  returning a `RelayConnectedClient` to the Python
  proxy.
- Relay route max attempts and proxy policy are resolved through Rust
  `c2-config` in the PyO3 boundary only when a relay path is actually used, so
  malformed relay proxy env values do not break no-relay direct IPC paths.
- `set_relay_anchor()` changes the native projection address; Python must not cache
  `RustRelayControlClient`, `_relay_control_client`, or `_relay_control_address`.
- Registration starts the local IPC server before relay projection when an
  effective relay address exists, even when Python passes `relay_address=None`;
  this preserves relay upstream validation without making relay the IPC owner.
- Explicit HTTP addresses call `RuntimeSession.connect_explicit_relay_http(...)`
  so connect-time route resolution validates the expected CRM contract before
  returning a proxy; Python does not store `_http_pool` or instantiate
  `RustHttpClientPool`.
- Explicit IPC addresses still bypass relay entirely.

**Implemented verification for this phase:**

- Rust tests cover session identity/config/route transaction invariants and
  compile the new relay projection path through `c2-runtime`.
- Python tests cover:
  - local thread-local binding wins before relay;
  - no local route and no relay raises the existing `LookupError`;
  - real `c3 relay` no-address connect succeeds through
    `RuntimeSession.connect_via_relay()`;
  - real relay missing route maps to `ResourceNotFound`;
  - relay unavailable maps to `RegistryUnavailable`;
  - `cc.set_relay_anchor()` updates `RuntimeSession.relay_anchor_address_override` and resets the
    session-owned relay projection when the address changes;
  - `registry.py` no longer has Python relay-control cache fields or direct
    relay-aware HTTP client construction;
  - explicit `ipc://` still works with relay unset or bad relay/proxy env;
  - relay registration starts the IPC server before relay upstream validation;
  - explicit HTTP `cc.connect(..., address=relay_url)` uses a relay-aware
    client, rejects CRM contract mismatch before returning, and closes the
    relay-aware client without leaving a global HTTP pool reference;
  - `registry.py` no longer imports `RustHttpClientPool`, stores `_http_pool`,
    or calls `RustHttpClientPool.instance()` directly.

**Verification commands recorded for Phase 5:**

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
cargo test --manifest-path core/Cargo.toml -p c2-runtime -q
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_runtime_session.py \
  sdk/python/tests/unit/test_relay_graceful_shutdown.py \
  sdk/python/tests/unit/test_ipc_config.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/integration/test_registry.py \
  -q --timeout=30 -rs
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/integration/test_http_relay.py::TestCcConnectHttp::test_no_address_connect_uses_runtime_session_relay_projection \
  sdk/python/tests/integration/test_http_relay.py::TestCcConnectHttp::test_no_address_relay_connect_maps_missing_route_to_resource_not_found \
  sdk/python/tests/integration/test_http_relay.py::TestCcConnectHttp::test_connect_http_close_closes_relay_aware_client \
  sdk/python/tests/unit/test_sdk_boundary.py::test_registry_does_not_own_relay_control_plane_mechanisms \
  -q --timeout=30 -rs -vv
git diff --check
```

Latest focused rerun after moving explicit HTTP pooling through
`RuntimeSession`:

```bash
cargo check --manifest-path sdk/python/native/Cargo.toml -q
cargo test --manifest-path core/Cargo.toml -p c2-runtime -q
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_runtime_session.py \
  sdk/python/tests/unit/test_sdk_boundary.py \
  sdk/python/tests/unit/test_ipc_config.py \
  sdk/python/tests/integration/test_http_relay.py::TestCcConnectHttp::test_connect_http_close_closes_relay_aware_client \
  sdk/python/tests/integration/test_http_relay.py::TestCcConnectHttp::test_no_address_connect_uses_runtime_session_relay_projection \
  sdk/python/tests/integration/test_http_relay.py::TestCcConnectHttp::test_no_address_relay_connect_maps_missing_route_to_resource_not_found \
  -q --timeout=30 -rs
git diff --check
```

Observed result before Phase 10: `c2-runtime` 12 passed, focused pytest 56
passed, and `git diff --check` passed. Phase 10 supersedes the explicit HTTP
pool-refcount assertion with relay-aware close validation.

**Phase exit criteria:**

- Relay is modeled as an optional projection above base IPC.
- Direct IPC still works without relay as a dependency.
- Python no longer owns relay control-client cache state.
- Name-only relay connect uses native `RuntimeSession.connect_via_relay()`.
- Explicit HTTP address connect/release/shutdown goes through native
  `RuntimeSession` methods, not Python-owned `_http_pool` state.
- The plan and boundary docs point to Phase 6 only for final stale-authority
  deletion and guidance updates.

### Phase 6: Delete Python Runtime Authority And Update Documentation

**Objective:** Remove obsolete Python-owned runtime/session state and update all
docs so future SDK work copies the Rust session boundary, not the old Python
registry internals.

**Implementation status:** implemented in the current worktree. This is not a
cosmetic cleanup phase; it is the phase that prevents zombie private APIs and
stale agent guidance from becoming the next SDK's reference implementation.

**Required deletions from Python registry after all native phases land:**

- `_server_id`
- `_server_address`
- `_server_id_override`
- `_client_config`
- `_client_ipc_overrides`
- `_pool_config_applied`
- `_relay_control_client`
- `_relay_control_address`
- `_relay_control_client_for()`
- `_auto_server_id()`
- `_address_for_server_id()`
- `_rollback_registration()`
- direct ownership of `RustClientPool.instance()`
- direct ownership of `RustHttpClientPool.instance()`

**Required documentation updates:**

  - `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md` marks process
  runtime/session state as Rust-owned once complete.
- `AGENTS.md` states Rust `RuntimeSession` owns process runtime, direct IPC
  session, client pools, route transactions, and relay projection.
- `.github/copilot-instructions.md` mirrors the durable boundary guidance.
- `sdk/python/tests/README.md` lists runtime-session and relay/direct-IPC
  verification commands, including Python 3.10 coverage.
- README examples remain unchanged at public API level unless implementation
  details are mentioned.

**Phase exit criteria:**

- No stale Python runtime authority remains.
- The plan is updated to reflect the final ownership split.
- AGENTS and SDK guidance no longer describe deleted Python-owned behavior as
  supported.
- `rg` checks for deleted private state names, direct relay-aware HTTP client
  or `RustHttpClientPool` construction in `registry.py`, and stale ownership
  claims are recorded in this section before the phase closes.

**Verification recorded for Phase 6:**

```bash
python - <<'PY'
from pathlib import Path

registry = Path('sdk/python/src/c_two/transport/registry.py').read_text()
for needle in [
    'self._server_id',
    'self._server_address',
    'self._server_id_override',
    'self._client_config',
    'self._client_ipc_overrides',
    'self._pool_config_applied',
    'self._relay_control_client',
    'self._relay_control_address',
    'def _relay_control_client_for',
    'def _auto_server_id',
    'def _address_for_server_id',
    'def _rollback_registration',
    'RustClientPool.instance()',
    'RustHttpClientPool.instance()',
    'self._http_pool',
    'RustRelayAwareHttpClient(',
]:
    assert needle not in registry, needle

docs = '\\n'.join(
    Path(path).read_text()
    for path in [
        'AGENTS.md',
        '.github/copilot-instructions.md',
        'docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md',
        'sdk/python/tests/README.md',
    ]
)
for stale in [
    'remaining explicit HTTP address pool projection',
    'explicit HTTP address pool projection remains',
    'Phase 6 moves or',
    'intentionally held by the Python facade',
]:
    assert stale not in docs, stale
PY

C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/unit/test_runtime_session.py::test_set_relay_anchor_blank_clears_native_override \
  sdk/python/tests/unit/test_runtime_session.py::test_runtime_session_shutdown_drains_explicit_http_pool \
  sdk/python/tests/unit/test_sdk_boundary.py::test_registry_does_not_own_relay_control_plane_mechanisms \
  -q --timeout=30 -rs
```

Observed result: the source assertion returned no stale registry/runtime
authority matches in production code or stale docs, and the focused pytest
checks passed. The blank-relay regression test was added after review found
that `cc.set_relay_anchor("   ")` cleared `settings` but left an empty native relay
override; the Python facade now projects the cleaned settings value into
`RuntimeSession`.

## PyO3 Boundary Design

The PyO3 layer should avoid forcing all server functionality through one huge
Python-facing object. Expose a small native `RuntimeSession` pyclass and
reuse existing server/client/http wrapper concepts.

Recommended Python-visible surface:

```python
session = _native.RuntimeSession(
    server_id=None,
    server_ipc_overrides={...} | None,
    client_ipc_overrides={...} | None,
    global_overrides={...} | None,
    relay_address="http://..." | None,
)

identity = session.ensure_server()
route_handle = session.register_route(
    name,
    dispatcher,
    methods,
    access_map,
    concurrency_mode,
    max_pending,
    max_workers,
    crm_ns,
    crm_ver,
)
outcome = session.unregister_route(name)
client = session.acquire_ipc_client("ipc://server")
session.release_ipc_client("ipc://server")
http_client = session.acquire_http_client("http://relay:8080")
session.release_http_client("http://relay:8080")
http_client = session.connect_via_relay(name)
session.shutdown()
```

The exact native class names may vary, but the ownership must not: Python may
hold references to handles returned by the session, but the session owns the
runtime state.

## Python Facade Design

`_ProcessRegistry` remains the public singleton facade because public API
stability matters even in 0.x when it costs little and avoids user-facing churn.
Its responsibility should shrink to:

```text
register()
  -> build Python CRMSlot candidate
  -> call native session.register_route(...)
  -> insert Python direct binding only after native success

connect(address=None)
  -> if address is None and Python binding exists: thread-local proxy
  -> elif address starts with ipc://: native session.acquire_ipc_client(address)
  -> elif address starts with http:// or https://: native session.connect_explicit_relay_http(address, name, expected_crm)
  -> else: native session.connect_via_relay(name)

unregister()
  -> native session.unregister_route(name)
  -> if local_removed: remove Python binding and invoke Python callback
  -> if explicit relay error exists: raise after cleanup

shutdown()
  -> native session.shutdown()
  -> remove Python bindings for removed routes and invoke callbacks once
  -> log relay cleanup errors, do not raise
```

Python must not store native state again for convenience. Any needed UI value
must be a projection from native session, for example `session.server_address`.

## Error Mapping

Native errors must be structured enough to preserve Python-visible behavior:

| Native condition | Python-visible behavior |
| --- | --- |
| local duplicate route | `ValueError` or existing duplicate local error before relay contact |
| relay duplicate route / HTTP 409 | `ResourceAlreadyRegistered` |
| relay resolve 404 | `ResourceNotFound` |
| relay status other than 404/409 in connect/register contexts | `ResourceUnavailable` or `RuntimeError` matching current API site |
| relay transport failure during connect | `RegistryUnavailable` |
| explicit unregister relay cleanup failure | local cleanup succeeds, then raise mapped error |
| shutdown relay cleanup failure | log only, no raise |
| invalid `ipc://` address | native validation error mapped to `ValueError` where currently public |

Do not erase relay HTTP status codes in generic `RuntimeError` messages before
Python has a chance to map them.

## Industrial Verification Matrix

Every implementation phase must pass the relevant focused tests plus the full
verification set before the phase is considered done:

```bash
git diff --check
cargo test --manifest-path core/Cargo.toml --workspace
uv sync --reinstall-package c-two
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
uv sync --group examples
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/integration/test_http_relay.py \
  sdk/python/tests/integration/test_relay_mesh.py \
  sdk/python/tests/integration/test_registry.py \
  sdk/python/tests/integration/test_runtime_session.py \
  sdk/python/tests/integration/test_direct_ipc_control.py \
  -q --timeout=30 -rs
```

Relay tests must be run with a local `c3` available:

```bash
python tools/dev/c3_tool.py --build --link
C2_RELAY_ANCHOR_ADDRESS= uv run pytest \
  sdk/python/tests/integration/test_http_relay.py \
  sdk/python/tests/integration/test_relay_mesh.py \
  -q --timeout=30 -rs
```

Python 3.10 remains required for downstream Taichi stacks. Runtime-session work
must be verified in a Python 3.10 environment in addition to the default
development Python when available:

```bash
UV_PYTHON=3.10 uv sync --reinstall-package c-two
C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

## Phase Acceptance Rules

A phase is complete only when all of these are true:

- The Rust owner for that phase's runtime concern is implemented and tested.
- Python no longer owns a second authoritative copy of that concern.
- Explicit direct IPC works with relay unset.
- Explicit direct IPC works with relay set to an unavailable endpoint.
- Thread-local same-process calls still bypass serialization.
- Remote SHM payloads are not converted to Python `bytes` before invocation.
- Relay behavior touched by the phase has focused integration coverage.
- Full Python and Rust verification commands pass.
- Docs and agent guidance no longer mention the removed Python authority.

## Out-Of-Scope For This Plan

- Moving Python CRM decorators or method discovery into Rust.
- Moving Python resource invocation into Rust.
- Replacing the thread-local fast path with serialized dispatch.
- Introducing relay-managed IPC registration.
- Adding cross-process duplicate prevention for standalone no-relay IPC.
- Preserving stale Python private attributes for compatibility after their
  behavior moves into Rust.
