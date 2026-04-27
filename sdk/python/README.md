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

Build and link the local `c3` CLI for source-checkout workflows:

```bash
python tools/dev/c3_tool.py --build --link
c3 --version
```

Run Rust checks when validating Rust changes:

```bash
cargo test --manifest-path core/Cargo.toml --workspace
cargo test --manifest-path cli/Cargo.toml
```

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
