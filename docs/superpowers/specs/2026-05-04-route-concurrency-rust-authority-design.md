# Route Concurrency Rust Authority Design

**Date:** 2026-05-04
**Status:** Draft for review
**Scope:** Make route concurrency configuration and execution state Rust-owned so the same `ConcurrencyConfig` limits apply to same-process thread-local calls, direct IPC, and relay-backed HTTP without a Python-side duplicate scheduler.

> **0.x constraint:** C-Two is pre-1.0. Do not preserve compatibility shims for stale scheduler ownership or dual-state execution paths. Prefer a clean cut that removes zombie code and keeps one authoritative concurrency model.

## Problem

The Python SDK now owns only the typed registration facade and the same-process adapter surface. Route concurrency authority lives in Rust:

- `ConcurrencyConfig` is created and validated in Python.
- `NativeServerBridge.register_crm()` passes `mode`, `max_pending`, and `max_workers` into Rust.
- Rust `c2-server` constructs the shared route handle and enforces mode plus capacity for both remote dispatch and same-process direct calls.
- Python `Scheduler` is a thin adapter over the native handle, not an owning scheduler.

That means the same registration input now behaves consistently across transport modes:

- same-process thread-local calls and remote IPC share the same native route handle;
- `max_pending` and `max_workers` are enforced by Rust for both paths;
- relay remains forwarding / discovery only.

This is the state we want future SDKs to inherit.

## Goals

1. Make Rust the single authority for route concurrency state.
2. Keep Python as an input/adapter layer, not a second scheduler.
3. Apply the same concurrency limits to:
   - same-process thread-local `call_direct()`
   - direct IPC (`ipc://`)
   - relay-backed HTTP
4. Preserve the current zero-copy request path. Concurrency control must gate execution, not materialize payload bytes.
5. Keep relay out of scheduling. Relay remains discovery and forwarding only.

## Non-Goals

- Do not route thread-local calls through remote IPC bytes dispatch.
- Do not make relay the owner of execution limits.
- Do not preserve a Python-owned `ThreadPoolExecutor` as the concurrency authority.
- Do not add compatibility aliases for legacy scheduler internals that would keep duplicate state alive.
- Do not change transferable serialization or SHM layout as part of this work.

## Proposed Architecture

### 1) Rust owns a shared route concurrency object

Introduce a Rust-owned route concurrency object in `c2-server` that contains the full resolved concurrency policy for one CRM route:

- `mode`
- `max_pending`
- `max_workers`

The object is the single runtime authority for all route calls. It is responsible for:

- read/write mode gating;
- admission limiting (`max_pending`);
- execution limiting (`max_workers`);
- producing a read-only snapshot for SDK projection.

The object must not inspect or copy request payload bytes.

### 2) Python stores only a handle, not scheduler state

Python `ConcurrencyConfig` remains the typed registration input. After registration, Python keeps only a native handle or a frozen snapshot projected from Rust.

The Python SDK must not retain a second mutable scheduler copy with its own pending counters, worker pool, lock state, shutdown flag, or inferred default worker count. The current Python `Scheduler` class is therefore retired as an owning scheduler, not preserved as a fallback authority.

`Scheduler` in `sdk/python/src/c_two/transport/server/scheduler.py` may remain only as a thin adapter over a native handle while the public class name is still useful for SDK internals and tests. It may expose:

- `execution_guard(method_idx)` for thread-local direct calls;
- `snapshot()` or equivalent read-only projection for tests/debugging;
- idempotent `close()` / `shutdown()` methods that forward to the native handle without owning drain state.

It must not implement its own locking, pending counters, `ThreadPoolExecutor`, or capacity checks.

### 3) Route registration builds one shared handle

`cc.register(...)` and low-level `Server.register_crm(...)` pass the full concurrency config into the native server bridge.

The native bridge creates the Rust concurrency object once and clones the same `Arc` into:

- the Rust `CrmRoute` stored by `c2-server`;
- the Python slot used by same-process direct calls.

This is the key boundary change: one route, one concurrency object, shared across transport modes.

The PyO3 `register_route()` boundary passes the complete concurrency policy:

- `mode`
- `max_pending`
- `max_workers`

`SchedulerLimits::try_from_usize()` validates the limits before the route is
registered. Python does not retain a second authoritative copy of these values.

The Python bridge no longer resolves a hidden `max_workers=4` default before Rust sees the config. The default policy is either:

- explicit `ConcurrencyConfig(...)` provided by the caller; or
- Rust default resolution when no config is provided.

If Python exposes a default snapshot, it is projected back from the native route handle after registration.

### 4) Execution uses the same object in both paths

**Thread-local path**

`CRMProxy.call_direct()` still invokes Python resource methods directly, but it wraps the call in a native-backed concurrency guard before entering the method body.

That keeps same-process direct calls zero-copy while ensuring they consume the same route-level permits as remote calls.


For the unconstrained hot path, `CRMProxy.call_direct()` bypasses the native guard only when the native handle reports that the route is truly unconstrained: `mode=parallel`, `max_pending=None`, and `max_workers=None`. `read_parallel` is not unconstrained, because it still enforces read/write semantics. This preserves the no-op fast path for plain local calls without letting capacity or read/write limits drift from Rust.

**Remote IPC / relay-backed HTTP**

Rust route dispatch acquires and releases the same concurrency guard around callback execution. The relay does not participate in this logic; it only resolves the route and forwards requests to the upstream server.

### 5) Concurrency semantics

The shared object should enforce these semantics consistently:

- `mode=parallel`: no read/write serialization, but still subject to `max_pending` / `max_workers` if configured.
- `mode=exclusive`: all route calls serialize.
- `mode=read_parallel`: reads may overlap, writes remain exclusive.

For the capacity fields:

- `max_pending` is the admission cap for in-flight route calls.
- `max_workers` is the execution cap for concurrent route callbacks.

Both caps are route-level and shared across local and remote execution paths.

If both are set, both are enforced. Neither field should be reinterpreted as a Python thread-pool size.

Capacity exhaustion is fail-fast. Read/write mode serialization may wait, but a
configured `max_pending` or `max_workers` limit must not create an unbounded
queue. If a remote request has already materialized SHM/chunk-backed
`RequestData` before acquisition fails, the server must release that request
resource before sending the route-capacity or route-closed error reply.

The native guard must not expose Tokio async lock guards through Python. The
shared object should use runtime-independent owned guard state so Python
`with scheduler.execution_guard(method_idx): ...` can acquire/release a guard
without holding borrowed async lock lifetimes across the FFI boundary.


### 6) Lifecycle and teardown ownership

The shared native route concurrency object needs an explicit lifecycle so Python and Rust do not race during unregister or shutdown.

Required lifecycle semantics:

1. `register_route` creates the native route handle in the `open` state and returns/clones that same handle into the Python `CRMSlot`.
2. `unregister_crm(name)` asks Rust to mark the shared handle `closing` and
   remove the route from the server route table under the same dispatcher write
   lock. This prevents new remote IPC / relay-forwarded calls from finding the
   route and prevents same-process thread-local calls from acquiring the still
   shared handle.
3. New thread-local guard acquisitions through the Python slot must fail
   immediately with a route-closed error instead of entering the resource
   method.
4. Existing in-flight guards, both local and remote, are allowed to finish. Guard drop releases the pending/execution permits.
5. `shutdown()` is idempotent and may mark all remaining handles `closing`; it must not double-drain or deadlock if Python slot shutdown and Rust server shutdown happen in either order.
6. Python may drop its `CRMSlot` only after invoking the Python shutdown callback and closing the native handle. Rust may drop the route-table `Arc` after unregister. The underlying object is freed only when both clones are gone and in-flight guards have dropped.

The design does not require waiting for all in-flight calls before removing the route from the lookup table; it does require that no new calls can enter after closing starts, and that in-flight calls release permits reliably.

## Public API Shape

### Python registration input

`ConcurrencyConfig` remains the public registration input object:

```python
ConcurrencyConfig(
    mode=ConcurrencyMode.READ_PARALLEL,
    max_pending=64,
    max_workers=8,
)
```

Python should continue to validate obvious local input errors early, but it must not become the final authority for the runtime policy.

### Read-only projection

After registration, Python may project the resolved values back from Rust for display or test assertions:

```python
snapshot = slot.concurrency.snapshot()
assert snapshot.mode == ConcurrencyMode.READ_PARALLEL
```

This projection is informational only. It must not be a second source of truth.

### Local adapter and legacy API removal

`Scheduler` stays only as a thin wrapper if needed by existing SDK entry points and tests. Its role is to present the native guard to Python code, not to manage concurrency itself.

Because C-Two is 0.x, the implementation should remove or rewrite legacy tests and APIs that require Python-owned state. The following Python `Scheduler` surfaces must not remain as independent behavior:

- `begin()` with a Python `_pending_count`;
- `execute()` / `execute_fast()` using Python locks;
- `pending_count` as a Python-owned mutable counter;
- lazy `executor` / `ThreadPoolExecutor` creation;
- `shutdown()` that drains Python-owned pending state.

If a surface is still needed by tests or debug tooling, it must forward to the native route handle or expose a read-only native snapshot. Do not keep old internals only to satisfy historical tests.

## Error Handling

- Invalid `ConcurrencyConfig` values should fail at registration time.
- Remote registration must never silently drop `max_pending` or `max_workers`.
- If a field cannot be enforced by the native object, registration should fail rather than accept and ignore it.
- `call_direct()` and remote route dispatch should surface capacity failures as route errors, not as silent drops.

## Testing Strategy

Cover the shared authority from both sides:

1. **Thread-local + remote mixed limit test**
   - Register one route with a finite concurrency config.
   - Exercise local `call_direct()` and remote IPC calls concurrently.
   - Assert the combined active count never exceeds the configured limit.

2. **Mode coverage**
   - Verify `exclusive`, `parallel`, and `read_parallel` still behave correctly under remote IPC.

3. **Capacity coverage**
   - Verify `max_pending` and `max_workers` are enforced for both same-process and remote calls.

4. **Relay independence**
   - Verify a bad `C2_RELAY_ANCHOR_ADDRESS` does not affect explicit `ipc://...` direct address calls.

5. **Zero-copy guardrail**
   - Verify the shared concurrency object does not change the SHM-to-`memoryview` path for large inputs.

6. **Remote cleanup on rejected acquisition**
   - Verify scheduler capacity/closed failures after request materialization
     release SHM/chunk resources and do not enter the CRM callback.

## Implementation Boundary

The change should be visible in these areas:

- Rust `c2-server` scheduler / route state
- Rust PyO3 bridge in `sdk/python/native`
- Python server bridge and slot storage
- Python scheduler adapter and direct-call guard
- Integration tests for mixed local/remote concurrency

The desired end state is simple:

- Rust owns the policy and execution guard.
- Python only passes configuration in and projects immutable state out.
- Relay remains discovery and forwarding only.
- Same limits apply regardless of transport.
