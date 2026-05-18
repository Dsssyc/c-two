from __future__ import annotations

import hashlib
import inspect
import re
import threading
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any

_IDENT_RE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._:/+-]*$')
_CAPABILITY_RE = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._+-]*$')
_SHA256_RE = re.compile(r'^[0-9a-f]{64}$')


@dataclass(frozen=True)
class CodecRef:
    id: str
    version: str
    schema: str | None = None
    schema_sha256: str | None = None
    capabilities: tuple[str, ...] = ()
    media_type: str | None = None
    portable: bool = True

    def __post_init__(self) -> None:
        object.__setattr__(self, 'id', _nonempty_identity(self.id, 'id'))
        object.__setattr__(self, 'version', _nonempty_identity(self.version, 'version'))
        if self.schema is not None:
            object.__setattr__(self, 'schema', _nonempty_identity(self.schema, 'schema'))
        if self.media_type is not None:
            object.__setattr__(self, 'media_type', _nonempty_identity(self.media_type, 'media_type'))
        if self.schema_sha256 is not None:
            if not isinstance(self.schema_sha256, str) or not _SHA256_RE.match(self.schema_sha256):
                raise ValueError('schema_sha256 must be a lowercase 64-character hex digest.')
        capabilities = _normalize_capabilities(self.capabilities)
        object.__setattr__(self, 'capabilities', capabilities)
        if not isinstance(self.portable, bool):
            raise TypeError('portable must be a bool.')

    @classmethod
    def from_schema(
        cls,
        *,
        id: str,
        version: str,
        schema: str,
        schema_text: str,
        capabilities: Iterable[str] = (),
        media_type: str | None = None,
        portable: bool = True,
    ) -> 'CodecRef':
        if not isinstance(schema_text, str):
            raise TypeError('schema_text must be a string.')
        digest = hashlib.sha256(schema_text.encode()).hexdigest()
        return cls(
            id=id,
            version=version,
            schema=schema,
            schema_sha256=digest,
            capabilities=tuple(capabilities),
            media_type=media_type,
            portable=portable,
        )

    def to_wire_ref(self) -> dict[str, Any]:
        ref: dict[str, Any] = {
            'id': self.id,
            'kind': 'codec_ref',
            'portable': self.portable,
            'version': self.version,
        }
        if self.schema is not None:
            ref['schema'] = self.schema
        if self.schema_sha256 is not None:
            ref['schema_sha256'] = self.schema_sha256
        if self.capabilities:
            ref['capabilities'] = list(self.capabilities)
        if self.media_type is not None:
            ref['media_type'] = self.media_type
        return ref


@dataclass(frozen=True)
class CodecBinding:
    transferable: type
    codec_ref: CodecRef

    def __post_init__(self) -> None:
        if not isinstance(self.transferable, type):
            raise TypeError('transferable must be a Transferable class.')
        _ensure_transferable_candidate(self.transferable)
        object.__setattr__(self, 'codec_ref', normalize_codec_ref(self.codec_ref))


_CODEC_PROVIDERS: list[Any] = []
_CODEC_BINDINGS: dict[type, CodecBinding] = {}
_CODEC_LOCK = threading.Lock()


def normalize_codec_ref(value: CodecRef | dict[str, Any]) -> CodecRef:
    if isinstance(value, CodecRef):
        return value
    if not isinstance(value, dict):
        raise TypeError('codec_ref must be a CodecRef or dict.')
    payload = dict(value)
    kind = payload.pop('kind', None)
    if kind is not None and kind != 'codec_ref':
        raise ValueError('codec_ref dict kind must be "codec_ref".')
    capabilities = payload.pop('capabilities', ())
    return CodecRef(capabilities=tuple(capabilities), **payload)


def codec_ref_for_transferable(transferable_cls: type) -> CodecRef | None:
    ref = getattr(transferable_cls, '__cc_codec_ref__', None)
    if ref is None:
        return None
    return normalize_codec_ref(ref)


def bind_codec(python_type: type, codec: CodecBinding | type | dict[str, Any]) -> None:
    if not isinstance(python_type, type):
        raise TypeError('python_type must be a type.')
    binding = _binding_from_candidate(codec)
    with _CODEC_LOCK:
        _CODEC_BINDINGS[python_type] = binding


def use_codec(provider: Any) -> Any:
    if not callable(provider) and not callable(getattr(provider, 'candidates_for_type', None)):
        raise TypeError('codec provider must be callable or expose candidates_for_type().')
    with _CODEC_LOCK:
        if provider not in _CODEC_PROVIDERS:
            _CODEC_PROVIDERS.append(provider)
    return provider


def resolve_codec(python_type: object, context: object | None = None) -> CodecBinding | None:
    with _CODEC_LOCK:
        explicit = _CODEC_BINDINGS.get(python_type) if isinstance(python_type, type) else None
        providers = tuple(_CODEC_PROVIDERS)
    if explicit is not None:
        return explicit

    matches: list[CodecBinding] = []
    for provider in providers:
        result = _query_provider(provider, python_type, context)
        for candidate in _candidate_iter(result):
            matches.append(_binding_from_candidate(candidate))
    if len(matches) > 1:
        names = ', '.join(binding.codec_ref.id for binding in matches)
        raise ValueError(
            f'Multiple codec candidates found for {_type_label(python_type)}: {names}. '
            'Use cc.bind_codec(...) to choose one explicitly.',
        )
    if not matches:
        return None
    return matches[0]


def _clear_codec_registry_for_tests() -> None:
    with _CODEC_LOCK:
        _CODEC_BINDINGS.clear()
        _CODEC_PROVIDERS.clear()


def _query_provider(provider: Any, python_type: object, context: object | None) -> Any:
    method = getattr(provider, 'candidates_for_type', None)
    if callable(method):
        return _call_provider(method, python_type, context)
    return _call_provider(provider, python_type, context)


def _call_provider(provider: Any, python_type: object, context: object | None) -> Any:
    try:
        signature = inspect.signature(provider)
    except (TypeError, ValueError):
        return provider(python_type, context)
    positional = [
        param
        for param in signature.parameters.values()
        if param.kind in {
            inspect.Parameter.POSITIONAL_ONLY,
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
        }
    ]
    has_varargs = any(
        param.kind is inspect.Parameter.VAR_POSITIONAL
        for param in signature.parameters.values()
    )
    if has_varargs or len(positional) >= 2:
        return provider(python_type, context)
    if len(positional) == 1:
        return provider(python_type)
    raise TypeError('codec provider must accept type or type plus context.')


def _candidate_iter(result: Any) -> Iterable[Any]:
    if result is None:
        return ()
    if isinstance(result, (CodecBinding, type, dict)):
        return (result,)
    if isinstance(result, Iterable) and not isinstance(result, (str, bytes, bytearray)):
        return tuple(result)
    raise TypeError(f'codec provider returned unsupported candidate {result!r}.')


def _type_label(value: object) -> str:
    if isinstance(value, type):
        return value.__name__
    return repr(value)


def _binding_from_candidate(candidate: CodecBinding | type | dict[str, Any]) -> CodecBinding:
    if isinstance(candidate, CodecBinding):
        return _materialize_transferable_codec_ref(candidate)
    if isinstance(candidate, dict):
        transferable = candidate.get('transferable')
        codec_ref = candidate.get('codec_ref')
        if transferable is None or codec_ref is None:
            raise ValueError('codec candidate dict must contain transferable and codec_ref.')
        return _materialize_transferable_codec_ref(
            CodecBinding(transferable=transferable, codec_ref=normalize_codec_ref(codec_ref)),
        )
    if isinstance(candidate, type):
        ref = codec_ref_for_transferable(candidate)
        if ref is None:
            raise ValueError(
                f'codec candidate {candidate.__name__} does not declare codec_ref.',
            )
        _ensure_transferable_candidate(candidate)
        return _materialize_transferable_codec_ref(
            CodecBinding(transferable=candidate, codec_ref=ref),
        )
    raise TypeError(f'unsupported codec candidate {candidate!r}.')


def _materialize_transferable_codec_ref(binding: CodecBinding) -> CodecBinding:
    existing = codec_ref_for_transferable(binding.transferable)
    if existing is None:
        setattr(binding.transferable, '__cc_codec_ref__', binding.codec_ref)
        return binding
    if existing != binding.codec_ref:
        raise ValueError(
            f'codec binding for {binding.transferable.__name__} conflicts with '
            'the transferable codec_ref.',
        )
    return binding


def _ensure_transferable_candidate(candidate: type) -> None:
    from .transferable import Transferable

    if not issubclass(candidate, Transferable) or candidate is Transferable:
        raise TypeError('codec candidates must be Transferable subclasses.')


def _nonempty_identity(value: str, field: str) -> str:
    if not isinstance(value, str):
        raise TypeError(f'{field} must be a string.')
    stripped = value.strip()
    if not stripped:
        raise ValueError(f'{field} cannot be empty.')
    if not _IDENT_RE.match(stripped):
        raise ValueError(f'{field} contains unsupported characters.')
    return stripped


def _normalize_capabilities(values: Iterable[str]) -> tuple[str, ...]:
    if isinstance(values, str):
        raise TypeError('capabilities must be an iterable of strings.')
    seen: set[str] = set()
    normalized: list[str] = []
    for value in values:
        if not isinstance(value, str):
            raise TypeError('capabilities must contain strings.')
        stripped = value.strip()
        if not stripped:
            raise ValueError('capability cannot be empty.')
        if not _CAPABILITY_RE.match(stripped):
            raise ValueError('capability contains unsupported characters.')
        if stripped in seen:
            raise ValueError(f'duplicate capability {stripped!r}.')
        seen.add(stripped)
        normalized.append(stripped)
    return tuple(sorted(normalized))
