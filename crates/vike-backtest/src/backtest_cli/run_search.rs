//! `backtest`'s parameter SEARCH: evaluator, parent run, ledger, report, and the persisted artifact.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use vike_analytics::binutil::has_flag;
use vike_data::HistStore;
use vike_model::runs;

use super::persist::data_witness;
use super::run_record::collect_data_fingerprint;
use super::search_flags::KeepTrials;
use super::search_run::{
    build_trials_document, open_search_run, search_identity_parts, write_search_run,
};
use super::trials_cmd::{TrialSort, sort_trials};
use super::{SearchFlags, render_trials};
use crate::harness::{self, BacktestProfile};
use crate::search::select;
use crate::{run_fingerprint, trial_ledger};

#[expect(clippy::too_many_arguments)]
pub(super) fn run_search(
    args: &[String],
    now_unix_secs: &dyn Fn() -> i64,
    build: Option<runs::BuildStamp<'_>>,
    search: SearchFlags,
    profile: BacktestProfile,
    profile_toml: String,
    profile_path: String,
    provenance: String,
    concrete: Option<Arc<dyn HistStore + Send + Sync>>,
    store: Arc<dyn HistStore + Send + Sync>,
    started_at: i64,
    runs_root: Option<PathBuf>,
) -> ExitCode {
    // The `(objective, label)` pair, built ONCE. The ladder spelled this block VERBATIM TWICE
    // (the tpe arm and the euler arm) and a THIRD way inline in the grid arm.
    //
    // ⚠ Declared BEFORE the evaluator: `StoreEvaluator::new` borrows the objective for the
    // evaluator's whole life, and locals drop in reverse declaration order.
    // ⚠ `select::objective_for`, shared with `crate::compute_server`: a remote run must
    // build the same objective under the same label, or two routes rank one grid differently.
    let (objective, label) = select::objective_for(search.rank);
    // ⚠ Taken HERE, before the evaluator: `StoreEvaluator::new` MOVES `label`, so a search's
    // identity cannot read it afterwards. One clone of a short string, so that the label the
    // search is RANKED by and the label its artifact RECORDS are the same value by
    // construction rather than by two matches agreeing.
    let rank_label = label.clone();

    // ⚠ Resolved HERE, once, at the composition root — not inside three library adapters. The
    // concrete adapters (`run_paramscan`/`run_paramscan_with`/`run_paramscan_euler`) still call
    // `ParamscanExec::from_env` themselves for the datahub and their own tests, so the
    // `("vike-backtest", "VIKE_SWEEP_SEQUENTIAL")` row on
    // `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` does not move. Calling
    // the existing `from_env()` from here adds no `env::var` literal for the scanner to find, so
    // that gate sees nothing new either — and it must stay `from_env()`: a `from_vars(&map)`
    // twin would need a SECOND registry row for the same `(name, krate)` pair with a different
    // layer.
    //
    // ⚠ TPE used to be handed a hard-coded `ParamscanExec::Sequential` and now gets whatever this
    // resolves. The output is byte-identical BY THE POOL RULE, not by the exec value:
    // `StoreEvaluator::evaluate` enters rayon only when `exec == Parallel && batch.len() > 1`,
    // and `TpeSearch::search` submits width-1 batches because each proposal reads the whole
    // observation history. Stated rather than assumed, because if that clause ever loosens TPE
    // silently gains a pool per single backtest.
    let exec = harness::ParamscanExec::from_env();

    // ⚠ **ONE evaluator, and WHICH CONSTRUCTOR is a PRESERVATION decision rather than a
    // preference.** The two-input match MOVED to `search::select::evaluator_for` with
    // stage 7 — `crate::compute_server` must make the identical choice for a REMOTE run, and
    // `select::uses_classic_evaluator` carries the whole argument (and is assertable
    // without a store, which is why the rule is now a named predicate rather than a comment).
    let eval = match select::evaluator_for(
        search.method,
        search.rank,
        &profile,
        store,
        &objective,
        label,
        exec,
    ) {
        Ok(e) => e,
        // The SAME prefix the adapters produced, so nothing an operator greps changes: today
        // the params-not-a-table refusal already comes out of this constructor inside
        // `run_paramscan_exec` and reaches stderr as `backtest: sweep run failed: …`.
        Err(e) => {
            eprintln!("backtest: sweep run failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ⚠ **The ONE place the trial MATRIX is armed, and it is off for every mode but
    // `returns`.** A consuming builder, so a search that did not ask retains not one float and
    // is byte-identical to a run from before the mode existed —
    // `harness::sweep::ReturnBuckets` carries the retention arithmetic, and
    // `KeepTrials::return_buckets` is the one mapping from the flag to it. Deliberately NOT a
    // parameter on `select::evaluator_for`: that function is shared with
    // `crate::compute_server`, which has no `--keep-trials` and would have to pass a value
    // meaning "unchanged" at a call site that knows nothing about ledgers.
    let eval = eval.with_return_buckets(search.keep.return_buckets());
    // ⚠ **THE OBSERVER DOOR, and it is called UNCONDITIONALLY.**
    // `select::arm_observers` is the ONE place the floor and the progress stream are
    // armed on a store-driven evaluator, so this binary and `crate::compute_server` cannot arm
    // them differently and a third surface arms them by calling this rather than by remembering
    // two builders and a budget lookup.
    //
    // ⚠ Unconditional rather than behind an `if`, because that function's own doc states both
    // halves are NO-OPS at their defaults: a `TradeFloor::DISARMED` plus a `ProgressMode::Auto`
    // on a non-terminal stderr hands back the evaluator with neither field set. So a search
    // that wrote neither flag is byte-identical to one from before they had doors, and
    // `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
    // `profile_sweep_is_byte_identical_local_and_remote` is unmoved — which is exactly what a
    // condition spelled two ways on two surfaces could not promise.
    //
    // ⚠ `search.method` is passed only so the progress line can ask that METHOD for a total
    // (`budget_hint_for`); the floor is method-agnostic. `SearchMethod` is `Copy`, so this does
    // not consume what `optimizer_for` reads below.
    let eval = select::arm_observers(eval, search.method, &profile, search.floor, search.progress);

    // ⚠ BEFORE `optimizer_for`, which takes `search.method` BY VALUE.
    let (method_name, seed, budget) = search_identity_parts(&search.method);
    // THE ONE STORE READ A SEARCH PERFORMS, and it now answers TWO questions.
    //
    // ⚠ **This used to be a witness-only walk, and the second answer is FREE.** A search has
    // always paid one `list_series` plus one manifest parse per series for its resume witness;
    // [`collect_data_fingerprint`] costs exactly that since `series_facts`
    // merged the coverage and commits reads into one parse. So the RECORD is collected here and
    // BOTH the witness ([`data_witness`], a pure rendering of it) and the run's INPUT ADDRESS
    // fall out of the same walk. Nothing was hoisted: the single-run call site is still below
    // this branch, for the reason its own doc gives.
    //
    // ⚠ A failure is the SENTINEL for the witness and an ABSENCE for the address, and the two
    // postures are different on purpose. A store that cannot be inventoried must never compare
    // EQUAL to anything, including to itself, so a resume against it is refused rather than
    // silently reusing scores over data nobody can witness — that is what the sentinel buys. An
    // ADDRESS is the opposite: an un-inventoried slice must be ABSENT rather than DIFFERENT
    // (`crate::run_fingerprint`'s module doc and `crates/vike-cli/src/cmd/runs/gate.rs`'s both
    // rest on it), so the manifest records `fingerprint: null` and the id keeps its pid form.
    // A store fault therefore costs a search its baseline comparison and never fabricates one.
    //
    // The line names it once; it does not fail the search, which persisting never does.
    let (store_data, search_data, input_addr) = match collect_data_fingerprint(
        concrete.as_deref().map(|h| h as &dyn HistStore),
        &profile,
        &provenance,
    ) {
        Ok(fp) => {
            let addr = run_fingerprint::input_fingerprint(&profile_toml, &fp);
            (data_witness(&fp), Some(fp), Some(addr))
        }
        Err(why) => {
            eprintln!(
                "backtest: the search's data could not be witnessed — {why}. It is \
                     recorded as `{}`, so this search can be resumed by nothing, and the run \
                     is NOT addressed",
                trial_ledger::DATA_UNREADABLE
            );
            (trial_ledger::DATA_UNREADABLE.to_string(), None, None)
        }
    };
    let identity = trial_ledger::SearchIdentity {
        // Re-read rather than retained: `BacktestProfile::from_path_with_text` keeps the TEXT
        // for the single-run record, but a search must address the FILE as it is now, and a
        // file that will not re-read cannot be proved unchanged — which is what
        // `trial_ledger::PROFILE_UNREADABLE` means and why `SearchIdentity::differences`
        // refuses it on both sides.
        profile_fnv1a64: std::fs::read(&profile_path)
            .map(|b| trial_ledger::fnv1a64_hex(&b))
            .unwrap_or_else(|_| trial_ledger::PROFILE_UNREADABLE.to_string()),
        profile_name: profile.name.clone(),
        // The run's ONE provenance (see `provenance` above): the datahub's address, or the
        // CANONICALIZED archive path — unlike the manifest's `config.path`, because two projects'
        // `--archive days` name two different directories and a shared runs root
        // (`VIKE_USER_DATA_DIR`) would otherwise make those two searches compare EQUAL.
        store: provenance.clone(),
        store_data,
        build: build.and_then(|b| b.git_sha).map(str::to_string),
        method: method_name.to_string(),
        // `rank_label` is taken above, before `StoreEvaluator::new` moves the original.
        rank_by: rank_label,
        seed,
        budget,
    };

    // ONE construction match. PR 3's genetic method costs exactly one arm here and one in
    // `parse_search_flags`.
    let method = select::optimizer_for(search.method);

    // The parent run, opened BEFORE the search — the ledger is written as trials complete, so
    // it needs a directory to be written into.
    let parent = match runs_root
        .as_deref()
        .ok_or_else(|| "no project directory above the working directory".to_string())
        .and_then(|root| {
            open_search_run(
                identity.clone(),
                search.keep,
                search.resume.as_deref(),
                root,
                started_at,
                input_addr.as_deref(),
            )
        }) {
        Ok(run) => Some(run),
        // ⚠ TWO postures, and the difference is what was ASKED FOR. A MINT that fails is a run
        // that cannot be saved — persisting is additive, so it is named and the search proceeds.
        // A RESUME that fails is a request that cannot be honoured: continuing as a fresh
        // search would silently redo the work the operator was trying to avoid, and would look
        // like success.
        Err(why) if search.resume.is_some() => {
            eprintln!("backtest: {why}");
            return ExitCode::from(2);
        }
        Err(why) => {
            eprintln!("backtest: search NOT saved — {why}");
            None
        }
    };

    // ⚠ `None` under `--keep-trials none` OR when the parent could not be opened — in both
    // cases the recorder evaluates exactly as the bare evaluator would and writes nothing.
    //
    // ⚠ **Keyed on [`KeepTrials::keeps_ledger`], never on `== Scalars`.** This matched the
    // `Scalars` variant by NAME until `returns` existed, and the `_ => None` arm would then
    // have silently made a `--keep-trials returns` search write no ledger at all — the one
    // failure that mode must not have, since it is `scalars` PLUS a matrix rather than instead
    // of one. That is also what `reopen_search_run`'s twin check is keyed on, so the writer and
    // the resumer agree by construction.
    let ledger = match parent.as_ref() {
        Some(p) if search.keep.keeps_ledger() => Some(p.path.clone()),
        _ => None,
    };

    // The warm cache. Read from the parent's own ledger — the SAME directory the recorder is
    // about to append to — because a resume continues the run rather than minting a second.
    //
    // ⚠ `latest_by_n` first: a previous resume may have re-evaluated a line its cache missed,
    // and the LATER record is the one that process actually computed.
    let warm = match (search.resume.as_deref(), parent.as_ref()) {
        (Some(_), Some(p)) => match trial_ledger::read_trials(&p.path) {
            Ok(read) => {
                if !read.unreadable.is_empty() {
                    // Named, never fatal: a torn line costs its own trial a re-evaluation and
                    // nothing else, which is exactly what JSON Lines buys.
                    eprintln!(
                        "backtest: {} ledger line(s) in {} did not parse and will be \
                         re-evaluated",
                        read.unreadable.len(),
                        p.run_id
                    );
                }
                let (trials, _superseded) = trial_ledger::latest_by_n(read.trials);
                harness::warm_from(&trials)
            }
            Err(e) => {
                eprintln!("backtest: the ledger could not be read — {e}; nothing is reused");
                std::collections::HashMap::new()
            }
        },
        _ => std::collections::HashMap::new(),
    };
    let resuming = search.resume.is_some();
    let recorder = harness::TrialRecorder::new(&eval, ledger, warm);

    // ONE call, through the ONE door: `optimize` runs `require_overridable_params` and
    // `accepts` before any loop starts, which is what makes `PointEvaluator::evaluate`
    // infallible by type. Nothing may call `Optimizer::search` directly.
    //
    // ⚠ The recorder DECORATES the evaluator rather than replacing it: with an empty warm cache
    // it hands the inner evaluator the original batch verbatim and returns its answer
    // untouched, which is what keeps a recorded search byte-identical to an unrecorded one.
    let harness::Optimized { report: sweep_report, summary } =
        match harness::optimize(method.as_ref(), &profile, &recorder) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("backtest: sweep run failed: {e}");
                return ExitCode::FAILURE;
            }
        };
    // Read after the work and before the terminal, so it measures the SEARCH.
    let finished_at = now_unix_secs();
    let tally = recorder.tally();
    if let Some(why) = recorder.first_write_error() {
        // ONE line however many appends failed: a directory that stopped accepting writes fails
        // every one of them, and an operator needs the reason, not the count of repetitions.
        eprintln!(
            "backtest: {} trial(s) were NOT written to the ledger — {why}",
            tally.write_failures
        );
    }

    // **The anti-overfitting statistics, computed ONCE over the RANKED rows.**
    //
    // ⚠ Here rather than inside `build_trials_document`, because the inputs are different: this
    // reads the ranked `ParamscanReport` and the evaluator's retained matrix, both of which are
    // in-memory answers of THIS process, while that function resolves the LEDGER off disk.
    // Folding them together would mean handing a document builder an evaluator.
    //
    // ⚠ Column order is `sweep_report.rows`' order, which is the RANKING — so row 0 is the
    // winner whose Sharpe is deflated, and the statistic cannot depend on the order rayon
    // workers happened to finish in. `harness::optimize::overfit_stats` states that contract.
    //
    // ⚠ `None` under every mode but `--keep-trials returns`, and also whenever the matrix was
    // not measurable (every trial failed, or the range was too short to give `T >= splits`).
    // An absent block is the honest answer: a zeroed one would read as "assessed, and clean".
    //
    // ⚠ Called UNCONDITIONALLY rather than under `if search.keep == Returns`, deliberately:
    // `overfit_stats` keys on what was actually CAPTURED, so the statistic and the retention
    // cannot come to disagree about whether a matrix exists. Under every other mode the
    // evaluator captured nothing, that function's first guard returns `None`, and the whole
    // cost is one lock and one clone of an empty map.
    let overfit = harness::optimize::overfit_stats(
        &sweep_report.rows,
        &eval.captured_returns(),
        harness::optimize::DEFAULT_CSCV_SPLITS,
    );
    if search.keep == KeepTrials::Returns && overfit.is_none() {
        // Named rather than silent: an operator who typed `--keep-trials returns` is owed the
        // reason their report.json carries no `overfit` block, and every cause is a property of
        // the search rather than a fault.
        eprintln!(
            "backtest: no overfit statistics — no trial retained a usable return vector, or \
             the range gives fewer than {} buckets to split. A resumed search's REUSED trials \
             carry a score rather than a curve and contribute none",
            harness::optimize::DEFAULT_CSCV_SPLITS
        );
    }

    // ⚠ The ANSWER is resolved before the WRITE and independently of it. On a resume this
    // document IS the output, and while the two were one call a full disk handed the operator
    // empty stdout with exit 0 — see [`build_trials_document`]. The write itself goes back
    // BELOW the print, which is [`persist_run`]'s posture and now this path's too.
    let document = match parent.as_ref() {
        Some(run) => match build_trials_document(run, &identity, search.keep, tally, overfit) {
            Ok(doc) => Some(doc),
            Err(why) => {
                eprintln!("backtest: the trial ledger could not be resolved — {why}");
                None
            }
        },
        None => None,
    };

    // The METHOD's own cost line, on stderr so a `--json` stdout stays a clean document, and
    // BEFORE the report — where both hand-written copies printed theirs. euler's is
    // `EulerBudget`'s `Display` and tpe's is the line `TpeSearch::search` now assembles from its
    // own config and its own ranked rows; the grid says nothing, exactly as before.
    if let Some(line) = summary {
        eprintln!("{line}");
    }

    // ONE serialize-or-print tail. The tpe arm copied this block WHOLESALE before its early
    // `return`; that copy and that `return` are gone.
    //
    // ⚠ WHAT A RESUME PRINTS IS DIFFERENT, and the reason is what a reused row HOLDS. A cache
    // hit answers with `report: None` (`search::trials::WarmTrial` carries a SCORE, not a
    // report — see that type for why rebuilding one from the ledger would print a number the
    // original run did not compute), and `harness::sweep::ParamscanReport`'s `Display` renders any
    // row with `report: None` as `FAILED: {error}`. So a resumed run's sweep report would print
    // every reused trial as a failure. The ledger is the complete answer and this is the same
    // renderer `backtest trials` uses — one implementation, two entry points.
    //
    // A FRESH search's stdout is UNTOUCHED: `crates/vike-cli/src/cmd/backtest.rs`'s `--local`
    // arm prints it verbatim and `scripts/cli_mcp_smoke.sh` asserts on `.rows`.
    if resuming {
        // ⚠ A resume whose ANSWER could not be produced is a FAILURE, never a silent success.
        // Reachable only when the ledger this process just appended to became unreadable — a
        // genuine fault, already named above. Exiting 0 with empty stdout here is what the
        // `--local` arm would forward to a `--json` consumer as a zero-byte document.
        let Some(document) = document.as_ref() else {
            eprintln!(
                "backtest: --resume produced no answer — the resumed run's ledger could not be \
                 resolved, so there is nothing to print. The search itself ran; \
                 `backtest trials {}` reads whatever reached disk",
                parent.as_ref().map(|p| p.run_id.as_str()).unwrap_or("<id>")
            );
            return ExitCode::FAILURE;
        };
        if has_flag(args, "--json") {
            match serde_json::to_string_pretty(document) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("backtest: failed to serialize the trials document: {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            let rows = sort_trials(document, TrialSort::Score, None);
            print!("{}", render_trials(document, &rows));
        }
    } else if has_flag(args, "--json") {
        match serde_json::to_string_pretty(&sweep_report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("backtest: failed to serialize sweep report: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{sweep_report}");
    }

    // ⚠ **AFTER printing, and never fatal** — [`persist_run`]'s posture, which this path now
    // shares: the search has already handed the operator its numbers by the time this runs.
    let saved = match (parent.as_ref(), document.as_ref()) {
        (Some(run), Some(doc)) => match write_search_run(
            run,
            &profile,
            &profile_path,
            &provenance,
            &identity,
            search.keep,
            build,
            started_at,
            finished_at,
            doc,
            search_data.as_ref(),
            input_addr.as_deref(),
        ) {
            Ok(dir) => Some(dir),
            Err(why) => {
                eprintln!("backtest: search NOT saved — {why}");
                None
            }
        },
        _ => None,
    };

    if let Some(dir) = saved.as_ref() {
        // ⚠ The reuse half of this line is what makes `--resume` FALSIFIABLE from the outside:
        // without it every assertion about a resumed ledger would also pass on a binary that
        // ignored the flag. `tally.evaluated` counts every candidate the searcher asked about,
        // reused ones INCLUDED, so the freshly-computed count is the difference.
        let resumed = if resuming {
            format!(
                ", resumed {} reused / {} evaluated",
                tally.reused,
                tally.evaluated - tally.reused
            )
        } else {
            String::new()
        };
        eprintln!("backtest: search saved to {}{resumed}", dir.display());
    }

    ExitCode::SUCCESS
}
