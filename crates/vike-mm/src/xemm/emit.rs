//! The xEMM's QUOTE-EMISSION arm — place / re-price / pull ONE side of the maker book.
//!
//! Structurally the same three mutually-exclusive branches as `SpreadMaker::requote_single_side`
//! (`crate::quote`), reproduced here rather than shared because the two makers differ entirely in
//! how they arrive at a price (`SpreadMaker` prices off its OWN book; this one off a foreign
//! venue's touch) and share only this ~30-line tail, which calls four `SpreadMaker`-private
//! helpers. The invariants, however, are carried over VERBATIM and are pinned by tests:
//!
//! 1. **A PLACE and a PULL are never tolerance-gated.** `within_tolerance` is reachable only from
//!    the re-price arm, so a quote that must go on — or come off — the book always does, whatever
//!    the anti-churn knob is set to.
//! 2. **A FILL invalidates that side's snapshot.** `SideState::own` is the maker's INTENDED quote,
//!    not the venue-side remainder, so after a PARTIAL fill an intended-vs-target comparison reads
//!    "no change" and the size top-up would be silently lost. `refresh_stale` forces the next tick
//!    through the modify arm exactly once.
//! 3. **`mass_cancel` is NEVER used.** In the live runtime it is scoped to the DISPATCHING series
//!    (`OrderIntent::MassCancel { venue, symbol }` built from `drain_broker`'s arguments), so from a
//!    hedge-fill dispatch it would target the TAKER venue and cancel nothing on the maker venue —
//!    a pull that silently does not pull. [`XemmMaker::pull_all`] is two `cancel_tagged`s, which key
//!    off the tag registry and therefore reach the right orders from every lane.
//!
//! The tags are the bare literals `"bid"`/`"ask"`. A strategy never sees a client-order-id: the
//! runtime resolves `{mount_idx}|{venue}|{symbol}|{tag}` at drain time, and that key is the MOUNT's
//! own series on EVERY lane — built by `CoreThread::tag_key`, not from `drain_broker`'s arguments —
//! so a tag names ONE quote regardless of which series woke the strategy. ⚠ Invariant 3 above is
//! untouched by that: `mass_cancel` lowers to `OrderIntent::MassCancel { venue, symbol }` built from
//! the DRAIN's arguments, which is a different code path from the tag registry and still
//! series-scoped, so it stays unused here.

use vike_model::HftBroker;

use crate::refresh::within_tolerance;
use crate::xemm::XemmMaker;

/// The bid side's tag.
pub(crate) const BID_TAG: &str = "bid";
/// The ask side's tag.
pub(crate) const ASK_TAG: &str = "ask";

impl XemmMaker {
    /// Place, re-price or pull ONE side. `target` is that side's already-clamped, already-snapped,
    /// already-skewed `(price, size)`; `suppressed` is the combined breaker / naked-band /
    /// zero-size verdict for this side this tick.
    pub(crate) fn emit_side<B: HftBroker>(
        &mut self,
        broker: &mut B,
        is_bid: bool,
        target: (f64, f64),
        suppressed: bool,
        ts: i64,
    ) {
        let (px, qty) = target;
        let (tag, side_code) = if is_bid { (BID_TAG, 1) } else { (ASK_TAG, -1) };
        if suppressed {
            self.pull_side(broker, is_bid);
        } else if !self.side(is_bid).placed {
            // PLACE — never tolerance-gated.
            broker.submit_limit_tagged(tag, side_code, qty, px);
            let side = self.side_mut(is_bid);
            side.placed = true;
            side.own = Some((px, qty));
            side.refresh_stale = false;
            side.quoted_ts = ts;
        } else if !self.refresh_skips(is_bid, px, qty) {
            // RE-PRICE IN PLACE — modify, never cancel/replace, so venue queue priority survives.
            broker.modify_tagged(tag, Some(qty), Some(px));
            let side = self.side_mut(is_bid);
            side.own = Some((px, qty));
            side.refresh_stale = false;
            side.quoted_ts = ts;
        }
    }

    /// PULL one side if it is resting — never tolerance-gated (invariant 1 above). Idempotent: a
    /// side that is not placed sends nothing, so a halted maker does not spam cancels every tick.
    pub(crate) fn pull_side<B: HftBroker>(&mut self, broker: &mut B, is_bid: bool) {
        if !self.side(is_bid).placed {
            return;
        }
        broker.cancel_tagged(if is_bid { BID_TAG } else { ASK_TAG });
        let side = self.side_mut(is_bid);
        side.placed = false;
        side.own = None;
        side.refresh_stale = false;
        side.quoted_ts = 0;
    }

    /// Take BOTH sides off the book — every halt, every refusal to price, every fault. Two
    /// `cancel_tagged`s rather than `mass_cancel`, for the reason in the module doc.
    pub(crate) fn pull_all<B: HftBroker>(&mut self, broker: &mut B) {
        self.pull_side(broker, true);
        self.pull_side(broker, false);
    }

    /// `true` when the fresh target is close enough to what this side already has RESTING that
    /// re-issuing the modify would be pure wire churn.
    ///
    /// Short-circuits to `false` (ALWAYS re-quote) when the side's snapshot is `refresh_stale` —
    /// invariant 2 — and when no tolerance is configured, which is the default and keeps an
    /// unconfigured maker re-pricing on every tick.
    pub(crate) fn refresh_skips(&self, is_bid: bool, px: f64, qty: f64) -> bool {
        let side = self.side(is_bid);
        if side.refresh_stale {
            return false;
        }
        let (Some(tol), Some(resting)) = (self.cfg.refresh_tolerance, side.own) else {
            return false;
        };
        within_tolerance(resting, (px, qty), tol)
    }
}
