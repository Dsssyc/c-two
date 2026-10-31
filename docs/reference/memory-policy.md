# C-Two transport memory policy

This page defines IPC memory budgets, allocation ownership, transport fallback and retained-payload accounting.

The Rust config resolver supplies defaults and validates overrides; Rust owns budget accounting, lazy allocation and tiered fallback, while SDKs expose typed overrides and read-only statistics. See the [configuration guide](configuration.md) for the full option reference.

On Windows, native pipe session facts and negotiated directional capabilities govern shared references. Different sessions use the existing owned/inline/chunked path, including copies of explicitly prepared SHM payloads with release through their original owner. The configured buddy-pool switch does not decide this reachability; reassembly may still use its own local memory pool.

## Finite budget cells

Both server and client IPC overrides accept these independent byte budgets:

| Override | Native default | Charged storage |
| --- | --- | --- |
| `shm_backing_budget_bytes` | 8 GiB | Locally created buddy and dedicated SHM mappings, including headers and alignment |
| `file_backing_budget_bytes` | 16 GiB | Locally created file backing length |
| `live_reassembly_budget_bytes` | 8 GiB | Reassembly capacity retained by incomplete, queued, executing or held payloads |

Zero rejects every positive reservation. The entire `u64` range is valid; zero does not mean unlimited. Per-message `max_payload_size` and `max_reassembly_bytes` limits apply independently; `max_total_chunks` applies to chunks in flight across requests, and `max_pool_segments` limits pool segments. Budgets cover C-Two-owned IPC backing and assembly, without bounding Python/FastDB heaps, HTTP buffers, receiver-opened peer mappings or total process RSS.

Server response pools, reassembly and prewarming share one server-direction budget. Cached request and reassembly pools in one Runtime's outgoing client domain share another. Directions and separate Runtimes are independent. Reservation occurs atomically before backing creation: refusal creates no mapping or file. Reusing free blocks in charged backing does not charge it again. Dedicated storage remains charged while waiting for peer `read_done` or owner GC.

Finite-deadline calls have separate admission limits, `C2_CALL_MAX_OUTSTANDING` and `C2_CALL_RETAINED_INPUT_BUDGET_BYTES`. These account for retained request input and continuing native transactions after caller timeout. Unlimited calls are outside that finite-call admission domain; these settings are separate from the three IPC budgets above.

## Lazy allocation and reclamation

`pool_prewarm_segments=0` is the default. Registering or connecting creates no buddy mapping until a large-buffer allocation needs it. Explicit prewarming maps the selected number of segments immediately.

IPC uses `pool_min_retained_segments=0`, allowing idle buddy segments to retire after `pool_decay_seconds` (60 seconds for both server and client). The pool can return to zero mappings while preserving generation counters and live allocations. Direct native `MemPool` defaults retain one segment; the SDK IPC projection selects zero.

Dedicated SHM has a 64-byte header containing an atomic `read_done` flag. After the reader finishes with a dedicated segment, it marks this flag and can unmap its view; the creator keeps its segment and budget charge after local free until `gc_dedicated()` observes the flag. `dedicated_crash_timeout_secs` is a fallback for a peer that never marks completion, not the normal recycling clock. An uncertain published reply can retain its creator-side charge under the separate response ownership rule below.

New buddy or dedicated backing also passes an OS memory-pressure heuristic. The IPC projection uses `spill_threshold=0.8`: a candidate's total mapped bytes exceeding that fraction of observed available memory is refused. This is not an IPC override key. In direct native use, a finite value at least 1 disables the heuristic; nonpositive or non-finite values send new mapping attempts to file backing or an explicit error. Reuse within existing backing remains available without a new reservation.

On Linux, overcommit can let mapping creation succeed while physical pages are committed only on write, so a successful `mmap` is not evidence that a later payload write has enough memory. The native pressure check therefore runs at new buddy and dedicated backing creation using the mapped size; it is a heuristic alongside finite C-Two budgets, not a process RSS or future-write guarantee. It leaves existing backing reuse available, can admit a smaller dedicated mapping after refusing a full buddy segment, and permits file backing where that allocation path supports it. When pressure appears, first lower concurrent or individual payload demand, inspect `cc.observe.memory_stats()` for backing, reassembly and retained-owner charges, and adjust the relevant typed IPC budget or pool size/count only after identifying the constrained domain. Increase reassembly capacity only when receivers are expected to accept larger complete payloads.

## Allocation and transport fallback

Client requests choose inline when request-pool SHM is not selected and the payload fits `chunk_size`; larger requests use checked chunks. SHM selection depends on `shm_threshold` (4 KiB by default) and wire representation limits. SHM requests write into the local request pool and transmit the 23-byte `BuddyPayload` coordinates, generation and allocation id; the peer lazily opens the exact prefix, index and generation. If SHM is unavailable because of budget, pressure or representation limits, small requests fall back to inline and larger requests use checked chunks. The default chunk size is 128 KiB. Payload and reassembly sizes bound each request; the chunk-count limit bounds chunks in flight across requests.

Server responses up to `shm_threshold` use inline frames. Larger responses try buddy reuse, buddy expansion and dedicated SHM before checked chunk fallback. Payloads that cannot fit buddy wire metadata also use the checked fallback.

SHM header version 3 records each buddy allocation's level and non-wrapping id under the existing allocator lock. Release must match that live record before changing free bits or counters, rejecting parent, child and stale-id releases. Logical sizes still round up to the allocation level, and trimming preserves the original release capacity. Records add 8 bytes per minimum block: the default 256 MiB / 4 KiB backing and its creation budget grow by 512 KiB. IPC 12 and C/FFI ABI 5 introduced this allocation-id representation. 0.9.x uses IPC 13 and C/FFI ABI 6 with SHM header 3. 0.10.0 uses the [IPC 14 fixed preamble](ipc-handshake.md), retaining ABI 6 and SHM header 3; retired handshakes and ABIs are rejected.

Receiver assembly reserves full capacity, `total_chunks × chunk_size`, before accepting the storage. Allocation tries buddy reuse, reuse after GC, buddy expansion, dedicated SHM and finally file mmap. Completion may trim logical length while the capacity charge remains until storage release. Resources receive the complete assembled input; chunk transfer does not provide incremental resource input.

Low-level explicit Rust release retains authority on failure. `MemPool::release_handle` returns `ReleaseHandleError` containing the original `handle` and `reason`; `RequestLease::release` and `ResponseLease::release` clear their carrier only after success. Never discard a `pool_free` failure. A caller may retry after contention ends; retry does not repair poison, stale identity or a wrong pool. Reassembly reservations are refunded only after storage release; cached mapping charges still follow the existing GC policy.

The ownership rules for consuming calls, Core/SDK Held and final Drop after an unconfirmed release are described below. These paths do not replay a business call.

FastDB content remains opaque at this layer. C-Two transports bytes or SHM coordinates and delegates nested payload specifications to FastDB Core.

`MemHandle` is the private, move-only owner for buddy SHM, dedicated SHM and file-spill allocations. SHM authority is tied to a non-reused process-local pool identity; `PoolAllocation` and wire coordinates contain metadata, not ownership. Raw copy, free and dedicated-mapping adoption require explicit `unsafe` ownership and lifetime contracts; a raw pointer query alone grants no safe memory access. Transport callers use `alloc_shm_handle()`, which keeps `alloc()`'s GC and error behavior. A file-spill owner closes its mapping before its backing file, including Windows delete-on-close cleanup.

Request and response SHM carriers are created at frame admission and bind the pool that actually allocated them; prepared SHM replies retain their allocated owner. A reconnect gives its receive task and replies a fresh pool container while retained replies keep the old one. Do not expose replaceable receive-pool slots or safe coordinate-to-lease constructors. Request SHM blocks keep their single Armed/Dispatched/Released transaction. Server callbacks receive a fixed `ResponsePool` owner; normal response preparation uses `try_prepare_shm_response`, and raw mutable pool access requires `unsafe`. Read access returns a mapped guard; an ordinary read guard can reveal its original writable lock through `RwLockReadGuard::rwlock`.

`c2-mem` owns SDK-visible buffer lease accounting, which records metadata and retention state without reading payload bytes or allocating a second buffer. Lease accounting does not replace `MemPool::free_at()` or `release_handle()` as the release authority. Python export admission shares that owner boundary: `PyCoreResponse` checks exports under `inner`, while `PyCoreRequestBuffer` serializes export, release and retained tracking under `lease`. Active exports reject release before invalidation or lease removal; tracking after release cannot revive a lease. View-release counter decrements remain atomic, and Core retains snapshot and release authority.

Each owner pool adds a fresh PID/UUID incarnation to its logical label, and only `MemPool` derives bounded OS backing names; SDKs must not append prefix/index suffixes. Buddy references in handshake 14 carry a checked `u32` backing generation and `u64 allocation_id`. Receiver pools use `MemPool::open_peer()` to open the exact prefix/index/generation lazily; handshake segment names are descriptive metadata, with no separate segment announcement protocol. Dedicated descriptors use allocation id 0. A buddy release must match the original allocation id and level under the shared allocator lock, so an old release cannot free a new allocation at the same offset. A live allocation prevents backing retirement. The last peer free drops an idle cached view; owner GC observes remote frees and preserves slot generation counters across reclamation. Dedicated indices never wrap beyond the wire's `u16` range.

Shared allocator header version 3 keeps a non-wrapping allocation sequence and shared live records while retaining the full PID in its 64-bit lock word. Creation and attach use the same checked layout geometry; OS page-rounded trailing capacity does not enlarge the logical data region. Death observers refuse access without changing that PID-only lock word, and a panicking holder poisons its own backing. Never steal a dead holder's lock or infer retirement from separately sampled lock and allocation counts. Retirement checks the allocation count under the same SHM lock with one nonblocking acquisition attempt.

Python no longer exports manual `c_two.mem.ChunkAssembler` or `MemHandle` wrappers or their `_native` names. Automatic IPC chunking remains in Rust's `ChunkRegistry` / `ChunkAssembler` / `ReassemblyBacking` chain, while Rust memory-pool APIs remain available. Do not restore Python consumed/released wrappers or aliases for the retired manual entry points.

Fallback uses the existing IPC inline/chunk and SHM paths; do not introduce a third transport stack. Capacity-limit failures propagate through the transport layer’s existing error paths; do not add a retry queue for refused allocations or abandoned releases.

## Disabling buddy

`pool_enabled=False` skips buddy reuse and expansion, including cached segments. Dedicated SHM, inline, chunks and file backing remain available. Receiver assembly also uses dedicated SHM and file fallback. Dedicated mappings still consume the SHM backing budget.

`pool_enabled` is accepted by `BaseIPCOverrides`, `ServerIPCOverrides` and `ClientIPCOverrides`. This minimal configuration disables only buddy, with dedicated SHM and the existing fallback paths still available:

```python
import c_two as cc

# Configure each side before its first registration or connection.
cc.set_server(ipc_overrides={'pool_enabled': False})
cc.set_client(ipc_overrides={'pool_enabled': False})
```

`shm_threshold` selects the inline/SHM boundary and does not disable every SHM path. The native parser rejects `shm_threshold` in `ipc_overrides`; configure it through `cc.set_transport_policy(shm_threshold=...)`. Raising the threshold can move sender traffic to inline or chunks while receiver assembly still needs backing.

Disabled buddy requires `pool_prewarm_segments=0`. `pool_min_retained_segments` cannot exceed `max_pool_segments` or `reassembly_max_segments`. Native parsing rejects invalid combinations.

Pool segment capacity and `max_payload_size` are independent for enabled and disabled buddy. A segment can hold several smaller messages. Positive size, representable indices, bounded segment counts and checked arithmetic remain required; requests and responses exceeding the payload limit are rejected regardless of available segment capacity.

```python
import c_two as cc

cc.set_server(ipc_overrides={
    'pool_enabled': False,
    'max_payload_size': 32 * 1024 * 1024,
})
cc.set_client(ipc_overrides={
    'pool_enabled': False,
    'max_payload_size': 32 * 1024 * 1024,
})
```

## Setting budgets

Call `cc.set_server()` before registration and `cc.set_client()` before connection; server and client use independent budget domains, so configure both when both directions need the limits.

```python
import c_two as cc

BUDGET = {
    'shm_backing_budget_bytes': 2 * 1024**3,
    'file_backing_budget_bytes': 4 * 1024**3,
    'live_reassembly_budget_bytes': 1024**3,
}
cc.set_server(ipc_overrides=dict(BUDGET))
cc.set_client(ipc_overrides=dict(BUDGET))
```

Equivalent environment variables are `C2_IPC_SHM_BACKING_BUDGET_BYTES`, `C2_IPC_FILE_BACKING_BUDGET_BYTES` and `C2_IPC_LIVE_REASSEMBLY_BUDGET_BYTES`. Code overrides take precedence over process environment, `.env` and native defaults. Outgoing client policy freezes on its first connection attempt, including failure; cache reuse requires equality of the complete resolved policy. Close the old Runtime and configure a new Runtime to select different limits. Older retained payloads remain charged to their original domain.

The finite defaults deliberately change the earlier aggregate limit and may need adjustment for workloads exceeding them; they do not guarantee that every scientific workload fits. Refusal happens before allocation and increments `rejected_allocations` / `rejected_bytes` in the relevant cell. Control frames such as ping and heartbeat allocate no data backing and consume none of these budget cells.

### Standalone relay

Relay resolves upstream IPC policy at startup through the same native resolver and stores it in `RelayConfig.upstream_ipc`. All data-plane upstream request and reassembly pools share the relay instance's budget, including reconnects. Configure the relay process through its CLI or environment.

```bash
c3 relay --bind 127.0.0.1:8080 \
  --ipc-pool-enabled false \
  --ipc-shm-backing-budget-bytes 134217728 \
  --ipc-file-backing-budget-bytes 268435456 \
  --ipc-live-reassembly-budget-bytes 134217728
```

These flags override `C2_IPC_POOL_ENABLED` and the three budget variables. Other client IPC settings, including prewarming, decay and `C2_SHM_THRESHOLD`, use the existing resolver. `c3 relay --dry-run` reports resolved policy. Disabled buddy still requires zero prewarming, and zero budgets still reject positive reservations.

This accounting covers relay-owned data-plane IPC backing and assembly. Registration proofs and control watches use separate lazy contexts. Peer mappings and HTTP buffers are outside these IPC budgets. HTTP response materialization can still consume the full response before batched sending. Relay forwarding admission separately bounds continuing transactions and retained input; see [relay configuration](configuration.md#relay-and-c3).

## Statistics and payload lifetime

`cc.observe.memory_stats()` returns read-only snapshots for `runtime_outgoing`, `server`, `retired` and `holds`. The outgoing domain freezes on its first connection attempt; observing it does not connect or freeze policy. `server` is `None` when no host exists. Retired domains have `state="retired"` and remain observable while an old proxy, in-flight call or held result retains a real accounting owner; this presentation is implementation behavior rather than a stable shape guarantee. Each domain reports `role`, `state`, three resolved `limits`, and `shm` / `file` / `reassembly` cells with `limit_bytes`, `used_bytes`, `peak_bytes`, `rejected_allocations` and `rejected_bytes`; peaks retain their high-water mark after release. Each domain also reports `abandoned_releases: int` beside `limits` and `cells`. Rust `BudgetSnapshot` and `MemoryScopeStats` expose the same saturating `u64`. `holds` reports `active_holds`, `total_held_bytes`, `oldest_hold_seconds` and `by_storage` for retained inline, SHM, handle and file-backed leases. These counters are separate charges for C-Two-owned IPC backing and live reassembly capacity: summing cells does not measure physical memory or prove zero-copy transport.

`cc.hold(proxy.method)(args)` returns a `HeldResult` with `.value`, `.unsafe_buffer` and `.release()`. Use explicit release or a context manager; `__del__` is the fallback. For a portable method, `.value` is a FastDB `Payload` owner. Release invalidates that owner and its checked views before freeing the transport lease. Materialize values through FastDB before keeping them beyond that lease. `unsafe_buffer` permits raw views whose exported pointers or NumPy aliases cannot be mechanically invalidated. See [Python payload usage](python-usage.md).

Explicit native cleanup failure keeps the original lease, storage charge and concrete connection with its holder. Calling `HeldResult.release()` again retries native cleanup; the FastDB invalidator is marked attempted before entry and runs at most once, including when it raises or Rust unwinds. After invalidation starts, `.value` and `.unsafe_buffer` stay inaccessible. An active export rejects native release before invalidation; release the extra view and retry. A view already released during that attempt is never restored. Manually supplied `HeldResult(release_cb=...)` callbacks remain once-only user callbacks. The primary error and this attempt's cleanup cause remain in the existing exception chain on Python 3.10.

A consuming exit or final carrier destructor cannot return cleanup responsibility. On failure it reports or logs the original cause, leaves unconfirmed allocator state untouched, keeps unreturned byte charges, and records one `abandoned_releases` event at the lowest carrier. There is no recovery worker, timer or error history. An explicit failed release still owned by its holder does not increment `abandoned_releases`; a successful transfer of release responsibility to the receiver does not increment it either. The counter records this process giving up an obligation; a receiver may subsequently free the same published allocation. An abnormal same-user SHM peer may also assert invalid, replayed or stale allocation coordinates. Unreleaseable asserted references count as abandoned obligations, so the counter can exceed the number of allocations actually left unreleased. It is neither a live leak count nor a guarantee that zero means all memory is healthy, and it never decreases after later cleanup. Counts do not keep retired scopes alive or survive process exit.

Prepared SHM replies transfer buddy release authority as soon as the complete frame is written, before native flush. A later flush failure, deadline or cancellation returns/logs its error and counts one uncertain outcome without freeing peer-visible storage. Dedicated replies keep their existing lifecycle on success; an uncertain published dedicated backing remains charged without setting `freed_at` and may remain until pool teardown even if `read_done` later arrives. Fill errors, panic, cancellation while waiting for the writer and failure before a complete frame use the original pool for cleanup; an inline/chunk fallback cannot hide their original failure. Poisoned or dead-holder segments stay unavailable for allocation and release. `Contended` only means the spin budget was exhausted; it proves neither that the holder is alive nor that later recovery will succeed. No path resets or steals the shared lock.

`cc.shutdown()` does not force-release retained payloads or erase their accounting. Charges and retained leases remain in retired domains. A zero charge and the removal of an observation record can occur at different times; the weak observation disappears after the last actual owner is gone. Statistics do not keep pools, connections, Runtimes, callbacks or payloads alive.

## Implementation references

- [Native configuration](../../core/foundation/c2-config/src/memory.rs) and [IPC validation](../../core/foundation/c2-config/src/ipc.rs).
- [Budget reservation](../../core/foundation/c2-mem/src/budget.rs), [pool allocation](../../core/foundation/c2-mem/src/pool.rs), [memory pressure](../../core/foundation/c2-mem/src/pressure.rs), [dedicated backing](../../core/foundation/c2-mem/src/dedicated.rs), [buddy segments](../../core/foundation/c2-mem/src/buddy_segment.rs) and [file spill](../../core/foundation/c2-mem/src/spill.rs).
- [Request transport](../../core/transport/c2-ipc/src/client.rs), [server responses](../../core/transport/c2-server/src/server.rs), [reassembly](../../core/protocol/c2-wire/src/assembler.rs) and [chunk registry](../../core/protocol/c2-wire/src/chunk/registry.rs).
- [Runtime observations](../../core/runtime/c2-core/src/memory.rs), [native session projection](../../sdk/python/native/src/runtime_session_ffi.rs), [Python IPC overrides](../../sdk/python/src/c_two/config/ipc.py), [native override parsing](../../sdk/python/native/src/config_ffi.rs), [settings](../../sdk/python/src/c_two/config/settings.py) and [HeldResult](../../sdk/python/src/c_two/crm/transferable.py).
- [Memory budget contract](../acceptance/memory-budget-contract.md), [memory policy plan](../acceptance/2026-09-26-memory-policy.md), [native memory validation](../acceptance/memory-native-final-validation.md) and [canonical endpoint validation](../acceptance/canonical-local-endpoint-validation.md) retain their original scopes.

The [0.10.0 release notes](../releases/0.10.0.md) describe failed-release ownership and `abandoned_releases` accounting; the [0.8.0 release notes](../releases/0.8.0.md) introduced the ownership, chunk-admission and response-path model they build on. The [0.7.4 publication record](../acceptance/0.7.4-publication.md) retains that release's source pair, platforms and public-artifact verification. Historical validation reports retain their original source pairs. Chunking and hold semantics do not establish incremental resource input, direct FastDB response construction, zero-copy decoding or a measured performance gain.
