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

A graceful close is not allowed to report the affected route as drained while
`active_local + active_remote > 0`. A timed or forced close may return before
active callbacks finish only by returning an explicit undrained outcome
(`active_drained=false`, `active_remaining>0`, `drain_timed_out=true` or
`forced=true`). Remaining active permits stay owned by the native execution
scheduler/runtime until their final cleanup path releases request resources,
route counters, and connection ownership. A timed/forced outcome must not run
SDK shutdown hooks or claim that route cleanup is complete.

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
drained active callbacks for that route. `active_drained` means
`active_local + active_remote == 0`; same-process thread-local calls are part of
the condition, not a Python-side exception. If a timeout or forced shutdown path
returns before that condition holds, the structured native outcome must say the
route did not drain and Python must not invoke `@on_shutdown` for that route.

This prevents shutdown hooks from racing with still-running resource methods on
the same Python resource object.

### I16. Direct IPC Admin Shutdown Uses Close Transaction

Direct IPC `ShutdownClient` / `ipc_shutdown()` is a real server shutdown entry.
It must not bypass the same close transaction used by `RuntimeSession` and
`NativeServerBridge` shutdown paths. The direct admin signal must close server
admission, cancel queued not-started work, preserve active permit ownership, and
produce or record the same structured drain outcome. If a legacy bool helper can
only report signal delivery, it must not be treated as proof that hooks ran or
that routes drained.

This prevents high-privilege same-host supervisors from stopping the IPC server
through a path that leaves queued work, active callbacks, route state, or Python
hooks outside the lifecycle contract.

### I17. FFI Lifecycle Entries Are Outcome-Gated

Every FFI surface that can unregister a route or stop a server must either be
removed from the public SDK surface or return/record the same structured native
close outcomes as `RuntimeSession`. PyO3 `RustServer.unregister_route()`,
PyO3 `RustServer.shutdown()`, and future C++/Go/Fortran/Java FFI server handles
must not expose bool/unit lifecycle methods that close routes or stop the server
without `RouteCloseOutcome` / `ShutdownOutcome`.

This prevents SDK or test code from bypassing close/drain/hook policy through a
lower-level native handle even after the Python bridge and top-level registry are
fixed.

### I18. Language-Neutral Runtime Builder Owns Server Thread Guardrails

The runtime that drives IPC server accept loops and remote callback
`spawn_blocking` work must be constructed through a Rust-core server runtime
builder that consumes `ServerIpcConfig.max_execution_workers`. SDK-specific FFI
layers may request server start, but they must not create independent Tokio
runtimes with hardcoded blocking-pool policy.

This keeps `max_execution_workers` language-neutral for future SDKs and prevents
the Python PyO3 wrapper from becoming the only implementation that applies the
server execution thread guardrail.

## Source-To-Sink Matrix

| Invariant | Producer | Storage | Consumer | Network/Wire | FFI/SDK | Test/Support | Legacy/Bypass |
| --- | --- | --- | --- | --- | --- | --- | --- |
| I1 global execution | `ServerIpcConfig.max_execution_workers` from Rust resolver/env/overrides | `ServerExecutionScheduler` global state | remote CRM dispatch before callback | IPC calls only; HTTP relay forwards to IPC server unchanged | Python passes config only; future SDKs use same server config | Rust server stress tests assert active callbacks never exceed limit | ban direct `spawn_blocking` callback paths outside scheduler |
| I2 global pending | `ServerIpcConfig.max_pending_requests` | `ServerExecutionScheduler` pending counter | call preparation before dispatch task spawn for inline/buddy; chunk completion before callback execution | IPC inline, SHM, handle, chunked call paths; incomplete chunks remain governed by chunk registry limits | no Python-owned counter | overload tests assert early `ResourceUnavailable` and cleanup | no hidden Tokio blocking queue as first backpressure layer; no unbounded call-task fan-out for complete inline/buddy calls |
| I3 route workers | `ConcurrencyConfig.max_workers` / route spec | route state inside execution scheduler | local and remote route permits | route metadata does not cross wire; route is resolved from handshake table | PyO3 forwards values; route handle exposed to local proxy | current max-worker tests updated from reject-fast to queue semantics | remove scheduler-only post-blocking rejection for remote |
| I4 route pending | `ConcurrencyConfig.max_pending` / route spec | route state inside execution scheduler | local and remote route admission | not wire-visible | PyO3 forwards values; no SDK-side fallback | route overload tests for local, remote, mixed calls | test fakes must not omit pending accounting |
| I5 read/write | CRM method access map | route state | permit acquisition and promotion | method index crosses wire | SDK builds access map from CRM metadata | read/write fairness tests | no raw callback invocation without access map |
| I6 local/remote shared | route registration builds one route handle | route handle cloned into `CrmRoute` and Python local slot | local proxy guard and remote scheduler | remote IPC path resolves route from dispatcher | `RouteConcurrency` wraps same core handle for acquire/snapshot only | mixed local/remote tests | no separate Python lock/counter state and no SDK-exposed scheduler close authority |
| I7 cross-language | `CrmCallback` trait and `CrmRoute` | `c2-server` | all SDK callback adapters | language-neutral IPC request/response data | Python is one adapter; other SDKs can adapt callback | Rust-only callback tests plus Python integration | no Python-specific field in core config |
| I8 cleanup | request materialization in dispatch path | request object until execution result/reject | scheduler rejection, route close, callback completion | IPC SHM/handle/chunked data | PyO3 only sees request after permit | SHM reuse tests after rejection | cleanup must not depend on callback running |
| I9 no bypass | server-owned route registration helpers | production, Rust tests, PyO3, and relay support all use the same registration path | production and tests | relay test support starts live IPC servers | all FFI constructors updated | source/compile-fail guardrails for old helper usage | old `Scheduler` execution APIs deleted or quarantined so no callback/test helper can import them |
| I10 connection drain | `Connection::FlightGuard`, request owner id, cancellation token | per-connection atomic counter plus scheduler queued-call index by connection/request | connection close cancels queued not-started calls, waits for active callbacks and cancellation cleanup | IPC connection lifecycle; chunk cleanup after active/cancel idle | no SDK-owned drain state | tests close connection with queued calls and assert queued callback does not run | manual `flight_inc` / `flight_dec` paths replaced with RAII guard; no closed-connection wait for worker capacity |
| I11 route construction authority | `Server` route registration API from validated route spec | private route fields plus scheduler identity/generation | dispatcher accepts only server-built routes | not wire-visible; affects every IPC route | PyO3 receives opaque route handle, not raw scheduler | relay/Rust test helpers use production server registration API | `CrmRoute { ... }`, public `scheduler` fields, public standalone route builders, and exported old `Scheduler` are removed or crate-private |
| I12 close/drain | unregister, shutdown, connection close, registration rollback, explicit drain policy | scheduler route/server closed state, active counters, cancellation queues, close journal | admission, queued waiter cancellation, active permit drop, drain barrier or timeout outcome | IPC close, admin lifecycle, and relay-registration rollback | SDK consumes structured unregister/shutdown/rollback outcomes; local wrapper cannot close routes directly | tests for queued, active, timeout, rollback, and new calls during close | no separate Python close authority, no `RouteConcurrency.close()`, no ignored rollback outcome, and no old scheduler close path |
| I13 chunk assembly | IPC frame reader and `ChunkRegistry` | chunk registry counters, assembler memory, optional bounded chunk-processing semaphore | chunk frame handling before logical call admission | chunked call frames before route callback execution | no SDK-owned chunk queue | chunk overload tests with incomplete frames | no unbounded `tokio::spawn` + payload clone path for chunk frames |
| I14 fair promotion | scheduler admission order and route queues | per-route FIFO queues plus server ready-route order | permit promotion when capacity changes | affects all IPC call variants after admission | local route waits use same route order | cross-route and read/write starvation tests | no busy sleep, LIFO, or per-route-only wake policy that can starve eligible work |
| I15 shutdown hooks | native per-route close/drain outcome | `RouteCloseOutcome` generation and drain fields consumed by Python | Python slot removal and `@on_shutdown` invocation | unregister/shutdown/rollback control path, not CRM wire | Python invokes hook only after `active_drained=true`, an unconsumed route generation, and a committed local slot | tests with blocked active method, unregister/shutdown, journal replay, and rollback non-invocation | no SDK-side hook invocation based only on route removal, rollback records, or stale journal records |
| I16 direct admin shutdown | `ShutdownClient` signal, `c2_ipc::control::shutdown`, PyO3 `ipc_shutdown`, Python `client.util.shutdown()` | server close transaction plus `CloseOutcomeJournal` record | signal handler, `Server::shutdown`, runtime bridge shutdown reconciliation via one-shot journal consume | IPC signal frame, no relay/CRM wire | bool admin helper reports delivery only unless a structured outcome is exposed | tests send direct IPC shutdown with queued/active calls and consume journal once | raw `Server::shutdown()` must enter close transaction or be private to it; no signal-only lifecycle bypass |
| I17 FFI lifecycle authority | SDK FFI server handles and native bridge methods | native close transaction outcome and close journal | SDK bridge cleanup and hook gate | no new wire; affects native lifecycle control | no public bool/unit `RustServer.unregister_route` / `RustServer.shutdown`; future SDKs consume the same outcomes | source guards and PyO3 API tests | no low-level lifecycle method can close routes or stop server without `RouteCloseOutcome` / `ShutdownOutcome` |
| I18 runtime builder | `ServerIpcConfig.max_execution_workers` and `ServerRuntimeOptions` | Rust-core server runtime builder | all SDK server start paths | not wire-visible | PyO3 and future SDKs call shared builder, not Tokio directly | Rust-only builder test and PyO3 projection test | no SDK-owned Tokio builder with hardcoded blocking-pool policy for server callbacks |

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

The SDK-visible route concurrency wrapper exposes local admission and snapshots
only. It must not expose `close()` / `shutdown()` as a public Python or PyO3
method because that would close a route handle without producing
`RouteCloseOutcome`, cancelling queued remote work, or gating `@on_shutdown`.
Route closure authority belongs to the native server close transaction; after a
successful transaction the shared handle is already closed, so Python must not
perform a second scheduler-only close.

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

Every ingress validates the field before server construction:

- absent value uses the Rust default below;
- explicit values must be `>= 1`;
- explicit `0`, negative Python values, parse failures, or values that cannot
  round-trip into the Rust config type are rejected in `c2-config`, PyO3 config
  parsing, and Python override projection tests.

The shared server runtime builder sets Tokio
`.max_blocking_threads(max_execution_workers)` as a guardrail. The primary
policy remains C-Two's `ServerExecutionScheduler`; Tokio's limit is only a
backstop.

The Tokio runtime policy is not owned by PyO3. Add a Rust-core
`ServerRuntimeOptions` / `ServerRuntimeBuilder` that derives:

```text
async_worker_threads = 2
max_blocking_threads = max_execution_workers
```

from `ServerIpcConfig` and is reused by PyO3 and future SDK bindings. The
hardcoded `.worker_threads(2)` value may remain as the async I/O runtime default,
but callback blocking-pool sizing must always come from
`max_execution_workers`. SDK bindings must not call `tokio::runtime::Builder`
directly for IPC server runtimes except through this shared builder.

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

The default may be based on OS parallelism because it is server-wide and
language-neutral. It is still capped to a conservative default maximum so a
machine with many CPUs does not silently allocate hundreds of blocking workers.
Operators that need a larger budget may opt in explicitly, but that explicit
override is a capacity decision and must pass validation instead of being
silently clamped.

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

The guardrail is intentionally strict: production and tests should not keep the
old `Scheduler`, `Scheduler::new`, `Scheduler::with_limits`,
`blocking_acquire`, `Scheduler::execute`, `route.scheduler`, or public
`scheduler` module paths alive as an allowlisted parallel mechanism. The clean
cut is to move shared enum definitions such as `AccessLevel` and
`ConcurrencyMode` into the new execution module or a small neutral types module,
then delete or fully quarantine the old scheduler code so it cannot be imported
by PyO3, runtime, relay support, or server tests.

PyO3 `build_route`, Rust relay test support, and core server test helpers must
call the same server-owned registration API. They must not construct
`CrmRoute { ... }`, instantiate a standalone route scheduler, or expose raw
scheduler objects to Python.

Registration through `RuntimeSession` is a transaction. A route that depends on
relay publication must not become externally routable through direct IPC until
the relay registration step has either succeeded or the route has been rolled
back. The required shape is a pending server route token or equivalent
non-routable reservation: validate and reserve the route, publish to relay if
requested, then commit the route into the dispatcher and open admission. If any
post-reservation step fails, abort through the same close transaction with
`closed_reason=registration_rollback`, record the `RouteCloseOutcome`, and
return it in the registration error. Do not ignore the rollback outcome.
Registration rollback outcomes are never hook-safe: no SDK may install or keep a
committed local slot for a route whose registration transaction failed, and a
rollback journal record must be marked consumed with the registration error so a
later bridge shutdown cannot replay it as a normal route shutdown.

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

For multi-route shutdown, these fields are per-route, not one global flag:

```text
RouteCloseOutcome:
  route_name
  close_generation
  local_removed
  queued_cancelled
  active_drained
  active_remaining
  drain_timed_out
  forced
  closed_reason
  close_error

UnregisterOutcome:
  close: RouteCloseOutcome
  relay_error

ShutdownOutcome:
  route_outcomes: Vec<RouteCloseOutcome>
  relay_errors
  server_was_started
  ipc_clients_drained
  http_clients_drained
```

`removed_routes` may remain as a convenience projection, but it must be derived
from `route_outcomes` and must not be sufficient for Python hook decisions.
`RuntimeSession` forwards the full close outcome through PyO3. Python removes
local slots and invokes `@on_shutdown` only for routes whose native per-route
outcome has `active_drained=true`. A route that fails to drain is reported
explicitly and does not run the shutdown hook.

`Server` owns a close outcome journal:

```text
CloseOutcomeJournal:
  close_generation
  unread_records: Vec<ServerCloseRecord>

ServerCloseRecord:
  source: unregister | shutdown | direct_ipc_shutdown | registration_rollback
  close_generation
  route_outcomes: Vec<RouteCloseOutcome>
  server_drained
```

Every close transaction creates exactly one record. Transactions invoked through
`RuntimeSession` return the outcome to their caller and mark that record consumed
for later bridge reconciliation before returning, including registration rollback
records returned through registration errors. Out-of-band transactions, such as
direct IPC admin shutdown, remain unread until PyO3 exposes an atomic
`RustServer.take_close_outcomes()` or equivalent `RuntimeSession` reconciliation
method that drains unread records once. `NativeServerBridge.shutdown()` consumes
only unread records before deciding whether Python hooks may run. If no per-route
`RouteCloseOutcome` exists for a slot, Python treats the route as not hook-safe.
Python also tracks the last hook generation per route so a stale or already
returned record cannot trigger a hook twice. This one-shot journal is required
for direct IPC shutdown, where the external bool acknowledgement cannot carry
drain data back to the server process's Python bridge.

`closed_reason=registration_rollback` is explicitly excluded from hook
eligibility even if `active_drained=true`, because the route never completed
registration as an SDK-visible resource. Python must clean up any provisional
registration state without invoking `@on_shutdown`.

Close transactions must not depend on a best-effort temporary runtime in a way
that can silently skip route closure. If `RuntimeSession::shutdown` or
`RuntimeSession::unregister_route` cannot drive the native close transaction
because runtime construction or scheduling fails, it must return an explicit
route outcome/error with `local_removed=false`, `active_drained=false`, and an
error reason in `close_error`. It must not follow that failure by calling a raw
signal-only `Server::shutdown()` and then reporting ordinary shutdown progress.

Direct IPC admin shutdown must call the same transaction. The current signal
wire may keep a bool-shaped acknowledgement for compatibility with the admin
probe helper, but that acknowledgement means only "the signal was delivered or
the server was already absent." It must not be reused as a drain result. If the
direct admin path stops the native server before Python sees the structured
outcome, `NativeServerBridge.shutdown()` must reconcile the recorded native
outcome before invoking any Python `@on_shutdown` hook; undrained routes remain
hook-suppressed.

Low-level PyO3 `RustServer` lifecycle methods are not separate authority. Public
`RustServer.unregister_route()` and `RustServer.shutdown()` must either be
removed from the Python module or changed to call the same close transaction and
return the same structured outcomes. A bool return from `unregister_route` or a
unit return from `shutdown` is not allowed because it cannot prove drain state.

## Implementation Work Items

1. Add `max_execution_workers` to `c2-config` server defaults, env resolver,
   override schema, validation, and dict projections; reject explicit values
   below `1` at every Rust/PyO3/Python override ingress.
2. Add Python `ServerIPCOverrides.max_execution_workers` typed facade and pass
   it into `RustServer`.
3. Add Rust-core `ServerRuntimeOptions` / `ServerRuntimeBuilder` and require PyO3
   and future SDK bindings to start IPC servers through it; the builder sets
   Tokio `.max_blocking_threads(max_execution_workers)`.
4. Add `ServerExecutionScheduler` and migrate route concurrency state into it.
5. Change `CrmRoute` to store a `RouteExecutionHandle`, make route fields
   private, and add a server-owned registration API that stamps scheduler
   identity/generation onto every registered route.
6. Make `Dispatcher::register` crate-private or otherwise unreachable from
   external route construction, and require public registration to validate that
   the route handle belongs to the receiving server.
7. Remove `Scheduler` from the public `c2-server` API; keep only
   `AccessLevel`, `ConcurrencyMode`, and new execution-handle types public, and
   move those shared enum definitions out of any old scheduler module if needed.
8. Change `RuntimeSession::register_route` so it no longer accepts externally
   constructed `CrmRoute`; route creation must flow through the server-owned
   registration API or an opaque server-created pending-route token.
9. Make `RuntimeSession::register_route` transactional: pending route token,
   optional relay publication, commit to dispatcher/open admission, or rollback
   through `RouteCloseOutcome` with `closed_reason=registration_rollback`; the
   rollback record is returned/consumed with the registration error and is not
   hook-eligible.
10. Update PyO3 `build_route`, runtime session tests, c2-server tests, and relay
   test support to use the production server-owned registration API instead of
   direct `CrmRoute { ... }` construction.
11. Change `RuntimeRouteSpec` validation in `c2-runtime` to read concurrency
   metadata from `RouteExecutionHandle` snapshots instead of
   `route.scheduler.snapshot()`.
12. Replace remote `execute_route_request(... spawn_blocking ...)` with
   scheduler-owned `execute_remote`.
13. Move inline and buddy logical-call admission into `handle_connection` before
    per-call `tokio::spawn`; keep chunked incomplete-call limits in
    `ChunkRegistry`, then admit the complete logical call after
    `chunk_registry.finish()`.
14. Bound chunked frame processing before payload clone/task fan-out, either by
    processing chunks inline in the connection loop or by acquiring a bounded
    chunk-processing permit tied to `ChunkRegistry` limits.
15. Implement fair, event-driven promotion across route queues and preserve
    writer fairness within `read_parallel` routes.
16. Replace manual connection `flight_inc` / `flight_dec` call paths with an
    RAII guard plus scheduler cancellation owner that covers queued, cancelled,
    and active calls without waiting for closed-connection queued work to
    receive worker capacity.
17. Change Python `RouteConcurrency` FFI to wrap the route execution handle and
    enter local guards through `ExecutionScope::Local`.
18. Delete or fully quarantine old `Scheduler::execute` and
    `blocking_acquire`; production and test helper callback execution must use
    `ServerExecutionScheduler` / `RouteExecutionHandle`.
19. Add native `RouteCloseOutcome` close/drain results and forward them through
    `UnregisterOutcome.close`, `ShutdownOutcome.route_outcomes`, PyO3, and
    Python `NativeServerBridge`; `removed_routes` alone must not drive hook
    invocation.
20. Add `CloseOutcomeJournal` on `Server`; every unregister, shutdown,
    direct-IPC shutdown, and registration rollback writes one record, and PyO3 /
    `RuntimeSession` expose an atomic one-shot consume/reconcile method. Records
    returned synchronously through `RuntimeSession` are marked consumed before the
    call returns, and Python dedupes hook execution by route close generation.
21. Add explicit close drain policy and timeout outcomes; timeout or forced
    close must return `active_drained=false`, keep native ownership of remaining
    active permits until cleanup, and must not trigger Python shutdown hooks.
22. Move Python `@on_shutdown` invocation behind `active_drained=true`.
23. Add snapshot/metrics bindings needed by tests.
24. Update existing tests that assert `max_workers` reject-fast semantics; keep
    reject-fast tests only for `max_pending`, `max_pending_requests`, and
    explicit nonblocking APIs.
25. Update Python unit-test fakes (`FakeScheduler`) to model the new route
    handle API enough to keep local direct-call tests meaningful, including
    closed-route behavior.
26. Add a Rust-only non-Python `CrmCallback` adapter test proving that
    language-neutral registration receives the same server execution budgets as
    the PyO3 path.
27. Update docs and examples so `max_workers` and `max_execution_workers` are
    described as queueing worker budgets, not reject-fast capacity checks or
    thread counts.
28. Change direct IPC `ShutdownClient` handling and raw `Server::shutdown()` so
    they enter the same server close transaction or are private helpers inside
    that transaction; direct `ipc_shutdown()` bool return must not be treated as
    a drain result.
29. Update `NativeServerBridge.shutdown()` reconciliation so an external direct
    IPC shutdown can consume recorded native close outcomes before deciding
    whether Python `@on_shutdown` hooks are allowed to run.
30. Remove public PyO3 and Python `RouteConcurrency.close()` /
    `RouteConcurrency.shutdown()` methods; local route handles may acquire guards
    and expose snapshots, but route closure must flow through the server close
    transaction that returns `RouteCloseOutcome`.
31. Remove or outcome-gate public PyO3 `RustServer.unregister_route()` and
    `RustServer.shutdown()`; no SDK FFI lifecycle method may return bool/unit for
    route/server close.
32. Remove the failure fallback where `RuntimeSession::shutdown` skips route
    close because a temporary runtime could not be created and then calls raw
    `Server::shutdown()`; runtime/scheduling failures must surface as explicit
    undrained per-route outcomes or errors.

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
- A timed or forced close with active callbacks leaves the active permits owned
  by native runtime cleanup; after the callback eventually finishes, snapshots no
  longer report leaked active/pending counters and resources are released once.
- Direct IPC `ShutdownClient` with queued and active route calls enters the same
  close transaction: queued calls are cancelled, active calls remain accounted,
  and the signal-only acknowledgement is not treated as a drain result.
- Raw `Server::shutdown()` cannot be called as a signal-only bypass from
  production code; it is either the close transaction itself or a private helper
  reachable only from that transaction.
- `RuntimeSession::shutdown` runtime-construction failure produces explicit
  undrained route outcomes/errors and does not fall through to raw server signal
  shutdown as if route close had been attempted.
- Connection close cancels queued not-started calls owned by that connection,
  does not execute their callbacks later, and still waits for active callbacks
  plus cancellation cleanup before per-connection cleanup.
- SHM request rejection frees the source allocation and allows reuse.
- Incomplete chunked frame floods are bounded by chunk registry/semaphore limits
  and cannot create unbounded task fan-out before a logical call is complete.
- Direct registration through `Dispatcher` or `CrmRoute` struct literals is no
  longer possible from downstream crates.
- Relay registration failure after local route reservation does not leave a
  routable route behind. The failed registration returns or records a
  `RouteCloseOutcome` with `closed_reason=registration_rollback`, and concurrent
  direct IPC callers either cannot observe the pending route or receive the
  deterministic closed/rollback error.
- Relay registration rollback does not invoke `@on_shutdown` and cannot leave a
  journal record that is later replayed as a normal drained route shutdown.
- Rust-only non-Python `CrmCallback` registration is limited by the same global,
  route, pending, and close/drain semantics as the PyO3 path.
- Rust-only server runtime builder tests prove `max_blocking_threads` is derived
  from `ServerIpcConfig.max_execution_workers`; PyO3 server start uses the same
  builder instead of constructing a server runtime directly.
- `c2-config` rejects `C2_IPC_MAX_EXECUTION_WORKERS=0` and accepts a positive
  explicit override without silently deriving it from registered routes.

Python integration tests:

- `cc.set_server(ipc_overrides={"max_execution_workers": 2})` limits remote
  active callbacks across multiple resources.
- Mixed local and remote calls share route `max_workers`.
- Local direct calls do not consume server `max_execution_workers`.
- `@on_shutdown` is not invoked while either a local direct call or a remote IPC
  resource method is still running; queued not-started calls are rejected/cleaned
  before the hook runs.
- Direct IPC `client.util.shutdown()` followed by bridge shutdown does not run
  hooks for undrained routes and does not run hooks twice for drained routes.
- Shutdown with multiple routes invokes hooks only for the route outcomes whose
  `active_drained=true`; `removed_routes` without per-route drain fields is not
  accepted by the Python bridge.
- A normal `cc.unregister()` or `cc.shutdown()` followed by bridge shutdown does
  not replay the same close journal record or invoke `@on_shutdown` twice for a
  route generation that was already returned to the synchronous caller.
- Direct calls cannot close a route by calling `RouteConcurrency.close()` or
  `RouteConcurrency.shutdown()`; those methods are absent from the SDK-visible
  wrapper and PyO3 class.
- PyO3 `_native.RustServer.unregister_route()` and `_native.RustServer.shutdown()`
  are not callable as bool/unit lifecycle bypasses; they are absent or return the
  structured native close outcome used by `RuntimeSession`.
- `cc.set_server(ipc_overrides={"max_execution_workers": 0})`,
  `C2_IPC_MAX_EXECUTION_WORKERS=0`, and a PyO3 constructor override of `0`
  all fail before server start.
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
  `Scheduler::new`, `Scheduler::with_limits`, `Scheduler::execute`,
  `blocking_acquire`, public old `scheduler` module paths, and standalone public
  route builders in production code, SDK bindings, relay support, and route
  registration tests. Do not allowlist old `Scheduler` execution tests as an
  alternate supported path; move needed enum/type tests to the new module.
- A source-level or compile-fail guard fails if public
  `RuntimeSession::register_route` accepts `CrmRoute` directly.
- A source-level test fails if Python invokes `_invoke_shutdown` without checking
  a native `active_drained` close outcome for that route.
- A source-level test fails if Python shutdown hook decisions use
  `removed_routes` without reading per-route `RouteCloseOutcome.active_drained`.
- A journal reconciliation test fails if a synchronously returned RuntimeSession
  close record remains unread or if a close generation can trigger the same hook
  twice.
- A source-level test fails if direct IPC `ShutdownClient` handling calls a raw
  signal-only `Server::shutdown()` path that cannot return or record route drain
  outcomes.
- A source-level test fails if `sdk/python/native` exposes PyO3
  `RouteConcurrency.close` / `shutdown` or if the Python scheduler wrapper calls
  `_native.close()` / `_native.shutdown()` directly.
- A source-level test fails if `sdk/python/native` exposes bool/unit
  `RustServer.unregister_route` / `RustServer.shutdown` lifecycle methods.
- A source-level test fails if `sdk/python/native` builds an IPC server Tokio
  runtime directly instead of using the Rust-core server runtime builder.
- A fault-injection test covers `RuntimeSession::shutdown` when the close-driving
  runtime/scheduler cannot be created; the result must be an explicit undrained
  route outcome/error, not raw server signal shutdown.
- A source-level or integration test fails if relay registration rollback ignores
  the native close outcome from aborting the pending route.
- A Python integration or native-bridge test fails if a
  `registration_rollback` close record can trigger `@on_shutdown`, either during
  the failed `register()` call or during later bridge shutdown reconciliation.

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

Shutdown semantics after the migration:

```text
active_drained: active_local + active_remote == 0
graceful close: waits until active_drained before reporting hook-safe success
timed/forced close: returns explicit undrained outcome and leaves active permits
  owned by native cleanup until they finish
direct IPC shutdown ack: signal delivery only unless paired with structured
  native close outcome
FFI lifecycle methods: absent or outcome-returning; bool/unit close is forbidden
registration rollback: pending route abort produces RouteCloseOutcome and never
  opens normal route admission or shutdown-hook eligibility
```

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
- The server Tokio runtime is built through a Rust-core runtime builder shared by
  PyO3 and future SDK bindings; SDK FFI must not hardcode callback blocking-pool
  policy.
- Explicit `max_execution_workers=0` is rejected by Rust config parsing, PyO3
  config projection, and Python override entry points; the default uses
  `available_parallelism().unwrap_or(4).clamp(4, 64)`.
- `max_pending_requests` is exercised by server execution tests, not just
  config parsing tests.
- Connection `wait_idle()` covers active callbacks and queued-call cancellation
  cleanup, not closed-connection wait-for-capacity.
- Connection close cancels queued calls that have not started and does not wait
  for those calls to acquire worker capacity.
- Python `@on_shutdown` runs only after native close outcomes prove local and
  remote active callbacks for that route have drained.
- `ShutdownOutcome` exposes per-route close outcomes; `removed_routes` is at
  most a convenience list and cannot be the source of truth for hook execution.
- Direct IPC shutdown outcomes are stored in a native one-shot close journal and
  consumed by bridge reconciliation before hook decisions.
- SDK-visible route concurrency handles expose guard acquisition and snapshots
  only; route close/shutdown cannot be invoked through the local wrapper.
- SDK-visible server lifecycle handles expose no bool/unit route/server close
  authority; low-level FFI lifecycle methods are absent or return structured
  close outcomes.
- Close drain timeout paths are explicit: they return `active_drained=false`,
  do not invoke Python shutdown hooks, and leave remaining active callback
  ownership with native cleanup instead of silently detaching work as if shutdown
  had completed cleanly.
- Top-level `cc.shutdown()`, `NativeServerBridge.shutdown()`,
  `RuntimeSession::shutdown`, direct IPC `ShutdownClient` / `ipc_shutdown()`, and
  raw `Server::shutdown()` all enter the close transaction or are explicitly
  private helpers inside it; none may signal-stop the server while bypassing
  route close/drain/cancel outcomes.
- Runtime or scheduling failures while entering a close transaction surface as
  explicit undrained route outcomes/errors; they cannot silently downgrade into
  signal-only shutdown.
- Public route registration cannot accept a route handle from another server, an
  old standalone scheduler, or a public `RuntimeSession::register_route`
  parameter that is an externally constructed `CrmRoute`.
- Runtime route registration is transactional: relay publication failure rolls
  back through `RouteCloseOutcome` and cannot leave a committed routable route or
  ignore an undrained rollback.
- Registration rollback records are consumed with the registration failure and
  are never eligible to invoke SDK shutdown hooks.
- Eligible queued calls are promoted fairly across routes and cannot starve when
  global and route capacity are available.
- Existing mixed local/remote concurrency tests are updated to the new queueing
  semantics and still prove shared route state.
- All rejected SHM/handle/chunked requests release resources exactly once.
- `rg "spawn_blocking" core/transport/c2-server/src` shows callback execution
  only through the new scheduler module, plus unrelated non-callback uses if
  any are documented.
- `rg "Scheduler::new|Scheduler::with_limits|blocking_acquire|route\\.scheduler|pub use scheduler::Scheduler"` in production, SDK binding, relay support, and registration test paths returns no live bypasses.
- `rg "fn unregister_route\\(|fn shutdown\\(" sdk/python/native/src/server_ffi.rs`
  shows no public bool/unit server lifecycle bypass, or shows only outcome-based
  methods.
- `rg "tokio::runtime::Builder::new_multi_thread" sdk/python/native/src/server_ffi.rs`
  returns no server runtime builder bypass.
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

The sixth strict review cycle found and addressed these plan gaps:

- Direct IPC `ShutdownClient` / `ipc_shutdown()` was not listed as a real
  shutdown source, even though the current server signal handler can call
  `Server::shutdown()` directly. The spec now adds direct admin shutdown as its
  own invariant, source-to-sink row, work item, tests, and acceptance criterion.
- Close timeout wording still did not define ownership for active callbacks that
  continue after an undrained result. The spec now requires native runtime
  ownership of remaining active permits until final cleanup and forbids treating
  timed/forced close as hook-safe completion.
- `active_drained` was route-level but did not explicitly include same-process
  thread-local calls. The spec now defines it as `active_local + active_remote ==
  0` and requires local active-call shutdown-hook tests.
- `max_execution_workers` had a default but no ingress validation invariant. The
  spec now requires explicit values below `1` to be rejected in Rust config,
  PyO3 config parsing, and Python override entry points.
- The guardrail for old `Scheduler` usage still allowed old execution tests as a
  parallel path. The spec now requires deleting or fully quarantining old
  `Scheduler` execution APIs and moving shared enum/type tests to the new
  execution module.

The seventh strict review cycle found and addressed these plan gaps:

- The source-to-sink matrix still said old `Scheduler::execute` could be
  test-only gated, contradicting the clean-cut guardrail. The matrix now requires
  deletion or full quarantine with no callback/test-helper import path.
- Shutdown hook gating was described as per-route, but the planned
  `ShutdownOutcome` shape was not explicit and could still let Python drive hooks
  from `removed_routes`. The spec now requires `RouteCloseOutcome` entries in
  unregister and shutdown outcomes and adds guardrails against `removed_routes`
  as the hook source of truth.

The eighth strict review cycle found and addressed this plan gap:

- Current PyO3 and Python `RouteConcurrency` wrappers expose `close()` /
  `shutdown()`, which can close the route scheduler without a native
  `RouteCloseOutcome`. The spec now limits SDK-visible route handles to
  acquire/snapshot operations and adds work items, tests, and guardrails removing
  direct route-close authority from the local wrapper.

The ninth strict review cycle found and addressed this plan gap:

- Current `RuntimeSession::shutdown` can skip route unregister when its temporary
  runtime creation fails and still proceed toward raw server shutdown. The spec
  now requires runtime/scheduling failures while entering close to become
  explicit undrained route outcomes/errors, with a fault-injection test, rather
  than a signal-only shutdown fallback.

The tenth strict review cycle found and addressed these plan gaps:

- Low-level PyO3 `RustServer.unregister_route()` and `RustServer.shutdown()` were
  not named as lifecycle bypasses, even though they can currently close routes or
  stop the server with bool/unit outcomes. The spec now adds an FFI lifecycle
  authority invariant and guardrails requiring those methods to be removed or
  changed to structured close outcomes.
- Direct IPC shutdown said it would record native outcomes but did not define
  storage or one-shot consumption. The spec now requires a native
  `CloseOutcomeJournal` with atomic PyO3/RuntimeSession reconciliation before
  Python hook decisions.
- Relay registration failure rollback was a real close source but was not in the
  close matrix or tests. The spec now makes registration transactional with a
  pending route token and `registration_rollback` `RouteCloseOutcome`.
- The Tokio runtime blocking-pool guardrail was described on the PyO3 wrapper
  instead of as language-neutral runtime construction. The spec now requires a
  Rust-core server runtime builder reused by PyO3 and future SDKs.

The eleventh strict review cycle found and addressed this plan gap:

- The close journal required every transaction to create a record but did not
  define how synchronously returned outcomes avoid later replay. The spec now
  requires close generations, marks RuntimeSession-returned records consumed
  before return, and requires Python hook dedupe by route close generation.

The twelfth strict review cycle found and addressed this plan gap:

- Close failure handling required an error reason but `RouteCloseOutcome` only
  listed `closed_reason`. The spec now adds `close_error` so runtime/scheduling
  failures are mechanically distinguishable from normal close reasons.

The thirteenth strict review cycle found and addressed these plan gaps:

- The source-to-sink matrix still did not show relay-registration rollback in the
  close wire/source path, and the hook row did not include close generations or
  journal replay. The matrix now includes rollback outcomes and stale-journal
  guardrails.
- Registration transaction wording used "preferred shape", which allowed a
  weaker implementation. The spec now requires a pending route token or
  equivalent non-routable reservation before relay publication.

The fourteenth strict review cycle found and addressed this plan gap:

- Registration rollback was in the close journal path but not explicitly excluded
  from SDK shutdown-hook replay. The spec now requires rollback records to be
  consumed with the registration error and marks `registration_rollback`
  outcomes as never hook-eligible.
