//! Fold stray PER-SYMBOL tick series into their family's GROUPED series.
//!
//! ```text
//! migrate_to_group --venue polymarket [--store DIR] [--group NAME] [--dry-run]
//! ```
//!
//! ## What it repairs
//!
//! A store holding the same instrument in BOTH layouts. Two real causes: a family recorded before
//! grouping existed, and two recorder bugs that let rows escape to per-symbol series before they
//! were fixed (a max-rows flush that skipped the group resolver, and a rotation that unmapped a
//! symbol while its rows were still buffered).
//!
//! Such a store is **not wrong** — `scan_*` reads both layouts, so no row is lost or hidden. It is
//! untidy in ways that cost later: the coverage report lists one instrument twice, maintenance
//! treats them as unrelated series, and every scan opens both.
//!
//! ## Safe by construction: it refuses when the target is ambiguous
//!
//! The store cannot know which family a stray token belonged to — that knowledge lived in the
//! recorder's membership map and is gone. So this only acts when the answer is UNAMBIGUOUS: the
//! venue must have **exactly one** grouped series for that kind. With none it has nothing to fold
//! into; with several it would be guessing, and guessing here writes rows into the wrong family.
//! `--group` names the target explicitly for the several case.
//!
//! Per series the work is `vike_data::DataFusionHist::migrate_series_to_group`, which copies,
//! verifies the grouped write landed, and only then deletes the source — so an interrupted run
//! leaves BOTH copies (untidy, which is what it already was) rather than a hole.
//!
//! `--dry-run` prints the plan and touches nothing.

use std::process::ExitCode;

use vike_backfill::cli::{arg, has_flag, log_config, store_root, CliSpec};
use vike_backfill::regroup::{plan_regroup, KindPlan};
use vike_data::DataFusionHist;

const USAGE: &str = "\
usage: migrate_to_group --venue NAME [--store DIR] [--group NAME] [--dry-run]

Folds stray per-symbol tick series into the venue's grouped series. Acts only when the target is
unambiguous — exactly ONE group per kind — since the store cannot know which family a stray
belonged to. A migration copies and verifies before deleting, and one series failing never aborts
the rest.

  --venue NAME   the venue whose stray series to fold (required)
  --store DIR    hist-store root (default: $VIKE_HIST_STORE, else <repo>/market_data/hist)
  --group NAME   name the target group when a kind has several (otherwise AMBIGUOUS, skipped)
  --dry-run      print what would be folded and write nothing
  -h, --help     print this and exit 0
  -V, --version  print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "migrate_to_group",
    usage: USAGE,
    valued: &["--venue", "--store", "--group"],
    toggles: &["--dry-run"],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _guards = vike_log::init(log_config("migrate-to-group"));

    let Some(venue) = arg(&args, "--venue") else {
        eprintln!("migrate_to_group: --venue is required\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let dry = has_flag(&args, "--dry-run");
    let forced_group = arg(&args, "--group");

    let root = store_root(arg(&args, "--store").as_deref(), &std::env::vars().collect());
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("opening store {}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };
    let series = match store.list_series() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("listing series: {e}");
            return ExitCode::FAILURE;
        }
    };

    let listed: Vec<(String, String, String, Option<String>)> =
        series.into_iter().map(|s| (s.kind, s.venue, s.symbol, s.group)).collect();
    let plans = plan_regroup(&listed, &venue, forced_group.as_deref());
    if plans.is_empty() {
        println!("{venue}: no per-symbol tick series to fold — nothing to do");
        return ExitCode::SUCCESS;
    }

    let mut planned = 0usize;
    let mut moved_rows = 0usize;
    let mut skipped = 0usize;

    for (kind, plan) in &plans {
        let group = match plan {
            KindPlan::Fold { group, .. } => group,
            KindPlan::NoTarget { symbols } => {
                println!(
                    "{kind}: {} per-symbol series, but NO grouped series to fold into — skipped",
                    symbols.len()
                );
                skipped += symbols.len();
                continue;
            }
            KindPlan::Ambiguous { groups, symbols } => {
                println!(
                    "{kind}: {} per-symbol series and {} groups ({}) — AMBIGUOUS, skipped. Re-run \
                     with --group NAME to name the target.",
                    symbols.len(),
                    groups.len(),
                    groups.join(", ")
                );
                skipped += symbols.len();
                continue;
            }
        };

        for symbol in plan.symbols() {
            planned += 1;
            if dry {
                println!("dry-run: {kind}/{venue}/{symbol} -> group={group}");
                continue;
            }
            match store.migrate_series_to_group(kind, &venue, symbol, group) {
                Ok(0) => println!("{kind}/{symbol}: nothing to move"),
                Ok(n) => {
                    moved_rows += n;
                    println!("{kind}/{symbol} -> group={group}: {n} rows");
                }
                Err(e) => {
                    // Do NOT abort the run: each series is independent, and a failure leaves that
                    // one's source intact (migrate copies+verifies before deleting).
                    eprintln!("{kind}/{symbol}: FAILED, source left in place: {e}");
                    skipped += 1;
                }
            }
        }
    }

    if dry {
        println!("dry-run: {planned} series would be folded; nothing written");
    } else {
        println!("done: {planned} series, {moved_rows} rows moved, {skipped} skipped");
    }
    ExitCode::SUCCESS
}
