//! Live per-venue connection state — the runtime counterpart to `vike_gateway_panel::status`'s
//! static credential-configuration grid. Producers are each bridge's live market feed, which writes
//! a human-readable status string into an `Arc<Mutex<String>>` via its own `set_status` (e.g.
//! `"connecting to Binance…"` / `"LIVE · Binance"` / `"{key} ws error (reconnecting): {e}"` — see
//! `crates/bridges/*/src/market_feed.rs`). The desktop opens no venue socket, so ITS handles come
//! from `vike_app_core::data::market_data_session` instead — one status line per venue the datahub
//! market-data link carries (binance/bybit/okx/aster/hyperliquid/polymarket) — and the Connections
//! tool folds them into the `live` map it passes `vike_gateway_panel::gateway_ui`. A venue with
//! no producer is absent from the map and reports [`DataClientConnectionState::Unknown`] until a producer is
//! added. This module owns only the pure model + the string→state classifier; the wiring lives in
//! vike-app-core. (This named `vike-app` and its own venue feeds as the collector until
//! 2026-09-28; the desktop lost those feeds with its local core.)
//!
//! It sits in `vike-model` — the bottom pure-domain layer — because it has TWO consumers with
//! opposite dependency budgets: the egui Connections tool (`vike-gateway-panel`), and
//! `vike_tradehub::reconcile_config::health_from_feed_status`, which the HEADLESS daemon calls and which
//! therefore may not reach through an egui crate to get here. Zero deps — pure `&str` in, enum out
//! — so the bottom layer is the only home that costs NEITHER consumer anything. (It started life as
//! `vike_gateway_panel::live_status`, then moved to `vike-ops` for the daemon's sake; that was still a
//! compromise home, because `vike-ops` then dragged `vike-core`/`vike-data`/`vike-alerting` behind
//! it and the egui side paid for all of that to reach 142 dependency-free lines.) Nothing
//! re-exports it: `vike-ops`' `vike_ops::feed_status` re-export is deleted, and `vike-gateway-panel`
//! carried a second pair of historical names (`vike_gateway_panel::{parse_feed_status,
//! DataClientConnectionState}`) and no longer does — every consumer names this module. (This said `vike-ops`
//! "re-exports the whole module" until 2026-09-28.)

/// Coarse live connection state for one venue, rendered as the Status-column dot in the
/// Connections tool. `Unknown` is the default — a venue with no live status producer wired in
/// always reads `Unknown`, never guessed as `Connected` or `Disconnected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataClientConnectionState {
    #[default]
    Unknown,
    Disconnected,
    Connecting,
    Connected,
    Error,
}

/// Parse a binance-style human-readable feed-status string (the contents of `App.feed_status`'s
/// `Mutex<String>`) into a coarse [`DataClientConnectionState`]. Case-insensitive substring match, checked
/// in this order (most-specific-terminal-state first, so a mixed message like `"ws error
/// (reconnecting)"` reads as `Error`, not `Connecting`, and `"disconnected"` never
/// false-positives on the `"connected"` substring it happens to contain):
///
/// 1. empty / `"—"` / `"idle"` / contains `"disconnected"` -> [`DataClientConnectionState::Disconnected`]
/// 2. contains `"fault"` / `"error"` / `"failed"` -> [`DataClientConnectionState::Error`]
/// 3. contains `"connected"` / `"live"` / `"streaming"` / `"subscribed"` ->
///    [`DataClientConnectionState::Connected`]
/// 4. contains `"connect"` / `"connecting"` / `"reconnect"` -> [`DataClientConnectionState::Connecting`]
/// 5. anything else -> [`DataClientConnectionState::Unknown`]
///
/// Pure — no I/O; callable straight from a snapshot (e.g. a `Mutex::lock().clone()`) of the
/// status string.
pub fn parse_feed_status(s: &str) -> DataClientConnectionState {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return DataClientConnectionState::Disconnected;
    }
    let lower = trimmed.to_lowercase();
    if lower == "—" || lower == "idle" || lower.contains("disconnected") {
        return DataClientConnectionState::Disconnected;
    }
    if lower.contains("fault") || lower.contains("error") || lower.contains("failed") {
        return DataClientConnectionState::Error;
    }
    if lower.contains("connected")
        || lower.contains("live")
        || lower.contains("streaming")
        || lower.contains("subscribed")
    {
        return DataClientConnectionState::Connected;
    }
    if lower.contains("connect") || lower.contains("connecting") || lower.contains("reconnect") {
        return DataClientConnectionState::Connecting;
    }
    DataClientConnectionState::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unknown() {
        assert_eq!(DataClientConnectionState::default(), DataClientConnectionState::Unknown);
    }

    #[test]
    fn empty_is_disconnected() {
        assert_eq!(parse_feed_status(""), DataClientConnectionState::Disconnected);
        assert_eq!(parse_feed_status("   "), DataClientConnectionState::Disconnected);
    }

    #[test]
    fn dash_idle_and_disconnected_map_to_disconnected() {
        assert_eq!(parse_feed_status("—"), DataClientConnectionState::Disconnected);
        assert_eq!(parse_feed_status("idle"), DataClientConnectionState::Disconnected);
        assert_eq!(parse_feed_status("Idle"), DataClientConnectionState::Disconnected);
        assert_eq!(parse_feed_status("disconnected"), DataClientConnectionState::Disconnected);
        assert_eq!(parse_feed_status("Disconnected"), DataClientConnectionState::Disconnected);
        // must NOT false-positive on the "connected" substring inside "disconnected"
        assert_ne!(parse_feed_status("disconnected"), DataClientConnectionState::Connected);
    }

    #[test]
    fn fault_error_failed_map_to_error() {
        assert_eq!(parse_feed_status("feed fault: reset"), DataClientConnectionState::Error);
        assert_eq!(
            parse_feed_status("btcusdt@kline_1m seed error: timeout"),
            DataClientConnectionState::Error
        );
        assert_eq!(parse_feed_status("subscription failed"), DataClientConnectionState::Error);
        // a mixed error+reconnect message must still read as Error, not Connecting
        assert_eq!(
            parse_feed_status("btcusdt@kline_1m ws error (reconnecting): timeout"),
            DataClientConnectionState::Error
        );
    }

    #[test]
    fn connected_live_streaming_subscribed_map_to_connected() {
        assert_eq!(parse_feed_status("connected"), DataClientConnectionState::Connected);
        assert_eq!(parse_feed_status("LIVE · Binance"), DataClientConnectionState::Connected);
        assert_eq!(parse_feed_status("streaming ticks"), DataClientConnectionState::Connected);
        assert_eq!(
            parse_feed_status("subscribed to btcusdt@kline_1m"),
            DataClientConnectionState::Connected
        );
    }

    #[test]
    fn connect_connecting_reconnect_map_to_connecting() {
        assert_eq!(
            parse_feed_status("connecting to Binance…"),
            DataClientConnectionState::Connecting
        );
        assert_eq!(parse_feed_status("reconnecting"), DataClientConnectionState::Connecting);
        assert_eq!(parse_feed_status("connect"), DataClientConnectionState::Connecting);
    }

    #[test]
    fn anything_else_is_unknown() {
        assert_eq!(
            parse_feed_status("some unrecognized status text"),
            DataClientConnectionState::Unknown
        );
        assert_eq!(parse_feed_status("n/a"), DataClientConnectionState::Unknown);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(parse_feed_status("CONNECTED"), DataClientConnectionState::Connected);
        assert_eq!(
            parse_feed_status("Connecting To Binance"),
            DataClientConnectionState::Connecting
        );
        assert_eq!(parse_feed_status("ERROR"), DataClientConnectionState::Error);
    }
}
