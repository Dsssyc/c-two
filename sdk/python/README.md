# C-Two Python SDK

This directory contains the Python package for C-Two.

## Development

Run development commands from the repository root.

Prerequisites:

- Python 3.10 or newer
- Python 3.12 for standard local development
- Python 3.14.3t when testing free-threading support
- Rust toolchain
- `uv`

Install dependencies and build the Python native extension. This also compiles
the required Rust core crates; no separate Rust prebuild step is needed:

```bash
uv sync
```

Rebuild the native extension after changing Rust code:

```bash
uv sync --reinstall-package c-two
```

Run the Python SDK tests:

```bash
C2_RELAY_ADDRESS= uv run pytest sdk/python/tests -q --timeout=30
```

Run Rust core checks when validating shared native runtime changes:

```bash
cargo test --manifest-path core/Cargo.toml --workspace
```

For CLI build, link, and test commands, see [`../../cli/README.md`](../../cli/README.md).

## Examples

Python examples live under `../../examples/python/`:

```bash
uv sync --group examples
uv run python examples/python/local.py
```

## Benchmarks

Python-specific benchmarks live in `benchmarks/`:

```bash
C2_RELAY_ADDRESS= uv run python sdk/python/benchmarks/segment_size_benchmark.py
```
