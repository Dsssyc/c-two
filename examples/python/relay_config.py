"""Example-only helpers for relay configuration."""
from __future__ import annotations


def resolved_relay_url(default: str | None = None) -> str | None:
    from c_two.config.settings import settings

    return settings.relay_address or default


def ensure_http_relay_url(value: str | None) -> str:
    if value is None:
        raise ValueError('relay URL must not be empty')
    value = value.strip()
    if not value:
        raise ValueError('relay URL must not be empty')
    if not value.startswith(('http://', 'https://')):
        raise ValueError('relay URL must start with http:// or https://')
    return value
