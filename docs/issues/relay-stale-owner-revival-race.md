# Relay Stale Owner Revival Race

**Date**: 2026-05-08
**Severity**: Medium (rare distributed liveness race during stale-owner replacement)
**Scope**: `c2-http` relay registration authority, upstream command loop, relay connection state

## Summary

This document was extracted from the interrupted review session
`019dec60-8de6-7492-9152-9ad8b61ebe49`. That session found a real relay command-loop
registration ordering bug: `RegisterUpstream` could dial a new candidate IPC
endpoint before rejecting a duplicate live owner. That ordering bug has been
handled by splitting command-loop registration into candidate eligibility,
candidate IPC attestation, and final commit.

Relay route replacement now fences relay-local time-of-check/time-of-use races
with a route-identity replacement token, an owner generation, an owner lease
epoch, and final-probe replacement evidence before committing a candidate
upstream registration. This prevents an old relay slot or route table entry from
being silently overwritten after another relay-local state transition.

The stale-owner revival gap described below is resolved. A candidate can replace
an existing owner only when the final owner probe proves `ConfirmedDead` or
`ConfirmedRouteMissing`, and the captured route identity plus connection-slot
freshness still match at final commit.

## Settled Context From The Bad Session

- Command-loop `RegisterUpstream` now rejects duplicate live owners before
  dialing the candidate IPC address.
- Non-skip IPC validation now requires a real `server_instance_id` from the
  candidate handshake; only skip-validation tests synthesize an instance.
- HTTP `/_register` still follows its original safer ordering: prepare with
  known identity, attest candidate IPC, then commit.
- Final replacement commit remains authoritative and rechecks current relay
  route state plus the captured slot pointer, address, owner generation, owner
  lease epoch, and replacement evidence.
- Regression coverage exists for duplicate live-owner rejection before
  candidate connect, stale-owner replacement after candidate attestation, and
  stale candidate preparation losing a final commit race.
- Additional coverage now proves same-owner lease renewal, successful upstream
  acquire, active-request fencing, disabled time deadlines, and peer-visible
  route-state stability all preserve the replacement fence.

## Current Behavior

The relay handles stale owner replacement in four layers:

1. Preflight checks validate the route name, server identity, and current relay
   route state.
2. If the route belongs to a different owner and the cached owner slot is evicted
   or disconnected, the relay probes the existing owner before allowing a
   candidate replacement to proceed.
3. Candidate registration still has to pass IPC handshake identity validation and
   route export validation.
4. Final commit uses `replace_if_owner_token(...)` to ensure the relay-local
   route and cached connection slot still match the stale owner that was probed,
   no request is active, and the old slot is already replaceable.

This is sufficient for relay-internal races where another register, unregister,
successful upstream acquire, same-owner registration, or connection-state update
changes the authoritative relay state before commit.

## Resolved Race

The original race was:

1. Route `grid` is registered to upstream owner A.
2. Relay marks owner A's cached connection as disconnected or evicted.
3. Candidate owner B attempts to register `grid`.
4. Relay probes owner A and observes `Dead` or `RouteMissing`.
5. Owner A becomes healthy again before owner B's final registration commit.
6. Relay-local state still has the same stale `OwnerToken`, so owner B commits
   and replaces the route.

The implemented behavior closes this sequence:

- If owner A re-registers or its relay-local lease is renewed before owner B
  commits, the owner lease epoch changes and B's captured token is stale.
- If owner A is reachable during B's final probe and still exposes the route,
  the probe returns live-owner semantics and B is rejected as a duplicate owner.
- If owner A is unreachable or route-missing during the final probe, B may commit
  only while `replace_if_owner_token(...)` still observes the captured slot
  pointer, address, owner generation, lease epoch, replaceable state, and
  `active_requests == 0`.

## Residual Risk

The original impact is now bounded by these constraints:

- A transient stale-owner probe cannot by itself authorize replacement; final
  commit consumes only `ConfirmedDead` or `ConfirmedRouteMissing` evidence.
- Calls routed through the relay renew the owner lease when the acquired route
  still matches, invalidating older replacement tokens.
- This is not a route-name-only trust regression: the replacement candidate still
  must pass IPC identity and route-export attestation.
- This is not a Python SDK ownership issue; the behavior belongs in Rust relay
  authority and transport state.

## Non-Goals

- Do not add Python-side relay authority or duplicate routing state.
- Do not trust route names without IPC identity validation.
- Do not add compatibility shims for earlier experimental relay behavior.
- Do not hide the race by retrying ordinary CRM calls in the SDK.

## Resolution

The relay-local freshness contract is now explicit:

- Relay ownership is fenced by the connection slot's owner generation plus owner
  lease epoch. `ConnectionPool` is the single mutable freshness authority.
- Owner freshness is local to the relay process. It is not serialized into peer
  snapshots, route announces, anti-entropy digests, normal route resolution
  responses, or Python SDK state.
- Lease duration is derived from `RelayConfig.idle_timeout_secs`. A value of `0`
  disables time-based deadlines, but it does not disable final-probe-based
  replacement when the old owner is proven dead or route-missing.
- Lease expiry alone is never ownership forfeiture. Replacement requires final
  owner-probe evidence: `ConfirmedDead` or `ConfirmedRouteMissing`.
- A same-owner re-registration or relay-local lease renewal increments the owner
  lease epoch and invalidates previously captured replacement tokens. A reconnect
  that makes the owner authoritative again receives a new owner generation.
- Successful relay-local upstream acquisition renews the current owner lease
  without mutating peer-visible route state, so normal traffic also fences stale
  replacement candidates.
- Silent external recovery preserves the old owner when the final owner probe can
  reach the old endpoint and observe the route. In that case the relay reports
  duplicate-owner semantics and does not commit the candidate replacement.

Final replacement commit now uses a pool-owned atomic operation,
`replace_if_owner_token(...)`, while route-table authority is held. The operation
checks the captured slot pointer, address, owner generation, owner lease epoch,
slot replaceability, and `active_requests == 0` under the captured slot lock
before retiring the old slot and installing the candidate connection. Route-table
mutation uses a prevalidated, non-fallible commit path after the pool fence so a
failed validation branch cannot strand the old owner.

## Historical Design Notes

The original remediation outline is retained as historical context. The chosen
contract is relay-local generation plus lease epoch in `ConnectionPool`, with
final-probe evidence consumed by `replace_if_owner_token(...)`; no cross-process
owner epoch or Python SDK authority was added.

### Phase 1: Define Owner Freshness Semantics

The selected freshness contract for relay-owned routes is:

- Relay-issued owner generation stored only in the connection slot.
- Relay-local owner lease epoch stored only in the connection slot.
- Final-probe evidence (`ConfirmedDead` or `ConfirmedRouteMissing`) carried to
  the final commit.
- Route identity (`route_name`, `server_id`, `server_instance_id`,
  `ipc_address`) captured with the slot token before candidate attestation.

The mechanism lives in Rust relay state, not in the Python SDK.

### Phase 2: Fence Replacement With Freshness

Replacement commit fails if the current slot has a different generation or lease
epoch than the one observed during the stale-owner probe. The candidate can
replace the old owner only when the final stale-owner decision is still valid at
commit time under the selected freshness model.

### Phase 3: Add Focused Race Tests

Implemented tests cover:

1. Probe returns dead, old owner re-registers before candidate commit, candidate
   commit is rejected.
2. Probe returns dead, old owner becomes reachable without relay re-registration,
   final probe preserves the old owner when it still exposes the route.
3. Candidate still cannot replace a live owner that successfully responds to the
   stale-owner probe.
4. Replacement remains possible when the old owner is truly dead and candidate
   identity/route attestation passes.
5. Same-owner lease renewal and successful upstream acquire invalidate older
   replacement tokens.
6. Owner lease renewal does not change full snapshots or anti-entropy digests.

## Related Files

- `core/transport/c2-http/src/relay/authority.rs` - registration preflight,
  stale-owner probing, and replacement token creation
- `core/transport/c2-http/src/relay/state.rs` - route table, cached owner state,
  and replacement commit checks
- `core/transport/c2-http/src/relay/server.rs` - command-loop upstream
  registration path
- `core/transport/c2-http/src/relay/router.rs` - HTTP registration path
- `core/transport/c2-http/src/relay/conn_pool.rs` - cached upstream clients and
  owner tokens
