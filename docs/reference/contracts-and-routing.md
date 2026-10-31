# Contracts and routing

This reference owns CRM contract identity, portable bindings, and direct IPC and relay route selection.

## CRM contracts and releases

Path: `sdk/python/src/c_two/crm/`

- CRM contracts are interface classes decorated with `@cc.crm(namespace='...', version='...')`.
- Only methods in the contract are exposed remotely.
- CRM route contracts are identified by route name plus the CRM namespace, CRM name, CRM version, ABI hash, and signature hash. Python may compute the descriptor/fingerprints from the CRM class, but Rust `c2-contract` validates the complete expected route contract at IPC and relay boundaries.
- A persistent CRM contract release is a Rust-validated `ContractReleaseRef` derived from canonical `c-two.contract.v2` content. It never contains a route name. `ExpectedRouteContract` is derived later from a validated release plus a runtime route name.
- Resource implementations are plain Python classes and are not decorated.
- Portable methods declare zero or one input and zero or one output with `@cc.transfer(input=FASTDB_SPEC, output=FASTDB_SPEC)`. Their Python parameter and return type is `fastdb4py.payload.Payload`; C-Two embeds each nested specification as an opaque JSON value and delegates it to FastDB Core.
- Python-only fallback values use pickle and must not be treated as portable descriptor or codegen inputs.
- C-Two 0.10.1 source uses FastDB 0.2.4; published releases keep the FastDB version recorded in their release notes. Python FastDB payloads require the optional `c-two[fastdb]` extra, pinned to `fastdb4py==0.2.4`. Ordinary imports, CRM definitions, calls and error wrapping must not import FastDB or NumPy. The dev group selects the extra for full tests; native Core/Rust pins are independent.
- FastDB retained response owners are selected by call-site `cc.hold(...)`; server-side borrowed inputs are selected only by `cc.register(..., input_lifetime={...: 'borrowed'})`.
- CRM methods can use `@cc.read` or `@cc.write`; writes are the default.
- `@cc.transfer(input=..., output=...)` is the only portable FastDB binding authoring surface. `@cc.transferable` and `@cc.transfer(buffer='hold')` are obsolete and must not be reintroduced as codec or lifetime selection mechanisms.
- `@on_shutdown` marks one public CRM contract method as a shutdown callback. The resource implements it normally; registration rejects implementation-only shutdown markers. It is not exposed through RPC.

## Core ownership and SDK parity

C-Two has a language-neutral Rust core and user-facing Rust and Python SDKs. Neither SDK is the canonical home for generic runtime mechanisms. The Rust SDK at `sdk/rust` is package `c-two`, imported as `c_two`, and is a thin facade over Core plus generated typed clients/services. The Python SDK owns Python domain logic, CRM authoring, Python resource invocation, serialization orchestration, and same-process direct-call glue. Rust `c2-core` is the single shared owner of route selection, client/host calls, retry classification, error normalization, transport lease ordering, and runtime lifecycle; the lower Core crates own memory, wire, contract, relay, and configuration mechanisms. PyO3/maturin projects that same Core into Python as `c_two._native`.

The Rust SDK may depend on the official `fastdb` Rust binding to adapt portable payloads, but `c2-core` must remain payload-owner-neutral. Rust users continue to name `fastdb::Payload`; do not re-export it under a C-Two semantic namespace. Rust and Python must expose the same portable route, payload, error, and lifetime capabilities even when their language-level syntax differs.

## Direct IPC

Direct IPC is a complete standalone mode. `cc.connect(..., address='ipc://...')` must bypass relay discovery and remain usable when no relay is configured or a relay environment variable points at an unavailable server. Relay is only a discovery/forwarding projection above IPC; do not make relay the owner of IPC registration, scheduling, or connection establishment.

Python resource servers create an auto-generated `ipc://` address. Use `cc.server_address()` after registration only when a same-host process needs to connect directly.

## Relay discovery and forwarding

`C2_RELAY_ANCHOR_ADDRESS` is the SDK process's relay anchor for control-plane registration and name resolution. It is not necessarily the relay that will carry every data-plane call: after resolution, remote HTTP calls use the selected route's `relay_url` directly. Direct IPC selection from relay resolution is allowed only when the anchor endpoint is loopback/local; nonlocal relay responses must be treated as HTTP relay targets even if they include an `ipc_address`.

Relay resolution and data-plane calls are contract-scoped, not name-only. `cc.connect(CRMClass, name='...')` derives an `ExpectedRouteContract` from the CRM class and the route name. Runtime relay resolve, probe, and call paths must carry and validate the full route name, CRM tag, ABI hash, and signature hash. Do not add production APIs, fallback behavior, cache keys, or wire paths that resolve, probe, or call a CRM route by name alone. A future "find by name" or diagnostic relay-mesh lookup must be a separate discovery/admin surface that returns candidate route metadata and CRM tags; it must not be reused as the normal CRM call path.

Relay-discovered IPC fast paths must validate endpoint identity in Rust before accepting the IPC client. The relay response identity (`server_id` and `server_instance_id`) must match the identity returned by the IPC handshake, and then the requested route name must exist. Do not reintroduce Python-side or route-name-only trust decisions for relay IPC.

The Python SDK does not embed or start a relay server. Start `c3 relay` outside the SDK, through the CLI, Docker Compose, Kubernetes, or other orchestration. Do not add SDK APIs that embed, start, or manage relay-server lifecycle.
