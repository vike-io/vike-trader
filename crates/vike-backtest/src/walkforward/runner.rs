//! Profile-driven WALK-FORWARD — the `[walkforward]` sibling of [`super::run_paramscan`]'s `[sweep]`,
//! in two drivers over ONE runner: [`run_walkforward`] walks FIXED parameters,
//! [`run_walkforward_optimized`] lets each window search its own training half first. Which
//! question each answers is in their own docs; the `[walkforward]` table chooses between them, and
//! [`super::ProfileWalkforward`] carries the knobs.
//!
//! [`run_walkforward`] is assembly, not new engine code: it resolves the profile's strategy through
//! the SAME `super::ProfileStrategy::build` door, builds the SAME bar-lane [`vike_simulator::StrategyEngineConfig`]
//! and loads the SAME bars as a plain [`super::run_backtest`] of that profile (both through the
//! shared `super::run` helpers, so a walk-forward can never disagree with a single run about what
//! the profile's `[engine]` means), and hands the series to the EXISTING
//! [`crate::walkforward::walk_forward_over_windows`] runner, over the window list
//! [`super::windows::resolve_windows`] produced from whichever `[walkforward]` window form the
//! profile declared.
//!
//! The Studio's walk-forward (`vike_backtesthub::studio_run::run_walkforward_slice`, which the `RunWalkforward`
//! wire verb serves) is this driver too, over the profile `super::ProfileBacktest::for_slice` builds
//! from its slice: what it cannot say is only what its DTO cannot carry.
//!
//! BAR mode, ONE series, deliberately: every window form resolves to INDEX ranges over a single
//! `&[Bar]` series. A tick or multi-symbol profile is a clean
//! [`HarnessError::Validation`], never a silent first-symbol fallback.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_data::HistoryStore;

use crate::harness::run::{
    bar_engine_params, cancelled_run, is_cancelled, load_profile_bars,
    refuse_after_failed_properties,
};
use crate::harness::{HarnessError, ProfileBacktest, report::periods_per_year};
use crate::walkforward::{WalkforwardWindowOutcome, walk_forward_over_windows};
use vike_analytics::result::WalkforwardReport;
use vike_analytics::validation::{WalkforwardMode, WalkforwardSplit};
use vike_model::Strategy;
use vike_model::run_request::replay::ReplayKind;
use vike_simulator::StrategyEngine;

/// What [`walkforward_preflight`] hands a driver: `(windows, symbol, bars, cash, data_warnings,
/// funding, conversions)`, `funding` the whole range's schedule and `conversions` the whole range's
/// conversion pairs (decision 0243, booked), both given to every window's engine. A named alias, not
/// an inline tuple type, to keep clippy's `type_complexity` lint quiet.
type Preflight = (
    Vec<WalkforwardSplit>,
    String,
    Vec<vike_marketdata::Bar>,
    f64,
    Vec<String>,
    crate::harness::run::FundingSchedule,
    Vec<vike_simulator::ConversionSeries>,
);

/// Everything BOTH walk-forward drivers must establish before their first window, in one place.
///
/// Errors: no `[walkforward]` table, an incoherent or unresolvable window form, a tick-mode or
/// multi-series profile, an unresolvable strategy, a symbol the run's ledger currency cannot book
/// (`crate::harness::run::refuse_off_ledger_series`), a store failure, an empty bar slice, or a window
/// form that yields no window over this range — every one a clean [`HarnessError`], raised BEFORE
/// any window loop, because neither driver's closure can return one.
///
/// Returns `(windows, symbol, bars, cash, data_warnings)`. Extracted when the optimizing driver
/// landed, and the
/// reason is a defect this repo has already paid for on this exact surface: the harness door and
/// the Studio door each spelled their own pre-flight, and the pair drifted — one of them ended up
/// enforcing four conditions where the other enforced one, and their annualization silently
/// disagreed by `sqrt(24)`. Two drivers over one runner is fine; two hand-written pre-flights is
/// how they stop meaning the same thing. That argument is also why the
/// [`super::windows::resolve_windows`] call lives HERE rather than in each driver: the window list
/// is the one thing both of them must agree about bar for bar.
///
/// `cancel` reaches the bar load (`load_profile_bars` reads it per series); `None` reads nothing.
fn walkforward_preflight(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    mode: WalkforwardMode,
    cancel: Option<&AtomicBool>,
) -> Result<Preflight, HarnessError> {
    let config = profile.walkforward.as_ref().ok_or_else(|| {
        HarnessError::Validation(
            "profile has no [walkforward] table — a walk-forward needs a window form, e.g. \
             `[walkforward]\nn_splits = 4` or `[walkforward]\ntrain = \"12mo\"\ntest = \"3mo\"`"
                .to_string(),
        )
    })?;
    if profile.data.kind != ReplayKind::Bar {
        return Err(HarnessError::Validation(
            "walk-forward is bar-mode only: the splitter divides ONE bar series by index, which a \
             tick replay does not produce — set data.kind = \"bar\""
                .to_string(),
        ));
    }

    // Pre-resolve BOTH per-window inputs so a bad strategy name / fee schedule / resolution sidecar
    // fails HERE with a real error rather than inside the window closure, which cannot return one.
    // A window loads no series the strategy reads by name (decision 0271), so one that reads any is
    // refused here, before a window runs it reading nothing.
    crate::data_plan::refuse_unplanned_inputs(
        &profile.strategy.build()?.inputs(),
        "a walk-forward",
    )?;
    // ⚠ THE LEDGER GUARD, and this door had none: each window below builds its own `StrategyEngine`
    // straight from `bar_engine_params`, so neither `run_backtest` nor `run_backtest_over_bars` (the
    // two places the guard sits) is on this path, and a `[walkforward]` profile on USDJPY at 150 with
    // `cash = 10000` booked 10,000 "dollars" where the truth is 66.62. Refused here, before any store
    // read and before ANY driver runs a window, so the fixed walk and the optimizing walk (whose
    // control arm also builds engines directly) share it. A symbol whose quote has a pair in
    // `engine.conversion` (decision 0243) is booked through it instead, as in `run_backtest`: the
    // same refusals first (`conversion_quotes`), the same pairs read below.
    let quotes = crate::data_plan::conversion_quotes(profile)?;
    let declared = profile.data.resolved_series();
    crate::harness::run::refuse_off_ledger_series(
        profile,
        declared
            .iter()
            .map(|s| (s.venue.as_str(), s.symbol.as_str()))
            .filter(|(_, s)| crate::data_plan::quoted_in(s, &quotes).is_none()),
    )?;
    // A coin-margined contract is booked in its settle coin, or refused in words, before a window runs.
    let coin = crate::harness::run::profile_run_ledger_coin(profile)?;
    bar_engine_params(profile, store, &Arc::default())?;

    // ⚠ THE DATA PRE-FLIGHT, and this door had none either: `run_backtest` is the one place the gate
    // sat, and no window goes through it, so an armed `data.require_coverage` read nothing on a walk.
    // Once per walk, because the plan is over the whole range every window is cut from.
    let mut data_warnings =
        crate::data_plan::enforce(profile, store.as_ref(), "the store this run opened")?;
    for line in &data_warnings {
        tracing::warn!("{line}");
    }
    // The conversion pairs, read and judged over the WHOLE range every window is cut from, before any
    // price is read, and booked to the run's symbols: `run_backtest`'s own two calls, so each window
    // takes the same rate at each instant by the same rule and the same staleness.
    let mut conversions = crate::data_plan::load_conversions(profile, store.as_ref())?;
    crate::data_plan::book_conversions(profile, &mut conversions, coin.is_some())?;

    let (mut series, funding) = load_profile_bars(profile, store, cancel, &mut data_warnings)?;
    if series.len() != 1 {
        return Err(HarnessError::Validation(format!(
            "walk-forward runs over ONE bar series — this profile resolves {}; name a single \
             symbol",
            series.len()
        )));
    }
    let (symbol, bars) = series.pop().expect("len == 1 checked above");
    if bars.is_empty() {
        return Err(HarnessError::Data(format!(
            "no bars for {symbol} ({}) in the profile's range",
            profile.data.interval
        )));
    }

    // The ONE place a window form becomes indices. `validate` already refused everything that is
    // wrong on the profile's own terms; this call is what adds the two facts only the store can
    // supply — `data.interval` and the series length.
    let windows = super::windows::resolve_windows(config, &bars, &profile.data.interval, mode)?;
    if windows.is_empty() {
        return Err(HarnessError::Validation(format!(
            "the [walkforward] window form yields NO windows over the {} bar(s) of {symbol} in \
             this range — a walk that evaluates nothing must not return a report that looks like \
             one",
            bars.len()
        )));
    }

    Ok((windows, symbol, bars, profile.engine.cash, data_warnings, funding, conversions))
}

/// The `[walkforward]` knobs BOTH drivers may read, resolved in ONE place and BEFORE the
/// pre-flight — they are pure string parses, so a typo should not cost a store read first.
///
/// An ABSENT section answers with the defaults rather than an error, deliberately: every driver
/// runs [`walkforward_preflight`], and that is where "this profile has no `[walkforward]` table"
/// is spelled. Two functions raising the same error is how the two pre-flights this file already
/// merged drifted apart in the first place.
///
/// For a profile that came through [`ProfileBacktest::from_toml_str`] these resolvers cannot fail
/// — `validate` ran every one of them at load. They stay fallible for the hand-built profile that
/// skipped it, exactly as [`super::windows::resolve_windows`] re-checks `n_splits`.
fn walkforward_knobs(
    profile: &ProfileBacktest,
) -> Result<(WalkforwardMode, WalkforwardWindowSearch, super::paramscan::RankMetric), HarnessError>
{
    let Some(config) = profile.walkforward.as_ref() else {
        return Ok((
            WalkforwardMode::Anchored,
            WalkforwardWindowSearch::None,
            super::paramscan::RankMetric::default(),
        ));
    };
    Ok((config.walk_mode()?, config.window_search()?, config.rank_metric()?))
}

/// Walk `profile` forward over its `[walkforward].n_splits` out-of-sample windows and return the
/// stitched [`WalkforwardReport`] — the FIXED-parameter walk.
///
/// Each window runs fresh at `engine.cash` and the OOS curves are rebased onto one running equity
/// (the [`walk_forward_over_windows`] stitch). Every window uses the SAME `[strategy.params]`, so this
/// answers "were these parameters stable out of sample?" and nothing more. The window that
/// searches its own train half is [`run_walkforward_optimized`].
///
/// It honours `[walkforward].mode` for the same reason it honours `n_splits` — a profile knob that
/// only SOME drivers read is a knob whose meaning depends on which verb you invoked — but the mode
/// cannot move this report: [`WalkforwardMode::Anchored`] and [`WalkforwardMode::Rolling`] differ in
/// `train_start` alone, and this driver's closure discards the training half. That is an
/// invariance, not an intention, so it is pinned by
/// `both_walk_modes_leave_the_fixed_walk_report_identical` below rather than asserted here and
/// left unchecked.
///
/// # The annualization, and the claim that was false before it
///
/// It is [`periods_per_year`] — for a bar profile, [`vike_analytics::report::periods_per_year_for_interval`]
/// over `data.interval`. The Studio's walk-forward
/// (`vike_backtesthub::studio_run::run_walkforward_slice_with_params`) is this module's driver over its
/// slice's profile, so the two doors agree because they are one walk, not because two sides were
/// written to match.
///
/// ⚠ This used to read "the mode is always `Anchored` and the annualization is the same two
/// derivations the Studio's walk-forward makes, so a profile cannot disagree with the Studio",
/// and BOTH halves have since stopped being true — in opposite directions, which is why the
/// sentence is gone rather than trimmed. The ANNUALIZATION half was false when written: the Studio
/// passed a bare `252.0` whatever the interval, so the same strategy over the same 1h bars came
/// back with an `out_of_sample_sharpe` differing by `sqrt(24) ≈ 4.9x` between this door and the Studio's —
/// `sqrt(1440) ≈ 37.9x` on the 1m bars the roundtrip fixture uses. Two doc comments asserted the
/// agreement and nothing in the tree compared the planes. The MODE half was true then and is
/// false now: `[walkforward].mode` is settable, and this driver honours it (see above).
///
/// What holds the annualization now is TWO tests, and the split is worth knowing because neither
/// alone is enough: `crates/vike-backtesthub/tests/walkforward_annualization.rs` drives the STUDIO
/// door end to end and pins both the factor it applies and the fact that both doors' DERIVATIONS
/// agree; this module's own
/// `the_harness_door_applies_the_derived_annualization_not_the_daily_anchor` pins that THIS door
/// applies its resolver rather than a literal — the one argument the cross-plane test cannot see.
/// Both run at a non-daily interval deliberately: at `"1d"` the derived factor and a hardcoded
/// `252` are the same number, so a daily fixture gates nothing here. These sentences describe
/// those tests; they are never the guarantee themselves.
pub fn run_walkforward(
    profile: &ProfileBacktest,
    // The same `Arc<dyn HistoryStore + Send + Sync>` seam every other harness entry point takes, so
    // this compiles against the trait with no DataFusion (the harness is feature-free; the
    // `hist-replay`/`datafusion-store` features died in the 2026-09-27 collapse and the move to `vike-backtesthub`) and any
    // backend can drive it.
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<WalkforwardReport, HarnessError> {
    run_walkforward_cancellable(profile, store, None)
}

/// [`run_walkforward`] with a STOP FLAG — what the backtesthub's `RunWalkforwardProfile` arm
/// calls with the connection's peer-gone flag (`vike_backtesthub::backtesthub_server`'s `PeerWatch`). `None` is
/// [`run_walkforward`] exactly.
///
/// The flag is read per series load in the pre-flight and once per WINDOW, before that window's
/// engine is built: a window that finds it set runs nothing (an empty result stands in for it) and
/// the walk returns [`cancelled_run`] instead of a report stitched from the windows that did run. A
/// window already running finishes.
pub fn run_walkforward_cancellable(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    cancel: Option<&AtomicBool>,
) -> Result<WalkforwardReport, HarnessError> {
    let (mode, ..) = walkforward_knobs(profile)?;
    let (windows, symbol, bars, cash, data_warnings, funding, conversions) =
        walkforward_preflight(profile, &store, mode, cancel)?;
    let mut stopped = false;
    // Across every window: one fill anywhere in the walk that skipped its grid taints the stitch.
    let properties_error = Arc::default();
    let report = walk_forward_over_windows(
        &bars,
        &windows,
        cash,
        periods_per_year(profile),
        // `_train` is ignored deliberately: this verb is the FIXED-parameter walk — an
        // out-of-sample stability check, not an optimize-then-test protocol, exactly as the
        // shipped profile template tells operators. The window that searches its train half is a
        // different driver.
        |_train, window| {
            if is_cancelled(cancel) {
                stopped = true;
                return WalkforwardWindowOutcome::fixed(vike_analytics::BacktestResult::default());
            }
            // Both rebuilt per window: `StrategyEngineConfig` is not `Clone` (it can carry a
            // `Box<dyn PositionSizer>` / properties closure) and each window needs a fresh
            // strategy. Both were proven constructible above, so neither `expect` can fire.
            let strategy = profile.strategy.build().expect("strategy pre-resolved above");
            let mut params = bar_engine_params(profile, &store, &properties_error)
                .expect("engine params pre-built above");
            params.funding_schedule = funding.clone();
            params.conversions = conversions.clone();
            WalkforwardWindowOutcome::fixed(
                StrategyEngine::new(vec![(symbol.clone(), window.to_vec())], strategy, params)
                    .run(),
            )
        },
    );
    if stopped {
        return Err(cancelled_run());
    }
    refuse_after_failed_properties(&properties_error)?;
    Ok(labelled(report, profile, data_warnings))
}

/// Which search a window runs on its OWN training half. A profile names it as
/// `[walkforward].search` ([`super::ProfileWalkforward::window_search`] is the parse, and the one place
/// the accepted spellings live); a caller driving [`run_walkforward_optimized_with`] passes it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WalkforwardWindowSearch {
    /// **The control, and it is a first-class mode rather than "omit the flag" deliberately.**
    ///
    /// Every window trains the profile's own `[strategy.params]`, so the report is the
    /// fixed-parameter walk's — reached through the optimizing driver, which is what makes it a
    /// comparison rather than a different program.
    ///
    /// It exists because our own cohort study measured the alternative and found nothing: across
    /// seven selection criteria, "no search at all" landed inside ONE seed's noise band at roughly
    /// a ninth of the cost, per-fold in-sample↔out-of-sample R² came out ≈ 0.01, and the model size
    /// selection bought carried r = 0.003 to out-of-sample over 406 folds. A walk-forward optimizer
    /// without its own control produces confident numbers with no resolving power, so the control
    /// ships with the instrument.
    #[default]
    None,
    /// The profile's `[sweep]` grid, expanded once and scored on each window's train half; the
    /// best-scoring point — and only it — is then run on that window's validation half.
    Sweep,
}

impl WalkforwardWindowSearch {
    /// Case-insensitive parse of the accepted spellings: `"none"` | `"paramscan"` (the table's word;
    /// `"sweep"` is refused since the owner's ruling 20, phase 4). `None` for
    /// anything else — the CALLER turns that into a refusal naming its own key, because the two
    /// doors that reach this knob spell it differently (`[walkforward].search` in a profile, the
    /// Studio DTO's own search field on the wire) and a shared parser must not pretend to know
    /// which one the reader wrote.
    ///
    /// ⚠ **Extracted so the two doors cannot drift on SPELLING**, the same move
    /// [`super::ProfileWalkforward::rank_metric`] already makes by parsing through
    /// [`super::paramscan::RankMetric::from_str_ci`] rather than a second `match` over the same
    /// strings. It deliberately does NOT absorb the profile door's named `"grid"` arm: that
    /// message tells a reader to edit a TOML key that the Studio door does not have, so it stays
    /// where the key is.
    pub fn from_str_ci(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(WalkforwardWindowSearch::None),
            "paramscan" => Some(WalkforwardWindowSearch::Sweep),
            _ => None,
        }
    }

    /// Whether this value actually SEARCHES. [`WalkforwardWindowSearch::None`] is a first-class control rather
    /// than an absence (see its own doc), so "is a search selected" and "was a value written" are
    /// different questions and this answers the first.
    pub fn searches(self) -> bool {
        matches!(self, WalkforwardWindowSearch::Sweep)
    }
}

/// Walk `profile` forward, SEARCHING inside each window, with every knob read from the profile's
/// own `[walkforward]` table — the door an operator's TOML reaches, and the one the compute-to-data
/// `RunWalkforwardProfile` verb serves without learning a new argument.
///
/// `search`, `mode` and `rank_by` resolve through [`super::ProfileWalkforward`]'s own resolvers (this
/// module's `walkforward_knobs`), so a profile that came through
/// [`ProfileBacktest::from_toml_str`] has already had all three accepted at load, and
/// `search = "sweep"` with no `[sweep]` table was refused there too. What the search MEANS is
/// [`run_walkforward_optimized_with`]'s doc — this is the argument-free spelling of it, not a
/// second implementation.
///
/// The pair is [`super::run_paramscan`] / [`super::run_paramscan_with`] wearing the same clothes: the plain
/// name takes the closed, TOML-nameable choices (a [`super::paramscan::RankMetric`] a profile can spell
/// by string), the `_with` twin takes the arbitrary [`crate::search::objective::SearchObjective`] that a
/// `Box<dyn Fn>` can never be spelled as in a config file.
pub fn run_walkforward_optimized(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<WalkforwardReport, HarnessError> {
    run_walkforward_optimized_cancellable(profile, store, None)
}

/// [`run_walkforward_optimized`] with a STOP FLAG — the backtesthub's `RunWalkforwardProfile`
/// arm's searched route, as [`run_walkforward_cancellable`] is its fixed one. `None` is
/// [`run_walkforward_optimized`] exactly.
///
/// The flag is read once per candidate POINT on each window's training half — the same one relaxed
/// load `crate::harness::SearchEvaluatorBars::with_cancel` makes per point — and once per window before
/// its validation run. A point that finds it set scores `NaN` without running, so the search runs
/// out of work at once, and the walk returns [`cancelled_run`] rather than the "every candidate
/// failed" refusal those `NaN`s would otherwise read as.
pub fn run_walkforward_optimized_cancellable(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    cancel: Option<&AtomicBool>,
) -> Result<WalkforwardReport, HarnessError> {
    let (mode, search, metric) = walkforward_knobs(profile)?;
    optimized_walk(profile, store, search, mode, &metric.objective(), cancel)
}

/// Walk `profile` forward, SEARCHING inside each window: score the `[sweep]` grid on the window's
/// training half, carry the winner (and only the winner) onto its validation half, and record what
/// each window chose in [`vike_analytics::result::WalkforwardWindow::chosen_params`].
///
/// This is the protocol the term "walk-forward" usually means, and it is a different question from
/// [`run_walkforward`]'s: that one asks whether FIXED parameters were stable out of sample, this
/// one asks whether the PROCEDURE of fit-then-trade survives out of sample. Both stitch through the
/// same runner and return the same report type, so the two are comparable by construction — which
/// is the entire point of [`WalkforwardWindowSearch::None`] living here rather than in a second binary.
///
/// # What it refuses, and why each refusal is not a nicety
///
/// * [`WalkforwardWindowSearch::Sweep`] with no `[sweep]` table is refused rather than degenerating to the
///   control. `super::paramscan::expand_paramscan` answers a table-less profile with ONE candidate, so the
///   search would silently become "no search" while the report still said it had optimized.
///   [`ProfileBacktest::validate`] refuses the same pairing at LOAD when the profile SPELLS it
///   (`search = "sweep"`), which is strictly earlier and strictly better — this check survives for
///   the two paths that reach here having skipped that one: a hand-built profile, and a caller
///   passing [`WalkforwardWindowSearch::Sweep`] to [`run_walkforward_optimized_with`] over a profile whose
///   table says otherwise.
/// * A window in which EVERY candidate failed is a hard error, not a skipped window. An optimizer
///   that quietly evaluates nothing and reports a number is the same failure shape as a zero-split
///   walk returning an all-zeros report with `Ok` — a run that never happened, wearing the clothes
///   of one that did.
/// * ONE candidate that could not read its instrument grid (`snap_to_properties`) fails the walk,
///   where one the strategy rejected only loses its window: the winner would otherwise come from
///   fewer points than the grid named, in a report with nothing to say so.
///
/// The `objective` is the same `Box<dyn Fn(&BacktestReport) -> f64>` the sweep and TPE lanes rank
/// with, and a failed candidate steers with `NaN`, which `vike_analytics::metrics::cmp_scores_desc` sorts
/// LAST — so a point that could not run can never win a window.
///
/// # Why the three parameters, when the profile can name them
///
/// `objective` is the reason this door exists at all: it is a `Box<dyn Fn>`, so a TOML file can
/// only ever name the ones something else named for it, and a caller holding the composite
/// `multi_metric` (or any objective of its own) has no other way in. `search` and `mode` ride
/// alongside so such a caller does not have to synthesize a `[walkforward]` table just to say what
/// it already knows. Everything else — `n_splits`, the whole `[engine]` surface, the data slice —
/// stays the profile's, because those DO have TOML spellings and a second source for them would be
/// a second answer to the same question. [`run_walkforward_optimized`] is the profile-driven door.
pub fn run_walkforward_optimized_with(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    search: WalkforwardWindowSearch,
    mode: WalkforwardMode,
    objective: &crate::search::objective::SearchObjective,
) -> Result<WalkforwardReport, HarnessError> {
    optimized_walk(profile, store, search, mode, objective, None)
}

/// The body of [`run_walkforward_optimized_with`], with the stop flag
/// [`run_walkforward_optimized_cancellable`] documents. `None` reads nothing.
fn optimized_walk(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    search: WalkforwardWindowSearch,
    mode: WalkforwardMode,
    objective: &crate::search::objective::SearchObjective,
    cancel: Option<&AtomicBool>,
) -> Result<WalkforwardReport, HarnessError> {
    let (windows, symbol, bars, cash, data_warnings, funding, conversions) =
        walkforward_preflight(profile, &store, mode, cancel)?;

    if search == WalkforwardWindowSearch::Sweep && !profile.is_paramscan() {
        return Err(HarnessError::Validation(
            "walk-forward optimization needs a [paramscan] table to search — this profile has none, \
             so every window would 'select' the one parameter set it already has. Add the grid, \
             or run the fixed-parameter walk instead."
                .to_string(),
        ));
    }

    // Expanded ONCE, outside the window loop: `expand_paramscan` reads only the profile, so a
    // per-window expansion would rebuild an identical list `n_splits` times.
    let points = super::paramscan::expand_paramscan(profile)?;

    // The window closure cannot return an error, so a failure is captured here and raised after
    // the walk — never swallowed into a report.
    let mut failed_window: Option<String> = None;
    let mut stopped = false;
    // Across the whole walk, the search included: a candidate that could not read its instrument
    // grid scores `NaN` like one the strategy rejected, and the others would then win a window that
    // never compared them. The refusal below is what tells the two apart.
    let properties_error = Arc::default();

    let report = walk_forward_over_windows(
        &bars,
        &windows,
        cash,
        periods_per_year(profile),
        |train, window| {
            if is_cancelled(cancel) {
                stopped = true;
                return WalkforwardWindowOutcome {
                    result: vike_analytics::BacktestResult::default(),
                    chosen_params: None,
                };
            }
            let winner = match search {
                // The control: the profile's own params, no scoring, one run.
                WalkforwardWindowSearch::None => None,
                WalkforwardWindowSearch::Sweep => {
                    let train_series = vec![(symbol.clone(), train.to_vec())];
                    let mut scored: Vec<(usize, f64, Option<String>)> =
                        super::paramscan::map_bounded(
                            points.iter().enumerate().collect::<Vec<_>>(),
                            |(i, point)| {
                                // A point that finds the flag set runs nothing and scores `NaN`, so
                                // the window's search runs out of work at once.
                                if is_cancelled(cancel) {
                                    return (i, f64::NAN, None);
                                }
                                let (row, score) =
                                    super::paramscan::grid::evaluation_scored_point_over_bars(
                                        profile,
                                        &store,
                                        train_series.clone(),
                                        &funding,
                                        &conversions,
                                        objective,
                                        &point.profile,
                                        point.overrides.clone(),
                                        &properties_error,
                                    );
                                (i, score, row.error)
                            },
                        );
                    // In grid order, before the sort: the cause a refusal names does not depend on
                    // which point the pool finished first. `None` when every point ran and none
                    // could be ranked.
                    let cause = scored.iter().find_map(|(.., error)| error.clone());
                    // The ONE ordering rule every scored lane in this crate shares — NaN
                    // ("unrankable", i.e. the candidate failed) sorts last, so it cannot win.
                    scored.sort_by(|a, b| vike_analytics::metrics::cmp_scores_desc(a.1, b.1));
                    match scored.first() {
                        Some(&(i, s, _)) if s.is_finite() => Some(i),
                        _ => {
                            failed_window.get_or_insert_with(|| {
                                let cause = cause
                                    .map_or(String::new(), |c| format!(": the first said {c}"));
                                format!(
                                    "every one of the {} candidate(s) failed on the training half \
                                     of the window ending at bar {}{cause}",
                                    points.len(),
                                    window.len()
                                )
                            });
                            None
                        }
                    }
                }
            };

            // Read again before the validation run: a flag set DURING the search left a winner
            // chosen from the points that happened to finish, which is no winner at all.
            if is_cancelled(cancel) {
                stopped = true;
                return WalkforwardWindowOutcome {
                    result: vike_analytics::BacktestResult::default(),
                    chosen_params: None,
                };
            }
            let chosen = winner.map(|i| points[i].overrides.clone());
            let run_profile = winner.map_or(profile, |i| &points[i].profile);
            let result = super::run::run_backtest_over_bars(
                run_profile,
                &store,
                vec![(symbol.clone(), window.to_vec())],
                &funding,
                &conversions,
                &properties_error,
            )
            .unwrap_or_else(|e| {
                // Pre-flight proved the strategy and the engine params constructible for the base
                // profile, and every candidate profile differs only in `[strategy.params]` values
                // the grid supplied — but a value the STRATEGY rejects is still reachable, and so
                // is a failed read of the instrument grid, so this is recorded rather than unwrapped.
                failed_window.get_or_insert_with(|| {
                    format!(
                        "the selected point failed on its own validation window: {}",
                        e.message()
                    )
                });
                vike_analytics::BacktestResult::default()
            });
            WalkforwardWindowOutcome { result, chosen_params: chosen }
        },
    );

    // ⚠ The stop FIRST: the `NaN`s a cancelled search scores are the cancellation's doing, and
    // reporting them as "every candidate failed" would blame the profile.
    if stopped {
        return Err(cancelled_run());
    }
    match failed_window {
        Some(why) => Err(HarnessError::Data(format!("walk-forward optimization: {why}"))),
        None => {
            refuse_after_failed_properties(&properties_error)?;
            Ok(labelled(report, profile, data_warnings))
        }
    }
}

/// `report` carrying the ledger currency of the profile that produced it, when that is not the
/// dollar: the stitched equity curve is an amount of it, and a report that did not say so would be
/// read as dollars. Both drivers end here, so the two protocols label alike, and carry what the
/// pre-flight found in the data and the strategy file they ran alike.
fn labelled(
    mut report: WalkforwardReport,
    profile: &ProfileBacktest,
    data_warnings: Vec<String>,
) -> WalkforwardReport {
    report.account_currency = crate::harness::run::ledger_label(profile);
    report.data_warnings = data_warnings;
    report.strategy_file = profile.strategy.file.clone();
    report.strategy_sha256 = profile.strategy.sha256().to_string();
    report
}

#[path = "runner_tests.rs"]
#[cfg(test)]
mod runner_tests;
