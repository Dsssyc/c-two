"""Typed IPC override schemas backed by the Rust config resolver."""
from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypedDict

from .settings import C2Settings

__all__ = [
    'BaseIPCOverrides',
    'ServerIPCOverrides',
    'ClientIPCOverrides',
]


class BaseIPCOverrides(TypedDict, total=False):
    """Shared IPC code-level overrides for server and client resolution."""

    # Enables the shared-memory buddy pool for large payload transfers.
    pool_enabled: bool
    # Byte size of each SHM pool segment allocated by the buddy pool.
    pool_segment_size: int
    # Maximum number of SHM pool segments the process may create/open.
    max_pool_segments: int
    # Byte size of each segment used when reassembling chunked transfers.
    reassembly_segment_size: int
    # Maximum number of reassembly segments retained for a transfer.
    reassembly_max_segments: int
    # Maximum number of chunks tracked across active reassembly operations.
    max_total_chunks: int
    # Seconds between garbage-collection sweeps for stale chunks.
    chunk_gc_interval: float
    # Fraction of a segment at which payloads switch to chunked transfer.
    chunk_threshold_ratio: float
    # Seconds before an incomplete chunk assembly is considered stale.
    chunk_assembler_timeout: float
    # Maximum bytes accepted for one reassembled payload.
    max_reassembly_bytes: int
    # Byte size of each chunk when chunked transfer is used.
    chunk_size: int


class ServerIPCOverrides(BaseIPCOverrides, total=False):
    """Server-side IPC code-level overrides."""

    # Maximum byte size of one decoded IPC frame accepted by the server.
    max_frame_size: int
    # Maximum byte size of one logical request/response payload.
    max_payload_size: int
    # Maximum number of concurrent pending server requests.
    max_pending_requests: int
    # Seconds before unused server-side pool resources may decay.
    pool_decay_seconds: float
    # Seconds between heartbeat probes; zero disables heartbeat probes.
    heartbeat_interval: float
    # Seconds to wait for a heartbeat response before treating a peer as dead.
    heartbeat_timeout: float


class ClientIPCOverrides(BaseIPCOverrides, total=False):
    """Client-side IPC code-level overrides."""


_SERVER_KEYS = set(ServerIPCOverrides.__annotations__)
_CLIENT_KEYS = set(ClientIPCOverrides.__annotations__)
_FORBIDDEN_IPC_KEYS = {
    'shm_threshold',
}


def _resolve_server_ipc_config(
    ipc_overrides: ServerIPCOverrides | Mapping[str, object] | None = None,
    settings: C2Settings | None = None,
) -> dict[str, Any]:
    """Resolve server IPC config through Rust from explicit overrides only."""
    native = _native_resolver()
    return dict(native.resolve_server_ipc_config(
        _normalize_server_ipc_overrides(ipc_overrides),
        _shm_overrides(settings),
    ))


def _resolve_client_ipc_config(
    ipc_overrides: ClientIPCOverrides | Mapping[str, object] | None = None,
    settings: C2Settings | None = None,
) -> dict[str, Any]:
    """Resolve client IPC config through Rust from explicit overrides only."""
    native = _native_resolver()
    return dict(native.resolve_client_ipc_config(
        _normalize_client_ipc_overrides(ipc_overrides),
        _shm_overrides(settings),
    ))


def _normalize_server_ipc_overrides(
    ipc_overrides: ServerIPCOverrides | Mapping[str, object] | None,
) -> ServerIPCOverrides | None:
    """Return a copied server IPC override map after structural checks."""
    cleaned = _clean_ipc_overrides(ipc_overrides, _SERVER_KEYS)
    return cleaned or None


def _normalize_client_ipc_overrides(
    ipc_overrides: ClientIPCOverrides | Mapping[str, object] | None,
) -> ClientIPCOverrides | None:
    """Return a copied client IPC override map after structural checks."""
    cleaned = _clean_ipc_overrides(ipc_overrides, _CLIENT_KEYS)
    return cleaned or None


def _native_resolver() -> Any:
    from c_two import _native

    return _native


def _clean_ipc_overrides(
    overrides: Mapping[str, object] | None,
    allowed: set[str],
) -> dict[str, object]:
    if overrides is None:
        return {}
    if not isinstance(overrides, Mapping):
        raise TypeError('ipc_overrides must be a mapping')

    keys = set(overrides)
    forbidden = sorted(keys & _FORBIDDEN_IPC_KEYS)
    if forbidden:
        raise ValueError(
            'shm_threshold is a global transport policy; '
            'use set_transport_policy(shm_threshold=...)',
        )

    unknown = sorted(keys - allowed)
    if unknown:
        joined = ', '.join(unknown)
        raise TypeError(f'unknown IPC override option(s): {joined}')

    return {
        str(key): value
        for key, value in overrides.items()
        if value is not None
    }


def _shm_overrides(settings: C2Settings | None) -> dict[str, object]:
    if settings is None:
        from .settings import settings as _settings
        settings = _settings
    if not isinstance(settings, C2Settings):
        raise TypeError('settings must be a C2Settings instance')
    return settings._shm_overrides()  # noqa: SLF001
