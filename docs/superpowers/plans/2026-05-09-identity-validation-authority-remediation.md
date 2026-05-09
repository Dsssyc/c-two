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
  - Delegate CrmTag validation to `c2-wire` and relay-id validation to `c2-config`.
- Modify `core/transport/c2-http/src/relay/authority.rs`
  - Enforce canonical CrmTag and relay-id validation before every route mutation.
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
- Python fast feedback mirrors the Rust rule, but Rust remains authoritative.
- Relay local state may never store empty CrmTag fields, including `skip_ipc_validation`.
- `skip_ipc_validation` remains test-only. HTTP skip registration must supply a complete CrmTag in the body. Command-loop skip registration, which has no CRM fields in its public CLI syntax, uses a fixed explicit test CrmTag and tests assert that behavior.
- Relay-id validation is enforced at `RouteAuthority`, not only at HTTP handlers. This covers HTTP peer endpoints, background digest application, and direct authority calls in tests.

## Task 1: Make c2-wire The Real CrmTag Authority

**Files:**
- Modify: `core/protocol/c2-wire/src/control.rs`
- Modify: `core/protocol/c2-wire/src/frame.rs`
- Modify: `core/protocol/c2-wire/src/handshake.rs`
- Test: `core/protocol/c2-wire/src/tests.rs`

- [ ] **Step 1: Add failing validator and codec tests**

Add these tests to `core/protocol/c2-wire/src/tests.rs`:

```rust
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

- [ ] **Step 6: Commit**

```bash
git add core/protocol/c2-wire/src/control.rs core/protocol/c2-wire/src/frame.rs core/protocol/c2-wire/src/handshake.rs core/protocol/c2-wire/src/tests.rs
git commit -m "fix: enforce canonical CrmTag validation in wire codec"
```

## Task 2: Align Python CrmTag Facades With Wire Authority

**Files:**
- Modify: `sdk/python/native/src/wire_ffi.rs`
- Modify: `sdk/python/tests/unit/test_wire.py`
- Modify: `sdk/python/src/c_two/crm/meta.py`
- Modify: `sdk/python/src/c_two/transport/server/native.py`
- Test: `sdk/python/tests/unit/test_crm_decorator.py`
- Test: `sdk/python/tests/unit/test_wire.py`

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

Add to `sdk/python/tests/unit/test_wire.py`:

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

def test_server_handshake_rejects_invalid_crm_tag():
    route = RouteInfo(
        name='grid',
        methods=[MethodEntry(name='get', index=0)],
        crm_ns='test.wire',
        crm_name='Bad\nRoute',
        crm_ver='0.1.0',
    )
    with pytest.raises(ValueError, match='control characters'):
        encode_server_handshake(
            [],
            CAP_CALL,
            [route],
            server_id='wire-server',
            server_instance_id='wire-instance',
        )
```

Update existing `RouteInfo("route_a", ...)` calls in `test_wire.py` to use `route_info(...)`.

- [ ] **Step 2: Run the failing tests**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_crm_decorator.py::TestIcrmValidation sdk/python/tests/unit/test_wire.py::TestHandshake -q --timeout=30 -rs
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

In `sdk/python/src/c_two/crm/meta.py`, add:

```python
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
    if any(ch < ' ' or ch == '\x7f' for ch in value):
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

- [ ] **Step 6: Run Python tests**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_crm_decorator.py::TestIcrmValidation sdk/python/tests/unit/test_wire.py -q --timeout=30 -rs
```

Expected: all selected tests pass.

- [ ] **Step 7: Commit**

```bash
git add sdk/python/native/src/wire_ffi.rs sdk/python/tests/unit/test_wire.py sdk/python/src/c_two/crm/meta.py sdk/python/src/c_two/transport/server/native.py sdk/python/tests/unit/test_crm_decorator.py
git commit -m "fix: align Python CrmTag facades with wire authority"
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
- Test: `core/transport/c2-http/src/relay/router.rs`
- Test: `core/transport/c2-http/src/relay/server.rs`
- Test: `core/transport/c2-http/src/relay/state.rs`

- [ ] **Step 1: Add failing server and relay tests**

Add to `core/transport/c2-server/src/server.rs` tests:

```rust
#[tokio::test]
async fn register_route_rejects_invalid_crm_tag_fields() {
    let s = Server::new(
        "ipc://invalid_crm_tag_route",
        ServerIpcConfig::default(),
        Some("invalid-crm-tag-route".to_string()),
    )
    .unwrap();
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

- [ ] **Step 5: Delegate relay CrmTag validation to c2-wire**

In `core/transport/c2-http/src/relay/route_table.rs`, replace the local CrmTag helper:

```rust
pub(crate) fn valid_crm_tag(crm_ns: &str, crm_name: &str, crm_ver: &str) -> bool {
    c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver).is_ok()
}
```

Delete `valid_crm_tag_field()`.

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

Add to `core/transport/c2-http/src/relay/authority.rs` tests:

```rust
#[test]
fn authority_rejects_invalid_relay_id_before_peer_route_mutation() {
    let state = RelayState::new(test_config(), null_disseminator());
    let authority = RouteAuthority::new(&state);
    let mut entry = peer_entry("grid", "bad\nrelay", 1000.0);
    entry.crm_ns = "test.ns".into();
    entry.crm_name = "Grid".into();
    entry.crm_ver = "0.1.0".into();

    let err = authority
        .execute(RouteCommand::AnnouncePeer {
            sender_relay_id: "bad\nrelay".into(),
            entry,
        })
        .expect_err("invalid relay id must not mutate route state");

    assert!(matches!(err, ControlError::OwnerMismatch));
    assert!(state.resolve("grid").is_empty());
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

Expected: fails until canonical relay-id validation is added and authority uses it.

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

- [ ] **Step 5: Add peer-handler early rejection**

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
    req = urllib.request.Request(
        f"{relay.url}/grid/get",
        data=b"",
        headers={
            "x-c2-expected-crm-ns": "test/grid",
            "x-c2-expected-crm-name": "Grid",
            "x-c2-expected-crm-ver": "0.1.0",
        },
        method="POST",
    )

    with pytest.raises(urllib.error.HTTPError) as exc:
        urllib.request.urlopen(req, timeout=5)

    assert exc.value.code == 400
```

- [ ] **Step 4: Run boundary and regression tests**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_sdk_boundary.py::test_relay_does_not_keep_second_crm_tag_validator sdk/python/tests/unit/test_sdk_boundary.py::test_route_authority_uses_canonical_relay_id_validator sdk/python/tests/unit/test_sdk_boundary.py::test_python_crm_metadata_is_not_parsed_from_slash_tag -q --timeout=30 -rs
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_relay_mesh.py::TestSingleRelay::test_skip_validation_register_rejects_incomplete_crm_tag -q --timeout=30 -rs
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_http_relay.py::TestCcConnectHttp::test_relay_call_rejects_invalid_expected_crm_header -q --timeout=30 -rs
```

Expected: all selected tests pass.

- [ ] **Step 5: Commit**

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

- [ ] **Step 3: Run Python verification**

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: full Python suite passes with the current skip count only.

- [ ] **Step 4: Run diff hygiene checks**

```bash
git diff --check
git status --short
```

Expected: no whitespace errors; status shows only intended source, test, and plan files.

- [ ] **Step 5: Perform final code review pass**

Review these exact invariants before declaring completion:

- `rg -n "valid_crm_tag_field|relay_id\\.trim\\(\\)\\.is_empty|tag\\.split\\('/'\\)" core sdk/python/src sdk/python/tests`
  - Expected: no production duplicate CrmTag helper, no authority relay-id trim-only validator, and no Python semantic parsing from slash tag.
- `rg -n "String::new\\(\\).*crm|crm_ns: String::new\\(\\)|crm_name: String::new\\(\\)|crm_ver: String::new\\(\\)" core/transport/c2-http/src/relay`
  - Expected: no relay registration path committing empty CrmTag metadata.
- `rg -n "validate_crm_tag\\(" core/protocol/c2-wire/src core/transport/c2-server/src sdk/python/native/src core/transport/c2-http/src/relay`
  - Expected: hits in wire codec, server ingress, native ingress, relay route table/authority/router boundaries.

- [ ] **Step 6: Final commit if verification changed tracked files**

```bash
git add core sdk/python docs
git commit -m "chore: verify identity validation authority hardening"
```

Only run this commit if formatting or verification produced tracked edits that were not included in earlier task commits.

## Self-Review

- Spec coverage: This plan covers the four strict-review findings: command-loop skip-validation empty CrmTags, c2-wire helper not enforced in codec paths, relay-id validation missing the authority choke point, and Python class-name/slash tag ambiguity.
- Regression control: The plan keeps `skip_ipc_validation` usable only with explicit non-empty test metadata and rejects incomplete HTTP skip registrations.
- Authority consistency: CrmTag validation is canonical in `c2-wire`; relay-id validation is canonical in `c2-config`; relay mutation uses `RouteAuthority` as the final gate.
- Performance: Validation runs at registration, handshake, resolve/header parsing, and peer ingestion boundaries. It does not add per-call serialization, route resolving, or payload copying.
- Security: Malformed identity fields cannot enter relay state through HTTP, peer gossip, background digest application, direct state commit, native server registration, or decoded IPC handshake metadata.
- Placeholder scan: No task uses unresolved placeholder wording; each task names exact files, concrete code changes, test commands, and expected outcomes.
