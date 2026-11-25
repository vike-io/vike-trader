//! `StrategyEngineConfig`, every knob of one run with its default, plus the settlement-source shapes.

use std::sync::Arc;

use indexmap::IndexMap;
use vike_marketdata::{Bar, FundingRatePoint};
use vike_model::RiskLimits;
use vike_model::{ContractOption, ContractValue, FeeSchedule, SymbolProperties};
#[cfg(doc)]
use vike_model::{OrderBroker, OrderKind, OrderResting};
use vike_venues::SessionCalendar;

use vike_portfolio::sizing::PositionSizer;

use super::conversion::ConversionSeries;
use super::in_bar::InBarRules;
use super::inputs::Inputs;
use super::knobs::{EquitySampling, FillModelKind, ShortfallCover};
#[cfg(doc)]
use super::state::{OrderBrokerSimulator, OrderBrokerSimulatorSymbolState, SettlementFill};
use vike_fills::market_impact::DEFAULT_IMPACT_WINDOW;
use vike_model::run_request::decide::DecideMode;

/// The binary-resolution settlement source shape (see [`StrategyEngineConfig::resolution`]):
/// `(symbol, timestamp_ms) -> Some(payout)` once the market is resolved, `None` while it trades.
/// `+ Send` (beyond the minimal closure shape) because callers move whole `StrategyEngineConfig` onto
/// worker threads — the same rule as [`PositionSizer`]'s `Send` supertrait.
pub type ResolutionSource = Box<dyn Fn(&str, i64) -> Option<f64> + Send>;

/// The option-expiry settlement source shape (see [`StrategyEngineConfig::option_specs`]):
/// `symbol -> Some(ContractOption)` for a symbol that is an option, `None` for anything else (the
/// underlying, a non-option instrument). Unlike [`ResolutionSource`] it takes no timestamp — an
/// option's strike/expiry/right/underlying do not vary over a run. `+ Send` for the same reason as
/// [`ResolutionSource`]: callers move whole `StrategyEngineConfig` onto worker threads.
///
/// The spec is the ONE option description, [`ContractOption`] — the same value a catalogued
/// `vike_model::Contract::Option` row carries (decision 0126; this crate no longer keeps a second
/// type of that name). Settlement reads four of its fields: `strike`, `expiry_ms` (epoch ms, the
/// engine's own timestamp unit), `right`, and `underlying` — the REGISTERED symbol whose engine mark
/// supplies the settle price. An `underlying` of `None` cannot settle and is reported once per
/// symbol (`crates/vike-simulator/src/engine/settlement.rs`'s module doc carries the rule). `style`, `margining`, `settle`
/// and `value_currency` are not read: every expiry here is a cash settlement at intrinsic value. The
/// per-contract `contract_size` is deliberately NOT read from the spec either — it is the option
/// symbol's own engine multiplier ([`StrategyEngineConfig::multipliers`]), applied by the shared settlement
/// fold exactly as it was on entry/marking, so the payoff `max(0, u − K)·contract_size·qty` needs no
/// second scaling knob (and cannot double-apply one). See [`OrderBrokerSimulator::option_payout`].
pub type OptionExpirySource = Box<dyn Fn(&str) -> Option<ContractOption> + Send>;

pub struct StrategyEngineConfig {
    pub fee_rate: f64,
    pub cash: f64,
    /// The currency [`Self::cash`] and every other money number of this run are written in and
    /// booked in; `None` (the default) is the one dollar ledger every run had before this field.
    ///
    /// ⚠ **A LABEL, and the engine never reads it.** The simulator books `price * qty * multiplier`
    /// in whatever unit its series is quoted in: over ONE quote currency its numbers are exactly
    /// amounts of that currency, which is all this names. A symbol quoted in another currency is
    /// converted only by [`Self::conversions`], which carries its rates. It rides here so the
    /// harness's door guards (`vike_backtest`'s `refuse_foreign_quoted_symbols`, the tick replay)
    /// read the SAME ledger the profile stated. Setting it converts nothing.
    pub account_currency: Option<String>,
    pub slippage: f64,
    /// Opt-in **fixed slippage**: a PRICE amount in the series' own quote units, not a fraction, added
    /// against the trader (buys up, sells down) on every fill that no limit bounds: market and
    /// market-close, stop and trailing, a take-profit with no limit, the protective stop, and the
    /// engine's own forced closes. A limit-bounded fill (a limit, resting or tagged, a stop-limit and
    /// a take-profit with a limit, `vike_model::OrderResting::is_limit_bounded`) pays none of it on
    /// any lane. `0.0` (the default) performs no
    /// arithmetic at all, so every frozen path is byte-identical ([`vike_fills::broker_sim::adverse_fill_amount`]).
    ///
    /// It stacks on [`Self::slippage`] rather than replacing it: the fraction prices first
    /// ([`vike_fills::broker_sim::adverse_fill_price`], untouched), then this is added, then the PIT
    /// grid rounds. The cash pre-check of the gated lane adds it too, so it judges the price the fill
    /// is charged. Settlements pay nothing, as they pay no [`Self::slippage`]. Not read by the
    /// vectorized kernel ([`crate::VectorBacktestEngine`]) or the paper exchange, which take their
    /// slippage from elsewhere.
    pub slippage_amount: f64,
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
    /// associativity as `ContractValue::fee` — and carries EVERY other shape to the fill site
    /// verbatim, applying it through `FeeSchedule::commission` at the price the fill transacted
    /// at (see [`OrderBrokerSimulator::fee_curve`]).
    ///
    /// ⚠ **It read the other way round, and the inversion is a safety property.** Only
    /// [`FeeSchedule::ProbabilityScaled`] was named as exact — on the true observation that
    /// `maker_taker_rates()` answers `(0.0, 0.0)` for it, so flattening would charge ZERO while
    /// looking configured (the prediction-market misreport port backlog G7 calls out). But that
    /// method answers `(0.0, 0.0)` for [`FeeSchedule::PerShareWithFloor`] as well, so the IBKR
    /// equities shape — a per-share fee, a per-order MINIMUM and a notional cap — was the SAME
    /// bug wearing an equities hat, and it is reachable from a backtest (a strategy's `per_contract` or
    /// `per_order` commission, `vike_backtest::run::costs`). Naming the FLAT side
    /// means a shape added to `FeeSchedule` later defaults to being charged exactly rather than
    /// to being charged nothing.
    ///
    /// `POLYMARKET_PROB_CURVE`'s all-zero rates still charge zero, so every existing consumer of
    /// that constant is numerically unchanged; so is every `PercentOfUnderlying` configuration
    /// whose premium cap does not bind (which is every real Deribit row — `bps/1e4 ≪ cap`, so
    /// `commission`'s `min` is a no-op and the surviving product is `fee`'s own expression).
    ///
    /// The exact path scales a schedule's RATE terms by the symbol's contract value and leaves its
    /// currency terms alone (`FeeSchedule::commission_on`): a `PerShareWithFloor` `$1.00` minimum is
    /// one dollar per order and its per-share fee is per contract, whatever the multiplier.
    pub fee_schedule: Option<FeeSchedule>,
    /// `(option, underlying)`, both REGISTERED symbols of the run: under a
    /// [`FeeSchedule::PercentOfUnderlying`] [`Self::fee_schedule`], a fill of `option` is charged
    /// [`FeeSchedule::commission_with_underlying`] at the underlying's engine mark (its last price, the
    /// one option settlement reads), deribit's `min(bps × underlying, cap × premium)`. A symbol not
    /// listed, or any other schedule, keeps `FeeSchedule::commission`, which charges the bps on the
    /// PREMIUM and understates an option's fee by the underlying-to-premium ratio. EMPTY (the default)
    /// is byte-identical.
    pub fee_underlyings: Vec<(String, String)>,
    /// `(symbol, schedule)`: the schedule a fill of `symbol` is charged at EXACTLY
    /// (`FeeSchedule::commission`, or [`FeeSchedule::commission_with_underlying`] for a listed
    /// option), over [`Self::fee_schedule`], so each symbol of a run that straddles a venue's fee
    /// lanes pays its own row (binance spot beside its perpetual, a deribit option beside its
    /// perpetual). A symbol not listed keeps the run-wide schedule. EMPTY (the default) is
    /// byte-identical.
    pub fee_schedules: Vec<(String, FeeSchedule)>,
    /// `(option, schedule)`: the DELIVERY fee an expiry settlement of `option` (an option of
    /// [`Self::option_specs`]) pays, [`FeeSchedule::commission_with_underlying`] on the settled quantity
    /// at the intrinsic payout and the underlying's mark that settlement read (deribit's
    /// `min(bps × underlying, cap × the option's value)`, the venue's table
    /// `vike_venues::option_delivery_fee_for`), charged as an option's trading fee is: on a linear contract
    /// (`ContractValue::linear_charge`), priced in the quote currency and booked in the ledger's money. An
    /// option not listed, an exercise and every binary resolution settle fee-free. EMPTY (the default)
    /// is byte-identical.
    pub delivery_fees: Vec<(String, FeeSchedule)>,
    pub multiplier: f64,
    pub multipliers: Vec<(String, f64)>,
    /// Per-symbol contract values that are not the linear [`Self::multiplier`] /
    /// [`Self::multipliers`] one (decision 0170): a symbol listed here is booked by its
    /// [`ContractValue`], and every other symbol is `ContractValue::linear` of its multiplier, which
    /// is what every run booked before this field (EMPTY, the default, is byte-identical).
    ///
    /// An INVERSE value makes the run a COIN LEDGER (decision 0258): [`Self::cash`], the balance, the
    /// trades' PnL, the fees and the funding are amounts of the contract's settle coin, through
    /// [`ContractValue::cash_leg`], [`ContractValue::in_settle`] and the `vike_fills::broker_sim`
    /// arms, and it is sized and gated in dollars at the contract's own price. The engine names no
    /// coin and holds no rate: the caller (the harness) refuses a run whose symbols do not all settle
    /// in ONE coin.
    pub contract_values: Vec<(String, ContractValue)>,
    /// The INVERSE symbols whose series counts its volume in the settle COIN (deribit's candles),
    /// where their orders count contracts. The volume cap ([`Self::volume_limit`]) and the impact
    /// model ([`Self::impact`]) compare such a volume with the coin an order is worth at the fill
    /// price (`ContractValue::in_settle` of its `order_notional`). Every other symbol's volume counts
    /// its order's own unit (a linear base, an inverse venue's contracts) and is compared with the
    /// size, as before. A linear symbol listed here is ignored; EMPTY, the default, is byte-identical.
    /// The engine names no venue: the caller (the harness) states which series these are.
    pub coin_volume: Vec<String>,
    /// The symbols traded on a SPOT (cash) account, the live engine's `vike_model::MarginMode::Cash`
    /// rows: a SELL on one never sells more than the long holds less the sells already out, refused at
    /// the submit in the live engine's own words (`vike_execution::risk::cash_oversell_refusal`, the ONE
    /// rule; `OrderBrokerSimulator::cash_refusal`). EMPTY, the default, is every symbol on margin and
    /// byte-identical. The engine names no venue: the caller (the harness) states which these are.
    pub cash_symbols: Vec<String>,
    pub leverage: Option<f64>,
    /// Outcome SHAPE when an opening/adding market order exceeds the margin budget (only read
    /// while a margin gate is armed — `leverage` or [`StrategyEngineConfig::risk_limits`]; see
    /// [`OrderBrokerSimulator::gate_order`]).
    ///
    /// **`false` (default) = DENY like live, through the LIVE GATE ITSELF**: since deny-vs-clamp
    /// PHASE 2 the order crosses the literal `vike_execution::RiskGate::check` — one formula, one code
    /// path — and a refusal is recorded WHOLE (zero fill) in [`OrderBrokerSimulator::dropped`] under the
    /// gate's OWN reason string (`"insufficient-margin"` etc.; the phase-1
    /// `"insufficient-leverage-room"` string is gone — a backtest denial now IS the live denial,
    /// same reason). `true` = the pre-phase-1 escape hatch, surviving phase 2 verbatim:
    /// [`OrderBrokerSimulator::cap_to_leverage`] silently TRUNCATES the order to fit `leverage * equity`
    /// and executes the shrunken size — the historical behavior, byte-identical; the `RiskGate`
    /// is NOT consulted on this path (the escape hatch restores the whole pre-phase-2 admission
    /// pipeline, so [`StrategyEngineConfig::risk_limits`] is inert while this knob is on).
    pub clamp_to_leverage: bool,
    /// Deny-vs-clamp PHASE 2 — the ONE-JUDGE config. `Some` mounts the literal live
    /// `vike_execution::RiskGate` on the market-order submit path ([`OrderBrokerSimulator::gate_order`])
    /// with exactly these limits, so EVERY live knob (min floors, notional caps, exposure cap,
    /// buying power with `required_free_buying_power_percent`/`closing_credit`, and any future check added to
    /// the gate) is backtest-effective automatically, with the live reason strings.
    ///
    /// `None` (default) + `leverage: Some(L)` still arms the gate, with the CONVENIENCE MAPPING
    /// `initial_margin_requirement = 1/L` (LEAN's own leverage↔initial-margin identity — see
    /// [`OrderBrokerSimulator::build_risk_gate`] for the equivalence proof and its limits). `None` + no
    /// `leverage` = no gate is ever constructed and the submit path is byte-identical to the
    /// pre-phase-2 engine — the big compat pin for non-margined backtests.
    ///
    /// Two fields are overridden at mount, deliberately (documented on
    /// [`OrderBrokerSimulator::build_risk_gate`]): the throttle is DISARMED (wall-clock, meaningless in sim
    /// time) and, when a [`StrategyEngineConfig::properties`] source is configured, the four
    /// instrument-grid fields (`tick_size`/`lot_size`/`min_qty`/`min_notional`) are refreshed
    /// per check from the point-in-time grid — the venue's own time-varying facts, exactly what
    /// live's `RiskLimits::from_properties` mount does once at fetch time.
    pub risk_limits: Option<RiskLimits>,
    /// **The run's MOUNT leverage cap** (`vike_execution::MountBudget::max_leverage`, the live
    /// mount's): the run is its one mount, so an OPENING order whose whole simulated account's gross
    /// notional after it (the order netted into its symbol's position) passes this many times the
    /// sizing equity is refused WHOLE, recorded in [`OrderBrokerSimulator::dropped`] in the live
    /// drain's words, by the ONE judge (`vike_execution::risk::judge_mount_leverage`, each verb alone)
    /// first in [`OrderBrokerSimulator::gate_order`], before the engine's refusals and the `RiskGate`. A
    /// covered reduce and a size-only decrease are not judged; an amend that grows an order is judged on its
    /// growth, and an amend that names a price at its new price on what it will rest. `None` (the default), or a figure that is not finite
    /// and `>= 1.0`, is OFF and byte-identical. It is no `RiskLimits` field, and it judges whether or not
    /// a gate is armed.
    pub max_mount_leverage: Option<f64>,
    pub maint_margin: f64,
    /// **Opt-in stress knob** (default `false`): restore the pre-law venue-style liquidation —
    /// trigger `eq_adv ≤ maint_margin · notional_adv` at the intrabar ADVERSE marks and
    /// force-close the WHOLE account (bar path) / the triggering symbol in full (tick path),
    /// byte-identical to the retired default. `false` runs the shared scope-parameterized law
    /// (`vike_model::money::liquidation`): the LEAN cross-pool workflow — the same
    /// `pool_breached`/`cross_liquidation_plan` the live watchdog runs — meaning a margined
    /// backtest now gets PARTIAL losers-first liquidation (with the LEAN `liquidation_buffer` grace
    /// line) instead of a total wipe. Comparable engines likewise ship their venue-style stress model
    /// off by default. Inert when `maint_margin` is `0.0` (margin off — the default).
    pub venue_style_liquidation: bool,
    /// The LEAN over-the-line buffer for the DEFAULT (shared-law) liquidation path: liquidate
    /// only when `maintenance > pool_equity · (1 + liquidation_buffer)` on top of the
    /// `pool_equity ≤ maintenance` law. Default `0.10` — the same
    /// `vike_model::MarginCallConfig::default().buffer` the live watchdog runs, so a default
    /// backtest and the live sweep judge breaches by the SAME two-condition rule. Set `0.0`
    /// for a bufferless venue-style trigger line. Not read when `venue_style_liquidation`.
    pub liquidation_buffer: f64,
    /// Opt-in **shortfall-cover margin model** (see [`ShortfallCover`], which carries the rule).
    /// `None` (the default) leaves the bar path's margin call exactly as `maint_margin` and the two
    /// fields above make it, and gates no fill. `Some` REPLACES that margin call on the bar path,
    /// judging the account against the rule's own margin rates (`maint_margin`,
    /// `venue_style_liquidation` and `liquidation_buffer` are not read), and refuses a fill the account
    /// cannot margin. Bar path only: `StrategyEngine::run_ticks` refuses a run that sets it.
    pub shortfall_cover: Option<ShortfallCover>,
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
    /// would change nothing — `ProfileBacktest::refusals` refuses that pair rather than accepting
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
    /// The in-bar fill rules the bar lane applies ([`InBarRules`]: how one bar's walk meets the
    /// resting book), every one off by default, byte-identical then. Bar path only:
    /// `StrategyEngine::run_ticks` reads none of them.
    pub in_bar: InBarRules,
    pub default_venue: Option<String>,
    pub fill_model: FillModelKind,
    /// enable the ledger mirror collection (the `_on_fill`/`_on_funding` twins)
    pub mirror: bool,
    /// Closed-trade accounting (`vike_trade_ledger`). Every run folds every fill,
    /// settlements included, into a `TradeClosedLedger` beside the net-position fold: what the strategy
    /// reads as its closed trades (`vike_model::OrderBroker::closed_trades`). `Some` states the ledger's
    /// options, marks it with every bar's walk and close on the bar path, and the result carries its
    /// statistics (`TradingRecord::closed_trades`), the open lots marked at each symbol's last price,
    /// against [`Self::cash`] and the terms' risk-free rate. `None` (the default) books by the default
    /// options, marks nothing and reports nothing, so every result is byte-identical. A tick run marks
    /// no bar: its trades' excursions are their entry and exit prices and its close-to-close and
    /// monthly figures are zero.
    pub closed_trades: Option<vike_trade_ledger::TradeClosedTerms>,
    /// Optional point-in-time instrument-filter lookup `(venue, symbol, timestamp_ms) -> grid`. `None`
    /// (default) leaves the fill path byte-identical. A harness builds this over
    /// `HistoryStore::properties_as_of`; the engine takes a plain closure (no HistoryStore dep).
    #[expect(clippy::type_complexity)]
    pub properties: Option<Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync>>,
    /// `(symbol, tick)`: the tick of each symbol's CURRENT kept instrument row, which the harness reads
    /// off the datahub's kept list (`<project>/instruments/<venue>.json`). It is the third answer to the
    /// tick a STRATEGY reads (`vike_model::OrderBroker::instrument_grid`, the owner's ruling of
    /// 2026-11-19): the tick [`Self::risk_limits`] states wins, then the dated record (of
    /// [`Self::properties`] at `now`, else of [`Self::dated_ticks`]), then this, then none. The TICK
    /// alone: no lot, minimum or notional is read from the row, and the pre-trade gate, the fills and a
    /// trigger's grid never read it, so nothing is snapped or rounded that was not before. When a grid a
    /// strategy read carried it (any field read: the engine cannot tell which), the run's data warnings
    /// say so once per symbol. A tick that is not a positive number is none. EMPTY, the default, is
    /// byte-identical.
    pub kept_ticks: Vec<(String, f64)>,
    /// `(symbol, timestamp_ms) -> Some(tick)`: the tick of the dated properties record of `symbol` (the run's
    /// slot name) in force at `timestamp_ms` (`None`: no record; `Some(0.0)`: a record that states no tick).
    /// The second answer to the tick a STRATEGY reads where [`Self::properties`] is `None` (a run that does
    /// not snap; the owner's precedence of 2026-11-19, [`Self::kept_ticks`]): asked once per symbol per UTC
    /// day, at the day's start, so a record observed during a day answers from the next one. The gate, the
    /// fills, a trigger and the emulated orders never read it. A source that cannot read latches its own
    /// error for the harness. `None` (the default) reads nothing.
    #[expect(clippy::type_complexity)]
    pub dated_ticks: Option<Arc<dyn Fn(&str, i64) -> Option<f64> + Send + Sync>>,
    /// Opt-in **perp funding-rate source** `(venue, symbol, timestamp_ms) -> Some(rate)` — the offline
    /// twin of live's unconditional `vike_execution::Account::apply_funding`. `None` (the default)
    /// leaves the funding path byte-identical: a held perp is charged funding ONLY on the steps
    /// whose [`Bar::funding`] is `Some`, and no production data path sets that field, so a
    /// perp-carry backtest charges ZERO funding (PnL optimistic).
    ///
    /// When `Some`, each bar step consults the source for a symbol whose `Bar.funding` is `None`
    /// (a recorded `Bar.funding` always WINS — it is the on-the-bar truth) and, if it returns
    /// `Some(rate)`, charges `value.funding(pos_size, close, rate)` for that step exactly as
    /// the `Bar.funding` path does — same fold order, same mirror emission. The SOURCE decides the
    /// CADENCE: it returns `Some(rate)` only at the venue's funding timestamps (e.g. every 8h) and
    /// `None` between them, mirroring how `Bar.funding` is `Some` only on a funding bar. `venue` is
    /// [`StrategyEngineConfig::default_venue`] and `symbol` is the UNQUALIFIED registration key
    /// ([`OrderBrokerSimulator::symbols`]), matching the [`StrategyEngineConfig::properties`] seam; with no
    /// `default_venue` set the source is never consulted (funding is venue-specific).
    ///
    /// Deliberately NOT consulted by the vectorized kernel ([`crate::VectorBacktestEngine`]),
    /// which already takes funding as a fully-materialized `T×S` matrix from its caller (no closure
    /// seam) — so this is an EVENT-ENGINE-only knob, exactly like `properties`/`resolution`/
    /// `impact`, and engine↔kernel reconciliation stays meaningful only with `funding_source: None`.
    #[expect(clippy::type_complexity)]
    pub funding_source: Option<Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>>,
    /// Opt-in per-symbol **funding schedule**: the venue's own funding instants, each charged ONCE on
    /// the position held AT that instant, at the charge's own mark (else the price of the first
    /// bar or tick at or after it), through `vike_model::ContractValue::funding` (an inverse
    /// contract in its settle coin). One charge is a [`FundingRatePoint`]: at `timestamp_ms` a position
    /// held then is charged `rate` (positive: a long pays), valued at `mark` when the venue
    /// published one (a positive number); its `premium` is not read. Each list is ascending by
    /// `timestamp_ms`. EMPTY (the default) leaves every path byte-identical.
    ///
    /// - **Bar lane**: at the START of each step, BEFORE that step's stops and fills, every charge
    ///   with `timestamp_ms` at or before the bar's `ts` not yet charged is charged at its mark, else at the
    ///   bar's OPEN: the position before the fills is the one held at the instant (an entry filled
    ///   on the bar does not pay, an exit filled on it does), and the open is the price at it.
    /// - **Tick lane** (`run_ticks`): on the symbol's first tick at or after the instant, before that
    ///   tick's fills, at the mark, else that tick's price.
    ///
    /// ⚠ A symbol LISTED here takes its funding from this schedule alone: its [`Bar::funding`] stays
    /// what a strategy reads and charges nothing, and [`Self::funding_source`] is not consulted for
    /// it. An unlisted symbol keeps the frozen bar rule (a `Bar.funding` charged at the close, after
    /// the fills), which the `fixtures/r4/` ledger-mirror suite pins. A charge after the run's last
    /// bar (or the symbol's last tick) is never charged: nothing was held through it in this run.
    /// Each list is shared (`Arc`): every run over one load (a walk-forward's windows) holds the same one.
    pub funding_schedule: Vec<(String, Arc<Vec<FundingRatePoint>>)>,
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
    /// The MAKER LAW for a tagged plain limit (a backtest profile's `[engine.quotes] optimistic`):
    /// `true` fills every limit placed by tag that is neither reduce-only nor triggered
    /// (`vike_model::OrderPlacement::Limit`, a maker's quote included) by
    /// `StrategyEngine::fill_tagged`'s optimistic law: its own pass (before the other resting orders
    /// on a coarse bar, after them on a tick, on neither the granular nor the shared-cash lane), whole
    /// at the touch, outside the volume cap, the fill-time caps, the in-bar rules and the staleness
    /// wait, its end sent as every seam order's is. `false` (the default) fills it by the ordinary limit
    /// law every other resting order fills by. Untagged orders and every other kind keep their law; an
    /// emulated order's child (`crates/vike-simulator/src/engine/sim_broker/emulated.rs`) takes the maker law
    /// on every run. The paper book and the live core read nothing of it.
    pub optimistic_quotes: bool,
    /// Opt-in **market-impact slippage model** (see [`vike_fills::market_impact::ImpactModel`]). `None` (default) keeps
    /// the frozen flat-`slippage` cost byte-identical — the wire-in
    /// ([`OrderBrokerSimulator::slippage_for`]) hands back the `slippage` field with no arithmetic applied.
    /// When `Some`, each TAKER fill's size is priced against a rolling volatility /
    /// average-volume window, and the estimate is ADDED to `slippage`.
    ///
    /// EVERY LANE, NOT JUST THE BAR PATH — but each lane is charged only the terms its own price
    /// law has not already paid ([`vike_fills::market_impact::ImpactTerms`], selected by
    /// [`OrderBrokerSimulator::impact_terms`]): the bar and L1-tick laws are size-INDIFFERENT and pay the
    /// whole model, while a fill priced by walking a replayed L2 book has already paid the
    /// temporary concession out of real displayed depth and is charged the permanent footprint
    /// alone. One model, one coefficient set; the lane picks the terms.
    ///
    /// MAKER fills (`OrderKind::Limit`, including the `tagged` HFT lane) are exempt ON THE BAR
    /// LANE ONLY ([`OrderBrokerSimulator::charges_impact`]) — there a limit fills at-or-better than its own
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
    pub impact: Option<Arc<dyn vike_fills::market_impact::ImpactModel>>,
    /// Lookback for the rolling `sigma`/`average_volume` the `impact` model is priced against; read
    /// only when `impact` is `Some`. Defaults to [`DEFAULT_IMPACT_WINDOW`].
    ///
    /// ⚠ It counts BARS on the bar lane and TRADE PRINTS on the tick lane
    /// ([`crate::impact::ImpactModelTickWindow`]) — the same number, spent in wildly different units. The
    /// default was chosen as one trading month of DAILY bars; on a liquid tape 21 prints is a
    /// fraction of a second, so a tick run should set this deliberately rather than inherit it.
    /// Lengthening it does NOT undo the unit mismatch, only the sampling noise: `sigma` and
    /// `average_volume` stay per-PRINT however many prints they average over, so the coefficient pair
    /// is what rescales the level (`vike_fills::market_impact::ImpactModelAlmgrenChriss::with_coefficients`, reachable
    /// from a strategy's stated slippage table as the `market_impact` model's `gamma` / `eta`).
    pub impact_window: usize,
    /// Opt-in order-latency model for the tick/book replay path (`run_ticks` only; the bar
    /// engine and the vectorized kernel never consult it). `None` (default) keeps the frozen
    /// zero-latency behavior byte-identical — no in-flight queue is built at all.
    ///
    /// When `Some`, every strategy-issued order-path mutation (new order, cancel-all, tagged
    /// submit/modify/cancel) is held in flight and applied at `submit_ts + entry()`, and each
    /// fill is delivered to `Strategy::on_fill` at `exchange_timestamp + response()`. A NEGATIVE entry
    /// latency (a recorded rejection) drops the action outright and records it in
    /// [`OrderBrokerSimulator::dropped`] under `"latency_reject"`. See [`crate::LatencyModelKind`].
    ///
    /// WHAT THE RESPONSE LEG DELAYS, PRECISELY — read this before trusting a latency-armed number:
    ///
    /// - DELAYED: `Strategy::on_fill`, and the strategy-visible shadow inventory that
    ///   [`OrderBroker::position`] returns (see [`OrderBrokerSimulator::shadow_pos`]).
    ///   These are the reads a maker actually steers on — the spread maker file's `SpreadMaker` polls
    ///   `position("")` on every requote for its skew and A-S reservation price.
    /// - NOT DELAYED (exchange truth, synchronous, by design): [`OrderBrokerSimulator::position_of`],
    ///   [`OrderBrokerSimulator::pending_of`], [`OrderBrokerSimulator::equity_now`] / [`OrderBroker::equity`],
    ///   and the fills' effect on cash, the trade log and the equity curve. They are the accounting surface `TradingRecord` is built from. A strategy that
    ///   sizes off `equity()` still reacts to a fill with ZERO response latency.
    /// - INVISIBLE while in flight: a submitted order is in neither `pending_of` nor the position
    ///   until it is delivered. Use [`OrderBrokerSimulator::in_flight_of`] to see it — the naive
    ///   `if pending_of(s).is_empty() { submit(..) }` guard otherwise re-fires every tick of the
    ///   entry-latency window. The pre-trade leverage cap DOES count in-flight market orders.
    pub latency_model: Option<crate::latency::LatencyModelKind>,
    /// Opt-in binary-resolution settlement source `(symbol, timestamp_ms) -> Some(payout)` once the
    /// market is resolved (pm-economics lane; payout `0.0` or `1.0` for a binary outcome token —
    /// any terminal value is honored). `None` (default) leaves every path byte-identical. When
    /// `Some`, each bar step / price tick checks it BEFORE the fill phase: a resolved symbol has
    /// its resting orders canceled (pending + tagged + protective stop — a resolved market can no
    /// longer trade) and any open position closed at the payout price by a fee-free,
    /// slippage-free, grid-free settlement fill, recorded distinctly in
    /// [`OrderBrokerSimulator::settlements`] and delivered to the strategy via `on_fill`. `symbol` is the
    /// UNQUALIFIED registration key (`OrderBrokerSimulator::symbols`), not the venue-tagged `bar.symbol`.
    ///
    /// Resolution is TERMINAL and latched per symbol: from the settling step onward the engine
    /// REFUSES every further order for that symbol on both lanes (`pending` and the tagged maker
    /// lane — see [`OrderBrokerSimulator::is_resolved`]), which is what keeps a strategy that re-enters from
    /// inside `on_fill` from re-opening a market that just settled in this same step.
    ///
    /// Interaction with `mirror`: settlement fills DO reach `mirror_fills` (the ledger must see the
    /// payout or its book never flattens), each right after its [`SettlementFill`] in
    /// [`OrderBrokerSimulator::settlements`], which marks it a payout, not a trade.
    /// The vectorized kernel has no resolution concept, so engine↔kernel mirror reconciliation is
    /// only meaningful with `resolution: None`.
    pub resolution: Option<ResolutionSource>,
    /// As-of timestamp for the END-OF-RUN resolution sweep (only read when `resolution` is
    /// `Some`). Real prediction-market tapes stop at the trading halt and the resolution posts
    /// LATER, so every recorded event has `ts < res_timestamp` and the in-loop checks never see the
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
    pub resolution_end_timestamp: Option<i64>,
    /// Opt-in **option-expiry cash settlement** source `symbol -> Some(ContractOption)` (the options
    /// lane — the analog of the binary [`StrategyEngineConfig::resolution`] lane, and it settles through the
    /// SAME machinery). `None` (default) leaves every path byte-identical. When `Some`, at/after an
    /// option's `expiry_ms` any held position is closed to CASH at its intrinsic value — CALL
    /// `max(0, underlying − strike)`, PUT `max(0, strike − underlying)`, per contract — by the same
    /// slippage-free, grid-free settlement fill the resolution lane uses, fee-free unless
    /// [`StrategyEngineConfig::delivery_fees`] lists the option
    /// ([`OrderBrokerSimulator::settle_at_payout`]), recorded in [`OrderBrokerSimulator::settlements`] and delivered to the
    /// strategy via `on_fill`. `symbol` is the UNQUALIFIED registration key ([`OrderBrokerSimulator::symbols`]).
    ///
    /// The underlying settle price is the engine's own mark for the option's `underlying` symbol
    /// (`OrderBrokerSimulator::price_of` — the same last-seen-price field `equity_now` marks against), so the
    /// underlying must be a REGISTERED symbol; an option whose underlying is not registered is not
    /// settled (its position is left held), and neither is one whose spec names no underlying at
    /// all (`underlying: None` — that one is also reported once, with a `tracing` warning). The
    /// check runs BEFORE each step's fill phase, so the
    /// underlying mark it reads is the previous step's close (a no-look-ahead lag; hold the
    /// underlying flat near expiry for an exact intrinsic). On the TICK lane every configured option
    /// is probed at EVERY event, whichever symbol's it is, so an option whose own tape stops before
    /// its expiry settles at the run's first event at or after it, against the underlying's price
    /// from before that event. `contract_size` is the option symbol's
    /// own multiplier ([`StrategyEngineConfig::multipliers`]) — see [`OptionExpirySource`].
    ///
    /// Expiry is TERMINAL, latched per symbol through the SHARED [`OrderBrokerSimulator::is_resolved`] latch
    /// (an expired option is "resolved"): from the settling step onward the engine refuses every
    /// further order for that option on both lanes, so a strategy that re-enters from inside
    /// `on_fill` cannot re-open an expired contract. The vectorized kernel has no expiry concept, so
    /// engine↔kernel reconciliation is only meaningful with `option_specs: None`.
    ///
    /// The spec the source answers is [`ContractOption`], the catalog's own `Contract::Option` payload
    /// (see [`OptionExpirySource`]), so a caller holding catalogued instruments hands their specs
    /// over as they are — no projection type in between.
    pub option_specs: Option<OptionExpirySource>,
    /// As-of timestamp for the END-OF-RUN option-expiry sweep (only read when `option_specs` is
    /// `Some`). A tape may stop before an option's `expiry_ms`; the in-loop checks only ever probe
    /// at event timestamps, so without a final sweep a position whose expiry falls just past the
    /// last event would never settle. After the event loop both `run` and `run_ticks` therefore
    /// settle any still-open option whose `expiry_ms` is at/before this as-of, defaulting to each
    /// symbol's LAST event ts. Set it to a FUTURE ts to force-settle an option expiring after the
    /// tape (hold-to-expiry over a short window — the [`StrategyEngineConfig::resolution_end_timestamp`] analog);
    /// leave it `None` (default) and the sweep only re-touches options that already expired within
    /// the tape — an idempotent no-op that never force-settles a still-live option.
    pub option_expiry_end_timestamp: Option<i64>,
    /// Opt-in **variation-margin settlement** cadence in milliseconds (`86_400_000` = daily; the
    /// LEAN `FutureSettlementModel` analog). `None` (the default) — and any non-positive value —
    /// leaves every path byte-identical: the settlement hook returns before touching any state.
    ///
    /// When `Some(period)`, the engine buckets each step's timestamp (`ts.div_euclid(period)`) and,
    /// on the FIRST step of a new bucket, settles every open position: the daily profit/loss
    /// `size * (mark - average_price) * multiplier` (mark = that step's last seen price) is booked into
    /// `OrderBrokerSimulatorSymbolState::realized` and accumulated into [`OrderBrokerSimulatorSymbolState::settled_profit`], and the
    /// position's cost basis is reset to the mark — unrealized becomes realized WITHOUT closing the
    /// position. Each settlement is recorded in [`OrderBrokerSimulator::variation_settlements`].
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
    /// `submit_stop` always arms the core's `OrderEmulationBook` emulator, whose fired conditional
    /// is released as a plain MARKET (`strategy_drive::submit_fired`) and therefore fills at
    /// the NEXT event's price — so on trigger the resting stop CONVERTS to a resting market
    /// child ([`OrderBrokerSimulator::stop_released`]) and fills the next bar's open / next tick's print,
    /// on every fill lane. Protective stops ([`OrderResting::stop`] → `OrderBrokerSimulatorSymbolState::stop`) are
    /// NOT affected: their live twin is the venue-side/paper resting stop, which fills
    /// same-event on both stacks. The default divergence is pinned in `tests/laws/trigger_law.rs`.
    pub emulator_release_stops: bool,
    /// Opt-in **session / market-hours gate** (law-map T10 — the ONE session law, served from
    /// [`vike_venues::session`]). `false` (the default) leaves every fill lane byte-identical: no
    /// per-symbol calendar is built and no fill path consults it (see [`OrderBrokerSimulator::session_closed`]
    /// — an empty session vec is one `Vec::get` returning `None`).
    ///
    /// When `true`, each SYMBOL resolves to its OWN [`vike_venues::SessionCalendar`], and nothing on
    /// that symbol fills on an event whose ts falls outside its session — resting orders, the
    /// tagged maker lane, and protective stops alike, on the frozen fill lanes AND their opt-in
    /// [`StrategyEngineConfig::queue_model`] tick twins (a closed market crosses no resting limit and no
    /// maker quote, queued or not) (see [`OrderBrokerSimulator::session_closed`] for why stops are gated here
    /// but not by the staleness lane). Per-symbol resolution is the fix for the
    /// venue-alone defect: a run mixing an alpaca equity and 24/7 alpaca crypto gates each
    /// correctly, which one `default_venue` per run cannot.
    ///
    /// Each symbol's calendar is chosen in this order:
    /// 1. [`StrategyEngineConfig::session_calendars`]`[symbol]` — the per-symbol override, if present;
    /// 2. else [`vike_venues::session_for`]`(default_venue)` — the venue-ONLY default (correct for a
    ///    single-session venue; fail-permissive always-open for a mixed venue like alpaca/ibkr);
    /// 3. else fail-permissive always-open (gate on, but nothing pins this symbol's session).
    ///
    /// So the (venue, asset-class) keying lives in the CALLER: build `session_calendars` from
    /// `vike_catalog::session_calendar_for(venue, asset_class)` per symbol. Enabling the knob with
    /// neither a `default_venue` nor any override is a silent no-op, not an error — the run's
    /// [`OrderBrokerSimulator::session_deferrals`] staying `0` is how a caller sees it.
    pub session_gate: bool,
    /// Per-symbol session-calendar overrides for [`StrategyEngineConfig::session_gate`]. Empty (the
    /// default) = every symbol falls back to the `default_venue` calendar. This is the **per-symbol
    /// override hook** and the site where (venue, asset-class) keying reaches the engine: resolve
    /// each symbol's calendar with `vike_catalog::session_calendar_for(venue, asset_class)` (or pin
    /// a bespoke one) and insert it under the symbol name. Ignored entirely when `session_gate` is
    /// `false`.
    pub session_calendars: IndexMap<String, SessionCalendar>,
    /// How densely the TICK lane records its equity curve — see [`EquitySampling`]. The default
    /// [`EquitySampling::EveryTick`] is the frozen behaviour, byte-identical. Set it only to trade
    /// curve fidelity for the ~16 bytes/tick the two curve vectors cost on a very long tape. A tick
    /// PROFILE sets [`EquitySampling::ByTime`] unless it names a density (decision 0078's amendment of 2026-11-20).
    pub equity_sampling: EquitySampling,
    /// The stored series the strategy reads beside its own bars, by name (decision 0271), and the
    /// length of the run's own bar: what `vike_model::OrderBroker::input` answers from, as of each event
    /// (`crates/vike-simulator/src/engine/sim_broker/inputs.rs`'s module doc has the rule). Empty (the
    /// default) for every strategy that declares no input; the harness loads and checks the series.
    pub inputs: Inputs,
    /// Whether `vike_model::OrderBroker::input_whole` answers: each input's WHOLE series, the bars still to
    /// come included (decision 0271's amendment of 2026-11-14). The harness sets it for a strategy file
    /// whose header says `runs_on = "backtest"`, and for nothing else. Off (the default): the read answers
    /// `None`, as paper and live do.
    pub input_whole: bool,
    /// The run's symbols that are TRADED LEGS ON ANOTHER VENUE, as `(symbol, venue)` (live: a leg the mount
    /// declared on a venue other than its own, `vike_core::MountLeg::at`). Such a slot fills as any other,
    /// but `StrategyEngine::run_ticks` hands its events to no hook: its touch reaches
    /// `Strategy::on_reference_quote(venue, ..)` as the `TickEvent::Reference` events its caller hands
    /// beside it, each when this box hears it (`TickEvent::heard_at`), not warmup-gated, as the live core
    /// hears a foreign leg. Tick lane only. Empty (the default): every slot is heard as before.
    pub reference_legs: Vec<(String, String)>,
    /// One conversion series per quote currency that is not the run's ledger (decision 0243 phase 2;
    /// `crates/vike-simulator/src/engine/sim_broker/conversion.rs`'s module doc has the rule), each
    /// with the symbols it BOOKS ([`ConversionSeries::symbols`]): such a symbol's fills and fees are
    /// priced in its quote currency and booked at the rate in force (`ConversionSeries::rate_at`'s), its
    /// value being the quote-currency value times that rate (`ContractValue::in_account`), so its PnL,
    /// margin, equity mark and sizing are in the ledger's money; an opening order on it is refused
    /// `quote-rate-stale` while the rate is stale. A LINEAR contract only. Empty (the default) for every
    /// run whose profile names no pair: nothing converts, byte-identical. The harness loads and checks
    /// each series before any price is read, and says which symbols each books.
    pub conversions: Vec<ConversionSeries>,
    /// A FLOOR on the warm-up gate, in STEPS — the door a caller (today the harness's
    /// `data.warmup` key) uses to raise the `index >= warmup` dispatch gate above what the
    /// strategy's own [`vike_model::Strategy::warmup`] declares. `None` (the default) is
    /// byte-identical to before this field existed: the gate is exactly `strategy.warmup()`.
    ///
    /// ⚠ **`max`, never replace, and never downwards.** [`crate::StrategyEngine::new`] resolves
    /// the effective number ONCE as `warmup.unwrap_or(0).max(strategy.warmup())` and both
    /// `run` and `run_ticks` gate on that; [`vike_analytics::TradingRecordSimulator::warmup`] reports it, so
    /// `vike_analytics::zero_trade`'s `warmup-shortfall` cause diagnoses a warm-up that
    /// swallowed the whole slice whichever side asked for it. A caller passing a SMALLER number
    /// than the strategy declares changes nothing, deliberately: `warmup()` is the strategy's
    /// statement about its own indicators, and letting an outside number lower it would let a
    /// run trade on indicators that had not converged — the failure this gate exists to prevent.
    pub warmup: Option<usize>,
}

impl Default for StrategyEngineConfig {
    fn default() -> Self {
        StrategyEngineConfig {
            fee_rate: 0.0,
            cash: vike_model::run_request::profile::DEFAULT_RUN_CASH,
            account_currency: None,
            slippage: 0.0,
            slippage_amount: 0.0,
            maker_fee: None,
            taker_fee: None,
            fee_schedule: None,
            fee_underlyings: Vec::new(),
            fee_schedules: Vec::new(),
            delivery_fees: Vec::new(),
            multiplier: 1.0,
            multipliers: Vec::new(),
            contract_values: Vec::new(),
            coin_volume: Vec::new(),
            cash_symbols: Vec::new(),
            leverage: None,
            clamp_to_leverage: false,
            risk_limits: None,
            max_mount_leverage: None,
            maint_margin: 0.0,
            venue_style_liquidation: false,
            liquidation_buffer: vike_model::MarginCallConfig::default().buffer,
            shortfall_cover: None,
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
            in_bar: InBarRules::default(),
            default_venue: None,
            fill_model: FillModelKind::Bar,
            mirror: false,
            closed_trades: None,
            properties: None,
            kept_ticks: Vec::new(),
            dated_ticks: None,
            funding_source: None,
            funding_schedule: Vec::new(),
            queue_model: None,
            queue_seed_depth: 0.0,
            queue_min_hold_ms: 0,
            optimistic_quotes: false,
            impact: None,
            impact_window: DEFAULT_IMPACT_WINDOW,
            latency_model: None,
            resolution: None,
            resolution_end_timestamp: None,
            option_specs: None,
            option_expiry_end_timestamp: None,
            settlement_period_ms: None,
            max_price_staleness_ms: None,
            emulator_release_stops: false,
            session_gate: false,
            session_calendars: IndexMap::new(),
            equity_sampling: EquitySampling::EveryTick,
            inputs: Inputs::default(),
            input_whole: false,
            reference_legs: Vec::new(),
            conversions: Vec::new(),
            warmup: None,
        }
    }
}
