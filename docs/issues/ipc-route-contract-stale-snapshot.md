# IPC Route Contract Stale Snapshot

**Date**: 2026-06-05
**Status**: Route catalog/watch redesign in progress; stale snapshot and relay
data-plane route-token hazards are covered by current core changes
**Severity**: High for long-lived multi-route IPC servers
**Scope**: `c2-ipc` client route catalog, direct IPC acquisition, relay upstream IPC acquisition, relay registration attestation

## Summary

An IPC client learns the server's route table during the initial handshake. That
handshake is a snapshot. When a long-lived IPC server registers another route
after a client connection already exists, the existing client can still report
`route missing` for the newly committed route.

This affected two production-relevant paths:

- direct IPC clients reused from the address-keyed `ClientPool`;
- HTTP relay data-plane calls when the relay reused an upstream `IpcClient`.

The repair moved beyond a single live lookup. IPC servers now expose a route
catalog with per-route identity and revisions, IPC clients can watch catalog
events, and relay data-plane calls bind acquisition to the selected route token
before dispatch. Direct IPC and relay upstream IPC still share the same
IPC-owned validation path; relay no longer treats route-name lookup alone as
enough to call an upstream.

## Reproduced Failure

The failing sequence is:

1. IPC server exports `manager`.
2. A client connects to the IPC address and caches a handshake snapshot with
   only `manager`.
3. The same server later commits `builder`.
4. The client pool reuses the existing IPC client for `builder`.
5. Local cached validation reports `route missing: builder` even though the
   server is alive and the route is committed.
6. Relay-aware SDK paths can then fall back to HTTP, making the real IPC cache
   defect look like a relay upstream reconnect problem.

## Decision

`c2-ipc` and `c2-server` own the route-catalog refresh/watch mechanism.

- `validate_route_contract(...)` remains a cache-only check.
- `refresh_route_contract(route_name)` asks the connected server for the current
  committed route contract and method table, then updates the client cache.
- `ensure_route_contract(expected)` and route-token validation first try the
  cache and perform a live query on cache miss, mismatch, or stale token.
- When `refresh_route_contract(...)` is called, a live `RouteNotFound` response
  is the semantic proof that the connected server does not currently export the
  committed route.
- Watch events proactively add, update, and remove client-side route records;
  compaction explicitly tells the client its watch offset is too old and a full
  refresh is required.
- Relay HTTP probe/call prechecks keep the selected route snapshot and acquire a
  route-bound upstream binding. If the route is replaced between precheck and
  acquire, relay returns `RouteStale` instead of replaying the call against the
  replacement.

This keeps the fast path cheap while removing the assumption that the handshake
route list is a permanent catalog.

## Known Remaining Gaps

The current remaining gaps are narrower:

- Some relay control-plane and malformed-request HTTP errors still use legacy
  ad hoc JSON bodies. Route semantic data-plane errors now use canonical C2
  error envelopes and Python maps them back into `CCError` subclasses.
- Additional SDKs must project the Rust `c2-error` registry the same way the
  Python SDK does.
- Future remote transports must preserve the same route-token binding and
  canonical error-envelope behavior rather than reintroducing route-name-only
  dispatch.

## HTTP Client Boundary

External HTTP relay clients do not have the same snapshot shape.

`RelayControlClient` caches resolution results by the full
`ExpectedRouteContract`, not by route name or server address. `HttpClient` calls
and probes also send the expected CRM contract headers on every request. Reusing
an HTTP connection pool entry for the same relay URL does not reuse an IPC
server route table.

The HTTP relay data plane still depends on IPC behind the relay. A request such
as `POST /builder/ping` acquires a relay-owned upstream `IpcClient`. That
upstream client is subject to IPC catalog drift, so relay acquisition validates
the selected route token before dispatch. A replacement route under the same
name is not an acceptable substitute for the prechecked route.

## Registration Boundary

Relay registration still connects to the IPC server for identity and route
contract attestation. That validation no longer treats `has_route(...)` from the
handshake snapshot as authoritative. Committed route registration uses the live
route contract query; pending route registration continues to use the pending
route attestation token path.

## Non-Goals

- No Python-side route refresh logic.
- No separate relay-specific IPC refresh implementation.
- No route subscription, route generation, or pushed route-table invalidation.
- No compatibility shim for accepting name-only CRM dispatch.
- No change to ordinary external HTTP client pooling semantics.

## Regression Coverage

- `c2-ipc`: a pooled direct IPC client connected before `builder` is registered
  must refresh and validate `builder` after the server commits it.
- `c2-http` relay: an HTTP relay data-plane request must succeed when the relay
  upstream slot contains an `IpcClient` whose original handshake snapshot lacks
  the later committed route.
- `c2-http` relay: a call prechecked against one route token must reject a
  same-contract replacement route with canonical `RouteStale`.
- Python SDK: a canonical relay route error returned during an HTTP CRM call
  must raise the matching `CCError` subclass instead of a generic
  `RuntimeError` / `ClientCallResource`.
