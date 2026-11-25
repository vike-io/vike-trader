//! The control door's reading of a Bybit symbol (decision 0231, under decision 0290's user names): a linear
//! perpetual is `ETHUSDT.P`, the SPOT pair the bare `ETHUSDT`, a dated linear future `ETHUSDT_261127`, a
//! coin-margined perpetual `BTCUSD.P` and a coin-margined dated contract `BTCUSD_261225`, each the book its kept row
//! states. An option is refused by name; the old exec-plane spellings (`ETHUSDT.S`, `ETHUSDT-27NOV26`, `BTCUSDZ26`)
//! name no row.

use vike_model::AssetClass::{CryptoFuture, CryptoPerp, CryptoSpot};

/// The door's reading of `typed` on bybit over the suites' kept rows (`crate::testutil::door_rows`).
fn door(typed: &str) -> Result<String, String> {
    vike_catalog::door_user_symbol("bybit", typed, &crate::testutil::door_rows())
}

#[test]
fn a_bybit_name_keeps_the_book_it_names() {
    let rows = crate::testutil::door_rows();
    for (typed, user, class) in [
        ("ethusdt.p", "ETHUSDT.P", CryptoPerp),
        ("ethusdt", "ETHUSDT", CryptoSpot),
        (" ethusdt_261127 ", "ETHUSDT_261127", CryptoFuture),
        ("btcusd.p", "BTCUSD.P", CryptoPerp),
        ("BTCUSD_261225", "BTCUSD_261225", CryptoFuture),
        ("dogeusdt", "DOGEUSDT", CryptoSpot),
    ] {
        assert_eq!(door(typed).as_deref(), Ok(user), "{typed}");
        let row = vike_catalog::kept_row_of_user_name(&rows, "bybit", user).expect(user);
        assert_eq!(row.asset_class(), class, "{typed}");
    }
}

#[test]
fn what_is_not_a_bybit_user_name_is_refused_in_words() {
    for raw in ["", "ETH USDT"] {
        let why = door(raw).expect_err(raw);
        assert!(why.contains("is not a symbol"), "{raw}: {why}");
    }
    for old in ["ETHUSDT.S", "ETHUSDT-27NOV26", "BTCUSDZ26", "ETH-USDT", "ETH/USDT"] {
        let why = door(old).expect_err(old);
        assert!(why.contains("carries no row"), "{old}: {why}");
    }
    // An option is named as one.
    let why = door("BTC-31OCT26-75500-C-USDT").expect_err("an option");
    assert!(why.contains("is an option") && why.contains("payoff"), "{why}");
}
