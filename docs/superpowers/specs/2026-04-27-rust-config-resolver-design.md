# Rust Config Resolver Design

**Date:** 2026-04-27  
**Status:** Draft for review  
**Scope:** Make Rust `c2-config` the canonical owner of runtime environment parsing, defaults, override merging, and validation for cross-language SDK support.

## Problem

C-Two currently has multiple configuration authorities:

- Rust `c2-config` defines IPC, relay, and pool config structs and defaults.
- Python `c_two.config.settings` reads `.env` and `C2_*` variables through `pydantic-settings`.
- Python `c_two.config.ipc` duplicates IPC defaults, validation, env merging, clamping, and derived values.
- CLI loads `.env` itself and uses clap `env = ...` bindings for relay flags.
- Some runtime environment variables are read directly from Python or Rust helpers.

This makes the repository harder to evolve into a cross-language SDK platform. A future SDK would need to either duplicate Python's config semantics or call Rust defaults that comments currently say are not authoritative.

## Goal

Create a single Rust configuration resolver that owns:

- `.env` loading.
- Process environment parsing.
- Canonical runtime defaults.
- Code-level override merging.
- Derived values.
- Validation.
- Resolved config output for Rust, CLI, Python, and future SDKs.

The intended precedence is:

```text
code overrides > process environment variables > env file > Rust defaults
```

Process environment variables must override values loaded from `.env`. `C2_ENV_FILE=""` disables env-file loading.

## Non-Goals

- Do not move Python's ergonomic user APIs into Rust. Python still owns `cc.set_config()`, `cc.set_server()`, `cc.set_client()`, and `cc.set_relay()` as code-level override interfaces.
- Do not put CLI presentation variables such as `NO_COLOR`, `CLICOLOR`, and `CLICOLOR_FORCE` into shared runtime config.
- Do not make every environment read an implicit global side effect. The resolver should be explicit and testable.
- Do not preserve `C2_IPC_ADDRESS` as a runtime setting unless a current implementation path is found. It appears in `.env.example` as residue but current runtime server addresses are auto-generated or passed explicitly.

## Current Environment Variable Inventory

### Shared Runtime Variables

These should be owned by the Rust resolver.

| Variable | Current source | Current use | Resolver decision |
| --- | --- | --- | --- |
| `C2_ENV_FILE` | CLI, Python settings, tests, `.env.example` | Select env file; empty disables loading | Keep. Resolver controls env-file loading. |
| `C2_RELAY_ADDRESS` | Python settings/docs/examples/tests | Relay URL for Python registration and name resolution | Keep. SDK-level resolved runtime setting. |
| `C2_RELAY_BIND` | CLI clap, Python settings, `.env.example` | Relay server bind address | Keep. Relay server config. |
| `C2_RELAY_ID` | CLI clap, Python settings, `.env.example` | Stable relay mesh ID | Keep. Relay server config. |
| `C2_RELAY_ADVERTISE_URL` | CLI clap, Python settings, `.env.example` | Public relay URL | Keep. Relay server config. |
| `C2_RELAY_SEEDS` | CLI clap, Python settings, `.env.example` | Comma-separated relay mesh seeds | Keep. Relay server config. |
| `C2_RELAY_IDLE_TIMEOUT` | CLI clap, Python settings, `.env.example` | Relay upstream IPC idle timeout | Keep. Relay server config. Default must be unified with `RelayConfig`. |
| `C2_RELAY_ANTI_ENTROPY_INTERVAL` | Python settings, `.env.example`, docs | Relay mesh anti-entropy interval | Keep. Relay server config. Currently not wired through CLI. |
| `C2_RELAY_USE_PROXY` | Rust `c2_http`, Python registry, docs | Whether relay HTTP traffic honors system proxy vars | Keep. Shared HTTP client behavior. |
| `C2_SHM_THRESHOLD` | Python settings, `.env.example` | SHM vs inline transfer threshold | Keep. Shared IPC runtime setting. |
| `C2_IPC_POOL_ENABLED` | Python settings, `.env.example` | Enable SHM pool | Keep. IPC base config. |
| `C2_IPC_POOL_SEGMENT_SIZE` | Python settings/tests, `.env.example` | IPC pool segment size | Keep. IPC base config. |
| `C2_IPC_MAX_POOL_SEGMENTS` | Python settings, `.env.example` | IPC pool segment count | Keep. IPC base config. |
| `C2_IPC_MAX_POOL_MEMORY` | Python settings, `.env.example` | IPC pool memory cap | Remove from public config. Resolver derives `max_pool_memory = pool_segment_size * max_pool_segments` because Rust runtime behavior is controlled by segment size and segment count. |
| `C2_IPC_POOL_DECAY_SECONDS` | Python settings, `.env.example` | Server pool decay | Keep. Server IPC config. |
| `C2_IPC_MAX_FRAME_SIZE` | Python settings, `.env.example` | IPC frame cap | Keep. Server IPC config. |
| `C2_IPC_MAX_PAYLOAD_SIZE` | Python settings, `.env.example` | IPC payload cap | Keep. Server IPC config. |
| `C2_IPC_MAX_PENDING_REQUESTS` | Python settings, `.env.example` | Server pending request cap | Keep. Server IPC config. |
| `C2_IPC_HEARTBEAT_INTERVAL` | Python settings, `.env.example` | Server heartbeat interval | Keep. Server IPC config. |
| `C2_IPC_HEARTBEAT_TIMEOUT` | Python settings, `.env.example` | Server heartbeat timeout | Keep. Server IPC config. |
| `C2_IPC_MAX_TOTAL_CHUNKS` | Python settings, `.env.example` | Chunk registry cap | Keep. IPC base config. |
| `C2_IPC_CHUNK_GC_INTERVAL` | Python settings, `.env.example` | Chunk GC interval | Keep. IPC base config. |
| `C2_IPC_CHUNK_THRESHOLD_RATIO` | Python settings, `.env.example` | Chunking threshold ratio | Keep. IPC base config. |
| `C2_IPC_CHUNK_ASSEMBLER_TIMEOUT` | Python settings, `.env.example` | Chunk assembler timeout | Keep. IPC base config. |
| `C2_IPC_MAX_REASSEMBLY_BYTES` | Python settings, `.env.example` | Chunk reassembly byte cap | Keep for now. It is a logical safety limit, not just pool capacity, but its current total/per-request semantics should be clarified. |
| `C2_IPC_CHUNK_SIZE` | Python settings, `.env.example` | Individual chunk size | Keep. IPC base config. |
| `C2_IPC_REASSEMBLY_SEGMENT_SIZE` | Python settings/tests, `.env.example` | Reassembly pool segment size | Keep. IPC base config with server/client defaults. |
| `C2_IPC_REASSEMBLY_MAX_SEGMENTS` | Python settings, `.env.example` | Reassembly pool segment count | Keep. IPC base config. |

### SDK-Local Runtime Variables

These are real runtime variables, but they should not enter the first shared Rust resolver unless the owning subsystem also moves into shared Rust runtime code.

| Variable | Current source | Current use | Resolver decision |
| --- | --- | --- | --- |
| `C2_HOLD_WARN_SECONDS` | Python native server | Python hold registry warning threshold | Keep Python-local for now, or move into a later diagnostics config if hold tracking becomes shared runtime behavior. |

### Variables To Exclude From Shared Resolver

| Variable | Reason |
| --- | --- |
| `NO_COLOR` | CLI display convention, not C-Two runtime config. |
| `CLICOLOR` | CLI display convention, not C-Two runtime config. |
| `CLICOLOR_FORCE` | CLI display convention, not C-Two runtime config. |
| `CC_IPC_SOCK_DIR` | Test/benchmark residue. Not a current shared `C2_*` runtime setting. |
| `PATH` | Developer tooling only. |
| `CARGO_HOME` | Developer tooling only. |
| `RUST_LOG` | Standard tracing/logging filter read by `tracing_subscriber`, not C-Two runtime config. |
| `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` | Standard proxy variables. C-Two should continue controlling whether relay traffic honors them through `C2_RELAY_USE_PROXY`, not by reparsing them in the config resolver. |

### Stale Or Suspicious Variables

| Variable | Finding | Decision |
| --- | --- | --- |
| `C2_IPC_ADDRESS` | Present in `.env.example` and old docs/specs, but current registry auto-generates server IPC addresses and no current runtime code reads `settings.ipc_address`. | Remove from public config docs unless a runtime use case is reintroduced. |
| `C2_IPC_SEGMENT_SIZE` | Appears in an older memory-pressure doc. Superseded by `C2_IPC_POOL_SEGMENT_SIZE`. | Treat as stale documentation. |
| `C2_IPC_MAX_SEGMENTS` | Appears in an older memory-pressure doc. Superseded by `C2_IPC_MAX_POOL_SEGMENTS`. | Treat as stale documentation. |
| `C2_TEST_PYTHON310` | Test-only escape hatch for selecting a Python 3.10 interpreter. Current test code can discover `python3.10` or use `uv python find 3.10`. | Remove. It pollutes the product `C2_*` namespace without carrying runtime config meaning. |

## Canonical Defaults

The resolver should use current Rust `c2-config` defaults as the canonical runtime defaults, except where an explicit design decision below changes a divergent entry point.

### IPC Defaults

| Field | Canonical default |
| --- | ---: |
| `shm_threshold` | `4096` |
| `pool_enabled` | `true` |
| `pool_segment_size` | `268_435_456` |
| `max_pool_segments` | `4` |
| `max_pool_memory` | derived as `pool_segment_size * max_pool_segments`; no public env/code override |
| `reassembly_segment_size` server | `67_108_864` |
| `reassembly_segment_size` client | `67_108_864` |
| `reassembly_max_segments` | `4` |
| `max_total_chunks` | `512` |
| `chunk_gc_interval_secs` | `5.0` |
| `chunk_threshold_ratio` | `0.9` |
| `chunk_assembler_timeout_secs` | `60.0` |
| `max_reassembly_bytes` | `8_589_934_592` |
| `chunk_size` | `131_072` |
| `max_frame_size` | `2_147_483_648` |
| `max_payload_size` | `17_179_869_184` |
| `max_pending_requests` | `1024` |
| `pool_decay_seconds` | `60.0` |
| `heartbeat_interval_secs` | `15.0` |
| `heartbeat_timeout_secs` | `30.0` |

### Relay Defaults

| Field | Canonical default |
| --- | ---: |
| `bind` | `"0.0.0.0:8080"` |
| `relay_id` | generated by `RelayConfig::generate_relay_id()` |
| `advertise_url` | empty, with effective URL derived from bind |
| `seeds` | empty |
| `idle_timeout_secs` | `60` |
| `anti_entropy_interval` | `60s` |
| `heartbeat_interval` | `5s` |
| `heartbeat_miss_threshold` | `3` |
| `dead_peer_probe_interval` | `30s` |
| `seed_retry_interval` | `10s` |
| `skip_ipc_validation` | `false` |
| `relay_use_proxy` | `false` |

`C2_RELAY_IDLE_TIMEOUT` belongs to the standalone Rust relay runtime. The
canonical default is 60 seconds; setting it to `0` explicitly disables
time-based eviction.

## Resolver Design

### Rust Types

Add a resolver module under `core/foundation/c2-config`, for example `src/resolver.rs`, and export it from `c2-config`.

The module should define:

```rust
pub struct ConfigResolver;

pub struct ConfigSources {
    pub env_file: EnvFilePolicy,
    pub process_env: EnvMap,
}

pub enum EnvFilePolicy {
    FromC2EnvFile,
    Disabled,
    Path(PathBuf),
}

pub struct RuntimeConfigOverrides {
    pub relay_address: Option<String>,
    pub relay: RelayConfigOverrides,
    pub server_ipc: ServerIpcConfigOverrides,
    pub client_ipc: ClientIpcConfigOverrides,
    pub shm_threshold: Option<u64>,
    pub relay_use_proxy: Option<bool>,
}

pub struct ResolvedRuntimeConfig {
    pub relay_address: Option<String>,
    pub relay: RelayConfig,
    pub server_ipc: ServerIpcConfig,
    pub client_ipc: ClientIpcConfig,
    pub shm_threshold: u64,
    pub relay_use_proxy: bool,
}
```

`EnvMap` can be an owned string map so tests can supply deterministic sources without mutating process-wide environment variables.

### Merge Algorithm

For each field:

1. Start with Rust default.
2. Apply env-file values.
3. Apply process environment values.
4. Apply code overrides.
5. Compute derived fields.
6. Validate the final resolved config.

Env-file loading must not overwrite an existing process environment value. This matches the current CLI behavior from `dotenvy::from_path`.

### Env Parsing Rules

- Empty strings should be treated as "unset" for optional string settings such as `C2_RELAY_ADDRESS`, `C2_RELAY_ID`, and `C2_RELAY_ADVERTISE_URL`.
- Numeric parse failures should return structured config errors that include the environment variable name.
- Boolean parsing should accept at least `1`, `true`, `yes`, `0`, `false`, and `no`, case-insensitively.
- `C2_RELAY_SEEDS` should parse as comma-separated URLs with whitespace trimming and empty entries removed.
- Duration-backed relay fields should parse seconds from integer or float strings as appropriate.

### Validation Rules

Move the stricter Python validation behavior into Rust where it is valid and shared:

- `pool_segment_size` must be `1..=u32::MAX`.
- `max_pool_segments` must be `1..=255`.
- `reassembly_max_segments` must be `1..=255`.
- `chunk_size` must be positive.
- `chunk_gc_interval_secs` must be positive.
- `chunk_assembler_timeout_secs` must be positive.
- `chunk_threshold_ratio` must be in `(0, 1]`.
- `reassembly_segment_size` must be positive.
- `max_frame_size` must be greater than the frame header size.
- `max_payload_size` must be positive.
- `shm_threshold` must be positive and must not exceed `max_frame_size` for server config.
- `heartbeat_interval_secs` must be non-negative.
- If heartbeat is enabled, `heartbeat_timeout_secs` must exceed `heartbeat_interval_secs`.
- Derived `max_pool_memory` must not overflow and, after validation of `max_pool_segments >= 1`, must be at least `pool_segment_size`.

### Derived Values

`max_pool_memory` should be derived as `pool_segment_size * max_pool_segments`. It should not be accepted from code, process env, or env file because it has no independent runtime behavior in the current Rust transport. The actual pool construction uses segment size and segment count.

`reassembly_segment_size` should have one default across server and client: `67_108_864` bytes (64 MiB). The field remains configurable because it controls the reassembly pool's buddy allocation grain. Large reassemblies can still use dedicated segments when they exceed the buddy segment shape.

`max_reassembly_bytes` should remain configurable for now because it is used by chunk lifecycle code as a logical safety limit. It currently serves both as a registry-wide soft byte limit and as a per-request assembler limit. That mixed meaning should be treated as follow-up design debt; if the limits need independent tuning, split it into separate total and per-request fields instead of deriving it from reassembly pool shape.

The current Python server builder clamps `pool_segment_size` down to `max_payload_size`. The resolver should not silently clamp new canonical behavior unless backward compatibility is required. The recommended behavior is:

- During the migration window, preserve the clamp only behind a clearly documented compatibility path if tests or users depend on it.
- Long term, replace silent clamp with validation error because code/env should not be mutated without user visibility.

## SDK Boundary

Python and future SDKs should expose code-level override APIs, not their own env parsers.

Python should migrate toward:

- `cc.set_config(shm_threshold=..., relay_address=..., relay_use_proxy=...)`
- `cc.set_server(**ipc_overrides)`, excluding derived-only fields such as `max_pool_memory`
- `cc.set_client(**ipc_overrides)`, excluding derived-only fields such as `max_pool_memory`
- `cc.set_relay(address)` as a compatibility wrapper for `set_config(relay_address=address)`

Python can keep `ServerIPCConfig` and `ClientIPCConfig` as compatibility facades, but they should be constructed from Rust-resolved config values. They should not own defaults or validation.

`pydantic-settings` should be removed once Python no longer owns env parsing.

## CLI Boundary

CLI should stop owning `.env` loading in `cli/src/main.rs` once `c2-config` exposes resolver support. CLI flags remain code-level overrides.

For `c3 relay`, clap should parse flags, then pass a `RelayConfigOverrides` object into the Rust resolver. The resolver applies:

```text
CLI flags > process env > env file > RelayConfig defaults
```

Registry subcommands currently require `--relay`. A later improvement may allow `C2_RELAY_ADDRESS` as a fallback through the resolver, but that is not required for the first implementation.

CLI color variables remain local to CLI display code.

## Rust Runtime Boundary

Runtime crates should prefer accepting resolved config structs over reading environment variables directly.

`c2_http::relay_use_proxy()` currently reads `C2_RELAY_USE_PROXY` directly. It should either:

- be redirected to a resolved `relay_use_proxy` value where a config object is available, or
- remain as a narrow compatibility helper only for call sites that cannot yet receive config.

The target architecture should avoid hidden process-env reads in lower transport crates.

## Migration Plan

1. Add resolver types and env parsing to `c2-config`.
2. Update `ServerIpcConfig::default()` so server and client `reassembly_segment_size` both default to 64 MiB.
3. Add Rust tests for env-file loading, process env precedence, code override precedence, parse errors, validation errors, and derived values.
4. Update CLI relay to call the resolver instead of loading `.env` directly and relying on clap env defaults.
5. Expose resolver functions through PyO3 for server/client/runtime config.
6. Update Python config builders to call Rust resolver and return compatibility dataclasses from resolved values.
7. Move Python `settings.relay_address` usage into registry-owned override state or a lightweight facade backed by resolver output.
8. Remove `pydantic-settings` from `sdk/python/pyproject.toml`.
9. Remove `C2_IPC_MAX_POOL_MEMORY` from Python settings, `.env.example`, and public docs; expose `max_pool_memory` only as a resolved/derived value if compatibility requires read access.
10. Update `.env.example`, CLI README, Python README, and root docs to list resolver-owned variables and canonical defaults.
11. Remove or mark stale docs for `C2_IPC_ADDRESS`, `C2_IPC_SEGMENT_SIZE`, `C2_IPC_MAX_SEGMENTS`, and `C2_TEST_PYTHON310`.

## Testing Requirements

Rust:

- Resolver defaults match the canonical `ServerIpcConfig::default()`, `ClientIpcConfig::default()`, and `RelayConfig::default()` after the server reassembly default is updated to 64 MiB.
- Env file values apply when process env is absent.
- Process env values override env file values.
- Code overrides override both process env and env file.
- `C2_ENV_FILE=""` disables env-file loading.
- Every resolver-owned environment variable has a parse test.
- Invalid values include the variable name in the error.
- Derived `max_pool_memory` behavior is tested, including that env/code attempts to set it are rejected or ignored according to the final migration compatibility policy.
- Relay idle-timeout default is tested at the resolver and CLI boundary.

Python:

- `build_server_config()` and `build_client_config()` return Rust-resolved values.
- Existing `cc.set_server()` and `cc.set_client()` kwargs beat env.
- `cc.set_relay()` beats `C2_RELAY_ADDRESS`.
- Python tests no longer import `pydantic-settings`.
- Python package installs without `pydantic-settings`.

CLI:

- `.env` loading still works for `c3 relay --dry-run`.
- Process env beats `.env`.
- CLI flags beat process env.
- Color env vars continue to affect help/banner output independently of resolver.

Docs:

- `.env.example` matches resolver-owned variables and canonical defaults.
- No public docs present stale `C2_IPC_ADDRESS` as a supported runtime setting.

## Open Decisions

1. Should the first implementation preserve Python's silent `pool_segment_size <= max_payload_size` clamp as compatibility behavior, or convert it immediately to validation error?
2. Should `C2_HOLD_WARN_SECONDS` remain Python SDK-only for now, or should it be renamed/moved into a broader Rust-owned diagnostics config?
3. Should CLI registry subcommands accept `C2_RELAY_ADDRESS` as a default relay URL in the first resolver migration, or remain explicit `--relay` only?
4. Should `max_reassembly_bytes` be split into separate total and per-request limits before implementation, or kept as one field during the resolver migration?
