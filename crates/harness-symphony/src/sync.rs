use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::changeset::{changeset_files, changeset_id, ChangesetError};
use crate::config::ResolvedConfig;
use crate::state::{RunStateStore, StateError};

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("{0}")]
    Changeset(#[from] ChangesetError),
    #[error("{0}")]
    State(#[from] StateError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("harness-cli failed for {path}: {stderr}")]
    ApplyFailed { path: String, stderr: String },
    #[error("sync io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncChange {
    pub id: String,
    pub path: PathBuf,
    pub applied: bool,
    pub operations: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncResult {
    pub changes: Vec<SyncChange>,
}

pub fn sync_changesets(config: &ResolvedConfig) -> Result<SyncResult, SyncError> {
    let store = RunStateStore::new(config.state_db.clone());
    store.init()?;
    let paths = changeset_files(&config.changeset_directory)?;
    let mut changes = Vec::new();
    for path in paths {
        let id = changeset_id(&path)?;
        if harness_db_has_changeset(&config.harness_db, &id)? && store.changeset_synced(&id)? {
            changes.push(SyncChange {
                id,
                path,
                applied: false,
                operations: 0,
            });
            continue;
        }
        let output = Command::new(config.repo_root.join("scripts/bin/harness-cli"))
            .args(["db", "changeset", "apply"])
            .arg(&path)
            .env("HARNESS_DB_PATH", &config.harness_db)
            .current_dir(&config.repo_root)
            .output()?;
        if !output.status.success() {
            return Err(SyncError::ApplyFailed {
                path: path.display().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let applied = stdout.contains(" applied ");
        let operations = parse_operations(&stdout);
        store.record_changeset_synced(&id, &path, applied)?;
        changes.push(SyncChange {
            id,
            path,
            applied,
            operations,
        });
    }
    Ok(SyncResult { changes })
}

pub fn unapplied_changesets(config: &ResolvedConfig) -> Result<Vec<PathBuf>, SyncError> {
    let store = RunStateStore::new(config.state_db.clone());
    store.init()?;
    let mut unapplied = Vec::new();
    for path in changeset_files(&config.changeset_directory)? {
        let id = changeset_id(&path)?;
        if !harness_db_has_changeset(&config.harness_db, &id)? || !store.changeset_synced(&id)? {
            unapplied.push(path);
        }
    }
    Ok(unapplied)
}

fn harness_db_has_changeset(db_path: &Path, id: &str) -> Result<bool, SyncError> {
    if !db_path.exists() {
        return Ok(false);
    }
    let connection = Connection::open(db_path)?;
    connection
        .query_row(
            "SELECT 1 FROM changeset_applied WHERE id=?1;",
            params![id],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(SyncError::from)
}

fn parse_operations(stdout: &str) -> usize {
    stdout
        .split('(')
        .nth(1)
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_operation_count_from_cli_output() {
        assert_eq!(
            parse_operations("Changeset run_1 applied (3 operation(s))."),
            3
        );
        assert_eq!(
            parse_operations("Changeset run_1 already applied; skipped."),
            0
        );
    }
}
