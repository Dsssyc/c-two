# Relay Ownership Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close issue2 by making relay local-upstream ownership independent from cached IPC connection state, and by serializing each route's lazy reconnect through a single per-route upstream slot.

**Architecture:** `RouteTable` remains the ownership source of truth. `ConnectionPool` becomes a map of route name to `UpstreamSlot`; each slot owns one IPC address, one optional current `IpcClient`, active request accounting, idle eviction state, and a reconnect state machine. Normal data-plane concurrency must wait on one in-flight reconnect instead of racing multiple `IpcClient::connect()` calls and resolving them with generation checks.

**Tech Stack:** Rust 2024, `c2-http` relay feature, `c2-ipc`, axum router tests, `c2-server` IPC test server, Cargo test/fmt.

---

## Global Architecture Context

This hardening work sits inside a larger relay transport design problem. The
immediate issue is correctness after default idle eviction: relay route
ownership must not be overwritten just because a cached IPC connection was
evicted. The broader issue is that the relay data path currently uses the IPC
layer too narrowly.

Current relay behavior:

- `POST /_register` and the local `RelayServer::register_upstream()` control
  path create upstream clients with `IpcClient::new(address)` and `connect()`.
- The HTTP data plane calls `IpcClient::call()`.
- `IpcClient::call()` is documented and implemented as the inline-only path.
- The richer IPC client path is `IpcClient::call_full()`, which can select buddy
  SHM or chunked transfer before falling back to inline.
- Buddy SHM requires the client to be constructed with a client-side `MemPool`
  through `IpcClient::with_pool(...)`; the relay currently does not do that.

That means the relay is not currently using the IPC client's full transport
selection, fallback, and backpressure surface. Large HTTP-relayed payloads are
therefore funneled through the simplest UDS inline path even though the IPC
layer already has mechanisms for chunking and shared-memory transfer.

Important design distinction:

- A single global `IpcClient` for the whole relay is not the right model because
  `IpcClient` is bound to one upstream IPC address and handshake route table. A
  relay can host multiple local upstream IPC servers at once.
- The approved target is one owned upstream slot per local route/address.
- The slot hides lazy connect, active request guards, idle close, duplicate-owner
  probes, reconnect waiters, and explicit close of discarded clients.

Current plan boundary:

- Do not switch the relay data plane to `call_full()` in this pass. That belongs
  to a follow-up relay data-plane transport unification design because buddy SHM
  also requires relay-owned client `MemPool`s and HTTP relay integration tests.
- Do not let the richer IPC transport question weaken the ownership invariant:
  duplicate registration is decided by route ownership, not by whether a cached
  client currently exists.

## Scope And Decisions

Decisions:

- Use the approved per-route `UpstreamSlot` design.
- Same route/address steady state should have at most one relay-owned client.
- Concurrent lazy reconnect for one route/address should create at most one new
  `IpcClient`; waiters reuse the result.
- `generation` should no longer be the normal data-plane coordination mechanism.
  A slot epoch may exist only as an internal owner-replacement defense.
- Idle sweeper should ask slots to evict themselves if idle; it should not
  understand reconnect internals.
- Route ownership and cached transport state stay separate.

Out of scope:

- Reintroducing Python `NativeRelay`.
- Moving relay server lifecycle into SDKs.
- Full `call_full()` / buddy SHM relay data-plane unification.

## File Map

- `core/transport/c2-http/src/relay/conn_pool.rs`
  - Replace generation-centered `ConnectionEntry` with `UpstreamSlot`.
  - Add async `acquire()` that serializes reconnect through one in-flight state.
  - Return a guard that decrements active request count on drop.
  - Add slot-level idle eviction and current-client eviction.
  - Retire removed/replaced slots so in-flight reconnects cannot return orphan
    leases after unregister or owner replacement.
- `core/transport/c2-http/src/relay/state.rs`
  - Expose async upstream acquire/probe methods.
  - Add race-safe registration commit result types and owner replacement tokens.
  - Keep `RouteTable` as source of truth for local ownership.
- `core/transport/c2-http/src/relay/router.rs`
  - Replace `try_reconnect()` and `RelayRequestGuard` with slot leases.
  - Make `POST /_register` use preflight/probe plus final commit recheck.
  - Add router tests for duplicate ownership and concurrent reconnect.
- `core/transport/c2-http/src/relay/server.rs`
  - Update idle sweeper and sync control registration to use new state APIs.
- `core/transport/c2-ipc/src/client.rs`
  - Add shared close support so relay-owned `Arc<IpcClient>` values can be
    closed without relying on `Arc::try_unwrap`.
- `docs/superpowers/plans/2026-04-27-relay-independence-idle-timeout.md`
  - Append a short addendum pointing to this hardening plan after implementation.

## Task 1: Introduce Per-Route Upstream Slot

**Files:**
- Modify: `core/transport/c2-http/src/relay/conn_pool.rs`
- Test: `core/transport/c2-http/src/relay/conn_pool.rs`

- [x] Add a failing async unit test showing concurrent acquire after eviction invokes the connector only once.
- [x] Add failing slot-retirement tests for remove/replace while reconnect is in flight.
- [x] Implement `UpstreamSlot`, `UpstreamLease`, `AcquireError`, and `ConnectionPool::acquire_with`.
- [x] Update existing connection-pool tests away from normal-path generation expectations.
- [ ] Run `cargo test --manifest-path core/transport/c2-http/Cargo.toml --features relay conn_pool`.

## Task 2: Route Data Plane Through Slot Leases

**Files:**
- Modify: `core/transport/c2-http/src/relay/state.rs`
- Modify: `core/transport/c2-http/src/relay/router.rs`
- Test: `core/transport/c2-http/src/relay/router.rs`

- [x] Add a router regression test for concurrent HTTP calls after idle eviction.
- [x] Add `RelayState::acquire_upstream()` and use it from `call_handler`.
- [x] On IPC call error, evict only the current slot client represented by the lease.
- [x] Add route-table recheck after acquire so orphan slot leases cannot serve removed routes.
- [x] Run focused router tests.

## Task 3: Make Registration Commit Race-Safe

**Files:**
- Modify: `core/transport/c2-http/src/relay/state.rs`
- Modify: `core/transport/c2-http/src/relay/router.rs`
- Test: `core/transport/c2-http/src/relay/state.rs`
- Test: `core/transport/c2-http/src/relay/router.rs`

- [x] Add state-level tests for no-owner commit races and dead-owner replacement token validation.
- [x] Add same-address repair coverage for evicted/disconnected slots.
- [x] Add router-level concurrent different-address registration test.
- [x] Implement preflight owner status, serialized owner probing, and final commit recheck.
- [x] Ensure rejected newly connected clients are explicitly closed.
- [x] Run registration-focused tests.

## Task 4: Cleanup And Verification

**Files:**
- Modify: `core/transport/c2-http/src/relay/conn_pool.rs`
- Modify: `core/transport/c2-http/src/relay/state.rs`
- Modify: `core/transport/c2-http/src/relay/server.rs`
- Modify: `docs/superpowers/plans/2026-04-27-relay-independence-idle-timeout.md`

- [x] Remove or narrow obsolete non-slot APIs from `RelayState`.
- [x] Update server comments and sync control registration to use commit results.
- [ ] Update the parent issue2 plan addendum.
- [ ] Run:
  - `cargo test --manifest-path core/transport/c2-http/Cargo.toml --features relay`
  - `cargo test --manifest-path cli/Cargo.toml`
  - `cargo fmt --check --manifest-path core/transport/c2-http/Cargo.toml`
  - `git diff --check`
