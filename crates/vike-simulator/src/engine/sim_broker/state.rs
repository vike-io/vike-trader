//! `OrderBrokerSimulator`'s state: the broker struct, the per-symbol `OrderBrokerSimulatorSymbolState` and its audit records.

use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use vike_execution::RiskGate;
use vike_marketdata::{Bar, FundingRatePoint};
#[cfg(doc)]
use vike_model::OrderBroker;
use vike_model::{
    FeeSchedule, Fill, OrderDrop, OrderFees, OrderResting, Position, SymbolProperties, Trade,
};
use vike_venues::SessionCalendar;

use crate::schedule::Schedule;
use vike_analytics::sizing::PositionSizer;

use super::knobs::{DecideMode, FillModelKind, ShortfallCover};
#[cfg(doc)]
use super::params::StrategyEngineConfig;
use super::params::{OptionExpirySource, ResolutionSource};

/// All mutable per-symbol state — ONE per symbol.
pub struct OrderBrokerSimulatorSymbolState {
    pub pos: Position,
    pub pending: Vec<OrderResting>,
    pub realized: f64,
    /// active protective stop price (None == none)
    pub stop: Option<f64>,
    pub entry_fee: f64,
    pub entry_timestamp: i64,
    /// last seen price
    pub price: f64,
    /// running high since the position opened (MAE/MFE)
    pub hi_since: f64,
    /// running low since the position opened
    pub lo_since: f64,
    /// higher-TF aggregates: (tf, target_ms, coarse bars)
    pub tf: Vec<(String, i64, Vec<Bar>)>,
    /// granular sub-bars bucketed per coarse step
    pub sub: Vec<Vec<Bar>>,
    /// The orders of `pending` that fill by the MAKER LAW (`StrategyEngineConfig::optimistic_quotes`; an
    /// emulated order's child always), each `(qid, tag)` as placed. They rest in `pending` like every
    /// order and are judged, committed, amended and cancelled as every order placed by tag is; what
    /// differs is the law: `StrategyEngine::fill_tagged` (and its queued twin) fills them in their own
    /// pass, and every other pass over `pending` (the fill lanes, the in-bar rules, `cancel_all`, a
    /// protective stop's sibling cancel) leaves them where they are. An entry outlives its order until
    /// `OrderBrokerSimulator`'s `forget_ended_tags` prunes it. Empty for every run that places none.
    pub(crate) quotes: Vec<(u64, String)>,
    /// Orders HELD off the book until their release ([`OrderBrokerSimulator::submit_released`]), the live
    /// core's held table's twin: not in `pending`, so they commit no margin and no fill lane sees
    /// them until `OrderBrokerSimulator::release_held` judges and places them. Empty for every run that holds
    /// nothing.
    pub held: Vec<OrderHeldEntry>,
    /// The EMULATED orders (decision 0275, `crates/vike-simulator/src/engine/sim_broker/emulated.rs`): a pegged
    /// order or a visible size the run's venue does not hold, by the strategy's tag in accepted order, each
    /// worked through one child in `quotes` by the live core's own law. Empty for every run that sends none.
    pub(crate) emulated: IndexMap<String, super::emulated::EmulatedOrder>,
    /// Cumulative variation-margin profit already settled into realized for the CURRENT position
    /// (the `SettledProfit` accumulator). Always `0.0` unless
    /// [`StrategyEngineConfig::settlement_period_ms`] is `Some` — see [`OrderBrokerSimulator::settle_variation_margin`].
    /// Reset to `0.0` whenever the position goes flat is NOT performed here: the accumulator is a
    /// run-lifetime audit total per symbol, and the basis reset makes a per-position reset
    /// unnecessary (there is never unsettled profit left behind on a flat book).
    pub settled_profit: f64,
}

/// One order of [`OrderBrokerSimulatorSymbolState::held`]: the order, the release it waits for, and the pre-trade gate's
/// terms its verb would have been judged on (`None` for a verb the gate does not judge), which are
/// judged at the release instead.
pub struct OrderHeldEntry {
    pub release: vike_model::OrderRelease,
    pub gate: Option<SizingGateTerms>,
    pub order: OrderResting,
}

/// The request terms `OrderBrokerSimulator::gate_order` judges, kept for a held order's release.
pub struct SizingGateTerms {
    pub order_type: vike_model::OrderType,
    pub price: Option<f64>,
    pub trigger: Option<f64>,
}

// Portfolio backtest outcome is the shared `BacktestResult` (see `vike_analytics::result`).

/// A funding cashflow forwarded to the mirror (`_on_funding` twin): signed cash delta.
#[derive(Debug, Clone)]
pub struct MirrorFunding {
    pub symbol: String,
    pub amount: f64,
    pub timestamp: i64,
}

/// One binary-resolution settlement applied by the opt-in [`StrategyEngineConfig::resolution`] source
/// (pm-economics lane) — the DISTINCT tag for a settlement fill. The `Fill` fed to `on_fill` is a
/// normal full-size close with `fee == 0.0` / `is_maker == false`; this record (collected in
/// [`OrderBrokerSimulator::settlements`], insertion order) is how reports and tests tell a resolution
/// settlement apart from a trading fill.
#[derive(Debug, Clone, PartialEq)]
pub struct SettlementFill {
    pub symbol: String,
    /// terminal payout price the position was closed at (`0.0` or `1.0` for a binary market)
    pub payout: f64,
    /// unsigned settled quantity (the whole open position)
    pub qty: f64,
    /// the closing side: `-1` settled a long, `+1` settled a short
    pub side: i32,
    pub timestamp: i64,
}

/// Engine state + strategy-facing verbs/reads — the ctx every Strategy handler receives.
pub struct OrderBrokerSimulator {
    pub symbols: Vec<String>,
    pub bars: Vec<Rc<Vec<Bar>>>, // indexed like symbols; Rc so the dispatch loop can hand the
    // strategy a &Bar via a cheap refcount-bump handle instead of cloning (String alloc) each bar
    // — bars_for keeps reading the live series (unlike the mem::take that broke r4).
    pub n: usize,
    pub fee_rate: f64,
    pub cash: f64,
    /// Running NET funding cashflow (received-positive / paid-negative), the backtest twin of
    /// `vike_execution::Account.funding_paid`: the sum of every per-interval `MirrorFunding.amount`
    /// (`= -ContractValue::funding`) accrued in the bar loop. Observability only — funding already moves
    /// `cash`; this breaks it out so a report can show funding P&L distinctly. Surfaced on
    /// [`vike_analytics::BacktestResult::funding_paid`].
    pub funding_paid: f64,
    pub slippage: f64,
    /// see [`StrategyEngineConfig::slippage_amount`]
    pub slippage_amount: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
    /// The EXACT per-fill fee schedule, set by [`crate::StrategyEngine::new`] for every
    /// [`StrategyEngineConfig::fee_schedule`] shape EXCEPT the two it names as flattenable
    /// ([`FeeSchedule::PercentMakerTaker`] and [`FeeSchedule::Free`], for which a flat fraction
    /// is the whole shape and the arithmetic is identical). `None` there — and for the default
    /// `fee_schedule: None` — keeps the frozen `fee(size, price, rate, mult)` fold
    /// byte-identical.
    ///
    /// ⚠ The field name still says "curve" because `ProbabilityScaled`'s `p(1−p)` curve was the
    /// first shape to need it; it now carries a floor and a cap too. The name is left alone
    /// deliberately — it is `pub(crate)` and renaming it would churn call sites for a word,
    /// while `crate::StrategyEngine::new`'s own comment is the authority on what reaches it.
    pub(crate) fee_curve: Option<FeeSchedule>,
    /// Index si -> the schedule si's fills are charged at exactly ([`StrategyEngineConfig::fee_schedules`]),
    /// `None` for a symbol on the run-wide path. EMPTY unless the run lists one.
    pub(crate) fee_by_si: Vec<Option<FeeSchedule>>,
    /// Index si -> the index of the symbol whose mark is si's underlying
    /// ([`StrategyEngineConfig::fee_underlyings`]). EMPTY unless the run lists one, so the default fee path
    /// pays one `Vec::get`; `None` for a symbol whose schedule is not `PercentOfUnderlying`.
    pub(crate) fee_underlying_by_si: Vec<Option<usize>>,
    /// Per-symbol CONTRACT VALUE (index si, see [`StrategyEngineConfig::contract_values`]): the linear value of
    /// the symbol's multiplier unless the run listed another. The ONE source of a symbol's factor: every
    /// money site reads it through its `ContractValue` methods, so a linear symbol books the product it
    /// always did and an inverse one books its settle coin.
    pub(crate) value_by_si: Vec<vike_model::ContractValue>,
    /// The conversion series of the run ([`StrategyEngineConfig::conversions`]), as it stated them.
    pub(crate) conversions: Vec<super::conversion::ConversionSeries>,
    /// Per-symbol (index si): the booking of a symbol quoted in another currency than the ledger through
    /// its series in [`Self::conversions`] (`crates/vike-simulator/src/engine/sim_broker/conversion.rs`'s
    /// module doc), whose rate in force is written into [`Self::value_by_si`]. EMPTY unless the run names one.
    pub(crate) quote_conversion_by_si: Vec<Option<super::conversion::QuoteConversionCursor>>,
    /// Per-symbol (index si): whether its series counts volume in the settle coin, an inverse symbol
    /// listed in [`StrategyEngineConfig::coin_volume`] ([`Self::coin_volume_of`]).
    pub(crate) coin_volume_by_si: Vec<bool>,
    /// Per-symbol (index si): whether it trades on a spot (cash) account, listed in
    /// [`StrategyEngineConfig::cash_symbols`] ([`Self::cash_refusal`]).
    pub(crate) cash_by_si: Vec<bool>,
    pub leverage: Option<f64>,
    /// leverage-breach outcome shape: `false` (default) = deny whole through the live gate,
    /// `true` = truncate (see [`StrategyEngineConfig::clamp_to_leverage`])
    pub clamp_to_leverage: bool,
    /// THE ONE JUDGE (deny-vs-clamp phase 2): the literal live `vike_execution::RiskGate`, mounted by
    /// [`OrderBrokerSimulator::build_risk_gate`] — `Some` exactly when a pre-trade gate is armed
    /// ([`StrategyEngineConfig::risk_limits`] given, or `leverage` mapped to `initial_margin_requirement = 1/L`)
    /// AND the clamp escape hatch is off. `None` = the submit path never builds a context and is
    /// byte-identical to the pre-phase-2 engine.
    pub(crate) risk_gate: Option<RiskGate>,
    pub maint_margin: f64,
    /// see [`StrategyEngineConfig::venue_style_liquidation`]
    pub venue_style_liquidation: bool,
    /// see [`StrategyEngineConfig::liquidation_buffer`]
    pub liquidation_buffer: f64,
    /// see [`StrategyEngineConfig::shortfall_cover`]
    pub shortfall_cover: Option<ShortfallCover>,
    /// The cuts [`StrategyEngineConfig::shortfall_cover`] has filled, counted BEFORE each one reaches
    /// `on_fill`, so a strategy that sees this move knows the fill in hand is a margin call and no
    /// order of its own.
    pub shortfall_cover_fills: u64,
    /// Per symbol, the last step and side of a fill that reduced its position, kept only while
    /// [`StrategyEngineConfig::shortfall_cover`] is set: an opening fill on that side in that step is the
    /// entry of a close-then-enter reversal, which the rule does not refuse.
    pub(crate) shortfall_reduced: Vec<Option<(usize, i32)>>,
    /// Per symbol, its position size at the start of the step, and the account's equity then; kept only while
    /// [`StrategyEngineConfig::shortfall_cover`] is set (`OrderBrokerSimulator::note_bar_start`), for the fill gate's stacking exemption.
    pub(crate) shortfall_bar_size: Vec<f64>,
    pub(crate) shortfall_bar_equity: f64,
    pub cash_gate: bool,
    /// see [`StrategyEngineConfig::decide`] / [`DecideMode`]
    pub decide: DecideMode,
    /// The order every cross-sectional walk takes, as indices into [`Self::symbols`] — built
    /// ONCE at construction by [`DecideMode::cross_section_order`] and read through
    /// [`Self::cross_section()`].
    ///
    /// Stored rather than recomputed because [`Self::equity_now`] is on the per-step path and is
    /// also called from inside `size_entry` for every submit: sorting `n` strings there would put
    /// a comparison-sort in the sizing path for a value that cannot change after construction
    /// (`symbols` is fixed by `StrategyEngine::new`). Under [`DecideMode::Sequential`] it is the
    /// identity permutation, which is what keeps every fold's f64 bits frozen.
    pub(crate) cross_section: Vec<usize>,
    pub(crate) active_mask: Option<Vec<(String, Vec<bool>)>>,
    pub max_open_positions: usize,
    pub max_open_long: usize,
    pub max_open_short: usize,
    pub(crate) sizer: Box<dyn PositionSizer>,
    pub volume_limit: Option<f64>,
    pub equity_peak: f64,
    pub step: usize,
    /// One `OrderDrop` (symbol, kind-or-reason, size, weight) per gate-dropped fill — the strategy-observable
    /// refusal channel (a `Strategy` reads it off its `ctx`). Reasons in use: the order KIND for
    /// cash-gate drops, `"volume_cap"`, `"latency_reject"`, and — since deny-vs-clamp phase 2 —
    /// the live `RiskGate`'s OWN reason strings (`"insufficient-margin"`, `"below-min-qty"`, …)
    /// from the default deny-whole submit gate ([`Self::gate_order`]); a backtest denial
    /// carries the exact string the live path would publish in `OrderDenied`.
    pub dropped: Vec<OrderDrop>,
    /// ⚠ **Counts the below-min REVERSALS this engine SPLIT**, where the LIVE `RiskGate` denies the
    /// whole order.
    ///
    /// The condition: the order opposes the position, EXCEEDS it (so it flips through flat, which
    /// `vike_model::is_covered_reduce` does not treat as a reduce), and falls below a venue floor.
    /// [`Self::fill_terms`] fills the closing half and refuses the opening half in [`Self::dropped`].
    /// So neither engine opens the far side; a split still flattens where a denial would have left
    /// the position standing, because the fill lane must not strand a close.
    ///
    /// ⚠ **AT REAL VENUE FLOORS THIS IS UNREACHABLE ABOVE DUST, and that is the useful result** —
    /// the counter records it rather than discovers it.
    ///
    /// A flip must EXCEED the position, so the condition needs `|position| < flip < min_qty`: the
    /// position must ALREADY be below the floor. An opening order cannot create that state (the same
    /// floor refuses it), so it takes a prior COVERED REDUCE leaving a sub-floor remainder, or a
    /// venue RAISING its minimum over a position's life. Then the flip itself must also be below the
    /// floor. Against the real floors of a BTC perpetual as the venues listed them — binance
    /// `min_notional` 5.0, bybit 5.0, okx 1.0 — that means
    /// REVERSING A POSITION WORTH UNDER ~$5. Any strategy sizing above a few dollars per order can
    /// never reach it.
    ///
    /// The counter turns the argument above into something a run can contradict: a non-zero reading
    /// on a real profile would mean the reasoning here is wrong, and a run that reads zero cannot
    /// have moved by the split.
    pub below_min_reversals: u64,
    pub trades: Vec<Trade>,
    pub intrabar_both_hit: u32,
    pub symbol: Vec<OrderBrokerSimulatorSymbolState>, // indexed like symbols
    pub now: i64,
    pub fill_model: FillModelKind,
    /// opt-in emulator-mirroring stop release (see [`StrategyEngineConfig::emulator_release_stops`]);
    /// `false` (default) = a triggered resting stop fills same-event, byte-identical
    pub emulator_release_stops: bool,
    /// current step index (Python: strategy.index)
    pub index: usize,
    pub schedule: Schedule,
    /// ledger mirror sinks (Some when mirror enabled; `_on_fill`/`_on_funding` twins): each fill
    /// with the EXACT signed fee subtracted from cash, untagged (the mirror adapter mints ids
    /// itself). A settlement's fill reaches it too, after its [`SettlementFill`] in `settlements`.
    pub mirror_fills: Option<Vec<Fill>>,
    pub mirror_funding: Option<Vec<MirrorFunding>>,
    /// the closed-trade ledger, folding every fill on every run: what the strategy reads
    /// (`OrderBroker::closed_trades`); [`StrategyEngineConfig::closed_trades`] states its options and arms its report
    pub(crate) closed_trades: vike_analytics::closed_trades::TradeClosedLedger,
    /// [`StrategyEngineConfig::closed_trades`] was stated: the ledger is marked each bar and reported
    pub(crate) closed_trade_report: bool,
    /// point-in-time instrument-filter lookup, or `None` for an unconstrained fill path
    #[expect(clippy::type_complexity)]
    pub(crate) properties:
        Option<Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync>>,
    /// opt-in perp funding-rate source (see [`StrategyEngineConfig::funding_source`]); `None` (default) =
    /// funding comes only from `Bar.funding`, byte-identical to the pre-seam engine
    #[expect(clippy::type_complexity)]
    pub(crate) funding_source: Option<Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>>,
    /// The per-symbol funding schedule by symbol index (see [`StrategyEngineConfig::funding_schedule`]) and
    /// the index of each list's next uncharged entry. BOTH EMPTY when no schedule was given, so the
    /// default path pays one `is_empty` branch per symbol and step.
    pub(crate) funding_schedule: Vec<Option<Vec<FundingRatePoint>>>,
    pub(crate) funding_next: Vec<usize>,
    /// venue key used with `properties`/`funding_source` (mirrors `StrategyEngineConfig::default_venue`)
    pub(crate) venue: Option<String>,
    /// opt-in binary-resolution settlement source (see [`StrategyEngineConfig::resolution`]); `None`
    /// (default) = every path byte-identical
    pub(crate) resolution: Option<ResolutionSource>,
    /// as-of ts for the end-of-run resolution sweep (see [`StrategyEngineConfig::resolution_end_timestamp`])
    pub(crate) resolution_end_timestamp: Option<i64>,
    /// opt-in option-expiry settlement source (see [`StrategyEngineConfig::option_specs`]); `None`
    /// (default) = every path byte-identical
    pub(crate) option_specs: Option<OptionExpirySource>,
    /// as-of ts for the end-of-run option-expiry sweep (see [`StrategyEngineConfig::option_expiry_end_timestamp`])
    pub(crate) option_expiry_end_timestamp: Option<i64>,
    /// Per-symbol "this option's spec names no underlying, and that was already reported" latch,
    /// indexed like `symbols`. EMPTY unless an `option_specs` source is configured. It exists only
    /// so the warning `OrderBrokerSimulator::option_payout` raises for an unsettleable spec fires ONCE per
    /// symbol rather than on every step (`crates/vike-simulator/src/engine/settlement.rs`'s module doc).
    pub(crate) option_no_underlying_reported: Vec<bool>,
    /// Per-symbol "this market has resolved / this option has expired" latch, indexed like
    /// `symbols`. EMPTY unless a `resolution` OR `option_specs` source is configured — so the
    /// default path pays a single `is_empty` branch at order intake and never allocates or indexes.
    /// Once latched it never clears: the market is settled (or the option expired) and cannot trade
    /// again (see [`Self::is_resolved`]). Shared by both terminal-settlement lanes.
    pub(crate) resolved: Vec<bool>,
    /// settlement fills applied by the `resolution` source, insertion order — the distinct
    /// settlement record (see [`SettlementFill`]); empty unless `resolution` fired
    pub settlements: Vec<SettlementFill>,
    /// opt-in variation-margin cadence in ms (see [`StrategyEngineConfig::settlement_period_ms`]); `None`
    /// (default) = the settlement hook is a single `is_none` branch and every path is byte-identical
    pub(crate) settlement_period_ms: Option<i64>,
    /// the settlement bucket (`ts.div_euclid(period)`) the run is currently inside; `None` until
    /// the first step anchors it. Always `None` when `settlement_period_ms` is `None`.
    pub(crate) settle_bucket: Option<i64>,
    /// variation-margin settlements applied, insertion order (see [`SettlementVariation`]);
    /// empty unless `settlement_period_ms` is `Some` and a position was open across a boundary
    pub variation_settlements: Vec<SettlementVariation>,
    /// opt-in stale-price wait bound (see [`StrategyEngineConfig::max_price_staleness_ms`]); `None`
    /// (default) = the freshness bookkeeping below never runs and every lane is byte-identical
    pub(crate) max_price_staleness_ms: Option<i64>,
    /// Per-symbol ts of the last FRESH PRINT, indexed like `symbols`. EMPTY unless
    /// `max_price_staleness_ms` is configured — so the default fill path pays exactly one
    /// `Option::is_none` branch and never allocates or indexes. `None` in-slot means "this symbol
    /// has not printed yet" (stale by definition — see [`vike_fills::staleness::is_stale`]).
    pub(crate) last_print_timestamp: Vec<Option<i64>>,
    /// How many times a market/market-close order was DEFERRED by the wait discipline (a
    /// diagnostic, so a run that never got a fresh print can say so instead of silently
    /// producing zero trades). Always `0` when `max_price_staleness_ms` is `None`.
    ///
    /// Mirrored onto [`vike_analytics::BacktestResult::stale_deferrals`] by both `run` and `run_ticks`,
    /// so callers that drop the engine (`harness::run_backtest`, the `backtesthub` bin) can read it
    /// too — the diagnostic is not engine-owner-only.
    pub stale_deferrals: u64,
    /// Per-symbol opt-in session calendar (see [`StrategyEngineConfig::session_gate`]), indexed like
    /// `symbols`. EMPTY (default) = the gate is off and no fill lane consults it — the fill path
    /// pays a single `Vec::get`-returns-`None` branch and never indexes (mirrors `last_print_timestamp`).
    /// A `None` in-slot means "no calendar pins this symbol" ⇒ fail-permissive always-open.
    pub(crate) session: Vec<Option<SessionCalendar>>,
    /// How many times the session gate refused a fill because the venue was closed — resting
    /// orders deferred plus tagged-lane and protective-stop passes skipped. A diagnostic twin of
    /// [`Self::stale_deferrals`], mirrored onto [`vike_analytics::BacktestResult::session_deferrals`] so a
    /// run with suspiciously few trades can distinguish "the venue was shut" from "the strategy
    /// never traded". Always `0` when `session_gate` is `false`.
    pub session_deferrals: u64,
    /// opt-in market-impact slippage model (see [`StrategyEngineConfig::impact`]); `None` (default) =
    /// the flat-slippage fill path, byte-identical
    pub(crate) impact: Option<Arc<dyn crate::impact::ImpactModel>>,
    /// rolling-window length for `impact` (see [`StrategyEngineConfig::impact_window`])
    pub(crate) impact_window: usize,
    /// The TICK lane's rolling market context for `impact`, indexed like `symbols` — the tick
    /// twin of the bar lane's [`crate::impact::window_stats`] read over `bars`.
    ///
    /// EMPTY on every other path, and sized by `StrategyEngine::run_ticks` ONLY when an `impact`
    /// model is configured — the same arm-for-the-replay-then-clear discipline as `latency` /
    /// `shadow_pos` / `books`. That is what makes the extension byte-identical by construction
    /// rather than by review: with no model the vector stays empty, [`Self::impact_context`]'s
    /// `Vec::get` returns `None`, and the bar-window read below it is the frozen line verbatim.
    ///
    /// It exists because the bar read cannot answer on a tick tape at all: `run_ticks` is
    /// routinely driven with an EMPTY `bars` series per symbol, so `window_stats` measured
    /// `None` and the model silently charged nothing — the tick lane looked wired and was inert.
    pub(crate) impact_ticks: Vec<crate::impact::ImpactModelTickWindow>,
    /// How many fills were priced with the adverse move ON ITS FLOOR — i.e. how many times
    /// [`Self::slippage_for`] returned a fraction that would have carried the price through zero,
    /// and [`vike_fills::broker_sim::adverse_fill_price`] saturated at
    /// [`vike_fills::broker_sim::MIN_ADVERSE_FACTOR`] instead of inverting it (the case that module's
    /// docs open with). `0` on every sane run, and `0` by construction whenever the flat
    /// `slippage` is under 1.0 and no [`StrategyEngineConfig::impact`] model is configured — the default.
    /// [`StrategyEngineConfig::slippage_amount`] counts here too when it would sell a positive quote at or
    /// below zero ([`Self::fixed_slippage`]), the same floor reached by a fixed amount.
    ///
    /// A diagnostic in the same spirit as [`Self::stale_deferrals`] / [`Self::session_deferrals`]
    /// and [`Self::dropped`]: a strategy reads it off its `ctx`, and a run whose cost model
    /// saturated must be able to SAY so rather than hand back a plausible-looking equity curve
    /// built on fills nobody could get. It is NOT a refusal and deliberately does not go into
    /// `dropped` — the fill happened, at the floored price — because `dropped` is the denial
    /// channel [`vike_analytics::zero_trade::aggregate_denials`] reads to explain why orders did NOT fill.
    ///
    /// Counted at [`Self::apply_fill`] ONLY: the twin `adverse_fill_price` call in
    /// `StrategyEngine`'s cash pre-check prices the same fill a second time and counting there too
    /// would double every entry. Not (yet) mirrored onto [`vike_analytics::BacktestResult`] the way the two
    /// deferral counters are, so `harness::run_backtest` and the `backtesthub` bin cannot read it —
    /// that mirror touches four more construction sites and is left as its own change.
    pub slippage_saturations: u64,
    /// Fills at which an impact model (a profile's `market_impact` slippage) was CONFIGURED and
    /// then charged NOTHING, because the market
    /// context it needs could not be measured.
    ///
    /// The failure this exists for is silent by construction: [`Self::slippage_for`] falls back to
    /// the flat `slippage` when [`Self::impact_context`] answers `None`, so an operator who
    /// configured an impact model gets a run priced without one — no error, no warning, and a
    /// number that looks exactly like a priced number.
    ///
    /// ⚠ It is not hypothetical, and the cause is usually the DATA rather than the config:
    /// `crate::impact::window_stats` folds through `measure_series`, which returns `None` on "a
    /// non-positive mean size" — so a bar series carrying `volume = 0` disables impact entirely, at
    /// every coefficient. Measured 2026-09-07 against a the CI box store whose hyperliquid bars are
    /// zero-volume in every series (and flat-OHLC besides): two runs differing only in
    /// the impact table were bit-identical, and raising the coefficients 5,000x moved nothing.
    ///
    /// A NON-ZERO count is not automatically a fault — the first fills of a run legitimately
    /// precede a measurable window. A count equal to the fill count is the loud case: impact was
    /// never priced at all.
    ///
    /// Counted at [`Self::apply_fill`] ONLY, for the reason [`Self::slippage_saturations`] gives:
    /// `StrategyEngine`'s cash pre-check prices the same fill a second time. UNLIKE that
    /// counter this one IS mirrored onto [`vike_analytics::BacktestResult::impact_unpriced`] — a diagnostic
    /// only tests can read would not have told the operator anything, which is the whole defect.
    pub impact_unpriced: u64,
    /// ORDER FILLS booked as MAKER, and its taker twin below — the REALISED maker/taker mix,
    /// counted where the fee is charged so the two can never disagree.
    ///
    /// ⚠ **It records the `is_maker` the FEE was computed under, and nothing else.** The dispatch
    /// sites set that from the order KIND rather than from crossing aggressiveness (the caveat
    /// [`Self::apply_fill`] carries in its own source), so the count inherits exactly that
    /// classification. That is the property worth having: a consumer comparing "what mix did this
    /// run realise" against "what schedule did it resolve" must read the mix the CHARGE used, not
    /// a second derivation of it.
    ///
    /// Incremented in [`Self::fold_fill`] and ONLY for an order fill — a settlement
    /// ([`Self::settle_at_payout`]) folds through the same function with `settlement = true`, no
    /// order behind it and a zero fee, so counting it would report a taker fill nobody placed.
    /// Mirrored onto [`vike_analytics::BacktestResult::maker_fills`] at end-of-run, like
    /// [`Self::impact_unpriced`] and for the same reason: a diagnostic only tests can read tells
    /// an operator nothing.
    pub maker_fills: u64,
    /// ORDER FILLS booked as TAKER — the other half of [`Self::maker_fills`], same rules, same
    /// increment site.
    pub taker_fills: u64,
    /// The running TOTAL of per-fill commission, the same `fee` [`Self::fold_fill`] debits from
    /// [`Self::cash`] — summed rather than recomputed, so the reported number is the charged one.
    /// Signed: a rebate-bearing schedule contributes negative terms. Order fills only, for the
    /// reason [`Self::maker_fills`] gives. Mirrored onto [`vike_analytics::BacktestResult::fees_paid`].
    pub fees_paid: f64,
    /// What each resting order a fill left partly filled has been charged so far, by its `qid`, with
    /// its symbol: what `vike_fills::broker_sim::commission` needs to charge a per-ORDER minimum
    /// once. The simulator's own table, so `OrderResting` (which the live core's book holds too)
    /// carries none of it. Empty unless an order fills in pieces (a market-to-limit's remainder, the
    /// queue model's partial fills). [`Self::note_order_fees`] keeps and drops the entries.
    pub(crate) order_fees: IndexMap<u64, (usize, OrderFees)>,
    /// [`StrategyEngineConfig::optimistic_quotes`]: a tagged plain limit placed while it is set fills by the
    /// maker law (`OrderBrokerSimulatorSymbolState::quotes`).
    pub(crate) optimistic_quotes: bool,
    /// The ONE source of `OrderResting::qid` ([`Self::next_queue_id`]): starts at 1, `0` is unstamped.
    pub(crate) queue_id_sequence: u64,
    /// ARMED opt-in order-latency gate ([`crate::latency`]). Always `None` at construction and
    /// while the BAR engine runs: `StrategyEngine::run_ticks` installs it for the duration of a
    /// tick replay and takes it back out at the end, which is what structurally guarantees the
    /// bar path and the vector kernel can never consult a latency model. While `Some`, the
    /// strategy-facing write verbs below queue their mutation instead of applying it.
    pub(crate) latency: Option<crate::latency::LatencyGate>,
    /// STRATEGY-VISIBLE shadow position, indexed like `symbols` — the response leg's other half.
    ///
    /// EMPTY unless the latency gate is armed (`run_ticks` sizes it at arm time from the true
    /// positions and clears it at disarm), so every other path costs one `is_empty` branch and the
    /// frozen behavior is byte-identical. While non-empty it is advanced ONLY by fills the
    /// response leg has actually DELIVERED, and it is what [`OrderBroker::position`] returns, a maker's
    /// `position("")` included.
    ///
    /// Why it exists: `apply_fill` folds a fill into `symbol[si].pos` the instant it matches, so a
    /// strategy that POLLS its inventory (rather than accumulating it in `on_fill`) used to see
    /// every fill with zero response latency — which made the whole response leg inert for
    /// the spread maker file's `SpreadMaker`, the flagship maker, whose inventory skew
    /// and Avellaneda–Stoikov reservation price are driven by exactly that poll.
    ///
    /// Deliberately NOT shadowed: `position_of`, `pending_of`, `equity_now`, `drawdown_now` and
    /// the engine's own internals, which stay EXCHANGE TRUTH — they are the accounting surface
    /// `BacktestResult` is built from, and a shadowed `on_stop` would disagree with it.
    pub(crate) shadow_pos: Vec<f64>,
    /// The replay's current L2 book per symbol, indexed like `symbols` — the state the
    /// [`FillModelKind::L2Book`] tier prices from and the [`OrderBroker::quote_vwap`] /
    /// [`OrderBroker::depth_within_price`] reads answer from.
    ///
    /// EMPTY except while `run_ticks` is replaying a tape that carries `Tick::Book` events (it
    /// sizes this at the start of the run and folds each book event into it under the
    /// [`vike_marketdata::L2Book::delta_decision`] law). Every other path — the bar engine, the vector
    /// kernel, a tick replay with no book series — leaves it empty, so those lanes pay one
    /// `Vec::get`-returns-`None` branch and are byte-identical.
    ///
    /// `Rc` rather than a bare `L2Book` for one reason, and it is not premature: delivering
    /// `Strategy::on_order_book(&mut broker, &book)` needs the book and the broker borrowed at
    /// once. An `Rc` clone is O(1) and lets the strategy hold a real book while ALSO calling the
    /// liquidity reads above on the same broker — the alternative (moving the book out for the
    /// duration of the callback) would make a strategy's own symbol look book-less exactly inside
    /// the handler that just told it the book changed. `Rc::make_mut` clones only when a delivery
    /// borrow is still alive, which it never is by the next event, so the fold stays O(1) too.
    pub(crate) books: Vec<Option<Rc<vike_marketdata::L2Book>>>,
    /// The release every order submitted inside [`Self::submit_released`] carries; `None` outside
    /// it, which is every other submit.
    pub(crate) releasing: Option<vike_model::OrderRelease>,
    /// The gate terms [`Self::gate_order`] deferred for the order being submitted inside a release,
    /// taken by [`Self::push_pending`] into its [`OrderHeldEntry`] entry. `None` otherwise.
    pub(crate) held_terms: Option<SizingGateTerms>,
    /// Whether [`StrategyEngineConfig::queue_model`] is set: the iceberg verb is refused under it
    /// ([`Self::submit_limit_iceberg`]).
    pub(crate) queue_model: bool,
    /// The order lists placed ([`Self::submit_group`]): their book and their held children. Empty
    /// for every run that places none.
    pub(crate) groups: super::groups::OrderLists,
    /// The combos placed ([`Self::submit_combo`]) and their legs' marks. Empty for every run that
    /// places none.
    pub(crate) combos: super::combos::Combos,
    /// The orders placed through `vike_model::OrderBroker` (decision 0269), `tag → (si, qid)`: each
    /// rests in the book of `symbols[si]`, the symbol the verb named (an order list's leg in its
    /// group), under its own `OrderResting::qid`, and the tag names it by that identity. Empty for
    /// every run that places none, which is what keeps every fill lane's tag stamp
    /// ([`Self::tag_of`]) one `is_empty`.
    pub(crate) order_tags: IndexMap<String, (usize, u64)>,
    /// The tags of `order_tags` a fill has named since they were filed, so an order's end says
    /// `Filled` rather than `Canceled` (decision 0269's wave 3: the simulator sends the end of an
    /// order placed through the seam as live does).
    pub(crate) seam_filled: Vec<String>,
    /// The tags of the emulated parents (`OrderBrokerSimulatorSymbolState::emulated`) placed through the seam, so their
    /// end reaches the strategy as any seam order's does. Empty for every run that places none.
    pub(crate) seam_parents: Vec<String>,
    /// The ends of orders placed through the seam not yet delivered to the strategy
    /// ([`Self::ended_seam_orders`]). Both empty for every run that places none.
    pub(crate) seam_ended: Vec<vike_model::OrderEvent>,
    /// The in-bar fill rules this run switched on (`StrategyEngineConfig::in_bar`) and what their verbs
    /// filed. Every rule off and nothing filed for every run that sets none.
    pub(crate) in_bar: super::in_bar::InBar,
    /// The strategy's inputs as of the current event (`StrategyEngineConfig::inputs`, decision 0271), what
    /// `OrderBroker::input` reads. Empty for every run whose strategy declares none.
    pub(crate) inputs: super::inputs::InputView,
    /// The freshness bound (ms) every leg's mark must meet when a combo fills, the paper book's
    /// `combo_mark_staleness_ms` (`vike_fills::staleness::is_stale`: stale when STRICTLY older).
    /// Default 0, every leg priced at the event's own ts; a tape whose legs print a few ms apart
    /// widens it to that spacing. There is no off state.
    pub combo_mark_staleness_ms: i64,
}

/// One variation-margin settlement point for one symbol — the audit record of a daily mark-to-market
/// sweep applied by the opt-in [`StrategyEngineConfig::settlement_period_ms`] cadence. Unlike a
/// [`SettlementFill`] this closes NOTHING: the position size is untouched, only its cost basis moves
/// to `mark` while `amount` crosses from unrealized into realized.
#[derive(Debug, Clone, PartialEq)]
pub struct SettlementVariation {
    pub symbol: String,
    /// settlement timestamp — the ts of the step that opened the new bucket
    pub timestamp: i64,
    /// the settlement price (that step's last seen price) the basis was reset to
    pub mark: f64,
    /// signed profit/loss realized at this settlement:
    /// `size * (mark - prior average_price) * multiplier`
    pub amount: f64,
    /// the symbol's cumulative settled profit AFTER this settlement
    /// ([`OrderBrokerSimulatorSymbolState::settled_profit`])
    pub settled_profit: f64,
}
