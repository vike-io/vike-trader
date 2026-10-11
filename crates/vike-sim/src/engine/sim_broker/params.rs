//! `EngineParams`, every knob of one run with its default, plus the settlement-source shapes.

use std::sync::Arc;

use indexmap::IndexMap;
use vike_model::RiskLimits;
use vike_model::{Bar, FeeSchedule, OptionSpec, SessionCalendar, SymbolProperties};
#[cfg(doc)]
use vike_model::{Broker, HftBroker, OrderKind, WorkingOrder};

use vike_analytics::sizing::PositionSizer;

use super::knobs::{DEFAULT_IMPACT_WINDOW, DecideMode, EquitySampling, FillModelKind};
#[cfg(doc)]
use super::state::{MirrorFill, SimBroker, SymbolState};

/// The binary-resolution settlement source shape (see [`EngineParams::resolution`]):
/// `(symbol, ts_ms) -> Some(payout)` once the market is resolved, `None` while it trades.
/// `+ Send` (beyond the minimal closure shape) because callers move whole `EngineParams` onto
/// worker threads — the same rule as [`PositionSizer`]'s `Send` supertrait.
pub type ResolutionSource = Box<dyn Fn(&str, i64) -> Option<f64> + Send>;

/// The option-expiry settlement source shape (see [`EngineParams::option_specs`]):
/// `symbol -> Some(OptionSpec)` for a symbol that is an option, `None` for anything else (the
/// underlying, a non-option instrument). Unlike [`ResolutionSource`] it takes no timestamp — an
/// option's strike/expiry/right/underlying do not vary over a run. `+ Send` for the same reason as
/// [`ResolutionSource`]: callers move whole `EngineParams` onto worker threads.
///
/// The spec is the ONE option description, [`OptionSpec`] — the same value a catalogued
/// `vike_model::Contract::Option` row carries (decision 0126; this crate no longer keeps a second
/// type of that name). Settlement reads four of its fields: `strike`, `expiry_ms` (epoch ms, the
/// engine's own timestamp unit), `right`, and `underlying` — the REGISTERED symbol whose engine mark
/// supplies the settle price. An `underlying` of `None` cannot settle and is reported once per
/// symbol (`crates/vike-sim/src/engine/settlement.rs`'s module doc carries the rule). `style`, `margining`, `settle`
/// and `value_ccy` are not read: every expiry here is a cash settlement at intrinsic value. The
/// per-contract `contract_size` is deliberately NOT read from the spec either — it is the option
/// symbol's own engine multiplier ([`EngineParams::multipliers`]), applied by the shared settlement
/// fold exactly as it was on entry/marking, so the payoff `max(0, u − K)·contract_size·qty` needs no
/// second scaling knob (and cannot double-apply one). See [`SimBroker::option_payout`].
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
    /// `Some` OVERRIDES that whole chain: [`crate::StrategyEngine::new`] derives the flat
    /// `(maker, taker)` FRACTIONS from the schedule via `maker_taker_rates()` and uses them
    /// exactly where `maker_fee.unwrap_or(fee_rate)` / `taker_fee.unwrap_or(fee_rate)` flowed —
    /// no per-fill branch, so the hot fold is unchanged either way.
    ///
    /// **Which shapes flatten is now a CLOSED list, and the other direction is the default.**
    /// [`crate::StrategyEngine::new`] names exactly two shapes as flattenable —
    /// [`FeeSchedule::PercentMakerTaker`] and [`FeeSchedule::Free`], where a flat fraction of
    /// notional IS the whole shape and `commission` computes the same expression in the same
    /// associativity as `broker_sim::fee` — and carries EVERY other shape to the fill site
    /// verbatim, applying it through `FeeSchedule::commission` at the price the fill transacted
    /// at (see [`SimBroker::fee_curve`]).
    ///
    /// ⚠ **It read the other way round, and the inversion is a safety property.** Only
    /// [`FeeSchedule::ProbabilityScaled`] was named as exact — on the true observation that
    /// `maker_taker_rates()` answers `(0.0, 0.0)` for it, so flattening would charge ZERO while
    /// looking configured (the prediction-market misreport port backlog G7 calls out). But that
    /// method answers `(0.0, 0.0)` for [`FeeSchedule::PerShareWithFloor`] as well, so the IBKR
    /// equities shape — a per-share fee, a per-order MINIMUM and a notional cap — was the SAME
    /// bug wearing an equities hat, and it became reachable from a profile the moment
    /// `vike_backtest::harness::profile`'s `FeeCfg` learned to name that shape. Naming the FLAT side
    /// means a shape added to `FeeSchedule` later defaults to being charged exactly rather than
    /// to being charged nothing.
    ///
    /// `POLYMARKET_PROB_CURVE`'s all-zero rates still charge zero, so every existing consumer of
    /// that constant is numerically unchanged; so is every `PercentOfUnderlying` configuration
    /// whose premium cap does not bind (which is every real Deribit row — `bps/1e4 ≪ cap`, so
    /// `commission`'s `min` is a no-op and the surviving product is `fee`'s own expression).
    ///
    /// ⚠ The exact path multiplies the schedule's answer by the symbol's CONTRACT MULTIPLIER,
    /// which is right for a rate-shaped or notional-capped term and WRONG for a currency-shaped
    /// one: a `PerShareWithFloor` `$1.00` minimum is one dollar, not one dollar per contract. It
    /// is exact at the default `multiplier = 1.0`, which is what an equities book runs at.
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
    /// (`vike_model::money::liquidation`): the LEAN cross-pool workflow — the same
    /// `pool_breached`/`cross_liquidation_plan` the live watchdog runs — meaning a margined
    /// backtest now gets PARTIAL losers-first liquidation (with the LEAN `liq_buffer` grace
    /// line) instead of a total wipe. Comparable engines likewise ship their venue-style stress model
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
    /// WHEN the cross-section is decided — see [`DecideMode`], which carries the whole argument.
    /// [`DecideMode::Sequential`] (the default) is byte-identical to the engine before this field
    /// existed. [`DecideMode::Simultaneous`] IMPLIES [`Self::cash_gate`]: the engine reads
    /// `cash_gate || decide.is_simultaneous()` wherever it used to read `cash_gate` alone, so
    /// selecting it routes the bar step through the shared-cash admission phase (and disables the
    /// granular sub-bar lane) without the caller also having to set the older flag.
    ///
    /// ⚠ BAR MODE ONLY, for the same reason `cash_gate` is: the allocator it orders lives in
    /// `StrategyEngine::fill_step_gated`, which only `StrategyEngine::run` calls.
    /// `StrategyEngine::run_ticks` never consults either field, so a tick profile asking for it
    /// would change nothing — `BacktestProfile::refusals` refuses that pair rather than accepting
    /// it silently.
    pub decide: DecideMode,
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
    #[expect(clippy::type_complexity)]
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
    #[expect(clippy::type_complexity)]
    pub funding_source: Option<Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>>,
    /// Opt-in queue-position model for resting LIMIT fills on the tick/book replay path
    /// (`run_ticks` only; the bar path never consults it). `None` (default) keeps the frozen
    /// "price touched = filled" crossing model byte-identical. See [`crate::QueueModelKind`].
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
    /// Opt-in **market-impact slippage model** (see [`crate::ImpactModel`]). `None` (default) keeps
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
    /// [`SimBroker::dropped`] under `"latency_reject"`. See [`crate::LatencyModelKind`].
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
    /// option's `expiry_ms` any held position is closed to CASH at its intrinsic value — CALL
    /// `max(0, underlying − strike)`, PUT `max(0, strike − underlying)`, per contract — by the same
    /// fee-free, slippage-free, grid-free settlement fill the resolution lane uses
    /// ([`SimBroker::settle_at_payout`]), recorded in [`SimBroker::settlements`] and delivered to the
    /// strategy via `on_fill`. `symbol` is the UNQUALIFIED registration key ([`SimBroker::symbols`]).
    ///
    /// The underlying settle price is the engine's own mark for the option's `underlying` symbol
    /// (`SimBroker::price_of` — the same last-seen-price field `equity_now` marks against), so the
    /// underlying must be a REGISTERED symbol; an option whose underlying is not registered is not
    /// settled (its position is left held), and neither is one whose spec names no underlying at
    /// all (`underlying: None` — that one is also reported once, with a `tracing` warning). The
    /// check runs BEFORE each step's fill phase, so the
    /// underlying mark it reads is the previous step's close (a no-look-ahead lag; hold the
    /// underlying flat near expiry for an exact intrinsic). `contract_size` is the option symbol's
    /// own multiplier ([`EngineParams::multipliers`]) — see [`OptionExpirySource`].
    ///
    /// Expiry is TERMINAL, latched per symbol through the SHARED [`SimBroker::is_resolved`] latch
    /// (an expired option is "resolved"): from the settling step onward the engine refuses every
    /// further order for that option on both lanes, so a strategy that re-enters from inside
    /// `on_fill` cannot re-open an expired contract. The vectorized kernel has no expiry concept, so
    /// engine↔kernel reconciliation is only meaningful with `option_specs: None`.
    ///
    /// The spec the source answers is [`OptionSpec`], the catalog's own `Contract::Option` payload
    /// (see [`OptionExpirySource`]), so a caller holding catalogued instruments hands their specs
    /// over as they are — no projection type in between.
    pub option_specs: Option<OptionExpirySource>,
    /// As-of timestamp for the END-OF-RUN option-expiry sweep (only read when `option_specs` is
    /// `Some`). A tape may stop before an option's `expiry_ms`; the in-loop checks only ever probe
    /// at event timestamps, so without a final sweep a position whose expiry falls just past the
    /// last event would never settle. After the event loop both `run` and `run_ticks` therefore
    /// settle any still-open option whose `expiry_ms` is at/before this as-of, defaulting to each
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
    /// Caveats: the basis reset means a later closing [`vike_model::Trade`] records the last
    /// settlement mark as its `entry_price` (its `pnl` is the final leg only — the SUM over trades
    /// plus the settled amounts is the total), and MAE/MFE stay measured against the ORIGINAL
    /// entry excursion window. The vectorized kernel has no settlement concept, so
    /// engine↔kernel reconciliation is only meaningful with `settlement_period_ms: None`.
    pub settlement_period_ms: Option<i64>,
    /// Opt-in stale-price wait discipline (LEAN `FutureFillModel` analog — see
    /// [`vike_fills::staleness`] for the full contract). `None` (default) leaves EVERY fill lane
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
    /// [`vike_model::venues::session`]). `false` (the default) leaves every fill lane byte-identical: no
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
    /// A FLOOR on the warm-up gate, in STEPS — the door a caller (today the harness's
    /// `data.warmup` key) uses to raise the `index >= warmup` dispatch gate above what the
    /// strategy's own [`vike_model::Strategy::warmup`] declares. `None` (the default) is
    /// byte-identical to before this field existed: the gate is exactly `strategy.warmup()`.
    ///
    /// ⚠ **`max`, never replace, and never downwards.** [`crate::StrategyEngine::new`] resolves
    /// the effective number ONCE as `warmup.unwrap_or(0).max(strategy.warmup())` and both
    /// `run` and `run_ticks` gate on that; [`vike_analytics::BacktestResult::warmup`] reports it, so
    /// `vike_analytics::zero_trade`'s `warmup-shortfall` cause diagnoses a warm-up that
    /// swallowed the whole slice whichever side asked for it. A caller passing a SMALLER number
    /// than the strategy declares changes nothing, deliberately: `warmup()` is the strategy's
    /// statement about its own indicators, and letting an outside number lower it would let a
    /// run trade on indicators that had not converged — the failure this gate exists to prevent.
    pub warmup: Option<usize>,
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
            decide: DecideMode::Sequential,
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
            warmup: None,
        }
    }
}
