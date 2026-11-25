//! The CP Gateway order reply-confirmation state machine. Placing an order returns EITHER a
//! question array (warnings to confirm via /iserver/reply/{id}) OR an acceptance. Confirming a
//! reply returns the SAME shape (another question, or acceptance) — so the transport loops
//! parse→reply until Accepted/Rejected or a bounded cap (`IbkrTransportCpapi::place_order`,
//! `MAX_REPLY_HOPS`).
//!
//! Wire shapes LIVE-VERIFIED against a paper CP Gateway (2026-07-15): the
//! `[{"id":..,"message":[..],"messageIds":[..]}]` question-array shape and the
//! `[{"order_id":"..","order_status":".."}]` acceptance-array shape (order_id is a STRING).
//!
//! Only the gateway's stated `error` is a refusal. A 2xx answer in any other shape says nothing about
//! the order, which the gateway may hold, so it is [`PlaceOutcome::Unrecognized`], never a refusal.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlaceOutcome {
    /// The gateway numbered the order. `order_status` is the row's own word (`PreSubmitted` in the capture), `None` when
    /// the row states none: IB's word, not ours, decides whether the order works (`IbkrTransportCpapi::place_order`).
    Accepted {
        order_id: String,
        order_status: Option<String>,
    },
    NeedsReply {
        reply_id: String,
    },
    Rejected {
        reason: String,
    },
    /// Neither a question, an acceptance nor a stated error: the outcome is unknown.
    Unrecognized {
        answer: String,
    },
}

/// Interpret a place-order OR reply response.
pub(crate) fn parse_place_response(v: &Value) -> PlaceOutcome {
    // Error object → rejected.
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        return PlaceOutcome::Rejected { reason: err.to_string() };
    }
    let Some(first) = v.as_array().and_then(|a| a.first()) else {
        return PlaceOutcome::Unrecognized { answer: v.to_string() };
    };
    // Question → needs reply.
    if let Some(id) = first.get("id").and_then(|x| x.as_str())
        && first.get("message").is_some()
    {
        return PlaceOutcome::NeedsReply { reply_id: id.to_string() };
    }
    // Acceptance → order id (string or numeric wire form), with the row's status word.
    let order_status = first.get("order_status").and_then(Value::as_str).map(str::to_string);
    let order_id = first.get("order_id");
    if let Some(oid) = order_id.and_then(|x| x.as_str()) {
        return PlaceOutcome::Accepted { order_id: oid.to_string(), order_status };
    }
    if let Some(oid) = order_id.and_then(|x| x.as_i64()) {
        return PlaceOutcome::Accepted { order_id: oid.to_string(), order_status };
    }
    PlaceOutcome::Unrecognized { answer: v.to_string() }
}

#[cfg(test)]
mod tests {
    use super::{PlaceOutcome, parse_place_response};
    use serde_json::json;
    use std::assert_matches;

    #[test]
    fn question_response_yields_reply_id() {
        let v = json!([{ "id": "reply-abc", "message": ["Are you sure you want to submit this order?"] }]);
        assert_matches!(
            parse_place_response(&v), PlaceOutcome::NeedsReply { reply_id } if reply_id == "reply-abc"
        );
    }

    #[test]
    fn accepted_response_yields_order_id_and_its_status_word() {
        let v = json!([{ "order_id": "123456", "order_status": "Submitted" }]);
        assert_matches!(
            parse_place_response(&v),
            PlaceOutcome::Accepted { order_id, order_status: Some(w) } if order_id == "123456" && w == "Submitted"
        );
        let wordless = json!([{ "order_id": "123456" }]);
        assert_matches!(
            parse_place_response(&wordless),
            PlaceOutcome::Accepted { order_status: None, .. }
        );
    }

    #[test]
    fn accepted_response_with_numeric_order_id() {
        let v = json!([{ "order_id": 123456, "order_status": "Submitted" }]);
        assert_matches!(
            parse_place_response(&v), PlaceOutcome::Accepted { order_id, .. } if order_id == "123456"
        );
    }

    #[test]
    fn error_response_is_rejected() {
        let v = json!({ "error": "Order value exceeds limit" });
        assert_matches!(
            parse_place_response(&v), PlaceOutcome::Rejected { reason } if reason.contains("exceeds")
        );
    }

    /// A 2xx answer that is no question, no acceptance and no stated error says nothing about the
    /// order: unknown, never a refusal.
    #[test]
    fn an_unrecognized_response_is_unknown_not_rejected() {
        for v in
            [json!({ "unexpected": true }), json!([]), json!([{ "order_status": "Submitted" }])]
        {
            assert_matches!(parse_place_response(&v), PlaceOutcome::Unrecognized { .. }, "{v}");
        }
    }
}
