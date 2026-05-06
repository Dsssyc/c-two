"""Remote IPC scheduler configuration is enforced by Rust c2-server."""
from __future__ import annotations

import pickle
import threading
import time

import pytest

import c_two as cc
from c_two.config import settings
from c_two.transport.registry import _ProcessRegistry
from c_two.transport.server.scheduler import ConcurrencyConfig, ConcurrencyMode


@pytest.fixture(autouse=True)
def _clean_env_and_registry(monkeypatch):
    previous_override_relay = settings._relay_address  # noqa: SLF001
    previous_override_shm_threshold = settings._shm_threshold  # noqa: SLF001
    settings.relay_address = None
    monkeypatch.delenv('C2_RELAY_ADDRESS', raising=False)
    yield
    try:
        cc.shutdown()
    except Exception:
        pass
    _ProcessRegistry._instance = None  # noqa: SLF001
    settings._relay_address = previous_override_relay  # noqa: SLF001
    settings._shm_threshold = previous_override_shm_threshold  # noqa: SLF001


def _direct_client(crm_type: type, name: str):
    address = cc.server_address()
    assert address is not None
    assert address.startswith('ipc://')
    return cc.connect(crm_type, name=name, address=address)


def _join_all(threads: list[threading.Thread]) -> None:
    for thread in threads:
        thread.join(timeout=5)
    alive = [thread.name for thread in threads if thread.is_alive()]
    assert not alive, f'threads did not finish: {alive}'


@cc.transferable
class Delay:
    value: float

    def serialize(value: 'Delay') -> bytes:
        return pickle.dumps(value.value)

    def deserialize(buf: memoryview) -> 'Delay':
        return Delay(float(pickle.loads(bytes(buf))))


@cc.transferable
class Window:
    start: float
    end: float

    def serialize(value: 'Window') -> bytes:
        return pickle.dumps((value.start, value.end))

    def deserialize(buf: memoryview) -> 'Window':
        start, end = pickle.loads(bytes(buf))
        return Window(float(start), float(end))


@cc.transferable
class Count:
    value: int

    def serialize(value: 'Count') -> bytes:
        return pickle.dumps(value.value)

    def deserialize(buf: memoryview) -> 'Count':
        return Count(int(pickle.loads(bytes(buf))))


@cc.crm(namespace='test.remote.scheduler', version='0.1.0')
class RemoteSchedulerProbe:
    @cc.read
    @cc.transfer(input=Delay, output=Window)
    def read_op(self, delay: Delay) -> Window: ...

    @cc.write
    @cc.transfer(input=Delay, output=Window)
    def write_op(self, delay: Delay) -> Window: ...

    @cc.write
    @cc.transfer(output=Count)
    def max_active(self) -> Count: ...


class RemoteSchedulerProbeImpl:
    def __init__(self):
        self.active = 0
        self.max_seen = 0
        self.lock = threading.Lock()

    def _run(self, delay: Delay) -> Window:
        start = time.monotonic()
        with self.lock:
            self.active += 1
            self.max_seen = max(self.max_seen, self.active)
        try:
            time.sleep(delay.value)
            return Window(start, time.monotonic())
        finally:
            with self.lock:
                self.active -= 1

    def read_op(self, delay: Delay) -> Window:
        return self._run(delay)

    def write_op(self, delay: Delay) -> Window:
        return self._run(delay)

    def max_active(self) -> Count:
        return Count(self.max_seen)


@cc.crm(namespace='test.remote.scheduler.shared', version='0.1.0')
class SharedLimitProbe:
    @cc.write
    @cc.transfer(input=Delay, output=Window)
    def hold(self, delay: Delay) -> Window: ...

    @cc.write
    @cc.transfer(output=Count)
    def probe(self) -> Count: ...


class SharedLimitProbeImpl:
    def __init__(self):
        self.hold_entered = threading.Event()
        self.release = threading.Event()
        self.probe_calls = 0
        self.lock = threading.Lock()

    def hold(self, delay: Delay) -> Window:
        start = time.monotonic()
        self.hold_entered.set()
        self.release.wait(timeout=2)
        time.sleep(delay.value)
        return Window(start, time.monotonic())

    def probe(self) -> Count:
        with self.lock:
            self.probe_calls += 1
            return Count(self.probe_calls)


def test_remote_ipc_exclusive_serializes_reads():
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='remote_exclusive',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.EXCLUSIVE),
    )
    cc.serve(blocking=False)

    errors: list[BaseException] = []

    def worker():
        client = _direct_client(RemoteSchedulerProbe, 'remote_exclusive')
        try:
            client.read_op(Delay(0.05))
        except BaseException as exc:
            errors.append(exc)
        finally:
            cc.close(client)

    threads = [threading.Thread(target=worker) for _ in range(3)]
    for thread in threads:
        thread.start()
    _join_all(threads)

    assert not errors
    probe = _direct_client(RemoteSchedulerProbe, 'remote_exclusive')
    try:
        assert probe.max_active().value == 1
    finally:
        cc.close(probe)


def test_remote_ipc_parallel_allows_concurrent_writes():
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='remote_parallel',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.PARALLEL),
    )
    cc.serve(blocking=False)

    errors: list[BaseException] = []

    def worker():
        client = _direct_client(RemoteSchedulerProbe, 'remote_parallel')
        try:
            client.write_op(Delay(0.08))
        except BaseException as exc:
            errors.append(exc)
        finally:
            cc.close(client)

    threads = [threading.Thread(target=worker) for _ in range(3)]
    for thread in threads:
        thread.start()
    _join_all(threads)

    assert not errors
    probe = _direct_client(RemoteSchedulerProbe, 'remote_parallel')
    try:
        assert probe.max_active().value >= 2
    finally:
        cc.close(probe)


def test_remote_ipc_read_parallel_allows_concurrent_reads():
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='remote_read_parallel_reads',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.READ_PARALLEL),
    )
    cc.serve(blocking=False)

    read_errors: list[BaseException] = []

    def read_worker():
        client = _direct_client(RemoteSchedulerProbe, 'remote_read_parallel_reads')
        try:
            client.read_op(Delay(0.05))
        except BaseException as exc:
            read_errors.append(exc)
        finally:
            cc.close(client)

    readers = [threading.Thread(target=read_worker) for _ in range(3)]
    for thread in readers:
        thread.start()
    _join_all(readers)

    assert not read_errors
    client = _direct_client(RemoteSchedulerProbe, 'remote_read_parallel_reads')
    try:
        assert client.max_active().value >= 2
    finally:
        cc.close(client)


def test_remote_ipc_read_parallel_serializes_writes():
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='remote_read_parallel_writes',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.READ_PARALLEL),
    )

    write_errors: list[BaseException] = []

    def write_worker():
        client = _direct_client(RemoteSchedulerProbe, 'remote_read_parallel_writes')
        try:
            client.write_op(Delay(0.05))
        except BaseException as exc:
            write_errors.append(exc)
        finally:
            cc.close(client)

    writers = [threading.Thread(target=write_worker) for _ in range(3)]
    for thread in writers:
        thread.start()
    _join_all(writers)

    assert not write_errors
    client = _direct_client(RemoteSchedulerProbe, 'remote_read_parallel_writes')
    try:
        assert client.max_active().value == 1
    finally:
        cc.close(client)


@pytest.mark.parametrize(
    'mode',
    [ConcurrencyMode.PARALLEL, ConcurrencyMode.EXCLUSIVE, ConcurrencyMode.READ_PARALLEL],
)
def test_remote_ipc_registers_all_public_concurrency_modes(mode):
    name = f'remote_mode_{mode.value}'
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name=name,
        concurrency=ConcurrencyConfig(mode=mode),
    )
    cc.serve(blocking=False)
    client = _direct_client(RemoteSchedulerProbe, name)
    try:
        assert client.max_active().value == 0
    finally:
        cc.close(client)


@cc.transferable
class LargePayload:
    data: bytes

    def serialize(value: 'LargePayload') -> bytes:
        return value.data

    def deserialize(buf: memoryview) -> 'LargePayload':
        return LargePayload(bytes(buf))

    def from_buffer(buf: memoryview) -> 'LargePayload':
        base = getattr(buf, 'obj', None)
        is_inline = getattr(base, 'is_inline', None)
        LargePayloadResource.last_buffer_info = (
            type(buf).__name__,
            type(base).__name__ if base is not None else None,
            is_inline,
            len(buf),
        )
        return LargePayload(bytes(buf[:8]))


@cc.crm(namespace='test.remote.scheduler.large', version='0.1.0')
class LargePayloadCRM:
    @cc.transfer(input=LargePayload, output=LargePayload, buffer='hold')
    def echo_large(self, payload: LargePayload) -> LargePayload: ...

    @cc.read
    def last_info(self) -> tuple[str | None, str | None, bool | None, int]: ...


class LargePayloadResource:
    last_buffer_info: tuple[str | None, str | None, bool | None, int] = (
        None, None, None, 0,
    )

    def echo_large(self, payload: LargePayload) -> LargePayload:
        return payload

    def last_info(self) -> tuple[str | None, str | None, bool | None, int]:
        return self.last_buffer_info


def test_remote_ipc_large_hold_input_reaches_python_as_shm_memoryview():
    settings.shm_threshold = 1024
    LargePayloadResource.last_buffer_info = (None, None, None, 0)
    cc.register(
        LargePayloadCRM,
        LargePayloadResource(),
        name='large_zero_copy',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.EXCLUSIVE),
    )
    cc.serve(blocking=False)

    client = _direct_client(LargePayloadCRM, 'large_zero_copy')
    try:
        payload = LargePayload(b'x' * 8192)
        result = client.echo_large(payload)
        assert result.data == b'x' * 8
        view_type, base_type, is_inline, size = client.last_info()
    finally:
        cc.close(client)

    assert view_type == 'memoryview'
    assert base_type == 'ShmBuffer'
    assert is_inline is False
    assert size == 8192


def test_remote_ipc_scheduler_direct_address_ignores_bad_relay(monkeypatch):
    cc.register(
        RemoteSchedulerProbe,
        RemoteSchedulerProbeImpl(),
        name='direct_no_relay_scheduler',
        concurrency=ConcurrencyConfig(mode=ConcurrencyMode.EXCLUSIVE),
    )
    cc.serve(blocking=False)
    direct_address = cc.server_address()
    assert direct_address is not None
    assert direct_address.startswith('ipc://')

    monkeypatch.setenv('C2_RELAY_ADDRESS', 'http://127.0.0.1:9')
    settings.relay_address = None

    client = cc.connect(
        RemoteSchedulerProbe,
        name='direct_no_relay_scheduler',
        address=direct_address,
    )
    try:
        client.write_op(Delay(0.001))
        assert client.max_active().value == 1
    finally:
        cc.close(client)


def test_remote_ipc_scheduler_max_workers_shares_with_local_thread_calls():
    cc.register(
        SharedLimitProbe,
        SharedLimitProbeImpl(),
        name='mixed_limit_workers',
        concurrency=ConcurrencyConfig(
            mode=ConcurrencyMode.PARALLEL,
            max_workers=1,
        ),
    )
    cc.serve(blocking=False)
    direct_address = cc.server_address()
    assert direct_address is not None

    resource = _ProcessRegistry.get()._server._slots['mixed_limit_workers'].crm_instance.resource  # noqa: SLF001
    local = cc.connect(SharedLimitProbe, name='mixed_limit_workers')
    remote = cc.connect(
        SharedLimitProbe,
        name='mixed_limit_workers',
        address=direct_address,
    )
    try:
        errors: list[BaseException] = []
        local_result: list[Window] = []

        def local_worker():
            try:
                local_result.append(local.hold(Delay(0.01)))
            except BaseException as exc:
                errors.append(exc)

        t1 = threading.Thread(target=local_worker)
        t1.start()
        assert resource.hold_entered.wait(timeout=1)

        with pytest.raises(Exception, match='max_workers=1'):
            remote.probe()

        assert resource.probe_calls == 0
        assert not errors
        resource.release.set()
        _join_all([t1])
        assert local_result and local_result[0].end >= local_result[0].start
    finally:
        cc.close(local)
        cc.close(remote)


def test_remote_ipc_scheduler_max_pending_shares_with_local_thread_calls():
    cc.register(
        SharedLimitProbe,
        SharedLimitProbeImpl(),
        name='mixed_limit_pending',
        concurrency=ConcurrencyConfig(
            mode=ConcurrencyMode.PARALLEL,
            max_pending=1,
        ),
    )
    cc.serve(blocking=False)
    direct_address = cc.server_address()
    assert direct_address is not None

    resource = _ProcessRegistry.get()._server._slots['mixed_limit_pending'].crm_instance.resource  # noqa: SLF001
    local = cc.connect(SharedLimitProbe, name='mixed_limit_pending')
    remote = cc.connect(
        SharedLimitProbe,
        name='mixed_limit_pending',
        address=direct_address,
    )
    try:
        errors: list[BaseException] = []

        def local_worker():
            try:
                local.hold(Delay(0.01))
            except BaseException as exc:
                errors.append(exc)

        t1 = threading.Thread(target=local_worker)
        t1.start()
        assert resource.hold_entered.wait(timeout=1)

        with pytest.raises(Exception, match='max_pending=1'):
            remote.probe()

        assert resource.probe_calls == 0
        assert not errors
        resource.release.set()
        _join_all([t1])
    finally:
        cc.close(local)
        cc.close(remote)
