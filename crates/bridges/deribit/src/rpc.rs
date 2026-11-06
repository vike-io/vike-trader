//! Pure JSON-RPC 2.0 helpers. Exact port of `exec/deribit/rpc.py`.

use std::sync::atomic::{AtomicI64, Ordering};

/// Monotonic-id JSON-RPC 2.0 request builder. One instance per logical connection.
pub struct JsonRpcBuilder {
    counter: AtomicI64,
}

impl Default for JsonRpcBuilder {
    fn default() -> Self {
        JsonRpcBuilder { counter: AtomicI64::new(1) }
    }
}

impl JsonRpcBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_id(&self) -> i64 {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }

    pub fn request(&self, method: &str, params: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0", "id": self.next_id(), "method": method, "params": params,
        })
    }
}

/// (id, result, error) from a response frame; all-None for non-dict/keepalive frames.
/// A well-formed JSON-RPC response carries exactly one of result/error.
pub fn parse_response(
    frame: &serde_json::Value,
) -> (Option<i64>, Option<serde_json::Value>, Option<serde_json::Value>) {
    if !frame.is_object() {
        return (None, None, None);
    }
    let rid = frame.get("id").and_then(|i| i.as_i64());
    (rid, frame.get("result").cloned(), frame.get("error").cloned())
}
