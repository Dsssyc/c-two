"""Relay QPS benchmark - measures requests/second through an external c3 relay.

Uses `hey` (Go HTTP benchmark tool) for accurate measurements.
Starts the CRM in-process, requires an already-running standalone relay, then
invokes hey as subprocess.

Start the relay first from a source checkout:

    python tools/dev/c3_tool.py --build --link
    c3 relay --bind 127.0.0.1:<port>

Outputs: QPS=<number>
"""
from __future__ import annotations

import glob
import json
import os
import pickle
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import c_two as cc
from c_two.transport.registry import _ProcessRegistry
from tests.fixtures.hello import HelloImpl
from tests.fixtures.ihello import Hello

# ── Configuration ─────────────────────────────────────────────────────────

RELAY_PORT = 19950 + (os.getpid() % 100)
TOTAL_REQUESTS = 3000
CONCURRENCY = 32

IPC_NAME = f'relay_bench_{os.getpid()}'
IPC_ADDR = f'ipc://{IPC_NAME}'
PAYLOAD_FILE = '/tmp/c2_bench_payload.bin'

_NO_PROXY_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def _relay_help(relay_url: str) -> str:
    bind = relay_url.removeprefix('http://').removeprefix('https://')
    return (
        f'External c3 relay is not reachable at {relay_url}. '
        f'Start it first with `c3 relay --bind {bind}` after running '
        '`python tools/dev/c3_tool.py --build --link` from the source checkout.'
    )


def _require_external_relay(relay_url: str) -> None:
    try:
        with _NO_PROXY_OPENER.open(f'{relay_url}/health', timeout=5) as resp:
            if resp.status >= 400:
                raise OSError(f'HTTP {resp.status}')
    except Exception as exc:
        raise SystemExit(_relay_help(relay_url)) from exc


def _post_json(url: str, payload: dict[str, str], timeout: float = 5.0) -> None:
    data = json.dumps(payload).encode('utf-8')
    req = urllib.request.Request(
        url,
        data=data,
        method='POST',
        headers={'Content-Type': 'application/json'},
    )
    with _NO_PROXY_OPENER.open(req, timeout=timeout) as resp:
        if resp.status >= 400:
            raise OSError(f'HTTP {resp.status}')


def _register_relay_upstream(
    relay_url: str,
    name: str,
    address: str,
    server_id: str,
) -> None:
    _post_json(
        f'{relay_url}/_register',
        {'name': name, 'server_id': server_id, 'address': address},
    )


def _unregister_relay_upstream(relay_url: str, name: str, server_id: str) -> None:
    _post_json(f'{relay_url}/_unregister', {'name': name, 'server_id': server_id})


def _stdout_tail(stdout: str, limit: int = 1200) -> str:
    return stdout[-limit:] if stdout else '<empty>'


def _run_hey(label: str, args: list[str]) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(
            f'hey {label} failed with exit code {result.returncode}\n'
            f'stderr:\n{result.stderr or "<empty>"}\n'
            f'stdout tail:\n{_stdout_tail(result.stdout)}'
        )
    if 'Status code distribution:' in result.stdout:
        ok = re.search(r'\[2\d\d\]\s+\d+\s+responses', result.stdout)
        if not ok:
            raise RuntimeError(
                f'hey {label} did not receive any 2xx responses\n'
                f'stderr:\n{result.stderr or "<empty>"}\n'
                f'stdout tail:\n{_stdout_tail(result.stdout)}'
            )
    return result


def _best_effort_unregister_relay_upstream(
    relay_url: str,
    name: str,
    server_id: str,
) -> None:
    try:
        _unregister_relay_upstream(relay_url, name, server_id)
    except urllib.error.HTTPError as exc:
        if exc.code != 404:
            print(f'Warning: failed to unregister relay route {name!r}: {exc}', file=sys.stderr)
    except Exception as exc:
        print(f'Warning: failed to unregister relay route {name!r}: {exc}', file=sys.stderr)


def cleanup_stale():
    for f in glob.glob('/tmp/c_two_ipc/relay_bench_*.sock'):
        try:
            os.unlink(f)
        except OSError:
            pass
    try:
        from c_two.mem import cleanup_stale_shm
        cleanup_stale_shm()
    except Exception:
        pass


def main():
    cleanup_stale()
    _ProcessRegistry.reset()
    relay_url = f'http://127.0.0.1:{RELAY_PORT}'
    _require_external_relay(relay_url)
    route_name = f'hello_bench_{os.getpid()}'
    route_registered = False
    server_id: str | None = None

    # Write payload file for hey
    with open(PAYLOAD_FILE, 'wb') as f:
        f.write(pickle.dumps(('Benchmark',)))

    try:
        cc.register(Hello, HelloImpl(), name=route_name)
        ipc_addr = cc.server_address()
        server_id = cc.server_id()
        if server_id is None:
            raise RuntimeError('registered server did not expose server_id')

        _register_relay_upstream(relay_url, route_name, ipc_addr, server_id)
        route_registered = True

        # Warmup with hey
        _run_hey(
            'warmup',
            ['hey', '-n', '500', '-c', '8', '-m', 'POST',
             '-D', PAYLOAD_FILE, '-T', 'application/octet-stream',
             f'{relay_url}/{route_name}/greeting'],
        )

        # Benchmark
        result = _run_hey(
            'benchmark',
            ['hey', '-n', str(TOTAL_REQUESTS), '-c', str(CONCURRENCY),
             '-m', 'POST', '-D', PAYLOAD_FILE,
             '-T', 'application/octet-stream',
             f'{relay_url}/{route_name}/greeting'],
        )

        # Extract QPS
        m = re.search(r'Requests/sec:\s+([\d.]+)', result.stdout)
        if m:
            qps = float(m.group(1))
            print(f'QPS={qps:.1f}')
        else:
            raise RuntimeError(
                'hey benchmark output did not include Requests/sec\n'
                f'stderr:\n{result.stderr or "<empty>"}\n'
                f'stdout tail:\n{_stdout_tail(result.stdout)}'
            )

        # Extract latency
        for line in result.stdout.splitlines():
            line = line.strip()
            if any(k in line for k in ['Average:', 'Fastest:', 'Slowest:', '50%', '99%', 'Status']):
                print(f'  {line}')
    finally:
        if route_registered and server_id is not None:
            _best_effort_unregister_relay_upstream(relay_url, route_name, server_id)
        cc.shutdown()
        _ProcessRegistry.reset()
        cleanup_stale()
        try:
            os.unlink(PAYLOAD_FILE)
        except OSError:
            pass


if __name__ == '__main__':
    main()
