//! The parameter-search flags: `--keep-trials`, and the ONE pure parser over every search flag.

use vike_analytics::binutil::arg;

use super::args::flag_given;
use super::{SEARCH_PROPERTY_FLAGS, SearchFlags, required_value};
use crate::harness::optimize::ProgressMode;
use crate::harness::{self, SearchSelection};
use crate::search::select::{self, METHOD_KNOBS};

// ⚠ The method ROSTER and its default are the PROTOCOL crate's, not a local literal: `vike-cli`
// spelling-checks `--optimizer` against the same const, and `vike-datahub-client` is the only crate
// both it and this one take as a normal dependency. `vike_datahub_client::proto`'s
// `SEARCH_METHODS` carries the argument.
use vike_datahub_client::{DEFAULT_SEARCH_METHOD, SEARCH_METHODS};

// ---------------------------------------------------------------------------------------------
// The parameter-search flags: ONE pure parser, run before any I/O.
//
// Ruling 13 of `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md` refused a separate
// `optimize` verb — the flags stay on `backtest` and the word "optimizer" lives in the FLAG. What
// this replaced was a hand-written ladder in [`run`] that parsed each flag INSIDE the branch that
// used it, so any flag belonging to a branch not taken was never read and never validated. That is
// one cause with four faces, and every one of them exited 0:
//
//   * `--optimizer tpe --search bogus` ran tpe (the `--optimizer` arm returned before `--search`);
//   * `--search euler --trials 0` ignored a `0` the tpe arm treats as fatal;
//   * `--search grid --euler-depth 99` silently discarded a euler-only flag — and so did
//     `--euler-depth abc`, because the malformed-value check lived in the untaken branch;
//   * `--optimizer=tpe` ran the GRID, because `binutil::arg` matched an exact token only.
//
// The rule this file now encodes, in one sentence: **every knob belongs to exactly one method, and
// a knob handed to a method that does not own it is a REFUSAL rather than a silent discard.**
// ---------------------------------------------------------------------------------------------

/// What `--keep-trials` selected.
///
/// ⚠ **`returns` is the mode that unblocked the anti-overfitting statistics, and it is NOT
/// `series` under another name.** `vike_analytics::overfit::pbo_cscv` and
/// `deflated_sharpe_with_effective_n` need an N-trial matrix of per-observation performance, and a
/// trial's equity curve is destroyed before a sweep row exists:
/// `crates/vike-backtest/src/harness/sweep/grid.rs`'s `row_from_outcome` consumes the
/// `BacktestResult` to derive `BacktestReport`'s scalars and drops `equity_curve`, `equity_ts`,
/// `trades` and `per_symbol_curves`. The measurement that dissolved the deadlock is that the
/// STATISTIC does not want the curve: `pbo_cscv` splits `T` observations into `n_splits` contiguous
/// blocks, so it needs `T >= n_splits` and nothing more. `returns` therefore retains a FIXED-SIZE
/// bucketed return vector per trial (`harness::sweep::ReturnBuckets`, 512 buckets = 4 KB a trial,
/// ~2 MB over a 500-point grid against ~80 MB of decimated curves) and writes the resulting
/// `crate::trial_ledger::OverfitStats` into the search's `report.json`, where
/// `vike-cli backtest gate --fail-if` can name `overfit.pbo` and its siblings.
///
/// ⚠ **`series` IS STILL REFUSED BY NAME, and the refusal is narrower than it was.** What it asks
/// for is the whole CURVE per trial, which is a retention question rather than a statistical one:
/// at `vike_model::runs::MAX_EQUITY_SAMPLES` it is forty times `returns`' cost on a box whose sweep
/// worker cap is already `harness::sweep::threads::DEFAULT_SWEEP_THREADS` because each concurrent
/// point materialises its own data slice — and no consumer in this tree reads a per-trial curve.
/// Keeping a SINGLE run's curve is what [`persist_run`] already does, and re-running the winner on
/// its own is how an operator gets one.
///
/// ⚠ **`returns` writes the same LEDGER as `scalars`**, which is what makes it resumable:
/// `reopen_search_run` keys its refusal on whether a ledger was kept ([`KeepTrials::keeps_ledger`]),
/// never on the exact spelling. A resumed search's REUSED trials contribute no column (a warm-cache
/// answer is a score, not a report — `search::trials::WarmTrial` says why), so the matrix covers
/// the freshly-evaluated trials and `OverfitStats::excluded` reports the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeepTrials {
    None,
    Scalars,
    /// `scalars`, PLUS the in-memory bucketed return matrix the overfit statistics are computed
    /// from. See this type's doc.
    Returns,
}

impl KeepTrials {
    /// The spelling written into `search.json` and `report.json`.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            KeepTrials::None => "none",
            KeepTrials::Scalars => "scalars",
            KeepTrials::Returns => "returns",
        }
    }

    /// Whether this mode writes a `crate::trial_ledger::TRIALS_FILE` at all — the ONE question
    /// `--resume` actually asks, and the reason that check is not `== Scalars` any more: a
    /// `returns` search keeps a byte-identical ledger, so refusing to resume it would send an
    /// operator to "run the whole search again" for a reason that is not true.
    pub(super) fn keeps_ledger(self) -> bool {
        match self {
            KeepTrials::None => false,
            KeepTrials::Scalars | KeepTrials::Returns => true,
        }
    }

    /// The recorded spelling, back as a mode. `None` for a spelling this binary does not know,
    /// which is a document written by a NEWER build rather than a corrupt one — the caller must
    /// refuse rather than guess, because guessing "it kept a ledger" would resume against a file
    /// whose format this binary has never seen.
    pub(super) fn from_recorded(s: &str) -> Option<Self> {
        [KeepTrials::None, KeepTrials::Scalars, KeepTrials::Returns]
            .into_iter()
            .find(|k| k.as_str() == s)
    }

    /// The bucket capture this mode arms on the evaluator. DISARMED for every mode but `returns`,
    /// so nobody who did not ask for the statistics retains a single float.
    pub(super) fn return_buckets(self) -> harness::sweep::ReturnBuckets {
        match self {
            KeepTrials::Returns => harness::sweep::ReturnBuckets::DEFAULT,
            KeepTrials::None | KeepTrials::Scalars => harness::sweep::ReturnBuckets::DISARMED,
        }
    }
}

/// The accepted `--keep-trials` values, rendered for a usage line — ONE roster, so the accepting
/// match and the message it refuses with cannot name different sets.
const KEEP_TRIALS_EXPECTED: &str = "expected none|scalars|returns";

/// `--keep-trials none|scalars|returns`, defaulting to `scalars`.
pub(super) fn parse_keep_trials(args: &[String]) -> Result<KeepTrials, String> {
    match required_value(args, "--keep-trials", KEEP_TRIALS_EXPECTED)?.as_deref() {
        None => Ok(KeepTrials::Scalars),
        Some(v) if v.eq_ignore_ascii_case("none") => Ok(KeepTrials::None),
        Some(v) if v.eq_ignore_ascii_case("scalars") => Ok(KeepTrials::Scalars),
        Some(v) if v.eq_ignore_ascii_case("returns") => Ok(KeepTrials::Returns),
        // A NAMED refusal, not the generic invalid-value line below: `series` is a word the design
        // documents, so an operator who wrote it made no typo and needs the reason.
        //
        // ⚠ **THIS SENTENCE WAS REWRITTEN, and the rewrite is the point.** It used to argue that a
        // per-trial curve was unreachable BECAUSE `row_from_outcome` drops it — which stopped being
        // the whole truth the moment `ReturnBuckets` reached into that same function. A refusal
        // that has become false is worse than no refusal: an operator reading the old text would
        // conclude the statistics were impossible, when the mode that computes them is one word
        // away. So this now refuses the CURVE on cost grounds and names what IS available.
        //
        // ⚠ Both numbers are INTERPOLATED, never typed: the worker cap from
        // `harness::sweep::threads::DEFAULT_SWEEP_THREADS` and the per-trial bucket count from
        // `harness::sweep::ReturnBuckets::DEFAULT_BUCKETS`. They are consts in another file, and a
        // hardcoded copy here would rot silently — `crates/vike-backtest/CLAUDE.md` cites the first
        // as a SYMBOL for exactly that reason.
        Some(v) if v.eq_ignore_ascii_case("series") => Err(format!(
            "--keep-trials series keeps a whole equity CURVE per trial and is still not available \
             — but what it was usually wanted FOR now is: `--keep-trials returns` retains a \
             {}-bucket return vector per trial and writes the anti-overfitting statistics (PBO via \
             CSCV, effective trial count, deflated Sharpe) into the search's report.json, where \
             `backtest gate --fail-if overfit.pbo:+10%` can name them. A curve is ~40x that \
             retention per trial, on a box whose sweep worker cap is already {} because each \
             concurrent point materialises its own data slice, and nothing in this tree reads a \
             per-trial curve. Use `--keep-trials returns` for the statistics, or re-run the \
             winning point on its own for a curve",
            harness::sweep::ReturnBuckets::DEFAULT_BUCKETS,
            harness::sweep::threads::DEFAULT_SWEEP_THREADS
        )),
        Some(v) => Err(format!("invalid --keep-trials {v:?} ({KEEP_TRIALS_EXPECTED})")),
    }
}

/// Parse `--rank-by`, `--optimizer` and the per-method knobs out of argv. PURE — no store, no
/// profile, no environment, no clock.
pub(super) fn parse_search_flags(args: &[String]) -> Result<SearchFlags, String> {
    // ⚠ `--search` IS RETIRED, IT IS REFUSED RATHER THAN ALIASED, AND IT IS CHECKED FIRST.
    //
    // The argument, at the call site because the brief asked for a decision and this is where it is
    // made. An alias cannot be made safe here: both spellings can be given with DIFFERENT values,
    // so every resolution of `--optimizer tpe --search euler` is a guess — and today's guess
    // ("`--optimizer` wins, silently") IS the first defect. An alias either keeps that precedence,
    // in which case the defect survives under a new name, or adds a conflict error, at which point
    // the operator must learn `--optimizer` anyway and the alias bought a permanent second spelling
    // for nothing. Deleting the second selector fixes it BY CONSTRUCTION rather than by adding a
    // check: after this there is only one selector, so there is nothing left for it to disagree
    // with. It also costs no compatibility — nothing in this tree passes `--search`, and the two
    // arm that SPAWNS this binary (`crates/vike-cli/src/cmd/backtest.rs`) sends only
    // `--profile`/`--rank-by`/`--optimizer` and the method knobs — never `--search`.
    //
    // The refusal echoes what the operator typed and names the replacement, which is
    // `vike_config::refuse_removed_env`'s shape applied to a flag. Checked FIRST, so
    // `--optimizer tpe --search bogus` refuses on `--search` — that argv IS the first defect.
    if flag_given(args, "--search") {
        let written = args
            .iter()
            .find(|a| *a == "--search" || a.starts_with("--search="))
            .cloned()
            .unwrap_or_else(|| "--search".to_string());
        return Err(format!(
            "--search is retired — the flag is now --optimizer grid|euler|tpe|genetic. You wrote \
             {written:?}; write `--optimizer <method>` instead"
        ));
    }

    // ⚠ ONE widening worth naming: an INVALID `--rank-by` value is now refused on a profile with no
    // `[paramscan]` table too, where the ladder — which lived inside `if profile.is_paramscan()` — ignored
    // it. A VALID value's documented ignore is untouched; what changed is that a typo is no longer
    // silently swallowed on one of the two profile shapes. Nothing spawns a bogus one:
    // `crates/vike-cli/src/cmd/backtest.rs` validates the value in its own parser before it builds
    // an argv at all.
    //
    // ⚠ RESOLVED BY `search::select`, not here: the same five names and the same refusal
    // sentence reach a REMOTE run through `crate::compute_server`, and one message for two routes
    // is what that module exists for.
    let rank = select::resolve_rank(arg(args, "--rank-by").as_deref())?;

    // The method NAME. The default is read off `GridSearch` rather than spelled, so the flag's
    // default and `Optimizer::name` cannot drift into two literals.
    //
    // ⚠ Through [`required_value`], NOT through `arg`: a trailing bare `--optimizer` answers `None`
    // there, which is "no flag given" and therefore the GRID — the flag's default silently
    // overruling the flag. That doc carries the argument; this is the one line that has to use it.
    //
    // ⚠ The `expected …` text is RENDERED from `vike_datahub_client::SEARCH_METHODS`, the one
    // roster four surfaces read, so this message and the check cannot name different sets.
    let expected = format!("expected {}", SEARCH_METHODS.join("|"));
    let named = required_value(args, "--optimizer", &expected)?;
    let mut requested = named.is_some();
    let name = named.clone().unwrap_or_else(|| DEFAULT_SEARCH_METHOD.to_string());

    // ⚠ OWNERSHIP BEFORE VALUE, on argv PRESENCE — the one half of the rule that cannot move into
    // `search::select`, because presence is an argv concept (`flag_given` sees both `--trials 8` and
    // `--trials=8`, and a bare trailing `--trials` is refused by `required_value` below). The TABLE
    // and the MESSAGE are the shared ones, so there is no second rule — only a second detector, and
    // `select::resolve` performs the same check over `Option` presence for the wire.
    for (flag, owners) in METHOD_KNOBS {
        if flag_given(args, flag) {
            requested = true;
            if !owners.iter().any(|o| name.eq_ignore_ascii_case(o)) {
                return Err(select::refuse_unowned_knob(flag, owners, &name));
            }
        }
    }

    // ⚠ NOT in `METHOD_KNOBS`: neither flag is owned by a METHOD — both are properties of the
    // search's ARTIFACT, and every method has one. They still set `requested`, so
    // `backtest run.toml --resume …` on a profile with no `[paramscan]` table refuses instead of
    // silently running one backtest — defect (e)'s rule, which is about what work was ASKED FOR.
    let keep = parse_keep_trials(args)?;
    let resume = required_value(args, "--resume", "expected a search run id")?;
    if let Some(id) = resume.as_deref()
        && id.trim().is_empty()
    {
        return Err(
            "--resume was given an empty run id — write `--resume <search-run-id>`, which \
                    `backtest trials` and the `search saved to …` line both print"
                .to_string(),
        );
    }
    // ⚠ **THE COMBINATION THAT DESTROYS THE ARTIFACT, refused HERE and not deeper.** `--resume`
    // re-opens a parent run and `open_search_run` then rewrites its `search.json`; with
    // `--keep-trials none` that header would come back saying the search kept no ledger, while
    // `trials.jsonl` sat intact beside it. The lasting damage is not the self-contradictory
    // document — it is that EVERY later `--resume` of that id is then refused with "that search ran
    // with --keep-trials none, so it kept no ledger and there is nothing to resume", which is
    // FALSE, and whose only stated remedy is to re-run the whole search: precisely the hours
    // `--resume` exists to save. One plausible argv — a shell alias, a script that always passes
    // the flag — burns the artifact with exit 0 and no warning.
    //
    // ⚠ Refused rather than SILENTLY PRESERVING the reopened header's `keep_trials`, which was the
    // other available fix. Preserving it obeys NEITHER flag: the operator wrote `--keep-trials
    // none` and the run keeps trials anyway — a DIFFERENT ANSWER rather than a refusal, which is
    // the defect class [`required_value`]'s doc argues against and the one this file has already
    // been bitten by. And the request is genuinely contradictory: a resume whose evaluations are
    // not appended can never complete the ledger it is continuing, so there is no reading under
    // which it does what it says. Refusing at argv triage also means nothing is opened and nothing
    // is rewritten — the artifact cannot be damaged even by the attempt.
    //
    // ⚠ Keyed on [`flag_given`], never on the resolved value: `KeepTrials::None` is reachable only
    // by WRITING the flag, and a DEFAULT must never be refused.
    if resume.is_some() && keep == KeepTrials::None && flag_given(args, "--keep-trials") {
        return Err(
            "--resume and --keep-trials none contradict each other: a resume continues a search's \
             LEDGER, and `none` writes no ledger to continue. It would also overwrite the resumed \
             run's search.json to say it kept no trials, after which every later --resume of that \
             id is refused for a reason that is not true. Drop one of the two flags"
                .to_string(),
        );
    }
    // ⚠ **THE TWO OBSERVERS, and neither is a [`METHOD_KNOBS`] row** — [`SEARCH_PROPERTY_FLAGS`]
    // carries why. Read through [`required_value`] like every other valued flag here, so a trailing
    // bare `--min-trades` is refused rather than read as absent and silently defaulted: the exact
    // defect that doc calls "(d)'s last spelling", and the one a script writing
    // `--min-trades $FLOOR` with `FLOOR` unset produces.
    //
    // ⚠ Both VALUES are resolved by `search::select`, not here, for the reason that module
    // exists: the floor's "`0` disarms" rule and the progress refusal that RENDERS
    // `ProgressMode::NAMES` are one implementation, so a third surface arming these cannot answer
    // differently. This file owns only the argv PRESENCE half.
    //
    // ⚠ The `expected …` text for `--progress` is RENDERED from that roster rather than typed, the
    // same property `--optimizer`'s `expected` line buys from `SEARCH_METHODS`: a fourth mode
    // cannot be accepted by the parser and missing from the value-less refusal.
    let min_trades = required_value(args, "--min-trades", "expected a non-negative integer")?;
    let floor = select::resolve_min_trades(min_trades.as_deref())?;
    let progress_expected = format!("expected {}", ProgressMode::NAMES.join("|"));
    let progress_written = required_value(args, "--progress", &progress_expected)?;
    let progress = select::resolve_progress(progress_written.as_deref())?;

    // ⚠ DERIVED from [`SEARCH_PROPERTY_FLAGS`], never re-spelled: `run`'s refusal renders the same
    // array, so a member that set `requested` here and was missing there would make that sentence
    // name no flag at all.
    requested = requested || SEARCH_PROPERTY_FLAGS.iter().any(|f| flag_given(args, f));

    // The three knob VALUES, read through `required_value` so a written-but-value-less spelling is
    // refused here rather than read as absent, then handed to the ONE resolver — which repeats the
    // ownership check over `Option` presence (the only presence a wire frame has) and owns every
    // value parser and every refusal sentence, so a `--local` run and an `--addr` run answer the
    // same way.
    let euler_depth = required_value(args, "--euler-depth", "expected an integer")?;
    let trials = required_value(args, "--trials", "expected a positive integer")?;
    let seed = required_value(args, "--seed", "expected a u64")?;

    let method = select::resolve(&SearchSelection {
        optimizer: named.as_deref(),
        euler_depth: euler_depth.as_deref(),
        trials: trials.as_deref(),
        seed: seed.as_deref(),
    })?;

    Ok(SearchFlags { method, rank, requested, keep, resume, floor, progress })
}
