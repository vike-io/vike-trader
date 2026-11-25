//! `vike-cli backtest prune --dry-run` — the saved runs the saved-run retention (`config.saved_runs_per_strategy`)
//! would remove, listed, and nothing removed.
//!
//! The rule is `vike_runs::RunRetention`'s, the one the backtesthub applies after each run it saves, over the same
//! runs root, the marks beside it and the alert rules, so the list is what its next save removes (apart from what
//! that new run itself pushes over the count). `--dry-run` is REQUIRED (`crate::cmd::backtest::read`'s
//! `ReadCommand::into_args`): this verb deletes nothing, and the daemon is the one remover. Without the row the rule
//! is off and nothing is listed.

use crate::cmd::backtest::read::Context;
use crate::cmd::backtest::read::ReadArgs;
use crate::exit::{CliError, CommandResult};

/// List what the retention would remove, on stdout (`--json`: one document); scan problems go to stderr.
pub(crate) fn run_prune(ctx: &Context<'_>, a: &ReadArgs) -> CommandResult<()> {
    let Some(retention) = ctx.retention else {
        if a.json {
            print_json(&serde_json::json!({ "schema": 1, "keep_per_strategy": null, "runs": [] }));
        } else {
            println!(
                "config.saved_runs_per_strategy is not set: the saved-run retention is off and the \
                 backtesthub removes no saved run"
            );
        }
        return Ok(());
    };
    let root = ctx.runs_root.ok_or_else(|| {
        CliError::failed(
            "no project directory above the working directory, so there is no runs directory. \
             `vike-cli init` creates one, or name it with VIKE_USER_DATA_DIR.",
        )
    })?;
    let scan = vike_runs::scan_runs(root);
    for problem in &scan.diagnostics {
        eprintln!("vike-cli backtest prune: {problem}");
    }
    let runs = retention.runs_to_prune(&scan).map_err(CliError::failed)?;
    let strategy_file = |run: &vike_runs::ScannedRun| -> String {
        run.manifest.strategy_file().unwrap_or_default().to_string()
    };
    if a.json {
        let rows: Vec<serde_json::Value> = runs
            .iter()
            .map(|run| {
                serde_json::json!({
                    "run_id": run.run_id,
                    "strategy_file": strategy_file(run),
                    "finished_at": run.manifest.finished_at,
                    "dir": run.dir.display().to_string(),
                })
            })
            .collect();
        print_json(&serde_json::json!({
            "schema": 1,
            "keep_per_strategy": retention.keep_per_strategy,
            "runs": rows,
        }));
        return Ok(());
    }
    println!(
        "config.saved_runs_per_strategy = {}: {} saved run(s) would be removed (--dry-run: none was)",
        retention.keep_per_strategy,
        runs.len()
    );
    for run in &runs {
        println!("  {}  {}  {}", run.run_id, run.manifest.finished_at, strategy_file(run));
    }
    Ok(())
}

fn print_json(doc: &serde_json::Value) {
    match serde_json::to_string_pretty(doc) {
        Ok(text) => println!("{text}"),
        // A document that cannot be serialized is a bug here, not the user's: say so rather than print half of one.
        Err(e) => eprintln!("vike-cli backtest prune: cannot render the JSON document: {e}"),
    }
}
