use std::fs;
use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error(
        "active run already exists: {0}. Finish, cancel, or fail that run before starting another."
    )]
    ActiveRunExists(String),
    #[error("run not found: {0}")]
    RunNotFound(String),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub run_id: String,
    pub story_id: String,
    pub branch: Option<String>,
    pub worktree: PathBuf,
    pub status: String,
    pub result_path: Option<PathBuf>,
    pub pr_url: Option<String>,
    pub sync_status: String,
    pub next_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct NewRunRecord {
    pub run_id: String,
    pub story_id: String,
    pub branch: Option<String>,
    pub worktree: PathBuf,
    pub status: String,
    pub result_path: Option<PathBuf>,
    pub sync_status: String,
    pub next_action: String,
}

pub struct RunStateStore {
    path: PathBuf,
}

impl RunStateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn init(&self) -> Result<(), StateError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&self.path)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS run_state (
                run_id TEXT PRIMARY KEY,
                story_id TEXT NOT NULL,
                branch TEXT,
                worktree TEXT NOT NULL,
                status TEXT NOT NULL,
                result_path TEXT,
                pr_url TEXT,
                sync_status TEXT NOT NULL DEFAULT 'not_applicable',
                next_action TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS changeset_sync (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                applied INTEGER NOT NULL,
                synced_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn add_run(&self, input: NewRunRecord) -> Result<(), StateError> {
        self.init()?;
        let mut connection = Connection::open(&self.path)?;
        let transaction = connection.transaction()?;
        if let Some(active) = active_run_id(&transaction)? {
            return Err(StateError::ActiveRunExists(active));
        }
        transaction.execute(
            "INSERT INTO run_state (
                run_id, story_id, branch, worktree, status, result_path,
                sync_status, next_action
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);",
            params![
                input.run_id,
                input.story_id,
                input.branch,
                input.worktree.display().to_string(),
                input.status,
                input.result_path.map(|path| path.display().to_string()),
                input.sync_status,
                input.next_action,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn update_status(
        &self,
        run_id: &str,
        status: &str,
        next_action: &str,
    ) -> Result<(), StateError> {
        self.init()?;
        let connection = Connection::open(&self.path)?;
        connection.execute(
            "UPDATE run_state
             SET status=?1, next_action=?2, updated_at=datetime('now')
             WHERE run_id=?3;",
            params![status, next_action, run_id],
        )?;
        if connection.changes() == 0 {
            return Err(StateError::RunNotFound(run_id.to_owned()));
        }
        Ok(())
    }

    pub fn list_runs(&self) -> Result<Vec<RunRecord>, StateError> {
        self.init()?;
        let connection = Connection::open(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT run_id, story_id, branch, worktree, status, result_path,
                    pr_url, sync_status, next_action
             FROM run_state
             ORDER BY created_at DESC, run_id DESC;",
        )?;
        let rows = statement.query_map([], run_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StateError::from)
    }

    pub fn show_run(&self, run_id: &str) -> Result<RunRecord, StateError> {
        self.init()?;
        let connection = Connection::open(&self.path)?;
        connection
            .query_row(
                "SELECT run_id, story_id, branch, worktree, status, result_path,
                        pr_url, sync_status, next_action
                 FROM run_state
                 WHERE run_id=?1;",
                params![run_id],
                run_from_row,
            )
            .optional()?
            .ok_or_else(|| StateError::RunNotFound(run_id.to_owned()))
    }

    pub fn active_run(&self) -> Result<Option<RunRecord>, StateError> {
        self.init()?;
        let connection = Connection::open(&self.path)?;
        connection
            .query_row(
                "SELECT run_id, story_id, branch, worktree, status, result_path,
                        pr_url, sync_status, next_action
                 FROM run_state
                 WHERE status IN ('prepared', 'running')
                 ORDER BY created_at ASC
                 LIMIT 1;",
                [],
                run_from_row,
            )
            .optional()
            .map_err(StateError::from)
    }

    pub fn update_pr_url(&self, run_id: &str, pr_url: &str) -> Result<(), StateError> {
        self.init()?;
        let connection = Connection::open(&self.path)?;
        connection.execute(
            "UPDATE run_state
             SET pr_url=?1, next_action='review pull request', updated_at=datetime('now')
             WHERE run_id=?2;",
            params![pr_url, run_id],
        )?;
        if connection.changes() == 0 {
            return Err(StateError::RunNotFound(run_id.to_owned()));
        }
        Ok(())
    }

    pub fn record_changeset_synced(
        &self,
        id: &str,
        path: &std::path::Path,
        applied: bool,
    ) -> Result<(), StateError> {
        self.init()?;
        let connection = Connection::open(&self.path)?;
        connection.execute(
            "INSERT INTO changeset_sync (id, path, applied, synced_at)
             VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                path=excluded.path,
                applied=excluded.applied,
                synced_at=datetime('now');",
            params![id, path.display().to_string(), i64::from(applied)],
        )?;
        Ok(())
    }

    pub fn changeset_synced(&self, id: &str) -> Result<bool, StateError> {
        self.init()?;
        let connection = Connection::open(&self.path)?;
        connection
            .query_row(
                "SELECT 1 FROM changeset_sync WHERE id=?1;",
                params![id],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(StateError::from)
    }
}

fn active_run_id(connection: &Connection) -> Result<Option<String>, StateError> {
    connection
        .query_row(
            "SELECT run_id FROM run_state
             WHERE status IN ('prepared', 'running')
             ORDER BY created_at ASC
             LIMIT 1;",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(StateError::from)
}

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        run_id: row.get(0)?,
        story_id: row.get(1)?,
        branch: row.get(2)?,
        worktree: PathBuf::from(row.get::<_, String>(3)?),
        status: row.get(4)?,
        result_path: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
        pr_url: row.get(6)?,
        sync_status: row.get(7)?,
        next_action: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_record(run_id: &str, status: &str) -> NewRunRecord {
        NewRunRecord {
            run_id: run_id.to_owned(),
            story_id: "US-STATE".to_owned(),
            branch: Some(format!("symphony/{run_id}")),
            worktree: PathBuf::from(format!(".symphony/worktrees/{run_id}")),
            status: status.to_owned(),
            result_path: Some(PathBuf::from(format!(".harness/runs/{run_id}/RESULT.json"))),
            sync_status: "not_applicable".to_owned(),
            next_action: "continue run".to_owned(),
        }
    }

    #[test]
    fn store_creates_state_db_and_records_run() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = RunStateStore::new(temp_dir.path().join(".symphony/state.db"));

        store.add_run(new_record("run_1", "prepared")).unwrap();

        assert!(temp_dir.path().join(".symphony/state.db").exists());
        let runs = store.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, "run_1");
        assert_eq!(runs[0].story_id, "US-STATE");
    }

    #[test]
    fn second_active_run_is_refused() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = RunStateStore::new(temp_dir.path().join(".symphony/state.db"));

        store.add_run(new_record("run_1", "prepared")).unwrap();
        let error = store.add_run(new_record("run_2", "prepared")).unwrap_err();

        assert!(matches!(error, StateError::ActiveRunExists(id) if id == "run_1"));
    }

    #[test]
    fn terminal_status_releases_active_lock() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = RunStateStore::new(temp_dir.path().join(".symphony/state.db"));

        store.add_run(new_record("run_1", "prepared")).unwrap();
        store
            .update_status("run_1", "completed", "review result")
            .unwrap();
        store.add_run(new_record("run_2", "prepared")).unwrap();

        assert_eq!(store.active_run().unwrap().unwrap().run_id, "run_2");
    }

    #[test]
    fn failed_and_cancelled_release_active_lock() {
        for terminal in ["failed", "cancelled"] {
            let temp_dir = tempfile::tempdir().unwrap();
            let store = RunStateStore::new(temp_dir.path().join(".symphony/state.db"));

            store.add_run(new_record("run_1", "running")).unwrap();
            store
                .update_status("run_1", terminal, "inspect result")
                .unwrap();

            assert!(store.active_run().unwrap().is_none());
        }
    }

    #[test]
    fn records_synced_changesets_idempotently() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = RunStateStore::new(temp_dir.path().join(".symphony/state.db"));

        assert!(!store.changeset_synced("run_1").unwrap());
        store
            .record_changeset_synced(
                "run_1",
                std::path::Path::new(".harness/changesets/run_1.changeset.jsonl"),
                true,
            )
            .unwrap();
        store
            .record_changeset_synced(
                "run_1",
                std::path::Path::new(".harness/changesets/run_1.changeset.jsonl"),
                false,
            )
            .unwrap();

        assert!(store.changeset_synced("run_1").unwrap());
    }
}
