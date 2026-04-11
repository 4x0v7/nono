//! Session discovery and management for the audit/rollback system
//!
//! Rollback sessions (which have file snapshots) live in `~/.nono/rollbacks/`.
//! Audit-only sessions (no snapshots, just command + network metadata) live in
//! `~/.nono/audit/`. Both directories contain subdirectories named by
//! `session_id`, each holding a `session.json` with [`SessionMetadata`].
//!
//! Rollback commands only look at the rollback root. Audit commands look at
//! both roots so that an audit-only session is visible in `nono audit list`
//! without being confused for a rollback session by pruning or restore.

use nono::undo::{SessionMetadata, SnapshotManager};
use nono::{NonoError, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Whether a discovered session carries rollback snapshots or is audit-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// Session stored in `~/.nono/rollbacks/` with full snapshot state.
    Rollback,
    /// Session stored in `~/.nono/audit/` with metadata only.
    AuditOnly,
}

/// Information about a discovered session (rollback or audit-only).
#[derive(Debug)]
pub struct SessionInfo {
    /// Session metadata loaded from session.json
    pub metadata: SessionMetadata,
    /// Path to the session directory
    pub dir: PathBuf,
    /// Total disk usage in bytes
    pub disk_size: u64,
    /// Whether the session's process is still running
    pub is_alive: bool,
    /// Whether the session appears stale (ended is None and PID is dead)
    pub is_stale: bool,
    /// Which root directory this session lives under.
    pub kind: SessionKind,
}

/// Get the rollback root directory (`~/.nono/rollbacks/`)
pub fn rollback_root() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or(NonoError::HomeNotFound)?;
    Ok(home.join(".nono").join("rollbacks"))
}

/// Get the audit-only root directory (`~/.nono/audit/`)
pub fn audit_root() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or(NonoError::HomeNotFound)?;
    Ok(home.join(".nono").join("audit"))
}

/// Scan a single session root directory and load metadata for each subdir.
///
/// Sessions with missing or corrupt metadata are silently skipped so that a
/// single broken session cannot break discovery.
fn scan_session_root(root: &Path, kind: SessionKind) -> Result<Vec<SessionInfo>> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();

    let entries = fs::read_dir(root).map_err(|e| {
        NonoError::Snapshot(format!(
            "Failed to read session directory {}: {e}",
            root.display()
        ))
    })?;

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        // Try to load session metadata
        let metadata = match SnapshotManager::load_session_metadata(&dir) {
            Ok(m) => m,
            Err(_) => continue, // Skip corrupt or incomplete sessions
        };

        let pid = parse_pid_from_session_id(&metadata.session_id);
        let is_alive = pid.map(is_process_alive).unwrap_or(false);
        let is_stale = metadata.ended.is_none() && !is_alive;
        let disk_size = calculate_dir_size(&dir);

        sessions.push(SessionInfo {
            metadata,
            dir,
            disk_size,
            is_alive,
            is_stale,
            kind,
        });
    }

    Ok(sessions)
}

/// Discover rollback sessions in `~/.nono/rollbacks/`.
///
/// Returns only sessions that have an associated rollback session directory
/// (i.e. snapshots were taken). Audit-only sessions are not included.
pub fn discover_rollback_sessions() -> Result<Vec<SessionInfo>> {
    let root = rollback_root()?;
    let mut sessions = scan_session_root(&root, SessionKind::Rollback)?;
    sessions.sort_by(|a, b| b.metadata.started.cmp(&a.metadata.started));
    Ok(sessions)
}

/// Discover all audit-visible sessions (rollback + audit-only).
///
/// Both `~/.nono/rollbacks/` and `~/.nono/audit/` are scanned. Rollback
/// sessions are audit sessions too (they contain the same metadata), so
/// `nono audit list` shows the union.
pub fn discover_audit_sessions() -> Result<Vec<SessionInfo>> {
    let rollback_root = rollback_root()?;
    let audit_root = audit_root()?;

    let mut sessions = scan_session_root(&rollback_root, SessionKind::Rollback)?;
    sessions.extend(scan_session_root(&audit_root, SessionKind::AuditOnly)?);
    sessions.sort_by(|a, b| b.metadata.started.cmp(&a.metadata.started));
    Ok(sessions)
}

/// Load a single session from a specific root directory, with path-traversal
/// protection.
fn load_session_from(root: &Path, session_id: &str, kind: SessionKind) -> Result<SessionInfo> {
    let dir = root.join(session_id);

    // Defense in depth: verify the resolved path is within the expected root.
    // Both canonicalizations must succeed -- fail closed if either cannot
    // be resolved (prevents bypassing the traversal check).
    let canonical_root = root.canonicalize().map_err(|e| {
        NonoError::SessionNotFound(format!(
            "Cannot canonicalize session root {}: {}",
            root.display(),
            e
        ))
    })?;
    let canonical_dir = dir.canonicalize().map_err(|_| {
        // Don't leak path details in error -- session simply doesn't exist
        NonoError::SessionNotFound(session_id.to_string())
    })?;
    if !canonical_dir.starts_with(&canonical_root) {
        return Err(NonoError::SessionNotFound(session_id.to_string()));
    }

    if !dir.exists() {
        return Err(NonoError::SessionNotFound(session_id.to_string()));
    }

    let metadata = SnapshotManager::load_session_metadata(&dir)?;
    let pid = parse_pid_from_session_id(&metadata.session_id);
    let is_alive = pid.map(is_process_alive).unwrap_or(false);
    let is_stale = metadata.ended.is_none() && !is_alive;
    let disk_size = calculate_dir_size(&dir);

    Ok(SessionInfo {
        metadata,
        dir,
        disk_size,
        is_alive,
        is_stale,
        kind,
    })
}

/// Load a rollback session by ID from `~/.nono/rollbacks/`.
///
/// The session_id is validated to prevent path traversal — it must not
/// contain path separators or `..` components.
pub fn load_rollback_session(session_id: &str) -> Result<SessionInfo> {
    validate_session_id(session_id)?;
    let root = rollback_root()?;
    load_session_from(&root, session_id, SessionKind::Rollback)
}

/// Load an audit session by ID, checking both rollback and audit roots.
///
/// Rollback sessions are preferred so that rich snapshot data is returned
/// when both directories happen to contain the same ID.
pub fn load_audit_session(session_id: &str) -> Result<SessionInfo> {
    validate_session_id(session_id)?;
    let rollback_root = rollback_root()?;
    if rollback_root.join(session_id).exists() {
        return load_session_from(&rollback_root, session_id, SessionKind::Rollback);
    }
    let audit_root = audit_root()?;
    load_session_from(&audit_root, session_id, SessionKind::AuditOnly)
}

/// Calculate the total disk usage of all rollback sessions.
///
/// Only `~/.nono/rollbacks/` is counted; audit-only sessions are not subject
/// to rollback retention limits.
pub fn total_rollback_storage_bytes() -> Result<u64> {
    let root = rollback_root()?;
    if !root.exists() {
        return Ok(0);
    }
    Ok(calculate_dir_size(&root))
}

/// Remove a session directory.
pub fn remove_session(dir: &Path) -> Result<()> {
    fs::remove_dir_all(dir).map_err(|e| {
        NonoError::Snapshot(format!(
            "Failed to remove session directory {}: {e}",
            dir.display()
        ))
    })
}

/// Validate a session ID to prevent path traversal.
///
/// Session IDs must match the format `YYYYMMDD-HHMMSS-<pid>` and must not
/// contain path separators, `..`, or other dangerous characters.
fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty() {
        return Err(NonoError::SessionNotFound("empty session ID".to_string()));
    }
    if session_id.contains(std::path::MAIN_SEPARATOR)
        || session_id.contains('/')
        || session_id.contains("..")
        || session_id.contains('\0')
    {
        return Err(NonoError::SessionNotFound(format!(
            "invalid session ID: {session_id}"
        )));
    }
    Ok(())
}

/// Parse the PID from a session ID formatted as `YYYYMMDD-HHMMSS-<pid>`.
fn parse_pid_from_session_id(session_id: &str) -> Option<u32> {
    session_id.rsplit('-').next()?.parse().ok()
}

/// Check if a process with the given PID is still alive.
fn is_process_alive(pid: u32) -> bool {
    // kill(pid, 0) checks if the process exists without sending a signal
    // SAFETY: This is a standard POSIX way to check process existence.
    // Signal 0 does not actually send anything.
    unsafe { nix::libc::kill(pid as nix::libc::pid_t, 0) == 0 }
}

/// Calculate the total size of all files in a directory tree.
fn calculate_dir_size(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Format a byte count as a human-readable string.
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_session_id_rejects_traversal() {
        assert!(validate_session_id("../../../etc").is_err());
        assert!(validate_session_id("foo/bar").is_err());
        assert!(validate_session_id("foo\0bar").is_err());
        assert!(validate_session_id("..").is_err());
        assert!(validate_session_id("").is_err());
    }

    #[test]
    fn validate_session_id_accepts_valid() {
        assert!(validate_session_id("20260214-143022-12345").is_ok());
        assert!(validate_session_id("test-session").is_ok());
    }

    #[test]
    fn parse_pid_from_session_id_valid() {
        assert_eq!(
            parse_pid_from_session_id("20260214-143022-12345"),
            Some(12345)
        );
    }

    #[test]
    fn parse_pid_from_session_id_invalid() {
        assert_eq!(parse_pid_from_session_id("no-pid-here"), None);
        assert_eq!(parse_pid_from_session_id(""), None);
    }

    #[test]
    fn format_bytes_display() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn discover_sessions_empty_dir() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // Override undo_root by testing calculate_dir_size directly
        let size = calculate_dir_size(dir.path());
        assert_eq!(size, 0);
    }

    #[test]
    fn calculate_dir_size_works() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        fs::write(dir.path().join("a.txt"), b"hello").expect("write");
        fs::write(dir.path().join("b.txt"), b"world!").expect("write");
        let size = calculate_dir_size(dir.path());
        assert_eq!(size, 11); // 5 + 6
    }

    #[test]
    fn is_current_process_alive() {
        assert!(is_process_alive(std::process::id()));
    }

    #[test]
    fn dead_process_not_alive() {
        // PID 99999999 is very unlikely to exist
        assert!(!is_process_alive(99_999_999));
    }
}
