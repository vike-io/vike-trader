//! `venue_engine_symbol`, the CLI's offline reading of an account's stated symbol: the door's grammar first (its
//! refusal in its own words), then bybit's kept row. The grammar's own cases stay beside the door that
//! plans on it (`crates/vike-tradehub/src/server/tests/`).

use super::*;

/// bybit's kept rows as the datahub's list holds them (spelled as `crates/bridges/bybit/src/catalog.rs` keys them): a
/// spot pair under its bare name, a perpetual with `.P`, a dated contract under the venue's own name.
fn bybit_rows() -> Vec<Instrument> {
    use crate::test_support::test_row;
    use AssetClass::{CryptoFuture, CryptoPerp, CryptoSpot};
    [
        ("BTCUSDT", CryptoSpot),
        ("BTCUSDT.P", CryptoPerp),
        ("BTCPERP.P", CryptoPerp),
        ("DOGEUSDT", CryptoSpot),
        ("BTCUSD", CryptoSpot),
        ("BTCUSD.P", CryptoPerp),
        ("ETHUSDT-27NOV26", CryptoFuture),
        ("BTCUSDZ26", CryptoFuture),
    ]
    .into_iter()
    .map(|(name, class)| test_row("bybit", name, "BTC", "USDT", class))
    .collect()
}

#[test]
fn a_name_the_venue_spells_is_canonicalised_and_one_its_list_does_not_carry_is_refused_by_name() {
    let rows = bybit_rows();
    assert_eq!(
        venue_engine_symbol("bybit", "btcusd", &rows).as_deref(),
        Ok("BTCUSD"),
        "the grammar's spelling"
    );
    for named in ["BTCUSDT", "BTCUSDZ26", "ETHUSDT-27NOV26", "DOGEUSDT.S", "BTCPERP"] {
        assert_eq!(venue_engine_symbol("bybit", named, &rows).as_deref(), Ok(named), "{named}");
    }
    let why = venue_engine_symbol("bybit", "BTCUSDX", &rows).expect_err("no row of it");
    assert!(why.contains("`BTCUSDX` names no bybit book: the kept list"), "{why}");
    let why = venue_engine_symbol("bybit", "BTCUSDT", &[]).expect_err("no list kept");
    assert!(why.contains("no bybit instrument list is kept on this box yet"), "{why}");

    // The grammar refuses first, in the door's words: another venue's spelling.
    let why = venue_engine_symbol("bybit", "BTC-PERPETUAL", &rows).expect_err("a deribit name");
    assert_eq!(Err(why), canonical_symbol(VenueSymbolSpelling::BareLinear, "BTC-PERPETUAL"));
    assert_eq!(
        venue_engine_symbol("deribit", "btc-perpetual", &[]).as_deref(),
        Ok("BTC-PERPETUAL"),
        "no other venue reads a kept row here"
    );
    assert!(venue_engine_symbol("okx", "BTCUSDT", &[]).is_err(), "okx's names are its own");
    assert!(
        venue_engine_symbol("nowhere", "BTCUSD", &[]).expect_err("no rule").contains("nowhere")
    );
}

/// **An engine name's kept row, by the lane suffixes alone**: `X.S` is the spot row `X`, a bare name the perpetual's
/// row `X.P` or a dated row `X`, and a bare name never the spot row of the same string. A name the list does not
/// carry, an option, another venue's row: `None`.
#[test]
fn a_bybit_engine_name_finds_its_kept_row_by_the_lane_suffixes() {
    let rows = bybit_rows();
    let row = |name: &str| bybit_kept_row(&rows, name).map(|r| r.raw_symbol.as_str());
    assert_eq!(row("BTCUSDT"), Some("BTCUSDT.P"), "the bare name is the perpetual");
    assert_eq!(row("BTCUSDT.S"), Some("BTCUSDT"), "the spot pair");
    assert_eq!(row("btcusd"), Some("BTCUSD.P"), "never the vestigial spot row of the same string");
    assert_eq!(row("BTCUSD.S"), Some("BTCUSD"));
    assert_eq!(row("ETHUSDT-27NOV26"), Some("ETHUSDT-27NOV26"), "a dated contract by its own name");
    assert_eq!(row("BTCUSDZ26"), Some("BTCUSDZ26"));
    assert_eq!(row("DOGEUSDT"), None, "a spot pair alone is no perpetual");
    for name in ["BTCUSDX", "BTC-31OCT26-75500-C-USDT", "", ".S", "ETHUSDT-27NOV26.S"] {
        assert_eq!(row(name), None, "{name}");
    }
    let other = vec![crate::test_support::test_row(
        "okx",
        "BTCUSDT.P",
        "BTC",
        "USDT",
        AssetClass::CryptoPerp,
    )];
    assert_eq!(bybit_kept_row(&other, "BTCUSDT"), None, "another venue's row");
}

/// Every row of the table is a roster venue (`every_roster_venue_has_a_symbol_rule`, beside the door, is the other
/// direction).
#[test]
fn every_rule_row_is_a_roster_venue() {
    for (venue, _) in SYMBOL_RULES {
        assert!(vike_model::VENUES.contains(venue), "{venue}");
    }
}

/// **One instrument, one spelling, at every door** (the C-D4 follow-up): `door_symbol` is the control door's own
/// reading for any venue with a rule, and `respelled_symbol` refuses only a name that grammar spells ANOTHER way. A
/// case-kept venue's own case passes, and a name the grammar refuses (an engine's own symbol the order grammar does
/// not model, a test's made-up name), a venue with no rule, and ibkr's and oanda's engine spellings (which the door
/// keeps as they are) are not judged.
#[test]
fn a_door_refuses_only_another_spelling_of_a_name_the_grammar_reads() {
    assert_eq!(door_symbol("bybit", "ethusdt.s"), Some(Ok("ETHUSDT.S".to_string())));
    assert_eq!(door_symbol("sim", "ANY"), None, "no rule, not read");
    for (venue, raw, canonical) in [
        ("bybit", "ethusdt.s", "ETHUSDT.S"),
        ("bybit", "BTCUSDT.P", "BTCUSDT"),
        ("binance", "btcusdt.p", "BTCUSDT.P"),
        ("fxcm", "EUR/USD", "EURUSD"),
        ("dukascopy", "eurusd", "EURUSD"),
        ("ig", "cs.d.eurusd.mini.ip", "CS.D.EURUSD.MINI.IP"),
    ] {
        let why = respelled_symbol(venue, raw).unwrap_or_else(|| panic!("{venue} {raw}"));
        assert!(why.contains(&format!("`{raw}` is written `{canonical}` on {venue}")), "{why}");
    }
    for (venue, raw) in [
        ("bybit", "ETHUSDT.S"),
        ("binance", "BTCUSDT.P"),
        ("hyperliquid", "kPEPE"),
        ("ctrader", "Wheat_H6"),
        ("ibkr", "AAPL.SMART.USD"),
        ("ibkr", "VOD.LSE.USD.STK"),
        ("ibkr", "EUR.IDEALPRO.USD"),
        ("ibkr", "BTCUSDT"),
        ("oanda", "EUR_USD"),
        ("binance", "MM_SPREAD_BTC"),
        ("polymarket", "ROW_TOK"),
        ("sim", "anything"),
        ("bybit", ""),
    ] {
        assert_eq!(respelled_symbol(venue, raw), None, "{venue} {raw}");
    }
}

/// **deribit's `DDMMMYY`, read once**: one or two day digits, an upper-case month, two year digits, and a day
/// the calendar has. Every other shape is `None`, and so is a day its month lacks.
#[test]
fn a_deribit_expiry_is_a_day_the_calendar_has() {
    for (expiry, date) in [
        ("27MAR26", Some((2026, 3, 27))),
        ("3NOV26", Some((2026, 11, 3))),
        ("03NOV26", Some((2026, 11, 3))),
        ("31DEC99", Some((2099, 12, 31))),
        ("29FEB28", Some((2028, 2, 29))),
        ("29FEB00", Some((2000, 2, 29))),
        ("29FEB26", None),
        ("31FEB26", None),
        ("31APR26", None),
        ("00JAN26", None),
        ("0JAN26", None),
        ("32JAN26", None),
        ("27mar26", None),
        ("27XXX26", None),
        ("127MAR26", None),
        ("7MAR6", None),
        ("27MAR2X", None),
        ("2XMAR26", None),
        ("27MÄR26", None),
        ("", None),
    ] {
        assert_eq!(deribit_expiry(expiry), date, "{expiry:?}");
    }
}

/// **A kept row is found by its exact spelling, else by the door's one spelling of it, and never made up**: fxcm's
/// list writes `EUR/USD` where an order writes `EURUSD`; a name the list does not carry, another venue's row, and two
/// rows one spelling cannot tell apart all answer `None`.
#[test]
fn a_kept_row_is_found_by_its_spelling_and_never_made_up() {
    use crate::test_support::test_row;
    let fx = |venue: &str, raw: &str| test_row(venue, raw, "EUR", "USD", AssetClass::Fx);
    let rows = vec![fx("fxcm", "EUR/USD"), fx("fxcm", "USD/JPY"), fx("dukascopy", "EUR/USD")];
    let found = |venue: &str, symbol: &str| {
        kept_row(&rows, venue, symbol).map(|r| (r.venue.clone(), r.raw_symbol.clone()))
    };
    let fxcm_pair = Some(("fxcm".to_string(), "EUR/USD".to_string()));
    assert_eq!(found("fxcm", "EUR/USD"), fxcm_pair);
    assert_eq!(found("fxcm", "EURUSD"), fxcm_pair, "the door spells both `EURUSD`");
    assert_eq!(found("fxcm", "eurusd"), fxcm_pair);
    assert_eq!(found("fxcm", "GBPUSD"), None, "a name the list does not carry");
    assert_eq!(found("oanda", "EURUSD"), None, "another venue's list is not this one's");
    let twice = vec![fx("fxcm", "EUR/USD"), fx("fxcm", "EURUSD")];
    assert_eq!(kept_row(&twice, "fxcm", "eur/usd"), None, "an ambiguous answer is no answer");
    assert_eq!(
        kept_row(&twice, "fxcm", "EURUSD").map(|r| r.raw_symbol.as_str()),
        Some("EURUSD"),
        "the exact spelling is not ambiguous"
    );
}
