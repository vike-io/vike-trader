//! The door's reading of a typed symbol (decision 0290): its shape, then the user name the venue's kept row answers,
//! for the control door ([`door_user_symbol`]), the CLI's and the boot's stated symbol ([`venue_engine_symbol`]) and
//! every door that takes a name as written ([`respelled_symbol`]); and the readers that stay beside it.

use super::*;
use crate::test_support::test_row;
use vike_model::InstrumentVenueIds;

/// An outcome token id of the shape polymarket gives.
const TOKEN: &str = "67102598924726349989742049657609063097299442010642912942032681022413402597017";

/// One or two kept rows of every roster venue, spelled as each venue's list keys them.
fn rows() -> Vec<Instrument> {
    use AssetClass::{CryptoPerp, CryptoSpot, Equity, Fx, Option, PredictionMarket};
    let coin = |row: Instrument, coin: &str| {
        row.with_wire(InstrumentVenueIds {
            coin: Some(coin.into()),
            ..InstrumentVenueIds::default()
        })
    };
    crate::spelled(vec![
        test_row("binance", "BTCUSDT", "BTC", "USDT", CryptoSpot),
        test_row("binance", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("aster", "ETHUSDT.P", "ETH", "USDT", CryptoPerp),
        test_row("bybit", "BTCUSDT", "BTC", "USDT", CryptoSpot),
        test_row("bybit", "BTCUSDT.P", "BTC", "USDT", CryptoPerp),
        test_row("bybit", "BTCUSD.P", "BTC", "USD", CryptoPerp),
        test_row("okx", "BTC-USDT", "BTC", "USDT", CryptoSpot),
        test_row("okx", "BTC-USDT-SWAP", "BTC", "USDT", CryptoPerp),
        test_row("deribit", "BTC-PERPETUAL", "BTC", "USD", CryptoPerp),
        test_row("deribit", "XRP_USDC-27MAR26-2D5-C", "XRP", "USDC", Option),
        coin(test_row("hyperliquid", "kPEPE", "kPEPE", "USDC", CryptoPerp), "kPEPE"),
        coin(test_row("hyperliquid", "HYPE/USDC", "HYPE", "USDC", CryptoSpot), "@107"),
        test_row("alpaca", "AAPL", "AAPL", "USD", Equity),
        test_row("alpaca", "BTC/USD", "BTC", "USD", CryptoSpot),
        test_row("ibkr", "AAPL.SMART.USD", "AAPL", "USD", Equity),
        test_row("ibkr", "USD/JPY/CASH/IDEALPRO", "USD", "JPY", Fx),
        test_row("ig", "CS.D.EURUSD.MINI.IP", "EUR", "USD", Fx),
        test_row("oanda", "EUR_USD", "EUR", "USD", Fx),
        test_row("fxcm", "EUR/USD", "EUR", "USD", Fx),
        test_row("dukascopy", "EURUSD", "EUR", "USD", Fx),
        test_row("ctrader", "EURUSDm", "EUR", "USD", Fx),
        test_row("polymarket", TOKEN, "Yes", "USDC", PredictionMarket),
    ])
}

/// **The door, `venue_engine_symbol` and `respelled_symbol` agree with the translator on every venue**: each kept
/// row's user name, typed in any case, is that user name at every door; written in another case it is refused as
/// written where a name is taken as written; and the venue's own (wire) spelling, where it is another, never reads as
/// it (decision 0117), bybit's bare perpetual name reading as the SPOT pair it now names.
#[test]
fn every_door_agrees_with_the_translator_on_every_venue() {
    let rows = rows();
    let mut venues: Vec<&str> = rows.iter().map(|r| r.venue.as_str()).collect();
    venues.dedup();
    assert_eq!(venues.len(), vike_model::VENUES.len(), "every roster venue has a row");
    for row in &rows {
        let venue = row.venue.as_str();
        let names = crate::names_of_row(row).unwrap_or_else(|e| panic!("{venue}: {e}"));
        let user = names.user.as_str();
        let lower = format!(" {} ", user.to_ascii_lowercase());
        assert_eq!(door_user_symbol(venue, &lower, &rows).as_deref(), Ok(user), "{venue} {lower}");
        assert_eq!(
            venue_engine_symbol(venue, &lower, &rows).as_deref(),
            Ok(user),
            "{venue} {lower}"
        );
        assert_eq!(respelled_in(venue, user, &rows), None, "{venue} {user}");
        if lower.trim() != user {
            let why =
                respelled_in(venue, &lower, &rows).unwrap_or_else(|| panic!("{venue} {lower}"));
            assert!(why.contains(&format!("is written `{user}` on {venue}")), "{why}");
        }
        if !names.wire.eq_ignore_ascii_case(user) {
            assert_ne!(
                door_user_symbol(venue, &names.wire, &rows).as_deref(),
                Ok(user),
                "{venue}: the wire name `{}` is read as the user name",
                names.wire
            );
        }
    }
}

/// **A mount on bybit `BTCUSDT.P` is the perpetual and the bare `BTCUSDT` the spot pair** (decision 0290's item 6,
/// audit flags BYB-1/BYB-7): each is written as it is (no door respells it), each finds its own row, and the old
/// exec-plane spot spelling `BTCUSDT.S` names nothing.
#[test]
fn a_bybit_mount_on_the_perpetual_name_is_the_perpetual_and_the_bare_name_the_spot_pair() {
    let rows = rows();
    for (name, class) in
        [("BTCUSDT.P", AssetClass::CryptoPerp), ("BTCUSDT", AssetClass::CryptoSpot)]
    {
        assert_eq!(respelled_in("bybit", name, &rows), None, "{name} is admitted as written");
        assert_eq!(door_user_symbol("bybit", name, &rows).as_deref(), Ok(name));
        let row = crate::kept_row_of_user_name(&rows, "bybit", name).expect("its row");
        assert_eq!(row.asset_class(), class, "{name}");
    }
    assert!(door_user_symbol("bybit", "BTCUSDT.S", &rows).is_err(), "the old spot spelling");
}

/// **No list is a refusal that names its fix**, the datahub's refresh for a venue it lists and the operator's own row
/// for one that publishes none; and `venue_engine_symbol` keeps a held-session venue's name as typed while it has no
/// list (its running account's session is what lists it), never a public venue's. Once a list exists, an old
/// spelling names nothing there too.
#[test]
fn no_list_names_its_fix_and_never_blocks_the_session_that_lists_it() {
    let why = door_user_symbol("oanda", "EURUSD", &[]).expect_err("no oanda list");
    assert!(
        why.contains("no oanda kept list: run `vike-cli data catalog refresh --venue oanda`"),
        "{why}"
    );
    let why = door_user_symbol("deribit", "BTCUSD.P", &[]).expect_err("no deribit list");
    assert!(why.contains("vike-cli data catalog refresh --venue deribit"), "{why}");
    let why = door_user_symbol("ibkr", "AAPL", &[]).expect_err("no ibkr list");
    assert!(why.contains("`vike-cli data instruments add --venue ibkr <name>`"), "{why}");

    let others =
        crate::spelled(vec![test_row("bybit", "BTCUSDT", "BTC", "USDT", AssetClass::CryptoSpot)]);
    for venue in ["oanda", "ctrader", "dukascopy", "fxcm", "ig"] {
        assert_eq!(
            venue_engine_symbol(venue, " eurusd ", &others).as_deref(),
            Ok("EURUSD"),
            "{venue}"
        );
    }
    for venue in ["deribit", "ibkr", "bybit"] {
        assert!(venue_engine_symbol(venue, "EURUSD", &[]).is_err(), "{venue}");
    }
    let rows = rows();
    let why = venue_engine_symbol("oanda", "EUR_USD", &rows).expect_err("an old spelling");
    assert!(why.contains("carries no row"), "{why}");
    assert_eq!(respelled_in("oanda", "eurusd", &[]), None, "no list judges nothing");
}

/// **A name refused by its SHAPE says why before any lookup**: an option off deribit, a deribit coin-settled option,
/// hyperliquid's wire name of a spot pair and a character no symbol has; a deribit USDC option is the venue's own
/// name with its decimal `d`, a polymarket token names itself, and a venue with no rule is not read.
#[test]
fn a_name_refused_by_its_shape_says_why_before_any_lookup() {
    let refused = |venue: &str, raw: &str| {
        door_symbol(venue, raw)
            .unwrap_or_else(|| panic!("{venue} has a rule"))
            .expect_err(&format!("{venue} {raw}"))
    };
    for (venue, raw) in [
        ("okx", "BTC-USD-261106-76000-C"),
        ("binance", "BTC-261225-85000-C"),
        ("aster", "btc-261225-85000-p"),
        ("bybit", "BTC-31OCT26-75500-C-USDT"),
        ("ibkr", "AAPL.SMART.USD.OPT.20261218"),
    ] {
        assert!(refused(venue, raw).contains("is an option"), "{venue} {raw}");
    }
    assert!(refused("deribit", "BTC-27MAR26-60000-C").contains("coin-settled option"));
    assert!(refused("hyperliquid", "@107").contains("wire name"));
    assert!(refused("binance", "BTC USDT").contains("is not a symbol"));
    assert!(refused("polymarket", "abc").contains("outcome token id"));
    assert_eq!(
        door_symbol("deribit", "xrp_usdc-27mar26-2d5-c"),
        Some(Ok("XRP_USDC-27MAR26-2d5-C".to_string()))
    );
    assert_eq!(door_symbol("polymarket", TOKEN), Some(Ok(TOKEN.to_string())));
    assert_eq!(door_symbol("sim", "ANY"), None, "no rule, not read");
    assert!(door_user_symbol("sim", "ANY", &rows()).expect_err("no rule").contains("sim"));
    let why = door_user_symbol(
        "deribit",
        "BTCUSDC",
        &crate::spelled(vec![test_row(
            "deribit",
            "BTC_USDC",
            "BTC",
            "USDC",
            AssetClass::CryptoSpot,
        )]),
    )
    .expect_err("a deribit spot pair");
    assert!(why.contains("is a spot pair"), "{why}");
}

/// Every row of the table is a roster venue (`every_roster_venue_has_a_symbol_rule`, beside the door, is the other
/// direction).
#[test]
fn every_rule_row_is_a_roster_venue() {
    for (venue, _) in SYMBOL_RULES {
        assert!(vike_model::VENUES.contains(venue), "{venue}");
    }
}

/// **A kept row is found by its exact spelling, else by its user name, and never made up**: fxcm's list writes
/// `EUR/USD` where an order writes `EURUSD`; a name the list does not carry, another venue's row, and two rows one
/// name cannot tell apart all answer `None`.
#[test]
fn a_kept_row_is_found_by_its_spelling_and_never_made_up() {
    let fx = |venue: &str, raw: &str, base: &str, quote: &str| {
        test_row(venue, raw, base, quote, AssetClass::Fx)
    };
    let rows = crate::spelled(vec![
        fx("fxcm", "EUR/USD", "EUR", "USD"),
        fx("fxcm", "USD/JPY", "USD", "JPY"),
        fx("dukascopy", "EUR/USD", "EUR", "USD"),
    ]);
    let found = |venue: &str, symbol: &str| {
        kept_row(&rows, venue, symbol).map(|r| (r.venue.clone(), r.raw_symbol.clone()))
    };
    let fxcm_pair = Some(("fxcm".to_string(), "EUR/USD".to_string()));
    assert_eq!(found("fxcm", "EUR/USD"), fxcm_pair);
    assert_eq!(found("fxcm", "EURUSD"), fxcm_pair, "its user name");
    assert_eq!(found("fxcm", "eurusd"), fxcm_pair);
    assert_eq!(found("fxcm", "GBPUSD"), None, "a name the list does not carry");
    assert_eq!(found("oanda", "EURUSD"), None, "another venue's list is not this one's");
    let twice = crate::spelled(vec![
        fx("fxcm", "EUR/USD", "EUR", "USD"),
        fx("fxcm", "EURUSD", "EUR", "USD"),
    ]);
    assert_eq!(kept_row(&twice, "fxcm", "eur/usd"), None, "an ambiguous answer is no answer");
    assert_eq!(
        kept_row(&twice, "fxcm", "EURUSD").map(|r| r.raw_symbol.as_str()),
        Some("EURUSD"),
        "the exact spelling is not ambiguous"
    );
}

/// **A `.bi5` dataset is the vendor's folder, and it finds its ONE kept dukascopy row** (`AAPLUSUSD` is
/// `AAPL.US/USD`), never another venue's, and two rows of one dataset are no answer, unless only one of them states
/// a price step (DUK-6: an operator's row beside the venue's step-less one), which then answers.
#[test]
fn a_bi5_dataset_finds_its_one_dukascopy_row() {
    use crate::test_support::test_row;
    let cfd = |venue: &str, raw: &str| test_row(venue, raw, "", "USD", AssetClass::Cfd);
    let rows = vec![cfd("dukascopy", "AAPL.US/USD"), cfd("fxcm", "MSFT.US/USD")];
    assert_eq!(bi5_dataset_name("AAPL.US/USD"), "AAPLUSUSD");
    assert_eq!(bi5_dataset_name("EUR/USD"), "EURUSD");
    assert_eq!(
        bi5_dataset_row(&rows, "AAPLUSUSD").map(|r| r.raw_symbol.as_str()),
        Some("AAPL.US/USD")
    );
    assert_eq!(bi5_dataset_row(&rows, "MSFTUSUSD"), None, "a dukascopy row only");
    assert_eq!(bi5_dataset_row(&rows, "AAPL.US/USD"), None, "the dataset, not the row's name");
    let twice = vec![cfd("dukascopy", "AAPL.US/USD"), cfd("dukascopy", "AAPLU.S/USD")];
    assert_eq!(bi5_dataset_row(&twice, "AAPLUSUSD"), None, "an ambiguous answer is no answer");
    let stepped = |raw: &str, tick_size: f64| {
        let mut row = cfd("dukascopy", raw);
        row.properties.tick_size = tick_size;
        row
    };
    let one_steps = vec![cfd("dukascopy", "AAPL.US/USD"), stepped("AAPLU.S/USD", 0.001)];
    assert_eq!(
        bi5_dataset_row(&one_steps, "AAPLUSUSD").map(|r| r.raw_symbol.as_str()),
        Some("AAPLU.S/USD"),
        "the one row that states a step answers beside a step-less one"
    );
    let both_step = vec![stepped("AAPL.US/USD", 0.001), stepped("AAPLU.S/USD", 0.001)];
    assert_eq!(
        bi5_dataset_row(&both_step, "AAPLUSUSD"),
        None,
        "two rows with a step are no answer"
    );
}

/// The one outcome-token test's bounds, the only pin of the 78-digit ceiling: an ERC-1155 id is a uint256, so 20 to 78
/// decimal digits and nothing else. A condition id, a signed or decimal number and a slug are never a token.
#[test]
fn an_outcome_token_is_twenty_to_seventy_eight_digits() {
    assert!(is_outcome_token(TOKEN), "the shape polymarket gives");
    assert!(is_outcome_token(&"1".repeat(20)), "twenty digits");
    assert!(!is_outcome_token(&"1".repeat(19)), "nineteen digits");
    assert!(is_outcome_token(&"9".repeat(78)), "a uint256's most digits");
    assert!(!is_outcome_token(&"9".repeat(79)), "more digits than a uint256 holds");
    for text in [
        "0x0000000000000000000000000000000000000000000000000000000000000001",
        "-12345678901234567890",
        "12345678901234567890.5",
        "123456789012345678901234567890x",
        "rain-market:Yes",
        "",
    ] {
        assert!(!is_outcome_token(text), "{text:?}");
    }
}
