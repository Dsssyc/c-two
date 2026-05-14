# Identity Validation Authority Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repair the identity-validation plan so CrmTag and relay-id validation are enforced at the true wire and mutation authority points without breaking existing relay command-loop, test-only skip-validation, or Python/native facade paths.

**Architecture:** Keep CrmTag validation canonical in `c2-wire` and make the handshake codec enforce it on both encode and decode. Keep relay-id validation canonical in `c2-config`, but enforce it at `RouteAuthority`, the relay mutation choke point, with handler and table checks as defense-in-depth. Treat `skip_ipc_validation` as a test-only path that must still publish a complete explicit test CrmTag; do not allow empty tags into relay state.

**Tech Stack:** Rust workspace crates (`c2-wire`, `c2-config`, `c2-server`, `c2-http`, `c2-python-native`), Python SDK, pytest, cargo tests.

---

## File Structure

- Modify `core/protocol/c2-wire/src/control.rs`
  - Add an encode error variant for invalid textual fields.
- Modify `core/protocol/c2-wire/src/frame.rs`
  - Add a decode error variant for invalid textual fields.
- Modify `core/protocol/c2-wire/src/handshake.rs`
  - Add canonical `validate_crm_tag()` and enforce it in server handshake encode/decode.
- Modify `core/protocol/c2-wire/src/tests.rs`
  - Add direct validator tests plus encode/decode rejection tests.
- Modify `sdk/python/native/src/wire_ffi.rs`
  - Require explicit CrmTag fields in Python `RouteInfo`.
- Modify `sdk/python/tests/unit/test_wire.py`
  - Update helper route construction and add malformed-CrmTag facade tests.
- Modify `sdk/python/tests/unit/test_security.py`
  - Update low-level handshake security tests to use explicit valid CrmTag metadata except in tests that deliberately assert invalid CrmTag behavior.
- Modify `sdk/python/src/c_two/crm/meta.py`
  - Validate namespace, CRM class name, and version before building `__tag__`.
- Modify `sdk/python/src/c_two/transport/server/native.py`
  - Stop parsing semantic data out of slash-delimited `__tag__`.
- Modify `sdk/python/tests/unit/test_crm_decorator.py`
  - Cover invalid namespace, class-name, version, slash, control, and overlong fields.
- Modify `core/transport/c2-server/src/server.rs`
  - Delegate route CrmTag validation to `c2-wire`.
- Modify `sdk/python/native/src/server_ffi.rs`
  - Reject invalid CrmTag at PyO3 route construction before native route state.
- Modify `core/transport/c2-http/src/relay/route_table.rs`
  - Delegate CrmTag validation to `c2-wire`, relay-id validation to `c2-config`, reject invalid direct `register_route()` entries as defense-in-depth, reject invalid `apply_tombstone()` tombstones, keep unregister+tombstone writes atomic, validate LOCAL private identity fields to the same standard as the authority path, validate relay URLs, and reject invalid `record_peer_join()` peer-table mutations.
- Modify `core/transport/c2-http/src/relay/authority.rs`
  - Enforce canonical CrmTag, IPC address, and relay-id validation before every route mutation and add self-contained authority tests with local fixtures.
- Modify `core/transport/c2-http/src/relay/state.rs`
  - Add `RegisterCommitResult::Invalid { reason }` and route address validation errors through it.
- Modify `core/transport/c2-http/src/relay/router.rs`
  - Reject invalid HTTP register IPC addresses, resolve/header CrmTags, and handle invalid commit results.
- Modify `core/transport/c2-http/src/relay/server.rs`
  - Validate the effective relay advertise URL before startup state is created and keep command-loop skip-validation usable by publishing an explicit test CrmTag.
- Modify `core/transport/c2-http/src/relay/peer_handlers.rs`
  - Add early relay-id and relay-URL rejection tests while relying on route-table/authority checks for final enforcement.
- Modify `core/foundation/c2-config/src/identity.rs`, `lib.rs`, and `relay.rs`
  - Add and apply canonical relay-id validation.
- Modify `sdk/python/tests/unit/test_sdk_boundary.py`
  - Guard against duplicate local CrmTag/relay-id validators.

## Design Decisions

- CrmTag fields are `crm_ns`, `crm_name`, and `crm_ver`. All three must be non-empty, no more than `MAX_HANDSHAKE_NAME_BYTES`, free of control characters, free of leading/trailing whitespace, and free of `/` or `\` path/tag delimiters.
- Server handshake encode and decode both validate CrmTag. A malformed peer handshake must not become trusted route metadata.
- Python fast feedback mirrors the Rust rule, including non-ASCII Unicode control characters, but Rust remains authoritative.
- Relay local state may never store empty CrmTag fields, including `skip_ipc_validation`.
- `skip_ipc_validation` remains test-only. HTTP skip registration must supply a complete CrmTag in the body. Command-loop skip registration, which has no CRM fields in its public CLI syntax, uses a fixed explicit test CrmTag and tests assert that behavior.
- Relay-id validation is enforced at `RouteAuthority`, not only at HTTP handlers. This covers HTTP peer endpoints, background digest application, and direct authority calls in tests.
- `RouteTable::register_route()` is still a crate-internal mutation primitive, so it must reject malformed route names, relay ids, CrmTags, non-finite timestamps, and invalid locality/private-field combinations itself. Authority validation is the primary gate; route-table validation prevents future internal call sites and test fixtures from bypassing the invariant.
- `RouteTable::apply_tombstone()` is also a crate-internal mutation primitive. It must reject malformed route names, relay ids, optional private server ids, and non-finite timestamps before mutating tombstone state; otherwise an internal caller can poison deletion state even when route registration is guarded.
- Route unregister helpers must treat route removal and tombstone insertion as one state transition. They must validate the tombstone candidate before removing a route and must not ignore a failed tombstone application after a removal.
- After Task 3 introduces mandatory CrmTag validation in relay registration, every successful direct `commit_register_upstream()` test fixture must pass explicit non-empty test CrmTag fields. Empty `String::new()` triplets are allowed only in tests that intentionally assert invalid CrmTag rejection.
- Direct route-table LOCAL route validation must not use weaker private-field checks than the authority path. `server_id` must pass both `c2_config::validate_server_id()` and the IPC handshake byte limit, `server_instance_id` must match the authority's non-empty/length/trim/path/ASCII/control rules, and `ipc_address` must be validated by the IPC address parser rather than by prefix checks.
- Invalid IPC addresses are input validation errors, not owner conflicts. They need an explicit `ControlError::InvalidAddress { reason }` path that maps to `RegisterCommitResult::Invalid { reason }` and HTTP 400 responses.
- Relay URLs are route identity data: they are exposed through `RouteInfo` and included in anti-entropy digests. `RelayServer::start()` must reject a malformed effective advertise URL before route state exists, and `RouteTable::register_route()`, snapshot merge, and peer join must reject malformed relay URLs before they enter route or peer state. Treat relay URLs as base URLs: allow `http`/`https`, host, optional port/path, but reject query strings, fragments, and userinfo because peer endpoint construction is string-based.
- `RouteTable::record_peer_join()` directly mutates the peer table, so relay-id and relay-URL validation must live there too. Peer handlers should reject malformed join envelopes early for clean control-flow, but the route table must still return `false` without mutation if a future internal caller bypasses handler checks.
- Any new enum variant in a relay state result type must be propagated through every exhaustive match before running tests. Use `rg` immediately after adding variants; do not rely on the compiler as the first integration check.
- Task 1 and Task 2 are a single commit boundary. Do not commit after Task 1 alone: once `c2-wire` rejects empty CrmTags, the current Python `RouteInfo` default empty metadata becomes invalid until Task 2 migrates the facade and tests.
- Any Python test that imports `c_two._native` after Rust native changes must be preceded by `env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv sync --reinstall-package c-two`. `cargo check` proves Rust compilation only; it does not reinstall the PyO3 extension used by pytest.
- Any relay integration test or full Python suite after `c2-http` relay changes must be preceded by `python tools/dev/c3_tool.py --build --link`, because pytest starts the existing `c3` binary from `cli/target` and can otherwise exercise stale relay code.

## Task 1: Make c2-wire The Real CrmTag Authority

**Files:**
- Modify: `core/protocol/c2-wire/src/control.rs`
- Modify: `core/protocol/c2-wire/src/frame.rs`
- Modify: `core/protocol/c2-wire/src/handshake.rs`
- Test: `core/protocol/c2-wire/src/tests.rs`

- [x] **Step 1: Add failing validator and codec tests**

Add these tests to `core/protocol/c2-wire/src/tests.rs`:

```rust
fn route(name: &str, methods: &[&str]) -> RouteInfo {
    RouteInfo {
        name: name.into(),
        crm_ns: "test.grid".into(),
        crm_name: "Grid".into(),
        crm_ver: "0.1.0".into(),
        methods: methods
            .iter()
            .enumerate()
            .map(|(index, method)| MethodEntry {
                name: (*method).into(),
                index: index as u16,
            })
            .collect(),
    }
}

#[test]
fn crm_tag_validator_accepts_normal_contract_identity() {
    validate_crm_tag("test.grid", "Grid", "0.1.0").unwrap();
}

#[test]
fn crm_tag_validator_rejects_malformed_fields() {
    let too_long = "x".repeat(MAX_HANDSHAKE_NAME_BYTES + 1);
    for (crm_ns, crm_name, crm_ver, needle) in [
        ("", "Grid", "0.1.0", "cannot be empty"),
        ("test.grid", "", "0.1.0", "cannot be empty"),
        ("test.grid", "Grid", "", "cannot be empty"),
        (" test.grid", "Grid", "0.1.0", "leading or trailing whitespace"),
        ("test.grid", "Grid\nInjected", "0.1.0", "control characters"),
        ("test/grid", "Grid", "0.1.0", "path or tag separators"),
        ("test.grid", "Bad\\Grid", "0.1.0", "path or tag separators"),
        ("test.grid", too_long.as_str(), "0.1.0", "cannot exceed"),
    ] {
        let err = validate_crm_tag(crm_ns, crm_name, crm_ver)
            .expect_err("malformed CrmTag field must fail");
        assert!(err.contains(needle), "expected {needle:?}, got {err:?}");
    }
}

#[test]
fn server_handshake_encode_rejects_invalid_crm_tag_fields() {
    let mut route = route("grid", &["get"]);
    route.crm_name = "Grid\0Injected".into();

    let err = encode_server_handshake(&[], CAP_CALL_V2, &[route], "", &test_identity())
        .expect_err("invalid CrmTag must fail during encode");

    assert!(err.to_string().contains("crm name"));
    assert!(err.to_string().contains("control characters"));
}

#[test]
fn server_handshake_decode_rejects_invalid_crm_tag_fields() {
    let route = RouteInfo {
        name: "grid".into(),
        crm_ns: "test.grid".into(),
        crm_name: "Grid".into(),
        crm_ver: "0.1.0".into(),
        methods: vec![MethodEntry {
            name: "get".into(),
            index: 0,
        }],
    };
    let identity = test_identity();
    let mut encoded = encode_server_handshake(&[], CAP_CALL_V2, &[route], "", &identity).unwrap();

    let grid_pos = encoded
        .windows("Grid".len())
        .position(|window| window == b"Grid")
        .expect("encoded CRM name should be present");
    encoded[grid_pos + 1] = b'\n';

    let err = decode_handshake(&encoded).expect_err("decode must reject malformed CrmTag");
    assert!(err.to_string().contains("crm name"));
    assert!(err.to_string().contains("control characters"));
}
```

- [x] **Step 2: Run the failing tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire crm_tag -- --nocapture
```

Expected: fails because `validate_crm_tag()` and invalid text error variants do not exist or are not wired into the codec.

- [x] **Step 3: Add descriptive text-field error variants**

In `core/protocol/c2-wire/src/control.rs`, change `EncodeError` to:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    FieldTooLong {
        field: &'static str,
        max: usize,
        actual: usize,
    },
    InvalidText {
        field: &'static str,
        reason: String,
    },
}
```

Update its `Display` implementation:

```rust
match self {
    Self::FieldTooLong { field, max, actual } => {
        write!(f, "{field} is too long: {actual} bytes > {max}")
    }
    Self::InvalidText { field, reason } => {
        write!(f, "{field} is invalid: {reason}")
    }
}
```

In `core/protocol/c2-wire/src/frame.rs`, add a decode variant:

```rust
InvalidText {
    field: &'static str,
    reason: String,
},
```

Update its `Display` implementation:

```rust
Self::InvalidText { field, reason } => {
    write!(f, "invalid {field}: {reason}")
}
```

- [x] **Step 4: Implement canonical CrmTag validation**

In `core/protocol/c2-wire/src/handshake.rs`, add near `validate_name_len()`:

```rust
pub fn validate_crm_tag(crm_ns: &str, crm_name: &str, crm_ver: &str) -> Result<(), String> {
    validate_crm_tag_field("crm namespace", crm_ns)?;
    validate_crm_tag_field("crm name", crm_name)?;
    validate_crm_tag_field("crm version", crm_ver)?;
    Ok(())
}

pub fn validate_crm_tag_field(label: &'static str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    if value.as_bytes().len() > MAX_HANDSHAKE_NAME_BYTES {
        return Err(format!(
            "{label} cannot exceed {MAX_HANDSHAKE_NAME_BYTES} bytes"
        ));
    }
    if value.trim() != value {
        return Err(format!(
            "{label} cannot contain leading or trailing whitespace"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} cannot contain control characters"));
    }
    if value.contains('/') || value.contains('\\') {
        return Err(format!("{label} cannot contain path or tag separators"));
    }
    Ok(())
}
```

Replace the three CRM length checks in `encode_server_handshake()` with:

```rust
validate_crm_tag(&route.crm_ns, &route.crm_name, &route.crm_ver).map_err(|reason| {
    EncodeError::InvalidText {
        field: "crm tag",
        reason,
    }
})?;
```

After reading `crm_ns`, `crm_name`, and `crm_ver` in `decode_handshake()`, add:

```rust
validate_crm_tag(&crm_ns, &crm_name, &crm_ver).map_err(|reason| DecodeError::InvalidText {
    field: "crm tag",
    reason,
})?;
```

- [x] **Step 5: Run c2-wire tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire crm_tag -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-wire -- --nocapture
```

Expected: all c2-wire tests pass.

- [x] **Step 6: Do not commit this intermediate state**

Do not commit after Task 1 alone. At this point `c2-wire` rejects empty CrmTags, while the Python `RouteInfo` facade still has empty defaults. Continue directly to Task 2 and make the first commit after the Python facade and tests have been migrated.

## Task 2: Align Python CrmTag Facades With Wire Authority

**Files:**
- Modify: `sdk/python/native/src/wire_ffi.rs`
- Modify: `sdk/python/tests/unit/test_wire.py`
- Modify: `sdk/python/tests/unit/test_security.py`
- Modify: `sdk/python/src/c_two/crm/meta.py`
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Modify: `sdk/python/src/c_two/transport/registry.py`
- Test: `sdk/python/tests/unit/test_crm_decorator.py`
- Test: `sdk/python/tests/unit/test_wire.py`
- Test: `sdk/python/tests/unit/test_security.py`
- Test: `sdk/python/tests/integration/test_registry.py`

- [x] **Step 1: Write failing Python facade tests**

Add to `sdk/python/tests/unit/test_crm_decorator.py` under `TestIcrmValidation`:

```python
def test_namespace_rejects_path_separator(self):
    with pytest.raises(ValueError, match='separators'):
        @cc.crm(namespace='bad/ns', version='1.0.0')
        class Svc:
            pass

def test_class_name_rejects_control_characters(self):
    Bad = type('Bad\nName', (), {'__module__': __name__})
    with pytest.raises(ValueError, match='control characters'):
        cc.crm(namespace='ns', version='1.0.0')(Bad)

def test_class_name_rejects_unicode_control_characters(self):
    Bad = type('Bad\u0085Name', (), {'__module__': __name__})
    with pytest.raises(ValueError, match='control characters'):
        cc.crm(namespace='ns', version='1.0.0')(Bad)

def test_class_name_rejects_path_separator(self):
    Bad = type('Bad/Name', (), {'__module__': __name__})
    with pytest.raises(ValueError, match='separators'):
        cc.crm(namespace='ns', version='1.0.0')(Bad)

def test_version_rejects_overlong_wire_field(self):
    with pytest.raises(ValueError, match='255 bytes'):
        @cc.crm(namespace='ns', version=f"1.0.{'x' * 256}")
        class Svc:
            pass
```

Add near the top of `sdk/python/tests/unit/test_wire.py`, after imports:

```python
def route_info(name: str, methods: list[MethodEntry]) -> RouteInfo:
    return RouteInfo(
        name=name,
        methods=methods,
        crm_ns='test.wire',
        crm_name='WireRoute',
        crm_ver='0.1.0',
    )

def test_route_info_requires_explicit_crm_tag():
    with pytest.raises(TypeError):
        RouteInfo(name='grid', methods=[MethodEntry(name='get', index=0)])

def test_route_info_rejects_invalid_crm_tag():
    with pytest.raises(ValueError, match='control characters'):
        RouteInfo(
            name='grid',
            methods=[MethodEntry(name='get', index=0)],
            crm_ns='test.wire',
            crm_name='Bad\nRoute',
            crm_ver='0.1.0',
        )
```

Add to `sdk/python/tests/integration/test_registry.py` near `RenamedHello`:

```python
class UndecoratedHello:
    def greeting(self, name: str) -> str:
        ...
```

Add under `TestRegisterConnect`:

```python
    def test_register_rejects_undecorated_contract_before_server_creation(self):
        with pytest.raises(ValueError, match='decorate it with @cc.crm'):
            cc.register(UndecoratedHello, HelloImpl(), name='hello')

        assert cc.server_address() is None

    def test_connect_rejects_undecorated_contract_before_transport(self):
        cc.register(Hello, HelloImpl(), name='hello')

        with pytest.raises(ValueError, match='decorate it with @cc.crm'):
            cc.connect(UndecoratedHello, name='hello')
```

Update every remaining `RouteInfo(...)` construction in `test_wire.py` so it supplies explicit CrmTag metadata. For existing route-name-only cases, use the new helper:

```python
r1 = route_info("route_a", [MethodEntry("m1", 0)])
r2 = route_info("route_b", [MethodEntry("m2", 0), MethodEntry("m3", 1)])
routes = [route_info("grid", [MethodEntry(name="get", index=0)])]
```

Add near the top of `sdk/python/tests/unit/test_security.py`, after imports:

```python
def route_info(name: str, methods: list[MethodEntry]) -> RouteInfo:
    return RouteInfo(
        name=name,
        methods=methods,
        crm_ns='test.security',
        crm_name='SecurityRoute',
        crm_ver='0.1.0',
    )
```

Update `test_security.py` route construction:

```python
routes = [route_info('r1', [MethodEntry('m1', 0)])]

routes = [
    route_info('ns1', [MethodEntry('add', 0), MethodEntry('mul', 1)]),
    route_info('ns2', [MethodEntry('get', 0)]),
]
```

Update `test_excessive_method_count()` in `test_security.py` so the handcrafted server handshake carries a valid CrmTag and still reaches the method-count check:

```python
buf.append(2)                   # route_name_len=2
buf += b'r1'                    # route_name
buf.append(len(b'test.security'))
buf += b'test.security'
buf.append(len(b'SecurityRoute'))
buf += b'SecurityRoute'
buf.append(len(b'0.1.0'))
buf += b'0.1.0'
buf += struct.pack('<H', _MAX_HANDSHAKE_METHODS + 1)  # method count
```

- [x] **Step 2: Run the failing tests**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_crm_decorator.py::TestIcrmValidation sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py::TestHandshakeBoundsChecking sdk/python/tests/integration/test_registry.py::TestRegisterConnect::test_connect_rejects_undecorated_contract_before_transport -q --timeout=30 -rs
```

Expected: fails until Python facade validation and explicit RouteInfo signatures are updated.

- [x] **Step 3: Require explicit CrmTag fields in Python RouteInfo**

In `sdk/python/native/src/wire_ffi.rs`, change the constructor signature:

```rust
#[pyo3(signature = (name, methods, crm_ns, crm_name, crm_ver))]
fn new(
    name: String,
    methods: Vec<Py<PyMethodEntry>>,
    crm_ns: &str,
    crm_name: &str,
    crm_ver: &str,
) -> PyResult<Self> {
    c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver)
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    Ok(Self {
        name,
        crm_ns: crm_ns.to_string(),
        crm_name: crm_name.to_string(),
        crm_ver: crm_ver.to_string(),
        methods,
    })
}
```

Change return type from `Self` to `PyResult<Self>`.

- [x] **Step 4: Add Python CRM field validation**

In `sdk/python/src/c_two/crm/meta.py`, add `unicodedata` beside the existing imports, then add the validation helper:

```python
import unicodedata

_CRM_TAG_MAX_BYTES = 255


def _validate_crm_tag_field(label: str, value: str) -> None:
    if not isinstance(value, str):
        raise ValueError(f'{label} must be a string.')
    if not value:
        raise ValueError(f'{label} of CRM cannot be empty.')
    if len(value.encode()) > _CRM_TAG_MAX_BYTES:
        raise ValueError(f'{label} cannot exceed {_CRM_TAG_MAX_BYTES} bytes.')
    if value.strip() != value:
        raise ValueError(f'{label} cannot contain leading or trailing whitespace.')
    if any(unicodedata.category(ch) == 'Cc' for ch in value):
        raise ValueError(f'{label} cannot contain control characters.')
    if '/' in value or '\\' in value:
        raise ValueError(f'{label} cannot contain path or tag separators.')
```

In `crm_wrapper()`, replace the existing namespace/version emptiness block with:

```python
_validate_crm_tag_field('Namespace', namespace)
_validate_crm_tag_field('CRM name', cls.__name__)
_validate_crm_tag_field('Version', version)
if not version.count('.') == 2:
    raise ValueError('Version must be a string in the format "major.minor.patch".')
```

- [x] **Step 5: Stop parsing namespace from `__tag__`**

In `sdk/python/src/c_two/transport/server/native.py`, replace `_extract_namespace()` with:

```python
@staticmethod
def _extract_namespace(crm_class: type) -> str:
    namespace = getattr(crm_class, '__cc_namespace__', '')
    if namespace:
        return namespace
    raise ValueError(
        f'{crm_class.__name__} is not a valid CRM contract class '
        '(decorate it with @cc.crm).'
    )
```

This makes undecorated CRM contracts fail at the Python boundary instead of producing empty native CrmTags.

In `sdk/python/src/c_two/transport/registry.py`, replace `_crm_contract_identity()` with a boundary check that also rejects undecorated client contract classes before IPC or HTTP clients are created:

```python
def _crm_contract_identity(crm_class: type) -> tuple[str, str, str]:
    crm_ns = getattr(crm_class, '__cc_namespace__', '')
    crm_name = getattr(crm_class, '__cc_name__', '')
    crm_ver = getattr(crm_class, '__cc_version__', '')
    if not crm_ns or not crm_name or not crm_ver:
        raise ValueError(
            f'{crm_class.__name__} is not a valid CRM contract class '
            '(decorate it with @cc.crm).'
        )
    return (crm_ns, crm_name, crm_ver)
```

Call `_crm_contract_identity(crm_class)` at the start of `_ProcessRegistry.register()` before lazy server creation so undecorated contract classes fail before native server identity is allocated. In `NativeServerBridge.register_crm()`, extract `(crm_ns, crm_name, crm_ver)` once from `__cc_*` attributes and use those values for `CRMSlot` and native registration instead of falling back to `crm_class.__name__` or empty strings.

- [x] **Step 6: Rebuild the Python native extension**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv sync --reinstall-package c-two
```

Expected: the PyO3 extension is rebuilt from the current Rust sources. Do not run the pytest commands below against a stale `_native` extension.

- [x] **Step 7: Run Python tests**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_crm_decorator.py::TestIcrmValidation sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py::TestHandshakeBoundsChecking sdk/python/tests/integration/test_registry.py::TestRegisterConnect::test_connect_rejects_undecorated_contract_before_transport -q --timeout=30 -rs
```

Expected: all selected tests pass.

- [x] **Step 8: Commit Task 1 and Task 2 together**

```bash
git add core/protocol/c2-wire/src/control.rs core/protocol/c2-wire/src/frame.rs core/protocol/c2-wire/src/handshake.rs core/protocol/c2-wire/src/tests.rs sdk/python/native/src/wire_ffi.rs sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py sdk/python/src/c_two/crm/meta.py sdk/python/src/c_two/transport/server/native.py sdk/python/src/c_two/transport/registry.py sdk/python/tests/unit/test_crm_decorator.py sdk/python/tests/integration/test_registry.py
git commit -m "fix: enforce CrmTag validation across wire and Python facades"
```

## Task 3: Enforce CrmTag At Native Server And Relay Registration Choke Points

**Files:**
- Modify: `core/transport/c2-server/src/server.rs`
- Modify: `sdk/python/native/src/server_ffi.rs`
- Modify: `core/transport/c2-http/src/relay/route_table.rs`
- Modify: `core/transport/c2-http/src/relay/authority.rs`
- Modify: `core/transport/c2-http/src/relay/state.rs`
- Modify: `core/transport/c2-http/src/relay/router.rs`
- Modify: `core/transport/c2-http/src/relay/server.rs`
- Test: `core/transport/c2-server/src/server.rs`
- Test: `core/transport/c2-http/src/relay/route_table.rs`
- Test: `core/transport/c2-http/src/relay/router.rs`
- Test: `core/transport/c2-http/src/relay/server.rs`
- Test: `core/transport/c2-http/src/relay/state.rs`

- [x] **Step 1: Add failing server and relay tests**

Add to `core/transport/c2-server/src/server.rs` tests:

```rust
#[tokio::test]
async fn register_route_rejects_invalid_crm_tag_fields() {
    let s = Server::new("ipc://invalid_crm_tag_route", ServerIpcConfig::default()).unwrap();
    let mut route = make_route("grid");
    route.crm_name = "Grid\0Injected".to_string();

    let err = s
        .register_route(route)
        .await
        .expect_err("invalid CrmTag must fail before route registration");
    assert!(err.to_string().contains("control characters"));
}
```

Add to `core/transport/c2-http/src/relay/state.rs` tests:

```rust
#[test]
fn local_commit_rejects_invalid_crm_tag_without_fake_duplicate() {
    let state = RelayState::new(test_config(), null_disseminator());
    let client = Arc::new(IpcClient::new("ipc://grid"));

    let result = state.commit_register_upstream(
        "grid".into(),
        "server-grid".into(),
        "server-grid-instance".into(),
        "ipc://grid".into(),
        "test.mesh".into(),
        "Grid\nInjected".into(),
        "0.1.0".into(),
        client,
        None,
    );

    assert!(matches!(result, RegisterCommitResult::Invalid { reason } if reason.contains("control characters")));
    assert!(state.resolve("grid").is_empty());
}

#[test]
fn local_commit_rejects_invalid_ipc_address_without_fake_duplicate() {
    let state = RelayState::new(test_config(), null_disseminator());
    let client = Arc::new(IpcClient::new("ipc://../escape"));

    let result = state.commit_register_upstream(
        "grid".into(),
        "server-grid".into(),
        "server-grid-instance".into(),
        "ipc://../escape".into(),
        "test.mesh".into(),
        "Grid".into(),
        "0.1.0".into(),
        client,
        None,
    );

    assert!(matches!(result, RegisterCommitResult::Invalid { reason } if reason.contains("path separators")));
    assert!(state.resolve("grid").is_empty());
}
```

Add to `core/transport/c2-http/src/relay/route_table.rs` tests:

```rust
#[test]
fn register_route_rejects_invalid_identity_fields_without_mutation() {
    let mut rt = RouteTable::new("relay-a".into());

    let mut bad_crm = peer_entry("grid", "relay-b", 1000.0);
    bad_crm.crm_name = "Grid\nInjected".into();
    assert!(!rt.register_route(bad_crm));
    assert!(rt.resolve("grid").is_empty());

    let bad_route_name = peer_entry("bad\nroute", "relay-b", 1000.0);
    assert!(!rt.register_route(bad_route_name));
    assert!(rt.resolve("bad\nroute").is_empty());

    let mut peer_with_private_fields = peer_entry("peer-private", "relay-b", 1000.0);
    peer_with_private_fields.server_id = Some("server-leak".into());
    peer_with_private_fields.server_instance_id = Some("instance-leak".into());
    peer_with_private_fields.ipc_address = Some("ipc://leaked".into());
    assert!(!rt.register_route(peer_with_private_fields));
    assert!(rt.resolve("peer-private").is_empty());

    let local_wrong_relay = local_entry("local-wrong-relay", "relay-b");
    assert!(!rt.register_route(local_wrong_relay));
    assert!(rt.resolve("local-wrong-relay").is_empty());

    let mut local_bad_server_id = local_entry("local-bad-server", "relay-a");
    local_bad_server_id.server_id =
        Some("s".repeat(c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES + 1));
    assert!(!rt.register_route(local_bad_server_id));
    assert!(rt.resolve("local-bad-server").is_empty());

    let mut local_bad_instance = local_entry("local-bad-instance", "relay-a");
    local_bad_instance.server_instance_id = Some("bad/instance".into());
    assert!(!rt.register_route(local_bad_instance));
    assert!(rt.resolve("local-bad-instance").is_empty());

    let mut local_bad_ipc = local_entry("local-bad-ipc", "relay-a");
    local_bad_ipc.ipc_address = Some("ipc://../escape".into());
    assert!(!rt.register_route(local_bad_ipc));
    assert!(rt.resolve("local-bad-ipc").is_empty());

    let mut bad_relay_url = peer_entry("bad-relay-url", "relay-b", 1000.0);
    bad_relay_url.relay_url = "not a url".into();
    assert!(!rt.register_route(bad_relay_url));
    assert!(rt.resolve("bad-relay-url").is_empty());
}

#[test]
fn apply_tombstone_rejects_invalid_identity_fields_without_mutation() {
    let mut rt = RouteTable::new("relay-b".into());
    register_alive_peer(&mut rt, "relay-a");
    assert!(rt.register_route(peer_entry("grid", "relay-a", 1000.0)));

    assert!(!rt.apply_tombstone(RouteTombstone {
        name: "bad\nroute".into(),
        relay_id: "relay-a".into(),
        removed_at: 2000.0,
        server_id: None,
        observed_at: Instant::now(),
    }));

    assert!(!rt.apply_tombstone(RouteTombstone {
        name: "grid".into(),
        relay_id: " ".into(),
        removed_at: 2000.0,
        server_id: None,
        observed_at: Instant::now(),
    }));

    assert!(!rt.apply_tombstone(RouteTombstone {
        name: "grid".into(),
        relay_id: "relay-a".into(),
        removed_at: 2000.0,
        server_id: Some("bad/server".into()),
        observed_at: Instant::now(),
    }));

    assert_eq!(rt.resolve("grid").len(), 1);
    assert!(rt.list_tombstones().is_empty());
}

#[test]
fn unregister_local_route_rejects_invalid_tombstone_without_removing_route() {
    let mut rt = RouteTable::new("relay-a".into());
    assert!(rt.register_route(local_entry("grid", "relay-a")));

    let (removed, _) = rt.unregister_local_route_with_tombstone("grid", "bad/server");

    assert!(removed.is_none());
    assert!(rt.local_route("grid").is_some());
    assert!(rt.list_tombstones().is_empty());
}

#[test]
fn valid_relay_url_rejects_non_base_urls() {
    assert!(valid_relay_url("http://relay-a:8080"));
    assert!(valid_relay_url("https://relay-a.example/mesh"));

    for url in [
        "not a url",
        "ftp://relay-a:8080",
        "http://relay-a:8080?token=secret",
        "http://relay-a:8080/#fragment",
        "http://user:pass@relay-a:8080",
    ] {
        assert!(!valid_relay_url(url), "{url}");
    }
}
```

Add to `core/transport/c2-http/src/relay/router.rs` tests:

```rust
#[tokio::test]
async fn skip_validation_register_rejects_incomplete_crm_tag() {
    let state = Arc::new(RelayState::new(
        Arc::new(c2_config::RelayConfig {
            relay_id: "test-relay".into(),
            skip_ipc_validation: true,
            ..Default::default()
        }),
        Arc::new(crate::relay::test_support::NoopDisseminator),
    ));

    let status = post_register_with_crm(
        state.clone(),
        "grid",
        "server-grid",
        "ipc://grid",
        "test.mesh",
        "",
        "0.1.0",
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(state.resolve("grid").is_empty());
}
```

Add to `core/transport/c2-http/src/relay/server.rs` tests:

```rust
#[tokio::test]
async fn command_register_skip_validation_publishes_explicit_test_crm_tag() {
    let (_state, disseminator, tx, task) = command_loop_state();
    let (reply, result) = oneshot::channel();

    tx.send(Command::RegisterUpstream {
        name: "grid".into(),
        server_id: "server-grid".into(),
        address: "ipc://grid".into(),
        reply,
    })
    .await
    .unwrap();

    result.await.unwrap().unwrap();
    drop(tx);
    task.await.unwrap();

    let envelopes = disseminator.envelopes();
    assert_eq!(envelopes.len(), 1);
    match &envelopes[0].message {
        PeerMessage::RouteAnnounce {
            crm_ns,
            crm_name,
            crm_ver,
            ..
        } => {
            assert_eq!(crm_ns, "test.relay.skip");
            assert_eq!(crm_name, "SkipValidationUpstream");
            assert_eq!(crm_ver, "0.1.0");
        }
        other => panic!("expected RouteAnnounce, got {other:?}"),
    }
}
```

Add to `core/transport/c2-http/src/relay/server.rs` tests:

```rust
#[test]
fn start_rejects_invalid_advertise_url_before_route_state() {
    let mut config = RelayConfig {
        bind: "127.0.0.1:0".into(),
        relay_id: "relay-invalid-url".into(),
        advertise_url: "not a url".into(),
        ..RelayConfig::default()
    };

    let err = match RelayServer::start(config.clone()) {
        Err(err) => err,
        Ok(mut relay) => {
            let _ = relay.stop();
            panic!("invalid advertise URL must fail");
        }
    };
    assert!(err.contains("advertise_url"));

    config.advertise_url = "ftp://relay-a:8080".into();
    let err = match RelayServer::start(config) {
        Err(err) => err,
        Ok(mut relay) => {
            let _ = relay.stop();
            panic!("non-http advertise URL must fail");
        }
    };
    assert!(err.contains("advertise_url"));
}
```

- [x] **Step 2: Run failing tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server register_route_rejects_invalid_crm_tag_fields -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay local_commit_rejects_invalid_ipc_address_without_fake_duplicate -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay register_route_rejects_invalid_identity_fields_without_mutation -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay apply_tombstone_rejects_invalid_identity_fields_without_mutation -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay unregister_local_route_rejects_invalid_tombstone_without_removing_route -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay valid_relay_url_rejects_non_base_urls -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay start_rejects_invalid_advertise_url_before_route_state -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay invalid_crm_tag -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay command_register_skip_validation_publishes_explicit_test_crm_tag -- --nocapture
```

Expected: tests fail until server validation, route-table direct mutation guards, relay state invalid-result mapping, and command-loop skip behavior are updated.

- [x] **Step 3: Delegate c2-server CrmTag validation to c2-wire**

In `core/transport/c2-server/src/server.rs`, replace CRM-only length checks in `validate_route_for_wire()` with:

```rust
c2_wire::handshake::validate_crm_tag(&route.crm_ns, &route.crm_name, &route.crm_ver)
    .map_err(ServerError::Protocol)?;
```

Keep route-name and method-name wire length checks.

- [x] **Step 4: Validate PyO3 route construction**

In `sdk/python/native/src/server_ffi.rs`, before constructing `CrmRoute` in `PyServer::build_route()`:

```rust
c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver)
    .map_err(PyValueError::new_err)?;
```

- [x] **Step 5: Delegate relay CrmTag validation to c2-wire and guard direct route-table mutation**

In `core/transport/c2-http/src/relay/route_table.rs`, remove the unused `MAX_HANDSHAKE_NAME_BYTES` import and replace the local CrmTag helper:

```rust
pub(crate) fn valid_crm_tag(crm_ns: &str, crm_name: &str, crm_ver: &str) -> bool {
    c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver).is_ok()
}
```

Delete `valid_crm_tag_field()`.

Add private `RouteTable` entry and tombstone validators that check both field syntax and mutation shape. Put `valid_route_entry()` and `valid_tombstone()` inside the `impl RouteTable` block so they can compare against `self.relay_id`; keep the small private-field helpers near the other local validators. A peer route must never carry private IPC/server identity, a local route must belong to this relay, and tombstones must not admit malformed route or relay identity:

```rust
fn valid_route_entry(&self, entry: &RouteEntry) -> bool {
    if !valid_route_name(&entry.name)
        || !valid_relay_id(&entry.relay_id)
        || !valid_relay_url(&entry.relay_url)
        || !valid_crm_tag(&entry.crm_ns, &entry.crm_name, &entry.crm_ver)
        || !entry.registered_at.is_finite()
    {
        return false;
    }

    match entry.locality {
        Locality::Local => {
            entry.relay_id == self.relay_id
                && entry
                    .server_id
                    .as_deref()
                    .is_some_and(valid_server_id)
                && entry
                    .server_instance_id
                    .as_deref()
                    .is_some_and(valid_server_instance_id)
                && entry
                    .ipc_address
                    .as_deref()
                    .is_some_and(valid_ipc_address)
        }
        Locality::Peer => {
            entry.relay_id != self.relay_id
                && entry.server_id.is_none()
                && entry.server_instance_id.is_none()
                && entry.ipc_address.is_none()
        }
    }
}

fn valid_tombstone(&self, tombstone: &RouteTombstone) -> bool {
    tombstone.removed_at.is_finite()
        && valid_route_name(&tombstone.name)
        && valid_relay_id(&tombstone.relay_id)
        && tombstone
            .server_id
            .as_deref()
            .map_or(true, valid_server_id)
}
```

Then make `register_route()` reject malformed direct writes before computing the key:

```rust
pub fn register_route(&mut self, entry: RouteEntry) -> bool {
    if !self.valid_route_entry(&entry) {
        return false;
    }
    let key = (entry.name.clone(), entry.relay_id.clone());
    if let Some(tombstone) = self.tombstones.get(&key) {
        if entry.registered_at <= tombstone.removed_at {
            return false;
        }
    }
    self.tombstones.remove(&key);
    self.routes.insert(key, entry);
    true
}
```

Make `apply_tombstone()` use the same low-level tombstone validator before computing the key or mutating state:

```rust
pub fn apply_tombstone(&mut self, mut tombstone: RouteTombstone) -> bool {
    if !self.valid_tombstone(&tombstone) {
        return false;
    }
    let key = (tombstone.name.clone(), tombstone.relay_id.clone());
    if let Some(entry) = self.routes.get(&key) {
        if entry.registered_at > tombstone.removed_at {
            return false;
        }
    }
    if let Some(existing) = self.tombstones.get(&key) {
        if existing.removed_at >= tombstone.removed_at {
            return false;
        }
        if tombstone.server_id.is_none() {
            tombstone.server_id = existing.server_id.clone();
        }
    }
    tombstone.observed_at = Instant::now();
    self.routes.remove(&key);
    self.tombstones.insert(key, tombstone);
    true
}
```

Replace the unregister helpers so tombstone validation happens before route removal. Do not remove an active route if the tombstone candidate is invalid, older than the active route, or rejected by `apply_tombstone()` because newer tombstone state already exists. The helper should clone the route for the return value, then let `apply_tombstone()` perform the only state mutation:

```rust
pub fn unregister_route_with_tombstone(
    &mut self,
    name: &str,
    relay_id: &str,
    removed_at: f64,
) -> Option<RouteEntry> {
    let tombstone = RouteTombstone {
        name: name.to_string(),
        relay_id: relay_id.to_string(),
        removed_at,
        server_id: None,
        observed_at: Instant::now(),
    };
    if !self.valid_tombstone(&tombstone) {
        return None;
    }
    let key = (name.to_string(), relay_id.to_string());
    if self
        .routes
        .get(&key)
        .is_some_and(|entry| entry.registered_at > removed_at)
    {
        return None;
    }
    let removed = self.routes.get(&key).cloned();
    if !self.apply_tombstone(tombstone) {
        return None;
    }
    removed
}

pub fn unregister_local_route_with_tombstone(
    &mut self,
    name: &str,
    server_id: &str,
) -> (Option<RouteEntry>, f64) {
    let relay_id = self.relay_id.clone();
    let removed_at = self.next_local_timestamp();
    let tombstone = RouteTombstone {
        name: name.to_string(),
        relay_id: relay_id.clone(),
        removed_at,
        server_id: Some(server_id.to_string()),
        observed_at: Instant::now(),
    };
    if !self.valid_tombstone(&tombstone) {
        return (None, removed_at);
    }
    let key = (name.to_string(), relay_id);
    if self
        .routes
        .get(&key)
        .is_some_and(|entry| entry.registered_at > removed_at)
    {
        return (None, removed_at);
    }
    let removed = self.routes.get(&key).cloned();
    if !self.apply_tombstone(tombstone) {
        return (None, removed_at);
    }
    (removed, removed_at)
}

pub fn unregister_local_route_if_matches(
    &mut self,
    expected: &RouteEntry,
) -> (Option<RouteEntry>, f64) {
    let relay_id = self.relay_id.clone();
    let key = (expected.name.clone(), relay_id.clone());
    let matches_expected = self
        .routes
        .get(&key)
        .is_some_and(|entry| local_route_matches(entry, expected));
    if !matches_expected {
        return (None, self.next_local_timestamp());
    }
    let removed_at = self.next_local_timestamp();
    let server_id = self.routes.get(&key).and_then(|entry| entry.server_id.clone());
    let tombstone = RouteTombstone {
        name: expected.name.clone(),
        relay_id,
        removed_at,
        server_id,
        observed_at: Instant::now(),
    };
    if !self.valid_tombstone(&tombstone) {
        return (None, removed_at);
    }
    if self
        .routes
        .get(&key)
        .is_some_and(|entry| entry.registered_at > removed_at)
    {
        return (None, removed_at);
    }
    let removed = self.routes.get(&key).cloned();
    if !self.apply_tombstone(tombstone) {
        return (None, removed_at);
    }
    (removed, removed_at)
}
```

Update `merge_snapshot()` tombstone prevalidation to reuse `valid_tombstone()` so snapshot handling and direct tombstone application cannot drift:

```rust
for tombstone in &tombstones {
    if tombstone.relay_id == self.relay_id {
        continue;
    }
    if !self.valid_tombstone(tombstone) {
        return;
    }
}
```

Add these local helper functions near the existing validators. Do not use a prefix-only IPC address check; `c2-http` already depends on `c2-ipc` under the `relay` feature, and `socket_path_from_ipc_address()` validates the IPC region through the canonical config validator. Keep `validate_server_instance_id_value()` as the one relay-local implementation and make both route-table bool checks and authority error checks reuse it:

```rust
fn valid_server_id(server_id: &str) -> bool {
    c2_config::validate_server_id(server_id).is_ok()
        && server_id.as_bytes().len() <= c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES
}

pub(crate) fn validate_server_instance_id_value(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("invalid server_instance_id: cannot be empty".to_string());
    }
    if value.as_bytes().len() > c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES {
        return Err(format!(
            "invalid server_instance_id: cannot exceed {} bytes",
            c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES
        ));
    }
    if value.trim() != value {
        return Err(
            "invalid server_instance_id: cannot contain leading or trailing whitespace".to_string(),
        );
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        return Err("invalid server_instance_id: cannot contain path separators".to_string());
    }
    if !value.is_ascii() {
        return Err("invalid server_instance_id: must be ASCII".to_string());
    }
    if value.chars().any(char::is_control) {
        return Err("invalid server_instance_id: cannot contain control characters".to_string());
    }
    Ok(())
}

fn valid_server_instance_id(value: &str) -> bool {
    validate_server_instance_id_value(value).is_ok()
}

fn valid_ipc_address(address: &str) -> bool {
    c2_ipc::socket_path_from_ipc_address(address).is_ok()
}

pub(crate) fn valid_relay_url(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(parsed) => {
            matches!(parsed.scheme(), "http" | "https")
                && parsed.host_str().is_some()
                && parsed.query().is_none()
                && parsed.fragment().is_none()
                && parsed.username().is_empty()
                && parsed.password().is_none()
        }
        Err(_) => false,
    }
}
```

The direct `register_route()` and `apply_tombstone()` checks are defense-in-depth for internal call sites and tests, not a replacement for authority validation.

In `core/transport/c2-http/src/relay/server.rs`, validate the effective advertise URL during startup after bind parsing and before `RelayState` is created:

```rust
let effective_advertise_url = config.effective_advertise_url();
if !crate::relay::route_table::valid_relay_url(&effective_advertise_url) {
    return Err(format!(
        "Invalid relay advertise_url '{}': must be an http(s) URL with a host",
        effective_advertise_url
    ));
}
```

- [x] **Step 6: Add invalid commit result and authority validation**

In `core/transport/c2-http/src/relay/state.rs`, extend:

```rust
pub enum RegisterCommitResult {
    Registered { entry: RouteEntry },
    SameOwner { entry: RouteEntry },
    Duplicate { existing_address: String },
    ConflictingOwner { existing_address: String },
    Invalid { reason: String },
}
```

Map validation errors in `commit_register_upstream()`:

```rust
Err(ControlError::InvalidName { reason })
| Err(ControlError::InvalidServerId { reason })
| Err(ControlError::InvalidServerInstanceId { reason })
| Err(ControlError::InvalidAddress { reason })
| Err(ControlError::ContractMismatch { reason }) => {
    RegisterCommitResult::Invalid { reason }
}
```

In `core/transport/c2-http/src/relay/authority.rs`, add an explicit address validation error:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlError {
    InvalidName { reason: String },
    InvalidServerId { reason: String },
    InvalidServerInstanceId { reason: String },
    InvalidAddress { reason: String },
    ContractMismatch { reason: String },
    AddressMismatch { existing_address: String },
    DuplicateRoute { existing_address: String },
    OwnerMismatch,
    NotFound,
}
```

Add the authority address validator:

```rust
pub(crate) fn validate_ipc_address(&self, address: &str) -> Result<(), ControlError> {
    c2_ipc::socket_path_from_ipc_address(address)
        .map(|_| ())
        .map_err(|err| ControlError::InvalidAddress {
            reason: err.to_string(),
        })
}
```

Immediately update every exhaustive `ControlError` match after adding `InvalidAddress`:

```bash
rg -n "ControlError::" core/transport/c2-http/src/relay -g '*.rs'
```

For unregister branches that cannot produce address validation errors, make the exhaustive arm explicit:

```rust
Err(ControlError::OwnerMismatch)
| Err(ControlError::AddressMismatch { .. })
| Err(ControlError::InvalidName { .. })
| Err(ControlError::InvalidServerId { .. })
| Err(ControlError::InvalidServerInstanceId { .. })
| Err(ControlError::InvalidAddress { .. })
| Err(ControlError::ContractMismatch { .. })
| Err(ControlError::DuplicateRoute { .. }) => UnregisterResult::OwnerMismatch,
```

In `core/transport/c2-http/src/relay/server.rs`, update `control_error_to_relay_error()` so command API validation errors remain user-visible validation errors:

```rust
fn control_error_to_relay_error(err: ControlError) -> RelayControlError {
    match err {
        ControlError::InvalidName { reason }
        | ControlError::InvalidServerId { reason }
        | ControlError::InvalidServerInstanceId { reason }
        | ControlError::InvalidAddress { reason }
        | ControlError::ContractMismatch { reason } => RelayControlError::Other(reason),
        ControlError::AddressMismatch { .. }
        | ControlError::DuplicateRoute { .. }
        | ControlError::OwnerMismatch
        | ControlError::NotFound => {
            RelayControlError::Other("invalid relay register command".to_string())
        }
    }
}
```

In `server.rs` command-loop `prepare_candidate_registration()` handling, include invalid addresses in the validation-error arm:

```rust
Err(ControlError::InvalidName { reason })
| Err(ControlError::InvalidServerId { reason })
| Err(ControlError::InvalidServerInstanceId { reason })
| Err(ControlError::InvalidAddress { reason })
| Err(ControlError::ContractMismatch { reason }) => {
    let _ = reply.send(Err(RelayControlError::Other(reason)));
    continue;
}
```

Make `register_local_preflight()`, `prepare_candidate_registration()`, `prepare_register()`, and `register_local()` validate candidate IPC addresses before owner probing or route-table writes. In `register_local_preflight()`:

```rust
self.validate_route_name(name)?;
self.validate_server_id(server_id)?;
self.validate_server_instance_id(server_instance_id)?;
self.validate_ipc_address(address)?;
```

In `prepare_candidate_registration()`:

```rust
self.validate_route_name(name)?;
self.validate_server_id(server_id)?;
self.validate_ipc_address(address)?;
```

At the start of `register_local()`:

```rust
self.validate_route_name(&name)?;
self.validate_server_id(&server_id)?;
self.validate_server_instance_id(&server_instance_id)?;
self.validate_ipc_address(&address)?;
self.validate_crm_tag(&crm_ns, &crm_name, &crm_ver)?;
```

Replace the private server-instance validator body in `authority.rs` with the route-table helper so the two layers cannot drift:

```rust
pub(crate) fn validate_server_instance_id(
    &self,
    server_instance_id: &str,
) -> Result<(), ControlError> {
    crate::relay::route_table::validate_server_instance_id_value(server_instance_id)
        .map_err(|reason| ControlError::InvalidServerInstanceId { reason })
}
```

Immediately update every exhaustive match on `RegisterCommitResult`:

```bash
rg -n "RegisterCommitResult::|commit_register_upstream\\(" core/transport/c2-http/src/relay -g '*.rs'
```

In the `core/transport/c2-http/src/relay/state.rs` test helper that unwraps successful local registration, include the new invalid case explicitly:

```rust
match state.commit_register_upstream(
    name.to_string(),
    server_id.to_string(),
    server_instance_id.to_string(),
    address.to_string(),
    crm_ns.to_string(),
    crm_name.to_string(),
    crm_ver.to_string(),
    client,
    None,
) {
    RegisterCommitResult::Registered { entry }
    | RegisterCommitResult::SameOwner { entry } => entry,
    RegisterCommitResult::Duplicate { existing_address }
    | RegisterCommitResult::ConflictingOwner { existing_address } => {
        panic!("unexpected duplicate route at {existing_address}")
    }
    RegisterCommitResult::Invalid { reason } => {
        panic!("unexpected invalid route in test helper: {reason}")
    }
}
```

In `core/transport/c2-http/src/relay/router.rs`, update the data-plane precheck hook match:

```rust
match state_for_hook.commit_register_upstream(
    route_for_hook.clone(),
    "server-new".into(),
    "server-new-instance".into(),
    new_address_for_hook,
    "test.other".into(),
    "OtherEcho".into(),
    "0.1.0".into(),
    new_client,
    None,
) {
    RegisterCommitResult::Registered { .. }
    | RegisterCommitResult::SameOwner { .. } => {}
    RegisterCommitResult::Duplicate { existing_address }
    | RegisterCommitResult::ConflictingOwner { existing_address } => {
        panic!("failed to install mismatched route in precheck hook: {existing_address}")
    }
    RegisterCommitResult::Invalid { reason } => {
        panic!("failed to install mismatched route in precheck hook: {reason}")
    }
}
```

Update any existing inline test fixture that writes empty local CrmTag fields through `RouteTable::register_route()` directly. For `route_authority_preflight_uses_route_table_when_connection_entry_is_missing`, use:

```rust
crm_ns: "test.old".into(),
crm_name: "OldGrid".into(),
crm_ver: "0.1.0".into(),
```

Also migrate direct `commit_register_upstream()` calls in relay tests that expect successful registration or owner-conflict behavior. Add constants near the top of each affected test module:

```rust
const TEST_CRM_NS: &str = "test.relay";
const TEST_CRM_NAME: &str = "RelayGrid";
const TEST_CRM_VER: &str = "0.1.0";
```

In `core/transport/c2-http/src/relay/state.rs`, replace the three `String::new()` CrmTag arguments in these tests with the constants:

```text
register_commit_rechecks_owner_after_preflight_no_owner
candidate_preparation_does_not_grant_registration_right_after_race
replacement_token_can_replace_same_slot_only_while_still_evicted
replacement_token_does_not_match_re_registered_same_address_owner
replacement_token_cannot_replace_reconnected_owner_slot
same_server_registration_is_idempotent_without_repairing_evicted_client
same_server_new_instance_refreshes_local_route_and_client
same_server_registration_with_different_address_conflicts
same_server_register_does_not_repair_evicted_slot
```

Each replacement must look like:

```rust
TEST_CRM_NS.to_string(),
TEST_CRM_NAME.to_string(),
TEST_CRM_VER.to_string(),
```

In `core/transport/c2-http/src/relay/router.rs`, apply the same replacement in:

```text
resolve_exposes_ipc_address_only_to_loopback_clients
call_unreachable_upstream_removes_stale_local_route
```

Do not replace the `String::new()` triplet in a test whose expected result is `RegisterCommitResult::Invalid { .. }`; those are the negative coverage for CrmTag validation. After the migration, run this scan:

```bash
rg -n "commit_register_upstream\\(|String::new\\(\\)" core/transport/c2-http/src/relay/state.rs core/transport/c2-http/src/relay/router.rs core/transport/c2-http/src/relay/server.rs
```

Expected: no successful relay registration fixture still passes empty CrmTag fields. Remaining empty triplets must be either the pre-Task-8 command-loop skip branch or tests that assert invalid CrmTag rejection.

In `core/transport/c2-http/src/relay/authority.rs`, add before the route-table write lock in `register_local()`:

```rust
self.validate_crm_tag(&crm_ns, &crm_name, &crm_ver)?;
```

In `attest_ipc_route_contract()`, after the empty-field check:

```rust
if !valid_crm_tag(crm_ns, crm_name, crm_ver) {
    return Err(ControlError::ContractMismatch {
        reason: format!("IPC upstream route '{route_name}' advertised an invalid CRM tag"),
    });
}
```

- [x] **Step 7: Reject invalid HTTP skip-registration tags**

In `core/transport/c2-http/src/relay/router.rs`, after reading body CRM fields and before `prepare_register()`:

```rust
let claimed_has_crm_tag = !crm_ns.is_empty() || !crm_name.is_empty() || !crm_ver.is_empty();
if (claimed_has_crm_tag || state.config().skip_ipc_validation)
    && !valid_crm_tag(&crm_ns, &crm_name, &crm_ver)
{
    return (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": "InvalidCrmTag",
            "message": "crm_ns, crm_name, and crm_ver must be non-empty, control-character-free, separator-free, and fit the IPC handshake field limit",
        })),
    )
        .into_response();
}
```

When matching `prepare_register()`, include invalid addresses in the existing bad-request arm:

```rust
Err(ControlError::InvalidName { reason })
| Err(ControlError::InvalidServerId { reason })
| Err(ControlError::InvalidServerInstanceId { reason })
| Err(ControlError::InvalidAddress { reason })
| Err(ControlError::ContractMismatch { reason }) => {
    return (StatusCode::BAD_REQUEST, reason).into_response();
}
```

Handle invalid commits:

```rust
RegisterCommitResult::Invalid { reason } => {
    close_arc_client(client);
    return (StatusCode::BAD_REQUEST, reason).into_response();
}
```

- [x] **Step 8: Keep command-loop skip-validation usable without empty tags**

In `core/transport/c2-http/src/relay/server.rs`, add:

```rust
const SKIP_VALIDATION_CRM_NS: &str = "test.relay.skip";
const SKIP_VALIDATION_CRM_NAME: &str = "SkipValidationUpstream";
const SKIP_VALIDATION_CRM_VER: &str = "0.1.0";
```

Replace the skip tuple in the command loop:

```rust
(
    format!("{server_id}-instance"),
    SKIP_VALIDATION_CRM_NS.to_string(),
    SKIP_VALIDATION_CRM_NAME.to_string(),
    SKIP_VALIDATION_CRM_VER.to_string(),
)
```

Handle invalid commit results:

```rust
RegisterCommitResult::Invalid { reason } => {
    tokio::spawn(async move { client.close_shared().await });
    Err(RelayControlError::Other(reason))
}
```

- [x] **Step 9: Run registration tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server register_route_rejects_invalid_crm_tag_fields -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay local_commit_rejects_invalid_ipc_address_without_fake_duplicate -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay register_route_rejects_invalid_identity_fields_without_mutation -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay apply_tombstone_rejects_invalid_identity_fields_without_mutation -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay unregister_local_route_rejects_invalid_tombstone_without_removing_route -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay valid_relay_url_rejects_non_base_urls -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay start_rejects_invalid_advertise_url_before_route_state -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay invalid_crm_tag -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay command_register_skip_validation_publishes_explicit_test_crm_tag -- --nocapture
cargo check --manifest-path sdk/python/native/Cargo.toml -q
```

Expected: all selected tests pass and the Python native crate still compiles after the `server_ffi.rs` route-construction validation change.

- [x] **Step 10: Commit**

```bash
git add core/transport/c2-server/src/server.rs sdk/python/native/src/server_ffi.rs core/transport/c2-http/src/relay/route_table.rs core/transport/c2-http/src/relay/authority.rs core/transport/c2-http/src/relay/state.rs core/transport/c2-http/src/relay/router.rs core/transport/c2-http/src/relay/server.rs
git commit -m "fix: enforce CrmTag validation at route mutation boundaries"
```

## Task 4: Centralize relay-id Validation At Config And Authority

**Files:**
- Modify: `core/foundation/c2-config/src/identity.rs`
- Modify: `core/foundation/c2-config/src/lib.rs`
- Modify: `core/foundation/c2-config/src/relay.rs`
- Modify: `core/transport/c2-http/src/relay/route_table.rs`
- Modify: `core/transport/c2-http/src/relay/authority.rs`
- Modify: `core/transport/c2-http/src/relay/peer_handlers.rs`
- Test: `core/foundation/c2-config/src/identity.rs`
- Test: `core/foundation/c2-config/src/relay.rs`
- Test: `core/transport/c2-http/src/relay/route_table.rs`
- Test: `core/transport/c2-http/src/relay/authority.rs`
- Test: `core/transport/c2-http/src/relay/peer_handlers.rs`

- [x] **Step 1: Add failing relay-id tests**

Add to `core/foundation/c2-config/src/identity.rs` tests:

```rust
#[test]
fn relay_id_rejects_path_like_or_control_values() {
    for value in [
        "",
        "../escape",
        "bad/name",
        "bad\\name",
        ".",
        "..",
        " leading",
        "trailing ",
        "bad\nrelay",
    ] {
        assert!(
            validate_relay_id(value).is_err(),
            "relay_id should be rejected: {value:?}"
        );
    }
}

#[test]
fn relay_id_accepts_generated_style_values() {
    validate_relay_id("host_1234_abcd1234").unwrap();
    validate_relay_id("relay-a").unwrap();
}
```

Add to `core/foundation/c2-config/src/relay.rs` tests:

```rust
#[test]
fn relay_config_rejects_invalid_relay_id() {
    let mut config = RelayConfig::default();
    config.relay_id = "bad\nrelay".to_string();

    let err = config.validate().expect_err("invalid relay_id should fail");
    assert!(err.contains("relay_id"));
}
```

Add to `core/transport/c2-http/src/relay/route_table.rs` tests:

```rust
#[test]
fn register_route_rejects_invalid_relay_id_without_mutation() {
    let mut rt = RouteTable::new("relay-a".into());

    let bad_relay = peer_entry("grid", "bad/relay", 1000.0);
    assert!(!rt.register_route(bad_relay));
    assert!(rt.resolve("grid").is_empty());
}

#[test]
fn apply_tombstone_rejects_path_like_relay_id_without_mutation() {
    let mut rt = RouteTable::new("relay-b".into());
    register_alive_peer(&mut rt, "relay-a");
    assert!(rt.register_route(peer_entry("grid", "relay-a", 1000.0)));

    assert!(!rt.apply_tombstone(RouteTombstone {
        name: "grid".into(),
        relay_id: "bad/relay".into(),
        removed_at: 2000.0,
        server_id: None,
        observed_at: Instant::now(),
    }));

    assert_eq!(rt.resolve("grid").len(), 1);
    assert!(rt.list_tombstones().is_empty());
}

#[test]
fn record_peer_join_rejects_invalid_relay_id_without_peer_mutation() {
    let mut rt = RouteTable::new("relay-a".into());

    assert!(!rt.record_peer_join("bad/relay".into(), "http://bad:8080".into()));
    assert!(!rt.has_peer("bad/relay"));

    assert!(!rt.record_peer_join("relay-a".into(), "http://self:8080".into()));
    assert!(!rt.has_peer("relay-a"));

    assert!(!rt.record_peer_join("relay-b".into(), "not a url".into()));
    assert!(!rt.has_peer("relay-b"));

    assert!(!rt.record_peer_join(
        "relay-c".into(),
        "http://relay-c:8080?token=secret".into(),
    ));
    assert!(!rt.has_peer("relay-c"));
}

#[test]
fn merge_snapshot_rejects_invalid_peer_url_without_partial_mutation() {
    let mut rt = RouteTable::new("relay-b".into());
    register_alive_peer(&mut rt, "relay-a");
    rt.register_route(peer_entry("existing", "relay-a", 1000.0));

    rt.merge_snapshot(FullSync {
        routes: vec![peer_entry("grid", "relay-c", 1000.0)],
        tombstones: vec![],
        peers: vec![PeerSnapshot {
            relay_id: "relay-c".into(),
            url: "not a url".into(),
            route_count: 1,
            status: PeerStatus::Alive,
        }],
    });

    assert_eq!(rt.resolve("existing").len(), 1);
    assert!(rt.resolve("grid").is_empty());
    assert!(rt.get_peer("relay-c").is_none());
}
```

Add this self-contained test module to the bottom of `core/transport/c2-http/src/relay/authority.rs`. Do not reuse private helpers from `state.rs` or `route_table.rs`; those helpers are scoped to their own `#[cfg(test)]` modules.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use c2_config::RelayConfig;

    use crate::relay::disseminator::Disseminator;
    use crate::relay::peer::PeerEnvelope;
    use crate::relay::state::RelayState;
    use crate::relay::types::{Locality, PeerSnapshot, RouteEntry};

    struct NullDisseminator;

    impl Disseminator for NullDisseminator {
        fn broadcast(
            &self,
            _envelope: PeerEnvelope,
            _peers: &[PeerSnapshot],
        ) -> Option<tokio::task::JoinHandle<()>> {
            None
        }
    }

    fn test_state() -> RelayState {
        RelayState::new(
            Arc::new(RelayConfig {
                relay_id: "relay-a".into(),
                advertise_url: "http://relay-a:8080".into(),
                ..RelayConfig::default()
            }),
            Arc::new(NullDisseminator),
        )
    }

    fn peer_entry(name: &str, relay_id: &str, registered_at: f64) -> RouteEntry {
        RouteEntry {
            name: name.into(),
            relay_id: relay_id.into(),
            relay_url: format!("http://{relay_id}:8080"),
            server_id: None,
            server_instance_id: None,
            ipc_address: None,
            crm_ns: "test.ns".into(),
            crm_name: "Grid".into(),
            crm_ver: "0.1.0".into(),
            locality: Locality::Peer,
            registered_at,
        }
    }

    #[test]
    fn authority_rejects_invalid_relay_id_before_peer_route_mutation() {
        let state = test_state();
        let authority = RouteAuthority::new(&state);
        let entry = peer_entry("grid", "bad\nrelay", 1000.0);

        let err = match authority.execute(RouteCommand::AnnouncePeer {
            sender_relay_id: "bad\nrelay".into(),
            entry,
        }) {
            Err(err) => err,
            Ok(_) => panic!("invalid relay id must not mutate route state"),
        };

        assert!(matches!(err, ControlError::OwnerMismatch));
        assert!(state.resolve("grid").is_empty());
    }
}
```

Add to `core/transport/c2-http/src/relay/peer_handlers.rs` tests:

```rust
#[tokio::test]
async fn join_rejects_invalid_relay_id_or_url_without_mutating_peer_table() {
    let state = test_state();
    let envelope = PeerEnvelope::new(
        "bad\nrelay",
        PeerMessage::RelayJoin {
            relay_id: "bad\nrelay".into(),
            url: "http://relay-b:8080".into(),
        },
    );

    let response = handle_peer_join(State(state.clone()), Json(envelope))
        .await
        .into_response();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(state.list_peers().is_empty());

    let bad_url = PeerEnvelope::new(
        "relay-b",
        PeerMessage::RelayJoin {
            relay_id: "relay-b".into(),
            url: "http://relay-b:8080?token=secret".into(),
        },
    );

    let response = handle_peer_join(State(state.clone()), Json(bad_url))
        .await
        .into_response();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(state.list_peers().is_empty());
}
```

- [x] **Step 2: Run failing relay-id tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config relay_id -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay relay_id -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay invalid_peer_url -- --nocapture
```

Expected: fails until canonical relay-id validation is added, authority uses it, and `RouteTable` rejects invalid peer joins and snapshot peer URLs itself.

Observed RED:
- `cargo test --manifest-path core/Cargo.toml -p c2-config relay_id -- --nocapture` failed because `validate_relay_id` did not exist.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay relay_id -- --nocapture` failed because `RouteTable::record_peer_join()` still returned `()`.
- Strict review added an extra RED check for default relay id generation: `cargo test --manifest-path core/Cargo.toml -p c2-config generated_relay_id -- --nocapture` failed until generated IDs were sanitized/truncated before validation.

- [x] **Step 3: Add canonical relay-id validation**

In `core/foundation/c2-config/src/identity.rs`, add:

```rust
pub fn validate_relay_id(relay_id: &str) -> Result<(), String> {
    validate_id("relay_id", relay_id)?;
    if relay_id.as_bytes().len() > 255 {
        return Err("relay_id cannot exceed 255 bytes".to_string());
    }
    Ok(())
}
```

In `core/foundation/c2-config/src/lib.rs`, re-export it:

```rust
pub use identity::{validate_ipc_region_id, validate_relay_id, validate_server_id};
```

In `core/foundation/c2-config/src/relay.rs`, call it at the start of `RelayConfig::validate()`:

```rust
crate::validate_relay_id(&self.relay_id)?;
```

- [x] **Step 4: Delegate relay mutation validation to c2-config**

In `core/transport/c2-http/src/relay/route_table.rs`, replace `valid_relay_id()`:

```rust
fn valid_relay_id(relay_id: &str) -> bool {
    c2_config::validate_relay_id(relay_id).is_ok()
}
```

In `core/transport/c2-http/src/relay/authority.rs`, replace `validate_relay_id()`:

```rust
pub(crate) fn validate_relay_id(&self, relay_id: &str) -> Result<(), ControlError> {
    c2_config::validate_relay_id(relay_id).map_err(|_| ControlError::OwnerMismatch)
}
```

This keeps malformed peer ids as ignored/unauthorized ownership attempts instead of exposing new public error semantics.

- [x] **Step 5: Add route-table and peer-handler early rejection**

In `core/transport/c2-http/src/relay/route_table.rs`, make peer-table mutation return whether it accepted the join. This is the low-level guard; handlers are not the authority for peer-table integrity:

```rust
pub fn record_peer_join(&mut self, relay_id: String, url: String) -> bool {
    if relay_id == self.relay_id || !valid_relay_id(&relay_id) || !valid_relay_url(&url) {
        return false;
    }

    let now = Instant::now();
    match self.peers.get_mut(&relay_id) {
        Some(peer) => {
            peer.url = url.clone();
            peer.last_heartbeat = now;
            peer.status = PeerStatus::Alive;
        }
        None => {
            self.peers.insert(
                relay_id.clone(),
                PeerInfo {
                    relay_id: relay_id.clone(),
                    url: url.clone(),
                    route_count: 0,
                    last_heartbeat: now,
                    status: PeerStatus::Alive,
                },
            );
        }
    }
    self.sync_peer_route_urls(&relay_id, &url);
    true
}
```

In `merge_snapshot()` peer prevalidation, reject malformed peer URLs before building replacement routes or applying tombstones:

```rust
for peer in &peers {
    if peer.relay_id == self.relay_id {
        continue;
    }
    if !valid_relay_id(&peer.relay_id) || !valid_relay_url(&peer.url) {
        return;
    }
}
```

In `core/transport/c2-http/src/relay/peer_handlers.rs`, add:

```rust
fn valid_peer_relay_id(relay_id: &str) -> bool {
    c2_config::validate_relay_id(relay_id).is_ok()
}

fn valid_peer_relay_url(url: &str) -> bool {
    crate::relay::route_table::valid_relay_url(url)
}
```

Before `record_peer_join()`:

```rust
if !valid_peer_relay_id(&sender_relay_id)
    || !valid_peer_relay_id(&relay_id)
    || !valid_peer_relay_url(&url)
{
    return (
        StatusCode::OK,
        Json(serde_json::json!({"status": "ignored"})),
    )
        .into_response();
}

let accepted = state.with_route_table_mut(|rt| rt.record_peer_join(relay_id, url));
if !accepted {
    return (
        StatusCode::OK,
        Json(serde_json::json!({"status": "ignored"})),
    )
        .into_response();
}
```

Before heartbeat mutation:

```rust
if !valid_peer_relay_id(&sender_relay_id) || !valid_peer_relay_id(&relay_id) {
    return StatusCode::OK;
}
```

In `handle_peer_leave()`, add immediately after `let sender_relay_id = envelope.sender_relay_id.clone();`:

```rust
if !valid_peer_relay_id(&sender_relay_id) {
    return StatusCode::OK;
}
```

In `handle_peer_digest()`, add immediately after `let sender_relay_id = envelope.sender_relay_id.clone();`:

```rust
if !valid_peer_relay_id(&sender_relay_id) {
    return StatusCode::OK.into_response();
}
```

- [x] **Step 6: Run relay-id tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config relay_id -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay relay_id -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay invalid_peer_url -- --nocapture
```

Expected: all selected tests pass.

Observed GREEN:
- `cargo test --manifest-path core/Cargo.toml -p c2-config relay_id -- --nocapture` passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay relay_id -- --nocapture` passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay invalid_peer_url -- --nocapture` passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-config -- --nocapture` passed: 52 tests.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture` passed: 203 tests.
- `cargo check --manifest-path sdk/python/native/Cargo.toml -q` passed.
- `git diff --check` and `git diff --cached --check` passed.

- [x] **Step 7: Commit**

```bash
git add core/foundation/c2-config/src/identity.rs core/foundation/c2-config/src/lib.rs core/foundation/c2-config/src/relay.rs core/transport/c2-http/src/relay/route_table.rs core/transport/c2-http/src/relay/authority.rs core/transport/c2-http/src/relay/peer_handlers.rs
git commit -m "fix: enforce relay id validation at relay authority"
```

## Task 5: Add Boundary Guards And Regression Coverage

**Files:**
- Modify: `sdk/python/tests/unit/test_sdk_boundary.py`
- Modify: `sdk/python/tests/integration/test_relay_mesh.py`
- Modify: `sdk/python/tests/integration/test_http_relay.py`

- [x] **Step 1: Add source-boundary guard tests**

Add to `sdk/python/tests/unit/test_sdk_boundary.py`:

```python
def test_relay_does_not_keep_second_crm_tag_validator():
    root = Path(__file__).resolve().parents[4]
    route_table = root / "core" / "transport" / "c2-http" / "src" / "relay" / "route_table.rs"
    source = route_table.read_text(encoding="utf-8")

    assert "fn valid_crm_tag_field" not in source
    assert "c2_wire::handshake::validate_crm_tag" in source


def test_route_authority_uses_canonical_relay_id_validator():
    root = Path(__file__).resolve().parents[4]
    authority = root / "core" / "transport" / "c2-http" / "src" / "relay" / "authority.rs"
    source = authority.read_text(encoding="utf-8")

    assert "c2_config::validate_relay_id" in source
    assert "relay_id.trim().is_empty()" not in source


def test_route_authority_reports_invalid_ipc_address_as_validation_error():
    root = Path(__file__).resolve().parents[4]
    authority = root / "core" / "transport" / "c2-http" / "src" / "relay" / "authority.rs"
    state = root / "core" / "transport" / "c2-http" / "src" / "relay" / "state.rs"
    authority_source = authority.read_text(encoding="utf-8")
    state_source = state.read_text(encoding="utf-8")

    assert "InvalidAddress { reason: String }" in authority_source
    assert "socket_path_from_ipc_address(address)" in authority_source
    assert "ControlError::InvalidAddress { reason }" in state_source


def test_python_crm_metadata_is_not_parsed_from_slash_tag():
    root = Path(__file__).resolve().parents[2] / "src" / "c_two" / "transport" / "server" / "native.py"
    source = root.read_text(encoding="utf-8")

    assert "tag.split('/')" not in source


def test_route_table_direct_mutations_validate_tombstones_and_private_identity():
    root = Path(__file__).resolve().parents[4]
    route_table = root / "core" / "transport" / "c2-http" / "src" / "relay" / "route_table.rs"
    source = route_table.read_text(encoding="utf-8")

    assert "fn valid_tombstone" in source
    assert "self.valid_tombstone(&tombstone)" in source
    assert "fn valid_server_instance_id" in source
    assert "fn valid_relay_url" in source
    assert "valid_relay_url(&entry.relay_url)" in source
    assert "valid_relay_url(&url)" in source
    assert "let removed = self.routes.get(&key).cloned();" in source
    assert "if !self.apply_tombstone(tombstone)" in source
    assert "c2_ipc::socket_path_from_ipc_address" in source
    assert 'starts_with("ipc://")' not in source
    assert "valid_nonempty_identity" not in source
```

- [x] **Step 2: Add skip-validation HTTP regression**

Add under `class TestSingleRelay` in `sdk/python/tests/integration/test_relay_mesh.py`:

```python
def test_skip_validation_register_rejects_incomplete_crm_tag(self, start_c3_relay):
    relay = start_c3_relay(skip_ipc_validation=True)
    req = _http_post(
        f"{relay.url}/_register",
        {
            "name": "grid",
            "server_id": "grid-a",
            "server_instance_id": "grid-a-instance",
            "address": "ipc://grid_a",
            "crm_ns": "test.mesh",
            "crm_name": "",
            "crm_ver": "0.1.0",
        },
    )

    with pytest.raises(urllib.error.HTTPError) as exc:
        urllib.request.urlopen(req, timeout=5)

    assert exc.value.code == 400
```

- [x] **Step 3: Add explicit HTTP relay mismatch coverage for slash/control headers**

Add under `class TestCcConnectHttp` in `sdk/python/tests/integration/test_http_relay.py`:

```python
def test_relay_call_rejects_invalid_expected_crm_header(self, start_c3_relay):
    relay = start_c3_relay(skip_ipc_validation=True)
    with httpx.Client(trust_env=False, timeout=5.0) as http:
        resp = http.post(
            f"{relay.url}/grid/get",
            content=b"",
            headers={
                "x-c2-expected-crm-ns": "test/grid",
                "x-c2-expected-crm-name": "Grid",
                "x-c2-expected-crm-ver": "0.1.0",
            },
        )

    assert resp.status_code == 400
    assert resp.json()["error"] == "InvalidCrmTag"
```

- [x] **Step 4: Rebuild Python native extension and c3 relay binary**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv sync --reinstall-package c-two
python tools/dev/c3_tool.py --build --link
```

Expected: pytest imports the rebuilt `_native` extension and relay integration tests start the rebuilt `c3` binary. If linking `c3` requires approval in a sandboxed agent session, request it instead of running relay tests against a stale binary.

Observed:
- First `uv sync --reinstall-package c-two` attempt failed on sandbox DNS access; reran with approval and rebuilt `_native`.
- `python tools/dev/c3_tool.py --build --link` built `c3` but refused to overwrite an existing link; reran `python tools/dev/c3_tool.py --build --link --force` with approval and linked the current binary.

- [x] **Step 5: Run boundary and regression tests**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_sdk_boundary.py::test_relay_does_not_keep_second_crm_tag_validator sdk/python/tests/unit/test_sdk_boundary.py::test_route_authority_uses_canonical_relay_id_validator sdk/python/tests/unit/test_sdk_boundary.py::test_route_authority_reports_invalid_ipc_address_as_validation_error sdk/python/tests/unit/test_sdk_boundary.py::test_python_crm_metadata_is_not_parsed_from_slash_tag sdk/python/tests/unit/test_sdk_boundary.py::test_route_table_direct_mutations_validate_tombstones_and_private_identity -q --timeout=30 -rs
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_relay_mesh.py::TestSingleRelay::test_skip_validation_register_rejects_incomplete_crm_tag -q --timeout=30 -rs
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_http_relay.py::TestCcConnectHttp::test_relay_call_rejects_invalid_expected_crm_header -q --timeout=30 -rs
```

Expected: all selected tests pass.

Observed GREEN:
- Boundary guards: 5 passed.
- `TestSingleRelay::test_skip_validation_register_rejects_incomplete_crm_tag`: passed.
- `TestCcConnectHttp::test_relay_call_rejects_invalid_expected_crm_header`: passed.

- [x] **Step 6: Commit**

```bash
git add sdk/python/tests/unit/test_sdk_boundary.py sdk/python/tests/integration/test_relay_mesh.py sdk/python/tests/integration/test_http_relay.py
git commit -m "test: guard identity validation authority boundaries"
```

## Task 6: Full Verification And Final Review

**Files:**
- No production files.

- [x] **Step 1: Format Rust code**

```bash
cargo fmt --manifest-path core/Cargo.toml --all --check
cargo fmt --manifest-path sdk/python/native/Cargo.toml --all --check
```

Expected: both format checks pass. If they fail, run the same commands without `--check`, inspect the diff, then rerun the checks.

Observed GREEN:
- `cargo fmt --manifest-path core/Cargo.toml --all --check` passed.
- `cargo fmt --manifest-path sdk/python/native/Cargo.toml --all --check` passed.

- [x] **Step 2: Run Rust verification**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-config -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-server -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-runtime -- --nocapture
cargo check --manifest-path sdk/python/native/Cargo.toml -q
```

Expected: all commands pass.

Observed GREEN:
- `c2-wire`: 85 passed.
- `c2-config`: 52 passed.
- `c2-server`: 83 passed.
- `c2-http --features relay`: 203 passed.
- `c2-http`: 15 passed.
- `c2-runtime`: 13 passed.
- `cargo check --manifest-path sdk/python/native/Cargo.toml -q` passed.

- [x] **Step 3: Rebuild Python native extension and c3 relay binary**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv sync --reinstall-package c-two
python tools/dev/c3_tool.py --build --link
```

Expected: `_native` and `c3` both reflect the current Rust sources before full Python verification starts.

Observed GREEN:
- `env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv sync --reinstall-package c-two` rebuilt `_native`.
- `python tools/dev/c3_tool.py --build --link --force` linked the current `c3`.

- [x] **Step 4: Run Python verification**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: full Python suite passes with the current skip count only.

Observed:
- First full run found one stale unit fixture in `test_name_collision.py` using an undecorated fake CRM. The implementation now correctly rejects that before Rust registration; the fixture was updated to use a valid `@cc.crm` contract while preserving the Rust-failure rollback path.
- Final full run passed: 765 passed, 2 skipped.

- [x] **Step 5: Run diff hygiene checks**

```bash
git diff --check
git status --short
```

Expected: no whitespace errors; status shows only intended source, test, and plan files.

Observed GREEN:
- `git diff --check` passed.
- `uv.lock` churn from `uv sync` was reverted back to the pre-existing `0.4.9` lock entry.

- [x] **Step 6: Perform final code review pass**

Review these exact invariants before declaring completion:

- `rg -n "\\bfn\\s+valid_crm_tag_field\\b|relay_id\\.trim\\(\\)\\.is_empty|tag\\.split\\('/'\\)" core/transport/c2-http/src/relay sdk/python/src`
  - Expected: no relay duplicate CrmTag helper, no authority relay-id trim-only validator, and no Python semantic parsing from slash tag. The canonical `c2-wire` helper is intentionally outside this search path.
- `rg -n "String::new\\(\\).*crm|crm_ns: String::new\\(\\)|crm_name: String::new\\(\\)|crm_ver: String::new\\(\\)" core/transport/c2-http/src/relay`
  - Expected: no relay registration path committing empty CrmTag metadata.
- `rg -n "commit_register_upstream\\(|String::new\\(\\)" core/transport/c2-http/src/relay/state.rs core/transport/c2-http/src/relay/router.rs core/transport/c2-http/src/relay/server.rs`
  - Expected: manually inspect every direct `commit_register_upstream()` call; no success/duplicate/same-owner fixture passes an empty CrmTag triplet. Empty triplets may remain only in deliberate invalid-CrmTag tests before assertion, not in successful registration setup.
- `rg -n "validate_crm_tag\\(" core/protocol/c2-wire/src core/transport/c2-server/src sdk/python/native/src core/transport/c2-http/src/relay`
  - Expected: hits in wire codec, server ingress, native ingress, relay route table/authority/router boundaries.
- `rg -n "valid_tombstone|apply_tombstone\\(" core/transport/c2-http/src/relay/route_table.rs`
  - Expected: `apply_tombstone()` rejects `!self.valid_tombstone(&tombstone)` before computing route keys or mutating state, `merge_snapshot()` prevalidates incoming tombstones with the same helper, and unregister helpers validate tombstones before route removal.
- `rg -n "starts_with\\(\"ipc://\"\\)|valid_nonempty_identity" core/transport/c2-http/src/relay/route_table.rs`
  - Expected: no prefix-only IPC address validation and no weak private identity helper; direct LOCAL route validation should use `valid_server_id`, `valid_server_instance_id`, and `c2_ipc::socket_path_from_ipc_address`.
- `rg -n "InvalidAddress|validate_ipc_address|socket_path_from_ipc_address\\(address\\)" core/transport/c2-http/src/relay/authority.rs core/transport/c2-http/src/relay/state.rs core/transport/c2-http/src/relay/router.rs`
  - Expected: invalid IPC addresses have an explicit authority error, map to `RegisterCommitResult::Invalid`, and return HTTP 400 rather than owner conflict or duplicate responses.
- `rg -n "valid_relay_url|record_peer_join\\(" core/transport/c2-http/src/relay/route_table.rs core/transport/c2-http/src/relay/peer_handlers.rs`
  - Expected: direct route entries, snapshot peers, and peer joins validate relay URLs before state mutation; `valid_relay_url()` rejects non-http(s), missing host, query string, fragment, and userinfo.
- `rg -n "RouteInfo\\([^\\n]*methods=.*crm_ns=|RouteInfo\\([^\\n]*\\[MethodEntry" sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py`
  - Expected: no old one-line `RouteInfo(...)` calls that rely on default empty CrmTag metadata. Any remaining direct construction must be multi-line and visibly pass `crm_ns`, `crm_name`, and `crm_ver`.

Observed GREEN:
- No duplicate relay CrmTag helper, trim-only relay-id validator, or Python slash-tag parsing hits.
- No relay registration path commits empty CrmTag metadata.
- Direct `commit_register_upstream()` call sites were inspected; success fixtures use explicit CRM constants or attested contract values, and empty/invalid data remains only in invalid-path tests.
- Canonical CrmTag validation hits are present in `c2-wire`, `c2-server`, Python native FFI, and relay authority/route-table boundaries.
- Tombstone, IPC address, relay URL, and peer join scans match the intended authority boundaries.
- One stale one-line `RouteInfo(...)` test fixture was found and converted to a multiline explicit TypeError fixture; the scan then returned no hits.

- [x] **Step 7: Final commit if verification changed tracked files**

```bash
git status --short
git diff --name-only
git add core/protocol/c2-wire/src/control.rs core/protocol/c2-wire/src/frame.rs core/protocol/c2-wire/src/handshake.rs core/protocol/c2-wire/src/tests.rs sdk/python/native/src/wire_ffi.rs sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py sdk/python/src/c_two/crm/meta.py sdk/python/src/c_two/transport/server/native.py sdk/python/src/c_two/transport/registry.py sdk/python/tests/unit/test_crm_decorator.py sdk/python/tests/integration/test_registry.py core/transport/c2-server/src/server.rs sdk/python/native/src/server_ffi.rs core/transport/c2-http/src/relay/route_table.rs core/transport/c2-http/src/relay/authority.rs core/transport/c2-http/src/relay/state.rs core/transport/c2-http/src/relay/router.rs core/transport/c2-http/src/relay/server.rs core/foundation/c2-config/src/identity.rs core/foundation/c2-config/src/lib.rs core/foundation/c2-config/src/relay.rs core/transport/c2-http/src/relay/peer_handlers.rs sdk/python/tests/unit/test_sdk_boundary.py sdk/python/tests/integration/test_relay_mesh.py sdk/python/tests/integration/test_http_relay.py docs/superpowers/plans/2026-05-09-identity-validation-authority-remediation.md
git commit -m "chore: verify identity validation authority hardening"
```

Only run this commit if formatting or verification produced tracked edits that were not included in earlier task commits. Do not use broad `git add core sdk/python docs`: first inspect `git diff -- <path>` for each dirty file, and stage only intentional implementation-plan paths. This matters in a dirty worktree because `sdk/python/tests/unit/test_sdk_boundary.py` and other touched files may already contain unrelated user edits.

## Self-Review

- Spec coverage: This plan covers the original strict-review findings: command-loop skip-validation empty CrmTags, c2-wire helper not enforced in codec paths, relay-id validation missing the authority choke point, and Python class-name/slash tag ambiguity. It also covers the follow-up plan-review gaps: unguarded direct tombstone mutation, non-atomic unregister+tombstone writes, weak direct route-table LOCAL private identity validation, missing invalid-address error mapping, and unvalidated relay URLs.
- Regression control: The plan keeps `skip_ipc_validation` usable only with explicit non-empty test metadata and rejects incomplete HTTP skip registrations.
- Destructive-review coverage: The plan now avoids undefined test helpers, uses current `Server::new()` signatures, migrates all Python `RouteInfo` test callers including `test_security.py`, rejects undecorated CRM classes before Python `connect()` creates IPC/HTTP clients, propagates `RegisterCommitResult::Invalid` through non-primary exhaustive matches, gives `authority.rs` self-contained relay-id fixtures, updates empty-tag inline test fixtures and direct `commit_register_upstream()` fixtures, strengthens direct route-table LOCAL private-field checks, guards direct tombstone mutation, keeps unregister operations atomic with tombstone writes by letting `apply_tombstone()` perform the only mutation, maps invalid IPC addresses as validation errors, validates relay base URLs before peer/route state mutation, keeps the first commit boundary after the Python facade migration, rebuilds `_native` and `c3` before Python verification, and scopes final invariant scans so they do not flag canonical authority code.
- Authority consistency: CrmTag validation is canonical in `c2-wire`; relay-id validation is canonical in `c2-config`; relay mutation uses `RouteAuthority` as the final gate.
- Internal mutation safety: `RouteTable::register_route()` rejects malformed direct entries, `RouteTable::apply_tombstone()` rejects malformed direct tombstones, unregister helpers do not remove routes when tombstone validation or tombstone application fails, and `RouteTable::record_peer_join()` rejects malformed peer ids and URLs, so future crate-internal call sites cannot bypass route identity invariants by skipping `RouteAuthority`.
- Performance: Validation runs at registration, handshake, resolve/header parsing, and peer ingestion boundaries. It does not add per-call serialization, route resolving, or payload copying.
- Security: Malformed identity fields and malformed relay base URLs cannot enter relay state through HTTP, peer gossip, background digest application, direct state commit, direct tombstone application, native server registration, decoded IPC handshake metadata, or Python client contract selection.
- Peer-table safety: `RouteTable::record_peer_join()` rejects malformed and self relay ids plus malformed URLs directly, so peer handler tests cannot be the only protection for peer-table integrity.
- Placeholder scan: No task uses unresolved placeholder wording; each task names exact files, concrete code changes, test commands, and expected outcomes.
