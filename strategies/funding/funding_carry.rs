//! Funding-rate CARRY controller — the #517 controller → executor seam consuming the #521 live
//! funding feed: rank the observed venues by perp FUNDING-RATE DIFFERENTIAL net of round-trip taker
//! fees, then open a delta-neutral pair — **LONG the low/negative-funding leg, SHORT the
//! high/positive-funding leg** — collecting the differential each settlement while the legs' price
//! exposure cancels. It only DECIDES: the runtime books the funding
//! (`vike_execution::Account::apply_funding` adds `FundingEvent.amount` to the balance).
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! why = """\
//!     TWO-LEG and cross-venue: needs one declared mount leg per carry venue, and \
//!     a strategy declares legs on its mount's own venue only \
//!     (`vike_model::Strategy::symbols`; the paper builder refuses a leg on another \
//!     venue) — plus the same missing live funding series as `funding_capture`"""
//! routes = "multi_leg"
//! multi_leg_why = """\
//!     TWO-LEG and cross-venue by construction: `venues` is the per-leg venue map its carry \
//!     book is built from (≥2 venues, or there is no carry to open) and an EMPTY `symbol` is \
//!     its REAL two-leg mode, so no value of these keys is required to name the mount's own \
//!     single market. `LIVE_CAPABLE` refuses the name for the same missing mount legs"""
//!
//! [params.symbol]
//! type = "text"
//! default = ""
//! route = "symbol"
//!
//! [params.qty]
//! type = "number"
//! default = 1.0
//!
//! [params.tp]
//! type = "number"
//!
//! [params.sl]
//! type = "number"
//!
//! [params.time_limit_ms]
//! type = "integer"
//!
//! [params.trailing]
//! type = "number"
//!
//! [params.hold_periods]
//! type = "number"
//! default = 1.0
//!
//! [params.entry_threshold]
//! type = "number"
//! default = 0.0
//!
//! [params.venue]
//! type = "text"
//! default = "sim"
//! route = "venue"
//!
//! [params.cooldown_ms]
//! type = "integer"
//! default = 0
//!
//! [params.venues]
//! type = "table"
//! route = "venue_map"
//! ```
//!
//! ## Sign conventions (the funding-carry law, frozen here)
//! On every wired perp venue a POSITIVE `funding_rate` means **longs pay shorts**
//! (`amount = funding_rate × notional` flows long → short): per interval per unit notional a
//! LONG's cashflow is `−funding_rate` and a SHORT's `+funding_rate`. Long `L` + short `S` therefore
//! collects `funding_S − funding_L` (the **gross differential**), maximized and non-negative by
//! putting the LONG on the LOWER rate — exactly what [`rank_best_carry`] does.
//!
//! Each leg crosses as a taker on entry AND exit, so the round trip costs `2 × (taker_L + taker_S)`
//! ([`roundtrip_taker_cost`]). The **net edge** `hold_periods × gross − roundtrip_cost`
//! ([`net_carry_edge`]) is the ranking metric and the gate against `entry_threshold`.
//!
//! ## The [`Controller`] seam
//! [`FundingCarryController`] keeps a per-venue FUNDING BOOK for its symbol, fed by
//! [`FundingCarryController::observe_funding`] (a live `Event::Funding.funding_rate` or a recorded
//! `Bar.funding`) and, inside [`Controller::evaluate`], auto-observed from the asked venue's latest
//! bar (`broker.bars(symbol).last().funding`). When the asked `venue` is a LEG of the best carry
//! clearing `entry_threshold`, `evaluate` returns that leg's correctly-SIDED [`PositionExecutorIntent`] for
//! [`ControllerHarness`] to open as a [`vike_strategy::PositionExecutor`].
//!
//! MULTI-VENUE (WIRED): the harness's per-symbol `venue_map` (`symbol -> venue`) makes ONE mount
//! ask `evaluate` per `(venue, symbol)`, filling the cross-venue book. `symbol` EMPTY (the default)
//! ⇒ TWO-LEG delta-neutral, one executor per leg of the best carry; `symbol` SET ⇒ SINGLE-LEG (only
//! that symbol's leg; the opposite venue's price exposure stays open). The book observes every
//! venue either way. Backtest: `engine.attach_funding` + a `[strategy.params.venues]` table (see
//! `crates/vike-backtest/tests/funding_carry_demo.rs`, which runs the library's twin of this
//! file).
//!
//! DELIBERATE LIMITATION: the COMPRESSION / FLIP close ([`should_close_carry`]) is a pure, tested
//! function but NOT wired — [`Controller::evaluate`] is invited only while a pair is FLAT, so a
//! held carry exits via its leg [`vike_model::TripleBarrier`]. Wiring it needs a controller-level
//! per-settlement hook.
//!
//! OFF / additive: nothing mounts it by default; an under-fed controller (fewer than two venues, or
//! the best net edge below threshold) emits NO intent. No Python twin.

use vike_model::{PositionControllerParams, TripleBarrier};
use vike_venues::{fee_lane, fee_schedule_for};

use vike_model::params::{ParamValue, Params};
use vike_model::{OrderBroker, Strategy};
use vike_strategy::{Controller, ControllerHarness, PositionExecutorIntent};

/// The strategy this file runs, built from the profile's `[strategy.params]`: the controller's own
/// knobs read by [`FundingCarryController::from_params`], the harness-level ones by
/// [`controller_harness`].
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(controller_harness(FundingCarryController::from_params(params), params))
}

/// What each declared key RESOLVED to, in the header's order (the mount's "effective params" line).
/// An absent barrier leg reads `(unarmed)`; the harness's `venue` fallback `sim` is a tag no venue
/// answers to and is reported for exactly that reason; a `venues` row whose value is not a string is
/// dropped by the reader, so the SURVIVING map is printed.
pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    fn number(v: f64) -> String {
        format!("{v}")
    }
    fn opt_number(v: Option<f64>) -> String {
        v.map(number).unwrap_or_else(|| "(unarmed)".to_string())
    }
    fn opt_ms(v: Option<i64>) -> String {
        v.map(|i| i.to_string()).unwrap_or_else(|| "(unarmed)".to_string())
    }
    fn venues(map: &[(String, String)]) -> String {
        if map.is_empty() {
            return "(none)".to_string();
        }
        map.iter().map(|(s, v)| format!("{s}:{v}")).collect::<Vec<_>>().join(",")
    }
    let c = FundingCarryController::from_params(params);
    let b = c.barriers();
    vec![
        (
            "symbol",
            if c.symbol().is_empty() { "(both legs)".to_string() } else { c.symbol().to_string() },
        ),
        ("qty", number(c.qty())),
        ("tp", opt_number(b.take_profit)),
        ("sl", opt_number(b.stop_loss)),
        ("time_limit_ms", opt_ms(b.time_limit_ms)),
        ("trailing", opt_number(b.trailing)),
        ("hold_periods", number(c.hold_periods())),
        ("entry_threshold", number(c.entry_threshold())),
        ("venue", harness_venue(params).to_string()),
        ("cooldown_ms", harness_cooldown_ms(params).to_string()),
        ("venues", venues(&harness_venue_map(params))),
    ]
}

/// Read a [`TripleBarrier`] from a params table — every leg OPTIONAL (absent ⇒ un-armed):
/// `tp`/`sl`/`trailing` are absolute price offsets from the entry fill, `time_limit_ms` the max
/// holding duration.
fn barriers_from_params(params: &Params) -> TripleBarrier {
    TripleBarrier::new(
        params.get("tp").and_then(ParamValue::as_f64),
        params.get("sl").and_then(ParamValue::as_f64),
        params.get("time_limit_ms").and_then(ParamValue::as_integer),
        params.get("trailing").and_then(ParamValue::as_f64),
    )
}

/// Wrap a [`Controller`] in its portable [`ControllerHarness`], reading the harness-level knobs
/// (NOT controller knobs) from the same params table: `venue` (default `"sim"`; the key this
/// controller reads to pick its leg), `cooldown_ms` (post-exit re-open spacing, default `0`) and the
/// `venues` map. The controller's OWN knobs were read by its `from_params`.
fn controller_harness<C: Controller>(controller: C, params: &Params) -> ControllerHarness<C> {
    ControllerHarness::new(controller, harness_venue(params), harness_cooldown_ms(params))
        .with_venue_map(harness_venue_map(params))
}

/// The harness's default venue tag. ⚠ The fallback is the literal `"sim"`, which on a LIVE mount is
/// a tag no venue answers to — [`echo`] reports it for exactly that reason.
fn harness_venue(params: &Params) -> &str {
    params.get("venue").and_then(ParamValue::as_str).unwrap_or("sim")
}

/// Post-exit re-open spacing, default `0`. Same one-read rule as [`harness_venue`].
fn harness_cooldown_ms(params: &Params) -> i64 {
    params.get("cooldown_ms").and_then(ParamValue::as_integer).unwrap_or(0)
}

/// Optional per-symbol venue routing: a `[strategy.params.venues]` table (`SYMBOL = "venue"`) lets
/// this CROSS-VENUE controller observe each series under its OWN venue from ONE mount. Absent ⇒
/// every symbol routes to the default `venue`.
///
/// ⚠ A row whose VALUE is not a string is dropped silently by the `filter_map` — a per-ROW coercion
/// no key-level type check reaches, which is why the echo prints the surviving map rather than the
/// table that was typed.
fn harness_venue_map(params: &Params) -> Vec<(String, String)> {
    params
        .get("venues")
        .and_then(ParamValue::as_table)
        .map(|t| {
            t.iter()
                .filter_map(|(symbol, v)| v.as_str().map(|ven| (symbol.clone(), ven.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// One venue's funding observation for one perp symbol — a [`rank_best_carry`] input row. Both
/// rates are per-notional FRACTIONS (`0.0001` = 1 bp); `taker_fee` is `≥ 0`.
#[derive(Debug, Clone, PartialEq)]
pub struct FundingQuote {
    pub venue: String,
    /// Latest funding rate (signed fraction of notional per funding interval).
    pub funding_rate: f64,
    /// Taker fee (fraction of notional per crossing).
    pub taker_fee: f64,
}

impl FundingQuote {
    pub fn new(venue: impl Into<String>, funding_rate: f64, taker_fee: f64) -> Self {
        FundingQuote { venue: venue.into(), funding_rate, taker_fee }
    }
}

/// A ranked carry: LONG `long_venue` (lower funding), SHORT `short_venue` (higher).
/// `gross_differential` (`≥ 0`) is the per-interval funding collected, `roundtrip_cost` the
/// one-time taker cost of BOTH legs, `net_edge` the scored metric ([`net_carry_edge`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RankedCarry {
    pub long_venue: String,
    pub short_venue: String,
    pub gross_differential: f64,
    pub roundtrip_cost: f64,
    pub net_edge: f64,
}

/// Round-trip taker cost of a two-leg carry (per-notional fraction): each leg crosses as a taker on
/// BOTH entry and exit, so the pair pays `2 × (long_taker + short_taker)`.
#[inline]
pub fn roundtrip_taker_cost(long_taker: f64, short_taker: f64) -> f64 {
    2.0 * (long_taker + short_taker)
}

/// The net edge of holding a carry `hold_periods` funding intervals:
/// `hold_periods × gross_differential − roundtrip_cost`. `1.0` is the conservative single-period
/// breakeven (ONE interval must clear the whole round trip).
#[inline]
pub fn net_carry_edge(gross_differential: f64, roundtrip_cost: f64, hold_periods: f64) -> f64 {
    hold_periods * gross_differential - roundtrip_cost
}

/// The best venue PAIR by [`net_carry_edge`] over `hold_periods`, oriented LONG = lower funding;
/// `None` below two venues. EVERY pair is scored because fees are per-venue: the max-GROSS pair is
/// not always the max-NET one. Ties keep the FIRST (insertion-order) candidate — replay-stable.
pub fn rank_best_carry(quotes: &[FundingQuote], hold_periods: f64) -> Option<RankedCarry> {
    let mut best: Option<RankedCarry> = None;
    for (i, a) in quotes.iter().enumerate() {
        for b in &quotes[i + 1..] {
            // Orient: LONG the lower funding, SHORT the higher (gross_differential ≥ 0). A tie
            // orients `a` (the earlier-observed venue) as the long leg — deterministic.
            let (long, short) = if a.funding_rate <= b.funding_rate { (a, b) } else { (b, a) };
            let gross = short.funding_rate - long.funding_rate;
            let cost = roundtrip_taker_cost(long.taker_fee, short.taker_fee);
            let cand = RankedCarry {
                long_venue: long.venue.clone(),
                short_venue: short.venue.clone(),
                gross_differential: gross,
                roundtrip_cost: cost,
                net_edge: net_carry_edge(gross, cost, hold_periods),
            };
            // Strictly-greater replaces, so an equal-edge later pair does NOT displace the first.
            let keep = matches!(&best, Some(current) if current.net_edge >= cand.net_edge);
            if !keep {
                best = Some(cand);
            }
        }
    }
    best
}

/// [`rank_best_carry`] filtered by `net_edge ≥ entry_threshold`; `None` = nothing worth opening.
/// A threshold of `0.0` opens any pair whose differential over `hold_periods` beats its round trip.
pub fn best_carry_to_open(
    quotes: &[FundingQuote],
    hold_periods: f64,
    entry_threshold: f64,
) -> Option<RankedCarry> {
    rank_best_carry(quotes, hold_periods).filter(|c| c.net_edge >= entry_threshold)
}

/// The position side `venue` should take in `carry`: `+1` (LONG) if it is the long leg, `−1` (SHORT)
/// if the short leg, `None` if `venue` is neither leg.
#[inline]
pub fn carry_leg_side(carry: &RankedCarry, venue: &str) -> Option<i32> {
    if carry.long_venue == venue {
        Some(1)
    } else if carry.short_venue == venue {
        Some(-1)
    } else {
        None
    }
}

/// Why a held funding carry should be CLOSED (the reasons [`should_close_carry`] returns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarryCloseReason {
    /// The funding differential REVERSED (`current_differential < 0`): the pair now PAYS to hold, so
    /// flatten immediately. The protective, most-urgent signal (checked first).
    Flipped,
    /// The accrued funding PnL reached the profit target.
    ProfitTarget,
    /// The differential COMPRESSED to/below the exit threshold (edge gone, still non-negative).
    Compressed,
}

/// Whether to CLOSE a held carry given its CURRENT differential (`short_funding − long_funding`),
/// the compression `exit_threshold`, `accrued_pnl` and an optional `profit_target`; `None` = hold.
///
/// Precedence is protective first (as [`vike_model::evaluate_barriers`]' stop-before-target):
/// **Flipped → ProfitTarget → Compressed**. A flip is defined by SIGN alone, so the compression
/// check (which a negative differential also satisfies) never masks it.
pub fn should_close_carry(
    current_differential: f64,
    exit_threshold: f64,
    accrued_pnl: f64,
    profit_target: Option<f64>,
) -> Option<CarryCloseReason> {
    if current_differential < 0.0 {
        return Some(CarryCloseReason::Flipped);
    }
    if let Some(target) = profit_target
        && accrued_pnl >= target
    {
        return Some(CarryCloseReason::ProfitTarget);
    }
    if current_differential <= exit_threshold {
        return Some(CarryCloseReason::Compressed);
    }
    None
}

/// The delta-neutral funding-carry [`Controller`] (module doc: the carry law, the seam, the
/// unwired early close). Mount it like any [`Controller`], e.g.
/// `ControllerHarness::new(FundingCarryController::new("BTCUSDT", 1.0, barriers), "binance", 0)`;
/// it is `Send`. Feed cross-venue funding via [`observe_funding`](Self::observe_funding).
#[derive(Debug, Clone)]
pub struct FundingCarryController {
    /// The perp symbol this carry trades on every venue (single-symbol; the carry is cross-VENUE).
    symbol: String,
    /// Size (units) each leg's [`PositionExecutorIntent`] carries.
    qty: f64,
    /// Funding intervals the entry economics amortize the round trip over (default `1.0`). NOT in
    /// the [`PositionControllerParams`] bag, so [`apply_params`](Controller::apply_params) leaves it alone.
    hold_periods: f64,
    /// Minimum net carry edge to OPEN. Default `0.0`. Hot-swapped from
    /// [`PositionControllerParams::threshold`].
    entry_threshold: f64,
    /// The triple barrier guarding each opened leg (its protective stop / time-limit exit).
    barriers: TripleBarrier,
    /// Latest funding rate per venue for `symbol`, in INSERTION order (linear scan, no map:
    /// replay-stable). STATE, not a tunable: [`apply_params`](Controller::apply_params) keeps it.
    funding_book: Vec<(String, f64)>,
}

impl FundingCarryController {
    /// A carry controller on `symbol`, `qty` per leg, each leg guarded by `barriers`. Defaults:
    /// `hold_periods = 1.0`, `entry_threshold = 0.0`, an empty book (opens NOTHING below two
    /// venues).
    pub fn new(symbol: impl Into<String>, qty: f64, barriers: TripleBarrier) -> Self {
        FundingCarryController {
            symbol: symbol.into(),
            qty,
            hold_periods: 1.0,
            entry_threshold: 0.0,
            barriers,
            funding_book: Vec::new(),
        }
    }

    /// Read a harness/registry params table (`BuyHold::from_params` reader convention): `symbol`
    /// (default `""`), `qty` (default `1.0`), the barrier keys of `barriers_from_params`, and
    /// `hold_periods` / `entry_threshold` (else the [`new`](Self::new) defaults). The registry
    /// wraps it in a [`ControllerHarness`] to make it a `Strategy`.
    pub fn from_params(params: &Params) -> Self {
        let f = |k: &str| params.get(k).and_then(ParamValue::as_f64);
        let symbol = params.get("symbol").and_then(ParamValue::as_str).unwrap_or("").to_string();
        let qty = f("qty").unwrap_or(1.0);
        let mut c = FundingCarryController::new(symbol, qty, barriers_from_params(params));
        if let Some(h) = f("hold_periods") {
            c = c.with_hold_periods(h);
        }
        if let Some(t) = f("entry_threshold") {
            c = c.with_entry_threshold(t);
        }
        c
    }

    /// Override the holding horizon (funding intervals) the round trip is amortized over.
    pub fn with_hold_periods(mut self, hold_periods: f64) -> Self {
        self.hold_periods = hold_periods;
        self
    }

    /// Override the minimum net carry edge to open.
    pub fn with_entry_threshold(mut self, entry_threshold: f64) -> Self {
        self.entry_threshold = entry_threshold;
        self
    }

    /// Record `venue`'s latest funding rate: a first-seen venue is appended, a re-observe
    /// overwrites its slot in place (deterministic order).
    pub fn observe_funding(&mut self, venue: &str, funding_rate: f64) {
        if let Some(slot) = self.funding_book.iter_mut().find(|(v, _)| v.as_str() == venue) {
            slot.1 = funding_rate;
        } else {
            self.funding_book.push((venue.to_string(), funding_rate));
        }
    }

    /// The book as [`FundingQuote`]s, in book order, each with its venue's TAKER fee from the
    /// static [`vike_venues::fee_schedule_for`] registry, read at the venue's fee LANE for this
    /// carry's symbol ([`vike_venues::fee_lane`]): a binance `BTCUSDT.P` leg is a USDⓈ-M perp leg.
    fn quotes(&self) -> Vec<FundingQuote> {
        self.funding_book
            .iter()
            .map(|(venue, rate)| {
                let taker_fee =
                    fee_schedule_for(fee_lane(venue, &self.symbol)).maker_taker_rates().1;
                FundingQuote { venue: venue.clone(), funding_rate: *rate, taker_fee }
            })
            .collect()
    }

    /// The best carry to open now over the observed book ([`best_carry_to_open`]), or `None`.
    pub fn best_carry(&self) -> Option<RankedCarry> {
        best_carry_to_open(&self.quotes(), self.hold_periods, self.entry_threshold)
    }

    /// The traded symbol.
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
    /// The per-leg size.
    pub fn qty(&self) -> f64 {
        self.qty
    }
    /// The assumed holding horizon (funding intervals).
    pub fn hold_periods(&self) -> f64 {
        self.hold_periods
    }
    /// The minimum net carry edge to open.
    pub fn entry_threshold(&self) -> f64 {
        self.entry_threshold
    }
    /// The per-leg triple barrier.
    pub fn barriers(&self) -> TripleBarrier {
        self.barriers
    }
}

impl Controller for FundingCarryController {
    /// When `venue` is a LEG of the best carry clearing the entry gate, that leg's correctly-sided
    /// market [`PositionExecutorIntent`]. `None` otherwise: a foreign symbol, fewer than two venues, the
    /// best edge below threshold, or `venue` not in the winning pair.
    fn evaluate<B: OrderBroker>(
        &mut self,
        broker: &B,
        venue: &str,
        symbol: &str,
    ) -> Option<PositionExecutorIntent> {
        // Auto-observe the asked venue's funding FIRST, for ANY symbol: the cross-venue book needs
        // every venue's series, including ones this controller never opens on.
        if let Some(f) = broker.bars(symbol).last().and_then(|b| b.funding) {
            self.observe_funding(venue, f);
        }
        // Symbol gate (module doc, MULTI-VENUE): SET ⇒ single-leg on that symbol, EMPTY ⇒ two-leg.
        // It gates OPENING only, never the book.
        if !self.symbol.is_empty() && symbol != self.symbol.as_str() {
            return None;
        }
        let carry = self.best_carry()?;
        let side = carry_leg_side(&carry, venue)?; // None ⇒ `venue` is not a leg of the best carry
        Some(PositionExecutorIntent::market(venue, symbol, side, self.qty, self.barriers))
    }

    /// Hot-swap from a live re-tune: `qty`, `barriers`, and [`PositionControllerParams::threshold`] as
    /// `entry_threshold` (the generic decision-knob slot, as `MomentumController` reads it).
    /// `cooldown_ms` is the HARNESS's; `hold_periods` and the `funding_book` STATE are preserved.
    /// Future opens only.
    fn apply_params(&mut self, params: &PositionControllerParams) {
        self.qty = params.qty;
        self.barriers = params.barriers;
        self.entry_threshold = params.threshold;
    }
}

// Copied from `crates/vike-strategy/src/strategies/funding_carry_tests.rs`. Run by `crates/vike-strategies-check`
// (`cargo test -p vike-strategies-check`); a build of this file compiles no test.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{OrderBrokerRecording, untagged_limits, untagged_markets};
    use vike_marketdata::Bar;
    use vike_model::Strategy;
    use vike_model::strategy::OrderBrokerMock;
    use vike_strategy::PositionExecutorEntryKind;

    // ---- pure helpers ----

    fn q(venue: &str, funding_rate: f64, taker_fee: f64) -> FundingQuote {
        FundingQuote::new(venue, funding_rate, taker_fee)
    }

    fn bar(ts: i64, symbol: &str, close: f64) -> Bar {
        Bar {
            timestamp: ts,
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(symbol.to_string()),
        }
    }

    // ============================================================================================
    // Pure decision core — round-trip cost, net edge, ranking, direction, exit
    // ============================================================================================

    #[test]
    fn roundtrip_cost_sums_both_legs_twice() {
        // each leg crosses twice (entry + exit) → 2 × (long_taker + short_taker)
        assert_eq!(roundtrip_taker_cost(0.001, 0.0005), 2.0 * (0.001 + 0.0005));
        assert_eq!(roundtrip_taker_cost(0.0, 0.0), 0.0);
    }

    #[test]
    fn net_edge_amortizes_cost_over_periods() {
        // hold_periods scales the funding collected; the cost is one-time.
        assert_eq!(net_carry_edge(0.001, 0.002, 1.0), 1.0 * 0.001 - 0.002);
        assert_eq!(net_carry_edge(0.001, 0.002, 8.0), 8.0 * 0.001 - 0.002);
        // a single-period differential below the round-trip cost is NEGATIVE (conservative default).
        assert!(net_carry_edge(0.0008, 0.0031, 1.0) < 0.0);
        // holding longer flips the same pair positive.
        assert!(net_carry_edge(0.0008, 0.0031, 10.0) > 0.0);
    }

    #[test]
    fn rank_none_below_two_venues() {
        assert_eq!(rank_best_carry(&[], 1.0), None);
        assert_eq!(rank_best_carry(&[q("binance", 0.001, 0.0005)], 1.0), None);
    }

    #[test]
    fn rank_longs_the_low_shorts_the_high() {
        // binance funding +0.0001, bybit +0.0006 → long binance (low), short bybit (high).
        let quotes = [q("binance", 0.0001, 0.0), q("bybit", 0.0006, 0.0)];
        let c = rank_best_carry(&quotes, 1.0).unwrap();
        assert_eq!(c.long_venue, "binance");
        assert_eq!(c.short_venue, "bybit");
        assert_eq!(c.gross_differential, 0.0006 - 0.0001);
        assert_eq!(c.roundtrip_cost, 0.0);
        assert_eq!(c.net_edge, 0.0006 - 0.0001);
    }

    #[test]
    fn rank_orients_regardless_of_observation_order() {
        // the HIGH-funding venue observed FIRST must still become the short leg.
        let quotes = [q("bybit", 0.0006, 0.0), q("binance", 0.0001, 0.0)];
        let c = rank_best_carry(&quotes, 1.0).unwrap();
        assert_eq!(c.long_venue, "binance", "lower funding is always the long leg");
        assert_eq!(c.short_venue, "bybit", "higher funding is always the short leg");
        assert_eq!(c.gross_differential, 0.0006 - 0.0001);
    }

    #[test]
    fn rank_handles_negative_funding_long_leg() {
        // negative-funding leg (shorts pay longs) is the natural long leg; differential widens.
        let quotes = [q("binance", -0.0002, 0.0), q("bybit", 0.0006, 0.0)];
        let c = rank_best_carry(&quotes, 1.0).unwrap();
        assert_eq!(c.long_venue, "binance");
        assert_eq!(c.short_venue, "bybit");
        assert_eq!(c.gross_differential, 0.0006 - (-0.0002));
    }

    #[test]
    fn rank_picks_max_net_edge_not_max_gross() {
        // A (0.0), B (0.010), C (0.011) funding; C is a HIGH-fee venue.
        //   A-B: gross 0.010, cost 0                 → net 0.010   (best NET)
        //   A-C: gross 0.011, cost 2*0.010 = 0.020   → net -0.009  (best GROSS, worst net)
        //   B-C: gross 0.001, cost 0.020             → net -0.019
        // So the ranking must prefer A-B (max net) over A-C (max gross) — fees change the winner.
        let quotes = [q("A", 0.0, 0.0), q("B", 0.010, 0.0), q("C", 0.011, 0.010)];
        let c = rank_best_carry(&quotes, 1.0).unwrap();
        assert_eq!((c.long_venue.as_str(), c.short_venue.as_str()), ("A", "B"));
        assert!((c.net_edge - 0.010).abs() < 1e-12, "net edge {}", c.net_edge);
    }

    #[test]
    fn rank_tie_breaks_on_first_pair() {
        // funding [0.0, 0.005, 0.0, 0.005], zero fees: SEVERAL pairs share the maximal net edge
        // 0.005 (A-B, A-D, C-B, C-D). The FIRST-scanned (A-B, i=0/j=1) must win — strictly-greater
        // replacement means a later equal-edge pair never displaces it.
        let quotes = [q("A", 0.0, 0.0), q("B", 0.005, 0.0), q("C", 0.0, 0.0), q("D", 0.005, 0.0)];
        let c = rank_best_carry(&quotes, 1.0).unwrap();
        assert_eq!((c.long_venue.as_str(), c.short_venue.as_str()), ("A", "B"));
    }

    #[test]
    fn best_carry_to_open_gates_on_threshold() {
        // funding 0.0 vs 0.01, zero taker fees → net edge is EXACTLY 0.01 (1.0*(0.01-0.0) - 0.0),
        // so the boundary compare is bit-exact (no subtraction rounding at the threshold).
        let quotes = [q("binance", 0.0, 0.0), q("bybit", 0.01, 0.0)];
        // below threshold → declined
        assert_eq!(best_carry_to_open(&quotes, 1.0, 0.02), None);
        // exactly at threshold → opened
        assert!(best_carry_to_open(&quotes, 1.0, 0.01).is_some());
        // default zero threshold → opened
        assert!(best_carry_to_open(&quotes, 1.0, 0.0).is_some());
    }

    #[test]
    fn carry_leg_side_maps_venue_to_direction() {
        let c = RankedCarry {
            long_venue: "binance".into(),
            short_venue: "bybit".into(),
            gross_differential: 0.0005,
            roundtrip_cost: 0.0,
            net_edge: 0.0005,
        };
        assert_eq!(carry_leg_side(&c, "binance"), Some(1), "long leg is +1");
        assert_eq!(carry_leg_side(&c, "bybit"), Some(-1), "short leg is -1");
        assert_eq!(carry_leg_side(&c, "okx"), None, "a non-leg venue declines");
    }

    #[test]
    fn close_holds_while_edge_intact() {
        // differential still wide, no target hit → hold.
        assert_eq!(should_close_carry(0.0006, 0.0001, 0.0, Some(100.0)), None);
    }

    #[test]
    fn close_on_compression() {
        // differential compressed to/below the exit threshold (still non-negative) → Compressed.
        assert_eq!(
            should_close_carry(0.0001, 0.0001, 0.0, None),
            Some(CarryCloseReason::Compressed)
        );
        assert_eq!(
            should_close_carry(0.00005, 0.0001, 0.0, None),
            Some(CarryCloseReason::Compressed)
        );
    }

    #[test]
    fn close_on_flip_takes_precedence_over_compression_and_target() {
        // negative differential = the pair now PAYS → Flipped, even though a compression check and
        // an already-hit profit target would ALSO fire (Flipped is checked first).
        assert_eq!(
            should_close_carry(-0.0001, 0.0005, 999.0, Some(1.0)),
            Some(CarryCloseReason::Flipped)
        );
    }

    #[test]
    fn close_on_profit_target_before_compression() {
        // edge still intact (above exit threshold) but the accrued PnL reached the target → close.
        assert_eq!(
            should_close_carry(0.0006, 0.0001, 100.0, Some(100.0)),
            Some(CarryCloseReason::ProfitTarget)
        );
        // below target with the edge intact → hold.
        assert_eq!(should_close_carry(0.0006, 0.0001, 99.0, Some(100.0)), None);
    }

    // ============================================================================================
    // Controller over the funding book (taker fees pulled from the fee registry)
    // ============================================================================================

    /// A large-differential carry that clears real binance/bybit taker fees in ONE period:
    /// binance funding −0.001 (long leg), bybit +0.005 (short leg).
    ///   gross = 0.005 − (−0.001) = 0.006
    ///   cost  = 2 × (binance taker 0.0010 + bybit taker 0.00055) = 0.0031
    ///   net(1 period) = 0.006 − 0.0031 = 0.0029 > 0  → opens at the default zero threshold.
    fn loaded_controller() -> FundingCarryController {
        let mut c = FundingCarryController::new(
            "BTCUSDT",
            1.0,
            TripleBarrier::new(None, Some(50.0), Some(28_800_000), None),
        );
        c.observe_funding("binance", -0.001);
        c.observe_funding("bybit", 0.005);
        c
    }

    #[test]
    fn controller_builds_quotes_with_registry_taker_fees() {
        let c = loaded_controller();
        let best = c.best_carry().expect("a profitable carry");
        assert_eq!(best.long_venue, "binance", "negative-funding binance is the long leg");
        assert_eq!(best.short_venue, "bybit", "positive-funding bybit is the short leg");
        assert_eq!(best.gross_differential, 0.005 - (-0.001));
        // real registry taker fees: binance 10 bps, bybit 5.5 bps → 2*(0.001 + 0.00055). The rates
        // come from `bps/10_000.0` divisions, so compare within an ulp-tolerant epsilon.
        assert!((best.roundtrip_cost - 2.0 * (0.001 + 0.00055)).abs() < 1e-15);
        assert!(best.net_edge > 0.0);
    }

    /// The taker fee is read at each venue's fee LANE for the carry's symbol (`vike_venues::fee_lane`): a
    /// perp carry (`BTCUSDT.P`) pays binance's USDⓈ-M taker, 5 bps, where the bare venue it keyed before
    /// priced the leg at binance SPOT's 10. bybit's one row is its perp row either way.
    #[test]
    fn a_perp_carry_prices_each_venue_at_its_lane() {
        let mut c = FundingCarryController::new(
            "BTCUSDT.P",
            1.0,
            TripleBarrier::new(None, Some(50.0), Some(28_800_000), None),
        );
        c.observe_funding("binance", -0.001);
        c.observe_funding("bybit", 0.005);
        let best = c.best_carry().expect("a profitable carry");
        assert!(
            (best.roundtrip_cost - 2.0 * (0.0005 + 0.00055)).abs() < 1e-15,
            "binance perp 5 bps + bybit 5.5 bps, both legs twice: {}",
            best.roundtrip_cost
        );
    }

    #[test]
    fn controller_opens_the_long_leg_on_the_low_funding_venue() {
        let mut c = loaded_controller();
        let b = OrderBrokerMock { price: 30_000.0, ..Default::default() }; // book-less → observe_funding only
        // asked about binance (the low-funding leg) → LONG intent at the configured size.
        let intent = Controller::evaluate(&mut c, &b, "binance", "BTCUSDT").expect("opens the leg");
        assert_eq!(intent.venue, "binance");
        assert_eq!(intent.symbol, "BTCUSDT");
        assert_eq!(intent.side, 1, "long the low-funding leg");
        assert_eq!(intent.qty, 1.0);
        assert_eq!(intent.entry, PositionExecutorEntryKind::Market);
        assert_eq!(intent.barriers.stop_loss, Some(50.0));
    }

    #[test]
    fn controller_opens_the_short_leg_on_the_high_funding_venue() {
        let mut c = loaded_controller();
        let b = OrderBrokerMock { price: 30_000.0, ..Default::default() };
        // asked about bybit (the high-funding leg) → SHORT intent.
        let intent = Controller::evaluate(&mut c, &b, "bybit", "BTCUSDT").expect("opens the leg");
        assert_eq!(intent.side, -1, "short the high-funding leg");
        assert_eq!(intent.venue, "bybit");
    }

    #[test]
    fn controller_declines_a_venue_outside_the_winning_pair() {
        let mut c = loaded_controller();
        c.observe_funding("okx", 0.002); // a third venue, not a leg of the best (binance/bybit) pair
        let b = OrderBrokerMock::default();
        // okx sits between the extremes → not in the winning pair → declines.
        assert_eq!(Controller::evaluate(&mut c, &b, "okx", "BTCUSDT"), None);
        // and the winning legs still open.
        assert!(Controller::evaluate(&mut c, &b, "binance", "BTCUSDT").is_some());
        assert!(Controller::evaluate(&mut c, &b, "bybit", "BTCUSDT").is_some());
    }

    #[test]
    fn controller_declines_a_foreign_symbol() {
        let mut c = loaded_controller();
        let b = OrderBrokerMock::default();
        assert_eq!(Controller::evaluate(&mut c, &b, "binance", "ETHUSDT"), None);
    }

    #[test]
    fn controller_declines_when_differential_below_fees() {
        // a thin differential that does NOT clear the round-trip taker cost in one period.
        let mut c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
        c.observe_funding("binance", 0.0001);
        c.observe_funding("bybit", 0.0003); // gross 0.0002 ≪ cost 0.0031 → net < 0
        let b = OrderBrokerMock::default();
        assert_eq!(c.best_carry(), None, "net edge below zero threshold → no carry");
        assert_eq!(Controller::evaluate(&mut c, &b, "binance", "BTCUSDT"), None);
    }

    #[test]
    fn controller_auto_observes_the_asked_venue_from_bar_funding() {
        // A broker whose latest bar carries funding auto-populates the asked venue's book slot, so a
        // single explicit observe of the OTHER venue is enough to form a pair.
        let mut c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
        c.observe_funding("bybit", 0.005); // the other leg, explicit
        let mut only_bar = bar(1, "BTCUSDT", 30_000.0);
        only_bar.funding = Some(-0.001); // binance's funding arrives via the bar
        let b =
            OrderBrokerRecording { price: 30_000.0, bars: vec![only_bar], ..Default::default() };
        // evaluate for binance auto-observes -0.001 from the bar, forming the binance/bybit pair.
        let intent = Controller::evaluate(&mut c, &b, "binance", "BTCUSDT").expect("pair formed");
        assert_eq!(intent.side, 1, "binance long via bar-observed funding");
    }

    #[test]
    fn apply_params_hot_swaps_quantity_barriers_threshold_and_preserves_state() {
        let mut c = loaded_controller();
        assert_eq!(c.qty(), 1.0);
        assert_eq!(c.entry_threshold(), 0.0);
        assert_eq!(c.hold_periods(), 1.0, "starts at the default hold horizon");

        let p = PositionControllerParams::new(
            5_000,                                            // cooldown — the HARNESS's, ignored here
            3.0,                                              // qty 1 → 3
            TripleBarrier::new(Some(20.0), None, None, None), // barriers re-armed
            0.01,                                             // threshold → entry_threshold
        );
        Controller::apply_params(&mut c, &p);

        assert_eq!(c.qty(), 3.0, "size hot-swapped");
        assert_eq!(
            c.entry_threshold(),
            0.01,
            "entry threshold hot-swapped from PositionControllerParams"
        );
        assert_eq!(c.barriers().take_profit, Some(20.0), "barriers hot-swapped");
        assert_eq!(c.hold_periods(), 1.0, "hold_periods (not in the bag) preserved");
        // the funding book (evolving STATE) is preserved, so the pair still ranks.
        assert!(c.best_carry().is_none(), "0.006 gross now below the new 0.01 threshold");
        // widen the differential enough to clear the new threshold → still ranks binance/bybit.
        c.observe_funding("bybit", 0.02);
        let best = c.best_carry().expect("clears the new threshold");
        assert_eq!(best.long_venue, "binance");
    }

    // ============================================================================================
    // Harness integration + OFF/default (byte-identical when off) proofs
    // ============================================================================================

    #[test]
    fn harness_opens_one_sided_leg_for_its_venue() {
        // Mount the controller in the EXISTING single-venue harness under "binance": it opens ONLY
        // the binance (long) leg — the documented single-venue-harness behavior.
        let mut c = FundingCarryController::new("BTCUSDT", 2.0, TripleBarrier::none());
        c.observe_funding("binance", -0.001);
        c.observe_funding("bybit", 0.005);
        let mut h = ControllerHarness::new(c, "binance", 0);
        let mut b = OrderBrokerMock { now: 1, price: 30_000.0, ..Default::default() };
        Strategy::on_bar(&mut h, &mut b, &bar(1, "BTCUSDT", 30_000.0));
        assert_eq!(h.active_count(), 1, "one leg opened");
        assert_eq!(
            untagged_markets(&b),
            vec![("BTCUSDT".to_string(), 1, 2.0)],
            "a single long market entry at the configured size"
        );
    }

    #[test]
    fn off_default_unfed_controller_places_no_orders() {
        // OFF / byte-identical-when-off proof: a freshly-constructed controller (no funding observed)
        // mounted in a harness and fed bars places ZERO orders — inert, exactly like a mount with no
        // strategy. Nothing opens until a profitable cross-venue carry is fed in.
        let c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
        let mut h = ControllerHarness::new(c, "binance", 0);
        let mut b = OrderBrokerMock { now: 1, price: 30_000.0, ..Default::default() };
        for ts in 1..=5 {
            Strategy::on_bar(&mut h, &mut b, &bar(ts, "BTCUSDT", 30_000.0));
        }
        assert_eq!(h.active_count(), 0, "no executor without a fed carry");
        assert!(untagged_markets(&b).is_empty(), "no market orders");
        assert!(untagged_limits(&b).is_empty(), "no limit orders");
    }

    #[test]
    fn off_single_venue_never_opens() {
        // A carry needs a PAIR: with only ONE venue observed the controller stays inert (no second
        // leg to rank against), regardless of how attractive that one venue's funding is.
        let mut c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
        c.observe_funding("binance", -0.05); // a huge negative funding, but still a single venue
        let b = OrderBrokerMock::default();
        assert_eq!(c.best_carry(), None, "one venue can never form a delta-neutral pair");
        assert_eq!(Controller::evaluate(&mut c, &b, "binance", "BTCUSDT"), None, "no leg opened");
    }
}
