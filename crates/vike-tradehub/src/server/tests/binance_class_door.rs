//! The control door's reading of the Binance classes beyond a plain perpetual (decision 0230, under decision 0290's
//! user names): a TradFi perpetual is a plain `.P` name, a dated USDS-M future `BTCUSDT_261225` (no `.P`: `.P` is a
//! perpetual, O-1), a COIN-M perpetual `BTCUSD.P`, each found by its kept row; an options symbol is refused by name.
//! aster's door reads the same grammar.

/// The door's reading of `typed` on `venue` over the suites' kept rows (`crate::testutil::door_rows`).
fn door(venue: &str, typed: &str) -> Result<String, String> {
    vike_catalog::door_user_symbol(venue, typed, &crate::testutil::door_rows())
}

#[test]
fn a_dated_future_and_a_coin_m_perpetual_are_read_by_their_user_names() {
    for (typed, user) in [
        ("BTCUSDT_261225", "BTCUSDT_261225"),
        ("btcusdt_261225", "BTCUSDT_261225"),
        ("TSLAUSDT.P", "TSLAUSDT.P"),
        ("xauusdt.p", "XAUUSDT.P"),
        (" btcusd.p ", "BTCUSD.P"),
    ] {
        assert_eq!(door("binance", typed).as_deref(), Ok(user), "{typed}");
    }
    // The old spellings name no row (decision 0117), and neither does a `_PERP` on a USDT book or a bare quote (BIN-5:
    // only a COIN-M perpetual was ever spelled so, and its user name is `BTCUSD.P`).
    for old in ["BTCUSDT_261225.P", "BTCUSD_PERP.P", "BTCUSD_PERP", "BTCUSDT_PERP.P", "USD_PERP.P"]
    {
        let why = door("binance", old).expect_err(old);
        assert!(why.contains("carries no row"), "{old}: {why}");
    }
}

#[test]
fn an_options_symbol_is_refused_by_name() {
    for venue in ["binance", "aster"] {
        for typed in ["BTC-261225-85000-C", "eth-261225-3000-p"] {
            let why = door(venue, typed).expect_err(typed);
            assert!(why.contains("is an option"), "{venue} {typed}: {why}");
        }
    }
    // Something that merely has hyphens is no option, and names no row either.
    for typed in ["ETH-USDT", "BTC-261225-85000", "BTC-261225-85000-X"] {
        let why = door("binance", typed).expect_err(typed);
        assert!(why.contains("carries no row"), "{typed}: {why}");
    }
}
