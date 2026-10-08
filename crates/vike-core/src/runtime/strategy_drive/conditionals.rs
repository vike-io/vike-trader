//! Conditional-order firing: the bar, price and mark triggers, and the fired order's submit.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Check the ConditionalBook against a closed bar and submit fired orders as plain
    /// MARKETs through the ONE live path (mint → RiskGate → client) — oracle semantics:
    /// only the FIRE crosses the gate; a veto surfaces as OrderDenied.
    pub(crate) fn fire_conditionals_bar(&mut self, venue: &str, symbol: &str, bar: &Bar) {
        let fired = match self.conditional_books.get_mut(&(venue.to_string(), symbol.to_string())) {
            Some(book) if !book.is_empty() => book.check_bar(bar),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, bar.ts);
    }

    /// Tick-price twin (Rust-native upgrade, `CoreConfig::conditionals_on_ticks`): the
    /// same book checked against a degenerate bar at the tick price — the Last lane.
    pub fn fire_conditionals_at_price(&mut self, venue: &str, symbol: &str, px: f64, now: i64) {
        let fired = match self.conditional_books.get_mut(&(venue.to_string(), symbol.to_string())) {
            Some(book) if !book.is_empty() => book.check_price(px, now),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, now);
    }

    /// MARK-lane twin (w2 `trigger_by`): a mark tick evaluates ONLY the `Some(Mark)` arms of the
    /// book (their trailing extremes ratchet on this lane too). Called off the conflated mark
    /// drain for EVERY mark tick — deliberately NOT behind `conditionals_on_ticks` (that knob
    /// gates the Last lane's sub-bar upgrade over the oracle's bar-close law; the mark lane has
    /// no bar-close equivalent at all, so gating it would mean a Mark arm could never fire).
    ///
    /// Because it is ungated it sits on the mark drain, so the no-Mark-arm path must cost
    /// nothing: the `has_mark_arms` scan runs FIRST and the (venue, symbol) lookup key — two
    /// String allocations — is built ONLY once some book actually holds a Mark arm. The scan is
    /// O(books) over the (venue, symbol) pairs carrying conditionals (zero when none are armed,
    /// a handful otherwise), never a walk of the arms themselves.
    pub(crate) fn fire_conditionals_at_mark(
        &mut self,
        venue: &str,
        symbol: &str,
        px: f64,
        now: i64,
    ) {
        if !self.conditional_books.values().any(|b| b.has_mark_arms()) {
            return;
        }
        let fired = match self.conditional_books.get_mut(&(venue.to_string(), symbol.to_string())) {
            Some(book) if book.has_mark_arms() => book.check_mark(px, now),
            _ => return,
        };
        self.submit_fired(venue, symbol, fired, now);
    }

    /// Release each fired conditional as a plain MARKET through the ONE live path, WRITE-AHEAD
    /// journaled as a [`vike_journal::JournalRecord::ConditionalFire`] (emulator-journal PR-1).
    ///
    /// The write-ahead record is what makes an emulated trigger replayable at all. The FIRE is a
    /// runtime reaction to a bar/tick, and market data is deliberately never journaled — so before
    /// this record `replay.rs` re-folded a fired session WITHOUT its release and the determinism
    /// fence caught it as a `HashMismatch` (the residual its module doc named). Journaling the
    /// DECISION (not the tick behind it) is the same trick `apply_strategy_intent` plays for a
    /// mounted strategy's orders: replay re-applies the recorded request through `apply_intent`
    /// and never re-evaluates a trigger, so a fire can neither double nor vanish.
    ///
    /// The record goes down BEFORE `apply_intent` (write-ahead: a crash between the two loses the
    /// release, never hides it) and carries the request with its coid still EMPTY — the mint
    /// happens inside `apply_intent`, which then writes its own `MintedSubmit`, exactly as for any
    /// other server-minted submit. Gated on journaling being on, so the no-journal path is
    /// byte-identical; firing is order cadence, never the p99 per-message fold.
    fn submit_fired(
        &mut self,
        venue: &str,
        symbol: &str,
        fired: Vec<crate::emulator::FiredConditional>,
        now: i64,
    ) {
        for f in fired {
            let req = OrderRequest {
                client_order_id: String::new(),
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                side: f.side,
                qty: f.qty,
                order_type: "market".to_string(),
                ts: now,
                ..Default::default()
            };
            if let Some(j) = self.journal.as_mut() {
                j.append_conditional_fire(now, &f.arm_id, f.trigger_px, &req)
                    .expect("journal append");
                self.journaled_since_snap += 1;
            }
            // ⚠ THE ARM'S OWN ACCOUNT, not the venue's default one. `conditional_books` is keyed by
            // `(venue, symbol)` — an EXCHANGE fact — so two accounts of one venue arm into one book
            // and a `FiredConditional` names no account; lowering this release through
            // `apply_intent` routed it by the payload's venue, which resolves the venue's DEFAULT
            // engine. A labelled mount's `Broker::submit_stop` therefore armed against its own book
            // and, on trigger, sold into the default account's: the protective exit OPENED a naked
            // position on an account that never asked for one while the position it was armed to
            // close stayed open — silently, both books wrong, and it is the exact shape the spread
            // configuration makes routine (long on one account, short on the other).
            //
            // `cond_engine` recorded the index at ARM time; a MISS keeps the historical payload
            // route, which is what a book seeded from a restored `Snap` still takes
            // (`CoreConfig::conditionals` carries `(venue, symbol)` and no route key — the declared
            // residual on that field).
            let route = match self.cond_engine.remove(&f.arm_id) {
                Some(eidx) => EngineRoute::Engine(eidx),
                None => EngineRoute::Payload,
            };
            self.apply_intent_routed(
                OrderIntent::Submit(Box::new(req)),
                now,
                CancelIntent::Unspecified,
                route,
            );
            self.dirty = true;
        }
    }
}
