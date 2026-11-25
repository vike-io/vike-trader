//! The chart key, in ONE file: the builder [`series_key`], its one parse `split_series_key` (and
//! `venue_of_key` over it), and `venue_bar_instrument`, the pure resolver that turns a catalog
//! `AssetClass` into the native instrument id the key is built from. Every map the chart and feed
//! layer keep (`charts`, `subscriptions`, `spawned`, the aggregators, the store-ask ledger) is keyed by
//! [`series_key`], so a key built or split anywhere else is a key that can disagree with it.
//!
//! **Every venue's key has ONE shape, `venue:SYMBOL@interval`.** Binance used to be the exception,
//! a bare `SYMBOL@interval`, and a bare key parsed back as Binance; a hand-built
//! `format!("{symbol}@{interval}")` therefore looked right on a Binance chart and silently named the
//! wrong series anywhere else. No key without a venue is built any more, and a string without one
//! parses to `None`: its caller has no venue to guess. The key lives in memory only (every holder
//! is rebuilt each launch; the saved files and the wires carry the venue, symbol and interval as
//! separate fields), so the shape changed with no file or wire changing.
//!
//! The two resolvers moved down verbatim out of `vike-app`'s CI-excluded `main.rs` so the merge
//! gate compiles, clippies and — above all — TESTS them: neither touches egui. (Each was a
//! per-venue mapping table, the shape that rots silently when a venue is added;
//! `venue_bar_instrument`'s perp allowlist rotted exactly that way and is gone, see its doc.)
//!
//! Two more lived beside them, `venue_str` and `venue_inst`, the DOM's venue selector mapped to a
//! venue name and a native instrument id. They were deleted with the DOM: a Trade window's address
//! already names its venue and the venue's own symbol (`vike_order_entry::trade_window::address::TradeWindowAddress`),
//! so there is nothing left to translate.

/// The interval a raw trade tape is keyed under: a tape's key is
/// `series_key(venue, symbol, TRADE_TAPE_INTERVAL)`, e.g. `"binance:BTCUSDT@trades"`.
pub(crate) const TRADE_TAPE_INTERVAL: &str = "trades";

/// Feed/chart key builder shared by `WindowState::key` and the app's feed-routing sites so the two
/// can never drift: `"venue:SYMBOL@interval"` for EVERY venue (venue lower-cased, matching the
/// `feeds` map keys), so identical symbols on different venues occupy distinct
/// `charts`/`subscriptions`/`spawned` slots.
pub fn series_key(venue: &str, symbol: &str, interval: &str) -> String {
    format!("{venue}:{symbol}@{interval}")
}

/// The `(venue, symbol, interval)` a [`series_key`] was built from: the venue before the FIRST
/// `:`, the interval after the LAST `@`, the symbol between. Venues are lower-case words and
/// symbols are concatenated (`BTCUSDT`), dashed (`BTC-USDT`) or dotted (`BTCUSDT.P`), so neither
/// split is ambiguous, and a symbol holding an `@` still splits right. `None` for a string with no
/// `:` or no `@`, which [`series_key`] never builds.
pub(crate) fn split_series_key(key: &str) -> Option<(&str, &str, &str)> {
    let (venue, rest) = key.split_once(':')?;
    let (symbol, interval) = rest.rsplit_once('@')?;
    Some((venue, symbol, interval))
}

/// Recover the venue a feed/chart key was subscribed on, the inverse of [`series_key`]; `None`
/// for a string that names no venue (see [`split_series_key`]). Used to route an unsubscribe and
/// a retry to the venue feed the key was subscribed on.
pub(crate) fn venue_of_key(key: &str) -> Option<&str> {
    split_series_key(key).map(|(venue, ..)| venue)
}

// Native bar instrument id for (venue, symbol, asset_class), or `None` for a product the chart
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
/// venue can serve is the DATAHUB's to say (its refusal arrives as `DataClientError::Unsupported`
/// and is recorded, never retried — `crates/vike-app-core/src/data/datahub/feed_retry.rs`'s
/// `DatahubFeedRetries`), not this table's. Passing the symbol through is what every working path
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
pub(crate) fn venue_bar_instrument(
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

    /// ONE shape for every venue, Binance included: a binance key and a bybit key are built the
    /// same way and both round-trip through [`split_series_key`] and [`venue_of_key`] — the
    /// property `reap_orphaned_feeds` and `stop_series` rely on to unsubscribe on the right feed.
    /// The same symbol on two venues is two keys.
    #[test]
    fn a_binance_key_and_a_bybit_key_round_trip() {
        assert_eq!(series_key("binance", "BTCUSDT", "1m"), "binance:BTCUSDT@1m");
        assert_eq!(series_key("bybit", "BTCUSDT", "5m"), "bybit:BTCUSDT@5m");
        assert_ne!(series_key("binance", "BTCUSDT", "1m"), series_key("bybit", "BTCUSDT", "1m"));
        for (venue, symbol, interval) in [
            ("binance", "BTCUSDT", "1m"),
            ("bybit", "BTCUSDT", "5m"),
            ("okx", "BTC-USDT-SWAP", "1h"),
            ("binance", "BTCUSDT.P", "100t"),
            ("hyperliquid", "BTC", TRADE_TAPE_INTERVAL),
        ] {
            let key = series_key(venue, symbol, interval);
            assert_eq!(split_series_key(&key), Some((venue, symbol, interval)), "{key}");
            assert_eq!(venue_of_key(&key), Some(venue), "{key}");
        }
    }

    /// A string with no venue (the retired bare Binance shape) or no interval names no series:
    /// it parses to `None`, never to a guessed venue. The venue is before the FIRST `:` and the
    /// interval after the LAST `@`, so an `@` inside a symbol does not move the split.
    #[test]
    fn a_key_without_a_venue_parses_to_none() {
        assert_eq!(venue_of_key("BTCUSDT@1m"), None, "the bare shape names no venue");
        assert_eq!(split_series_key("BTCUSDT@trades"), None);
        assert_eq!(split_series_key("binance:BTCUSDT"), None, "no interval");
        assert_eq!(venue_of_key(""), None);
        assert_eq!(split_series_key("polymarket:a@b@1m"), Some(("polymarket", "a@b", "1m")));
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
