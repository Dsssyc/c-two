"""Frozen IPC configuration dataclasses backed by the Rust config resolver."""
from __future__ import annotations

from dataclasses import dataclass, field, fields
import math
from typing import Any

from .settings import C2Settings


@dataclass(frozen=True)
class BaseIPCConfig:
    """Shared IPC configuration for both server and client."""

    pool_enabled: bool = True
    pool_segment_size: int = 268_435_456        # 256 MB
    max_pool_segments: int = 4
    max_pool_memory: int = field(init=False)
    reassembly_segment_size: int = 67_108_864   # 64 MB
    reassembly_max_segments: int = 4
    max_total_chunks: int = 512
    chunk_gc_interval: float = 5.0
    chunk_threshold_ratio: float = 0.9
    chunk_assembler_timeout: float = 60.0
    max_reassembly_bytes: int = 8_589_934_592   # 8 GB
    chunk_size: int = 131_072                   # 128 KB

    def __post_init__(self) -> None:
        segment_size = int(self.pool_segment_size)
        segment_count = int(self.max_pool_segments)
        object.__setattr__(self, 'max_pool_memory', segment_size * segment_count)

        if self.pool_segment_size <= 0 or self.pool_segment_size > 0xFFFFFFFF:
            raise ValueError(
                f'pool_segment_size must be in (0, {0xFFFFFFFF}], '
                f'got {self.pool_segment_size}'
            )
        if not 1 <= self.max_pool_segments <= 255:
            raise ValueError(
                f'max_pool_segments must be 1..255, got {self.max_pool_segments}'
            )
        if not (0 < self.chunk_threshold_ratio <= 1):
            raise ValueError(
                f'chunk_threshold_ratio must be in (0, 1], '
                f'got {self.chunk_threshold_ratio}'
            )
        if not math.isfinite(self.chunk_gc_interval):
            raise ValueError(
                f'chunk_gc_interval must be finite, got {self.chunk_gc_interval}'
            )
        if self.chunk_gc_interval <= 0:
            raise ValueError(
                f'chunk_gc_interval must be positive, got {self.chunk_gc_interval}'
            )
        _validate_duration_seconds('chunk_gc_interval', self.chunk_gc_interval)
        if not math.isfinite(self.chunk_assembler_timeout):
            raise ValueError(
                f'chunk_assembler_timeout must be finite, '
                f'got {self.chunk_assembler_timeout}'
            )
        if self.chunk_assembler_timeout <= 0:
            raise ValueError(
                f'chunk_assembler_timeout must be positive, '
                f'got {self.chunk_assembler_timeout}'
            )
        _validate_duration_seconds(
            'chunk_assembler_timeout',
            self.chunk_assembler_timeout,
        )
        if not 1 <= self.reassembly_max_segments <= 255:
            raise ValueError(
                f'reassembly_max_segments must be 1..255, '
                f'got {self.reassembly_max_segments}'
            )
        if self.reassembly_segment_size <= 0:
            raise ValueError(
                f'reassembly_segment_size must be positive, '
                f'got {self.reassembly_segment_size}'
            )


@dataclass(frozen=True)
class ServerIPCConfig(BaseIPCConfig):
    """Server-side IPC configuration."""

    max_frame_size: int = 2_147_483_648         # 2 GB
    max_payload_size: int = 17_179_869_184      # 16 GB
    max_pending_requests: int = 1024
    pool_decay_seconds: float = 60.0
    heartbeat_interval: float = 15.0
    heartbeat_timeout: float = 30.0

    def __post_init__(self) -> None:
        super().__post_init__()
        if self.max_frame_size <= 16:
            raise ValueError(
                f'max_frame_size must be > 16, got {self.max_frame_size}'
            )
        if self.max_payload_size <= 0:
            raise ValueError(
                f'max_payload_size must be positive, got {self.max_payload_size}'
            )
        if self.pool_segment_size > self.max_payload_size:
            raise ValueError(
                f'pool_segment_size ({self.pool_segment_size}) must not exceed '
                f'max_payload_size ({self.max_payload_size})'
            )
        if not math.isfinite(self.pool_decay_seconds):
            raise ValueError(
                f'pool_decay_seconds must be finite, got {self.pool_decay_seconds}'
            )
        _validate_duration_seconds('pool_decay_seconds', self.pool_decay_seconds)
        if not math.isfinite(self.heartbeat_interval):
            raise ValueError(
                f'heartbeat_interval must be finite, got {self.heartbeat_interval}'
            )
        if self.heartbeat_interval < 0:
            raise ValueError(
                f'heartbeat_interval must be >= 0, got {self.heartbeat_interval}'
            )
        _validate_duration_seconds('heartbeat_interval', self.heartbeat_interval)
        if not math.isfinite(self.heartbeat_timeout):
            raise ValueError(
                f'heartbeat_timeout must be finite, got {self.heartbeat_timeout}'
            )
        _validate_duration_seconds('heartbeat_timeout', self.heartbeat_timeout)
        if self.heartbeat_interval > 0 and self.heartbeat_timeout <= self.heartbeat_interval:
            raise ValueError(
                f'heartbeat_timeout ({self.heartbeat_timeout}) must exceed '
                f'heartbeat_interval ({self.heartbeat_interval})'
            )


@dataclass(frozen=True)
class ClientIPCConfig(BaseIPCConfig):
    """Client-side IPC configuration."""

    reassembly_segment_size: int = 67_108_864   # 64 MB


_SERVER_INIT_FIELD_NAMES = {f.name for f in fields(ServerIPCConfig) if f.init}
_CLIENT_INIT_FIELD_NAMES = {f.name for f in fields(ClientIPCConfig) if f.init}


def build_server_config(
    settings: C2Settings | None = None,
    **kwargs: object,
) -> ServerIPCConfig:
    """Build a ``ServerIPCConfig`` through Rust config resolution."""
    _reject_unknown_kwargs(kwargs, _SERVER_INIT_FIELD_NAMES)
    native = _native_resolver()
    resolved = native.resolve_server_ipc_config(
        _clean_overrides(kwargs),
        _global_overrides(settings),
    )
    return ServerIPCConfig(**_dataclass_kwargs(resolved, _SERVER_INIT_FIELD_NAMES))


def build_client_config(
    settings: C2Settings | None = None,
    **kwargs: object,
) -> ClientIPCConfig:
    """Build a ``ClientIPCConfig`` through Rust config resolution."""
    _reject_unknown_kwargs(kwargs, _CLIENT_INIT_FIELD_NAMES)
    native = _native_resolver()
    resolved = native.resolve_client_ipc_config(
        _clean_overrides(kwargs),
        _global_overrides(settings),
    )
    return ClientIPCConfig(**_dataclass_kwargs(resolved, _CLIENT_INIT_FIELD_NAMES))


def _native_resolver() -> Any:
    from c_two import _native

    return _native


def _clean_overrides(kwargs: dict[str, object]) -> dict[str, object]:
    return {key: value for key, value in kwargs.items() if value is not None}


def _validate_duration_seconds(name: str, value: float) -> None:
    if not math.isfinite(value):
        raise ValueError(f'{name} must be finite, got {value}')
    if value < 0:
        raise ValueError(f'{name} must be >= 0, got {value}')
    # Rust Duration stores seconds in u64 and nanoseconds separately. Values
    # beyond u64::MAX seconds would panic in Duration::from_secs_f64.
    if value >= 2 ** 64:
        raise ValueError(f'{name} must be a representable duration in seconds')


def _global_overrides(settings: C2Settings | None) -> dict[str, object]:
    if settings is None:
        from .settings import settings as _settings
        settings = _settings
    if not isinstance(settings, C2Settings):
        raise TypeError('settings must be a C2Settings instance')
    return settings._global_overrides()  # noqa: SLF001


def _reject_unknown_kwargs(kwargs: dict[str, object], allowed: set[str]) -> None:
    unknown = sorted(set(kwargs) - allowed)
    if unknown:
        joined = ', '.join(unknown)
        raise TypeError(f'unknown IPC config option(s): {joined}')


def _dataclass_kwargs(resolved: dict[str, object], allowed: set[str]) -> dict[str, object]:
    return {key: value for key, value in resolved.items() if key in allowed}
