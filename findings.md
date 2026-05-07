# Findings

## Dead Peer Route Resurrection

- Current `trusted_peer()` and `RouteAuthority::trusted_peer_owner()` use peer existence rather than liveness.
- A peer marked `Dead` remains in the peer table for probing/recovery, so existence is not enough to trust route mutations.
- Delayed `RouteAnnounce` and `DigestDiff::Active` can currently reinsert routes after failure detection removed them.
- Resolution is also a defensive boundary: peer routes owned by non-Alive peers should not be returned if any stale route survives through direct table mutation, snapshot merge, or future code.

## Recovery Path Constraint

- `dead_peer_probe_loop` marks the peer Alive before digest exchange, so requiring Alive for route mutations should still allow recovered peers to rejoin once health probing succeeds.

## Snapshot Linkage

- `merge_snapshot` is another route-ingestion path.
- It must not import active routes for peers whose snapshot status is not Alive.
- It must preserve incoming peer status for newly inserted peers; defaulting every snapshot peer to Alive can bypass failure state from the sender's view.

## SDK Responsibility Boundary

- Language SDKs should not own transport/control-plane mechanisms beyond transferable serialization and presentation.
- SDKs should act as typed glue into the Rust core: code-level overrides, CRM object/method binding, and proxy construction are acceptable; env resolution, relay control-plane correctness, retries/idempotency, route caching, and failure fallback belong in Rust.
- This boundary prevents duplicated behavior across future SDKs and avoids Python-only safety fixes that other language SDKs would have to reimplement.

## Relay Local IPC Preference And HTTP Timeout

- HTTP CRM calls currently use `HttpClientPool::default_timeout = 30.0`, passed into `reqwest::Client::timeout(...)`; a resource method taking longer than 30 seconds through relay fails as an HTTP transport send error and Python wraps it as `ClientCallResource`.
- Relay route authority stores routes keyed by `(name, relay_id)`, so one relay has at most one local route for a given resource name.
- `RouteEntry::to_route_info()` exposes `ipc_address` only for `Locality::Local`; `Peer` routes strip it even if a peer entry somehow carries one.
- Peer sync additionally scrubs `ipc_address` and `server_id` before sharing snapshots.
- Current `RuntimeSession.connect_via_relay()` returns `RelayAwareHttpClient`, so name-only relay connect always builds an HTTP proxy even when `_resolve` exposes a local IPC target.
- Safer design: Rust/native resolves name to a concrete target (`ipc://...` for local relay-owned route, `http(s)://...` for relay route), creates the matching native client at connect time, and only falls back from local IPC to HTTP before any CRM method call is sent.

## Phase 6 Strict Review Findings

- Native `RelayConnectedClient` initially wrapped a bare `HttpClient` for HTTP mode after relay target selection. That would have bypassed `RelayAwareHttpClient::call_async()` stale-route re-resolution for name-based relay connections that resolve to HTTP routes.
- Remediation: HTTP-mode `RelayConnectedClient` now stores an internal `RelayAwareHttpClient` and uses a shared `call_relay_aware_http_client()` PyO3 helper. The old Python-visible `RustRelayAwareHttpClient` class remains removed.
- Performance review found the first remediation would have probed HTTP once during target selection and again while constructing the HTTP client. RuntimeSession now exposes `resolve_relay_connection()`, returning either an IPC target or an already primed relay-aware HTTP client, so the initial HTTP path does not perform a duplicate connect-time resolve/probe.
- `connect_relay_http_client()` remains only for the IPC-unavailable fallback path; fallback still happens before any CRM method call is sent, and IPC configuration errors still propagate instead of silently falling back to HTTP.
- Residue search after cleanup found no code references to `resolve_relay_target(`, `resolve_relay_http_target(`, `RustRelayAwareHttpClient`, a bare `Arc<HttpClient>` in `RelayConnectedInner::Http`, or `call_http_client(py, client, route_name...)` in the native relay-connected path. Remaining search hits are explicit HTTP pool APIs, AGENTS guardrails, or documentation checklists.

## Phase 7 Relay Anchor Locality Review Findings

- The original local-IPC preference trusted any `ipc_address` returned by relay resolution. That was only safe for the SDK process's same-locality relay anchor, not for arbitrary remote relay responses.
- The repaired client path keeps relay selection in Rust/native and gates direct IPC on a loopback/local anchor URL. If the anchor is nonlocal, route resolution ignores `ipc_address` and uses HTTP route selection.
- Relay-side `/_resolve` now strips `ipc_address` for non-loopback HTTP callers. This is defense-in-depth; peer routes already omit IPC addresses, and absence of Axum connect-info is treated conservatively as nonlocal instead of failing the endpoint.
- The data-plane performance constraint is preserved: remote HTTP calls still use the resolved `relay_url` directly through `RelayAwareHttpClient`; the anchor relay is not inserted as a forwarding hop after route selection.
- Clean-cut review found no active `C2_RELAY_ADDRESS`, `cc.set_relay(...)`, `resolve_relay_address`, `settings.relay_address`, `_relay_address`, or `relay_address_override` code paths. Remaining `C2_RELAY_ADDRESS` hits are review/plan retired-name notes and the regression test proving the old env var is ignored.
