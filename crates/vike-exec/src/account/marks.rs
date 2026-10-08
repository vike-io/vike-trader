//! The account mark slot: its one writer (`set_mark_from`), ownership windows, read accessors.

use super::{Account, MarkKey, MarkSource};
use ustr::Ustr;

impl Account {
    /// THE ONLY WRITER of the account mark slot. `src` names the price CONCEPT and `now_ms` is
    /// the CORE clock (never a venue event time — see below); the precedence law documented on
    /// [`MarkSource`] is applied here so no caller can forget it. Returns whether the write
    /// landed, so a caller that wants to observe the law can (nothing on the hot path does).
    ///
    /// ONE CLOCK ON PURPOSE. Ownership is aged against the core clock on BOTH sides — the stored
    /// entry is stamped with the `now_ms` of its own write, not the venue's event time. Venue event
    /// times (right for the `PriceBoard`) are NOT comparable across feeds (binance `E`, okx a row
    /// `ts`, hyperliquid none), and aging one against the core clock would make a venue lagging
    /// more than the owner's window look permanently stale, handing the slot to candle closes
    /// forever.
    pub fn set_mark_from(
        &mut self,
        venue: &str,
        symbol: &str,
        px: f64,
        src: MarkSource,
        now_ms: i64,
    ) -> bool {
        // HOSTILE-VENUE GUARD. As THE only writer of this slot, one check covers every producer —
        // a fill's `mark_price`, the market-data mark lane, bar closes, trade ticks, reconcile
        // marks. A non-finite mark is silently corrosive: `unrealized_pnl` multiplies it into
        // equity, so ONE poisoned slot makes `equity_all` NaN, and the RiskGate's margin lane then
        // compares against NaN — every comparison FALSE, so the account reads as unconstrained.
        //
        // ⚠ A `px > 0.0` test is NOT this check (several callers have one): it excludes NaN and
        // -inf, but `+inf > 0.0` is TRUE, and +inf is just as poisonous (`inf * 0.0 == NaN`).
        // Rejecting returns `false`, the "write did not land" answer every caller tolerates.
        if !px.is_finite() {
            return false;
        }
        // Interned `Copy` key: a hash + table probe, no allocation.
        let key: MarkKey = (Ustr::from(venue), Ustr::from(symbol));
        if !src.is_venue_mark()
            && let Some((owner, at)) = self.mark_meta.get(&key)
        {
            // The ownership window is keyed off the CURRENT OWNER's source. See [`MarkSource`].
            let window = self.ownership_window(*owner);
            if owner.is_venue_mark() && now_ms.saturating_sub(*at) <= window {
                return false;
            }
        }
        self.marks.insert(key, px);
        self.mark_meta.insert(key, (src, now_ms));
        true
    }

    /// The ownership window a given owner source holds its slot for (see [`MarkSource`]'s
    /// "PER-SOURCE WINDOWS"). Non-venue-mark sources never own, so their window is unused.
    fn ownership_window(&self, owner: MarkSource) -> i64 {
        match owner {
            MarkSource::ReconcileMark => self.reconcile_staleness_ms,
            _ => self.mark_staleness_ms,
        }
    }

    /// The current mark for a symbol, if one has ever been written. Interns its `&str` arguments
    /// (bounded label sets) rather than allocating.
    pub fn mark_of(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.mark_of_key(&(Ustr::from(venue), Ustr::from(symbol)))
    }

    /// [`Self::mark_of`] for a caller that already holds the interned key — the allocation-free,
    /// intern-free read the fold-side callers use.
    pub fn mark_of_key(&self, key: &MarkKey) -> Option<f64> {
        self.marks.get(key).copied()
    }

    /// Every `(venue, symbol) -> mark` pair in insertion order (read-models, snapshots).
    pub fn marks_iter(&self) -> impl Iterator<Item = (&MarkKey, &f64)> {
        self.marks.iter()
    }

    /// Which concept currently owns a symbol's slot, and the core-clock ms it was written at.
    /// Exposed for tests and diagnostics; no fold formula reads it.
    pub fn mark_provenance(&self, venue: &str, symbol: &str) -> Option<(MarkSource, i64)> {
        self.mark_meta.get(&(Ustr::from(venue), Ustr::from(symbol))).copied()
    }

    /// Override the STREAMED venue-mark ownership window (see [`MarkSource`]). The live runtime
    /// calls this from `CoreConfig::mark_staleness_ms`; `0` makes streamed-mark ownership expire
    /// immediately, restoring pure last-write-wins for that source.
    pub fn set_mark_staleness_ms(&mut self, ms: i64) {
        self.mark_staleness_ms = ms;
    }

    /// Override the RECONCILE-mark ownership window (see [`MarkSource`]'s "PER-SOURCE WINDOWS").
    /// The live runtime calls this from `CoreConfig::reconcile_mark_staleness_ms`. Should exceed
    /// the deployment's `VIKE_RECONCILE_INTERVAL_MS` so a reconcile mark holds its slot between
    /// passes; `0` makes it expire immediately.
    pub fn set_reconcile_staleness_ms(&mut self, ms: i64) {
        self.reconcile_staleness_ms = ms;
    }
}
