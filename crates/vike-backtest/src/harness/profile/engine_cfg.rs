//! The `[engine]` section: `EngineCfg`, the cost/cash/fill configuration handed to the sim broker.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{FeeCfg, ImpactCfg, ResolutionCfg, SizerCfg};
use crate::harness::HarnessError;
use vike_sim::{EquitySampling, FillModelKind, QueueModelKind};

#[cfg(doc)]
use super::{BacktestProfile, decide_mode};

/// Engine cost/cash configuration handed to the sim broker.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCfg {
    pub cash: f64,
    #[serde(default)]
    pub fee_rate: f64,
    #[serde(default)]
    pub slippage: f64,
    #[serde(default)]
    pub snap_to_properties: bool,
    /// Opt-in: window-join the stored market `funding` series onto the replayed bars' `Bar.funding`
    /// at each funding-event timestamp, for perp funding accrual + funding-reading strategies. The
    /// market funding-rate backfill (#761) writes a `(venue, symbol, "funding")` bar series whose
    /// each `Bar.funding = Some(rate)` sits at a funding-event ts; with this on,
    /// [`crate::harness::window_join_funding`] attaches each rate to the ONE price bar whose window
    /// contains the event (NEVER forward-filled onto every bar — that would make the `SimBroker`
    /// bar-loop accrual charge every bar instead of once per 8h/1h interval). Both consumers then
    /// work: the accrual (`engine.rs`, charges when `Bar.funding.is_some()`) and a funding-reading
    /// strategy's signal (`FundingCarryController` remembers the last rate in its funding book).
    ///
    /// BAR MODE ONLY — the per-interval accrual runs in the bar loop; the tick lane has no funding
    /// fold, so a profile that sets this with `data.kind = "tick"` is rejected by
    /// [`BacktestProfile::validate`] (the mirror of the tick-only `feed_latency` rule). The funding lookup
    /// keys off each RESOLVED series' own venue, so it works for a cross-venue `[[data.series]]`
    /// slice too (unlike `snap_to_properties`, which is single-venue). Absent (the default `false`)
    /// = the funding series is never read, byte-identical. On but with no stored funding data = a
    /// warning + unchanged bars, never an error.
    ///
    /// EXPECTATION: the price bar interval must be no COARSER than the funding cadence, so at most
    /// one funding event lands in a price bar's window. If more than one does (e.g. 1d price bars
    /// over 8h funding), only the LAST is kept and the collision is logged loudly — use a finer
    /// price interval to avoid collapsing charges.
    #[serde(default)]
    pub attach_funding: bool,
    /// Opt-in feed-latency replay (see [`crate::hist_replay::TickReplayConfig::feed_latency`]): deliver ticks to
    /// the strategy in recorded ARRIVAL (`local_ts`) order instead of venue order, while order
    /// matching stays on venue `ts`. Absent (the default `false`) = today's venue-ordered replay,
    /// byte-identical. TICK MODE ONLY — a bar has no `local_ts`, so a profile that sets this with
    /// `data.kind = "bar"` is rejected by [`BacktestProfile::validate`] rather than silently
    /// ignored (the mirror of the bar-only `attach_funding` rule).
    #[serde(default)]
    pub feed_latency: bool,
    /// Opt-in ORDER latency (see [`vike_sim::LatencyModelKind`]): a fixed entry-leg delay in MILLISECONDS on
    /// every strategy order action (place / modify / cancel). The maker's quotes, re-prices and
    /// pulls only reach the matching engine `order_latency_ms` after it decides them, so it cannot
    /// react within one tick — the missing clock skew that otherwise lets a maker enter and exit in
    /// the same instant. `0` (the default) = zero latency, byte-identical. TICK MODE ONLY (only
    /// `run_ticks` arms the latency gate; the bar lane and vector kernel never consult it).
    #[serde(default)]
    pub order_latency_ms: i64,
    /// Opt-in FILL-notification latency (the response leg, see [`vike_sim::LatencyModelKind`]): a fixed delay in
    /// MILLISECONDS before the strategy LEARNS of a fill. The fill books at the real time (equity/PnL
    /// are exact), but the strategy-visible shadow position ([`vike_model::HftBroker::position`])
    /// does not advance until `fill_latency_ms` later — so a maker that polls its inventory cannot
    /// react (place its exit) inside that gap. Models the "time to realise you're filled" half of a
    /// real round-trip reaction. `0` (the default) = off, byte-identical. TICK MODE ONLY.
    #[serde(default)]
    pub fill_latency_ms: i64,
    /// Opt-in tick-lane fill-model override: `"l2book"` (the ONE accepted value; case-insensitive,
    /// like [`Self::queue_model`]) selects the depth-capped
    /// [`vike_sim::FillModelKind::L2Book`] (a resting order fills only up to the DISPLAYED book
    /// depth; when the within-limit depth cannot cover its size it RESTS rather than filling size the
    /// market never showed), vs the default L1 spread-crossing `Tick` model (fills the full size at
    /// the quote). Absent ⇒ `Tick`, byte-identical. Any OTHER value is a fail-fast
    /// [`HarnessError::Validation`] at load ([`EngineCfg::fill_model_kind`], the mirror of the
    /// [`Self::queue_model`] rule) — a typo like `"l2_book"` must not silently select the
    /// optimistic `Tick` model and undo the depth-cap realism knob. TICK MODE ONLY. Pair with
    /// `slippage = 0.0` (the book walk IS the slippage).
    #[serde(default)]
    pub fill_model: Option<String>,
    #[serde(default)]
    pub seed_bar_interval_ms: Option<i64>,
    /// Opt-in market-impact slippage on top of the flat `slippage` (see [`vike_sim::ImpactModel`]).
    /// Absent (the default) = the frozen flat-slippage cost, byte-identical.
    ///
    /// BOTH MODES since the lane split. This was BAR MODE ONLY and rejected outright in tick
    /// mode, on the reasoning that "the tick lane replays a real book and needs no model" — half
    /// right, and the wrong half is now the point of the knob: an L1 tick tape has no book at
    /// all (it fills any size at the quote), and even an L2 replay walks a RECORDING that never
    /// moves in response to the order, so the permanent footprint is missing there too. Each
    /// lane is charged only what its own price law has not already paid — see
    /// [`vike_sim::ImpactTerms`], which is where that decision lives.
    ///
    /// ⚠ [`ImpactCfg::window`] changes UNITS with the mode (bars vs trade prints).
    #[serde(default)]
    pub impact: Option<ImpactCfg>,
    /// Stop-verb release timing (see [`vike_sim::EngineParams::emulator_release_stops`]).
    ///
    /// ⚠ The HARNESS default is `false`, matching [`vike_sim::EngineParams::default()`]: a fired
    /// conditional stop fills SAME-EVENT at the trigger oracle's price, the raw-engine legacy
    /// behaviour (the pinned backtest divergence, law-map A2). This field previously documented a
    /// harness default of `true` ("mirror-live") while reaching the engine through NEITHER
    /// `EngineParams` construction site in `crate::harness::run` — both end in
    /// `..Default::default()` — so every harness run was `false` in practice regardless of what a
    /// profile's TOML said or what this doc claimed. The default now agrees with what has always
    /// actually happened, and the field is now wired into BOTH construction sites, so an explicit
    /// `true` really does arm mirror-live release: a fired conditional stop converts to a resting
    /// MARKET child that fills the NEXT event, exactly as the live emulator's `ConditionalBook`
    /// does.
    ///
    /// Whether the harness SHOULD default to mirror-live (so a profile run's fill timing matches
    /// what the same order would do against a live venue) rather than the raw-engine default is a
    /// separate, still-open OWNER DECISION — this fix wires the knob and corrects the doc; it
    /// deliberately does not also flip live simulation behaviour as a side effect.
    #[serde(default = "default_emulator_release_stops")]
    pub emulator_release_stops: bool,
    /// Opt-in fee SCHEDULE (port backlog G7) — the shapes a flat [`Self::fee_rate`] cannot
    /// express. Absent (the default) = the flat `fee_rate` path, byte-identical. Setting BOTH
    /// is a validation error: two cost models would be configured and only one could win.
    #[serde(default)]
    pub fee: Option<FeeCfg>,
    /// Opt-in binary-resolution settlement (port backlog G6): builds the
    /// [`vike_sim::EngineParams::resolution`] source + `resolution_end_ts` the engine has always
    /// consumed. Absent (the default) = no settlement source, byte-identical.
    #[serde(default)]
    pub resolution: Option<ResolutionCfg>,
    /// Opt-in QUEUE-POSITION fill model for the tick lane (see [`vike_sim::QueueModelKind`]): a resting
    /// limit — INCLUDING a tagged MAKER quote — no longer fills the instant price touches it, but
    /// only once a taker TRADE has consumed the size AHEAD of it in the FIFO queue (seeded from the
    /// replayed L2 book's size at that price, or the last L1 quote, or [`Self::queue_seed_depth`]).
    /// This is the realistic passive-maker fill: a quote earns the spread only when flow actually
    /// hits it, with partial fills on the trade's excess over the front. Absent (the default) = the
    /// frozen simple-crossing fill (`fill_tagged`), byte-identical. TICK MODE ONLY — the queue lane
    /// is consulted exclusively by `run_ticks`, so a bar-mode profile setting it is rejected by
    /// [`BacktestProfile::validate`] (the mirror of the `feed_latency` rule). Values (case-insensitive):
    /// `"risk_adverse"` (the conservative bound — front shrinks only on hard evidence),
    /// `"prob_power"` / `"prob_power:N"` (probabilistic, `f(x)=x^N`, default `N=1`), `"prob_log"`.
    /// For a real maker backtest feed the TRADE tape (and, for true size-ahead seeding, the BOOK).
    #[serde(default)]
    pub queue_model: Option<String>,
    /// Fallback front-of-queue depth (in size units) seeded when neither the replayed L2 book nor an
    /// L1 quote gives a size at a resting order's price — see [`vike_sim::QueueModelKind`]. Only consulted
    /// when [`Self::queue_model`] is set. Absent ⇒ `0.0` (a quote with no observed size-ahead fills
    /// on the first at-price trade).
    #[serde(default)]
    pub queue_seed_depth: Option<f64>,
    /// Minimum-hold floor in MS for the queued lane ([`vike_sim::EngineParams::queue_min_hold_ms`]): a
    /// position-reducing (closing) fill is deferred until this long after the position opened, so the
    /// backtest can't fabricate a sub-second maker round-trip. Only consulted when [`Self::queue_model`]
    /// is set. Absent ⇒ `0` (off). Calibrate from real MM flip times (Polymarket BTC-5m ≈ 2s p10).
    #[serde(default)]
    pub queue_min_hold_ms: Option<i64>,
    /// Equity-curve density for the tick lane (see [`vike_sim::EquitySampling`]) — the SWEEP knob for
    /// a very long tape. The curve is two `Vec`s grown 16 bytes per priced tick, so a 100M-tick
    /// replay carries 1.6 GB of samples per run whether or not anything reads them.
    ///
    /// - absent or `1` ⇒ [`vike_sim::EquitySampling::EveryTick`], the frozen default, byte-identical;
    /// - `N > 1` ⇒ [`vike_sim::EquitySampling::EveryN`], keep one sample per `N` ticks (plus a closing
    ///   sample at the last tick, so `equity_curve.last()` still agrees with `final_equity`);
    /// - `0` ⇒ [`vike_sim::EquitySampling::Off`], record nothing.
    ///
    /// ⚠ Anything but the default makes every curve-DERIVED report figure (max drawdown, sharpe,
    /// the return series) an approximation — `Off` degenerates them entirely. `final_equity`,
    /// `n_trades`, the trade log and `per_symbol_pnl` are never derived from the curve and do not
    /// move. TICK MODE ONLY: the bar lane records one sample per BAR (bounded by the bar count and
    /// needing no thinning), so a bar-mode profile setting this is REJECTED by
    /// [`BacktestProfile::validate`] rather than silently ignored — the mirror of the
    /// `feed_latency` / `queue_model` rule.
    #[serde(default)]
    pub equity_sample_every: Option<usize>,
    /// Higher timeframes synthesised from the base bar stream, look-ahead safe: a coarse bar
    /// becomes visible only once its window has fully elapsed. Each entry is an
    /// `vike_model::time::interval_ms` spelling (`"4h"`, `"1d"`). Absent (the default) registers
    /// nothing, which is byte-identical to before this key existed.
    ///
    /// ⚠ Validated at LOAD by [`BacktestProfile::validate`] rather than in the engine, because
    /// `StrategyEngine::new` registers each entry with `parse_timeframe(tf).expect(..)` — a
    /// process abort where every neighbouring key gives a named refusal.
    ///
    /// ⚠ **UNMET SPEC CLAUSE, recorded here rather than dropped silently.**
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §9.1 requires BOTH
    /// timeframe failure modes to become load-time refusals before this key ships: (1) the
    /// unparseable-interval panic, validated in [`BacktestProfile::validate`] — which the rule
    /// above delivers — and (2) the ask-for-an-undeclared-timeframe panic, "checked against the
    /// strategy's declared timeframe requirement". **(2) was not built.** What shipped instead is
    /// `vike_sim::SimBroker::bars_for`/`forming_for` returning `None` where they used to
    /// `panic!("timeframe {tf:?} not registered")`, so a strategy asking for a timeframe this
    /// profile did not declare now runs SILENTLY on no higher-TF data where it previously aborted
    /// loudly. It is latent: those two methods have zero callers in this tree and no strategy
    /// declares a timeframe requirement at all. Giving strategies a way to DECLARE one — a
    /// different plan — is what would close it; until then this key ships with §9.1(2) open.
    #[serde(default)]
    pub timeframes: Vec<String>,
    /// Admit a fill only when the shared cash pool can fund it. Default `false`, which is what
    /// every profile written before this key did.
    ///
    /// ⚠ It is a WHOLE-STEP MODE SWITCH, not a per-order check: `true` routes the bar step through
    /// `StrategyEngine::fill_step_gated` (the owning type is the ENGINE, not `SimBroker` — this
    /// doc named the wrong one) instead of the per-symbol `fill_pending` loop, and unconditionally
    /// DISABLES the granular sub-bar lane. Turning it on therefore changes fill granularity as
    /// well as admission.
    ///
    /// ⚠ BAR MODE ONLY, and [`BacktestProfile::validate`] refuses it on a tick profile:
    /// `StrategyEngine::run` is its only reader and `run_ticks` never consults it.
    #[serde(default)]
    pub cash_gate: bool,
    /// WHEN a multi-symbol step is decided — `"sequential"` (absent = the default, byte-identical
    /// to every profile written before this key) or `"simultaneous"`. Resolved by
    /// [`decide_mode`], which owns the unknown-spelling refusal; the ARGUMENT is in
    /// [`vike_sim::DecideMode`], and this doc deliberately does not restate it.
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
    /// With a pre-trade gate armed, `SimBroker::gate_order` folds the `pending` set — which
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
    /// ⚠ Defaults to `0.10` to match `EngineParams::default()` — a bare `#[serde(default)]` would
    /// give `0.0` and silently change every profile that omits the key.
    #[serde(default = "default_liq_buffer")]
    pub liq_buffer: f64,
    /// Opt-in stress knob. ⚠ `true` selects the retired TOTAL-WIPE liquidation model — once
    /// `eq_adv <= maint_margin * notional_adv` at the intrabar adverse marks, it force-closes the
    /// WHOLE account (bar mode, `StrategyEngine::check_liquidation`) / the triggering symbol in
    /// full (tick mode, `StrategyEngine::check_liquidation_tick`). `false` (the default) runs the
    /// shared LEAN law (`vike_model::cross_liquidation_plan`) instead: PARTIAL, losers-first
    /// liquidation with the `liq_buffer` grace line — the opposite of a total wipe. Despite its
    /// name, `true` is the CRUDER model, not the gentler one.
    #[serde(default)]
    pub venue_style_liquidation: bool,
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
    /// [`Self::multipliers`]. ⚠ Defaults to `1.0`, matching `EngineParams::default()` — a bare
    /// `#[serde(default)]` would give `0.0` and zero every position's notional.
    #[serde(default = "default_multiplier")]
    pub multiplier: f64,
    /// Per-symbol contract multipliers, e.g. `ES = 50.0`. A `BTreeMap` rather than a `HashMap`
    /// because the engine takes an ordered `Vec` and a run must not depend on hash order.
    #[serde(default)]
    pub multipliers: BTreeMap<String, f64>,
    /// The run's own leverage. ⚠ Distinct from `[risk] max_leverage`, which reaches the same gate
    /// through `ProfileRisk::im_requirement` but is a CEILING — this is what the run uses, that is
    /// what it may not exceed.
    #[serde(default)]
    pub leverage: Option<f64>,
    /// Clamp an order down to what [`Self::leverage`] allows instead of rejecting it.
    ///
    /// ⚠ UNAVAILABLE to any profile carrying `[risk]`, and [`BacktestProfile::validate`] refuses
    /// the pair outright. `SimBroker::build_risk_gate` returns `None` on this flag BEFORE it
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
    /// [`vike_sim::EngineParams::session_gate`] alone. [`vike_sim::EngineParams::session_calendars`] is
    /// `IndexMap<String, SessionCalendar>` and a profile can only name a calendar by STRING — with
    /// no by-name `SessionCalendar` constructor/lookup in this crate, a TOML `[engine.sessions]`
    /// table would parse and reach nothing. A per-symbol calendar table does not exist yet, and
    /// shipping the gate without one is still correct.
    ///
    /// ⚠ **What is NOT correct — and what this doc asserted until the whole-branch review — is
    /// that arming it without a calendar table is a silent no-op.** It is not. With no per-symbol
    /// override each symbol falls back to `vike_model::session_for(default_venue)`, and
    /// `default_venue` is present on BOTH lanes far more often than "no override means
    /// always-open" implies: [`crate::hist_replay::replay_ticks`] assigns
    /// `cfg.params.default_venue` **unconditionally** (tick replay is single-venue), so a tick
    /// profile ALWAYS has one, and `crate::harness::run::bar_engine_params` assigns it whenever
    /// [`Self::snap_to_properties`] is on. `vike_model::session_for` then answers
    /// `vike_model::venues::session::FX_WEEK` — a real calendar excluding Fri 22:00 → Sun 21:00 UTC — for
    /// `dukascopy | oanda | ig | fxcm | ctrader`, `vike_model::venues::session::CRYPTO_24_7` for the
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
    /// ([`vike_analytics::sizing::PassThroughSizer`]) — what every profile did before this key existed
    /// (`StrategyEngine::new` installs it whenever `EngineParams::sizer` is `None`).
    #[serde(default)]
    pub sizer: Option<SizerCfg>,
}

/// The HARNESS default for [`EngineCfg::multiplier`], equal to `EngineParams::default()`'s `1.0`.
fn default_multiplier() -> f64 {
    1.0
}

/// The HARNESS default for [`EngineCfg::liq_buffer`], equal to `EngineParams::default()`'s `0.10`.
/// ⚠ Do not replace with `#[serde(default)]`: that yields `0.0` and arms liquidation at the
/// maintenance line with no cushion.
fn default_liq_buffer() -> f64 {
    0.10
}

impl EngineCfg {
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
                        "unknown queue_model {other:?} (want risk_adverse | prob_power[:n] | prob_log)"
                    )));
                }
            }
        } else {
            match s.as_str() {
                "risk_adverse" | "risk-adverse" | "conservative" => QueueModelKind::RiskAdverse,
                "prob_power" => QueueModelKind::ProbPower(1.0),
                "prob_log" => QueueModelKind::ProbLog,
                other => {
                    return Err(bad(format!(
                        "unknown queue_model {other:?} (want risk_adverse | prob_power[:n] | prob_log)"
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

/// The HARNESS default for [`EngineCfg::emulator_release_stops`]: `false`, matching
/// [`vike_sim::EngineParams::default()`]. ⚠ This previously returned `true` ("mirror-live") while the
/// field reached the engine through NEITHER `EngineParams` construction site in
/// `crate::harness::run` — so every harness run was `false` in practice no matter what this
/// function answered. `false` makes the code agree with what has always actually happened; now
/// that the field is wired into both sites, an explicit `true` in a profile's TOML really does arm
/// mirror-live release. Flipping this default to mirror-live is a separate, still-open owner
/// decision — see the field doc.
fn default_emulator_release_stops() -> bool {
    false
}
