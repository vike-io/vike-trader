//! `store rename`: move one instrument's stored series under its new name, on the store's own box.
//!
//! The work is `vike_store::HistoryStoreDataFusion`'s `plan_rename` and `rename_series`, whose module
//! doc (`crates/vike-store/src/datafusion_hist/rename.rs`) is the contract: what survives, the crash
//! story and what a move costs. `vike-cli data history rename` spawns this arm and carries why it is an
//! operator verb run after the deploy rather than a deploy step.

use std::path::Path;
use std::process::ExitCode;

use vike_model::StoreKindId;
use vike_store::HistoryStoreDataFusion;
use vike_store::datafusion_hist::RenameStep;

use super::root::store_root_resolved;
use crate::datahub_cli::args::{StoreFlags, usage_text};

/// `store rename` — every per-symbol series of `--venue` stored under `--symbol` moves under `--to`.
///
/// Built like `run_rm_series`: the store and the rung that chose it LEAD on stdout, a mistyped root
/// is refused before the open creates one, the PLAN is printed before anything is written, and the
/// confirmation is `--yes` or a typed `rename N series` at a terminal — never a line read from a pipe.
/// Matching nothing is a SUCCESS that says so, because the deploy's runbook re-runs this blindly. One
/// series that fails is reported and the others still move; the exit is then non-zero and a re-run
/// finishes the job.
///
/// It opens the store with `open`, not `open_read_only`: the WAL recovery that runs there brings a
/// crashed append of either name into its manifest before a part is moved. The series lock is the
/// only arbiter against a running datahub, as for every verb here.
pub(super) fn run_rename_series(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&Path>,
    row: Option<String>,
    flags: &StoreFlags,
    source: Option<String>,
) -> ExitCode {
    use std::io::IsTerminal;

    let resolved = store_root_resolved(flags.store_path(), row, vars, cwd);
    let json = flags.json;
    if json {
        eprintln!("store: {resolved}");
    } else {
        println!("store: {resolved}");
    }
    let refuse = |why: String| {
        eprintln!("vike-datahub store rename: {why}");
        ExitCode::from(2)
    };

    let (Some(venue), Some(from), Some(to)) =
        (flags.venue.as_deref(), flags.symbol.as_deref(), flags.to.as_deref())
    else {
        return refuse(format!(
            "--venue, --symbol (the name stored now) and --to (the new name) are all REQUIRED\n\n{}",
            usage_text(&["store", "rename"])
        ));
    };
    if flags.group.is_some() {
        return refuse(
            "--group: a rename moves per-symbol series only. A grouped series holds many \
             instruments in one part, told apart by each row's own symbol, so renaming the group \
             would rename none of them"
                .to_string(),
        );
    }
    let kind = match flags.kind.as_deref().map(str::parse::<StoreKindId>).transpose() {
        Ok(kind) => kind,
        Err(e) => return refuse(e.to_string()),
    };
    // ⚠ BEFORE the open, which `create_dir_all`s its root — `run_rm_series`' rule.
    if !resolved.root.is_dir() {
        return refuse(format!(
            "no store at {}: nothing was created or renamed. Check the path, or name the store with \
             --store DIR: this verb reads no settings.",
            resolved.root.display()
        ));
    }
    let store = match HistoryStoreDataFusion::open(&resolved.root) {
        Ok(s) => s,
        Err(e) => {
            return refuse(format!(
                "cannot open the hist store at {}: {e}",
                resolved.root.display()
            ));
        }
    };
    let plan = match store.plan_rename(
        venue,
        from,
        to,
        kind,
        flags.interval.as_deref(),
        source.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => return refuse(e.to_string()),
    };
    // Under `--json` the plan goes to stderr, not nowhere: `run_rm_series`' rule.
    for step in &plan {
        if json {
            eprintln!("{}", step.line());
        } else {
            println!("{}", step.line());
        }
    }
    let document = |outcome: Option<&[serde_json::Value]>| {
        rename_json(&resolved, venue, from, to, &plan, outcome)
    };
    if plan.is_empty() {
        if json {
            println!("{}", document(Some(&[])));
        } else {
            println!("nothing to rename: no per-symbol series of {venue} is stored under {from:?}");
        }
        return ExitCode::SUCCESS;
    }
    if flags.dry_run {
        if json {
            println!("{}", document(None));
        } else {
            println!("--dry-run: nothing was renamed");
        }
        return ExitCode::SUCCESS;
    }
    if let Err(e) = confirm_rename(plan.len(), flags.yes, std::io::stdin().is_terminal()) {
        return refuse(e);
    }

    let mut outcomes = Vec::with_capacity(plan.len());
    let mut failed = 0usize;
    for step in &plan {
        match store.rename_series(&step.source, to) {
            Ok(done) => {
                if !json {
                    println!(
                        "renamed {}: {} parts moved ({} rows), {} already in the target",
                        step.line(),
                        done.parts_moved,
                        done.rows_moved,
                        done.parts_already_in_target
                    );
                }
                outcomes.push(serde_json::json!({ "series": step, "outcome": done }));
            }
            Err(e) => {
                failed += 1;
                eprintln!("FAILED {}: {e}", step.line());
                outcomes.push(serde_json::json!({ "series": step, "error": e.to_string() }));
            }
        }
    }
    if json {
        println!("{}", document(Some(&outcomes)));
    } else {
        println!("{} of {} series renamed", plan.len() - failed, plan.len());
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "vike-datahub store rename: {failed} of {} series could not be renamed (see above); \
             nothing of theirs was lost, and re-running finishes the job",
            plan.len()
        );
        ExitCode::from(2)
    }
}

/// The confirmation gate, PURE over its inputs so both branches are unit-tested — `run_rm_series`'
/// `confirm_removal` rule, bound to the number of series this plan renames.
fn confirm_rename(series: usize, yes: bool, stdin_is_terminal: bool) -> Result<(), String> {
    if yes {
        return Ok(());
    }
    if !stdin_is_terminal {
        return Err(format!(
            "refusing to rename {series} series without --yes: stdin is not a terminal, so there is \
             nobody to confirm. A confirmation read from a PIPE is not a confirmation."
        ));
    }
    let want = format!("rename {series} series");
    eprintln!("type `{want}` to confirm, or anything else to abort:");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| format!("reading the confirmation: {e}"))?;
    if line.trim() == want {
        Ok(())
    } else {
        Err(format!("not confirmed (expected `{want}`) — nothing was renamed"))
    }
}

/// The `--json` document: the RESOLVED store root and its rung (which `vike-cli data history rename
/// --json` nests rather than guesses), the request, the plan, and each series' outcome — `null` for a
/// dry run, empty when nothing matched.
fn rename_json(
    resolved: &vike_paths::store_path::StoreRoot,
    venue: &str,
    from: &str,
    to: &str,
    plan: &[RenameStep],
    outcome: Option<&[serde_json::Value]>,
) -> String {
    let doc = serde_json::json!({
        "store_root": resolved.root.display().to_string(),
        "store_rung": resolved.rung.as_str(),
        "store_rung_why": resolved.rung.why(),
        "venue": venue,
        "from": from,
        "to": to,
        "series": plan,
        "outcome": outcome,
    });
    serde_json::to_string_pretty(&doc).expect("a tree of plain data; serialization is total")
}

#[cfg(test)]
mod rename_series_tests {
    use super::*;

    /// `--yes` passes; no `--yes` and no terminal is a REFUSAL naming the count, never a read.
    #[test]
    fn yes_confirms_and_a_pipe_is_refused() {
        assert!(confirm_rename(3, true, false).is_ok());
        let refused = confirm_rename(3, false, false).unwrap_err();
        assert!(refused.contains("rename 3 series") && refused.contains("--yes"), "{refused}");
    }
}
