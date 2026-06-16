use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::changeset::{append_rendered_section, ChangesetError};
use crate::config::ResolvedConfig;
use crate::state::{NewRunRecord, RunStateStore, StateError};

#[derive(Debug, Error)]
pub enum RunError {
    #[error("story {0} not found in harness database")]
    StoryNotFound(String),
    #[error("story {id} is not runnable because status is {status}; only planned or in_progress can be prepared")]
    StoryNotRunnable { id: String, status: String },
    #[error("harness database not found at {0}. Run: scripts/bin/harness-cli init")]
    MissingDatabase(String),
    #[error("git worktree failed: {0}")]
    GitWorktree(String),
    #[error("agent.command is not configured. Set agent.command in .harness/symphony.yml.")]
    MissingAgentCommand,
    #[error("agent command failed with status {status}: {stderr}")]
    AgentCommandFailed { status: String, stderr: String },
    #[error("run result validation failed: {0}")]
    InvalidResult(String),
    #[error("{0}")]
    State(#[from] StateError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Changeset(#[from] ChangesetError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRun {
    pub run_id: String,
    pub story_id: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub contract_path: PathBuf,
    pub harness_db_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedRun {
    pub prepared: PreparedRun,
    pub outcome: String,
    pub summary_path: PathBuf,
    pub result_path: PathBuf,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct RunContract {
    pub version: u32,
    pub run_id: String,
    pub mode: String,
    pub story_id: String,
    pub worktree: String,
    pub harness_db_path: String,
    pub env: RunEnvironment,
    pub required_outputs: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub agent_instructions: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct RunEnvironment {
    pub harness_db_path: String,
    pub harness_run_id: String,
    pub harness_run_mode: String,
}

#[derive(Debug, Deserialize)]
struct ResultFile {
    version: u32,
    run_id: String,
    story_id: String,
    outcome: String,
    validation: Option<ResultValidation>,
    summary_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResultValidation {
    commands: Option<Vec<ValidationCommand>>,
    unavailable: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ValidationCommand {
    command: String,
    result: String,
}

pub fn prepare_run(config: &ResolvedConfig, story_id: &str) -> Result<PreparedRun, RunError> {
    let story = load_story(&config.harness_db, story_id)?;
    if !matches!(story.status.as_str(), "planned" | "in_progress") {
        return Err(RunError::StoryNotRunnable {
            id: story.id,
            status: story.status,
        });
    }

    let run_id = generate_run_id();
    let branch = format!("symphony/{run_id}");
    let worktree = config.worktrees_dir.join(&run_id);
    let run_dir = config.runs_dir.join(&run_id);
    let contract_path = run_dir.join("RUN_CONTRACT.json");
    let harness_db_path = worktree.join("harness.db");

    fs::create_dir_all(&config.worktrees_dir)?;
    fs::create_dir_all(&run_dir)?;
    create_worktree(&config.repo_root, &branch, &worktree)?;
    fs::copy(&config.harness_db, &harness_db_path)?;

    let contract = build_contract(config, &run_id, story_id, &worktree, &harness_db_path);
    write_contract(&contract_path, &contract)?;
    write_agents_shim(&worktree.join("AGENTS.md"), &contract_path, &contract)?;

    RunStateStore::new(config.state_db.clone()).add_run(NewRunRecord {
        run_id: run_id.clone(),
        story_id: story_id.to_owned(),
        branch: Some(branch.clone()),
        worktree: worktree.clone(),
        status: "prepared".to_owned(),
        result_path: Some(PathBuf::from(format!(".harness/runs/{run_id}/RESULT.json"))),
        sync_status: "not_applied".to_owned(),
        next_action: format!("Launch agent for {story_id} or inspect {contract_path:?}"),
    })?;

    Ok(PreparedRun {
        run_id,
        story_id: story_id.to_owned(),
        branch,
        worktree,
        contract_path,
        harness_db_path,
    })
}

pub fn execute_run(config: &ResolvedConfig, story_id: &str) -> Result<CompletedRun, RunError> {
    if config.agent_adapter != "custom" {
        return Err(RunError::InvalidResult(format!(
            "unsupported agent adapter '{}'",
            config.agent_adapter
        )));
    }
    if config.agent_command.is_empty() {
        return Err(RunError::MissingAgentCommand);
    }

    let prepared = prepare_run(config, story_id)?;
    let output = Command::new(&config.agent_command[0])
        .args(&config.agent_command[1..])
        .current_dir(&prepared.worktree)
        .env("HARNESS_DB_PATH", &prepared.harness_db_path)
        .env("HARNESS_RUN_ID", &prepared.run_id)
        .env("HARNESS_RUN_MODE", "execute")
        .output()?;
    if !output.status.success() {
        RunStateStore::new(config.state_db.clone()).update_status(
            &prepared.run_id,
            "failed",
            "inspect agent command failure",
        )?;
        return Err(RunError::AgentCommandFailed {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    let completed = validate_finished_run(config, prepared)?;
    RunStateStore::new(config.state_db.clone()).update_status(
        &completed.prepared.run_id,
        &completed.outcome,
        "review run result",
    )?;
    Ok(completed)
}

fn load_story(db_path: &Path, story_id: &str) -> Result<Story, RunError> {
    if !db_path.exists() {
        return Err(RunError::MissingDatabase(db_path.display().to_string()));
    }
    let connection = Connection::open(db_path)?;
    connection
        .query_row(
            "SELECT id, status FROM story WHERE id=?1;",
            params![story_id],
            |row| {
                Ok(Story {
                    id: row.get(0)?,
                    status: row.get(1)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| RunError::StoryNotFound(story_id.to_owned()))
}

fn create_worktree(repo_root: &Path, branch: &str, worktree: &Path) -> Result<(), RunError> {
    let output = Command::new("git")
        .args(["worktree", "add", "-b", branch])
        .arg(worktree)
        .arg("HEAD")
        .current_dir(repo_root)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(RunError::GitWorktree(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn build_contract(
    config: &ResolvedConfig,
    run_id: &str,
    story_id: &str,
    worktree: &Path,
    harness_db_path: &Path,
) -> RunContract {
    let required_outputs = vec![
        format!(".harness/runs/{run_id}/SUMMARY.md"),
        format!(".harness/runs/{run_id}/RESULT.json"),
    ];
    let forbidden_paths = vec![
        "harness.db".to_owned(),
        ".symphony/state.db".to_owned(),
        ".symphony/worktrees/**".to_owned(),
    ];
    RunContract {
        version: 1,
        run_id: run_id.to_owned(),
        mode: "execute".to_owned(),
        story_id: story_id.to_owned(),
        worktree: display_path(config, worktree),
        harness_db_path: display_path(config, harness_db_path),
        env: RunEnvironment {
            harness_db_path: display_path(config, harness_db_path),
            harness_run_id: run_id.to_owned(),
            harness_run_mode: "execute".to_owned(),
        },
        required_outputs,
        forbidden_paths,
        agent_instructions: vec![
            "Follow AGENTS.md and Harness docs.".to_owned(),
            "Implement only the assigned story scope.".to_owned(),
            "Use the copied harness.db.".to_owned(),
            "Run the configured verification command when available.".to_owned(),
        ],
    }
}

fn write_contract(path: &Path, contract: &RunContract) -> Result<(), RunError> {
    let text = serde_json::to_string_pretty(contract)?;
    fs::write(path, format!("{text}\n"))?;
    Ok(())
}

fn write_agents_shim(
    path: &Path,
    contract_path: &Path,
    contract: &RunContract,
) -> Result<(), RunError> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let block = render_agents_shim(contract_path, contract);
    fs::write(path, format!("{existing}\n{block}"))?;
    Ok(())
}

fn validate_finished_run(
    config: &ResolvedConfig,
    prepared: PreparedRun,
) -> Result<CompletedRun, RunError> {
    let summary_path = prepared
        .worktree
        .join(format!(".harness/runs/{}/SUMMARY.md", prepared.run_id));
    let result_path = prepared
        .worktree
        .join(format!(".harness/runs/{}/RESULT.json", prepared.run_id));

    if !summary_path.exists() {
        return Err(RunError::InvalidResult(format!(
            "SUMMARY.md missing at {}",
            summary_path.display()
        )));
    }
    if !result_path.exists() {
        return Err(RunError::InvalidResult(format!(
            "RESULT.json missing at {}",
            result_path.display()
        )));
    }

    let result = parse_result_file(&result_path)?;
    if result.version != 1 {
        return Err(RunError::InvalidResult(
            "RESULT.json version must be 1".to_owned(),
        ));
    }
    if result.run_id != prepared.run_id {
        return Err(RunError::InvalidResult(
            "RESULT.json run_id mismatch".to_owned(),
        ));
    }
    if result.story_id != prepared.story_id {
        return Err(RunError::InvalidResult(
            "RESULT.json story_id mismatch".to_owned(),
        ));
    }
    if !valid_outcome(&result.outcome) {
        return Err(RunError::InvalidResult(format!(
            "invalid outcome '{}'",
            result.outcome
        )));
    }
    if !has_validation_evidence(result.validation.as_ref()) {
        return Err(RunError::InvalidResult(
            "validation evidence missing or unavailable reason absent".to_owned(),
        ));
    }
    if let Some(summary) = result.summary_path.as_deref() {
        if summary.trim().is_empty() {
            return Err(RunError::InvalidResult(
                "summary_path must not be empty".to_owned(),
            ));
        }
    }
    ensure_forbidden_paths_not_staged(config, &prepared.worktree)?;
    if config.changeset_render_in_summary {
        let changeset_path = prepared.worktree.join(format!(
            ".harness/changesets/{}.changeset.jsonl",
            prepared.run_id
        ));
        if changeset_path.exists() {
            append_rendered_section(
                &summary_path,
                &changeset_path,
                &format!(".harness/changesets/{}.changeset.jsonl", prepared.run_id),
            )?;
        }
    }

    Ok(CompletedRun {
        prepared,
        outcome: result.outcome,
        summary_path,
        result_path,
    })
}

fn parse_result_file(path: &Path) -> Result<ResultFile, RunError> {
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(RunError::Json)
}

fn valid_outcome(value: &str) -> bool {
    matches!(
        value,
        "completed" | "blocked" | "needs_intake" | "partial" | "failed" | "cancelled"
    )
}

fn has_validation_evidence(validation: Option<&ResultValidation>) -> bool {
    let Some(validation) = validation else {
        return false;
    };
    if validation
        .commands
        .as_ref()
        .is_some_and(|commands| !commands.is_empty() && commands.iter().all(valid_command))
    {
        return true;
    }
    validation
        .unavailable
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

fn valid_command(command: &ValidationCommand) -> bool {
    !command.command.trim().is_empty()
        && matches!(command.result.as_str(), "pass" | "fail" | "unavailable")
}

fn ensure_forbidden_paths_not_staged(
    _config: &ResolvedConfig,
    worktree: &Path,
) -> Result<(), RunError> {
    let output = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(worktree)
        .output()?;
    if !output.status.success() {
        return Err(RunError::GitWorktree(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let forbidden = ["harness.db", ".symphony/state.db"];
    let staged = String::from_utf8_lossy(&output.stdout);
    for path in staged
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if forbidden.contains(&path) || path.starts_with(".symphony/worktrees/") {
            return Err(RunError::InvalidResult(format!(
                "forbidden path staged for commit: {path}"
            )));
        }
    }
    Ok(())
}

fn render_agents_shim(contract_path: &Path, contract: &RunContract) -> String {
    format!(
        "<!-- HARNESS-SYMPHONY:BEGIN -->\n\
## Harness Symphony Run\n\n\
- Story: `{}`\n\
- Contract: `{}`\n\
- Harness DB: `{}`\n\
- Required outputs: `{}` and `{}`\n\
- Forbidden paths: `{}`\n\
\n\
Use `HARNESS_DB_PATH={}`, `HARNESS_RUN_ID={}`, and `HARNESS_RUN_MODE=execute` for Harness CLI writes.\n\
<!-- HARNESS-SYMPHONY:END -->\n",
        contract.story_id,
        contract_path.display(),
        contract.harness_db_path,
        contract.required_outputs[0],
        contract.required_outputs[1],
        contract.forbidden_paths.join("`, `"),
        contract.env.harness_db_path,
        contract.env.harness_run_id,
    )
}

fn display_path(config: &ResolvedConfig, path: &Path) -> String {
    path.strip_prefix(&config.repo_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn generate_run_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("run_{}_{}", timestamp, std::process::id())
}

#[derive(Debug)]
struct Story {
    id: String,
    status: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;

    fn config() -> ResolvedConfig {
        ResolvedConfig {
            version: 1,
            repo_root: PathBuf::from("/repo"),
            harness_db: PathBuf::from("/repo/harness.db"),
            state_db: PathBuf::from("/repo/.symphony/state.db"),
            runs_dir: PathBuf::from("/repo/.harness/runs"),
            worktrees_dir: PathBuf::from("/repo/.symphony/worktrees"),
            single_active_run: true,
            agent_adapter: "custom".to_owned(),
            agent_command: vec![],
            agent_timeout_minutes: 120,
            pull_request_create: "ask".to_owned(),
            pull_request_provider: "github".to_owned(),
            pull_request_draft_for: vec![],
            changeset_directory: PathBuf::from("/repo/.harness/changesets"),
            changeset_render_in_summary: true,
            allow_here_for_tiny: true,
            compact_keep_last: 50,
            keep_failed_worktrees: true,
            cleanup_after_sync: false,
        }
    }

    #[test]
    fn contract_contains_required_run_fields() {
        let config = config();
        let contract = build_contract(
            &config,
            "run_1",
            "US-036",
            Path::new("/repo/.symphony/worktrees/run_1"),
            Path::new("/repo/.symphony/worktrees/run_1/harness.db"),
        );

        assert_eq!(contract.version, 1);
        assert_eq!(contract.mode, "execute");
        assert_eq!(contract.story_id, "US-036");
        assert_eq!(contract.worktree, ".symphony/worktrees/run_1");
        assert_eq!(
            contract.harness_db_path,
            ".symphony/worktrees/run_1/harness.db"
        );
        assert_eq!(contract.env.harness_run_id, "run_1");
        assert_eq!(contract.env.harness_run_mode, "execute");
        assert!(contract
            .required_outputs
            .contains(&".harness/runs/run_1/RESULT.json".to_owned()));
        assert!(contract.forbidden_paths.contains(&"harness.db".to_owned()));
    }

    #[test]
    fn agents_shim_names_contract_and_boundaries() {
        let config = config();
        let contract = build_contract(
            &config,
            "run_1",
            "US-037",
            Path::new("/repo/.symphony/worktrees/run_1"),
            Path::new("/repo/.symphony/worktrees/run_1/harness.db"),
        );
        let shim = render_agents_shim(
            Path::new("/repo/.harness/runs/run_1/RUN_CONTRACT.json"),
            &contract,
        );

        assert!(shim.contains("HARNESS-SYMPHONY:BEGIN"));
        assert!(shim.contains("US-037"));
        assert!(shim.contains("RUN_CONTRACT.json"));
        assert!(shim.contains("HARNESS_DB_PATH=.symphony/worktrees/run_1/harness.db"));
        assert!(shim.contains("Forbidden paths"));
    }

    #[test]
    fn outcome_and_validation_rules_match_finish_protocol() {
        assert!(valid_outcome("completed"));
        assert!(valid_outcome("blocked"));
        assert!(valid_outcome("needs_intake"));
        assert!(valid_outcome("partial"));
        assert!(valid_outcome("failed"));
        assert!(valid_outcome("cancelled"));
        assert!(!valid_outcome("done"));

        let commands = ResultValidation {
            commands: Some(vec![ValidationCommand {
                command: "cargo test".to_owned(),
                result: "pass".to_owned(),
            }]),
            unavailable: None,
        };
        assert!(has_validation_evidence(Some(&commands)));

        let unavailable = ResultValidation {
            commands: None,
            unavailable: Some("manual validation not available in fixture".to_owned()),
        };
        assert!(has_validation_evidence(Some(&unavailable)));

        let missing = ResultValidation {
            commands: Some(Vec::new()),
            unavailable: None,
        };
        assert!(!has_validation_evidence(Some(&missing)));
    }

    #[test]
    fn parse_result_rejects_invalid_json() {
        let temp_dir = tempfile::tempdir().unwrap();
        let result_path = temp_dir.path().join("RESULT.json");
        fs::write(&result_path, "{").unwrap();

        assert!(parse_result_file(&result_path).is_err());
    }
}
