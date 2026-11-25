//! IG order-lifecycle decisions that need no network: what a create, a cancel and an amend MEAN once
//! IG has answered.
//!
//! `execution/mod.rs`'s command thread owns the I/O (the session, the retry ladder, the shared maps); this
//! module owns the DECISIONS, each taking the I/O as a closure so the offline suite
//! (`crates/bridges/ig/src/execution/lifecycle_tests.rs`) drives the real decision with fabricated IG
//! replies. The contract every function here serves is the venue-adapter contract's: **no order
//! may silently vanish, and a rejection carries the venue's own reason.**
//!
//! - **Create.** [`non_epic_refusal`] turns a symbol that cannot be an IG epic into an explicit
//!   rejection BEFORE the network (IG answers a bare `EURUSD` with HTTP 500
//!   `error.service.create.working.order.generic`, a code that names nothing).
//! - **Cancel.** [`cancel_working_order`]: `DELETE /workingorders/otc/{dealId}` answers a
//!   `dealReference`, not an outcome. The outcome is that reference's `/confirms` reply, and
//!   `OrderCanceled` (terminal) is emitted only for an ACCEPTED one ([`map_cancel_confirm`]). A cancel
//!   whose outcome is unknown (the DELETE's reply, its reference or its confirm lost, or IG saying it does
//!   not know) is OUTCOME UNKNOWN (decision 0303): the shared cancel answer, never sent again, and settled by
//!   IG's activity history and working-order list ([`Unsettled::note_cancel`]) or the stream's `DELETED`.
//! - **Amend.** [`amend_working_order`]: `PUT /workingorders/otc/{dealId}` v2 moves the LEVEL of a
//!   resting order and nothing else — IG's body has no size field — so a size change is an explicit
//!   `OrderModifyRejected`, never a silent no-op, and the answer is again the `/confirms` reply
//!   (`status: AMENDED`). An amend whose answer was lost is OUTCOME UNKNOWN through the shared modify answer
//!   ([`Unsettled::note_amend`]) and is settled by IG's working-order list: the order moves to the level IG
//!   lists when IG moved it.
//!
//! ⚠ **An amend's `dealReference` is deliberately NOT recorded in `DealRefs`.** Any streamed line
//! for it that reached `decode_trade_confirm` would be read as a deal confirm, and an `AMENDED`
//! confirm carries a `size` and a `level`, exactly what the decoder reads as an execution. (The
//! decoder is also taught `AMENDED` now, and the stream decodes only a position OPENED; this is
//! the third belt.)

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::exec_actor::{ExecutionClientActorCancelOutcome, cancel_event};
use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_bridge_core::rest::{amend_answer_time, cancel_rejected, modify_rejected};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_execution::execution_engine::LOCAL_VIEW_QUANTITY_TOLERANCE;
use vike_model::events::{Event, OrderCanceled, OrderModified, OrderModifyRejected};
use vike_model::{OrderRefusalId, OrderRequest, OrderType};

use super::event_mapper::rejected_with_unknown_result;
use super::{VENUE, confirm_echoes, fmt_level, map_confirm, note_deal_id};
use crate::execution::user_data::OrderVenueIdRegistryDealReference;
use vike_bridge_core::transport::http_error::VenueApiHttpError;

/// A level IG lists within this of the order's own is that level: the wire writes five decimals (`fmt_level`).
const LEVEL_TOLERANCE: f64 = 1e-9;

/// The pauses between asking again about an unsettled order; the last one repeats. The submit's own ladder has just spent
/// about 2.4 s. A `/confirms` read is a non-trading request and IG meters those, so this is a backoff and not a poll.
const ASK_AGAIN_AFTER: [Duration; 5] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(30),
];

/// How long an unsettled order is asked about before it is given up, loudly: an open or a close is then the reconcile's,
/// an amend's level is the operator's to read (the reconcile corrects no level). How long IG keeps a `/confirms` answer
/// is unmeasured, so this is a bound on the asking and says nothing about IG.
const ASK_AGAIN_FOR: Duration = Duration::from_secs(600);

/// What a close written with no reference is matched against in the activity history: a row this long before the POST
/// still counts, for the difference between IG's clock and this box's. A close of the same epic, side and size inside it
/// can be matched in its place; its deal is then one the engine already booked, and it drops the fill as a duplicate.
const CLOSE_CLOCK_MARGIN_MS: i64 = 30_000;

/// **The orders a `/confirms` ladder left unsettled, asked about again on the exec thread** (`execution/mod.rs`'s `next_command`,
/// one read between two commands). Ending the ladder with nothing more to ask was the lie: an order whose POST IG took and
/// whose confirm never came read `Accepted` with no venue id for ever, and since only `/confirms` carries a `REJECTED` such as
/// `MARKET_CLOSED`, **a stop IG refused read as protection**. An unknown create or close was left to the stuck-order ladder
/// instead, whose `OrderRejected` could stand over a live deal.
///
/// Each row ends at its first conclusive answer, or at [`ASK_AGAIN_FOR`] with an `error`.
#[derive(Default)]
pub(crate) struct Unsettled {
    rows: Vec<UnsettledRow>,
}

struct UnsettledRow {
    order_client_id: String,
    asking: Asking,
    asked: usize,
    next_at: Instant,
    give_up_at: Instant,
}

enum Asking {
    /// An open: its `/confirms` read. `acked`: IG answered the POST, so the reply needs no echo. `accept_sent`: the
    /// id-less `OrderAccepted` went out, so a rejection cannot be `OrderRejected` any more: the FSM refuses that from
    /// `Accepted`, and the order would read live for ever.
    Confirm { deal_ref: String, market: bool, acked: bool, accept_sent: bool },
    /// A close carries no reference (IG's close body has no such field), so the one read is the activity history.
    Close { epic: String, side: i32, qty: f64, posted_ms: i64 },
    /// An amend IG may have applied with nothing to say whether it did (its PUT's answer or its confirm was lost): the
    /// read is IG's working-order list, for the level `deal_id` rests at. `before`: the order's level when it went out.
    Amend { deal_id: String, before: Option<f64> },
    /// A cancel IG may have carried out with nothing to say whether it did (S4b IG-1: the DELETE's reply was lost, and with it
    /// the reference its streamed `DELETED` is known by). The reads are IG's activity history, for the order deleted in IG's
    /// words, and its working-order list, for the order still resting. `deal_ref`: the DELETE's reference when IG answered
    /// with one (the stream still listens for it). `posted_ms`: when the DELETE was sent.
    Cancel { deal_id: String, deal_ref: Option<String>, posted_ms: i64 },
}

impl Unsettled {
    /// An open whose ladder settled nothing.
    pub(crate) fn note_confirm(
        &mut self,
        order_client_id: &str,
        deal_ref: &str,
        market: bool,
        acked: bool,
        accept_sent: bool,
        now: Instant,
    ) {
        let asking = Asking::Confirm { deal_ref: deal_ref.to_string(), market, acked, accept_sent };
        self.push(order_client_id, asking, now);
    }

    /// A close whose outcome is unknown, posted at `posted_ms`.
    pub(crate) fn note_close(&mut self, req: &OrderRequest, posted_ms: i64, now: Instant) {
        let asking =
            Asking::Close { epic: req.symbol.to_string(), side: req.side, qty: req.qty, posted_ms };
        self.push(&req.order_client_id, asking, now);
    }

    /// An amend of `order` (`deal_id` at IG) that [`amend_working_order`] left unsettled. It replaces what an
    /// earlier amend of the order left: the level IG lists is the one answer to both.
    pub(crate) fn note_amend(&mut self, order: &OrderRequest, deal_id: &str, now: Instant) {
        self.forget_amend(&order.order_client_id);
        let before =
            if order.order_type == OrderType::Limit { order.price } else { order.trigger_price };
        self.push(
            &order.order_client_id,
            Asking::Amend { deal_id: deal_id.to_string(), before },
            now,
        );
    }

    /// A cancel of `order_client_id` (`deal_id` at IG) that [`cancel_working_order`] left of unknown outcome. It replaces what
    /// an earlier cancel of the order left.
    pub(crate) fn note_cancel(
        &mut self,
        order_client_id: &str,
        deal_id: &str,
        deal_ref: Option<&str>,
        posted_ms: i64,
        now: Instant,
    ) {
        self.rows.retain(|r| {
            r.order_client_id != order_client_id || !matches!(r.asking, Asking::Cancel { .. })
        });
        let deal_ref = deal_ref.map(str::to_string);
        self.push(
            order_client_id,
            Asking::Cancel { deal_id: deal_id.to_string(), deal_ref, posted_ms },
            now,
        );
    }

    /// IG confirmed an amend of `order_client_id`: what an earlier one left is not asked about any more.
    pub(crate) fn forget_amend(&mut self, order_client_id: &str) {
        self.rows.retain(|r| {
            r.order_client_id != order_client_id || !matches!(r.asking, Asking::Amend { .. })
        });
    }

    fn push(&mut self, order_client_id: &str, asking: Asking, now: Instant) {
        self.rows.push(UnsettledRow {
            order_client_id: order_client_id.to_string(),
            asking,
            asked: 0,
            next_at: now + ASK_AGAIN_AFTER[0],
            give_up_at: now + ASK_AGAIN_FOR,
        });
    }

    /// When the next row is due, `None` when nothing is unsettled.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.rows.iter().map(|r| r.next_at).min()
    }

    /// Ask about the ONE row that has been due longest, and return what its answer settles. One a call, because a read can
    /// take the agent's whole 30 s in an outage and commands wait behind it. `confirm`, `activity` and `working_orders`
    /// are the reads (the history from a time in epoch ms, as its body; the `GET /workingorders` v2 body).
    pub(crate) fn ask_due(
        &mut self,
        now: Instant,
        ts: i64,
        deal_refs: &OrderVenueIdRegistryDealReference,
        deal_ids: &Arc<Mutex<OrderVenueIdRegistry<String>>>,
        confirm: impl FnOnce(&str) -> Result<Value, VenueApiHttpError>,
        activity: impl FnOnce(i64) -> Result<String, String>,
        working_orders: impl FnOnce() -> Result<String, String>,
    ) -> Vec<Event> {
        let due = self.rows.iter().enumerate().filter(|(_, r)| r.next_at <= now);
        let Some(i) = due.min_by_key(|(_, r)| r.next_at).map(|(i, _)| i) else { return Vec::new() };
        let row = &mut self.rows[i];
        // An amend IG's list settles with nothing to send (IG kept the level, or no longer lists the order) still ends.
        let mut settled_quietly = false;
        let events = match &row.asking {
            Asking::Confirm { deal_ref, market, acked, accept_sent } => match confirm(deal_ref) {
                Ok(c) => confirm_events(
                    &row.order_client_id,
                    ts,
                    deal_ref,
                    *market,
                    *acked,
                    *accept_sent,
                    &c,
                )
                .inspect(|events| {
                    if !events.is_empty() {
                        note_deal_id(deal_ids, &row.order_client_id, &c);
                    }
                }),
                Err(e) => {
                    tracing::debug!(
                        target: "vike_ig::execution", venue = VENUE, order_client_id = %row.order_client_id, %deal_ref,
                        status = e.status, error = %e.message, "IG's /confirms still does not answer"
                    );
                    None
                }
            },
            Asking::Close { epic, side, qty, posted_ms } => {
                let since = posted_ms - CLOSE_CLOCK_MARGIN_MS;
                match activity(since).and_then(|body| {
                    crate::reconcile::close_in_activity(&body, epic, *side, *qty, since)
                }) {
                    Ok(close) => close.map(|c| map_confirm(&row.order_client_id, ts, true, &c)),
                    Err(error) => {
                        tracing::debug!(
                            target: "vike_ig::execution", venue = VENUE, order_client_id = %row.order_client_id, %error,
                            "IG's activity history could not be read for an unknown close"
                        );
                        None
                    }
                }
            }
            Asking::Amend { deal_id, before } => {
                match working_orders().and_then(|body| listed_level(&body, deal_id)) {
                    Ok(listed) => {
                        let events = amend_settles(
                            &row.order_client_id,
                            deal_id,
                            *before,
                            listed,
                            amend_answer_time(None),
                        );
                        settled_quietly = events.is_empty();
                        Some(events)
                    }
                    Err(error) => {
                        tracing::debug!(
                            target: "vike_ig::execution", venue = VENUE, order_client_id = %row.order_client_id, %error,
                            "IG's working orders could not be read for an amend nothing confirmed"
                        );
                        None
                    }
                }
            }
            // The stream heard the DELETE's own `DELETED` meanwhile and ended the order: nothing is left to ask.
            Asking::Cancel { deal_ref: Some(deal_ref), .. }
                if !deal_refs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .awaits_stream(deal_ref) =>
            {
                settled_quietly = true;
                None
            }
            Asking::Cancel { deal_id, posted_ms, .. } => {
                let since = posted_ms - CLOSE_CLOCK_MARGIN_MS;
                match activity(since)
                    .and_then(|body| crate::reconcile::deleted_in_activity(&body, deal_id))
                {
                    Ok(true) => Some(vec![cancel_event(
                        &row.order_client_id,
                        ExecutionClientActorCancelOutcome::Canceled,
                    )]),
                    // Not deleted in IG's words (yet): still listed, the DELETE did not land and the order rests, which
                    // is what the platform holds; gone, its activity is late or it filled (the stream's and the
                    // reconcile's to say), and it is asked again.
                    Ok(false) => {
                        match working_orders().and_then(|body| still_listed(&body, deal_id)) {
                            Ok(listed) => {
                                if listed {
                                    tracing::info!(
                                        target: "vike_ig::execution", venue = VENUE, order_client_id = %row.order_client_id,
                                        %deal_id, "IG still lists the working order: the cancel of unknown outcome did not land"
                                    );
                                }
                                settled_quietly = listed;
                                None
                            }
                            Err(error) => {
                                tracing::debug!(
                                    target: "vike_ig::execution", venue = VENUE, order_client_id = %row.order_client_id, %error,
                                    "IG's working orders could not be read for a cancel of unknown outcome"
                                );
                                None
                            }
                        }
                    }
                    Err(error) => {
                        tracing::debug!(
                            target: "vike_ig::execution", venue = VENUE, order_client_id = %row.order_client_id, %error,
                            "IG's activity history could not be read for a cancel of unknown outcome"
                        );
                        None
                    }
                }
            }
        };
        if settled_quietly {
            self.rows.remove(i);
            return Vec::new();
        }
        if let Some(events) = events.filter(|events| !events.is_empty()) {
            tracing::warn!(
                target: "vike_ig::execution", venue = VENUE, order_client_id = %row.order_client_id, asked = row.asked + 1,
                "IG answered about an order its first answer left unsettled"
            );
            let refused = events
                .iter()
                .any(|e| matches!(e, Event::OrderRejected(_) | Event::OrderCanceled(_)));
            // An ended order is never asked about, cancelled or amended again.
            if refused {
                deal_refs.lock().unwrap().forget(&row.order_client_id);
                deal_ids
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .forget(&row.order_client_id);
            }
            self.rows.remove(i);
            return events;
        }
        if now >= row.give_up_at {
            // The reconcile reports a working order's level but corrects none, so an amend's words name what does.
            if matches!(row.asking, Asking::Amend { .. }) {
                tracing::error!(
                    target: "vike_ig::execution", venue = VENUE, order_client_id = %row.order_client_id, asked = row.asked + 1,
                    "IG did not list this order's level within {} minutes and it is not asked about any more: the platform keeps the level it holds; read IG's in IG's platform or with `trade venue open-orders ig`",
                    ASK_AGAIN_FOR.as_secs() / 60
                );
            } else {
                tracing::error!(
                    target: "vike_ig::execution", venue = VENUE, order_client_id = %row.order_client_id, asked = row.asked + 1,
                    "IG has not answered about this order and it is not asked about any more: its fate is UNKNOWN, and only the reconcile can show it"
                );
            }
            self.rows.remove(i);
            return Vec::new();
        }
        row.asked += 1;
        row.next_at = now + ASK_AGAIN_AFTER[row.asked.min(ASK_AGAIN_AFTER.len() - 1)];
        Vec::new()
    }
}

/// What one `/confirms` reply settles of an order asked about again: `None` for a reply of a POST IG did not answer that
/// does not name its reference (a 200 about something else proves nothing), else the events, empty when it says nothing yet.
fn confirm_events(
    order_client_id: &str,
    ts: i64,
    deal_ref: &str,
    market: bool,
    acked: bool,
    accept_sent: bool,
    confirm: &Value,
) -> Option<Vec<Event>> {
    if !acked && !confirm_echoes(confirm, deal_ref) {
        return None;
    }
    let text = |k: &str| confirm.get(k).and_then(Value::as_str);
    // A rejection whose reason says IG does not know the result ends nothing: `map_confirm` says nothing yet for it.
    if accept_sent
        && text("dealStatus") == Some("REJECTED")
        && !rejected_with_unknown_result(confirm)
    {
        let reason = text("reason").unwrap_or("REJECTED");
        tracing::warn!(
            target: "vike_ig::execution", venue = VENUE, %order_client_id, %reason,
            "IG REJECTED a deal this process had reported accepted because its /confirms had not answered"
        );
        return Some(vec![Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: super::event_mapper::rejection_words(reason).into(),
            timestamp: ts,
            post_only_would_cross: false,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })]);
    }
    Some(map_confirm(order_client_id, ts, market, confirm))
}

/// The refusal for a name that is not an IG epic, or `None` when it is one: the ONE epic-shape rule,
/// `vike_catalog::is_ig_epic` (five upper-case dotted segments), which the history lane holds a series to as well.
///
/// ⚠ **A SHAPE test, not a lookup.** The epic of a USER name is its kept row's (`crate::epic`, decision 0290: one
/// pair has several epics, MINI, CFD, TODAY, and choosing one is the operator's row, never a guess); this is asked of
/// that row's name, so a row IG could not address is refused before the network, naming the form, where IG answers
/// a name that is no epic with HTTP 500 `error.service.create.working.order.generic`.
pub(crate) fn non_epic_refusal(symbol: &str) -> Option<String> {
    if vike_catalog::is_ig_epic(symbol) {
        return None;
    }
    Some(format!(
        "ig: {symbol:?} is not an epic: IG addresses a market by its epic, five upper-case dotted segments \
         (e.g. CS.D.EURUSD.MINI.IP); nothing was asked of IG"
    ))
}

/// A `/confirms` reply to a working-order DELETE, as a [`ExecutionClientActorCancelOutcome`].
///
/// `OrderCanceled` is TERMINAL, so it is earned: `dealStatus: ACCEPTED` and a `status` of
/// `DELETED` (an absent `status` is taken as DELETED — the deal's `dealStatus` is the verdict and
/// the DELETE endpoint can only delete). A `REJECTED` confirm carries IG's reason, unless that reason says IG does
/// not know the result ([`rejected_with_unknown_result`]): then the cancel is unconfirmed, not refused. Anything else
/// is named, never guessed into a terminal.
pub(crate) fn map_cancel_confirm(confirm: &Value) -> ExecutionClientActorCancelOutcome {
    let text = |k: &str| confirm.get(k).and_then(Value::as_str);
    if rejected_with_unknown_result(confirm) {
        return ExecutionClientActorCancelOutcome::Rejected(
            format!(
                "IG does not know whether the cancel took (reason {}): the order's state is unconfirmed; the trade \
                 stream or IG's activity settles it",
                text("reason").unwrap_or_default()
            ),
            OrderRefusalId::OutcomeUnknown,
        );
    }
    match (text("dealStatus"), text("status")) {
        (Some("REJECTED"), _) => ExecutionClientActorCancelOutcome::Rejected(
            format!("IG rejected the cancel: {}", text("reason").unwrap_or("REJECTED")),
            OrderRefusalId::VenueRefused,
        ),
        (Some("ACCEPTED"), None | Some("DELETED")) => ExecutionClientActorCancelOutcome::Canceled,
        // Neither a deletion nor a refusal: whether the cancel took is not said.
        (deal_status, status) => ExecutionClientActorCancelOutcome::Rejected(
            format!(
                "IG's cancel confirm is not a deletion (dealStatus {}, status {}) — the order is NOT \
                 reported cancelled",
                deal_status.unwrap_or("absent"),
                status.unwrap_or("absent")
            ),
            OrderRefusalId::OutcomeUnknown,
        ),
    }
}

/// Cancel the working order `order_client_id` (`deal_id` at IG) and say what IG CONFIRMED, as the event to send: `None`
/// when the trade stream already ended the order with its own `OrderCanceled` (nothing more to send).
///
/// `delete(path)` is the DELETE call and `confirm(deal_reference)` the retried `/confirms` read.
/// ⚠ This used to be `Ok(_) => Canceled` on the DELETE's 200 alone — an acknowledgement that a
/// request was taken, reported as an outcome.
///
/// ⚠ **The cancel's reference is taught to the stream ([`OrderVenueIdRegistryDealReference`]`::record_stream_only`) BEFORE
/// the ladder runs.** IG answers the DELETE in a few hundred milliseconds and streams its `DELETED`
/// confirm right behind it, while the ladder takes seconds (each attempt bounded by the agent's
/// 30 s): taught only after the ladder failed, the reference came after the confirm it was taught
/// for, and the order stayed ACCEPTED locally over a working order IG had deleted. Whichever of the
/// two answers first ends the order, once (`take_stream_only`).
///
/// ⚠ **A cancel IG may have carried out with nothing to say so is OUTCOME UNKNOWN** (decision 0303, S4b IG-1): the
/// DELETE's reply lost after the write, a reply naming no reference, a confirm that never answered or one saying IG
/// does not know. Its answer is the shared `vike_bridge_core::rest::cancel_rejected` over the sentinel
/// (`order.outcome_unknown`), never a refusal and never a second DELETE, and the order is read back
/// ([`Unsettled::note_cancel`]). A DELETE IG refused, or one that provably never left, is the same builder over the
/// error's own outcome (`order.venue_refused`, `order.not_sent`) and reads nothing.
pub(crate) fn cancel_working_order(
    deal_refs: &OrderVenueIdRegistryDealReference,
    unsettled: &mut Unsettled,
    order_client_id: &str,
    deal_id: &str,
    delete: impl FnOnce(&str) -> Result<Value, VenueApiHttpError>,
    confirm: impl FnOnce(&str) -> Result<Value, VenueApiHttpError>,
) -> Option<Event> {
    let path = format!("/workingorders/otc/{deal_id}");
    let posted_ms = vike_model::now_ms();
    let response = match delete(&path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "vike_ig::execution", venue = VENUE, %order_client_id, %deal_id,
                status = e.status, error = %e.message, outcome_unknown = e.outcome_unknown(),
                "IG's working-order DELETE failed"
            );
            if e.outcome_unknown() {
                unsettled.note_cancel(order_client_id, deal_id, None, posted_ms, Instant::now());
            }
            return Some(cancel_rejected(order_client_id, &e.to_venue_error()));
        }
    };
    let mut unknown = |msg: String, deal_ref: Option<&str>| {
        unsettled.note_cancel(order_client_id, deal_id, deal_ref, posted_ms, Instant::now());
        cancel_rejected(order_client_id, &VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg })
    };
    let Some(deal_ref) = response.get("dealReference").and_then(Value::as_str) else {
        tracing::warn!(
            target: "vike_ig::execution", venue = VENUE, %order_client_id, %deal_id,
            "IG answered the DELETE without a dealReference — nothing to confirm the cancel with"
        );
        return Some(unknown(
            "IG answered the cancel without a dealReference, so it could not be confirmed — the \
             order may or may not still rest"
                .to_string(),
            None,
        ));
    };
    deal_refs.lock().unwrap().record_stream_only(order_client_id, deal_ref);
    let confirmed = confirm(deal_ref);
    // An answered ladder takes the reference back, so a late streamed copy ends nothing twice. A confirm that says IG does
    // not know the result answers nothing: the stream keeps listening for the deletion.
    let stream_waits = {
        let mut refs = deal_refs.lock().unwrap();
        if confirmed.as_ref().is_ok_and(|c| !rejected_with_unknown_result(c)) {
            refs.take_stream_only(deal_ref).is_some()
        } else {
            refs.awaits_stream(deal_ref)
        }
    };
    if !stream_waits {
        tracing::info!(
            target: "vike_ig::execution", venue = VENUE, %order_client_id, %deal_id, %deal_ref,
            "the trade stream confirmed the cancel before IG's /confirms did; the order is ended once"
        );
        return None;
    }
    Some(match confirmed {
        Ok(c) => match map_cancel_confirm(&c) {
            ExecutionClientActorCancelOutcome::Rejected(reason, _)
                if rejected_with_unknown_result(&c) =>
            {
                unknown(reason, Some(deal_ref))
            }
            outcome => {
                if let ExecutionClientActorCancelOutcome::Rejected(reason, _) = &outcome {
                    tracing::warn!(
                        target: "vike_ig::execution", venue = VENUE, %order_client_id, %deal_id, %deal_ref,
                        %reason, "IG's confirm for the cancel is not a deletion"
                    );
                }
                cancel_event(order_client_id, outcome)
            }
        },
        Err(e) => {
            tracing::warn!(
                target: "vike_ig::execution", venue = VENUE, %order_client_id, %deal_id, %deal_ref,
                status = e.status, error = %e.message,
                "IG took the working-order DELETE but its /confirms never answered — the cancel is \
                 UNCONFIRMED; the order stays live until the trade stream or IG's activity settles it"
            );
            unknown(
                format!(
                    "cancel sent (dealReference {deal_ref}) but IG's /confirms did not answer ({e}) — \
                     the order's state is unconfirmed"
                ),
                Some(deal_ref),
            )
        }
    })
}

/// The `PUT /workingorders/otc/{dealId}` v2 body: the level, the order's kind and its TIF.
///
/// `type` and `timeInForce` are REQUIRED by the endpoint even for a level-only move, so they
/// restate what `build_request` created the order with (LIMIT/STOP from the order's kind, GTC —
/// every working order here rests `GOOD_TILL_CANCELLED`, so `goodTillDate` is null). There is no
/// size field to send: that is the endpoint's fact, and why [`amend_working_order`] refuses one.
pub(crate) fn build_amend_body(order: &OrderRequest, level: f64) -> Value {
    json!({
        "level": fmt_level(level),
        "type": if order.order_type == OrderType::Limit { "LIMIT" } else { "STOP" },
        "timeInForce": "GOOD_TILL_CANCELLED",
        "goodTillDate": Value::Null,
    })
}

/// A `/confirms` reply to a working-order PUT: `Ok(())` for `ACCEPTED` + `AMENDED`, else the
/// reason it is not an amend and its catalog id: IG's `REJECTED` is its refusal; any other shape says neither that the
/// amend took nor that IG refused it.
pub(crate) fn map_amend_confirm(confirm: &Value) -> Result<(), (String, OrderRefusalId)> {
    let text = |k: &str| confirm.get(k).and_then(Value::as_str);
    match (text("dealStatus"), text("status")) {
        (Some("ACCEPTED"), Some("AMENDED")) => Ok(()),
        (Some("REJECTED"), _) => Err((
            format!("IG rejected the amend: {}", text("reason").unwrap_or("REJECTED")),
            OrderRefusalId::VenueRefused,
        )),
        (deal_status, status) => Err((
            format!(
                "IG's amend confirm is not an amendment (dealStatus {}, status {})",
                deal_status.unwrap_or("absent"),
                status.unwrap_or("absent")
            ),
            OrderRefusalId::OutcomeUnknown,
        )),
    }
}

/// Amend a resting order's PRICE and say what happened: exactly ONE event, `OrderModified` or a
/// NON-terminal `OrderModifyRejected` (the order keeps its terms, or — when IG's answer was lost —
/// OUTCOME UNKNOWN), and whether IG may have moved the level with nothing to say whether it did (`true`: the exec
/// thread reads IG's working orders for it, [`Unsettled::note_amend`]). A sent amend is answered with the shared
/// `vike_bridge_core::rest::modify_rejected` (S4b IG-4, decision 0303): an unknown one over the sentinel,
/// `order.outcome_unknown`, never a refusal and never sent again; a PUT IG refused, or one that provably never left,
/// over the error's own outcome (`order.venue_refused`, `order.not_sent`).
///
/// `put(path, body)` is the PUT and `confirm(deal_reference)` the retried `/confirms` read.
/// `deal_id` is the order's `dealId` as this process knows it (`None` = unknown, e.g. a confirm
/// that never answered, or an order from before a restart). The event is stamped when the answer
/// is had, after the reads (`vike_bridge_core::rest::amend_answer_time`). ⚠ The confirm's `date` is not
/// read as the venue's time: it is zone-less, and the one measurement that calls it UTC (2026-10-30,
/// `crate::execution::user_data`'s `events_for_line`) fell on a day London's clock WAS UTC, so London time (an
/// hour off in summer) is not ruled out. Read it once a summer confirm is measured against the box clock.
pub(crate) fn amend_working_order(
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    deal_id: Option<&str>,
    put: impl FnOnce(&str, &Value) -> Result<Value, VenueApiHttpError>,
    confirm: impl FnOnce(&str) -> Result<Value, VenueApiHttpError>,
) -> (Event, bool) {
    let order_client_id = order.order_client_id.as_str();
    let rejected = |reason: String, refusal: OrderRefusalId| {
        tracing::warn!(target: "vike_ig::execution", venue = VENUE, %order_client_id, %reason, "IG amend not applied");
        Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: order_client_id.to_string(),
            reason: reason.into(),
            timestamp: amend_answer_time(None),
            refusal_id: vike_model::events::refusal_id_of(refusal.word()),
        })
    };
    let reject = |reason: String, refusal: OrderRefusalId| (rejected(reason, refusal), false);
    let unsettled = |reason: String| {
        tracing::warn!(target: "vike_ig::execution", venue = VENUE, %order_client_id, %reason, "IG amend outcome unknown");
        let err = VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: reason };
        (modify_rejected(order_client_id, &err, amend_answer_time(None)), true)
    };
    // An amend naming a size within the one quantity tolerance of the order's own is not a size change.
    if let Some(q) = new_qty
        && (!q.is_finite() || (q - order.qty).abs() > LOCAL_VIEW_QUANTITY_TOLERANCE)
    {
        return reject(
            format!(
                "ig: a working order's size cannot be amended (IG's PUT /workingorders/otc moves the \
                 level only; asked {q}, order is {}) — cancel and resubmit at the new size",
                order.qty
            ),
            OrderRefusalId::VenueCapability,
        );
    }
    // Named, not `!is_market`: every other kind (a market order, and one this bridge builds no order for)
    // rests nothing to amend, and `build_amend_body` writes LIMIT or STOP only.
    if !matches!(order.order_type, OrderType::Limit | OrderType::Stop) {
        return reject(
            "ig: only a resting LIMIT or STOP order can be amended".to_string(),
            OrderRefusalId::AmendRefused,
        );
    }
    let Some(level) = new_price.filter(|p| p.is_finite() && *p > 0.0) else {
        return reject(
            format!("ig: the amend carries no usable new level ({new_price:?})"),
            OrderRefusalId::NotSent,
        );
    };
    let Some(deal_id) = deal_id else {
        return reject(
            format!(
                "ig: no working order for {order_client_id} — this process holds no dealId for it (its confirm \
                 never answered, or it predates a restart)"
            ),
            OrderRefusalId::NotSent,
        );
    };
    let body = build_amend_body(order, level);
    let response = match put(&format!("/workingorders/otc/{deal_id}"), &body) {
        Ok(v) => v,
        // ⚠ An unknown outcome is not "the order keeps its terms": the request may have reached IG, and IG's streamed `WOU`
        // of the move is not read.
        Err(e) => {
            let unknown = e.outcome_unknown();
            tracing::warn!(
                target: "vike_ig::execution", venue = VENUE, %order_client_id, error = %e, outcome_unknown = unknown,
                "IG amend PUT failed"
            );
            return (
                modify_rejected(order_client_id, &e.to_venue_error(), amend_answer_time(None)),
                unknown,
            );
        }
    };
    let Some(deal_ref) = response.get("dealReference").and_then(Value::as_str) else {
        return unsettled(
            "IG answered the amend without a dealReference, so it could not be confirmed — the \
             order's level is unconfirmed"
                .to_string(),
        );
    };
    match confirm(deal_ref) {
        // IG does not know whether the amend took: its working-order list is read for the level (`Unsettled::note_amend`).
        Ok(c) if rejected_with_unknown_result(&c) => unsettled(format!(
            "IG does not know whether the amend took (reason {}): the order's level is unconfirmed until IG's \
             working-order list is read for it",
            c.get("reason").and_then(Value::as_str).unwrap_or_default()
        )),
        Ok(c) => match map_amend_confirm(&c) {
            Ok(()) => (
                Event::OrderModified(OrderModified {
                    order_client_id: order_client_id.to_string(),
                    venue_order_id: Some(deal_id.into()),
                    new_qty: None,
                    new_price: Some(level),
                    new_trigger_price: None,
                    timestamp: amend_answer_time(None),
                }),
                false,
            ),
            Err((reason, refusal)) => reject(reason, refusal),
        },
        Err(e) => unsettled(format!(
            "amend sent (dealReference {deal_ref}) but IG's /confirms did not answer ({e}) — the \
             order's level is unconfirmed"
        )),
    }
}

/// The level IG's working-order list (`GET /workingorders` v2, read by the reconcile's own parser) rests
/// `deal_id` at; `Ok(None)` when IG no longer lists it.
fn listed_level(body: &str, deal_id: &str) -> Result<Option<f64>, String> {
    let orders = crate::reconcile::parse_all_working_orders(body, &|_| None)?;
    let Some(order) = orders.iter().find(|o| o.venue_order_id.as_str() == deal_id) else {
        return Ok(None);
    };
    order
        .price
        .or(order.trigger_price)
        .map(Some)
        .ok_or_else(|| format!("IG lists the working order {deal_id} with no usable level"))
}

/// Does IG's working-order list (`GET /workingorders` v2, the reconcile's own parser) still hold `deal_id`?
fn still_listed(body: &str, deal_id: &str) -> Result<bool, String> {
    let orders = crate::reconcile::parse_all_working_orders(body, &|_| None)?;
    Ok(orders.iter().any(|o| o.venue_order_id.as_str() == deal_id))
}

/// What IG's working-order list settles of an amend nothing confirmed (`listed`: the level IG rests
/// `deal_id` at, `None` when it no longer lists the order): `OrderModified` to IG's level when it is
/// not the order's level before the amend (the amend landed, or IG holds another one), nothing when
/// IG kept that level (the amend did not land) or the order no longer rests (its fill or its cancel
/// is the stream's and the reconcile's to say).
fn amend_settles(
    order_client_id: &str,
    deal_id: &str,
    before: Option<f64>,
    listed: Option<f64>,
    ts: i64,
) -> Vec<Event> {
    match listed {
        Some(level) if before.is_none_or(|b| (level - b).abs() > LEVEL_TOLERANCE) => {
            tracing::warn!(
                target: "vike_ig::execution", venue = VENUE, %order_client_id, %deal_id, level,
                "IG holds the working order at another level than the platform after an amend nothing confirmed; the order moves to IG's"
            );
            vec![Event::OrderModified(OrderModified {
                order_client_id: order_client_id.to_string(),
                venue_order_id: Some(deal_id.into()),
                new_qty: None,
                new_price: Some(level),
                new_trigger_price: None,
                timestamp: ts,
            })]
        }
        Some(level) => {
            tracing::info!(
                target: "vike_ig::execution", venue = VENUE, %order_client_id, %deal_id, level,
                "IG kept the working order's level: the amend nothing confirmed did not land"
            );
            Vec::new()
        }
        None => {
            tracing::info!(
                target: "vike_ig::execution", venue = VENUE, %order_client_id, %deal_id,
                "IG no longer lists the working order an amend left unsettled: how it ended is the stream's and the reconcile's to say"
            );
            Vec::new()
        }
    }
}

#[path = "lifecycle_tests.rs"]
#[cfg(test)]
mod lifecycle_tests;
