# IPC Route Contract Stale Snapshot

**Date**: 2026-06-05
**Status**: Addressed by the route catalog/watch redesign. This issue is kept
as the historical root-cause record; the implemented design and verification
record live in `docs/plans/2026-06-05-route-catalog-watch-redesign.md`.
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

`c2-ipc` and `c2-server` own route catalog lookup, watch, and call-time route
token validation.

- `RouteList`, `RouteLookup`, and `RouteWatch` replace handshake-snapshot
  authority.
- The old production `refresh_route_contract(...)` path has been removed.
- Internal route ensure boundaries are route-catalog backed: they use clean
  directory state only when the watch stream is current, otherwise they perform
  authoritative lookup or relist.
- Watch events proactively add, update, close, and remove client-side route
  records. `Compacted` explicitly tells the client its watch offset is too old
  and a full relist is required.
- Relay HTTP probe/call prechecks keep the selected route snapshot and acquire a
  route-bound upstream binding. If the route is replaced between precheck and
  acquire, relay returns `RouteStale` instead of replaying the call against the
  replacement.
- Relay authority, upstream control watch, and upstream data-plane pooling are
  separate mechanisms. Idle eviction affects only relay-created data-plane IPC
  clients and does not withdraw routes.

This keeps the fast path cheap while removing the assumption that the handshake
route list is a permanent catalog.

## Remaining Boundaries

- Additional SDKs must project the Rust `c2-error` registry the same way the
  Python SDK does.
- Future remote transports must preserve the same route-token binding and
  canonical error-envelope behavior rather than reintroducing route-name-only
  dispatch.
- The relay route-authority event log is currently an internal primitive. A
  public relay watch/list surface should be added only when a concrete mesh
  consumer needs it.

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
- Python SDK: when loopback relay resolution selects local IPC and that direct
  acquire fails, the runtime must not retry the same failed route through the
  same relay HTTP data plane. With no distinct target, callers see
  `FallbackDenied` carrying `direct_ipc_failure` details.
- Python SDK: a same-process thread-local proxy must still pass through native
  route admission and raise public `ResourceClosed` after unregister, rather
  than calling a removed Python resource binding directly.
