//! `backtest trials <id>`: the reading verb over a finished (or interrupted) search's ledger.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_analytics::binutil::has_flag;
use vike_model::runs;

use super::usage::TRIALS_USAGE;
use super::{render_trials, required_value};
use crate::harness;
use crate::trial_ledger;

/// Which field `--sort` ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrialSort {
    Score,
    N,
    Return,
    Sharpe,
    MaxDd,
    Trades,
    Equity,
}

impl TrialSort {
    /// ONE table: `(flag spelling, the variant, the `metrics` key it reads, bigger-is-better)`. The
    /// key is `None` for the two fields that come off the record itself rather than out of its
    /// metrics. Driving the name, the parse and the metric lookup from one row is what stops a
    /// seventh field being added to two of the three and forgotten in the third.
    ///
    /// ⚠ The metric keys are `vike_analytics::report::BacktestReport`'s SERIALIZED field names, because that is
    /// what `crate::trial_ledger::TrialRecord::metrics` holds — `serde_json::to_value` of a real
    /// report.
    pub(super) const ROWS: &'static [(&'static str, TrialSort, Option<&'static str>, bool)] = &[
        ("score", TrialSort::Score, None, true),
        ("n", TrialSort::N, None, false),
        ("return", TrialSort::Return, Some("total_return"), true),
        ("sharpe", TrialSort::Sharpe, Some("sharpe"), true),
        ("max_dd", TrialSort::MaxDd, Some("max_drawdown"), false),
        ("trades", TrialSort::Trades, Some("n_trades"), true),
        ("equity", TrialSort::Equity, Some("final_equity"), true),
    ];

    pub(super) fn names() -> String {
        Self::ROWS.iter().map(|(n, ..)| *n).collect::<Vec<_>>().join("|")
    }

    fn from_name(s: &str) -> Option<Self> {
        let lower = s.to_ascii_lowercase();
        Self::ROWS.iter().find(|(n, ..)| *n == lower).map(|(_, v, _, _)| *v)
    }

    /// The metric key and direction, or `None` for `score`/`n`.
    pub(super) fn metric(self) -> Option<(&'static str, bool)> {
        Self::ROWS
            .iter()
            .find(|(_, v, _, _)| *v == self)
            .and_then(|(_, _, k, d)| k.map(|k| (k, *d)))
    }
}

pub(super) fn parse_trial_sort(args: &[String]) -> Result<TrialSort, String> {
    match required_value(args, "--sort", &format!("expected {}", TrialSort::names()))?.as_deref() {
        None => Ok(TrialSort::Score),
        Some(v) => TrialSort::from_name(v)
            .ok_or_else(|| format!("invalid --sort {v:?} (expected {})", TrialSort::names())),
    }
}

/// Order a document's trials for DISPLAY, and truncate.
///
/// ⚠ The persisted document is always in `n` order; this reorders a COPY. Unrankable rows (a `NaN`
/// score, or a missing metric) sort LAST under every field, which is the same rule
/// `harness::cmp_scores_desc` applies to a sweep report: a failed point must never lead a table.
/// `sort_by` is STABLE, so ties keep evaluation order — the property the whole search path already
/// relies on.
pub(super) fn sort_trials(
    doc: &trial_ledger::TrialsDocument,
    sort: TrialSort,
    top: Option<usize>,
) -> Vec<trial_ledger::TrialRecord> {
    let mut rows = doc.trials.clone();
    match sort {
        TrialSort::N => rows.sort_by_key(|t| t.n),
        TrialSort::Score => rows.sort_by(|a, b| harness::cmp_scores_desc(a.score, b.score)),
        other => {
            let (key, bigger_is_better) = other.metric().expect("every other variant has a metric");
            rows.sort_by(|a, b| {
                let get = |t: &trial_ledger::TrialRecord| {
                    t.metrics.as_ref().and_then(|m| m.get(key)).and_then(|v| v.as_f64())
                };
                match (get(a), get(b)) {
                    (Some(x), Some(y)) => {
                        let ord = harness::cmp_scores_desc(x, y);
                        if bigger_is_better { ord } else { ord.reverse() }
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            });
        }
    }
    if let Some(n) = top {
        rows.truncate(n);
    }
    rows
}

/// `<project>/user_data/runs/<id>`, or a message saying which of the two things was missing.
///
/// ⚠ `user_runs_dir_from` is the WALK, and this binary may call it:
/// `crates/vike-boot/tests/one_owner.rs`'s `a_crate_that_boots_may_not_walk_again` forbids that call
/// only in a crate that calls `vike_boot::boot`, and this one does not. `vike-cli` DOES, so an
/// operator-facing verb built over this one must derive its root from `Resolved::user_data_dir`
/// joined with `RUNS_SUBDIR` instead.
fn locate_run(
    vars: &std::collections::HashMap<String, String>,
    id: &str,
) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let runs_root = vike_model::paths::state_path::user_runs_dir_from(
        vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
        &cwd,
    )
    .ok_or_else(|| {
        format!(
            "no project directory above {} — a run lives under <project>/user_data/runs/, and \
             this working directory has no project marker above it",
            cwd.display()
        )
    })?;
    let dir = runs_root.join(id);
    if dir.is_dir() { Ok(dir) } else { Err(format!("no run {id:?} under {}", runs_root.display())) }
}

/// The run's `TrialsDocument`, refusing a run that is not a search.
///
/// Prefers the persisted `runs::REPORT_FILE` and falls back to rebuilding from the ledger, which is
/// what an INTERRUPTED search has: its manifest and report were never written, but `search.json`
/// and however many ledger lines completed are on disk. A verb that could only read a finished
/// search would be useless on exactly the run an operator most wants to look at.
fn load_trials_document(dir: &Path, id: &str) -> Result<trial_ledger::TrialsDocument, String> {
    if let Ok(manifest) = runs::read_manifest(dir) {
        if manifest.kind != trial_ledger::SEARCH_RUN_KIND {
            return Err(format!(
                "run {id:?} is a {:?} run, not a {:?} — `trials` lists the child trials of a \
                 parameter SEARCH, and a run of another kind has none",
                manifest.kind,
                trial_ledger::SEARCH_RUN_KIND
            ));
        }
        let path = dir.join(runs::REPORT_FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            return serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()));
        }
    }
    // No manifest (or no report): an unfinished search. `search.json` is written FIRST precisely so
    // this case has an identity to report.
    let header = trial_ledger::read_search_header(dir).map_err(|e| {
        format!("run {id:?} carries no search header — {e}. It is not a parameter search")
    })?;
    let read = match trial_ledger::read_trials(dir) {
        Ok(read) => read,
        Err(runs::RunReadError::Missing { .. }) => trial_ledger::TrialsRead::default(),
        Err(e) => return Err(e.to_string()),
    };
    let unreadable = read.unreadable.len();
    let (trials, superseded) = trial_ledger::latest_by_n(read.trials);
    let failed = trials.iter().filter(|t| t.error.is_some()).count();
    Ok(trial_ledger::TrialsDocument {
        schema: header.schema,
        run_id: header.run_id,
        keep_trials: header.keep_trials,
        identity: header.identity,
        // An unfinished search has no tally of its own; the ledger's length is what is known.
        evaluated: trials.len(),
        reused: 0,
        failed,
        unreadable,
        superseded,
        trials,
        // ⚠ **`None`, and it cannot be otherwise.** The statistics are computed from the in-memory
        // return matrix of the process that SEARCHED, and the vectors are deliberately not
        // persisted (`crate::trial_ledger::OverfitStats`' doc carries that decision and what it
        // costs) — so a run whose `report.json` is missing has no recoverable matrix. Synthesising
        // a block here from the ledger's scalars would be a different statistic wearing the same
        // key names.
        overfit: None,
    })
}

/// Write the top-sorted trial's overrides as a `[strategy.params]` TOML fragment.
///
/// ⚠ A sweep override's key is a BARE param name — `harness::sweep::profile_with_overrides` inserts
/// each one straight into `profile.strategy.params` — so the fragment is that table and nothing
/// else. `toml::Value`'s `Display` already renders TOML syntax for every value a sweep axis can
/// declare, which is the same rendering the sweep report's own table prints.
fn export_params(
    doc: &trial_ledger::TrialsDocument,
    rows: &[trial_ledger::TrialRecord],
    path: &str,
) -> Result<(), String> {
    let best = rows.first().ok_or_else(|| {
        format!(
            "this search recorded no trials (keep-trials = {}), so there are no params to export",
            doc.keep_trials
        )
    })?;
    let score = if best.score.is_finite() {
        format!("{:.6}", best.score)
    } else {
        "unrankable".to_string()
    };
    let mut out = format!(
        "# exported by `backtest trials {} --export-params`\n# trial #{} of {}, score {}\n\
         [strategy.params]\n",
        doc.run_id,
        best.n,
        doc.trials.len(),
        score
    );
    for (k, v) in &best.overrides {
        out.push_str(&format!("{k} = {v}\n"));
    }
    std::fs::write(path, out).map_err(|e| format!("cannot write {path}: {e}"))
}

/// Run `backtest trials <id>`.
///
/// ⚠ **ARTIFACT-ONLY**: no store, no profile, no socket — §6 of the CLI-surface design requires the
/// reading verbs to work "on a laptop with neither, on runs minted months ago".
pub(crate) fn run_trials(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
) -> ExitCode {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("{TRIALS_USAGE}");
        return ExitCode::SUCCESS;
    }
    let Some(id) = args.first().filter(|a| !a.starts_with('-')) else {
        eprintln!("backtest: `trials` needs a search run id\n\n{TRIALS_USAGE}");
        return ExitCode::from(2);
    };
    // ⚠ The design's SELECTOR GRAMMAR (`@last`, a unique prefix, `@baseline/NAME`, `<id>#<n>`) is
    // ONE implementation shared by every verb that takes a run, and it is not this binary's to
    // invent half of. Refused by NAME so an operator learns what this verb takes, rather than
    // meeting a "no such directory" about a path they never typed.
    if id.starts_with('@') || !id.contains('-') {
        eprintln!(
            "backtest: {id:?} is not a full run id. This verb takes the FULL id — the one \
             `backtest: search saved to …` printed, and the name of the directory under \
             <project>/user_data/runs/. Prefixes, `@last` and marks are the CLI's shared selector \
             grammar and are not implemented here"
        );
        return ExitCode::from(2);
    }

    let sort = match parse_trial_sort(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };
    let top = match required_value(args, "--top", "expected a positive count") {
        Ok(None) => None,
        Ok(Some(v)) => match v.parse::<usize>() {
            Ok(n) if n > 0 => Some(n),
            _ => {
                eprintln!("backtest: invalid --top {v:?} (expected a positive count)");
                return ExitCode::from(2);
            }
        },
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };
    let export = match required_value(args, "--export-params", "expected an output file path") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };

    let dir = match locate_run(vars, id) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };
    let doc = match load_trials_document(&dir, id) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };

    let rows = sort_trials(&doc, sort, top);

    if let Some(path) = export.as_deref() {
        if let Err(why) = export_params(&doc, &rows, path) {
            eprintln!("backtest: --export-params failed — {why}");
            return ExitCode::FAILURE;
        }
        eprintln!("backtest: params written to {path}");
    }

    if has_flag(args, "--json") {
        // The SAME type `report.json` holds, with `trials` in the requested order — the array order
        // IS the rank, which is why no `rank` key is invented. Every diagnostic goes to stderr, so
        // stdout stays a pure document.
        let shown = trial_ledger::TrialsDocument { trials: rows, ..doc };
        match serde_json::to_string_pretty(&shown) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("backtest: failed to serialize the trials document: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{}", render_trials(&doc, &rows));
    }
    ExitCode::SUCCESS
}
