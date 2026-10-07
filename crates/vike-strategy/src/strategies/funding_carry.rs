//! Funding-rate CARRY controller — the #517 controller → executor seam consuming the #521 live
//! funding feed that nothing currently reads.
//!
//! A delta-neutral funding-carry [`Controller`] (the WHAT): rank the observed venues by their perp
//! FUNDING-RATE DIFFERENTIAL net of round-trip taker fees, then open a two-leg, price-neutral pair —
//! **LONG the low/negative-funding leg, SHORT the high/positive-funding leg** — so the pair collects
//! the differential each funding settlement while the two legs' price exposure cancels (delta-neutral;
//! the safety-default-ON fit). The differential itself is booked automatically: the runtime already
//! folds `Event::Funding` settlements into the account cash (`vike_exec::Account::apply_funding`
//! adds `FundingEvent.amount` to the balance), so this controller only DECIDES — it never folds
//! funding itself.
//!
//! ## Sign conventions (the funding-carry law, frozen here)
//! Funding-rate convention (every wired perp venue): a POSITIVE `funding_rate` means **longs pay
//! shorts** (`amount = funding_rate × notional` flows long → short). So per funding interval, per
//! unit notional:
//! - a LONG position's funding cashflow is `−funding_rate` (it PAYS when the rate is positive);
//! - a SHORT position's funding cashflow is `+funding_rate` (it RECEIVES when the rate is positive).
//!
//! For a carry that LONGs venue `L` and SHORTs venue `S`, the per-interval collected funding is
//! therefore `(+funding_S) + (−funding_L) = funding_S − funding_L` — the **gross differential**. It
//! is maximized (and non-negative) by orienting the LONG leg onto the LOWER funding rate and the
//! SHORT leg onto the HIGHER one, which is exactly what [`rank_best_carry`] does.
//!
//! Round-trip taker cost: each leg crosses as a taker on BOTH entry and exit (2 crossings), so a
//! two-leg carry pays `2 × (taker_L + taker_S)` in per-notional fraction terms
//! ([`roundtrip_taker_cost`]). The scored **net edge** amortizes that one-time cost over the funding
//! collected across an assumed holding of `hold_periods` intervals
//! (`hold_periods × gross_differential − roundtrip_cost`, [`net_carry_edge`]) — the ranking metric,
//! and the entry gate against `entry_threshold`.
//!
//! ## How it maps onto the [`Controller`] seam (and what is a follow-up)
//! [`FundingCarryController`] carries a small per-venue FUNDING BOOK (latest observed `funding_rate`
//! per venue for the traded symbol), fed two ways:
//! - [`FundingCarryController::observe_funding`] — the explicit seam a live mount drives from the
//!   funding feed (`Event::Funding.funding_rate`) or a recorded `Bar.funding`; and
//! - inside [`Controller::evaluate`], the harness's OWN venue is auto-observed from the latest
//!   recorded bar (`broker.bars(symbol).last().funding`) when a bar carries one (a book-less broker
//!   like the test `MockBroker` simply relies on `observe_funding`).
//!
//! `evaluate` ranks the book and, when the asked `venue` is a LEG of the best carry that clears
//! `entry_threshold`, returns the correctly-SIDED [`PositionIntent`] (long the low leg, short the
//! high leg) for the existing [`crate::ControllerHarness`] to open as a [`crate::PositionExecutor`].
//!
//! MULTI-VENUE (the cross-venue delta-neutral pair, WIRED): [`crate::ControllerHarness`] carries an
//! optional per-symbol `venue_map` (`symbol -> venue`), so ONE mount asks `evaluate` per
//! `(venue, symbol)` under each series' OWN venue and the cross-venue funding book fills from a
//! single harness. Two `symbol` modes:
//! - `symbol` EMPTY (the default) ⇒ TWO-LEG delta-neutral: a leg opens on EVERY venue that is a leg
//!   of the best carry (long the low-funding venue, short the high-funding one), one executor per
//!   leg — the true cross-venue carry from one mount.
//! - `symbol` SET ⇒ SINGLE-LEG: open only that symbol's leg (directional funding capture; the
//!   opposite venue's price exposure is left open). The book still observes every venue.
//!
//! In the backtest this is driven by `engine.attach_funding` + a `[strategy.params.venues]` table
//! (see `crates/vike-backtest/tests/funding_carry_demo.rs`).
//!
//! DELIBERATE LIMITATION (documented, not silently swallowed — reported as a follow-up):
//! - The differential-COMPRESSION / FLIP close is a pure decision function ([`should_close_carry`])
//!   but is NOT wired through today's seam: [`Controller::evaluate`] is invited only while a pair is
//!   FLAT, so a held carry exits via its leg [`vike_model::TripleBarrier`] (time-limit re-evaluation /
//!   protective stop), not via a funding-differential re-check. Wiring the funding-driven early close
//!   needs a controller-level per-settlement hook (a later `Controller` extension). The pure exit
//!   core is built + tested here so that wiring is a thin follow-up.
//!
//! ## OFF / additive
//! A NET-NEW [`Controller`] type; NOTHING mounts it by default, so a default build is byte-identical.
//! An unconfigured / under-fed controller (fewer than two venues observed, or the best net edge below
//! threshold) emits NO intent — the same inert behavior a mount with no strategy has. RUST-NATIVE —
//! no Python twin (like [`crate::controller::MomentumController`]).

use vike_model::{Broker, ControllerParams, TripleBarrier, fee_schedule_for};

use crate::controller::{Controller, as_f64, barriers_from_params};
use crate::position_executor::PositionIntent;
use toml::Value;

/// One venue's funding observation for a single perp symbol — the input row [`rank_best_carry`]
/// ranks. Both rates are per-notional FRACTIONS (e.g. `0.0001` = 1 bp): `funding_rate` is SIGNED
/// (positive ⇒ longs pay shorts, see the module sign law); `taker_fee` is the per-crossing taker
/// cost (`≥ 0`).
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

/// A ranked delta-neutral funding-carry candidate: LONG `long_venue` (the lower-funding leg), SHORT
/// `short_venue` (the higher-funding leg). `gross_differential = short_funding − long_funding` (`≥ 0`
/// by construction) is the per-interval funding collected; `roundtrip_cost` is the one-time round-trip
/// taker cost of BOTH legs; `net_edge = hold_periods × gross_differential − roundtrip_cost` is the
/// scored metric the ranking maximizes.
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

/// The scored net edge of holding a carry for `hold_periods` funding intervals: the funding collected
/// (`hold_periods × gross_differential`) minus the one-time `roundtrip_cost`. `hold_periods = 1.0` is
/// the conservative single-period breakeven — the pair must clear its ENTIRE round-trip taker cost
/// from ONE interval's differential.
#[inline]
pub fn net_carry_edge(gross_differential: f64, roundtrip_cost: f64, hold_periods: f64) -> f64 {
    hold_periods * gross_differential - roundtrip_cost
}

/// Rank every venue PAIR by net carry edge and return the single best (long leg, short leg), or
/// `None` when fewer than two venues are known. Each unordered pair is oriented LONG = lower-funding
/// leg / SHORT = higher-funding leg (so `gross_differential ≥ 0`), scored by [`net_carry_edge`] over
/// `hold_periods`. Because the round-trip cost is per-venue, the max-GROSS pair is not always the
/// max-NET pair (a fat differential onto a high-fee venue can lose to a thinner one onto cheap
/// venues) — so all pairs are scored, not just the extremes. Ties keep the FIRST (insertion-order)
/// candidate — a deterministic, replay-stable linear scan (few venues; the crate's "no map dep"
/// convention).
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

/// The best carry that CLEARS the entry gate: [`rank_best_carry`] filtered by `net_edge ≥
/// entry_threshold`. `None` = nothing worth opening (fewer than two venues, or the best net edge is
/// below threshold). A threshold of `0.0` opens any pair whose funding differential (over
/// `hold_periods`) beats its round-trip taker cost.
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

/// Decide whether to CLOSE a held carry, given the pair's CURRENT funding differential
/// (`short_funding − long_funding` for the held legs), the compression `exit_threshold`, the
/// `accrued_pnl` collected so far, and an optional `profit_target`. `None` = hold.
///
/// Precedence (fixed, deterministic — protective first, mirroring [`crate::evaluate_barriers`]'s
/// stop-before-target rule): **Flipped → ProfitTarget → Compressed**. A flip (`differential < 0`) is
/// defined purely by sign, so it fires regardless of `exit_threshold` and is never masked by the
/// compression check (which a negative differential would also satisfy).
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

/// The delta-neutral funding-carry [`Controller`] — see the module docs for the carry law, the
/// [`Controller`]-seam mapping, and the documented single-venue-harness / early-close follow-ups.
///
/// Mount it (per leg venue) exactly like [`crate::MomentumController`]:
/// `ControllerHarness::new(FundingCarryController::new("BTCUSDT", 1.0, barriers), "binance", 0)`.
/// It is `Send` (only `String`/`f64`/`Vec` fields), so it is a valid boxed live strategy inside a
/// harness. Feed cross-venue funding via [`observe_funding`](Self::observe_funding).
#[derive(Debug, Clone)]
pub struct FundingCarryController {
    /// The perp symbol this carry trades on every venue (single-symbol; the carry is cross-VENUE).
    symbol: String,
    /// Size (units) each leg's [`PositionIntent`] carries.
    qty: f64,
    /// Funding intervals the entry economics assume the carry is held — amortizes the one-time
    /// round-trip taker cost across the collected funding. Default `1.0` (conservative single-period
    /// breakeven). NOT part of the [`ControllerParams`] live bag (which has no such field), so
    /// [`apply_params`](Controller::apply_params) leaves it untouched — a constructor knob only, a
    /// future additive `ControllerParams` field.
    hold_periods: f64,
    /// Minimum net carry edge (funding collected − round-trip taker cost, over `hold_periods`) to
    /// OPEN. Default `0.0`. Hot-swapped from [`ControllerParams::threshold`].
    entry_threshold: f64,
    /// The triple barrier each opened leg is guarded by (a per-leg protective stop / time-limit
    /// re-evaluation — the emulated exit within the current single-position executor seam).
    barriers: TripleBarrier,
    /// Latest observed funding rate per venue for `symbol`, in INSERTION order (few venues, linear
    /// scan, no map dep — the crate's reproducible-replay convention). Evolving STATE, not a tunable:
    /// [`apply_params`](Controller::apply_params) preserves it (like `MomentumController::last_ref`).
    funding_book: Vec<(String, f64)>,
}

impl FundingCarryController {
    /// A carry controller trading `symbol`, sizing each leg at `qty`, guarding each opened leg with
    /// `barriers`. Defaults: `hold_periods = 1.0`, `entry_threshold = 0.0`, an empty funding book
    /// (so it opens NOTHING until at least two venues are observed).
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

    /// Read a harness/registry TOML params table into a carry controller (the `BuyHold::from_params`
    /// reader convention — unknown keys ignored, missing keys default): `symbol` (default `""`), `qty`
    /// (default `1.0`), the triple-barrier legs `tp`/`sl`/`time_limit_ms`/`trailing` (all optional — see
    /// [`barriers_from_params`]), and the two carry knobs `hold_periods` / `entry_threshold` (applied
    /// only when present, else the [`new`](Self::new) defaults `1.0` / `0.0`). The backtest registry
    /// wraps the result in a [`ControllerHarness`](crate::ControllerHarness) to make it a `Strategy`.
    ///
    /// SINGLE-LEG in a lone backtest (see the module doc's DELIBERATE LIMITATIONS): the single-venue
    /// harness opens only the leg on ITS own `venue`, so a full cross-venue delta-neutral pair needs
    /// the multi-venue-harness follow-up — a `funding_carry` mount is a documented single-leg run.
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

    /// Override the assumed holding horizon (funding intervals) the entry economics amortize the
    /// round-trip taker cost over (default `1.0`). Builder form: `...::new(..).with_hold_periods(8.0)`.
    pub fn with_hold_periods(mut self, hold_periods: f64) -> Self {
        self.hold_periods = hold_periods;
        self
    }

    /// Override the minimum net carry edge to open (default `0.0`). Builder form:
    /// `...::new(..).with_entry_threshold(0.0005)`.
    pub fn with_entry_threshold(mut self, entry_threshold: f64) -> Self {
        self.entry_threshold = entry_threshold;
        self
    }

    /// Record/overwrite a venue's latest funding rate for the traded symbol — the seam the live
    /// funding feed (`Event::Funding.funding_rate`) / a recorded `Bar.funding` feeds. Deterministic
    /// insertion order: a first-seen venue is appended; a re-observe overwrites its slot in place.
    pub fn observe_funding(&mut self, venue: &str, funding_rate: f64) {
        if let Some(slot) = self.funding_book.iter_mut().find(|(v, _)| v.as_str() == venue) {
            slot.1 = funding_rate;
        } else {
            self.funding_book.push((venue.to_string(), funding_rate));
        }
    }

    /// Build the ranked [`FundingQuote`] list from the funding book, pulling each venue's TAKER fee
    /// from the static [`vike_model::fee_schedule_for`] registry (`maker_taker_rates().1` — the
    /// per-notional taker fraction). Deterministic order (mirrors the book).
    fn quotes(&self) -> Vec<FundingQuote> {
        self.funding_book
            .iter()
            .map(|(venue, rate)| {
                let taker_fee = fee_schedule_for(venue).maker_taker_rates().1;
                FundingQuote { venue: venue.clone(), funding_rate: *rate, taker_fee }
            })
            .collect()
    }

    /// The best carry to open right now given the observed funding book, or `None` (fewer than two
    /// venues, or the best net edge below `entry_threshold`). Exposed for tests and a future
    /// two-leg / two-harness mount.
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
    /// Rank the funding book and, when `venue` is a LEG of the best carry that clears the entry gate,
    /// return that leg's correctly-sided market [`PositionIntent`] (LONG the low-funding leg, SHORT
    /// the high-funding leg). `None` otherwise: a foreign symbol, fewer than two venues observed, the
    /// best edge below threshold, or `venue` not in the winning pair.
    fn evaluate<B: Broker>(
        &mut self,
        broker: &B,
        venue: &str,
        symbol: &str,
    ) -> Option<PositionIntent> {
        // Auto-observe the asked venue's OWN funding from its latest recorded bar FIRST — for ANY
        // symbol, not just the one this controller trades. A cross-venue carry book needs every
        // venue's funding, and the harness asks `evaluate` per `(venue, symbol)` (with per-symbol
        // venue routing), so each venue's series feeds the book here even when its symbol is one this
        // controller does not open on. (A book-less broker — the test `MockBroker` — returns no bars
        // and relies purely on `observe_funding`; moving this above the symbol gate is inert there.)
        if let Some(f) = broker.bars(symbol).last().and_then(|b| b.funding) {
            self.observe_funding(venue, f);
        }
        // Symbol gate — two modes:
        //   * `symbol` SET (e.g. "BTCUSDT") ⇒ SINGLE-LEG: open only that symbol's leg of the carry,
        //     the directional funding-capture trade (leaves the opposite venue's price exposure open).
        //   * `symbol` EMPTY (the default) ⇒ TWO-LEG delta-neutral: open a leg on EVERY venue that is
        //     a leg of the best carry (long the low-funding venue, short the high-funding one). Each
        //     venue's series routes here (via the harness `venue_map`) under its own `(venue, symbol)`,
        //     so the harness opens ONE executor per leg — the true cross-venue carry from one mount.
        // (OBSERVES every venue regardless — the gate gates OPENING only, never the book.)
        if !self.symbol.is_empty() && symbol != self.symbol.as_str() {
            return None;
        }
        let carry = self.best_carry()?;
        let side = carry_leg_side(&carry, venue)?; // None ⇒ `venue` is not a leg of the best carry
        Some(PositionIntent::market(venue, symbol, side, self.qty, self.barriers))
    }

    /// Hot-swap this controller's tunables from a live-params re-tune (position-executor stage 6):
    /// the intent-template `qty` + `barriers`, and the decision knob [`ControllerParams::threshold`]
    /// reinterpreted as the carry `entry_threshold` (the min net edge to open — the generic "decision
    /// knob" slot, matching how `MomentumController` reads it as its momentum threshold). `cooldown_ms`
    /// is the HARNESS's (applied by the harness itself), so it is ignored here. `hold_periods` is not
    /// in the bag and is preserved; the evolving `funding_book` is STATE, likewise preserved — a
    /// re-tune takes effect on the very next [`evaluate`](Controller::evaluate) (future opens only).
    fn apply_params(&mut self, params: &ControllerParams) {
        self.qty = params.qty;
        self.barriers = params.barriers;
        self.entry_threshold = params.threshold;
    }
}

#[path = "funding_carry_tests.rs"]
#[cfg(test)]
mod funding_carry_tests;
