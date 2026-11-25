//! The OPT-IN queue-position fill lane: the queue-gated twins of the frozen per-tick fill pass.
//!
//! Reached ONLY when [`StrategyEngineConfig::queue_model`](crate::StrategyEngineConfig) is `Some` (see
//! [`crate::queue_model`]) — a default run never enters this module at all, and the frozen
//! `fill_pending_tick` / `fill_tagged` lanes it mirrors stay byte-identical.
//!
//! Split out of `engine.rs` so the twins are readable as twins: the pair sat ~700 lines from the
//! code it mirrors, and its doc comments said things like "the queued twin of the guard in
//! `fill_tagged`" — a hand-synced duplication no reader could check without scrolling between
//! two ends of a 2 200-line file.
//!
//! The per-order gate sequence is no longer duplicated at all. Session → staleness → emulator
//! stop-release → trigger price lives in ONE function on the parent module
//! ([`StrategyEngine::gate_pending`]) that this lane's TAKER branch calls like every other lane,
//! and the maker-lane session gate in one more ([`StrategyEngine::defer_tagged_session`]). What
//! stays here is only what genuinely differs, and is why the twin exists at all: queue-state
//! identity and sync (`OrderResting::qid`), the [`queue_gate`] verdict layered on top of the
//! frozen price law, partial-fill remainder bookkeeping, and the maker straddle / min-hold
//! guards. The one gate this lane still runs by hand is [`StrategyEngine::defer_session`], on
//! its price-conditional branch — documented at that call site. A held order
//! (`OrderBrokerSimulator::submit_released`) is not in `pending` before its release, so this lane first sees,
//! and seeds, it then.

use indexmap::IndexMap;
use vike_marketdata::{Bar, L2Book};
use vike_model::{OrderKind, OrderResting, Strategy};

use super::{OrderBrokerSimulator, OrderGate, StrategyEngine, TickEvent};
use crate::queue_model::{QueueModel, QueueState, QueueTracker, QueueTrackerOrder};

/// One resting limit's queue verdict against one price-triggering tick. Computed AFTER the
/// existing fill model already said the price condition holds (`fill_price_for` = `Some`) —
/// the queue gate composes on top, never replaces, the frozen price logic.
enum QueueVerdict {
    /// stays resting — front not cleared and no crossing evidence
    Rest,
    /// fills in full at the fill model's price
    Fill,
    /// fills exactly this qty (the trade's excess beyond the front); the remainder keeps resting
    Partial(f64),
}

/// The queue gate for one resting limit at `price` (side `side`, remaining `size`) against the
/// RAW tick (the derived `Bar` shape can't distinguish a trade from a quote):
/// - a STRICT price cross (traded/quoted THROUGH the level) fills in full — the whole level,
///   queue ahead included, was consumed;
/// - a trade AT the price consumes the front first; only its excess fills (partially);
/// - a quote touch fills only once the front is already cleared (no trade proof otherwise).
///
/// Simplifications (documented, deliberate): trades are not filtered by aggressor side
/// (`is_buyer_maker` is a future refinement), and simulated orders never consume each other's
/// queue (each order is gated independently — the standard shadow-order convention).
fn queue_gate(
    model: &dyn QueueModel,
    st: &mut QueueState,
    tick: &TickEvent,
    side: i32,
    price: f64,
    size: f64,
) -> QueueVerdict {
    let buy = side > 0;
    match tick {
        TickEvent::Trade(t) => {
            let cross = if buy { t.price < price } else { t.price > price };
            if cross {
                return QueueVerdict::Fill;
            }
            // triggered but not crossed → the trade printed exactly AT `price`
            let ahead = st.front_quantity;
            model.on_trade(st, t.size);
            let fillable = (t.size - ahead).max(0.0);
            if fillable >= size {
                QueueVerdict::Fill
            } else if fillable > 0.0 {
                QueueVerdict::Partial(fillable)
            } else {
                QueueVerdict::Rest
            }
        }
        TickEvent::Quote(q) => {
            let cross = if buy { q.ask < price } else { q.bid > price };
            if cross || model.is_front_cleared(st) {
                QueueVerdict::Fill
            } else {
                QueueVerdict::Rest
            }
        }
        // book events and heard events never reach the price/fill path (intercepted in run_ticks)
        TickEvent::Book(_)
        | TickEvent::Reference { .. }
        | TickEvent::Mark { .. }
        | TickEvent::Flow(_)
        | TickEvent::FeedStatus { .. } => QueueVerdict::Rest,
    }
}

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    /// The queue-gated twin of the per-tick fill pass — reached ONLY when
    /// `StrategyEngineConfig::queue_model` is `Some` (see [`crate::queue_model`]). Same structure as the
    /// frozen path (trigger → resolve → dispatch → tagged lane), with resting LIMIT /
    /// LIMIT-CLOSE orders additionally gated by their queue-position estimate: a price TOUCH no
    /// longer fills instantly — trades at the price consume the estimated front first and only
    /// their excess fills (partially); a strict cross still fills in full. Non-limit kinds
    /// (market / market-close / stop / trailing) are takers and pass through unchanged.
    ///
    /// State sync: untagged states are keyed by the order's ENGINE-LOCAL identity
    /// (`OrderResting::qid`, stamped here the first time an order is seen), never by a
    /// `(side, price)` fingerprint — so the maker re-quote pattern `cancel_all` + re-submit at
    /// the same price correctly re-seeds at the back of the level instead of inheriting the
    /// canceled order's advanced front (see [`crate::queue_model::QueueTrackerSymbol`]). States whose
    /// order vanished since the last pass (cancel_all / OCO sibling-cancel / fill) are dropped
    /// by this pass's rebuild.
    pub(super) fn fill_pending_tick_queued(
        &mut self,
        si: usize,
        event: &Bar,
        tick: &TickEvent,
        book: Option<&L2Book>,
    ) {
        let mut tracker = self.queue.take().expect("queued path requires the tracker");

        // ---- untagged pending lane ----
        let pending = std::mem::take(&mut self.core.symbol[si].pending);
        let mut old_states = std::mem::take(&mut tracker.symbol[si].pending);
        let mut new_states: IndexMap<u64, QueueTrackerOrder> = IndexMap::new();
        let mut triggered: Vec<(OrderResting, f64)> = Vec::new();
        let mut still: Vec<OrderResting> = Vec::new();
        // queue_ids whose resting remainder must be reduced by whatever ACTUALLY fills below
        let mut partial_queue_ids: Vec<u64> = Vec::new();
        for mut o in pending {
            // a quote (`OrderBrokerSimulatorSymbolState::quotes`) takes the maker pass below, with its queue state by tag
            if self.core.is_quote(si, o.qid) {
                still.push(o);
                continue;
            }
            // `queue_kind` is a pure classification (no gate has run yet), so splitting on it
            // first lets the TAKER branch reach the shared gate sequence whole while the queued
            // branch below keeps the one piece it genuinely needs on its own.
            let queue_kind =
                matches!(o.kind, OrderKind::Limit | OrderKind::LimitClose) && o.price.is_some();
            if !queue_kind {
                // THE gate sequence — session → staleness → stop release → trigger — the SAME
                // `gate_pending` the frozen twin and the three bar lanes call. Nothing on this
                // branch is queue-aware: a taker crosses the book, it never rests in it.
                match self.gate_pending(si, &mut o, event) {
                    OrderGate::Rest => still.push(o),
                    OrderGate::Trigger(fp) => triggered.push((o, fp)),
                    OrderGate::End => {}
                }
                continue;
            }
            // Session gate (opt-in; no-op by default) — the HALF of the shared sequence a
            // price-conditional queued limit needs: a CLOSED market fills NO kind, resting limits
            // included, while the staleness half exempts price-conditional kinds anyway (a
            // repeated stale price cannot spuriously satisfy a condition). Gate off ⇒
            // `session_closed` is always false ⇒ byte-identical. A deferred order keeps resting
            // and is retried on the first in-session event; its queue state, if it had one, is
            // rebuilt on that retry — a market shut for the interval accrued no priority worth
            // preserving (an order that `continue`s here is never added to `new_states`).
            if self.defer_session(si, event.timestamp) {
                still.push(o);
                continue;
            }
            let price = o.price.expect("limit requires price");
            if o.qid == 0 {
                o.qid = self.core.next_queue_id(); // first sighting: mint this order's identity
            }
            // this order's own state, or a fresh back-of-level seed. The identity check also
            // covers a level change (nothing re-prices an untagged resting order today, so it
            // is a belt-and-braces guard, not a reachable path).
            let mut oq = match old_states.shift_remove(&o.qid) {
                Some(oq) if oq.matches(o.side, price.to_bits(), o.qid) => oq,
                _ => tracker.new_entry(si, o.side, price, o.qid, book),
            };
            match self.core.fill_price_for(si, &mut o, event) {
                // price condition not met — and a non-triggering event can't be AT this price,
                // so there is no queue consumption to fold either
                None => {
                    new_states.insert(o.qid, oq);
                    still.push(o);
                }
                Some(fp) => {
                    let verdict = queue_gate(
                        tracker.model.as_ref(),
                        &mut oq.state,
                        tick,
                        o.side,
                        price,
                        o.size,
                    );
                    match verdict {
                        QueueVerdict::Fill => triggered.push((o, fp)), // state retires with it
                        QueueVerdict::Partial(qty) => {
                            let mut part = o.clone();
                            part.size = qty;
                            triggered.push((part, fp));
                            // The order keeps resting at its FULL size for now: the intended
                            // partial can still be capped (resolve_intrabar_fills), vetoed
                            // (position/active-mask caps), clamped (volume_limit) or rejected
                            // (the PIT properties grid) downstream. Deducting it up front would
                            // erode the resting order by qty that never filled — the remainder
                            // is reduced by the ACTUAL fill after dispatch instead.
                            partial_queue_ids.push(o.qid);
                            new_states.insert(o.qid, oq);
                            still.push(o);
                        }
                        QueueVerdict::Rest => {
                            new_states.insert(o.qid, oq);
                            still.push(o);
                        }
                    }
                }
            }
        }
        // states left in old_states belong to orders gone since the last pass — dropped
        tracker.symbol[si].pending = new_states;
        self.core.symbol[si].pending = still;
        self.core.open_group_pass(si, &triggered);
        let resolved = self.resolve_triggered(si, triggered, event);
        let mut filled_partials: Vec<(u64, f64)> = Vec::new();
        for (o, fp) in resolved {
            if o.size <= 1e-12 {
                continue;
            }
            let applied = self.dispatch_fill(si, &o, fp, event.volume, event.timestamp);
            if applied > 0.0 && partial_queue_ids.contains(&o.qid) {
                filled_partials.push((o.qid, applied));
            }
        }
        // Shrink each partially-filled order's resting remainder by what really filled. Looked
        // up by identity because an `on_fill` callback fired mid-dispatch may already have
        // canceled or replaced it (then there is simply nothing to shrink). A remainder ground
        // down to dust retires — order AND state — exactly as the tagged lane does.
        let mut retire: Vec<u64> = Vec::new();
        for (qid, applied) in filled_partials {
            if let Some(rest) = self.core.symbol[si].pending.iter_mut().find(|r| r.qid == qid) {
                rest.size = (rest.size - applied).max(0.0);
                if rest.size <= 1e-12 {
                    retire.push(qid);
                }
            }
        }
        if !retire.is_empty() {
            self.core.symbol[si].pending.retain(|o| !retire.contains(&o.qid));
            for qid in retire {
                tracker.symbol[si].pending.shift_remove(&qid);
                self.core.order_fees.swap_remove(&qid);
            }
        }
        self.core.close_group_pass();

        // ---- tagged (HFT maker) lane — same position as the frozen path's fill_tagged ----
        self.fill_tagged_queued(si, event, tick, book, &mut tracker);
        self.queue = Some(tracker);
    }

    /// Queue-gated twin of [`Self::fill_tagged`] (tick path only). A tag names a quote SLOT,
    /// not a resting order, so the sync keys on the tag PLUS the slot's current side, price and
    /// engine-local identity (`OrderResting::qid`): a re-priced tag, a `cancel` + re-place,
    /// and an `amend` qty INCREASE (a new `qid`, `OrderBrokerSimulator::amend_queue_id`) all RE-SEED at the
    /// back of the level — each is a new order on a real venue, and each forfeits the priority the
    /// old state had accumulated. An amend-DOWN keeps its place, as venues do. Gone tags drop their
    /// state; partial fills shrink the resting quote in place instead of retiring it.
    fn fill_tagged_queued(
        &mut self,
        si: usize,
        event: &Bar,
        tick: &TickEvent,
        book: Option<&L2Book>,
        tracker: &mut QueueTracker,
    ) {
        // Session gate (opt-in; no-op by default) — the SAME `defer_tagged_session` the frozen
        // [`StrategyEngine::fill_tagged`] calls, no longer a hand-synced twin of it. A shut market
        // crosses no resting maker quote, on this lane no less than the frozen one, so a closed
        // event fills nothing AND does not advance any tag's queue estimate (the sync / gate below
        // is skipped whole). Gate off ⇒ never defers ⇒ byte-identical: every tag-state sync below
        // runs exactly as before.
        if self.defer_tagged_session(si, event.timestamp) {
            return;
        }
        let book_now = &self.core.symbol[si].pending;
        let live: Vec<(String, i32, f64, u64)> = self
            .core
            .resting_quotes(si)
            .into_iter()
            .filter_map(|(tag, qid)| {
                let o = book_now.iter().find(|o| o.qid == qid)?;
                o.price.map(|p| (tag, o.side, p, qid))
            })
            .collect();
        tracker.sync_tags(si, &live, book);
        if live.is_empty() {
            return;
        }
        let QueueTracker { model, symbol, seed_depth, min_hold_ms, .. } = tracker;
        let seed_depth = *seed_depth;
        let min_hold_ms = *min_hold_ms;
        let sq = &mut symbol[si];
        // STRADDLE GUARD (intrabar-path realism): a resting bid and a resting ask cannot BOTH fill
        // on ONE event — the price took a single intrabar path, not both. Once one side fills this
        // event, the OPPOSITE side is deferred to the next event, so a maker can never book an
        // instant 0-ms round-trip (buy-low-sell-high on the same tick — the artifact that fabricated
        // free spread). Same-side ladder fills are unaffected. Queued lane only (opt-in via
        // `queue_model`); the frozen `fill_tagged` keeps its documented straddle for byte-compat.
        let mut filled_side_this_event: Option<i32> = None;
        for (tag, _, _, qid) in live {
            // The quote as it rests NOW: an `on_fill` earlier in this pass may have re-priced it (it
            // is read fresh below) or pulled it. One placed under the tag since, or amended up (a new
            // `qid`), is not this pass's: it starts fresh next tick.
            let resting = self.core.symbol[si].pending.iter().find(|o| o.qid == qid).cloned();
            let Some(mut o) = resting.filter(|_| self.core.is_quote(si, qid)) else { continue };
            if filled_side_this_event == Some(-o.side) {
                continue; // opposite side already filled this event ⇒ rest, retried next tick
            }
            // MIN-HOLD gate: a fill that REDUCES the current position (the closing leg of a maker
            // round-trip) is deferred until `min_hold_ms` after the position opened — so no
            // physically-impossible sub-second flip. `entry_timestamp` is stamped on the opening fill below.
            let pos_pre = self.core.symbol[si].pos.size;
            let reduces = vike_model::is_reducing_direction(o.side, pos_pre);
            if min_hold_ms > 0
                && reduces
                && event.timestamp - self.core.symbol[si].entry_timestamp < min_hold_ms
            {
                continue;
            }
            // read the price FRESH off the live order (an on_fill callback earlier in this
            // pass may have re-priced it) so the gate matches what fill_price_for saw
            let Some(price) = o.price else { continue };
            let Some(fp) = self.core.fill_price_for(si, &mut o, event) else { continue };
            let verdict = {
                // sync_tags ran BEFORE the loop; an on_fill callback earlier in this same pass
                // can have moved this tag since (the classic fill-one-side-requote-the-other
                // maker flow). Re-seed at the level it actually rests on now rather than
                // gating it with the stale level's — possibly already cleared — front.
                let stale = match sq.tagged.get(&tag) {
                    Some(tq) => !tq.matches(o.side, price.to_bits(), o.qid),
                    None => continue,
                };
                if stale {
                    let fresh =
                        sq.new_entry_with(model.as_ref(), seed_depth, o.side, price, o.qid, book);
                    sq.tagged.insert(tag.clone(), fresh);
                }
                let Some(tq) = sq.tagged.get_mut(&tag) else { continue };
                queue_gate(model.as_ref(), &mut tq.state, tick, o.side, price, o.size)
            };
            match verdict {
                QueueVerdict::Rest => {}
                QueueVerdict::Fill => {
                    self.core.symbol[si].pending.retain(|r| r.qid != qid);
                    sq.tagged.shift_remove(&tag);
                    // same apply_fill/PIT-gate semantics as fill_tagged: a `None` means the
                    // grid gated it — the quote is already off the book, no on_fill fires. The
                    // order ends here, and its fee entry with it.
                    let mut fees = self.core.order_fees_of(o.qid);
                    self.core.order_fees.swap_remove(&o.qid);
                    if let Some(mut fill) = self.core.apply_order_fill(
                        si,
                        o.side,
                        o.size,
                        fp,
                        o.weight,
                        event.timestamp,
                        true,
                        true,
                        &mut fees,
                    ) {
                        fill.tag = Some(tag.clone()); // the fill names its quote (`fill_tagged`'s)
                        self.core.hear_child_fill(si, &mut fill); // an emulated child's names its parent
                        self.fire_on_fill(fill);
                        filled_side_this_event = Some(o.side); // block the opposite side this event
                        if !reduces {
                            self.core.symbol[si].entry_timestamp = event.timestamp; // stamp the min-hold clock
                        }
                    }
                }
                QueueVerdict::Partial(qty) => {
                    let fill_size = qty.min(o.size);
                    let mut fees = self.core.order_fees_of(o.qid);
                    if let Some(mut fill) = self.core.apply_order_fill(
                        si,
                        o.side,
                        fill_size,
                        fp,
                        o.weight,
                        event.timestamp,
                        true,
                        true,
                        &mut fees,
                    ) {
                        fill.tag = Some(tag.clone()); // the fill names its quote (`fill_tagged`'s)
                        // shrink the resting quote by what actually filled (grid-rounded);
                        // a dust remainder takes it off the book like a full fill would
                        let rests = &mut self.core.symbol[si].pending;
                        if let Some(k) = rests.iter().position(|r| r.qid == qid) {
                            rests[k].size = (rests[k].size - fill.size).max(0.0);
                            if rests[k].size <= 1e-12 {
                                rests.remove(k);
                                sq.tagged.shift_remove(&tag);
                            }
                        }
                        // kept while the quote still rests as this order, dropped once it does not
                        self.core.note_order_fees(si, o.qid, fees);
                        self.core.hear_child_fill(si, &mut fill); // an emulated child's names its parent
                        self.fire_on_fill(fill);
                        filled_side_this_event = Some(o.side); // block the opposite side this event
                        if !reduces {
                            self.core.symbol[si].entry_timestamp = event.timestamp; // stamp the min-hold clock
                        }
                    }
                }
            }
        }
    }
}
