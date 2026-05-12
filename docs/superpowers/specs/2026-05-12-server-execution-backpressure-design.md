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

The design must include a Rust-only non-Python callback adapter test so this is
proven mechanically and not only by the Python PyO3 integration path.

### I8. Request Cleanup On Rejection Or Close

Every rejected, cancelled, or closed remote request must release owned request
resources exactly once, including SHM-backed `RequestData::Shm`,
`RequestData::Handle`, chunk reassembly buffers, and inline buffers.

This prevents memory leaks and stale buffer leases under overload or route
unregister/shutdown races.

### I9. No Legacy Bypass

No production or test helper may continue to invoke remote CRM callbacks through
the old direct `spawn_blocking` + `blocking_acquire` path. Tests must register
routes through the same server-owned registration authority as production.

This prevents half-migrations where tests pass through old helpers while real
IPC uses the new scheduler, or the reverse.

### I10. Connection Close Cancels Queued Work

Every remote call that has been admitted or queued must be counted as
connection in-flight work until it sends a response or is rejected and cleaned
up. Connection shutdown must cancel queued calls that have not started callback
execution, wait only for active callbacks plus cancellation cleanup, and then
clean up per-connection chunk and SHM state.

This prevents a connection close from running `wait_idle()` and
`chunk_registry.cleanup_connection()` while a logical call still owns request
data, without forcing a closed connection to wait for queued work to eventually
receive worker capacity.

### I11. Route Construction Authority

Registered routes must be constructible only through server-owned `c2-server`
APIs that attach the route to that concrete server's
`ServerExecutionScheduler`. Downstream crates, SDK bindings, and test helpers
must not be able to create a routable `CrmRoute` by directly filling public
fields, by calling a standalone public builder, or by supplying a standalone
scheduler.

This prevents stale or partial migration paths where production registration,
PyO3 helpers, relay test support, or Rust tests bypass the global execution
budget by constructing routes with the old per-route `Scheduler`.

### I12. Close And Drain Semantics

Route unregister and server shutdown must atomically close admission for the
affected route/server, reject and clean up queued calls that have not started
callback execution, and keep active callbacks accounted until response or error
completion or until an explicit drain timeout reports that active work did not
finish.

This prevents unregister/shutdown races where new calls slip in after close,
queued calls hang forever, or shutdown returns while admitted callback work still
owns request resources.

### I13. Chunk Assembly Boundary

Incomplete chunked frames are not counted as admitted CRM calls, but their
buffering and async processing must still be bounded by the existing chunk
registry limits before payload cloning or task fan-out can grow without bound.
Only a fully reassembled logical CRM call enters the execution scheduler.

This prevents overload cases where `max_pending_requests` is correct for
complete calls but chunked traffic creates an independent unbounded work queue
before the call becomes schedulable.

### I14. Fair Promotion

Queued calls that are eligible for execution must be promoted without
starvation. Promotion must respect per-route FIFO order, route read/write
fairness, and a server-wide fair ordering across ready routes.

This prevents a hot route or a stream of newly admitted reads from indefinitely
delaying older queued work that has available route and global capacity.

### I15. Shutdown Hooks After Active Drain

Python `@on_shutdown` callbacks must run only after Rust reports that the route
has closed admission, rejected and cleaned all not-started queued calls, and
drained active callbacks for that route. If a future timeout or forced shutdown
path is added, the structured native outcome must say the route did not drain
and Python must not invoke `@on_shutdown` for that route.

This prevents shutdown hooks from racing with still-running resource methods on
the same Python resource object.

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
| I9 no bypass | server-owned route registration helpers | production, Rust tests, PyO3, and relay support all use the same registration path | production and tests | relay test support starts live IPC servers | all FFI constructors updated | source/compile-fail guardrails for old helper usage | old `Scheduler::execute` removed or test-only gated |
| I10 connection drain | `Connection::FlightGuard`, request owner id, cancellation token | per-connection atomic counter plus scheduler queued-call index by connection/request | connection close cancels queued not-started calls, waits for active callbacks and cancellation cleanup | IPC connection lifecycle; chunk cleanup after active/cancel idle | no SDK-owned drain state | tests close connection with queued calls and assert queued callback does not run | manual `flight_inc` / `flight_dec` paths replaced with RAII guard; no closed-connection wait for worker capacity |
| I11 route construction authority | `Server` route registration API from validated route spec | private route fields plus scheduler identity/generation | dispatcher accepts only server-built routes | not wire-visible; affects every IPC route | PyO3 receives opaque route handle, not raw scheduler | relay/Rust test helpers use production server registration API | `CrmRoute { ... }`, public `scheduler` fields, public standalone route builders, and exported old `Scheduler` are removed or crate-private |
| I12 close/drain | unregister, shutdown, connection close, explicit drain policy | scheduler route/server closed state, active counters, cancellation queues | admission, queued waiter cancellation, active permit drop, drain barrier or timeout outcome | IPC close and admin lifecycle | SDK consumes structured unregister/shutdown outcomes | tests for queued, active, timeout, and new calls during close | no separate Python close authority or old scheduler close path |
| I13 chunk assembly | IPC frame reader and `ChunkRegistry` | chunk registry counters, assembler memory, optional bounded chunk-processing semaphore | chunk frame handling before logical call admission | chunked call frames before route callback execution | no SDK-owned chunk queue | chunk overload tests with incomplete frames | no unbounded `tokio::spawn` + payload clone path for chunk frames |
| I14 fair promotion | scheduler admission order and route queues | per-route FIFO queues plus server ready-route order | permit promotion when capacity changes | affects all IPC call variants after admission | local route waits use same route order | cross-route and read/write starvation tests | no busy sleep, LIFO, or per-route-only wake policy that can starve eligible work |
| I15 shutdown hooks | native route close/drain outcome | `UnregisterOutcome` / `ShutdownOutcome` drain fields consumed by Python | Python slot removal and `@on_shutdown` invocation | unregister/shutdown control path, not CRM wire | Python invokes hook only after `active_drained=true` | tests with blocked active method and unregister/shutdown | no SDK-side hook invocation based only on route removal |

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

`RouteExecutionHandle` carries an identity for its owning
`ServerExecutionScheduler`, including a generation value. Public server
registration rejects any route handle not produced by that server instance. This
prevents handles from another server, a test-only scheduler, or the retired
per-route scheduler from being inserted into the dispatcher.

The scheduler holds a single mutex-protected state tree:

```text
Global state:
  closed
  pending_remote
  active_remote_workers
  queued_by_connection
  max_pending_requests
  max_execution_workers

Per-route state:
  closed
  draining
  mode
  access_map
  max_pending
  max_workers
  fifo_wait_queue
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

Promotion is event-driven: releasing an execution permit, closing a route, or
admitting a newly runnable call wakes the scheduler to promote the oldest
eligible work without busy sleeps. The scheduler must not use a policy that can
leave an eligible queued call waiting indefinitely while global and route
capacity are available.

### Remote Execution Flow

All remote call variants use the same choke point:

```text
handle_connection / dispatch_chunked_call
  decode logical route + method index as early as possible
  resolve route and validate method index
  acquire RemotePendingPermit + FlightGuard + cancellation owner
  materialize request ownership only after admission where possible
  result = server.execution_scheduler.execute_remote(permit, request, callback)
  send_route_execution_result(...)
```

`RemotePendingPermit` is acquired before `spawn_blocking`. For inline and buddy
calls, the connection loop decodes the call-control envelope and performs route
admission before spawning a per-call async task. Buddy SHM data is read only
after pending admission succeeds, so rejected calls do not materialize large
request buffers.

The current IPC frame reader still receives the frame body before it can decode
the call-control envelope; that allocation remains bounded by `max_frame_size`.
This design's pending budget begins at the decoded logical CRM call boundary and
must happen before per-call task fan-out, SHM/handle materialization,
`PyShmBuffer`/memoryview creation, and callback execution.

For chunked calls, incomplete chunks remain governed by the existing chunk
registry limits (`max_total_chunks`, assembler timeout, reassembly memory). The
complete logical call enters the execution scheduler only after
`chunk_registry.finish()` returns route name, method index, and request handle.
If chunk frames continue to be handled in spawned async tasks, the spawn and
payload clone must be behind a bounded chunk-processing permit or be replaced by
inline processing in the connection loop. `max_pending_requests` is not allowed
to be the only backpressure for incomplete chunk traffic.

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

Each queued remote permit is indexed by `(connection_id, request_id)`. When a
connection closes, the server cancels all queued permits owned by that
connection before `wait_idle()` observes idle. Cancelled queued calls release
request data and do not later promote to callback execution. Active callbacks
that already hold an `ExecutionPermit` remain accounted until they finish; the
response path must tolerate the writer being closed and still release all owned
resources.

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
- `route concurrency capacity exceeded: max_pending=N`
- `route closed`
- `server closing`

`max_workers` no longer has a normal reject-fast error for remote calls. It is
a worker budget that queues until route pending or global pending capacity is
exceeded. `try_acquire` may remain for diagnostics or explicit nonblocking
local APIs, but production remote dispatch must not use it.

`max_execution_workers` likewise does not have a normal reject-fast overload
error on the remote path. If global worker capacity is unavailable, the request
waits in the scheduler until `max_pending_requests`, route `max_pending`,
connection cancellation, route close, or server close decides its fate.
`server execution capacity exceeded: max_execution_workers=N` may appear only in
explicit nonblocking diagnostics, never in production remote CRM dispatch.

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

### Registration Authority And Public API Guardrails

The migration must remove the current ability to construct routable server
routes by struct literal. `CrmRoute` fields that define callback, method table,
contract metadata, and execution handle become private. Public route
registration is exposed as a server-owned API such as
`Server::register_route_spec(spec, callback)` that validates the route spec,
callback adapter, and access map, then attaches the resulting route to that
server's scheduler. A standalone public `CrmRouteBuilder` is not allowed because
it would recreate an authority bypass unless it were crate-private and callable
only from `Server`.

`Dispatcher::register` becomes `pub(crate)` and is called only after `Server`
has validated scheduler identity. Public APIs should expose route registration
through `Server`/`RuntimeSession`, not through arbitrary `Dispatcher` mutation.

`RuntimeSession::register_route` must also stop accepting an externally
constructed `CrmRoute`. It should either call the same server-owned registration
API with `RuntimeRouteSpec` and callback adapter inputs, or accept only an opaque
server-created pending-route token whose scheduler identity has already been
stamped by that `Server`.

The old `Scheduler` type must no longer be part of the public `c2-server` API.
`AccessLevel` and `ConcurrencyMode` may remain public, but `pub use
scheduler::Scheduler` and any production import of `Scheduler` are removed. If a
small amount of old scheduler code is temporarily useful during the migration,
it must be crate-private or test-only and must not be able to execute CRM
callbacks.

PyO3 `build_route`, Rust relay test support, and core server test helpers must
call the same server-owned registration API. They must not construct
`CrmRoute { ... }`, instantiate a standalone route scheduler, or expose raw
scheduler objects to Python.

### Unregister, Shutdown, And Hook Ordering

`Server::unregister_route` and server shutdown use a native close transaction
with an explicit drain policy:

```text
close route/server admission
cancel queued calls that have not started callback execution
wait for active execution permits to drop, or return timeout outcome
return structured close outcome
```

The close outcome includes at least:

```text
queued_cancelled
active_drained
active_remaining
drain_timed_out
closed_reason
```

`RuntimeSession` forwards those fields in `UnregisterOutcome` and
`ShutdownOutcome`. Python removes local slots and invokes `@on_shutdown` only
for routes whose native outcome has `active_drained=true`. A route that fails to
drain is reported explicitly and does not run the shutdown hook.

## Implementation Work Items

1. Add `max_execution_workers` to `c2-config` server defaults, env resolver,
   override schema, validation, and dict projections.
2. Add Python `ServerIPCOverrides.max_execution_workers` typed facade and pass
   it into `RustServer`.
3. Add `ServerExecutionScheduler` and migrate route concurrency state into it.
4. Change `CrmRoute` to store a `RouteExecutionHandle`, make route fields
   private, and add a server-owned registration API that stamps scheduler
   identity/generation onto every registered route.
5. Make `Dispatcher::register` crate-private or otherwise unreachable from
   external route construction, and require public registration to validate that
   the route handle belongs to the receiving server.
6. Remove `Scheduler` from the public `c2-server` API; keep only
   `AccessLevel`, `ConcurrencyMode`, and new execution-handle types public.
7. Change `RuntimeSession::register_route` so it no longer accepts externally
   constructed `CrmRoute`; route creation must flow through the server-owned
   registration API or an opaque server-created pending-route token.
8. Update PyO3 `build_route`, runtime session tests, c2-server tests, and relay
   test support to use the production server-owned registration API instead of
   direct `CrmRoute { ... }` construction.
9. Change `RuntimeRouteSpec` validation in `c2-runtime` to read concurrency
   metadata from `RouteExecutionHandle` snapshots instead of
   `route.scheduler.snapshot()`.
10. Replace remote `execute_route_request(... spawn_blocking ...)` with
   scheduler-owned `execute_remote`.
11. Move inline and buddy logical-call admission into `handle_connection` before
    per-call `tokio::spawn`; keep chunked incomplete-call limits in
    `ChunkRegistry`, then admit the complete logical call after
    `chunk_registry.finish()`.
12. Bound chunked frame processing before payload clone/task fan-out, either by
    processing chunks inline in the connection loop or by acquiring a bounded
    chunk-processing permit tied to `ChunkRegistry` limits.
13. Implement fair, event-driven promotion across route queues and preserve
    writer fairness within `read_parallel` routes.
14. Replace manual connection `flight_inc` / `flight_dec` call paths with an
    RAII guard plus scheduler cancellation owner that covers queued, cancelled,
    and active calls without waiting for closed-connection queued work to
    receive worker capacity.
15. Change Python `RouteConcurrency` FFI to wrap the route execution handle and
    enter local guards through `ExecutionScope::Local`.
16. Remove or test-gate old `Scheduler::execute` and any production
    `blocking_acquire` remote path.
17. Add native route close/drain outcomes and forward them through
    `RuntimeSession`, PyO3, and Python `NativeServerBridge`.
18. Add explicit close drain policy and timeout outcomes; timeout or forced
    close must return `active_drained=false` and must not trigger Python
    shutdown hooks.
19. Move Python `@on_shutdown` invocation behind `active_drained=true`.
20. Add snapshot/metrics bindings needed by tests.
21. Update existing tests that assert `max_workers` reject-fast semantics; keep
    reject-fast tests only for `max_pending`, `max_pending_requests`, and
    explicit nonblocking APIs.
22. Update Python unit-test fakes (`FakeScheduler`) to model the new route
    handle API enough to keep local direct-call tests meaningful, including
    closed-route behavior.
23. Add a Rust-only non-Python `CrmCallback` adapter test proving that
    language-neutral registration receives the same server execution budgets as
    the PyO3 path.
24. Update docs and examples so `max_workers` and `max_execution_workers` are
    described as queueing worker budgets, not reject-fast capacity checks or
    thread counts.

## Required Tests

Rust core tests:

- One route with `max_workers=2`, ten concurrent remote-style executions:
  at most two callbacks active; all ten complete unless pending capacity is
  lower.
- One route with `max_pending=3`: the fourth admitted call is rejected before
  callback execution and releases request resources.
- Ten routes with `max_workers=10`, server `max_execution_workers=4`: global
  active remote callbacks never exceed four.
- Server `max_execution_workers=1`, `max_pending_requests>=2`: a second remote
  call queues instead of returning
  `server execution capacity exceeded: max_execution_workers=1`.
- Server `max_pending_requests=8`: ninth remote call is rejected before
  `spawn_blocking`.
- `read_parallel`: readers run concurrently, writes are exclusive, and waiting
  writers block newly arriving readers.
- Cross-route fairness: two or more saturated routes with available per-route
  capacity all make progress under a lower global `max_execution_workers`.
- Route unregister closes queued calls and rejects new calls.
- Route unregister with active calls leaves active permits accounted until those
  calls finish, closes queued waiters with deterministic errors, and returns
  `active_drained=true` only after active count reaches zero.
- Route unregister with a drain timeout returns `active_drained=false`,
  `drain_timed_out=true`, and does not report the route as safe for SDK
  shutdown hooks.
- Connection close cancels queued not-started calls owned by that connection,
  does not execute their callbacks later, and still waits for active callbacks
  plus cancellation cleanup before per-connection cleanup.
- SHM request rejection frees the source allocation and allows reuse.
- Incomplete chunked frame floods are bounded by chunk registry/semaphore limits
  and cannot create unbounded task fan-out before a logical call is complete.
- Direct registration through `Dispatcher` or `CrmRoute` struct literals is no
  longer possible from downstream crates.
- Rust-only non-Python `CrmCallback` registration is limited by the same global,
  route, pending, and close/drain semantics as the PyO3 path.

Python integration tests:

- `cc.set_server(ipc_overrides={"max_execution_workers": 2})` limits remote
  active callbacks across multiple resources.
- Mixed local and remote calls share route `max_workers`.
- Local direct calls do not consume server `max_execution_workers`.
- `@on_shutdown` is not invoked while an active resource method is still running;
  queued not-started calls are rejected/cleaned before the hook runs.
- Direct IPC remains relay-independent when `C2_RELAY_ANCHOR_ADDRESS` points to
  an unavailable relay.
- Existing syntax compatibility tests still pass on Python 3.10.

Guardrail tests:

- A source-level test or lint script fails if production `server.rs` invokes
  `tokio::task::spawn_blocking` for CRM callbacks outside
  `ServerExecutionScheduler`.
- Test helpers registering routes must call the same server-owned registration
  API used by production registration.
- A source-level test fails if production inline or buddy call frames spawn a
  per-call task before logical-call admission.
- A source-level test fails on `pub use scheduler::Scheduler`, `pub scheduler:`,
  or direct `CrmRoute {` construction outside the server-owned registration
  module.
- A source-level test fails if `sdk/python/native` imports the old
  `c2_server::scheduler::Scheduler`.
- A compile-fail or source allowlist guard fails on `route.scheduler`,
  `Scheduler::new`, `Scheduler::with_limits`, `blocking_acquire`, and
  standalone public route builders outside explicitly allowed new execution
  module unit tests.
- A source-level or compile-fail guard fails if public
  `RuntimeSession::register_route` accepts `CrmRoute` directly.
- A source-level test fails if Python invokes `_invoke_shutdown` without checking
  a native `active_drained` close outcome for that route.

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

Connection close semantics after the migration:

```text
queued remote calls owned by the closed connection: cancel, clean, never run
active remote callbacks owned by the closed connection: finish accounting and cleanup
response send after connection close: best-effort/error-tolerant cleanup path
```

## Acceptance Criteria

- No production remote CRM callback can reach `spawn_blocking` before passing
  global and route execution admission.
- Inline and buddy call frames are admitted before the per-call async task is
  spawned; chunked calls are admitted immediately after final reassembly and
  before callback execution.
- Incomplete chunked frames are bounded independently of CRM execution pending
  admission.
- `max_execution_workers` saturation queues admitted calls; it is not a normal
  remote dispatch capacity error.
- Tokio `.max_blocking_threads()` is set from `max_execution_workers`.
- `max_pending_requests` is exercised by server execution tests, not just
  config parsing tests.
- Connection `wait_idle()` covers active callbacks and queued-call cancellation
  cleanup, not closed-connection wait-for-capacity.
- Connection close cancels queued calls that have not started and does not wait
  for those calls to acquire worker capacity.
- Python `@on_shutdown` runs only after native close outcomes prove active
  callbacks for that route have drained.
- Close drain timeout paths are explicit: they return `active_drained=false`,
  do not invoke Python shutdown hooks, and do not silently detach active
  callbacks as if shutdown had completed cleanly.
- Public route registration cannot accept a route handle from another server, an
  old standalone scheduler, or a public `RuntimeSession::register_route`
  parameter that is an externally constructed `CrmRoute`.
- Eligible queued calls are promoted fairly across routes and cannot starve when
  global and route capacity are available.
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
  `Connection::wait_idle()`. The spec now requires queued-call cancellation on
  connection close, active-call in-flight coverage, and cleanup before
  per-connection chunk/SHM cleanup.
- The first draft did not mention `RuntimeRouteSpec` validation. The spec now
  requires runtime registration validation to move from old scheduler snapshots
  to route execution snapshots.
- The first draft did not explicitly separate incomplete chunk reassembly from
  complete CRM call execution. The spec now keeps incomplete chunks under
  `ChunkRegistry` limits and admits only the complete logical CRM call.

The second strict review cycle found and addressed these plan gaps:

- Current `CrmRoute` fields are public and `Dispatcher::register` accepts
  externally constructed routes, which would let SDK bindings or tests bypass
  the new server-owned scheduler. The spec now adds route construction authority
  as a first-class invariant and requires private route fields plus a
  server-owned registration API.
- Current `c2-server` publicly exports the old `Scheduler`, and PyO3 / relay
  test support directly instantiate it. The spec now requires removing that
  public export, updating PyO3 and test helpers to use the production
  server-owned registration API, and adding source-level guardrails against
  direct scheduler imports.
- The earlier wording implied pending admission happened before all frame-body
  allocation. The spec now states the exact boundary: the frame body remains
  bounded by `max_frame_size`, while execution pending admission starts at the
  decoded logical CRM call boundary before task fan-out, SHM materialization,
  memoryview creation, and callback execution.

The third strict review cycle found and addressed these plan gaps:

- Current chunked call handling spawns async work before a complete CRM call
  exists. The spec now treats incomplete chunk assembly as a separate bounded
  source-to-sink path and requires chunk processing to be bounded before payload
  clone/task fan-out.
- The earlier queueing design did not state cross-route promotion fairness. The
  spec now requires event-driven fair promotion across ready routes, in addition
  to per-route read/write fairness.

The fourth strict review cycle found and addressed these plan gaps:

- The earlier connection-drain invariant required waiting for queued calls on a
  closed connection, which could keep closed-connection work alive until worker
  capacity became available. The spec now requires cancellation of queued
  not-started calls on connection close, while active callbacks remain accounted
  until cleanup.
- The earlier close/drain wording did not prove that Python `@on_shutdown`
  hooks run after active callbacks finish. The spec now requires native
  structured drain outcomes and gates hook invocation on `active_drained=true`.
- The earlier error semantics listed `max_execution_workers` as a normal remote
  capacity error even though it is supposed to queue. The spec now restricts that
  error to explicit nonblocking diagnostics.
- The earlier route builder wording allowed a standalone public builder. The spec
  now permits only server-owned route registration authority and requires
  stronger source/compile-fail guardrails.
- The earlier cross-language invariant was conceptual. The spec now requires a
  Rust-only non-Python callback adapter test as mechanical proof.

The fifth strict review cycle found and addressed these plan gaps:

- `RuntimeSession::register_route` was still allowed, by the plan wording, to
  accept an externally constructed `CrmRoute`. The spec now requires
  `RuntimeSession` to use the server-owned registration API or an opaque
  server-created pending-route token, and adds a guardrail against public
  `CrmRoute` parameters.
- Close/drain did not define what happens when active callbacks do not finish.
  The spec now requires an explicit drain policy and timeout outcome; timeout
  paths return `active_drained=false` and cannot trigger Python shutdown hooks.
