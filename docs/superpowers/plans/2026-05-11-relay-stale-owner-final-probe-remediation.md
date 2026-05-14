# Relay Stale Owner Final-Probe Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Close the relay stale-owner half-fix by ensuring stale pre-attestation owner observations can never authorize a route replacement commit.

**Status:** Implemented in the current worktree. Keep this document as the
closed-loop remediation record and verification checklist, not as an open plan.

**Architecture:** Keep relay ownership authority in Rust `c2-http`. Split stale-owner replacement into candidate eligibility, candidate IPC attestation, final replacement proof, and commit. A pre-attestation probe may allow dialing a candidate, but only a post-attestation final probe may produce the opaque `OwnerReplacement` accepted by commit. This plan closes the current relay-local evidence-reuse race; it must not claim an absolute cross-process "owner cannot become live after final proof" guarantee unless a separate IPC owner-fence protocol is added.

**Tech Stack:** Rust `c2-http` relay modules, Tokio tests, IPC test support, existing `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay` verification.

---

## Current Diagnosis

The pre-fix code performed the old-owner probe before candidate IPC attestation:

- `core/transport/c2-http/src/relay/server.rs`: command-loop registration calls `prepare_candidate_registration(...)`, then connects the candidate IPC endpoint, then calls `commit_register_upstream(...)`.
- `core/transport/c2-http/src/relay/router.rs`: HTTP `/_register` calls `prepare_register(...)`, then connects and attests the candidate IPC endpoint, then calls `commit_register_upstream(...)`.
- `core/transport/c2-http/src/relay/authority.rs`: `prepare_candidate_registration(...)` and `prepare_register(...)` convert `OwnerProbe::Dead` / `OwnerProbe::RouteMissing` directly into `OwnerReplacementEvidence`.
- `core/transport/c2-http/src/relay/conn_pool.rs`: `replace_if_owner_token(...)` only checks relay-local slot generation, lease epoch, active request count, slot state, and the provided evidence. It does not probe the old IPC owner.

That allowed silent external recovery between the pre-attestation probe and
final commit if no relay-local lease, reconnect, acquire, or re-registration
changed the owner token. The implemented remediation removes that stale-evidence
path completely while documenting that relay-local final proof is not the same
thing as a cross-process owner lease/fence.

## Closed-Loop Invariants

1. **No stale preliminary observation may authorize commit.**
   A candidate may replace an existing owner only when a fresh final proof, collected after candidate IPC identity and route-contract attestation, proves the captured owner route is dead, route-missing, identity-mismatched, or contract-mismatched at final proof time. The plan must not claim stronger post-final-proof external liveness exclusion without an IPC owner-fence protocol.

2. **Pre-attestation evidence is not commit evidence.**
   The initial stale-owner probe only gates whether the relay may spend work dialing the candidate. It must not produce the `OwnerReplacementEvidence` consumed by commit.

3. **Both registration surfaces must share the same replacement proof.**
   HTTP `/_register` and command-loop `c3 relay --upstream` must both perform final proof after candidate IPC identity and route-contract attestation.

4. **Final proof must validate the captured owner route, not name alone.**
   A final probe must compare the old endpoint against captured `route_name`, `server_id`, `server_instance_id`, `ipc_address`, `crm_ns`, `crm_name`, `crm_ver`, `abi_hash`, and `signature_hash`.

5. **Commit remains a relay-local atomic fence.**
   `replace_if_owner_token(...)` must continue checking slot pointer, address, owner generation, owner lease epoch, active requests, replaceable state, and final evidence while the route table commit path rechecks captured route identity.

6. **Python SDK remains uninvolved.**
   This is Rust relay authority. Do not add Python retries, route caches, fallback calls, or SDK-side owner state.

7. **No public or crate-local evidence construction bypass remains.**
   Production registration paths must not be able to synthesize `OwnerReplacementEvidence` through an exported token struct, public fields, raw struct literals, test helpers, or default fallbacks. Final evidence must flow through one choke point: `RouteAuthority::confirm_replacement_for_commit(...)`.

## Source-to-Sink Design

| Stage | Producer | Consumer | Storage | Network/Wire | Guardrail |
| --- | --- | --- | --- | --- | --- |
| Captured owner snapshot | `RouteAuthority::different_owner_replacement(...)` | pre-probe, final-probe, commit | `OwnerReplacementCandidate` | none | Captures full route identity and `OwnerToken` |
| Candidate eligibility | `prepare_candidate_registration(...)` / `prepare_register(...)` | server/router candidate connect path | transient `OwnerReplacementCandidate` | IPC probe to old owner | Rejects live owner before dialing candidate |
| Candidate attestation | server/router registration path | final proof | candidate `IpcClient` plus contract fields | IPC handshake and route contract read | Rejects identity, route, or contract mismatch |
| Final replacement proof | `confirm_replacement_for_commit(...)` | `commit_register_upstream(...)` | opaque `OwnerReplacement` | fresh IPC probe to captured old endpoint | Only post-attestation proof carries evidence |
| Commit | `RelayState::commit_register_upstream(...)` | route table + connection pool | route table and connection slot | none | OwnerToken, route identity, active request, state checks |

## Selected Approach

Use a type split and sealed proof flow instead of a comment-only fix:

- `OwnerReplacementCandidate` means "the candidate may be dialed after a preliminary stale-owner probe."
- `OwnerReplacement` means "a final proof was collected after candidate attestation and may be passed to commit."
- `OwnerReplacementEvidence` must only be attached in the final confirmation function.
- `OwnerReplacementToken` must be removed. `RelayState::commit_register_upstream(...)` must accept `Option<OwnerReplacement>` directly and pass that opaque proof into `RouteAuthority::execute(...)`.
- `OwnerReplacementCandidate` and `OwnerReplacement` fields must be private. Expose only narrowly scoped accessors if a caller needs an error string; do not expose evidence-bearing fields to `server.rs`, `router.rs`, or tests.

This prevents the old half-fix pattern where a stale pre-attestation probe result can be reused as a commit proof.

## Files And Responsibilities

- Modify `core/transport/c2-http/src/relay/authority.rs`
  - Extend `OwnerReplacementCandidate` to capture the full old route contract.
  - Make `prepare_candidate_registration(...)` return `Result<Option<OwnerReplacementCandidate>, ControlError>`.
  - Make `RegisterPreparation::Available` carry `Option<OwnerReplacementCandidate>`.
  - Add `confirm_replacement_for_commit(...)`.
  - Replace name-only `probe_owner(...)` with a captured-owner probe that checks identity and contract.

- Modify `core/transport/c2-http/src/relay/server.rs`
  - Keep duplicate-live-owner rejection before candidate connect.
  - After candidate handshake and route contract attestation, call `confirm_replacement_for_commit(...)`.
  - Pass only the final proof to `commit_register_upstream(...)`.

- Modify `core/transport/c2-http/src/relay/router.rs`
  - Mirror the command-loop ordering.
  - Do not let HTTP registration pass preliminary candidates to commit.

- Modify `core/transport/c2-http/src/relay/state.rs`
  - Remove `OwnerReplacementToken`.
  - Change `commit_register_upstream(...)` to accept `Option<OwnerReplacement>` directly.
  - Do not expose any state-layer struct that can carry caller-supplied `OwnerReplacementEvidence`.

- Modify `core/transport/c2-http/src/relay/conn_pool.rs`
  - Keep owner-token checks unchanged in this implementation.
  - Do not move probe logic into the connection pool; the pool remains slot/freshness authority only.

- Modify `docs/issues/relay-stale-owner-revival-race.md`
  - During implementation, mark the issue as open / partially mitigated before code lands.
  - After code and tests land, update the resolution section with actual final-probe evidence.

- Modify `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`
  - Replace the addendum's stale "remaining relay robustness gap" wording after
    implementation verification.
  - Record exact verification evidence and avoid claiming SDK ownership changes.
  - Keep the cross-process owner-fence boundary explicit: this fix closes stale evidence reuse, not an absolute "owner cannot become live after final proof" guarantee.

## Implementation Tasks

### Task 1: Add Failing Final-Probe Regression Tests

**Files:**
- Modify: `core/transport/c2-http/src/relay/router.rs`

- [x] **Step 1: Add a final-confirmation test for silent recovery**

Add an async Rust test that builds this sequence:

```rust
#[tokio::test]
async fn final_replacement_confirmation_rejects_silent_recovered_owner() {
    let state = test_state();
    let old_address = unique_ipc_address("final_probe_old");
    let candidate_address = unique_ipc_address("final_probe_candidate");

    let old_server = start_live_server(&old_address, "server-old").await;
    assert_eq!(
        post_register(state.clone(), "grid", "server-old", &old_address).await,
        StatusCode::CREATED
    );

    state.evict_connection("grid");
    old_server.shutdown();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let preliminary = RouteAuthority::new(&state)
        .prepare_register("grid", "server-new", "server-new-instance", &candidate_address)
        .await
        .expect("preliminary replacement should be allowed");

    let replacement = match preliminary {
        RegisterPreparation::Available { replacement: Some(replacement) } => replacement,
        _ => panic!("expected replacement candidate"),
    };

    let recovered_old = start_live_server(&old_address, "server-old").await;
    let confirmed = RouteAuthority::new(&state)
        .confirm_replacement_for_commit(Some(replacement))
        .await;

    assert!(matches!(
        confirmed,
        Err(ControlError::DuplicateRoute { existing_address })
            if existing_address == old_address
    ));

    recovered_old.shutdown();
}
```

Place this test inside the existing `router.rs` test module so it can reuse `test_state()`, `unique_ipc_address(...)`, `start_live_server(...)`, and `post_register(...)`.

- [x] **Step 2: Add still-dead final confirmation test**

Add a paired test proving that final confirmation still allows replacement when the old owner remains dead:

```rust
#[tokio::test]
async fn final_replacement_confirmation_allows_still_dead_owner() {
    let state = test_state();
    let old_address = unique_ipc_address("final_probe_dead_old");
    let candidate_address = unique_ipc_address("final_probe_dead_candidate");

    let old_server = start_live_server(&old_address, "server-old").await;
    assert_eq!(
        post_register(state.clone(), "grid", "server-old", &old_address).await,
        StatusCode::CREATED
    );

    state.evict_connection("grid");
    old_server.shutdown();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let preliminary = RouteAuthority::new(&state)
        .prepare_register("grid", "server-new", "server-new-instance", &candidate_address)
        .await
        .expect("preliminary replacement should be allowed");
    let replacement = match preliminary {
        RegisterPreparation::Available { replacement: Some(replacement) } => replacement,
        _ => panic!("expected replacement candidate"),
    };

    let confirmed = RouteAuthority::new(&state)
        .confirm_replacement_for_commit(Some(replacement))
        .await
        .expect("dead owner should confirm replacement");

    assert!(confirmed.is_some());
}
```

- [x] **Step 3: Add command-loop preliminary API coverage**

Add a test that uses `prepare_candidate_registration(...)`, the preliminary API used by the command-loop `RegisterUpstream` path:

```rust
#[tokio::test]
async fn final_replacement_confirmation_rejects_silent_recovered_owner_from_command_preparation() {
    let state = test_state();
    let old_address = unique_ipc_address("final_probe_command_old");
    let candidate_address = unique_ipc_address("final_probe_command_candidate");

    let old_server = start_live_server(&old_address, "server-old").await;
    assert_eq!(
        post_register(state.clone(), "grid", "server-old", &old_address).await,
        StatusCode::CREATED
    );

    state.evict_connection("grid");
    old_server.shutdown();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let replacement = RouteAuthority::new(&state)
        .prepare_candidate_registration("grid", "server-new", &candidate_address)
        .await
        .expect("preliminary command-loop replacement should be allowed")
        .expect("expected replacement candidate");

    let recovered_old = start_live_server(&old_address, "server-old").await;
    let confirmed = RouteAuthority::new(&state)
        .confirm_replacement_for_commit(Some(replacement))
        .await;

    assert!(matches!(
        confirmed,
        Err(ControlError::DuplicateRoute { existing_address })
            if existing_address == old_address
    ));

    recovered_old.shutdown();
}
```

- [x] **Step 4: Run tests and record failure**

Run:

```bash
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay final_replacement_confirmation
```

Expected before implementation: compile failure because `confirm_replacement_for_commit(...)` does not exist, or behavioral failure if a temporary stub incorrectly accepts silent recovery.

### Task 2: Split Candidate Eligibility From Final Proof

**Files:**
- Modify: `core/transport/c2-http/src/relay/authority.rs`

- [x] **Step 1: Extend the captured replacement candidate**

Change `OwnerReplacementCandidate` to capture full route identity with private fields:

```rust
#[derive(Clone, Debug)]
pub(crate) struct OwnerReplacementCandidate {
    route_name: String,
    server_id: String,
    server_instance_id: String,
    ipc_address: String,
    existing_address: String,
    crm_ns: String,
    crm_name: String,
    crm_ver: String,
    abi_hash: String,
    signature_hash: String,
    token: OwnerToken,
}
```

Populate the new fields from `existing: &RouteEntry` inside `different_owner_replacement(...)`. Do not add a public constructor. The only producer remains `RouteAuthority::different_owner_replacement(...)`.

Also change `OwnerReplacement` so all fields are private:

```rust
#[derive(Clone, Debug)]
pub(crate) struct OwnerReplacement {
    route_name: String,
    server_id: String,
    server_instance_id: String,
    ipc_address: String,
    existing_address: String,
    token: OwnerToken,
    evidence: OwnerReplacementEvidence,
}
```

Keep `OwnerReplacementCandidate::with_evidence(...)` private to `authority.rs`; callers outside `authority.rs` must not be able to attach evidence.

- [x] **Step 2: Change preliminary APIs to return candidates only**

Change:

```rust
pub(crate) async fn prepare_candidate_registration(
    &self,
    name: &str,
    server_id: &str,
    address: &str,
) -> Result<Option<OwnerReplacementCandidate>, ControlError>
```

and:

```rust
pub(crate) enum RegisterPreparation {
    Available {
        replacement: Option<OwnerReplacementCandidate>,
    },
    SameOwner,
    DuplicateAlive {
        existing_address: String,
    },
}
```

In `prepare_candidate_registration(...)` and `prepare_register(...)`, keep the pre-connect probe, but return `Some(replacement)` instead of `replacement.with_evidence(...)` for `Dead` or `RouteMissing`.

- [x] **Step 3: Add final confirmation API**

Add:

```rust
pub(crate) async fn confirm_replacement_for_commit(
    &self,
    replacement: Option<OwnerReplacementCandidate>,
) -> Result<Option<OwnerReplacement>, ControlError> {
    let Some(replacement) = replacement else {
        return Ok(None);
    };

    match self.probe_captured_owner(&replacement).await {
        OwnerProbe::Alive => Err(ControlError::DuplicateRoute {
            existing_address: replacement.existing_address,
        }),
        OwnerProbe::RouteMissing => Ok(Some(
            replacement.with_evidence(OwnerReplacementEvidence::ConfirmedRouteMissing),
        )),
        OwnerProbe::Dead => Ok(Some(
            replacement.with_evidence(OwnerReplacementEvidence::ConfirmedDead),
        )),
        OwnerProbe::Stale => Err(ControlError::DuplicateRoute {
            existing_address: replacement.existing_address,
        }),
    }
}
```

- [x] **Step 4: Replace name-only owner probe with captured route probe**

Add:

```rust
async fn probe_captured_owner(&self, replacement: &OwnerReplacementCandidate) -> OwnerProbe {
    let mut client = IpcClient::new(&replacement.existing_address);
    match client.connect().await {
        Ok(()) => {
            let identity_matches = client.server_id() == Some(replacement.server_id.as_str())
                && client.server_instance_id() == Some(replacement.server_instance_id.as_str());

            let route_matches = identity_matches
                && client
                    .route_contract(&replacement.route_name)
                    .is_some_and(|contract| {
                        contract.route_name == replacement.route_name
                            && contract.crm_ns == replacement.crm_ns
                            && contract.crm_name == replacement.crm_name
                            && contract.crm_ver == replacement.crm_ver
                            && contract.abi_hash == replacement.abi_hash
                            && contract.signature_hash == replacement.signature_hash
                    });

            client.close().await;

            if !route_matches {
                OwnerProbe::RouteMissing
            } else if self.state.matches_owner_token(&replacement.route_name, &replacement.token) {
                OwnerProbe::Alive
            } else {
                OwnerProbe::Stale
            }
        }
        Err(_) => OwnerProbe::Dead,
    }
}
```

Remove `probe_owner(...)` after migrating both preliminary functions to `probe_captured_owner(...)`; there should be exactly one owner-probe implementation after this task.

- [x] **Step 5: Remove the state-layer replacement token bypass**

Delete `OwnerReplacementToken` from `core/transport/c2-http/src/relay/state.rs`:

```rust
pub struct OwnerReplacementToken {
    pub route_name: String,
    pub server_id: String,
    pub server_instance_id: String,
    pub ipc_address: String,
    pub existing_address: String,
    pub token: OwnerToken,
    pub evidence: OwnerReplacementEvidence,
}
```

Change `RelayState::commit_register_upstream(...)` from:

```rust
replacement: Option<OwnerReplacementToken>,
```

to:

```rust
replacement: Option<OwnerReplacement>,
```

and remove the conversion block that reconstructs `OwnerReplacement` from caller-provided fields. `RelayState` must pass the opaque `replacement` directly into `RouteCommand::RegisterLocal { ... }`.

Delete the `replacement_token(...) -> OwnerReplacementToken` test helper in `state.rs`. For tests that need to validate route-table commit behavior with a replacement proof, generate the proof through the real path:

1. register an old local route;
2. evict or disconnect the old connection;
3. call `RouteAuthority::prepare_register(...)` or `prepare_candidate_registration(...)`;
4. call `RouteAuthority::confirm_replacement_for_commit(...)`;
5. pass the returned opaque `OwnerReplacement` to `commit_register_upstream(...)`.

For tests that only verify slot-level stale token, active request, or replaceable-state checks, keep them in `conn_pool.rs` against `replace_if_owner_token(...)`, where `OwnerReplacementEvidence` is still the low-level input under test.

### Task 3: Wire Final Confirmation Into Both Registration Surfaces

**Files:**
- Modify: `core/transport/c2-http/src/relay/server.rs`
- Modify: `core/transport/c2-http/src/relay/router.rs`

- [x] **Step 1: Update command-loop registration**

In `server.rs`, keep preliminary `prepare_candidate_registration(...)` before candidate connect. Store its `Option<OwnerReplacementCandidate>` as `replacement_candidate`.

After candidate IPC identity, `server_instance_id`, route existence, and route contract attestation pass, wrap the candidate client in `Arc<IpcClient>` and then call:

```rust
let client = Arc::new(client);
let replacement = match RouteAuthority::new(&state)
    .confirm_replacement_for_commit(replacement_candidate)
    .await
{
    Ok(replacement) => replacement,
    Err(ControlError::DuplicateRoute { .. })
    | Err(ControlError::AddressMismatch { .. })
    | Err(ControlError::OwnerMismatch)
    | Err(ControlError::NotFound) => {
        tokio::spawn(async move { client.close_shared().await });
        let _ = reply.send(Err(RelayControlError::DuplicateRoute { name }));
        continue;
    }
    Err(ControlError::InvalidName { reason })
    | Err(ControlError::InvalidServerId { reason })
    | Err(ControlError::InvalidServerInstanceId { reason })
    | Err(ControlError::InvalidAddress { reason })
    | Err(ControlError::ContractMismatch { reason }) => {
        tokio::spawn(async move { client.close_shared().await });
        let _ = reply.send(Err(RelayControlError::Other(reason)));
        continue;
    }
};
```

Then pass `replacement` directly to `state.commit_register_upstream(...)`. Do not map it through any state-layer token or struct literal.

- [x] **Step 2: Update HTTP registration**

In `router.rs`, keep `prepare_register(...)` before candidate connect. Store `replacement_candidate`.

After candidate IPC identity and route contract attestation pass, call `confirm_replacement_for_commit(...)`. Map duplicate/stale final proof to `duplicate_route_response(...)`, close the candidate client, and do not call commit. Pass the returned `Option<OwnerReplacement>` directly into `state.commit_register_upstream(...)`.

- [x] **Step 3: Preserve same-owner behavior**

If `prepare_register(...)` returns `SameOwner`, skip final confirmation and pass `None` to commit. Existing same-owner contract mismatch checks in `register_local(...)` remain authoritative.

### Task 4: Add Guardrails Against Reusing Preliminary Evidence

**Files:**
- Modify: `core/transport/c2-http/src/relay/authority.rs`
- Modify: `core/transport/c2-http/src/relay/server.rs`
- Modify: `core/transport/c2-http/src/relay/router.rs`
- Modify: `core/transport/c2-http/src/relay/state.rs`

- [x] **Step 1: Add source guard for evidence creation**

Add a Rust unit test near relay authority tests:

```rust
#[test]
fn preliminary_registration_paths_do_not_attach_replacement_evidence() {
    let source = include_str!("authority.rs");
    let prepare_candidate = source_between(
        source,
        "pub(crate) async fn prepare_candidate_registration",
        "pub(crate) async fn prepare_register",
    )
        .expect("prepare_candidate_registration body should be found");
    let prepare_register = source_between(
        source,
        "pub(crate) async fn prepare_register",
        "pub(crate) async fn confirm_replacement_for_commit",
    )
    .expect("prepare_register body should be found");

    assert!(
        !prepare_candidate.contains(".with_evidence("),
        "preliminary command-loop preparation must not create commit evidence"
    );
    assert!(
        !prepare_candidate.contains("OwnerReplacementEvidence::"),
        "preliminary command-loop preparation must not name commit evidence"
    );
    assert!(
        !prepare_register.contains(".with_evidence("),
        "preliminary HTTP preparation must not create commit evidence"
    );
    assert!(
        !prepare_register.contains("OwnerReplacementEvidence::"),
        "preliminary HTTP preparation must not name commit evidence"
    );
}
```

Add this test-only helper in the same test module:

```rust
fn source_between<'a>(source: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = source.find(start)?;
    let after_start = &source[start_idx..];
    let end_idx = after_start.find(end)?;
    Some(&after_start[..end_idx])
}
```

- [x] **Step 2: Add source guard for both registration surfaces**

Add tests that assert both `server.rs` and `router.rs` contain `confirm_replacement_for_commit` before `commit_register_upstream`. Use source order checks:

```rust
let source = source_between(
    include_str!("server.rs"),
    "Command::RegisterUpstream {",
    "Command::UnregisterUpstream {",
)
.expect("command-loop register branch should be found");
let confirm = source.find("confirm_replacement_for_commit").expect("final confirm is required");
let commit = source.find("commit_register_upstream").expect("commit is required");
assert!(confirm < commit, "command loop must final-probe before commit");
```

Repeat for `router.rs` with:

```rust
let source = source_between(
    include_str!("router.rs"),
    "async fn handle_register",
    "fn close_arc_client",
)
.expect("HTTP register handler should be found");
let confirm = source.find("confirm_replacement_for_commit").expect("final confirm is required");
let commit = source.find("commit_register_upstream").expect("commit is required");
assert!(confirm < commit, "HTTP register must final-probe before commit");
```

- [x] **Step 3: Add source guard against state-layer replacement-token bypass**

Add a test that fails if `state.rs` reintroduces a caller-constructible evidence carrier:

```rust
#[test]
fn state_layer_does_not_export_replacement_evidence_token() {
    let source = include_str!("state.rs");
    assert!(
        !source.contains("OwnerReplacementToken"),
        "state.rs must not expose a replacement token that callers can fill with evidence"
    );
    assert!(
        !source.contains("replacement: Option<OwnerReplacementToken>"),
        "commit_register_upstream must consume opaque OwnerReplacement directly"
    );
}
```

- [x] **Step 4: Add source guard against public proof fields**

Add an authority source guard that checks the proof structs do not expose fields:

```rust
#[test]
fn replacement_proof_types_do_not_expose_fields() {
    let source = include_str!("authority.rs");
    let owner_replacement = source_between(
        source,
        "pub(crate) struct OwnerReplacement {",
        "pub(crate) struct OwnerReplacementCandidate {",
    )
    .expect("OwnerReplacement definition should be found");
    assert!(
        !owner_replacement
            .lines()
            .skip(1)
            .any(|line| line.trim_start().starts_with("pub ")),
        "OwnerReplacement fields must stay private so evidence cannot be constructed outside authority"
    );
    let candidate = source_between(
        source,
        "pub(crate) struct OwnerReplacementCandidate {",
        "impl OwnerReplacementCandidate",
    )
    .expect("OwnerReplacementCandidate definition should be found");
    assert!(
        !candidate
            .lines()
            .skip(1)
            .any(|line| line.trim_start().starts_with("pub ")),
        "OwnerReplacementCandidate fields must stay private so callers cannot forge captured owner state"
    );
}
```

- [x] **Step 5: Add source guard against state-layer proof reconstruction**

Add a guard that prevents `commit_register_upstream(...)` from rebuilding proof structs from caller-controlled fields:

```rust
#[test]
fn state_commit_does_not_reconstruct_replacement_proof() {
    let source = include_str!("state.rs");
    let body = source_between(
        source,
        "pub fn commit_register_upstream(",
        "pub fn unregister_upstream(",
    )
    .expect("commit_register_upstream body should be found");
    assert!(
        !body.contains("OwnerReplacement {"),
        "state commit must not reconstruct OwnerReplacement from caller-provided fields"
    );
    assert!(
        !body.contains("evidence:"),
        "state commit must not copy caller-provided evidence into replacement proof"
    );
}
```

### Task 5: Update Documentation To Match The Actual State

**Files:**
- Modify: `docs/issues/relay-stale-owner-revival-race.md`
- Modify: `docs/plans/2026-05-04-thin-sdk-rust-core-boundary.md`

- [x] **Step 1: Before implementation lands**

Mark `docs/issues/relay-stale-owner-revival-race.md` as open / partially mitigated if the code has not landed yet. Replace "resolved" claims with:

```markdown
The relay currently fences relay-local state changes with owner tokens,
generation, lease epoch, and active-request checks. It does not yet perform the
fresh final owner probe required to prevent stale pre-attestation evidence from
authorizing route replacement commit.
```

- [x] **Step 2: After implementation and verification**

Update both docs with exact test evidence:

```markdown
Verified by:

- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay final_replacement_confirmation`
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay`
```

Do not claim SDK boundary changes. The fix remains Rust relay authority.

Use this exact closure wording:

```markdown
This closes the stale-evidence relay replacement path: preliminary owner probes
may only decide whether to dial a candidate, and final replacement proof is
collected after candidate IPC identity and route-contract attestation. This is a
relay-local proof boundary. It does not claim a cross-process guarantee that an
external owner cannot become live after final proof without a future IPC
owner-fence protocol.
```

## Required Verification

Run:

```bash
cargo fmt --manifest-path core/Cargo.toml --all --check
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay final_confirmation_stays_after_candidate_attestation
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay final_replacement_confirmation
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay preliminary_registration_paths_do_not_attach_replacement_evidence
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay state_layer_does_not_export_replacement_evidence_token
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay state_commit_does_not_reconstruct_replacement_proof
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay command_register
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay register_
cargo test --manifest-path core/Cargo.toml -p c2-http --features relay
# Audit command: expected no output and exit code 1.
rg -n "OwnerReplacementToken|replacement: Option<OwnerReplacementToken>" core/transport/c2-http/src/relay
git diff --check
```

Expected final result:

- formatting passes;
- source-order tests prove both registration surfaces create final proof after
  candidate route attestation and before commit;
- final confirmation tests pass;
- command-loop registration tests pass;
- HTTP registration tests pass;
- full `c2-http` relay-enabled suite passes;
- source guard tests prove no `.with_evidence(` / `OwnerReplacementEvidence::` appears in preliminary registration function bodies;
- audit `rg` exits with code 1 and no output, proving there is no `OwnerReplacementToken` and no `replacement: Option<OwnerReplacementToken>`;
- no whitespace errors.

Fresh verification evidence from this implementation pass:

- `cargo fmt --manifest-path core/Cargo.toml --all --check`: exit 0.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay final_confirmation_stays_after_candidate_attestation -q`: 2 passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay final_replacement_confirmation -q`: 3 passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay preliminary_registration_paths_do_not_attach_replacement_evidence -q`: 1 passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay state_layer_does_not_export_replacement_evidence_token -q`: 1 passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay state_commit_does_not_reconstruct_replacement_proof -q`: 1 passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay command_register -q`: 8 passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay register_ -q`: 48 passed.
- `cargo test --manifest-path core/Cargo.toml -p c2-http --features relay -q`: 247 passed.
- `rg -n "OwnerReplacementToken|replacement: Option<OwnerReplacementToken>|probe_owner|replacement_token" core/transport/c2-http/src/relay`: exit 1 with no output.
- `git diff --check`: exit 0.

## Closed-Loop Design Review

### Findings

Critical: no plan-level critical gap remains inside the scoped relay-local objective after the design removes `OwnerReplacementToken`, private-seals proof fields, and makes final confirmation the only evidence construction path.

High: preliminary evidence reuse would leave the original race open.
Evidence: the pre-fix code created `OwnerReplacementEvidence` in
`prepare_candidate_registration(...)` and `prepare_register(...)`, before
candidate attestation. The implementation removes this by changing both APIs to
return `OwnerReplacementCandidate` and only creating `OwnerReplacement` inside
`confirm_replacement_for_commit(...)`.

High: fixing only one registration surface would leave a bypass.
Evidence: both command-loop registration and HTTP `/_register` call `commit_register_upstream(...)`. The design requires both `server.rs` and `router.rs` to call `confirm_replacement_for_commit(...)` after candidate attestation and before commit, with source-order guard tests.

Medium: final probe by route name alone would be a partial fix.
Evidence: route names are no longer sufficient authority in the current relay design. The design extends `OwnerReplacementCandidate` with full captured route identity and requires final probe to compare server identity plus CRM tag and contract hashes.

Medium: relying only on relay-local generation/lease checks would not cover silent external recovery.
Evidence: `replace_if_owner_token(...)` can only detect relay-local state
changes. The implementation keeps those checks but adds the missing network
observation immediately before commit.

Medium: an exported evidence-bearing token would preserve an old bypass even after adding final confirmation.
Evidence: the pre-fix `OwnerReplacementToken` exposed public fields in
`state.rs`. The implementation deletes that token and makes commit consume
opaque `OwnerReplacement` directly.

Low: documentation must not overstate relay-local final proof as cross-process fencing.
Evidence: the issue and boundary docs now describe the implemented final owner
proof as relay-local and explicitly reserve any stronger external owner-fence
guarantee for future IPC protocol work.

### Matrix Coverage

| Invariant | Code change | Test/guard |
| --- | --- | --- |
| No stale preliminary observation authorizes commit | `confirm_replacement_for_commit(...)` final probe | `final_replacement_confirmation_rejects_silent_recovered_owner` |
| Pre-probe evidence cannot reach commit | Return `OwnerReplacementCandidate` from preliminary APIs | source guard: no `.with_evidence(` in preliminary functions |
| HTTP and command-loop both covered | Update `router.rs` and `server.rs` | HTTP and command-preparation final-confirmation tests plus source-order guards for both files |
| Full route identity checked | Extend candidate with CRM and hash fields | final probe compares identity + contract |
| Relay-local atomic fence retained | Leave `replace_if_owner_token(...)` checks intact | existing owner-token replacement tests plus full suite |
| Old evidence-token bypass removed | Delete `OwnerReplacementToken`; make proof fields private; prevent state proof reconstruction | source guards: no `OwnerReplacementToken`, no public proof fields, no state proof reconstruction |
| Docs match code | Update issue and boundary docs | `git diff --check`, review evidence section |

### Residual Risks

- The final probe adds one extra IPC connection attempt only on stale-owner replacement paths, not normal registration or normal calls. This is acceptable because replacement is rare and correctness-sensitive.
- A recovered owner that becomes live after final proof but before commit is outside this relay-local proof boundary. Do not describe this plan as an absolute external owner-fencing fix. If product requirements demand that stronger guarantee, the next plan must add an IPC owner-fence protocol before any "fully resolved" wording is allowed.
- If final probe observes a different server at the old address, the design treats the captured owner route as missing. This avoids falsely preserving a stale route to an owner that no longer matches the captured identity.

### Review Verdict

This revised design is closed for the current Rust relay authority boundary: preliminary stale-owner observation no longer authorizes commit, both registration entry points are covered, state-layer evidence-token construction is removed, and commit proof is tied to a fresh network observation plus the existing relay-local atomic fence. It is not an IPC owner-fence design and must not be documented as one.
