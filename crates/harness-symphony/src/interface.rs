use std::env;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use thiserror::Error;

use crate::config::{ConfigError, ResolvedConfig, SymphonyConfig};
use crate::doctor::{print_report, run_doctor, DoctorError};
use crate::pr::{create_pr, PrCreateResult, PrError};
use crate::retention::{compact_runs, CompactResult, RetentionError};
use crate::run::{execute_run, prepare_run, CompletedRun, PreparedRun, RunError};
use crate::state::{RunRecord, RunStateStore, StateError};
use crate::sync::{sync_changesets, unapplied_changesets, SyncError, SyncResult};
use crate::work::{list_work, WorkError, WorkItem};

#[derive(Parser, Debug)]
#[command(name = "harness-symphony")]
#[command(about = "local isolated runner for Harness stories", long_about = None)]
#[command(version)]
pub struct Cli {
    #[arg(long, global = true)]
    repo_root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Inspect Symphony readiness.
    Doctor,
    /// Discover runnable Harness work.
    Work(WorkArgs),
    /// Prepare or execute a story run.
    Run(RunArgs),
    /// Inspect local run state.
    Runs(RunsArgs),
    /// Show local Symphony status.
    Status,
    /// Apply committed Harness changesets to local harness.db.
    Sync,
    /// Create or inspect pull requests for run artifacts.
    Pr(PrArgs),
    /// Inspect resolved Symphony configuration.
    Config(ConfigArgs),
}

#[derive(Args, Debug)]
struct WorkArgs {
    #[command(subcommand)]
    action: WorkAction,
}

#[derive(Subcommand, Debug)]
enum WorkAction {
    /// List runnable Harness stories.
    List,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Harness story id.
    story_id: String,
    /// Prepare the isolated workspace and contract without launching an agent.
    #[arg(long)]
    prepare_only: bool,
}

#[derive(Args, Debug)]
struct RunsArgs {
    #[command(subcommand)]
    action: RunsAction,
}

#[derive(Subcommand, Debug)]
enum RunsAction {
    /// List local Symphony runs.
    List,
    /// Show one local Symphony run.
    Show { run_id: String },
    /// Compact old committed run artifacts.
    Compact {
        /// Number of newest run artifact directories to keep.
        #[arg(long)]
        keep_last: Option<u32>,
    },
}

#[derive(Args, Debug)]
struct PrArgs {
    #[command(subcommand)]
    action: PrAction,
}

#[derive(Subcommand, Debug)]
enum PrAction {
    /// Create a pull request for a finished run.
    Create {
        run_id: String,
        /// Print the PR plan without invoking a provider.
        #[arg(long)]
        dry_run: bool,
    },
    /// Retry pull request creation for a finished run.
    Retry {
        run_id: String,
        /// Print the PR plan without invoking a provider.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Args, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    action: ConfigAction,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Print the resolved config paths and adapter settings.
    Show,
}

#[derive(Debug, Error)]
pub enum InterfaceError {
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Doctor(#[from] DoctorError),
    #[error("{0}")]
    Work(#[from] WorkError),
    #[error("{0}")]
    State(#[from] StateError),
    #[error("{0}")]
    Run(#[from] RunError),
    #[error("{0}")]
    Sync(#[from] SyncError),
    #[error("{0}")]
    Retention(#[from] RetentionError),
    #[error("{0}")]
    Pr(#[from] PrError),
    #[error("could not determine current directory: {0}")]
    CurrentDir(std::io::Error),
}

pub fn run(cli: Cli) -> Result<(), InterfaceError> {
    let repo_root = match cli.repo_root {
        Some(path) => path,
        None => env::current_dir().map_err(InterfaceError::CurrentDir)?,
    };
    let config = SymphonyConfig::load(&repo_root)?;
    let resolved = config.resolve(&repo_root);

    match cli.command {
        Command::Config(args) => match args.action {
            ConfigAction::Show => print_config(&resolved),
        },
        Command::Doctor => {
            let report = run_doctor(&resolved)?;
            let has_failures = report.has_failures();
            print_report(&report);
            if has_failures {
                std::process::exit(1);
            }
        }
        Command::Work(args) => match args.action {
            WorkAction::List => print_work_items(&list_work(&resolved.harness_db)?),
        },
        Command::Run(args) => {
            if args.prepare_only {
                print_prepared_run(&prepare_run(&resolved, &args.story_id)?);
            } else {
                print_completed_run(&execute_run(&resolved, &args.story_id)?);
            }
        }
        Command::Runs(args) => match args.action {
            RunsAction::List => print_runs(&RunStateStore::new(resolved.state_db).list_runs()?),
            RunsAction::Show { run_id } => {
                print_run_detail(&RunStateStore::new(resolved.state_db).show_run(&run_id)?);
            }
            RunsAction::Compact { keep_last } => {
                print_compact_result(&compact_runs(
                    &resolved,
                    keep_last.unwrap_or(resolved.compact_keep_last),
                )?);
            }
        },
        Command::Pr(args) => match args.action {
            PrAction::Create { run_id, dry_run } | PrAction::Retry { run_id, dry_run } => {
                print_pr_result(&create_pr(&resolved, &run_id, dry_run)?);
            }
        },
        Command::Sync => print_sync_result(&sync_changesets(&resolved)?),
        Command::Status => print_status(
            &RunStateStore::new(resolved.state_db.clone()).active_run()?,
            &unapplied_changesets(&resolved)?,
        ),
    }

    Ok(())
}

fn print_work_items(items: &[WorkItem]) {
    let rows = items
        .iter()
        .map(|item| {
            vec![
                item.id.clone(),
                item.status.clone(),
                item.lane.clone(),
                item.verify.clone(),
                item.runnable.clone(),
                item.reason.clone(),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &["ID", "Status", "Lane", "Verify", "Runnable", "Reason"],
        &rows,
    );
}

fn print_config(config: &ResolvedConfig) {
    println!("version: {}", config.version);
    println!("repo_root: {}", config.repo_root.display());
    println!("harness_db: {}", config.harness_db.display());
    println!("state_db: {}", config.state_db.display());
    println!("runs_dir: {}", config.runs_dir.display());
    println!("worktrees_dir: {}", config.worktrees_dir.display());
    println!("single_active_run: {}", config.single_active_run);
    println!("agent_adapter: {}", config.agent_adapter);
    println!("agent_command: {}", config.agent_command.join(" "));
    println!("agent_timeout_minutes: {}", config.agent_timeout_minutes);
    println!("pull_request_create: {}", config.pull_request_create);
    println!("pull_request_provider: {}", config.pull_request_provider);
    println!(
        "pull_request_draft_for: {}",
        config.pull_request_draft_for.join(",")
    );
    println!(
        "changeset_directory: {}",
        config.changeset_directory.display()
    );
    println!(
        "changeset_render_in_summary: {}",
        config.changeset_render_in_summary
    );
    println!("allow_here_for_tiny: {}", config.allow_here_for_tiny);
    println!("compact_keep_last: {}", config.compact_keep_last);
    println!("keep_failed_worktrees: {}", config.keep_failed_worktrees);
    println!("cleanup_after_sync: {}", config.cleanup_after_sync);
}

fn print_prepared_run(run: &PreparedRun) {
    println!("Prepared run {}", run.run_id);
    println!("Story: {}", run.story_id);
    println!("Branch: {}", run.branch);
    println!("Worktree: {}", run.worktree.display());
    println!("Harness DB: {}", run.harness_db_path.display());
    println!("Contract: {}", run.contract_path.display());
    println!("Env:");
    println!("  HARNESS_DB_PATH={}", run.harness_db_path.display());
    println!("  HARNESS_RUN_ID={}", run.run_id);
    println!("  HARNESS_RUN_MODE=execute");
}

fn print_completed_run(run: &CompletedRun) {
    println!("Completed run {}", run.prepared.run_id);
    println!("Story: {}", run.prepared.story_id);
    println!("Outcome: {}", run.outcome);
    println!("Summary: {}", run.summary_path.display());
    println!("Result: {}", run.result_path.display());
}

fn print_runs(runs: &[RunRecord]) {
    let rows = runs
        .iter()
        .map(|run| {
            vec![
                run.run_id.clone(),
                run.story_id.clone(),
                run.branch.clone().unwrap_or_default(),
                run.worktree.display().to_string(),
                run.status.clone(),
                run.result_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                run.pr_url.clone().unwrap_or_default(),
                run.sync_status.clone(),
                run.next_action.clone(),
            ]
        })
        .collect::<Vec<_>>();
    print_table(
        &[
            "Run", "Story", "Branch", "Worktree", "Status", "Result", "PR", "Sync", "Next",
        ],
        &rows,
    );
}

fn print_run_detail(run: &RunRecord) {
    println!("run_id: {}", run.run_id);
    println!("story_id: {}", run.story_id);
    println!("branch: {}", run.branch.clone().unwrap_or_default());
    println!("worktree: {}", run.worktree.display());
    println!("status: {}", run.status);
    println!(
        "result_path: {}",
        run.result_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
    );
    println!("pr_url: {}", run.pr_url.clone().unwrap_or_default());
    println!("sync_status: {}", run.sync_status);
    println!("next_action: {}", run.next_action);
}

fn print_status(active: &Option<RunRecord>, unapplied_changesets: &[PathBuf]) {
    if let Some(run) = active {
        println!("Active run: {} ({})", run.run_id, run.status);
        println!("Story: {}", run.story_id);
        println!("Next: {}", run.next_action);
    } else {
        println!("No active Symphony run.");
    }
    if unapplied_changesets.is_empty() {
        println!("Changesets: all applied locally.");
    } else {
        println!(
            "Changesets: {} committed changeset(s) are unapplied locally.",
            unapplied_changesets.len()
        );
        println!("Next: harness-symphony sync");
    }
}

fn print_sync_result(result: &SyncResult) {
    let applied = result
        .changes
        .iter()
        .filter(|change| change.applied)
        .count();
    let skipped = result.changes.len().saturating_sub(applied);
    println!(
        "Sync complete: {applied} applied, {skipped} skipped, {} total.",
        result.changes.len()
    );
    for change in &result.changes {
        let status = if change.applied { "applied" } else { "skipped" };
        println!(
            "{} {} ({} operation(s))",
            change.id, status, change.operations
        );
    }
}

fn print_compact_result(result: &CompactResult) {
    println!(
        "Compaction complete: {} kept, {} removed.",
        result.kept.len(),
        result.removed.len()
    );
    for path in &result.removed {
        println!("removed {}", path.display());
    }
}

fn print_pr_result(result: &PrCreateResult) {
    if let Some(url) = &result.url {
        println!("PR created: {url}");
    } else {
        println!("PR dry run for {}", result.plan.run_id);
    }
    println!("Title: {}", result.plan.title);
    println!("Draft: {}", result.plan.draft);
    println!("Body: {}", result.plan.body_path.display());
    for file in &result.plan.files {
        println!("Artifact: {}", file.display());
    }
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.len());
        }
    }

    print_row(headers, &widths);
    let separator = widths
        .iter()
        .map(|width| "-".repeat(*width))
        .collect::<Vec<_>>();
    let separator_refs = separator.iter().map(String::as_str).collect::<Vec<_>>();
    print_row(&separator_refs, &widths);
    for row in rows {
        let cells = row.iter().map(String::as_str).collect::<Vec<_>>();
        print_row(&cells, &widths);
    }
}

fn print_row(values: &[&str], widths: &[usize]) {
    for (index, value) in values.iter().enumerate() {
        let width = widths[index] + 2;
        print!("{value:<width$}");
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_exposes_e05_top_level_commands() {
        let help = Cli::command().render_long_help().to_string();

        assert!(help.contains("doctor"));
        assert!(help.contains("work"));
        assert!(help.contains("run"));
        assert!(help.contains("runs"));
        assert!(help.contains("status"));
        assert!(help.contains("sync"));
        assert!(help.contains("pr"));
        assert!(help.contains("config"));
    }
}
