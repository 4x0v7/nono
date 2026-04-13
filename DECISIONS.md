# Issue #269: Decouple Audit from Rollback — Decision Log

## Goal
Make audit and rollback orthogonal features. Audit becomes default for all
supervised sessions; `--no-audit` opts out. Rollback remains opt-in via
`--rollback`.

## Decisions

### D1: Audit-only sessions live in `~/.nono/rollbacks/`
**Decision**: Keep the existing directory for now.
**Rationale**: Renaming to `~/.nono/sessions/` is a breaking change. Better to
ship the functional fix first, discuss naming with maintainer afterward.
**Status**: Provisional — open for feedback in PR.

### D2: Audit-only sessions count toward existing rollback limits
**Decision**: Audit-only sessions don't trigger `enforce_rollback_limits` — that
only runs when `--rollback` is set. Audit-only sessions are ~1KB so storage is
negligible, but they do accumulate in the same directory.
**Rationale**: Running the full limit enforcement (session count + disk scan) for
~1KB audit sessions would add I/O overhead for minimal benefit.
**Status**: Provisional — should audit-only sessions have their own lighter
cleanup mechanism, or is the current approach fine?

### D3: `nono rollback list` already filters out audit-only sessions
**Verified**: `rollback_commands.rs` filters on change count > 0 by default, so
audit-only sessions (which have no file changes) don't appear in `nono rollback list`.
No code changes needed.

### D4: Extracted `ensure_session_dir` helper
**Decision**: Shared helper for session dir creation used by both audit and rollback.
**Rationale**: Both audit-only and rollback-only cases need a session directory.
When both are active, audit creates the dir first and rollback shares it.
When only rollback is active (`--rollback --no-audit`), rollback creates its own.

### D5: Converted `RollbackRuntimeState` from tuple to struct
**Decision**: Added `session_id` field so rollback owns its identity independently
of audit state.
**Rationale**: The rollback path in `finalize_supervised_exit` previously used
`audit_state.map(|s| s.session_id).unwrap_or_default()` — empty string fallback
for a session_id is bad. Now rollback always has its own session_id.

### D6: `enforce_rollback_limits` only runs when rollback is requested
**Decision**: Keep the existing behavior — limits are enforced in
`initialize_rollback_state` which only runs when `--rollback` is set.
**Rationale**: Audit-only sessions are tiny (~1KB). Running the full limit
enforcement for audit-only sessions would add I/O overhead for minimal benefit.

---

## Open Questions (for PR discussion)
- **Directory naming**: Should `~/.nono/rollbacks/` be renamed to
  `~/.nono/sessions/` now that it holds audit-only sessions too? (Breaking change
  — probably a follow-up.)
- **Audit-only cleanup**: Should there be a lightweight cleanup for audit-only
  sessions (e.g. age-based pruning), or is unbounded accumulation acceptable
  given their ~1KB size?
- **`nono shell` audit**: Should `nono shell` also get audit by default, or is
  this scoped to `nono run` only?
