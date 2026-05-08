# Relay Stale Owner Revival Race

**Date**: 2026-05-08
**Severity**: Medium (rare distributed liveness race during stale-owner replacement)
**Scope**: `c2-http` relay registration authority, upstream command loop, relay connection state

## Summary

Relay route replacement now fences relay-local time-of-check/time-of-use races with
`OwnerToken` before committing a candidate upstream registration. This prevents an
old relay slot or route table entry from being silently overwritten after another
relay-local state transition.

A remaining robustness gap exists outside relay-local authority: an existing
upstream owner can be probed as dead or route-missing, then become healthy again
before the candidate replacement commits, without any fresh relay-visible owner
generation change. In that case the relay can still complete the candidate
replacement because the relay-local slot remains replaceable.

## Current Behavior

The relay handles stale owner replacement in two layers:

1. Preflight checks validate the route name, server identity, and current relay
   route state.
2. If the route belongs to a different owner and the cached owner slot is evicted
   or disconnected, the relay probes the existing owner before allowing a
   candidate replacement to proceed.
3. Candidate registration still has to pass IPC handshake identity validation and
   route export validation.
4. Final commit uses an `OwnerToken` to ensure the relay-local route and cached
   connection slot still match the stale owner that was probed.

This is sufficient for relay-internal races where another register, unregister,
or connection-state update changes the authoritative relay state before commit.

## Residual Gap

The relay does not currently have a cross-process owner epoch, lease, or fencing
token that can prove the old upstream did not recover after the stale-owner
probe. A possible sequence is:

1. Route `grid` is registered to upstream owner A.
2. Relay marks owner A's cached connection as disconnected or evicted.
3. Candidate owner B attempts to register `grid`.
4. Relay probes owner A and observes `Dead` or `RouteMissing`.
5. Owner A becomes healthy again before owner B's final registration commit.
6. Relay-local state still has the same stale `OwnerToken`, so owner B can commit
   and replace the route.

If owner A re-registers through the relay before owner B commits, the relay-local
token should change and the stale replacement should be rejected. The gap is the
case where owner A recovers externally without creating a fresh relay-visible
generation.

## Impact

The impact is bounded but worth tracking:

- A transient stale-owner probe can allow a replacement even though the old owner
  becomes healthy immediately afterward.
- Calls routed through the relay may shift to the replacement owner until the old
  owner explicitly re-registers or the route is otherwise reconciled.
- This is not a route-name-only trust regression: the replacement candidate still
  must pass IPC identity and route-export attestation.
- This is not a Python SDK ownership issue; the behavior belongs in Rust relay
  authority and transport state.

## Non-Goals

- Do not add Python-side relay authority or duplicate routing state.
- Do not trust route names without IPC identity validation.
- Do not add compatibility shims for earlier experimental relay behavior.
- Do not hide the race by retrying ordinary CRM calls in the SDK.

## Recommended Future Work

### Phase 1: Define Owner Freshness Semantics

Choose the canonical freshness contract for relay-owned routes:

- Relay-issued owner generation stored in the route entry and connection slot.
- IPC handshake generation returned by the upstream server.
- Lease or heartbeat token that must be fresh at replacement commit.
- Explicit tombstone/fencing token issued when a stale owner is declared
  replaceable.

The chosen mechanism should live in Rust core relay/IPC state, not in the Python
SDK.

### Phase 2: Fence Replacement With Freshness

Make replacement commit fail if the old owner can prove a fresher generation or
lease than the one observed during the stale-owner probe. The candidate should
only replace the old owner when the stale-owner decision is still valid at
commit time under the selected freshness model.

### Phase 3: Add Focused Race Tests

Add tests that cover:

1. Probe returns dead, old owner re-registers before candidate commit, candidate
   commit is rejected.
2. Probe returns dead, old owner becomes reachable without relay re-registration,
   expected behavior is determined by the chosen lease/generation model.
3. Candidate still cannot replace a live owner that successfully responds to the
   stale-owner probe.
4. Replacement remains possible when the old owner is truly dead and candidate
   identity/route attestation passes.

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
