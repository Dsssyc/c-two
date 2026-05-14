# CrmTag Resource Discovery Hardening Plan

## Goal

Make resource discovery and relay data-plane forwarding require both the user
route name and the full CRM access contract tag. `name` remains the resource
instance routing key. `CrmTag { namespace, name, version }` is the CRM model
identity, where `CrmTag.name` is the CRM contract class/model name.

## Root Cause

The current system treats resource discovery as name-first. Rust/relay metadata
stores and gossips `crm_ns` and `crm_ver`, but drops the CRM model name that
Python already encodes in `__tag__ = namespace/ClassName/version`. This creates
two concrete gaps:

- Relay lazy reconnect can accept a new IPC server at the same address by
  checking only `has_route(name)`.
- HTTP relay clients filter cached route results by partial CRM identity, but
  calls and probes do not carry an expected tag to the relay data plane.

## Non-Goals

- Do not add `@cc.crm(name=...)` or any CRM tag alias. The CRM class/model name
  is the tag name.
- Do not make one relay own multiple local routes with the same routing `name`.
  `name` remains unique per relay owner; `CrmTag` is a discovery and validation
  constraint, not a second instance key.
- Do not move Python method discovery or Python resource invocation into Rust.

## Phase 11.1: CrmTag Wire And Server Metadata

**Status:** complete

### Files

- `core/protocol/c2-wire/src/handshake.rs`
- `core/protocol/c2-wire/src/tests.rs`
- `core/transport/c2-server/src/dispatcher.rs`
- `core/transport/c2-server/src/server.rs`
- `core/transport/c2-ipc/src/client.rs`
- `core/transport/c2-ipc/src/sync_client.rs`
- `sdk/python/native/src/wire_ffi.rs`
- `sdk/python/native/src/server_ffi.rs`
- `sdk/python/native/src/runtime_session_ffi.rs`
- `sdk/python/src/c_two/transport/server/native.py`
- `sdk/python/src/c_two/transport/registry.py`
- `sdk/python/tests/unit/test_wire.py`
- `sdk/python/tests/unit/test_ipc_config.py`
- `sdk/python/tests/integration/test_registry.py`

### Required RED Tests

- Wire handshake roundtrip includes `crm_name` between namespace and version.
- Direct IPC rejects same route `name` when namespace/version match but
  `CrmTag.name` differs.
- Same-process thread-local connect rejects a mismatched `CrmTag.name`.

### Implementation

- Add Rust `CrmTag` structs in the relevant wire/server/client layers rather
  than continuing to pass loose `(crm_ns, crm_ver)` pairs.
- Keep field names on public JSON/Python surfaces explicit:
  `crm_ns`, `crm_name`, `crm_ver`.
- Python `_crm_contract_identity()` returns `(namespace, class_name, version)`.
- Native registration builds `CrmTag.name` from the CRM class name and sends it
  through RuntimeSession into `CrmRoute` and the IPC handshake.

### Review Gate

- Read all changed wire/server/client/native/Python paths.
- Verify no code path derives `CrmTag.name` from resource routing `name`.
- Verify no Python-owned route selection authority is introduced.
- Verify wire length limits apply to all three tag fields.

## Phase 11.2: Relay Gossip And Resolve Use Full CrmTag

**Status:** complete

### Files

- `core/transport/c2-http/src/relay/types.rs`
- `core/transport/c2-http/src/relay/peer.rs`
- `core/transport/c2-http/src/relay/peer_handlers.rs`
- `core/transport/c2-http/src/relay/route_table.rs`
- `core/transport/c2-http/src/relay/authority.rs`
- `core/transport/c2-http/src/relay/router.rs`
- `core/transport/c2-http/src/relay/server.rs`
- `core/transport/c2-http/src/relay/test_support.rs`
- `core/transport/c2-http/src/client/control.rs`
- `core/transport/c2-http/src/client/relay_aware.rs`
- `core/runtime/c2-runtime/src/session.rs`

### Required RED Tests

- `RouteAnnounce`, `DigestDiffEntry::Active`, full snapshots, and
  `/_resolve` preserve `crm_name`.
- Resolve with expected tag excludes routes whose `crm_name` differs even when
  route `name`, namespace, and version match.
- Cached wrong-tag route forces a fresh resolve and then returns
  `CRMContractMismatch`, not `ResourceNotFound`.

### Implementation

- Add `crm_name` to `RouteEntry`, `RouteInfo`, peer gossip, anti-entropy digest
  hashes, snapshot merge, and route table replacement.
- Add expected-tag query support to resolve APIs and remove name-only runtime
  resolve behavior. Any future resource-search diagnostic must be a separate
  discovery surface that returns route plus CRM tag/hash metadata, not a call
  routing fallback.
- Relay-aware clients pass expected `CrmTag` into resolve, cache filtering, and
  route ordering. Cache remains name-keyed only if mismatch still triggers a
  forced refresh; otherwise key by `(name, CrmTag)`.

### Review Gate

- Read every route ingestion path: HTTP register, CLI upstream, peer announce,
  digest diff, full snapshot.
- Verify peer-private fields are still scrubbed and no IPC address leaks to
  peers/nonlocal callers.
- Verify digest hash includes every public route field that can affect
  discovery: `relay_url`, `crm_ns`, `crm_name`, `crm_ver`, `registered_at`.

## Phase 11.3: Relay Data Plane And Lazy Reconnect Enforce CrmTag

**Status:** complete

### Files

- `core/transport/c2-http/src/relay/state.rs`
- `core/transport/c2-http/src/relay/router.rs`
- `core/transport/c2-http/src/client/client.rs`
- `core/transport/c2-http/src/client/relay_aware.rs`
- `sdk/python/native/src/http_ffi.rs`
- `sdk/python/native/src/runtime_session_ffi.rs`
- `sdk/python/tests/integration/test_http_relay.py`

### Required RED Tests

- After idle eviction, relay lazy reconnect rejects an IPC server with matching
  route `name` but different `server_instance_id`.
- After idle eviction, relay lazy reconnect rejects matching route `name` but
  different `CrmTag`.
- HTTP relay call with a cached old compatible route returns
  `CRMContractMismatch` if the relay now owns the same `name` with a different
  tag.
- Stale reconnect failure must not remove a newly registered same-address route
  whose identity/tag no longer matches the stale lease.

### Implementation

- `RelayState::acquire_upstream()` reads the current local `RouteEntry` as an
  attestation snapshot and validates reconnect handshakes against address,
  `server_id`, `server_instance_id`, and full `CrmTag`.
- Route removal for unreachable upstreams must use the same owner/tag snapshot
  or an owner token, not address-only matching.
- Add expected tag headers to HTTP probe/call from relay-aware clients:
  `x-c2-expected-crm-ns`, `x-c2-expected-crm-name`,
  `x-c2-expected-crm-ver`.
- Relay `/_probe/{name}` and `/{name}/{method}` validate the expected tag
  before acquiring/forwarding the upstream.

### Review Gate

- Read state/conn-pool/router/client code together for TOCTOU behavior.
- Verify no CRM method call is replayed after a possible send.
- Verify normal successful calls do not add an extra resolve per call beyond
  the existing relay-aware route loop.
- Verify contract mismatch is a hard error and never falls through to HTTP
  fallback.

## Phase 11.4: Strict Completion Review And Verification

**Status:** complete

### Required Verification

- `cargo test --manifest-path core/Cargo.toml -p c2-wire -- --nocapture`
- `cargo test --manifest-path core/Cargo.toml -p c2-server -- --nocapture`
- `cargo test --manifest-path core/Cargo.toml -p c2-ipc -- --nocapture`
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture`
- `cargo test --manifest-path core/Cargo.toml -p c2-runtime -- --nocapture`
- `cargo check --manifest-path sdk/python/native/Cargo.toml -q`
- Focused Python suites covering wire, IPC config, runtime session, registry,
  server, and HTTP relay.
- `cargo fmt --manifest-path core/Cargo.toml --all --check`
- `cargo fmt --manifest-path sdk/python/native/Cargo.toml --all --check`
- `git diff --check`

### Completion Audit

- Confirm every CRM identity path uses `CrmTag` or the explicit three-field
  equivalent.
- Confirm no active production path uses `(crm_ns, crm_ver)` as the complete
  contract identity.
- Confirm route discovery and data-plane forwarding both require `name` and
  full `CrmTag` for typed clients.
- Confirm direct IPC remains relay-independent.
- Confirm route `name` remains the resource instance key and `CrmTag.name`
  remains the CRM model/class name.

## Phase 11.5: Recovery Review Remediation

**Status:** complete

### Scope

Follow up on the strict recovery review performed after session
`019e0703-b98c-77e1-ad0b-482dbfca21b1` was damaged.

### Required RED Tests

- Relay `/_probe/{name}` and `/{name}/{method}` reject a CrmTag mismatch on
  the acquired route snapshot, not only the route table state observed before
  acquisition.
- Peer `RouteAnnounce`, `DigestDiffEntry::Active`, and full snapshot merge
  reject empty, oversized, or control-character CrmTag fields before mutating
  the route table.
- Relay control resolve cache uses the complete expected route contract as its
  key, so entries for different CRM tags or hashes cannot collide through route
  name reuse; route/tag validators reject control-character identities at
  boundaries.

### Implementation Notes

- Prefer a small Rust-side CrmTag validation helper in the relay route-table
  authority layer before considering wider shared-crate movement.
- Do not keep low-level name-only resolve behavior on runtime relay resolution
  paths. A future resource-search diagnostic may query by name, but it must be
  a separate surface that returns candidate route metadata instead of producing
  a callable route without a full expected contract.
- Avoid a per-call network resolve. Post-acquire validation should compare the
  already acquired `RouteEntry` snapshot.

### Review Gate

- Re-read route-table ingestion, peer handlers, relay router data-plane, relay
  state acquisition, and control-client cache invalidation together.
- Confirm no method call is sent before the final acquired-route CrmTag check.
- Confirm invalid snapshot input cannot partially mutate routes, tombstones, or
  peer state.
- Confirm cache invalidation removes all entries for a route without relying on
  string-prefix hacks.
