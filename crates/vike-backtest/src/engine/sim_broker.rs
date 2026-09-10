//! `SimBroker` — the backtest fill engine plus its per-symbol state and cost/params types. Split
//! out of engine.rs (behavior byte-identical; whole items moved verbatim). `engine.rs` re-exports
//! the public types, so `engine::{SimBroker, EngineParams, MirrorFill, MirrorFunding}` and the
//! crate-root `vike_backtest::*` paths are unchanged.

use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use vike_exec::{RiskContext, RiskGate, RiskLimits, TradingState};
use vike_model::{
    Bar, Broker, FeeSchedule, Fill, FillKind, HftBroker, OrderKind, OrderRequest, Position,
    SessionCalendar, SymbolProperties, Trade, TradeFold, WorkingOrder, py_sum,
};

use crate::broker_sim::{adverse_fill_price, fee as fee_fn};
use crate::fill_model::{BarFillModel, FillModel, L2BookFillModel, TickFillModel};
use crate::schedule::Schedule;
use crate::sizing::{PositionSizer, SizeContext};

/// Which fill model the engine uses (Bar = default, Tick = L1 spread-crossing, L2Book = walk the
/// resting book). Lives here (with `SimBroker`/`EngineParams`, its only users) so the module graph
/// stays acyclic — engine.rs re-exports it, keeping the `engine::FillModelKind` /
/// `vike_backtest::FillModelKind` paths stable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FillModelKind {
    #[default]
    Bar,
    Tick,
    /// [`crate::fill_model::L2BookFillModel`] — a taker is priced by CONSUMING the replayed L2
    /// book for its own size, not by reading one tick price. Requires a tape carrying
    /// `Tick::Book` events (it degrades to [`FillModelKind::Tick`] wherever no book exists), and
    /// is only ever reached by asking for it, so selecting `Bar`/`Tick` is byte-identical to the
    /// engine before this variant existed.
    ///
    /// Pair it with `slippage: 0.0`: the walk IS the slippage, and stacking a flat
    /// `adverse_fill_price` haircut on top would double-charge it.
    L2Book,
}

/// How densely the TICK lane records its equity curve ([`EngineParams::equity_sampling`]).
///
/// The curve is two `Vec`s (`equity_curve` + `equity_ts`) grown by 16 bytes per priced tick,
/// unconditionally — on a 100M-tick tape that is 1.6 GB of resident memory per run, paid by every
/// point of a parameter sweep even when nothing ever reads the curve. This knob is the opt-out.
///
/// [`Self::EveryTick`] is the DEFAULT and is byte-identical to the engine before this type
/// existed: a run that does not set it produces the identical curve — same length, same values,
/// same timestamps. The other two variants are a deliberate fidelity trade — see each.
///
/// TICK LANE ONLY (`StrategyEngine::run_ticks`). The bar lane records one sample per BAR, which is
/// bounded by the bar count and needs no thinning, so `run` ignores this field entirely.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EquitySampling {
    /// One sample per priced tick — the frozen default.
    #[default]
    EveryTick,
    /// Keep one sample per `n` priced ticks (ticks `0, n, 2n, …`), plus a closing sample at the
    /// run's LAST priced tick so `equity_curve.last()` still agrees with `final_equity` and the
    /// end-of-run settlement sweeps still patch the right row.
    ///
    /// `n <= 1` names the same sampling as [`Self::EveryTick`] and is treated as it.
    ///
    /// Fidelity: every curve-DERIVED statistic (max drawdown, sharpe, the return series) is then
    /// computed over the subsample and is an approximation — a drawdown that opens and closes
    /// between two kept samples is invisible. Nothing else moves: `final_equity`, `trades`,
    /// `n_trades`, `per_symbol_pnl` and every fill are unaffected, because none of them is derived
    /// from the curve.
    EveryN(usize),
    /// Record nothing — both curve vectors stay EMPTY. For a sweep that ranks on
    /// `final_equity`/`n_trades` only. Every curve-derived statistic degenerates (a metric over an
    /// empty series), so never combine this with a drawdown- or sharpe-ranked sweep.
    Off,
}

/// Default lookback (bars) for the opt-in impact model's rolling `sigma`/`avg_volume` window
/// ([`EngineParams::impact_window`]). One trading month of daily bars — long enough that a single
/// outlier bar cannot dominate the volatility estimate, short enough to track a regime change.
pub const DEFAULT_IMPACT_WINDOW: usize = 21;

/// All mutable per-symbol state — ONE per symbol.
pub struct SymbolState {
    pub pos: Position,
    pub pending: Vec<WorkingOrder>,
    pub realized: f64,
    /// active protective stop price (None == none)
    pub stop: Option<f64>,
    pub entry_fee: f64,
    pub entry_ts: i64,
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
    /// Resting TAGGED limit orders (the HFT maker lane — [`HftBroker`]), keyed by the strategy's
    /// stable `tag`. Kept SEPARATE from `pending` so a tag's identity survives the fill loop's
    /// take/rebuild of `pending`, and so the existing (parity-gated) untagged paths are byte-for-byte
    /// untouched: this map is empty for every non-HFT strategy. Filled or canceled tags are REMOVED,
    /// which reproduces the live tag→coid registry's OBSERVABLE semantics (a modify/cancel on a gone
    /// tag is a no-op; a re-submit under the same tag replaces). `IndexMap` (not `HashMap`) so the
    /// fill/iteration order is deterministic insertion order — reproducible backtests.
    pub tagged: IndexMap<String, WorkingOrder>,
    /// Cumulative variation-margin profit already settled into realized for the CURRENT position
    /// (the `SettledProfit` accumulator). Always `0.0` unless
    /// [`EngineParams::settlement_period_ms`] is `Some` — see [`SimBroker::settle_variation_margin`].
    /// Reset to `0.0` whenever the position goes flat is NOT performed here: the accumulator is a
    /// run-lifetime audit total per symbol, and the basis reset makes a per-position reset
    /// unnecessary (there is never unsettled profit left behind on a flat book).
    pub settled_profit: f64,
}

// Portfolio backtest outcome is the shared `BacktestResult` (see crate::result).

/// A fill forwarded to the ledger mirror (`_on_fill` twin): the EXACT signed fee subtracted
/// from cash; per-symbol, no order arg (the mirror adapter mints ids itself).
#[derive(Debug, Clone)]
pub struct MirrorFill {
    pub symbol: String,
    pub side: i32,
    pub size: f64,
    pub price: f64,
    pub fee: f64,
    pub ts: i64,
    pub is_maker: bool,
    /// `true` only for a binary-resolution settlement close (the opt-in
    /// [`EngineParams::resolution`] lane) — a venue PAYOUT, not a trade. Always `false` for an
    /// order fill, so every pre-existing mirror consumer sees exactly what it always did. A
    /// settlement still reaches the mirror (the ledger must see the cash move and the position
    /// closing, or its book would never flatten), and this flag is how a consumer posts it as a
    /// redemption rather than as a zero-fee taker trade.
    pub is_settlement: bool,
}

/// A funding cashflow forwarded to the mirror (`_on_funding` twin): signed cash delta.
#[derive(Debug, Clone)]
pub struct MirrorFunding {
    pub symbol: String,
    pub amount: f64,
    pub ts: i64,
}

/// One binary-resolution settlement applied by the opt-in [`EngineParams::resolution`] source
/// (pm-economics lane) — the DISTINCT tag for a settlement fill. The `Fill` fed to `on_fill` is a
/// normal full-size close with `fee == 0.0` / `is_maker == false`; this record (collected in
/// [`SimBroker::settlements`], insertion order) is how reports and tests tell a resolution
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
    pub ts: i64,
}

/// The binary-resolution settlement source shape (see [`EngineParams::resolution`]):
/// `(symbol, ts_ms) -> Some(payout)` once the market is resolved, `None` while it trades.
/// `+ Send` (beyond the minimal closure shape) because callers move whole `EngineParams` onto
/// worker threads — the same rule as [`PositionSizer`]'s `Send` supertrait.
pub type ResolutionSource = Box<dyn Fn(&str, i64) -> Option<f64> + Send>;

/// Call or put — the option right, for the opt-in [`EngineParams::option_specs`] expiry-settlement
/// lane. A vike-backtest-LOCAL type: a shared `OptionRight` would belong in `vike-model` (a
/// down-only crate every layer could reuse), but none exists there yet and minting one is out of
/// this crate's scope — see the field doc on [`EngineParams::option_specs`]. (`vike-options` has
/// its own `OptionKind`, but this crate takes no dependency on it for a single two-variant enum.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionRight {
    Call,
    Put,
}

/// The static contract facts an option needs to settle at expiry (the opt-in
/// [`EngineParams::option_specs`] lane): `strike`, `expiry_ts` (ms since epoch), the `right`
/// (call/put), and the `underlying` symbol whose engine mark supplies the settle price. The
/// per-contract `contract_size` is deliberately NOT carried here — it is the option symbol's own
/// engine multiplier ([`EngineParams::multipliers`]), applied by the shared settlement fold exactly
/// as it was on entry/marking, so the payoff `max(0, u − K)·contract_size·qty` needs no second
/// scaling knob (and cannot double-apply one). See [`SimBroker::option_payout`].
#[derive(Debug, Clone, PartialEq)]
pub struct OptionSpec {
    pub strike: f64,
    pub expiry_ts: i64,
    pub right: OptionRight,
    pub underlying: String,
}

/// The option-expiry settlement source shape (see [`EngineParams::option_specs`]):
/// `symbol -> Some(OptionSpec)` for a symbol that is an option, `None` for anything else (the
/// underlying, a non-option instrument). Unlike [`ResolutionSource`] it takes no timestamp — an
/// option's strike/expiry/right/underlying do not vary over a run. `+ Send` for the same reason as
/// [`ResolutionSource`]: callers move whole `EngineParams` onto worker threads.
pub type OptionExpirySource = Box<dyn Fn(&str) -> Option<OptionSpec> + Send>;

pub struct EngineParams {
    pub fee_rate: f64,
    pub cash: f64,
    pub slippage: f64,
    pub maker_fee: Option<f64>,
    pub taker_fee: Option<f64>,
    /// Opt-in venue fee-shape bridge (wires the previously-dead
    /// [`FeeSchedule::maker_taker_rates`] into the engine — fee model follow-up 3). `None`
    /// (default) leaves the `maker_fee`/`taker_fee`/`fee_rate` precedence above byte-identical.
    /// `Some` OVERRIDES that whole chain: [`StrategyEngine::new`] derives the flat
    /// `(maker, taker)` FRACTIONS from the schedule via `maker_taker_rates()` and uses them
    /// exactly where `maker_fee.unwrap_or(fee_rate)` / `taker_fee.unwrap_or(fee_rate)` flowed —
    /// no per-fill branch, so the hot fold is unchanged either way.
    ///
    /// **Exception, and the one shape that does NOT flatten:**
    /// [`FeeSchedule::ProbabilityScaled`] has no flat equivalent at all
    /// (`maker_taker_rates()` reports `(0.0, 0.0)` for it), so flattening it would charge
    /// exactly ZERO while looking configured — the prediction-market fee misreport the
    /// `cheap_np` port backlog's G7 calls out. `Some(ProbabilityScaled { .. })` is therefore
    /// routed to the EXACT per-fill `FeeSchedule::commission` instead (see
    /// [`SimBroker::fee_curve`]): `qty × rate × p(1−p) × multiplier`, evaluated at the price the
    /// fill actually transacted at. `POLYMARKET_PROB_CURVE`'s all-zero rates still charge zero,
    /// so every existing consumer of that constant is numerically unchanged.
    ///
    /// The remaining non-percent shapes (`PerShareWithFloor`, `PercentOfUnderlying`'s
    /// premium cap) still flatten through `maker_taker_rates()` — a caller that needs their
    /// full floor/cap shape wants the paper engine's
    /// `PaperExecutionClient::with_fee_schedule` path instead, not this seam.
    pub fee_schedule: Option<FeeSchedule>,
    pub multiplier: f64,
    pub multipliers: Vec<(String, f64)>,
    pub leverage: Option<f64>,
    /// Outcome SHAPE when an opening/adding market order exceeds the margin budget (only read
    /// while a margin gate is armed — `leverage` or [`EngineParams::risk_limits`]; see
    /// [`SimBroker::gate_order`]).
    ///
    /// **`false` (default) = DENY like live, through the LIVE GATE ITSELF**: since deny-vs-clamp
    /// PHASE 2 the order crosses the literal `vike_exec::RiskGate::check` — one formula, one code
    /// path — and a refusal is recorded WHOLE (zero fill) in [`SimBroker::dropped`] under the
    /// gate's OWN reason string (`"insufficient-margin"` etc.; the phase-1
    /// `"insufficient-leverage-room"` string is gone — a backtest denial now IS the live denial,
    /// same reason). `true` = the pre-phase-1 escape hatch, surviving phase 2 verbatim:
    /// [`SimBroker::cap_to_leverage`] silently TRUNCATES the order to fit `leverage * equity`
    /// and executes the shrunken size — the historical behavior, byte-identical; the `RiskGate`
    /// is NOT consulted on this path (the escape hatch restores the whole pre-phase-2 admission
    /// pipeline, so [`EngineParams::risk_limits`] is inert while this knob is on).
    pub clamp_to_leverage: bool,
    /// Deny-vs-clamp PHASE 2 — the ONE-JUDGE config. `Some` mounts the literal live
    /// `vike_exec::RiskGate` on the market-order submit path ([`SimBroker::gate_order`])
    /// with exactly these limits, so EVERY live knob (min floors, notional caps, exposure cap,
    /// buying power with `required_free_bp_pct`/`closing_credit`, and any future check added to
    /// the gate) is backtest-effective automatically, with the live reason strings.
    ///
    /// `None` (default) + `leverage: Some(L)` still arms the gate, with the CONVENIENCE MAPPING
    /// `im_requirement = 1/L` (LEAN's own leverage↔initial-margin identity — see
    /// [`SimBroker::build_risk_gate`] for the equivalence proof and its limits). `None` + no
    /// `leverage` = no gate is ever constructed and the submit path is byte-identical to the
    /// pre-phase-2 engine — the big compat pin for non-margined backtests.
    ///
    /// Two fields are overridden at mount, deliberately (documented on
    /// [`SimBroker::build_risk_gate`]): the throttle is DISARMED (wall-clock, meaningless in sim
    /// time) and, when a [`EngineParams::properties`] source is configured, the four
    /// instrument-grid fields (`tick_size`/`lot_size`/`min_qty`/`min_notional`) are refreshed
    /// per check from the point-in-time grid — the venue's own time-varying facts, exactly what
    /// live's `RiskLimits::from_properties` mount does once at fetch time.
    pub risk_limits: Option<RiskLimits>,
    pub maint_margin: f64,
    /// **Opt-in stress knob** (default `false`): restore the pre-law venue-style liquidation —
    /// trigger `eq_adv ≤ maint_margin · notional_adv` at the intrabar ADVERSE marks and
    /// force-close the WHOLE account (bar path) / the triggering symbol in full (tick path),
    /// byte-identical to the retired default. `false` runs the shared scope-parameterized law
    /// (`vike_model::liquidation`): the LEAN cross-pool workflow — the same
    /// `pool_breached`/`cross_liquidation_plan` the live watchdog runs — meaning a margined
    /// backtest now gets PARTIAL losers-first liquidation (with the LEAN `liq_buffer` grace
    /// line) instead of a total wipe. Nautilus likewise ships its venue-style stress model
    /// off by default. Inert when `maint_margin` is `0.0` (margin off — the default).
    pub venue_style_liquidation: bool,
    /// The LEAN over-the-line buffer for the DEFAULT (shared-law) liquidation path: liquidate
    /// only when `maintenance > pool_equity · (1 + liq_buffer)` on top of the
    /// `pool_equity ≤ maintenance` law. Default `0.10` — the same
    /// `vike_exec::MarginCallConfig::default().buffer` the live watchdog runs, so a default
    /// backtest and the live sweep judge breaches by the SAME two-condition rule. Set `0.0`
    /// for a bufferless venue-style trigger line. Not read when `venue_style_liquidation`.
    pub liq_buffer: f64,
    pub cash_gate: bool,
    pub active_mask: Option<Vec<(String, Vec<bool>)>>,
    pub timeframes: Vec<String>,
    pub max_open_positions: usize,
    pub max_open_long: usize,
    pub max_open_short: usize,
    pub sizer: Option<Box<dyn PositionSizer>>,
    pub volume_limit: Option<f64>,
    pub granular_by_symbol: Vec<(String, Vec<Bar>)>,
    pub default_venue: Option<String>,
    pub fill_model: FillModelKind,
    /// enable the ledger mirror collection (the `_on_fill`/`_on_funding` twins)
    pub mirror: bool,
    /// Optional point-in-time instrument-filter lookup `(venue, symbol, ts_ms) -> grid`. `None`
    /// (default) leaves the fill path byte-identical. A harness builds this over
    /// `HistStore::properties_as_of`; the engine takes a plain closure (no HistStore dep).
    #[allow(clippy::type_complexity)]
    pub properties: Option<Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync>>,
    /// Opt-in **perp funding-rate source** `(venue, symbol, ts_ms) -> Some(rate)` — the offline
    /// twin of live's unconditional `vike_exec::Account::apply_funding`. `None` (the default)
    /// leaves the funding path byte-identical: a held perp is charged funding ONLY on the steps
    /// whose [`Bar::funding`] is `Some`, and no production data path sets that field, so a
    /// perp-carry backtest charges ZERO funding (PnL optimistic).
    ///
    /// When `Some`, each bar step consults the source for a symbol whose `Bar.funding` is `None`
    /// (a recorded `Bar.funding` always WINS — it is the on-the-bar truth) and, if it returns
    /// `Some(rate)`, charges `funding_charge(pos_size, close, rate, mult)` for that step exactly as
    /// the `Bar.funding` path does — same fold order, same mirror emission. The SOURCE decides the
    /// CADENCE: it returns `Some(rate)` only at the venue's funding timestamps (e.g. every 8h) and
    /// `None` between them, mirroring how `Bar.funding` is `Some` only on a funding bar. `venue` is
    /// [`EngineParams::default_venue`] and `symbol` is the UNQUALIFIED registration key
    /// ([`SimBroker::symbols`]), matching the [`EngineParams::properties`] seam; with no
    /// `default_venue` set the source is never consulted (funding is venue-specific).
    ///
    /// Deliberately NOT consulted by the vectorized kernel ([`crate::VectorBacktestEngine`]),
    /// which already takes funding as a fully-materialized `T×S` matrix from its caller (no closure
    /// seam) — so this is an EVENT-ENGINE-only knob, exactly like `properties`/`resolution`/
    /// `impact`, and engine↔kernel reconciliation stays meaningful only with `funding_source: None`.
    #[allow(clippy::type_complexity)]
    pub funding_source: Option<Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>>,
    /// Opt-in queue-position model for resting LIMIT fills on the tick/book replay path
    /// (`run_ticks` only; the bar path never consults it). `None` (default) keeps the frozen
    /// "price touched = filled" crossing model byte-identical. See [`crate::queue_model`].
    pub queue_model: Option<crate::queue_model::QueueModelKind>,
    /// Seed depth for a new resting order when neither the replayed book nor the last L1 quote
    /// knows the resting qty at its price (`queue_model` only). `0.0` (default) seeds an empty
    /// queue — the order starts at the front, i.e. the old optimistic behavior for that order.
    pub queue_seed_depth: f64,
    /// Opt-in **minimum-hold** floor (ms) for the queued lane (`queue_model` only): a fill that would
    /// REDUCE the current position (the closing side of a maker round-trip) is DEFERRED until at least
    /// this long after the position was opened — so a maker cannot book a physically-impossible
    /// same-millisecond / sub-second round-trip. `0` (default) disables it, byte-identical. Calibrate
    /// from real market-maker behavior (Polymarket BTC-5m MM wallets flip at a ~2s p10 / ~11s median).
    pub queue_min_hold_ms: i64,
    /// Opt-in **market-impact slippage model** (see [`crate::impact`]). `None` (default) keeps
    /// the frozen flat-`slippage` cost byte-identical — the wire-in
    /// ([`SimBroker::slippage_for`]) hands back the `slippage` field with no arithmetic applied.
    /// When `Some`, each TAKER fill's size is priced against a rolling volatility /
    /// average-volume window, and the estimate is ADDED to `slippage`.
    ///
    /// EVERY LANE, NOT JUST THE BAR PATH — but each lane is charged only the terms its own price
    /// law has not already paid ([`crate::impact::ImpactTerms`], selected by
    /// [`SimBroker::impact_terms`]): the bar and L1-tick laws are size-INDIFFERENT and pay the
    /// whole model, while a fill priced by walking a replayed L2 book has already paid the
    /// temporary concession out of real displayed depth and is charged the permanent footprint
    /// alone. One model, one coefficient set; the lane picks the terms.
    ///
    /// MAKER fills (`OrderKind::Limit`, including the `tagged` HFT lane) are exempt ON THE BAR
    /// LANE ONLY ([`SimBroker::charges_impact`]) — there a limit fills at-or-better than its own
    /// price because the market came to it. On the two replay lanes a `Limit` fill is priced by a
    /// marketable branch (the L1 tier fills it AT THE ASK for any size; the L2 tier walks the
    /// ladder), so it demanded liquidity and is charged like any other taker. A maker strategy
    /// backtested on the tick or L2 lane therefore DOES pay this on the legs that crossed — which
    /// is the point: exempting them let a taker escape the model by being respelt as a marketable
    /// limit.
    ///
    /// Deliberately NOT consulted by the vectorized kernel ([`crate::VectorBacktestEngine`]),
    /// which has no per-fill market context — so engine↔kernel reconciliation is only meaningful
    /// with `impact: None` (its default, which is what the parity gate runs).
    pub impact: Option<Arc<dyn crate::impact::ImpactModel>>,
    /// Lookback for the rolling `sigma`/`avg_volume` the `impact` model is priced against; read
    /// only when `impact` is `Some`. Defaults to [`DEFAULT_IMPACT_WINDOW`].
    ///
    /// ⚠ It counts BARS on the bar lane and TRADE PRINTS on the tick lane
    /// ([`crate::impact::TickWindow`]) — the same number, spent in wildly different units. The
    /// default was chosen as one trading month of DAILY bars; on a liquid tape 21 prints is a
    /// fraction of a second, so a tick run should set this deliberately rather than inherit it.
    /// Lengthening it does NOT undo the unit mismatch, only the sampling noise: `sigma` and
    /// `avg_volume` stay per-PRINT however many prints they average over, so the coefficient pair
    /// is what rescales the level (`crate::impact::AlmgrenChriss::with_coefficients`, reachable
    /// from a profile as `engine.impact.gamma` / `engine.impact.eta`).
    pub impact_window: usize,
    /// Opt-in order-latency model for the tick/book replay path (`run_ticks` only; the bar
    /// engine and the vectorized kernel never consult it). `None` (default) keeps the frozen
    /// zero-latency behavior byte-identical — no in-flight queue is built at all.
    ///
    /// When `Some`, every strategy-issued order-path mutation (new order, cancel-all, tagged
    /// submit/modify/cancel) is held in flight and applied at `submit_ts + entry()`, and each
    /// fill is delivered to `Strategy::on_fill` at `exch_ts + response()`. A NEGATIVE entry
    /// latency (a recorded rejection) drops the action outright and records it in
    /// [`SimBroker::dropped`] under `"latency_reject"`. See [`crate::latency`].
    ///
    /// WHAT THE RESPONSE LEG DELAYS, PRECISELY — read this before trusting a latency-armed number:
    ///
    /// - DELAYED: `Strategy::on_fill`, and the strategy-visible shadow inventory that
    ///   [`Broker::position`] / [`HftBroker::position`] return (see [`SimBroker::shadow_pos`]).
    ///   These are the reads a maker actually steers on — `vike-mm`'s `SpreadMaker` polls
    ///   `HftBroker::position` on every requote for its skew and A-S reservation price.
    /// - NOT DELAYED (exchange truth, synchronous, by design): [`SimBroker::position_of`],
    ///   [`SimBroker::pending_of`], [`SimBroker::equity_now`] / [`Broker::equity`],
    ///   [`SimBroker::drawdown_now`], and the fills' effect on cash, the trade log and the equity
    ///   curve. They are the accounting surface `BacktestResult` is built from. A strategy that
    ///   sizes off `equity()` still reacts to a fill with ZERO response latency.
    /// - INVISIBLE while in flight: a submitted order is in neither `pending_of` nor the position
    ///   until it is delivered. Use [`SimBroker::in_flight_of`] to see it — the naive
    ///   `if pending_of(s).is_empty() { submit(..) }` guard otherwise re-fires every tick of the
    ///   entry-latency window. The pre-trade leverage cap DOES count in-flight market orders.
    pub latency_model: Option<crate::latency::LatencyModelKind>,
    /// Opt-in binary-resolution settlement source `(symbol, ts_ms) -> Some(payout)` once the
    /// market is resolved (pm-economics lane; payout `0.0` or `1.0` for a binary outcome token —
    /// any terminal value is honored). `None` (default) leaves every path byte-identical. When
    /// `Some`, each bar step / price tick checks it BEFORE the fill phase: a resolved symbol has
    /// its resting orders canceled (pending + tagged + protective stop — a resolved market can no
    /// longer trade) and any open position closed at the payout price by a fee-free,
    /// slippage-free, grid-free settlement fill, recorded distinctly in
    /// [`SimBroker::settlements`] and delivered to the strategy via `on_fill`. `symbol` is the
    /// UNQUALIFIED registration key (`SimBroker::symbols`), not the venue-tagged `bar.symbol`.
    ///
    /// Resolution is TERMINAL and latched per symbol: from the settling step onward the engine
    /// REFUSES every further order for that symbol on both lanes (`pending` and the tagged maker
    /// lane — see [`SimBroker::is_resolved`]), which is what keeps a strategy that re-enters from
    /// inside `on_fill` from re-opening a market that just settled in this same step.
    ///
    /// Interaction with `mirror`: settlement fills DO reach `mirror_fills`, flagged
    /// [`MirrorFill::is_settlement`] (the ledger must see the payout or its book never flattens).
    /// The vectorized kernel has no resolution concept, so engine↔kernel mirror reconciliation is
    /// only meaningful with `resolution: None`.
    pub resolution: Option<ResolutionSource>,
    /// As-of timestamp for the END-OF-RUN resolution sweep (only read when `resolution` is
    /// `Some`). Real prediction-market tapes stop at the trading halt and the resolution posts
    /// LATER, so every recorded event has `ts < res_ts` and the in-loop checks never see the
    /// market resolve — without a final sweep a held-to-expiry position would be marked at the
    /// last traded price instead of settled at the payout.
    ///
    /// After the event loop both `run` and `run_ticks` therefore query the source ONE more time
    /// per symbol at this timestamp, defaulting to `i64::MAX` — "did this market EVER resolve?".
    /// The settlement is stamped at that timestamp when set, else at the symbol's LAST event ts
    /// (never `i64::MAX`, which would poison trade timestamps). Set it explicitly to:
    /// - pin a windowed backtest that must NOT settle past its window (use the window end), or
    /// - suppress end-of-run settlement entirely (use any ts before the resolution), or
    /// - keep a source that does arithmetic on `ts` from overflowing on `i64::MAX`.
    pub resolution_end_ts: Option<i64>,
    /// Opt-in **option-expiry cash settlement** source `symbol -> Some(OptionSpec)` (the options
    /// lane — the analog of the binary [`EngineParams::resolution`] lane, and it settles through the
    /// SAME machinery). `None` (default) leaves every path byte-identical. When `Some`, at/after an
    /// option's `expiry_ts` any held position is closed to CASH at its intrinsic value — CALL
    /// `max(0, underlying − strike)`, PUT `max(0, strike − underlying)`, per contract — by the same
    /// fee-free, slippage-free, grid-free settlement fill the resolution lane uses
    /// ([`SimBroker::settle_at_payout`]), recorded in [`SimBroker::settlements`] and delivered to the
    /// strategy via `on_fill`. `symbol` is the UNQUALIFIED registration key ([`SimBroker::symbols`]).
    ///
    /// The underlying settle price is the engine's own mark for the option's `underlying` symbol
    /// (`SimBroker::price_of` — the same last-seen-price field `equity_now` marks against), so the
    /// underlying must be a REGISTERED symbol; an option whose underlying is not registered is not
    /// settled (its position is left held). The check runs BEFORE each step's fill phase, so the
    /// underlying mark it reads is the previous step's close (a no-look-ahead lag; hold the
    /// underlying flat near expiry for an exact intrinsic). `contract_size` is the option symbol's
    /// own multiplier ([`EngineParams::multipliers`]) — see [`OptionSpec`].
    ///
    /// Expiry is TERMINAL, latched per symbol through the SHARED [`SimBroker::is_resolved`] latch
    /// (an expired option is "resolved"): from the settling step onward the engine refuses every
    /// further order for that option on both lanes, so a strategy that re-enters from inside
    /// `on_fill` cannot re-open an expired contract. The vectorized kernel has no expiry concept, so
    /// engine↔kernel reconciliation is only meaningful with `option_specs: None`.
    ///
    /// (`OptionSpec`/`OptionRight` are vike-backtest-LOCAL types. A shared `OptionSpec` belongs in
    /// `vike-model` so live/exec could reuse it — reported as a follow-up, out of this crate's
    /// scope; nothing here is blocked by defining it locally for now.)
    pub option_specs: Option<OptionExpirySource>,
    /// As-of timestamp for the END-OF-RUN option-expiry sweep (only read when `option_specs` is
    /// `Some`). A tape may stop before an option's `expiry_ts`; the in-loop checks only ever probe
    /// at event timestamps, so without a final sweep a position whose expiry falls just past the
    /// last event would never settle. After the event loop both `run` and `run_ticks` therefore
    /// settle any still-open option whose `expiry_ts` is at/before this as-of, defaulting to each
    /// symbol's LAST event ts. Set it to a FUTURE ts to force-settle an option expiring after the
    /// tape (hold-to-expiry over a short window — the [`EngineParams::resolution_end_ts`] analog);
    /// leave it `None` (default) and the sweep only re-touches options that already expired within
    /// the tape — an idempotent no-op that never force-settles a still-live option.
    pub option_expiry_end_ts: Option<i64>,
    /// Opt-in **variation-margin settlement** cadence in milliseconds (`86_400_000` = daily; the
    /// LEAN `FutureSettlementModel` analog). `None` (the default) — and any non-positive value —
    /// leaves every path byte-identical: the settlement hook returns before touching any state.
    ///
    /// When `Some(period)`, the engine buckets each step's timestamp (`ts.div_euclid(period)`) and,
    /// on the FIRST step of a new bucket, settles every open position: the daily profit/loss
    /// `size * (mark - avg_price) * multiplier` (mark = that step's last seen price) is booked into
    /// `SymbolState::realized` and accumulated into [`SymbolState::settled_profit`], and the
    /// position's cost basis is reset to the mark — unrealized becomes realized WITHOUT closing the
    /// position. Each settlement is recorded in [`SimBroker::variation_settlements`].
    ///
    /// **Equity is invariant across a settlement, by construction.** This engine already carries
    /// full signed notional through `cash` (`apply_fill` does `cash -= delta * price * mult`), so
    /// `equity_now() = cash + Σ size·price·mult` is continuously marked to market and open PnL is
    /// ALREADY spendable equity — unlike LEAN, whose futures holdings value 0 and where the
    /// settlement is therefore the only thing that moves money. In vike's model the settlement is
    /// the realized/unrealized SPLIT moving, which is exactly what a margin-accurate multi-day
    /// backtest needs: `realized` (and the per-symbol equity curve's realized component) tracks the
    /// cash the venue would have swept daily, while total PnL, the equity curve and every parity
    /// gate are unchanged.
    ///
    /// The first step of a run only ANCHORS the bucket (no settlement) — there is no prior mark to
    /// settle against. Settlement runs AFTER the funding charge for that step, so a funded perp's
    /// fee/funding fold order is preserved.
    ///
    /// Caveats: the basis reset means a later closing [`crate::Trade`] records the last
    /// settlement mark as its `entry_price` (its `pnl` is the final leg only — the SUM over trades
    /// plus the settled amounts is the total), and MAE/MFE stay measured against the ORIGINAL
    /// entry excursion window. The vectorized kernel has no settlement concept, so
    /// engine↔kernel reconciliation is only meaningful with `settlement_period_ms: None`.
    pub settlement_period_ms: Option<i64>,
    /// Opt-in stale-price wait discipline (LEAN `FutureFillModel` analog — see
    /// [`crate::staleness`] for the full contract). `None` (default) leaves EVERY fill lane
    /// byte-identical to the pre-feature engine.
    ///
    /// When `Some(bound_ms)`, a `Market` / `MarketClose` order whose symbol has not produced a
    /// fresh print within `bound_ms` is DEFERRED — left resting in `pending` — instead of filling
    /// against fill-forwarded data; it fills on the next fresh print. A "fresh print" is an event
    /// with `volume > 0.0` or a two-sided quote; price-conditional kinds (limit/stop/trailing and
    /// the protective-stop lane) are never gated.
    ///
    /// This bites the BAR tier, where flat zero-volume forward-filled bars are real. The tick
    /// tier is inert by construction (each fill event is the symbol's own just-arrived tick, so
    /// its age is always `0`). `Some(0)` is the tightest setting: fill only on an event that is
    /// itself a print.
    pub max_price_staleness_ms: Option<i64>,
    /// Opt-in **emulator-mirroring stop release** (trigger-law wave 2, law-map A2). `false`
    /// (default) keeps the historical semantics byte-identical: a resting [`OrderKind::Stop`]
    /// fills SAME-EVENT at the trigger oracle's price. `true` mirrors the live law — live,
    /// `submit_stop` always arms the core's `ConditionalBook` emulator, whose fired conditional
    /// is released as a plain MARKET (`strategy_drive::submit_fired`) and therefore fills at
    /// the NEXT event's price — so on trigger the resting stop CONVERTS to a resting market
    /// child ([`SimBroker::stop_released`]) and fills the next bar's open / next tick's print,
    /// on every fill lane. Protective stops ([`WorkingOrder::stop`] → `SymbolState::stop`) are
    /// NOT affected: their live twin is the venue-side/paper resting stop, which fills
    /// same-event on both stacks. The default divergence is pinned in `tests/laws/trigger_law.rs`.
    pub emulator_release_stops: bool,
    /// Opt-in **session / market-hours gate** (law-map T10 — the ONE session law, served from
    /// [`vike_model::session`]). `false` (the default) leaves every fill lane byte-identical: no
    /// per-symbol calendar is built and no fill path consults it (see [`SimBroker::session_closed`]
    /// — an empty session vec is one `Vec::get` returning `None`).
    ///
    /// When `true`, each SYMBOL resolves to its OWN [`vike_model::SessionCalendar`], and nothing on
    /// that symbol fills on an event whose ts falls outside its session — resting orders, the
    /// tagged maker lane, and protective stops alike, on the frozen fill lanes AND their opt-in
    /// [`EngineParams::queue_model`] tick twins (a closed market crosses no resting limit and no
    /// maker quote, queued or not) (see [`SimBroker::session_closed`] for why stops are gated here
    /// but not by the staleness lane). Per-symbol resolution is the fix for the
    /// venue-alone defect: a run mixing an alpaca equity and 24/7 alpaca crypto gates each
    /// correctly, which one `default_venue` per run cannot.
    ///
    /// Each symbol's calendar is chosen in this order:
    /// 1. [`EngineParams::session_calendars`]`[symbol]` — the per-symbol override, if present;
    /// 2. else [`vike_model::session_for`]`(default_venue)` — the venue-ONLY default (correct for a
    ///    single-session venue; fail-permissive always-open for a mixed venue like alpaca/ibkr);
    /// 3. else fail-permissive always-open (gate on, but nothing pins this symbol's session).
    ///
    /// So the (venue, asset-class) keying lives in the CALLER: build `session_calendars` from
    /// `vike_catalog::session_calendar_for(venue, asset_class)` per symbol. Enabling the knob with
    /// neither a `default_venue` nor any override is a silent no-op, not an error — the run's
    /// [`SimBroker::session_deferrals`] staying `0` is how a caller sees it.
    pub session_gate: bool,
    /// Per-symbol session-calendar overrides for [`EngineParams::session_gate`]. Empty (the
    /// default) = every symbol falls back to the `default_venue` calendar. This is the **per-symbol
    /// override hook** and the site where (venue, asset-class) keying reaches the engine: resolve
    /// each symbol's calendar with `vike_catalog::session_calendar_for(venue, asset_class)` (or pin
    /// a bespoke one) and insert it under the symbol name. Ignored entirely when `session_gate` is
    /// `false`.
    pub session_calendars: IndexMap<String, SessionCalendar>,
    /// How densely the TICK lane records its equity curve — see [`EquitySampling`]. The default
    /// [`EquitySampling::EveryTick`] is the frozen behaviour, byte-identical. Set it only to trade
    /// curve fidelity for the ~16 bytes/tick the two curve vectors cost on a very long tape.
    pub equity_sampling: EquitySampling,
}

impl Default for EngineParams {
    fn default() -> Self {
        EngineParams {
            fee_rate: 0.0,
            cash: 10_000.0,
            slippage: 0.0,
            maker_fee: None,
            taker_fee: None,
            fee_schedule: None,
            multiplier: 1.0,
            multipliers: Vec::new(),
            leverage: None,
            clamp_to_leverage: false,
            risk_limits: None,
            maint_margin: 0.0,
            venue_style_liquidation: false,
            liq_buffer: 0.10,
            cash_gate: false,
            active_mask: None,
            timeframes: Vec::new(),
            max_open_positions: 0,
            max_open_long: 0,
            max_open_short: 0,
            sizer: None,
            volume_limit: None,
            granular_by_symbol: Vec::new(),
            default_venue: None,
            fill_model: FillModelKind::Bar,
            mirror: false,
            properties: None,
            funding_source: None,
            queue_model: None,
            queue_seed_depth: 0.0,
            queue_min_hold_ms: 0,
            impact: None,
            impact_window: DEFAULT_IMPACT_WINDOW,
            latency_model: None,
            resolution: None,
            resolution_end_ts: None,
            option_specs: None,
            option_expiry_end_ts: None,
            settlement_period_ms: None,
            max_price_staleness_ms: None,
            emulator_release_stops: false,
            session_gate: false,
            session_calendars: IndexMap::new(),
            equity_sampling: EquitySampling::EveryTick,
        }
    }
}

/// Engine state + strategy-facing verbs/reads — the ctx every Strategy handler receives.
pub struct SimBroker {
    pub symbols: Vec<String>,
    pub bars: Vec<Rc<Vec<Bar>>>, // indexed like symbols; Rc so the dispatch loop can hand the
    // strategy a &Bar via a cheap refcount-bump handle instead of cloning (String alloc) each bar
    // — bars_for/forming_for keep reading the live series (unlike the mem::take that broke r4).
    pub n: usize,
    pub fee_rate: f64,
    pub cash: f64,
    /// Running NET funding cashflow (received-positive / paid-negative), the backtest twin of
    /// `vike_exec::Account.funding_paid`: the sum of every per-interval `MirrorFunding.amount`
    /// (`= -funding_charge`) accrued in the bar loop. Observability only — funding already moves
    /// `cash`; this breaks it out so a report can show funding P&L distinctly. Surfaced on
    /// [`crate::BacktestResult::funding_paid`].
    pub funding_paid: f64,
    pub slippage: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
    /// The EXACT per-fill fee schedule, set by [`StrategyEngine::new`] ONLY for a
    /// [`EngineParams::fee_schedule`] shape that `FeeSchedule::maker_taker_rates()` cannot
    /// express — today exactly [`FeeSchedule::ProbabilityScaled`], whose `qty × rate × p(1−p)`
    /// curve has no flat equivalent and would otherwise silently charge nothing (port backlog
    /// G7). `None` for every other configuration, which keeps the frozen
    /// `fee(size, price, rate, mult)` fold byte-identical.
    pub(crate) fee_curve: Option<FeeSchedule>,
    pub multiplier: f64,
    pub(crate) mult: Vec<(String, f64)>,
    pub(crate) mult_by_si: Vec<f64>, // per-symbol resolved multiplier (index si) — precomputed, no per-call scan
    pub leverage: Option<f64>,
    /// leverage-breach outcome shape: `false` (default) = deny whole through the live gate,
    /// `true` = truncate (see [`EngineParams::clamp_to_leverage`])
    pub clamp_to_leverage: bool,
    /// THE ONE JUDGE (deny-vs-clamp phase 2): the literal live `vike_exec::RiskGate`, mounted by
    /// [`SimBroker::build_risk_gate`] — `Some` exactly when a pre-trade gate is armed
    /// ([`EngineParams::risk_limits`] given, or `leverage` mapped to `im_requirement = 1/L`)
    /// AND the clamp escape hatch is off. `None` = the submit path never builds a context and is
    /// byte-identical to the pre-phase-2 engine.
    pub(crate) risk_gate: Option<RiskGate>,
    pub maint_margin: f64,
    /// see [`EngineParams::venue_style_liquidation`]
    pub venue_style_liquidation: bool,
    /// see [`EngineParams::liq_buffer`]
    pub liq_buffer: f64,
    pub cash_gate: bool,
    pub(crate) active_mask: Option<Vec<(String, Vec<bool>)>>,
    pub max_open_positions: usize,
    pub max_open_long: usize,
    pub max_open_short: usize,
    pub(crate) sizer: Box<dyn PositionSizer>,
    pub volume_limit: Option<f64>,
    pub equity_peak: f64,
    pub step: usize,
    /// (symbol, kind-or-reason, size, weight) for gate-dropped fills — the strategy-observable
    /// refusal channel (a `Strategy` reads it off its `ctx`). Reasons in use: the order KIND for
    /// cash-gate drops, `"volume_cap"`, `"latency_reject"`, and — since deny-vs-clamp phase 2 —
    /// the live `RiskGate`'s OWN reason strings (`"insufficient-margin"`, `"below-min-qty"`, …)
    /// from the default deny-whole submit gate ([`Self::gate_order`]); a backtest denial
    /// carries the exact string the live path would publish in `OrderDenied`.
    pub dropped: Vec<(String, String, f64, f64)>,
    /// ⚠ **MEASUREMENT of the one KNOWN live-vs-backtest divergence that is still open.** Counts
    /// fills this engine executed that the LIVE `RiskGate` would have DENIED — nothing is refused
    /// and no fill decision changes.
    ///
    /// The condition is a below-min REVERSAL: the order opposes the position, EXCEEDS it (so it
    /// flips through flat, which `vike_model::is_covered_reduce` does not treat as a reduce), and
    /// falls below a venue floor. `apply_fill`'s opening/closing split is DIRECTION-ONLY, so it
    /// calls the whole thing closing and executes it entire; the live gate requires COVERAGE and
    /// refuses. `crates/vike-exec/src/risk.rs`'s "KNOWN RESIDUAL DIVERGENCE" comment is the other
    /// end of it.
    ///
    /// ⚠ **AT REAL VENUE FLOORS THIS IS UNREACHABLE ABOVE DUST, and that is the useful result** —
    /// the counter records it rather than discovers it.
    ///
    /// A flip must EXCEED the position, so the condition needs `|position| < flip < min_qty`: the
    /// position must ALREADY be below the floor. An opening order cannot create that state (the same
    /// floor refuses it), so it takes a prior COVERED REDUCE leaving a sub-floor remainder, or a
    /// venue RAISING its minimum over a position's life. Then the flip itself must also be below the
    /// floor. Against `crates/vike-mount/src/fallback.rs`'s real grids — binance `min_notional` 5.0,
    /// bybit 5.0, okx 1.0 — that means REVERSING A POSITION WORTH UNDER ~$5. Any strategy sizing
    /// above a few dollars per order can never reach it.
    ///
    /// So the divergence is not worth FIXING, which matters because the fix is not free: it changes
    /// fill decisions (every `[risk]`-armed backtest of a flipping strategy would need re-baselining)
    /// and the obvious repair does not even converge the two engines — live denies the WHOLE order,
    /// while splitting the flip and floor-gating its opening half makes the backtest FLATTEN. Three
    /// outcomes, so closing it is a policy choice rather than a patch.
    ///
    /// The counter stays because it is ~10 lines on a cold path and it turns the argument above into
    /// something a run can contradict. A non-zero reading on a real profile would mean the reasoning
    /// here is wrong — which is exactly what a claim like this should expose itself to.
    pub below_min_reversals: u64,
    pub trades: Vec<Trade>,
    pub intrabar_both_hit: u32,
    pub sym: Vec<SymbolState>, // indexed like symbols
    pub now: i64,
    pub fill_model: FillModelKind,
    /// opt-in emulator-mirroring stop release (see [`EngineParams::emulator_release_stops`]);
    /// `false` (default) = a triggered resting stop fills same-event, byte-identical
    pub emulator_release_stops: bool,
    /// current step index (Python: strategy.index)
    pub index: usize,
    pub schedule: Schedule,
    /// ledger mirror sinks (Some when mirror enabled)
    pub mirror_fills: Option<Vec<MirrorFill>>,
    pub mirror_funding: Option<Vec<MirrorFunding>>,
    /// point-in-time instrument-filter lookup, or `None` for an unconstrained fill path
    #[allow(clippy::type_complexity)]
    pub(crate) properties:
        Option<Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync>>,
    /// opt-in perp funding-rate source (see [`EngineParams::funding_source`]); `None` (default) =
    /// funding comes only from `Bar.funding`, byte-identical to the pre-seam engine
    #[allow(clippy::type_complexity)]
    pub(crate) funding_source: Option<Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>>,
    /// venue key used with `properties`/`funding_source` (mirrors `EngineParams::default_venue`)
    pub(crate) venue: Option<String>,
    /// opt-in binary-resolution settlement source (see [`EngineParams::resolution`]); `None`
    /// (default) = every path byte-identical
    pub(crate) resolution: Option<ResolutionSource>,
    /// as-of ts for the end-of-run resolution sweep (see [`EngineParams::resolution_end_ts`])
    pub(crate) resolution_end_ts: Option<i64>,
    /// opt-in option-expiry settlement source (see [`EngineParams::option_specs`]); `None`
    /// (default) = every path byte-identical
    pub(crate) option_specs: Option<OptionExpirySource>,
    /// as-of ts for the end-of-run option-expiry sweep (see [`EngineParams::option_expiry_end_ts`])
    pub(crate) option_expiry_end_ts: Option<i64>,
    /// Per-symbol "this market has resolved / this option has expired" latch, indexed like
    /// `symbols`. EMPTY unless a `resolution` OR `option_specs` source is configured — so the
    /// default path pays a single `is_empty` branch at order intake and never allocates or indexes.
    /// Once latched it never clears: the market is settled (or the option expired) and cannot trade
    /// again (see [`Self::is_resolved`]). Shared by both terminal-settlement lanes.
    pub(crate) resolved: Vec<bool>,
    /// settlement fills applied by the `resolution` source, insertion order — the distinct
    /// settlement record (see [`SettlementFill`]); empty unless `resolution` fired
    pub settlements: Vec<SettlementFill>,
    /// opt-in variation-margin cadence in ms (see [`EngineParams::settlement_period_ms`]); `None`
    /// (default) = the settlement hook is a single `is_none` branch and every path is byte-identical
    pub(crate) settlement_period_ms: Option<i64>,
    /// the settlement bucket (`ts.div_euclid(period)`) the run is currently inside; `None` until
    /// the first step anchors it. Always `None` when `settlement_period_ms` is `None`.
    pub(crate) settle_bucket: Option<i64>,
    /// variation-margin settlements applied, insertion order (see [`VariationSettlement`]);
    /// empty unless `settlement_period_ms` is `Some` and a position was open across a boundary
    pub variation_settlements: Vec<VariationSettlement>,
    /// opt-in stale-price wait bound (see [`EngineParams::max_price_staleness_ms`]); `None`
    /// (default) = the freshness bookkeeping below never runs and every lane is byte-identical
    pub(crate) max_price_staleness_ms: Option<i64>,
    /// Per-symbol ts of the last FRESH PRINT, indexed like `symbols`. EMPTY unless
    /// `max_price_staleness_ms` is configured — so the default fill path pays exactly one
    /// `Option::is_none` branch and never allocates or indexes. `None` in-slot means "this symbol
    /// has not printed yet" (stale by definition — see [`crate::staleness::is_stale`]).
    pub(crate) last_print_ts: Vec<Option<i64>>,
    /// How many times a market/market-close order was DEFERRED by the wait discipline (a
    /// diagnostic, so a run that never got a fresh print can say so instead of silently
    /// producing zero trades). Always `0` when `max_price_staleness_ms` is `None`.
    ///
    /// Mirrored onto [`crate::BacktestResult::stale_deferrals`] by both `run` and `run_ticks`,
    /// so callers that drop the engine (`harness::run_backtest`, the `backtest` bin) can read it
    /// too — the diagnostic is not engine-owner-only.
    pub stale_deferrals: u64,
    /// Per-symbol opt-in session calendar (see [`EngineParams::session_gate`]), indexed like
    /// `symbols`. EMPTY (default) = the gate is off and no fill lane consults it — the fill path
    /// pays a single `Vec::get`-returns-`None` branch and never indexes (mirrors `last_print_ts`).
    /// A `None` in-slot means "no calendar pins this symbol" ⇒ fail-permissive always-open.
    pub(crate) session: Vec<Option<SessionCalendar>>,
    /// How many times the session gate refused a fill because the venue was closed — resting
    /// orders deferred plus tagged-lane and protective-stop passes skipped. A diagnostic twin of
    /// [`Self::stale_deferrals`], mirrored onto [`crate::BacktestResult::session_deferrals`] so a
    /// run with suspiciously few trades can distinguish "the venue was shut" from "the strategy
    /// never traded". Always `0` when `session_gate` is `false`.
    pub session_deferrals: u64,
    /// opt-in market-impact slippage model (see [`EngineParams::impact`]); `None` (default) =
    /// the flat-slippage fill path, byte-identical
    pub(crate) impact: Option<Arc<dyn crate::impact::ImpactModel>>,
    /// rolling-window length for `impact` (see [`EngineParams::impact_window`])
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
    pub(crate) impact_ticks: Vec<crate::impact::TickWindow>,
    /// How many fills were priced with the adverse move ON ITS FLOOR — i.e. how many times
    /// [`Self::slippage_for`] returned a fraction that would have carried the price through zero,
    /// and [`crate::broker_sim::adverse_fill_price`] saturated at
    /// [`crate::broker_sim::MIN_ADVERSE_FACTOR`] instead of inverting it (the case that module's
    /// docs open with). `0` on every sane run, and `0` by construction whenever the flat
    /// `slippage` is under 1.0 and no [`EngineParams::impact`] model is configured — the default.
    ///
    /// A diagnostic in the same spirit as [`Self::stale_deferrals`] / [`Self::session_deferrals`]
    /// and [`Self::dropped`]: a strategy reads it off its `ctx`, and a run whose cost model
    /// saturated must be able to SAY so rather than hand back a plausible-looking equity curve
    /// built on fills nobody could get. It is NOT a refusal and deliberately does not go into
    /// `dropped` — the fill happened, at the floored price — because `dropped` is the denial
    /// channel [`crate::zero_trade::aggregate_denials`] reads to explain why orders did NOT fill.
    ///
    /// Counted at [`Self::apply_fill`] ONLY: the twin `adverse_fill_price` call in
    /// `StrategyEngine`'s cash pre-check prices the same fill a second time and counting there too
    /// would double every entry. Not (yet) mirrored onto [`crate::BacktestResult`] the way the two
    /// deferral counters are, so `harness::run_backtest` and the `backtest` bin cannot read it —
    /// that mirror touches four more construction sites and is left as its own change.
    pub slippage_saturations: u64,
    /// Fills at which `[engine.impact]` was CONFIGURED and then charged NOTHING, because the market
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
    /// `[engine.impact]` were bit-identical, and raising the coefficients 5,000x moved nothing.
    ///
    /// A NON-ZERO count is not automatically a fault — the first fills of a run legitimately
    /// precede a measurable window. A count equal to the fill count is the loud case: impact was
    /// never priced at all.
    ///
    /// Counted at [`Self::apply_fill`] ONLY, for the reason [`Self::slippage_saturations`] gives:
    /// `StrategyEngine`'s cash pre-check prices the same fill a second time. UNLIKE that
    /// counter this one IS mirrored onto [`crate::BacktestResult::impact_unpriced`] — a diagnostic
    /// only tests can read would not have told the operator anything, which is the whole defect.
    pub impact_unpriced: u64,
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
    /// response leg has actually DELIVERED, and it is what [`Broker::position`] and
    /// [`HftBroker::position`] return.
    ///
    /// Why it exists: `apply_fill` folds a fill into `sym[si].pos` the instant it matches, so a
    /// strategy that POLLS its inventory (rather than accumulating it in `on_fill`) used to see
    /// every fill with zero response latency — which made the whole response leg inert for
    /// `vike-mm`'s `SpreadMaker`, the crate's flagship `HftBroker` consumer, whose inventory skew
    /// and Avellaneda–Stoikov reservation price are driven by exactly that poll.
    ///
    /// Deliberately NOT shadowed: `position_of`, `pending_of`, `equity_now`, `drawdown_now` and
    /// the engine's own internals, which stay EXCHANGE TRUTH — they are the accounting surface
    /// `BacktestResult` is built from, and a shadowed `on_stop` would disagree with it.
    pub(crate) shadow_pos: Vec<f64>,
    /// The replay's current L2 book per symbol, indexed like `symbols` — the state the
    /// [`FillModelKind::L2Book`] tier prices from and the [`Broker::quote_vwap`] /
    /// [`Broker::depth_within_price`] reads answer from.
    ///
    /// EMPTY except while `run_ticks` is replaying a tape that carries `Tick::Book` events (it
    /// sizes this at the start of the run and folds each book event into it under the
    /// [`vike_model::L2Book::delta_decision`] law). Every other path — the bar engine, the vector
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
    pub(crate) books: Vec<Option<Rc<vike_model::L2Book>>>,
}

/// One variation-margin settlement point for one symbol — the audit record of a daily mark-to-market
/// sweep applied by the opt-in [`EngineParams::settlement_period_ms`] cadence. Unlike a
/// [`SettlementFill`] this closes NOTHING: the position size is untouched, only its cost basis moves
/// to `mark` while `amount` crosses from unrealized into realized.
#[derive(Debug, Clone, PartialEq)]
pub struct VariationSettlement {
    pub symbol: String,
    /// settlement timestamp — the ts of the step that opened the new bucket
    pub ts: i64,
    /// the settlement price (that step's last seen price) the basis was reset to
    pub mark: f64,
    /// signed profit/loss realized at this settlement:
    /// `size * (mark - prior avg_price) * multiplier`
    pub amount: f64,
    /// the symbol's cumulative settled profit AFTER this settlement
    /// ([`SymbolState::settled_profit`])
    pub settled_profit: f64,
}

impl SimBroker {
    fn idx(&self, symbol: &str) -> usize {
        self.symbols
            .iter()
            .position(|s| s == symbol)
            .unwrap_or_else(|| panic!("unknown symbol {symbol:?}"))
    }

    /// The PIT grid for `symbols[si]` at `ts`, or `None` when there is no filter source, no venue
    /// key, or no record — in which case the fill path stays unconstrained.
    fn grid_for(&self, si: usize, ts: i64) -> Option<SymbolProperties> {
        let (venue, f) = (self.venue.as_deref()?, self.properties.as_ref()?);
        f(venue, &self.symbols[si], ts)
    }

    /// The per-symbol VENUE HOLD table in NANOSECONDS the latency gate arms with, indexed like
    /// [`Self::symbols`] — each market's `SymbolProperties::taker_hold_ms` at `ts`, resolved ONCE
    /// through the same PIT [`EngineParams::properties`] seam the fill grid uses (so a replay reads
    /// the hold the venue declared for that market, not a global constant).
    ///
    /// `ts` is the RUN START, not each action's stamp: a venue hold is a standing property of the
    /// market, and re-resolving it per submitted action would put a PIT lookup on the order path
    /// and let the in-flight queue's ordering drift mid-run.
    ///
    /// Returns EMPTY — the byte-identical, frozen path — with no properties source, no venue key,
    /// or when no symbol declares a hold (which is every venue but Polymarket).
    pub(crate) fn taker_hold_table_ns(&self, ts: i64) -> Vec<i64> {
        if self.venue.is_none() || self.properties.is_none() {
            return Vec::new();
        }
        let table: Vec<i64> = (0..self.symbols.len())
            .map(|si| {
                self.grid_for(si, ts)
                    .map_or(0, |p| i64::from(p.taker_hold_ms).saturating_mul(1_000_000))
            })
            .collect();
        if table.iter().all(|&h| h == 0) { Vec::new() } else { table }
    }

    /// The perp funding rate for `symbols[si]` at `ts` from the optional
    /// [`EngineParams::funding_source`] seam, or `None` when there is no source, no venue key, or
    /// no funding event at this ts — in which case no funding beyond `Bar.funding` is charged.
    pub(crate) fn funding_rate_for(&self, si: usize, ts: i64) -> Option<f64> {
        let (venue, f) = (self.venue.as_deref()?, self.funding_source.as_ref()?);
        f(venue, &self.symbols[si], ts)
    }

    // --- membership / caps ---

    pub fn is_active(&self, symbol: &str) -> bool {
        self.is_active_idx(self.idx(symbol))
    }

    pub(crate) fn is_active_idx(&self, si: usize) -> bool {
        match &self.active_mask {
            None => true,
            Some(mask) => {
                let sym = &self.symbols[si];
                mask.iter().find(|(s, _)| s == sym).map(|(_, m)| m[self.step]).unwrap_or(true)
            }
        }
    }

    // --- binary-resolution latch (the opt-in `EngineParams::resolution` lane) ---

    /// Has `symbol` already resolved in this run (see [`EngineParams::resolution`])? Once true it
    /// stays true — the market is settled, so the engine REFUSES every further order for it on
    /// both lanes. Always `false` when no resolution source is configured. A strategy can read
    /// this before quoting to avoid pushing orders that would be dropped.
    ///
    /// Total: an UNKNOWN symbol answers `false` rather than panicking (unlike the write verbs'
    /// `idx()`) — see the body for why a predicate differs from a write verb here.
    pub fn is_resolved(&self, symbol: &str) -> bool {
        // TOTAL on purpose — an unknown symbol is `false`, not a panic. This is public read-only
        // getter surface a strategy calls to decide whether to quote, typically from inside
        // `on_fill` / `on_bar` where a panic aborts the whole backtest. The engine's other
        // `idx()` callers are WRITE verbs, where an unknown symbol is a genuine strategy bug
        // worth failing loudly on; a predicate has an honest answer for a symbol this run never
        // registered — it has not resolved here — so it returns that instead.
        match self.symbols.iter().position(|s| s == symbol) {
            Some(si) => self.is_resolved_idx(si),
            None => false,
        }
    }

    /// `resolved` is EMPTY unless a resolution source is configured, so the default (parity-gated)
    /// order-intake path costs exactly one `is_empty` branch and never indexes.
    #[inline]
    pub(crate) fn is_resolved_idx(&self, si: usize) -> bool {
        !self.resolved.is_empty() && self.resolved[si]
    }

    /// Latch `symbols[si]` as resolved (idempotent). Called by the engine's resolution sweep
    /// BEFORE it settles, so the settlement's own `on_fill` re-entry is already refused.
    pub(crate) fn mark_resolved(&mut self, si: usize) {
        if !self.resolved.is_empty() {
            self.resolved[si] = true;
        }
    }

    // --- option-expiry payout (the opt-in `EngineParams::option_specs` lane) ---

    /// The PER-CONTRACT intrinsic settlement value for `symbols[si]` when it is a configured option
    /// that has expired at/before `probe_ts`, else `None`. CALL → `max(0, u − strike)`,
    /// PUT → `max(0, strike − u)`, where `u` is the engine's own mark for the option's `underlying`
    /// symbol ([`Self::price_of`] — the same last-seen-price field `equity_now` marks against).
    ///
    /// Returns `None` — the option is NOT settled — when: there is no source; the symbol is not an
    /// option (the source returns `None`); the option has not yet expired (`probe_ts < expiry_ts`);
    /// or the `underlying` names no registered symbol (no mark to settle against). The
    /// per-contract value is intentionally NOT scaled by the contract size here — the shared
    /// [`Self::settle_at_payout`] fold applies the option symbol's multiplier (its contract size)
    /// to `qty × payout`, exactly as it did on the entry fill, so `max(0, u − K)·contract_size·qty`
    /// falls out with no second scaling and no risk of double-applying it.
    pub(crate) fn option_payout(&self, si: usize, probe_ts: i64) -> Option<f64> {
        let source = self.option_specs.as_ref()?;
        let spec = source(&self.symbols[si])?;
        if probe_ts < spec.expiry_ts {
            return None;
        }
        let under_si = self.symbols.iter().position(|s| *s == spec.underlying)?;
        let u = self.sym[under_si].price;
        Some(match spec.right {
            OptionRight::Call => (u - spec.strike).max(0.0),
            OptionRight::Put => (spec.strike - u).max(0.0),
        })
    }

    /// The ONE order-intake choke point for the untagged `pending` lane: push `o` onto
    /// `symbols[si]`'s book UNLESS that symbol has resolved, in which case the order is REFUSED.
    /// Resolution is terminal — the market cannot trade again — so a strategy that submits from
    /// inside `on_fill` (the classic re-hedge / re-enter pattern) can never re-open a position in
    /// a market that settled in this very step, which would otherwise fill against the resolution
    /// bar's stale price and be settled again on the next step. The tagged maker lane is gated the
    /// same way in [`HftBroker::submit_limit_tagged`].
    ///
    /// Refusals are deliberately NOT recorded in `dropped`: a maker re-quoting on every tick after
    /// resolution would grow that diagnostics vec without bound. [`Self::is_resolved`] is the read
    /// a strategy or test uses instead, and the settlement itself is in [`Self::settlements`].
    #[inline]
    fn push_pending(&mut self, si: usize, o: WorkingOrder) {
        if self.is_resolved_idx(si) {
            return;
        }
        // Opt-in latency gate (tick replay only; `None` everywhere else = the frozen path).
        if self.latency.is_some() {
            let desc = crate::latency::LatencyOrder::new(o.side, o.size, o.price);
            let (now, size, weight) = (self.now, o.size, o.weight);
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(now, &desc, crate::latency::InFlightAction::Push { si, order: o });
            if !sent {
                self.record_latency_reject(si, size, weight);
            }
            return;
        }
        self.sym[si].pending.push(o);
    }

    /// Record an order-path action the latency model REJECTED (a negative entry latency: the
    /// request never reached the matching engine) in the existing `dropped` diagnostics channel,
    /// under the kind `"latency_reject"` — the same convention `"volume_cap"` uses.
    ///
    /// Without this a calibrated series with a rejection burst silently swallows a large fraction
    /// of a strategy's orders and the run reports nothing, leaving "why did my maker stop quoting"
    /// undiagnosable. Bounded by the number of actions the strategy issues, unlike the
    /// post-resolution refusals `push_pending` deliberately does not record.
    fn record_latency_reject(&mut self, si: usize, size: f64, weight: f64) {
        let sym = self.symbols[si].clone();
        self.dropped.push((sym, "latency_reject".into(), size, weight));
    }

    /// Apply ONE delivered in-flight action — the drain's per-action dispatch. Each arm is the
    /// zero-latency body of the matching strategy verb, so the delayed path and the immediate
    /// path share their semantics verbatim (including the resolution refusal, re-checked HERE
    /// because a market can resolve while an action is still in flight).
    pub(crate) fn apply_in_flight(&mut self, action: crate::latency::InFlightAction) {
        use crate::latency::InFlightAction as A;
        match action {
            A::Push { si, order } => {
                if !self.is_resolved_idx(si) {
                    self.sym[si].pending.push(order);
                }
            }
            A::CancelAll { si } => self.sym[si].pending.clear(),
            A::SubmitTagged { tag, order } => {
                if !self.is_resolved_idx(Self::HFT_SI) {
                    self.sym[Self::HFT_SI].tagged.insert(tag, order);
                }
            }
            A::ModifyTagged { tag, new_qty, new_price } => {
                if let Some(o) = self.sym[Self::HFT_SI].tagged.get_mut(&tag) {
                    if let Some(q) = new_qty {
                        if q > o.size {
                            o.qid = 0; // amend UP forfeits queue priority (see modify_tagged)
                        }
                        o.size = q;
                    }
                    if let Some(p) = new_price {
                        o.price = Some(p);
                    }
                }
            }
            A::CancelTagged { tag } => {
                self.sym[Self::HFT_SI].tagged.shift_remove(&tag);
            }
        }
    }

    // The three open-position caps below count POSITIONS (`pos.size != 0.0`), never resting or
    // in-flight orders, and are consulted at FILL time (`dispatch_fill`), not at submit. The
    // opt-in latency gate is therefore inert for them by construction: an order held in flight is
    // no more a position than the resting `pending` order it would otherwise have been, and by the
    // time it can breach a cap it has been delivered and filled through the same check. This is
    // NOT the shape `cap_to_leverage` has — that one is a pre-trade check that sums PENDING
    // notional, so it does have to see the in-flight queue (see there).
    pub(crate) fn at_open_cap(&self) -> bool {
        let cap = self.max_open_positions;
        cap != 0 && self.sym.iter().filter(|st| st.pos.size != 0.0).count() >= cap
    }

    pub(crate) fn at_long_cap(&self) -> bool {
        let cap = self.max_open_long;
        cap != 0 && self.sym.iter().filter(|st| st.pos.size > 0.0).count() >= cap
    }

    pub(crate) fn at_short_cap(&self) -> bool {
        let cap = self.max_open_short;
        cap != 0 && self.sym.iter().filter(|st| st.pos.size < 0.0).count() >= cap
    }

    // --- ATR helper ---

    /// Mean true range over the last `n` bars up to (and including) the current step.
    fn atr(&self, si: usize, n: usize) -> f64 {
        let bars = &self.bars[si];
        if bars.is_empty() {
            return 0.0;
        }
        let end = self.step + 1;
        let start = 1.max(end.saturating_sub(n));
        if end < 2 {
            return 0.0;
        }
        let mut trs: Vec<f64> = Vec::new();
        for i in start..end {
            let bar = &bars[i];
            let prev_close = bars[i - 1].close;
            let tr = (bar.high - bar.low)
                .max((bar.high - prev_close).abs())
                .max((bar.low - prev_close).abs());
            trs.push(tr);
        }
        if trs.is_empty() { 0.0 } else { py_sum(trs.iter().copied()) / trs.len() as f64 }
    }

    // --- reads ---

    pub fn position_of(&self, symbol: &str) -> Position {
        self.sym[self.idx(symbol)].pos
    }

    pub fn price_of(&self, symbol: &str) -> f64 {
        self.sym[self.idx(symbol)].price
    }

    pub fn multiplier_of(&self, symbol: &str) -> f64 {
        self.mult.iter().find(|(s, _)| s == symbol).map(|(_, m)| *m).unwrap_or(self.multiplier)
    }

    pub(crate) fn multiplier_of_idx(&self, si: usize) -> f64 {
        self.mult_by_si[si] // O(1) index; byte-identical to multiplier_of(symbols[si])
    }

    /// cash + Σ pos·price·mult (Python builtin sum → py_sum, symbols order).
    pub fn equity_now(&self) -> f64 {
        self.cash
            + py_sum((0..self.symbols.len()).map(|si| {
                vike_model::signed_notional(
                    self.sym[si].pos.size,
                    self.sym[si].price,
                    self.multiplier_of_idx(si),
                )
            }))
    }

    pub fn drawdown_now(&self) -> f64 {
        let eq = self.equity_now();
        let peak = self.equity_peak;
        if peak <= 0.0 {
            return 0.0;
        }
        f64::max(0.0, (peak - eq) / peak)
    }

    /// Resting untagged orders the MATCHING ENGINE holds for `symbol`.
    ///
    /// HAZARD under the opt-in latency gate: an order the strategy has submitted but that is still
    /// in flight is in NEITHER this slice nor the position — it does not exist anywhere the
    /// strategy can see it except [`Self::in_flight_of`]. The common idiom
    /// `if ctx.pending_of(s).is_empty() { ctx.submit_limit(..) }` therefore re-fires on every tick
    /// of the entry-latency window and can rest N duplicate orders; write it as
    /// `if ctx.pending_of(s).is_empty() && ctx.in_flight_of(s) == 0 { .. }` instead. (The TAGGED
    /// maker lane is immune — an insert under an existing tag replaces it.)
    pub fn pending_of(&self, symbol: &str) -> &[WorkingOrder] {
        &self.sym[self.idx(symbol)].pending
    }

    /// The armed protective stop level for `symbol`, or `None` when nothing is armed. Read-only
    /// view of the implicit bracket `submit(.., stop)` arms — the level `check_stop` breaches
    /// against, and the OCO sibling a closing-side limit fill disarms.
    pub fn protective_stop_of(&self, symbol: &str) -> Option<f64> {
        self.sym[self.idx(symbol)].stop
    }

    /// How many NEW-ORDER actions for `symbol` are held in flight by the opt-in latency gate —
    /// submitted by the strategy, not yet visible to the matching engine. Always `0` when the
    /// gate is not armed, which is every path but a `run_ticks` replay with
    /// `EngineParams::latency_model = Some(..)`. Pair it with [`Self::pending_of`] (see the
    /// duplicate-submit hazard documented there).
    ///
    /// Total on an unknown symbol (`0`) rather than a panic, like [`Self::is_resolved`]: it is a
    /// read a strategy calls to decide whether to quote.
    pub fn in_flight_of(&self, symbol: &str) -> usize {
        let (Some(g), Some(si)) =
            (self.latency.as_ref(), self.symbols.iter().position(|s| s == symbol))
        else {
            return 0;
        };
        g.in_flight_orders_for(si)
    }

    /// Advance the strategy-visible shadow position by ONE fill the response leg has DELIVERED.
    /// No-op (and no symbol lookup) unless the gate is armed — `shadow_pos` is empty otherwise.
    /// `Fill::symbol` is always `symbols[si]` (see `apply_fill`), so the lookup always resolves;
    /// an unrecognized symbol is skipped rather than panicking inside a delivery loop.
    pub(crate) fn advance_shadow(&mut self, fill: &Fill) {
        if self.shadow_pos.is_empty() {
            return;
        }
        if let Some(si) = self.symbols.iter().position(|s| *s == fill.symbol) {
            self.shadow_pos[si] += fill.side as f64 * fill.size;
        }
    }

    // --- sizing ---

    fn is_opening(&self, si: usize, side_sign: i32) -> bool {
        let pos = &self.sym[si].pos;
        pos.size == 0.0 || (pos.size > 0.0) == (side_sign > 0)
    }

    /// Total $ risk currently open across the book (builtin sum → py_sum, symbols order).
    fn open_risk(&self) -> f64 {
        py_sum(
            (0..self.symbols.len())
                .filter(|&si| self.sym[si].stop.is_some() && self.sym[si].pos.size != 0.0)
                .map(|si| {
                    (self.sym[si].price - self.sym[si].stop.unwrap()).abs()
                        * self.sym[si].pos.size.abs()
                        * self.multiplier_of_idx(si)
                }),
        )
    }

    /// Run the sizer on opening/increasing entries; pass through when raw or reducing.
    fn size_entry(
        &self,
        si: usize,
        side_sign: i32,
        size: f64,
        raw: bool,
        stop: Option<f64>,
    ) -> f64 {
        if raw || !self.is_opening(si, side_sign) {
            return size;
        }
        let equity = self.equity_now();
        let peak = self.equity_peak;
        let drawdown = if peak > 0.0 { f64::max(0.0, 1.0 - equity / peak) } else { 0.0 };
        self.sizer.size(&SizeContext {
            symbol: self.symbols[si].clone(),
            side: side_sign,
            intent: size,
            basis_price: self.sym[si].price,
            equity,
            cash: self.cash,
            multiplier: self.multiplier_of_idx(si),
            atr: self.atr(si, 14),
            drawdown,
            risk_stop: stop,
            open_risk: self.open_risk(),
        })
    }

    /// Mount THE ONE JUDGE (deny-vs-clamp phase 2): build the pre-trade `RiskGate` a `SimBroker`
    /// admits market orders through, or `None` when no gate is armed. Called once at engine
    /// construction (`StrategyEngine::new`); one gate per `SimBroker` (per venue-sim), exactly
    /// as live runs one gate per venue/account session.
    ///
    /// * `clamp_to_leverage == true` ⇒ `None` — the escape hatch restores the WHOLE pre-phase-2
    ///   admission pipeline ([`Self::cap_to_leverage`] truncation, verbatim); the gate is never
    ///   consulted and `risk_limits` is inert (documented on the knob).
    /// * `risk_limits: Some` ⇒ those limits verbatim (every live knob, present and future).
    /// * else `leverage: Some(L)` ⇒ `RiskLimits { im_requirement: Some(1/L) }` — **the
    ///   leverage→initial-margin mapping.** Equivalence to the retired leverage-room formula:
    ///   with uniform `im = 1/L` and `margin_used` folded pending-aware
    ///   ([`Self::margin_in_use_pending_aware`]), the gate's
    ///   `order_margin ≤ equity − margin_used` is `qty·px·mult/L ≤ eq − (open+pending)/L`, i.e.
    ///   `qty·px·mult ≤ L·eq − open − pending` — algebraically the exact `cap_to_leverage`
    ///   fits-whole test, `<=` boundary included. Its LIMITS: (a) equality is algebraic, not
    ///   bit-for-bit (the gate divides where the old room test multiplied), so a knife-edge f64
    ///   boundary can in principle flip — the margin lane was never parity-frozen, so no golden
    ///   pins that; (b) the old direction-only "reducing never denied" arm is now the gate's
    ///   stricter covered-reduce bypass plus the LEAN `closing_credit` — an uncovered REVERSAL
    ///   faces buying power for its full size (credited `2·|pos|·mark·mult·im` for the leg it
    ///   closes), so an overshoot beyond the credit now DENIES where the old formula waved it
    ///   through. That is the gate's verdict winning on purpose (live is the judge), pinned in
    ///   `riskgate_simbroker_parity.rs`.
    /// * neither ⇒ `None` — the submit path never builds a gate context: byte-identical for
    ///   every non-margined backtest (the big compat pin).
    ///
    /// The mounted gate always has `max_orders_per_window` DISARMED: the throttle is a
    /// WALL-CLOCK order-rate limit and sim time is not wall time — a backtest replaying a month
    /// in a second would spuriously rate-limit everything, and sim-timing it would gate on the
    /// tape's timestamps, a semantics live never has. Every other check runs as live.
    pub(crate) fn build_risk_gate(p: &EngineParams) -> Option<RiskGate> {
        if p.clamp_to_leverage {
            return None;
        }
        let mut limits = match (&p.risk_limits, p.leverage) {
            (Some(l), _) => l.clone(),
            (None, Some(lev)) => {
                RiskLimits { im_requirement: Some(1.0 / lev), ..RiskLimits::new() }
            }
            (None, None) => return None,
        };
        limits.max_orders_per_window = None; // wall-clock throttle: meaningless in sim time
        Some(RiskGate::new(limits))
    }

    /// Σ committed initial margin the gate's buying-power lane judges against — the backtest's
    /// `RiskContext::margin_used`, mirroring live's `Account::margin_in_use_by` fold (each open
    /// position priced `|size|·mark·mult·im`, per-symbol `im_for` falling back to the ORDER
    /// symbol's rate; every SimBroker position is CROSS, so the live fold's isolated/cash pool
    /// partition (#497) is vacuously satisfied — the mode-partitioned law flows through the gate
    /// unchanged the day the sim grows margin modes) **plus the resting/in-flight market-order
    /// notional margin the retired `cap_to_leverage` counted**. Live's `Account` cannot see
    /// resting orders, but a pre-trade cap must model the LOCAL view — without this term every
    /// submit inside one entry-latency window would be granted the full budget
    /// (`latency_model.rs` pins it), so the pending fold is kept, valued at current marks with
    /// the same `py_sum` order as the formula it replaces.
    fn margin_in_use_pending_aware(&self, order_im: f64) -> f64 {
        let lim = &self.risk_gate.as_ref().expect("armed").limits;
        py_sum((0..self.symbols.len()).map(|i| {
            let im = lim.im_for(&self.symbols[i]).unwrap_or(order_im);
            let px_mult = self.sym[i].price * self.multiplier_of_idx(i);
            let mut signed = py_sum(
                self.sym[i]
                    .pending
                    .iter()
                    .filter(|o| o.kind == OrderKind::Market)
                    .map(|o| o.side as f64 * o.size),
            );
            if let Some(g) = self.latency.as_ref() {
                signed += g.in_flight_market_signed(i);
            }
            (self.sym[i].pos.size.abs() * px_mult + signed.abs() * px_mult) * im
        }))
    }

    /// The pre-trade judge at market-order intake — ONE dispatch point over the two outcome
    /// shapes (deny-vs-clamp phases 1+2):
    ///
    /// * DEFAULT (`clamp_to_leverage == false`): **the literal live gate.** The order crosses
    ///   `vike_exec::RiskGate::check` — the same code path `ExecutionEngine::gate_and_register`
    ///   runs live — over a [`RiskContext`] built from this engine's own state, mirroring the
    ///   live construction field for field: `position_size`/`mark_price` from the symbol state,
    ///   `equity` = [`Self::equity_now`] (the sim account is continuously marked to market, the
    ///   `equity_all` twin), `margin_used` = [`Self::margin_in_use_pending_aware`],
    ///   `closing_credit` = live's `2·|pos|·mark·mult·im` reversing credit, `multiplier` = the
    ///   symbol's real contract multiplier (unconditionally — live currently wires `1.0` when
    ///   the margin lane is unarmed, a known ctx-construction bug this side deliberately does
    ///   not reproduce; the gate CODE is shared, the ctx is each caller's to get right). A
    ///   refusal is whole (zero fill) and recorded in [`Self::dropped`] under the gate's OWN
    ///   reason string — a backtest denial IS the live denial. When a
    ///   [`EngineParams::properties`] source is configured, the gate's four instrument-grid
    ///   fields are refreshed from the point-in-time grid at `self.now` before the check, so
    ///   venue floors judge pre-trade exactly as live's `from_properties` limits do (on top of
    ///   `apply_fill`'s venue-side fill-time check, which stays — the venue re-checks at
    ///   execution, live and here).
    /// * OPT-IN (`clamp_to_leverage == true`): the historical TRUNCATE shape, verbatim —
    ///   [`Self::cap_to_leverage`] shrinks the order to the remaining room and the shrunken
    ///   size executes silently (including its quirks, e.g. the `equity <= 0` early-zero that
    ///   predates the reducing check). The phase-1 deny arm that lived here (the
    ///   `"insufficient-leverage-room"` room test) is GONE — `cap_to_leverage` survives only as
    ///   this clamp-knob path.
    ///
    /// A COVERED reducing order is never denied (anti-stranding — the gate's own
    /// `is_covered_reduce` bypass, the same rule that keeps `apply_fill`'s floors off closing
    /// fills), including on a bankrupt account. An exactly-fitting order passes whole
    /// (`has_sufficient_margin` is `<=`).
    ///
    /// The IMPACT knobs (`max_slippage_bps`/`require_fillable`) are NOT armed here: this site
    /// calls plain [`RiskGate::check`], never `check_with_book` — deliberately, because the
    /// live runtime ALSO has no `check_with_book` caller yet ("no such site exists today",
    /// `risk.rs` module doc: neither the engine nor the runtime retains a per-symbol book), and
    /// the replay books live in `run_ticks` locals, not on `SimBroker`. Arming the veto here
    /// FIRST would create a fresh backtest-vs-live divergence in the opposite direction; when a
    /// live site opts in, the tick/book lane grows the same call.
    /// ⚠ **Every order-submitting verb goes through here now, not just the market ones.**
    ///
    /// This was `gate_market_order`, called from `submit_market` and `submit_market_close` ALONE.
    /// `submit_limit`, `submit_stop`, `submit_trailing`, `submit_limit_close` and
    /// `HftBroker::submit_limit_tagged` faced NO pre-trade gate at all, so a backtest of a limit
    /// strategy ran with the operator's `[risk]` budget effectively switched off — no per-order
    /// notional cap, no projected-exposure cap, no buying-power / `max_leverage` check — while LIVE
    /// gates all of them. The backtest was the PERMISSIVE side, so a strategy could validate here
    /// and be refused in production. `vike-mm` quotes limits exclusively.
    ///
    /// ⚠ **`price`/`trigger` go on the REQUEST; `ctx.mark_price` stays the MARK.** That split is
    /// deliberate and is not a detail. `RiskGate::check_inner` already computes
    /// `ref_price = req.price.or(req.trigger_price).unwrap_or(ctx.mark_price)` for the notional
    /// lanes, so a limit order is judged on its own price for min/max-notional while the
    /// projected-exposure cap keeps valuing at the mark. Folding the order price into `mark_price`
    /// instead would judge EXPOSURE on a price the position is not worth — exposure is a valuation
    /// question, not an order-price one, and doing it that way would import the live gate's own
    /// `mark_price`-basis bug into the backtest rather than fixing anything.
    #[allow(clippy::too_many_arguments)]
    fn gate_order(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        weight: f64,
        order_type: &str,
        price: Option<f64>,
        trigger: Option<f64>,
    ) -> f64 {
        if self.clamp_to_leverage {
            return self.cap_to_leverage(si, side_sign, size);
        }
        if self.risk_gate.is_none() || size <= 0.0 {
            return size; // no gate armed / nothing to judge — byte-identical pass-through
        }
        // ---- RiskContext, mirroring `ExecutionEngine::gate_and_register` field for field ----
        let mark = self.sym[si].price;
        let mult = self.multiplier_of_idx(si);
        let im = {
            let lim = &self.risk_gate.as_ref().expect("checked Some").limits;
            lim.im_for(&self.symbols[si])
        };
        // The margin fields are computed ONLY when the buying-power lane is armed for this
        // symbol — the live construction's exact gating (they are read by no other check).
        let (equity, margin_used, closing_credit) = match im {
            Some(im_req) => {
                let pos = self.sym[si].pos.size;
                let credit = if pos != 0.0 && side_sign as f64 * pos < 0.0 {
                    // direction-reversing order: margin the close frees + the LEAN re-open
                    // credit (single-requirement model: mm == im in the gate) — live verbatim
                    2.0 * pos.abs() * mark * mult * im_req
                } else {
                    0.0
                };
                (self.equity_now(), self.margin_in_use_pending_aware(im_req), credit)
            }
            None => (0.0, 0.0, 0.0),
        };
        // The ACCOUNT-aggregate ceiling's other half (`RiskLimits::max_account_exposure`), folded
        // only when that lane is armed — the same gating the margin fields above use, and for the
        // same reason: it is O(symbols) and an unarmed backtest must pay nothing for it.
        //
        // ⚠ **It is folded HERE rather than left at `0.0`, and that is the whole standing rule of
        // this function**: the backtest must never be the PERMISSIVE side (see this fn's doc — the
        // gap it was written to close was exactly a limit strategy validating here against a budget
        // live enforces). A `0.0` would vacate the account ceiling for every backtest while live
        // refused, so a strategy could pass validation and be denied in production.
        //
        // Same shape as the live producer (`ExecutionEngine::resolved_account_exposure_excluding`):
        // GROSS (`|size|`, never netting), the ORDER's own symbol's POSITION excluded because the
        // gate re-adds it projected, everything valued at the current mark and the symbol's own
        // multiplier. The sim account is one venue's one account by construction, so there is no
        // foreign-venue row to skip here — the live fold's venue skip has no counterpart. `py_sum`
        // in `symbols` order, matching `margin_in_use_pending_aware` and `equity_now` beside it.
        //
        // ⚠ **RESTING ORDERS COUNT, on EVERY symbol including the order's own** — the live
        // producer's second half, and the reason that one exists: without it N orders submitted
        // inside one fill window are each judged as though the others committed nothing, and the
        // ceiling is exceeded by an arbitrary multiple. A backtest that skipped this term would be
        // the PERMISSIVE side for exactly the strategy shape that trips it live (a maker resting
        // quotes on many symbols), which is the failure this whole function's doc is about.
        //
        // ⚠ Two deliberate differences from the MARGIN term directly above, both of which move
        // this fold TOWARD the live producer rather than away from it: every pending KIND is
        // counted (that fold takes `OrderKind::Market` alone, while live's `is_live()` registry
        // walk counts a resting limit too), and each order contributes its own `|size|` instead of
        // the per-symbol SIGNED sum's magnitude (exposure is gross by construction — see
        // `vike_exec::RiskLimits::max_account_exposure`; netting two opposed resting orders to zero
        // is precisely the reading that axis refuses). A COVERED reduce contributes nothing, the
        // live fold's own skip, through the same shared predicate.
        let account_exposure_excl_order =
            if self.risk_gate.as_ref().expect("checked Some").limits.max_account_exposure.is_some()
            {
                py_sum((0..self.symbols.len()).map(|i| {
                    let px_mult = self.sym[i].price * self.multiplier_of_idx(i);
                    let position = if i == si {
                        0.0
                    } else {
                        vike_model::gross_notional(
                            self.sym[i].pos.size,
                            self.sym[i].price,
                            self.multiplier_of_idx(i),
                        )
                    };
                    let resting = py_sum(self.sym[i].pending.iter().filter_map(|o| {
                        let covered = vike_model::is_covered_reduce(
                            false, // the sim's `WorkingOrder` carries no reduce_only flag; the
                            // predicate's IMPLICIT arm (opposite direction, fully covered) is the
                            // one that can fire here, and it is the same arm live relies on for an
                            // untagged exit.
                            o.side,
                            self.sym[i].pos.size,
                            o.size,
                        );
                        (!covered).then(|| o.size.abs() * px_mult)
                    }));
                    // …and the ORDERS ALREADY ON THE WIRE under the opt-in latency model, which
                    // have left `pending` and not yet filled. Taken exactly as the margin term
                    // above takes them (per-symbol SIGNED sum, magnitude); `None` — every default
                    // run — contributes nothing and this line is inert.
                    let in_flight = self
                        .latency
                        .as_ref()
                        .map_or(0.0, |g| g.in_flight_market_signed(i).abs() * px_mult);
                    position + resting + in_flight
                }))
            } else {
                0.0
            };
        let ctx = RiskContext {
            position_size: self.sym[si].pos.size,
            mark_price: mark,
            trading_state: TradingState::Active,
            now_ms: self.now,
            equity,
            margin_used,
            closing_credit,
            multiplier: mult,
            account_exposure_excl_order,
        };
        let req = OrderRequest {
            client_order_id: String::new(), // sim orders have no coid; the gate never reads it
            venue: self.venue.clone().unwrap_or_default(),
            symbol: self.symbols[si].clone(),
            order_type: order_type.to_string(),
            side: side_sign,
            qty: size,
            price,
            trigger_price: trigger,
            ..Default::default()
        };
        // BEHAVIOR NOTE (leverage+properties runs only — review M1): with the gate armed, the
        // venue floors now judge at PRE-TRADE time through this refresh, not only at apply_fill:
        // a below-min OPENING order is denied at submit with the gate's reason ("below-min-qty")
        // instead of dropped at fill ("min_qty"), and a below-min REVERSAL that previously flipped
        // whole at apply_fill is now DENIED — agreeing with live, which denies. Pure-leverage runs
        // without a properties source are unaffected (no grid to refresh).
        // Refresh the gate's instrument-grid fields from the PIT properties grid (the venue's
        // own time-varying facts; the operator knobs in `risk_limits` are never touched). Only
        // when a grid source is configured — otherwise explicit `risk_limits` floors stand.
        let grid = if self.venue.is_some() && self.properties.is_some() {
            Some(self.grid_for(si, self.now).as_ref().map(RiskLimits::from_properties))
        } else {
            None
        };
        let gate = self.risk_gate.as_mut().expect("checked Some");
        if let Some(g) = grid {
            let g = g.unwrap_or_else(RiskLimits::new); // no PIT record = unconstrained
            let lim = &mut gate.limits;
            lim.tick_size = g.tick_size;
            lim.lot_size = g.lot_size;
            lim.min_qty = g.min_qty;
            lim.min_notional = g.min_notional;
        }
        let verdict = gate.check(&req, &ctx);
        if verdict.ok {
            // The gate's normalized (lot-rounded) request is deliberately NOT substituted:
            // `apply_fill` re-snaps to the PIT grid at fill time, the sim's venue-side site.
            return size;
        }
        let sym = self.symbols[si].clone();
        self.dropped.push((sym, verdict.reason, size, weight));
        0.0
    }

    /// Shrink an opening/adding market order so projected TOTAL account notional
    /// <= leverage*equity (account-level; pending-aware; reducing never shrunk). The frozen
    /// pre-phase-1 TRUNCATE formula — since deny-vs-clamp phase 2 reached ONLY via
    /// [`Self::gate_order`] with `clamp_to_leverage == true` (the escape hatch); the
    /// default path now judges through the live `RiskGate` and never consults this.
    fn cap_to_leverage(&self, si: usize, side_sign: i32, size: f64) -> f64 {
        let Some(leverage) = self.leverage else {
            return size;
        };
        let eq = self.equity_now();
        if eq <= 0.0 {
            return 0.0;
        }
        let pos = &self.sym[si].pos;
        if pos.size != 0.0 && (pos.size > 0.0) != (side_sign > 0) {
            return size; // reducing/closing: never capped
        }
        let max_notional = leverage * eq;
        let cur = py_sum((0..self.symbols.len()).map(|i| {
            vike_model::gross_notional(
                self.sym[i].pos.size,
                self.sym[i].price,
                self.multiplier_of_idx(i),
            )
        }));
        let pending = py_sum((0..self.symbols.len()).map(|i| {
            let mut signed = py_sum(
                self.sym[i]
                    .pending
                    .iter()
                    .filter(|o| o.kind == OrderKind::Market)
                    .map(|o| o.side as f64 * o.size),
            );
            // ...plus the market orders this account has already SENT but the matching engine has
            // not yet seen (opt-in latency gate only; `None` leaves the fold byte-identical). A
            // pre-trade cap must model the LOCAL view — without this, every submit made inside one
            // entry-latency window would be granted the full leverage room and the cap would be
            // silently unenforced for the whole window. See `LatencyGate::in_flight_market_signed`.
            if let Some(g) = self.latency.as_ref() {
                signed += g.in_flight_market_signed(i);
            }
            vike_model::gross_notional(signed, self.sym[i].price, self.multiplier_of_idx(i))
        }));
        let room_notional = max_notional - cur - pending;
        if room_notional <= 0.0 {
            return 0.0;
        }
        let room = room_notional / (self.sym[si].price * self.multiplier_of_idx(si));
        if size <= room { size } else { room }
    }

    // --- order intake (strategy-facing verbs) ---

    pub fn submit(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        weight: f64,
        raw: bool,
        stop: Option<f64>,
    ) {
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, stop); // sizer first, then the pre-trade gate
        let size = self.gate_order(si, side_sign, size, weight, "market", None, None);
        if size > 0.0 {
            let mut o = WorkingOrder::new(OrderKind::Market, side_sign, size);
            o.weight = weight;
            o.stop = stop;
            self.push_pending(si, o);
        }
    }

    pub fn submit_close(&mut self, symbol: &str) {
        let si = self.idx(symbol);
        let pos = self.sym[si].pos;
        if pos.size != 0.0 {
            let side = vike_model::closing_side(pos.size);
            let o = WorkingOrder::new(OrderKind::Market, side, pos.size.abs());
            self.push_pending(si, o);
        }
    }

    pub fn submit_limit(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        price: f64,
        weight: f64,
        raw: bool,
        stop: Option<f64>,
    ) {
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, stop);
        let size = self.gate_order(si, side_sign, size, weight, "limit", Some(price), None);
        if size <= 0.0 {
            return; // denied by the pre-trade gate — same shape as the market verbs
        }
        let mut o = WorkingOrder::new(OrderKind::Limit, side_sign, size);
        o.price = Some(price);
        o.weight = weight;
        o.stop = stop;
        self.push_pending(si, o);
    }

    pub fn submit_stop(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        price: f64,
        weight: f64,
        raw: bool,
    ) {
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        // A stop's reference is its TRIGGER, which is what `check_inner`'s
        // `price.or(trigger_price)` falls to when there is no limit price.
        let size = self.gate_order(si, side_sign, size, weight, "stop", None, Some(price));
        if size <= 0.0 {
            return;
        }
        let mut o = WorkingOrder::new(OrderKind::Stop, side_sign, size);
        o.price = Some(price);
        o.weight = weight;
        self.push_pending(si, o);
    }

    pub fn submit_trailing(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        trail: f64,
        weight: f64,
        raw: bool,
    ) {
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        // A trailing order carries no price of its own — it references the mark, which is exactly
        // what `check_inner`'s `unwrap_or(ctx.mark_price)` fallback supplies.
        let size = self.gate_order(si, side_sign, size, weight, "trailing", None, None);
        if size <= 0.0 {
            return;
        }
        let extreme_snap = self.sym[si].price;
        let mut o = WorkingOrder::new(OrderKind::Trailing, side_sign, size);
        o.trail = Some(trail);
        o.extreme = Some(extreme_snap);
        o.weight = weight;
        self.push_pending(si, o);
    }

    pub fn submit_market_close(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        weight: f64,
        raw: bool,
    ) {
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        let size = self.gate_order(si, side_sign, size, weight, "market", None, None);
        if size > 0.0 {
            let mut o = WorkingOrder::new(OrderKind::MarketClose, side_sign, size);
            o.weight = weight;
            self.push_pending(si, o);
        }
    }

    pub fn submit_limit_close(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        price: f64,
        weight: f64,
        raw: bool,
    ) {
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        let size = self.gate_order(si, side_sign, size, weight, "limit", Some(price), None);
        if size <= 0.0 {
            return;
        }
        let mut o = WorkingOrder::new(OrderKind::LimitClose, side_sign, size);
        o.price = Some(price);
        o.weight = weight;
        self.push_pending(si, o);
    }

    /// Pull every untagged resting order for `symbol`. Under the opt-in latency gate the cancel
    /// is HELD in flight — which is the whole point: a cancel issued on the tick that would
    /// have filled the order arrives at the matching engine too late to save it.
    pub fn cancel_all(&mut self, symbol: &str) {
        let si = self.idx(symbol);
        if self.latency.is_some() {
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &crate::latency::LatencyOrder::NONE,
                crate::latency::InFlightAction::CancelAll { si },
            );
            if !sent {
                // a rejected CANCEL is the dangerous one: the orders stay resting
                self.record_latency_reject(si, 0.0, 0.0);
            }
            return;
        }
        self.sym[si].pending.clear();
    }

    /// The STRATEGY-level target verb (`Strategy._engine_target`): sizes through the ONE shared
    /// sizing law `vike_model::units_from_percent` (`pct·eq/(price·multiplier)`) — the SAME
    /// converter the live `order_target_percent` verb uses (`vike-core::runtime::broker`), so a
    /// mult≠1 instrument sizes identically live and in backtest. 1e-12 dead-band, submitted raw.
    /// At mult==1 `price·1.0 == price` exactly, so this is bit-identical to the former
    /// `pct·eq/price` (and the price==0 guard is folded into `units_from_percent`'s `denom>0.0`).
    pub fn strategy_order_target_percent(&mut self, symbol: &str, pct: f64) {
        let price = self.price_of(symbol);
        let mult = self.multiplier_of(symbol);
        let target = vike_model::units_from_percent(pct, self.equity_now(), price, mult);
        let pos = self.position_of(symbol).size;
        let delta = target - pos;
        if delta.abs() > 1e-12 {
            self.submit(symbol, if delta > 0.0 { 1 } else { -1 }, delta.abs(), 0.0, true, None);
        }
    }

    // --- higher-TF reads (mirror the single engine, per symbol) ---

    pub fn bars_for(&self, symbol: &str, tf: &str) -> &[Bar] {
        let si = self.idx(symbol);
        let (_, ms, coarse) = self.sym[si]
            .tf
            .iter()
            .find(|(name, _, _)| name == tf)
            .unwrap_or_else(|| panic!("timeframe {tf:?} not registered"));
        let window_start = self.now - self.now.rem_euclid(*ms);
        let idx = coarse.partition_point(|b| b.ts < window_start);
        &coarse[..idx]
    }

    pub fn forming_for(&self, symbol: &str, tf: &str) -> Option<Bar> {
        let si = self.idx(symbol);
        let (_, ms, _) = self.sym[si]
            .tf
            .iter()
            .find(|(name, _, _)| name == tf)
            .unwrap_or_else(|| panic!("timeframe {tf:?} not registered"));
        let window_start = self.now - self.now.rem_euclid(*ms);
        let base = &self.bars[si];
        let lo = base.partition_point(|b| b.ts < window_start);
        let hi = base.partition_point(|b| b.ts <= self.now);
        let window = &base[lo..hi];
        if window.is_empty() {
            return None;
        }
        let mut high = f64::NEG_INFINITY;
        let mut low = f64::INFINITY;
        for b in window {
            high = high.max(b.high);
            low = low.min(b.low);
        }
        Some(Bar {
            ts: window_start,
            open: window[0].open,
            high,
            low,
            close: window[window.len() - 1].close,
            volume: py_sum(window.iter().map(|b| b.volume)),
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
    }

    // --- slippage (flat, plus the opt-in size-dependent impact model) ---

    /// The adverse-move fraction to charge one fill of `size` on `symbols[si]` at `ts`.
    ///
    /// BYTE-IDENTICAL DEFAULT: with no [`EngineParams::impact`] model this returns the
    /// `slippage` field itself — the same `f64`, with no arithmetic performed on it — so every
    /// frozen fill path is bit-for-bit what it was. That is why the `None` arm is an early
    /// return rather than `self.slippage + 0.0`: adding a zero is not the identity on `-0.0`,
    /// and a parity gate is not the place to find that out.
    ///
    /// A FILL THAT SUPPLIED LIQUIDITY IS NOT CHARGED IMPACT, and which fills those are is
    /// [`Self::charges_impact`]'s question, not `is_maker`'s. The flat `slippage` applies to every
    /// fill exactly as it did on the frozen path; only the size-dependent addend is conditional.
    ///
    /// With a model configured this is TWO lane-shaped decisions, each with its own function so
    /// neither can be read off the other's comment: WHICH SERIES supplies the market context
    /// ([`Self::impact_context`] — the tick window when `run_ticks` armed one, the bar window
    /// otherwise, both free of lookahead) and WHICH TERMS of the model this lane has not already
    /// paid ([`Self::impact_terms`] — everything, unless the fill was priced by walking a
    /// replayed L2 book, which already charged the temporary half from real depth).
    ///
    /// A context too short or degenerate to measure (either window returning `None` — the first
    /// bars of a run, the first prints of a replay, a volume-less resampled series, a quote-only
    /// tape) charges the flat slippage alone rather than guessing.
    ///
    /// UNBOUNDED ABOVE, deliberately, and that is not the omission it looks like. The sum returned
    /// here can exceed 1.0 — `impact_frac` is bounded below (a cost is never negative) but never
    /// above — which on a SELL used to invert the fill price. The bound is NOT applied here: this
    /// function does not know the side, and clamping the sum would silently change the `None`/
    /// `is_maker` early returns this doc promises are the `slippage` field itself with no
    /// arithmetic performed on it. It is applied one layer down, at
    /// [`crate::broker_sim::adverse_fill_price`], which holds both the fraction and the side and is
    /// the single site every other producer of a slippage number also funnels through — so the
    /// cash pre-check, the vector kernel and the paper exchange are bounded by the same floor. See
    /// that module's docs, and [`Self::slippage_saturations`] for the diagnostic.
    /// Returns the slippage fraction AND whether an impact charge was SKIPPED for want of a market
    /// context — the second half exists because that skip is otherwise invisible, and a caller that
    /// can count ([`Self::apply_fill`] holds `&mut self`) must be able to see it without respelling
    /// the condition. The `bool` is `true` ONLY for "configured, and could not be priced": a run
    /// with no impact model at all reports `false`, because nothing was skipped.
    pub(crate) fn slippage_for(
        &self,
        si: usize,
        size: f64,
        ts: i64,
        is_maker: bool,
    ) -> (f64, bool) {
        if !self.charges_impact(is_maker) {
            return (self.slippage, false);
        }
        let Some(model) = self.impact.as_ref() else {
            return (self.slippage, false);
        };
        let Some(stats) = self.impact_context(si, ts) else {
            // CONFIGURED but unmeasurable — the one branch worth counting.
            return (self.slippage, true);
        };
        let extra = model.impact_frac_for(
            &crate::impact::ImpactInputs {
                qty: size.abs(),
                avg_volume: stats.avg_volume,
                sigma: stats.sigma,
            },
            self.impact_terms(si),
        );
        (self.slippage + extra, false)
    }

    /// The market context [`Self::slippage_for`] prices one fill against — measured off whichever
    /// series this lane actually has.
    ///
    /// TICK LANE FIRST, and only because `run_ticks` armed it: `impact_ticks` is EMPTY everywhere
    /// else, so the `Vec::get` misses and the bar read below is the frozen line, character for
    /// character. The tick window carries no `ts` argument because it cannot need one — it holds
    /// the prints that have ALREADY been folded, and `run_ticks` records each print after every
    /// fill site that tick reaches (`fill_pending_tick` AND `check_liquidation_tick`), so the
    /// lookahead discipline is enforced by WHEN the window is written rather than by a cutoff
    /// applied when it is read.
    ///
    /// ⚠ **That first branch is a SHORT-CIRCUIT and not a fallback, deliberately.** A tick replay
    /// CAN carry a real per-symbol bar series — `TickReplayConfig::seed_bar_interval_ms` (exposed
    /// as `EngineCfg::seed_bar_interval_ms`) builds one for exactly this lane — and once a window
    /// is armed that series is ignored even while the window is still warming up and measuring
    /// `None`. Falling back to it would be strictly worse than charging nothing: `sigma` and
    /// `avg_volume` are PER-PERIOD quantities, so the bar read and the print read answer in
    /// different units by a factor of the tape's print rate, and a fallback would price a run's
    /// first fills in bar units and its later ones in print units with nothing in the output
    /// saying which. One lane, one unit; [`Self::impact_prints`] is how an operator sees that the
    /// window is warming rather than that the model is off.
    ///
    /// BAR LANE otherwise, unchanged: the last `impact_window` bars of this symbol that CLOSED
    /// BEFORE `ts` — the bar `ts` falls INSIDE is excluded, because its own close and volume are
    /// not knowable at the moment the order prices, and its volume would in part be this very
    /// order. The window end is `partition_point(|b| b.ts <= ts) - 1` (the index of the bar
    /// containing `ts`), NOT `partition_point(|b| b.ts < ts)`: the two agree on the coarse bar
    /// lanes, where `ts` is exactly the landing bar's own `ts`, but only the former holds on the
    /// granular sub-bar lane (`dispatch_fill` is called with `sub.ts`, strictly INSIDE the coarse
    /// bar).
    fn impact_context(&self, si: usize, ts: i64) -> Option<crate::impact::MarketStats> {
        if let Some(window) = self.impact_ticks.get(si) {
            return window.stats();
        }
        let bars = &self.bars[si];
        let hi = bars.partition_point(|b| b.ts <= ts).saturating_sub(1);
        crate::impact::window_stats(&bars[..hi], self.impact_window)
    }

    /// Which HALVES of the impact model this lane has NOT already charged (see
    /// [`crate::impact::ImpactTerms`]).
    ///
    /// The one case that is not [`crate::impact::ImpactTerms::Both`] is a fill priced by WALKING a
    /// replayed book: [`FillModelKind::L2Book`] with a book actually present for this symbol. That
    /// walk consumed real displayed depth for the order's own size, which IS the temporary
    /// concession — asking the model for it again would charge the same liquidity twice. What the
    /// walk cannot supply is the permanent footprint, because the replayed book is a recording
    /// that never saw the order.
    ///
    /// The predicate is deliberately the SAME pair [`Self::fill_price_for`] dispatches on, read
    /// off broker state rather than passed in: `L2BookFillModel` degrades to the L1 tier exactly
    /// when `book_at` is `None`, so a bookless symbol on an `L2Book` run is charged `Both` — which
    /// is correct, because on that event nothing walked anything.
    ///
    /// ⚠ **It is a per-EVENT book-presence test, not a per-FILL "was this price a walk" test, and
    /// on one path those differ.** Every taker fill funnels through [`Self::apply_fill`], including
    /// the FORCED closes `StrategyEngine::check_liquidation_tick` books at the adverse print or the
    /// last mark — prices that never touched [`Self::fill_price_for`] and walked nothing. On an
    /// `L2Book` replay with a book present those are charged `PermanentOnly` and are therefore
    /// UNDER-charged, which is the wrong direction on exactly the fills where flattering a
    /// backtest matters most. Closing it means carrying the terms down from the site that produced
    /// the price rather than re-deriving them from broker state, which is a signature change
    /// across every `apply_fill` caller; it is declared here rather than assumed away, and it is
    /// bounded — the miss is the temporary term of a liquidation on a book-bearing L2 tape.
    fn impact_terms(&self, si: usize) -> crate::impact::ImpactTerms {
        if self.fill_model == FillModelKind::L2Book && self.book_at(si).is_some() {
            crate::impact::ImpactTerms::PermanentOnly
        } else {
            crate::impact::ImpactTerms::Both
        }
    }

    /// Whether this fill pays the size-dependent addend at all — the LANE's answer to "did this
    /// fill supply liquidity or demand it".
    ///
    /// A taker always pays. A `is_maker` fill pays on every lane EXCEPT the bar lane, and that
    /// asymmetry is the whole point: `is_maker` is set from the order KIND
    /// (`crates/vike-backtest/src/engine.rs`'s `dispatch_fill` — "a MARKETABLE limit books as a
    /// maker fill"), which the FEE side needs, and which says nothing about aggressiveness. What
    /// a `Limit` fill is actually priced at is a per-lane fact:
    ///
    /// - **bar** — `vike_model::order_fill_price`'s `Limit` arm returns `price.min(bar.open)` for
    ///   a buy: at-or-better than the limit, reached because the market came to a resting order.
    ///   That IS a supplied-liquidity fill, and a haircut on top would execute it through its own
    ///   price. EXEMPT, and byte-identical to the frozen path.
    /// - **tick** — [`crate::fill_model::TickFillModel`] fills a buy limit only when `ask <=
    ///   price`, AT THE ASK, for ANY size. It crossed the spread and took the touch. CHARGED.
    /// - **L2** — `crate::fill_model::book_taker_price` capped at the limit: a walk down real
    ///   resting levels for the order's own size. CHARGED.
    ///
    /// Exempting the replay lanes made the entire charge AVOIDABLE — a taker respelt as a
    /// marketable limit one tick through the touch fills at the same price for the same size and
    /// paid nothing. The accepted cost of charging it (a fill can land past its own limit price)
    /// and why the two alternatives are worse are argued in `crate::impact`'s module docs.
    ///
    /// ⚠ Keyed on the FILL MODEL rather than on which loop is running, because that is the thing
    /// that chose the price law. The one configuration where the two disagree is a hand-built
    /// `EngineParams` running the BAR loop with `FillModelKind::Tick` over CONSOLIDATED bars
    /// (`high != low`), where `TickFillModel` delegates back to the bar law and a limit can fill
    /// passively yet be charged here. `crates/vike-backtest/src/harness/run.rs`'s `run_backtest`
    /// sets `fill_model` on its TICK arm alone, so no profile can reach that configuration, and
    /// the error runs pessimistic.
    fn charges_impact(&self, is_maker: bool) -> bool {
        !is_maker || self.fill_model != FillModelKind::Bar
    }

    // --- fill application (called by the engine dispatchers) ---

    /// Apply one fill; returns `None` when the PIT grid gates the fill (an opening/increasing
    /// fill below `min_qty`/`min_notional`, or a dust size after step-rounding) — the caller
    /// must treat `None` as "no fill happened" and skip `fire_on_fill`.
    pub(crate) fn apply_fill(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        price: f64,
        ts: i64,
        is_maker: bool,
    ) -> Option<Fill> {
        // Bound to a local so the saturation can be COUNTED without pricing the fill twice; the
        // value handed to `adverse_fill_price` is the same `f64` the one-liner passed before, so
        // the fill price is byte-identical on every input that did not already invert.
        let (slip, impact_skipped) = self.slippage_for(si, size, ts, is_maker);
        if crate::broker_sim::adverse_move_saturates(side_sign, slip) {
            self.slippage_saturations += 1;
        }
        // Counted HERE and nowhere else, for the reason both counters' docs give: the cash
        // pre-check in `StrategyEngine` prices this same fill a second time.
        if impact_skipped {
            self.impact_unpriced += 1;
        }
        let price = adverse_fill_price(price, side_sign, slip);
        let (size, price) = match self.grid_for(si, ts) {
            None => (size, price),
            Some(f) => {
                let price = vike_model::round_to(price, vike_model::nz_step(f.tick_size));
                let rounded = vike_model::round_to(size, vike_model::nz_step(f.step_size));
                // Reject only OPENING/increasing fills — a closing fill must always execute so a
                // position is never stranded below-min.
                //
                // NOTE the deliberate asymmetry vs `vike_exec::RiskGate`: this predicate is
                // DIRECTION-ONLY (`vike_model::is_reducing_direction`), while the gate's floor
                // bypass is `is_covered_reduce` (direction AND `|position| >= |qty|`). A below-min
                // REVERSAL therefore executes whole here but is denied by the gate. That
                // divergence is pre-existing and documented on both sides; unifying it changes
                // FILL DECISIONS, so it is NOT folded into this hoist — see the `risk.rs`
                // "KNOWN RESIDUAL DIVERGENCE" comment for the intended SimBroker-side fix (split
                // a flip and floor-gate only its opening half).
                let opening = !vike_model::is_reducing_direction(side_sign, self.sym[si].pos.size);
                // DELIBERATELY hand-rolled, NOT `vike_model::order_notional` — the ALLOWLIST
                // entry for this file in `vike-ops/tests/duplicate_shape_gate.rs` carries the
                // full reason. Short version: the helper takes the magnitude of all THREE
                // factors, this floor takes the SIGNED product, and the two verdicts genuinely
                // disagree whenever an odd number of factors is negative. Adopting the helper
                // would be a FILL-DECISION change, not a byte-identical hoist.
                //
                // ⚠ Hoisted ABOVE the opening/closing split so the floor verdict is spelled ONCE
                // and both consumers below read the same two booleans — the opening branch's
                // refusal, and the closing branch's divergence counter. Pure arithmetic with no
                // side effects, so computing it on the closing path too is byte-identical; the
                // alternative (recomputing it under the counter) is a second spelling of the very
                // shape `notional_is_never_respelled` exists to prevent, and that gate caught it.
                let notional = rounded * price * self.multiplier_of_idx(si);
                let below_qty = f.min_qty > 0.0 && rounded < f.min_qty;
                let below_not = f.min_notional > 0.0 && notional < f.min_notional;
                // ⚠ MEASUREMENT ONLY — counts the known live-vs-backtest divergence, changes
                // nothing. See `SimBroker::below_min_reversals`.
                //
                // The condition is exactly "live denies, backtest fills": the order OPPOSES the
                // position (so this branch calls it closing and waves it through), it EXCEEDS the
                // position (a flip, so `is_covered_reduce` is false and the live gate's floor bypass
                // does NOT apply), and it is BELOW a venue floor (so the live gate refuses it).
                if !opening
                    && self.sym[si].pos.size != 0.0
                    && rounded.abs() > self.sym[si].pos.size.abs()
                    && (below_qty || below_not)
                {
                    self.below_min_reversals += 1;
                }
                if opening {
                    if below_qty || below_not {
                        let reason = if below_qty { "min_qty" } else { "min_notional" };
                        self.dropped.push((
                            self.symbols[si].clone(),
                            reason.into(),
                            rounded,
                            price,
                        ));
                        return None;
                    }
                    (rounded, price)
                } else {
                    // Closing/reducing fill must ALWAYS execute. If step-rounding zeroed a small
                    // remainder — reachable because the PIT `step_size` can WIDEN over a position's
                    // life (day-1 open on a fine grid, later close on a coarser one) — fall back to
                    // the raw size so the position is never stranded (also guards the engine's own
                    // force-close paths: stops, liquidation, close-inactive).
                    (if rounded > 0.0 { rounded } else { size }, price)
                }
            }
        };
        if size <= 0.0 {
            return None; // step-rounding zeroed a dust size
        }
        // NOTE (fee-model caveat, see `vike_model::fees`): `is_maker` reaches here decided by ORDER
        // KIND at the dispatch sites, not by crossing aggressiveness — a marketable limit books as
        // a maker fill. Harmless for the flat maker/taker rates used here; it flips the fee SIGN
        // under a rebate-bearing `FeeSchedule::ProbabilityScaled`.
        let rate = if is_maker { self.maker_fee } else { self.taker_fee };
        let mult = self.multiplier_of_idx(si);
        // `fee_curve` is `Some` ONLY for a schedule shape with no flat rate (today:
        // `ProbabilityScaled`) — see `EngineParams::fee_schedule`. Every other configuration
        // takes the frozen `size * price * rate * mult` fold below, byte-identical.
        let fee = match self.fee_curve {
            Some(schedule) => schedule.commission(is_maker, size, price) * mult,
            None => fee_fn(size, price, rate, mult),
        };
        Some(self.fold_fill(si, side_sign, size, price, ts, is_maker, fee, false))
    }

    /// The post-cost fold shared by [`Self::apply_fill`] (which computes slippage / PIT-grid /
    /// fee first) and [`Self::settle_at_payout`] (fee `0.0`, no slippage, no grid — a venue
    /// settlement, not an order fill): cash moves, the `TradeFold` cost-basis step,
    /// excursion/stop bookkeeping, and the trade + mirror records. Extracted VERBATIM from
    /// `apply_fill` — the f64 op order is byte-identical, so every parity gate is unaffected.
    fn fold_fill(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        price: f64,
        ts: i64,
        is_maker: bool,
        fee: f64,
        settlement: bool,
    ) -> Fill {
        let mult = self.multiplier_of_idx(si);
        let delta = side_sign as f64 * size;
        self.cash -= fee; // transaction cost
        self.cash -= vike_model::signed_notional(delta, price, mult); // signed notional moves cash
        let st = &mut self.sym[si];
        // Drive the shared trade fold over this symbol's position state (copy in, fold, write
        // back). The excursion (`hi_since`/`lo_since`) and protective-`stop` bookkeeping stays
        // engine-local — the fold owns only the cost-basis/fee-apportionment math.
        let mut fold = TradeFold {
            size: st.pos.size,
            avg_px: st.pos.avg_price,
            entry_fee: st.entry_fee,
            entry_ts: st.entry_ts,
        };
        let step = fold.apply(side_sign, size, price, fee, ts, mult);
        st.pos.size = fold.size;
        st.pos.avg_price = fold.avg_px;
        st.entry_fee = fold.entry_fee;
        st.entry_ts = fold.entry_ts;
        match step.kind {
            FillKind::Open => {
                st.hi_since = price; // reset excursion extremes to entry
                st.lo_since = price;
            }
            FillKind::Add => {}
            _ => {
                // reduce / close / flip
                let c = step.closed.expect("reduce/close/flip yields a closed trade");
                st.realized += c.pnl;
                let entry = c.entry_price;
                // NB: `c.is_long` is the PRIOR sign — the closed portion's direction, exactly as
                // in Python (the fold captured it before updating `size`).
                let (mfe, mae) = if entry != 0.0 {
                    if c.is_long {
                        ((st.hi_since - entry) / entry, (st.lo_since - entry) / entry)
                    } else {
                        ((entry - st.lo_since) / entry, (entry - st.hi_since) / entry)
                    }
                } else {
                    (0.0, 0.0)
                };
                match step.kind {
                    FillKind::Flip => {
                        st.hi_since = price;
                        st.lo_since = price;
                        st.stop = None; // old stop belonged to the closed position
                    }
                    FillKind::Close => st.stop = None,
                    _ => {} // Reduce: keep the stop and the running extremes
                }
                self.trades.push(Trade {
                    entry_price: c.entry_price,
                    exit_price: c.exit_price,
                    size: c.size,
                    pnl: c.pnl,
                    fees: c.fees,
                    entry_ts: c.entry_ts,
                    exit_ts: c.exit_ts,
                    symbol: self.symbols[si].clone(),
                    mae,
                    mfe,
                    is_long: c.is_long,
                });
            }
        }
        if let Some(sink) = &mut self.mirror_fills {
            sink.push(MirrorFill {
                symbol: self.symbols[si].clone(),
                side: side_sign,
                size,
                price,
                fee,
                ts,
                is_maker,
                is_settlement: settlement,
            });
        }
        Fill { side: side_sign, size, price, fee, ts, is_maker, symbol: self.symbols[si].clone() }
    }

    /// Binary-resolution settlement (pm-economics lane): close the WHOLE open position at the
    /// terminal `payout` price. Fee-free, slippage-free and PIT-grid-free — a settlement is a
    /// venue event, not an order fill, so none of the order-fill costs/gates apply (and a closing
    /// fill must never be gated anyway — the anti-stranding rule). Records a distinct
    /// [`SettlementFill`] in [`Self::settlements`] and returns the (fee `0.0`) close `Fill` for
    /// `on_fill`; `None` when already flat. The caller (the engine's resolution sweep) latches the
    /// symbol resolved and cancels its resting orders BEFORE calling this, so the `on_fill` this
    /// fill triggers cannot re-open the market. When the ledger mirror is enabled the settlement
    /// is pushed as a [`MirrorFill`] flagged `is_settlement` — a payout, not a trade.
    pub(crate) fn settle_at_payout(&mut self, si: usize, payout: f64, ts: i64) -> Option<Fill> {
        let pos = self.sym[si].pos.size;
        if pos == 0.0 {
            return None;
        }
        let side = vike_model::closing_side(pos);
        let qty = pos.abs();
        self.settlements.push(SettlementFill {
            symbol: self.symbols[si].clone(),
            payout,
            qty,
            side,
            ts,
        });
        Some(self.fold_fill(si, side, qty, payout, ts, false, 0.0, true))
    }

    /// Variation-margin settlement sweep for one step (the opt-in
    /// [`EngineParams::settlement_period_ms`] cadence — see that field for the full contract).
    ///
    /// Returns IMMEDIATELY when the cadence is unset or non-positive, which is what keeps every
    /// default run byte-identical: no bucket is ever computed, no symbol is ever visited. When the
    /// step's bucket (`ts.div_euclid(period)`) still matches the stored one this is also a no-op;
    /// on the first step of a run the bucket is merely ANCHORED (there is no prior mark to settle
    /// against).
    ///
    /// On a boundary, for each symbol with an open position: `dpl = size * (price - avg_price) *
    /// mult` is added to `realized` and to `settled_profit`, and `avg_price` is reset to `price`.
    /// `equity_now()` is unchanged by construction (it never reads `avg_price`), and the per-symbol
    /// curve `realized + size·(price − avg_price)·mult` is likewise invariant — the two terms move
    /// by `+dpl` and `−dpl`. Symbols are visited in `symbols` order (the engine's deterministic
    /// insertion order), so the sequence of settlements is reproducible; no cross-symbol f64 sum is
    /// formed here, so there is no `py_sum` site to preserve.
    pub(crate) fn settle_variation_margin(&mut self, ts: i64) {
        let period = match self.settlement_period_ms {
            Some(p) if p > 0 => p,
            _ => return,
        };
        let bucket = ts.div_euclid(period);
        match self.settle_bucket {
            Some(b) if b == bucket => return,
            Some(_) => self.settle_bucket = Some(bucket),
            None => {
                // first step: anchor only — nothing has been marked yet
                self.settle_bucket = Some(bucket);
                return;
            }
        }
        for si in 0..self.symbols.len() {
            let mult = self.multiplier_of_idx(si);
            let st = &self.sym[si];
            if st.pos.size == 0.0 {
                continue;
            }
            let mark = st.price;
            let dpl = st.pos.size * (mark - st.pos.avg_price) * mult;
            if dpl == 0.0 {
                continue; // flat mark: nothing crosses, and the basis is already `mark`
            }
            let st = &mut self.sym[si];
            st.pos.avg_price = mark;
            st.realized += dpl;
            st.settled_profit += dpl;
            let settled_profit = st.settled_profit;
            self.variation_settlements.push(VariationSettlement {
                symbol: self.symbols[si].clone(),
                ts,
                mark,
                amount: dpl,
                settled_profit,
            });
        }
    }

    // --- stale-price wait discipline (the opt-in `EngineParams::max_price_staleness_ms` lane) ---

    /// Record `event` as this symbol's last fresh print when it carries one. A no-op — a single
    /// `is_none` branch, no indexing — unless the wait discipline is configured.
    ///
    /// Called ONCE per symbol per event, immediately BEFORE that event's fill phase, so an event
    /// that is itself a print has age `0` and fills on the spot. See [`crate::staleness`].
    pub(crate) fn note_print(&mut self, si: usize, event: &Bar) {
        if self.max_price_staleness_ms.is_none() {
            return;
        }
        self.note_print_at(si, event.ts, crate::staleness::is_fresh_print(event));
    }

    /// Scalar twin of [`Self::note_print`] for callers that hold a borrow of `self.bars` and so
    /// cannot pass a `&Bar` across the `&mut self` write (the bar-path step loop).
    pub(crate) fn note_print_at(&mut self, si: usize, ts: i64, fresh: bool) {
        if self.max_price_staleness_ms.is_none() {
            return;
        }
        if fresh {
            self.last_print_ts[si] = Some(ts);
        }
    }

    /// Must a `kind` order on `symbols[si]` WAIT rather than fill at `ts`? `false` whenever the
    /// discipline is off (the byte-identical default), whenever the kind is price-conditional,
    /// and whenever the symbol's last fresh print is within the bound.
    #[inline]
    pub(crate) fn defer_stale(&self, si: usize, kind: OrderKind, ts: i64) -> bool {
        let Some(bound) = self.max_price_staleness_ms else { return false };
        crate::staleness::defers(kind)
            && crate::staleness::is_stale(self.last_print_ts[si], ts, bound)
    }

    /// Is `symbol`'s price currently stale? Read-only strategy surface — a maker can check this
    /// before sending a taker order it knows would be deferred. Always `false` when the
    /// discipline is off; TOTAL on an unknown symbol (answers `false`, same rule as
    /// [`Self::is_resolved`] — a predicate has an honest answer where a write verb would panic).
    ///
    /// CLOCK: measured against **that symbol's own current bar ts** when the bar path is active,
    /// falling back to the engine clock `now` otherwise (the tick path, where `now` IS the
    /// just-routed tick's ts). That matters on a ragged multi-symbol tape: `now` is symbol 0's
    /// bar ts, while the fill lane judges each symbol against `bars[si][step].ts`, so reading the
    /// engine clock here would let this predicate disagree with the engine's actual deferral
    /// decision. Using the symbol's own bar keeps the two in agreement.
    pub fn price_is_stale(&self, symbol: &str) -> bool {
        if self.max_price_staleness_ms.is_none() {
            return false;
        }
        match self.symbols.iter().position(|s| s == symbol) {
            Some(si) => {
                let ts =
                    self.bars.get(si).and_then(|b| b.get(self.index)).map_or(self.now, |b| b.ts);
                self.defer_stale(si, OrderKind::Market, ts)
            }
            None => false,
        }
    }

    // --- session / market-hours gate (the opt-in `EngineParams::session_gate` lane) ---

    /// Is `symbols[si]`'s market CLOSED at `ts`? `false` — one `Vec::get` returning `None`, no
    /// calendar lookup — whenever the gate is off (the byte-identical default), the symbol has no
    /// calendar pinned (fail-permissive), or that symbol's market trades 24/7.
    ///
    /// This is the per-symbol counterpart to [`Self::defer_stale`], and it deliberately gates MORE
    /// than staleness does. Staleness exempts price-conditional kinds and protective stops: a
    /// repeated stale price cannot spuriously satisfy a condition, and gating a protective stop
    /// would strand live risk while the venue is perfectly able to fill it. A CLOSED market is a
    /// different fact — no order of any kind can execute, because there is no matching engine
    /// running. A protective stop that "fires" over a weekend is fiction; the real one fills at
    /// the next open, into the gap. So this gate applies to every fill lane uniformly — the coarse,
    /// granular, cash-gated and tick pending passes (`defer_fill`), the tagged maker lane
    /// (`fill_tagged`), protective stops (`check_stop`), AND the opt-in `queue_model` tick twins for
    /// both the queued resting limit and the queued maker tag (`fill_pending_tick_queued` /
    /// `fill_tagged_queued`) — and the stop simply fires on the first in-session event at that
    /// event's (gapped) price, which is exactly the adverse-gap law
    /// [`crate::engine::StrategyEngine::check_stop`] already implements. Keyed PER SYMBOL so a
    /// mixed-venue run gates each instrument by its own market.
    #[inline]
    pub(crate) fn session_closed(&self, si: usize, ts: i64) -> bool {
        match self.session.get(si) {
            Some(Some(cal)) => !cal.is_open(ts),
            _ => false, // gate off (empty vec), si out of range, or no calendar ⇒ open
        }
    }

    /// Is `symbol`'s market currently open? Read-only strategy surface, the sibling of
    /// [`Self::price_is_stale`] — a strategy can check before quoting into a market that cannot
    /// fill. Always `true` when the gate is off (empty session vec), and `true` on an unknown
    /// symbol (a total predicate, same stance as `price_is_stale`), so a strategy reading it is
    /// never misled into thinking a default run is session-aware.
    ///
    /// CLOCK: that symbol's own current-step bar ts when the bar path is active, falling back to
    /// the engine clock `now` (the tick path). Same discipline as [`Self::price_is_stale`]: on a
    /// ragged multi-symbol tape `now` is symbol 0's ts, but the fill lane judges each symbol
    /// against its own bar ts — reading the engine clock here would let this predicate disagree
    /// with the actual gate decision.
    pub fn symbol_is_open(&self, symbol: &str) -> bool {
        if self.session.is_empty() {
            return true; // gate off
        }
        match self.symbols.iter().position(|s| s == symbol) {
            Some(si) => {
                let ts =
                    self.bars.get(si).and_then(|b| b.get(self.index)).map_or(self.now, |b| b.ts);
                !self.session_closed(si, ts)
            }
            None => true,
        }
    }

    /// The replayed L2 book for `symbols[si]`, or `None` on every path that carries none.
    pub(crate) fn book_at(&self, si: usize) -> Option<&vike_model::L2Book> {
        self.books.get(si).and_then(|b| b.as_deref())
    }

    /// How many TRADE PRINTS the tick lane's impact context currently holds for `symbol` — or
    /// `None` when no context is armed, which is every path except a `run_ticks` replay with an
    /// [`EngineParams::impact`] model configured.
    ///
    /// A diagnostic in the same spirit as [`Self::slippage_saturations`], and it answers a
    /// question this lane can otherwise only answer silently: a quote-only tape records no prints,
    /// so an armed model measures nothing and charges nothing. "The model was off" and "the model
    /// had nothing to measure" produce identical fills, and an operator must be able to tell them
    /// apart. Reads `0` during the run's warmup and `None` once the replay has torn its window
    /// down; an unknown symbol is `None` too.
    pub fn impact_prints(&self, symbol: &str) -> Option<usize> {
        let si = self.symbols.iter().position(|s| s == symbol)?;
        self.impact_ticks.get(si).map(crate::impact::TickWindow::len)
    }

    /// The fill-price decision. `si` selects the symbol whose replayed book the
    /// [`FillModelKind::L2Book`] tier prices from; the other two tiers ignore it entirely, so the
    /// frozen paths are byte-identical.
    pub(crate) fn fill_price_for(&self, si: usize, o: &mut WorkingOrder, bar: &Bar) -> Option<f64> {
        match self.fill_model {
            FillModelKind::Bar => BarFillModel.fill_price(o, bar),
            FillModelKind::Tick => TickFillModel.fill_price(o, bar),
            FillModelKind::L2Book => L2BookFillModel.fill_price_book(o, bar, self.book_at(si)),
        }
    }

    /// The opt-in emulator-mirroring stop release (see
    /// [`EngineParams::emulator_release_stops`]): when armed and `o` is a resting
    /// [`OrderKind::Stop`] whose trigger the SAME oracle fill check says has crossed on `event`,
    /// CONVERT it in place to a resting MARKET child — the live `ConditionalBook` release law —
    /// and return `true`; the caller keeps it resting, so it fills at the NEXT event's price.
    /// `false` (the knob off, a non-stop kind, or no trigger) leaves `o` untouched — the
    /// byte-identical default path. The Stop fill check never mutates the order, so consulting
    /// it here and again on a later event is side-effect-free.
    pub(crate) fn stop_released(&self, si: usize, o: &mut WorkingOrder, event: &Bar) -> bool {
        if !self.emulator_release_stops || o.kind != OrderKind::Stop {
            return false;
        }
        if self.fill_price_for(si, o, event).is_none() {
            return false;
        }
        o.kind = OrderKind::Market;
        o.price = None;
        true
    }
}

/// The sim engine core is the strategy's `Broker` in backtest: DIRECT mutation, monomorphized
/// dispatch (never `dyn` — the hot loop must not pay a vtable per verb). Portable strategies
/// (`impl<B: Broker> Strategy<B>`) get exactly this common-denominator surface; backtest-rich
/// verbs (weighted/bracket submits, cancel_all, target-percent, schedule) stay inherent on
/// `SimBroker` for `impl Strategy<SimBroker>` strategies.
impl Broker for SimBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        // raw = true: the portable surface has no sizer concept (neither does the live path)
        self.submit(symbol, side, qty, 0.0, true, None);
    }

    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, price: f64) {
        SimBroker::submit_limit(self, symbol, side, qty, price, 0.0, true, None);
    }

    fn position(&self, symbol: &str) -> f64 {
        // The STRATEGY's view: the response-latency shadow while the opt-in latency gate is armed
        // (see `SimBroker::shadow_pos`), exchange truth otherwise — and `shadow_pos` is EMPTY on
        // every path but an armed `run_ticks`, so this is the frozen line verbatim there.
        // `position_of` is the un-shadowed read for anything that needs the exchange's own state.
        let si = self.idx(symbol);
        if !self.shadow_pos.is_empty() {
            return self.shadow_pos[si];
        }
        self.sym[si].pos.size
    }

    fn price(&self, symbol: &str) -> f64 {
        self.price_of(symbol)
    }

    fn equity(&self) -> f64 {
        self.equity_now()
    }

    fn bars(&self, symbol: &str) -> &[Bar] {
        // closed bars up to & including the current step (tick runs may outrun the bar series)
        let si = self.idx(symbol);
        let series: &Vec<Bar> = &self.bars[si];
        let end = (self.index + 1).min(series.len());
        &series[..end]
    }

    fn index(&self) -> usize {
        self.index
    }

    fn now(&self) -> i64 {
        self.now
    }

    /// Answered from the replayed L2 book with EXACTLY the law the fill path uses
    /// ([`crate::fill_model::book_taker_price`]) — so a strategy that gates on this read and then
    /// submits that size gets the fill it was quoted, and the two can never drift apart into a
    /// live-vs-backtest divergence.
    ///
    /// `None` on every book-less path (bar engine, vector kernel, tick replay with no `Tick::Book`
    /// series, unknown symbol) and whenever displayed depth cannot cover `qty`.
    fn quote_vwap(&self, symbol: &str, side: i32, qty: f64) -> Option<f64> {
        let si = self.symbols.iter().position(|s| s == symbol)?;
        crate::fill_model::book_taker_price(self.book_at(si)?, side, qty, None)
    }

    /// Displayed size at `limit_px` or better, straight off the replayed book. `0.0` on every
    /// book-less path and for an unknown symbol — which correctly sizes a depth-sizing strategy
    /// down to no trade rather than to an imaginary one.
    fn depth_within_price(&self, symbol: &str, side: i32, limit_px: f64) -> f64 {
        let Some(si) = self.symbols.iter().position(|s| s == symbol) else { return 0.0 };
        self.book_at(si).map_or(0.0, |b| b.quantity_for_price(side, limit_px))
    }
}

impl SimBroker {
    /// The symbol index the single-symbol [`HftBroker`] surface targets: the FIRST (index 0), which
    /// IS the mounted (venue, symbol) series for a maker backtest — you mount the maker on a
    /// one-symbol `SimBroker`. The `HftBroker` trait is single-symbol scoped by contract (no symbol
    /// argument on its verbs); a multi-symbol `SimBroker` is a portfolio construct that does not
    /// mount an `HftBroker` strategy.
    const HFT_SI: usize = 0;
}

/// The backtest HFT maker surface ([`HftBroker`]): tagged resting-limit submit / modify / cancel +
/// the mounted symbol's signed position. This is what lets `SpreadMaker` (and any future
/// `impl<B: HftBroker> Strategy<B>` maker) mount in the backtest engine — the crossing fills of the
/// resting tagged limits are applied by [`StrategyEngine::fill_tagged`] on the standard bar and tick
/// paths. Single-symbol scoped (see [`SimBroker::HFT_SI`]); the verbs never touch the untagged
/// `pending` lane, so every existing (parity-gated) backtest is byte-for-byte unaffected.
impl HftBroker for SimBroker {
    fn position(&self) -> f64 {
        // The maker's OWN view of its inventory: the response-latency shadow while the opt-in
        // gate is armed, exchange truth otherwise (`shadow_pos` is empty on every other path, so
        // this is the frozen line verbatim). `SpreadMaker` polls this on every requote to drive
        // its skew and its A-S reservation price — it is the single most important read the
        // response leg has to cover. See `SimBroker::shadow_pos`.
        if !self.shadow_pos.is_empty() {
            return self.shadow_pos[Self::HFT_SI];
        }
        self.sym[Self::HFT_SI].pos.size
    }

    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        // A passive resting LIMIT (maker) at `price`. RAW: no sizer, no leverage cap — the portable
        // HFT surface, exactly like `Broker::submit_limit`. Inserting under `tag` REPLACES any prior
        // resting order for that tag (a re-submit), matching the live tag→coid registry's overwrite.
        // A RESOLVED market refuses new quotes, exactly as `push_pending` refuses new orders — the
        // maker keeps re-quoting on every tick, so the tag must simply not rest (nothing recorded).
        if self.is_resolved_idx(Self::HFT_SI) {
            return;
        }
        // ⚠ RAW means no SIZER and no leverage cap — it never meant no RISK GATE. This verb is how
        // `vike-mm` places every quote it has, so leaving it ungated meant the maker's whole
        // backtest ran outside the operator's `[risk]` budget while live judged each quote.
        // `weight` is 0.0: the portable HFT surface carries no per-order weight, matching the
        // `raw` market path.
        let qty = self.gate_order(Self::HFT_SI, side, qty, 0.0, "limit", Some(price), None);
        if qty <= 0.0 {
            return;
        }
        let mut o = WorkingOrder::new(OrderKind::Limit, side, qty);
        o.price = Some(price);
        if self.latency.is_some() {
            let desc = crate::latency::LatencyOrder::new(side, qty, Some(price));
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &desc,
                crate::latency::InFlightAction::SubmitTagged { tag: tag.to_string(), order: o },
            );
            if !sent {
                self.record_latency_reject(Self::HFT_SI, qty, 0.0);
            }
            return;
        }
        self.sym[Self::HFT_SI].tagged.insert(tag.to_string(), o);
    }

    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        // Re-price / re-size the resting order IN PLACE (there is no queue to lose in the sim — see
        // `fill_tagged`'s documented limits). No-op if the tag is unknown or already filled/canceled
        // (its entry was removed) — the trait contract, and what the live `modify_order` gate does on
        // a terminal order.
        if self.latency.is_some() {
            let desc = crate::latency::LatencyOrder::new(0, new_qty.unwrap_or(0.0), new_price);
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &desc,
                crate::latency::InFlightAction::ModifyTagged {
                    tag: tag.to_string(),
                    new_qty,
                    new_price,
                },
            );
            if !sent {
                self.record_latency_reject(Self::HFT_SI, new_qty.unwrap_or(0.0), 0.0);
            }
            return;
        }
        if let Some(o) = self.sym[Self::HFT_SI].tagged.get_mut(tag) {
            if let Some(q) = new_qty {
                // An amend UP is a new order at the back of the queue on every real venue
                // (an amend DOWN keeps priority). Clearing the engine-local identity is how
                // that is signalled to the OPT-IN queue model, which re-seeds on an identity
                // change; nothing else reads `qid`, so the default path is unaffected.
                if q > o.size {
                    o.qid = 0;
                }
                o.size = q;
            }
            if let Some(p) = new_price {
                o.price = Some(p);
            }
        }
    }

    fn cancel_tagged(&mut self, tag: &str) {
        // Pull ONE resting quote and leave every other tag untouched; no-op on an unknown/terminal
        // tag. `shift_remove` (not `swap_remove`) keeps the survivors in insertion order for a
        // deterministic replay. Under the opt-in latency gate the pull is HELD in flight.
        if self.latency.is_some() {
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &crate::latency::LatencyOrder::NONE,
                crate::latency::InFlightAction::CancelTagged { tag: tag.to_string() },
            );
            if !sent {
                // a rejected CANCEL is the dangerous one: the quote stays resting
                self.record_latency_reject(Self::HFT_SI, 0.0, 0.0);
            }
            return;
        }
        self.sym[Self::HFT_SI].tagged.shift_remove(tag);
    }
}
