//! Deribit public/auth + subscribe JSON-RPC frame builders. Exact port of
//! `exec/deribit/ws_auth.py`. NEVER log the auth frames (client_id + client_secret /
//! refresh_token ride in plaintext params).

pub fn build_client_credentials_auth(
    client_id: &str,
    client_secret: &str,
    scope: Option<&str>,
    rpc_id: i64,
) -> serde_json::Value {
    let mut params = serde_json::json!({
        "grant_type": "client_credentials",
        "client_id": client_id,
        "client_secret": client_secret,
    });
    if let Some(s) = scope {
        params["scope"] = serde_json::json!(s);
    }
    serde_json::json!({"jsonrpc": "2.0", "id": rpc_id, "method": "public/auth", "params": params})
}

pub fn build_refresh_token_auth(refresh_token: &str, rpc_id: i64) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": rpc_id, "method": "public/auth",
        "params": {"grant_type": "refresh_token", "refresh_token": refresh_token},
    })
}

/// `public/test` — Deribit's no-auth liveness probe. Safe to log (carries nothing).
///
/// This is the venue's app-level ping: the reply is an ordinary inbound JSON-RPC frame, which is
/// what gives the user-data lane's silent-stall watchdog a cadence to key off. Without it, the only
/// inbound traffic on an idle account is the 600s token-refresh reply — far too sparse to detect a
/// dead socket in useful time.
///
/// Deliberately NOT `public/set_heartbeat`: that mechanism obliges the client to ANSWER
/// server-initiated `test_request` notifications, which would need a frame-driven reply seam the
/// shared pump does not have (its `decode` is pure and has no socket handle; its keepalive hook is
/// time-driven, not frame-driven). Driving `public/test` from the keepalive hook we already own
/// buys the same liveness with NO change to the shared pump.
pub fn build_public_test(rpc_id: i64) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": rpc_id, "method": "public/test", "params": {}})
}

/// Safe to log (channel names only). The subscription binds to the AUTHED socket.
pub fn build_private_subscribe(channels: &[String], rpc_id: i64) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": rpc_id, "method": "private/subscribe",
        "params": {"channels": channels},
    })
}
