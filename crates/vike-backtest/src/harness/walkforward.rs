//! Profile-driven WALK-FORWARD — the `[walkforward]` sibling of [`super::run_sweep`]'s `[sweep]`,
//! in two drivers over ONE runner: [`run_walkforward`] walks FIXED parameters,
//! [`run_walkforward_optimized`] lets each window search its own training half first. Which
//! question each answers is in their own docs; the `[walkforward]` table chooses between them, and
//! [`super::WalkforwardCfg`] carries the knobs.
//!
//! [`run_walkforward`] is assembly, not new engine code: it resolves the profile's strategy through
//! the SAME [`super::strategy_by_name`] registry, builds the SAME bar-lane [`crate::EngineParams`]
//! and loads the SAME bars as a plain [`super::run_backtest`] of that profile (both through the
//! shared `super::run` helpers, so a walk-forward can never disagree with a single run about what
//! the profile's `[engine]` means), and hands the series to the EXISTING
//! [`crate::walkforward::walk_forward_strategy`] runner.
//!
//! What that buys over the Studio's DTO-shaped walk-forward
//! (`vike_studio_core::run_walkforward_slice`, which the `RunWalkforward` wire verb serves): the
//! whole `[engine]` surface — the `fee` SCHEDULE, `[engine.impact]`, `[engine.resolution]`,
//! `[risk]`, `snap_to_properties`, `attach_funding` — instead of only the three flat
//! `cash`/`fee_rate`/`slippage` scalars a `WireEngineParams` can carry.
//!
//! BAR mode, ONE series, deliberately: [`crate::walkforward::walk_forward_strategy`] splits a
//! single `&[Bar]` series by INDEX. A tick or multi-symbol profile is a clean
//! [`HarnessError::Validation`], never a silent first-symbol fallback.

use std::sync::Arc;

use vike_data::HistStore;

use super::run::{bar_engine_params, load_profile_bars};
use super::{BacktestProfile, DataKind, HarnessError, report::periods_per_year, strategy_by_name};
use crate::StrategyEngine;
use crate::validation::WalkMode;
use crate::walkforward::{WalkForwardReport, WindowOutcome, walk_forward_strategy};

/// Everything BOTH walk-forward drivers must establish before their first window, in one place.
///
/// Errors: no `[walkforward]` table, a zero split count, a tick-mode or multi-series profile, an
/// unresolvable strategy, a store failure, or an empty bar slice — every one a clean
/// [`HarnessError`], raised BEFORE any window loop, because neither driver's closure can return
/// one.
///
/// Returns `(n_splits, symbol, bars, cash)`. Extracted when the optimizing driver landed, and the
/// reason is a defect this repo has already paid for on this exact surface: the harness door and
/// the Studio door each spelled their own pre-flight, and the pair drifted — one of them ended up
/// enforcing four conditions where the other enforced one, and their annualization silently
/// disagreed by `sqrt(24)`. Two drivers over one runner is fine; two hand-written pre-flights is
/// how they stop meaning the same thing.
fn walkforward_preflight(
    profile: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Result<(usize, String, Vec<vike_model::Bar>, f64), HarnessError> {
    let cfg = profile.walkforward.as_ref().ok_or_else(|| {
        HarnessError::Validation(
            "profile has no [walkforward] table — a walk-forward needs a split count, e.g. \
             `[walkforward]\nn_splits = 4`"
                .to_string(),
        )
    })?;
    // `validate` already rejects `n_splits = 0` at load; re-checked so a hand-built profile that
    // skipped `from_toml_str` cannot reach `walk_forward_splits` with a zero count.
    if cfg.n_splits == 0 {
        return Err(HarnessError::Validation(
            "walkforward.n_splits must be >= 1, got 0".to_string(),
        ));
    }
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

    Ok((cfg.n_splits, symbol, bars, profile.engine.cash))
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
/// skipped it, exactly as [`walkforward_preflight`] re-checks `n_splits`.
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
/// (the [`walk_forward_strategy`] stitch). Every window uses the SAME `[strategy.params]`, so this
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
/// It is [`periods_per_year`] — for a bar profile, [`super::report::periods_per_year_for_interval`]
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
    // this compiles against the trait with no DataFusion (the `hist-replay`/`datafusion-store`
    // split) and any backend can drive it.
    store: Arc<dyn HistStore + Send + Sync>,
) -> Result<WalkForwardReport, HarnessError> {
    let (mode, ..) = walkforward_knobs(profile)?;
    let (n_splits, symbol, bars, cash) = walkforward_preflight(profile, &store)?;
    let report = walk_forward_strategy(
        &bars,
        n_splits,
        mode,
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
/// The pair is [`super::run_sweep`] / [`super::run_sweep_with`] wearing the same clothes: the plain
/// name takes the closed, TOML-nameable choices (a [`super::sweep::RankMetric`] a profile can spell
/// by string), the `_with` twin takes the arbitrary [`crate::objective::Objective`] that a
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
///   control. `super::sweep::expand_sweep` answers a table-less profile with ONE candidate, so the
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
    objective: &crate::objective::Objective,
) -> Result<WalkForwardReport, HarnessError> {
    let (n_splits, symbol, bars, cash) = walkforward_preflight(profile, &store)?;

    if search == WindowSearch::Sweep && !profile.is_sweep() {
        return Err(HarnessError::Validation(
            "walk-forward optimization needs a [sweep] table to search — this profile has none, \
             so every window would 'select' the one parameter set it already has. Add the grid, \
             or run the fixed-parameter walk instead."
                .to_string(),
        ));
    }

    // Expanded ONCE, outside the window loop: `expand_sweep` reads only the profile, so a
    // per-window expansion would rebuild an identical list `n_splits` times.
    let points = super::sweep::expand_sweep(profile)?;

    // The window closure cannot return an error, so a failure is captured here and raised after
    // the walk — never swallowed into a report.
    let mut failed_window: Option<String> = None;

    let report = walk_forward_strategy(
        &bars,
        n_splits,
        mode,
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
                            let (_, score) = super::sweep::eval_scored_point_over_bars(
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
                crate::BacktestResult::default()
            });
            WindowOutcome { result, chosen_params: chosen }
        },
    );

    match failed_window {
        Some(why) => Err(HarnessError::Data(format!("walk-forward optimization: {why}"))),
        None => Ok(report),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // This file's BODY spells it `super::sweep::RankMetric` (the fully-qualified style every
    // `super::sweep::` / `super::run::` call site here uses); a test comparing VALUES wants the
    // bare name, and `harness`'s own re-export is the shortest honest path to it.
    use crate::harness::RankMetric;

    const BASE: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

    /// A two-point `[sweep]` grid over `buy_hold`'s `size`. Every `search = "sweep"` fixture needs
    /// one: the load-time rule below refuses that spelling on a profile with nothing to search.
    const GRID: &str = "[sweep]\nsize = [1.0, 2.0]\n";

    fn profile(extra: &str) -> BacktestProfile {
        BacktestProfile::from_toml_str(&format!("{BASE}{extra}")).unwrap()
    }

    #[test]
    fn the_walkforward_section_parses_and_defaults_to_absent() {
        assert!(profile("").walkforward.is_none(), "no [walkforward] ⇒ None (unchanged profiles)");
        assert_eq!(profile("[walkforward]\nn_splits = 4\n").walkforward.unwrap().n_splits, 4);
    }

    #[test]
    fn a_zero_split_count_is_rejected_at_load() {
        let err = BacktestProfile::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 0\n"))
            .unwrap_err();
        assert!(matches!(err, HarnessError::Validation(ref m) if m.contains("n_splits")), "{err}");
    }

    #[test]
    fn an_unknown_walkforward_key_is_a_parse_error() {
        // `deny_unknown_fields` on the section: a typo'd knob fails at load rather than silently
        // doing nothing (the same contract every other profile section has). Each of the three
        // knobs this section grew is one letter away from a spelling serde would have to reject,
        // and the misspelling of an OPTIONAL field is the dangerous one — it would deserialize to
        // `None` and read exactly like "the operator did not ask for it".
        for junk in ["splits = 3", "serach = \"grid\"", "walk_mode = \"rolling\""] {
            let err = BacktestProfile::from_toml_str(&format!(
                "{BASE}[walkforward]\nn_splits = 2\n{junk}\n"
            ))
            .unwrap_err();
            assert!(matches!(err, HarnessError::Parse(_)), "{junk}: {err}");
        }
    }

    /// The three knobs this section grew are all ABSENT-BY-DEFAULT, and absent resolves to the walk
    /// it described before they existed: no search, an anchored train window, and the sweep's own
    /// default ranking metric. This is the byte-identical-for-existing-profiles claim, made against
    /// the resolvers rather than against prose.
    #[test]
    fn the_new_knobs_default_to_the_pre_optimization_walk() {
        let p = profile("[walkforward]\nn_splits = 4\n");
        let cfg = p.walkforward.as_ref().expect("the section is present");
        assert_eq!(cfg.n_splits, 4);
        assert!(cfg.search.is_none(), "search absent");
        assert!(cfg.mode.is_none(), "mode absent");
        assert!(cfg.rank_by.is_none(), "rank_by absent");
        assert_eq!(cfg.window_search().unwrap(), WindowSearch::None);
        assert_eq!(cfg.walk_mode().unwrap(), WalkMode::Anchored);
        assert_eq!(cfg.rank_metric().unwrap(), RankMetric::default());
    }

    /// `search` takes both spellings of the grid and an explicit control, normalizes case and
    /// whitespace like every other string knob in a profile, and a TYPO fails at LOAD naming the
    /// valid set. The last part is the one that matters: a silent degradation to the control would
    /// run the no-search walk under a report the operator reads as an optimization's.
    #[test]
    fn the_search_knob_round_trips_and_a_typo_fails_at_load() {
        let search = |s: &str| {
            profile(&format!("{GRID}[walkforward]\nn_splits = 2\nsearch = \"{s}\"\n"))
                .walkforward
                .expect("the section is present")
                .window_search()
                .unwrap()
        };
        assert_eq!(search("none"), WindowSearch::None);
        // Case and surrounding whitespace are forgiven — the `queue_model_kind` idiom. The SPELLING
        // is not: see below.
        for spelling in ["sweep", "SWEEP", " sweep "] {
            assert_eq!(search(spelling), WindowSearch::Sweep, "search = {spelling:?}");
        }

        // ⚠ `"grid"` is REFUSED, and refused by NAME rather than falling into the catch-all. It
        // parsed as a synonym for exactly one release — the design doc said `sweep`, the first
        // implementation said `grid`, and both were accepted rather than choosing. A profile
        // written in that window has to be told what to write, not merely that it is wrong.
        let renamed = BacktestProfile::from_toml_str(&format!(
            "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"grid\"\n"
        ))
        .unwrap_err();
        let renamed = format!("{renamed}");
        assert!(
            renamed.contains("renamed") && renamed.contains("\"sweep\""),
            "the removed spelling must name its replacement, not just fail: {renamed}"
        );

        // ...and an actual typo still lands in the catch-all, which now advertises `sweep`.
        let err = BacktestProfile::from_toml_str(&format!(
            "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"gird\"\n"
        ))
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown walkforward.search") && msg.contains("sweep"), "{msg}");
    }

    /// `mode` round-trips both [`WalkMode`] variants and refuses anything else at load. It is a
    /// settable knob at all only because the runner started reading the train half — until then
    /// the two modes returned byte-identical reports.
    #[test]
    fn the_mode_knob_round_trips_and_a_typo_fails_at_load() {
        let mode = |s: &str| {
            profile(&format!("[walkforward]\nn_splits = 2\nmode = \"{s}\"\n"))
                .walkforward
                .expect("the section is present")
                .walk_mode()
                .unwrap()
        };
        assert_eq!(mode("anchored"), WalkMode::Anchored);
        assert_eq!(mode("Rolling"), WalkMode::Rolling);
        assert_eq!(mode(" rolling "), WalkMode::Rolling);
        let err = BacktestProfile::from_toml_str(&format!(
            "{BASE}[walkforward]\nn_splits = 2\nmode = \"expanding\"\n"
        ))
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown walkforward.mode") && msg.contains("rolling"), "{msg}");
    }

    /// `rank_by` names one of the four [`RankMetric`]s — and it names them through
    /// `RankMetric::from_str_ci` itself, the SAME parser the sweep bin's `--rank-by` uses, so this
    /// test is also the pin that the two doors cannot start accepting different spellings.
    #[test]
    fn the_rank_by_knob_round_trips_and_a_typo_fails_at_load() {
        let metric = |s: &str| {
            profile(&format!("[walkforward]\nn_splits = 2\nrank_by = \"{s}\"\n"))
                .walkforward
                .expect("the section is present")
                .rank_metric()
                .unwrap()
        };
        assert_eq!(metric("sharpe"), RankMetric::Sharpe);
        assert_eq!(metric("RETURN"), RankMetric::TotalReturn);
        assert_eq!(metric("max_dd"), RankMetric::MaxDrawdown);
        assert_eq!(metric(" equity "), RankMetric::FinalEquity);
        let err = BacktestProfile::from_toml_str(&format!(
            "{BASE}[walkforward]\nn_splits = 2\nrank_by = \"sortino\"\n"
        ))
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown walkforward.rank_by") && msg.contains("sharpe"), "{msg}");
    }

    /// `search = "sweep"` with no `[sweep]` table fails at LOAD rather than at the first window: the
    /// pairing is a contradiction in the profile TEXT — no store, no data slice and no verb can
    /// make it true — which puts it in the `n_splits = 0` class and not in the bar-mode class,
    /// whose rules depend on what the profile resolves against a store. The runtime twin in
    /// `run_walkforward_optimized_with` survives for the profiles that never came through here.
    #[test]
    fn grid_search_without_a_sweep_table_is_refused_at_load() {
        let err = BacktestProfile::from_toml_str(&format!(
            "{BASE}[walkforward]\nn_splits = 2\nsearch = \"sweep\"\n"
        ))
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("[sweep]"), "the refusal names what is missing: {msg}");
        // The rule is about the PAIRING, not about the key: the same profile with a grid loads...
        assert!(
            BacktestProfile::from_toml_str(&format!(
                "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"sweep\"\n"
            ))
            .is_ok(),
            "grid + [sweep] is the legal pairing"
        );
        // ...and the CONTROL never needs one, so an explicit `none` beside no grid stays legal.
        assert!(
            BacktestProfile::from_toml_str(&format!(
                "{BASE}[walkforward]\nn_splits = 2\nsearch = \"none\"\n"
            ))
            .is_ok(),
            "the control searches nothing, so it requires nothing"
        );
    }

    /// Every runner test needs SOME `HistStore` handle (even the guards that return before touching
    /// it), and this crate has no DataFusion-free double — `vike_data::MemHistStore` lives behind a
    /// `test-support` feature vike-backtest does not dev-depend on, and hand-rolling one here would
    /// mean stubbing the whole trait. So the runner tests ride the `datafusion-store` lane over an
    /// EMPTY temp-dir store, exactly like `harness::run`'s own store-backed tests; the profile-shape
    /// tests above stay on the trait-only `hist-replay` lane.
    #[cfg(feature = "datafusion-store")]
    fn empty_store() -> (tempfile::TempDir, Arc<dyn HistStore + Send + Sync>) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn HistStore + Send + Sync> =
            Arc::new(vike_data::DataFusionHist::open(dir.path()).unwrap());
        (dir, store)
    }

    #[cfg(feature = "datafusion-store")]
    #[test]
    fn a_profile_without_the_section_is_a_clean_error() {
        let (_dir, store) = empty_store();
        let err = run_walkforward(&profile(""), store).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(ref m) if m.contains("[walkforward]")));
    }

    #[cfg(feature = "datafusion-store")]
    #[test]
    fn a_tick_profile_is_a_clean_error() {
        let (_dir, store) = empty_store();
        let toml = BASE.replace("kind = \"bar\"", "kind = \"tick\"");
        let p = BacktestProfile::from_toml_str(&format!("{toml}[walkforward]\nn_splits = 2\n"))
            .unwrap();
        let err = run_walkforward(&p, store).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(ref m) if m.contains("bar-mode only")));
    }

    /// An empty bar slice is a DATA error, not a silent zero-window report.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn an_empty_bar_slice_is_a_data_error() {
        let (_dir, store) = empty_store();
        let err = run_walkforward(&profile("[walkforward]\nn_splits = 2\n"), store).unwrap_err();
        assert!(matches!(err, HarnessError::Data(ref m) if m.contains("no bars")), "{err}");
    }

    /// A cross-venue two-series slice cannot be walked forward (the splitter takes ONE series).
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn a_multi_series_profile_is_a_clean_error() {
        let (_dir, store) = empty_store();
        let toml = r#"
[data]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"
[[data.series]]
venue = "binance"
symbol = "BTCUSDT"
[[data.series]]
venue = "binance"
symbol = "ETHUSDT"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
[walkforward]
n_splits = 2
"#;
        let p = BacktestProfile::from_toml_str(toml).unwrap();
        let err = run_walkforward(&p, store).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(ref m) if m.contains("ONE bar series")));
    }

    /// End-to-end over a REAL store: one window per split, a stitched curve, finite summary stats.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn runs_one_window_per_split_over_a_real_store() {
        use vike_data::DataFusionHist;
        use vike_model::Bar;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars: Vec<Bar> = (0..240)
            .map(|i| {
                let close = 100.0 + i as f64 * 0.1;
                Bar {
                    ts: i as i64 * 1000,
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 1.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                }
            })
            .collect();
        store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

        let p = profile("[walkforward]\nn_splits = 4\n");
        let rep = run_walkforward(&p, store).unwrap();
        assert_eq!(rep.windows.len(), 4, "one OOS window per split");
        assert!(!rep.oos_equity_curve.is_empty(), "the stitched curve is populated");
        assert!(rep.oos_return.is_finite());
        assert!((0.0..=1.0).contains(&rep.wf_consistency));
    }

    /// The harness door APPLIES its own resolver — the half a cross-plane test cannot see.
    ///
    /// The cross-plane gate (`crates/vike-studio-core/tests/walkforward_annualization.rs`) proves
    /// the STUDIO door's applied factor and proves both doors' DERIVATIONS agree. What neither it
    /// nor anything else covered is the single `periods_per_year(profile)` argument at this
    /// module's `walk_forward_strategy` call — and this branch exists precisely because a doc
    /// comment asserted a property nothing checked.
    ///
    /// ⚠ It runs at `"1h"` deliberately. `runs_one_window_per_split_over_a_real_store` above uses
    /// `"1d"`, where the derived factor and a hardcoded `252` are THE SAME NUMBER — so that test
    /// would pass against either spelling and cannot gate this. At `"1h"` they differ by
    /// `sqrt(24)`, and the `assert_ne!` is what stops the bit-exact assertion from passing when
    /// both sides regress together.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn the_harness_door_applies_the_derived_annualization_not_the_daily_anchor() {
        use crate::metrics::sharpe;
        use crate::report::DAILY_PERIODS_PER_YEAR;
        use vike_data::DataFusionHist;
        use vike_model::Bar;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars: Vec<Bar> = (0..240)
            .map(|i| {
                let close = 100.0 + i as f64 * 0.1;
                Bar {
                    ts: i as i64 * 1000,
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 1.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                }
            })
            .collect();
        store.append_bars("binance", "BTCUSDT", "1h", &bars, None).unwrap();

        let p = BacktestProfile::from_toml_str(&format!(
            "{}{}",
            BASE.replace("interval = \"1d\"", "interval = \"1h\""),
            "[walkforward]\nn_splits = 4\n"
        ))
        .unwrap();
        let rep = run_walkforward(&p, store).unwrap();

        let derived = periods_per_year(&p);
        assert_eq!(derived, DAILY_PERIODS_PER_YEAR * 24.0, "1h resolves to 6,048");

        // Precondition: a dispersion-free curve makes `sharpe` return 0.0 at EVERY factor, which
        // would make the inequality below vacuous.
        let at_daily = sharpe(&rep.oos_equity_curve, DAILY_PERIODS_PER_YEAR);
        assert!(
            at_daily != 0.0 && at_daily.is_finite(),
            "fixture must have dispersion: {at_daily}"
        );

        assert_eq!(
            rep.oos_sharpe.to_bits(),
            sharpe(&rep.oos_equity_curve, derived).to_bits(),
            "the reported oos_sharpe IS the stitched curve annualized at the derived factor"
        );
        assert_ne!(
            rep.oos_sharpe, at_daily,
            "...and NOT at the daily anchor — the regression this test exists to catch"
        );
    }
    /// `BASE` with a range wide enough for every bar [`seeded_store`] writes. The frozen `BASE`
    /// stops at 100_000 ms, so a longer series would be silently TRUNCATED by the loader — and a
    /// truncated series changes the split arithmetic, which is exactly what the tests below
    /// measure.
    #[cfg(feature = "datafusion-store")]
    fn wide_profile(extra: &str) -> BacktestProfile {
        let base = BASE.replace("to = \"100000\"", "to = \"100000000\"");
        BacktestProfile::from_toml_str(&format!("{base}{extra}")).unwrap()
    }

    /// A real (temp-dir) store holding ONE bar series under `BASE`'s own `(venue, symbol,
    /// interval)`: `closes` as flat OHLC bars one second apart. Flat OHLC is all these tests need —
    /// `buy_hold` reads nothing but the close, and the engine fills at the NEXT bar's open.
    #[cfg(feature = "datafusion-store")]
    fn seeded_store(closes: &[f64]) -> (tempfile::TempDir, Arc<dyn HistStore + Send + Sync>) {
        use vike_model::Bar;

        let dir = tempfile::tempdir().unwrap();
        let hist = vike_data::DataFusionHist::open(dir.path()).unwrap();
        let bars: Vec<Bar> = closes
            .iter()
            .enumerate()
            .map(|(i, &close)| Bar {
                ts: i as i64 * 1000,
                open: close,
                high: close,
                low: close,
                close,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            })
            .collect();
        hist.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();
        let store: Arc<dyn HistStore + Send + Sync> = Arc::new(hist);
        (dir, store)
    }

    /// Every number a [`WalkForwardReport`] carries, as IEEE-754 BIT PATTERNS. Two reports then
    /// compare EXACTLY — and two `NaN` Sharpes compare EQUAL, which `f64` itself will not do — so
    /// "these two runs produced the same report" cannot pass by accident on a degenerate curve.
    /// The workspace's frozen-fixture convention, applied to a report instead of a file.
    #[cfg(feature = "datafusion-store")]
    fn report_bits(rep: &WalkForwardReport) -> Vec<u64> {
        let mut out = vec![
            rep.oos_return.to_bits(),
            rep.oos_sharpe.to_bits(),
            rep.wf_consistency.to_bits(),
            rep.windows.len() as u64,
        ];
        out.extend(rep.oos_equity_curve.iter().map(|v| v.to_bits()));
        for w in &rep.windows {
            out.push(w.test_range.0 as u64);
            out.push(w.test_range.1 as u64);
            out.push(w.oos_return.to_bits());
        }
        out
    }

    /// The `size` a searched window chose, read back out of its `chosen_params`.
    #[cfg(feature = "datafusion-store")]
    fn chosen_size(rep: &WalkForwardReport, window: usize) -> f64 {
        rep.windows[window]
            .chosen_params
            .as_ref()
            .expect("a searched window records what it chose")
            .iter()
            .find(|(k, _)| k == "size")
            .and_then(|(_, v)| v.as_float())
            .expect("the grid's one axis is a float array named `size`")
    }

    /// THE CONTROL'S OWN GATE: [`WindowSearch::None`] through the optimizing driver reproduces the
    /// fixed-parameter walk BIT FOR BIT. If those two disagreed, the control would be a different
    /// program rather than a comparison, and every "the search bought nothing" verdict measured
    /// against it would be measuring the seam instead of the search.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn the_control_reproduces_the_fixed_parameter_walk_exactly() {
        let closes: Vec<f64> = (0..300).map(|i| 100.0 + i as f64 * 0.1).collect();
        let (_dir, store) = seeded_store(&closes);
        let p = wide_profile("[walkforward]\nn_splits = 3\n");
        let fixed = run_walkforward(&p, Arc::clone(&store)).unwrap();
        let control = run_walkforward_optimized(&p, store).unwrap();
        assert_eq!(
            report_bits(&fixed),
            report_bits(&control),
            "the control run IS the fixed-parameter walk, reached through the other driver"
        );
        assert!(
            control.windows.iter().all(|w| w.chosen_params.is_none()),
            "a window that chose nothing records nothing"
        );
    }

    /// The fixed walk HONOURS `[walkforward].mode` and cannot MOVE with it: the two modes differ in
    /// `train_start` alone, and this driver's closure discards the training half. Pinned rather
    /// than merely asserted in a doc, because it is precisely the invariance the optimizing driver
    /// breaks — and the invariance is what makes wiring the knob into both drivers safe.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn both_walk_modes_leave_the_fixed_walk_report_identical() {
        let closes: Vec<f64> = (0..300).map(|i| 100.0 + i as f64 * 0.1).collect();
        let (_dir, store) = seeded_store(&closes);
        let anchored_p = wide_profile("[walkforward]\nn_splits = 3\nmode = \"anchored\"\n");
        let rolling_p = wide_profile("[walkforward]\nn_splits = 3\nmode = \"rolling\"\n");
        let anchored = run_walkforward(&anchored_p, Arc::clone(&store)).unwrap();
        let rolling = run_walkforward(&rolling_p, store).unwrap();
        assert_eq!(
            report_bits(&anchored),
            report_bits(&rolling),
            "the fixed walk reads no train half, so the mode cannot move its report"
        );
    }

    /// ...and THE MODE IS NOT DECORATION once a window searches: `Anchored` and `Rolling` pick
    /// DIFFERENT winners over the same bars. This is the other half of the pair above — together
    /// they say the knob does nothing where it should do nothing and something where it should.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn the_two_walk_modes_choose_different_winners_when_a_window_searches() {
        // Up hard, then down: bars 0..100 climb 100 -> 199, bars 100..300 fall back to 99.5. With
        // `n_splits = 2` the splitter's chunk is 100, so the SECOND window (test `[200, 300)`)
        // trains on `[0, 200)` anchored — net UP, where the bigger `size` wins by 10x — and on
        // `[100, 200)` rolling — net DOWN, where that same size LOSES by 10x. The FIRST window's
        // train half is `[0, 100)` under both modes (`Rolling`'s `train_start` saturates at 0), so
        // its winner is this comparison's own control: it must agree.
        let closes: Vec<f64> = (0..300)
            .map(|i| if i < 100 { 100.0 + i as f64 } else { 199.0 - (i - 100) as f64 * 0.5 })
            .collect();
        let (_dir, store) = seeded_store(&closes);
        let grid = concat!(
            "[sweep]\nsize = [0.1, 1.0]\n",
            "[walkforward]\nn_splits = 2\nsearch = \"sweep\"\nrank_by = \"return\"\n"
        );
        let anchored_p = wide_profile(&format!("{grid}mode = \"anchored\"\n"));
        let rolling_p = wide_profile(&format!("{grid}mode = \"rolling\"\n"));
        let anchored = run_walkforward_optimized(&anchored_p, Arc::clone(&store)).unwrap();
        let rolling = run_walkforward_optimized(&rolling_p, store).unwrap();

        assert_eq!(
            chosen_size(&anchored, 0),
            chosen_size(&rolling, 0),
            "window 0's train half is identical under both modes, so its winner must be too"
        );
        assert_eq!(chosen_size(&anchored, 1), 1.0, "the anchored train half is net UP");
        assert_eq!(chosen_size(&rolling, 1), 0.1, "the rolling train half is net DOWN");
    }
}
