//! Fill deferral: tagged maker fills and the session / stale-price wait disciplines.

use vike_model::{Bar, OrderKind, Strategy};

use super::{SimBroker, StrategyEngine};

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    /// Fill resting TAGGED maker quotes ([`HftBroker`]) against `event` (a coarse bar, a granular
    /// sub-bar, or a tick-derived bar) through the SAME crossing-fill model + `apply_fill` cost path
    /// as untagged orders — so tagged fills can never drift from the golden-gated fill math. A
    /// resting bid (buy limit) fills when the market moves at-or-below its price; a resting ask
    /// (sell limit) at-or-above — the `OrderKind::Limit` branch of `order_fill_price` (Bar model) /
    /// `TickFillModel` (L1 quote model). Tagged limits fill as MAKER (`is_maker = true`), and — via
    /// the shared `apply_fill` — snap to the point-in-time instrument grid and are gated below the
    /// PIT min exactly like untagged fills (PR-2a / #165). Filled tags are removed (see
    /// [`SymbolState::tagged`]); the maker learns of the fill via `on_fill`.
    ///
    /// SIMPLE CROSSING MODEL — a defensible FIRST model, NOT full L2 queue-position modeling. Its
    /// documented limits (R8 `L2BookFillModel` queue modeling is the follow-up — see `fill_model.rs`):
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
        if self.core.sym[si].tagged.is_empty() {
            return;
        }
        // Session gate (opt-in; no-op by default) — the ONE maker-lane gate, shared verbatim with
        // the queued twin (see `defer_tagged_session`). A resting quote cannot be crossed by a
        // market that is shut; it stays in the tag map and is retried on the first in-session
        // event. The tag map is non-empty here (checked above), so this reduces to the plain
        // session check the inline guard used to be.
        if self.defer_tagged_session(si, event.ts) {
            return;
        }
        // Snapshot the tags in insertion order, then apply each crossing fill in turn (a bid fill can
        // turn the ask into a reduce, exactly as two sequential fills would). Iterating the snapshot
        // (not the live map) makes removing the current tag mid-walk safe. Tagged orders are always
        // `OrderKind::Limit`, whose fill model never mutates the order, so a no-fill leaves the
        // original resting in the map untouched (the working clone is simply discarded).
        let tags: Vec<String> = self.core.sym[si].tagged.keys().cloned().collect();
        for tag in tags {
            let Some(mut o) = self.core.sym[si].tagged.get(&tag).cloned() else { continue };
            let Some(fp) = self.core.fill_price_for(si, &mut o, event) else { continue };
            self.core.sym[si].tagged.shift_remove(&tag); // crossed → terminal; retire the tag
            // The SAME `apply_fill` the untagged lanes use, which snaps price/size to the
            // point-in-time instrument grid and gates sub-min OPENING fills (PR-2a / #165) — so the
            // tagged maker lane inherits that grid-snapping for free and can never drift from the
            // untagged math. `None` means the PIT grid gated the fill (below min_qty/min_notional,
            // or a dust size after step-rounding); the tag is already retired (the crossing consumed
            // it, exactly as an untagged triggered-but-gated order is dropped from `pending`), so we
            // skip `fire_on_fill` and let the maker re-quote on its next `on_quote_tick`.
            if let Some(fill) = self.core.apply_fill(si, o.side, o.size, fp, event.ts, true) {
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

    /// The **session** half of [`Self::defer_fill`] (see [`SimBroker::session_closed`]): defer —
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

    /// The **tagged-lane** session gate, one function for both maker lanes ([`Self::fill_tagged`]
    /// and its queued twin, whose comment used to read "the queued twin of the guard in
    /// `fill_tagged`"): defer — and count one session deferral — when this symbol's market is shut
    /// at `ts` AND tags actually rest, so the counter measures refused MAKER passes only and never
    /// a quiet symbol with nothing quoted. `&&` short-circuits, so an empty tag map never reaches
    /// [`Self::defer_session`] and therefore never writes the counter.
    ///
    /// A deferred pass leaves the tag map exactly as it was: a resting quote a shut market cannot
    /// cross stays quoted and is retried on the first in-session event.
    #[inline]
    pub(super) fn defer_tagged_session(&mut self, si: usize, ts: i64) -> bool {
        !self.core.sym[si].tagged.is_empty() && self.defer_session(si, ts)
    }
}
