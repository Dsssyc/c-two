from __future__ import annotations

import pytest

import c_two as cc
from c_two.error import ResourceSerializeOutput
from c_two.config import settings
from c_two.transport.registry import _ProcessRegistry


@pytest.fixture(autouse=True)
def _cleanup(monkeypatch):
    old_threshold = settings._shm_threshold  # noqa: SLF001
    monkeypatch.delenv("C2_RELAY_ADDRESS", raising=False)
    cc.shutdown()
    _ProcessRegistry._instance = None  # noqa: SLF001
    yield
    settings._shm_threshold = old_threshold  # noqa: SLF001
    cc.shutdown()
    _ProcessRegistry._instance = None  # noqa: SLF001


def _non_inline_holds(stats: dict) -> int:
    return sum(
        stats["by_storage"][storage]["active_holds"]
        for storage in ("shm", "handle", "file_spill")
    )


def _connect(crm_type: type, name: str):
    address = cc.server_address()
    assert address is not None
    assert address.startswith("ipc://")
    return cc.connect(crm_type, name=name, address=address)


@cc.transferable
class BytesResponse:
    data: memoryview | bytes

    def serialize(value: "BytesResponse") -> bytes:
        return bytes(value.data)

    def deserialize(data: memoryview) -> "BytesResponse":
        return BytesResponse(bytes(data))

    def from_buffer(data: memoryview) -> "BytesResponse":
        return BytesResponse(data)


@cc.crm(namespace="cc.test.response_allocation_bytes", version="0.1.0")
class BytesResponseCRM:
    @cc.transfer(output=BytesResponse)
    def payload(self, size: int) -> BytesResponse:
        ...


class BytesResponseResource:
    def payload(self, size: int) -> BytesResponse:
        return BytesResponse(b"b" * size)


@cc.transferable
class MemoryViewResponse:
    data: memoryview | bytearray | bytes

    def serialize(value: "MemoryViewResponse") -> memoryview:
        return memoryview(value.data)

    def deserialize(data: memoryview) -> "MemoryViewResponse":
        return MemoryViewResponse(bytes(data))

    def from_buffer(data: memoryview) -> "MemoryViewResponse":
        return MemoryViewResponse(data)


@cc.crm(namespace="cc.test.response_allocation_memoryview", version="0.1.0")
class MemoryViewResponseCRM:
    @cc.transfer(output=MemoryViewResponse)
    def payload(self, size: int) -> MemoryViewResponse:
        ...


class MemoryViewResponseResource:
    def payload(self, size: int) -> MemoryViewResponse:
        return MemoryViewResponse(bytearray(b"m" * size))


def test_large_bytes_response_uses_native_non_inline_storage_without_relay(monkeypatch):
    settings.shm_threshold = 1024
    cc.register(BytesResponseCRM, BytesResponseResource(), name="bytes_response")
    cc.serve(blocking=False)
    monkeypatch.setenv("C2_RELAY_ADDRESS", "http://127.0.0.1:9")
    client = _connect(BytesResponseCRM, "bytes_response")
    try:
        held = cc.hold(client.payload)(8192)
        try:
            assert bytes(held.value.data[:4]) == b"bbbb"
            stats = cc.hold_stats()
            assert stats["active_holds"] == 1
            assert stats["total_held_bytes"] == 8192
            assert _non_inline_holds(stats) == 1
            assert stats["by_storage"]["inline"]["active_holds"] == 0
        finally:
            held.release()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(client)


def test_large_memoryview_response_uses_native_non_inline_storage():
    settings.shm_threshold = 1024
    cc.register(
        MemoryViewResponseCRM,
        MemoryViewResponseResource(),
        name="memoryview_response",
    )
    cc.serve(blocking=False)
    client = _connect(MemoryViewResponseCRM, "memoryview_response")
    try:
        held = cc.hold(client.payload)(8192)
        try:
            assert bytes(held.value.data[:4]) == b"mmmm"
            assert isinstance(held.value.data, memoryview)
            stats = cc.hold_stats()
            assert stats["active_holds"] == 1
            assert stats["total_held_bytes"] == 8192
            assert _non_inline_holds(stats) == 1
            assert stats["by_storage"]["inline"]["active_holds"] == 0
        finally:
            held.release()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(client)


def test_small_response_remains_inline():
    settings.shm_threshold = 1024
    cc.register(BytesResponseCRM, BytesResponseResource(), name="small_response")
    cc.serve(blocking=False)
    client = _connect(BytesResponseCRM, "small_response")
    try:
        held = cc.hold(client.payload)(32)
        try:
            assert bytes(held.value.data[:4]) == b"bbbb"
            stats = cc.hold_stats()
            assert stats["active_holds"] == 1
            assert stats["total_held_bytes"] == 32
            assert stats["by_storage"]["inline"]["active_holds"] == 1
            assert _non_inline_holds(stats) == 0
        finally:
            held.release()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(client)


def test_response_over_max_payload_size_returns_resource_output_error():
    settings.shm_threshold = 1024
    cc.set_server(
        ipc_overrides={
            "pool_segment_size": 8192,
            "max_payload_size": 8192,
            "max_frame_size": 16384,
        }
    )
    cc.register(
        BytesResponseCRM,
        BytesResponseResource(),
        name="response_over_max_payload",
    )
    cc.serve(blocking=False)
    client = _connect(BytesResponseCRM, "response_over_max_payload")
    try:
        with pytest.raises(ResourceSerializeOutput) as exc_info:
            client.payload(8193)
        assert "response payload size 8193 exceeds max_payload_size 8192" in str(
            exc_info.value
        )
    finally:
        cc.close(client)
