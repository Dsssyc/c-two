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
