//! `run_backtest` — the harness run dispatcher: loads a [`super::ProfileBacktest`]'s
//! data slice from an `Arc<dyn HistoryStore>` (any backend: the datahub client in prod, `HistoryStoreMemory`
//! in tests) and runs the resolved strategy in bar-mode
//! (`StrategyEngine`) or tick-mode (`replay_ticks`), depending on `profile.data.kind`.
//!
//! The two modes wire `StrategyEngineConfig.properties` (the `snap_to_properties` point-in-time instrument
//! grid, PR-2a) DIFFERENTLY, and that difference is load-bearing:
//! - Bar mode never goes through `replay_ticks`, so nothing else would wire `params.properties` —
//!   this function sets it directly when `profile.engine.snap_to_properties` is on.
//! - Tick mode hands `snap_to_properties` straight through to `TickReplayConfig`; `replay_ticks`
//!   itself sets `params.properties` in that case (see `tick_replay::replay_ticks`). Setting it here
//!   too would be redundant (and diverge from the loader's own `properties_source` closure), so tick
//!   mode leaves `StrategyEngineConfig.properties` untouched.
//!
//! Both modes load the profile's RESOLVED series (`ProfileData::resolved_series`), so a cross-venue
//! `[[data.series]]` slice and the frozen single-venue `venue` + `symbols` pair take the same
//! code path — the latter is just the former's whole-lane expansion (port backlog G4). The
//! opt-in `[engine.resolution]` settlement source (G6) and `[engine.fee]` schedule (G7) are
//! built once, here, and handed to `StrategyEngineConfig` for both modes.
//!
//! ⚠ Resolution and fee are named above because they are BUILT here rather than copied across —
//! **they are not a roster of the `[engine]` surface this file wires, and nothing in this header
//! is.** Every `ProfileEngine` field reaches `StrategyEngineConfig` at one or both of the two construction
//! sites below; the authority for that set is `super::profile::ProfileEngine`'s own field list, and
//! `crates/vike-backtest/tests/engine_cfg_reaches_the_engine.rs` is what holds the two sites to it. A
//! prose list here would rot the way the one on `bar_engine_params` did (16 fields short inside
//! one branch), so this file keeps none.
//!
//! ⚠ **`StrategyEngineConfig` is not fed by `[engine]` ALONE, and that gate cannot see the other feeder.**
//! Two `[data]` keys reach it here: `data.warmup` becomes `StrategyEngineConfig::warmup` at both
//! construction sites (the warm-up FLOOR — it is a statement about how much HISTORY the slice must
//! spend before the first tradeable event, so it lives with the slice), and `data.detail_interval`
//! becomes `StrategyEngineConfig::granular_by_symbol` through [`load_profile_detail_bars`] in
//! `run_backtest`'s bar arm. Both are named because a reader auditing "what configures the engine"
//! would otherwise check `ProfileEngine` and the gate above and conclude the set was complete — it is
//! not, and nothing mechanical holds these two. Read `super::profile::ProfileData` for their rules.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use vike_data::HistoryStore;
use vike_marketdata::Bar;
use vike_model::{RiskLimits, Strategy};

use super::profile::{checked_margin_call, optimistic_quotes, slippage_terms};
use super::{HarnessError, ProfileBacktest};
use crate::tick_replay::{TickReplayConfig, properties_source, replay_ticks};
use vike_analytics::BacktestResult;
use vike_analytics::closed_trades::{
    DEFAULT_RISK_FREE_RATE, TradeClosedLedgerOptions, TradeClosedTerms,
};
use vike_marketdata::FundingRatePoint;
use vike_model::run_request::replay::ReplayKind;
use vike_simulator::{StrategyEngine, StrategyEngineConfig};

// Where the bars come from (`data.source`/`data.file`): the per-series read both bar loaders make.
mod bar_source;
pub(crate) use bar_source::source_label;
mod funding;
pub(crate) use funding::attaches_funding;
pub use funding::{FundingAccrual, FundingJoinStats, names_a_perpetual, window_join_funding};

/// The engine's funding schedule a load produced, one entry per series with stored funding
/// (`vike_simulator::StrategyEngineConfig::funding_schedule`'s shape). EMPTY when nothing attached funding.
pub type FundingSchedule = Vec<(String, Vec<FundingRatePoint>)>;

/// What [`load_profile_bars`] answers: the bars of each resolved series and the funding schedule
/// that came with them.
pub type RunBars = (Vec<(String, Vec<Bar>)>, FundingSchedule);

/// What `attach_funding_series` answers for one series with stored funding: its charges and the
/// report lines naming any hole in it.
type FundingRateSeries = (Vec<FundingRatePoint>, Vec<String>);

/// The error a run whose stop flag was set ends with ([`stop_if_cancelled`]). A caller that set the
/// flag discards it — the backtesthub answers its own `cancelled: …` sentence instead.
const CANCELLED_RUN: &str = "cancelled: the run was stopped before it finished";

/// Is `cancel` armed AND set? **The one spelling of the stop-flag check** every cancellable harness
/// path runs — the run dispatcher here, both walk-forward drivers and both point evaluators: one
/// relaxed atomic load when a flag is armed, nothing when none is, never an allocation or a syscall.
///
/// `Relaxed` is enough: the flag orders nothing else, and a set flag seen one point late costs one
/// point.
pub fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// The error a stopped run returns — for a path that noticed the flag somewhere it could not
/// return (a walk-forward window closure) and raises it afterwards.
pub(crate) fn cancelled_run() -> HarnessError {
    HarnessError::Data(CANCELLED_RUN.to_string())
}

/// `Err(`[`cancelled_run`]`)` once [`is_cancelled`] answers `true`, so a stage boundary in a
/// fallible path is one `?`.
pub(crate) fn stop_if_cancelled(cancel: Option<&AtomicBool>) -> Result<(), HarnessError> {
    if is_cancelled(cancel) {
        return Err(cancelled_run());
    }
    Ok(())
}

/// The optional pre-trade `RiskLimits` this profile's `[risk]` section describes
/// (runprofile-wiring-step2) — `None` EXACTLY when the section is absent, so a profile written
/// before this field existed maps to a `None` `StrategyEngineConfig.risk_limits` byte-identically
/// (`OrderBrokerSimulator::build_risk_gate`'s `(None, None) => None` arm never mounts a gate). `Some`
/// delegates to [`vike_model::ProfileRisk::to_risk_limits`] — the SAME compile-checked converter
/// paper/live use, so a limit proven in a backtest run carries unchanged into those modes. Shared
/// by both the bar-mode and tick-mode `StrategyEngineConfig` construction below (one mapping, not two).
fn risk_limits_for(profile: &ProfileBacktest) -> Option<RiskLimits> {
    profile.risk.as_ref().map(|r| r.to_risk_limits())
}

/// The BAR-mode [`StrategyEngineConfig`] a profile describes, built in one place.
///
/// ⚠ This doc used to enumerate that surface — "`cash`/`fee_rate`/the `fee` SCHEDULE/`slippage`/
/// `[risk]`/`[engine.resolution]`/the impact table/`snap_to_properties`" — and call the list "the
/// whole `[engine]` surface the bar lane honors". It was sixteen fields short by the end of the
/// branch that added them, and any list written here rots the same way. **The authority is
/// `super::profile::ProfileEngine`'s own field set**, and
/// `crates/vike-backtest/tests/engine_cfg_reaches_the_engine.rs` is what holds this function and its
/// tick-arm twin to it: every declared field must be read at BOTH sites unless a row in that file
/// states why one site (or both) is correct. Read the struct, not this paragraph.
///
/// Extracted from [`run_backtest`]'s bar arm (a pure code move) so the walk-forward runner
/// ([`crate::walkforward::runner::run_walkforward`]) gets IDENTICAL engine params instead of
/// re-deriving its own — the divergent-default bug shape this workspace keeps paying for. It is a
/// FACTORY, not a cached value, because [`StrategyEngineConfig`] is not `Clone` (it can carry a
/// `Box<dyn PositionSizer>` and a properties closure), and the walk-forward runner needs a fresh
/// one per OOS window.
///
/// `properties_error` is what [`properties_source`] keeps a failed lookup in, under
/// `snap_to_properties`: a caller that RUNS these params passes the result through
/// [`refuse_after_failed_properties`].
pub(crate) fn bar_engine_params(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    properties_error: &Arc<OnceLock<String>>,
) -> Result<StrategyEngineConfig, HarnessError> {
    let venue = profile.data.default_venue();
    let series = profile.data.resolved_series();
    let symbols: Vec<String> = series.iter().map(|s| s.symbol.clone()).collect();

    // Opt-in fee SCHEDULE (G7). `None` leaves the flat `fee_rate` chain untouched; `validate`
    // already refused the two together. A profile that states neither takes the venue table
    // (`fee_schedule_for_run`).
    //
    // ⚠ `build_for(&series)`, NOT `build()`: `[engine.fee] kind = "venue"` resolves the venue's
    // fee LANE from the run's OWN series (a `.P` symbol is priced on the perp row, not the spot
    // one), and the slice is the only thing that knows which lane this run trades. `build()` is
    // the load-time shape door `ProfileBacktest::refusals` drives, which cannot see the slice —
    // see `ProfileEngineFee::build_for`. Every non-`venue` kind answers identically through either door.
    let (fee_schedule, fee_schedules) = fee_schedule_for_run(
        profile.engine.fee.as_ref(),
        profile.engine.fee_rate,
        profile.engine.off_dollar_ledger(),
        &series,
        &profile.kept.rows,
    )?;
    let fee_underlyings = fee_underlyings_for_run(fee_schedule, &fee_schedules, &series)?;
    // Opt-in binary-resolution settlement (G6). Built here (not in `validate`) because it reads
    // the winners sidecar relative to the profile's own directory and cross-checks the run's
    // actual symbols.
    let resolution = profile
        .engine
        .resolution
        .as_ref()
        .map(|r| r.build(profile.base_dir.as_deref(), &symbols))
        .transpose()?;
    let (resolution, resolution_end_timestamp) = match resolution {
        Some((src, end)) => (Some(src), end),
        None => (None, None),
    };
    // Opt-in position sizer (Task 12). `None` leaves `StrategyEngine::new`'s own
    // `PositionSizerPassThrough` fallback untouched; `validate` already refused an unbuildable kind.
    let sizer = profile.engine.sizer.as_ref().map(|s| s.build()).transpose()?;
    // The ONE slippage model (`[engine.slippage]`) as the engine's three fields; absent = none.
    let slip = slippage_terms(profile.engine.slippage.as_ref())?;
    // The opt-in margin call (`[engine.margin_call]`); `validate` refused a bad number and the tick lane.
    let shortfall_cover = profile.engine.margin_call.map(checked_margin_call).transpose()?;

    // `default_venue` re-tags each bar's `symbol` to `SYMBOL.VENUE` (StrategyEngine::new's
    // frozen R2 behavior), which a symbol-inferring strategy (e.g. `buy_hold` reading
    // `bar.symbol`) can't route on. In bar mode the venue tag is ONLY needed as the
    // `properties_as_of` venue key, so set it just when snapping — otherwise leave bars on
    // their bare symbol so the common (no-snap) backtest "just works" without a magic
    // `strategy.params.symbol`.
    let mut params = StrategyEngineConfig {
        cash: profile.engine.cash,
        // The ledger currency the profile states (a label, `StrategyEngineConfig::account_currency`): the tick
        // replay's guard reads the SAME ledger the run was admitted under.
        account_currency: profile.engine.account_currency.clone(),
        fee_rate: profile.engine.fee_rate,
        fee_schedule,
        fee_underlyings,
        fee_schedules,
        slippage: slip.fraction,
        slippage_amount: slip.amount,
        default_venue: profile.engine.snap_to_properties.then(|| venue.clone()),
        resolution,
        resolution_end_timestamp,
        // Optional pre-trade RiskGate limits (runprofile-wiring-step2): the SAME
        // `[risk]` → `RiskLimits` converter paper/live use. Absent `[risk]` ⇒ `None` ⇒
        // `OrderBrokerSimulator::build_risk_gate` never mounts a gate — byte-identical to before this
        // field existed. `validate` already refused a profile that sets
        // `max_orders_per_window` (meaningless wall-clock throttle in sim time), so no
        // further filtering is needed here.
        risk_limits: risk_limits_for(profile),
        timeframes: profile.engine.timeframes.clone(),
        cash_gate: profile.engine.cash_gate,
        // The in-bar fill rules (`ProfileEngine::in_bar`): absent ⇒ every rule off, byte-identical.
        in_bar: profile.engine.in_bar.unwrap_or_default(),
        // The law a tagged plain limit fills by (`ProfileEngine::quotes`): absent ⇒ the file's header.
        optimistic_quotes: optimistic_quotes(profile.engine.quotes, &profile.strategy).0,
        // Closed-trade accounting (`ProfileEngine::closed_trades`): the report's block, nothing else.
        closed_trades: profile.engine.closed_trades.then(|| TradeClosedTerms {
            options: TradeClosedLedgerOptions { whole_entry_fee: profile.engine.whole_entry_fee },
            risk_free_rate: profile.engine.risk_free_rate.unwrap_or(DEFAULT_RISK_FREE_RATE),
        }),
        // WHEN the cross-section is decided (`vike_simulator::DecideMode`). `"simultaneous"` IMPLIES
        // `cash_gate`, and the engine reads the disjunction — so this line can turn the gated
        // admission phase on without the older flag being set, which is deliberate: an allocator
        // is what a cross-section is ordered IN. `refusals` has already refused the three
        // combinations that would make the mode a claim it cannot meet (tick lane, an armed
        // pre-trade gate, a detail tape).
        decide: super::profile::decide_mode(profile.engine.decide.as_deref())?,
        maint_margin: profile.engine.maint_margin,
        liquidation_buffer: profile.engine.liquidation_buffer,
        venue_style_liquidation: profile.engine.venue_style_liquidation,
        shortfall_cover,
        volume_limit: profile.engine.volume_limit,
        max_open_positions: profile.engine.max_open_positions,
        max_open_long: profile.engine.max_open_long,
        max_open_short: profile.engine.max_open_short,
        multiplier: profile.engine.multiplier,
        multipliers: profile.engine.multipliers.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        // A coin ledger's contract values, or its refusal (`profile_run_ledger_coin`); empty otherwise.
        contract_values: profile_run_ledger_coin(profile)?.map(|c| c.values).unwrap_or_default(),
        // ...and which of them count their candles' volume in the coin (`coin_volume`).
        coin_volume: coin_volume(profile, ReplayKind::Bar)?,
        // `engine.margin_mode = "cash"`: a spot account, every symbol judged by the spot law; absent: every
        // spot pair by its name (the spot rule); "cross" written out: none, the old margin account.
        cash_symbols: super::profile::cash_symbols(profile.engine.margin_mode.as_deref(), &series)?,
        leverage: profile.engine.leverage,
        clamp_to_leverage: profile.engine.clamp_to_leverage,
        settlement_period_ms: profile.engine.settlement_period_ms,
        session_gate: profile.engine.session_gate,
        sizer,
        // See `ProfileEngine::emulator_release_stops`'s doc: the harness default is `false` (matching
        // `StrategyEngineConfig::default()`), and this is one of the two construction sites that must
        // assign it for a profile's explicit `true` to reach the engine.
        emulator_release_stops: profile.engine.emulator_release_stops,
        // The declared warm-up FLOOR (`data.warmup`). `None` leaves the gate at exactly
        // `Strategy::warmup()`, byte-identical; `Some(n)` raises it and can never lower it (see
        // `StrategyEngineConfig::warmup`). Resolved through `ProfileData::warmup_steps`, which is also where
        // the refusals live — so a `"3mo"` or a duration over an unparseable `data.interval`
        // fails HERE rather than being silently rounded to something.
        //
        // ⚠ It is assigned at this site, which `run_backtest_over_bars` shares, so a walk-forward
        // window warms up per window — the right answer: each OOS window is a fresh engine over a
        // fresh slice, and its indicators are as cold at its first bar as the run's were at the
        // range start.
        warmup: profile.data.warmup_steps()?,
        ..Default::default()
    };
    if profile.engine.snap_to_properties {
        params.properties = Some(properties_source(store.clone(), Arc::clone(properties_error)));
    }
    // The `market_impact` model. Any other model leaves `params.impact` `None` and its window at
    // the default, as a profile with no impact always did. The tick arm assigns the SAME terms.
    if let Some((model, window)) = slip.impact {
        params.impact = Some(model);
        params.impact_window = window;
    }
    Ok(params)
}

/// `Err` when a run's `properties_as_of` failed ([`properties_source`]'s ⚠): the result it would have
/// reported holds fills that were never checked against the instrument grid.
pub(crate) fn refuse_after_failed_properties(
    properties_error: &OnceLock<String>,
) -> Result<(), HarnessError> {
    properties_error.get().map_or(Ok(()), |why| Err(HarnessError::Data(why.clone())))
}

/// [`fee_schedule_for_run`]'s answer: the run-wide schedule and each series' own. Named for clippy's
/// `type_complexity`.
pub(crate) type RunFees = (Option<vike_model::FeeSchedule>, Vec<(String, vike_model::FeeSchedule)>);

/// The words of the report line a series with no stored funding history gets
/// (`funding_for_series`), and the phrase the realism stamp's funding term reads that line by
/// (`crates/vike-backtest/src/harness/report.rs`'s `funding_words`): one spelling for both.
pub(crate) const NO_FUNDING_HISTORY: &str = "the store holds no funding history";

/// The run's `[engine.fee]` schedule (`None` = the flat `fee_rate` chain) and each series' own
/// schedule when a `kind = "venue"` run straddles the venue's fee lanes (`ProfileEngineFee::per_series`, empty
/// otherwise), built for the run's own series — and refused when one is a BUILT-IN venue table whose
/// amounts are dollars on a ledger that is not the dollar.
///
/// ⚠ **`kind = "venue"` carries USD constants.** The currency-shaped rows in
/// `vike_venues::fee_schedule_for` are interactive brokers' `PerShareWithFloor` and `BpsWithFloor`
/// ones ($0.005 a share and a $1 minimum per order, $0.85 a futures contract, a $2 minimum per forex
/// order); every other FX/CFD/crypto row is a rate or free and is unit-free. On a euro or
/// yen ledger that $1 would be read as one euro or one yen, silently. The refusal says so and points
/// at the explicit `kind = "per_share_with_floor"` whose `per_share` and `min` the profile writes in
/// its own currency, which this check does not touch. Both `StrategyEngineConfig` sites call this, so the
/// bar and tick lanes agree.
///
/// It takes the table (`profile.engine.fee.as_ref()`) rather than the profile on purpose: the
/// `engine_cfg_reaches_the_engine` gate reads the literal `profile.engine.fee` at each
/// `StrategyEngineConfig` site, and a call that hid it would be a field reaching the engine by faith.
///
/// ⚠ **A profile that states NO fee** (no `[engine.fee]`, `engine.fee_rate` 0) is costed at the
/// `kind = "venue"` table of each series' own fee lane (`vike_venues::fee_lane` of its venue and
/// symbol), as the paper mount of the same instrument is (owner, 2026-11-08: it charged every fill
/// zero before); a Studio slice states that table outright (`ProfileBacktest::for_slice`). A slice
/// over two lanes, or two venues, pays each symbol its own row, never zero; `kind = "free"` is how a
/// profile says a run is free. serde cannot tell `fee_rate = 0.0` from an absent key, so a written
/// zero takes the venue table too. The realism stamp's fee words read the same answer
/// (`crates/vike-backtest/src/harness/report.rs`'s `fee_words`).
pub(crate) fn fee_schedule_for_run(
    fee: Option<&super::profile::ProfileEngineFee>,
    fee_rate: f64,
    ledger: Option<&'static str>,
    series: &[crate::tick_replay::ProfileDataSeries],
    kept: &[vike_model::Instrument],
) -> Result<RunFees, HarnessError> {
    let defaulted;
    let (fee, stated) = match fee {
        Some(fee) => (fee, true),
        None if fee_rate == 0.0 => {
            let Some(first) = series.first() else { return Ok((None, Vec::new())) };
            defaulted = super::profile::ProfileEngineFee::of_venue(&first.venue);
            (&defaulted, false)
        }
        None => return Ok((None, Vec::new())),
    };
    let schedule = fee.build_for(series, kept)?;
    let per_series = if stated {
        fee.per_series(series, kept)
    } else {
        // Every series at its own venue's lane (by its kept row: `kept_lane`); one shared lane keeps the run-wide
        // schedule alone.
        fn lane<'s>(
            s: &'s crate::tick_replay::ProfileDataSeries,
            kept: &[vike_model::Instrument],
        ) -> &'s str {
            super::profile::kept_lane(&s.venue, &s.symbol, kept)
        }
        let first = series.first().map(|s| (&s.venue, lane(s, kept)));
        if series.iter().all(|s| Some((&s.venue, lane(s, kept))) == first) {
            Vec::new()
        } else {
            series
                .iter()
                .map(|s| (s.symbol.clone(), vike_venues::fee_schedule_for(lane(s, kept))))
                .collect()
        }
    };
    let dollar_table =
        std::iter::once(&schedule).chain(per_series.iter().map(|(_, s)| s)).any(|s| {
            matches!(
                s,
                vike_model::FeeSchedule::PerShareWithFloor { .. }
                    | vike_model::FeeSchedule::BpsWithFloor { .. }
            )
        });
    if fee.kind == "venue"
        && dollar_table
        && let Some(currency) = ledger
    {
        // `Data`, like the ledger guard beside it: this module carries no profile-schema refusal (those
        // are published from the modules `profile_surface::HARNESS_MODULES` lists), and a run refused
        // for what its profile MEANS against its data is a data refusal here.
        return Err(HarnessError::Data(format!(
            "engine.fee.kind = \"venue\" for {:?}{} is a built-in table whose per-share or \
             per-contract rate and per-order minimum are DOLLAR amounts, and this run's ledger is \
             {currency}: they would be read as {currency} amounts, silently. State the schedule yourself with \
             engine.fee.kind = \
             \"per_share_with_floor\" and its per_share and min written in {currency}",
            fee.venue.as_deref().unwrap_or(""),
            if stated { "" } else { " (what a profile that states no fee is costed at)" }
        )));
    }
    Ok((Some(schedule), per_series))
}

/// What each option of a run charged at `PercentOfUnderlying` (its own schedule in `per_series`, else
/// the run-wide `schedule`) pays its fee on (`vike_simulator::StrategyEngineConfig::fee_underlyings`): the series of
/// its underlying the run loads (`vike_catalog::option_underlyings`), or a REFUSAL when it loads none,
/// because the fee would then be charged on the premium, under the venue's by the underlying-to-premium
/// ratio (a measured deribit fill: 0.2318 paid, 0.0057 on the premium).
fn fee_underlyings_for_run(
    schedule: Option<vike_model::FeeSchedule>,
    per_series: &[(String, vike_model::FeeSchedule)],
    series: &[crate::tick_replay::ProfileDataSeries],
) -> Result<Vec<(String, String)>, HarnessError> {
    let mut out = Vec::new();
    for s in series {
        let own = per_series.iter().find(|(symbol, _)| *symbol == s.symbol).map(|(_, f)| *f);
        if !matches!(own.or(schedule), Some(vike_model::FeeSchedule::PercentOfUnderlying { .. })) {
            continue;
        }
        let Some(names) = vike_catalog::option_underlyings(&s.venue, &s.symbol) else { continue };
        let loaded =
            names.iter().find(|n| series.iter().any(|x| x.venue == s.venue && x.symbol == **n));
        let Some(underlying) = loaded else {
            // `Data`, like the ledger refusal above: what the profile means against its slice.
            return Err(HarnessError::Data(format!(
                "{} on {} is an option, and its fee is a share of its UNDERLYING (min(bps x underlying, \
                 cap x premium), engine.fee): this run loads no series of the underlying, so the fee would \
                 be charged on the premium instead, about the underlying-to-premium ratio too low. Add {} \
                 or {} on {} to the run's symbols",
                s.symbol, s.venue, names[0], names[1], s.venue
            )));
        };
        out.push((s.symbol.clone(), underlying.clone()));
    }
    Ok(out)
}

/// **Refuse a run over a symbol that cannot be booked in the run's ledger currency** (decision 0183,
/// item 13).
///
/// Without a conversion pair the simulator books `price * quantity` in whatever unit the series is
/// quoted in, into ONE cash pool. That is exact when the pool's currency IS the series' quote currency
/// and wrong by the rate otherwise: USDJPY at 150 read as dollars books 150 times the real dollars of
/// PnL, fee and margin (100,000 units closed 0.10 higher is 10,000 JPY, which is 10,000 / 150.10 =
/// 66.62 USD, not 10,000 USD), EURGBP 1.35 times too few, and a percent-of-equity order is sized
/// against the wrong notional. A symbol whose quote has a pair (`engine.conversion`, decision 0243) is
/// booked at its rate instead, so a profile run hands this guard only the symbols no pair books
/// (`crate::data_plan::quoted_in`).
///
/// `account_currency` is the run's `engine.account_currency` (`None` = the dollar). A symbol passes when its
/// NAME states exactly that quote; on the dollar ledger a name that states no fiat quote (crypto, a
/// bare coin) passes too, which is what kept every run that worked running, and on any other ledger
/// it does not (`vike_catalog::off_ledger_list` carries the rule). A currency the build does not
/// know is a [`HarnessError::Validation`], as at profile load.
///
/// This is the door of every run that CAN state a ledger (a profile): its sentences name
/// `engine.account_currency`. A door that cannot (the Studio's picker) calls
/// [`refuse_foreign_quoted_picks`] instead, which says the same thing in words that do not send a
/// user to a key they have no way to set. A profile run reads each symbol's quote off its KEPT row
/// where it holds one (`refuse_off_ledger_series`); this name-only form is the tick replay's own
/// second guard.
pub fn refuse_foreign_quoted_symbols<'a>(
    symbols: impl IntoIterator<Item = &'a str>,
    account_ccy: Option<&str>,
) -> Result<(), HarnessError> {
    let quoted = symbols.into_iter().map(|s| (s, vike_catalog::fiat_quote_of(s)));
    refuse_off_ledger(quoted, account_ccy, RunLedgerDoor::Profile)
}

/// [`refuse_foreign_quoted_symbols`] over a PROFILE's `(venue, symbol)` series, each quote read off its KEPT
/// row where the run holds one (`ProfileBacktest::kept`, `KeptRows::fiat_quote`), else off its name: the guard
/// every profile run takes.
pub(crate) fn refuse_off_ledger_series<'a>(
    profile: &ProfileBacktest,
    series: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), HarnessError> {
    let quoted = series.into_iter().map(|(venue, s)| (s, profile.kept.fiat_quote(venue, s)));
    refuse_off_ledger(quoted, profile.engine.account_currency.as_deref(), RunLedgerDoor::Profile)
}

/// [`refuse_foreign_quoted_symbols`] for a door with NO profile and so no way to state a ledger: it
/// books in the dollar, and a symbol `(venue, symbol)` quoted in another fiat currency (its `kept` row's
/// quote, else its name's) is refused in words that send the user to a profile run
/// (`engine.account_currency`) rather than to a key they cannot set. Public for the Studio's loaders, which
/// call it.
pub fn refuse_foreign_quoted_picks<'a>(
    series: impl IntoIterator<Item = (&'a str, &'a str)>,
    kept: &super::KeptRows,
) -> Result<(), HarnessError> {
    let quoted = series.into_iter().map(|(venue, s)| (s, kept.fiat_quote(venue, s)));
    refuse_off_ledger(quoted, None, RunLedgerDoor::Picker)
}

/// Which kind of door is refusing, which decides only the WORDS of the way out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunLedgerDoor {
    /// A profile run: `engine.account_currency` is the user's to set.
    Profile,
    /// A picker or a wire request: there is no ledger field, the run is on the dollar.
    Picker,
}

/// The body of both refusals. The way out it suggests is one that would NOT be refused itself: a
/// candidate ledger (each quote the names state, and the dollar) is offered only when
/// `vike_catalog::off_ledger_list` accepts EVERY symbol on it, so `[EURGBP, BTCUSDT]` is never told
/// to use `engine.account_currency = "GBP"` only to be refused again for `BTCUSDT`.
fn refuse_off_ledger<'a>(
    quoted: impl IntoIterator<Item = (&'a str, Option<&'static str>)>,
    account_ccy: Option<&str>,
    door: RunLedgerDoor,
) -> Result<(), HarnessError> {
    let ledger = super::profile::resolve_ledger_currency(account_ccy)?;
    let quoted: Vec<(&str, Option<&'static str>)> = quoted.into_iter().collect();
    let Some(list) = vike_catalog::off_ledger_list_of(quoted.iter().copied(), ledger) else {
        return Ok(());
    };
    // The distinct quotes the symbols are in, first-seen.
    let mut quotes: Vec<&'static str> = Vec::new();
    for quote in quoted.iter().filter_map(|(_, q)| *q) {
        if !quotes.contains(&quote) {
            quotes.push(quote);
        }
    }
    // The ledger that WOULD take every symbol, if any: each stated quote, then the dollar.
    let mut candidates = quotes.clone();
    if !candidates.contains(&vike_catalog::LEDGER_CURRENCY) {
        candidates.push(vike_catalog::LEDGER_CURRENCY);
    }
    let viable = candidates
        .into_iter()
        .filter(|c| *c != ledger)
        .find(|c| vike_catalog::off_ledger_list_of(quoted.iter().copied(), c).is_none());

    let rate = match (quotes.as_slice(), door) {
        ([only], RunLedgerDoor::Profile) if *only != ledger => format!(
            "Booking them in {ledger} needs a {only}/{ledger} conversion-rate series: name its pair as \
             engine.conversion.{only} = \"<series>\" in a profile backtest, the door that reads one \
             (decision 0243). Without it: "
        ),
        ([only], RunLedgerDoor::Picker) if *only != ledger => format!(
            "Booking them in {ledger} would need a {only}/{ledger} conversion-rate series, which this \
             door does not carry. "
        ),
        _ => String::new(),
    };
    let way_out = match (viable, door) {
        (Some(q), RunLedgerDoor::Profile) if q == vike_catalog::LEDGER_CURRENCY => format!(
            "Run them on the dollar ledger (leave engine.account_currency out, or set it to \"{q}\"), \
             or backtest only symbols quoted in {ledger}"
        ),
        (Some(q), RunLedgerDoor::Profile) => format!(
            "Either state the ledger in their own currency (engine.account_currency = \"{q}\", with \
             engine.cash and every other money value of the profile written in {q}, and read the \
             result in {q}), or backtest a {ledger}-quoted symbol"
        ),
        (Some(q), RunLedgerDoor::Picker) => format!(
            "This door has no ledger field, so it cannot book them: run them from a profile that \
             states engine.account_currency = \"{q}\" (`vike-cli backtest run --profile …`, or `--set \
             engine.account_currency={q}`) with engine.cash written in {q}, or pick a {ledger}-quoted \
             symbol"
        ),
        (None, door) => {
            let split = if quotes.len() >= 2 {
                "One run books ONE currency, so symbols quoted in different currencies share it \
                 only through a conversion-rate series per currency"
            } else {
                "No single ledger currency books all of them: a fiat-quoted symbol fits only its \
                 own currency's ledger, and a name that states no fiat quote (a crypto or stock \
                 name) fits only the dollar ledger"
            };
            match door {
                RunLedgerDoor::Profile => format!(
                    "{split}. Name a conversion pair for each quote currency that is not {ledger} \
                     (engine.conversion.<QUOTE> = \"<series>\", decision 0243) and one profile \
                     backtest books them all in {ledger}, or split them into separate runs, each with \
                     engine.account_currency set to the currency its symbols are quoted in"
                ),
                RunLedgerDoor::Picker => format!(
                    "{split}. This door has no ledger field: run each group from its own profile, \
                     with engine.account_currency set to the currency its symbols are quoted in"
                ),
            }
        }
    };
    let stated = match (door, account_ccy) {
        (RunLedgerDoor::Picker, _) => {
            "this door has no ledger field, so the run books in USD".into()
        }
        (RunLedgerDoor::Profile, None) => {
            "engine.account_currency is absent, which means USD".to_string()
        }
        (RunLedgerDoor::Profile, Some(currency)) => {
            format!("engine.account_currency is {:?}", currency.trim())
        }
    };
    // The measured size of the error, said once and only where "dollars" is the unit being misread.
    let size_of_it = if ledger == vike_catalog::LEDGER_CURRENCY {
        " (USDJPY at 150 books 150 times the real dollars, EURGBP 1.35 times too few)"
    } else {
        ""
    };
    Err(HarnessError::Data(format!(
        "the run is refused: {list} cannot be booked in the {ledger} ledger of this run \
         ({stated}). The simulator keeps ONE cash pool in ONE currency and converts a quote \
         currency only through the pair a profile names for it, so it would book that currency's \
         PnL, fees and margin as \
         {ledger}{size_of_it} and size percent-of-equity orders against the wrong notional. \
         {rate}{way_out}"
    )))
}

/// **Refuse a coin-margined (inverse) contract at a door that books every symbol in dollars**: the
/// Studio's picks, which hand the engine no contract value, and a tick replay whose
/// params carry none for it. The kind comes from the name (`vike_catalog::is_inverse_contract`): a run
/// holds no row of the contract.
///
/// Booked at the simulator's default linear value, `BTC-PERPETUAL`'s order of 10 (10 dollars) reads
/// 657,000 at 65,700, and its cash, fees and margin with it. A PROFILE run books a deribit or bybit inverse
/// contract in its settle coin instead ([`run_ledger_coin`], decisions 0170's amendments and 0258).
pub fn refuse_inverse_contracts<'a>(
    series: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), HarnessError> {
    let named = inverse_rows(series, |_| true);
    if named.is_empty() {
        return Ok(());
    }
    Err(HarnessError::Data(format!(
        "the run is refused: {} is a coin-margined (inverse) contract. Its quantity is a quote-currency \
         amount, so the price does not enter its value, and its PnL is not price difference times size. \
         This door books every symbol in dollars as size x price x multiplier, so it would value an order \
         of 10 on BTC-PERPETUAL at 65,700 at 657,000 instead of 10, and its cash, fees and margin with it. \
         Run it from a profile, which books a deribit or bybit inverse contract in its settle coin, or \
         backtest the linear contract of the same coin (a USDT or USDC book)",
        named.join(", ")
    )))
}

/// The `"SYMBOL on VENUE"` rows of the inverse contracts of `series` that `keep` keeps, each once, in
/// first-seen order: the phrase every inverse refusal names its offenders with.
fn inverse_rows<'a>(
    series: impl IntoIterator<Item = (&'a str, &'a str)>,
    keep: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    for (venue, symbol) in series {
        let row = format!("{symbol} on {venue}");
        if vike_catalog::is_inverse_contract(venue, symbol) && keep(venue) && !named.contains(&row)
        {
            named.push(row);
        }
    }
    named
}

/// **A run over coin-margined (inverse) contracts is a COIN LEDGER** (decision 0258, which live and
/// paper book by): its cash, balance, trades' PnL, fees and funding are amounts of the contracts' settle
/// coin, and `engine.cash` is read in that coin. The settle coin and each symbol's
/// `vike_model::ContractValue` are the catalog's answers for the NAME (`vike_catalog::inverse_settle_coin`,
/// `vike_catalog::contract_value`), the face read off the contract's KEPT row where the run holds one and only
/// the row states it (`vike_catalog::kept_contract_value`: binance COIN-M, okx).
#[derive(Debug, Clone, PartialEq)]
pub struct RunLedgerCoin {
    /// The settle coin (`BTC`): the run's ledger currency.
    pub coin: String,
    /// Each inverse symbol's value, the run's `StrategyEngineConfig::contract_values`.
    pub values: Vec<(String, vike_model::ContractValue)>,
}

/// The coin ledger of `series` (`(venue, symbol)` rows; `kept`, the run's kept instrument rows), `None` when no
/// symbol is inverse, or a refusal:
///
/// - an inverse contract whose face value only its row states (binance COIN-M's `contractSize`, okx's
///   `ctVal`) and neither a kept row nor a committed row of it does (the catalog carries those,
///   `vike_catalog::inverse_face_value`): a guessed 100 would book every number wrong by that factor;
/// - an inverse contract beside a linear symbol, or two settle coins: the simulator keeps ONE cash pool
///   in ONE currency and holds no rate series.
pub fn run_ledger_coin<'a>(
    series: impl IntoIterator<Item = (&'a str, &'a str)>,
    kept: &super::KeptRows,
) -> Result<Option<RunLedgerCoin>, HarnessError> {
    let series: Vec<(&str, &str)> = series.into_iter().collect();
    let inverse: Vec<(&str, &str)> = series
        .iter()
        .copied()
        .filter(|(venue, symbol)| vike_catalog::is_inverse_contract(venue, symbol))
        .collect();
    if inverse.is_empty() {
        return Ok(None);
    }
    // Each inverse symbol's value is the catalog's one value of its name (`vike_catalog::contract_value`,
    // the value a paper book books), its face its KEPT row's where only that row states it, `None` where no
    // row the run holds states it.
    let mut values: Vec<(String, vike_model::ContractValue)> = Vec::new();
    let mut rowless: Vec<String> = Vec::new();
    for &(venue, symbol) in &inverse {
        match vike_catalog::kept_contract_value(venue, symbol, kept.row(venue, symbol)) {
            Some(value) if values.iter().all(|(s, _)| s != symbol) => {
                values.push((symbol.to_string(), value));
            }
            Some(_) => {}
            None => {
                let row = format!("{symbol} on {venue}");
                if !rowless.contains(&row) {
                    rowless.push(row);
                }
            }
        }
    }
    if !rowless.is_empty() {
        return Err(HarnessError::Data(format!(
            "the run is refused: {} is a coin-margined (inverse) contract whose face value (the dollars \
             one contract is worth: binance COIN-M's contractSize, okx's ctVal) only the contract's own \
             row states, and the catalog holds no row of it; a guessed 100 would book every number wrong \
             by that factor. Backtest the same coin's inverse contract on deribit or bybit, or its linear \
             contract (a USDT or USDC book)",
            rowless.join(", ")
        )));
    }
    let mut linear: Vec<String> = Vec::new();
    for (venue, symbol) in &series {
        let row = format!("{symbol} on {venue}");
        if !vike_catalog::is_inverse_contract(venue, symbol) && !linear.contains(&row) {
            linear.push(row);
        }
    }
    let mut coins: Vec<String> = Vec::new();
    for coin in inverse.iter().filter_map(|(v, s)| vike_catalog::inverse_settle_coin(v, s)) {
        if !coins.contains(&coin) {
            coins.push(coin);
        }
    }
    if !linear.is_empty() || coins.len() != 1 {
        let inverse = inverse_rows(inverse.iter().copied(), |_| true).join(", ");
        let mix = if linear.is_empty() {
            format!("{inverse} settle in {}", coins.join(" and "))
        } else {
            format!("{inverse} books in its settle coin and {} in dollars", linear.join(", "))
        };
        return Err(HarnessError::Data(format!(
            "the run is refused: {mix}, and the simulator keeps ONE cash pool in ONE currency with no \
             conversion-rate series. Split them into separate runs, one settle coin each"
        )));
    }
    Ok(Some(RunLedgerCoin { coin: coins.remove(0), values }))
}

/// The ledger currency a report of `profile` must NAME: a coin ledger's settle coin, else
/// `ProfileEngine::off_dollar_ledger` (a fiat ledger off the dollar), else `None` (the dollar, unlabelled).
pub(crate) fn ledger_label(profile: &ProfileBacktest) -> Option<String> {
    let series = profile.data.resolved_series();
    match run_ledger_coin(
        series.iter().map(|s| (s.venue.as_str(), s.symbol.as_str())),
        &profile.kept,
    ) {
        Ok(Some(ledger)) => Some(ledger.coin),
        _ => profile.engine.off_dollar_ledger().map(str::to_string),
    }
}

/// [`run_ledger_coin`] of a PROFILE, refusing the knobs a coin ledger does not book yet: every one is linear
/// at its site, so it would size or price the coin as dollars.
///
/// - `engine.account_currency`: a fiat ledger's label, where the run's money is the settle coin;
/// - `engine.multiplier` / `multipliers`: the face value is the venue's, read off the name.
///
/// The `market_impact` slippage and `engine.volume_limit` run: the series' volume unit is [`volume_unit`]'s rule.
pub(crate) fn profile_run_ledger_coin(
    profile: &ProfileBacktest,
) -> Result<Option<RunLedgerCoin>, HarnessError> {
    let series = profile.data.resolved_series();
    let Some(ledger) = run_ledger_coin(
        series.iter().map(|s| (s.venue.as_str(), s.symbol.as_str())),
        &profile.kept,
    )?
    else {
        return Ok(None);
    };
    let e = &profile.engine;
    let knob = [
        (e.account_currency.is_some(), "engine.account_currency (its money is the settle coin)"),
        (e.multiplier != 1.0 || !e.multipliers.is_empty(), "engine.multiplier(s)"),
    ]
    .into_iter()
    .filter_map(|(set, name)| set.then_some(name))
    .collect::<Vec<_>>();
    if knob.is_empty() {
        return Ok(Some(ledger));
    }
    Err(HarnessError::Data(format!(
        "a coin-margined run books in its settle coin {} (engine.cash is {} too), and these keys are not \
         booked in it yet: {}. Drop them from this profile",
        ledger.coin,
        ledger.coin,
        knob.join(", ")
    )))
}

/// What a series' VOLUME counts against its orders' quantity ([`volume_unit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumeUnit {
    /// The order's own unit (a linear base, an inverse venue's contracts): compared with the size.
    Order,
    /// The settle coin of an inverse contract whose orders count contracts.
    Coin,
    /// Contracts of a per-instrument size only the instrument's row states, which a run does not hold.
    Unstated,
}

/// **What a stored series' VOLUME counts**, the one rule the volume cap and the impact model read, by
/// venue, lane and contract kind. Each venue's own field, which its bridge stores verbatim except okx's
/// candles, whose bridge stores the base column (`crates/bridges/okx/src/history.rs`'s `volume_column`):
///
/// | series | candles (the bar lane: the plain series) | trade prints (the tick lane) |
/// |---|---|---|
/// | deribit inverse | the COIN: `volume`, `cost` being the dollars (`crates/bridges/deribit/src/history.rs`) | dollars, the order's unit: `amount` (`crates/bridges/deribit/src/feed/decode.rs`) |
/// | bybit inverse | contracts of one dollar, the order's unit: `volume`, `turnover` being the coin (`crates/bridges/bybit/src/history.rs`) | contracts: `v` (`crates/bridges/bybit/src/feed/tick/mod.rs`) |
/// | okx inverse | the COIN: `volCcy` | contracts, the order's unit: `sz` |
/// | binance COIN-M | the COIN: the kline's base-asset column (`crates/bridges/binance/src/history.rs`'s `BINANCE_KLINE_COIN_M`) | contracts, the order's unit |
/// | okx linear swap, future or option | the base: `volCcy`, `vol` being contracts of `ctVal` | contracts of `ctVal`: `sz`, UNSTATED |
/// | every other linear series | the base, the order's unit | the base |
///
/// Bars built from trades (`tardis_backfill`, binance's aggregated trades) are a `source=` lane of their
/// own, which no backtest reads.
pub(crate) fn volume_unit(venue: &str, symbol: &str, lane: ReplayKind) -> VolumeUnit {
    let inverse = vike_catalog::is_inverse_contract(venue, symbol);
    match (venue, lane) {
        ("deribit" | "okx" | "binance", ReplayKind::Bar) if inverse => VolumeUnit::Coin,
        ("okx", ReplayKind::Tick) if !inverse && !vike_catalog::names_spot_pair(venue, symbol) => {
            VolumeUnit::Unstated
        }
        _ => VolumeUnit::Order,
    }
}

/// The run's `StrategyEngineConfig::coin_volume` on `lane` (the inverse symbols whose volume counts the coin,
/// [`volume_unit`]), refusing `engine.volume_limit` and the `market_impact` slippage over a series whose volume is
/// [`VolumeUnit::Unstated`]: a cap or a charge in the wrong unit is wrong by the contract size.
pub(crate) fn coin_volume(
    profile: &ProfileBacktest,
    lane: ReplayKind,
) -> Result<Vec<String>, HarnessError> {
    let series = profile.data.resolved_series();
    let unit = |s: &crate::tick_replay::ProfileDataSeries| volume_unit(&s.venue, &s.symbol, lane);
    let e = &profile.engine;
    let impact = e.slippage.as_ref().is_some_and(|s| s.model == "market_impact");
    if e.volume_limit.is_some_and(|v| v != 0.0) || impact {
        let unstated = series.iter().filter(|s| unit(s) == VolumeUnit::Unstated);
        let rows: Vec<String> = unstated.map(|s| format!("{} on {}", s.symbol, s.venue)).collect();
        if !rows.is_empty() {
            return Err(HarnessError::Data(format!(
                "engine.volume_limit and the market_impact slippage model compare an order with \
                 the series' volume, and \
                 the trade prints of {} count it in contracts of the instrument's ctVal, which only its \
                 row states and a backtest does not hold, where an order counts the base. Run it on the \
                 bar lane, whose candles state the base, or drop those keys",
                rows.join(", ")
            )));
        }
    }
    Ok(series.iter().filter(|s| unit(s) == VolumeUnit::Coin).map(|s| s.symbol.clone()).collect())
}

/// Refuse a multi-symbol universe whose series are not the same length.
///
/// `StrategyEngine::new` carries `assert!(lengths.len() <= 1, ...)` — a plain `assert!`, so it
/// fires in release — and nothing between the store and it aligns anything. Left alone, a universe
/// whose members have different listing dates aborts the process AFTER the whole store scan is
/// paid for, with a message naming neither symbol.
///
/// This is the pre-flight the tick lane already performs: `tick_replay`'s `replay_ticks_core`
/// returns `TickReplayError::MisalignedSeededBars`. ⚠ It refuses rather than repairing: forward-fill and
/// intersection are a profile key in a later plan, and picking one here would silently change what
/// a run means.
fn refuse_ragged_series(loaded: &[(String, Vec<Bar>)]) -> Result<(), HarnessError> {
    if loaded.len() < 2 {
        return Ok(());
    }
    let first_len = loaded[0].1.len();
    if loaded.iter().all(|(_, s)| s.len() == first_len) {
        return Ok(());
    }
    let mut rows: Vec<String> =
        loaded.iter().map(|(symbol, s)| format!("{symbol}={}", s.len())).collect();
    rows.sort();
    Err(HarnessError::Data(format!(
        "the requested symbols returned series of different lengths ({}) — the bar engine requires \
         one row per symbol per step and refuses rather than guessing which rows to invent. A \
         member listed later than the window start, or a gap the store never filled, is the usual \
         cause; narrow the window to the range every symbol covers, or run them separately",
        rows.join(", ")
    )))
}

/// Load this profile's BAR series from `store`, one `load_bars` per RESOLVED series (a cross-venue
/// `[[data.series]]` slice reads each series from its own venue; the single-venue form expands to
/// the same thing), window-joining the stored market `funding` series when `attach_funding` is on.
///
/// Extracted from [`run_backtest`]'s bar arm (a pure code move) so the walk-forward runner loads
/// the SAME bars — funding join included — as a plain run of the same profile.
///
/// `cancel` is read before EACH series load ([`stop_if_cancelled`]): a wide universe is one store
/// scan per symbol, and a caller whose client has gone stops paying for the scans it has not
/// started. `None` reads nothing.
///
/// `data_warnings` collects what the load found and ran anyway, for the caller's report.
///
/// The second half of the answer is the engine's FUNDING SCHEDULE (`attach_funding_series`), which the
/// caller hands to every engine it builds over these bars (`StrategyEngineConfig::funding_schedule`), a
/// walk-forward window's included: the schedule spans the whole range, and a window charges only the
/// instants inside it.
pub(crate) fn load_profile_bars(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    cancel: Option<&AtomicBool>,
    data_warnings: &mut Vec<String>,
) -> Result<RunBars, HarnessError> {
    let range = profile.range()?;
    let mut schedule = FundingSchedule::new();
    // A lane's or a file's bars are not the venue's candles, and the report says so (decision 0268).
    data_warnings.extend(bar_source::source_line(profile));
    let loaded: Vec<(String, Vec<Bar>)> = profile
        .data
        .resolved_series()
        .iter()
        .map(|s| {
            stop_if_cancelled(cancel)?;
            let interval = &profile.data.interval;
            let mut price =
                bar_source::load_series_bars(profile, store, &s.venue, &s.symbol, interval, range)?;
            // The stored market `funding` series for that same `(venue, symbol)`, when the run
            // attaches it, becomes the strategy's `Bar.funding` signal and the engine's schedule.
            let (venue, symbol) = (s.venue.as_str(), s.symbol.as_str());
            if let Some(charges) = funding_for_series(
                profile.engine.attach_funding,
                store.as_ref(),
                venue,
                symbol,
                range,
                &mut price,
                data_warnings,
            )? {
                schedule.push((s.symbol.clone(), charges));
            }
            Ok((s.symbol.clone(), price))
        })
        .collect::<Result<Vec<_>, HarnessError>>()?;
    refuse_ragged_series(&loaded)?;
    Ok((loaded, schedule))
}

/// Load the opt-in INTRABAR DETAIL TAPE (`data.detail_interval`) — one finer series per resolved
/// symbol, over the SAME range as the coarse bars — in the shape
/// [`vike_simulator::StrategyEngineConfig::granular_by_symbol`] takes. EMPTY when the key is absent, which is the
/// byte-identical no-op: `StrategyEngine::new` buckets nothing and every symbol fills on the
/// coarse lane.
///
/// # What this door unlocks, and why the engine half already existed
///
/// The granular sub-bar lane (`StrategyEngine::fill_pending_granular`, and the per-coarse-step
/// bucketing in `StrategyEngine::new`) has been in the engine since the port and could be reached
/// by NO profile: nothing in `super::profile` assigned `granular_by_symbol`, so the finest fill
/// tier this engine has was live code with no door. That is why this is a loader rather than a
/// second replay engine — the ambiguity resolution item 2 asks for is what
/// `fill_pending_granular` already does by walking sub-bars in time order instead of handing the
/// triggered pair to `vike_fills::resolve_intrabar_fills` to order adverse-first.
///
/// # Three properties that are load-bearing rather than incidental
///
/// 1. **The symbol list is `ProfileData::resolved_series`, the same list the coarse load walks.**
///    `StrategyEngine::new` `expect`s that every `granular_by_symbol` key is a symbol of the run
///    (the third undocumented-abort panic recorded on that function), and building the two lists
///    from one source makes that panic unreachable from this door BY CONSTRUCTION rather than by
///    a check that could rot.
/// 2. **[`refuse_ragged_series`] is deliberately NOT applied.** The coarse series must be
///    length-aligned because the bar engine takes one row per symbol per step; the detail series
///    are bucketed per symbol INDEPENDENTLY and never indexed in lockstep, so two symbols whose
///    finer tapes have different lengths are correct rather than ragged.
/// 3. **An EMPTY detail series is refused, not skipped.** `StrategyEngine::new` has
///    `if subs.is_empty() { continue; }`, so a symbol the store holds no finer series for would
///    silently fall back to the coarse guessing lane while the operator believed the realism knob
///    was on — the same silence the lane-asymmetric `[engine]` refusals exist to stop, landing on
///    a per-symbol basis where it is harder to notice. It is an I/O-time refusal because nothing
///    load-time can know what the store holds (the shape `ProfileEngineResolution::build`'s winners-coverage
///    check already uses).
pub(super) fn load_profile_detail_bars(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> Result<Vec<(String, Vec<Bar>)>, HarnessError> {
    let Some(interval) = profile.data.detail_interval.as_deref() else {
        return Ok(Vec::new());
    };
    // The raw string is what the store is asked for, but the value is only legitimate if it
    // RESOLVES: the parse, the bar-mode-only rule and the strictly-finer-than-base comparison all
    // live on `ProfileData::detail_interval_ms`, and this call is where they are raised on the run
    // path. Discarding the millisecond answer is deliberate — the engine buckets by timestamp
    // against the coarse bars' own edges and needs no step size of its own.
    profile.data.detail_interval_ms()?;
    let range = profile.range()?;
    profile
        .data
        .resolved_series()
        .iter()
        .map(|s| {
            // From the same lane as the coarse bars when the profile names one (`data.source`).
            let subs =
                bar_source::load_series_bars(profile, store, &s.venue, &s.symbol, interval, range)?;
            if subs.is_empty() {
                return Err(HarnessError::Data(format!(
                    "data.detail_interval = {interval:?} but the store holds no {interval} bars \
                     for ({:?}, {:?}) in this range — the intrabar detail tape would silently \
                     fall back to the coarse stop-versus-target guess for that symbol alone, so \
                     the run is refused instead. Backfill the finer series, or drop the key",
                    s.venue, s.symbol
                )));
            }
            Ok((s.symbol.clone(), subs))
        })
        .collect()
}

/// Run one backtest profile end-to-end: resolve the strategy, load the configured data slice
/// from `store`, and run it (bar-mode `StrategyEngine` or tick-mode `replay_ticks`).
pub fn run_backtest(
    profile: &ProfileBacktest,
    // `Arc<dyn HistoryStore>`, not the concrete `HistoryStoreDataFusion`, so any store implementing the trait
    // can drive a single run — the `backtesthub` bin hands it the routed datahub client.
    // (It was written for vike-backfill's `poly_ch_backtest` bin and that bin's
    // `ClickHousePolyHistStore`, the #691 ClickHouse-reading backtest bridge — both deleted, the bin
    // by #2030 on 2026-09-20; the seam is what outlived them, and an archive-Parquet store dropped
    // into it unchanged.) The sweep entrypoints
    // (`sweep`/`euler`) take the SAME `Arc<dyn HistoryStore + Send + Sync>` — their rayon fan-out only
    // clones the `Arc` and calls this dispatcher, needing nothing beyond `Send + Sync`, which the
    // trait object provides — so the whole harness compiles against the trait with no DataFusion
    // (the harness never names the store engine; `vike-store` is a DEV dependency here).
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<BacktestResult, HarnessError> {
    run_backtest_cancellable(profile, store, None)
}

/// [`run_backtest`] with a STOP FLAG — what the backtesthub's `RunBacktest` arm calls with the
/// connection's peer-gone flag (`vike_backtesthub::backtesthub_server`'s `PeerWatch`). `None` is [`run_backtest`]
/// exactly: every check below is [`stop_if_cancelled`], which reads nothing when no flag is armed.
///
/// One run is ONE point, so the flag is read at its stage boundaries, not inside the engine: before
/// each series load (`load_profile_bars`), before the intrabar detail load, and once more before
/// the engine or the tick replay starts. ⚠ **A run already inside `StrategyEngine::run` or
/// `replay_ticks` finishes** — interrupting the fold would put a check on the engine's per-bar hot
/// path in `vike-simulator` for every run in the workspace — and its result is the caller's to discard.
pub fn run_backtest_cancellable(
    profile: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    cancel: Option<&AtomicBool>,
) -> Result<BacktestResult, HarnessError> {
    let strategy = profile.strategy.build()?;
    // The series the strategy reads by name beside its own bars (decision 0271), read once here.
    let declared = strategy.inputs();
    let range = profile.range()?;
    let venue = profile.data.default_venue();
    let series = profile.data.resolved_series();
    let symbols: Vec<String> = series.iter().map(|s| s.symbol.clone()).collect();
    // A symbol the simulator cannot book in the run's one ledger currency (`engine.account_currency`, the
    // dollar when absent) is refused before anything is read, unless `engine.conversion` names the
    // pair of its quote (decision 0243), which books it at that pair's rate.
    let quotes = crate::data_plan::conversion_quotes(profile)?;
    refuse_off_ledger_series(
        profile,
        series
            .iter()
            .map(|s| (s.venue.as_str(), s.symbol.as_str()))
            .filter(|(_, s)| crate::data_plan::quoted_in(s, &quotes).is_none()),
    )?;
    // ...and a coin-margined contract is booked in its settle coin, or refused in words.
    let coin = profile_run_ledger_coin(profile)?;

    // ⚠ THE DATA PRE-FLIGHT, and it is placed here rather than first on purpose. Every refusal
    // above still fires before it, so this lane's existing error ORDER is untouched (the property
    // `run_backtest_over_bars`'s doc calls a contract) — and it cannot change what any profile
    // written before `data.require_coverage`/`data.universe` existed does, because
    // `crate::data_plan::enforce` reads NOTHING from the store unless one of them, or
    // `engine.snap_to_properties`, is armed.
    //
    // What an armed gate ends: a window with a complete trade tape and no book at all loads,
    // runs to completion and REPORTS FILLS. Nothing between the store and the engine notices —
    // per series both manifests look unremarkable, and `vike_data::find_gaps` is blind to the
    // commonest shape of it by construction (its holes are strictly inside a recorded span, so a
    // window that opens before the tape does has no gap). `crates/vike-data/src/store/coverage.rs`'s
    // module doc carries the measurement.
    //
    // `WARN` reaches the operator through the log AND `BacktestResult::data_warnings`, because the
    // log is the daemon's: over `--addr` the report is all the caller gets, and the strict universe
    // refusal promises `covered` runs "with the finding on the record". The REFUSAL is the return
    // value, which is what a refusal should be.
    let mut data_warnings =
        crate::data_plan::enforce(profile, store.as_ref(), "the store this run opened")?;
    for line in &data_warnings {
        tracing::warn!("{line}");
    }
    // The strategy's inputs, read and judged before the price lane is: one the store does not cover
    // over the window refuses the run by name (`crate::data_plan::load_inputs`). A strategy that
    // declares none reads nothing here and its params carry the empty `vike_simulator::Inputs`.
    let inputs = crate::data_plan::load_inputs(profile, &declared, store.as_ref())?;
    // The conversion pairs `engine.conversion` names (decision 0243), read and judged the same way: one
    // the store does not cover over the window refuses the run by name. Each then names the symbols it
    // BOOKS, or the run is refused in words for what the booking does not value. A profile that names
    // none reads nothing here and its params carry no conversion.
    let mut conversions = crate::data_plan::load_conversions(profile, store.as_ref())?;
    crate::data_plan::book_conversions(profile, &mut conversions, coin.is_some())?;

    let mut result = match profile.data.kind {
        ReplayKind::Bar => {
            // Both halves of the bar lane live in shared helpers so the walk-forward runner
            // (`crate::walkforward::runner`) builds the SAME engine params and loads the SAME bars.
            // ⚠ Deliberately NOT delegating to `run_backtest_over_bars` below — see its doc: the
            // delegation would reorder this lane's errors.
            let properties_error = Arc::default();
            let mut params = bar_engine_params(profile, &store, &properties_error)?;
            params.inputs = inputs;
            params.conversions = conversions;
            let (bars, funding_schedule) =
                load_profile_bars(profile, &store, cancel, &mut data_warnings)?;
            params.funding_schedule = funding_schedule;
            stop_if_cancelled(cancel)?;
            // Opt-in intrabar detail tape (`data.detail_interval`) — EMPTY unless the key is set,
            // which is the byte-identical no-op. Assigned AFTER the two calls above so this lane's
            // existing error ORDER is untouched (see `run_backtest_over_bars`'s doc on why that
            // order is a contract), and assigned HERE rather than inside `bar_engine_params`
            // deliberately: that factory is shared with `run_backtest_over_bars`, whose caller is
            // a walk-forward window holding a SLICED coarse series. `StrategyEngine::new` buckets
            // a sub-bar by `partition_point(|e| e <= sub.ts) - 1`, so every sub-bar past the last
            // coarse bar lands in the LAST bucket — a whole-range tape against a windowed slice
            // would let the final step of every training window fill against the future. The
            // pairing is refused in `ProfileBacktest::refusals` rather than relied on here, but
            // the wiring is kept where it cannot happen at all.
            params.granular_by_symbol = load_profile_detail_bars(profile, &store)?;
            stop_if_cancelled(cancel)?;
            let result = StrategyEngine::new(bars, strategy, params).run();
            refuse_after_failed_properties(&properties_error)?;
            result
        }
        ReplayKind::Tick => {
            // Opt-in fee SCHEDULE (G7). `None` leaves the flat `fee_rate` chain untouched;
            // `validate` already refused the two together; neither stated takes the venue table
            // (`fee_schedule_for_run`). `build_for(&series)` for the same
            // reason the bar arm above uses it: the `kind = "venue"` LANE comes from this run's
            // own series.
            let (fee_schedule, fee_schedules) = fee_schedule_for_run(
                profile.engine.fee.as_ref(),
                profile.engine.fee_rate,
                profile.engine.off_dollar_ledger(),
                &series,
                &profile.kept.rows,
            )?;
            let fee_underlyings = fee_underlyings_for_run(fee_schedule, &fee_schedules, &series)?;
            // Opt-in binary-resolution settlement (G6). Built here (not in `validate`) because it
            // reads the winners sidecar relative to the profile's own directory and cross-checks
            // the run's actual symbols.
            let resolution = profile
                .engine
                .resolution
                .as_ref()
                .map(|r| r.build(profile.base_dir.as_deref(), &symbols))
                .transpose()?;
            let (resolution, resolution_end_timestamp) = match resolution {
                Some((src, end)) => (Some(src), end),
                None => (None, None),
            };
            // Opt-in position sizer (Task 12). Same wire-in as the bar arm above.
            let sizer = profile.engine.sizer.as_ref().map(|s| s.build()).transpose()?;
            // The same slippage terms the bar arm builds (`[engine.slippage]`, absent = none).
            let slip = slippage_terms(profile.engine.slippage.as_ref())?;
            let mut params = StrategyEngineConfig {
                cash: profile.engine.cash,
                // Same label as the bar arm: `replay_ticks` guards its symbols against it.
                account_currency: profile.engine.account_currency.clone(),
                fee_rate: profile.engine.fee_rate,
                fee_schedule,
                fee_underlyings,
                fee_schedules,
                slippage: slip.fraction,
                slippage_amount: slip.amount,
                resolution,
                resolution_end_timestamp,
                // See the bar-mode branch above for the rationale — same field, same converter.
                risk_limits: risk_limits_for(profile),
                // Opt-in FIFO queue-position fill (tick lane only — `validate` rejects it on bars).
                // When set, `StrategyEngine::new` builds a `QueueTracker` and `run_ticks` routes
                // resting limits — including tagged maker quotes — through the queue gate, so a quote
                // fills only once a taker trade consumes the size ahead of it. Absent ⇒ `None` ⇒ the
                // frozen simple-crossing fill, byte-identical.
                queue_model: profile.engine.queue_model_kind()?,
                queue_seed_depth: profile.engine.queue_seed_depth.unwrap_or(0.0),
                queue_min_hold_ms: profile.engine.queue_min_hold_ms.unwrap_or(0),
                // Opt-in ORDER latency (two legs): `order_latency_ms` is the ENTRY leg — every
                // strategy order action reaches the matching engine that late; `fill_latency_ms` is
                // the RESPONSE leg — the strategy LEARNS of a fill that late (its shadow position
                // lags), so it can't react inside the gap. Either > 0 arms the model; both `0` ⇒
                // `None` ⇒ zero-latency, byte-identical.
                latency_model: (profile.engine.order_latency_ms > 0
                    || profile.engine.fill_latency_ms > 0)
                    .then(|| {
                        vike_simulator::LatencyModelKind::constant(
                            profile.engine.order_latency_ms.saturating_mul(1_000_000),
                            profile.engine.fill_latency_ms.saturating_mul(1_000_000),
                        )
                    }),
                // Equity-curve density (see `ProfileEngine::equity_sample_every`): absent ⇒
                // `EveryTick` ⇒ the frozen per-tick curve, byte-identical. A sweep over a very
                // long tape sets it to stop paying 16 bytes/tick per run for samples it never
                // reads. `validate` already rejected it in bar mode.
                equity_sampling: profile.engine.equity_sampling(),
                // `timeframes` is deliberately absent: `validate` refuses it on this lane.
                // ⚠ `cash_gate` is refused on this lane too (`validate`), so the value forwarded
                // here is always `false` and `run_ticks` would not read it anyway — the gate is
                // `StrategyEngine::run`'s bar step alone. It is assigned rather than omitted so
                // `crates/vike-backtest/tests/engine_cfg_reaches_the_engine.rs` keeps REQUIRING it at
                // both sites: an exemption row would stop checking this field here forever, and
                // forwarding a constant `false` costs nothing.
                cash_gate: profile.engine.cash_gate,
                // `in_bar` likewise: refused on this lane (`validate`), forwarded (always every rule
                // off) so the gate keeps requiring it at both sites; `run_ticks` reads none of it.
                in_bar: profile.engine.in_bar.unwrap_or_default(),
                // The law a tagged plain limit fills by, as the bar arm resolves it (a maker's lane).
                optimistic_quotes: optimistic_quotes(profile.engine.quotes, &profile.strategy).0,
                closed_trades: profile.engine.closed_trades.then(|| TradeClosedTerms {
                    options: TradeClosedLedgerOptions {
                        whole_entry_fee: profile.engine.whole_entry_fee,
                    },
                    risk_free_rate: profile.engine.risk_free_rate.unwrap_or(DEFAULT_RISK_FREE_RATE),
                }),
                // ...and `decide` for the SAME two reasons, spelled once next to its twin:
                // `"simultaneous"` is refused on this lane (`refusals`), so the value forwarded
                // here is always `DecideMode::Sequential` and `run_ticks` reads neither field
                // anyway — and it is assigned rather than omitted so the gate above keeps
                // REQUIRING it at both sites instead of taking a permanent exemption row.
                decide: super::profile::decide_mode(profile.engine.decide.as_deref())?,
                maint_margin: profile.engine.maint_margin,
                liquidation_buffer: profile.engine.liquidation_buffer,
                venue_style_liquidation: profile.engine.venue_style_liquidation,
                volume_limit: profile.engine.volume_limit,
                max_open_positions: profile.engine.max_open_positions,
                max_open_long: profile.engine.max_open_long,
                max_open_short: profile.engine.max_open_short,
                multiplier: profile.engine.multiplier,
                multipliers: profile
                    .engine
                    .multipliers
                    .iter()
                    .map(|(k, v)| (k.clone(), *v))
                    .collect(),
                // A coin ledger's contract values (`profile_run_ledger_coin`); empty for every other run.
                contract_values: coin.map(|c| c.values).unwrap_or_default(),
                // ...and which count their prints' volume in the coin (`coin_volume`: none today), or the
                // refusal of the volume keys over prints whose unit only a row states.
                coin_volume: coin_volume(profile, ReplayKind::Tick)?,
                // The spot account, as the bar arm states it.
                cash_symbols: super::profile::cash_symbols(
                    profile.engine.margin_mode.as_deref(),
                    &profile.data.resolved_series(),
                )?,
                leverage: profile.engine.leverage,
                clamp_to_leverage: profile.engine.clamp_to_leverage,
                settlement_period_ms: profile.engine.settlement_period_ms,
                session_gate: profile.engine.session_gate,
                sizer,
                // See `ProfileEngine::emulator_release_stops`'s doc: the harness default is `false`
                // (matching `StrategyEngineConfig::default()`), and this is the second of the two
                // construction sites that must assign it for a profile's explicit `true` to reach
                // the engine.
                emulator_release_stops: profile.engine.emulator_release_stops,
                // The declared warm-up FLOOR (`data.warmup`), assigned on BOTH lanes because
                // `run_ticks` gates on the same expression `run` does — its index counts EVENTS,
                // so `warmup = "200bars"` means 200 ticks here. `ProfileData::warmup_steps` refuses a
                // DURATION spelling on this lane rather than dividing it by a cadence a tick tape
                // does not have.
                warmup: profile.data.warmup_steps()?,
                ..Default::default()
            };

            // Opt-in depth-capped fill model (`fill_model = "l2book"`): a resting order fills only up
            // to the DISPLAYED book depth (insufficient depth ⇒ it rests), vs the default L1
            // spread-crossing model (fills the full size at the quote). Resolved STRICTLY by
            // `ProfileEngine::fill_model_kind` — absent ⇒ `Tick`, an unknown spelling is a Validation
            // error (already caught by `validate` at load), never a silent optimistic-`Tick`
            // fallback. The tick-replay loader respects an explicit `L2Book` choice and forces
            // `Tick` otherwise, so assigning the resolved `Tick` default here is byte-identical.
            params.fill_model = profile.engine.fill_model_kind()?;
            // The inputs, read as of each tick (`StrategyEngineConfig::inputs`).
            params.inputs = inputs;
            // The conversion pairs, a rate as of each fill (`StrategyEngineConfig::conversions`).
            params.conversions = conversions;

            // The `market_impact` model on the TICK lane too (see the slippage block of `validate`
            // for why the tick lane is not refused it): the SAME terms the bar arm assigns, so the two
            // lanes cannot hold differently-configured models; which TERMS each fill is charged is
            // decided per fill by `crates/vike-simulator/src/engine/sim_broker/slippage.rs`'s
            // `impact_terms`, not here.
            if let Some((model, window)) = slip.impact {
                params.impact = Some(model);
                params.impact_window = window;
            }
            // The funding schedule on the TICK lane too: each instant is charged on the symbol's
            // first tick at or after it, on the position held then (`StrategyEngineConfig::funding_schedule`).
            // No bar carries a signal here, so the join is skipped (an empty slice).
            for s in &series {
                let (venue, symbol) = (s.venue.as_str(), s.symbol.as_str());
                let store = store.as_ref();
                if let Some(charges) = funding_for_series(
                    profile.engine.attach_funding,
                    store,
                    venue,
                    symbol,
                    range,
                    &mut [],
                    &mut data_warnings,
                )? {
                    params.funding_schedule.push((s.symbol.clone(), charges));
                }
            }

            let config = TickReplayConfig {
                venue: venue.clone(),
                symbols: symbols.clone(),
                series: (!profile.data.series.is_empty()).then(|| series.clone()),
                range,
                seed_bar_interval_ms: profile.engine.seed_bar_interval_ms,
                params,
                snap_to_properties: profile.engine.snap_to_properties,
                // Opt-in feed-latency delivery (tick lane only — `validate` rejects it on the bar
                // lane). Absent = `false` = today's venue-ordered replay, byte-identical.
                feed_latency: profile.engine.feed_latency,
            };

            stop_if_cancelled(cancel)?;
            replay_ticks(store, strategy, config).map_err(|e| HarnessError::Data(e.to_string()))?
        }
    };
    data_warnings.append(&mut result.data_warnings);
    result.data_warnings = data_warnings;
    Ok(result)
}

/// [`run_backtest`]'s BAR lane over bars the CALLER already holds — the seam a walk-forward window
/// needs, and the reason the search apparatus can be reused per window at all.
///
/// # Why this exists
///
/// Every candidate a search evaluates goes through `super::paramscan`'s `point_row`, which calls
/// [`run_backtest`], which calls [`load_profile_bars`] — so scoring `G` candidates costs `G` loads
/// of the profile's whole range. A walk-forward window needs the opposite: score each candidate on
/// the TRAIN slice it is already holding. It cannot express that slice as a store range either,
/// because [`crate::walkforward::walk_forward_strategy`] splits by INDEX and deliberately reads no
/// `Bar.ts` at all — asking it for a range would mean teaching it a calendar it is built to avoid.
///
/// What this removes is the LOAD, not the store: `bar_engine_params` still takes it, because
/// `snap_to_properties` reads a properties grid from it. That read is skipped unless the profile
/// asks for it, so the per-candidate cost of this path is the engine run and nothing else.
///
/// # ⚠ Why [`run_backtest`]'s bar arm does NOT delegate here
///
/// It would reorder that lane's errors. `run_backtest` resolves the strategy FIRST, before it ever
/// touches the store, so a bad strategy name on a bar profile fails before a store failure can. A
/// delegating arm would have to load the bars before calling this, inverting that pair. The three
/// lines are assembly over shared helpers — `ProfileStrategy::build`, `bar_engine_params`,
/// `StrategyEngine::new` — not a second implementation of anything, and `run_backtest` staying
/// byte-identical is what lets the walk-forward work claim it moved no existing number.
///
/// `properties_error` is the caller's so that it can outlive this run: a walk-forward passes one for
/// the whole walk, because a window's search keeps a failed candidate only as a `NaN`, and only the
/// slot can say that candidate lost to a failed read rather than to the others.
///
/// `conversions` are the pairs `engine.conversion` names (decision 0243), loaded, judged and booked by
/// the caller with its bars over the whole range they were cut from (`crate::data_plan::load_conversions`
/// and `book_conversions`, as [`run_backtest`] does): each symbol a pair books is booked at its rate here,
/// by the simulator's one rule. Empty for a caller that loaded none, and then a profile naming a pair is
/// refused, never run at par.
pub(crate) fn run_backtest_over_bars(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    bars: Vec<(String, Vec<Bar>)>,
    funding: &[(String, Vec<FundingRatePoint>)],
    conversions: &[vike_simulator::ConversionSeries],
    properties_error: &Arc<OnceLock<String>>,
) -> Result<BacktestResult, HarnessError> {
    let strategy = profile.strategy.build()?;
    // This door holds bars and loads no input (decision 0271), so a strategy reading one is refused.
    crate::data_plan::refuse_unplanned_inputs(
        &strategy.inputs(),
        "a run over bars it was handed (a walk-forward window, a search over held bars)",
    )?;
    // ...and books the conversion pairs its caller loaded (decision 0243): a profile naming one that
    // reached here with none is refused.
    if conversions.is_empty() {
        crate::data_plan::refuse_unplanned_conversions(
            &profile.engine.conversion,
            "a run over bars it was handed (a walk-forward window, a search over held bars)",
        )?;
    }
    // Each held series' venue is its profile series' (the bars were loaded from them, by symbol).
    let series = profile.data.resolved_series();
    let default_venue = profile.data.default_venue();
    let venue_of = |symbol: &str| {
        series
            .iter()
            .find(|s| s.symbol == symbol)
            .map_or(default_venue.as_str(), |s| s.venue.as_str())
    };
    refuse_off_ledger_series(
        profile,
        bars.iter()
            .map(|(symbol, _)| (venue_of(symbol), symbol.as_str()))
            .filter(|(_, s)| !crate::data_plan::books(conversions, s)),
    )?;
    // The caller holds the bars and not their venue: `bar_engine_params` reads the profile's series for
    // a coin ledger's values, and refuses what it cannot book there. `funding` is the schedule the
    // caller's `load_profile_bars` produced with those bars (empty when it attached none).
    let mut params = bar_engine_params(profile, store, properties_error)?;
    params.funding_schedule = funding.to_vec();
    params.conversions = conversions.to_vec();
    let result = StrategyEngine::new(bars, strategy, params).run();
    refuse_after_failed_properties(properties_error)?;
    Ok(result)
}

/// One series' funding for a run, both lanes (a Studio slice is a profile run too,
/// `ProfileBacktest::for_slice`): `None` when the run does not attach funding to it
/// (`attach_funding` is `engine.attach_funding`; absent = ON for a perpetual, [`attaches_funding`])
/// or the store holds none for it in the window, which is a report line saying how to fetch it or
/// run without it on purpose; else its charges, any hole in the series a report line too. `price`
/// is the series' bars, which gain the strategy's `Bar.funding` signal (`&mut []` on a tick lane).
fn funding_for_series(
    attach_funding: Option<bool>,
    store: &(dyn HistoryStore + Send + Sync),
    venue: &str,
    symbol: &str,
    range: vike_data::TimestampRange,
    price: &mut [Bar],
    data_warnings: &mut Vec<String>,
) -> Result<Option<Vec<FundingRatePoint>>, HarnessError> {
    if !attaches_funding(attach_funding, venue, symbol) {
        return Ok(None);
    }
    let Some((charges, gaps)) = attach_funding_series(store, venue, symbol, range, price)? else {
        data_warnings.push(match attach_funding {
            Some(true) => format!(
                "{symbol}: engine.attach_funding is on and {NO_FUNDING_HISTORY} for it in this \
                 window, so this report charged it none"
            ),
            _ => format!(
                "{symbol}: a perpetual, and {NO_FUNDING_HISTORY} for it in this window, so this \
                 report charged it NO FUNDING — fetch it with `vike-cli data hist \
                 fetch {venue}:{symbol}:funding`, or set `engine.attach_funding = false` to run \
                 without it on purpose"
            ),
        });
        return Ok(None);
    };
    data_warnings.extend(gaps);
    Ok(Some(charges))
}

/// Load the stored market `funding` series for `(venue, symbol)` and turn it into what the run reads:
/// the strategy's `Bar.funding` signal on `price` ([`window_join_funding`]), the engine's charges
/// (`funding::funding_charges`, each at its own instant and the venue's own accrual,
/// [`FundingAccrual::of_venue`]) and the report lines naming any hole in the series inside the run's
/// window (`funding::funding_gaps`). The series is read under `vike_data::source::FUNDING_INTERVAL`,
/// the label the market funding-rate backfill writes; each stored `Bar` carries `funding =
/// Some(rate)` at its funding instant and, when the venue published one, its mark on `close`
/// (`crates/vike-backfill/src/funding_rate.rs`'s `point_to_bar`; `0.0` = none). An empty or absent
/// series leaves `price` unchanged and answers `None` (the caller warns, never errors).
fn attach_funding_series(
    store: &(dyn HistoryStore + Send + Sync),
    venue: &str,
    symbol: &str,
    range: vike_data::TimestampRange,
    price: &mut [Bar],
) -> Result<Option<FundingRateSeries>, HarnessError> {
    let fbars = store
        .load_bars(venue, symbol, vike_data::source::FUNDING_INTERVAL, range)
        .map_err(|e| HarnessError::Data(e.to_string()))?;
    // Each stored bar's `funding` IS the rate at its ts, ts-ascending (the store's contract, which
    // `window_join_funding` and the engine's schedule both rely on); `close` is the published mark.
    let mut points: Vec<(i64, f64, Option<f64>)> = fbars
        .iter()
        .filter_map(|b| b.funding.map(|r| (b.timestamp, r, (b.close > 0.0).then_some(b.close))))
        .collect();
    // One point per settlement, a guard: the funding ingest writes one bar per settlement hour, but the store
    // dedups by commit key and never by row, so a series written before that rule (two overlapping backfills)
    // can hold a settlement twice.
    points.dedup_by_key(|p| p.0);
    if points.is_empty() {
        tracing::warn!(
            venue,
            symbol,
            "engine.attach_funding is on but the stored `funding` series is empty/absent — no \
             funding is charged for this series"
        );
        return Ok(None);
    }
    let events: Vec<(i64, f64)> = points.iter().map(|&(ts, rate, _)| (ts, rate)).collect();
    let stats = window_join_funding(price, &events);
    if stats.collisions > 0 {
        tracing::info!(
            venue,
            symbol,
            collisions = stats.collisions,
            "engine.attach_funding: {} funding point(s) share a price bar with another — the \
             strategy reads their SUM on that bar, and each is charged at its own instant",
            stats.collisions
        );
    }
    let charges = funding::funding_charges(&points, FundingAccrual::of_venue(venue));
    // Holes are judged over the bars the run replays, or (the tick lane, no bars) the profile's range.
    let ts: Vec<i64> = events.iter().map(|&(ts, _)| ts).collect();
    let window = match (price.first(), price.last()) {
        (Some(first), Some(last)) => Some((first.timestamp, last.timestamp)),
        _ => range.start.zip(range.end),
    };
    let gaps =
        window.map_or_else(Vec::new, |(from, to)| funding::funding_gaps(symbol, &ts, from, to));
    Ok(Some((charges, gaps)))
}

/// `risk_limits_for` needs no store at all — a plain trait-only test
/// still proves the struct-level half of the byte-identical-default claim: absent `[risk]` maps
/// to `None`, present maps to the SAME `ProfileRisk::to_risk_limits` conversion the field-level
/// `harness::profile` tests already pin. The behavioral half (a real `OrderBrokerSimulator` denial) lives in
/// `tests/harness_risk_wiring.rs`, which needs only a fixture `HistoryStore`, not `HistoryStoreDataFusion`.
#[cfg(test)]
mod risk_limits_for_tests;

/// `refuse_ragged_series` needs no store at all — a plain trait-only test (no
/// `HistoryStoreDataFusion`) still proves this guard, unlike the store-backed `tests` module below.
#[cfg(test)]
mod align_tests;

/// A symbol whose name states another fiat quote is refused before the engine is built: the tests
/// need a store (a bar and a tick series), so they sit beside `tests` rather than with the
/// store-free guards above.
#[cfg(test)]
mod foreign_quote_tests;

// `refuse_inverse_contracts` at the doors a profile reaches, and the worked numbers behind it.
#[cfg(test)]
mod inverse_contract_tests;

// A backtest of each FX venue's symbol on its stored bars and ticks, with no venue session (owner 2026-11-14).
#[cfg(test)]
mod fx_stored_data_tests;

// Every test here builds a concrete `HistoryStoreDataFusion` fixture from the `vike-store` dev-dependency;
// the library itself compiles `run_backtest` against the seam and links no engine.
#[cfg(test)]
mod tests;
