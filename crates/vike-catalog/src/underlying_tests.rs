//! Tests for the underlying grouping: kinds, the dollar-quote fold, the pickers and venue counts.

use super::*;
use crate::test_support::test_row;
use vike_model::AssetClass::{CryptoFuture, CryptoPerp, CryptoSpot, Equity, Option as Opt};

#[test]
fn the_offered_kinds_are_spot_perp_forex_cfd_and_stocks_and_nothing_else() {
    assert_eq!(kind_of(CryptoSpot), Some(SymbolPickerKind::Spot));
    assert_eq!(kind_of(CryptoPerp), Some(SymbolPickerKind::Perp));
    assert_eq!(kind_of(AssetClass::Fx), Some(SymbolPickerKind::Forex));
    assert_eq!(kind_of(AssetClass::Cfd), Some(SymbolPickerKind::Cfd));
    assert_eq!(kind_of(Equity), Some(SymbolPickerKind::Stock));
    assert_eq!(kind_of(AssetClass::Etf), Some(SymbolPickerKind::Stock));
    for c in [
        CryptoFuture,
        Opt,
        AssetClass::Future,
        AssetClass::Index,
        AssetClass::PredictionMarket,
        AssetClass::Bond,
        AssetClass::Fund,
        AssetClass::Warrant,
        AssetClass::Commodity,
        AssetClass::Combo,
    ] {
        assert_eq!(
            kind_of(c),
            None,
            "{c:?} must not be offered (options and futures have their own windows, polymarket its cockpit)"
        );
    }
}

/// A forex pair keeps its quote (EUR/USD is not EUR/USDT), a stock is its ticker alone, and the
/// dollar fold stays a crypto thing.
#[test]
fn forex_and_stocks_group_on_their_own_terms() {
    let all = [
        test_row("dukascopy", "EURUSD", "EUR", "USD", AssetClass::Fx),
        test_row("oanda", "EUR_USD", "EUR", "USD", AssetClass::Fx),
        test_row("oanda", "EUR_GBP", "EUR", "GBP", AssetClass::Fx),
        test_row("alpaca", "AAPL", "AAPL", "USD", Equity),
        test_row("ibkr", "AAPL", "AAPL", "USD", Equity),
        test_row("ig", "US500", "US500", "USD", AssetClass::Cfd),
    ];
    let refs: Vec<&Instrument> = all.iter().collect();
    let g = group_by_underlying(&refs);
    let heads: Vec<(String, SymbolPickerKind, usize)> =
        g.iter().map(|u| (u.heading(), u.kind, u.listings.len())).collect();
    assert_eq!(
        heads,
        [
            ("EUR/USD".to_string(), SymbolPickerKind::Forex, 2),
            ("EUR/GBP".to_string(), SymbolPickerKind::Forex, 1),
            ("AAPL".to_string(), SymbolPickerKind::Stock, 2),
            ("US500/USD".to_string(), SymbolPickerKind::Cfd, 1),
        ]
    );
}

#[test]
fn dollar_quotes_fold_into_one_line_and_other_quotes_do_not() {
    let all = [
        test_row("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("deribit", "BTC-PERPETUAL", "BTC", "USD", CryptoPerp),
        test_row("okx", "BTC-USDT-SWAP", "BTC", "USDT", CryptoPerp),
        test_row("binance", "ETHBTC", "ETH", "BTC", CryptoSpot),
        test_row("binance", "ETHUSDT", "ETH", "USDT", CryptoSpot),
    ];
    let refs: Vec<&Instrument> = all.iter().collect();
    let g = group_by_underlying(&refs);
    assert_eq!(g.len(), 3);
    assert_eq!(
        (g[0].heading(), g[0].kind, g[0].listings.len()),
        ("BTC".into(), SymbolPickerKind::Perp, 3)
    );
    assert_eq!(g[1].heading(), "ETH/BTC");
    assert_eq!(g[2].heading(), "ETH");
}

/// The dollar quotes are every model dollar label plus the crypto-only stablecoins, the six as before.
#[test]
fn the_dollar_quotes_are_the_model_labels_and_the_crypto_stablecoins() {
    assert!(vike_model::DOLLAR_LABELS.iter().all(|label| DOLLAR_QUOTES.contains(label)));
    assert_eq!(DOLLAR_QUOTES, ["USD", "USDT", "USDC", "BUSD", "FDUSD", "TUSD"]);
}

#[test]
fn a_venue_with_two_listings_keeps_the_best_ranked_and_counts_the_rest() {
    let cat = Catalog::from_instruments(vec![
        test_row("binance", "BTCUSDC.P", "BTC", "USDC", CryptoPerp),
        test_row("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
    ]);
    let q = SymbolPickerQuery { text: "btc", kind: None, venues: &[] };
    let g = picker_results(&cat, &q, 10);
    assert_eq!(g.len(), 1);
    // USDT outranks USDC on a tie, whatever the catalog order.
    assert_eq!(g[0].listings[0].instrument.raw_symbol, "BTCUSDT.P");
    assert_eq!(g[0].listings[0].others, 1);
    // The exact symbol wins outright, and nothing else matches it.
    let q = SymbolPickerQuery { text: "btcusdc.p", kind: None, venues: &[] };
    let g = picker_results(&cat, &q, 10);
    assert_eq!(g[0].listings[0].instrument.raw_symbol, "BTCUSDC.P");
    assert_eq!(g[0].listings[0].others, 0);
}

#[test]
fn options_dated_futures_and_equities_are_never_offered() {
    let cat = Catalog::from_instruments(vec![
        test_row("deribit", "BTC-27NOV26-58000-C", "BTC", "BTC", Opt),
        test_row("deribit", "BTC-27NOV26", "BTC", "USD", CryptoFuture),
        test_row("ibkr", "ESZ6", "ES", "USD", AssetClass::Future),
        test_row("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
    ]);
    let q = SymbolPickerQuery { text: "", kind: None, venues: &[] };
    let g = picker_results(&cat, &q, 10);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].listings[0].instrument.venue, "bybit");
}

#[test]
fn the_kind_switch_and_the_venue_filter_narrow_the_results() {
    let cat = Catalog::from_instruments(vec![
        test_row("binance", "BTCUSDT", "BTC", "USDT", CryptoSpot),
        test_row("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("okx", "BTC-USDT", "BTC", "USDT", CryptoSpot),
    ]);
    let only_perp =
        SymbolPickerQuery { text: "btc", kind: Some(SymbolPickerKind::Perp), venues: &[] };
    assert_eq!(picker_results(&cat, &only_perp, 10).len(), 1);
    let okx = ["okx".to_string()];
    let only_okx = SymbolPickerQuery { text: "btc", kind: None, venues: &okx };
    let g = picker_results(&cat, &only_okx, 10);
    assert!(!g.is_empty());
    assert!(g.iter().all(|u| u.listings.iter().all(|l| l.instrument.venue == "okx")));
}

#[test]
fn an_empty_query_lists_the_widest_underlying_first() {
    let cat = Catalog::from_instruments(vec![
        test_row("binance", "XRPUSDT.P", "XRP", "USDT", CryptoPerp),
        test_row("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
    ]);
    let g = picker_results(&cat, &SymbolPickerQuery { text: "", kind: None, venues: &[] }, 10);
    assert_eq!(g[0].heading(), "BTC");
}

/// The flat list is one row per matching INSTRUMENT, ranked as a search ranks, spot and
/// perpetual only — two instruments of one venue are two rows.
#[test]
fn the_flat_list_has_a_row_per_instrument_and_only_spot_and_perpetual() {
    let cat = Catalog::from_instruments(vec![
        test_row("binance", "BTCUSDT", "BTC", "USDT", CryptoSpot),
        test_row("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("binance", "BTCUSDC.P", "BTC", "USDC", CryptoPerp),
        test_row("deribit", "BTC-27NOV26", "BTC", "USD", CryptoFuture),
        test_row("deribit", "BTC-27NOV26-58000-C", "BTC", "BTC", Opt),
        test_row("okx", "BTC-USDT-SWAP", "BTC", "USDT", CryptoPerp),
    ]);
    let rows = picker_flat(&cat, "btc", 10);
    let symbols: Vec<&str> = rows.iter().map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(symbols.len(), 4, "{symbols:?}");
    assert!(symbols.contains(&"BTCUSDT.P") && symbols.contains(&"BTCUSDC.P"), "{symbols:?}");
    assert!(!symbols.iter().any(|s| s.contains("27NOV26")), "futures and options never");
    assert_eq!(picker_flat(&cat, "btc", 2).len(), 2, "the cap applies");
}

/// With no query the underlying on the most venues leads, its venues together.
#[test]
fn an_empty_flat_query_leads_with_the_widest_underlying() {
    let cat = Catalog::from_instruments(vec![
        test_row("binance", "XRPUSDT.P", "XRP", "USDT", CryptoPerp),
        test_row("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
    ]);
    let rows = picker_flat(&cat, "", 10);
    let venues: Vec<(&str, &str)> =
        rows.iter().map(|i| (i.base.as_str(), i.venue.as_str())).collect();
    assert_eq!(venues, [("BTC", "binance"), ("BTC", "bybit"), ("XRP", "binance")]);
}
/// With no query to rank by, a venue's chip opens its USDT listing, not whichever the venue
/// lists first: Bybit lists a USDC perpetual and an inverse one beside the USDT one.
#[test]
fn an_empty_query_opens_each_venues_usdt_listing_first() {
    let cat = Catalog::from_instruments(vec![
        test_row("bybit", "BTCPERP.P", "BTC", "USDC", CryptoPerp),
        test_row("bybit", "BTCUSD.P", "BTC", "USD", CryptoPerp),
        test_row("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
    ]);
    let g = picker_results(&cat, &SymbolPickerQuery { text: "", kind: None, venues: &[] }, 10);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].listings[0].instrument.raw_symbol, "BTCUSDT.P");
    assert_eq!(g[0].listings[0].others, 2);
}

/// **One rule for "the same instrument on another venue"**: a `BTCUSDT` window moved to a venue that
/// lists only `BTC-EUR` and `BTC-USDC` (EUR first) opens the USDC pair, the one the picker draws on
/// `BTCUSDT`'s own line; the exact quote still wins within the line, and a venue that lists the base on
/// no such line still answers.
#[test]
fn another_venues_listing_is_the_one_on_the_pickers_line() {
    let all = [
        test_row("binance", "BTCUSDT", "BTC", "USDT", CryptoSpot),
        test_row("okx", "BTC-EUR", "BTC", "EUR", CryptoSpot),
        test_row("okx", "BTC-USDC", "BTC", "USDC", CryptoSpot),
        test_row("bybit", "BTCUSDC", "BTC", "USDC", CryptoSpot),
        test_row("bybit", "BTCUSDT", "BTC", "USDT", CryptoSpot),
        test_row("deribit", "BTC-PERPETUAL", "BTC", "USD", CryptoPerp),
    ];
    let cat = Catalog::from_instruments(all.to_vec());
    let opens: Vec<(&str, &str)> = same_instrument_per_venue(&cat, "BTC", "USDT", Some(CryptoSpot))
        .iter()
        .map(|i| (i.venue.as_str(), i.raw_symbol.as_str()))
        .collect();
    assert_eq!(
        opens,
        [
            ("binance", "BTCUSDT"),
            ("okx", "BTC-USDC"),
            ("bybit", "BTCUSDT"),
            ("deribit", "BTC-PERPETUAL")
        ]
    );
    let refs: Vec<&Instrument> = all.iter().collect();
    let line = &group_by_underlying(&refs)[0];
    let okx = line.listings.iter().find(|l| l.instrument.venue == "okx").expect("okx on the line");
    assert_eq!(okx.instrument.raw_symbol, "BTC-USDC", "the picker draws the same pair there");
    assert!(
        same_instrument_per_venue(&cat, "BTCUSDT", "quote", None).is_empty(),
        "no base, nothing"
    );
}

#[test]
fn the_line_cap_applies_after_grouping() {
    let cat = Catalog::from_instruments(vec![
        test_row("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("binance", "ETHUSDT.P", "ETH", "USDT", CryptoPerp),
    ]);
    let g = picker_results(&cat, &SymbolPickerQuery { text: "", kind: None, venues: &[] }, 1);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].listings.len(), 2, "a cap on LINES must not drop a line's venues");
}

#[test]
fn venue_counts_counts_spot_and_perpetual_only_in_first_seen_order() {
    let cat = Catalog::from_instruments(vec![
        test_row("okx", "BTC-USDT", "BTC", "USDT", CryptoSpot),
        test_row("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("okx", "BTC-27NOV26", "BTC", "USD", CryptoFuture),
        test_row("okx", "ETH-USDT", "ETH", "USDT", CryptoSpot),
    ]);
    assert_eq!(venue_counts(&cat), vec![("okx".to_string(), 2), ("binance".to_string(), 1)]);
}
