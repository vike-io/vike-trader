//! The HEDGE LEDGER — the xEMM's only inventory authority, and a TARGET rather than a DELTA.
//!
//! # Why a target, and why the maker never reads inventory through the `Broker` seam
//!
//! The obvious ledger is a delta one: on a maker fill add the owed hedge, on a hedge fill subtract
//! it. It has a silent, unbounded failure mode. A hedge whose ack is lost — the ordinary case that
//! `hedge_timeout_ms` exists for — is RE-FIRED; if the original then fills too, a delta ledger has
//! subtracted once and sent twice, so the position doubles and the maker believes it is flat. This
//! ledger instead holds the two legs' SIGNED positions and derives what is still owed:
//!
//! ```text
//!   target   = −hedge_qty(maker_pos, hedge_ratio)     // the hedge position we WANT
//!   residual = target − hedge_pos                     // what still has to be sent
//!   naked    = maker_pos + hedge_pos                  // the exposure actually carried
//! ```
//!
//! A retry re-sends `residual`, which the late original fill has already shrunk — so **a retry can
//! never double the position**, whatever order the acks arrive in.
//!
//! `Broker::position(hedge_symbol)` would be the natural source for `hedge_pos`, and it is still
//! not one — but ⚠ **two of the three reasons this doc used to give are HISTORY, and saying so
//! matters more than keeping the longer list.** `vike_core`'s `declared_views` used to resolve every
//! declared leg against the MOUNT's engine (so a foreign leg read the maker venue's book), and
//! `dispatch_applied_fills` — the very hook a hedge fires from — used to build its `LiveBroker` with
//! EMPTY per-symbol tables (so a per-symbol read there fell through to the dispatch scalar). BOTH
//! are fixed: a declared leg now resolves against its own venue's engine, on every strategy-hook
//! lane. `crates/vike-core/tests/wiring/multi_symbol_reads.rs` is the regression proof.
//!
//! What survives is narrower and still decisive here:
//!
//! - `HftBroker::position` — the verb this maker's own trait bound gives it — takes NO symbol and
//!   returns the DISPATCHING engine's scalar. Reading a hedge leg through it is wrong by SIGNATURE,
//!   not by runtime defect, so no runtime fix can retire it;
//! - a broker read answers about SETTLED account state, and this ledger exists for the IN-FLIGHT
//!   hedge. `residual = target − hedge_pos` is exactly what makes a retry safe, and no
//!   `Broker::position` can see a sent-but-unacked hedge — a ledger derived from one would re-fire
//!   the full residual and double the position on a late ack, the failure this file removes.
//!
//! So the fill stream stays the authority, it still needs zero plumbing, and a poisoned-broker test
//! pins that the maker reads none of the seam.
//!
//! House style: naïve f64 folds (no `mul_add`), pure, in-file `#[cfg(test)]`.

use super::pricing::hedge_qty;

/// A hedge order that has been SENT and not yet fully accounted for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct InFlight {
    /// EVENT ts the order was sent at — the timeout clock.
    pub(crate) ts: i64,
}

/// The two legs' signed inventory plus the in-flight/attempt bookkeeping. `Default` is a flat,
/// idle ledger.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct HedgeLedger {
    /// SIGNED position on the MAKER venue, folded from maker-leg fills (`+` long).
    pub(crate) maker_pos: f64,
    /// SIGNED position on the TAKER venue, folded from hedge-leg fills (`+` long).
    pub(crate) hedge_pos: f64,
    /// The currently-sent hedge, if any.
    pub(crate) in_flight: Option<InFlight>,
    /// How many times the CURRENT residual has been sent. Reset to `0` the moment the residual
    /// settles, so a fresh maker fill always starts with a full attempt budget.
    pub(crate) attempts: u32,
}

impl HedgeLedger {
    /// Fold one MAKER-leg fill. `side` is `+1` buy / `−1` sell; `size` is unsigned.
    pub(crate) fn on_maker_fill(&mut self, side: i32, size: f64) {
        self.maker_pos += (side.signum() as f64) * size;
    }

    /// Fold one HEDGE-leg fill. Same sign convention.
    pub(crate) fn on_hedge_fill(&mut self, side: i32, size: f64) {
        self.hedge_pos += (side.signum() as f64) * size;
    }

    /// The signed hedge position this ledger WANTS, given the configured ratio.
    pub(crate) fn target(&self, hedge_ratio: f64) -> f64 {
        -hedge_qty(self.maker_pos, hedge_ratio)
    }

    /// The order still owed as `(side, qty)`, or `None` when the hedge is complete within `dust`.
    ///
    /// `dust` forgives a residual smaller than the taker venue's `min_qty`: a sub-minimum order is
    /// REJECTED by the venue, and a rejected hedge is an unhedged position that would be retried
    /// forever. `<= dust` (inclusive) so a residual exactly at the dust bound settles rather than
    /// oscillating. A non-finite residual is treated as settled — there is no order to name — which
    /// is the only safe reading of an unknowable number here.
    pub(crate) fn residual(&self, hedge_ratio: f64, dust: f64) -> Option<(i32, f64)> {
        let delta = self.target(hedge_ratio) - self.hedge_pos;
        if !delta.is_finite() || delta.abs() <= dust.max(0.0) {
            return None;
        }
        Some((if delta > 0.0 { 1 } else { -1 }, delta.abs()))
    }

    /// The UNHEDGED exposure the strategy actually carries: the two legs net. `0.0` when perfectly
    /// hedged. Note this is NOT the residual — with `hedge_ratio < 1.0` a fully-executed hedge
    /// still leaves a deliberate naked position, which is exactly what the naked bands measure.
    pub(crate) fn naked(&self) -> f64 {
        self.maker_pos + self.hedge_pos
    }

    /// Record that a hedge order was just SENT at `ts`.
    pub(crate) fn fire(&mut self, ts: i64) {
        self.in_flight = Some(InFlight { ts });
        self.attempts += 1;
    }

    /// Record that nothing is owed any more — clears the in-flight marker AND the attempt budget,
    /// so the next maker fill starts fresh.
    pub(crate) fn settle(&mut self) {
        self.in_flight = None;
        self.attempts = 0;
    }

    /// `true` when a sent hedge has been outstanding for at least `timeout_ms` of EVENT time.
    /// `timeout_ms <= 0` disables the timeout (an outstanding hedge is never re-fired), which also
    /// means the attempt budget can never be consumed past the first send.
    pub(crate) fn timed_out(&self, now: i64, timeout_ms: i64) -> bool {
        if timeout_ms <= 0 {
            return false;
        }
        self.in_flight.is_some_and(|f| now - f.ts >= timeout_ms)
    }

    /// `true` when the residual has been re-fired as many times as allowed and is STILL owed and
    /// timed out — the `HaltReason::HedgeUnfilled` condition. Continuing to quote past this point
    /// grows an exposure the taker venue has demonstrably refused to close.
    pub(crate) fn attempts_exhausted(
        &self,
        now: i64,
        hedge_ratio: f64,
        dust: f64,
        timeout_ms: i64,
        max_attempts: u32,
    ) -> bool {
        self.residual(hedge_ratio, dust).is_some()
            && self.attempts >= max_attempts
            && self.timed_out(now, timeout_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A maker BUY owes a hedge SELL of the same size, and vice versa — the sign convention the
    /// whole strategy rests on.
    #[test]
    fn a_maker_fill_owes_the_opposite_hedge() {
        let mut l = HedgeLedger::default();
        l.on_maker_fill(1, 5.0);
        assert_eq!(l.residual(1.0, 0.0), Some((-1, 5.0)), "bought 5 on A ⇒ sell 5 on B");
        let mut s = HedgeLedger::default();
        s.on_maker_fill(-1, 2.5);
        assert_eq!(s.residual(1.0, 0.0), Some((1, 2.5)), "sold 2.5 on A ⇒ buy 2.5 on B");
    }

    /// A PARTIAL hedge fill leaves exactly the remainder owed — the target ledger's basic claim.
    #[test]
    fn a_partial_hedge_fill_leaves_only_the_residual() {
        let mut l = HedgeLedger::default();
        l.on_maker_fill(1, 5.0);
        l.on_hedge_fill(-1, 2.0);
        assert_eq!(l.residual(1.0, 0.0), Some((-1, 3.0)), "3 of the 5 still owed");
        assert_eq!(l.naked().to_bits(), 3.0_f64.to_bits(), "and 3 is the exposure carried");
        l.on_hedge_fill(-1, 3.0);
        assert_eq!(l.residual(1.0, 0.0), None, "fully hedged ⇒ nothing owed");
        assert_eq!(l.naked().to_bits(), 0.0_f64.to_bits());
    }

    /// THE LAW A DELTA LEDGER BREAKS: fire, time out, re-fire, and THEN have the original land.
    /// Because the residual is derived from the two positions rather than decremented, the late
    /// fill shrinks it and the retry can never have doubled the position.
    #[test]
    fn a_retry_never_doubles_the_position() {
        let mut l = HedgeLedger::default();
        l.on_maker_fill(1, 5.0);
        // fire #1
        let (side, qty) = l.residual(1.0, 0.0).expect("owed");
        assert_eq!((side, qty), (-1, 5.0));
        l.fire(0);
        assert!(l.timed_out(3_000, 3_000), "the ack never came");
        // fire #2 — still 5 owed, because nothing has filled
        assert_eq!(l.residual(1.0, 0.0), Some((-1, 5.0)));
        l.fire(3_000);
        // the ORIGINAL now fills, late.
        l.on_hedge_fill(-1, 5.0);
        assert_eq!(
            l.residual(1.0, 0.0),
            None,
            "the late original settles the target — a delta ledger would still owe 5 and would \
             have sent 10 in total"
        );
        // ...and the retry's own fill would take it NEGATIVE, which the residual now reports as an
        // over-hedge to be unwound rather than as "flat".
        l.on_hedge_fill(-1, 5.0);
        assert_eq!(l.residual(1.0, 0.0), Some((1, 5.0)), "an over-hedge is owed back, not ignored");
    }

    /// `hedge_ratio == 0.0` owes nothing at all — the deliberately-unhedged configuration fires no
    /// order rather than firing a zero-size one.
    #[test]
    fn a_zero_ratio_owes_nothing_but_still_reports_the_exposure() {
        let mut l = HedgeLedger::default();
        l.on_maker_fill(1, 5.0);
        assert_eq!(l.residual(0.0, 0.0), None, "ratio 0 ⇒ no hedge order");
        assert_eq!(l.naked().to_bits(), 5.0_f64.to_bits(), "but the exposure is fully visible");
    }

    /// A partial ratio hedges its fraction and leaves the rest as a DELIBERATE naked position —
    /// which the bands still measure, so "deliberate" never means "invisible".
    #[test]
    fn a_partial_ratio_leaves_a_deliberate_residual_exposure() {
        let mut l = HedgeLedger::default();
        l.on_maker_fill(1, 10.0);
        assert_eq!(l.residual(0.6, 0.0), Some((-1, 6.0)));
        l.on_hedge_fill(-1, 6.0);
        assert_eq!(l.residual(0.6, 0.0), None, "the 60% hedge is complete");
        assert_eq!(l.naked().to_bits(), 4.0_f64.to_bits(), "4 units are deliberately naked");
    }

    /// Dust forgives a residual the taker venue would REJECT — otherwise a sub-`min_qty` remainder
    /// is retried until the attempt budget halts the maker for no reason.
    #[test]
    fn a_dust_residual_settles_instead_of_being_retried_forever() {
        let mut l = HedgeLedger::default();
        l.on_maker_fill(1, 5.0);
        l.on_hedge_fill(-1, 4.999);
        assert_eq!(l.residual(1.0, 0.01), None, "0.001 is under the 0.01 dust bound");
        assert!(l.residual(1.0, 0.0).is_some(), "and is genuinely owed without a dust bound");
        // exactly AT the bound settles (inclusive), so it cannot oscillate.
        let mut at = HedgeLedger::default();
        at.on_maker_fill(1, 0.01);
        assert_eq!(at.residual(1.0, 0.01), None, "a residual exactly at the bound settles");
    }

    /// Exhaustion needs all three: something still owed, the budget spent, AND the last send timed
    /// out. A hedge that is merely in flight is not a fault.
    #[test]
    fn exhaustion_needs_owed_and_spent_and_timed_out() {
        let mut l = HedgeLedger::default();
        l.on_maker_fill(1, 5.0);
        l.fire(0);
        l.fire(3_000);
        assert!(!l.attempts_exhausted(4_000, 1.0, 0.0, 3_000, 2), "not yet timed out");
        assert!(l.attempts_exhausted(6_000, 1.0, 0.0, 3_000, 2), "owed + spent + timed out");
        assert!(!l.attempts_exhausted(6_000, 1.0, 0.0, 3_000, 3), "budget not spent");
        l.on_hedge_fill(-1, 5.0);
        assert!(!l.attempts_exhausted(6_000, 1.0, 0.0, 3_000, 2), "nothing owed ⇒ no fault");
    }

    /// `settle` clears the budget, so a NEW maker fill after a completed hedge starts with the full
    /// attempt allowance rather than inheriting the last cycle's.
    #[test]
    fn settling_restores_the_attempt_budget() {
        let mut l = HedgeLedger::default();
        l.on_maker_fill(1, 5.0);
        l.fire(0);
        l.fire(3_000);
        assert_eq!(l.attempts, 2);
        l.on_hedge_fill(-1, 5.0);
        l.settle();
        assert_eq!((l.attempts, l.in_flight), (0, None));
    }
}
