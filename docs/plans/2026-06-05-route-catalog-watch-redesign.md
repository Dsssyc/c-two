# Route Catalog Watch Redesign Implementation Plan

**Date:** 2026-06-05
**Status:** Phase 1 implemented; Phase 2 pending
**Scope:** IPC route lifecycle, relay route authority, relay upstream pools, relay-aware HTTP fallback, Rust error taxonomy, Python SDK error facade
**Supersedes:** `docs/issues/ipc-route-contract-stale-snapshot.md` as the long-term design

## Assumptions

1. C-Two is still in 0.x, so wire protocol, Rust APIs, Python native FFI, and
   SDK error classes can be changed without preserving incorrect old behavior.
2. Direct IPC remains a complete standalone mode. Relay discovery can select a
   local IPC endpoint, but relay does not own IPC registration, scheduling, or
   direct IPC connection establishment.
3. Production route consistency must not depend on an IPC handshake snapshot or
   on a relay data-plane upstream client staying alive.
4. A route is a versioned resource owned by a server instance. A connection is
   only a transport vehicle and can be evicted, closed, or reconnected without
   changing route ownership.
5. The design is implemented in Rust core first. Python exposes typed errors
   and thin facades, not recreated route catalog or relay fallback decisions.

## Objective

Replace the current route-snapshot and lazy-refresh model with a clean route
catalog protocol that is:

- versioned, so clients can tell which route state they observed;
- watchable, so long-lived clients and relay upstream pools get route changes
  promptly instead of discovering them only on a later call;
- recoverable, so missed watch history forces a full catalog rebuild rather
  than trusting stale cache;
- authoritative at call time, so the server rejects closed, removed, replaced,
  or contract-mismatched routes before invoking resource code;
- typed in its errors, so relay withdrawal and SDK exceptions are driven by
  semantic error variants rather than broad `Handshake(String)` buckets;
- clean-cut, so obsolete name-only, snapshot-authoritative, and loopback
  self-fallback behavior is removed instead of left as a compatibility shim.

Success means the hydro production failure class is addressed at the root:
long-lived IPC and relay-aware clients can observe later route registrations,
route removals, route replacements, owner restarts, relay idle eviction, and
watch gaps without route state corruption or misleading fallback.

## Industrial References And C-Two Adaptation

This design adapts mature control-plane patterns, but does not copy their APIs.

- Envoy xDS: dynamic resources have versions, delta updates send only changed
  resources, ACK/NACK separates applied from rejected updates, and lazy resource
  request is a supported control-plane pattern. Source:
  <https://www.envoyproxy.io/docs/envoy/latest/api-docs/xds_protocol.html>
- Kubernetes API: clients list a resource collection, keep the returned
  `resourceVersion`, watch changes after that version, and must clear cache and
  relist after `410 Gone` when history is no longer available. Source:
  <https://kubernetes.io/docs/reference/using-api/api-concepts/>
- etcd API: revisions are a logical clock, watch streams deliver ordered
  events from a requested revision, progress notifications help disconnected
  watchers recover, and compacted history is reported explicitly. Source:
  <https://etcd.io/docs/v3.6/learning/api/>
- gRPC health checking: clients can watch backend health and stop sending calls
  while the service is unhealthy, resuming when it becomes healthy. Source:
  <https://grpc.io/docs/guides/health-checking/>

The C-Two adaptation is:

- `RouteList` is the consistent list operation.
- `RouteWatch` is the route update stream.
- `catalog_revision` is the collection-level `resourceVersion`.
- `CatalogCompacted` is the explicit "history unavailable, rebuild required"
  signal.
- `RouteAck` / `RouteNack` is the control-plane update acknowledgement surface
  for future extension and diagnostics.
- call-time `route_uid` and `route_revision` validation is the final authority.

## Existing Pain Points Covered

### P1: cached route contract is accepted after route lifecycle changes

The transitional `ensure_route_contract(...)` refreshes only on cache miss or
contract mismatch. If the route existed in the cached handshake snapshot and is
later closed, removed, or replaced with the same contract, cache validation can
still succeed. The new design removes "cache match means current" from all
route-bound acquire and dispatch paths.

### P1: broad handshake errors drive unsafe relay withdrawal

`IpcError::Handshake` currently includes protocol/decode failures as well as
semantic identity and contract failures. Relay code can interpret any
`Handshake` as proof that a route should be withdrawn. The new design splits
transport, protocol, identity, contract, catalog, and lifecycle errors before
relay withdrawal decisions.

### P2: relay registration reports generic failure to callers

Registration code logs more detail than it returns. The new design requires
structured registration rejection payloads and Python exceptions with stable
codes and diagnostic details.

### Production symptom: relay idle eviction looks like route loss

Relay idle eviction must only close relay-created data-plane upstream clients.
It must not represent registration liveness, route ownership, or route validity.
The new design separates relay route authority, upstream route watch/control,
and data-plane connection pooling.

## Terminology

`server_id`
: Logical server identity. It determines the canonical IPC address and can be
  configured by the process. It is stable across native server restarts when the
  same logical server is intentionally re-created. It is useful for ownership
  grouping, canonical address derivation, and relay authority bookkeeping, but
  it is not sufficient by itself to prove that an old route token still points
  at the same live server incarnation.

`server_instance_id`
: Incarnation identity for one server process lifetime. It changes after
  process restart or native server recreation, even when `server_id` is stable.
  Route tokens, relay attestations, and direct IPC fast paths must compare this
  value to reject stale clients after server ABA: same `server_id`, same route
  name, maybe same contract, but a different live server instance.

`route_name`
: User chosen routing key passed to `cc.register(..., name=...)` and
  `cc.connect(..., name=...)`.

`route_uid`
: Unique identity for one committed route registration. Re-registering the same
  `route_name` after remove, close, or owner replacement creates a new
  `route_uid`.

`route_revision`
: Version of one route record. State, contract, owner, lease, and health changes
  advance it.

`catalog_revision`
: Monotonic version of the whole route catalog. Every route event advances it.
  In server IPC context this is `server_catalog_revision`; in relay context this
  is `relay_authority_revision`. Wire structs must use the specific field name
  for the layer they belong to whenever both can appear in the same payload.

`route_state`
: Lifecycle state. Initial states are `Pending`, `Ready`, `Draining`, `Closed`,
  and `Removed`.

`compaction_revision`
: Oldest retained route event revision. A watcher whose last revision is older
  than this cannot be incrementally repaired and must relist.

`tombstone_retention`
: Duration for retaining removed-route negative state in relay authority and
  route catalog event history. Tombstones and event-log entries must be compacted
  under the same revision boundary so a watcher never observes a retained
  tombstone without the event history needed to understand it.

## Route Catalog Data Model

Rust core owns the route catalog. The authoritative server-side record is:

```rust
pub struct RouteRecord {
    pub route_name: String,
    pub route_uid: RouteUid,
    pub route_revision: u64,
    pub catalog_revision: u64,
    pub owner_server_id: String,
    pub owner_server_instance_id: String,
    pub owner_epoch: u64,
    pub contract: c2_contract::ExpectedRouteContract,
    pub method_table: c2_wire::MethodTable,
    pub max_payload_size: u64,
    pub state: RouteState,
    pub state_reason: Option<RouteStateReason>,
    pub lease_deadline_ms: Option<u64>,
}
```

Implementation note:

- `route_uid` must be generated by Rust when the route moves from `Pending` to
  `Ready`.
- `owner_server_id` and `owner_server_instance_id` are prefixed with `owner_`
  because a relay catalog can store routes owned by local or peer servers. For a
  direct IPC server catalog they identify the local server that owns the route.
- `owner_epoch` is local to one `server_id` and increments when the runtime
  creates a new server instance identity. It is diagnostic and secondary to
  `server_instance_id`; it must not replace `server_instance_id` in trust checks.
- `lease_deadline_ms` applies to relay authority and owner liveness. Direct IPC
  server catalogs can leave it unset unless the route is projected into relay.

`RouteState`:

```rust
pub enum RouteState {
    Pending,
    Ready,
    Draining,
    Closed,
    Removed,
}
```

`RouteStateReason`:

```rust
pub enum RouteStateReason {
    RegisterPrepared,
    RegisterCommitted,
    ExplicitUnregister,
    ServerShutdown,
    OwnerLeaseExpired,
    OwnerReplaced,
    ContractChanged,
    AdministrativeClose,
}
```

Invariants:

- `catalog_revision` is monotonic within one route authority.
- relay route ordering must use route UID, relay authority revision, and owner
  identity. It must not depend on wall-clock timestamps for correctness.
- `route_revision` is monotonic for one `route_uid`.
- A `route_name` can map to at most one active `Ready` or `Draining` route in a
  catalog.
- A new owner instance for the same route name must either prove idempotent same
  route identity or create a new `route_uid`.
- `Removed` is a terminal semantic state. Its event can be compacted only after
  the tombstone retention window.
- `Closed` rejects new calls but can still be visible long enough to explain
  why an old proxy stopped working.

## IPC Wire Protocol Clean Cut

The old handshake route list becomes a bootstrap hint only. It must not be the
long-lived route authority.

### Handshake

Server handshake includes:

```rust
pub struct ServerHandshake {
    pub wire_version: u16,
    pub capabilities: u64,
    pub server_identity: ServerIdentity,
    pub shm_segments: Vec<SegmentInfo>,
    pub catalog_revision: u64,
    pub min_watch_revision: u64,
}
```

Route tables are removed from mandatory handshake semantics. During the
transition to this clean cut, tests must assert that no acquire path treats
handshake route presence as proof of current route validity.

The route watch is multiplexed over the existing IPC connection using
server-to-client signal/control frames. It is not a second UDS for ordinary
direct IPC clients. Relay `UpstreamControl` may hold its own control-only
`IpcClient` per upstream server so route watch lifecycle remains separate from
the relay data-plane pool.

### RouteList

Request:

```rust
pub struct RouteListRequest {
    pub selector: RouteSelector,
    pub min_revision: Option<u64>,
}
```

Response:

```rust
pub struct RouteListResponse {
    pub catalog_revision: u64,
    pub min_watch_revision: u64,
    pub routes: Vec<RouteRecordWire>,
}
```

`RouteList` is used:

- after IPC connection establishment;
- after watch stream loss when the local directory is dirty and cannot safely
  serve a route;
- after `CatalogCompacted`;
- by relay registration attestation when it needs current committed route
  state.

### RouteWatch

Request:

```rust
pub struct RouteWatchRequest {
    pub from_revision: u64,
    pub selector: RouteSelector,
    pub allow_heartbeat: bool,
}
```

Events:

```rust
pub enum RouteWatchEvent {
    Added(RouteRecordWire),
    Updated(RouteRecordWire),
    Removed {
        route_name: String,
        route_uid: RouteUid,
        catalog_revision: u64,
        reason: RouteStateReason,
    },
    Closed {
        route_name: String,
        route_uid: RouteUid,
        catalog_revision: u64,
        reason: RouteStateReason,
    },
    Heartbeat {
        catalog_revision: u64,
    },
    Compacted {
        compacted_revision: u64,
        current_revision: u64,
    },
}
```

Rules:

- Events are ordered by `catalog_revision`.
- A watcher must apply only the next expected revision or mark the directory
  dirty and relist.
- `Heartbeat` advances confidence without changing route state.
- `Compacted` means incremental repair is impossible from the requested
  revision. The client must mark the directory as compacted, discard every
  cached positive and negative route entry from that server, call `RouteList`,
  and resume watch from the returned `server_catalog_revision` /
  `relay_authority_revision`.
- `Compacted` is not a route removal event. It is a history-retention boundary:
  the authority is explicitly saying "I no longer have enough ordered events to
  prove your cache can be patched incrementally."
- Unknown event variants are protocol errors in 0.x. There is no compatibility
  downgrade path.

### RouteLookup

`RouteLookup` is an authoritative point query used when a route-bound acquire
must prove current state before it can proceed.

Request:

```rust
pub struct RouteLookupRequest {
    pub expected: c2_contract::ExpectedRouteContract,
    pub observed_route_uid: Option<RouteUid>,
    pub observed_route_revision: Option<u64>,
}
```

Response:

```rust
pub enum RouteLookupResponse {
    Ready(RouteRecordWire),
    NotFound { route_name: String },
    Removed { route_name: String, route_uid: Option<RouteUid> },
    Closed { route_name: String, route_uid: RouteUid, reason: RouteStateReason },
    Stale { current: RouteRecordWire },
    ContractMismatch { current: RouteRecordWire },
}
```

`RouteLookup` replaces cache-only `ensure_route_contract(...)` for safety
critical decisions.

### RouteAck And RouteNack

`RouteAck` and `RouteNack` are implemented with the watch protocol in Phase 3,
not merely reserved. Direct IPC clients send an ACK after applying a contiguous
event sequence. A NACK is sent when the client rejects an event due to invalid
wire data, contract validation failure, revision gap, or local apply error.

The server does not wait for ACK before making a route change authoritative.
ACK/NACK is diagnostic and flow-control input, not a distributed consensus
commit. After a NACK, the client marks its directory dirty and relists.

```rust
pub struct RouteAck {
    pub nonce: u64,
    pub applied_revision: u64,
}

pub struct RouteNack {
    pub nonce: u64,
    pub rejected_revision: u64,
    pub error: C2ErrorEnvelope,
}
```

## Client RouteDirectory

Each `IpcClient` owns a `RouteDirectory`:

```rust
pub struct RouteDirectory {
    pub server_identity: ServerIdentity,
    pub catalog_revision: u64,
    pub min_watch_revision: u64,
    pub health: DirectoryHealth,
    pub routes_by_name: HashMap<String, RouteRecord>,
}

pub enum DirectoryHealth {
    Clean,
    Dirty { reason: DirectoryDirtyReason },
    Compacted { compacted_revision: u64 },
}
```

Rules:

- A clean directory can satisfy a fast acquire only if the route record is
  `Ready`, the contract matches, the `server_instance_id` matches the connected
  server, and the directory has not missed any watch event.
- A dirty directory cannot use a cached positive result for safety critical
  route-bound acquire. It must call `RouteLookup` or relist.
- Negative cache entries are advisory only. `RouteLookup` remains authoritative.
- Watch disconnect marks the directory dirty. It does not close the IPC
  connection by itself.
- Watch compaction clears the route cache and relists before the next
  route-bound acquire.
- Watch event buffering is bounded. If the client cannot apply events fast
  enough, it marks the directory dirty, sends NACK if possible, drops buffered
  events, and relists before the next route-bound acquire.
- The watch task must not hold the route directory write lock while awaiting
  I/O, invoking callbacks, or sending ACK/NACK.

Python proxy boundary:

- Native `cc.connect(...)` returns a route token with the acquired client.
- Python `CRMProxy` does not interpret the token, but every remote call must
  pass through a native client that carries `route_uid` and observed revision.
- Closing and reconnecting a proxy can acquire a newer route token. A stale
  existing proxy must fail with typed stale/removed/closed errors rather than
  silently updating itself to a different resource instance.

Thread-local path:

- Same-process thread-local calls do not need IPC route watch, but they must not
  bypass route lifecycle state. `cc.unregister(...)`, route close, and shutdown
  must make existing thread-local proxies fail under the same logical
  `ResourceRemoved` / `ResourceClosed` semantics.
- Thread-local route identity can use the same `route_uid` stored by the native
  runtime session, with Python local bindings as the callback dispatch target.

## Server Dispatch Authority

Every route-bound call frame must carry:

```rust
pub struct RouteCallIdentity {
    pub route_name: String,
    pub route_uid: RouteUid,
    pub expected_contract_hash: ContractHashPair,
    pub observed_route_revision: u64,
}
```

Before resource callback execution, server dispatch must validate:

- route exists;
- route UID matches;
- route is `Ready`;
- route contract matches expected CRM tag and hashes;
- route revision is not stale under the route policy;
- method index belongs to the current method table;
- route admission is open.

Failure returns a typed C2 error and must not invoke Python resource code.

Wire change:

- `c2-wire` call control must grow from route name + method index to route call
  identity + method index.
- Golden fixtures must be updated for inline, SHM, chunked, and error replies.
- Method lookup must use the route record identified by `route_uid`, not the
  route name snapshot stored in the client.
- Unknown or absent route token is a protocol error in production remote paths,
  not a fallback to name-only dispatch.

`Draining` policy:

- `Draining` rejects all new remote calls by default with `ResourceClosed`
  carrying `state=Draining`.
- In-flight calls continue under the existing route admission and shutdown drain
  rules.
- A future read-only draining policy must be a separate explicit design. It is
  out of scope for this repair because allowing selected new calls during
  draining would make route lifecycle semantics harder to prove.

## Relay Redesign

Relay owns route authority for mesh/discovery. It does not own IPC server route
state. The relay runtime must split three concepts that are currently too
coupled:

### RouteAuthority

Stores route metadata, local owner identity, peer relay routes, leases,
tombstones, and catalog revisions. It does not store an `IpcClient` as proof of
route existence.

### UpstreamControl

Per local upstream server, uses a temporary attestation client during register
and a long-lived route watch after registration. The watch keeps RouteAuthority
current. It is not part of the relay data-plane idle pool.

If the upstream control watch disconnects, relay marks affected local routes as
`WatchDisconnected` but does not withdraw them. The next HTTP data-plane acquire
must perform `RouteLookup` against the expected server instance before forwarding
or returning a typed `RouteWatchUnavailable` / `ResourceUnavailable`.

### UpstreamDataPool

Creates IPC clients only for HTTP probe/call forwarding. Idle eviction applies
only here. Evicting a data-plane client must not unregister, withdraw, or
invalidate the route.

Relay withdraw is allowed only after semantic proof:

- explicit unregister;
- owner lease expiration;
- upstream route watch reports `Removed` or terminal `Closed`;
- `RouteLookup` against the expected server instance confirms not found,
  removed, closed, identity mismatch, or contract mismatch;
- peer relay sends a valid newer tombstone.

Relay withdraw is forbidden for:

- `Connection reset by peer`;
- EOF;
- connection refused;
- timeout;
- frame decode error;
- protocol violation;
- idle data-plane eviction;
- pool close.

Those failures mark only the current data-plane client unhealthy.

## HTTP Relay-Aware Client

HTTP and direct IPC must share route token semantics:

- relay resolve returns `route_uid`, `route_revision`, `server_id`,
  `server_instance_id`, relay owner identity, CRM contract, and route state;
- HTTP probe/call carries expected CRM contract and observed route token;
- relay validates RouteAuthority before acquiring upstream;
- upstream IPC server validates again at call dispatch;
- stale HTTP route errors are structured so the client can re-resolve and try
  a different candidate when one exists.

Loopback self-fallback rule:

- If a loopback relay resolve result selects a local IPC endpoint and direct IPC
  acquire fails, the client must not fallback to the same local relay HTTP data
  plane.
- The client may continue to a candidate with a different relay URL, server
  instance, or nonlocal owner.
- When no distinct candidate exists, expose the original typed IPC error to the
  caller.

## Error Taxonomy

Rust internal errors must be precise enough for relay policy decisions.

```rust
pub enum RouteConnectErrorKind {
    TransportIo,
    FrameDecode,
    ProtocolViolation,
    IdentityMismatch,
    ContractMismatch,
    RouteNotFound,
    RouteRemoved,
    RouteClosed,
    RouteStale,
    CatalogCompacted,
    WatchDisconnected,
    LeaseExpired,
    LoopbackFallbackDenied,
}
```

Public C2 error registry additions:

```text
ResourceClosed
ResourceRemoved
RouteStale
ContractMismatch
IdentityMismatch
RouteCatalogCompacted
RouteWatchUnavailable
ProtocolViolation
FallbackDenied
```

These public names are part of the C-Two `CCError` surface, not only relay log
labels. Each public code must be:

- registered in Rust `c2-error` with a stable numeric code, canonical name, and
  default message category;
- encoded and decoded through `C2ErrorEnvelope`;
- usable by Rust IPC, server, runtime, HTTP, and relay crates without ad hoc
  string parsing;
- exported by the PyO3 native error registry;
- generated as Python `ERROR_Code` values and concrete `CCError` subclasses;
- represented in relay HTTP JSON errors with the same `code`, `name`,
  `message`, and `details` shape.

Internal `RouteConnectErrorKind` values may be more granular than public error
classes, but every public-facing failure must cross the Rust/Python boundary as
a registered C2 error code.

Mapping:

| Internal condition | Public error | Relay withdraw? |
| --- | --- | --- |
| connection reset, EOF, refused, timeout | `ResourceUnavailable` with `cause_kind=TransportIo` | no |
| frame decode failure | `ProtocolViolation` | no |
| unsupported route catalog response | `ProtocolViolation` | no |
| expected vs actual server identity mismatch | `IdentityMismatch` | yes, only for that attested owner |
| expected CRM tag/hash mismatch | `ContractMismatch` | yes, only after authoritative lookup |
| authoritative missing route | `ResourceNotFound` | yes |
| authoritative removed route | `ResourceRemoved` | yes |
| authoritative closed route | `ResourceClosed` | yes |
| observed route UID/revision is old | `RouteStale` | no, re-resolve first |
| watch history compacted | `RouteCatalogCompacted` | no, relist |
| watch disconnected and no safe route state | `RouteWatchUnavailable` | no |
| local IPC failed and same loopback relay fallback denied | `FallbackDenied` | no |

Error wire becomes an envelope:

```rust
pub struct C2ErrorEnvelope {
    pub version: u16,
    pub code: c2_error::ErrorCode,
    pub name: String,
    pub message: String,
    pub details: BTreeMap<String, String>,
}
```

Wire encoding:

- `c2-error` owns the canonical codec.
- Empty error payload still means "no error".
- Non-empty error payload is `b"C2E1"` followed by deterministic JSON for
  `C2ErrorEnvelope`.
- `details` is a `BTreeMap<String, String>` so golden fixtures are stable.
- `name` is redundant with `code`, but is included to improve log readability
  and unknown-code diagnostics.
- The old `code:message` decoder is removed in this clean cut. Tests that
  currently assert `b"703:grid exists"` must be rewritten to assert the new
  `C2E1` envelope fixture.

Registration control errors and relay HTTP errors must reuse the same logical
codes and detail keys even when transported as HTTP JSON. The HTTP JSON shape
does not need to be byte-identical to `C2E1`, but it must carry `code`, `name`,
`message`, and `details`.

## Python SDK Boundary

Python exposes generated `ERROR_Code` values and concrete `CCError` subclasses
for every public code added by this design. It must not perform route catalog
decisions.

Python `cc.connect(...)` behavior:

- explicit `address="ipc://..."` uses direct IPC only and bypasses relay;
- name-based connect via loopback relay can choose direct IPC when identity and
  route token match;
- name-based connect via nonlocal relay uses HTTP relay target;
- fallback decisions are returned from Rust native with typed reasons;
- Python logs diagnostic context but does not reclassify failures.

`CCError` objects carry optional `details` from the Rust error envelope. Tests
must prove malformed `C2E1` payloads and unknown future error codes do not crash
Python and map to `Unknown` with details preserved when possible. Legacy
`code:message` payloads are not accepted by the new codec after the clean cut.

## Implementation Phases

Each phase ends with review, remediation, verification, and a commit. Do not
advance while the phase has unresolved correctness findings.

### Phase 0: Spec And Baseline

Deliverables:

- commit the current transitional stale-snapshot fix as a rollback baseline;
- remove temporary repro directories after scenarios are captured in tests or
  this plan;
- add this route catalog/watch plan;
- review the plan against current production pain points and regression risk.

Verification:

- `git status --short` is clean after the baseline commit;
- plan review has no unresolved P1/P2 findings.

Status:

- baseline commit: `49005a5 fix: refresh stale ipc route snapshots`;
- temporary `tmp/` repro folders removed;
- plan review passes 1 and 2 completed with no open Phase 1 blockers.

### Phase 1: Typed Error Foundation

Deliverables:

- replace broad route-acquire `Handshake(String)` use with typed Rust variants;
- add `C2ErrorEnvelope` and public registry entries in `c2-error`;
- update Python generated error mapping and subclasses;
- update IPC/HTTP/relay code to classify errors without string matching;
- keep route selection and fallback behavior unchanged except for typed errors
  and the new error envelope.

Acceptance:

- relay withdraw helper can express semantic vs transport failures without
  matching `Handshake`;
- Python can deserialize and raise new typed route/catalog errors;
- no fallback behavior changes yet.

Verification:

- `cargo test --manifest-path core/Cargo.toml -p c2-error`
- `cargo test --manifest-path core/Cargo.toml -p c2-ipc`
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay`
- `uv sync --reinstall-package c-two`
- `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_error.py sdk/python/tests/unit/test_runtime_session.py -q --timeout=30`

Status:

- implemented Rust `C2E1` error envelope in `c2-error`;
- registered route/catalog public error codes and exported Python `CCError`
  subclasses with `details`;
- replaced non-handshake IPC route/contract/protocol/SHM/chunk failures with
  typed `IpcError` variants;
- updated relay acquisition/withdraw classification to use typed semantic
  variants instead of broad `Handshake(String)`;
- verification passed on 2026-06-05:
  - `cargo test --manifest-path core/Cargo.toml -p c2-error`;
  - `cargo test --manifest-path core/Cargo.toml -p c2-wire`;
  - `cargo test --manifest-path core/Cargo.toml -p c2-ipc`;
  - `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay`;
  - `cargo test --manifest-path core/Cargo.toml -p c2-runtime`;
  - `uv sync --reinstall-package c-two`;
  - `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_error.py sdk/python/tests/unit/test_native_error_registry.py sdk/python/tests/unit/test_mesh_errors.py sdk/python/tests/unit/test_runtime_session.py -q --timeout=30`;
  - `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_error_propagation.py -q --timeout=30`.

### Phase 2: Server RouteCatalog And Call-Time Validation

Deliverables:

- introduce server-side `RouteCatalog` with revisions, route UID, state, and
  event log;
- route register/unregister/shutdown update the catalog atomically with
  dispatcher state;
- call frames carry route UID and observed revision;
- dispatch validates route identity/state/contract before callback invocation;
- remove handshake snapshot as authority.
- add event-log retention with a count limit and a time limit. Initial internal
  constants: retain at least 4096 route events and at least 10 minutes of route
  history. These are internal constants, not new environment variables.
- update `c2-wire` call control to carry route call identity and update all
  canonical wire fixtures.

Acceptance:

- cached existing route removed after handshake is rejected before resource
  callback;
- route name replacement with same contract creates new UID and old proxies get
  `RouteStale` or `ResourceRemoved`;
- later route registration is observable through catalog state, not only
  one-off refresh.
- thread-local stale proxies observe the same route lifecycle errors as remote
  proxies.

Verification:

- focused fail-first tests for remove, close, replacement, same-name same-contract
  replacement, method index under stale route;
- wire fixture tests for route call identity;
- `cargo test --manifest-path core/Cargo.toml -p c2-server`
- `cargo test --manifest-path core/Cargo.toml -p c2-ipc`

### Phase 3: IPC RouteList, RouteLookup, RouteWatch, RouteDirectory

Deliverables:

- add wire messages for list, lookup, watch, heartbeat, compacted;
- implement client `RouteDirectory`;
- add watch task lifecycle to `IpcClient`;
- implement `RouteAck` and `RouteNack`;
- make acquire use clean directory fast path or authoritative lookup/relist;
- remove `refresh_route_contract(...)` and `ensure_route_contract(...)` as
  production APIs.

Acceptance:

- a route registered after handshake reaches existing client through watch
  before first call where possible;
- missed watch events mark directory dirty and force lookup/relist;
- compacted history clears cache and rebuilds;
- NACK marks the directory dirty and forces relist;
- bounded watch overflow marks the directory dirty and forces relist;
- direct explicit IPC works without relay configured.

Verification:

- `cargo test --manifest-path core/Cargo.toml -p c2-wire`
- `cargo test --manifest-path core/Cargo.toml -p c2-ipc`
- direct IPC integration test with relay env unset and unavailable relay env.

### Phase 4: Relay RouteAuthority And Upstream Boundaries

Deliverables:

- split local upstream registration attestation, upstream route watch, and
  data-plane IPC pool;
- register stores route metadata and owner identity, not a long-lived
  registration data-plane client;
- idle eviction only affects `UpstreamDataPool`;
- relay RouteAuthority updates from upstream route watch and explicit unregister;
- relay catalog event compaction is tied to tombstone retention. GC can remove a
  tombstone only after the corresponding route remove event is older than the
  catalog compaction boundary;
- generic I/O and protocol failures no longer withdraw routes.

Acceptance:

- register followed by no HTTP probe/call creates no idle-evictable data-plane
  upstream client;
- explicit HTTP call can create and later idle-evict a data-plane upstream
  without withdrawing the route;
- upstream route removed via watch withdraws route with named reason;
- connection reset during data-plane reconnect returns typed unavailable and
  keeps route.

Verification:

- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay`
- tests for register-only, explicit HTTP idle eviction, route watch remove,
  route watch compacted, generic I/O no withdraw.
- tombstone GC logs name, relay id, server id, route UID when available, removed
  revision, and compaction revision.
- relay mesh route ordering tests prove old timestamp-only ordering no longer
  decides active vs tombstone state.

### Phase 5: Relay-Aware HTTP And Loopback Fallback Clean Cut

Deliverables:

- resolve responses include route UID, revision, server instance, and state;
- HTTP call/probe carries route token and expected contract;
- relay call path validates RouteAuthority route token before upstream acquire;
- loopback self-fallback is deleted;
- only genuinely distinct remote candidates can be tried after local IPC
  failure.

Acceptance:

- loopback relay resolving local IPC never falls back to the same local relay
  HTTP data plane after direct IPC failure;
- distinct remote relay candidate can still be tried;
- stale route token causes re-resolve, not ambiguous call replay;
- HTTP and IPC stale route behavior share the same error taxonomy.

Verification:

- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay`
- `uv sync --reinstall-package c-two`
- `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_http_relay.py sdk/python/tests/unit/test_runtime_session.py -q --timeout=30`

### Phase 6: Cleanup, Docs, Full Verification

Deliverables:

- remove transitional route contract refresh APIs and tests that assert the old
  model;
- update AGENTS-sensitive docs to state the new route catalog boundary;
- update issue doc to point to this implemented plan;
- remove dead code, stale comments, and compatibility shims;
- run final review and full verification.

Verification:

- `cargo test --manifest-path core/Cargo.toml --workspace`
- `uv sync --reinstall-package c-two`
- `C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30`
- `git diff --check`
- source scans:
  - `rg -n "Handshake\\(" core/transport core/runtime sdk/python/native/src`
    returns only startup handshake/decode contexts, not route-policy matching;
  - `rg -n "ensure_route_contract|refresh_route_contract" core sdk/python/native/src`
    returns no production API references;
  - `rg -n "resolve\\([^,)]*name|name_only|route_names\\(\\)" core/transport/c2-http sdk/python/native/src`
    returns no production relay resolve/call path;
  - `rg -n "falling back to HTTP relay|LoopbackFallbackDenied|same local relay" sdk/python/native/src core/transport/c2-http`
    proves same-relay loopback HTTP fallback is denied.

## Test Matrix

| Scenario | Expected result | Phase |
| --- | --- | --- |
| route registered after client handshake | watch updates directory or lookup succeeds without HTTP fallback | 3 |
| route removed after client cached it | next acquire/call returns `ResourceRemoved` before callback | 2 |
| route closed/draining | new calls rejected with `ResourceClosed`; in-flight policy honored | 2 |
| same route name re-registered with same contract | old proxy rejected by UID/revision | 2 |
| same route name re-registered with different contract | `ContractMismatch`; no call dispatch | 2 |
| server process restarts with same server_id | `server_instance_id` mismatch prevents old route acceptance | 2 |
| thread-local proxy after unregister | fails with route lifecycle error instead of calling removed binding | 2 |
| watch disconnect | directory dirty; no cache-positive acquire without lookup/relist | 3 |
| watch compacted | cache cleared; RouteList rebuild required | 3 |
| watch buffer overflow | directory dirty; RouteList rebuild required | 3 |
| relay register only, no HTTP calls | no idle-evictable data-plane upstream | 4 |
| explicit HTTP call through relay | creates data-plane upstream that may be idle-evicted safely | 4 |
| data-plane reconnect reset | `ResourceUnavailable`, route retained | 4 |
| upstream route watch removed | relay withdraws with semantic reason | 4 |
| loopback local IPC failure | no same-relay HTTP fallback; original typed IPC error exposed | 5 |
| local IPC failure with distinct remote candidate | remote candidate may be attempted | 5 |
| relay tombstone GC | logs route names and revisions; watch compaction semantics preserved | 4 |
| direct explicit IPC with relay env unavailable | unaffected by relay | 3 |

## Regression Guardrails

- Do not move route consistency logic into Python.
- Do not use route name alone for production resolve, probe, call, or cache keys.
- Do not use connection pool membership as proof of route liveness.
- Do not withdraw on generic I/O, EOF, timeout, refused, decode, or protocol
  errors.
- Do not accept cached route state after the directory is dirty or compacted.
- Do not invoke resource callbacks before route UID, revision, state, method
  index, and contract are validated.
- Do not keep transitional compatibility shims after Phase 6.
- Do not add environment variables for behavior that can be derived from route
  catalog state, relay identity, and existing timeout/pool settings.

## Spec Review Checklist

Review every phase before implementation and after implementation:

- [ ] Covers route registered after handshake.
- [ ] Covers cached route removed or closed after handshake.
- [ ] Covers route name ABA via `route_uid`.
- [ ] Covers server process ABA via `server_instance_id`.
- [ ] Covers relay idle eviction without route withdrawal.
- [ ] Covers relay register/unregister logging and typed caller errors.
- [ ] Covers tombstone GC and watch compaction semantics.
- [ ] Covers direct IPC independence from relay.
- [ ] Covers loopback self-fallback deletion.
- [ ] Covers HTTP stale route token behavior.
- [ ] Covers Rust internal typed errors and Python public `CCError` classes.
- [ ] Defines source scans to remove obsolete APIs and avoid old-code
  hangers-on.
- [ ] Keeps hot-path performance bounded through clean directory fast path,
  route-specific watch subscriptions, and no per-call full catalog list.

## Review Pass 1

Status: completed on 2026-06-05.

Findings and resolutions:

1. `C2ErrorEnvelope` encoding was underspecified. Resolution: use `c2-error`
   owned `C2E1` deterministic JSON envelope and remove old `code:message`
   tests in the clean cut.
2. Route watch transport was ambiguous. Resolution: multiplex route watch events
   on the same IPC connection with signal/control frames; relay may keep a
   separate control-only client per upstream server.
3. Event retention and tombstone GC were not coupled. Resolution: catalog event
   compaction and tombstone retention share a revision boundary; tombstone GC
   logs compaction metadata.
4. `Draining` allowed policy ambiguity. Resolution: default rejects all new
   remote calls; read-only draining is explicitly out of scope.
5. ACK/NACK was optional. Resolution: implement ACK/NACK in Phase 3 for
   diagnostics, NACK recovery, and future flow-control input.

Remaining open items: none for Phase 1 start. New findings from later review
passes must be added here before implementation proceeds.

## Review Pass 2

Status: completed on 2026-06-05.

Findings and resolutions:

1. `catalog_revision` was ambiguous across IPC server catalog and relay route
   authority. Resolution: docs now require layer-specific field names when both
   appear in one payload and forbid timestamp-only route ordering.
2. The plan changed call dispatch semantics but did not explicitly require
   `c2-wire` call-control fixture changes. Resolution: Phase 2 now requires
   route call identity in call control and golden fixture updates.
3. Watch backpressure was missing. Resolution: bounded overflow marks the
   directory dirty, drops buffered events, sends NACK if possible, and relists
   before acquire.
4. Python proxy and thread-local behavior were under-specified. Resolution:
   native connect returns route token; stale remote proxies fail instead of
   silently rebinding; thread-local proxies observe route lifecycle errors.
5. Relay mesh still risked retaining old timestamp ordering semantics.
   Resolution: Phase 4 now requires route UID / authority revision ordering
   tests proving timestamp-only ordering is gone.

Remaining open items: none for Phase 1 start.

## Review Pass 3

Status: completed on 2026-06-05.

Findings and resolutions:

1. `owner_server_id` and `owner_server_instance_id` needed sharper semantics.
   Resolution: `owner_server_id` is the stable logical owner identity, while
   `owner_server_instance_id` is the live owner incarnation and is mandatory for
   rejecting server ABA after restarts or native server recreation.
2. `Compacted` could be mistaken for a route removal event. Resolution:
   `Compacted` is defined as a route-event history retention boundary; clients
   must clear route directories and rebuild with `RouteList`, not withdraw
   routes based on compaction alone.
3. The new error taxonomy needed an explicit public SDK contract. Resolution:
   every public route/catalog error introduced by this design must be registered
   in Rust `c2-error`, encoded through `C2ErrorEnvelope`, exported through PyO3,
   and generated as Python `ERROR_Code` values plus concrete `CCError`
   subclasses.

Remaining open items: none for Phase 1 start.
