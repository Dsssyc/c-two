# Direct IPC Shutdown Two-Phase Design

**Date:** 2026-05-14
**Status:** Normative design correction; implementation pending
**Scope:** Direct IPC admin shutdown through `c2-wire`, `c2-ipc`,
`c2-server`, `c2-runtime`, PyO3, and thin SDK facades.

## Context

Direct IPC `shutdown("ipc://...")` is a same-host admin/control-plane helper. It
is not a normal CRM method and it is not top-level `cc.shutdown()` for the
current process. The helper currently conflates two separate concerns:

- the control RPC deadline for sending a shutdown signal and receiving an
  acknowledgement;
- the server-side graceful drain period for active resource callbacks and
  route-close outcomes.

That coupling creates a fragile budget split. For example, a caller timeout of
`500ms` can allow the server to wait almost the entire timeout for drain and
leave too little time for the acknowledgement frame to return to the caller.
Under load this can report `acknowledged=false` even when the server accepted the
shutdown signal.

The durable design must match the runtime boundary: Rust core owns direct IPC
control semantics; Python and future C++/Go/Fortran/Java SDKs expose thin
facades only.

## External References

The external graceful-shutdown APIs below are used only to justify the lifecycle
shape: initiate orderly shutdown, reject new work, let in-flight work complete,
and make completion observation explicit.

- gRPC graceful shutdown guide: initiate graceful shutdown, reject new RPCs, let
  in-flight RPCs complete, and use a separate forceful mechanism if a deadline
  expires.
  https://grpc.io/docs/guides/server-graceful-stop/
- gRPC Go: `GracefulStop()` blocks until pending RPCs finish, while `Stop()` is
  the forceful safety net.
  https://pkg.go.dev/google.golang.org/grpc/examples/features/gracefulstop
- gRPC Python: `Server.stop(grace)` separates graceful shutdown from forceful
  termination and applies grace as a shutdown policy, not as a transport ack
  reserve.
  https://grpc.github.io/grpc/python/grpc.html#grpc.Server.stop
- gRPC Java: `shutdown()` starts orderly shutdown; `awaitTermination(timeout,
  unit)` is the explicit wait step; `shutdownNow()` is the forceful path.
  https://javadoc.io/static/io.grpc/grpc-api/1.67.1/io/grpc/Server.html
- Go `net/http`: `Server.Shutdown(ctx)` uses the caller's context deadline as
  the wait boundary and reports context expiry explicitly.
  https://pkg.go.dev/net/http#Server.Shutdown

C-Two direct IPC `shutdown(address, timeout)` maps to the initiate step, not to
`awaitTermination()` and not to a combined "initiate plus drain" operation. The
completion wait belongs to a runtime/bridge barrier. A cross-process,
route-outcome observe API is intentionally outside this correction.

## Decision

Direct IPC shutdown is a two-phase lifecycle operation:

1. **Initiate shutdown.** The control helper sends a `ShutdownClient` signal and
   waits only for a fast, structured acknowledgement that the server has entered
   shutdown/draining. The helper must not wait for active CRM callbacks to drain.
2. **Observe completion.** A separate runtime barrier observes that the server
   has stopped and consumes the native close journal that carries route close
   outcomes.

The public `timeout` on the direct IPC control helper means **acknowledgement
timeout only**. It does not mean "server drain budget" and it must not be split
into a server-side wait budget plus a remaining transport margin.

Do not expose `shutdown_wait_budget`, `ack_reserve`, or any equivalent low-level
transport tuning knob. Those are implementation details and would force each SDK
to learn Rust IPC internals.

Current implementation scope is:

- direct IPC initiate acknowledgement for all SDKs through Rust-owned control
  semantics;
- owner-process completion observation through `RuntimeSession::shutdown()` /
  bridge barriers;
- no external route-outcome observe helper in this correction.

An external same-host supervisor that needs only process-level proof may poll
until the socket is gone and `ping()` fails. An external supervisor that needs
route close outcomes or hook-safety proof needs a future authenticated admin
observe surface; that API must not be smuggled into the initiate helper and is
not part of this correction.

## Frozen Invariants

| Invariant | Required property | Regression prevented |
| --- | --- | --- |
| I1 ack-only initiate | Direct IPC `shutdown()` timeout bounds only signal send and acknowledgement receive. | False negative acknowledgement under load; hidden server drain budgets. |
| I2 no route outcomes in initiate | Live-server initiate ack never carries hook-safe route outcomes. | SDKs treating `acknowledged=true` as proof that active callbacks drained. |
| I3 single native close transaction | Accepted direct IPC shutdown enters the same server close transaction and journal path as runtime shutdown. | Queued work, active permits, or hooks escaping lifecycle policy. |
| I4 exactly-once journal consumption | Route close outcomes are stored with one shutdown generation and consumed once by the runtime/bridge observe path. | Duplicate hooks, stale hook replay, or lost close outcomes. |
| I5 SDK-neutral contract | Rust core defines the wire and outcome contract; SDKs expose only thin typed facades. | Python-only behavior that C++/Go/Fortran/Java would need to reimplement. |
| I6 mechanical guardrails | Tests/source guards fail if wait-budget payloads, bool/unit lifecycle bypasses, or initiate-as-drain semantics return. | Half implementations and old side paths remaining callable. |

## Required Semantics

### Initiate: Direct IPC Control Helper

Rust API shape:

```rust
pub fn shutdown(address: &str, timeout: Duration) -> Result<DirectShutdownAck, IpcError>;
```

`DirectShutdownAck` is a required contract, not a suggested shape:

```text
DirectShutdownAck:
  acknowledged: bool
  shutdown_started: bool
  server_stopped: bool
  route_outcomes: Vec<ShutdownControlRouteOutcome>
```

Field semantics:

- `acknowledged`: the caller received a syntactically valid shutdown
  acknowledgement from the peer, or the socket was already absent.
- `shutdown_started`: the live peer accepted the shutdown command and entered
  the draining lifecycle. It is false for missing sockets and communication
  failures.
- `server_stopped`: true only when the helper can prove the addressed server is
  already stopped without observing drain. In this design that is limited to the
  missing-socket idempotent case.
- `route_outcomes`: always empty in initiate acknowledgements. Route close
  outcomes belong to the observe phase.

Live server accepted response:

```text
acknowledged=true
shutdown_started=true
server_stopped=false
route_outcomes=[]
```

Absent socket response:

```text
acknowledged=true
shutdown_started=false
server_stopped=true
route_outcomes=[]
```

Communication failure, malformed response, or acknowledgement timeout:

```text
acknowledged=false
shutdown_started=false
server_stopped=false
route_outcomes=[]
```

Invalid IPC addresses return `Err(IpcError::Config(_))` from the Rust core. All
language SDKs must preserve the semantic category "client-side IPC address
configuration error". Each SDK may map that category to its language-idiomatic
error surface, but must document and test the mapping, and must not construct
socket paths or validate IPC region syntax outside Rust.

### Wire Contract

The `ShutdownClient` request is an initiate command only. Its payload carries no
duration, grace value, worker budget, route list, or route outcome request.

The required wire helpers are:

```rust
pub fn encode_shutdown_initiate() -> Vec<u8>;
pub fn decode_shutdown_initiate(payload: &[u8]) -> Result<(), String>;
pub fn encode_shutdown_ack(outcome: &DirectShutdownAck) -> Result<Vec<u8>, String>;
pub fn decode_shutdown_ack(payload: &[u8]) -> Result<DirectShutdownAck, String>;
```

`encode_shutdown_initiate()` returns exactly the single-byte `ShutdownClient`
discriminant as the complete initiate payload. That single byte is the canonical
initiate request, not a legacy incomplete request. `decode_shutdown_initiate()`
accepts exactly that single byte and rejects request payloads with appended
duration, grace, route, or wait-budget fields. `ShutdownAck` remains structured;
the single-byte `ShutdownAck` discriminant is not a complete acknowledgement.
The old wait-budget request format is removed and must not remain accepted as a
fallback.

### Server: Draining Lifecycle

After accepting `ShutdownClient`, `c2-server` must:

- stop accepting new connections and new CRM calls;
- close route admission through the same close transaction used by
  `RuntimeSession` / `NativeServerBridge` shutdown;
- cancel queued not-started work through the scheduler cleanup path;
- keep active callbacks owned by their native permits until they finish;
- record route outcomes in the server close journal for later bridge/runtime
  reconciliation.

The control handler must write the live accepted acknowledgement before waiting
for drain:

```text
acknowledged=true
shutdown_started=true
server_stopped=false
route_outcomes=[]
```

Python `@on_shutdown` hooks run only in the observe phase after the native route
outcome reports `active_drained=true`.

### Observe: Runtime And Bridge Barrier

Completion observation is explicit and owner-process scoped in this correction:

- same-process Python bridge cleanup uses `NativeServerBridge.shutdown()` /
  `RuntimeSession::shutdown()` to wait on the native runtime barrier and consume
  close-journal outcomes;
- future SDK server handles expose an outcome-returning wait/barrier API instead
  of bool/unit lifecycle methods;
- an external direct IPC supervisor that needs route outcomes must wait for a
  future explicit admin observe API. Polling socket disappearance is allowed only
  as process-level liveness evidence, not as route-drain or hook-safety proof.

The observation phase is where a caller may choose a drain deadline or forceful
shutdown policy in a future admin design. This correction does not add a
forceful kill/abort API. Runtime or bridge barrier timeouts must report explicit
undrained/error outcomes; they must not terminate active callbacks behind the
scheduler's ownership model. Any future forceful admin path requires a separate
authenticated design and must not reuse the initiate helper as a hidden force
switch.

### Shutdown Generation Contract

The server owns a monotonic in-memory `shutdown_generation` for the current
server instance.

- Producer: the first valid initiate or owner-process shutdown transition from
  `Ready` to `Draining` increments or allocates the active generation.
- Storage: route close journal records store the generation with route name,
  route identity, closed reason, active drain result, and any close error.
- Duplicate initiate: a valid initiate received while the server is already
  draining returns the same live accepted acknowledgement and does not allocate a
  new generation.
- Consumer: `RuntimeSession::shutdown()` / bridge observe drains records for the
  active generation once and marks that generation consumed before invoking any
  SDK hook.
- Replay protection: a consumed generation cannot trigger hooks again, even if a
  later bridge cleanup repeats.
- Restart boundary: a successful new server start clears stale shutdown journals
  and begins with no consumed hook eligibility from the previous server
  instance.
- Failure boundary: if the runtime barrier times out or fails, the observe
  outcome reports undrained/error state for that generation; hooks remain
  suppressed for undrained routes.

### Lifecycle And Journal State Table

| State / event | Initiate response | Close transaction | Journal behavior | Hook behavior |
| --- | --- | --- | --- | --- |
| Missing socket | `acknowledged=true`, `shutdown_started=false`, `server_stopped=true` | None | None | None |
| Ready server receives first valid initiate | `acknowledged=true`, `shutdown_started=true`, `server_stopped=false` | Start one shutdown generation and close admission | Outcomes recorded after drain | Not run from initiate ack |
| Draining server receives duplicate initiate | Same live accepted ack | Reuse existing shutdown generation and do not rebroadcast the shutdown watch signal | No duplicate journal record | No duplicate hook |
| Malformed initiate payload | Non-ack outcome | None | None | None |
| Drain completes | No initiate response change | Server reaches stopped lifecycle | Route outcomes become observable once | Hooks eligible only for `active_drained=true` |
| Bridge observes before drain completes | Not applicable | Waits for native barrier | Consumes generation after stopped | Hooks run once after eligible outcomes |
| Bridge observes after generation consumed | Not applicable | Idempotent terminal barrier | No replay | No hook replay |
| Runtime barrier times out or fails | Not applicable | Observe outcome carries explicit barrier error | Undrained routes remain not hook-safe | Hooks suppressed for undrained routes |

## Source-To-Sink Matrix

| Invariant | Producer | Storage | Consumer | Network/Wire | FFI/SDK | Test/Support | Legacy/Bypass guard |
| --- | --- | --- | --- | --- | --- | --- | --- |
| I1 ack-only initiate | `c2-ipc::control::shutdown()` | None | `c2-server::handle_shutdown_signal()` | IPC signal frame carries exactly one `ShutdownClient` byte as initiate | PyO3/Python pass timeout only as ack deadline | Blocked-callback behavior test plus source/order guard prove ack is written before drain wait | Source guard rejects `shutdown_server_wait_budget`, `wait_budget`, and duration fields in shutdown request path |
| I2 no route outcomes in initiate | `handle_shutdown_signal()` | Immediate ack payload | direct IPC control caller | `ShutdownAck` for initiate has empty `route_outcomes` | SDK exposes `route_outcomes=[]` for initiate | Tests assert live accepted ack has empty route outcomes | Guard rejects tests/docs that treat initiate ack as hook-safe |
| I3 single close transaction | `Server::shutdown_and_wait()` or internal direct IPC signal handler | scheduler closed state, active permits, close journal | runtime/bridge observe via outcome-owned APIs | No relay/CRM wire involvement | No SDK-owned scheduler/close fallback | queued/active shutdown tests cover same path | raw signal-only lifecycle API and raw journal drain are private |
| I4 exactly-once journal | native close transaction | per-generation shutdown journal | `RuntimeSession::shutdown()` / bridge | not returned by initiate wire | Python hook gate consumes outcomes once | hook once and journal replay tests | source guard forbids stale removed-route hook decisions |
| I5 SDK-neutral contract | `c2-wire` / Rust core structs | typed native outcome | all language SDK facades | single canonical codec | Python mirrors required fields; future SDKs do the same | PyO3 dict and SDK facade tests check field completeness | no Python-only constants, path derivation, or fallback parsing |
| I6 guardrails | source tests and behavior tests | CI | maintainers/agents | scans wire/control/server paths | scans PyO3/Python public surfaces | target Rust and Python tests | no wait-budget payload, no bool/unit close bypass, no old generic signal ack path, and no post-shutdown large/body-wait frame path for non-shutdown traffic |

## Error Ownership

Initiate phase reports only address validation and communication-level state:

- invalid IPC address: Rust `Err(IpcError::Config(_))`;
- missing socket: acknowledged already-stopped response;
- timeout/malformed/disconnect before ack: non-ack response;
- accepted live ack: shutdown started, completion not observed.

Observe phase reports lifecycle and drain state:

- active route drain success/failure;
- queued cancellation;
- runtime barrier timeout or failure;
- hook eligibility;
- one-shot journal consumption.

Drain or hook failures must not be squeezed into the initiate ack. If the server
accepts shutdown and later fails to drain or observe cleanly, that is an observe
outcome, not an acknowledgement failure.

## Non-Goals

- Do not add an environment variable for direct IPC shutdown wait budget.
- Do not keep a Python-owned scheduler, close, or hook-safety fallback.
- Do not route direct IPC shutdown through relay.
- Do not make the initiate acknowledgement carry hook-safe route outcomes.
- Do not treat `acknowledged=true` as proof that routes drained.
- Do not add external route-outcome observation in this correction. That requires
  a separate authenticated admin design.
- Do not add a forceful direct IPC kill/abort API in this correction.

## Implementation Plan

1. Update `c2-wire` shutdown control request encoding to
   `encode_shutdown_initiate()` / `decode_shutdown_initiate()` with no duration
   payload. Remove wait-budget request encoding and decoding entirely.
2. Add or rename the Rust shutdown acknowledgement type to the required
   `DirectShutdownAck` contract with `shutdown_started` semantics, then
   propagate the field through `c2-ipc`, `c2-server`, PyO3, Python, and future
   SDK contract docs.
3. Update `c2-ipc::control::shutdown()` to send the initiate command and wait
   only for acknowledgement within the caller timeout.
4. Update `c2-server::handle_shutdown_signal()` to enter the server close
   transaction, return the live accepted acknowledgement immediately, and let the
   normal server lifecycle produce close-journal outcomes after drain.
5. Make raw signal-only lifecycle helpers and raw journal drains private. Public
   server lifecycle entries must be outcome-owned, such as
   `shutdown_and_wait()` for the owner initiating shutdown and
   `observe_external_shutdown_outcomes()` for a runtime/bridge reconciling an
   already initiated direct IPC shutdown.
6. Update `c2-runtime::RuntimeSession::shutdown()` and bridge reconciliation so
   external direct IPC shutdown followed by bridge shutdown consumes the close
   journal exactly once and runs hooks only after drain.
7. Replace tests that expect full route outcomes from direct `shutdown()` with
   tests that expect immediate accepted acknowledgement while an active callback
   is still blocked.
8. Add source and behavior guardrails that reject reintroducing a server-side
   wait budget, bool/unit lifecycle bypass, Python-owned parsing, or
   initiate-as-drain semantics.
9. Sweep active docs that mention direct IPC shutdown wire semantics so they
   either point to this design or mark older wait-budget/single-byte-rejected
   language as superseded.

Known active text audited during this design correction. These files must remain
aligned with this design when the implementation lands:

| File | Alignment requirement |
| --- | --- |
| `docs/plans/2026-05-04-direct-ipc-control-helpers-rust.md` | `shutdown_started` is required; direct `shutdown()` returns a `DirectShutdownAck`-compatible dict; older bool/route-outcome initiate snippets are superseded. |
| `docs/plans/2026-03-29-wire-v1-removal.md` | Single-byte `ShutdownClient` is the complete initiate request; `ShutdownAck` remains structured. |
| `docs/superpowers/plans/2025-07-25-heartbeat-detection.md` | Python must not expose shutdown payload constants; Rust owns the codecs and the same canonical initiate rule. |
| `docs/superpowers/specs/2026-05-12-server-execution-backpressure-design.md` | I16 uses `DirectShutdownAck`; initiate acknowledgement is structured and never a drain result. |

## Acceptance Criteria

- Production code does not contain `shutdown_server_wait_budget`,
  `decode_shutdown_request_wait_budget`, or any `ShutdownClient` request payload
  that carries a server drain duration.
- `DirectShutdownAck` contains `acknowledged`, `shutdown_started`,
  `server_stopped`, and `route_outcomes` at the Rust, PyO3, and Python facade
  boundaries.
- A live accepted direct IPC initiate ack is exactly
  `acknowledged=true`, `shutdown_started=true`, `server_stopped=false`, and
  `route_outcomes=[]`.
- Direct IPC `shutdown(address, timeout=0.5)` returns the live accepted
  acknowledgement while a resource callback remains blocked. A behavior test must
  prove the acknowledgement is observable before releasing the blocked callback,
  and a source/order guard or Rust unit test must prove the handler writes the
  ack before awaiting drain completion.
- The same blocked callback prevents `NativeServerBridge.shutdown()` /
  `RuntimeSession::shutdown()` from completing until it is released.
- `@on_shutdown` runs exactly once and only after active callbacks drain.
- Duplicate direct IPC shutdown during draining does not create a second close
  journal generation, does not rebroadcast shutdown-watch wakeups, and does not
  trigger duplicate hooks. A connection accepted after shutdown has already been
  requested must still read a duplicate `ShutdownClient` frame instead of closing
  only because its cloned shutdown watch receiver has an unseen current value.
- That duplicate-initiate exception is narrow: after shutdown is requested, a
  newly accepted connection may read only the canonical fixed-length shutdown
  initiate frame. Any non-shutdown frame must close before waiting for or
  allocating its declared body.
- Idle peers and partial fixed-length shutdown frames accepted after shutdown is
  already requested must not enter the active connection drain barrier. Duplicate
  initiate inspection is a bounded control-frame check only; it is not a
  graceful route-drain timeout or a user-tunable shutdown budget.
- Missing sockets remain idempotent acknowledged-stopped outcomes with
  `shutdown_started=false`.
- Invalid IPC addresses still fail through Rust validation; SDK facades do not
  construct socket paths or validate region syntax themselves.
- Explicit direct IPC remains relay-independent with no relay environment
  dependency.
- Source guards fail if raw public bool/unit lifecycle methods or signal-only
  shutdown bypasses reappear.
- Active design text must not describe direct IPC `shutdown()` as waiting for
  active CRM callbacks to drain before acknowledging the signal. Historical
  review notes may mention the superseded design only as rejected context.
