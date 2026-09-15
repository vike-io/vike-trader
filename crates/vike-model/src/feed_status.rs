//! Live per-venue connection state — the runtime counterpart to `vike_connections::status`'s
//! static credential-configuration grid. Producers are each bridge's live market feed, which writes
//! a human-readable status string into an `Arc<Mutex<String>>` via its own `set_status` (e.g.
//! `"connecting to Binance…"` / `"LIVE · Binance"` / `"{key} ws error (reconnecting): {e}"` — see
//! `crates/bridges/*/src/market_feed.rs`). vike-app collects those handles into the `live` map it
//! passes `vike_connections::connections_ui`; today that covers every streaming feed
//! (binance/bybit/okx/aster/hyperliquid/polymarket). A venue with no live feed producer yet
//! (deribit, whose state would come from its exec/user-data WS; the FX/broker venues, which aren't
//! mounted as live app feeds) is absent from the map and reports [`ConnectionState::Unknown`]
//! until a producer is added. This module owns only the pure model + the string→state classifier;
//! the wiring lives in vike-app.
//!
//! It sits in `vike-model` — the bottom pure-domain layer — because it has TWO consumers with
//! opposite dependency budgets: the egui Connections tool (`vike-connections`), and
//! `vike_ops::reconcile_config::health_from_feed_status`, which the HEADLESS daemon calls and which
//! therefore may not reach through an egui crate to get here. Zero deps — pure `&str` in, enum out
//! — so the bottom layer is the only home that costs NEITHER consumer anything. (It started life as
//! `vike_connections::live_status`, then moved to `vike-ops` for the daemon's sake; that was still a
//! compromise home, because `vike-ops` drags `vike-core`/`vike-data`/`vike-alerting` behind it and
//! the egui side paid for all of that to reach 142 dependency-free lines.) Both `vike-ops` (as
//! `vike_ops::feed_status`) and `vike-connections` (as `vike_connections::{parse_feed_status,
//! ConnectionState}`) re-export it under their historical paths, so every existing call site is
//! unchanged.

/// Coarse live connection state for one venue, rendered as the Status-column dot in the
/// Connections tool. `Unknown` is the default — a venue with no live status producer wired in
/// always reads `Unknown`, never guessed as `Connected` or `Disconnected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Unknown,
    Disconnected,
    Connecting,
    Connected,
    Error,
}

/// Parse a binance-style human-readable feed-status string (the contents of `App.feed_status`'s
/// `Mutex<String>`) into a coarse [`ConnectionState`]. Case-insensitive substring match, checked
/// in this order (most-specific-terminal-state first, so a mixed message like `"ws error
/// (reconnecting)"` reads as `Error`, not `Connecting`, and `"disconnected"` never
/// false-positives on the `"connected"` substring it happens to contain):
///
/// 1. empty / `"—"` / `"idle"` / contains `"disconnected"` -> [`ConnectionState::Disconnected`]
/// 2. contains `"fault"` / `"error"` / `"failed"` -> [`ConnectionState::Error`]
/// 3. contains `"connected"` / `"live"` / `"streaming"` / `"subscribed"` ->
///    [`ConnectionState::Connected`]
/// 4. contains `"connect"` / `"connecting"` / `"reconnect"` -> [`ConnectionState::Connecting`]
/// 5. anything else -> [`ConnectionState::Unknown`]
///
/// Pure — no I/O; callable straight from a snapshot (e.g. a `Mutex::lock().clone()`) of the
/// status string.
pub fn parse_feed_status(s: &str) -> ConnectionState {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return ConnectionState::Disconnected;
    }
    let lower = trimmed.to_lowercase();
    if lower == "—" || lower == "idle" || lower.contains("disconnected") {
        return ConnectionState::Disconnected;
    }
    if lower.contains("fault") || lower.contains("error") || lower.contains("failed") {
        return ConnectionState::Error;
    }
    if lower.contains("connected")
        || lower.contains("live")
        || lower.contains("streaming")
        || lower.contains("subscribed")
    {
        return ConnectionState::Connected;
    }
    if lower.contains("connect") || lower.contains("connecting") || lower.contains("reconnect") {
        return ConnectionState::Connecting;
    }
    ConnectionState::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unknown() {
        assert_eq!(ConnectionState::default(), ConnectionState::Unknown);
    }

    #[test]
    fn empty_is_disconnected() {
        assert_eq!(parse_feed_status(""), ConnectionState::Disconnected);
        assert_eq!(parse_feed_status("   "), ConnectionState::Disconnected);
    }

    #[test]
    fn dash_idle_and_disconnected_map_to_disconnected() {
        assert_eq!(parse_feed_status("—"), ConnectionState::Disconnected);
        assert_eq!(parse_feed_status("idle"), ConnectionState::Disconnected);
        assert_eq!(parse_feed_status("Idle"), ConnectionState::Disconnected);
        assert_eq!(parse_feed_status("disconnected"), ConnectionState::Disconnected);
        assert_eq!(parse_feed_status("Disconnected"), ConnectionState::Disconnected);
        // must NOT false-positive on the "connected" substring inside "disconnected"
        assert_ne!(parse_feed_status("disconnected"), ConnectionState::Connected);
    }

    #[test]
    fn fault_error_failed_map_to_error() {
        assert_eq!(parse_feed_status("feed fault: reset"), ConnectionState::Error);
        assert_eq!(
            parse_feed_status("btcusdt@kline_1m seed error: timeout"),
            ConnectionState::Error
        );
        assert_eq!(parse_feed_status("subscription failed"), ConnectionState::Error);
        // a mixed error+reconnect message must still read as Error, not Connecting
        assert_eq!(
            parse_feed_status("btcusdt@kline_1m ws error (reconnecting): timeout"),
            ConnectionState::Error
        );
    }

    #[test]
    fn connected_live_streaming_subscribed_map_to_connected() {
        assert_eq!(parse_feed_status("connected"), ConnectionState::Connected);
        assert_eq!(parse_feed_status("LIVE · Binance"), ConnectionState::Connected);
        assert_eq!(parse_feed_status("streaming ticks"), ConnectionState::Connected);
        assert_eq!(parse_feed_status("subscribed to btcusdt@kline_1m"), ConnectionState::Connected);
    }

    #[test]
    fn connect_connecting_reconnect_map_to_connecting() {
        assert_eq!(parse_feed_status("connecting to Binance…"), ConnectionState::Connecting);
        assert_eq!(parse_feed_status("reconnecting"), ConnectionState::Connecting);
        assert_eq!(parse_feed_status("connect"), ConnectionState::Connecting);
    }

    #[test]
    fn anything_else_is_unknown() {
        assert_eq!(parse_feed_status("some unrecognized status text"), ConnectionState::Unknown);
        assert_eq!(parse_feed_status("n/a"), ConnectionState::Unknown);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(parse_feed_status("CONNECTED"), ConnectionState::Connected);
        assert_eq!(parse_feed_status("Connecting To Binance"), ConnectionState::Connecting);
        assert_eq!(parse_feed_status("ERROR"), ConnectionState::Error);
    }
}
