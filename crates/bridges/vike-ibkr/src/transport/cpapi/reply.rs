//! The CP Gateway order reply-confirmation state machine. Placing an order returns EITHER a
//! question array (warnings to confirm via /iserver/reply/{id}) OR an acceptance. Confirming a
//! reply returns the SAME shape (another question, or acceptance) — so the transport loops
//! parse→reply until Accepted/Rejected or a bounded cap (`CpapiTransport::place_order`,
//! `MAX_REPLY_HOPS`).
//!
//! Wire shapes LIVE-VERIFIED against a paper CP Gateway (2026-07-15): the
//! `[{"id":..,"message":[..],"messageIds":[..]}]` question-array shape and the
//! `[{"order_id":"..","order_status":".."}]` acceptance-array shape (order_id is a STRING).

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceOutcome {
    Accepted { order_id: String },
    NeedsReply { reply_id: String },
    Rejected { reason: String },
}

/// Interpret a place-order OR reply response.
pub fn parse_place_response(v: &Value) -> PlaceOutcome {
    // Error object → rejected.
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        return PlaceOutcome::Rejected { reason: err.to_string() };
    }
    let Some(first) = v.as_array().and_then(|a| a.first()) else {
        return PlaceOutcome::Rejected {
            reason: format!("unrecognized cpapi place response: {v}"),
        };
    };
    // Question → needs reply.
    if let Some(id) = first.get("id").and_then(|x| x.as_str()) {
        if first.get("message").is_some() {
            return PlaceOutcome::NeedsReply { reply_id: id.to_string() };
        }
    }
    // Acceptance → order id (string or numeric wire form).
    if let Some(oid) = first.get("order_id").and_then(|x| x.as_str()) {
        return PlaceOutcome::Accepted { order_id: oid.to_string() };
    }
    if let Some(oid) = first.get("order_id").and_then(|x| x.as_i64()) {
        return PlaceOutcome::Accepted { order_id: oid.to_string() };
    }
    PlaceOutcome::Rejected { reason: format!("unrecognized cpapi place row: {first}") }
}

#[cfg(test)]
mod tests {
    use super::{parse_place_response, PlaceOutcome};
    use serde_json::json;

    #[test]
    fn question_response_yields_reply_id() {
        let v = json!([{ "id": "reply-abc", "message": ["Are you sure you want to submit this order?"] }]);
        assert!(
            matches!(parse_place_response(&v), PlaceOutcome::NeedsReply { reply_id } if reply_id == "reply-abc")
        );
    }

    #[test]
    fn accepted_response_yields_order_id() {
        let v = json!([{ "order_id": "123456", "order_status": "Submitted" }]);
        assert!(
            matches!(parse_place_response(&v), PlaceOutcome::Accepted { order_id } if order_id == "123456")
        );
    }

    #[test]
    fn accepted_response_with_numeric_order_id() {
        let v = json!([{ "order_id": 123456, "order_status": "Submitted" }]);
        assert!(
            matches!(parse_place_response(&v), PlaceOutcome::Accepted { order_id } if order_id == "123456")
        );
    }

    #[test]
    fn error_response_is_rejected() {
        let v = json!({ "error": "Order value exceeds limit" });
        assert!(
            matches!(parse_place_response(&v), PlaceOutcome::Rejected { reason } if reason.contains("exceeds"))
        );
    }

    #[test]
    fn unrecognized_response_is_rejected_not_panicking() {
        let v = json!({ "unexpected": true });
        assert!(matches!(parse_place_response(&v), PlaceOutcome::Rejected { .. }));
    }
}
