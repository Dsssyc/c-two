import os
import signal
import socket
import subprocess
import tempfile
from pathlib import Path
from collections.abc import Callable, Iterator
from typing import TextIO
import urllib.request

# Disable .env loading before any c_two import — must be first.
os.environ['C2_ENV_FILE'] = ''

import pytest
import threading
import time
import c_two as cc

from c_two.transport.server import Server
from c_two.transport.client.util import ping

from tests.fixtures.hello import HelloImpl
from tests.fixtures.ihello import Hello

# Disable proxy for localhost to avoid HTTP test failures
os.environ.setdefault('NO_PROXY', '127.0.0.1,localhost')
os.environ.setdefault('no_proxy', '127.0.0.1,localhost')


# Unique address factory to avoid conflicts between tests
_address_counter = 0
_address_lock = threading.Lock()

def _next_id():
    global _address_counter
    with _address_lock:
        _address_counter += 1
        return _address_counter


@pytest.fixture
def unique_ipc_address():
    return f'ipc://test_hello_{_next_id()}'


@pytest.fixture(params=['ipc'])
def protocol_address(request, unique_ipc_address):
    """Parametrized fixture providing a unique address for each supported protocol."""
    addresses = {
        'ipc': unique_ipc_address,
    }
    return addresses[request.param]


@pytest.fixture
def hello_crm():
    """Create a Hello CRM instance."""
    return HelloImpl()


def _wait_for_server(address: str, timeout: float = 5.0) -> None:
    """Poll until the server responds to ping."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if ping(address, timeout=0.5):
                return
        except Exception:
            pass
        time.sleep(0.05)
    raise TimeoutError(f'Server at {address} not ready after {timeout}s')


def repo_root() -> Path:
    return Path(__file__).resolve().parents[3]


def free_tcp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(('127.0.0.1', 0))
        return int(sock.getsockname()[1])


def c3_binary() -> Path:
    root = repo_root()
    name = 'c3.exe' if os.name == 'nt' else 'c3'
    candidates = [
        root / 'cli' / 'target' / 'debug' / name,
        root / 'cli' / 'target' / 'release' / name,
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    pytest.skip(
        'c3 binary is required for relay tests. '
        'Run `python tools/dev/c3_tool.py --build --link` from the repository root.'
    )


def wait_for_relay(
    url: str,
    timeout: float = 5.0,
    proc: subprocess.Popen[str] | None = None,
) -> None:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc is not None and proc.poll() is not None:
            raise RuntimeError(f'Relay process exited with code {proc.returncode}')
        try:
            with opener.open(f'{url}/health', timeout=0.5) as resp:
                if resp.status == 200:
                    return
        except Exception:
            pass
        time.sleep(0.1)
    raise TimeoutError(f'Relay at {url} not ready after {timeout}s')


def stop_process(proc: subprocess.Popen[str]) -> None:
    if proc.poll() is not None:
        return
    if os.name == 'nt':
        proc.terminate()
    else:
        proc.send_signal(signal.SIGINT if hasattr(signal, 'SIGINT') else signal.SIGTERM)
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


class RelayProcess:
    def __init__(
        self,
        proc: subprocess.Popen[str],
        url: str,
        bind: str,
        stdout_log: TextIO,
        stderr_log: TextIO,
    ):
        self.proc = proc
        self.url = url
        self.bind = bind
        self.stdout_log = stdout_log
        self.stderr_log = stderr_log

    def stop(self) -> None:
        try:
            stop_process(self.proc)
        finally:
            self.stdout_log.close()
            self.stderr_log.close()


def _read_log(log: TextIO) -> str:
    log.flush()
    log.seek(0)
    return log.read()


def _open_process_log() -> TextIO:
    return tempfile.TemporaryFile(mode='w+', encoding='utf-8')


@pytest.fixture
def start_c3_relay() -> Iterator[Callable[..., RelayProcess]]:
    processes: list[RelayProcess] = []

    def _launch(
        *,
        actual_port: int,
        relay_id: str | None = None,
        seeds: list[str] | None = None,
        skip_ipc_validation: bool = False,
        idle_timeout: int | None = None,
    ) -> RelayProcess:
        bind = f'127.0.0.1:{actual_port}'
        url = f'http://127.0.0.1:{actual_port}'
        args = [str(c3_binary()), 'relay', '--bind', bind, '--advertise-url', url]
        if relay_id is not None:
            args.extend(['--relay-id', relay_id])
        if seeds:
            args.extend(['--seeds', ','.join(seeds)])
        if skip_ipc_validation:
            args.append('--skip-ipc-validation')
        if idle_timeout is not None:
            args.extend(['--idle-timeout', str(idle_timeout)])

        env = os.environ.copy()
        env['C2_ENV_FILE'] = ''
        env['NO_PROXY'] = '127.0.0.1,localhost'
        env['no_proxy'] = '127.0.0.1,localhost'
        stdout_log = _open_process_log()
        stderr_log = _open_process_log()
        try:
            proc = subprocess.Popen(
                args,
                cwd=repo_root(),
                env=env,
                stdout=stdout_log,
                stderr=stderr_log,
                text=True,
            )
        except Exception:
            stdout_log.close()
            stderr_log.close()
            raise
        relay = RelayProcess(
            proc=proc,
            url=url,
            bind=bind,
            stdout_log=stdout_log,
            stderr_log=stderr_log,
        )
        try:
            wait_for_relay(url, proc=proc)
        except Exception:
            stop_process(proc)
            stdout = _read_log(stdout_log)
            stderr = _read_log(stderr_log)
            stdout_log.close()
            stderr_log.close()
            raise AssertionError(
                f'c3 relay failed to start\nstdout:\n{stdout}\nstderr:\n{stderr}'
            )
        processes.append(relay)
        return relay

    def _start(
        *,
        port: int | None = None,
        relay_id: str | None = None,
        seeds: list[str] | None = None,
        skip_ipc_validation: bool = False,
        idle_timeout: int | None = None,
    ) -> RelayProcess:
        last_error: AssertionError | None = None
        attempts = 1 if port is not None else 5
        for _ in range(attempts):
            actual_port = port if port is not None else free_tcp_port()
            try:
                return _launch(
                    actual_port=actual_port,
                    relay_id=relay_id,
                    seeds=seeds,
                    skip_ipc_validation=skip_ipc_validation,
                    idle_timeout=idle_timeout,
                )
            except AssertionError as exc:
                last_error = exc
                if port is not None:
                    raise
        assert last_error is not None
        raise last_error

    yield _start

    cleanup_errors: list[Exception] = []
    for relay in reversed(processes):
        try:
            relay.stop()
        except Exception as exc:
            cleanup_errors.append(exc)
    if cleanup_errors:
        raise AssertionError(f'failed to stop {len(cleanup_errors)} c3 relay process(es)')


@pytest.fixture
def hello_server(protocol_address, hello_crm):
    """Start a Hello CRM server on the given protocol, yield the address, then shut down."""
    server = Server(
        bind_address=protocol_address,
        crm_class=Hello,
        crm_instance=hello_crm,
    )
    server.start()
    _wait_for_server(protocol_address)

    yield protocol_address

    server.shutdown()
