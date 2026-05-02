# Progress

## 2026-04-30

- Confirmed cwd `/Users/soku/Desktop/codespace/WorldInProgress/c-two`, branch `dev-feature`.
- Started phase 1: dead peer route resurrection.
- Read relay peer handlers, authority, route table, state, background loops, disseminator, and router resolution path.
- Added regression tests for Dead peer announce/digest and route resolution, confirmed they failed, then fixed trust checks and route resolution to require Alive peers.
- Ran `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture`: 103 tests passed.
- Linkage review found `merge_snapshot` can still import routes for non-Alive peers and currently inserts new snapshot peers as Alive regardless of snapshot status.
- Test command error: attempted two cargo test filters in one command; cargo accepts one filter. Retrying with a single prefix filter.
- Added heartbeat recovery and snapshot ingestion regression tests. Fixed route mutation trust to require Alive, kept heartbeat recovery on known peers, and blocked snapshot route import for non-Alive or locally Dead owners.
- Ran `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture`: 108 tests passed.
- Linkage review after phase 1: remaining direct route mutations are centralized in `RouteAuthority` or `RouteTable::merge_snapshot`; tests/helpers directly insert routes only under `#[cfg(test)]`. `resolve` is the data-plane exposure boundary and now filters non-Alive peer routes.
- Started phase 2: unregister retry safety. Added HTTP regression tests for same-server idempotent retry and different-server rejection after tombstone.
- Test command error repeated: attempted two cargo test filters in one command; retrying with a shared filter.
- Implemented private local owner tombstones for unregister idempotency. `RouteTombstone.server_id` is serde-skipped and only used locally; newer tombstones without owner data preserve the private owner id.
- HTTP `_unregister` and command-loop unregister now return success for same-server already-removed routes; Python still treats relay 404 as error.
- Ran `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture`: 113 tests passed.
- Ran `uv run pytest sdk/python/tests/unit/test_relay_graceful_shutdown.py -q`: 5 passed.
- Phase 3 added regression for newer snapshot route clearing older tombstone. Fixed `merge_snapshot` to use `register_route(entry)` instead of raw `routes.insert`.
- Ran `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture`: 114 tests passed.
- Phase 4 added regression for invalid peer snapshots causing partial mutation. Fixed `merge_snapshot` to prevalidate all peers before route/tombstone mutations and removed an obsolete post-mutation validation branch.
- Ran `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture`: 115 tests passed.
- Phase 5 hardened IPC address parsing in Rust server/client and Python client util. Rust client preserves `IpcClient::new` API by storing address config errors and returning `IpcError::Config` from `connect()` before touching UDS paths.
- Updated benchmark/test helpers to reuse the Python socket path helper instead of manually stripping `ipc://`.
- Final verification after formatting:
  - `cargo fmt --manifest-path core/Cargo.toml --all`
  - `cargo fmt --manifest-path cli/Cargo.toml --all`
  - `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture`: 115 passed
  - `cargo test --manifest-path core/Cargo.toml -p c2-server -- --nocapture`: 48 passed
  - `cargo test --manifest-path core/Cargo.toml -p c2-ipc -- --nocapture`: 29 passed
  - `cargo test --manifest-path core/Cargo.toml -p c2-config -- --nocapture`: 37 passed
  - `cargo test --manifest-path cli/Cargo.toml -- --nocapture`: 23 passed across CLI test binaries
  - `uv run pytest sdk/python/tests/unit/test_ipc_address_validation.py sdk/python/tests/unit/test_relay_graceful_shutdown.py sdk/python/tests/unit/test_ipc_config.py sdk/python/tests/unit/test_name_collision.py sdk/python/tests/unit/test_benchmark_relay_helpers.py sdk/python/tests/unit/test_python_examples_syntax.py -q`: 63 passed
  - `git diff --check`: clean

## 2026-05-02

- Recorded the SDK boundary rule: language SDKs must stay as typed glue except transferable serialization; env/config, relay control-plane correctness, retries/idempotency, and fallback mechanisms belong in Rust.
- Added a regression for relay restart ordering: a fresh local register must outrank an old tombstone already learned from peers.
- Confirmed the regression failed with the previous pure process-local logical counter (`registered_at=1` under old `removed_at=10`).
- Changed `RouteTable::next_local_timestamp()` to a hybrid logical timestamp based on epoch milliseconds plus process-local monotonic increments, then confirmed the new regression passes.
- Added a Python registry regression for Rust unregister failure. Confirmed the old code popped the Python registration before `NativeServerBridge.unregister_crm()` succeeded, then changed unregister ordering so Python bookkeeping is removed only after the Rust route deletion succeeds.
- Found the same split-brain pattern in relay-registration rollback. Added a regression proving `_rollback_registration()` hid the Python registration when Rust route removal failed, then changed rollback to keep the registration visible and skip server shutdown unless Rust unregister succeeded.
- Added route-table regressions for `NaN`/`inf` peer timestamps. Confirmed they were accepted, then centralized rejection in `register_route()` and `apply_tombstone()` so announce, digest, and snapshot ingestion share the same guard.
- Linkage review found `unregister_route_with_tombstone()` could remove a route before `apply_tombstone()` rejected a non-finite `removed_at`. Added a regression and rejected non-finite delete timestamps before any mutation.
- Verification after this batch:
  - `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture`: 130 passed
  - `uv run pytest sdk/python/tests/unit/test_name_collision.py sdk/python/tests/unit/test_relay_graceful_shutdown.py sdk/python/tests/unit/test_ipc_config.py -q`: 43 passed
  - `cargo test --manifest-path core/Cargo.toml -p c2-config -- --nocapture`: 40 passed
  - `git diff --check`: clean
