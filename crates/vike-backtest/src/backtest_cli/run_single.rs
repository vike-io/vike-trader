//! `backtest`'s SINGLE run: fingerprint, `run_backtest`, the report, then the persisted run.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use vike_analytics::binutil::has_flag;
use vike_data::HistStore;
use vike_model::runs;

use super::run_record::collect_data_fingerprint;
use super::{BacktestRunFacts, persist_run};
use crate::harness::{self, BacktestProfile};
use crate::run_fingerprint;

// `periods_per_year` (the Sharpe annualization factor) now lives in `harness::report` as the SINGLE
// source of truth, so the single-run path here and the sweep rank an identical profile on the same
// Sharpe scale. Imported below as `harness::report::periods_per_year`.

#[expect(clippy::too_many_arguments)]
pub(super) fn run_single(
    args: &[String],
    now_unix_secs: &dyn Fn() -> i64,
    build: Option<runs::BuildStamp<'_>>,
    profile: BacktestProfile,
    profile_toml: String,
    profile_path: String,
    provenance: String,
    concrete: Option<Arc<dyn HistStore + Send + Sync>>,
    store: Arc<dyn HistStore + Send + Sync>,
    started_at: i64,
    runs_root: Option<PathBuf>,
) -> ExitCode {
    // ⚠ **BELOW the sweep branch, deliberately, and it used to sit above it.** Above it, every byte
    // of a multi-megabyte grouped manifest was parsed and then DISCARDED, before a search that was
    // about to run hundreds of backtests.
    //
    // ⚠ **The sweep branch calls this collector too now, and this line is still not a hoist.** The
    // old argument was arithmetic: the capture asked the CONCRETE store two questions per series and
    // each one parsed that series' manifest INDEPENDENTLY, so a search could not afford it. That
    // double read was the STORE's rather than this caller's, and it is closed —
    // `DataFusionHist::series_facts` answers coverage and commits from one parse, which is exactly
    // what a search was already spending on its data witness. So the branch above collects the
    // record at its own call site and addresses its run from it, this line stays where it is, and
    // the two paths keep their own diagnostics: `run NOT addressed` is the single run's wording and
    // a search has never printed it.
    //
    // ⚠ `None` rather than a wrong answer. The collector refuses rather than guessing when the
    // store cannot be read (see its doc), and a run whose inputs cannot be ADDRESSED still runs and
    // is still saved — it simply records `fingerprint: null` and keeps the pid form of its id.
    //
    // ⚠ Read BEFORE the run, so the record says what the store held when the run STARTED — which is
    // the thing the result depends on.
    let data_fingerprint = match collect_data_fingerprint(
        concrete.as_deref().map(|h| h as &dyn HistStore),
        &profile,
        &provenance,
    ) {
        Ok(fp) => Some(fp),
        Err(why) => {
            eprintln!("backtest: run NOT addressed — {why}");
            None
        }
    };

    // `store` is ALREADY the trait object (one coercion, above the sweep branch), so the
    // single-run path hands it over as-is rather than re-wrapping a concrete handle.
    let result = match harness::run_backtest(&profile, store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("backtest: run failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Read here rather than after printing: `finished_at` must measure the RUN, not the terminal.
    let finished_at = now_unix_secs();

    // ⚠ **The stamp is attached HERE, not inside `from_result`.** That function lives in
    // vike-analytics, which cannot name the gated `BacktestProfile` the stamp is resolved from —
    // so the producer is the door. A report with no stamp reads `realism: None`, which
    // `vike_analytics::report::BacktestReport::realism` documents as "nobody stamped this run" and
    // NOT as "this run was free": the two must not be the same bytes, because the second is the
    // one somebody acts on.
    let report = vike_analytics::report::BacktestReport::from_result(
        profile.name.clone(),
        &result,
        harness::report::periods_per_year(&profile),
    )
    .with_realism(harness::report::realism_stamp(&profile));

    if has_flag(args, "--json") {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("backtest: failed to serialize report: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{report}");
    }

    // ⚠ **AFTER printing, and never fatal.** Persisting is additive: a run whose directory cannot
    // be created has still computed its numbers, and they have already reached stdout by the time
    // this line runs. The failure is NAMED on stderr rather than swallowed — an operator who
    // believes their runs are being saved and finds an empty `user_data/runs/` is worse off than
    // one who was told — but it does not change the exit code, because refusing to exit 0 over a
    // filesystem that would not take a metadata file would make saving a run a new way to lose one.
    //
    // Both lines go to stderr for the reason every other diagnostic in this binary does: `--json`
    // stdout is a machine contract and stays byte-identical.
    //
    // ⚠ `runs_root` was resolved ABOVE the sweep branch — one walk decides for both paths.
    match runs_root.as_deref() {
        Some(runs_root) => {
            // The address over what this run READ, computed once and used twice: it names the
            // manifest's `fingerprint` and it is the middle segment of the run id. `None` when the
            // data half could not be collected — an address over an unknown data slice would be a
            // number that looks authoritative and is not. Declared before `facts`, which borrows
            // it: locals drop in reverse declaration order.
            let input_addr = data_fingerprint
                .as_ref()
                .map(|d| run_fingerprint::input_fingerprint(&profile_toml, d));
            let facts = BacktestRunFacts {
                profile: &profile,
                profile_path: &profile_path,
                profile_toml: &profile_toml,
                store: &provenance,
                runs_root,
                data: data_fingerprint.as_ref(),
                fingerprint: input_addr.as_deref(),
                build,
                started_at,
                finished_at,
            };
            match persist_run(&facts, &result, &report) {
                Ok(dir) => eprintln!("backtest: run saved to {}", dir.display()),
                Err(why) => eprintln!("backtest: run NOT saved — {why}"),
            }
        }
        None => {
            eprintln!("backtest: run NOT saved — no project directory above the working directory")
        }
    }

    ExitCode::SUCCESS
}
