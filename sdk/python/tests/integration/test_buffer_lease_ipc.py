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
    monkeypatch.delenv("C2_RELAY_ADDRESS", raising=False)
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


@cc.crm(namespace="cc.test.buffer_lease_input", version="0.1.0")
class InputHoldCRM:
    @cc.transfer(input=BytesView, output=BytesView, buffer="hold")
    def echo(self, data: BytesView) -> BytesView:
        ...


class InputHoldResource:
    def __init__(self) -> None:
        self.retained = []

    def echo(self, data: BytesView) -> BytesView:
        self.retained.append(data.data)
        return BytesView(data.data[:4])


def test_resource_input_hold_counts_inline_or_shm_until_request_buffer_release():
    settings.shm_threshold = 1024
    resource = InputHoldResource()
    cc.register(InputHoldCRM, resource, name="input_hold")
    cc.serve(blocking=False)
    address = cc.server_address()
    assert address is not None
    client = cc.connect(InputHoldCRM, name="input_hold", address=address)
    try:
        result = client.echo(BytesView(b"x" * 8192))
        assert bytes(result.data) == b"xxxx"
        stats = cc.hold_stats()
        assert stats["by_direction"]["resource_input"]["active_holds"] >= 1
        resource.retained.clear()
        gc.collect()
    finally:
        cc.close(client)
    assert cc.hold_stats()["by_direction"]["resource_input"]["active_holds"] == 0


@cc.crm(namespace="cc.test.grid_like_buffer_lease", version="0.1.0")
class GridLikeCRM:
    @cc.transfer(output=BytesView)
    def subdivide_grids(self, count: int) -> BytesView:
        ...


class GridLikeResource:
    def subdivide_grids(self, count: int) -> BytesView:
        # Simulates variable-size serialized Arrow/grid output without requiring pyarrow.
        return BytesView(("grid:" + ",".join(str(i) for i in range(count))).encode())


def test_grid_like_hold_is_storage_transparent_for_inline_and_shm():
    settings.shm_threshold = 256
    cc.register(GridLikeCRM, GridLikeResource(), name="grid_like")
    cc.serve(blocking=False)
    address = cc.server_address()
    assert address is not None
    grid = cc.connect(GridLikeCRM, name="grid_like", address=address)
    try:
        small = cc.hold(grid.subdivide_grids)(3)
        try:
            assert bytes(small.value.data).startswith(b"grid:0,1,2")
            small_stats = cc.hold_stats()
            assert small_stats["by_storage"]["inline"]["active_holds"] == 1
        finally:
            small.release()

        large = cc.hold(grid.subdivide_grids)(300)
        try:
            assert bytes(large.value.data[:6]) == b"grid:0"
            large_stats = cc.hold_stats()
            assert large_stats["active_holds"] == 1
            assert (
                large_stats["by_storage"]["shm"]["active_holds"]
                + large_stats["by_storage"]["handle"]["active_holds"]
                + large_stats["by_storage"]["file_spill"]["active_holds"]
            ) >= 1
        finally:
            large.release()
        assert cc.hold_stats()["active_holds"] == 0
    finally:
        cc.close(grid)
