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
//! high leg) for the existing [`crate::ControllerHarness`] to open as a [`PositionExecutor`].
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

use vike_model::{fee_schedule_for, Broker, ControllerParams, TripleBarrier};

use crate::controller::{as_f64, barriers_from_params, Controller};
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
    if let Some(target) = profit_target {
        if accrued_pnl >= target {
            return Some(CarryCloseReason::ProfitTarget);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::ControllerHarness;
    use crate::position_executor::EntryKind;
    use vike_model::strategy::MockBroker;
    use vike_model::{Bar, Strategy};

    // ---- pure helpers ----

    fn q(venue: &str, funding_rate: f64, taker_fee: f64) -> FundingQuote {
        FundingQuote::new(venue, funding_rate, taker_fee)
    }

    fn bar(ts: i64, symbol: &str, close: f64) -> Bar {
        Bar {
            ts,
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

    #[test]
    fn controller_opens_the_long_leg_on_the_low_funding_venue() {
        let mut c = loaded_controller();
        let b = MockBroker { px: 30_000.0, ..Default::default() }; // book-less → observe_funding only
                                                                   // asked about binance (the low-funding leg) → LONG intent at the configured size.
        let intent = Controller::evaluate(&mut c, &b, "binance", "BTCUSDT").expect("opens the leg");
        assert_eq!(intent.venue, "binance");
        assert_eq!(intent.symbol, "BTCUSDT");
        assert_eq!(intent.side, 1, "long the low-funding leg");
        assert_eq!(intent.qty, 1.0);
        assert_eq!(intent.entry, EntryKind::Market);
        assert_eq!(intent.barriers.stop_loss, Some(50.0));
    }

    #[test]
    fn controller_opens_the_short_leg_on_the_high_funding_venue() {
        let mut c = loaded_controller();
        let b = MockBroker { px: 30_000.0, ..Default::default() };
        // asked about bybit (the high-funding leg) → SHORT intent.
        let intent = Controller::evaluate(&mut c, &b, "bybit", "BTCUSDT").expect("opens the leg");
        assert_eq!(intent.side, -1, "short the high-funding leg");
        assert_eq!(intent.venue, "bybit");
    }

    #[test]
    fn controller_declines_a_venue_outside_the_winning_pair() {
        let mut c = loaded_controller();
        c.observe_funding("okx", 0.002); // a third venue, not a leg of the best (binance/bybit) pair
        let b = MockBroker::default();
        // okx sits between the extremes → not in the winning pair → declines.
        assert_eq!(Controller::evaluate(&mut c, &b, "okx", "BTCUSDT"), None);
        // and the winning legs still open.
        assert!(Controller::evaluate(&mut c, &b, "binance", "BTCUSDT").is_some());
        assert!(Controller::evaluate(&mut c, &b, "bybit", "BTCUSDT").is_some());
    }

    #[test]
    fn controller_declines_a_foreign_symbol() {
        let mut c = loaded_controller();
        let b = MockBroker::default();
        assert_eq!(Controller::evaluate(&mut c, &b, "binance", "ETHUSDT"), None);
    }

    #[test]
    fn controller_declines_when_differential_below_fees() {
        // a thin differential that does NOT clear the round-trip taker cost in one period.
        let mut c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
        c.observe_funding("binance", 0.0001);
        c.observe_funding("bybit", 0.0003); // gross 0.0002 ≪ cost 0.0031 → net < 0
        let b = MockBroker::default();
        assert_eq!(c.best_carry(), None, "net edge below zero threshold → no carry");
        assert_eq!(Controller::evaluate(&mut c, &b, "binance", "BTCUSDT"), None);
    }

    #[test]
    fn controller_auto_observes_the_asked_venue_from_bar_funding() {
        // A broker whose latest bar carries funding auto-populates the asked venue's book slot, so a
        // single explicit observe of the OTHER venue is enough to form a pair.
        struct BarBroker {
            bars: Vec<Bar>,
        }
        impl Broker for BarBroker {
            fn submit_market(&mut self, _s: &str, _side: i32, _q: f64) {}
            fn submit_limit(&mut self, _s: &str, _side: i32, _q: f64, _p: f64) {}
            fn position(&self, _s: &str) -> f64 {
                0.0
            }
            fn price(&self, _s: &str) -> f64 {
                30_000.0
            }
            fn equity(&self) -> f64 {
                0.0
            }
            fn bars(&self, _s: &str) -> &[Bar] {
                &self.bars
            }
            fn index(&self) -> usize {
                0
            }
            fn now(&self) -> i64 {
                0
            }
        }
        let mut c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
        c.observe_funding("bybit", 0.005); // the other leg, explicit
        let mut only_bar = bar(1, "BTCUSDT", 30_000.0);
        only_bar.funding = Some(-0.001); // binance's funding arrives via the bar
        let b = BarBroker { bars: vec![only_bar] };
        // evaluate for binance auto-observes -0.001 from the bar, forming the binance/bybit pair.
        let intent = Controller::evaluate(&mut c, &b, "binance", "BTCUSDT").expect("pair formed");
        assert_eq!(intent.side, 1, "binance long via bar-observed funding");
    }

    #[test]
    fn apply_params_hot_swaps_qty_barriers_threshold_and_preserves_state() {
        let mut c = loaded_controller();
        assert_eq!(c.qty(), 1.0);
        assert_eq!(c.entry_threshold(), 0.0);
        assert_eq!(c.hold_periods(), 1.0, "starts at the default hold horizon");

        let p = ControllerParams::new(
            5_000,                                            // cooldown — the HARNESS's, ignored here
            3.0,                                              // qty 1 → 3
            TripleBarrier::new(Some(20.0), None, None, None), // barriers re-armed
            0.01,                                             // threshold → entry_threshold
        );
        Controller::apply_params(&mut c, &p);

        assert_eq!(c.qty(), 3.0, "size hot-swapped");
        assert_eq!(c.entry_threshold(), 0.01, "entry threshold hot-swapped from ControllerParams");
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
        let mut b = MockBroker { now: 1, px: 30_000.0, ..Default::default() };
        Strategy::on_bar(&mut h, &mut b, &bar(1, "BTCUSDT", 30_000.0));
        assert_eq!(h.active_count(), 1, "one leg opened");
        assert_eq!(
            b.markets,
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
        let mut b = MockBroker { now: 1, px: 30_000.0, ..Default::default() };
        for ts in 1..=5 {
            Strategy::on_bar(&mut h, &mut b, &bar(ts, "BTCUSDT", 30_000.0));
        }
        assert_eq!(h.active_count(), 0, "no executor without a fed carry");
        assert!(b.markets.is_empty(), "no market orders");
        assert!(b.limits.is_empty(), "no limit orders");
    }

    #[test]
    fn off_single_venue_never_opens() {
        // A carry needs a PAIR: with only ONE venue observed the controller stays inert (no second
        // leg to rank against), regardless of how attractive that one venue's funding is.
        let mut c = FundingCarryController::new("BTCUSDT", 1.0, TripleBarrier::none());
        c.observe_funding("binance", -0.05); // a huge negative funding, but still a single venue
        let b = MockBroker::default();
        assert_eq!(c.best_carry(), None, "one venue can never form a delta-neutral pair");
        assert_eq!(Controller::evaluate(&mut c, &b, "binance", "BTCUSDT"), None, "no leg opened");
    }
}
