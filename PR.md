## Summary

Decouples the audit trail from the rollback system so that every supervised session persists audit metadata by default, regardless of whether `--rollback` is set or writable tracked paths exist.

Previously, `session.json` was only written inside the `if let Some(...) = rollback_state` block, meaning:
- Sessions without `--rollback` produced no audit trail
- Sessions with only read-only paths (e.g. `--rollback --allow-cwd`) produced no audit trail

Now audit and rollback are orthogonal: audit is on by default (`--no-audit` opts out), rollback remains opt-in (`--rollback`).

Closes #269
Fixes #268

## Changes

- **Extract `ensure_session_dir` helper** — shared by both audit and rollback for session directory creation. When both are active, audit creates the dir and rollback shares it. When only rollback is active (`--rollback --no-audit`), rollback creates its own.
- **Simplify `create_audit_state`** — no longer checks `rollback_requested` or `rollback_disabled`, gates only on `audit_disabled`.
- **Convert `RollbackRuntimeState` to named struct** — rollback now owns its own `session_id` instead of deriving it from `audit_state.map(|s| s.session_id).unwrap_or_default()` (which produced an empty string when audit was off).
- **Add audit-only persistence path in `finalize_supervised_exit`** — when rollback is inactive, audit writes minimal `session.json` with command, timestamps, exit code, and network events.
- **Remove `conflicts_with = "rollback"` from `--no-audit`** — `--rollback --no-audit` is now a valid combination (rollback snapshots without audit metadata).

## Behavior matrix

| Flags | audit_state | rollback_state | Result |
|-------|------------|----------------|--------|
| *(default)* | Some | None | Audit-only `session.json` (new default) |
| `--rollback` | Some | Some (shared dir) | Full audit + rollback snapshots |
| `--no-audit` | None | None | Nothing persisted |
| `--rollback --no-audit` | None | Some (own dir) | Rollback snapshots only (new) |
| `--rollback --allow-cwd` (read-only) | Some | None (no writable paths) | Audit-only `session.json` (was broken) |

## Test plan

- [x] `make check` (clippy + fmt) passes
- [x] `make test` passes
- [x] New unit tests for `create_audit_state` and `ensure_session_dir` (gating logic, directory creation, session ID format, 0700 permissions on unix)
- [x] Manual validation of all 4 flag combinations on Ubuntu with Landlock V6

## Design decisions open for discussion

- **Directory naming**: Audit-only sessions currently live in `~/.nono/rollbacks/`. Should this be renamed to `~/.nono/sessions/` now that it holds more than rollback data? (Breaking change — probably a follow-up.)
- **Audit-only cleanup**: `enforce_rollback_limits` only runs when `--rollback` is set. Audit-only sessions are ~1KB each so storage is negligible, but they accumulate without cleanup. Should there be a lightweight age-based pruning mechanism?
- **`nono shell` audit**: Should `nono shell` also get audit by default, or is this scoped to `nono run` only?
