//! The market-channel frames the scripted feed and shard suites hand the socket, built one way.

/// The real wire shape (brief, live capture 2026-07-08): ONE `price_change` frame whose PLURAL
/// `price_changes` array carries entries for possibly SEVERAL assets, each entry — not the frame —
/// with its own `asset_id`, exactly the frame a batched socket receives. `entries` is
/// `(asset_id, price, size, side)`; one asset_id for every entry builds a single-asset frame.
pub fn price_change_frame(entries: &[(&str, &str, &str, &str)]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = entries
        .iter()
        .map(|(asset_id, p, s, side)| {
            serde_json::json!({"asset_id": asset_id, "price": p, "size": s, "side": side})
        })
        .collect();
    serde_json::json!({"event_type": "price_change", "price_changes": arr})
}
