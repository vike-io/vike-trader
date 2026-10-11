//! Funding-rate CARRY controller — the #517 controller → executor seam consuming the #521 live
//! funding feed: rank the observed venues by perp FUNDING-RATE DIFFERENTIAL net of round-trip taker
//! fees, then open a delta-neutral pair — **LONG the low/negative-funding leg, SHORT the
//! high/positive-funding leg** — collecting the differential each settlement while the legs' price
//! exposure cancels. It only DECIDES: the runtime books the funding
//! (`vike_exec::Account::apply_funding` adds `FundingEvent.amount` to the balance).
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
//! clearing `entry_threshold`, `evaluate` returns that leg's correctly-SIDED [`PositionIntent`] for
//! [`crate::ControllerHarness`] to open as a [`crate::PositionExecutor`].
//!
//! MULTI-VENUE (WIRED): the harness's per-symbol `venue_map` (`symbol -> venue`) makes ONE mount
//! ask `evaluate` per `(venue, symbol)`, filling the cross-venue book. `symbol` EMPTY (the default)
//! ⇒ TWO-LEG delta-neutral, one executor per leg of the best carry; `symbol` SET ⇒ SINGLE-LEG (only
//! that symbol's leg; the opposite venue's price exposure stays open). The book observes every
//! venue either way. Backtest: `engine.attach_funding` + a `[strategy.params.venues]` table (see
//! `crates/vike-backtest/tests/funding_carry_demo.rs`).
//!
//! DELIBERATE LIMITATION: the COMPRESSION / FLIP close ([`should_close_carry`]) is a pure, tested
//! function but NOT wired — [`Controller::evaluate`] is invited only while a pair is FLAT, so a
//! held carry exits via its leg [`vike_model::TripleBarrier`]. Wiring it needs a controller-level
//! per-settlement hook.
//!
//! OFF / additive: nothing mounts it by default; an under-fed controller (fewer than two venues, or
//! the best net edge below threshold) emits NO intent. No Python twin.

use vike_model::{Broker, ControllerParams, TripleBarrier, fee_schedule_for};

use crate::controller::{Controller, as_f64, barriers_from_params};
use crate::position_executor::PositionIntent;
use toml::Value;

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
            let keep = matches!(&best, Some(cur) if cur.net_edge >= cand.net_edge);
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
/// Precedence is protective first (as [`crate::evaluate_barriers`]' stop-before-target):
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
/// unwired early close). Mount it like [`crate::MomentumController`], e.g.
/// `ControllerHarness::new(FundingCarryController::new("BTCUSDT", 1.0, barriers), "binance", 0)`;
/// it is `Send`. Feed cross-venue funding via [`observe_funding`](Self::observe_funding).
#[derive(Debug, Clone)]
pub struct FundingCarryController {
    /// The perp symbol this carry trades on every venue (single-symbol; the carry is cross-VENUE).
    symbol: String,
    /// Size (units) each leg's [`PositionIntent`] carries.
    qty: f64,
    /// Funding intervals the entry economics amortize the round trip over (default `1.0`). NOT in
    /// the [`ControllerParams`] bag, so [`apply_params`](Controller::apply_params) leaves it alone.
    hold_periods: f64,
    /// Minimum net carry edge to OPEN. Default `0.0`. Hot-swapped from
    /// [`ControllerParams::threshold`].
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
    /// wraps it in a [`ControllerHarness`](crate::ControllerHarness) to make it a `Strategy`.
    pub fn from_params(params: &Value) -> Self {
        let f = |k: &str| params.get(k).and_then(as_f64);
        let symbol = params.get("symbol").and_then(Value::as_str).unwrap_or("").to_string();
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
    /// static [`vike_model::fee_schedule_for`] registry.
    fn quotes(&self) -> Vec<FundingQuote> {
        self.funding_book
            .iter()
            .map(|(venue, rate)| {
                let taker_fee = fee_schedule_for(venue).maker_taker_rates().1;
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
    /// market [`PositionIntent`]. `None` otherwise: a foreign symbol, fewer than two venues, the
    /// best edge below threshold, or `venue` not in the winning pair.
    fn evaluate<B: Broker>(
        &mut self,
        broker: &B,
        venue: &str,
        symbol: &str,
    ) -> Option<PositionIntent> {
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
        Some(PositionIntent::market(venue, symbol, side, self.qty, self.barriers))
    }

    /// Hot-swap from a live re-tune: `qty`, `barriers`, and [`ControllerParams::threshold`] as
    /// `entry_threshold` (the generic decision-knob slot, as `MomentumController` reads it).
    /// `cooldown_ms` is the HARNESS's; `hold_periods` and the `funding_book` STATE are preserved.
    /// Future opens only.
    fn apply_params(&mut self, params: &ControllerParams) {
        self.qty = params.qty;
        self.barriers = params.barriers;
        self.entry_threshold = params.threshold;
    }
}

#[path = "funding_carry_tests.rs"]
#[cfg(test)]
mod funding_carry_tests;
