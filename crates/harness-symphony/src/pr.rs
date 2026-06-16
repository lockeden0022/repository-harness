use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

use crate::config::ResolvedConfig;
use crate::state::{RunRecord, RunStateStore, StateError};

#[derive(Debug, Error)]
pub enum PrError {
    #[error("PR creation is disabled by pull_request.create")]
    Disabled,
    #[error("run outcome {0} should not open a PR by default")]
    OutcomeNotAllowed(String),
    #[error("run artifacts are missing for {0}")]
    MissingArtifacts(String),
    #[error("unsupported PR provider: {0}")]
    UnsupportedProvider(String),
    #[error("gh command failed: {0}")]
    GhFailed(String),
    #[error("git command failed: {0}")]
    GitFailed(String),
    #[error("{0}")]
    State(#[from] StateError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrPlan {
    pub run_id: String,
    pub draft: bool,
    pub title: String,
    pub body_path: PathBuf,
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCreateResult {
    pub plan: PrPlan,
    pub url: Option<String>,
}

pub fn plan_pr(config: &ResolvedConfig, run: &RunRecord) -> Result<PrPlan, PrError> {
    if matches!(config.pull_request_create.as_str(), "disabled" | "never") {
        return Err(PrError::Disabled);
    }
    if matches!(run.status.as_str(), "failed" | "cancelled") {
        return Err(PrError::OutcomeNotAllowed(run.status.clone()));
    }
    let draft = run.status != "completed"
        && config
            .pull_request_draft_for
            .iter()
            .any(|outcome| outcome == &run.status);
    if run.status != "completed" && !draft {
        return Err(PrError::OutcomeNotAllowed(run.status.clone()));
    }

    let summary = config
        .repo_root
        .join(format!(".harness/runs/{}/SUMMARY.md", run.run_id));
    let result = config
        .repo_root
        .join(format!(".harness/runs/{}/RESULT.json", run.run_id));
    let changeset = config
        .changeset_directory
        .join(format!("{}.changeset.jsonl", run.run_id));
    if !summary.exists() || !result.exists() || !changeset.exists() {
        return Err(PrError::MissingArtifacts(run.run_id.clone()));
    }

    Ok(PrPlan {
        run_id: run.run_id.clone(),
        draft,
        title: format!("{}: {}", run.story_id, run.status),
        body_path: summary.clone(),
        files: vec![summary, result, changeset],
    })
}

pub fn create_pr(
    config: &ResolvedConfig,
    run_id: &str,
    dry_run: bool,
) -> Result<PrCreateResult, PrError> {
    let store = RunStateStore::new(config.state_db.clone());
    let run = store.show_run(run_id)?;
    let plan = plan_pr(config, &run)?;
    ensure_forbidden_files_not_staged(&config.repo_root)?;
    if dry_run {
        return Ok(PrCreateResult { plan, url: None });
    }
    if config.pull_request_provider != "github" {
        return Err(PrError::UnsupportedProvider(
            config.pull_request_provider.clone(),
        ));
    }
    let mut command = Command::new("gh");
    command
        .args(["pr", "create", "--title", &plan.title, "--body-file"])
        .arg(&plan.body_path);
    if plan.draft {
        command.arg("--draft");
    }
    let output = command.current_dir(&config.repo_root).output()?;
    if !output.status.success() {
        return Err(PrError::GhFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    store.update_pr_url(run_id, &url)?;
    Ok(PrCreateResult {
        plan,
        url: Some(url),
    })
}

fn ensure_forbidden_files_not_staged(repo_root: &Path) -> Result<(), PrError> {
    let output = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(repo_root)
        .output()?;
    if !output.status.success() {
        return Err(PrError::GitFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let staged = String::from_utf8_lossy(&output.stdout);
    for path in staged.lines().map(str::trim) {
        if path == "harness.db" || path.starts_with(".symphony/") {
            return Err(PrError::GitFailed(format!(
                "forbidden file staged for PR: {path}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn config(root: &Path) -> ResolvedConfig {
        ResolvedConfig {
            version: 1,
            repo_root: root.to_path_buf(),
            harness_db: root.join("harness.db"),
            state_db: root.join(".symphony/state.db"),
            runs_dir: root.join(".harness/runs"),
            worktrees_dir: root.join(".symphony/worktrees"),
            single_active_run: true,
            agent_adapter: "custom".to_owned(),
            agent_command: vec![],
            agent_timeout_minutes: 120,
            pull_request_create: "ask".to_owned(),
            pull_request_provider: "github".to_owned(),
            pull_request_draft_for: vec!["blocked".to_owned(), "needs_intake".to_owned()],
            changeset_directory: root.join(".harness/changesets"),
            changeset_render_in_summary: true,
            allow_here_for_tiny: true,
            compact_keep_last: 50,
            keep_failed_worktrees: true,
            cleanup_after_sync: false,
        }
    }

    fn run(status: &str, root: &Path) -> RunRecord {
        RunRecord {
            run_id: "run_1".to_owned(),
            story_id: "US-041".to_owned(),
            branch: Some("symphony/run_1".to_owned()),
            worktree: root.join(".symphony/worktrees/run_1"),
            status: status.to_owned(),
            result_path: Some(PathBuf::from(".harness/runs/run_1/RESULT.json")),
            pr_url: None,
            sync_status: "not_applied".to_owned(),
            next_action: "review".to_owned(),
        }
    }

    #[test]
    fn completed_runs_get_normal_pr_plan() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = config(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join(".harness/runs/run_1")).unwrap();
        fs::create_dir_all(&config.changeset_directory).unwrap();
        fs::write(
            temp_dir.path().join(".harness/runs/run_1/SUMMARY.md"),
            "summary",
        )
        .unwrap();
        fs::write(
            temp_dir.path().join(".harness/runs/run_1/RESULT.json"),
            "{}",
        )
        .unwrap();
        fs::write(
            config.changeset_directory.join("run_1.changeset.jsonl"),
            "{}",
        )
        .unwrap();

        let plan = plan_pr(&config, &run("completed", temp_dir.path())).unwrap();

        assert!(!plan.draft);
        assert_eq!(plan.files.len(), 3);
    }

    #[test]
    fn configured_blocked_runs_get_draft_plan() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = config(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join(".harness/runs/run_1")).unwrap();
        fs::create_dir_all(&config.changeset_directory).unwrap();
        fs::write(
            temp_dir.path().join(".harness/runs/run_1/SUMMARY.md"),
            "summary",
        )
        .unwrap();
        fs::write(
            temp_dir.path().join(".harness/runs/run_1/RESULT.json"),
            "{}",
        )
        .unwrap();
        fs::write(
            config.changeset_directory.join("run_1.changeset.jsonl"),
            "{}",
        )
        .unwrap();

        let plan = plan_pr(&config, &run("blocked", temp_dir.path())).unwrap();

        assert!(plan.draft);
    }

    #[test]
    fn failed_runs_do_not_get_pr_by_default() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = config(temp_dir.path());

        assert!(matches!(
            plan_pr(&config, &run("failed", temp_dir.path())).unwrap_err(),
            PrError::OutcomeNotAllowed(_)
        ));
    }
}
