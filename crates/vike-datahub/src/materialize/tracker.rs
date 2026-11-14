//! `JournalMaterializerTracker`: the order-lifecycle fold the materializer runs every WAL record through, by way of the real `OrderRecord` FSM.

use std::collections::HashSet;

use indexmap::IndexMap;
use vike_data::ExecOrderRow;
use vike_execution::{Command, Ingest, OrderIntent};
use vike_journal::JournalRecord;
use vike_model::events::{Event, FillEvent};
use vike_model::{OrderRecord, OrderRequest, OrderStatus};

/// One tracked order: the REAL FSM aggregate plus the one column it does not carry.
///
/// [`OrderRecord`] is the whole order state — status, resting terms, venue order id, the running
/// fill VWAP (averaged by the fill's catalog contract value, `vike_catalog::contract_value`, so an
/// inverse contract's is harmonic as the engine's is). The only thing beside it is `ts`, the
/// materializer-owned monotonic row timestamp
/// (`ExecOrderRow.ts`, what `vike_data::execution_index::recent_order_statuses` collapses on): the FSM
/// is timeless, and an out-of-order event must never rewind the durable row's clock.
#[derive(Clone)]
pub(super) struct JournalMaterializerTrackerOrder {
    pub(super) order: OrderRecord,
    timestamp: i64,
}

/// Running order-lifecycle state the materializer folds the WAL into (`order_client_id -> current snapshot`).
/// Held by the writer thread across drain passes; rebuilt empty on process restart (see the module
/// note's restart/staleness bound).
///
/// `Clone` because a drain pass folds into a COPY and commits it only once every append of the pass
/// landed (`crate::materialize`'s `materialize_once`): a failed pass leaves this untouched, so its
/// retry folds the same records once, not twice.
#[derive(Clone, Default)]
pub(super) struct JournalMaterializerTracker {
    pub(super) rows: IndexMap<String, JournalMaterializerTrackerOrder>,
    /// touched-but-symbol-unresolved order client ids skipped this process run (observability; see module note).
    pub(super) skipped_unresolved: u64,
    /// Lifecycle events `OrderRecord::apply` REFUSED this process run — the events the live engine
    /// also dropped (`Engine::on_event`'s `apply_result.is_err() -> return`). Counted, not
    /// applied: before the FSM fold this module applied them silently, writing history that never
    /// happened. Observability only; a nonzero count is normal (idempotent/out-of-order WS replays).
    dropped_invalid: u64,
}

impl JournalMaterializerTracker {
    /// Fold ONE journal record into the tracker, recording every order client id whose row changed in `touched`.
    pub(super) fn fold_record(&mut self, rec: &JournalRecord, touched: &mut HashSet<String>) {
        match rec {
            JournalRecord::Command {
                message: Ingest::Command(Command::Order(intent)),
                now_ms,
                ..
            } => {
                self.seed_from_intent(intent, *now_ms, touched);
            }
            JournalRecord::StrategySubmit { intent, now_ms, .. } => {
                self.seed_from_intent(intent, *now_ms, touched);
            }
            // Server-minted submit: the resolved request, journaled AFTER its order client id was minted (the
            // write-ahead `Command` for the SAME order carried an empty order client id we skip). This is what lets
            // a server-minted order that terminalizes without ever filling be seeded — folded exactly
            // like a non-empty-order-client-id Submit intent (its order client id is now the minted one).
            JournalRecord::MintedSubmit { request: req, now_ms, .. } => {
                self.seed_from_intent(
                    &OrderIntent::Submit(Box::new(req.clone())),
                    *now_ms,
                    touched,
                );
            }
            // A `Command` carrying a VENUE EVENT, over EITHER door: `Ingest::Event`, or
            // `Ingest::GatewayEvent` (an event that came over a gateway's trade link, journal v18),
            // whose cursor is the tradehub's resume bookkeeping and no concern of the exec log.
            // `Ingest::venue_event` answers for both and is exhaustive over `Ingest`, so the next door
            // is a compile error THERE and not a silent drop here. A `Command` carrying anything else --
            // a non-`Order` command (a Cancel/Modify/... is materialized from the venue's own
            // authoritative `Order*` EVENT, folded by this arm, never from the local intent), an
            // `Ingest::Watchdog`, or a market-data message (never journaled at all) -- is none and
            // folds nothing. The refined `Command` arm above takes the order intents.
            JournalRecord::Command { message: msg, .. } => {
                if let Some(event) = msg.venue_event() {
                    self.fold_event(event, touched);
                }
            }

            // ── EXHAUSTIVE by design: NO `_` arm. A new `JournalRecord` variant must fail to
            // compile HERE until someone decides whether it feeds the exec log — see the "Adding a
            // variant" contract on [`vike_journal::JournalRecord`], whose table names this site. ⚠ The
            // crate distance came BACK on 2026-10-11: the enum is vike-journal's and this match is
            // vike-datahub's again (the store writer's, decision 0084), as it was in vike-ops when a
            // variant slipped past unnoticed (PR #915). What catches it now is not the distance but the
            // exhaustiveness below: the enum is not `#[non_exhaustive]`, so a new variant fails THIS
            // crate's build the moment vike-journal adds it. Keep both properties. Everything below is
            // deliberately ignored:
            //
            // A full-state checkpoint. Its `engines` payload is a restore base for `vike-core`, not
            // an exec-log event stream: every order it describes already materialized from the
            // records that built it, so folding it would re-append stale snapshots.
            JournalRecord::Snapshot { .. } => {}
            // A periodic portfolio OBSERVATION (equity/balance/positions) — no order, no fill.
            JournalRecord::PortfolioSnapshot { .. } => {}
            // An emulated conditional's ARM / DISARM. Nothing reached a venue, so there is no
            // `execution_order` row: an armed stop is local runtime state until it FIRES.
            JournalRecord::OrderEmulationArmed { .. }
            | JournalRecord::OrderEmulationDisarmed { .. } => {}
            // A conditional's FIRE and a margin-call LIQUIDATION both RELEASE an order — but with
            // an empty order client id, which `seed_from_intent` cannot key on. Both materialize through the
            // `MintedSubmit` (+ later `Fill`) records `apply_intent` writes for them immediately
            // after minting, so folding them here as well would double-seed the same order.
            JournalRecord::OrderEmulationFire { .. }
            | JournalRecord::MarginCallLiquidate { .. } => {}
            // A managed GTD/Day expiry DECISION. The cancel it decided reaches the exec log through
            // the venue's own authoritative `OrderCanceled` event; a row here would double-count it
            // (and would land BEFORE the venue confirmed anything).
            JournalRecord::GtdExpire { .. } => {}
            // A wall-clock schedule FIRE decision. The orders `on_schedule` produced arrive as
            // their own `StrategySubmit`/`MintedSubmit`/`Fill` records, folded by the arms above.
            JournalRecord::ScheduleFire { .. } => {}
            // A shared close's grace ran out (decision 0271): a DECISION marker like `ScheduleFire`;
            // the step's orders arrive as their own records.
            JournalRecord::SharedCloseTimeout { .. } => {}
            // A mount opened on a strategy file: an observation, no order and no fill.
            JournalRecord::StrategyFileMounted { .. } => {}
        }
    }

    /// Seed/enrich rows from a Submit-carrying intent. Empty-order-client-id (server-minted-later) requests are
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
            if r.order_client_id.is_empty() {
                continue;
            }
            let tracked = self.rows.entry(r.order_client_id.clone()).or_insert_with(|| {
                // EXACTLY `Engine::gate_and_register`'s registration: a freshly-submitted
                // order enters the FSM at INITIALIZED. The adapter's own `OrderSubmitted` — emitted
                // BEFORE the venue round trip ("Submitted → REST → Accepted|Rejected", the
                // emitter-split contract `bridge_conformance.rs` machine-checks) and journaled on
                // the ingest lane like every other venue event — is what advances it to SUBMITTED.
                JournalMaterializerTrackerOrder {
                    order: OrderRecord::new(r.clone()),
                    timestamp: now_ms,
                }
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
                req.account = r.account.clone();
            }
            tracked.timestamp = tracked.timestamp.max(now_ms);
            touched.insert(r.order_client_id.clone());
        }
    }

    /// Fold one `Order*` lifecycle event into the order client id's order — through
    /// [`OrderRecord::apply`], the live engine's ONE mutator, guards included. An order client id seen here
    /// for the first time is adopted first (see [`adopt_order`]).
    ///
    /// An event `apply` REFUSES changes nothing: the order keeps its state, the row's `ts` does not
    /// advance, and the order client id is NOT marked touched — so no `execution_order` row is appended for it.
    /// That is exactly what the live engine did with the same record (`on_event` returns on
    /// `apply_result.is_err()`), which is the entire point of folding through the FSM.
    fn fold_event(&mut self, event: &Event, touched: &mut HashSet<String>) {
        // ONE match over the lifecycle events, for the three things the FSM does NOT give us: the
        // row key, the durable row's monotonic ts, and — for the two fill wraps — the embedded
        // `FillEvent` a bare row still needs for its `(venue, symbol)` partition key. Everything
        // else about this fold is `apply`'s answer.
        //
        // `Event::OrderCancelRejected`/`OrderModifyRejected` are deliberately NOT here: the FSM
        // treats both as advisories that change neither status nor terms, so folding them could
        // only manufacture a row for an order client id nothing else in this window mentions.
        let (order_client_id, ts, fill): (&str, i64, Option<&FillEvent>) = match event {
            Event::OrderSubmitted(e) => (e.order_client_id.as_str(), e.timestamp, None),
            Event::OrderAccepted(e) => (e.order_client_id.as_str(), e.timestamp, None),
            Event::OrderTriggered(e) => (e.order_client_id.as_str(), e.timestamp, None),
            Event::OrderPartiallyFilled(e) => {
                (e.order_client_id.as_str(), e.timestamp, Some(&e.fill))
            }
            Event::OrderFilled(e) => (e.order_client_id.as_str(), e.timestamp, Some(&e.fill)),
            Event::OrderCanceled(e) => (e.order_client_id.as_str(), e.timestamp, None),
            Event::OrderRejected(e) => (e.order_client_id.as_str(), e.timestamp, None),
            Event::OrderExpired(e) => (e.order_client_id.as_str(), e.timestamp, None),
            Event::OrderLiquidated(e) => (e.order_client_id.as_str(), e.timestamp, None),
            Event::OrderDenied(e) => (e.order_client_id.as_str(), e.timestamp, None),
            Event::OrderModified(e) => (e.order_client_id.as_str(), e.timestamp, None),
            _ => return,
        };
        let tracked = self
            .rows
            .entry(order_client_id.to_string())
            .or_insert_with(|| adopt_order(order_client_id, event));
        // THE fold. No transition table here, no status strings, no fill accumulator — `apply` is
        // the live FSM and is the only mutator of order state on either side of this seam. A fill
        // is averaged by its own symbol's contract value, as the engine's fold averages it (harmonic
        // on an inverse contract); a name the catalog gives no value averages as a linear one.
        let value = fill
            .and_then(|f| vike_catalog::contract_value(f.venue.as_str(), f.symbol.as_str()))
            .unwrap_or_default();
        let outcome = tracked.order.apply_valued(event, value);
        if outcome.is_ok() {
            // MATERIALIZER-ONLY, not an FSM concern: the `execution_order` partition key. A row created
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
            tracked.timestamp = tracked.timestamp.max(ts);
        }
        // (`tracked`'s borrow of `self.rows` ends here.)
        if let Err(e) = outcome {
            self.dropped_invalid = self.dropped_invalid.saturating_add(1);
            tracing::debug!(
                order_client_id = %order_client_id,
                refusal = %e,
                dropped_invalid = self.dropped_invalid,
                "lifecycle event refused by the order FSM; not materialized (the live engine dropped it too)"
            );
            return;
        }
        touched.insert(order_client_id.to_string());
    }
}

/// The [`OrderRecord`] an order client id FIRST seen via a bare lifecycle event is adopted into — the
/// insert-only, materializer-side twin of `Engine::reregister_orders` (which adopts a
/// venue-reported order at the status the venue reports).
///
/// The request is a shell: `(venue, symbol, side, qty, …)` stay empty/zero until a Submit intent,
/// a `MintedSubmit` or a fill resolves them — the module's symbol-resolution note.
///
/// The seeded STATUS answers one question — "what state must an order already be in for THIS event
/// to be legal?" — which is that event's allowed-from set in `vike_execution`'s transition table, read
/// ONCE, at adoption. It is not a second copy of the table: three seeds cover all eleven events,
/// because `ACCEPTED` is in the allowed-from set of every event except the four pre-acceptance
/// ones. Adopting is what keeps the module's restart bound at STALENESS rather than turning it into
/// a loss — a post-restart tail whose submit/accept sits below the checkpoint still terminalizes —
/// and every event AFTER the adopting one goes through the guarded `apply` like any other.
pub(super) fn adopt_order(order_client_id: &str, event: &Event) -> JournalMaterializerTrackerOrder {
    let mut order = OrderRecord::new(OrderRequest {
        order_client_id: order_client_id.to_string(),
        ..Default::default()
    });
    order.status = match event {
        // allowed-from {INITIALIZED}
        Event::OrderSubmitted(_) | Event::OrderDenied(_) => OrderStatus::Initialized,
        // allowed-from {SUBMITTED} (`OrderRejected` also accepts INITIALIZED; either seed works)
        Event::OrderAccepted(_) | Event::OrderRejected(_) => OrderStatus::Submitted,
        // Triggered / the two fill wraps / Canceled / Expired / Liquidated / Modified: ACCEPTED is
        // in all of their allowed-from sets (`CAN_RECEIVE_CANCEL` and `MODIFIABLE` included).
        _ => OrderStatus::Accepted,
    };
    JournalMaterializerTrackerOrder { order, timestamp: 0 }
}

/// Render the durable `execution_order` snapshot from the FSM aggregate. Every column is READ off
/// [`OrderRecord`] (status via `OrderStatus::as_str`, the same strings `OrderStatus::parse` reads
/// back on the recon side) — there is no second copy of the order's state that could disagree.
pub(super) fn execution_order_row(tracked: &JournalMaterializerTrackerOrder) -> ExecOrderRow {
    let o = &tracked.order;
    let r = &o.request;
    ExecOrderRow {
        ts: tracked.timestamp,
        order_client_id: r.order_client_id.clone(),
        venue: r.venue.clone(),
        symbol: r.symbol.clone(),
        side: r.side,
        qty: r.qty,
        order_type: r.order_type.as_str().to_owned(),
        status: o.status.as_str().to_string(),
        price: r.price,
        trigger_price: r.trigger_price,
        venue_order_id: o.venue_order_id.clone(),
        filled_qty: o.filled_qty,
        avg_fill_px: o.average_fill_price,
        // The account the submit named, in `AccountLabel`'s screen spelling (`DEFAULT` for the
        // unlabelled account); `None` for an order whose request named none.
        account: r.account.as_ref().map(|a| a.to_string()),
    }
}

#[path = "tracker_tests.rs"]
#[cfg(test)]
mod tracker_tests;
