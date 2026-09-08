//! Venue routing — the four pure resolvers that turn a chart/feed key, a DOM venue selector or a
//! catalog `AssetClass` into the venue name + native instrument id the feed layer actually
//! subscribes with. Moved down verbatim out of `vike-app`'s CI-excluded `main.rs`
//! (`venue_of_key`, `venue_bar_instrument`, `App::venue_str`, `App::venue_inst`) so the merge gate
//! compiles, clippies and — above all — TESTS them: none of the four touched egui, and every one
//! is a per-venue mapping table, exactly the shape that rots silently when a venue is added.
//!
//! [`venue_str`]/[`venue_inst`] were associated functions on `App` (`Self::venue_str(..)`) purely
//! because that is where they happened to be written — neither ever read `self`, so both are plain
//! free functions here; every call site keeps its bare name via a `use`.
//!
//! [`venue_of_key`] is the inverse of [`workspace::series_key`](crate::workspace::series_key), and
//! its `:`-split convention is the SAME one
//! [`feed_lifecycle::orphaned_trade_feed_keys`](crate::feed_lifecycle::orphaned_trade_feed_keys)
//! already parses `"…@trades"` keys with (that function's doc cites this one by name).

use crate::workspace::DEFAULT_VENUE;
use vike_panels::dom;

/// Recover the venue a feed/chart key was subscribed on, the inverse of [`workspace::series_key`]:
/// a non-Binance key is `"venue:SYMBOL@interval"` (the venue is the segment before the first `:`),
/// and a bare `"SYMBOL@interval"` (no `:`) is Binance. Binance symbols are concatenated (`BTCUSDT`)
/// and OKX ids are dashed (`BTC-USDT`) — neither contains a `:`, so the first-`:` split is
/// unambiguous. Used by `reap_orphaned_feeds` to unsubscribe on the right venue feed.
///
/// [`workspace::series_key`]: crate::workspace::series_key
pub fn venue_of_key(key: &str) -> &str {
    match key.split_once(':') {
        Some((venue, _)) => venue,
        None => DEFAULT_VENUE,
    }
}

/// Native bar instrument id for (venue, symbol, asset_class), or `None` when the venue has no bar
/// feed for that product yet (caller skips the subscribe rather than charting spot). Slice 1: OKX
/// derivatives pass through (product-agnostic candle WS — `subscribe_bars` already accepts OKX's
/// dashed instIds like `BTC-USDT-SWAP`, proven by the DOM); spot/passthrough everywhere; other
/// venues' non-spot bars are a later PR (binance fstream / bybit linear klines) so they return
/// `None`. `AssetClass::Future` (a non-crypto future — index/commodity) is bucketed with the
/// passthrough arm, mirroring `Instrument::id()`'s own suffix rule (only CryptoPerp/CryptoFuture/
/// Option get a product suffix there).
pub fn venue_bar_instrument(
    venue: &str,
    symbol: &str,
    ac: Option<vike_catalog::AssetClass>,
) -> Option<String> {
    use vike_catalog::AssetClass::*;
    match ac {
        None
        | Some(CryptoSpot)
        | Some(Equity)
        | Some(Etf)
        | Some(Fx)
        | Some(Cfd)
        | Some(Index)
        | Some(PredictionMarket)
        | Some(Future) => Some(symbol.to_string()),
        Some(CryptoPerp) | Some(CryptoFuture) | Some(Option) => match venue {
            // OKX: its dashed instId (`BTC-USDT-SWAP`) is already product-distinct + its candle WS is
            // product-agnostic. Binance/Aster: the catalog tags perps with a distinct `.P` symbol
            // (`BTCUSDT.P`) which the binance/bybit/aster feeds strip + route to the fstream / linear
            // / fapi kline WS. Hyperliquid: the perp coin IS the unified symbol (`"BTC"`) and its
            // candle WS is product-agnostic (like OKX), so it passes straight through. All pass the
            // (already-distinct) symbol through. Venues with no perp bar feed yet (deribit /
            // polymarket) → None → the subscribe is skipped.
            "okx" | "binance" | "bybit" | "aster" | "hyperliquid" => Some(symbol.to_string()),
            _ => None,
        },
    }
}

/// The DOM venue selector → the lower-case venue name every venue-keyed map in the app uses
/// (`feeds`, `BookStore`, `spawned`, `series_key`'s prefix).
pub fn venue_str(venue: dom::DomVenue) -> &'static str {
    match venue {
        dom::DomVenue::Binance => "binance",
        dom::DomVenue::Bybit => "bybit",
        dom::DomVenue::Okx => "okx",
        dom::DomVenue::Aster => "aster",
        dom::DomVenue::Hyperliquid => "hyperliquid",
    }
}

/// Canonical symbol → the venue's own instrument id. Binance/Bybit/Aster use the canonical form
/// (Bybit linear-perp shares the spot symbol string, e.g. `BTCUSDT`; Aster's DOM depth lane is
/// spot-only — `Feeds::subscribe_depth` passes the symbol straight through with no `.P`
/// stripping, matching its Binance-shaped instId) — all fall through to the identity `_` arm.
/// OKX uses the dashed SWAP-perp inst (e.g. `BTCUSDT` → `BTC-USDT-SWAP`) — the same instrument
/// its live `OkxPerpRest` trades, so the displayed book, the per-venue orders/position, the bar
/// feed, and the (live) execution all key on one string. The `BookStore` is keyed by
/// `(venue_str, this)` — `tool_views::dom_tool_content` looks the book up with the same mapping.
pub fn venue_inst(venue: dom::DomVenue, canonical: &str) -> String {
    match venue {
        dom::DomVenue::Okx => canonical
            .strip_suffix("USDT")
            .map(|base| format!("{base}-USDT-SWAP"))
            .unwrap_or_else(|| canonical.to_string()),
        // Hyperliquid perps are bare coins (`BTCUSDT` → `BTC`): the feed's `subscribe_depth`
        // takes the coin directly (no symbology needed for perps) and its `l2Book` snapshot
        // lane serves the DOM. A non-`USDT` canonical falls through unchanged.
        dom::DomVenue::Hyperliquid => canonical
            .strip_suffix("USDT")
            .map(str::to_string)
            .unwrap_or_else(|| canonical.to_string()),
        _ => canonical.to_string(),
    }
}

/// Unit tests for the four resolvers — they had NONE while they lived in the CI-excluded
/// `main.rs` (nothing there compiles in a gate, so a wrong per-venue row could never fail a
/// build). Same standalone-pure-helper pattern as `feed_lifecycle`'s `backfill_gate_tests`.
#[cfg(test)]
mod tests {
    use super::*;
    use vike_catalog::AssetClass;

    /// The inverse-of-`series_key` contract, both directions: a bare key is Binance (the default
    /// venue), a `venue:`-prefixed key names its venue, and `venue_of_key(series_key(v, s, i))`
    /// round-trips for BOTH shapes — the property `reap_orphaned_feeds` relies on to unsubscribe
    /// on the right feed.
    #[test]
    fn venue_of_key_is_the_inverse_of_series_key() {
        use crate::workspace::series_key;
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

    /// The crypto-derivative arm is a per-venue allowlist: the five venues with a wired
    /// (product-agnostic or `.P`-stripping) kline feed pass through; every other venue returns
    /// `None` so the caller SKIPS the subscribe rather than silently charting spot.
    #[test]
    fn crypto_derivative_bars_are_allowlisted_per_venue() {
        for ac in [AssetClass::CryptoPerp, AssetClass::CryptoFuture, AssetClass::Option] {
            for venue in ["okx", "binance", "bybit", "aster", "hyperliquid"] {
                assert_eq!(
                    venue_bar_instrument(venue, "BTCUSDT.P", Some(ac)),
                    Some("BTCUSDT.P".to_string()),
                    "{venue} / {ac:?} must pass through"
                );
            }
            for venue in ["deribit", "polymarket", "ig", "oanda", ""] {
                assert_eq!(
                    venue_bar_instrument(venue, "BTC-PERPETUAL", Some(ac)),
                    None,
                    "{venue} / {ac:?} has no perp bar feed — must be None"
                );
            }
        }
    }

    /// The DOM selector → venue-name table, pinned verbatim: these strings key `feeds`, the
    /// `BookStore` and `series_key`'s prefix, so a typo silently orphans a whole venue's book.
    #[test]
    fn venue_str_pins_the_dom_venue_names() {
        assert_eq!(venue_str(dom::DomVenue::Binance), "binance");
        assert_eq!(venue_str(dom::DomVenue::Bybit), "bybit");
        assert_eq!(venue_str(dom::DomVenue::Okx), "okx");
        assert_eq!(venue_str(dom::DomVenue::Aster), "aster");
        assert_eq!(venue_str(dom::DomVenue::Hyperliquid), "hyperliquid");
    }

    /// Every `venue_str` output is lower-case and `:`-free — the two properties `series_key`'s
    /// `"{venue}:{symbol}@{interval}"` shape and [`venue_of_key`]'s first-`:` split both assume.
    #[test]
    fn venue_str_names_are_lowercase_and_colon_free() {
        for v in [
            dom::DomVenue::Binance,
            dom::DomVenue::Bybit,
            dom::DomVenue::Okx,
            dom::DomVenue::Aster,
            dom::DomVenue::Hyperliquid,
        ] {
            let s = venue_str(v);
            assert_eq!(s, s.to_lowercase(), "{s} must be lower-case");
            assert!(!s.contains(':'), "{s} must not contain a ':'");
        }
    }

    /// OKX rewrites `BTCUSDT` → the dashed SWAP inst; Hyperliquid strips to the bare coin;
    /// Binance/Bybit/Aster are the identity arm. Same-string-everywhere is load-bearing: the book,
    /// the per-venue orders/position and the (live) execution all key on this one output.
    #[test]
    fn venue_inst_maps_the_canonical_symbol_per_venue() {
        assert_eq!(venue_inst(dom::DomVenue::Okx, "BTCUSDT"), "BTC-USDT-SWAP");
        assert_eq!(venue_inst(dom::DomVenue::Hyperliquid, "BTCUSDT"), "BTC");
        for v in [dom::DomVenue::Binance, dom::DomVenue::Bybit, dom::DomVenue::Aster] {
            assert_eq!(venue_inst(v, "BTCUSDT"), "BTCUSDT");
        }
    }

    /// A canonical symbol that does NOT end in `USDT` falls through unchanged on the two
    /// rewriting venues too (the `unwrap_or_else` arm), rather than producing a mangled inst.
    #[test]
    fn venue_inst_leaves_a_non_usdt_canonical_alone() {
        assert_eq!(venue_inst(dom::DomVenue::Okx, "BTC-USD-SWAP"), "BTC-USD-SWAP");
        assert_eq!(venue_inst(dom::DomVenue::Hyperliquid, "BTC"), "BTC");
        assert_eq!(venue_inst(dom::DomVenue::Okx, ""), "");
    }
}
