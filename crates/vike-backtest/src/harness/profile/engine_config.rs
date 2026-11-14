//! The `[engine]` section: `ProfileEngine`, the cost/cash/fill configuration handed to the sim broker.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{
    ProfileBacktest, ProfileEngineFee, ProfileEngineResolution, ProfileEngineSizer,
    ProfileEngineSlippage,
};
use crate::data_plan::DataPlanConversion;
use crate::harness::HarnessError;
use vike_simulator::{EquitySampling, FillModelKind, InBarRules, QueueModelKind, ShortfallCover};

#[cfg(doc)]
use super::decide_mode;

/// Engine cost/cash configuration handed to the sim broker.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileEngine {
    pub cash: f64,
    /// The currency `engine.cash` and every other money value of this profile (`engine.fee.min`,
    /// `engine.fee.per_share`, the risk ceilings) are WRITTEN in, and the report is READ in: a fiat
    /// code such as JPY, in any case. Absent (the default) = the dollar, which is what every
    /// profile written before this key was, byte-identical.
    ///
    /// ⚠ **It converts NOTHING.** The simulator books `price * qty` in whatever unit the series is
    /// quoted in and holds no conversion rate (decision 0183 item 13), so the only ledgers it can
    /// book exactly are the symbols' own quote currency: USDJPY (and EURJPY, GBPJPY, …) on
    /// `"JPY"`, EURGBP on `"GBP"`. [`crate::harness::refuse_foreign_quoted_symbols`] therefore
    /// refuses a run in which any symbol is not quoted in THIS currency (a dollar-quoted one on a
    /// yen ledger included, and a name that states no fiat quote on any ledger but the dollar),
    /// naming the offenders; a run that mixes quote currencies needs a rate series and stays refused.
    ///
    /// It is explicit on purpose. Reading it off the symbols would silently re-read an existing
    /// `cash = 10000` as ten thousand yen. It reaches the report as the realism stamp's
    /// `engine.account_currency`, and a table prints it only when it is not the dollar.
    #[serde(default)]
    pub account_currency: Option<String>,
    /// The CONVERSION PAIR of each quote currency that is not the ledger's (decision 0243 phase 2):
    /// `QUOTE = "<series>"`, the series in the input grammar (`VENUE:SYMBOL:INTERVAL`, or
    /// `LANE:VENUE:SYMBOL:INTERVAL` for a lane such as a CSV import's `user-<label>`; one part,
    /// `INTERVAL`, is the run's own symbol): `JPY = "dukascopy:USDJPY:1h"`,
    /// `USDT = "user-coinbase:coinbase:USDT-USD:1d"`. The pair's SYMBOL says which way its price
    /// points against [`Self::account_currency`]: `QUOTE`+`LEDGER` as it stands, `LEDGER`+`QUOTE` inverted
    /// (`vike_model::AccountPerQuoteLeg::of_pair_name`). Refused at load: a key that is no currency
    /// code, the ledger itself or a currency named twice, a series the grammar does not read, and a
    /// symbol that names neither way round (`resolve_conversions`).
    ///
    /// A quote currency with no row is booked as today, as the ledger's own unit, so a stablecoin
    /// quote is converted ONLY when it has one (the owner's amendment of 2026-11-10), and absent (the
    /// default) is every profile written before this table, byte-identical. Each row is a planned
    /// series of its own (`data.explain` lists it as a `conversion` row), read before any price; a
    /// series the store does not hold, or holds with a hole inside the window, refuses the run by name
    /// (`crate::data_plan::load_conversions`).
    #[serde(default)]
    pub conversion: BTreeMap<String, String>,
    /// The ACCOUNT's margin mode, in `vike_model::MarginMode`'s own words. `"cash"` is a SPOT
    /// account: every symbol the run trades takes the live engine's spot rule
    /// (`vike_simulator::StrategyEngineConfig::cash_symbols`), so a sell past the long less the sells already out
    /// is refused in its words ("a spot account cannot sell more than it holds…") and lands in the
    /// result's `dropped`. `"cross"` WRITTEN OUT is a margin account for every symbol (a sell past
    /// the long opens a short), what every profile written before the spot rule ran. ABSENT reads
    /// the rule off each series' name (the spot rule, owner 2026-11-11: one spot law in live, paper
    /// and backtest): a spot pair takes it, a derivative does not ([`super::cash_symbols`]).
    /// `"isolated"` is refused at load (vike does not run isolated margin yet), as is any other
    /// word ([`super::margin_mode`]).
    #[serde(default)]
    pub margin_mode: Option<String>,
    #[serde(default)]
    pub fee_rate: f64,
    /// The run's slippage: `[engine.slippage]` with ONE `model` (`none`, `percent`, `ticks`,
    /// `market_impact`), the one place a profile states it ([`ProfileEngineSlippage`]'s module doc). Absent
    /// (the default) is no slippage, what every profile without one always ran.
    ///
    /// ⚠ The old scalar `engine.slippage = <fraction>` and the old `[engine.impact]` table are
    /// REFUSED at load with the new spelling in the message, never read
    /// (`super::slippage_config`'s `refuse_old_spelling`). The `market_impact` model prices BOTH lanes,
    /// each charged only what its own price law has not already paid ([`vike_simulator::ImpactTerms`]);
    /// its `window` counts bars in bar mode and trade prints in tick mode.
    #[serde(default)]
    pub slippage: Option<ProfileEngineSlippage>,
    #[serde(default)]
    pub snap_to_properties: bool,
    /// Charge each series the stored market `funding` series (`vike-cli data hist fetch
    /// <venue>:<symbol>:funding` writes it) on the VENUE'S OWN schedule, the stored timestamps, never
    /// a fixed interval: each funding instant is charged once on the position held AT it (the one
    /// from before that bar's fills; on the tick lane, before the first tick at or after it), at the
    /// venue's published mark when stored (binance, deribit's index) else the bar's open or the tick's
    /// price, an inverse contract in its settle coin; deribit's continuous accrual is charged per
    /// minute of its hour (`crate::harness::run::FundingAccrual`). A funding-reading strategy reads
    /// the SUM of the rates due by each bar on its `Bar.funding` ([`crate::harness::window_join_funding`]).
    ///
    /// ⚠ **ABSENT means ON FOR A PERPETUAL** ([`Self::attaches_funding`]): a series whose name is a
    /// perpetual (`crate::harness::run::names_a_perpetual`) is charged whatever funding history the
    /// store holds for it, and a perpetual with none runs uncharged with a report line saying so; any
    /// other series reads nothing. `true` reads it for every series (and names one with no history);
    /// `false` never reads it and says nothing. Both lanes; the lookup keys off each RESOLVED series'
    /// own venue, so a cross-venue `[[data.series]]` slice works too. A hole in the stored series is a
    /// report line (`data_warnings`), never an error.
    #[serde(default)]
    pub attach_funding: Option<bool>,
    /// Opt-in feed-latency replay (see [`crate::tick_replay::TickReplayConfig::feed_latency`]): deliver ticks to
    /// the strategy in recorded ARRIVAL (`local_timestamp`) order instead of venue order, while order
    /// matching stays on venue `ts`. Absent (the default `false`) = today's venue-ordered replay,
    /// byte-identical. TICK MODE ONLY — a bar has no `local_timestamp`, so a profile that sets this with
    /// `data.kind = "bar"` is rejected by [`ProfileBacktest::validate`] rather than silently
    /// ignored.
    #[serde(default)]
    pub feed_latency: bool,
    /// Opt-in ORDER latency (see [`vike_simulator::LatencyModelKind`]): a fixed entry-leg delay in MILLISECONDS on
    /// every strategy order action (place / modify / cancel). The maker's quotes, re-prices and
    /// pulls only reach the matching engine `order_latency_ms` after it decides them, so it cannot
    /// react within one tick — the missing clock skew that otherwise lets a maker enter and exit in
    /// the same instant. `0` (the default) = zero latency, byte-identical. TICK MODE ONLY (only
    /// `run_ticks` arms the latency gate; the bar lane and vector kernel never consult it).
    #[serde(default)]
    pub order_latency_ms: i64,
    /// Opt-in FILL-notification latency (the response leg, see [`vike_simulator::LatencyModelKind`]): a fixed delay in
    /// MILLISECONDS before the strategy LEARNS of a fill. The fill books at the real time (equity/PnL
    /// are exact), but the strategy-visible shadow position ([`vike_model::OrderBroker::position`])
    /// does not advance until `fill_latency_ms` later — so a maker that polls its inventory cannot
    /// react (place its exit) inside that gap. Models the "time to realise you're filled" half of a
    /// real round-trip reaction. `0` (the default) = off, byte-identical. TICK MODE ONLY.
    #[serde(default)]
    pub fill_latency_ms: i64,
    /// Opt-in tick-lane fill-model override: `"l2book"` (the ONE accepted value; case-insensitive,
    /// like [`Self::queue_model`]) selects the depth-capped
    /// [`vike_simulator::FillModelKind::L2Book`] (a resting order fills only up to the DISPLAYED book
    /// depth; when the within-limit depth cannot cover its size it RESTS rather than filling size the
    /// market never showed), vs the default L1 spread-crossing `Tick` model (fills the full size at
    /// the quote). Absent ⇒ `Tick`, byte-identical. Any OTHER value is a fail-fast
    /// [`HarnessError::Validation`] at load ([`ProfileEngine::fill_model_kind`], the mirror of the
    /// [`Self::queue_model`] rule) — a typo like `"l2_book"` must not silently select the
    /// optimistic `Tick` model and undo the depth-cap realism knob. TICK MODE ONLY. Pair with no
    /// `[engine.slippage]` (the book walk IS the slippage).
    #[serde(default)]
    pub fill_model: Option<String>,
    #[serde(default)]
    pub seed_bar_interval_ms: Option<i64>,
    /// Stop-verb release timing (see [`vike_simulator::StrategyEngineConfig::emulator_release_stops`]).
    ///
    /// ⚠ The HARNESS default is `false`, matching [`vike_simulator::StrategyEngineConfig::default()`]: a fired
    /// conditional stop fills SAME-EVENT at the trigger oracle's price, the raw-engine legacy
    /// behaviour (the pinned backtest divergence, law-map A2). This field previously documented a
    /// harness default of `true` ("mirror-live") while reaching the engine through NEITHER
    /// `StrategyEngineConfig` construction site in `crate::harness::run` — both end in
    /// `..Default::default()` — so every harness run was `false` in practice regardless of what a
    /// profile's TOML said or what this doc claimed. The default now agrees with what has always
    /// actually happened, and the field is now wired into BOTH construction sites, so an explicit
    /// `true` really does arm mirror-live release: a fired conditional stop converts to a resting
    /// MARKET child that fills the NEXT event, exactly as the live emulator's `OrderEmulationBook`
    /// does.
    ///
    /// Whether the harness SHOULD default to mirror-live (so a profile run's fill timing matches
    /// what the same order would do against a live venue) rather than the raw-engine default is a
    /// separate, still-open OWNER DECISION — this fix wires the knob and corrects the doc; it
    /// deliberately does not also flip live simulation behaviour as a side effect.
    #[serde(default = "default_emulator_release_stops")]
    pub emulator_release_stops: bool,
    /// Opt-in fee SCHEDULE (port backlog G7) — the shapes a flat [`Self::fee_rate`] cannot
    /// express. Absent with a zero `fee_rate` (the default) = the venue table of each symbol's own
    /// fee lane, as the Studio prices it (a slice over two lanes pays each symbol its own row,
    /// never zero). Setting BOTH
    /// is a validation error: two cost models would be configured and only one could win.
    #[serde(default)]
    pub fee: Option<ProfileEngineFee>,
    /// Opt-in binary-resolution settlement (port backlog G6): builds the
    /// [`vike_simulator::StrategyEngineConfig::resolution`] source + `resolution_end_timestamp` the engine has always
    /// consumed. Absent (the default) = no settlement source, byte-identical.
    #[serde(default)]
    pub resolution: Option<ProfileEngineResolution>,
    /// Opt-in QUEUE-POSITION fill model for the tick lane (see [`vike_simulator::QueueModelKind`]): a resting
    /// limit — INCLUDING a tagged MAKER quote — no longer fills the instant price touches it, but
    /// only once a taker TRADE has consumed the size AHEAD of it in the FIFO queue (seeded from the
    /// replayed L2 book's size at that price, or the last L1 quote, or [`Self::queue_seed_depth`]).
    /// This is the realistic passive-maker fill: a quote earns the spread only when flow actually
    /// hits it, with partial fills on the trade's excess over the front. Absent (the default) = the
    /// frozen simple-crossing fill (`fill_tagged`), byte-identical. TICK MODE ONLY — the queue lane
    /// is consulted exclusively by `run_ticks`, so a bar-mode profile setting it is rejected by
    /// [`ProfileBacktest::validate`] (the mirror of the `feed_latency` rule). Values (case-insensitive):
    /// `"risk_averse"` (the conservative bound — front shrinks only on hard evidence),
    /// `"prob_power"` / `"prob_power:N"` (probabilistic, `f(x)=x^N`, default `N=1`), `"prob_log"`.
    /// For a real maker backtest feed the TRADE tape (and, for true size-ahead seeding, the BOOK).
    #[serde(default)]
    pub queue_model: Option<String>,
    /// Fallback front-of-queue depth (in size units) seeded when neither the replayed L2 book nor an
    /// L1 quote gives a size at a resting order's price — see [`vike_simulator::QueueModelKind`]. Only consulted
    /// when [`Self::queue_model`] is set. Absent ⇒ `0.0` (a quote with no observed size-ahead fills
    /// on the first at-price trade).
    #[serde(default)]
    pub queue_seed_depth: Option<f64>,
    /// Minimum-hold floor in MS for the queued lane ([`vike_simulator::StrategyEngineConfig::queue_min_hold_ms`]): a
    /// position-reducing (closing) fill is deferred until this long after the position opened, so the
    /// backtest can't fabricate a sub-second maker round-trip. Only consulted when [`Self::queue_model`]
    /// is set. Absent ⇒ `0` (off). Calibrate from real MM flip times (Polymarket BTC-5m ≈ 2s p10).
    #[serde(default)]
    pub queue_min_hold_ms: Option<i64>,
    /// Equity-curve density for the tick lane (see [`vike_simulator::EquitySampling`]) — the SWEEP knob for
    /// a very long tape. The curve is two `Vec`s grown 16 bytes per priced tick, so a 100M-tick
    /// replay carries 1.6 GB of samples per run whether or not anything reads them.
    ///
    /// - absent or `1` ⇒ [`vike_simulator::EquitySampling::EveryTick`], the frozen default, byte-identical;
    /// - `N > 1` ⇒ [`vike_simulator::EquitySampling::EveryN`], keep one sample per `N` ticks (plus a closing
    ///   sample at the last tick, so `equity_curve.last()` still agrees with `final_equity`);
    /// - `0` ⇒ [`vike_simulator::EquitySampling::Off`], record nothing.
    ///
    /// ⚠ Anything but the default makes every curve-DERIVED report figure (max drawdown, sharpe,
    /// the return series) an approximation — `Off` degenerates them entirely. `final_equity`,
    /// `n_trades`, the trade log and `per_symbol_pnl` are never derived from the curve and do not
    /// move. TICK MODE ONLY: the bar lane records one sample per BAR (bounded by the bar count and
    /// needing no thinning), so a bar-mode profile setting this is REJECTED by
    /// [`ProfileBacktest::validate`] rather than silently ignored — the mirror of the
    /// `feed_latency` / `queue_model` rule.
    #[serde(default)]
    pub equity_sample_every: Option<usize>,
    /// Higher timeframes synthesised from the base bar stream, look-ahead safe: a coarse bar
    /// becomes visible only once its window has fully elapsed. Each entry is an
    /// `vike_marketdata::interval::interval_ms` spelling (`"4h"`, `"1d"`). Absent (the default) registers
    /// nothing, which is byte-identical to before this key existed.
    ///
    /// ⚠ Validated at LOAD by [`ProfileBacktest::validate`] rather than in the engine, because
    /// `StrategyEngine::new` registers each entry with `parse_timeframe(tf).expect(..)` — a
    /// process abort where every neighbouring key gives a named refusal.
    ///
    /// ⚠ **UNMET SPEC CLAUSE, recorded here rather than dropped silently.**
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §9.1 requires BOTH
    /// timeframe failure modes to become load-time refusals before this key ships: (1) the
    /// unparseable-interval panic, validated in [`ProfileBacktest::validate`] — which the rule
    /// above delivers — and (2) the ask-for-an-undeclared-timeframe panic, "checked against the
    /// strategy's declared timeframe requirement". **(2) was not built.** What shipped instead is
    /// `vike_simulator::OrderBrokerSimulator::bars_for` returning `None` where it used to
    /// `panic!("timeframe {tf:?} not registered")`, so a strategy asking for a timeframe this
    /// profile did not declare now runs SILENTLY on no higher-TF data where it previously aborted
    /// loudly. It is latent: that method has zero callers in this tree and no strategy
    /// declares a timeframe requirement at all. Giving strategies a way to DECLARE one — a
    /// different plan — is what would close it; until then this key ships with §9.1(2) open.
    #[serde(default)]
    pub timeframes: Vec<String>,
    /// Admit a fill only when the shared cash pool can fund it. Default `false`, which is what
    /// every profile written before this key did.
    ///
    /// ⚠ It is a WHOLE-STEP MODE SWITCH, not a per-order check: `true` routes the bar step through
    /// `StrategyEngine::fill_step_gated` (the owning type is the ENGINE, not `OrderBrokerSimulator` — this
    /// doc named the wrong one) instead of the per-symbol `fill_pending` loop, and unconditionally
    /// DISABLES the granular sub-bar lane. Turning it on therefore changes fill granularity as
    /// well as admission.
    ///
    /// ⚠ BAR MODE ONLY, and [`ProfileBacktest::validate`] refuses it on a tick profile:
    /// `StrategyEngine::run` is its only reader and `run_ticks` never consults it.
    #[serde(default)]
    pub cash_gate: bool,
    /// Add the closed-trade block to the report: one lot per entry fill, exits closing the oldest
    /// first, every row net of both commissions, with its trades-analysis statistics in all, long
    /// and short columns, the account-wide figures and the list of trades. Default `true`; `false`
    /// leaves the block out, as a report written before 2026-11-10 lacks it.
    ///
    /// It changes no fill and no existing number (`vike_analytics::closed_trades` has the rule;
    /// the block is the report's `closed_trades`, its rows the catalog's `closed_*` ids). A row's
    /// bars are counted on the run's bar series (a fill's bar is the one its time falls in).
    #[serde(default = "default_closed_trades")]
    pub closed_trades: bool,
    /// The yearly risk-free rate the closed-trade block's monthly Sharpe and Sortino subtract, a
    /// fraction (`0.03` is 3%). Absent: the reference's 2%. Read only where `closed_trades`
    /// is on; it prices nothing.
    #[serde(default)]
    pub risk_free_rate: Option<f64>,
    /// How the closed-trade block books a lot's entry commission when the lot is closed in parts:
    /// `false` (the default) shares it by quantity among the rows that close it, `true` books the
    /// whole of it on the first row that closes any of it
    /// (`vike_analytics::closed_trades::TradeClosedLedgerOptions::whole_entry_fee`). The two agree once a lot is
    /// closed whole. Read only where `closed_trades` is on, and refused without it; it prices nothing.
    #[serde(default)]
    pub whole_entry_fee: bool,
    /// WHEN a multi-symbol step is decided — `"sequential"` (absent = the default, byte-identical
    /// to every profile written before this key) or `"simultaneous"`. Resolved by
    /// [`decide_mode`], which owns the unknown-spelling refusal; the ARGUMENT is in
    /// [`vike_simulator::DecideMode`], and this doc deliberately does not restate it.
    ///
    /// # What it buys, in one sentence
    ///
    /// `"simultaneous"` makes the run's answer a property of the instrument SET rather than of
    /// the order `data.symbols` was typed in — the cross-sectional folds (`equity_now` and the
    /// three risk folds) and the shared-cash allocator's tie-break both walk the symbols in a
    /// canonical, name-derived order that no permutation of this profile's own symbol list can
    /// change. Today the list order is a strategy parameter nobody chose, no profile records and
    /// no report shows.
    ///
    /// # ⚠ It IMPLIES `cash_gate`, so it inherits both of that key's consequences
    ///
    /// The step routes through `StrategyEngine::fill_step_gated` instead of the per-symbol
    /// `fill_pending` loop, and the granular sub-bar lane is unconditionally DISABLED. That is
    /// not packaging: `fill_step_gated` is the engine's only lane that collects the whole step's
    /// orders before admitting any of them, so it is the only place a cross-section exists to be
    /// ordered. `[data] detail_interval` is refused with it for exactly the reason it is refused
    /// with `cash_gate` — the detail tape would be loaded, paid for and ignored.
    ///
    /// # ⚠ It is REFUSED beside an armed `[risk]` table or `engine.leverage`, and that is a
    /// declared GAP rather than a preference
    ///
    /// With a pre-trade gate armed, `OrderBrokerSimulator::gate_order` folds the `pending` set — which
    /// ACCUMULATES during the `on_bar` fan-out — so the symbol judged first faces the step's
    /// whole budget and every later one faces the remainder. That term is correct (a pre-trade
    /// cap must model the local view; its own doc argues why), which is why it is not removed
    /// here: the fix is a step-boundary budget snapshot, and that is NOT BUILT. Accepting the
    /// pair would let a run claim a cross-sectional decision whose DECISION half was still taken
    /// in list order — the one outcome worse than not having the mode.
    #[serde(default)]
    pub decide: Option<String>,
    /// Maintenance-margin RATE (a fraction of adverse notional), folded as
    /// `|size| · adverse · multiplier · maint_margin` by the liquidation watchdog.
    ///
    /// ⚠ `<= 0.0` short-circuits `check_liquidation` ENTIRELY — `0.0` means margin is OFF, not
    /// "zero margin required". ⚠ It is not part of the pre-trade `RiskGate`; `[risk]` owns that.
    #[serde(default)]
    pub maint_margin: f64,
    /// Equity cushion held above the maintenance requirement before the watchdog liquidates.
    /// ⚠ Defaults to `0.10` to match `StrategyEngineConfig::default()` — a bare `#[serde(default)]` would
    /// give `0.0` and silently change every profile that omits the key.
    #[serde(default = "default_liquidation_buffer")]
    pub liquidation_buffer: f64,
    /// Opt-in stress knob. ⚠ `true` selects the retired TOTAL-WIPE liquidation model — once
    /// `eq_adv <= maint_margin * notional_adv` at the intrabar adverse marks, it force-closes the
    /// WHOLE account (bar mode, `StrategyEngine::check_liquidation`) / the triggering symbol in
    /// full (tick mode, `StrategyEngine::check_liquidation_tick`). `false` (the default) runs the
    /// shared LEAN law (`vike_model::cross_liquidation_plan`) instead: PARTIAL, losers-first
    /// liquidation with the `liquidation_buffer` grace line — the opposite of a total wipe. Despite its
    /// name, `true` is the CRUDER model, not the gentler one.
    #[serde(default)]
    pub venue_style_liquidation: bool,
    /// Opt-in margin call ([`ShortfallCover`], whose doc is the rule): the account against its own
    /// long and short margin rates, a position it cannot carry cut by a multiple of the shortfall,
    /// and a fill it cannot margin refused whole. Absent (the default) leaves every fill and the
    /// liquidation above as they were. ⚠ BAR MODE ONLY, and it REPLACES the `maint_margin`
    /// liquidation, so [`ProfileBacktest::validate`] refuses it beside a `maint_margin` or
    /// `venue_style_liquidation`, and on a tick profile.
    #[serde(default)]
    pub margin_call: Option<ShortfallCover>,
    /// Participation cap: the fraction of the EVENT's own volume any one fill may take. `None`
    /// (the default) means uncapped, which is what every profile did before this key — and which
    /// lets a backtest "trade" more than the market traded.
    ///
    /// ⚠ BOTH LANES, and the denominator is the lane's own event rather than a bar. This doc and
    /// its validation message both said "a bar's own volume", which is only half the surface:
    /// `StrategyEngine::fill_pending_tick` hands `event.volume` to the SAME
    /// `StrategyEngine::dispatch_fill` the bar lanes use, and on the tick path that `event` is the
    /// symbol's single just-arrived print. So in bar mode the cap is a fraction of a BAR (or of a
    /// sub-bar on the granular lane) and in tick mode a fraction of ONE PRINT — a far tighter
    /// constraint at the same number. Calibrate per lane; `0.05` does not mean the same thing in
    /// both.
    #[serde(default)]
    pub volume_limit: Option<f64>,
    /// Ceiling on concurrently open positions. ⚠ `0` is the engine's UNLIMITED, not "none" — it is
    /// the default, and it is what every profile written before this key did.
    #[serde(default)]
    pub max_open_positions: usize,
    /// Ceiling on concurrently open LONG positions. ⚠ `0` means unlimited, as above.
    #[serde(default)]
    pub max_open_long: usize,
    /// Ceiling on concurrently open SHORT positions. ⚠ `0` means unlimited, as above.
    #[serde(default)]
    pub max_open_short: usize,
    /// Contract multiplier applied to every symbol without its own row in
    /// [`Self::multipliers`]. ⚠ Defaults to `1.0`, matching `StrategyEngineConfig::default()` — a bare
    /// `#[serde(default)]` would give `0.0` and zero every position's notional.
    #[serde(default = "default_multiplier")]
    pub multiplier: f64,
    /// Per-symbol contract multipliers, e.g. `ES = 50.0`. A `BTreeMap` rather than a `HashMap`
    /// because the engine takes an ordered `Vec` and a run must not depend on hash order.
    #[serde(default)]
    pub multipliers: BTreeMap<String, f64>,
    /// The run's own leverage. ⚠ Distinct from `[risk] max_leverage`, which reaches the same gate
    /// through `ProfileRisk::initial_margin_requirement` but is a CEILING — this is what the run uses, that is
    /// what it may not exceed.
    #[serde(default)]
    pub leverage: Option<f64>,
    /// Clamp an order down to what [`Self::leverage`] allows instead of rejecting it.
    ///
    /// ⚠ UNAVAILABLE to any profile carrying `[risk]`, and [`ProfileBacktest::validate`] refuses
    /// the pair outright. `OrderBrokerSimulator::build_risk_gate` returns `None` on this flag BEFORE it
    /// reads `risk_limits`, so the clamp disarms the pre-trade gate ENTIRELY — every `[risk]`
    /// limit is discarded, not just `max_leverage`. That is the opposite precedence from the
    /// clamp-off case, where `[risk]` is what wins and [`Self::leverage`] is what is discarded.
    #[serde(default)]
    pub clamp_to_leverage: bool,
    /// Variation-settlement cadence in milliseconds — how often open-position profit is realised
    /// into cash instead of accruing. `None` (the default) never settles, which is what every
    /// profile did before this key.
    #[serde(default)]
    pub settlement_period_ms: Option<i64>,
    /// Defer an order that falls outside its symbol's trading session instead of filling it.
    /// Deferrals are already counted and already reach the report as
    /// `BacktestResult::session_deferrals`.
    ///
    /// ⚠ RULED (task 11 of the 2026-09-12 backtest-engine-cfg-exposure plan): this key exposes
    /// [`vike_simulator::StrategyEngineConfig::session_gate`] alone. [`vike_simulator::StrategyEngineConfig::session_calendars`] is
    /// `IndexMap<String, SessionCalendar>` and a profile can only name a calendar by STRING — with
    /// no by-name `SessionCalendar` constructor/lookup in this crate, a TOML `[engine.sessions]`
    /// table would parse and reach nothing. A per-symbol calendar table does not exist yet, and
    /// shipping the gate without one is still correct.
    ///
    /// ⚠ **What is NOT correct — and what this doc asserted until the whole-branch review — is
    /// that arming it without a calendar table is a silent no-op.** It is not. With no per-symbol
    /// override each symbol falls back to `vike_venues::session_for(default_venue)`, and
    /// `default_venue` is present on BOTH lanes far more often than "no override means
    /// always-open" implies: [`crate::tick_replay::replay_ticks`] assigns
    /// `config.params.default_venue` **unconditionally** (tick replay is single-venue), so a tick
    /// profile ALWAYS has one, and `crate::harness::run::bar_engine_params` assigns it whenever
    /// [`Self::snap_to_properties`] is on. `vike_venues::session_for` then answers
    /// `vike_venues::session::FX_WEEK` — a real calendar excluding Fri 22:00 → Sun 21:00 UTC — for
    /// `dukascopy | oanda | ig | fxcm | ctrader`, `vike_venues::session::CRYPTO_24_7` for the
    /// crypto/prediction venues, and always-open for everything else (the mixed-asset venues,
    /// where venue alone cannot say).
    ///
    /// So: on an FX venue `session_gate = true` **defers fills today** and
    /// `BacktestResult::session_deferrals` is not `0`. It is a genuine no-op only where the
    /// resolved calendar is always-open — a crypto/mixed venue, or a bar profile with
    /// `snap_to_properties` off, which leaves `default_venue` `None`. Enabling it is a modelling
    /// choice about the venue's week, not a harmless flag.
    #[serde(default)]
    pub session_gate: bool,
    /// How a strategy's requested size becomes an order size
    /// ([`vike_analytics::sizing::PositionSizer`], the WealthLab PosSizer port). `None` (the
    /// default) passes the request through unchanged
    /// ([`vike_analytics::sizing::PositionSizerPassThrough`]) — what every profile did before this key existed
    /// (`StrategyEngine::new` installs it whenever `StrategyEngineConfig::sizer` is `None`).
    #[serde(default)]
    pub sizer: Option<ProfileEngineSizer>,
    /// The simulator's in-bar fill rules ([`InBarRules`], whose doc is each rule's law): how one
    /// bar's walk meets the resting orders. Absent (the default) = every rule off, byte-identical to
    /// every profile written before this table. BAR MODE ONLY, refused on a tick profile and beside
    /// the shared-cash lane for the rules that walk a bar ([`ProfileBacktest::validate`]).
    #[serde(default)]
    pub in_bar: Option<InBarRules>,
    /// The law a TAGGED plain limit fills by ([`QuotesConfig`]): `optimistic = true` is the simulator's
    /// maker law, `false` the ordinary limit law every other resting order fills by. Absent (the
    /// default) is what the strategy file's header declares (`[strategy] optimistic_quotes`: on for
    /// the makers that quoted on the simulator's maker lane), off for a file that declares nothing.
    /// Both lanes. The realism stamp names the law the run used (`engine.quotes`).
    #[serde(default)]
    pub quotes: Option<QuotesConfig>,
}

/// `[engine.quotes]`: which law a tagged plain limit fills by
/// (`vike_simulator::StrategyEngineConfig::optimistic_quotes`).
#[derive(Debug, Clone, Copy, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct QuotesConfig {
    /// `true`: a limit placed by tag, neither reduce-only nor triggered, fills by the MAKER LAW, in its
    /// own pass, whole at the touch, outside the volume cap, the fill-time caps, the in-bar rules and
    /// the staleness wait, and its end reaches no order-event hook; `false`: by the ordinary limit law.
    /// Untagged orders and every other kind keep their law either way.
    pub optimistic: bool,
}

/// [`ProfileEngine::quotes`] resolved for a run of `strategy`: what the profile states, else what the
/// strategy file's header declares (`StrategySource::optimistic_quotes`; off where no file was
/// read). The second answer says the profile stated it.
pub(crate) fn optimistic_quotes(
    quotes: Option<QuotesConfig>,
    strategy: &super::ProfileStrategy,
) -> (bool, bool) {
    match quotes {
        Some(q) => (q.optimistic, true),
        None => (strategy.source.as_ref().is_some_and(|s| s.optimistic_quotes), false),
    }
}

/// `[engine.margin_call]`'s numbers as [`ShortfallCover::validate`] judges them, refused naming the
/// key: what [`ProfileBacktest::validate`] and the run's `StrategyEngineConfig` both read.
pub(crate) fn checked_margin_call(call: ShortfallCover) -> Result<ShortfallCover, HarnessError> {
    call.validate().map_err(|e| HarnessError::Validation(format!("engine.margin_call.{e}")))?;
    Ok(call)
}

/// The HARNESS default for [`ProfileEngine::multiplier`], equal to `StrategyEngineConfig::default()`'s `1.0`.
fn default_multiplier() -> f64 {
    1.0
}

/// The HARNESS default for [`ProfileEngine::liquidation_buffer`], equal to `StrategyEngineConfig::default()`'s `0.10`.
/// ⚠ Do not replace with `#[serde(default)]`: that yields `0.0` and arms liquidation at the
/// maintenance line with no cushion.
fn default_liquidation_buffer() -> f64 {
    0.10
}

/// [`ProfileEngine::account_currency`] resolved: the dollar when absent, else the fiat code upper-cased.
///
/// The ONE door every reader of the key goes through (the load-time check in `validate`, the realism
/// stamp, the symbol guard `crate::harness::refuse_foreign_quoted_symbols`), so a currency this build
/// does not recognise is a refusal naming the key and never a ledger the guard then compares symbols
/// against. Takes the raw `Option<&str>` rather than the config because the guard's callers hold a
/// run's `StrategyEngineConfig::account_currency`, not a profile.
pub(crate) fn resolve_ledger_currency(raw: Option<&str>) -> Result<&'static str, HarnessError> {
    let Some(raw) = raw else {
        return Ok(vike_catalog::LEDGER_CURRENCY);
    };
    vike_venues::fiat_code(raw).ok_or_else(|| {
        HarnessError::Validation(format!(
            "unknown engine.account_currency {raw:?} (want a fiat currency code such as USD, EUR, GBP \
             or JPY; absent = USD, the dollar ledger)"
        ))
    })
}

/// [`ProfileEngine::conversion`] read against the run's ledger (`resolve_ledger_currency`): each row's
/// quote currency, its series and which way the pair points, in the table's order; none for a profile
/// that names none. The ONE reading of the table: the load-time check in `validate`, the plan
/// (`data.explain`) and the run's read (`crate::data_plan::load_conversions`) all go through it.
///
/// # Errors
/// [`HarnessError::Validation`] for the first row that is refused, naming it.
pub fn resolve_conversions(
    profile: &ProfileBacktest,
) -> Result<Vec<DataPlanConversion>, HarnessError> {
    let rows = &profile.engine.conversion;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ledger = resolve_ledger_currency(profile.engine.account_currency.as_deref())?;
    let mut out: Vec<DataPlanConversion> = Vec::with_capacity(rows.len());
    for (key, series) in rows {
        let Some(quote) = vike_model::currency_code(key)
            .filter(|code| code.bytes().all(|b| b.is_ascii_alphanumeric()))
        else {
            return Err(HarnessError::Validation(format!(
                "engine.conversion {key:?} is not a currency code (the quote currency the row \
                 converts into the ledger, such as JPY or USDT)"
            )));
        };
        if quote == ledger {
            return Err(HarnessError::Validation(format!(
                "engine.conversion.{quote} names a pair for the ledger's own currency ({ledger}, \
                 engine.account_currency), which converts into itself: drop the row"
            )));
        }
        if out.iter().any(|c| c.quote_currency == quote) {
            return Err(HarnessError::Validation(format!(
                "engine.conversion names {quote} twice (keys are read in any case): keep one row"
            )));
        }
        let own = crate::data_plan::own_series(profile);
        let (id, interval_ms) = vike_data::store::series::parse_input_name_for(series, own)
            .map_err(|e| {
                HarnessError::Validation(format!("engine.conversion.{quote} = {series:?}: {e}"))
            })?;
        let Some(leg) = vike_model::AccountPerQuoteLeg::of_pair_name(&id.symbol, &quote, ledger)
        else {
            return Err(HarnessError::Validation(format!(
                "engine.conversion.{quote} = {series:?}: the pair {} is neither {quote}{ledger} (a \
                 {quote} priced in {ledger}) nor {ledger}{quote} (a {ledger} priced in {quote}), so \
                 which way its price points against the {ledger} ledger cannot be read off its name. \
                 Name a series of one of the two (any of - / _ . between them)",
                id.symbol
            )));
        };
        out.push(DataPlanConversion { quote_currency: quote, id, interval_ms, leg });
    }
    Ok(out)
}

impl ProfileEngine {
    /// Does this run read the stored funding series of `(venue, symbol)`? [`Self::attach_funding`] when
    /// the profile states it, else whether the name is a perpetual: funding is ON by default for a
    /// perpetual. The rule is `crate::harness::run::attaches_funding`'s, which the run's read calls too.
    pub(crate) fn attaches_funding(&self, venue: &str, symbol: &str) -> bool {
        crate::harness::run::attaches_funding(self.attach_funding, venue, symbol)
    }

    /// The ledger currency a report of this profile must NAME: [`Self::account_currency`] resolved, when
    /// it is not the dollar, else `None` (the dollar needs no label, and a label that appeared on
    /// every report would move every dollar document). An unreadable code is `None` too: `validate`
    /// has refused it at load, so only a hand-built profile reaches a producer with one.
    pub(crate) fn off_dollar_ledger(&self) -> Option<&'static str> {
        resolve_ledger_currency(self.account_currency.as_deref())
            .ok()
            .filter(|currency| *currency != vike_catalog::LEDGER_CURRENCY)
    }

    /// Resolve the optional [`Self::queue_model`] string to a [`QueueModelKind`] for the tick queue
    /// lane. `None` (absent) ⇒ the frozen simple-crossing fill. Case-insensitive; `"prob_power:N"`
    /// carries the power exponent (`"prob_power"` alone ⇒ `N = 1`). An unrecognized value is a
    /// fail-fast [`HarnessError::Validation`], never a silent fallback.
    pub(crate) fn queue_model_kind(&self) -> Result<Option<QueueModelKind>, HarnessError> {
        let Some(raw) = self.queue_model.as_deref() else {
            return Ok(None);
        };
        let s = raw.trim().to_ascii_lowercase();
        let bad = |m: String| HarnessError::Validation(m);
        let kind = if let Some((head, tail)) = s.split_once(':') {
            match head {
                "prob_power" => QueueModelKind::ProbPower(tail.parse::<f64>().map_err(|_| {
                    bad(format!("queue_model \"prob_power:{tail}\": {tail:?} is not a number"))
                })?),
                other => {
                    return Err(bad(format!(
                        "unknown queue_model {other:?} (want risk_averse | prob_power[:n] | prob_log)"
                    )));
                }
            }
        } else {
            match s.as_str() {
                "risk_averse" | "risk-averse" | "conservative" => QueueModelKind::RiskAverse,
                // The misspelling the profile word had until 2026-11 is refused with its new spelling.
                old @ ("risk_adverse" | "risk-adverse") => {
                    return Err(bad(format!("queue_model {old:?} is spelled \"risk_averse\" now")));
                }
                "prob_power" => QueueModelKind::ProbPower(1.0),
                "prob_log" => QueueModelKind::ProbLog,
                other => {
                    return Err(bad(format!(
                        "unknown queue_model {other:?} (want risk_averse | prob_power[:n] | prob_log)"
                    )));
                }
            }
        };
        Ok(Some(kind))
    }

    /// Resolve the optional [`Self::equity_sample_every`] stride to an [`EquitySampling`].
    /// Absent or `1` ⇒ the frozen [`EquitySampling::EveryTick`]; `0` ⇒ [`EquitySampling::Off`];
    /// `N > 1` ⇒ [`EquitySampling::EveryN`]. Total (no error case): every `usize` names a valid
    /// density, unlike the string-keyed `queue_model`/`fill_model` resolvers where a typo could
    /// silently select a different model.
    pub(crate) fn equity_sampling(&self) -> EquitySampling {
        match self.equity_sample_every {
            None | Some(1) => EquitySampling::EveryTick,
            Some(0) => EquitySampling::Off,
            Some(n) => EquitySampling::EveryN(n),
        }
    }

    /// Resolve the optional [`Self::fill_model`] string to the tick-lane [`FillModelKind`].
    /// `None` (absent) ⇒ the default L1 spread-crossing [`FillModelKind::Tick`]. Case-insensitive
    /// (trimmed), mirroring [`Self::queue_model_kind`]. An unrecognized value is a fail-fast
    /// [`HarnessError::Validation`] naming the valid set, never a silent fallback — the old
    /// "absent / any other value ⇒ `Tick`" lenience meant a typo (`"L2_Book"`, `"l2_book"`)
    /// quietly selected the optimistic L1 model and undid the depth-cap realism knob (#819).
    pub(crate) fn fill_model_kind(&self) -> Result<FillModelKind, HarnessError> {
        let Some(raw) = self.fill_model.as_deref() else {
            return Ok(FillModelKind::Tick);
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "l2book" => Ok(FillModelKind::L2Book),
            other => Err(HarnessError::Validation(format!(
                "unknown fill_model {other:?} (want l2book; absent = the default Tick model)"
            ))),
        }
    }
}

/// The HARNESS default for [`ProfileEngine::emulator_release_stops`]: `false`, matching
/// [`vike_simulator::StrategyEngineConfig::default()`]. ⚠ This previously returned `true` ("mirror-live") while the
/// field reached the engine through NEITHER `StrategyEngineConfig` construction site in
/// `crate::harness::run` — so every harness run was `false` in practice no matter what this
/// function answered. `false` makes the code agree with what has always actually happened; now
/// that the field is wired into both sites, an explicit `true` in a profile's TOML really does arm
/// mirror-live release. Flipping this default to mirror-live is a separate, still-open owner
/// decision — see the field doc.
fn default_emulator_release_stops() -> bool {
    false
}

/// [`ProfileEngine::closed_trades`] when a profile states none: on.
fn default_closed_trades() -> bool {
    true
}
