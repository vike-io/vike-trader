//! Venue routing — the two pure resolvers that turn a chart/feed key or a catalog `AssetClass`
//! into the venue name + native instrument id the feed layer actually subscribes with. Moved down
//! verbatim out of `vike-app`'s CI-excluded `main.rs` (`venue_of_key`, `venue_bar_instrument`) so
//! the merge gate compiles, clippies and — above all — TESTS them: neither touches egui. (Each was
//! a per-venue mapping table, the shape that rots silently when a venue is added;
//! `venue_bar_instrument`'s perp allowlist rotted exactly that way and is gone, see its doc.)
//!
//! Two more lived here, `venue_str` and `venue_inst`, the DOM's venue selector mapped to a venue
//! name and a native instrument id. They were deleted with the DOM: a Trade window's address
//! already names its venue and the venue's own symbol (`crate::orders::order_dispatch::TradeAddress`),
//! so there is nothing left to translate.
//!
//! [`venue_of_key`] is the inverse of [`workspace::series_key`](crate::ui::workspace::series_key), and
//! its `:`-split convention is the SAME one
//! [`feed_lifecycle::orphaned_trade_feed_keys`](crate::ui::feed_lifecycle::orphaned_trade_feed_keys)
//! already parses `"…@trades"` keys with (that function's doc cites this one by name).

use crate::ui::workspace::DEFAULT_VENUE;

/// Recover the venue a feed/chart key was subscribed on, the inverse of [`workspace::series_key`]:
/// a non-Binance key is `"venue:SYMBOL@interval"` (the venue is the segment before the first `:`),
/// and a bare `"SYMBOL@interval"` (no `:`) is Binance. Binance symbols are concatenated (`BTCUSDT`)
/// and OKX ids are dashed (`BTC-USDT`) — neither contains a `:`, so the first-`:` split is
/// unambiguous. Used by `reap_orphaned_feeds` to unsubscribe on the right venue feed.
///
/// [`workspace::series_key`]: crate::ui::workspace::series_key
pub fn venue_of_key(key: &str) -> &str {
    match key.split_once(':') {
        Some((venue, _)) => venue,
        None => DEFAULT_VENUE,
    }
}

/// Native bar instrument id for (venue, symbol, asset_class), or `None` for a product the chart
/// layer cannot key a series for (caller skips the subscribe and the series is never wanted).
///
/// Every class passes the symbol through unchanged — on EVERY venue — except `AssetClass::Combo`:
/// a spread or strategy instrument is `None` everywhere. The derivatives (`CryptoPerp`,
/// `CryptoFuture`, `Option`) used to answer `None` outside a five-venue allowlist, and that was
/// not a harmless "no feed for this product": `ensure_feed_on` returns on `None` BEFORE it records
/// the key in `FeedSlots::spawned`, and `spawned` is also the filter
/// [`core_sync::sync_from_core`](crate::ui::core_sync::sync_from_core) folds a series through, so a
/// chart whose class the Symbol picker had set (`asset_class: Some(..)`) showed `NO FEED` while
/// the backend's store held its bars. This desktop opens no venue socket, so which products a
/// venue can serve is the DATAHUB's to say (its refusal arrives as `LiveDataError::Unsupported`
/// and is recorded, never retried — `crates/vike-app-core/src/ui/feed_lifecycle.rs`'s
/// `FeedRetries`), not this table's. Passing the symbol through is what every working path
/// already did: an OKX dashed instId (`BTC-USDT-SWAP`), a Binance/Aster `.P` perp symbol and a
/// Hyperliquid coin are each already product-distinct, and a Deribit option name is the venue's
/// own id.
///
/// The `Combo` row is the one that survives from the old reasoning ("no venue's bar feed here
/// serves a spread or strategy instrument"). `AssetClass::Future` (a non-crypto future —
/// index/commodity) and the listed classes decision 0126 added (`Bond`, `Fund`, `Warrant`,
/// `Commodity`) pass through like the rest, mirroring `Instrument::id()`'s own suffix rule.
///
/// ⚠ `venue` is read by no row any more. It stays in the signature because decision 0019 names
/// this function as the generic-consumer seam for per-venue instrument mapping, and its one
/// caller already passes it; the next venue whose bar id is not its symbol adds the row HERE.
pub fn venue_bar_instrument(
    _venue: &str,
    symbol: &str,
    ac: Option<vike_model::AssetClass>,
) -> Option<String> {
    use vike_model::AssetClass::*;
    // Exhaustive on purpose (no `_` arm): a new class must be classified here, not inherit a row.
    match ac {
        None
        | Some(CryptoSpot)
        | Some(CryptoPerp)
        | Some(CryptoFuture)
        | Some(Option)
        | Some(Equity)
        | Some(Etf)
        | Some(Fx)
        | Some(Cfd)
        | Some(Index)
        | Some(PredictionMarket)
        | Some(Future)
        | Some(Bond)
        | Some(Fund)
        | Some(Warrant)
        | Some(Commodity) => Some(symbol.to_string()),
        Some(Combo) => None,
    }
}

/// Unit tests for the resolvers — they had NONE while they lived in the CI-excluded
/// `main.rs` (nothing there compiles in a gate, so a wrong per-venue row could never fail a
/// build). Same standalone-pure-helper pattern as `feed_lifecycle`'s `orphaned_feed_tests`.
#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::AssetClass;

    /// The inverse-of-`series_key` contract, both directions: a bare key is Binance (the default
    /// venue), a `venue:`-prefixed key names its venue, and `venue_of_key(series_key(v, s, i))`
    /// round-trips for BOTH shapes — the property `reap_orphaned_feeds` relies on to unsubscribe
    /// on the right feed.
    #[test]
    fn venue_of_key_is_the_inverse_of_series_key() {
        use crate::ui::workspace::series_key;
        assert_eq!(venue_of_key("BTCUSDT@1m"), DEFAULT_VENUE);
        assert_eq!(venue_of_key("okx:BTC-USDT@1m"), "okx");
        assert_eq!(venue_of_key("bybit:BTCUSDT@5m"), "bybit");
        for (venue, symbol) in
            [(DEFAULT_VENUE, "BTCUSDT"), ("okx", "BTC-USDT"), ("hyperliquid", "BTC")]
        {
            let k = series_key(venue, symbol, "1m");
            assert_eq!(venue_of_key(&k), venue, "round-trip failed for {k}");
        }
    }

    /// Neither a concatenated Binance symbol nor a dashed OKX instId contains a `:`, so a
    /// dash/`.P` in the SYMBOL never confuses the first-`:` split.
    #[test]
    fn venue_of_key_splits_on_the_first_colon_only() {
        assert_eq!(venue_of_key("BTCUSDT.P@1m"), DEFAULT_VENUE);
        assert_eq!(venue_of_key("okx:BTC-USDT-SWAP@1h"), "okx");
        assert_eq!(venue_of_key(""), DEFAULT_VENUE);
    }

    /// Every non-derivative asset class (and an unknown/absent class) passes the symbol through
    /// unchanged, on ANY venue — including `Future`, which is deliberately bucketed with the
    /// passthrough arm rather than the crypto-derivative one.
    #[test]
    fn non_crypto_derivative_classes_pass_the_symbol_through_on_every_venue() {
        for ac in [
            None,
            Some(AssetClass::CryptoSpot),
            Some(AssetClass::Equity),
            Some(AssetClass::Etf),
            Some(AssetClass::Fx),
            Some(AssetClass::Cfd),
            Some(AssetClass::Index),
            Some(AssetClass::PredictionMarket),
            Some(AssetClass::Future),
            Some(AssetClass::Bond),
            Some(AssetClass::Fund),
            Some(AssetClass::Warrant),
            Some(AssetClass::Commodity),
        ] {
            for venue in ["binance", "okx", "deribit", "polymarket", "some-unwired-venue"] {
                assert_eq!(
                    venue_bar_instrument(venue, "BTCUSDT", ac),
                    Some("BTCUSDT".to_string()),
                    "{venue} / {ac:?} must pass through"
                );
            }
        }
    }

    /// The crypto-derivative arm has NO per-venue allowlist: perp, future and option pass the
    /// symbol through on EVERY venue — the five CEX venues that used to be the allowlist and
    /// deribit, polymarket and an unwired slug alike. The datahub, not this table, says what a
    /// venue can serve; answering `None` here is what left a deribit option chart `NO FEED`
    /// (the key never reached `FeedSlots::spawned`).
    #[test]
    fn crypto_derivative_bars_pass_through_on_every_venue() {
        for ac in [AssetClass::CryptoPerp, AssetClass::CryptoFuture, AssetClass::Option] {
            for venue in [
                "okx",
                "binance",
                "bybit",
                "aster",
                "hyperliquid",
                "deribit",
                "polymarket",
                "ig",
                "",
            ] {
                assert_eq!(
                    venue_bar_instrument(venue, "BTC-PERPETUAL", Some(ac)),
                    Some("BTC-PERPETUAL".to_string()),
                    "{venue} / {ac:?} must pass through"
                );
            }
        }
    }

    /// A combo (spread / strategy) instrument is `None` everywhere — the one class the resolver
    /// still refuses — including on the venues whose other derivatives pass through.
    #[test]
    fn a_combo_has_no_bar_feed_on_any_venue() {
        for venue in ["okx", "binance", "bybit", "aster", "hyperliquid", "deribit", "ibkr", ""] {
            assert_eq!(
                venue_bar_instrument(venue, "BTC-FS-27JUN26_PERP", Some(AssetClass::Combo)),
                None,
                "{venue}: a combo must skip the bar subscribe"
            );
        }
    }
}
