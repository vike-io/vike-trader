//! Binance WS-API `userDataStream.subscribe.signature` helper. Exact port of
//! `exec/binance/ws_auth.py`.
//!
//! Signs HMAC-SHA256 hex over the SORTED (alphabetical) params joined as `k=v&...`
//! (EXCLUDING the signature itself). The signature rides in the JSON params dict, not a
//! query string or header. HARD: the secret/signature never leak into any log line.

use vike_bridge_core::signer::hmac_sha256_hex;
use vike_bridge_core::transport::ws_ack_is_transient;
pub use vike_bridge_core::ws::AckResult;

/// Hex HMAC-SHA256 over sorted params (params must exclude "signature").
pub fn binance_ws_sign(api_secret: &str, params: &[(&str, String)]) -> String {
    let mut sorted: Vec<&(&str, String)> = params.iter().collect();
    sorted.sort_by_key(|(k, _)| *k);
    let payload = sorted.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    hmac_sha256_hex(api_secret.as_bytes(), payload.as_bytes())
}

/// Build the signed subscribe request JSON. WARNING: carries apiKey + signature — NEVER
/// log it. `req_id` is injectable for determinism (live callers pass a fresh unique id).
pub fn build_subscribe_request(
    api_key: &str,
    api_secret: &str,
    now_ms: i64,
    recv_window: i64,
    req_id: &str,
) -> serde_json::Value {
    let params_to_sign: Vec<(&str, String)> = vec![
        ("apiKey", api_key.to_string()),
        ("recvWindow", recv_window.to_string()),
        ("timestamp", now_ms.to_string()),
    ];
    let signature = binance_ws_sign(api_secret, &params_to_sign);
    serde_json::json!({
        "id": req_id,
        "method": "userDataStream.subscribe.signature",
        "params": {
            "apiKey": api_key,
            "recvWindow": recv_window,
            "timestamp": now_ms,
            "signature": signature,
        },
    })
}

pub fn match_subscribe_ack(frame: &serde_json::Value, req_id: &str) -> AckResult {
    let Some(obj) = frame.as_object() else {
        return AckResult::NotOurs;
    };
    if obj.get("id").and_then(|i| i.as_str()) != Some(req_id) {
        return AckResult::NotOurs;
    }
    let status_ok = obj.get("status").and_then(|s| s.as_i64()) == Some(200);
    if !status_ok || obj.contains_key("error") {
        let err = obj.get("error").and_then(|e| e.as_object());
        let msg = err.and_then(|e| e.get("msg")).and_then(|m| m.as_str()).unwrap_or("");
        let code = err.and_then(|e| e.get("code")).and_then(|c| c.as_i64()).unwrap_or(0);
        let text = format!("Binance WS subscribe failed: {msg}");
        // Transient (reconnect) vs genuine auth failure (surface + stop): the SHARED list — see
        // `vike_bridge_core::transport::ws_ack_is_transient`, which carries Binance's own -1021
        // (recvWindow skew) / -1003 (too many requests) verbatim. An absent code (`unwrap_or(0)`
        // above) is NOT transient, so a malformed rejection still surfaces.
        return if ws_ack_is_transient(code) {
            AckResult::TransientErr(text)
        } else {
            AckResult::Err(text)
        };
    }
    AckResult::Ok
}
