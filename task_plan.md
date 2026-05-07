# Relay Local IPC Preference And HTTP Timeout Plan

## Goal

Fix two related relay-backed connection issues:

1. Relay HTTP CRM calls must not be stuck behind an unconfigurable 30 second total request timeout.
2. Name-only `cc.connect(..., name=...)` with a configured relay should prefer the relay's local IPC route when the owning relay exposes one, without requiring application code to know the generated `ipc://` address.

## Constraints

- Keep Python SDK as thin glue. Routing, relay resolution, timeout config, retries, and fallback belong in Rust/native layers.
- Preserve explicit `address="ipc://..."` relay independence.
- Never use peer `ipc_address` for client direct IPC. Peer routes must remain HTTP relay targets.
- Do not retry a CRM method over HTTP after a method call may have been sent over IPC. Fallback is allowed only during connect/probe/handshake.
- Avoid compatibility shims and zombie paths; C-Two is 0.x.

## Phases

| Phase | Status | Scope |
| --- | --- | --- |
| 1 | complete | Add failing tests for HTTP call timeout config and relay local IPC route target selection. |
| 2 | complete | Implement Rust config and HTTP client timeout plumbing. |
| 3 | complete | Implement Rust-owned relay target selection and Python/native client projection with IPC preference. |
| 4 | complete | Run staged review/remediation for performance, security, logic, regression, and over-design risk. |
| 5 | complete | Run final verification and completion audit. |
| 6 | complete | Perform strict post-completion review for HTTP timeout coverage, name-based relay IPC preference, safety/performance logic, and zombie-code residue. |
| 7 | complete | Clean-cut relay anchor naming and gate direct IPC selection to same-locality relay anchors. |

## Review Gates

After each implementation phase:

- Inspect changed files for unnecessary compatibility branches, duplicate authority, stale code, and over-broad abstractions.
- Check security boundary: peer IPC address cannot leak into direct IPC; local IPC fallback does not enable arbitrary HTTP-origin IPC dialing beyond what `_resolve` already exposes.
- Check logic boundary: write calls are not retried after a partially sent method call.
- Check performance boundary: no extra per-call relay resolve if a selected target/client is already connected; no extra Python-level serialization path.
- Add targeted remediation before proceeding.

## Errors Encountered

| Error | Attempt | Resolution |
| --- | --- | --- |
| Stale pytest node ids | 1 | Re-ran the current test names discovered from `test_http_relay.py`. |
| Sandboxed `uv` could not fetch `maturin` | 1 | Re-ran the affected pytest command with approved network escalation and then reused the built package for subsequent tests. |
| Phase 6 found relay HTTP mode bypassing relay-aware stale-route handling | 1 | Added a failing native boundary test, then changed HTTP-mode `RelayConnectedClient` to hold an internal `RelayAwareHttpClient`. |
| Phase 6 found duplicate connect-time HTTP resolve/probe after the first remediation | 1 | Added a failing boundary assertion, then replaced target-only RuntimeSession FFI selection with `RelayResolvedConnection`, carrying the already primed HTTP client. |

## Phase 7: Relay Anchor Address Clean Cut And IPC Locality Gate

**Status:** complete

### Goal

Resolve the P1 trust-boundary bug introduced by relay local IPC preference:
`ipc_address` returned by relay resolution is only safe when the configured
relay is the SDK process's relay anchor in the same IPC locality domain. Remote
HTTP data-plane calls must still go directly to the selected `route.relay_url`
so remote calls do not add a local-relay forwarding hop.

### Decisions

- Rename user-facing SDK discovery config from `C2_RELAY_ADDRESS` to
  `C2_RELAY_ANCHOR_ADDRESS` with a 0.x clean cut. Do not read the old variable
  as an alias.
- Rename Python API `cc.set_relay(...)` to `cc.set_relay_anchor(...)`.
- Keep `relay_url` naming for selected data-plane route targets; do not rename
  relay server `bind`, `advertise_url`, `seeds`, or peer mesh fields.
- Enable direct IPC target selection only when the relay anchor endpoint is
  provably local/loopback. Otherwise ignore returned `ipc_address` and use the
  HTTP relay target path.
- Add relay-side defense-in-depth: non-loopback `/_resolve` callers must not
  receive `ipc_address`.

### Required Review Gates

After each sub-phase:

- Security: remote relay responses cannot make SDK clients dial arbitrary local
  IPC addresses.
- Performance: remote data-plane still uses selected `route.relay_url` directly,
  not `client -> anchor -> remote relay`.
- Logic: IPC fallback to HTTP remains connect/probe-only; no method call replay.
- Design: no Python-owned route locality judgment; Rust/native owns selection.
- Cleanup: no stale `C2_RELAY_ADDRESS` / `set_relay(...)` active guidance remains.
