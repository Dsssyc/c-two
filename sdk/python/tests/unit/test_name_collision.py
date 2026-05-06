"""Unit tests for CRM routing name collision prevention.

Verifies that:
- Duplicate names are rejected at both Server and registry level.
- Same CRM class can be registered under different names.
- Different CRM classes can co-exist with unique names.
"""
from __future__ import annotations

import os
import uuid

import pytest

from c_two.config import settings
from c_two.transport import Server, ConcurrencyConfig, ConcurrencyMode
from c_two.transport.registry import _ProcessRegistry
from c_two.transport.server.native import CRMSlot, NativeServerBridge
from c_two.transport.wire import MethodTable

from tests.fixtures.ihello import Hello
from tests.fixtures.hello import HelloImpl
from tests.fixtures.counter import Counter, CounterImpl


_IPC_SOCK_DIR = os.environ.get('CC_IPC_SOCK_DIR', '/tmp/c_two_ipc')


def _unique_addr() -> str:
    return f'ipc://test_collision_{uuid.uuid4().hex[:12]}'


# ---------------------------------------------------------------------------
# Server-level name collision
# ---------------------------------------------------------------------------

class TestServerNameCollision:

    def test_duplicate_name_raises(self):
        """Registering two CRMs under the same name raises ValueError."""
        server = Server(bind_address=_unique_addr())
        server.register_crm(Hello, HelloImpl(), name='my_hello')
        with pytest.raises(ValueError, match='already registered'):
            server.register_crm(Hello, HelloImpl(), name='my_hello')
        server.shutdown()

    def test_duplicate_name_different_icrm_raises(self):
        """Different CRM classes under the same name also raises."""
        server = Server(bind_address=_unique_addr())
        server.register_crm(Hello, HelloImpl(), name='shared_name')
        with pytest.raises(ValueError, match='already registered'):
            server.register_crm(Counter, CounterImpl(), name='shared_name')
        server.shutdown()

    def test_same_icrm_different_names_ok(self):
        """Same CRM class can be registered under distinct names."""
        server = Server(bind_address=_unique_addr())
        server.register_crm(Hello, HelloImpl(), name='hello_a')
        server.register_crm(Hello, HelloImpl(), name='hello_b')
        assert set(server.names) == {'hello_a', 'hello_b'}
        server.shutdown()

    def test_different_icrm_unique_names_ok(self):
        """Different CRM classes with unique names coexist."""
        server = Server(bind_address=_unique_addr())
        server.register_crm(Hello, HelloImpl(), name='hello')
        server.register_crm(Counter, CounterImpl(), name='counter')
        assert set(server.names) == {'hello', 'counter'}
        server.shutdown()

    def test_reregister_after_unregister(self):
        """After unregistering, the same name can be reused."""
        server = Server(bind_address=_unique_addr())
        server.register_crm(Hello, HelloImpl(), name='reuse_me')
        server.unregister_crm('reuse_me')
        # Should succeed now
        server.register_crm(Counter, CounterImpl(), name='reuse_me')
        assert server.names == ['reuse_me']
        server.shutdown()

    def test_unregister_nonexistent_raises(self):
        """Unregistering a name that was never registered raises KeyError."""
        server = Server(bind_address=_unique_addr())
        with pytest.raises(KeyError):
            server.unregister_crm('does_not_exist')
        server.shutdown()

    def test_unregister_removes_native_route_before_python_shutdown(self):
        """Native route removal happens before Python CRM shutdown callbacks."""
        events: list[str] = []

        class FakeRuntimeSession:
            def unregister_route(self, rust_server, name, relay_address=None):  # noqa: ANN001, ARG002
                events.append(f'native_unregister:{name}:{relay_address}')
                return {
                    'route_name': name,
                    'local_removed': True,
                    'relay_error': None,
                }

        class FakeScheduler:
            def shutdown(self) -> None:
                events.append('scheduler_close')

        class FakeResource:
            def cleanup(self) -> None:
                events.append('crm_shutdown')

        class FakeCRM:
            resource = FakeResource()

        bridge = object.__new__(NativeServerBridge)
        bridge._slots = {  # noqa: SLF001
            'grid': CRMSlot(
                name='grid',
                crm_instance=FakeCRM(),
                direct_instance=FakeResource(),
                method_table=MethodTable(),
                scheduler=FakeScheduler(),
                methods=[],
                shutdown_method='cleanup',
            ),
        }
        bridge._slots_lock = __import__('threading').Lock()  # noqa: SLF001
        bridge._default_name = 'grid'  # noqa: SLF001
        bridge._rust_server = object()  # noqa: SLF001

        bridge.unregister_crm(
            'grid',
            runtime_session=FakeRuntimeSession(),
            relay_address='http://relay.test',
        )

        assert events == [
            'native_unregister:grid:http://relay.test',
            'scheduler_close',
            'crm_shutdown',
        ]
        assert bridge.names == []

    def test_unregister_keeps_local_slot_when_native_unregister_missing(self):
        """Missing native route should leave the Python slot intact."""
        events: list[str] = []

        class FakeRuntimeSession:
            def unregister_route(self, rust_server, name, relay_address=None):  # noqa: ANN001, ARG002
                events.append('native_unregister_missing')
                return {
                    'route_name': name,
                    'local_removed': False,
                    'relay_error': None,
                }

        class FakeScheduler:
            def shutdown(self) -> None:
                events.append('scheduler_close')

        class FakeResource:
            def cleanup(self) -> None:
                events.append('crm_shutdown')

        class FakeCRM:
            resource = FakeResource()

        bridge = object.__new__(NativeServerBridge)
        bridge._slots = {  # noqa: SLF001
            'grid': CRMSlot(
                name='grid',
                crm_instance=FakeCRM(),
                direct_instance=FakeResource(),
                method_table=MethodTable(),
                scheduler=FakeScheduler(),
                methods=[],
                shutdown_method='cleanup',
            ),
        }
        bridge._slots_lock = __import__('threading').Lock()  # noqa: SLF001
        bridge._default_name = 'grid'  # noqa: SLF001
        bridge._rust_server = object()  # noqa: SLF001

        with pytest.raises(KeyError, match='Name not registered in native server'):
            bridge.unregister_crm('grid', runtime_session=FakeRuntimeSession())

        assert events == ['native_unregister_missing']
        assert bridge.names == ['grid']

    def test_shutdown_uses_native_removed_routes_before_python_shutdown(self):
        """Server shutdown closes native route handles before CRM callbacks."""
        events: list[str] = []

        class FakeRuntimeSession:
            def shutdown(self, rust_server, route_names=None, relay_address=None):  # noqa: ANN001, ARG002
                events.append(
                    f'native_shutdown:{list(route_names or [])}:{relay_address}'
                )
                return {
                    'removed_routes': list(route_names or []),
                    'relay_errors': [],
                    'server_was_started': True,
                    'ipc_clients_drained': True,
                    'http_clients_drained': False,
                }

        class FakeRustServer:
            def shutdown(self) -> None:
                events.append('rust_shutdown')

        class FakeScheduler:
            def shutdown(self) -> None:
                events.append('scheduler_close')

        class FakeResource:
            def cleanup(self) -> None:
                events.append('crm_shutdown')

        class FakeCRM:
            resource = FakeResource()

        bridge = object.__new__(NativeServerBridge)
        bridge._slots = {  # noqa: SLF001
            'grid': CRMSlot(
                name='grid',
                crm_instance=FakeCRM(),
                direct_instance=FakeResource(),
                method_table=MethodTable(),
                scheduler=FakeScheduler(),
                methods=[],
                shutdown_method='cleanup',
            ),
        }
        bridge._slots_lock = __import__('threading').Lock()  # noqa: SLF001
        bridge._default_name = 'grid'  # noqa: SLF001
        bridge._rust_server = FakeRustServer()  # noqa: SLF001
        bridge._started = True  # noqa: SLF001

        bridge.shutdown(
            runtime_session=FakeRuntimeSession(),
            relay_address='http://relay.test',
        )

        assert events == [
            "native_shutdown:['grid']:http://relay.test",
            'scheduler_close',
            'crm_shutdown',
            'rust_shutdown',
        ]
        assert bridge.names == []

    def test_shutdown_keeps_slot_when_native_does_not_remove_route(self):
        """Python cleanup only runs for routes named in the native outcome."""
        events: list[str] = []

        class FakeRuntimeSession:
            def shutdown(self, rust_server, route_names=None, relay_address=None):  # noqa: ANN001, ARG002
                events.append(f'native_shutdown:{list(route_names or [])}')
                return {
                    'removed_routes': [],
                    'relay_errors': [],
                    'server_was_started': True,
                    'ipc_clients_drained': True,
                    'http_clients_drained': False,
                }

        class FakeRustServer:
            def shutdown(self) -> None:
                events.append('rust_shutdown')

        class FakeScheduler:
            def shutdown(self) -> None:
                events.append('scheduler_close')

        class FakeResource:
            def cleanup(self) -> None:
                events.append('crm_shutdown')

        class FakeCRM:
            resource = FakeResource()

        bridge = object.__new__(NativeServerBridge)
        bridge._slots = {  # noqa: SLF001
            'grid': CRMSlot(
                name='grid',
                crm_instance=FakeCRM(),
                direct_instance=FakeResource(),
                method_table=MethodTable(),
                scheduler=FakeScheduler(),
                methods=[],
                shutdown_method='cleanup',
            ),
        }
        bridge._slots_lock = __import__('threading').Lock()  # noqa: SLF001
        bridge._default_name = 'grid'  # noqa: SLF001
        bridge._rust_server = FakeRustServer()  # noqa: SLF001
        bridge._started = True  # noqa: SLF001

        bridge.shutdown(runtime_session=FakeRuntimeSession())

        assert events == ["native_shutdown:['grid']", 'rust_shutdown']
        assert bridge.names == ['grid']

    def test_register_rolls_back_python_slot_when_rust_registration_fails(self, monkeypatch):
        events: list[str] = []

        class FakeRustServer:
            def register_route(
                self,
                name,
                dispatcher,
                methods,
                access_map,
                concurrency_mode,
                max_pending,
                max_workers,
            ):  # noqa: ARG002
                events.append(
                    f'rust_register:{concurrency_mode}:{max_pending}:{max_workers}'
                )
                raise RuntimeError('boom')

            def unregister_route(self, name: str) -> bool:
                events.append(f'rust_unregister:{name}')
                return True

            def is_started(self) -> bool:
                return True

        class FakeCRM:
            pass

        bridge = object.__new__(NativeServerBridge)
        bridge._slots = {}  # noqa: SLF001
        bridge._slots_lock = __import__('threading').Lock()  # noqa: SLF001
        bridge._default_name = None  # noqa: SLF001
        bridge._started = False  # noqa: SLF001
        bridge._hold_registry = object()  # noqa: SLF001
        bridge._hold_sweep_interval = 10  # noqa: SLF001
        bridge._shm_threshold = 1024  # noqa: SLF001
        bridge._default_concurrency = ConcurrencyConfig(max_workers=1)  # noqa: SLF001
        bridge._rust_server = FakeRustServer()  # noqa: SLF001

        def fake_create(_crm_class, crm_instance):
            return crm_instance

        def fake_discover(_crm_class):
            return []

        def fake_extract_namespace(_crm_class):
            return 'unit'

        def fake_make_dispatcher(route_name, slot):  # noqa: ARG001
            return lambda *_args, **_kwargs: None

        bridge._create_crm_instance = fake_create  # noqa: SLF001
        bridge._discover_methods = fake_discover  # noqa: SLF001
        bridge._extract_namespace = fake_extract_namespace  # noqa: SLF001
        bridge._make_dispatcher = fake_make_dispatcher  # noqa: SLF001

        with pytest.raises(RuntimeError, match='boom'):
            bridge.register_crm(FakeCRM, FakeCRM(), name='grid')

        assert events == ['rust_register:read_parallel:None:1']
        assert bridge.names == []


# ---------------------------------------------------------------------------
# Registry-level name collision
# ---------------------------------------------------------------------------

class TestRegistryNameCollision:

    @pytest.fixture(autouse=True)
    def _clean_registry(self):
        """Reset the global registry before/after each test."""
        _ProcessRegistry.reset()
        settings.relay_address = None
        yield
        _ProcessRegistry.reset()
        settings.relay_address = None

    def test_duplicate_name_raises(self):
        registry = _ProcessRegistry.get()
        registry.register(Hello, HelloImpl(), name='hello')
        with pytest.raises(ValueError, match='already registered'):
            registry.register(Hello, HelloImpl(), name='hello')

    def test_duplicate_name_different_icrm_raises(self):
        registry = _ProcessRegistry.get()
        registry.register(Hello, HelloImpl(), name='shared')
        with pytest.raises(ValueError, match='already registered'):
            registry.register(Counter, CounterImpl(), name='shared')

    def test_same_icrm_different_names_ok(self):
        registry = _ProcessRegistry.get()
        registry.register(Hello, HelloImpl(), name='hello_1')
        registry.register(Hello, HelloImpl(), name='hello_2')
        assert set(registry.names) == {'hello_1', 'hello_2'}

    def test_reregister_after_unregister(self):
        registry = _ProcessRegistry.get()
        registry.register(Hello, HelloImpl(), name='temp')
        registry.unregister('temp')
        registry.register(Counter, CounterImpl(), name='temp')
        assert registry.names == ['temp']

    def test_unregister_uses_native_outcome_and_preserves_slot_on_failure(self):
        registry = _ProcessRegistry.get()
        registry.register(Hello, HelloImpl(), name='temp')

        class FailingServer:
            @property
            def names(self) -> list[str]:
                return ['temp']

            def unregister_crm(self, name: str, **kwargs) -> None:  # noqa: ARG002
                assert kwargs['runtime_session'] is registry._runtime_session  # noqa: SLF001
                raise RuntimeError('native unregister failed')

            def shutdown(self) -> None:
                pass

        registry._server = FailingServer()  # noqa: SLF001

        with pytest.raises(RuntimeError, match='native unregister failed'):
            registry.unregister('temp')

        assert registry.names == ['temp']

    def test_registry_no_longer_exposes_legacy_relay_unregister(self):
        """Relay unregister cleanup is returned by native RuntimeSession."""
        registry = _ProcessRegistry.get()
        registry.register(Hello, HelloImpl(), name='temp')

        assert not hasattr(registry, '_relay_unregister')

        registry.unregister('temp')
        registry.shutdown()

    def test_unregister_nonexistent_raises(self):
        registry = _ProcessRegistry.get()
        with pytest.raises(KeyError):
            registry.unregister('nope')
