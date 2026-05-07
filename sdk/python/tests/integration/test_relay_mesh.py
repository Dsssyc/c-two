"""Integration tests for the relay mesh resource discovery system.

Tests cover:
- Single-relay register/resolve/unregister
- Two-relay gossip route propagation
- Route withdrawal propagation across peers
- Peer discovery and listing
"""
from __future__ import annotations

import json
import time
import urllib.error
import urllib.request

import pytest


def _http_post(url: str, body: dict) -> urllib.request.Request:
    return urllib.request.Request(
        url,
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )


def _register_body(name: str, server_id: str, address: str) -> dict[str, str]:
    return {
        "name": name,
        "server_id": server_id,
        "server_instance_id": f"{server_id}-instance",
        "address": address,
    }


def _http_get_json(url: str):
    with urllib.request.urlopen(url, timeout=5) as resp:
        return json.loads(resp.read())


def _wait_for_json(getter, predicate, *, timeout: float = 8.0, interval: float = 0.1):
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    last_value = None
    while time.monotonic() < deadline:
        try:
            value = getter()
            last_value = value
            if predicate(value):
                return value
        except Exception as exc:
            last_error = exc
        time.sleep(interval)
    if last_error is not None:
        raise AssertionError(f"Timed out waiting for state; last error: {last_error}") from last_error
    raise AssertionError(f"Timed out waiting for state; last value: {last_value!r}")


def _wait_for_route(url: str, name: str, *, timeout: float = 8.0):
    return _wait_for_json(
        lambda: _http_get_json(f"{url}/_resolve/{name}"),
        lambda routes: any(route["name"] == name for route in routes),
        timeout=timeout,
    )


def _wait_for_route_missing(url: str, name: str, *, timeout: float = 8.0) -> None:
    deadline = time.monotonic() + timeout
    last_routes = None
    while time.monotonic() < deadline:
        try:
            last_routes = _http_get_json(f"{url}/_resolve/{name}")
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return
            raise
        if not any(route["name"] == name for route in last_routes):
            return
        time.sleep(0.1)
    raise AssertionError(f"Timed out waiting for {name!r} to disappear; last routes: {last_routes!r}")


def _wait_for_peer(url: str, relay_id: str, *, timeout: float = 8.0):
    return _wait_for_json(
        lambda: _http_get_json(f"{url}/_peers"),
        lambda peers: any(peer["relay_id"] == relay_id for peer in peers),
        timeout=timeout,
    )


class TestSingleRelay:
    """Tests with one relay."""

    def test_register_and_resolve(self, start_c3_relay):
        relay = start_c3_relay(skip_ipc_validation=True)
        base = relay.url

        # Register a mock upstream.
        req = _http_post(
            f"{base}/_register",
            _register_body("grid", "test-grid", "ipc://test_grid"),
        )
        with urllib.request.urlopen(req, timeout=5) as resp:
            assert resp.status == 201

        # Resolve.
        routes = _http_get_json(f"{base}/_resolve/grid")
        assert len(routes) >= 1
        assert routes[0]["name"] == "grid"

        # Unregister.
        req = _http_post(
            f"{base}/_unregister",
            {"name": "grid", "server_id": "test-grid"},
        )
        with urllib.request.urlopen(req, timeout=5) as resp:
            assert resp.status == 200

        # Resolve should now 404.
        with pytest.raises(urllib.error.HTTPError, match="404"):
            urllib.request.urlopen(f"{base}/_resolve/grid", timeout=5)

    def test_peers_empty(self, start_c3_relay):
        relay = start_c3_relay(skip_ipc_validation=True)

        peers = _http_get_json(f"{relay.url}/_peers")
        assert peers == []


class TestTwoRelayMesh:
    """Tests with two relays in a mesh."""

    def test_gossip_route_propagation(self, start_c3_relay):
        relay_a = start_c3_relay(
            relay_id="relay-a",
            skip_ipc_validation=True,
        )
        url_a = relay_a.url

        relay_b = start_c3_relay(
            relay_id="relay-b",
            seeds=[url_a],
            skip_ipc_validation=True,
        )
        url_b = relay_b.url
        _wait_for_peer(url_a, "relay-b")

        # Register on relay A.
        req = _http_post(
            f"{url_a}/_register",
            _register_body("grid", "grid-a", "ipc://grid_a"),
        )
        urllib.request.urlopen(req, timeout=5)

        # Resolve on relay B should find "grid".
        routes = _wait_for_route(url_b, "grid")
        assert len(routes) >= 1
        assert routes[0]["name"] == "grid"
        assert routes[0]["server_instance_id"] is None

        # Both relays should see each other as peers.
        peers_a = _http_get_json(f"{url_a}/_peers")
        assert any(p["relay_id"] == "relay-b" for p in peers_a)

    def test_route_withdraw_propagation(self, start_c3_relay):
        relay_a = start_c3_relay(
            relay_id="relay-a",
            skip_ipc_validation=True,
        )
        url_a = relay_a.url

        relay_b = start_c3_relay(
            relay_id="relay-b",
            seeds=[url_a],
            skip_ipc_validation=True,
        )
        url_b = relay_b.url
        _wait_for_peer(url_a, "relay-b")

        # Register on A.
        req = _http_post(
            f"{url_a}/_register",
            _register_body("net", "net-a", "ipc://net_a"),
        )
        urllib.request.urlopen(req, timeout=5)

        # Verify B can resolve.
        routes = _wait_for_route(url_b, "net")
        assert len(routes) >= 1

        # Unregister on A.
        req = _http_post(
            f"{url_a}/_unregister",
            {"name": "net", "server_id": "net-a"},
        )
        urllib.request.urlopen(req, timeout=5)

        # Resolve on B should now 404.
        _wait_for_route_missing(url_b, "net")

    def test_peer_discovery_bidirectional(self, start_c3_relay):
        """Both relays should discover each other after join."""
        relay_a = start_c3_relay(
            relay_id="relay-a",
            skip_ipc_validation=True,
        )
        url_a = relay_a.url

        relay_b = start_c3_relay(
            relay_id="relay-b",
            seeds=[url_a],
            skip_ipc_validation=True,
        )
        url_b = relay_b.url

        # A sees B.
        peers_a = _wait_for_peer(url_a, "relay-b")
        assert any(p["relay_id"] == "relay-b" for p in peers_a)

        # B sees A.
        peers_b = _wait_for_peer(url_b, "relay-a")
        assert any(p["relay_id"] == "relay-a" for p in peers_b)

    def test_resolve_returns_relay_url(self, start_c3_relay):
        """/_resolve response includes the relay_url of the registering relay."""
        relay_a = start_c3_relay(
            relay_id="relay-a",
            skip_ipc_validation=True,
        )
        url_a = relay_a.url

        relay_b = start_c3_relay(
            relay_id="relay-b",
            seeds=[url_a],
            skip_ipc_validation=True,
        )
        url_b = relay_b.url
        _wait_for_peer(url_a, "relay-b")

        # Register on A.
        req = _http_post(
            f"{url_a}/_register",
            _register_body("solver", "solver-a", "ipc://solver_a"),
        )
        urllib.request.urlopen(req, timeout=5)

        # Resolve on B — relay_url should point to A.
        routes = _wait_for_route(url_b, "solver")
        assert len(routes) == 1
        assert routes[0]["relay_url"] == url_a
