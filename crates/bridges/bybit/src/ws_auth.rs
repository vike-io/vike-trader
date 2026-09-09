//! Bybit V5 private-WS auth. Exact port of `exec/bybit/ws_auth.py`: the WS signature
//! prehash is `GET/realtime{expires_ms}` — DIFFERENT from REST (ts+key+recv+payload).

use vike_bridge_core::signer::hmac_sha256_hex;
use vike_bridge_core::transport::ws_ack_is_transient;
pub use vike_bridge_core::ws::AckResult;

pub fn bybit_ws_sign(api_secret: &str, expires_ms: i64) -> String {
    hmac_sha256_hex(api_secret.as_bytes(), format!("GET/realtime{expires_ms}").as_bytes())
}

/// `{'op':'auth','args':[api_key, expires, sign]}` — NEVER logged (key + signature).
pub fn build_auth_frame(api_key: &str, api_secret: &str, now_ms: i64) -> serde_json::Value {
    let expires = now_ms + 5000; // expires_skew_ms
    let sign = bybit_ws_sign(api_secret, expires);
    serde_json::json!({"op": "auth", "args": [api_key, expires, sign]})
}

pub fn build_subscribe_frame(topics: &[&str]) -> serde_json::Value {
    serde_json::json!({"op": "subscribe", "args": topics})
}

/// Bybit-shaped keepalive frame (`_bybit_ping`).
pub fn ping_frame() -> String {
    r#"{"req_id":"ping_1","op":"ping"}"#.to_string()
}

pub fn match_op_ack(frame: &serde_json::Value, op: &str) -> AckResult {
    if frame.get("op").and_then(|o| o.as_str()) != Some(op) {
        return AckResult::NotOurs;
    }
    if frame.get("success").and_then(|s| s.as_bool()) != Some(true) {
        let msg = frame.get("ret_msg").and_then(|m| m.as_str()).unwrap_or("");
        // Bybit v5 uses retCode; older frames use ret_code — read either.
        let code = frame
            .get("retCode")
            .or_else(|| frame.get("ret_code"))
            .and_then(|c| c.as_i64())
            .unwrap_or(0);
        let text = format!("Bybit WS {op} failed: {msg}");
        // Transient (reconnect) vs genuine auth failure (surface + stop): the SHARED list — see
        // `vike_bridge_core::transport::ws_ack_is_transient`, which carries Bybit's own 10002 (req
        // ts outside the recv window) / 10006 (too many visits) verbatim. An absent code
        // (`unwrap_or(0)` above) is NOT transient, so a malformed rejection still surfaces.
        return if ws_ack_is_transient(code) {
            AckResult::TransientErr(text)
        } else {
            AckResult::Err(text)
        };
    }
    AckResult::Ok
}

#[cfg(test)]
mod ack_class_tests {
    use super::*;
    #[test]
    fn classifies_transient_vs_auth() {
        let skew = serde_json::json!({"op":"auth","success":false,"ret_msg":"ts","retCode":10002});
        let rate =
            serde_json::json!({"op":"auth","success":false,"ret_msg":"rate","retCode":10006});
        let bad =
            serde_json::json!({"op":"auth","success":false,"ret_msg":"bad key","retCode":10003});
        assert!(matches!(match_op_ack(&skew, "auth"), AckResult::TransientErr(_)));
        assert!(matches!(match_op_ack(&rate, "auth"), AckResult::TransientErr(_)));
        assert!(matches!(match_op_ack(&bad, "auth"), AckResult::Err(_)));
    }
}
