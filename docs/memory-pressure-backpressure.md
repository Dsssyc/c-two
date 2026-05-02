# IPC Memory Pressure And Backpressure

## Current Model

C-Two's active IPC stack lives in Rust:

- `core/foundation/c2-config` resolves defaults, environment inputs, and code-level overrides.
- `core/foundation/c2-mem` owns shared-memory pools and allocation policy.
- `core/protocol/c2-wire` handles chunking and reassembly for large payloads.
- `core/transport/c2-ipc` and `core/transport/c2-server` move requests over UDS plus SHM.

Python does not implement an IPC config resolver and does not expose memory-pool defaults. Python SDK code can only provide typed code-level overrides through `cc.set_server(ipc_overrides=...)`, `cc.set_client(ipc_overrides=...)`, and the global `cc.set_transport_policy(shm_threshold=...)`.

## Backpressure Boundaries

The runtime protects memory use at three points:

| Boundary | Mechanism |
| --- | --- |
| Config resolution | Rust validates sizes, counts, finite durations, and derived values before runtime use. |
| Pool allocation | The Rust memory pool enforces segment size and segment count limits. |
| Chunk reassembly | Chunk registries cap segment counts, total chunks, stale assembly time, and maximum reassembled bytes. |

`max_pool_memory` is not a user setting. It is derived from:

```text
pool_segment_size * max_pool_segments
```

Large payload handling is controlled by chunking and reassembly limits, not by the removed Python `MemoryPressureError` path.

## Failure Semantics

Current high-level callers should expect transport failures to surface through the active error types, such as `ResourceUnavailable`, `RegistryUnavailable`, or underlying native IPC errors depending on the call path. The old Python `rpc_v2` documents and `MemoryPressureError` examples are obsolete and should not be used as implementation guidance.

When memory pressure is observed in tests or production:

1. Lower request concurrency or payload size.
2. Increase `pool_segment_size` or `max_pool_segments` through typed IPC overrides.
3. Increase reassembly limits only when the receiving side is expected to accept larger chunked payloads.
4. Keep `shm_threshold` as a process-wide transport policy, set before creating servers or clients.

## Test Focus

Memory-pressure tests should exercise the current Rust-backed paths:

- config resolver rejection for invalid or non-finite values;
- pool allocation limits and release behavior;
- chunk assembly timeout and cleanup;
- large payload transfer across IPC and relay paths;
- failure propagation without process aborts or stale SHM leaks.
