# Server Execution Backpressure Design

> **Status:** Draft, pending review approval
> **Date:** 2026-05-12
> **Scope:** `c2-config`, `c2-server`, `c2-runtime`, Python PyO3 bindings, Python SDK config/tests
> **Goal:** Make server-side CRM callback execution concurrency and backpressure explicit, Rust-owned, and usable by future SDKs.

## Overview

The current IPC server accepts call frames, spawns an async dispatch task, then
submits CRM callback execution to Tokio's `spawn_blocking`. Route-level
concurrency is checked inside the blocking task. That leaves an implicit Tokio
blocking queue between request admission and C-Two's own route limits.

This design moves CRM execution admission into `c2-server` before
`spawn_blocking`. The Rust core becomes the source of truth for global server
execution budget, route execution budget, pending request budget, and
read/write fairness. SDKs only provide callback adapters.

## Current Code Evidence

The relevant current production paths are:

- IPC call frames are dispatched with unbounded `tokio::spawn` call tasks in
  `core/transport/c2-server/src/server.rs`.
- CRM callbacks are run through `tokio::task::spawn_blocking`.
- Route `max_workers` and `max_pending` are checked by
  `core/transport/c2-server/src/scheduler.rs` after a blocking thread has
  already begun running.
- `ServerIpcConfig.max_pending_requests` exists in `c2-config`, but the server
  dispatch path does not use it as a CRM execution admission gate.
- Python forwards `ConcurrencyConfig.max_workers` and `max_pending` through
  PyO3 into the Rust route scheduler.

The design must preserve direct IPC independence, same-process thread-local
calls, zero-copy request cleanup, and Rust ownership of language-neutral
runtime behavior.

## Invariants

### I1. Global Execution Budget

At most `max_execution_workers` remote CRM callbacks may be actively executing
inside one IPC server process at a time.

This prevents Tokio's default blocking pool limit from becoming an accidental
runtime policy, avoids hundreds of surprise OS threads, and gives operators a
single server-level execution budget.

### I2. Global Pending Budget

At most `max_pending_requests` remote CRM calls may be admitted into the server
execution scheduler at a time, counting active and queued requests.

This prevents unbounded memory growth from decoded request buffers, SHM handles,
chunk reassembly results, async task fan-out, and hidden Tokio blocking queue
depth.

### I3. Route Execution Budget

For one registered route, at most `ConcurrencyConfig.max_workers` callbacks may
be actively executing at a time. If capacity is unavailable, requests wait in
the route queue until route pending capacity is exhausted or the route/server is
closed.

This turns route `max_workers` into a deterministic worker budget instead of a
post-`spawn_blocking` reject path.

### I4. Route Pending Budget

For one registered route, at most `ConcurrencyConfig.max_pending` calls may be
admitted into that route's pending set, counting active and queued calls. If the
limit is reached, new calls are rejected before callback execution and before
large request ownership can leak.

This gives each resource an isolation boundary and prevents one hot route from
consuming all server pending slots.

### I5. Read/Write Access Semantics

`read_parallel` must preserve the existing semantics: read methods may run
concurrently, write methods are exclusive, and waiting writers are not starved
by newly arriving reads.

This protects mutable resource state while preserving high throughput for
explicitly read-only CRM methods.

### I6. Local And Remote Route Limits Share State

Same-process thread-local calls and remote IPC calls to the same registered
route must share the same route concurrency state. Local calls are not counted
against the server's remote execution thread budget, but they do count against
route `max_workers`, route pending, and read/write access.

This preserves the behavior that a resource object has one concurrency policy
regardless of whether the caller is local or remote.

### I7. Cross-Language Core Ownership

Execution admission must be expressed in Rust core terms: route, method index,
access mode, callback permit, request ownership, and response completion. It
must not depend on Python-specific concepts such as the GIL, PyO3, or Python
thread pools.

This allows future C++, Go, Fortran, Java, and other SDKs to reuse the same
server semantics by adapting their language resource methods into
`CrmCallback`.

### I8. Request Cleanup On Rejection Or Close

Every rejected, cancelled, or closed remote request must release owned request
resources exactly once, including SHM-backed `RequestData::Shm`,
`RequestData::Handle`, chunk reassembly buffers, and inline buffers.

This prevents memory leaks and stale buffer leases under overload or route
unregister/shutdown races.

### I9. No Legacy Bypass

No production or test helper may continue to invoke remote CRM callbacks through
the old direct `spawn_blocking` + `blocking_acquire` path. Direct route
construction in tests must use the same execution handles and scheduler
authority as production route registration.

This prevents half-migrations where tests pass through old helpers while real
IPC uses the new scheduler, or the reverse.

### I10. Connection Drain Covers Queued Work

Every remote call that has been admitted or queued must be counted as
connection in-flight work until it sends a response or is rejected and cleaned
up. Connection shutdown must wait for queued and active calls before cleaning up
per-connection chunk and SHM state.

This prevents a connection close from running `wait_idle()` and
`chunk_registry.cleanup_connection()` while a queued logical call still owns
request data or still intends to send a response.

## Source-To-Sink Matrix

| Invariant | Producer | Storage | Consumer | Network/Wire | FFI/SDK | Test/Support | Legacy/Bypass |
| --- | --- | --- | --- | --- | --- | --- | --- |
| I1 global execution | `ServerIpcConfig.max_execution_workers` from Rust resolver/env/overrides | `ServerExecutionScheduler` global state | remote CRM dispatch before callback | IPC calls only; HTTP relay forwards to IPC server unchanged | Python passes config only; future SDKs use same server config | Rust server stress tests assert active callbacks never exceed limit | ban direct `spawn_blocking` callback paths outside scheduler |
| I2 global pending | `ServerIpcConfig.max_pending_requests` | `ServerExecutionScheduler` pending counter | call preparation before dispatch task spawn for inline/buddy; chunk completion before callback execution | IPC inline, SHM, handle, chunked call paths; incomplete chunks remain governed by chunk registry limits | no Python-owned counter | overload tests assert early `ResourceUnavailable` and cleanup | no hidden Tokio blocking queue as first backpressure layer; no unbounded call-task fan-out for complete inline/buddy calls |
| I3 route workers | `ConcurrencyConfig.max_workers` / route spec | route state inside execution scheduler | local and remote route permits | route metadata does not cross wire; route is resolved from handshake table | PyO3 forwards values; route handle exposed to local proxy | current max-worker tests updated from reject-fast to queue semantics | remove scheduler-only post-blocking rejection for remote |
| I4 route pending | `ConcurrencyConfig.max_pending` / route spec | route state inside execution scheduler | local and remote route admission | not wire-visible | PyO3 forwards values; no SDK-side fallback | route overload tests for local, remote, mixed calls | test fakes must not omit pending accounting |
| I5 read/write | CRM method access map | route state | permit acquisition and promotion | method index crosses wire | SDK builds access map from CRM metadata | read/write fairness tests | no raw callback invocation without access map |
| I6 local/remote shared | route registration builds one route handle | route handle cloned into `CrmRoute` and Python local slot | local proxy guard and remote scheduler | remote IPC path resolves route from dispatcher | `RouteConcurrency` wraps same core handle | mixed local/remote tests | no separate Python lock/counter state |
| I7 cross-language | `CrmCallback` trait and `CrmRoute` | `c2-server` | all SDK callback adapters | language-neutral IPC request/response data | Python is one adapter; other SDKs can adapt callback | Rust-only callback tests plus Python integration | no Python-specific field in core config |
| I8 cleanup | request materialization in dispatch path | request object until execution result/reject | scheduler rejection, route close, callback completion | IPC SHM/handle/chunked data | PyO3 only sees request after permit | SHM reuse tests after rejection | cleanup must not depend on callback running |
| I9 no bypass | route registration helpers | direct route construction tests and relay support | production and tests | relay test support starts live IPC servers | all FFI constructors updated | `rg` guardrail for old helper usage | old `Scheduler::execute` removed or test-only gated |
| I10 connection drain | `Connection::FlightGuard` / in-flight counter | per-connection atomic counter | connection close waits on queued and active calls | IPC connection lifecycle; chunk cleanup after idle | no SDK-owned drain state | tests close connection with queued calls and assert no premature cleanup | manual `flight_inc` / `flight_dec` paths replaced with RAII guard |

## Proposed Architecture

### New Core Module

Add `core/transport/c2-server/src/execution.rs` with:

- `ServerExecutionScheduler`
- `RouteExecutionHandle`
- `ExecutionScope::{Remote, Local}`
- `ExecutionPermit`
- `ExecutionSnapshot`
- `ExecutionAcquireError`

`Server` owns one `Arc<ServerExecutionScheduler>`. Each `CrmRoute` stores a
`RouteExecutionHandle` instead of a standalone `Arc<Scheduler>`.

The scheduler holds a single mutex-protected state tree:

```text
Global state:
  closed
  pending_remote
  active_remote_workers
  max_pending_requests
  max_execution_workers

Per-route state:
  closed
  mode
  access_map
  max_pending
  max_workers
  pending_local
  pending_remote
  active_local
  active_remote
  active_readers
  writer_active
  waiting_writers
```

Remote calls reserve global pending and route pending at admission time. They
atomically promote to global active worker and route active state when they
become runnable. Local calls use the same route state but do not reserve global
remote worker capacity.

### Remote Execution Flow

All remote call variants use the same choke point:

```text
handle_connection / dispatch_chunked_call
  decode logical route + method index as early as possible
  resolve route and validate method index
  acquire RemotePendingPermit + FlightGuard
  materialize request ownership only after admission where possible
  result = server.execution_scheduler.execute_remote(permit, request, callback)
  send_route_execution_result(...)
```

`RemotePendingPermit` is acquired before `spawn_blocking`. For inline and buddy
calls, the connection loop decodes the call-control envelope and performs route
admission before spawning a per-call async task. Buddy SHM data is read only
after pending admission succeeds, so rejected calls do not materialize large
request buffers.

For chunked calls, incomplete chunks remain governed by the existing chunk
registry limits (`max_total_chunks`, assembler timeout, reassembly memory). The
complete logical call enters the execution scheduler only after
`chunk_registry.finish()` returns route name, method index, and request handle.

If global or route pending capacity is exceeded, the server returns
`ExecutionAcquireError::Capacity` and releases any owned `RequestData`
immediately. If the request is admitted but must wait for access mode or worker
capacity, it waits in the Rust scheduler, not in Tokio's blocking queue.

Only after both global and route execution capacity are available does the
server submit the callback to `spawn_blocking`.

`RemotePendingPermit` owns or is paired with a `FlightGuard`. Dropping the
permit before callback execution decrements global/route pending and releases
connection in-flight state. Promoting it to `ExecutionPermit` transfers that
lifecycle until response send or error cleanup finishes.

### Local Same-Process Flow

The Python `RouteConcurrency` wrapper changes from wrapping the old
`Scheduler` to wrapping `RouteExecutionHandle`. Its `execution_guard(method_idx)`
uses `ExecutionScope::Local`.

Local calls therefore share route `max_workers`, route pending, and read/write
state with remote calls. They do not count against `max_execution_workers`
because they run on the caller's thread rather than the IPC server's execution
pool.

### Config

Add a server IPC config field:

```rust
pub max_execution_workers: u32
```

The default is:

```rust
clamp(std::thread::available_parallelism(), min = 4, max = 64)
```

`max_execution_workers` is configurable through:

- `C2_IPC_MAX_EXECUTION_WORKERS`
- `cc.set_server(ipc_overrides={"max_execution_workers": ...})`
- PyO3 `RustServer` constructor

`RustServer.start_runtime_and_wait()` also sets Tokio
`.max_blocking_threads(max_execution_workers)` as a guardrail. The primary
policy remains C-Two's `ServerExecutionScheduler`; Tokio's limit is only a
backstop.

The default implementation must handle `available_parallelism()` failure by
falling back to the minimum value:

```rust
available_parallelism()
    .map(NonZeroUsize::get)
    .unwrap_or(4)
    .clamp(4, 64)
```

Do not derive `max_execution_workers` from the sum of registered route
`max_workers`. Routes are dynamic and route limits express isolation, not OS
thread budget.

### Error Semantics

Remote overload errors map to the existing server-side
`ResourceUnavailable` wire error family with precise messages:

- `server execution capacity exceeded: max_pending_requests=N`
- `server execution capacity exceeded: max_execution_workers=N`
- `route concurrency capacity exceeded: max_pending=N`
- `route closed`
- `server closing`

`max_workers` no longer has a normal reject-fast error for remote calls. It is
a worker budget that queues until route pending or global pending capacity is
exceeded. `try_acquire` may remain for diagnostics or explicit nonblocking
local APIs, but production remote dispatch must not use it.

### Metrics And Snapshots

Expose Rust snapshots first, then Python read-only projection:

```text
ServerExecutionSnapshot:
  max_pending_requests
  max_execution_workers
  pending_remote
  active_remote_workers
  rejected_global_pending
  rejected_route_pending
  rejected_closed

RouteExecutionSnapshot:
  mode
  max_pending
  max_workers
  pending_local
  pending_remote
  active_local
  active_remote
  active_workers_total
  closed
```

These snapshots are required for tests and operational debugging. They do not
need to become a stable public API until the implementation proves useful.

### Runtime Session And Registration Validation

`RuntimeRouteSpec` remains the language-neutral registration contract. It must
continue to validate that the route created by an SDK matches the spec supplied
to `RuntimeSession::register_route`.

After this migration, that validation reads from `RouteExecutionHandle` /
`RouteExecutionSnapshot`, not the old `route.scheduler.snapshot()`. The
validated fields remain:

- concurrency mode
- method access map
- route `max_pending`
- route `max_workers`
- CRM namespace/name/version
- ABI hash and signature hash
- method list

This is a guardrail against SDK-side partial registration where the dispatcher
route and runtime registration spec disagree.

## Implementation Work Items

1. Add `max_execution_workers` to `c2-config` server defaults, env resolver,
   override schema, validation, and dict projections.
2. Add Python `ServerIPCOverrides.max_execution_workers` typed facade and pass
   it into `RustServer`.
3. Add `ServerExecutionScheduler` and migrate route concurrency state into it.
4. Change `CrmRoute` to store a `RouteExecutionHandle`; update constructors in
   PyO3, runtime session tests, c2-server tests, and relay test support.
5. Change `RuntimeRouteSpec` validation in `c2-runtime` to read concurrency
   metadata from `RouteExecutionHandle` snapshots instead of
   `route.scheduler.snapshot()`.
6. Replace remote `execute_route_request(... spawn_blocking ...)` with
   scheduler-owned `execute_remote`.
7. Move inline and buddy logical-call admission into `handle_connection` before
   per-call `tokio::spawn`; keep chunked incomplete-call limits in
   `ChunkRegistry`, then admit the complete logical call after
   `chunk_registry.finish()`.
8. Replace manual connection `flight_inc` / `flight_dec` call paths with an
   RAII guard that covers queued and active calls.
9. Change Python `RouteConcurrency` FFI to wrap the route execution handle and
   enter local guards through `ExecutionScope::Local`.
10. Remove or test-gate old `Scheduler::execute` and any production
   `blocking_acquire` remote path.
11. Add snapshot/metrics bindings needed by tests.
12. Update existing tests that assert `max_workers` reject-fast semantics; keep
   reject-fast tests only for `max_pending` and explicit nonblocking APIs.
13. Update Python unit-test fakes (`FakeScheduler`) to model the new route
    handle API enough to keep local direct-call tests meaningful.
14. Update docs and examples so `max_workers` is described as a queueing worker
    budget, not a thread count.

## Required Tests

Rust core tests:

- One route with `max_workers=2`, ten concurrent remote-style executions:
  at most two callbacks active; all ten complete unless pending capacity is
  lower.
- One route with `max_pending=3`: the fourth admitted call is rejected before
  callback execution and releases request resources.
- Ten routes with `max_workers=10`, server `max_execution_workers=4`: global
  active remote callbacks never exceed four.
- Server `max_pending_requests=8`: ninth remote call is rejected before
  `spawn_blocking`.
- `read_parallel`: readers run concurrently, writes are exclusive, and waiting
  writers block newly arriving readers.
- Route unregister closes queued calls and rejects new calls.
- Connection close waits for queued and active admitted calls before
  per-connection cleanup.
- SHM request rejection frees the source allocation and allows reuse.

Python integration tests:

- `cc.set_server(ipc_overrides={"max_execution_workers": 2})` limits remote
  active callbacks across multiple resources.
- Mixed local and remote calls share route `max_workers`.
- Local direct calls do not consume server `max_execution_workers`.
- Direct IPC remains relay-independent when `C2_RELAY_ANCHOR_ADDRESS` points to
  an unavailable relay.
- Existing syntax compatibility tests still pass on Python 3.10.

Guardrail tests:

- A source-level test or lint script fails if production `server.rs` invokes
  `tokio::task::spawn_blocking` for CRM callbacks outside
  `ServerExecutionScheduler`.
- Test helpers constructing `CrmRoute` must call the same route-handle builder
  used by production registration.
- A source-level test fails if production inline or buddy call frames spawn a
  per-call task before logical-call admission.

## Migration Notes

This is a 0.x clean cut. Do not preserve the old reject-fast `max_workers`
behavior as a compatibility mode. Update tests, docs, and examples in the same
change.

Remote semantics after the migration:

```text
max_execution_workers: server-wide active remote callbacks
max_pending_requests: server-wide active + queued remote calls
route max_workers: per-route active local + remote callbacks
route max_pending: per-route active + queued local + remote calls
```

Thread-local local calls remain zero-serialization direct calls. They still
enter the route concurrency guard, but they do not route through Rust byte
dispatch and do not consume remote execution worker capacity.

## Acceptance Criteria

- No production remote CRM callback can reach `spawn_blocking` before passing
  global and route execution admission.
- Inline and buddy call frames are admitted before the per-call async task is
  spawned; chunked calls are admitted immediately after final reassembly and
  before callback execution.
- Tokio `.max_blocking_threads()` is set from `max_execution_workers`.
- `max_pending_requests` is exercised by server execution tests, not just
  config parsing tests.
- Connection `wait_idle()` covers queued calls as well as active callbacks.
- Existing mixed local/remote concurrency tests are updated to the new queueing
  semantics and still prove shared route state.
- All rejected SHM/handle/chunked requests release resources exactly once.
- `rg "spawn_blocking" core/transport/c2-server/src` shows callback execution
  only through the new scheduler module, plus unrelated non-callback uses if
  any are documented.
- Full Python SDK tests and Rust workspace tests pass before implementation is
  considered complete.

## Closed-Loop Review Record

The first strict review cycle found and addressed these plan gaps:

- The first draft placed admission in `dispatch_call` after a per-call async
  task already existed. The spec now requires inline and buddy logical-call
  admission in `handle_connection` before per-call task spawn.
- The first draft did not state how queued calls interact with
  `Connection::wait_idle()`. The spec now requires RAII in-flight coverage for
  queued and active calls.
- The first draft did not mention `RuntimeRouteSpec` validation. The spec now
  requires runtime registration validation to move from old scheduler snapshots
  to route execution snapshots.
- The first draft did not explicitly separate incomplete chunk reassembly from
  complete CRM call execution. The spec now keeps incomplete chunks under
  `ChunkRegistry` limits and admits only the complete logical CRM call.
