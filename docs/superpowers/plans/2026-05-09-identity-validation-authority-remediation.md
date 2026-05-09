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
  - Delegate CrmTag validation to `c2-wire`, relay-id validation to `c2-config`, reject invalid direct `register_route()` entries as defense-in-depth, and reject invalid `record_peer_join()` peer-table mutations.
- Modify `core/transport/c2-http/src/relay/authority.rs`
  - Enforce canonical CrmTag and relay-id validation before every route mutation and add self-contained authority tests with local fixtures.
- Modify `core/transport/c2-http/src/relay/state.rs`
  - Add `RegisterCommitResult::Invalid { reason }`.
- Modify `core/transport/c2-http/src/relay/router.rs`
  - Reject invalid HTTP register/resolve/header CrmTags and handle invalid commit results.
- Modify `core/transport/c2-http/src/relay/server.rs`
  - Keep command-loop skip-validation usable by publishing an explicit test CrmTag.
- Modify `core/transport/c2-http/src/relay/peer_handlers.rs`
  - Add early relay-id rejection tests while relying on authority for final enforcement.
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
- `RouteTable::record_peer_join()` directly mutates the peer table, so relay-id validation must live there too. Peer handlers should reject malformed join envelopes early for clean control-flow, but the route table must still return `false` without mutation if a future internal caller bypasses handler checks.
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

- [ ] **Step 1: Add failing validator and codec tests**

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

- [ ] **Step 2: Run the failing tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire crm_tag -- --nocapture
```

Expected: fails because `validate_crm_tag()` and invalid text error variants do not exist or are not wired into the codec.

- [ ] **Step 3: Add descriptive text-field error variants**

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

- [ ] **Step 4: Implement canonical CrmTag validation**

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

- [ ] **Step 5: Run c2-wire tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire crm_tag -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-wire -- --nocapture
```

Expected: all c2-wire tests pass.

- [ ] **Step 6: Do not commit this intermediate state**

Do not commit after Task 1 alone. At this point `c2-wire` rejects empty CrmTags, while the Python `RouteInfo` facade still has empty defaults. Continue directly to Task 2 and make the first commit after the Python facade and tests have been migrated.

## Task 2: Align Python CrmTag Facades With Wire Authority

**Files:**
- Modify: `sdk/python/native/src/wire_ffi.rs`
- Modify: `sdk/python/tests/unit/test_wire.py`
- Modify: `sdk/python/tests/unit/test_security.py`
- Modify: `sdk/python/src/c_two/crm/meta.py`
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Test: `sdk/python/tests/unit/test_crm_decorator.py`
- Test: `sdk/python/tests/unit/test_wire.py`
- Test: `sdk/python/tests/unit/test_security.py`

- [ ] **Step 1: Write failing Python facade tests**

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

- [ ] **Step 2: Run the failing tests**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_crm_decorator.py::TestIcrmValidation sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py::TestHandshakeBoundsChecking -q --timeout=30 -rs
```

Expected: fails until Python facade validation and explicit RouteInfo signatures are updated.

- [ ] **Step 3: Require explicit CrmTag fields in Python RouteInfo**

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

- [ ] **Step 4: Add Python CRM field validation**

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

- [ ] **Step 5: Stop parsing namespace from `__tag__`**

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

- [ ] **Step 6: Rebuild the Python native extension**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv sync --reinstall-package c-two
```

Expected: the PyO3 extension is rebuilt from the current Rust sources. Do not run the pytest commands below against a stale `_native` extension.

- [ ] **Step 7: Run Python tests**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_crm_decorator.py::TestIcrmValidation sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py::TestHandshakeBoundsChecking -q --timeout=30 -rs
```

Expected: all selected tests pass.

- [ ] **Step 8: Commit Task 1 and Task 2 together**

```bash
git add core/protocol/c2-wire/src/control.rs core/protocol/c2-wire/src/frame.rs core/protocol/c2-wire/src/handshake.rs core/protocol/c2-wire/src/tests.rs sdk/python/native/src/wire_ffi.rs sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py sdk/python/src/c_two/crm/meta.py sdk/python/src/c_two/transport/server/native.py sdk/python/tests/unit/test_crm_decorator.py
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

- [ ] **Step 1: Add failing server and relay tests**

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

- [ ] **Step 2: Run failing tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server register_route_rejects_invalid_crm_tag_fields -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay register_route_rejects_invalid_identity_fields_without_mutation -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay invalid_crm_tag -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay command_register_skip_validation_publishes_explicit_test_crm_tag -- --nocapture
```

Expected: tests fail until server, relay state, and command-loop skip behavior are updated.

- [ ] **Step 3: Delegate c2-server CrmTag validation to c2-wire**

In `core/transport/c2-server/src/server.rs`, replace CRM-only length checks in `validate_route_for_wire()` with:

```rust
c2_wire::handshake::validate_crm_tag(&route.crm_ns, &route.crm_name, &route.crm_ver)
    .map_err(ServerError::Protocol)?;
```

Keep route-name and method-name wire length checks.

- [ ] **Step 4: Validate PyO3 route construction**

In `sdk/python/native/src/server_ffi.rs`, before constructing `CrmRoute` in `PyServer::build_route()`:

```rust
c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver)
    .map_err(PyValueError::new_err)?;
```

- [ ] **Step 5: Delegate relay CrmTag validation to c2-wire and guard direct route-table mutation**

In `core/transport/c2-http/src/relay/route_table.rs`, remove the unused `MAX_HANDSHAKE_NAME_BYTES` import and replace the local CrmTag helper:

```rust
pub(crate) fn valid_crm_tag(crm_ns: &str, crm_name: &str, crm_ver: &str) -> bool {
    c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver).is_ok()
}
```

Delete `valid_crm_tag_field()`.

Add a private `RouteTable` entry validator that checks both field syntax and route shape. Put `valid_route_entry()` inside the `impl RouteTable` block so it can compare against `self.relay_id`; keep the small `valid_nonempty_identity()` helper near the other local validators. A peer route must never carry private IPC/server identity, and a local route must belong to this relay:

```rust
fn valid_nonempty_identity(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

fn valid_route_entry(&self, entry: &RouteEntry) -> bool {
    if !valid_route_name(&entry.name)
        || !valid_relay_id(&entry.relay_id)
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
                    .is_some_and(|server_id| c2_config::validate_server_id(server_id).is_ok())
                && entry
                    .server_instance_id
                    .as_deref()
                    .is_some_and(valid_nonempty_identity)
                && entry
                    .ipc_address
                    .as_deref()
                    .is_some_and(|address| address.starts_with("ipc://"))
        }
        Locality::Peer => {
            entry.relay_id != self.relay_id
                && entry.server_id.is_none()
                && entry.server_instance_id.is_none()
                && entry.ipc_address.is_none()
        }
    }
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

Keep the existing `merge_snapshot()` prevalidation. The direct `register_route()` check is defense-in-depth for internal call sites and tests, not a replacement for authority validation.

- [ ] **Step 6: Add invalid commit result and authority validation**

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
| Err(ControlError::ContractMismatch { reason }) => {
    RegisterCommitResult::Invalid { reason }
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

- [ ] **Step 7: Reject invalid HTTP skip-registration tags**

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

Handle invalid commits:

```rust
RegisterCommitResult::Invalid { reason } => {
    close_arc_client(client);
    return (StatusCode::BAD_REQUEST, reason).into_response();
}
```

- [ ] **Step 8: Keep command-loop skip-validation usable without empty tags**

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

- [ ] **Step 9: Run registration tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server register_route_rejects_invalid_crm_tag_fields -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay register_route_rejects_invalid_identity_fields_without_mutation -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay invalid_crm_tag -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay command_register_skip_validation_publishes_explicit_test_crm_tag -- --nocapture
```

Expected: all selected tests pass.

- [ ] **Step 10: Commit**

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

- [ ] **Step 1: Add failing relay-id tests**

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
fn record_peer_join_rejects_invalid_relay_id_without_peer_mutation() {
    let mut rt = RouteTable::new("relay-a".into());

    assert!(!rt.record_peer_join("bad/relay".into(), "http://bad:8080".into()));
    assert!(!rt.has_peer("bad/relay"));

    assert!(!rt.record_peer_join("relay-a".into(), "http://self:8080".into()));
    assert!(!rt.has_peer("relay-a"));
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
async fn join_rejects_invalid_relay_id_without_mutating_peer_table() {
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
}
```

- [ ] **Step 2: Run failing relay-id tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config relay_id -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay relay_id -- --nocapture
```

Expected: fails until canonical relay-id validation is added, authority uses it, and `RouteTable` rejects invalid peer joins itself.

- [ ] **Step 3: Add canonical relay-id validation**

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

- [ ] **Step 4: Delegate relay mutation validation to c2-config**

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

- [ ] **Step 5: Add route-table and peer-handler early rejection**

In `core/transport/c2-http/src/relay/route_table.rs`, make peer-table mutation return whether it accepted the join. This is the low-level guard; handlers are not the authority for peer-table integrity:

```rust
pub fn record_peer_join(&mut self, relay_id: String, url: String) -> bool {
    if relay_id == self.relay_id || !valid_relay_id(&relay_id) {
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

In `core/transport/c2-http/src/relay/peer_handlers.rs`, add:

```rust
fn valid_peer_relay_id(relay_id: &str) -> bool {
    c2_config::validate_relay_id(relay_id).is_ok()
}
```

Before `record_peer_join()`:

```rust
if !valid_peer_relay_id(&sender_relay_id) || !valid_peer_relay_id(&relay_id) {
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

At the start of leave and digest handlers, reject invalid `sender_relay_id` with `StatusCode::OK` before any state lookup.

- [ ] **Step 6: Run relay-id tests**

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config relay_id -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay relay_id -- --nocapture
```

Expected: all selected tests pass.

- [ ] **Step 7: Commit**

```bash
git add core/foundation/c2-config/src/identity.rs core/foundation/c2-config/src/lib.rs core/foundation/c2-config/src/relay.rs core/transport/c2-http/src/relay/route_table.rs core/transport/c2-http/src/relay/authority.rs core/transport/c2-http/src/relay/peer_handlers.rs
git commit -m "fix: enforce relay id validation at relay authority"
```

## Task 5: Add Boundary Guards And Regression Coverage

**Files:**
- Modify: `sdk/python/tests/unit/test_sdk_boundary.py`
- Modify: `sdk/python/tests/integration/test_relay_mesh.py`
- Modify: `sdk/python/tests/integration/test_http_relay.py`

- [ ] **Step 1: Add source-boundary guard tests**

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


def test_python_crm_metadata_is_not_parsed_from_slash_tag():
    root = Path(__file__).resolve().parents[2] / "src" / "c_two" / "transport" / "server" / "native.py"
    source = root.read_text(encoding="utf-8")

    assert "tag.split('/')" not in source
```

- [ ] **Step 2: Add skip-validation HTTP regression**

Add to `sdk/python/tests/integration/test_relay_mesh.py`:

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

- [ ] **Step 3: Add explicit HTTP relay mismatch coverage for slash/control headers**

Add to `sdk/python/tests/integration/test_http_relay.py`:

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

- [ ] **Step 4: Rebuild Python native extension and c3 relay binary**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv sync --reinstall-package c-two
python tools/dev/c3_tool.py --build --link
```

Expected: pytest imports the rebuilt `_native` extension and relay integration tests start the rebuilt `c3` binary. If linking `c3` requires approval in a sandboxed agent session, request it instead of running relay tests against a stale binary.

- [ ] **Step 5: Run boundary and regression tests**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_sdk_boundary.py::test_relay_does_not_keep_second_crm_tag_validator sdk/python/tests/unit/test_sdk_boundary.py::test_route_authority_uses_canonical_relay_id_validator sdk/python/tests/unit/test_sdk_boundary.py::test_python_crm_metadata_is_not_parsed_from_slash_tag -q --timeout=30 -rs
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_relay_mesh.py::TestSingleRelay::test_skip_validation_register_rejects_incomplete_crm_tag -q --timeout=30 -rs
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_http_relay.py::TestCcConnectHttp::test_relay_call_rejects_invalid_expected_crm_header -q --timeout=30 -rs
```

Expected: all selected tests pass.

- [ ] **Step 6: Commit**

```bash
git add sdk/python/tests/unit/test_sdk_boundary.py sdk/python/tests/integration/test_relay_mesh.py sdk/python/tests/integration/test_http_relay.py
git commit -m "test: guard identity validation authority boundaries"
```

## Task 6: Full Verification And Final Review

**Files:**
- No production files.

- [ ] **Step 1: Format Rust code**

```bash
cargo fmt --manifest-path core/Cargo.toml --all --check
cargo fmt --manifest-path sdk/python/native/Cargo.toml --all --check
```

Expected: both format checks pass. If they fail, run the same commands without `--check`, inspect the diff, then rerun the checks.

- [ ] **Step 2: Run Rust verification**

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

- [ ] **Step 3: Rebuild Python native extension and c3 relay binary**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv sync --reinstall-package c-two
python tools/dev/c3_tool.py --build --link
```

Expected: `_native` and `c3` both reflect the current Rust sources before full Python verification starts.

- [ ] **Step 4: Run Python verification**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: full Python suite passes with the current skip count only.

- [ ] **Step 5: Run diff hygiene checks**

```bash
git diff --check
git status --short
```

Expected: no whitespace errors; status shows only intended source, test, and plan files.

- [ ] **Step 6: Perform final code review pass**

Review these exact invariants before declaring completion:

- `rg -n "\\bfn\\s+valid_crm_tag_field\\b|relay_id\\.trim\\(\\)\\.is_empty|tag\\.split\\('/'\\)" core/transport/c2-http/src/relay sdk/python/src`
  - Expected: no relay duplicate CrmTag helper, no authority relay-id trim-only validator, and no Python semantic parsing from slash tag. The canonical `c2-wire` helper is intentionally outside this search path.
- `rg -n "String::new\\(\\).*crm|crm_ns: String::new\\(\\)|crm_name: String::new\\(\\)|crm_ver: String::new\\(\\)" core/transport/c2-http/src/relay`
  - Expected: no relay registration path committing empty CrmTag metadata.
- `rg -n "validate_crm_tag\\(" core/protocol/c2-wire/src core/transport/c2-server/src sdk/python/native/src core/transport/c2-http/src/relay`
  - Expected: hits in wire codec, server ingress, native ingress, relay route table/authority/router boundaries.
- `rg -n "RouteInfo\\([^\\n]*methods=.*crm_ns=|RouteInfo\\([^\\n]*\\[MethodEntry" sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py`
  - Expected: no old one-line `RouteInfo(...)` calls that rely on default empty CrmTag metadata. Any remaining direct construction must be multi-line and visibly pass `crm_ns`, `crm_name`, and `crm_ver`.

- [ ] **Step 7: Final commit if verification changed tracked files**

```bash
git status --short
git diff --name-only
git add core/protocol/c2-wire/src/control.rs core/protocol/c2-wire/src/frame.rs core/protocol/c2-wire/src/handshake.rs core/protocol/c2-wire/src/tests.rs sdk/python/native/src/wire_ffi.rs sdk/python/tests/unit/test_wire.py sdk/python/tests/unit/test_security.py sdk/python/src/c_two/crm/meta.py sdk/python/src/c_two/transport/server/native.py sdk/python/tests/unit/test_crm_decorator.py core/transport/c2-server/src/server.rs sdk/python/native/src/server_ffi.rs core/transport/c2-http/src/relay/route_table.rs core/transport/c2-http/src/relay/authority.rs core/transport/c2-http/src/relay/state.rs core/transport/c2-http/src/relay/router.rs core/transport/c2-http/src/relay/server.rs core/foundation/c2-config/src/identity.rs core/foundation/c2-config/src/lib.rs core/foundation/c2-config/src/relay.rs core/transport/c2-http/src/relay/peer_handlers.rs sdk/python/tests/unit/test_sdk_boundary.py sdk/python/tests/integration/test_relay_mesh.py sdk/python/tests/integration/test_http_relay.py docs/superpowers/plans/2026-05-09-identity-validation-authority-remediation.md
git commit -m "chore: verify identity validation authority hardening"
```

Only run this commit if formatting or verification produced tracked edits that were not included in earlier task commits. Do not use broad `git add core sdk/python docs`: first inspect `git diff -- <path>` for each dirty file, and stage only intentional implementation-plan paths. This matters in a dirty worktree because `sdk/python/tests/unit/test_sdk_boundary.py` and other touched files may already contain unrelated user edits.

## Self-Review

- Spec coverage: This plan covers the four strict-review findings: command-loop skip-validation empty CrmTags, c2-wire helper not enforced in codec paths, relay-id validation missing the authority choke point, and Python class-name/slash tag ambiguity.
- Regression control: The plan keeps `skip_ipc_validation` usable only with explicit non-empty test metadata and rejects incomplete HTTP skip registrations.
- Destructive-review coverage: The plan now avoids undefined test helpers, uses current `Server::new()` signatures, migrates all Python `RouteInfo` test callers including `test_security.py`, propagates `RegisterCommitResult::Invalid` through non-primary exhaustive matches, gives `authority.rs` self-contained relay-id fixtures, updates empty-tag inline test fixtures, keeps the first commit boundary after the Python facade migration, rebuilds `_native` and `c3` before Python verification, and scopes final invariant scans so they do not flag canonical authority code.
- Authority consistency: CrmTag validation is canonical in `c2-wire`; relay-id validation is canonical in `c2-config`; relay mutation uses `RouteAuthority` as the final gate.
- Internal mutation safety: `RouteTable::register_route()` rejects malformed direct entries too, so future crate-internal call sites cannot bypass route identity invariants by skipping `RouteAuthority`.
- Performance: Validation runs at registration, handshake, resolve/header parsing, and peer ingestion boundaries. It does not add per-call serialization, route resolving, or payload copying.
- Security: Malformed identity fields cannot enter relay state through HTTP, peer gossip, background digest application, direct state commit, native server registration, or decoded IPC handshake metadata.
- Peer-table safety: `RouteTable::record_peer_join()` rejects malformed and self relay ids directly, so peer handler tests cannot be the only protection for peer-table integrity.
- Placeholder scan: No task uses unresolved placeholder wording; each task names exact files, concrete code changes, test commands, and expected outcomes.
