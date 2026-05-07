"""Direct IPC control helpers are backed by Rust c2-ipc and do not use relay."""
from __future__ import annotations

import os
import time
import uuid

import pytest

from c_two.transport import Server
from c_two.transport.client.util import ping, shutdown

from tests.fixtures.hello import HelloImpl
from tests.fixtures.ihello import Hello


def _unique_region(prefix: str = 'direct_ctl') -> str:
    return f'{prefix}_{os.getpid()}_{uuid.uuid4().hex[:12]}'


def _wait_for_ping(address: str, timeout: float = 5.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if ping(address, timeout=0.2):
            return
        time.sleep(0.05)
    raise TimeoutError(f'{address} did not respond to ping')


@pytest.fixture
def direct_server(monkeypatch):
    monkeypatch.delenv('C2_RELAY_ADDRESS', raising=False)
    address = f'ipc://{_unique_region()}'
    server = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    server.start()
    _wait_for_ping(address)
    yield address, server
    server.shutdown()


def test_ping_returns_true_against_direct_ipc_without_relay(direct_server):
    address, _server = direct_server
    assert ping(address, timeout=0.5) is True


def test_ping_ignores_bad_relay_env(monkeypatch, direct_server):
    address, _server = direct_server
    monkeypatch.setenv('C2_RELAY_ADDRESS', 'http://127.0.0.1:9')
    assert ping(address, timeout=0.5) is True


def test_shutdown_stops_direct_ipc_without_relay(monkeypatch):
    monkeypatch.delenv('C2_RELAY_ADDRESS', raising=False)
    address = f'ipc://{_unique_region("shutdown")}'
    server = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    server.start()
    _wait_for_ping(address)

    assert shutdown(address, timeout=0.5) is True

    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        if not ping(address, timeout=0.2):
            break
        time.sleep(0.05)
    else:
        pytest.fail('server still responds to ping after shutdown signal')

    server.shutdown()


def test_control_helpers_reject_invalid_addresses():
    assert ping('tcp://not-ipc', timeout=0.01) is False
    assert shutdown('tcp://not-ipc', timeout=0.01) is False


def test_server_start_returns_only_after_direct_ipc_ready(monkeypatch):
    monkeypatch.delenv('C2_RELAY_ADDRESS', raising=False)
    address = f'ipc://{_unique_region("ready")}'
    server = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    try:
        server.start(timeout=5.0)
        assert ping(address, timeout=0.5) is True
    finally:
        server.shutdown()


def test_server_start_readiness_ignores_bad_relay_env(monkeypatch):
    monkeypatch.setenv('C2_RELAY_ADDRESS', 'http://127.0.0.1:9')
    address = f'ipc://{_unique_region("ready_bad_relay")}'
    server = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    try:
        server.start(timeout=5.0)
        assert ping(address, timeout=0.5) is True
    finally:
        server.shutdown()


def test_starting_second_server_does_not_unlink_active_socket(monkeypatch):
    monkeypatch.delenv('C2_RELAY_ADDRESS', raising=False)
    address = f'ipc://{_unique_region("active")}'
    first = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    second = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello2',
    )
    try:
        first.start(timeout=5.0)
        assert ping(address, timeout=0.5) is True
        with pytest.raises(
            RuntimeError,
            match='active listener|address already in use|failed to start',
        ):
            second.start(timeout=1.0)
        assert ping(address, timeout=0.5) is True
    finally:
        try:
            second.shutdown()
        except Exception:
            pass
        first.shutdown()


def test_bridge_shutdown_after_direct_ipc_shutdown_allows_new_server(monkeypatch):
    monkeypatch.delenv('C2_RELAY_ADDRESS', raising=False)
    address = f'ipc://{_unique_region("external_shutdown")}'
    server = Server(
        bind_address=address,
        crm_class=Hello,
        crm_instance=HelloImpl(),
        name='hello',
    )
    try:
        server.start(timeout=5.0)
        assert ping(address, timeout=0.5) is True
        assert shutdown(address, timeout=0.5) is True

        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            if not ping(address, timeout=0.2):
                break
            time.sleep(0.05)
        else:
            pytest.fail('server still responds to ping after IPC shutdown')

        server.shutdown()

        replacement = Server(
            bind_address=address,
            crm_class=Hello,
            crm_instance=HelloImpl(),
            name='hello',
        )
        try:
            replacement.start(timeout=5.0)
            assert ping(address, timeout=0.5) is True
        finally:
            replacement.shutdown()
    finally:
        server.shutdown()
