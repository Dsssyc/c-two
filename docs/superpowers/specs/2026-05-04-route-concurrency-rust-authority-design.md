# Route Concurrency Rust Authority Design

**Date:** 2026-05-04  
**Status:** Draft for review  
**Scope:** Make route concurrency configuration and execution state Rust-owned so the same `ConcurrencyConfig` limits apply to same-process thread-local calls, direct IPC, and relay-backed HTTP without a Python-side duplicate scheduler.

> **0.x constraint:** C-Two is pre-1.0. Do not preserve compatibility shims for stale scheduler ownership or dual-state execution paths. Prefer a clean cut that removes zombie code and keeps one authoritative concurrency model.

## Problem

Today the Python SDK still owns part of route concurrency behavior:

- `ConcurrencyConfig` is created and validated in Python.
- `Scheduler(cc_config)` in Python still owns thread-local execution guards and capacity bookkeeping.
- `NativeServerBridge.register_crm()` forwards only `cc_config.mode.value` into Rust.
- Rust `c2-server` enforces only the mode (`parallel` / `exclusive` / `read_parallel`) for remote route dispatch.

That means the same registration input can produce different effective behavior depending on transport:

- same-process thread-local calls use Python scheduler state;
- remote IPC and relay-backed HTTP use Rust route dispatch state;
- `max_pending` and `max_workers` can be accepted in Python but not enforced by Rust route execution.

This is the exact class of “accepted but not authoritative” state drift that makes later SDKs harder to implement.

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

Python `ConcurrencyConfig` remains the typed registration input. After registration, Python should keep only a native handle or frozen snapshot projected from Rust.

The Python SDK must not retain a second mutable scheduler copy with its own pending counters, worker pool, or lock state.

`Scheduler` in `sdk/python/src/c_two/transport/server/scheduler.py` becomes a thin adapter over the native handle if the public surface still needs the class name. It may expose:

- `execution_guard(access_mode)` for thread-local direct calls;
- `snapshot()` or equivalent read-only projection for tests/debugging.

It must not own separate enforcement state.

### 3) Route registration builds one shared handle

`cc.register(...)` and low-level `Server.register_crm(...)` should pass the full concurrency config into the native server bridge.

The native bridge creates the Rust concurrency object once and clones the same `Arc` into:

- the Rust `CrmRoute` stored by `c2-server`;
- the Python slot used by same-process direct calls.

This is the key boundary change: one route, one concurrency object, shared across transport modes.

### 4) Execution uses the same object in both paths

**Thread-local path**

`CRMProxy.call_direct()` still invokes Python resource methods directly, but it wraps the call in a native-backed concurrency guard before entering the method body.

That keeps same-process direct calls zero-copy while ensuring they consume the same route-level permits as remote calls.

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

### Local adapter

`Scheduler` stays only as a thin wrapper if needed by existing SDK entry points and tests. Its role is to present the native guard to Python code, not to manage concurrency itself.

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
   - Verify a bad `C2_RELAY_ADDRESS` does not affect explicit `ipc://...` direct address calls.

5. **Zero-copy guardrail**
   - Verify the shared concurrency object does not change the SHM-to-`memoryview` path for large inputs.

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

