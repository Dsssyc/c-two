"""Integration tests for the HTTP relay chain.

Tests the full pipeline: RustHttpClient -> standalone c3 relay -> RustClient -> Server -> CRM

Also tests ``cc.connect(address='http://...')`` end-to-end.
"""
from __future__ import annotations

import os
import threading

import pytest

import httpx

import c_two as cc
from c_two.config.settings import settings
from c_two._native import RustHttpClientPool
from c_two.transport.client.proxy import CRMProxy
from c_two.transport.registry import _ProcessRegistry

from tests.fixtures.hello import HelloImpl
from tests.fixtures.ihello import Hello
from tests.fixtures.counter import Counter, CounterImpl


def _acquire_http(url: str):
    """Acquire a RustHttpClient from the singleton pool."""
    return RustHttpClientPool.instance().acquire(url)


def _release_http(url: str):
    """Release a RustHttpClient reference back to the pool."""
    RustHttpClientPool.instance().release(url)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture(autouse=True)
def _clean_registry():
    """Ensure a clean registry for every test."""
    _ProcessRegistry.reset()
    yield
    _ProcessRegistry.reset()


@pytest.fixture
def relay_stack(start_c3_relay):
    """Start Server + standalone c3 relay and return (relay_url, ipc_address).

    Registers a Hello CRM as 'hello' and a Counter CRM as 'counter'. The relay
    starts empty; upstreams are added through the HTTP control plane.
    """
    # Register CRMs via SOTA API.
    cc.register(Hello, HelloImpl(), name='hello')
    cc.register(Counter, CounterImpl(), name='counter')
    ipc_addr = cc.server_address()

    relay = start_c3_relay()
    relay_url = relay.url

    with httpx.Client(trust_env=False, timeout=5.0) as http:
        for name in ('hello', 'counter'):
            resp = http.post(
                f'{relay_url}/_register',
                json={'name': name, 'address': ipc_addr},
            )
            assert resp.status_code == 201

    yield relay_url, ipc_addr


# ---------------------------------------------------------------------------
# Full-chain tests
# ---------------------------------------------------------------------------

class TestHttpRelayFullChain:
    """End-to-end: HttpClient → Relay → Server → Hello CRM."""

    def test_hello_via_http(self, relay_stack):
        """Simple string call through HTTP relay."""
        relay_url, _ = relay_stack
        client = _acquire_http(relay_url)
        try:
            crm = Hello()
            crm.client = CRMProxy.http(client, 'hello')
            result = crm.greeting('HTTP')
            assert result == 'Hello, HTTP!'
        finally:
            _release_http(relay_url)

    def test_add_via_http(self, relay_stack):
        """Numeric call through HTTP relay."""
        relay_url, _ = relay_stack
        client = _acquire_http(relay_url)
        try:
            crm = Hello()
            crm.client = CRMProxy.http(client, 'hello')
            result = crm.add(42, 58)
            assert result == 100
        finally:
            _release_http(relay_url)

    def test_get_items_via_http(self, relay_stack):
        """List return through HTTP relay."""
        relay_url, _ = relay_stack
        client = _acquire_http(relay_url)
        try:
            crm = Hello()
            crm.client = CRMProxy.http(client, 'hello')
            result = crm.get_items([10, 20, 30])
            assert result == ['item-10', 'item-20', 'item-30']
        finally:
            _release_http(relay_url)

    def test_get_data_transferable_via_http(self, relay_stack):
        """Custom transferable round-trip through HTTP relay."""
        relay_url, _ = relay_stack
        client = _acquire_http(relay_url)
        try:
            crm = Hello()
            crm.client = CRMProxy.http(client, 'hello')
            result = crm.get_data(5)
            assert result.name == 'data-5'
            assert result.value == 50
        finally:
            _release_http(relay_url)

    def test_multi_crm_routing(self, relay_stack):
        """Route to different CRMs by name through HTTP relay."""
        relay_url, _ = relay_stack
        client = _acquire_http(relay_url)
        try:
            # Hello CRM
            hello = HelloImpl()
            hello.client = CRMProxy.http(client, 'hello')
            assert hello.greeting('Route') == 'Hello, Route!'

            # Counter CRM
            counter = CounterImpl()
            counter.client = CRMProxy.http(client, 'counter')
            counter.increment(1)
            counter.increment(1)
            assert counter.get() == 2
        finally:
            _release_http(relay_url)

    def test_health_endpoint(self, relay_stack):
        """GET /health returns OK."""
        relay_url, _ = relay_stack
        client = _acquire_http(relay_url)
        try:
            health = client.health()
            assert health is True
        finally:
            _release_http(relay_url)

    def test_concurrent_http_calls(self, relay_stack):
        """Multiple threads calling through HTTP relay."""
        relay_url, _ = relay_stack
        client = _acquire_http(relay_url)
        errors: list[str] = []
        n_threads = 8
        n_calls = 5

        def worker(tid: int) -> None:
            try:
                crm = Hello()
                crm.client = CRMProxy.http(client, 'hello')
                for i in range(n_calls):
                    result = crm.add(tid, i)
                    if result != tid + i:
                        errors.append(f'T{tid}[{i}]: {result} != {tid + i}')
            except Exception as e:
                errors.append(f'T{tid}: {e}')

        threads = [threading.Thread(target=worker, args=(i,)) for i in range(n_threads)]
        try:
            for t in threads:
                t.start()
            for t in threads:
                t.join(timeout=60)
            assert errors == [], f'Errors: {errors}'
        finally:
            _release_http(relay_url)


# ---------------------------------------------------------------------------
# cc.connect(address='http://...') tests
# ---------------------------------------------------------------------------

class TestCcConnectHttp:
    """Test the ``cc.connect(address='http://...')`` integration."""

    def test_connect_http_mode(self, relay_stack):
        """cc.connect with HTTP address returns http-mode proxy."""
        relay_url, _ = relay_stack
        crm = cc.connect(Hello, name='hello', address=relay_url)
        try:
            assert crm.client._mode == 'http'
            assert crm.client.supports_direct_call is False
        finally:
            cc.close(crm)

    def test_connect_http_call(self, relay_stack):
        """cc.connect with HTTP address can make real CRM calls."""
        relay_url, _ = relay_stack
        crm = cc.connect(Hello, name='hello', address=relay_url)
        try:
            result = crm.greeting('SOTA')
            assert result == 'Hello, SOTA!'
        finally:
            cc.close(crm)

    def test_connect_http_multi_crm(self, relay_stack):
        """cc.connect to different CRMs via HTTP."""
        relay_url, _ = relay_stack

        hello = cc.connect(Hello, name='hello', address=relay_url)
        counter = cc.connect(Counter, name='counter', address=relay_url)
        try:
            assert hello.greeting('Multi') == 'Hello, Multi!'
            counter.increment(1)
            assert counter.get() == 1
        finally:
            cc.close(hello)
            cc.close(counter)

    def test_connect_http_close_releases_pool(self, relay_stack):
        """cc.close releases the HTTP pool reference."""
        relay_url, _ = relay_stack
        registry = _ProcessRegistry.get()

        crm = cc.connect(Hello, name='hello', address=relay_url)
        assert registry._http_pool.refcount(relay_url) == 1

        cc.close(crm)
        assert registry._http_pool.refcount(relay_url) == 0

    def test_connect_http_with_slash_in_name(self, start_c3_relay):
        """CRM names containing '/' (toodle-style resource paths) work over HTTP relay.

        Regression: axum's single-segment ``Path<String>`` extractor would
        404 on a raw ``/_resolve/a/b`` and split ``/{name}/{method}``
        wrong. Both the Python registry and the Rust HTTP client now
        percent-encode ``/`` as ``%2F``.
        """
        slashed_name = 'toodle/grid/0'
        cc.register(Hello, HelloImpl(), name=slashed_name)
        ipc_addr = cc.server_address()

        relay = start_c3_relay()
        relay_url = relay.url
        with httpx.Client(trust_env=False, timeout=5.0) as http:
            resp = http.post(
                f'{relay_url}/_register',
                json={'name': slashed_name, 'address': ipc_addr},
            )
            assert resp.status_code == 201

        # Forced HTTP mode (address explicitly set).
        crm = cc.connect(Hello, name=slashed_name, address=relay_url)
        try:
            assert crm.greeting('Slash') == 'Hello, Slash!'
        finally:
            cc.close(crm)

    def test_relay_traffic_bypasses_system_proxy(self, monkeypatch, start_c3_relay):
        """All relay HTTP traffic must ignore HTTP_PROXY by default.

        Regression: when a user sets HTTP_PROXY/HTTPS_PROXY (e.g. a corporate
        forward proxy or a docker host proxy), Python urllib previously routed
        relay control traffic through it, and proxies are known to normalize
        percent-encoded ``%2F`` in URL paths to ``/`` — which then breaks
        resource names containing ``/``. This test points HTTP_PROXY at a
        black-hole address and verifies that name resolution + a CRM call
        still succeed (i.e., proxy was bypassed).
        """
        # Black-hole proxy: connecting to TEST-NET-1 (RFC 5737) is guaranteed
        # to time out / fail. If the request actually goes through the proxy,
        # the test will fail loudly.
        for var in ('http_proxy', 'https_proxy', 'HTTP_PROXY', 'HTTPS_PROXY'):
            monkeypatch.setenv(var, 'http://192.0.2.1:9')
        for var in ('no_proxy', 'NO_PROXY'):
            monkeypatch.delenv(var, raising=False)

        name = 'proxy/bypass/test'
        relay = start_c3_relay()
        relay_url = relay.url
        previous_relay = settings.relay_address
        registrar = _ProcessRegistry()
        resolver = _ProcessRegistry()
        crm = None
        try:
            registrar.set_relay(relay_url)
            # Registration must use _relay_register via Python _relay_urlopen.
            registrar.register(Hello, HelloImpl(), name=name)
            resolver.set_relay(relay_url)

            # No-address connect must resolve through _resolve_via_relay.
            crm = resolver.connect(Hello, name=name)
            # reqwest call must succeed (Rust HttpClient builder bypasses proxy).
            assert crm.greeting('NoProxy') == 'Hello, NoProxy!'
        finally:
            if crm is not None:
                resolver.close(crm)
            resolver.shutdown()
            settings.relay_address = previous_relay
            registrar.shutdown()
            settings.relay_address = previous_relay


# ---------------------------------------------------------------------------
# Control-plane endpoint tests
# ---------------------------------------------------------------------------

class TestRelayControlPlane:
    """Test the relay control-plane endpoints (/_register, /_unregister, /_routes)."""

    def test_register_via_http_control(self, start_c3_relay):
        """POST /_register adds an upstream and allows calls."""
        cc.register(Hello, HelloImpl(), name='hello')
        ipc_addr = cc.server_address()

        relay = start_c3_relay()
        relay_url = relay.url

        with httpx.Client(trust_env=False, timeout=5.0) as http:
            # Register via control endpoint.
            resp = http.post(
                f'{relay_url}/_register',
                json={'name': 'hello', 'address': ipc_addr},
            )
            assert resp.status_code == 201
            assert resp.json()['registered'] == 'hello'

            # Verify route appears in /_routes.
            resp = http.get(f'{relay_url}/_routes')
            assert resp.status_code == 200
            routes = resp.json()['routes']
            assert any(r['name'] == 'hello' for r in routes)

        # Verify data-plane call works.
        client = _acquire_http(relay_url)
        try:
            crm = Hello()
            crm.client = CRMProxy.http(client, 'hello')
            assert crm.greeting('Control') == 'Hello, Control!'
        finally:
            _release_http(relay_url)

    def test_register_duplicate_upsert(self, start_c3_relay):
        """POST /_register with same name upserts (returns 201)."""
        cc.register(Hello, HelloImpl(), name='hello')
        ipc_addr = cc.server_address()

        relay = start_c3_relay()
        relay_url = relay.url

        with httpx.Client(trust_env=False, timeout=5.0) as http:
            resp = http.post(
                f'{relay_url}/_register',
                json={'name': 'hello', 'address': ipc_addr},
            )
            assert resp.status_code == 201

            # Same-relay duplicate registration uses upsert semantics.
            resp = http.post(
                f'{relay_url}/_register',
                json={'name': 'hello', 'address': ipc_addr},
            )
            assert resp.status_code == 201

    def test_unregister_removes_route(self, start_c3_relay):
        """POST /_unregister removes the route; calls return 404."""
        cc.register(Hello, HelloImpl(), name='hello')
        ipc_addr = cc.server_address()

        relay = start_c3_relay()
        relay_url = relay.url

        with httpx.Client(trust_env=False, timeout=5.0) as http:
            # Register, then unregister.
            http.post(
                f'{relay_url}/_register',
                json={'name': 'hello', 'address': ipc_addr},
            )
            resp = http.post(
                f'{relay_url}/_unregister',
                json={'name': 'hello'},
            )
            assert resp.status_code == 200

            # Verify route is gone.
            resp = http.get(f'{relay_url}/_routes')
            routes = resp.json()['routes']
            assert not any(r['name'] == 'hello' for r in routes)

            # Data-plane call should return 404.
            resp = http.post(
                f'{relay_url}/hello/greeting',
                content=b'test',
            )
            assert resp.status_code == 404

    def test_unregister_missing_404(self, start_c3_relay):
        """POST /_unregister for unknown name returns 404."""
        relay = start_c3_relay(skip_ipc_validation=True)
        relay_url = relay.url

        with httpx.Client(trust_env=False, timeout=5.0) as http:
            resp = http.post(
                f'{relay_url}/_unregister',
                json={'name': 'nonexistent'},
            )
            assert resp.status_code == 404

    def test_health_shows_registered_routes(self, start_c3_relay):
        """GET /health lists all registered route names."""
        cc.register(Hello, HelloImpl(), name='hello')
        cc.register(Counter, CounterImpl(), name='counter')
        ipc_addr = cc.server_address()

        relay = start_c3_relay()
        relay_url = relay.url

        with httpx.Client(trust_env=False, timeout=5.0) as http:
            for name in ('hello', 'counter'):
                resp = http.post(
                    f'{relay_url}/_register',
                    json={'name': name, 'address': ipc_addr},
                )
                assert resp.status_code == 201

            resp = http.get(f'{relay_url}/health')
            health = resp.json()
            assert health['status'] == 'ok'
            assert set(health['routes']) == {'hello', 'counter'}

    def test_call_unknown_route_404(self, start_c3_relay):
        """POST /{route}/{method} for unregistered route returns 404."""
        relay = start_c3_relay(skip_ipc_validation=True)
        relay_url = relay.url

        with httpx.Client(trust_env=False, timeout=5.0) as http:
            resp = http.post(
                f'{relay_url}/nonexistent/method',
                content=b'data',
            )
            assert resp.status_code == 404
            assert 'nonexistent' in resp.json()['error']
