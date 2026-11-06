//! The venue-namespaced series key and title, and the `asset_class` runtime default.

use super::*;

#[cfg(test)]
mod venue_key_tests {
    use super::*;

    #[test]
    fn binance_key_and_title_are_byte_identical_to_the_historical_single_venue_form() {
        let w = chart_window("BTCUSDT", "1m"); // fresh window defaults to Binance
        assert_eq!(w.venue, DEFAULT_VENUE);
        assert_eq!(w.key(), "BTCUSDT@1m", "Binance key must stay the historical bare form");
        assert_eq!(w.title, "BTCUSDT · 1m", "Binance title unchanged");
    }

    #[test]
    fn non_binance_key_is_namespaced_and_title_is_venue_prefixed() {
        let mut w = chart_window("BTCUSDT", "1m");
        w.venue = "bybit".into();
        w.retitle();
        assert_eq!(w.key(), "bybit:BTCUSDT@1m", "non-Binance keys are venue-namespaced");
        assert_eq!(w.title, "BYBIT BTCUSDT · 1m");

        // A shared symbol on two venues yields DISTINCT keys — no `charts`/`subs` collision.
        assert_ne!(w.key(), chart_window("BTCUSDT", "1m").key());
    }

    #[test]
    fn series_key_matches_win_key_for_both_venues() {
        assert_eq!(series_key("binance", "BTCUSDT", "1m"), "BTCUSDT@1m");
        assert_eq!(series_key("okx", "BTC-USDT", "1m"), "okx:BTC-USDT@1m");
    }
}

/// `WinState.asset_class` (feed-routing slice 1). `WinState` itself is never the serde type —
/// window persistence goes through `super::persist::WinSnap` (see that module's `asset_class`
/// field + its own backward-compat test), so the "old JSON still loads" guarantee is proven
/// there. What's proven here is the runtime default this field must have so every existing
/// window-construction path (`new`/`tool`, quick-picks, plain-symbol paths) is unaffected: a
/// fresh `WinState` is `None` (spot/legacy), and `Option<vike_model::AssetClass>` itself
/// round-trips through JSON losslessly (the property `WinSnap::asset_class` relies on).
#[cfg(test)]
mod asset_class_tests {
    use super::*;
    use vike_model::AssetClass;

    #[test]
    fn fresh_window_defaults_to_none() {
        let w = chart_window("BTCUSDT", "1m");
        assert_eq!(w.asset_class, None);
    }

    #[test]
    fn option_asset_class_json_round_trip_preserves_some_and_defaults_none() {
        // Mirrors the exact serde shape `WinSnap::asset_class` uses
        // (`#[serde(default, skip_serializing_if = "Option::is_none")]`): a JSON object with
        // the key absent (old-format file) must deserialize to `None`, and `Some(..)` must
        // round-trip losslessly.
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            asset_class: Option<AssetClass>,
        }

        // Old JSON with the key entirely absent still deserializes, defaulting to None.
        let old: Wrapper = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(old.asset_class, None);

        // A `Some(CryptoPerp)` value survives a full serde round-trip.
        let w = Wrapper { asset_class: Some(AssetClass::CryptoPerp) };
        let json = serde_json::to_string(&w).unwrap();
        let back: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
    }
}
