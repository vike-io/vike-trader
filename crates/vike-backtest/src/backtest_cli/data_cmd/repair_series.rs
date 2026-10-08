//! `backtest data repair`: rebuild ONE series' manifest from its parts, rehearsing by default.

use std::path::PathBuf;
use std::process::ExitCode;

use vike_analytics::binutil::{arg, has_flag};
use vike_data::DataFusionHist;

use crate::backtest_cli::usage::DATA_USAGE;

// ─── `data repair`: giving the manifest rebuild an operator ────────────────────────────────────

/// `backtest data repair` — rebuild ONE series' manifest from its parts.
///
/// # Why this verb exists at all
///
/// `crates/vike-data/src/store/datafusion_hist/rebuild.rs`'s `DataFusionHist::rebuild_series_manifest` has been
/// the repair for a lost or unreadable index since the manifest became a cache rather than ground
/// truth, and until this arm **no binary called it** — `git grep` found tests and doc comments and
/// nothing else. `crates/vike-data/src/store/datafusion_hist/manifest.rs`'s `read_manifest` names it in
/// the error an operator actually reads, so the failure message prescribed a cure no command
/// dispensed. The only move an operator had was deleting `_manifest.delta` by hand, which throws
/// away every commit since the last fold.
///
/// # ⚠ REHEARSAL IS THE DEFAULT, and that is not the same rule `rm` has
///
/// `data rm` REFUSES without `--yes`, because somebody typing `rm` means to delete and the danger
/// is doing it unseen. `data repair` PRINTS THE PLAN AND EXITS 0 without `--yes`, because the
/// danger here is the opposite one: a rebuild can succeed and still cost the series its idempotency
/// log, so the fact an operator needs is the VERDICT, and the verdict is only knowable by running
/// the same pass the write runs. The rehearsal IS that pass, lock-free and writing nothing, so it
/// costs a live writer exactly nothing. `--dry-run` spells the default explicitly and WINS over
/// `--yes`, which is `rm`'s rule unchanged: a rehearsal must never require stripping a flag.
///
/// ⚠ A rehearsal that exits 0 having written nothing is the one place this arm could hand somebody
/// a false green, so it says `NOTHING WAS WRITTEN` in as many words and the `--json` document
/// carries a `written` field that is `null` rather than `true`.
///
/// # ⚠ ONE series, named exactly — there is deliberately no `--all`
///
/// Three reasons, and the first alone settles it:
///
/// 1. **The headline failure is invisible to enumeration.** `DataFusionHist::list_series` finds
///    leaves by the presence of `_manifest.json`, so a series whose base was deleted — precisely
///    what `read_manifest` refuses and this verb repairs — is in no `list_series`, no `inventory()`
///    and no `vike-cli data hist ls`. An `--all` built on enumeration would answer "0 series
///    repaired" on the exact state it exists for.
/// 2. **The lock hold is per-series and unbounded.** A rebuild reads every part footer with that
///    series' lock HELD; `--all` turns one bounded critical section into N of them with no operator
///    in front of any.
/// 3. **The verdict is per-series.** `parts_without_keys` and `parts_unreadable` are decisions an
///    operator takes one series at a time, and an `--all` would fold N of them into one exit code.
///
/// So a multi-series repair is N invocations, each with its own rehearsal — the same answer
/// `vike_data::store::removal`'s `SeriesSelector` gives for a cleanup spanning venues.
///
/// # ⚠ `open_read_only`, never `open`
///
/// `DataFusionHist::open` performs two writes: it creates the root (a typo'd `--store` becomes a
/// fresh empty store) and it runs the WAL recovery sweep, which takes each affected series' lock.
/// Both are wrong here, and the second is disqualifying: `recover()` calls `read_manifest` and
/// propagates its error out of `open`, so a series with BOTH a base-less delta log and a
/// `_wal.arrow` makes `open` fail for the whole store — on the very error whose repair needs a
/// handle. `open_read_only` creates nothing and recovers nothing, and still has every `append_*`
/// verb, which is the combination this verb wants.
pub(super) fn run_repair_series(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
) -> ExitCode {
    use vike_data::store::removal::SeriesSelector;

    // ⚠ FIRST, before the store is touched: the root AND the rung that chose it — `run_rm_series`'
    // rule, and for its reason. A store does not merge, so "which store" is the question a
    // write-shaped verb answers before "which series".
    let resolved =
        crate::binutil::store_root_resolved(arg(args, "--store").map(PathBuf::from), vars);
    let json = has_flag(args, "--json");
    if json {
        eprintln!("store: {resolved}");
    } else {
        println!("store: {resolved}");
    }

    let selector = SeriesSelector {
        kind: arg(args, "--kind").unwrap_or_default(),
        venue: arg(args, "--venue").unwrap_or_default(),
        symbol: arg(args, "--symbol"),
        group: arg(args, "--group"),
        interval: arg(args, "--interval"),
    };
    // The same two-stage validation `data rm` runs, and the same split: shape first (facts about
    // the command line), then the layout table (facts about what this store partitions by).
    if let Err(e) = selector.validate_shape() {
        eprintln!("backtest data repair: {e}\n\n{DATA_USAGE}");
        return ExitCode::from(2);
    }
    if let Err(e) = selector.validate_against_kinds() {
        eprintln!("backtest data repair: {e}");
        return ExitCode::from(2);
    }
    // ⚠ A WILDCARD is refused rather than expanded — see this function's doc for why there is no
    // `--all`. `is_sweep` is the SELECTOR's own answer and is kind-aware (`--symbol X` fully names
    // a tick series and wildcards every interval of a bar one), so this refusal cannot ask for an
    // `--interval` on a kind that has no `interval=` segment.
    if selector.is_sweep() {
        eprintln!(
            "backtest data repair: `{}` names more than one series, and a repair names exactly \
             one. A rebuild holds that series' lock across every part footer it reads, and its \
             verdict — what came back and what did not — is per-series, so a wildcard would fold N \
             unbounded critical sections and N verdicts into one exit code. Name the series: \
             --symbol S (plus --interval I on `bar`) or --group G. For several, run this verb \
             several times.",
            selector.describe()
        );
        return ExitCode::from(2);
    }
    let id = match (&selector.symbol, &selector.group) {
        (_, Some(group)) => vike_data::SeriesId::grouped(&selector.kind, &selector.venue, group),
        (Some(symbol), None) => vike_data::SeriesId::per_symbol(
            &selector.kind,
            &selector.venue,
            symbol,
            selector.interval.clone(),
        ),
        // Unreachable: `is_sweep` is true whenever neither is named. Spelled as a refusal rather
        // than `unreachable!()` so a future loosening of that predicate is a message instead of a
        // panic in a binary an operator is running against their own store.
        (None, None) => {
            eprintln!("backtest data repair: neither --symbol nor --group named a series");
            return ExitCode::from(2);
        }
    };

    // ⚠ `open_read_only`, not `open` — see this function's doc. Its absent-root error is also the
    // right answer for a typo'd `--store`: a REPAIR that invented an empty store and then reported
    // nothing to repair would be the confident wrong answer.
    let store = match DataFusionHist::open_read_only(&resolved.root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "backtest data repair: cannot open the hist store at {}: {e}",
                resolved.root.display()
            );
            return ExitCode::from(2);
        }
    };
    let plan = match store.plan_series_manifest_rebuild(&id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("backtest data repair: {e}");
            return ExitCode::from(2);
        }
    };
    // ⚠ The leaf must EXIST. `rebuild_manifest` reads an absent directory as an empty series, and
    // `SeriesLock::acquire` would create it — so a mistyped selector reaching the write publishes
    // an empty manifest at a path nothing ever wrote, minting a phantom series `list_series` then
    // enumerates forever. Refused here, before the plan is even printed.
    if !plan.leaf_present {
        eprintln!(
            "backtest data repair: no series directory at {} — there is nothing here to rebuild \
             FROM. Check the selector; note that a series whose base manifest was deleted is \
             missing from `vike-cli data hist ls` while its DIRECTORY is still on disk, so an ABSENT \
             directory means the selector is wrong rather than the series being the broken one.",
            plan.series_dir
        );
        return ExitCode::from(2);
    }

    let dry_run = has_flag(args, "--dry-run");
    let write = repair_writes(has_flag(args, "--yes"), dry_run);
    // The plan is SHOWN either way, and under `--json` it goes to STDERR rather than nowhere —
    // `run_rm_series`' rule: stdout is the document and nothing else, but a run about to rebuild an
    // index must have shown the operator what it would cost.
    for line in plan.lines() {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    if !write {
        if json {
            println!("{}", repair_json(&resolved, &plan, None, None));
        } else {
            // ⚠ ONE sentence for BOTH spellings, unlike `run_rm_series`' `--dry-run:` prefix — and
            // the difference is that there a rehearsal is the exception while here it is the
            // DEFAULT. `vike-cli data hist repair` forwards `--dry-run` for a line that carried neither
            // flag (so the child is told what that side decided rather than inheriting a default
            // spelled twice), which means a `--dry-run:`-prefixed message would quote back a flag
            // the operator never typed.
            println!(
                "NOTHING WAS WRITTEN — this was a rehearsal. Re-run with --yes to perform it."
            );
        }
        return ExitCode::SUCCESS;
    }

    match store.rebuild_series_manifest_if_uncontended(&id) {
        // ⚠ THE LIVE-WRITER DECISION, and it is a refusal rather than a wait. See
        // `rebuild_series_manifest_if_uncontended`'s doc for why WINNING a contended lock is the
        // outcome to avoid: the rebuild's critical section is every part footer in the series, and
        // a `RecorderSink` whose flush spins that budget out DISCARDS its buffer.
        Ok(None) => {
            if json {
                println!("{}", repair_json(&resolved, &plan, None, Some(REPAIR_CONTENDED)));
            }
            eprintln!(
                "backtest data repair: {REPAIR_CONTENDED} Remedy: stop the writer on this store \
                 (the recorder daemon, a `vike-backend datahub --record`, or a backfill) and \
                 re-run — or re-run now if what you collided with was a passing compaction. ⚠ The \
                 reverse does NOT hold: a lock this verb DOES get is not proof the series is idle, \
                 because a recorder holds it only while committing."
            );
            ExitCode::from(2)
        }
        Ok(Some(report)) => {
            // The verdict is computed from the report the WRITE returned, over the plan's own
            // `orphan_commits` — the one loss no `RebuildReport` field can carry, because a rebuild
            // derives keys from part footers and an orphan is a key no part has.
            let done = vike_data::store::datafusion_hist::RepairPlan { report, ..plan.clone() };
            if json {
                println!("{}", repair_json(&resolved, &done, Some(true), None));
            } else {
                for line in done.outcome_lines() {
                    println!("{line}");
                }
            }
            // ⚠ **A LOSSY SUCCESS IS NOT A CLEAN EXIT.** The index is rebuilt and the rows are
            // readable, so this is not a failure — but exiting 0 over a store that just lost its
            // idempotency log is what sends an operator straight into a backfill that duplicates
            // rows. The exit code is the only thing a wrapper reads, so it is what says so.
            // `run_rm_series`' partial-failure rung, for the same reason.
            if done.is_lossless() {
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "backtest data repair: the rebuild SUCCEEDED and was LOSSY — see the LOSSY \
                     lines above. The index is rebuilt and the rows read again; what did NOT come \
                     back is named there, with what to do about it."
                );
                ExitCode::from(2)
            }
        }
        Err(e) => {
            // ⚠ **An `Err` here is NOT "nothing happened".** The publish and the delta-log clear
            // are two steps (`manifest::fold_base`), so a failure in the second returns `Err` with
            // a new base already durable on disk. Saying so is the difference between an operator
            // re-running a rehearsal (right) and assuming the store is untouched (wrong).
            eprintln!(
                "backtest data repair: {e}\n⚠ this is NOT necessarily 'nothing happened': the \
                 rebuild publishes a new base and THEN clears the delta log, so a failure in the \
                 second step leaves the new base in place. Re-run this verb WITHOUT --yes to see \
                 the store's current state before deciding anything."
            );
            ExitCode::from(2)
        }
    }
}

/// The contention refusal's first sentence, spelled once because two surfaces carry it — stderr for
/// a human, and the `--json` document's `refused` field for a wrapper that must branch on it.
const REPAIR_CONTENDED: &str = "another writer holds this series' lock, so the rebuild was NOT \
                                attempted and NOTHING WAS WRITTEN.";

/// Does this `data repair` line WRITE? PURE over its two flags, so both branches are unit-tested
/// rather than only reachable through a store.
///
/// ⚠ `--dry-run` WINS over `--yes`, which is `run_rm_series`' rule unchanged: a rehearsal must not
/// require stripping a flag, because the line an operator re-runs is the line already in their
/// shell history.
fn repair_writes(yes: bool, dry_run: bool) -> bool {
    yes && !dry_run
}

/// The `--json` document for `data repair`.
///
/// ⚠ It carries the RESOLVED store root and its RUNG for `rm_series_json`'s reason: `vike-cli data
/// hist repair --json` wraps this document rather than re-deriving the root, because that binary
/// resolves nothing (the engine runs in another process) and a path it guessed at would be the
/// confident-sounding wrong answer.
///
/// `written` is `null` for a rehearsal and for the contention refusal — the two cases where nothing
/// was attempted — and `true` for a performed rebuild. `lossless` is what a wrapper branches on,
/// and it is the same verdict the non-zero exit encodes.
fn repair_json(
    resolved: &vike_model::paths::store_path::StoreRoot,
    plan: &vike_data::store::datafusion_hist::RepairPlan,
    written: Option<bool>,
    refused: Option<&str>,
) -> String {
    let doc = serde_json::json!({
        "store_root": resolved.root.display().to_string(),
        "store_rung": resolved.rung.as_str(),
        "store_rung_why": resolved.rung.why(),
        "plan": plan,
        "parts_seen": plan.parts_seen(),
        "lossless": plan.is_lossless(),
        "losses": plan.losses(),
        "notes": plan.notes(),
        "written": written,
        "refused": refused,
    });
    serde_json::to_string_pretty(&doc).expect("a tree of plain data; serialization is total")
}

#[cfg(test)]
mod repair_series_tests {
    use super::*;

    /// ⚠ **`--dry-run` WINS over `--yes`.** The rehearsal must not require stripping a flag, so the
    /// one combination an operator reaches by ADDING a flag to a line they already have must be the
    /// safe one — and the BARE form, the one somebody types first, must rehearse.
    #[test]
    fn dry_run_wins_over_yes_and_the_bare_form_rehearses() {
        assert!(repair_writes(true, false), "--yes alone performs the rebuild");
        assert!(!repair_writes(true, true), "--dry-run WINS over --yes");
        assert!(!repair_writes(false, true), "--dry-run alone rehearses");
        assert!(!repair_writes(false, false), "the BARE form rehearses — this verb's default");
    }
}
