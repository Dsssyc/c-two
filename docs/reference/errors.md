# Error contracts and diagnostics

This page defines the public semantic and Core error families, canonical wire codec, transfer-hook classification, and local I/O diagnostics.

## Semantic error wire

Errors are modeled as `CCError` subclasses with numeric `ERROR_Code` values. Rust `core/foundation/c2-error` owns the canonical error registry and `code:message` wire codec. Python generates `ERROR_Code` from `_native.error_registry()` and delegates `CCError.serialize()` / `CCError.deserialize()` to `_native.encode_error_wire()` and `_native.decode_error_wire_parts()`. Do not reintroduce Python-side parsing of the error wire payload, and do not use or add legacy-named error codec APIs. Error classes are named by location, for example `ResourceDeserializeInput`, `ClientSerializeInput`, `ResourceExecuteFunction`, and `ClientCallResource`. Enum values live under `ERROR_AT_RESOURCE_*` and `ERROR_AT_CLIENT_*`.

Transfer hook errors are split by hook type: `deserialize()` failures remain `ERROR_AT_RESOURCE_INPUT_DESERIALIZING` or `ERROR_AT_CLIENT_OUTPUT_DESERIALIZING`, while `from_buffer()` / `fromBuffer()` failures use `ERROR_AT_RESOURCE_INPUT_FROM_BUFFER` or `ERROR_AT_CLIENT_OUTPUT_FROM_BUFFER`. Do not label zero-deserialization buffer view construction as deserialization. On any `from_buffer()` failure before a resource or user receives a retained value, release the native memoryview and buffer owner immediately so Rust lease stats do not retain stale holds.

## Public CoreError and local causes

Python Core errors are defined and typed by the public `c_two.error.CoreError` class, sharing `CCBaseError` with semantic `CCError` and retaining `RuntimeError` ancestry. PyO3 imports that same class; do not restore `_native.CoreError` or add a compatibility alias.

Local bind and connect diagnostics follow the original error chain and lifecycle watch. `IpcError::Connect` retains `EndpointIoError`; server bind failures retain `EndpointBindCause` through `ServerFailure.bind_cause`. Core exposes `LocalIoCause`, and Python projects `io_operation`, `io_kind`, `raw_os_error`, and the bind-only `endpoint_unverified_reason`. Preserve the original OS kind and code inside directory context while retaining outer policy refusals. Deadline, handshake, and contract errors must not gain fabricated local causes. These fields do not authorize retries, restart, or cleanup; do not add an error cache or infer the operation from transport phase.

The public `CoreError` category literal includes `protocol_version_mismatch`. Its `local_protocol_version` and `peer_protocol_version` are nonzero on that category and `None` on every other error. The [IPC handshake specification](ipc-handshake.md) gives the precise terminal behavior and error fields. The [local endpoint lifecycle reference](local-endpoint-lifecycle.md#public-structured-python-errors) lists all public structured fields and native cause projections.
