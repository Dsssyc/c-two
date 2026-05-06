from __future__ import annotations

import gc

import pytest

import c_two as cc
from c_two.config import settings
from c_two.transport.registry import _ProcessRegistry


@pytest.fixture(autouse=True)
def _cleanup():
    old_threshold = settings._shm_threshold  # noqa: SLF001
    cc.shutdown()
    _ProcessRegistry._instance = None  # noqa: SLF001
    yield
    settings._shm_threshold = old_threshold  # noqa: SLF001
    cc.shutdown()
    _ProcessRegistry._instance = None  # noqa: SLF001


@cc.transferable
class BytesView:
    data: memoryview | bytes

    def serialize(value: "BytesView") -> bytes:
        return bytes(value.data)

    def deserialize(data: memoryview) -> "BytesView":
        return BytesView(bytes(data))

    def from_buffer(data: memoryview) -> "BytesView":
        return BytesView(data)


@cc.crm(namespace="cc.test.buffer_lease", version="0.1.0")
class PayloadCRM:
    @cc.transfer(output=BytesView)
    def payload(self, size: int) -> BytesView:
        ...


class PayloadResource:
    def payload(self, size: int) -> BytesView:
        return BytesView(b"x" * size)


def _connect_payload():
    address = cc.server_address()
    assert address is not None
    return cc.connect(PayloadCRM, name="payload", address=address)


def test_client_hold_inline_response_counts_and_releases_without_copy():
    settings.shm_threshold = 1024
    cc.register(PayloadCRM, PayloadResource(), name="payload")
    cc.serve(blocking=False)
    client = _connect_payload()
    try:
        held = cc.hold(client.payload)(16)
        try:
            stats = cc.hold_stats()
            assert stats["active_holds"] == 1
            assert stats["total_held_bytes"] == 16
            assert stats["by_storage"]["inline"]["active_holds"] == 1
            assert isinstance(held.value.data, memoryview)
            assert bytes(held.value.data[:4]) == b"xxxx"
        finally:
            held.release()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(client)


def test_client_hold_shm_response_counts_and_releases_without_relay(monkeypatch):
    settings.shm_threshold = 1024
    cc.register(PayloadCRM, PayloadResource(), name="payload")
    cc.serve(blocking=False)
    monkeypatch.setenv("C2_RELAY_ADDRESS", "http://127.0.0.1:9")
    client = _connect_payload()
    try:
        held = cc.hold(client.payload)(8192)
        try:
            stats = cc.hold_stats()
            assert stats["active_holds"] == 1
            assert stats["total_held_bytes"] == 8192
            assert (
                stats["by_storage"]["shm"]["active_holds"]
                + stats["by_storage"]["handle"]["active_holds"]
                >= 1
            )
            assert isinstance(held.value.data, memoryview)
            assert bytes(held.value.data[:4]) == b"xxxx"
        finally:
            held.release()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(client)


def test_client_hold_drop_releases_retained_response():
    settings.shm_threshold = 1024
    cc.register(PayloadCRM, PayloadResource(), name="payload")
    cc.serve(blocking=False)
    client = _connect_payload()
    try:
        held = cc.hold(client.payload)(4096)
        assert cc.hold_stats()["active_holds"] == 1
        del held
        gc.collect()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(client)
