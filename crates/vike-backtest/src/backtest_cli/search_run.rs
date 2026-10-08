//! A parameter search's PARENT run: mint or re-open it, resolve its trials document, write it.

use std::path::{Path, PathBuf};

use vike_model::runs;

use super::SearchRun;
use super::persist::{run_detail_data, run_detail_realism};
use super::search_flags::KeepTrials;
use crate::harness::{self, BacktestProfile, SearchMethod};
use crate::search::select;
use crate::{run_fingerprint, trial_ledger};

/// Mint (or, on `--resume`, re-open) the search's parent run directory and write its
/// `crate::trial_ledger::SEARCH_FILE`.
///
/// ⚠ **Called BEFORE `harness::optimize`**, unlike [`persist_run`], and that is forced rather than
/// chosen: the ledger is written as trials complete, so it needs a directory to be written into.
/// The FAILURE posture for a MINT is unchanged — a caller that gets an `Err` prints it and searches
/// anyway. A RESUME is the opposite and its call site says why.
pub(super) fn open_search_run(
    identity: trial_ledger::SearchIdentity,
    keep: KeepTrials,
    resume: Option<&str>,
    runs_root: &Path,
    started_at: i64,
    fingerprint: Option<&str>,
) -> Result<SearchRun, String> {
    let (run_id, path, resumed_from) = match resume {
        Some(id) => reopen_search_run(runs_root, id, &identity)?,
        None => {
            // Creating the directory is what MINTS the id — `vike_model::runs::RunManifest::run_id`
            // carries that argument, and `crates/vike-backtest/CLAUDE.md` forbids turning it into
            // an exists-test.
            //
            // ⚠ The ADDRESS rides in the id here exactly as it does for a single run, and `None`
            // is a real answer rather than this producer's permanent state: a store that could not
            // be inventoried keeps the pid form, which is what "ABSENT, never DIFFERENT" looks like
            // in a directory name. ⚠ A RESUME mints nothing, so the resumed run keeps the id it was
            // minted with — which is the whole point of resuming one.
            let run = runs::create_run_dir(runs_root, started_at, fingerprint)
                .map_err(|e| e.to_string())?;
            (run.run_id, run.path, None)
        }
    };

    let header = trial_ledger::SearchHeader {
        schema: trial_ledger::TRIAL_LEDGER_SCHEMA,
        run_id: run_id.clone(),
        keep_trials: keep.as_str().to_string(),
        trials_file: trial_ledger::TRIALS_FILE.to_string(),
        identity,
        resumed_from: resumed_from.clone(),
    };
    trial_ledger::write_search_header(&path, &header).map_err(|e| e.to_string())?;
    Ok(SearchRun { run_id, path, resumed_from })
}

/// `--resume`'s half of [`open_search_run`]: re-open an existing search parent after proving it is
/// the SAME search.
///
/// ⚠ It reads `crate::trial_ledger::SEARCH_FILE`, not the manifest, and that is the whole reason
/// that file exists: the manifest is written LAST as the completion marker, so the run an operator
/// most wants to resume — one that was killed — does not have one.
///
/// Every refusal is a REFUSAL rather than a fallback to a fresh search. Silently starting over would
/// look like success while redoing hours of work, and silently reusing scores computed under
/// different inputs would be a wrong answer nobody could see.
fn reopen_search_run(
    runs_root: &Path,
    id: &str,
    identity: &trial_ledger::SearchIdentity,
) -> Result<(String, PathBuf, Option<String>), String> {
    let path = runs_root.join(id);
    if !path.is_dir() {
        return Err(format!(
            "--resume {id:?}: no such run under {}. `backtest trials <id>` and the \
             `search saved to …` line both print the id a run can be resumed by",
            runs_root.display()
        ));
    }
    let header = trial_ledger::read_search_header(&path)
        .map_err(|e| format!("--resume {id:?}: {e}. That run is not a parameter search"))?;
    // ⚠ **Keyed on whether a LEDGER was kept, not on the exact spelling** — this read
    // `!= KeepTrials::Scalars.as_str()` until `returns` existed, at which point a `returns` search
    // (whose ledger is byte-identical to a `scalars` one) would have been refused with "it kept no
    // ledger", which is FALSE, and whose only stated remedy is re-running the whole search. That is
    // the same failure the `--resume` + `--keep-trials none` guard in `parse_search_flags` exists
    // to prevent, arriving from the other side.
    //
    // ⚠ An UNRECOGNISED spelling is refused SEPARATELY and deliberately: it means the run was
    // written by a newer build, so this binary cannot know what its ledger holds. Guessing "it kept
    // one" would resume against a format it has never seen.
    match KeepTrials::from_recorded(&header.keep_trials) {
        Some(mode) if mode.keeps_ledger() => {}
        Some(_) => {
            return Err(format!(
                "--resume {id:?}: that search ran with --keep-trials {}, so it kept no ledger and \
                 there is nothing to resume. Run it again without --resume",
                header.keep_trials
            ));
        }
        None => {
            return Err(format!(
                "--resume {id:?}: that search recorded --keep-trials {:?}, which this build does \
                 not know — it was written by a newer binary, so what its ledger holds cannot be \
                 assumed. Resume it with the build that wrote it, or run the search again without \
                 --resume",
                header.keep_trials
            ));
        }
    }
    let diffs = header.identity.differences(identity);
    if !diffs.is_empty() {
        return Err(format!(
            "--resume {id:?}: this is not the same search. A cached score is an answer to the \
             inputs it was computed under, so a difference is refused rather than resolved:\n  {}",
            diffs.join("\n  ")
        ));
    }
    Ok((header.run_id, path, Some(id.to_string())))
}

/// Resolve the search's LEDGER into the document its `report.json` holds — and, on a resume, into
/// the thing it PRINTS.
///
/// ⚠ **Separated from the write ([`write_search_run`]) deliberately, and the separation is a
/// correctness fix rather than tidiness.** While the two were one function, a resume's output came
/// out of the persist call's `Ok` arm — so a full disk or a directory that lost write permission
/// mid-run gave the operator EMPTY STDOUT with exit 0, and `crates/vike-cli/src/cmd/backtest.rs`'s
/// `--local` arm prints that verbatim, handing a `--json` consumer a zero-byte document and a
/// success. The answer a run computed must not depend on whether its metadata could be saved; that
/// is the same posture `persist_run` states, which the single-run path has always had and this one
/// did not.
///
/// ⚠ `overfit` arrives as a PARAMETER rather than being computed here, and the split is the same
/// one this function's own separation makes: the statistics are an in-memory answer of the process
/// that searched (they need the ranked rows and the evaluator's retained matrix), while everything
/// else here is resolved off DISK from the ledger. `None` for every search that did not opt into
/// `--keep-trials returns`, and for one whose matrix was not measurable.
pub(super) fn build_trials_document(
    run: &SearchRun,
    identity: &trial_ledger::SearchIdentity,
    keep: KeepTrials,
    tally: harness::RecorderTally,
    overfit: Option<trial_ledger::OverfitStats>,
) -> Result<trial_ledger::TrialsDocument, String> {
    // A ledger that was never written (`--keep-trials none`, or a run whose every append failed) is
    // an EMPTY document rather than an error: the counts below still say what the search spent.
    let read = match trial_ledger::read_trials(&run.path) {
        Ok(read) => read,
        Err(runs::RunReadError::Missing { .. }) => trial_ledger::TrialsRead::default(),
        Err(e) => return Err(e.to_string()),
    };
    let unreadable = read.unreadable.len();
    let (trials, superseded) = trial_ledger::latest_by_n(read.trials);
    let failed = trials.iter().filter(|t| t.error.is_some()).count();

    Ok(trial_ledger::TrialsDocument {
        schema: trial_ledger::TRIAL_LEDGER_SCHEMA,
        run_id: run.run_id.clone(),
        keep_trials: keep.as_str().to_string(),
        identity: identity.clone(),
        evaluated: tally.evaluated,
        reused: tally.reused,
        failed,
        unreadable,
        superseded,
        trials,
        overfit,
    })
}

/// Write the search's `vike_model::runs::REPORT_FILE` (the [`build_trials_document`] result) and
/// then its `vike_model::runs::MANIFEST_FILE` — report first, manifest last, exactly as
/// [`persist_run`] and `runs::write_run` do, so a directory holding a manifest is still a run that
/// finished writing.
///
/// Called AFTER the answer has been printed and never fatal, which is [`persist_run`]'s posture and
/// now this path's too — see [`build_trials_document`] for what it cost while it was not.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_search_run(
    run: &SearchRun,
    profile: &BacktestProfile,
    profile_path: &str,
    // ⚠ **What to RECORD as the provenance of this data — a path on a local run, the datahub's
    // ADDRESS on a routed one.** It was `store_root: &Path` until
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` routed this path, and a `&Path`
    // here is no longer a fact the caller has: on the wire arm the resolved root is THIS box's
    // default, a directory the run never opened, and recording it would put a confident falsehood
    // in the one document a later reader uses to reproduce the run. `HistoryRoute::label` renders
    // it, so the record and the run's own disclosure cannot disagree.
    store_label: &str,
    identity: &trial_ledger::SearchIdentity,
    keep: KeepTrials,
    build: Option<runs::BuildStamp<'_>>,
    started_at: i64,
    finished_at: i64,
    document: &trial_ledger::TrialsDocument,
    data: Option<&run_fingerprint::DataFingerprint>,
    fingerprint: Option<&str>,
) -> Result<PathBuf, String> {
    let manifest = runs::RunManifest {
        schema: runs::MANIFEST_SCHEMA,
        run_id: run.run_id.clone(),
        kind: trial_ledger::SEARCH_RUN_KIND.to_string(),
        produced_by: "backtest".to_string(),
        started_at: runs::utc_rfc3339(started_at),
        finished_at: runs::utc_rfc3339(finished_at),
        git_sha: build.and_then(|b| b.git_sha).map(str::to_string),
        // ⚠ **The SAME address a single run carries, computed the SAME way** —
        // `run_fingerprint::input_fingerprint` over the config TEXT and the store's own coverage —
        // so a search and a backtest over identical inputs land in one comparable space rather than
        // in two that merely look alike. `None` only when the store could not be inventoried, which
        // is the ABSENT-never-DIFFERENT posture the call site argues.
        //
        // The `search.json` identity is still what a RESUME compares, and it is a different
        // question: it witnesses the ingest COMMIT KEYS, which this address deliberately excludes
        // (`run_fingerprint::SeriesFingerprint::commits` says why an address over a rebuildable
        // log would orphan every baseline).
        fingerprint: fingerprint.map(str::to_string),
        config: runs::RunConfig {
            path: Some(profile_path.to_string()),
            name: profile.name.clone(),
        },
        detail: serde_json::json!({
            "strategy": profile.strategy.name,
            "data": run_detail_data(profile, data),
            // ⚠ The BASE profile's cost model, which every point inherits except on a key its own
            // `[paramscan]` overrides. A grid that sweeps `engine.fee_rate` therefore has points
            // this one row does not describe — and recording nothing would be worse, because then
            // a search's manifest says nothing about costs at all, which is the defect the stamp
            // exists to close. The per-point stamp belongs on the per-point report.
            "realism": run_detail_realism(profile),
            "store": store_label,
            "build": build.and_then(|b| b.summary),
            // What a LISTING needs in order to render a search row without opening the ledger.
            "search": {
                "schema": trial_ledger::TRIAL_LEDGER_SCHEMA,
                "optimizer": identity.method,
                "rank_by": identity.rank_by,
                "seed": identity.seed,
                "budget": identity.budget,
                "resumed_from": run.resumed_from,
            },
            // Read off the DOCUMENT rather than recomputed, so a listing's summary and the report
            // beside it cannot disagree about one search.
            "trials": {
                "file": trial_ledger::TRIALS_FILE,
                "keep": keep.as_str(),
                "evaluated": document.evaluated,
                "reused": document.reused,
                "failed": document.failed,
                "unreadable": document.unreadable,
                "superseded": document.superseded,
            },
        }),
    };

    runs::write_run(&run.path, &manifest, document).map_err(|e| e.to_string())?;
    Ok(run.path.clone())
}

/// The three method-shaped facts a search's identity records: the method's own name, its seed, and
/// its budget scalar.
///
/// ⚠ Read off the RESOLVED [`SearchMethod`] rather than off argv, so a default counts the same as a
/// written value — `--optimizer tpe` and `--optimizer tpe --trials 64` are the same search and must
/// resume each other. `harness::SearchOutcome::summary` cannot supply this: it is a pre-rendered
/// String, and only two of the four methods have a typed budget at all — neither of which reaches
/// the seam. Making the budget machine-readable at that boundary is a separate change.
///
/// ⚠ DELEGATES to `search::select::identity_parts`, which moved there with the method enum
/// itself: a second reading of a `SearchMethod` is a second chance to disagree about what `budget`
/// means for euler.
pub fn search_identity_parts(method: &SearchMethod) -> (&'static str, Option<u64>, Option<u64>) {
    select::identity_parts(method)
}
