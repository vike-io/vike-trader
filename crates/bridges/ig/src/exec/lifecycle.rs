//! IG order-lifecycle decisions that need no network: what a create, a cancel and an amend MEAN once
//! IG has answered.
//!
//! `exec.rs`'s command thread owns the I/O (the session, the retry ladder, the shared maps); this
//! module owns the DECISIONS, each taking the I/O as a closure so the offline suite
//! (`crates/bridges/ig/src/exec/lifecycle_tests.rs`) drives the real decision with fabricated IG
//! replies. The contract every function here serves is the venue-adapter contract's: **no order
//! may silently vanish, and a rejection carries the venue's own reason.**
//!
//! - **Create.** [`non_epic_refusal`] turns a symbol that cannot be an IG epic into an explicit
//!   rejection BEFORE the network (IG answers a bare `EURUSD` with HTTP 500
//!   `error.service.create.working.order.generic`, a code that names nothing).
//! - **Cancel.** [`cancel_working_order`]: `DELETE /workingorders/otc/{dealId}` answers a
//!   `dealReference`, not an outcome. The outcome is that reference's `/confirms` reply, and
//!   `OrderCanceled` (terminal) is emitted only for an ACCEPTED one ([`map_cancel_confirm`]).
//! - **Amend.** [`amend_working_order`]: `PUT /workingorders/otc/{dealId}` v2 moves the LEVEL of a
//!   resting order and nothing else — IG's body has no size field — so a size change is an explicit
//!   `OrderModifyRejected`, never a silent no-op, and the answer is again the `/confirms` reply
//!   (`status: AMENDED`).
//!
//! ⚠ **An amend's `dealReference` is deliberately NOT recorded in `DealRefs`.** Any streamed line
//! for it that reached `decode_trade_confirm` would be read as a deal confirm, and an `AMENDED`
//! confirm carries a `size` and a `level`, exactly what the decoder reads as an execution. (The
//! decoder is also taught `AMENDED` now, and the stream decodes only a position OPENED; this is
//! the third belt.)

use serde_json::{Value, json};
use vike_bridge_core::exec_actor::CancelOutcome;
use vike_model::events::{Event, OrderModified, OrderModifyRejected};
use vike_model::{OrderRequest, OrderType};

use super::stream::DealRefMap;
use super::{VENUE, fmt_level, is_market};
use crate::rest::IgApiError;

/// An amend that names a size within this of the order's own is not a size change.
const QTY_TOLERANCE: f64 = 1e-9;

/// The refusal for a symbol that cannot be an IG epic, or `None` when it can.
///
/// ⚠ **A SHAPE test, not a catalog lookup, on purpose.** Every IG epic is dotted
/// (`CS.D.EURUSD.MINI.IP`, `IX.D.DAX.DAILY.IP`) and a bare pair name (`EURUSD`) never is. This
/// bridge has no symbol->epic mapping, and inventing one here would be a guess about which of an
/// instrument's several epics (MINI, CFD, TODAY) the operator means. So the symbol is taken as the
/// epic, and only the one thing that can be known without IG — that it is not one — is refused
/// before the network, naming the form to submit.
pub(crate) fn non_epic_refusal(symbol: &str) -> Option<String> {
    if symbol.contains('.') {
        return None;
    }
    Some(format!(
        "ig: symbol {symbol:?} is not an IG epic — IG addresses a market by its epic (dotted, e.g. \
         CS.D.EURUSD.MINI.IP), never by a bare pair name, and this bridge maps no symbol to an \
         epic; submit the epic"
    ))
}

/// A `/confirms` reply to a working-order DELETE, as a [`CancelOutcome`].
///
/// `OrderCanceled` is TERMINAL, so it is earned: `dealStatus: ACCEPTED` and a `status` of
/// `DELETED` (an absent `status` is taken as DELETED — the deal's `dealStatus` is the verdict and
/// the DELETE endpoint can only delete). A `REJECTED` confirm carries IG's reason; anything else
/// is named, never guessed into a terminal.
pub(crate) fn map_cancel_confirm(confirm: &Value) -> CancelOutcome {
    let text = |k: &str| confirm.get(k).and_then(Value::as_str);
    match (text("dealStatus"), text("status")) {
        (Some("REJECTED"), _) => CancelOutcome::Rejected(format!(
            "IG rejected the cancel: {}",
            text("reason").unwrap_or("REJECTED")
        )),
        (Some("ACCEPTED"), None | Some("DELETED")) => CancelOutcome::Canceled,
        (deal_status, status) => CancelOutcome::Rejected(format!(
            "IG's cancel confirm is not a deletion (dealStatus {}, status {}) — the order is NOT \
             reported cancelled",
            deal_status.unwrap_or("absent"),
            status.unwrap_or("absent")
        )),
    }
}

/// Cancel the working order `coid` (`deal_id` at IG) and report what IG CONFIRMED.
///
/// `delete(path)` is the DELETE call and `confirm(deal_reference)` the retried `/confirms` read.
/// ⚠ This used to be `Ok(_) => Canceled` on the DELETE's 200 alone — an acknowledgement that a
/// request was taken, reported as an outcome.
///
/// When the confirm cannot be read the answer is a NON-terminal `Rejected` that says so, and the
/// cancel's reference is taught to the stream ([`DealRefMap`]`::record_stream_only`) so IG's
/// streamed `DELETED` confirm — if the cancel did land — still terminalizes the order.
pub(crate) fn cancel_working_order(
    deal_refs: &DealRefMap,
    coid: &str,
    deal_id: &str,
    delete: impl FnOnce(&str) -> Result<Value, IgApiError>,
    confirm: impl FnOnce(&str) -> Result<Value, IgApiError>,
) -> CancelOutcome {
    let path = format!("/workingorders/otc/{deal_id}");
    let response = match delete(&path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "vike_ig::exec", venue = VENUE, %coid, %deal_id,
                status = e.status, error = %e.message, "IG refused the working-order DELETE"
            );
            return CancelOutcome::Rejected(e.to_string());
        }
    };
    let Some(deal_ref) = response.get("dealReference").and_then(Value::as_str) else {
        tracing::warn!(
            target: "vike_ig::exec", venue = VENUE, %coid, %deal_id,
            "IG answered the DELETE without a dealReference — nothing to confirm the cancel with"
        );
        return CancelOutcome::Rejected(
            "IG answered the cancel without a dealReference, so it could not be confirmed — the \
             order may or may not still rest"
                .to_string(),
        );
    };
    match confirm(deal_ref) {
        Ok(c) => {
            let outcome = map_cancel_confirm(&c);
            if let CancelOutcome::Rejected(reason) = &outcome {
                tracing::warn!(
                    target: "vike_ig::exec", venue = VENUE, %coid, %deal_id, %deal_ref,
                    %reason, "IG's confirm for the cancel is not a deletion"
                );
            }
            outcome
        }
        Err(e) => {
            deal_refs.lock().unwrap().record_stream_only(coid, deal_ref);
            tracing::warn!(
                target: "vike_ig::exec", venue = VENUE, %coid, %deal_id, %deal_ref,
                status = e.status, error = %e.message,
                "IG took the working-order DELETE but its /confirms never answered — the cancel is \
                 UNCONFIRMED; the order stays live until the trade stream or reconcile settles it"
            );
            CancelOutcome::Rejected(format!(
                "cancel sent (dealReference {deal_ref}) but IG's /confirms did not answer ({e}) — \
                 the order's state is unconfirmed"
            ))
        }
    }
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
/// reason it is not an amend.
pub(crate) fn map_amend_confirm(confirm: &Value) -> Result<(), String> {
    let text = |k: &str| confirm.get(k).and_then(Value::as_str);
    match (text("dealStatus"), text("status")) {
        (Some("ACCEPTED"), Some("AMENDED")) => Ok(()),
        (Some("REJECTED"), _) => {
            Err(format!("IG rejected the amend: {}", text("reason").unwrap_or("REJECTED")))
        }
        (deal_status, status) => Err(format!(
            "IG's amend confirm is not an amendment (dealStatus {}, status {})",
            deal_status.unwrap_or("absent"),
            status.unwrap_or("absent")
        )),
    }
}

/// Amend a resting order's PRICE and say what happened: exactly ONE event, `OrderModified` or a
/// NON-terminal `OrderModifyRejected` (the order keeps its terms, or — when IG's answer was lost —
/// the reason says they are unconfirmed).
///
/// `put(path, body)` is the PUT and `confirm(deal_reference)` the retried `/confirms` read.
/// `deal_id` is the order's `dealId` as this process knows it (`None` = unknown, e.g. a confirm
/// that never answered, or an order from before a restart).
pub(crate) fn amend_working_order(
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    deal_id: Option<&str>,
    ts: i64,
    put: impl FnOnce(&str, &Value) -> Result<Value, IgApiError>,
    confirm: impl FnOnce(&str) -> Result<Value, IgApiError>,
) -> Event {
    let coid = order.client_order_id.as_str();
    let reject = |reason: String| {
        tracing::warn!(target: "vike_ig::exec", venue = VENUE, %coid, %reason, "IG amend not applied");
        Event::OrderModifyRejected(OrderModifyRejected {
            client_order_id: coid.to_string(),
            reason: reason.into(),
            ts,
        })
    };
    if let Some(q) = new_qty
        && (!q.is_finite() || (q - order.qty).abs() > QTY_TOLERANCE)
    {
        return reject(format!(
            "ig: a working order's size cannot be amended (IG's PUT /workingorders/otc moves the \
             level only; asked {q}, order is {}) — cancel and resubmit at the new size",
            order.qty
        ));
    }
    if is_market(order) {
        return reject("ig: only a resting LIMIT or STOP order can be amended".to_string());
    }
    let Some(level) = new_price.filter(|p| p.is_finite() && *p > 0.0) else {
        return reject(format!("ig: the amend carries no usable new level ({new_price:?})"));
    };
    let Some(deal_id) = deal_id else {
        return reject(format!(
            "ig: no working order for {coid} — this process holds no dealId for it (its confirm \
             never answered, or it predates a restart)"
        ));
    };
    let body = build_amend_body(order, level);
    let response = match put(&format!("/workingorders/otc/{deal_id}"), &body) {
        Ok(v) => v,
        Err(e) => return reject(e.to_string()),
    };
    let Some(deal_ref) = response.get("dealReference").and_then(Value::as_str) else {
        return reject(
            "IG answered the amend without a dealReference, so it could not be confirmed — the \
             order's level is unconfirmed"
                .to_string(),
        );
    };
    match confirm(deal_ref) {
        Ok(c) => match map_amend_confirm(&c) {
            Ok(()) => Event::OrderModified(OrderModified {
                client_order_id: coid.to_string(),
                venue_order_id: Some(deal_id.into()),
                new_qty: None,
                new_price: Some(level),
                ts,
            }),
            Err(reason) => reject(reason),
        },
        Err(e) => reject(format!(
            "amend sent (dealReference {deal_ref}) but IG's /confirms did not answer ({e}) — the \
             order's level is unconfirmed"
        )),
    }
}

#[path = "lifecycle_tests.rs"]
#[cfg(test)]
mod lifecycle_tests;
