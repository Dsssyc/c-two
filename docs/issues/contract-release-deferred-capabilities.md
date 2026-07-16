# Contract Release Deferred Capabilities

**Date:** 2026-07-15
**Status:** Open
**Scope:** Capabilities intentionally not provided by the route-independent `ContractRelease` slice

## Implemented Baseline

C-Two can validate and canonicalize `c-two.contract.v1`, construct an immutable content-addressed `ContractRelease`, serialize a strict `c-two.contract-release-ref.v1`, verify that reference against resolved descriptor bytes, and derive an `ExpectedRouteContract` only after a route name is supplied.

## Open Capabilities

| Capability | Current limitation | Why not in this slice | Impact | Owner | Exit criteria |
| --- | --- | --- | --- | --- | --- |
| Contract compatibility | Release matching is exact by descriptor digest; no semver/range acceptance exists. | Compatibility requires explicit ABI/signature evolution rules after exact identity is stable. | Consumers cannot request a compatible range and must pin one release. | C-Two | Rust-owned compatibility rules reject ambiguity and ABI-incompatible matches with structured errors and cross-language vectors. |
| Resolver and storage | C-Two defines no registry, URL, object store, or digest resolver. | Storage and distribution topology are consumer/deployment concerns. | A ref alone cannot fetch its descriptor. | Catalog/deployment owner | A consumer resolves bytes by digest, then passes them through `ContractRelease` verification without C-Two owning catalog state. |
| Signature and trust | A release digest proves integrity, not publisher identity, authorization, or revocation state. | C-Two does not own Authority identity, key distribution, policy, or revocation. | Callers must not treat a valid release as trusted by itself. | Authority/policy owner, including Toodle where applicable | A downstream signed envelope binds the C-Two ref to its Authority trust model without changing C-Two release identity. |
| Rust SDK | No supported `sdk/rust` client/server facade consumes the release yet. | The SDK must be a real end-to-end consumer of existing core transports, not an empty package. | Rust applications use low-level core crates and lack a stable SDK facade. | C-Two | Rust client and host slices reuse `c2-wire`, `c2-ipc`, `c2-http`, `c2-server`, `c2-runtime`, and `c2-contract`, with runnable documentation. |
| FastDB Rust call-db runtime | Rust cannot yet consume FastDB call-db bindings or owned views through a FastDB-owned stable API. | FastDB owns schema, storage, encode/decode, owned bytes, and view lifetime. C-Two must not copy them. | Portable payload proof is limited to descriptor identity and opaque bytes. | FastDB | FastDB provides a stable Rust or C-ABI-backed Rust runtime with binding validation, encode/decode, owned bytes, retained views, and golden vectors. |
| Bidirectional Rust/Python proof | No Rust host/client cross-language CRM proof is included. | It depends on both the real Rust SDK and FastDB Rust runtime for the payload-bearing path. | `ContractRelease` is proven as an identity primitive, not as complete SDK interoperability. | C-Two with FastDB dependency | Rust client to Python CRM, Python client to Rust CRM, and Rust-to-Rust calls pass for no-payload and one FastDB request/response; mismatch fails structurally. |

## Guardrail

Do not add a JSON/pickle transport, a C-Two-owned FastDB parser, an unsigned `trusted` flag, or a placeholder SDK to make these rows appear complete.
