"""Process-level configuration facade for Python code overrides."""
from __future__ import annotations

class C2Settings:
    """Process-level facade for SDK code overrides.

    Environment variables and ``.env`` files are resolved by the Rust
    ``c2-config`` resolver. This object only stores Python code-level
    overrides such as ``cc.set_relay()``.
    """

    def __init__(
        self,
        *,
        relay_address: str | None = None,
        shm_threshold: int | None = None,
    ) -> None:
        self._relay_address = _clean_optional_str(relay_address)
        self._shm_threshold: int | None = None
        if shm_threshold is not None:
            self.shm_threshold = shm_threshold

    @property
    def relay_address(self) -> str | None:
        if self._relay_address is not None:
            return self._relay_address
        return _resolve_relay_address()

    @relay_address.setter
    def relay_address(self, value: str | None) -> None:
        self._relay_address = _clean_optional_str(value)

    @property
    def shm_threshold(self) -> int:
        if self._shm_threshold is not None:
            return self._shm_threshold
        return _resolve_shm_threshold()

    @shm_threshold.setter
    def shm_threshold(self, value: int | None) -> None:
        if value is None:
            self._shm_threshold = None
            return
        value = int(value)
        if value <= 0:
            raise ValueError(f'shm_threshold must be > 0, got {value}')
        self._shm_threshold = value

    def _shm_overrides(self) -> dict[str, int]:
        overrides: dict[str, int] = {}
        if self._shm_threshold is not None:
            overrides['shm_threshold'] = self._shm_threshold
        return overrides


def _clean_optional_str(value: str | None) -> str | None:
    if value is None:
        return None
    value = value.strip()
    return value or None


def _resolve_shm_threshold() -> int:
    from c_two._native import resolve_shm_threshold

    shm_overrides = settings._shm_overrides()  # noqa: SLF001
    return int(resolve_shm_threshold(shm_overrides))


def _resolve_relay_address() -> str | None:
    from c_two._native import resolve_relay_address

    return resolve_relay_address()


settings = C2Settings()
