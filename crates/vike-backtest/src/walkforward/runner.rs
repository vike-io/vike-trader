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
//! ONE series, deliberately: every window form resolves to INDEX ranges over a single axis, a bar profile's
//! `&[Bar]` series or a tick profile's seed-bar grid over its ONE prepared tape (`super::ticks`, the owner's yes of
//! 2026-11-20). A multi-symbol profile is a clean [`HarnessError::Validation`], never a silent first-symbol
//! fallback. Both lanes' searched walks are one loop (`walk_windows`); the bar lane's fixed walk keeps its own.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_data::HistoryStore;

use crate::HarnessError;
use crate::run::refusals::ProfileRefusal;
use crate::run::{
    bar_engine_params, cancelled_run, is_cancelled, load_profile_bars,
    refuse_after_failed_properties,
};
use crate::run::{profile::ProfileBacktest, report::periods_per_year};
use crate::search::paramscan::{ParamscanPoint, ParamscanRow};
use crate::walkforward::{WalkforwardWindowOutcome, walk_forward_over_splits};
use vike_analytics::TradingRecord;
use vike_analytics::validation::{WalkforwardMode, WalkforwardSplit};
use vike_analytics::walkforward::WalkforwardResults;
use vike_marketdata::Bar;
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
    vike_simulator::BarSeriesShared,
    f64,
    Vec<String>,
    crate::run::FundingSchedule,
    Vec<vike_simulator::ConversionSeries>,
);

/// Everything BOTH walk-forward drivers must establish before their first window, in one place.
///
/// Errors: no `[walkforward]` table, an incoherent or unresolvable window form, a tick-mode or
/// multi-series profile, an unresolvable strategy, a symbol the run's ledger currency cannot book
/// (`crate::run::refuse_off_ledger_series`), a store failure, an empty bar slice, or a window
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
    searched: Option<&[crate::search::paramscan::ParamscanPoint]>,
    cancel: Option<&AtomicBool>,
) -> Result<Preflight, HarnessError> {
    let searches = searched.is_some();
    let config = walkforward_table(profile)?;
    let mut held = one_series_preflight(profile, store, cancel)?;
    let (symbol, bars) = held.series.pop().expect("one_series_preflight hands exactly one series");

    // The ONE place a window form becomes indices. `validate` already refused everything that is
    // wrong on the profile's own terms; this call is what adds the two facts only the store can
    // supply — `data.interval` and the series length.
    let windows = super::windows::resolve_windows(config, &bars, &profile.data.interval, mode)?;
    if windows.is_empty() {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardNoWindows {
            bars: bars.len(),
            symbol: symbol.clone(),
        }));
    }
    // Every window's engine starts FRESH on its own bars and decides nothing until the warm-up has
    // passed, so a window no longer than the warm-up is a window of zeros that reads as a result. Its
    // training half matters only to a walk that searches it.
    let warmup = walk_warmup(profile, searched)?;
    let at_point = warmup_point_words(searches);
    let shortest = |len: fn(&WalkforwardSplit) -> usize| windows.iter().map(len).min();
    let test = shortest(|w| w.test_end.saturating_sub(w.test_start)).unwrap_or(0);
    let train = shortest(|w| w.train_end.saturating_sub(w.train_start)).unwrap_or(0);
    if test <= warmup || (searches && train <= warmup) {
        return Err(match config.n_splits {
            Some(k) => super::windows::splits_too_many(bars.len(), k, warmup, searches),
            None => HarnessError::from(ProfileRefusal::WalkforwardWindowsShorterThanWarmup {
                bars: bars.len(),
                symbol: symbol.clone(),
                warmup,
                at_point: at_point.to_string(),
                test,
                training_clause: if searches {
                    format!(" and the shortest training half {train}")
                } else {
                    String::new()
                },
                train_clause: if searches { " and walkforward.train" } else { "" }.to_string(),
            }),
        });
    }

    Ok((
        windows,
        symbol,
        bars,
        profile.engine.cash,
        held.data_warnings,
        held.funding,
        held.conversions,
    ))
}

/// The profile's `[walkforward]` table, or the refusal of a walk with none (both lanes' pre-flights).
pub(super) fn walkforward_table(
    profile: &ProfileBacktest,
) -> Result<&crate::run::profile::ProfileWalkforward, HarnessError> {
    profile
        .walkforward
        .as_ref()
        .ok_or_else(|| HarnessError::from(ProfileRefusal::WalkforwardTableMissing))
}

/// The bars an engine of `profile` waits before its first decision: `data.warmup` or the strategy's
/// own `Strategy::warmup`, whichever is longer, the engine's own rule
/// (`vike_simulator::StrategyEngineConfig::warmup`), at the profile's own parameters.
pub(crate) fn engine_warmup(profile: &ProfileBacktest) -> Result<usize, HarnessError> {
    Ok(profile.data.warmup_steps()?.unwrap_or(0).max(profile.one_strategy()?.build()?.warmup()))
}

/// The warm-up a walk's windows must outlast: [`engine_warmup`] of the profile for a walk that runs
/// its own parameters (`searched` `None`), and the SMALLEST over the grid's points for a walk that
/// searches them, whose base parameters never run: every window trains the points and trades the
/// winner, so a window only a long-warm-up point cannot use is still a window. A point whose
/// strategy does not build is left out (it only loses its windows); none building falls back to
/// the profile's.
pub(crate) fn walk_warmup(
    profile: &ProfileBacktest,
    searched: Option<&[crate::search::paramscan::ParamscanPoint]>,
) -> Result<usize, HarnessError> {
    match searched
        .and_then(|points| points.iter().filter_map(|p| engine_warmup(&p.profile).ok()).min())
    {
        Some(least) => Ok(least),
        None => engine_warmup(profile),
    }
}

/// What a warm-up refusal adds after "data.warmup, else the strategy's own": for a searched walk, that
/// the number is the grid point's that warms up least.
pub(crate) fn warmup_point_words(searches: bool) -> &'static str {
    if searches { ", at the [paramscan] point that warms up least" } else { "" }
}

/// What [`held_bars_preflight`] hands a driver that runs over bars it holds: every series the
/// profile resolves, with the run's data warnings, its funding schedule and its booked conversion
/// pairs (decision 0243), the last two handed to every engine the driver builds. Each series is held
/// ONCE, tagged as every engine over it tags it, so a window, a searched point or a permuted history
/// runs on a view of it and copies no bar (`vike_simulator::BarSeriesShared`).
pub(crate) struct HeldBars {
    pub(crate) series: Vec<(String, vike_simulator::BarSeriesShared)>,
    pub(crate) data_warnings: Vec<String>,
    pub(crate) funding: crate::run::FundingSchedule,
    pub(crate) conversions: Vec<vike_simulator::ConversionSeries>,
}

/// The walk-forward family's pre-flight up to its window list: bar mode, the guards of
/// [`held_bars_preflight`], ONE series, and at least one bar of it. `held.series` holds exactly one
/// entry on `Ok`. Both walk drivers reach it through `walkforward_preflight`, and the combinatorial
/// walk (`super::combinatorial`), which cuts its own splits, calls it directly.
pub(crate) fn one_series_preflight(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    cancel: Option<&AtomicBool>,
) -> Result<HeldBars, HarnessError> {
    if profile.data.kind != ReplayKind::Bar {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardBarModeOnly));
    }
    let held = held_bars_preflight(profile, store, "a walk-forward", cancel)?;
    if held.series.len() != 1 {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardSeriesNotOne {
            series: held.series.len(),
        }));
    }
    let (symbol, bars) = &held.series[0];
    if bars.is_empty() {
        return Err(HarnessError::Data(format!(
            "no bars for {symbol} ({}) in the profile's range",
            profile.data.interval
        )));
    }
    Ok(held)
}

/// Every guard a plain `run_backtest` of `profile` raises before its first price, then the bars of
/// every series it resolves: for a driver that builds its engines over bars
/// it holds (a walk-forward's windows, a permutation test's histories,
/// `crate::run::run_backtest_over_bars`). `door` names that driver in the refusal of a
/// strategy that reads an input series (decision 0271), which no such driver loads. The caller
/// checks the replay kind first, in its own words.
pub(crate) fn held_bars_preflight(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    door: &str,
    cancel: Option<&AtomicBool>,
) -> Result<HeldBars, HarnessError> {
    // Pre-resolve BOTH per-window inputs so a bad strategy name / fee schedule / resolution sidecar
    // fails HERE with a real error rather than inside the window closure, which cannot return one.
    // A window loads no series the strategy reads by name (decision 0271), so one that reads any is
    // refused here, before a window runs it reading nothing.
    crate::run::data_plan::refuse_unplanned_inputs(
        &profile.one_strategy()?.build()?.inputs(),
        door,
    )?;
    // ⚠ THE LEDGER GUARD, and this door had none: each window below builds its own `StrategyEngine`
    // straight from `bar_engine_params`, so neither `run_backtest` nor `run_backtest_over_bars` (the
    // two places the guard sits) is on this path, and a `[walkforward]` profile on USDJPY at 150 with
    // `cash = 10000` booked 10,000 "dollars" where the truth is 66.62. Refused here, before any store
    // read and before ANY driver runs a window, so the fixed walk and the optimizing walk (whose
    // control arm also builds engines directly) share it. A symbol whose quote has a pair in
    // `engine.conversion` (decision 0243) is booked through it instead, as in `run_backtest`: the
    // same refusals first (`conversion_quotes`), the same pairs read below.
    let coin = walk_ledger(profile)?;
    let costs = crate::run::costs::run_costs(profile)?;
    let default_venue = bar_engine_params(profile, store, &Arc::default(), &costs)?.default_venue;

    // ⚠ THE DATA PRE-FLIGHT, and this door had none either: `run_backtest` is the one place the gate
    // sat, and no window goes through it, so an armed `data.require_coverage` read nothing on a walk.
    // Once per walk, because the plan is over the whole range every window is cut from.
    let mut data_warnings =
        crate::run::data_plan::enforce(profile, store.as_ref(), "the store this run opened")?;
    for line in &data_warnings {
        tracing::warn!("{line}");
    }
    // The conversion pairs, read and judged over the WHOLE range every window is cut from, before any
    // price is read, and booked to the run's symbols: `run_backtest`'s own two calls, so each window
    // takes the same rate at each instant by the same rule and the same staleness.
    let mut conversions = crate::run::data_plan::load_conversions(profile, store.as_ref())?;
    crate::run::data_plan::book_conversions(profile, &mut conversions, coin.is_some())?;
    // USDT on the dollar ledger: its rate found with no setting, or 1:1 (decision 0243, amended 2026-11-16), said.
    for line in crate::run::data_plan::one_to_one_lines(profile, &conversions)? {
        tracing::info!("{line}");
        data_warnings.push(line);
    }

    let (series, funding) = load_profile_bars(profile, store, cancel, &mut data_warnings)?;
    let series = series
        .into_iter()
        .map(|(slot, bars)| {
            let bars = vike_simulator::BarSeriesShared::from(bars);
            let tagged = bars.tagged(default_venue.as_deref(), &slot);
            (slot, tagged)
        })
        .collect();
    Ok(HeldBars { series, data_warnings, funding, conversions })
}

/// The ledger guard of a walk, before any store read: a symbol the run's ledger currency cannot book refused
/// (a pair of `engine.conversion` books its quote, decision 0243, after the same refusals `run_backtest` makes),
/// and a coin-margined contract booked in its settle coin or refused in words. Both lanes' pre-flights.
pub(super) fn walk_ledger(
    profile: &ProfileBacktest,
) -> Result<Option<crate::run::RunLedgerCoin>, HarnessError> {
    let quotes = crate::run::data_plan::conversion_quotes(profile)?;
    let declared = profile.data.resolved_series();
    crate::run::refuse_off_ledger_series(
        profile,
        declared.iter().map(|s| (s.venue.as_str(), s.symbol.as_str())).filter(|(v, s)| {
            crate::run::data_plan::quoted_in(&profile.kept, v, s, &quotes).is_none()
        }),
    )?;
    // A coin-margined contract is booked in its settle coin, or refused in words, before a window runs.
    crate::run::profile_run_ledger_coin(profile)
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
/// stitched [`WalkforwardResults`] — the FIXED-parameter walk.
///
/// Each window runs fresh at `engine.cash` and the OOS curves are rebased onto one running equity
/// (the [`crate::walkforward::walk_forward_over_windows`] stitch). Every window uses the SAME
/// `[strategy.params]`, so this answers "were these parameters stable out of sample?" and nothing
/// more. The window that
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
/// It is [`periods_per_year`] — for a bar profile, [`vike_analytics::results::periods_per_year_for_interval`]
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
/// back with a walk-forward Sharpe differing by `sqrt(24) ≈ 4.9x` between this door and the Studio's —
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
) -> Result<WalkforwardResults, HarnessError> {
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
) -> Result<WalkforwardResults, HarnessError> {
    let (mode, ..) = walkforward_knobs(profile)?;
    if profile.data.kind == ReplayKind::Tick {
        // The fixed walk over a tick tape is the searched walk's control: one run a window, nothing scored.
        let unscored = super::paramscan::RankMetric::default().objective();
        let search = WalkforwardWindowSearch::None;
        return super::ticks::tick_walk(profile, store, &[], search, mode, &unscored, cancel);
    }
    let (windows, symbol, bars, cash, data_warnings, funding, conversions) =
        walkforward_preflight(profile, &store, mode, None, cancel)?;
    let mut stopped = false;
    // Across every window: one fill anywhere in the walk that skipped its grid taints the stitch.
    let properties_error = Arc::default();
    let report = walk_forward_over_splits(
        Some(&bars[..]),
        &windows,
        cash,
        periods_per_year(profile),
        // The train half is ignored deliberately: this verb is the FIXED-parameter walk — an
        // out-of-sample stability check, not an optimize-then-test protocol, exactly as the
        // shipped profile template tells operators. The window that searches its train half is a
        // different driver.
        |split| {
            if is_cancelled(cancel) {
                stopped = true;
                return WalkforwardWindowOutcome::fixed(vike_analytics::TradingRecord::default());
            }
            // Both rebuilt per window: `StrategyEngineConfig` is not `Clone` (it can carry a
            // `Box<dyn PositionSizer>` / properties closure) and each window needs a fresh
            // strategy. Both were proven constructible above, so neither `expect` can fire.
            let strategy = profile.one_strategy().and_then(|s| s.build());
            let strategy = strategy.expect("strategy pre-resolved above");
            let costs = crate::run::costs::costs_of_built(profile, strategy.as_ref())
                .expect("costs pre-resolved above");
            let mut params = bar_engine_params(profile, &store, &properties_error, &costs)
                .expect("engine params pre-built above");
            params.funding_schedule = funding.clone();
            params.conversions = conversions.clone();
            let window = bars.slice(split.test_start..split.test_end);
            WalkforwardWindowOutcome::fixed(
                StrategyEngine::new_shared(vec![(symbol.clone(), window)], strategy, params).run(),
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
) -> Result<WalkforwardResults, HarnessError> {
    run_walkforward_optimized_cancellable(profile, store, None)
}

/// [`run_walkforward_optimized`] with a STOP FLAG — the backtesthub's `RunWalkforwardProfile`
/// arm's searched route, as [`run_walkforward_cancellable`] is its fixed one. `None` is
/// [`run_walkforward_optimized`] exactly.
///
/// The flag is read once per candidate POINT on each window's training half — the same one relaxed
/// load `crate::search::optimize::SearchEvaluatorBars::with_cancel` makes per point — and once per window before
/// its validation run. A point that finds it set scores `NaN` without running, so the search runs
/// out of work at once, and the walk returns [`cancelled_run`] rather than the "every candidate
/// failed" refusal those `NaN`s would otherwise read as.
pub fn run_walkforward_optimized_cancellable(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    cancel: Option<&AtomicBool>,
) -> Result<WalkforwardResults, HarnessError> {
    let (mode, search, metric) = walkforward_knobs(profile)?;
    optimized_walk(profile, store, search, mode, &metric.objective(), cancel)
}

/// Walk `profile` forward, SEARCHING inside each window: score the `[sweep]` grid on the window's
/// training half, carry the winner (and only the winner) onto its validation half, and record what
/// each window chose in [`vike_analytics::walkforward::WalkforwardResultsStep::chosen_params`].
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
/// The `objective` is the same `Box<dyn Fn(&TradingResults) -> f64>` the sweep and TPE lanes rank
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
) -> Result<WalkforwardResults, HarnessError> {
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
) -> Result<WalkforwardResults, HarnessError> {
    // Expanded ONCE, outside the window loop: `expand_paramscan` reads only the profile, so a
    // per-window expansion would rebuild an identical list `n_splits` times. Before the pre-flight,
    // whose warm-up check of a searched walk is over these points.
    let points = super::paramscan::expand_paramscan(profile)?;
    if profile.data.kind == ReplayKind::Tick {
        return super::ticks::tick_walk(profile, store, &points, search, mode, objective, cancel);
    }
    // The walk's cash is the profile's (`walk_windows` reads it there).
    let (windows, symbol, bars, _, data_warnings, funding, conversions) = walkforward_preflight(
        profile,
        &store,
        mode,
        search.searches().then_some(points.as_slice()),
        cancel,
    )?;
    refuse_a_search_without_a_grid(profile, search)?;

    // A candidate that could not read its instrument grid scores `NaN` like one the strategy rejected,
    // and the others would then win a window that never compared them: each candidate's own failed
    // read (below) refuses the walk in grid order, and this slot, the validation runs', after it.
    let properties_error = Arc::default();
    // A window's training half and its validation half are views of the bars the pre-flight held once
    // (`vike_simulator::BarSeriesShared`): each candidate clones the view, and no run copies a bar.
    let hold = |train: Range<usize>| vec![(symbol.clone(), bars.slice(train))];
    // Each candidate keeps its own failed read, which `walk_windows` keeps in grid order (`WalkScore`); the walk's
    // own slot is its validation runs'.
    let score = |train: &Vec<(String, vike_simulator::BarSeriesShared)>, point: &ParamscanPoint| {
        let read: Arc<std::sync::OnceLock<String>> = Arc::default();
        let (row, score) = super::paramscan::grid::evaluation_scored_point_over_bars(
            profile,
            &store,
            train.clone(),
            &funding,
            &conversions,
            objective,
            &point.profile,
            point.overrides.clone(),
            &read,
        );
        (row, score, read.get().cloned())
    };
    let run = |run_profile: &ProfileBacktest, test: Range<usize>| {
        super::run::run_backtest_over_bars(
            run_profile,
            &store,
            vec![(symbol.clone(), bars.slice(test))],
            &funding,
            &conversions,
            &properties_error,
        )
    };
    let lane = WalkLane { bars: Some(&bars[..]), hold: &hold, score: &score, run: &run };
    let report = walk_windows(profile, &windows, &points, search, cancel, lane)?;
    refuse_after_failed_properties(&properties_error)?;
    Ok(labelled(report, profile, data_warnings))
}

/// The refusal of a SEARCHED walk over a profile with no `[paramscan]` table, which would "select" the one
/// parameter set it already has (both lanes' walks ask it after their pre-flight).
pub(super) fn refuse_a_search_without_a_grid(
    profile: &ProfileBacktest,
    search: WalkforwardWindowSearch,
) -> Result<(), HarnessError> {
    if search == WalkforwardWindowSearch::Sweep && !profile.is_paramscan() {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardSearchWithoutGrid));
    }
    Ok(())
}

/// How a walk runs its windows over the axis its window list indexes (a bar series, a tick walk's seed-bar
/// grid): `hold` a training range for a search, `score` one grid point on what was held, `run` a profile over a
/// validation range, fresh at `engine.cash`.
pub(super) struct WalkLane<'a, H> {
    /// The axis when it is a bar series, for each step's window times (`walk_forward_over_splits`); `None` on a grid.
    pub(super) bars: Option<&'a [Bar]>,
    pub(super) hold: &'a (dyn Fn(Range<usize>) -> H + Sync),
    pub(super) score: &'a (dyn Fn(&H, &ParamscanPoint) -> WalkScore + Sync),
    pub(super) run: &'a WalkRun<'a>,
}

/// One grid point scored by a lane on a training range: the evaluation fold's row and steering score, and the
/// READ that run could not make when that is why it failed (its refusal, both lanes'). A candidate that could not
/// read did not lose: the walk is refused once it ends ([`refuse_a_failed_read`]).
pub(super) type WalkScore = (ParamscanRow, f64, Option<String>);

/// The refusal of a walk one of whose candidates could not read its data: `read` is the FIRST such in grid order
/// (the earliest window or split, then the lowest point), so two runs of one walk refuse in one sentence whichever
/// candidate the pool finished first.
pub(super) fn refuse_a_failed_read(read: Option<String>) -> Result<(), HarnessError> {
    read.map_or(Ok(()), |why| {
        Err(HarnessError::Data(format!(
            "a candidate of the walk's search could not read its data, so the walk is refused \
             rather than ranked without it: {why}"
        )))
    })
}

/// One run of a profile over a range of a walk's axis, fresh at `engine.cash` (both lanes' validation windows
/// and combinatorial groups).
pub(super) type WalkRun<'a> =
    dyn Fn(&ProfileBacktest, Range<usize>) -> Result<TradingRecord, HarnessError> + 'a;

/// Every window of `windows` searched (or not) and traded through `lane`, and the stitched report:
/// [`run_walkforward_optimized_with`]'s protocol, and the ONE loop both lanes' walks run.
pub(super) fn walk_windows<H: Sync>(
    profile: &ProfileBacktest,
    windows: &[WalkforwardSplit],
    points: &[ParamscanPoint],
    search: WalkforwardWindowSearch,
    cancel: Option<&AtomicBool>,
    lane: WalkLane<'_, H>,
) -> Result<WalkforwardResults, HarnessError> {
    // The window closure cannot return an error, so a failure is captured here and raised after
    // the walk — never swallowed into a report.
    let mut failed_window: Option<String> = None;
    // The first candidate in grid order that could not read (`WalkScore`): windows run in order, and in one the
    // points' answers come back in grid order whichever the pool finished first.
    let mut failed_read: Option<String> = None;
    let mut stopped = false;

    let report = walk_forward_over_splits(
        lane.bars,
        windows,
        profile.engine.cash,
        periods_per_year(profile),
        |sp| {
            if is_cancelled(cancel) {
                stopped = true;
                return WalkforwardWindowOutcome {
                    result: TradingRecord::default(),
                    chosen_params: None,
                };
            }
            let test = sp.test_start..sp.test_end;
            let winner = match search {
                // The control: the profile's own params, no scoring, one run.
                WalkforwardWindowSearch::None => None,
                WalkforwardWindowSearch::Sweep => {
                    let train = (lane.hold)(sp.train_start..sp.train_end);
                    let mut scored: Vec<(usize, f64, Option<String>, Option<String>)> =
                        super::paramscan::map_bounded(
                            points.iter().enumerate().collect::<Vec<_>>(),
                            |(i, point)| {
                                // A point that finds the flag set runs nothing and scores `NaN`, so
                                // the window's search runs out of work at once.
                                if is_cancelled(cancel) {
                                    return (i, f64::NAN, None, None);
                                }
                                let (row, score, read) = (lane.score)(&train, point);
                                (i, score, row.error, read)
                            },
                        );
                    // In grid order, before the sort: the cause a refusal names does not depend on
                    // which point the pool finished first. `None` when every point ran and none
                    // could be ranked.
                    let cause = scored.iter().find_map(|(_, _, error, _)| error.clone());
                    if failed_read.is_none() {
                        failed_read = scored.iter().find_map(|(.., read)| read.clone());
                    }
                    // The ONE ordering rule every scored lane in this crate shares — NaN
                    // ("unrankable", i.e. the candidate failed) sorts last, so it cannot win.
                    scored.sort_by(|a, b| vike_analytics::metrics::cmp_scores_desc(a.1, b.1));
                    match scored.first() {
                        Some(&(i, s, ..)) if s.is_finite() => Some(i),
                        _ => {
                            failed_window.get_or_insert_with(|| {
                                let cause = cause
                                    .map_or(String::new(), |c| format!(": the first said {c}"));
                                format!(
                                    "every one of the {} candidate(s) failed on the training half \
                                     of the window ending at bar {}{cause}",
                                    points.len(),
                                    test.len()
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
                    result: TradingRecord::default(),
                    chosen_params: None,
                };
            }
            let chosen = winner.map(|i| points[i].overrides.clone());
            let run_profile = winner.map_or(profile, |i| &points[i].profile);
            let result = (lane.run)(run_profile, test).unwrap_or_else(|e| {
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
                TradingRecord::default()
            });
            WalkforwardWindowOutcome { result, chosen_params: chosen }
        },
    );

    // ⚠ The stop FIRST: the `NaN`s a cancelled search scores are the cancellation's doing, and
    // reporting them as "every candidate failed" would blame the profile.
    if stopped {
        return Err(cancelled_run());
    }
    if let Some(why) = failed_window {
        return Err(HarnessError::Data(format!("walk-forward optimization: {why}")));
    }
    refuse_a_failed_read(failed_read)?;
    Ok(report)
}

/// `report` carrying the ledger currency of the profile that produced it, when that is not the
/// dollar: the stitched equity curve is an amount of it, and a report that did not say so would be
/// read as dollars. Both drivers end here, so the two protocols label alike, and carry what the
/// pre-flight found in the data and the strategy file they ran alike, on the walk's `overall`
/// results (the combinatorial walk's every path too, `super::combinatorial`).
pub(super) fn labelled(
    mut report: WalkforwardResults,
    profile: &ProfileBacktest,
    data_warnings: Vec<String>,
) -> WalkforwardResults {
    report.account_currency = crate::run::ledger_label(profile);
    report.overall.data_warnings = data_warnings;
    let (file, sha256, sdk) = profile.strategy_identity();
    report.overall = report.overall.with_strategy(&file, &sha256, sdk);
    report
}

#[path = "runner_tests.rs"]
#[cfg(test)]
mod runner_tests;

#[path = "runner_short_data_tests.rs"]
#[cfg(test)]
mod runner_short_data_tests;

#[path = "runner_view_tests.rs"]
#[cfg(test)]
mod runner_view_tests;
