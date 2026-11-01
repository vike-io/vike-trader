//! The control door's spelling of the Binance classes beyond a plain perpetual (decision 0230): a TradFi
//! perpetual is a plain `.P` symbol, a dated USDS-M future keeps its delivery date in the symbol and is spelled with
//! the lane suffix (`BTCUSDT_261225.P`), and an options symbol is refused by name. The spelling is the one binance
//! and aster share (`Spelling::LaneSuffixed`), so aster's door is held to the same rows.

use crate::server::symbol_admission::{Spelling, canonical_symbol};

#[test]
fn a_dated_future_is_spelled_with_the_lane_suffix_and_its_date() {
    for (typed, canonical) in [
        ("BTCUSDT_261225.P", "BTCUSDT_261225.P"),
        ("btcusdt_261225.p", "BTCUSDT_261225.P"),
        ("  ETHUSDT_270326.P ", "ETHUSDT_270326.P"),
        ("TSLAUSDT.P", "TSLAUSDT.P"),
        ("XAUUSDT.P", "XAUUSDT.P"),
        ("1000PEPEUSDT.P", "1000PEPEUSDT.P"),
        // A COIN-M perpetual is let through so the facts can refuse it by name (it is not in the USDS-M list).
        ("btcusd_perp.p", "BTCUSD_PERP.P"),
    ] {
        assert_eq!(
            canonical_symbol(Spelling::LaneSuffixed, typed).as_deref(),
            Ok(canonical),
            "{typed}"
        );
    }
}

#[test]
fn a_dated_future_without_the_lane_suffix_is_refused_saying_how_to_write_it() {
    let why = canonical_symbol(Spelling::LaneSuffixed, "btcusdt_261225").expect_err("bare dated");
    assert!(why.contains("futures contract") && why.contains("`BTCUSDT_261225.P`"), "{why}");
    assert!(why.contains("spot lane"), "{why}");
    let why = canonical_symbol(Spelling::LaneSuffixed, "BTCUSD_PERP").expect_err("bare COIN-M");
    assert!(why.contains("`BTCUSD_PERP.P`"), "{why}");
}

#[test]
fn an_underscore_means_a_delivery_date_or_nothing() {
    for bad in [
        "BTC_USDT.P",
        "BTCUSDT_26122.P",
        "BTCUSDT_2612255.P",
        "BTCUSDT_26122A.P",
        "_261225.P",
        "BTCUSDT__261225.P",
        "BTC_USDT_261225.P",
        "BTCUSDT_PERPX.P",
        "BTCUSDT_PERP_261225.P",
    ] {
        let why = canonical_symbol(Spelling::LaneSuffixed, bad).expect_err(bad);
        assert!(why.contains("is not an exchange symbol"), "{bad}: {why}");
    }
    // No other spelling gains the underscore.
    for spelling in [Spelling::BareLinear, Spelling::Hyphenated] {
        assert!(canonical_symbol(spelling, "BTCUSDT_261225").is_err(), "{spelling:?}");
    }
}

#[test]
fn an_options_symbol_is_refused_by_name_with_the_measurement() {
    for typed in ["BTC-261225-85000-C", "eth-261225-3000-p", "XAUUSDT-261225-4000-C"] {
        let why = canonical_symbol(Spelling::LaneSuffixed, typed).expect_err(typed);
        assert!(why.contains("options symbol") && why.contains("no options API"), "{typed}: {why}");
        assert!(!why.contains("not an exchange symbol"), "{typed}: {why}");
    }
    // Something that merely has hyphens is not an option: the generic refusal.
    for typed in ["ETH-USDT", "BTC-261225-85000", "BTC-261225-85000-X", "BTC-26122-85000-C"] {
        let why = canonical_symbol(Spelling::LaneSuffixed, typed).expect_err(typed);
        assert!(why.contains("is not an exchange symbol"), "{typed}: {why}");
    }
}
