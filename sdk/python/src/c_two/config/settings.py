"""Process-level configuration facade for Python code overrides."""
from __future__ import annotations

from typing import Any


class C2Settings:
    """Lightweight compatibility facade for SDK-level code overrides.

    Environment variables and ``.env`` files are resolved by the Rust
    ``c2-config`` resolver. This object only stores Python code-level
    overrides such as ``cc.set_relay()``.
    """

    def __init__(
        self,
        *,
        relay_address: str | None = None,
        shm_threshold: int | None = None,
        relay_seeds: str | None = None,
    ) -> None:
        self._relay_address = _clean_optional_str(relay_address)
        self._relay_seeds = _clean_optional_str(relay_seeds)
        self._shm_threshold: int | None = None
        if shm_threshold is not None:
            self.shm_threshold = shm_threshold

    @property
    def relay_address(self) -> str | None:
        if self._relay_address is not None:
            return self._relay_address
        return _resolve_runtime_config().get('relay_address')

    @relay_address.setter
    def relay_address(self, value: str | None) -> None:
        self._relay_address = _clean_optional_str(value)

    @property
    def relay_seeds(self) -> str:
        if self._relay_seeds is not None:
            return self._relay_seeds
        seeds = _resolve_relay_config().get('seeds', [])
        return ','.join(str(seed) for seed in seeds)

    @relay_seeds.setter
    def relay_seeds(self, value: str | None) -> None:
        self._relay_seeds = _clean_optional_str(value)

    @property
    def relay_seed_list(self) -> list[str]:
        if self._relay_seeds is not None:
            return [s.strip() for s in self._relay_seeds.split(',') if s.strip()]
        seeds = _resolve_relay_config().get('seeds', [])
        return [str(seed) for seed in seeds]

    @property
    def shm_threshold(self) -> int:
        if self._shm_threshold is not None:
            return self._shm_threshold
        return int(_resolve_runtime_config().get('shm_threshold', 4096))

    @shm_threshold.setter
    def shm_threshold(self, value: int | None) -> None:
        if value is None:
            self._shm_threshold = None
            return
        value = int(value)
        if value <= 0:
            raise ValueError(f'shm_threshold must be > 0, got {value}')
        self._shm_threshold = value

    def _global_overrides(self) -> dict[str, Any]:
        overrides: dict[str, Any] = {}
        if self._relay_address is not None:
            overrides['relay_address'] = self._relay_address
        if self._shm_threshold is not None:
            overrides['shm_threshold'] = self._shm_threshold
        return overrides


def _clean_optional_str(value: str | None) -> str | None:
    if value is None:
        return None
    value = value.strip()
    return value or None


def _resolve_runtime_config() -> dict[str, Any]:
    from c_two._native import resolve_runtime_config

    return resolve_runtime_config()


def _resolve_relay_config() -> dict[str, Any]:
    relay = _resolve_runtime_config().get('relay', {})
    return relay if isinstance(relay, dict) else {}


settings = C2Settings()
