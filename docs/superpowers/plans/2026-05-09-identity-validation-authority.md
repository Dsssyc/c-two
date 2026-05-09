# Identity Validation Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make CrmTag and relay identity validation canonical, Rust-owned, and consistently enforced before any route or peer identity enters runtime state.

**Architecture:** Add one canonical CrmTag validation surface in `c2-wire`, because the size limit is the IPC handshake field limit and every route contract crosses that protocol boundary. Add one canonical relay-id validation surface in `c2-config`, because relay identity is configuration-owned and used by relay mesh state. Relay, server, runtime, and Python bindings will call these validators at ingress; route table internals then assume stored identity data is valid.

**Tech Stack:** Rust workspace crates (`c2-wire`, `c2-config`, `c2-server`, `c2-runtime`, `c2-http`, `c2-python-native`), Python SDK, pytest, cargo tests.

---

## File Structure

- Modify `core/protocol/c2-wire/src/handshake.rs`
  - Add `validate_crm_tag_field()` and `validate_crm_tag()` helpers.
  - Keep `RouteInfo` wire shape unchanged to avoid unrelated protocol churn.
- Modify `core/protocol/c2-wire/src/tests.rs`
  - Add protocol-level tests for CrmTag validation rules.
- Modify `core/foundation/c2-config/src/identity.rs`
  - Add `validate_relay_id()`.
- Modify `core/foundation/c2-config/src/lib.rs`
  - Re-export `validate_relay_id`.
- Modify `core/foundation/c2-config/src/relay.rs`
  - Validate configured `relay_id`.
- Modify `core/transport/c2-server/src/server.rs`
  - Enforce CrmTag validation during native route registration.
- Modify `sdk/python/native/src/server_ffi.rs`
  - Validate Python-provided CrmTag before building a `CrmRoute`, so errors fail before mutable server state.
- Modify `core/transport/c2-http/src/relay/route_table.rs`
  - Replace the relay-local CrmTag validator with the `c2-wire` helper.
  - Make `valid_relay_id()` delegate to `c2-config`.
- Modify `core/transport/c2-http/src/relay/authority.rs`
  - Validate attested IPC CrmTags and local commit CrmTags.
  - Use canonical relay-id validation for peer commands.
- Modify `core/transport/c2-http/src/relay/router.rs`
  - Reject invalid claimed CrmTag fields before `skip_ipc_validation` commit.
  - Keep non-skip behavior derived from attested IPC metadata.
- Modify `core/transport/c2-http/src/relay/peer_handlers.rs`
  - Reject invalid join/heartbeat/leave sender relay ids before mutating peer state.
- Modify `sdk/python/src/c_two/crm/meta.py`
  - Add Python-side fast feedback for invalid CRM namespace/version/class-name components.
- Modify focused tests under:
  - `core/transport/c2-http/src/relay/router.rs`
  - `core/transport/c2-http/src/relay/peer_handlers.rs`
  - `core/transport/c2-http/src/relay/route_table.rs`
  - `sdk/python/tests/unit/test_crm_decorator.py`

## Task 1: Canonical CrmTag Validator In c2-wire

**Files:**
- Modify: `core/protocol/c2-wire/src/handshake.rs`
- Test: `core/protocol/c2-wire/src/tests.rs`

- [ ] **Step 1: Write failing protocol tests**

Add these tests to `core/protocol/c2-wire/src/tests.rs`:

```rust
#[test]
fn crm_tag_validator_accepts_normal_contract_identity() {
    c2_wire::handshake::validate_crm_tag("test.grid", "Grid", "0.1.0").unwrap();
}

#[test]
fn crm_tag_validator_rejects_empty_fields() {
    for (crm_ns, crm_name, crm_ver) in [
        ("", "Grid", "0.1.0"),
        ("test.grid", "", "0.1.0"),
        ("test.grid", "Grid", ""),
    ] {
        let err = c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver)
            .expect_err("empty CrmTag field must fail");
        assert!(err.contains("cannot be empty"), "unexpected error: {err}");
    }
}

#[test]
fn crm_tag_validator_rejects_control_characters_and_wrapped_whitespace() {
    for (crm_ns, crm_name, crm_ver) in [
        ("test.grid\n", "Grid", "0.1.0"),
        ("test.grid", " Grid", "0.1.0"),
        ("test.grid", "Grid\0Injected", "0.1.0"),
        ("test.grid", "Grid", "0.1.0\t"),
    ] {
        let err = c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver)
            .expect_err("malformed CrmTag field must fail");
        assert!(
            err.contains("control characters") || err.contains("leading or trailing whitespace"),
            "unexpected error: {err}",
        );
    }
}

#[test]
fn crm_tag_validator_rejects_fields_that_cannot_fit_handshake() {
    let too_long = "x".repeat(c2_wire::handshake::MAX_HANDSHAKE_NAME_BYTES + 1);
    let err = c2_wire::handshake::validate_crm_tag("test.grid", &too_long, "0.1.0")
        .expect_err("oversized CrmTag field must fail");
    assert!(err.contains("cannot exceed"), "unexpected error: {err}");
}
```

- [ ] **Step 2: Run the failing tests**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire crm_tag_validator -- --nocapture
```

Expected: compile failure or test failure because `validate_crm_tag()` does not exist yet.

- [ ] **Step 3: Implement the validator**

Add this near `validate_name_len()` in `core/protocol/c2-wire/src/handshake.rs`:

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
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify the validator passes**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-wire crm_tag_validator -- --nocapture
```

Expected: all new `crm_tag_validator_*` tests pass.

- [ ] **Step 5: Commit**

```bash
git add core/protocol/c2-wire/src/handshake.rs core/protocol/c2-wire/src/tests.rs
git commit -m "feat: add canonical CrmTag validation"
```

## Task 2: Reject Invalid CrmTags At Server And Python Binding Ingress

**Files:**
- Modify: `core/transport/c2-server/src/server.rs`
- Modify: `sdk/python/native/src/server_ffi.rs`
- Test: `core/transport/c2-server/src/server.rs`
- Test: `sdk/python/tests/unit/test_crm_decorator.py`
- Modify: `sdk/python/src/c_two/crm/meta.py`

- [ ] **Step 1: Write failing native server test**

Add this test inside `core/transport/c2-server/src/server.rs` test module:

```rust
#[tokio::test]
async fn register_route_rejects_invalid_crm_tag_fields() {
    let s = Server::new(
        "ipc://invalid_crm_tag_route",
        ServerIpcConfig::default(),
        Some("invalid_crm_tag_route".to_string()),
    )
    .unwrap();
    let mut route = make_route("grid");
    route.crm_name = "Grid\0Injected".to_string();

    let err = s
        .register_route(route)
        .await
        .expect_err("invalid CrmTag must fail before route registration");
    assert!(
        err.to_string().contains("control characters"),
        "unexpected error: {err}",
    );
}
```

- [ ] **Step 2: Write failing Python decorator tests**

Add these tests to `sdk/python/tests/unit/test_crm_decorator.py` under `TestIcrmValidation`:

```python
    def test_namespace_rejects_control_characters(self):
        with pytest.raises(ValueError, match='control characters'):
            @cc.crm(namespace="bad\nnamespace", version="1.0.0")
            class Svc:
                pass

    def test_namespace_rejects_wrapped_whitespace(self):
        with pytest.raises(ValueError, match='leading or trailing whitespace'):
            @cc.crm(namespace=" ns", version="1.0.0")
            class Svc:
                pass
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server register_route_rejects_invalid_crm_tag_fields -- --nocapture
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_crm_decorator.py::TestIcrmValidation -q --timeout=30 -rs
```

Expected: Rust test fails because server route registration only checks length; Python tests fail because the decorator does not reject control characters or wrapped whitespace.

- [ ] **Step 4: Enforce CrmTag validation in `c2-server`**

In `core/transport/c2-server/src/server.rs`, update `validate_route_for_wire()`:

```rust
    c2_wire::handshake::validate_crm_tag(&route.crm_ns, &route.crm_name, &route.crm_ver)
        .map_err(ServerError::Protocol)?;
```

Keep the existing `validate_name_len()` checks for route and method names. Remove the three old CRM-only length checks so there is one CRM tag rule, not two parallel rules.

- [ ] **Step 5: Enforce CrmTag validation in PyO3 route construction**

In `sdk/python/native/src/server_ffi.rs`, add this before constructing `CrmRoute` in `PyServer::build_route()`:

```rust
        c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver)
            .map_err(PyValueError::new_err)?;
```

- [ ] **Step 6: Add Python-side fast feedback**

In `sdk/python/src/c_two/crm/meta.py`, add helper:

```python
def _validate_crm_tag_field(label: str, value: str) -> None:
    if not isinstance(value, str):
        raise ValueError(f'{label} must be a string.')
    if not value:
        raise ValueError(f'{label} of CRM cannot be empty.')
    if value.strip() != value:
        raise ValueError(f'{label} cannot contain leading or trailing whitespace.')
    if any(ch < ' ' or ch == '\x7f' for ch in value):
        raise ValueError(f'{label} cannot contain control characters.')
    if len(value.encode()) > 255:
        raise ValueError(f'{label} cannot exceed 255 bytes.')
```

Then replace the existing namespace/version emptiness checks in `crm_wrapper()` with:

```python
        _validate_crm_tag_field('Namespace', namespace)
        _validate_crm_tag_field('Version', version)
```

Keep the existing semantic version format check after this block.

- [ ] **Step 7: Run tests to verify this task passes**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-server register_route_rejects_invalid_crm_tag_fields -- --nocapture
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_crm_decorator.py::TestIcrmValidation -q --timeout=30 -rs
```

Expected: both commands pass.

- [ ] **Step 8: Commit**

```bash
git add core/transport/c2-server/src/server.rs sdk/python/native/src/server_ffi.rs sdk/python/src/c_two/crm/meta.py sdk/python/tests/unit/test_crm_decorator.py
git commit -m "fix: reject invalid CrmTags at server ingress"
```

## Task 3: Enforce CrmTag Validation On Relay Local Registration

**Files:**
- Modify: `core/transport/c2-http/src/relay/route_table.rs`
- Modify: `core/transport/c2-http/src/relay/authority.rs`
- Modify: `core/transport/c2-http/src/relay/router.rs`
- Test: `core/transport/c2-http/src/relay/router.rs`

- [ ] **Step 1: Write failing relay tests**

Add these tests to `core/transport/c2-http/src/relay/router.rs` test module:

```rust
#[tokio::test]
async fn skip_validation_register_rejects_invalid_claimed_crm_tag() {
    let state = Arc::new(RelayState::new(
        Arc::new(c2_config::RelayConfig {
            relay_id: "test-relay".into(),
            skip_ipc_validation: true,
            ..Default::default()
        }),
        Arc::new(crate::relay::test_support::NoopDisseminator),
    ));

    assert_eq!(
        post_register_with_crm(
            state.clone(),
            "grid",
            "server-grid",
            "ipc://grid",
            "test.mesh",
            "Grid\0Injected",
            "0.1.0",
        )
        .await,
        StatusCode::BAD_REQUEST,
    );
    assert!(state.resolve("grid").is_empty());
}

#[tokio::test]
async fn local_commit_rejects_invalid_crm_tag_even_when_called_directly() {
    let state = test_state();
    let stale_client = Arc::new(IpcClient::new("ipc://grid"));

    let result = state.commit_register_upstream(
        "grid".into(),
        "server-grid".into(),
        "server-grid-instance".into(),
        "ipc://grid".into(),
        "test.mesh".into(),
        "Grid\nInjected".into(),
        "0.1.0".into(),
        stale_client,
        None,
    );

    assert!(matches!(
        result,
        RegisterCommitResult::Invalid { .. }
    ));
    assert!(state.resolve("grid").is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay skip_validation_register_rejects_invalid_claimed_crm_tag -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay local_commit_rejects_invalid_crm_tag_even_when_called_directly -- --nocapture
```

Expected: first test returns `201` or commits invalid state; second test does not compile until `RegisterCommitResult::Invalid` is added.

- [ ] **Step 3: Route table delegates to canonical CrmTag validator**

In `core/transport/c2-http/src/relay/route_table.rs`, replace local CrmTag validation with:

```rust
pub(crate) fn valid_crm_tag(crm_ns: &str, crm_name: &str, crm_ver: &str) -> bool {
    c2_wire::handshake::validate_crm_tag(crm_ns, crm_name, crm_ver).is_ok()
}
```

Remove the now-duplicated `valid_crm_tag_field()` helper.

- [ ] **Step 4: Add explicit invalid register result**

In `core/transport/c2-http/src/relay/state.rs`, extend `RegisterCommitResult`:

```rust
    Invalid { reason: String },
```

Map `ControlError::ContractMismatch { reason }`, `InvalidName`, `InvalidServerId`, and `InvalidServerInstanceId` from `commit_register_upstream()` to this `Invalid` variant instead of fake duplicate/conflicting owner responses.

- [ ] **Step 5: Validate CrmTag in `register_local()`**

In `core/transport/c2-http/src/relay/authority.rs`, add this before acquiring the route-table write lock in `register_local()`:

```rust
        self.validate_crm_tag(&crm_ns, &crm_name, &crm_ver)?;
```

Also update `attest_ipc_route_contract()` after empty-field checks:

```rust
    if !valid_crm_tag(crm_ns, crm_name, crm_ver) {
        return Err(ControlError::ContractMismatch {
            reason: format!(
                "IPC upstream route '{route_name}' advertised an invalid CRM tag"
            ),
        });
    }
```

- [ ] **Step 6: Reject invalid claimed tags in HTTP `/_register`**

In `core/transport/c2-http/src/relay/router.rs`, validate body CrmTag whenever any field is supplied or `skip_ipc_validation` is enabled:

```rust
    let claimed_has_crm_tag = !crm_ns.is_empty() || !crm_name.is_empty() || !crm_ver.is_empty();
    if (claimed_has_crm_tag || state.config().skip_ipc_validation)
        && !valid_crm_tag(&crm_ns, &crm_name, &crm_ver)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "InvalidCrmTag",
                "message": "crm_ns, crm_name, and crm_ver must be non-empty, control-character-free, and fit the IPC handshake field limit",
            })),
        )
            .into_response();
    }
```

Keep this before `prepare_register()` so invalid input fails before duplicate-owner probing.

- [ ] **Step 7: Handle invalid commit result at HTTP and command-loop callers**

In `router.rs`, handle `RegisterCommitResult::Invalid { reason }`:

```rust
        RegisterCommitResult::Invalid { reason } => {
            close_arc_client(client);
            return (StatusCode::BAD_REQUEST, reason).into_response();
        }
```

In `core/transport/c2-http/src/relay/server.rs`, map `RegisterCommitResult::Invalid { reason }` to:

```rust
Err(RelayControlError::Other(reason))
```

- [ ] **Step 8: Run tests to verify relay registration passes**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay skip_validation_register_rejects_invalid_claimed_crm_tag -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay local_commit_rejects_invalid_crm_tag_even_when_called_directly -- --nocapture
```

Expected: both tests pass.

- [ ] **Step 9: Commit**

```bash
git add core/transport/c2-http/src/relay/route_table.rs core/transport/c2-http/src/relay/authority.rs core/transport/c2-http/src/relay/router.rs core/transport/c2-http/src/relay/state.rs core/transport/c2-http/src/relay/server.rs
git commit -m "fix: validate CrmTags before relay local registration"
```

## Task 4: Canonical relay_id Validation

**Files:**
- Modify: `core/foundation/c2-config/src/identity.rs`
- Modify: `core/foundation/c2-config/src/lib.rs`
- Modify: `core/foundation/c2-config/src/relay.rs`
- Modify: `core/transport/c2-http/src/relay/route_table.rs`
- Modify: `core/transport/c2-http/src/relay/peer_handlers.rs`
- Test: `core/foundation/c2-config/src/identity.rs`
- Test: `core/transport/c2-http/src/relay/peer_handlers.rs`
- Test: `core/transport/c2-http/src/relay/route_table.rs`

- [ ] **Step 1: Write failing config tests**

Add this to `core/foundation/c2-config/src/identity.rs` tests:

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

Add this to `core/foundation/c2-config/src/relay.rs` tests:

```rust
#[test]
fn relay_config_rejects_invalid_relay_id() {
    let mut config = RelayConfig::default();
    config.relay_id = "bad\nrelay".to_string();

    let err = config
        .validate()
        .expect_err("invalid relay_id should fail config validation");
    assert!(err.contains("relay_id"), "unexpected error: {err}");
}
```

- [ ] **Step 2: Write failing mesh ingestion tests**

Add this to `core/transport/c2-http/src/relay/peer_handlers.rs` tests:

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

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config relay_id -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay join_rejects_invalid_relay_id -- --nocapture
```

Expected: config tests fail because `validate_relay_id()` does not exist or `RelayConfig::validate()` does not call it; mesh test fails because join accepts the invalid id.

- [ ] **Step 4: Implement canonical relay-id validator**

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

In `core/foundation/c2-config/src/relay.rs`, call it from `RelayConfig::validate()`:

```rust
        crate::validate_relay_id(&self.relay_id)?;
```

- [ ] **Step 5: Delegate relay route-table validation to config**

In `core/transport/c2-http/src/relay/route_table.rs`, replace `valid_relay_id()` with:

```rust
fn valid_relay_id(relay_id: &str) -> bool {
    c2_config::validate_relay_id(relay_id).is_ok()
}
```

- [ ] **Step 6: Reject invalid peer message ids at handlers**

In `core/transport/c2-http/src/relay/peer_handlers.rs`, add helper:

```rust
fn valid_peer_relay_id(relay_id: &str) -> bool {
    c2_config::validate_relay_id(relay_id).is_ok()
}
```

Use it in `handle_peer_join()` before `record_peer_join()`:

```rust
        if !valid_peer_relay_id(&sender_relay_id) || !valid_peer_relay_id(&relay_id) {
            return (
                StatusCode::OK,
                Json(serde_json::json!({"status": "ignored"})),
            )
                .into_response();
        }
```

Use the same check in heartbeat, leave, and digest before state mutation:

```rust
        if !valid_peer_relay_id(&sender_relay_id) || !valid_peer_relay_id(&relay_id) {
            return StatusCode::OK;
        }
```

For digest messages, check `sender_relay_id` before `trusted_peer()`.

- [ ] **Step 7: Run tests to verify relay-id validation passes**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-config relay_id -- --nocapture
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay relay_id -- --nocapture
```

Expected: all relay-id focused tests pass.

- [ ] **Step 8: Commit**

```bash
git add core/foundation/c2-config/src/identity.rs core/foundation/c2-config/src/lib.rs core/foundation/c2-config/src/relay.rs core/transport/c2-http/src/relay/route_table.rs core/transport/c2-http/src/relay/peer_handlers.rs
git commit -m "fix: centralize relay id validation"
```

## Task 5: Boundary And Regression Coverage

**Files:**
- Test: `sdk/python/tests/unit/test_sdk_boundary.py`
- Test: `sdk/python/tests/integration/test_relay_mesh.py`
- Test: `core/transport/c2-http/src/relay/router.rs`

- [ ] **Step 1: Add source-boundary guard for duplicate CrmTag validators**

Add this test to `sdk/python/tests/unit/test_sdk_boundary.py`:

```python
def test_relay_does_not_keep_a_second_crm_tag_validator():
    root = Path(__file__).resolve().parents[4]
    route_table = root / "core" / "transport" / "c2-http" / "src" / "relay" / "route_table.rs"
    source = route_table.read_text(encoding="utf-8")

    assert "fn valid_crm_tag_field" not in source
    assert "c2_wire::handshake::validate_crm_tag" in source
```

- [ ] **Step 2: Add skip-validation mesh regression**

In `sdk/python/tests/integration/test_relay_mesh.py`, add:

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

- [ ] **Step 3: Run focused regression tests**

Run:

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/unit/test_sdk_boundary.py::test_relay_does_not_keep_a_second_crm_tag_validator -q --timeout=30 -rs
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/integration/test_relay_mesh.py::TestSingleRelay::test_skip_validation_register_rejects_incomplete_crm_tag -q --timeout=30 -rs
```

Expected: both tests pass.

- [ ] **Step 4: Commit**

```bash
git add sdk/python/tests/unit/test_sdk_boundary.py sdk/python/tests/integration/test_relay_mesh.py
git commit -m "test: guard identity validation boundaries"
```

## Task 6: Full Verification

**Files:**
- No production files.

- [ ] **Step 1: Format Rust code**

Run:

```bash
cargo fmt --manifest-path core/Cargo.toml
cargo fmt --manifest-path sdk/python/native/Cargo.toml
```

Expected: formatting completes without errors.

- [ ] **Step 2: Run Rust verification**

Run:

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

Run:

```bash
env UV_CACHE_DIR=.uv-cache C2_RELAY_ANCHOR_ADDRESS= uv run pytest sdk/python/tests/ -q --timeout=30 -rs
```

Expected: full Python suite passes with the existing skip count only.

- [ ] **Step 4: Check whitespace and accidental churn**

Run:

```bash
git diff --check
git status --short
```

Expected: `git diff --check` reports no whitespace errors. `git status --short` shows only intended modified files and commits already made for the plan tasks.

- [ ] **Step 5: Final commit if any verification-only edits were needed**

```bash
git add core sdk/python docs
git commit -m "chore: verify identity validation hardening"
```

Only run this commit if formatting or verification produced tracked edits that were not included in earlier task commits.

## Self-Review

- Spec coverage: The plan covers the two review findings: inconsistent CrmTag write validation and weak relay-id validation. It also includes SDK fast feedback and boundary regression tests so the behavior remains Rust-owned.
- Placeholder scan: No task uses TBD/TODO/fill-in wording; each change step names exact files and contains concrete code or commands.
- Type consistency: The plan keeps existing wire/JSON field names (`crm_ns`, `crm_name`, `crm_ver`) and adds canonical validator functions, avoiding a broad type migration in the same change. `RegisterCommitResult::Invalid { reason }` is introduced before tests depend on it.
- Scope control: This plan does not redesign route keys or relay authentication. It hardens identity validity before state mutation, which is the defect under review.
