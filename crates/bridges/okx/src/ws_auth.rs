//! OKX V5 private-WS login auth. Exact port of `exec/okx/ws_auth.py`:
//! base64(HMAC-SHA256(secret, `{ts_seconds}GET/users/self/verify`)) — ts in epoch
//! SECONDS (not ms, not ISO); keepalive is the raw text frame "ping" (server: "pong").

use vike_bridge_core::signer::hmac_sha256_base64;
pub use vike_bridge_core::ws::AckResult;

pub const PING_TEXT: &str = "ping";

pub fn okx_ws_sign(api_secret: &str, ts_seconds: &str) -> String {
    hmac_sha256_base64(
        api_secret.as_bytes(),
        format!("{ts_seconds}GET/users/self/verify").as_bytes(),
    )
}

/// `{"op":"login","args":[{apiKey,passphrase,timestamp,sign}]}` — NEVER log it.
pub fn build_login_frame(
    api_key: &str,
    api_secret: &str,
    passphrase: &str,
    now_s: i64,
) -> serde_json::Value {
    let ts = now_s.to_string();
    serde_json::json!({
        "op": "login",
        "args": [{
            "apiKey": api_key,
            "passphrase": passphrase,
            "timestamp": ts,
            "sign": okx_ws_sign(api_secret, &ts),
        }],
    })
}

pub fn build_subscribe_frame(inst_type: &str) -> serde_json::Value {
    serde_json::json!({
        "op": "subscribe",
        "args": [{"channel": "orders", "instType": inst_type}],
    })
}

/// OKX codes that are TRANSIENT (reconnect), not genuine auth failures — the SHARED list (see
/// [`vike_bridge_core::transport::ws_ack_is_transient`]), which carries OKX's own 50011 (rate
/// limit) and 50102 (timestamp expired / clock skew) verbatim.
///
/// OKX is the one venue whose ack codes arrive as wire STRINGS, so this parses first: an
/// unparseable code is NOT transient (`false`), matching the old literal `matches!` — a code we
/// can't even read must never license an unbounded reconnect loop on the order stream.
fn okx_ack_is_transient(code: &str) -> bool {
    code.parse::<i64>().is_ok_and(vike_bridge_core::transport::ws_ack_is_transient)
}

pub fn match_event_ack(frame: &serde_json::Value, event: &str) -> AckResult {
    let Some(obj) = frame.as_object() else {
        return AckResult::NotOurs;
    };
    let ev = obj.get("event").and_then(|e| e.as_str()).unwrap_or("");
    let code = match obj.get("code") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => "0".to_string(),
    };
    if ev == event && code == "0" {
        return AckResult::Ok;
    }
    // Python: `(event=='error' or code!='0') and event in (target, 'error')` — clippy's
    // minimal form is equivalent: an 'error' event always raises; a bad code raises only
    // on the target's own ack.
    if ev == "error" || (code != "0" && ev == event) {
        let msg = obj.get("msg").and_then(|m| m.as_str()).unwrap_or("");
        let text = format!("OKX WS {event} failed: {msg}");
        return if okx_ack_is_transient(&code) {
            AckResult::TransientErr(text)
        } else {
            AckResult::Err(text)
        };
    }
    AckResult::NotOurs
}

#[cfg(test)]
mod ack_class_tests {
    use super::*;
    #[test]
    fn classifies_transient_vs_auth() {
        let rate = serde_json::json!({"event":"error","code":"50011","msg":"rate"});
        let skew = serde_json::json!({"event":"error","code":"50102","msg":"expired"});
        let bad = serde_json::json!({"event":"error","code":"60009","msg":"login failed"});
        assert!(matches!(match_event_ack(&rate, "login"), AckResult::TransientErr(_)));
        assert!(matches!(match_event_ack(&skew, "login"), AckResult::TransientErr(_)));
        assert!(matches!(match_event_ack(&bad, "login"), AckResult::Err(_)));
    }
}
