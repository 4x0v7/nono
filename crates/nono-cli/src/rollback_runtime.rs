use crate::launch_runtime::{rollback_base_exclusions, RollbackLaunchOptions};
use crate::{config, output, rollback_preflight, rollback_session, rollback_ui};
use nono::{AccessMode, CapabilitySet, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::warn;

pub(crate) struct AuditState {
    pub(crate) session_id: String,
    pub(crate) session_dir: PathBuf,
    /// Paths the sandbox had user-granted access to. Used to populate
    /// `SessionMetadata.tracked_paths` for audit-only sessions so that
    /// `nono audit list` can group and filter them by project.
    pub(crate) tracked_paths: Vec<PathBuf>,
}

pub(crate) struct RollbackRuntimeState {
    pub(crate) manager: nono::undo::SnapshotManager,
    pub(crate) baseline: nono::undo::SnapshotManifest,
    pub(crate) tracked_paths: Vec<PathBuf>,
    pub(crate) atomic_temp_before: HashSet<PathBuf>,
    pub(crate) session_id: String,
    /// Rollback session directory (under `~/.nono/rollbacks/` or a custom
    /// destination). Shared with audit metadata when both are active.
    pub(crate) session_dir: PathBuf,
}

pub(crate) struct RollbackExitContext<'a> {
    pub(crate) audit_state: Option<&'a AuditState>,
    pub(crate) rollback_state: Option<RollbackRuntimeState>,
    pub(crate) proxy_handle: Option<&'a nono_proxy::server::ProxyHandle>,
    pub(crate) started: &'a str,
    pub(crate) ended: &'a str,
    pub(crate) command: &'a [String],
    pub(crate) exit_code: i32,
    pub(crate) silent: bool,
    pub(crate) rollback_prompt_disabled: bool,
}

fn rollback_vcs_exclusions() -> Vec<String> {
    [".git", ".hg", ".svn"]
        .iter()
        .map(|entry| String::from(*entry))
        .collect()
}

fn enforce_rollback_limits(silent: bool) {
    let config = match config::user::load_user_config() {
        Ok(Some(config)) => config,
        Ok(None) => config::user::UserConfig::default(),
        Err(e) => {
            tracing::warn!("Failed to load user config for rollback limits: {e}");
            return;
        }
    };

    let sessions = match rollback_session::discover_rollback_sessions() {
        Ok(sessions) => sessions,
        Err(e) => {
            tracing::warn!("Failed to discover sessions for limit enforcement: {e}");
            return;
        }
    };

    if sessions.is_empty() {
        return;
    }

    let max_sessions = config.rollback.max_sessions;
    let storage_bytes_f64 =
        (config.rollback.max_storage_gb.max(0.0) * 1024.0 * 1024.0 * 1024.0).min(u64::MAX as f64);
    let max_storage_bytes = storage_bytes_f64 as u64;

    let completed: Vec<&rollback_session::SessionInfo> = sessions
        .iter()
        .filter(|session| !session.is_alive)
        .collect();

    let mut pruned = 0usize;
    let mut pruned_bytes = 0u64;

    if completed.len() > max_sessions {
        for session in &completed[max_sessions..] {
            if let Err(e) = rollback_session::remove_session(&session.dir) {
                tracing::warn!(
                    "Failed to prune session {}: {e}",
                    session.metadata.session_id
                );
            } else {
                pruned = pruned.saturating_add(1);
                pruned_bytes = pruned_bytes.saturating_add(session.disk_size);
            }
        }
    }

    let total = match rollback_session::total_rollback_storage_bytes() {
        Ok(total) => total,
        Err(_) => return,
    };

    if total > max_storage_bytes {
        let remaining = match rollback_session::discover_rollback_sessions() {
            Ok(sessions) => sessions,
            Err(_) => return,
        };

        let mut current_total = total;
        for session in remaining.iter().rev().filter(|session| !session.is_alive) {
            if current_total <= max_storage_bytes {
                break;
            }
            if let Err(e) = rollback_session::remove_session(&session.dir) {
                tracing::warn!(
                    "Failed to prune session {}: {e}",
                    session.metadata.session_id
                );
            } else {
                current_total = current_total.saturating_sub(session.disk_size);
                pruned = pruned.saturating_add(1);
                pruned_bytes = pruned_bytes.saturating_add(session.disk_size);
            }
        }
    }

    if pruned > 0 && !silent {
        eprintln!(
            "  Auto-pruned {} old session(s) (freed {})",
            pruned,
            rollback_session::format_bytes(pruned_bytes),
        );
    }
}

/// Generate a fresh session ID of the form `YYYYMMDD-HHMMSS-<pid>`.
///
/// Uses UTC so IDs are stable across timezones and DST transitions, which is
/// important because rollback session IDs are parsed for sorting and PID
/// extraction from disk state written on possibly-different hosts.
fn generate_session_id() -> String {
    format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        std::process::id()
    )
}

/// Create `<root>/<session_id>` with 0700 permissions.
fn create_session_dir(root: &Path, session_id: &str) -> Result<PathBuf> {
    let session_dir = root.join(session_id);
    std::fs::create_dir_all(&session_dir).map_err(|e| {
        nono::NonoError::Snapshot(format!(
            "Failed to create session directory {}: {}",
            session_dir.display(),
            e
        ))
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        if let Err(e) = std::fs::set_permissions(&session_dir, perms) {
            warn!("Failed to set session directory permissions to 0700: {e}");
        }
    }

    Ok(session_dir)
}

/// Create a new rollback session directory under `~/.nono/rollbacks/`
/// (or a user-supplied destination) and return the generated ID and path.
fn ensure_rollback_session_dir(
    rollback_destination: Option<&PathBuf>,
) -> Result<(String, PathBuf)> {
    let root = match rollback_destination {
        Some(path) => path.clone(),
        None => rollback_session::rollback_root()?,
    };
    let session_id = generate_session_id();
    let session_dir = create_session_dir(&root, &session_id)?;
    Ok((session_id, session_dir))
}

/// Create a new audit-only session directory under `~/.nono/audit/`.
///
/// Kept separate from `~/.nono/rollbacks/` so audit-only sessions are never
/// mistaken for rollback history and are not subject to rollback retention
/// limits.
fn ensure_audit_session_dir() -> Result<(String, PathBuf)> {
    let root = rollback_session::audit_root()?;
    let session_id = generate_session_id();
    let session_dir = create_session_dir(&root, &session_id)?;
    Ok((session_id, session_dir))
}

/// Extract paths the user explicitly granted access to, in a form suitable
/// for `SessionMetadata.tracked_paths`.
///
/// Returns directory capabilities (not files) whose source is `User`. Both
/// read and write access modes are included so that audit-only sessions still
/// carry the project root(s) the sandbox operated on — this is what
/// `nono audit list` groups by and what `--path` filters against.
fn user_tracked_paths(caps: &CapabilitySet) -> Vec<PathBuf> {
    caps.fs_capabilities()
        .iter()
        .filter(|cap| !cap.is_file && matches!(cap.source, nono::CapabilitySource::User))
        .map(|cap| cap.resolved.clone())
        .collect()
}

/// Describes a rollback session that audit metadata should be co-written into.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RollbackSharing<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) session_dir: &'a Path,
    pub(crate) tracked_paths: &'a [PathBuf],
}

impl RollbackRuntimeState {
    /// View of the rollback session that audit can co-write into.
    pub(crate) fn as_sharing(&self) -> RollbackSharing<'_> {
        RollbackSharing {
            session_id: &self.session_id,
            session_dir: &self.session_dir,
            tracked_paths: &self.tracked_paths,
        }
    }
}

/// Create the audit state for a supervised session.
///
/// - When audit is disabled (`--no-audit`), returns `None`.
/// - When rollback is active (`sharing` is `Some`), shares the rollback
///   session ID and directory so both records live in a single `session.json`.
/// - Otherwise creates a fresh directory under `~/.nono/audit/` that holds
///   only audit metadata and populates `tracked_paths` from `caps`.
pub(crate) fn create_audit_state(
    audit_disabled: bool,
    sharing: Option<RollbackSharing<'_>>,
    caps: &CapabilitySet,
) -> Result<Option<AuditState>> {
    if audit_disabled {
        return Ok(None);
    }

    if let Some(share) = sharing {
        return Ok(Some(AuditState {
            session_id: share.session_id.to_string(),
            session_dir: share.session_dir.to_path_buf(),
            tracked_paths: share.tracked_paths.to_vec(),
        }));
    }

    let (session_id, session_dir) = ensure_audit_session_dir()?;
    Ok(Some(AuditState {
        session_id,
        session_dir,
        tracked_paths: user_tracked_paths(caps),
    }))
}

pub(crate) fn warn_if_rollback_flags_ignored(rollback: &RollbackLaunchOptions, silent: bool) {
    if !rollback.disabled {
        return;
    }

    let has_rollback_flags = rollback.track_all
        || !rollback.include.is_empty()
        || !rollback.exclude_patterns.is_empty()
        || !rollback.exclude_globs.is_empty();
    if has_rollback_flags {
        warn!(
            "--no-rollback is active; rollback flags \
             (--rollback-all, --rollback-include, --rollback-exclude) \
             have no effect"
        );
        if !silent {
            eprintln!(
                "  [nono] Warning: --no-rollback is active; \
                 rollback customization flags have no effect."
            );
        }
    }
}

pub(crate) fn initialize_rollback_state(
    rollback: &RollbackLaunchOptions,
    caps: &CapabilitySet,
    silent: bool,
) -> Result<Option<RollbackRuntimeState>> {
    if !rollback.requested || rollback.disabled {
        return Ok(None);
    }

    let tracked_paths: Vec<PathBuf> = caps
        .fs_capabilities()
        .iter()
        .filter(|cap| {
            !cap.is_file
                && matches!(cap.access, AccessMode::Write | AccessMode::ReadWrite)
                && matches!(cap.source, nono::CapabilitySource::User)
        })
        .map(|cap| cap.resolved.clone())
        .collect();

    if tracked_paths.is_empty() {
        return Ok(None);
    }

    enforce_rollback_limits(silent);

    // Rollback owns its own session directory under `~/.nono/rollbacks/` (or
    // a user-provided destination); audit will share it via `create_audit_state`
    // when both are active.
    let (session_id, session_dir) = ensure_rollback_session_dir(rollback.destination.as_ref())?;

    let mut patterns = if rollback.track_all {
        rollback_vcs_exclusions()
    } else {
        rollback_base_exclusions()
    };
    patterns.extend(rollback.exclude_patterns.iter().cloned());
    patterns.sort_unstable();
    patterns.dedup();
    let base_patterns = patterns.clone();
    let exclusion_config = nono::undo::ExclusionConfig {
        use_gitignore: true,
        exclude_patterns: patterns,
        exclude_globs: rollback.exclude_globs.clone(),
        force_include: rollback.include.clone(),
    };
    let gitignore_root = tracked_paths
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
    let mut exclusion = nono::undo::ExclusionFilter::new(exclusion_config, &gitignore_root)?;

    if !rollback.track_all {
        let preflight_result =
            rollback_preflight::run_preflight(&tracked_paths, &exclusion, &rollback.skip_dirs);

        if preflight_result.needs_warning() {
            let auto_excluded: Vec<&rollback_preflight::HeavyDir> = preflight_result
                .heavy_dirs
                .iter()
                .filter(|dir| !rollback.include.contains(&dir.name))
                .collect();

            if !auto_excluded.is_empty() {
                let excluded_names: Vec<String> =
                    auto_excluded.iter().map(|dir| dir.name.clone()).collect();
                let mut all_patterns = base_patterns.clone();
                all_patterns.extend(excluded_names);
                all_patterns.sort_unstable();
                all_patterns.dedup();
                let updated_config = nono::undo::ExclusionConfig {
                    use_gitignore: true,
                    exclude_patterns: all_patterns,
                    exclude_globs: rollback.exclude_globs.clone(),
                    force_include: rollback.include.clone(),
                };
                exclusion = nono::undo::ExclusionFilter::new(updated_config, &gitignore_root)?;

                if !silent {
                    rollback_preflight::print_auto_exclude_notice(
                        &auto_excluded,
                        &preflight_result,
                    );
                }
            }
        }
    }

    let mut manager = nono::undo::SnapshotManager::new(
        session_dir.clone(),
        tracked_paths.clone(),
        exclusion,
        nono::undo::WalkBudget::default(),
    )?;

    let baseline = manager.create_baseline()?;
    let atomic_temp_before = manager.collect_atomic_temp_files();

    output::print_rollback_tracking(&tracked_paths, silent);

    Ok(Some(RollbackRuntimeState {
        manager,
        baseline,
        tracked_paths,
        atomic_temp_before,
        session_id,
        session_dir,
    }))
}

pub(crate) fn finalize_supervised_exit(ctx: RollbackExitContext<'_>) -> Result<()> {
    let RollbackExitContext {
        audit_state,
        rollback_state,
        proxy_handle,
        started,
        ended,
        command,
        exit_code,
        silent,
        rollback_prompt_disabled,
    } = ctx;

    let mut network_events = proxy_handle.map_or_else(
        Vec::new,
        nono_proxy::server::ProxyHandle::drain_audit_events,
    );

    let mut audit_saved = false;

    if let Some(RollbackRuntimeState {
        mut manager,
        baseline,
        tracked_paths,
        atomic_temp_before,
        session_id: rb_session_id,
        session_dir: _,
    }) = rollback_state
    {
        let (final_manifest, changes) = manager.create_incremental(&baseline)?;
        let merkle_roots = vec![baseline.merkle_root, final_manifest.merkle_root];

        let meta = nono::undo::SessionMetadata {
            session_id: rb_session_id,
            started: started.to_string(),
            ended: Some(ended.to_string()),
            command: command.to_vec(),
            tracked_paths,
            snapshot_count: manager.snapshot_count(),
            exit_code: Some(exit_code),
            merkle_roots,
            network_events: std::mem::take(&mut network_events),
        };
        manager.save_session_metadata(&meta)?;
        audit_saved = true;

        if !changes.is_empty() {
            output::print_rollback_session_summary(&changes, silent);

            if !rollback_prompt_disabled && !silent {
                let _ = rollback_ui::review_and_restore(&manager, &baseline, &changes);
            }
        }

        let _ = manager.cleanup_new_atomic_temp_files(&atomic_temp_before);
    }

    // Audit-only path: no rollback snapshots, just persist session metadata
    // with network events. This is the default for supervised sessions.
    // `tracked_paths` is populated from the user-granted capabilities so that
    // `nono audit list` can still group and filter audit-only sessions.
    if !audit_saved {
        if let Some(audit_state) = audit_state {
            let meta = nono::undo::SessionMetadata {
                session_id: audit_state.session_id.clone(),
                started: started.to_string(),
                ended: Some(ended.to_string()),
                command: command.to_vec(),
                tracked_paths: audit_state.tracked_paths.clone(),
                snapshot_count: 0,
                exit_code: Some(exit_code),
                merkle_roots: Vec::new(),
                network_events,
            };
            nono::undo::SnapshotManager::write_session_metadata(&audit_state.session_dir, &meta)?;
        }
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn create_audit_state_returns_none_when_disabled() {
        let caps = CapabilitySet::new();
        let result = create_audit_state(true, None, &caps).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn create_audit_state_shares_rollback_session_when_active() {
        let caps = CapabilitySet::new();
        let tmp = tempfile::tempdir().unwrap();
        let shared_dir = tmp.path().to_path_buf();
        let tracked = vec![PathBuf::from("/tmp/project")];
        let sharing = RollbackSharing {
            session_id: "shared-123",
            session_dir: &shared_dir,
            tracked_paths: &tracked,
        };

        let state = create_audit_state(false, Some(sharing), &caps)
            .unwrap()
            .unwrap();

        assert_eq!(state.session_id, "shared-123");
        assert_eq!(state.session_dir, shared_dir);
        assert_eq!(state.tracked_paths, tracked);
    }

    #[test]
    fn generate_session_id_contains_pid() {
        let session_id = generate_session_id();
        let pid = std::process::id().to_string();
        assert!(
            session_id.contains(&pid),
            "session_id '{session_id}' should contain pid '{pid}'"
        );
    }

    #[test]
    fn generate_session_id_uses_utc_date_prefix() {
        // ID format is `YYYYMMDD-HHMMSS-<pid>`. Verify the timestamp portion
        // matches the current UTC date — catches accidental switches back to
        // Local time (the original reviewer concern).
        let session_id = generate_session_id();
        let utc_prefix = chrono::Utc::now().format("%Y%m%d").to_string();
        assert!(
            session_id.starts_with(&utc_prefix),
            "session_id '{session_id}' should start with UTC date '{utc_prefix}'"
        );
    }

    #[test]
    fn create_session_dir_makes_dir_under_root() {
        let tmp = tempfile::tempdir().unwrap();
        let session_dir = create_session_dir(tmp.path(), "my-session").unwrap();

        assert!(session_dir.exists());
        assert!(session_dir.starts_with(tmp.path()));
        assert!(session_dir.ends_with("my-session"));
    }

    #[cfg(unix)]
    #[test]
    fn create_session_dir_sets_0700_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let session_dir = create_session_dir(tmp.path(), "perms-test").unwrap();

        let mode = std::fs::metadata(&session_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "session dir should have 0700 permissions");
    }

    #[test]
    fn ensure_rollback_session_dir_honours_custom_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().to_path_buf();

        let (session_id, session_dir) = ensure_rollback_session_dir(Some(&dest)).unwrap();

        assert!(!session_id.is_empty());
        assert!(session_dir.exists());
        assert!(session_dir.starts_with(tmp.path()));
    }

    #[test]
    fn user_tracked_paths_returns_empty_for_default_caps() {
        let caps = CapabilitySet::new();
        assert!(user_tracked_paths(&caps).is_empty());
    }
}
