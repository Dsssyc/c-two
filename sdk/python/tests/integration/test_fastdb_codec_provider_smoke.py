from __future__ import annotations

import pytest

import c_two as cc
from c_two.config.settings import settings
from c_two.crm.codec import _clear_codec_registry_for_tests
from c_two.transport.registry import _ProcessRegistry


@pytest.fixture(autouse=True)
def clean_runtime():
    previous_relay = settings.relay_anchor_address
    _ProcessRegistry.reset()
    _clear_codec_registry_for_tests()
    try:
        yield
    finally:
        _ProcessRegistry.reset()
        settings.relay_anchor_address = previous_relay
        _clear_codec_registry_for_tests()


def _make_fastdb_contract(namespace: str):
    pytest.importorskip('fastdb4py', reason='fastdb C-Two smoke requires fastdb4py')
    from fastdb4py import F64, STR, feature
    from fastdb4py.c_two_provider import install_c_two_provider

    install_c_two_provider(cc_module=cc)

    class RpcPoint:
        pass

    RpcPoint.__annotations__ = {'x': F64, 'y': F64, 'name': STR}
    RpcPoint = feature(RpcPoint)

    class FastdbPoint:
        def echo(self, point):
            ...

    FastdbPoint.echo.__annotations__ = {'point': RpcPoint, 'return': RpcPoint}
    FastdbPoint = cc.crm(namespace=f'test.fastdb.{namespace}', version='0.1.0')(FastdbPoint)

    class FastdbPointResource:
        def echo(self, point):
            return RpcPoint(x=point.x + 1.0, y=point.y + 2.0, name=f'{point.name}:ok')

    return RpcPoint, FastdbPoint, FastdbPointResource


def _assert_fastdb_echo(crm, Point) -> None:
    result = crm.echo(Point(x=1.0, y=2.0, name='rpc'))

    assert result.x == pytest.approx(2.0)
    assert result.y == pytest.approx(4.0)
    assert result.name == 'rpc:ok'


def test_fastdb_provider_payloads_work_thread_local_and_direct_ipc():
    Point, FastdbPoint, FastdbPointResource = _make_fastdb_contract('thread-ipc')
    cc.register(FastdbPoint, FastdbPointResource(), name='fastdb-point-thread-ipc')

    thread_crm = cc.connect(FastdbPoint, name='fastdb-point-thread-ipc')
    try:
        assert thread_crm.client._mode == 'thread'  # noqa: SLF001
        _assert_fastdb_echo(thread_crm, Point)
    finally:
        cc.close(thread_crm)

    address = cc.server_address()
    assert address is not None
    ipc_crm = cc.connect(FastdbPoint, name='fastdb-point-thread-ipc', address=address)
    try:
        assert ipc_crm.client._mode == 'ipc'  # noqa: SLF001
        _assert_fastdb_echo(ipc_crm, Point)
    finally:
        cc.close(ipc_crm)


def test_fastdb_provider_payloads_work_explicit_http_relay(start_c3_relay):
    Point, FastdbPoint, FastdbPointResource = _make_fastdb_contract('relay')
    relay = start_c3_relay()
    cc.set_relay_anchor(relay.url)
    cc.register(FastdbPoint, FastdbPointResource(), name='fastdb-point-relay')

    crm = cc.connect(FastdbPoint, name='fastdb-point-relay', address=relay.url)
    try:
        assert crm.client._mode == 'http'  # noqa: SLF001
        _assert_fastdb_echo(crm, Point)
    finally:
        cc.close(crm)
