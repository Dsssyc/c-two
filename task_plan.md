# Relay Control-Plane Repair Plan

## Goal

Repair the remaining Issue 1/Issue 2 correctness and safety gaps in `dev-feature` without adding compatibility debt or zombie fallback behavior.

## Current Worktree Guard

- Expected cwd: `/Users/soku/Desktop/codespace/WorldInProgress/c-two`
- Expected branch: `dev-feature`
- Dirty tree is intentional; do not reset or revert unrelated edits.

## Phases

1. [complete] Dead peer route resurrection
2. [complete] Idempotent/retry-safe unregister
3. [complete] `merge_snapshot` route/tombstone consistency
4. [complete] `merge_snapshot` transactionality
5. [complete] IPC region/address path validation
6. [complete] Final format and relevant test sweep

## Review Rule

For each phase: add a failing regression test, verify red, implement the smallest correct fix, verify green, then do a linkage review across adjacent call paths before moving on.
