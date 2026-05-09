# Python Examples Redesign

**Date:** 2026-04-28  
**Status:** Approved design  
**Scope:** Redesign Python examples as clear teaching and manual smoke-test entry points with one transport mode per script.

## Problem

The current Python examples mix multiple transport modes in the same entry
points:

- `examples/python/client.py` supports both explicit IPC addresses and relay
  name resolution.
- `examples/python/relay_client.py` repeats the relay path from `client.py`.
- `examples/python/crm_process.py` can run as a plain IPC resource process or
  register with a relay.
- `examples/python/relay_mesh/` also demonstrates relay name resolution, but
  its boundary relative to the basic relay example is unclear.

This makes the examples harder to use as teaching material and less reliable as
manual smoke tests. A developer can set `C2_RELAY_ANCHOR_ADDRESS` in `.env` and
accidentally change a supposedly IPC-only example into a relay example. That
violates the intended precedence model where explicit code/CLI input beats
environment and env-file configuration.

## Goal

Make each Python example demonstrate exactly one runtime mode:

- Local in-process or thread-local behavior.
- Direct IPC between a resource process and a client.
- Relay-backed name resolution through a single relay.
- Relay mesh resource discovery as a separate advanced example.

The examples are not a polished application. They are teaching fixtures and
manual smoke-test commands for maintainers who want to quickly verify that a
recent change still runs end-to-end.

## Non-Goals

- Do not build an automatic "best transport" example.
- Do not keep a single multi-mode client entry point.
- Do not preserve example file names for compatibility. The project is still
  pre-1.0 and examples should favor clarity over compatibility.
- Do not add broad example frameworks, config loaders, or helper abstractions
  beyond small shared utilities that keep scripts readable.
- Do not move production SDK configuration semantics into examples.

## Proposed Structure

### Shared Grid Contract

Keep the existing reusable Grid files:

```text
examples/python/grid/grid_contract.py
examples/python/grid/nested_grid.py
examples/python/grid/transferables.py
```

These define the contract and resource implementation used by all Grid examples.

### Local Example

Keep or rename `examples/python/local.py` as the single-process teaching entry
point. It should demonstrate local/thread-preferred calls only. It should not
read relay configuration and should not require an IPC address.

Expected command:

```bash
uv run python examples/python/local.py
```

### Direct IPC Example

Create explicit IPC-only scripts:

```text
examples/python/ipc_resource.py
examples/python/ipc_client.py
```

`ipc_resource.py` starts a Grid resource process and prints its `ipc://...`
address. It does not call `cc.set_relay_anchor()` and does not read
`C2_RELAY_ANCHOR_ADDRESS`.

`ipc_client.py` requires an explicit `ipc://...` address argument. It does not
read relay config. If `C2_RELAY_ANCHOR_ADDRESS` is present in the process environment
or `.env`, this script must still use only the explicit IPC address.

Expected commands:

```bash
uv run python examples/python/ipc_resource.py
uv run python examples/python/ipc_client.py ipc://...
```

### Single Relay Example

Create relay-only scripts:

```text
examples/python/relay_resource.py
examples/python/relay_client.py
examples/python/relay_config.py
```

`relay_resource.py` starts the same Grid resource and registers it with a relay.
`relay_client.py` connects by resource name through the relay. Both scripts are
relay-only.

Relay URL precedence:

```text
explicit --relay-url > C2_RELAY_ANCHOR_ADDRESS / C2_ENV_FILE resolved by Rust config > default http://127.0.0.1:8300
```

The helper in `relay_config.py` may remain example-local. Its purpose is only
to keep relay URL resolution consistent across relay examples while preserving
the shared Rust-backed resolver.

Expected commands:

```bash
c3 relay --bind 127.0.0.1:8300
uv run python examples/python/relay_resource.py --relay-url http://127.0.0.1:8300
uv run python examples/python/relay_client.py --relay-url http://127.0.0.1:8300
```

For manual smoke tests, omitting `--relay-url` should work when the relay uses
the default address:

```bash
c3 relay --bind 127.0.0.1:8300
uv run python examples/python/relay_resource.py
uv run python examples/python/relay_client.py
```

### Relay Mesh Example

Keep `examples/python/relay_mesh/` as the advanced relay mesh example, but make
its README and script names explicitly distinguish it from the single-relay
example.

The mesh example should assume relay operation and should use the same relay URL
resolution helper or an equivalent local helper:

```text
explicit CLI option if added > Rust-resolved C2_RELAY_ANCHOR_ADDRESS / env file > default http://127.0.0.1:8300
```

It should not be presented as the default relay smoke test. The single-relay
example owns that role.

## Configuration Rules

IPC examples:

- Must not read `C2_RELAY_ANCHOR_ADDRESS`.
- Must not call `cc.set_relay_anchor()`.
- Must fail clearly if no IPC address is provided.
- Must continue using the explicit IPC address even when `.env` contains a relay
  URL.

Relay examples:

- May read relay URL through the Rust-backed Python settings facade.
- Must let `--relay-url` override env/env-file values.
- May default to `http://127.0.0.1:8300` because they are explicitly
  relay-only teaching scripts.
- Must not accept or silently prefer IPC addresses.

Mesh examples:

- May default to the local relay URL for ease of manual testing.
- Must document that they are advanced examples and not the basic relay smoke
  path.

## Documentation Updates

Update `examples/README.md` to show three clear paths:

1. Local single-process example.
2. Direct IPC resource/client example.
3. Single relay resource/client example.

Keep mesh documentation in `examples/python/relay_mesh/README.md`, but link to
it as an advanced follow-up after the basic relay example.

Every command block should be copy/paste runnable from a source checkout after:

```bash
uv sync --group examples
python tools/dev/c3_tool.py --build --link
```

The README should state that tests, examples, and SDK development require
building `c3` through `tools/dev/c3_tool.py` when relay behavior is involved.

## Tests

Add or update tests to cover:

- `ipc_client.py` requires an IPC address.
- `ipc_client.py ipc://...` ignores `C2_RELAY_ANCHOR_ADDRESS` from both process env and
  `C2_ENV_FILE`.
- `relay_client.py --relay-url X` uses `X`.
- `relay_client.py` reads `C2_RELAY_ANCHOR_ADDRESS` from env file through the Rust
  resolver.
- `relay_client.py` defaults to `http://127.0.0.1:8300` when no relay URL is
  configured.
- End-to-end direct IPC smoke test: `ipc_resource.py` plus `ipc_client.py`.
- End-to-end relay smoke test: `c3 relay`, `relay_resource.py`, and
  `relay_client.py`.

The existing Python example syntax tests should be updated to compile/import the
new scripts and stop referring to removed dual-mode behavior.

## Migration Plan

The implementation should be a clean break:

1. Introduce the new IPC-only and relay-only scripts.
2. Update README commands to use the new names.
3. Remove or replace the old dual-mode `client.py` entry point.
4. Remove relay behavior from the IPC resource entry point, or split
   `crm_process.py` into mode-specific resource scripts.
5. Update tests to assert the new mode boundaries.

If keeping short aliases is useful for developer ergonomics, aliases should be
thin wrappers that preserve the same one-mode semantics. They should not
reintroduce automatic mode selection.

## Acceptance Criteria

- A new developer can identify which script demonstrates which transport mode
  without reading script internals.
- Manual smoke testing has one command sequence for IPC and one for relay.
- Setting `C2_RELAY_ANCHOR_ADDRESS` cannot change direct IPC examples.
- Explicit `--relay-url` beats env/env-file in relay examples.
- `C2_RELAY_ANCHOR_ADDRESS` env-file support remains demonstrated in relay examples.
- No Python example directly parses C-Two runtime env variables when a shared
  Rust-backed resolver path exists.
