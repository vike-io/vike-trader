//! Profile-driven WALK-FORWARD — the `[walkforward]` sibling of [`super::run_paramscan`]'s `[sweep]`,
//! in two drivers over ONE runner: [`run_walkforward`] walks FIXED parameters,
//! [`run_walkforward_optimized`] lets each window search its own training half first. Which
//! question each answers is in their own docs; the `[walkforward]` table chooses between them, and
//! [`super::WalkforwardCfg`] carries the knobs.
//!
//! [`run_walkforward`] is assembly, not new engine code: it resolves the profile's strategy through
//! the SAME [`super::strategy_by_name`] registry, builds the SAME bar-lane [`vike_sim::EngineParams`]
//! and loads the SAME bars as a plain [`super::run_backtest`] of that profile (both through the
//! shared `super::run` helpers, so a walk-forward can never disagree with a single run about what
//! the profile's `[engine]` means), and hands the series to the EXISTING
//! [`crate::walkforward::walk_forward_over_windows`] runner, over the window list
//! [`super::windows::resolve_windows`] produced from whichever `[walkforward]` window form the
//! profile declared.
//!
//! What that buys over the Studio's DTO-shaped walk-forward
//! (`vike_studio_core::run_walkforward_slice`, which the `RunWalkforward` wire verb serves): the
//! whole `[engine]` surface — the `fee` SCHEDULE, `[engine.impact]`, `[engine.resolution]`,
//! `[risk]`, `snap_to_properties`, `attach_funding` — instead of only the three flat
//! `cash`/`fee_rate`/`slippage` scalars a `WireEngineParams` can carry.
//!
//! BAR mode, ONE series, deliberately: every window form resolves to INDEX ranges over a single
//! `&[Bar]` series. A tick or multi-symbol profile is a clean
//! [`HarnessError::Validation`], never a silent first-symbol fallback.

use std::sync::Arc;

use vike_data::HistStore;

use crate::harness::run::{bar_engine_params, load_profile_bars};
use crate::harness::{
    BacktestProfile, DataKind, HarnessError, report::periods_per_year, strategy_by_name,
};
use crate::walkforward::{WalkForwardReport, WindowOutcome, walk_forward_over_windows};
use vike_analytics::validation::{Split, WalkMode};
use vike_sim::StrategyEngine;

/// Everything BOTH walk-forward drivers must establish before their first window, in one place.
///
/// Errors: no `[walkforward]` table, an incoherent or unresolvable window form, a tick-mode or
/// multi-series profile, an unresolvable strategy, a store failure, an empty bar slice, or a window
/// form that yields no window over this range — every one a clean [`HarnessError`], raised BEFORE
/// any window loop, because neither driver's closure can return one.
///
/// Returns `(windows, symbol, bars, cash)`. Extracted when the optimizing driver landed, and the
/// reason is a defect this repo has already paid for on this exact surface: the harness door and
/// the Studio door each spelled their own pre-flight, and the pair drifted — one of them ended up
/// enforcing four conditions where the other enforced one, and their annualization silently
/// disagreed by `sqrt(24)`. Two drivers over one runner is fine; two hand-written pre-flights is
/// how they stop meaning the same thing. That argument is also why the
/// [`super::windows::resolve_windows`] call lives HERE rather than in each driver: the window list
/// is the one thing both of them must agree about bar for bar.
fn walkforward_preflight(
    profile: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    mode: WalkMode,
) -> Result<(Vec<Split>, String, Vec<vike_model::Bar>, f64), HarnessError> {
    let cfg = profile.walkforward.as_ref().ok_or_else(|| {
        HarnessError::Validation(
            "profile has no [walkforward] table — a walk-forward needs a window form, e.g. \
             `[walkforward]\nn_splits = 4` or `[walkforward]\ntrain = \"12mo\"\ntest = \"3mo\"`"
                .to_string(),
        )
    })?;
    if profile.data.kind != DataKind::Bar {
        return Err(HarnessError::Validation(
            "walk-forward is bar-mode only: the splitter divides ONE bar series by index, which a \
             tick replay does not produce — set data.kind = \"bar\""
                .to_string(),
        ));
    }

    // Pre-resolve BOTH per-window inputs so a bad strategy name / fee schedule / resolution sidecar
    // fails HERE with a real error rather than inside the window closure, which cannot return one.
    strategy_by_name(&profile.strategy.name, &profile.strategy.params)?;
    bar_engine_params(profile, store)?;

    let mut series = load_profile_bars(profile, store)?;
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
    let windows = super::windows::resolve_windows(cfg, &bars, &profile.data.interval, mode)?;
    if windows.is_empty() {
        return Err(HarnessError::Validation(format!(
            "the [walkforward] window form yields NO windows over the {} bar(s) of {symbol} in \
             this range — a walk that evaluates nothing must not return a report that looks like \
             one",
            bars.len()
        )));
    }

    Ok((windows, symbol, bars, profile.engine.cash))
}

/// The `[walkforward]` knobs BOTH drivers may read, resolved in ONE place and BEFORE the
/// pre-flight — they are pure string parses, so a typo should not cost a store read first.
///
/// An ABSENT section answers with the defaults rather than an error, deliberately: every driver
/// runs [`walkforward_preflight`], and that is where "this profile has no `[walkforward]` table"
/// is spelled. Two functions raising the same error is how the two pre-flights this file already
/// merged drifted apart in the first place.
///
/// For a profile that came through [`BacktestProfile::from_toml_str`] these resolvers cannot fail
/// — `validate` ran every one of them at load. They stay fallible for the hand-built profile that
/// skipped it, exactly as [`super::windows::resolve_windows`] re-checks `n_splits`.
fn walkforward_knobs(
    profile: &BacktestProfile,
) -> Result<(WalkMode, WindowSearch, super::sweep::RankMetric), HarnessError> {
    let Some(cfg) = profile.walkforward.as_ref() else {
        return Ok((WalkMode::Anchored, WindowSearch::None, super::sweep::RankMetric::default()));
    };
    Ok((cfg.walk_mode()?, cfg.window_search()?, cfg.rank_metric()?))
}

/// Walk `profile` forward over its `[walkforward].n_splits` out-of-sample windows and return the
/// stitched [`WalkForwardReport`] — the FIXED-parameter walk.
///
/// Each window runs fresh at `engine.cash` and the OOS curves are rebased onto one running equity
/// (the [`walk_forward_over_windows`] stitch). Every window uses the SAME `[strategy.params]`, so this
/// answers "were these parameters stable out of sample?" and nothing more. The window that
/// searches its own train half is [`run_walkforward_optimized`].
///
/// It honours `[walkforward].mode` for the same reason it honours `n_splits` — a profile knob that
/// only SOME drivers read is a knob whose meaning depends on which verb you invoked — but the mode
/// cannot move this report: [`WalkMode::Anchored`] and [`WalkMode::Rolling`] differ in
/// `train_start` alone, and this driver's closure discards the training half. That is an
/// invariance, not an intention, so it is pinned by
/// `both_walk_modes_leave_the_fixed_walk_report_identical` below rather than asserted here and
/// left unchecked.
///
/// # The annualization, and the claim that was false before it
///
/// It is [`periods_per_year`] — for a bar profile, [`vike_analytics::report::periods_per_year_for_interval`]
/// over `data.interval`. The Studio's walk-forward
/// (`vike_studio_core::run_walkforward_slice_with_params`) calls that same function over its
/// slice's interval, so the two doors agree because they SHARE one derivation, not because two
/// sides were written to match.
///
/// ⚠ This used to read "the mode is always `Anchored` and the annualization is the same two
/// derivations the Studio's walk-forward makes, so a profile cannot disagree with the Studio",
/// and BOTH halves have since stopped being true — in opposite directions, which is why the
/// sentence is gone rather than trimmed. The ANNUALIZATION half was false when written: the Studio
/// passed a bare `252.0` whatever the interval, so the same strategy over the same 1h bars came
/// back with an `oos_sharpe` differing by `sqrt(24) ≈ 4.9x` between this door and the Studio's —
/// `sqrt(1440) ≈ 37.9x` on the 1m bars the roundtrip fixture uses. Two doc comments asserted the
/// agreement and nothing in the tree compared the planes. The MODE half was true then and is
/// false now: `[walkforward].mode` is settable, and this driver honours it (see above).
///
/// What holds the annualization now is TWO tests, and the split is worth knowing because neither
/// alone is enough: `crates/vike-studio-core/tests/walkforward_annualization.rs` drives the STUDIO
/// door end to end and pins both the factor it applies and the fact that both doors' DERIVATIONS
/// agree; this module's own
/// `the_harness_door_applies_the_derived_annualization_not_the_daily_anchor` pins that THIS door
/// applies its resolver rather than a literal — the one argument the cross-plane test cannot see.
/// Both run at a non-daily interval deliberately: at `"1d"` the derived factor and a hardcoded
/// `252` are the same number, so a daily fixture gates nothing here. These sentences describe
/// those tests; they are never the guarantee themselves.
pub fn run_walkforward(
    profile: &BacktestProfile,
    // The same `Arc<dyn HistStore + Send + Sync>` seam every other harness entry point takes, so
    // this compiles against the trait with no DataFusion (the default-build/`datafusion-store`
    // split — `hist-replay`/`datafusion-store` until the 2026-09-27 feature collapse) and any
    // backend can drive it.
    store: Arc<dyn HistStore + Send + Sync>,
) -> Result<WalkForwardReport, HarnessError> {
    let (mode, ..) = walkforward_knobs(profile)?;
    let (windows, symbol, bars, cash) = walkforward_preflight(profile, &store, mode)?;
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
            // Both rebuilt per window: `EngineParams` is not `Clone` (it can carry a
            // `Box<dyn PositionSizer>` / properties closure) and each window needs a fresh
            // strategy. Both were proven constructible above, so neither `expect` can fire.
            let strategy = strategy_by_name(&profile.strategy.name, &profile.strategy.params)
                .expect("strategy pre-resolved above");
            let params = bar_engine_params(profile, &store).expect("engine params pre-built above");
            WindowOutcome::fixed(
                StrategyEngine::new(vec![(symbol.clone(), window.to_vec())], strategy, params)
                    .run(),
            )
        },
    );
    Ok(report)
}

/// Which search a window runs on its OWN training half. A profile names it as
/// `[walkforward].search` ([`super::WalkforwardCfg::window_search`] is the parse, and the one place
/// the accepted spellings live); a caller driving [`run_walkforward_optimized_with`] passes it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowSearch {
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

impl WindowSearch {
    /// Case-insensitive parse of the two accepted spellings: `"none"` | `"sweep"`. `None` for
    /// anything else — the CALLER turns that into a refusal naming its own key, because the two
    /// doors that reach this knob spell it differently (`[walkforward].search` in a profile, the
    /// Studio DTO's own search field on the wire) and a shared parser must not pretend to know
    /// which one the reader wrote.
    ///
    /// ⚠ **Extracted so the two doors cannot drift on SPELLING**, the same move
    /// [`super::WalkforwardCfg::rank_metric`] already makes by parsing through
    /// [`super::sweep::RankMetric::from_str_ci`] rather than a second `match` over the same
    /// strings. It deliberately does NOT absorb the profile door's named `"grid"` arm: that
    /// message tells a reader to edit a TOML key that the Studio door does not have, so it stays
    /// where the key is.
    pub fn from_str_ci(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(WindowSearch::None),
            "sweep" => Some(WindowSearch::Sweep),
            _ => None,
        }
    }

    /// Whether this value actually SEARCHES. [`WindowSearch::None`] is a first-class control rather
    /// than an absence (see its own doc), so "is a search selected" and "was a value written" are
    /// different questions and this answers the first.
    pub fn searches(self) -> bool {
        matches!(self, WindowSearch::Sweep)
    }
}

/// Walk `profile` forward, SEARCHING inside each window, with every knob read from the profile's
/// own `[walkforward]` table — the door an operator's TOML reaches, and the one the compute-to-data
/// `RunWalkforwardProfile` verb serves without learning a new argument.
///
/// `search`, `mode` and `rank_by` resolve through [`super::WalkforwardCfg`]'s own resolvers (this
/// module's `walkforward_knobs`), so a profile that came through
/// [`BacktestProfile::from_toml_str`] has already had all three accepted at load, and
/// `search = "sweep"` with no `[sweep]` table was refused there too. What the search MEANS is
/// [`run_walkforward_optimized_with`]'s doc — this is the argument-free spelling of it, not a
/// second implementation.
///
/// The pair is [`super::run_paramscan`] / [`super::run_paramscan_with`] wearing the same clothes: the plain
/// name takes the closed, TOML-nameable choices (a [`super::sweep::RankMetric`] a profile can spell
/// by string), the `_with` twin takes the arbitrary [`crate::search::objective::Objective`] that a
/// `Box<dyn Fn>` can never be spelled as in a config file.
pub fn run_walkforward_optimized(
    profile: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
) -> Result<WalkForwardReport, HarnessError> {
    let (mode, search, metric) = walkforward_knobs(profile)?;
    run_walkforward_optimized_with(profile, store, search, mode, &metric.objective())
}

/// Walk `profile` forward, SEARCHING inside each window: score the `[sweep]` grid on the window's
/// training half, carry the winner (and only the winner) onto its validation half, and record what
/// each window chose in [`crate::walkforward::WfWindow::chosen_params`].
///
/// This is the protocol the term "walk-forward" usually means, and it is a different question from
/// [`run_walkforward`]'s: that one asks whether FIXED parameters were stable out of sample, this
/// one asks whether the PROCEDURE of fit-then-trade survives out of sample. Both stitch through the
/// same runner and return the same report type, so the two are comparable by construction — which
/// is the entire point of [`WindowSearch::None`] living here rather than in a second binary.
///
/// # What it refuses, and why each refusal is not a nicety
///
/// * [`WindowSearch::Sweep`] with no `[sweep]` table is refused rather than degenerating to the
///   control. `super::sweep::expand_paramscan` answers a table-less profile with ONE candidate, so the
///   search would silently become "no search" while the report still said it had optimized.
///   [`BacktestProfile::validate`] refuses the same pairing at LOAD when the profile SPELLS it
///   (`search = "sweep"`), which is strictly earlier and strictly better — this check survives for
///   the two paths that reach here having skipped that one: a hand-built profile, and a caller
///   passing [`WindowSearch::Sweep`] to [`run_walkforward_optimized_with`] over a profile whose
///   table says otherwise.
/// * A window in which EVERY candidate failed is a hard error, not a skipped window. An optimizer
///   that quietly evaluates nothing and reports a number is the same failure shape as a zero-split
///   walk returning an all-zeros report with `Ok` — a run that never happened, wearing the clothes
///   of one that did.
///
/// The `objective` is the same `Box<dyn Fn(&BacktestReport) -> f64>` the sweep and TPE lanes rank
/// with, and a failed candidate steers with `NaN`, which `super::sweep::cmp_scores_desc` sorts
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
    profile: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    search: WindowSearch,
    mode: WalkMode,
    objective: &crate::search::objective::Objective,
) -> Result<WalkForwardReport, HarnessError> {
    let (windows, symbol, bars, cash) = walkforward_preflight(profile, &store, mode)?;

    if search == WindowSearch::Sweep && !profile.is_paramscan() {
        return Err(HarnessError::Validation(
            "walk-forward optimization needs a [sweep] table to search — this profile has none, \
             so every window would 'select' the one parameter set it already has. Add the grid, \
             or run the fixed-parameter walk instead."
                .to_string(),
        ));
    }

    // Expanded ONCE, outside the window loop: `expand_paramscan` reads only the profile, so a
    // per-window expansion would rebuild an identical list `n_splits` times.
    let points = super::sweep::expand_paramscan(profile)?;

    // The window closure cannot return an error, so a failure is captured here and raised after
    // the walk — never swallowed into a report.
    let mut failed_window: Option<String> = None;

    let report = walk_forward_over_windows(
        &bars,
        &windows,
        cash,
        periods_per_year(profile),
        |train, window| {
            let winner = match search {
                // The control: the profile's own params, no scoring, one run.
                WindowSearch::None => None,
                WindowSearch::Sweep => {
                    let train_series = vec![(symbol.clone(), train.to_vec())];
                    let mut scored: Vec<(usize, f64)> = super::sweep::map_bounded(
                        points.iter().enumerate().collect::<Vec<_>>(),
                        |(i, point)| {
                            let (_, score) = super::sweep::grid::eval_scored_point_over_bars(
                                profile,
                                &store,
                                train_series.clone(),
                                objective,
                                &point.profile,
                                point.overrides.clone(),
                            );
                            (i, score)
                        },
                    );
                    // The ONE ordering rule every scored lane in this crate shares — NaN
                    // ("unrankable", i.e. the candidate failed) sorts last, so it cannot win.
                    scored.sort_by(|a, b| super::sweep::cmp_scores_desc(a.1, b.1));
                    match scored.first() {
                        Some(&(i, s)) if s.is_finite() => Some(i),
                        _ => {
                            failed_window.get_or_insert_with(|| {
                                format!(
                                    "every one of the {} candidate(s) failed on the training half \
                                     of the window ending at bar {}",
                                    points.len(),
                                    window.len()
                                )
                            });
                            None
                        }
                    }
                }
            };

            let chosen = winner.map(|i| points[i].overrides.clone());
            let run_profile = winner.map_or(profile, |i| &points[i].profile);
            let result = super::run::run_backtest_over_bars(
                run_profile,
                &store,
                vec![(symbol.clone(), window.to_vec())],
            )
            .unwrap_or_else(|_| {
                // Pre-flight proved the strategy and the engine params constructible for the base
                // profile, and every candidate profile differs only in `[strategy.params]` values
                // the grid supplied — but a value the STRATEGY rejects is still reachable, so this
                // is recorded rather than unwrapped.
                failed_window.get_or_insert_with(|| {
                    "the selected point failed on its own validation \
                                             window"
                        .to_string()
                });
                vike_analytics::BacktestResult::default()
            });
            WindowOutcome { result, chosen_params: chosen }
        },
    );

    match failed_window {
        Some(why) => Err(HarnessError::Data(format!("walk-forward optimization: {why}"))),
        None => Ok(report),
    }
}

#[path = "runner_tests.rs"]
#[cfg(test)]
mod runner_tests;
