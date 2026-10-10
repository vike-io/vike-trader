//! `OrderTracker`: the order-lifecycle fold the materializer runs every WAL record through, by way of the real `ManagedOrder` FSM.

use std::collections::HashSet;

use crate::JournalRecord;
use indexmap::IndexMap;
use vike_data::ExecOrderRow;
use vike_exec::{Command, Ingest, ManagedOrder, OrderIntent, OrderStatus};
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent};

/// One tracked order: the REAL FSM aggregate plus the one column it does not carry.
///
/// [`ManagedOrder`] is the whole order state — status, resting terms, venue order id, the running
/// fill VWAP. The only thing beside it is `ts`, the materializer-owned monotonic row timestamp
/// (`ExecOrderRow.ts`, what `vike_data::exec_index::recent_order_statuses` collapses on): the FSM
/// is timeless, and an out-of-order event must never rewind the durable row's clock.
pub(super) struct TrackedOrder {
    pub(super) order: ManagedOrder,
    ts: i64,
}

/// Running order-lifecycle state the materializer folds the WAL into (`coid -> current snapshot`).
/// Held by the writer thread across drain passes; rebuilt empty on process restart (see the module
/// note's restart/staleness bound).
#[derive(Default)]
pub(super) struct OrderTracker {
    pub(super) rows: IndexMap<String, TrackedOrder>,
    /// touched-but-symbol-unresolved coids skipped this process run (observability; see module note).
    pub(super) skipped_unresolved: u64,
    /// Lifecycle events `ManagedOrder::apply` REFUSED this process run — the events the live engine
    /// also dropped (`ExecutionEngine::on_event`'s `apply_result.is_err() -> return`). Counted, not
    /// applied: before the FSM fold this module applied them silently, writing history that never
    /// happened. Observability only; a nonzero count is normal (idempotent/out-of-order WS replays).
    dropped_invalid: u64,
}

impl OrderTracker {
    /// Fold ONE journal record into the tracker, recording every coid whose row changed in `touched`.
    pub(super) fn fold_record(&mut self, rec: &JournalRecord, touched: &mut HashSet<String>) {
        match rec {
            JournalRecord::Cmd { msg: Ingest::Command(Command::Order(intent)), now_ms, .. } => {
                self.seed_from_intent(intent, *now_ms, touched);
            }
            JournalRecord::StrategySubmit { intent, now_ms, .. } => {
                self.seed_from_intent(intent, *now_ms, touched);
            }
            // Server-minted submit: the resolved request, journaled AFTER its coid was minted (the
            // write-ahead `Cmd` for the SAME order carried an empty coid we skip). This is what lets
            // a server-minted order that terminalizes without ever filling be seeded — folded exactly
            // like a non-empty-coid Submit intent (its coid is now the minted one).
            JournalRecord::MintedSubmit { req, now_ms, .. } => {
                self.seed_from_intent(
                    &OrderIntent::Submit(Box::new(req.clone())),
                    *now_ms,
                    touched,
                );
            }
            JournalRecord::Cmd { msg: Ingest::Event(ev), .. } => {
                self.fold_event(ev, touched);
            }

            // ── EXHAUSTIVE by design: NO `_` arm. A new `JournalRecord` variant must fail to
            // compile HERE until someone decides whether it feeds the exec log — see the "Adding a
            // variant" contract on [`crate::JournalRecord`]. ⚠ This said the materializer lived in a
            // DIFFERENT crate from the one owning the enum, "which is precisely how a variant used to
            // slip past unnoticed (PR #915)". Since 2026-09-25 it is the SAME crate, so a new variant
            // and this match are one compile apart rather than two — the exhaustiveness below still
            // does the work, but the distance that made #915 possible is gone. Everything below is
            // deliberately ignored:
            //
            // A `Cmd` carrying any OTHER `Ingest` — a non-`Order` command (a Cancel/Modify/… is
            // materialized from the venue's own authoritative `Order*` EVENT, folded by the arm
            // above, never from the local intent), an `Ingest::Watchdog`, or a market-data message
            // (never journaled at all). The refined `Cmd` arms above take the two that matter; this
            // is their complement and is what keeps the match exhaustive over `Cmd`.
            JournalRecord::Cmd { .. } => {}
            // A full-state checkpoint. Its `engines` payload is a restore base for `vike-core`, not
            // an exec-log event stream: every order it describes already materialized from the
            // records that built it, so folding it would re-append stale snapshots.
            JournalRecord::Snap { .. } => {}
            // A periodic portfolio OBSERVATION (equity/balance/positions) — no order, no fill.
            JournalRecord::PortfolioSnap { .. } => {}
            // An emulated conditional's ARM / DISARM. Nothing reached a venue, so there is no
            // `exec_order` row: an armed stop is local runtime state until it FIRES.
            JournalRecord::ConditionalArmed { .. } | JournalRecord::ConditionalDisarmed { .. } => {}
            // A conditional's FIRE and a margin-call LIQUIDATION both RELEASE an order — but with
            // an empty coid, which `seed_from_intent` cannot key on. Both materialize through the
            // `MintedSubmit` (+ later `Fill`) records `apply_intent` writes for them immediately
            // after minting, so folding them here as well would double-seed the same order.
            JournalRecord::ConditionalFire { .. } | JournalRecord::MarginCallLiquidate { .. } => {}
            // A managed GTD/Day expiry DECISION. The cancel it decided reaches the exec log through
            // the venue's own authoritative `OrderCanceled` event; a row here would double-count it
            // (and would land BEFORE the venue confirmed anything).
            JournalRecord::GtdExpire { .. } => {}
            // A wall-clock schedule FIRE decision. The orders `on_schedule` produced arrive as
            // their own `StrategySubmit`/`MintedSubmit`/`Fill` records, folded by the arms above.
            JournalRecord::ScheduleFire { .. } => {}
        }
    }

    /// Seed/enrich rows from a Submit-carrying intent. Empty-coid (server-minted-later) requests are
    /// skipped — they cannot be keyed here (the mint happens in the fold, after this record).
    fn seed_from_intent(
        &mut self,
        intent: &OrderIntent,
        now_ms: i64,
        touched: &mut HashSet<String>,
    ) {
        let reqs: &[OrderRequest] = match intent {
            OrderIntent::Submit(r) => std::slice::from_ref(r.as_ref()),
            OrderIntent::SubmitBatch(rs) => rs.as_slice(),
            _ => &[],
        };
        for r in reqs {
            if r.client_order_id.is_empty() {
                continue;
            }
            let tracked = self.rows.entry(r.client_order_id.clone()).or_insert_with(|| {
                // EXACTLY `ExecutionEngine::gate_and_register`'s registration: a freshly-submitted
                // order enters the FSM at INITIALIZED. The adapter's own `OrderSubmitted` — emitted
                // BEFORE the venue round trip ("Submitted → REST → Accepted|Rejected", the
                // emitter-split contract `bridge_conformance.rs` machine-checks) and journaled on
                // the ingest lane like every other venue event — is what advances it to SUBMITTED.
                TrackedOrder { order: ManagedOrder::new(r.clone()), ts: now_ms }
            });
            // Identity/terms come from the submit; fill them if unset (do not clobber a later
            // fill's accumulated qty/px or a modify's rewritten resting terms).
            let req = &mut tracked.order.request;
            if req.symbol.is_empty() {
                req.venue = r.venue.clone();
                req.symbol = r.symbol.clone();
                req.side = r.side;
                req.qty = r.qty;
                req.order_type = r.order_type.clone();
                req.price = r.price;
                req.trigger_price = r.trigger_price;
            }
            tracked.ts = tracked.ts.max(now_ms);
            touched.insert(r.client_order_id.clone());
        }
    }

    /// Fold one `Order*` lifecycle event into the coid's order — through
    /// [`ManagedOrder::apply`], the live engine's ONE mutator, guards included. A coid seen here
    /// for the first time is adopted first (see [`adopt_order`]).
    ///
    /// An event `apply` REFUSES changes nothing: the order keeps its state, the row's `ts` does not
    /// advance, and the coid is NOT marked touched — so no `exec_order` row is appended for it.
    /// That is exactly what the live engine did with the same record (`on_event` returns on
    /// `apply_result.is_err()`), which is the entire point of folding through the FSM.
    fn fold_event(&mut self, ev: &Event, touched: &mut HashSet<String>) {
        // ONE match over the lifecycle events, for the three things the FSM does NOT give us: the
        // row key, the durable row's monotonic ts, and — for the two fill wraps — the embedded
        // `FillEvent` a bare row still needs for its `(venue, symbol)` partition key. Everything
        // else about this fold is `apply`'s answer.
        //
        // `Event::OrderCancelRejected`/`OrderModifyRejected` are deliberately NOT here: the FSM
        // treats both as advisories that change neither status nor terms, so folding them could
        // only manufacture a row for a coid nothing else in this window mentions.
        let (coid, ts, fill): (&str, i64, Option<&FillEvent>) = match ev {
            Event::OrderSubmitted(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderAccepted(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderTriggered(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderPartiallyFilled(e) => (e.client_order_id.as_str(), e.ts, Some(&e.fill)),
            Event::OrderFilled(e) => (e.client_order_id.as_str(), e.ts, Some(&e.fill)),
            Event::OrderCanceled(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderRejected(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderExpired(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderLiquidated(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderDenied(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderModified(e) => (e.client_order_id.as_str(), e.ts, None),
            _ => return,
        };
        let tracked = self.rows.entry(coid.to_string()).or_insert_with(|| adopt_order(coid, ev));
        // THE fold. No transition table here, no status strings, no fill accumulator — `apply` is
        // the live FSM and is the only mutator of order state on either side of this seam.
        let outcome = tracked.order.apply(ev);
        if outcome.is_ok() {
            // MATERIALIZER-ONLY, not an FSM concern: the `exec_order` partition key. A row created
            // bare — no Submit intent and no `MintedSubmit` in this drain window (module note's
            // restart bound) — learns `(venue, symbol, side)` from the first fill that folds. The
            // live engine never needs this; it holds the request from `gate_and_register`.
            if let Some(f) = fill {
                let req = &mut tracked.order.request;
                if req.symbol.is_empty() {
                    req.venue = f.venue.to_string();
                    req.symbol = f.symbol.to_string();
                    req.side = f.side;
                }
            }
            tracked.ts = tracked.ts.max(ts);
        }
        // (`tracked`'s borrow of `self.rows` ends here.)
        if let Err(e) = outcome {
            self.dropped_invalid = self.dropped_invalid.saturating_add(1);
            tracing::debug!(
                coid = %coid,
                refusal = %e,
                dropped_invalid = self.dropped_invalid,
                "lifecycle event refused by the order FSM; not materialized (the live engine dropped it too)"
            );
            return;
        }
        touched.insert(coid.to_string());
    }
}

/// The [`ManagedOrder`] a coid FIRST seen via a bare lifecycle event is adopted into — the
/// insert-only, materializer-side twin of `ExecutionEngine::reregister_orders` (which adopts a
/// venue-reported order at the status the venue reports).
///
/// The request is a shell: `(venue, symbol, side, qty, …)` stay empty/zero until a Submit intent,
/// a `MintedSubmit` or a fill resolves them — the module's symbol-resolution note.
///
/// The seeded STATUS answers one question — "what state must an order already be in for THIS event
/// to be legal?" — which is that event's allowed-from set in `vike_exec`'s transition table, read
/// ONCE, at adoption. It is not a second copy of the table: three seeds cover all eleven events,
/// because `ACCEPTED` is in the allowed-from set of every event except the four pre-acceptance
/// ones. Adopting is what keeps the module's restart bound at STALENESS rather than turning it into
/// a loss — a post-restart tail whose submit/accept sits below the checkpoint still terminalizes —
/// and every event AFTER the adopting one goes through the guarded `apply` like any other.
pub(super) fn adopt_order(coid: &str, ev: &Event) -> TrackedOrder {
    let mut order =
        ManagedOrder::new(OrderRequest { client_order_id: coid.to_string(), ..Default::default() });
    order.status = match ev {
        // allowed-from {INITIALIZED}
        Event::OrderSubmitted(_) | Event::OrderDenied(_) => OrderStatus::Initialized,
        // allowed-from {SUBMITTED} (`OrderRejected` also accepts INITIALIZED; either seed works)
        Event::OrderAccepted(_) | Event::OrderRejected(_) => OrderStatus::Submitted,
        // Triggered / the two fill wraps / Canceled / Expired / Liquidated / Modified: ACCEPTED is
        // in all of their allowed-from sets (`CAN_RECEIVE_CANCEL` and `MODIFIABLE` included).
        _ => OrderStatus::Accepted,
    };
    TrackedOrder { order, ts: 0 }
}

/// Render the durable `exec_order` snapshot from the FSM aggregate. Every column is READ off
/// [`ManagedOrder`] (status via `OrderStatus::as_str`, the same strings `OrderStatus::parse` reads
/// back on the recon side) — there is no second copy of the order's state that could disagree.
pub(super) fn exec_order_row(tracked: &TrackedOrder) -> ExecOrderRow {
    let o = &tracked.order;
    let r = &o.request;
    ExecOrderRow {
        ts: tracked.ts,
        client_order_id: r.client_order_id.clone(),
        venue: r.venue.clone(),
        symbol: r.symbol.clone(),
        side: r.side,
        qty: r.qty,
        order_type: r.order_type.clone(),
        status: o.status.as_str().to_string(),
        price: r.price,
        trigger_price: r.trigger_price,
        venue_order_id: o.venue_order_id.clone(),
        filled_qty: o.filled_qty,
        avg_fill_px: o.avg_fill_px,
    }
}

#[path = "tracker_tests.rs"]
#[cfg(test)]
mod tracker_tests;
