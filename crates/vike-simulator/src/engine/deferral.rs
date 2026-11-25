//! Fill deferral: the maker law's pass over the quotes and the session / stale-price wait
//! disciplines.

use vike_marketdata::Bar;
use vike_model::{OrderKind, Strategy};

use super::{OrderBrokerSimulator, StrategyEngine};

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    /// The MAKER LAW's pass (`StrategyEngineConfig::optimistic_quotes`): fill the quotes resting on
    /// `symbols[si]` (`OrderBrokerSimulatorSymbolState::quotes`, in book order) against `event` (a coarse bar or a
    /// tick-derived bar) through the SAME crossing-fill model + `apply_fill` cost path as every
    /// other order — so a quote's fill can never drift from the golden-gated fill math. A resting bid
    /// (buy limit) fills when the market moves at-or-below its price; a resting ask (sell limit)
    /// at-or-above — the `OrderKind::Limit` branch of `order_fill_price` (Bar model) /
    /// `FillModelTick` (L1 quote model). A quote fills as MAKER (`is_maker = true`), and — via the
    /// shared `apply_fill` — snaps to the point-in-time instrument grid and is gated below the PIT
    /// min exactly like any fill (PR-2a / #165). A filled quote leaves the book; the strategy learns
    /// of the fill via `on_fill`, under the quote's tag, and then of its end, `Filled`, through
    /// `on_order_event` before its next market hook (`deliver_order_ends`), as live sends it.
    ///
    /// SIMPLE CROSSING MODEL — a defensible FIRST model, NOT full L2 queue-position modeling. Its
    /// documented limits (R8 `FillModelL2Book` queue modeling is the follow-up — see `fill_model.rs`):
    /// - NO queue position: a resting quote fills the instant price touches it, as if always at the
    ///   FRONT of the queue. Real venues fill by price/time priority, so a passive quote can sit
    ///   unfilled while size ahead of it trades. This OVER-fills the maker (optimistic fill rate).
    /// - Full-size fill on the crossing event (no partial fills / no size-ahead depletion).
    /// - Intrabar straddle: if one event's range spans BOTH quotes, BOTH fill this step (in
    ///   insertion order — bid before ask); a single bar can't reveal the true intrabar path.
    /// - modify-in-place is FREE (no queue priority to forfeit), unlike a real re-price.
    ///
    /// Net effect: sim maker fills/PnL are an OPTIMISTIC bound. Treat this as a behavioral / plumbing
    /// check (does the maker rest, fill, skew, and break as designed?), NOT a fill-rate/PnL forecast.
    pub(super) fn fill_tagged(&mut self, si: usize, event: &Bar) {
        if self.core.symbol[si].quotes.is_empty() {
            return;
        }
        // Session gate (opt-in; no-op by default) — the ONE maker-law gate, shared verbatim with
        // the queued twin (see `defer_tagged_session`). A resting quote cannot be crossed by a
        // market that is shut; it keeps resting and is retried on the first in-session event.
        if self.defer_tagged_session(si, event.timestamp) {
            return;
        }
        // Snapshot the quotes in book order, then apply each crossing fill in turn (a bid fill can
        // turn the ask into a reduce, exactly as two sequential fills would). Each is looked up again
        // by its tag when its turn comes (`OrderBrokerSimulator::quote_now`): an `on_fill` before it may have
        // re-priced it, pulled it, or pulled it and placed another under the tag. A quote is always
        // `OrderKind::Limit`, whose fill model never mutates the order, so a no-fill leaves the
        // original resting untouched (the working clone is simply discarded).
        let seen = self.core.resting_quotes(si);
        for (tag, qid) in &seen {
            let Some(k) = self.core.quote_now(si, tag, *qid, &seen) else { continue };
            let mut o = self.core.symbol[si].pending[k].clone();
            let Some(fp) = self.core.fill_price_for(si, &mut o, event) else { continue };
            self.core.symbol[si].pending.remove(k); // crossed → terminal; off the book
            let tag = tag.clone();
            // The SAME `apply_fill` the other lanes use, which snaps price/size to the point-in-time
            // instrument grid and gates sub-min OPENING fills (PR-2a / #165) — so the maker law
            // inherits that grid-snapping for free and can never drift from the ordinary math.
            // `None` means the PIT grid gated the fill (below min_qty/min_notional, or a dust size
            // after step-rounding); the quote is already off the book (the crossing consumed it,
            // exactly as a triggered-but-gated order is dropped from `pending`), so we skip
            // `fire_on_fill`: its end is `Canceled`, and the maker re-quotes on its next hook.
            if let Some(mut fill) =
                self.core.apply_fill(si, o.side, o.size, fp, event.timestamp, true, true)
            {
                fill.tag = Some(tag); // the fill names its quote, as the live tag registry names it
                self.core.hear_child_fill(si, &mut fill); // an emulated child's names its parent
                self.fire_on_fill(fill);
            }
        }
    }

    /// The deferral check the shared fill lanes use: must this order WAIT rather than fill on this
    /// event? Two opt-in disciplines answer here so no lane can consult one and miss the other, and
    /// each counts its own deferral for diagnostics. `false` — and no counter write — whenever both
    /// are off (the byte-identical default).
    ///
    /// - **Session** ([`Self::defer_session`]): this SYMBOL's market is shut, so no kind fills.
    ///   Checked first — a closed market subsumes any question about how fresh its last price was.
    /// - **Staleness** ([`Self::defer_stale_wait`]): the market is open but this symbol's price is
    ///   stale, so market-ish kinds wait for a fresh print.
    ///
    /// A deferred order is not canceled or rejected: it stays resting exactly as it was and is
    /// retried on the next event, so it fills at the next in-session / freshly-printed price.
    ///
    /// Reached through [`Self::gate_pending`], which is what every untagged fill lane calls; the
    /// two halves are ALSO callable on their own, and the queued tick lane's price-conditional
    /// branch (`engine::queued`) does exactly that — it needs the session half (a closed market
    /// fills nothing, queued limits and maker quotes included) WITHOUT the staleness half, which
    /// exempts price-conditional kinds anyway.
    #[inline]
    pub(super) fn defer_fill(&mut self, si: usize, kind: OrderKind, ts: i64) -> bool {
        // `||` short-circuits, so `defer_session` runs first (and alone when it defers) exactly as
        // the two-`if` form did — byte-identical for every lane that calls this.
        self.defer_session(si, ts) || self.defer_stale_wait(si, kind, ts)
    }

    /// The **session** half of [`Self::defer_fill`] (see [`OrderBrokerSimulator::session_closed`]): defer —
    /// and count one session deferral — when this SYMBOL's market is shut at `ts`. UNIVERSAL: a
    /// closed market can fill no kind, resting limit and maker quote alike, so the queued lanes
    /// consult this for every order rather than takers only.
    ///
    /// It is the engine's ONLY writer of `session_deferrals` and its only caller of
    /// `session_closed`: the untagged lanes reach it through [`Self::gate_pending`], the maker
    /// lanes through [`Self::defer_tagged_session`], and [`Self::check_stop`] calls it directly.
    /// Every one of those sites establishes its own "count a REFUSED pass, not a quiet event"
    /// precondition (a non-empty tag map, an armed stop) before calling.
    #[inline]
    pub(super) fn defer_session(&mut self, si: usize, ts: i64) -> bool {
        if self.core.session_closed(si, ts) {
            self.core.session_deferrals += 1;
            return true;
        }
        false
    }

    /// The **staleness** half of [`Self::defer_fill`] (see [`vike_fills::staleness`]): defer — and
    /// count one stale deferral — when the market is open but this symbol's last price is stale, so a
    /// market-ish kind waits for a fresh print. Price-conditional kinds are exempt (a repeated
    /// stale price cannot spuriously satisfy a condition), which is why the queued lane runs this
    /// on its taker branch only.
    #[inline]
    fn defer_stale_wait(&mut self, si: usize, kind: OrderKind, ts: i64) -> bool {
        if self.core.defer_stale(si, kind, ts) {
            self.core.stale_deferrals += 1;
            return true;
        }
        false
    }

    /// The **maker-law** session gate, one function for both maker passes ([`Self::fill_tagged`]
    /// and its queued twin, whose comment used to read "the queued twin of the guard in
    /// `fill_tagged`"): defer — and count one session deferral — when this symbol's market is shut
    /// at `ts` AND quotes actually rest, so the counter measures refused MAKER passes only and never
    /// a quiet symbol with nothing quoted. `&&` short-circuits, so a book with no quote never
    /// reaches [`Self::defer_session`] and therefore never writes the counter.
    ///
    /// A deferred pass leaves the book exactly as it was: a resting quote a shut market cannot
    /// cross stays quoted and is retried on the first in-session event.
    #[inline]
    pub(super) fn defer_tagged_session(&mut self, si: usize, ts: i64) -> bool {
        self.core.rests_a_quote(si) && self.defer_session(si, ts)
    }
}
