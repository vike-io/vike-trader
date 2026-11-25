use super::{contract_value, is_coin_settled_option, is_inverse_contract, option_underlyings};
use vike_model::ContractValue;

/// Decision 0290's user names of the four venues' coin-margined contracts (their venue names in the comments).
#[test]
fn the_inverse_names_of_each_venue() {
    for (venue, symbol) in [
        ("deribit", "BTCUSD.P"),      // BTC-PERPETUAL
        ("deribit", "ETHUSD.P"),      // ETH-PERPETUAL
        ("deribit", "BTCUSD_261009"), // BTC-9OCT26
        ("deribit", "BTCUSD_261227"), // BTC-27DEC26
        ("bybit", "BTCUSD.P"),
        ("bybit", "HYPEUSD.P"),
        ("bybit", "BTCUSD_261225"),
        ("bybit", "ETHUSD_270326"),
        ("binance", "BTCUSD.P"),      // BTCUSD_PERP
        ("binance", "ETHUSD.P"),      // ETHUSD_PERP
        ("binance", "BTCUSD_261225"), // BTCUSD_261225 on dapi
        ("okx", "BTCUSD.P"),          // BTC-USD-SWAP
        ("okx", "BTCUSD_261030"),     // BTC-USD-261030
        // A typed lower case names the same contract.
        ("deribit", "btcusd.p"),
        ("binance", "btcusd.p"),
    ] {
        assert!(is_inverse_contract(venue, symbol), "{venue} {symbol}");
    }
}

#[test]
fn the_linear_names_of_each_venue() {
    for (venue, symbol) in [
        ("deribit", "BTCUSDC.P"),
        ("deribit", "SOLUSDC.P"),
        ("deribit", "BTCUSDC_261227"),
        ("deribit", "BTCUSDC"),
        ("deribit", "BTC-27DEC26-60000-C"),
        ("deribit", "SOL_USDC-27DEC26-150-P"),
        ("deribit", ""),
        // An old venue name is no user name, and is not read as one (decision 0117).
        ("deribit", "BTC-PERPETUAL"),
        ("deribit", "BTC-27DEC26"),
        ("deribit", "ETH-31FEB26"),
        ("bybit", "BTCUSDT"),
        ("bybit", "BTCUSDT.P"),
        ("bybit", "BTCUSDC.P"),
        ("bybit", "ETHUSDT_261127"),
        ("bybit", "BTCUSDC_261227"),
        // BYB-4: a SPOT pair quoted in USD is a pair, never an inverse contract (the user grammar's bare name).
        ("bybit", "BTCUSD"),
        ("bybit", "USDTUSD"),
        // An old exec name is no user name, and is not read as one.
        ("bybit", "BTCUSDZ26"),
        ("bybit", "USD"),
        ("bybit", "UZ2"),
        ("bybit", ".P"),
        ("bybit", "USD.P"),
        ("bybit", ""),
        ("binance", "BTCUSDT"),
        ("binance", "BTCUSDT.P"),
        ("binance", "BTCUSDT_261225"),
        ("binance", "BTCUSDC.P"),
        ("binance", "USD.P"),
        // binance's old laned names are no user names (decision 0117)
        ("binance", "BTCUSD_PERP.P"),
        ("binance", "BTCUSD_261225.P"),
        ("okx", "BTCUSDT.P"),
        ("okx", "BTCUSDC.P"),
        ("okx", "BTCUSDT_261225"),
        ("okx", "BTCUSDT"),
        ("okx", "BTCUSD"),
        ("okx", "BTC-USD-SWAP"),
        ("okx", "BTC-USD-261012-75000-C"),
    ] {
        assert!(!is_inverse_contract(venue, symbol), "{venue} {symbol}");
    }
}

/// One name, two venues: the reason the venue is an argument.
#[test]
fn the_same_name_can_be_inverse_on_one_venue_and_linear_on_another() {
    for venue in ["bybit", "binance", "okx", "deribit"] {
        assert!(is_inverse_contract(venue, "BTCUSD.P"), "{venue}");
    }
    for venue in ["ctrader", "oanda", "ig", "ibkr", "hyperliquid", "aster"] {
        assert!(!is_inverse_contract(venue, "BTCUSD.P"), "{venue}");
    }
}

/// **A kept row is read before the name** (BYB-4, BIN-5): where this process holds the venue's list, the row's own
/// margining answers, whatever the grammar says; a name the list carries no row of is the grammar's. (A coin no other
/// test names: the held list is this process's.)
#[test]
fn a_kept_row_answers_before_the_name() {
    let linear = crate::test_support::test_row(
        "deribit",
        "ZZZ_USD-PERPETUAL",
        "ZZZ",
        "USD",
        vike_model::AssetClass::CryptoPerp,
    );
    crate::hold_kept_list("deribit", vec![linear]);
    assert!(!is_inverse_contract("deribit", "ZZZUSD.P"), "its row says linear");
    assert!(is_inverse_contract("deribit", "ZZZUSD_261227"), "no row: the grammar");
}

/// The whole value of an inverse NAME: the venue's dollar where the venue fixes it (deribit, bybit), and no answer
/// on binance COIN-M or okx, whose face only the contract's row states (`contractSize`, `ctVal`): never the common
/// 100, and no face table either ([`a_kept_row_states_the_face_only_its_row_can`] reads the row).
#[test]
fn an_inverse_name_is_valued_at_the_venues_dollar_or_not_at_all() {
    assert_eq!(contract_value("deribit", "BTCUSD.P"), Some(ContractValue::inverse(1.0)));
    assert_eq!(contract_value("deribit", "BTCUSD_261227"), Some(ContractValue::inverse(1.0)));
    assert_eq!(contract_value("bybit", "BTCUSD.P"), Some(ContractValue::inverse(1.0)));
    assert_eq!(contract_value("bybit", "ADAUSD.P"), Some(ContractValue::inverse(1.0)));
    assert_eq!(contract_value("bybit", "BTCUSD_261225"), Some(ContractValue::inverse(1.0)));
    assert_eq!(contract_value("bybit", "USDTUSD"), Some(ContractValue::default()), "BYB-4: a pair");
    for (venue, symbol) in [
        ("binance", "SOLUSD.P"),
        ("binance", "ETHUSD_261225"),
        ("okx", "ETHUSD.P"),
        ("okx", "ETHUSD_261225"),
    ] {
        assert_eq!(contract_value(venue, symbol), None, "no row, no face: {venue} {symbol}");
    }
}

/// A linear name is one unit on every roster venue but the three whose unit differs by symbol (a name that
/// is none of their fixed shapes gets no answer); a venue outside the roster gets none either, never a guessed
/// 1.0.
#[test]
fn a_linear_name_is_one_unit_except_where_only_the_row_states_it() {
    for venue in vike_model::VENUES {
        let want = (!["ibkr", "ig", "fxcm"].contains(venue)).then(ContractValue::default);
        assert_eq!(contract_value(venue, "XYZ"), want, "{venue}");
    }
    assert_eq!(contract_value("deribit", "BTCUSDC.P"), Some(ContractValue::default()));
    assert_eq!(contract_value("spot", "BTCUSDT"), None, "a venue outside the roster");
}

/// A deribit option settled in USDC is linear 1.0; one settled in its coin has no value (its premium is coin).
#[test]
fn a_deribit_coin_settled_option_has_no_value_and_a_usdc_one_is_linear() {
    for coin in ["BTC-27DEC26-60000-C", "ETH-5OCT26-2300-P", "btc-5oct26-74000-c"] {
        assert_eq!(contract_value("deribit", coin), None, "{coin}");
    }
    for usdc in ["SOL_USDC-12OCT26-96-P", "BTC_USDC-27DEC26-60000-C"] {
        assert_eq!(contract_value("deribit", usdc), Some(ContractValue::default()), "{usdc}");
    }
}

/// **The public coin-option check** is exactly the shape [`contract_value`] refuses on deribit: a coin-settled option
/// is one, a USDC option, a future, a perpetual and every other venue's same name are not.
#[test]
fn only_a_deribit_coin_settled_option_is_a_coin_settled_option() {
    for coin in ["BTC-27DEC26-60000-C", "ETH-5OCT26-2300-P", "btc-5oct26-74000-c"] {
        assert!(is_coin_settled_option("deribit", coin), "{coin}");
        assert_eq!(contract_value("deribit", coin), None, "{coin}: the one shape it refuses");
    }
    for not in ["SOL_USDC-12OCT26-96-P", "BTCUSD.P", "BTCUSD_261227", "BTCUSDC", ""] {
        assert!(!is_coin_settled_option("deribit", not), "{not:?}");
    }
    for venue in ["okx", "bybit", "binance", "no-such-venue"] {
        assert!(!is_coin_settled_option(venue, "BTC-27DEC26-60000-C"), "{venue}");
    }
}

/// A deribit USDC option is written on its book's perpetual and spot pair, named by their USER names (decision
/// 0290: the venue's `BTC_USDC-PERPETUAL` and `BTC_USDC`); no other name names an underlying.
#[test]
fn a_deribit_usdc_option_names_its_underlyings_and_nothing_else_does() {
    assert_eq!(
        option_underlyings("deribit", "BTC_USDC-27NOV26-80000-C"),
        Some(["BTCUSDC.P".to_string(), "BTCUSDC".to_string()])
    );
    assert_eq!(
        option_underlyings("deribit", "sol_usdc-12oct26-96-p"),
        Some(["SOLUSDC.P".to_string(), "SOLUSDC".to_string()])
    );
    for none in ["BTC-27DEC26-60000-C", "BTCUSDC.P", "BTCUSDC", "BTCUSD_261227"] {
        assert_eq!(option_underlyings("deribit", none), None, "{none}");
    }
    assert_eq!(option_underlyings("okx", "BTC_USDC-27NOV26-80000-C"), None);
}

/// The ibkr KEPT rows that fix one unit (IBKR-3, decision 0290: by the row's class, a user name states none), the
/// ones whose unit only the row's multiplier states, and every ibkr name without its row; the fxcm shapes whose name
/// fixes one unit, and the ones whose unit only the row states.
#[test]
fn the_ibkr_rows_and_fxcm_names_that_fix_one_unit() {
    use crate::test_support::test_row;
    use vike_model::AssetClass::{CryptoSpot, Equity, Etf, Future, Fx, Index, Option};
    for (name, wire, base, quote, class) in [
        ("AAPL", "AAPL.SMART.USD", "AAPL", "USD", Equity),
        ("SPY", "SPY.SMART.USD", "SPY", "USD", Etf),
        ("USDJPY", "USD.JPY.IDEALPRO", "USD", "JPY", Fx),
        ("BTCUSD", "BTC.PAXOS.USD.CRYPTO", "BTC", "USD", CryptoSpot),
    ] {
        let row = test_row("ibkr", wire, base, quote, class);
        let value = super::kept_contract_value("ibkr", name, Some(&row));
        assert_eq!(value, Some(ContractValue::default()), "{name}");
        assert_eq!(contract_value("ibkr", name), None, "{name} without its row");
    }
    for (wire, base, class) in [
        ("ES.CME.USD.FUT.20261218", "ES", Future),
        ("SPX.CBOE.USD.IND", "SPX", Index),
        ("SPY.SMART.USD.OPT.20261218.500.C", "SPY", Option),
    ] {
        let row = test_row("ibkr", wire, base, "USD", class);
        assert_eq!(super::kept_contract_value("ibkr", wire, Some(&row)), None, "{wire}");
    }
    for name in ["AAPL.SMART.USD", "EUR.USD.IDEALPRO", "AAPL", "ES", ""] {
        assert_eq!(contract_value("ibkr", name), None, "{name}: no row, no unit");
    }
    for one in ["EURUSD", "eurusd", "EUR/USD", "USDCNH"] {
        assert_eq!(contract_value("fxcm", one), Some(ContractValue::default()), "{one}");
    }
    for row_only in
        ["XAUUSD", "XAU/USD", "US30", "USOIL", "COPPER", "NVDA.EXT", "BTC/USD", "EURUSDX"]
    {
        assert_eq!(contract_value("fxcm", row_only), None, "{row_only}");
    }
    assert_eq!(contract_value("ig", "CS.D.EURUSD.CFD.IP"), None, "an epic's size is its row's");
}

/// The settle coin of an inverse name is its base coin, which the user name spells first; a linear name (or a venue
/// that lists no inverse contract) has none.
#[test]
fn an_inverse_name_settles_in_its_base_coin() {
    for (venue, symbol, coin) in [
        ("deribit", "BTCUSD.P", "BTC"),
        ("deribit", "ethusd_261227", "ETH"),
        ("bybit", "BTCUSD.P", "BTC"),
        ("bybit", "ETHUSD.P", "ETH"),
        ("bybit", "BTCUSD_261225", "BTC"),
        ("bybit", "HYPEUSD.P", "HYPE"),
        ("binance", "BTCUSD.P", "BTC"),
        ("binance", "ETHUSD_261225", "ETH"),
        ("okx", "BTCUSD.P", "BTC"),
        ("okx", "ETHUSD_261225", "ETH"),
    ] {
        let got = super::inverse_settle_coin(venue, symbol);
        assert_eq!(got.as_deref(), Some(coin), "{venue} {symbol}");
    }
    for (venue, symbol) in [
        ("deribit", "BTCUSDC.P"),
        ("bybit", "BTCUSDT"),
        ("bybit", "BTCUSD"),
        ("binance", "BTCUSDT.P"),
        ("okx", "BTCUSDT.P"),
        ("ctrader", "BTCUSD"),
    ] {
        assert_eq!(super::inverse_settle_coin(venue, symbol), None, "{venue} {symbol}");
    }
}

/// Every binance COIN-M and okx inverse contract whose face the deleted face tables stated (pinned on them,
/// 2026-11-14), by its USER name (decision 0290), any case: a reader handed one finds the row by it
/// (`crate::kept_row`'s user-name half) and values it by the row's own margining and face.
const LISTED_USER_FACES: &[(&str, &str, f64)] = &[
    ("binance", "BTCUSD.P", 100.0),
    ("binance", "btcusd.p", 100.0),
    ("binance", "ETHUSD.P", 10.0),
    ("binance", "BTCUSD_261225", 100.0),
    ("binance", "BTCUSD_270326", 100.0),
    ("okx", "BTCUSD.P", 100.0),
    ("okx", "btcusd_261030", 100.0),
];

/// **Every face the deleted tables stated is its RECORDED kept row's** (`crate::recorded_kept_lists`, the rows each
/// bridge's parser reads off its committed payload), in every listed spelling; without the row it is no answer.
#[test]
fn every_listed_face_is_its_recorded_kept_rows() {
    use super::kept_contract_value;
    let rows: Vec<vike_model::Instrument> =
        crate::recorded_kept_lists().into_iter().flat_map(|list| list.instruments).collect();
    for &(venue, user, face) in LISTED_USER_FACES {
        let row = crate::kept_row(&rows, venue, user);
        assert!(row.is_some(), "the recorded list carries {user} on {venue}");
        assert_eq!(
            kept_contract_value(venue, user, row),
            Some(ContractValue::inverse(face)),
            "its row: {venue} {user}"
        );
        assert_eq!(kept_contract_value(venue, user, None), None, "no row: {venue} {user}");
    }
}

/// A kept row of `symbol` on `venue` stating `contract_size` (the face reader reads the size and nothing else).
fn kept(venue: &str, symbol: &str, contract_size: f64) -> vike_model::Instrument {
    let perp = vike_model::Contract::Perp(vike_model::ContractPerp {
        margining: vike_model::Margining::Inverse,
        settle: "ETH".into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    });
    vike_model::Instrument::new(venue, symbol, "ETH", "USD", perp)
        .with_properties(vike_model::SymbolProperties { contract_size, ..Default::default() })
}

/// The KEPT row states the face only the row can (binance COIN-M, okx); without a row, or with a row that states no
/// size, there is no face; the venues that fix one unit and every linear name ignore the row.
#[test]
fn a_kept_row_states_the_face_only_its_row_can() {
    use super::kept_contract_value;
    let eth_swap = kept("okx", "ETH-USD-SWAP", 10.0);
    assert_eq!(contract_value("okx", "ETHUSD.P"), None, "no row: no face");
    assert_eq!(
        kept_contract_value("okx", "ETHUSD.P", Some(&eth_swap)),
        Some(ContractValue::inverse(10.0))
    );
    let coin_m = kept("binance", "ETHUSD_PERP.P", 10.0);
    assert_eq!(
        kept_contract_value("binance", "ETHUSD.P", Some(&coin_m)),
        Some(ContractValue::inverse(10.0))
    );
    for (venue, symbol) in [("okx", "BTCUSD.P"), ("binance", "BTCUSD.P"), ("okx", "ETHUSD.P")] {
        let no_size = kept(venue, symbol, 0.0);
        assert_eq!(
            kept_contract_value(venue, symbol, Some(&no_size)),
            None,
            "a row with no size: no face, {venue} {symbol}"
        );
        assert_eq!(kept_contract_value(venue, symbol, None), None, "{venue} {symbol}");
    }
    // deribit's row size is the LOT, not the unit; bybit's states none: both keep the venue's dollar.
    let lot = kept("deribit", "BTC-PERPETUAL", 10.0);
    assert_eq!(
        kept_contract_value("deribit", "BTCUSD.P", Some(&lot)),
        Some(ContractValue::inverse(1.0))
    );
    // A LINEAR row (its own margining says so, the authority over the name) keeps the default unit whatever its size.
    let mut linear = kept("okx", "BTC-USDT-SWAP", 0.01);
    if let vike_model::Contract::Perp(perp) = &mut linear.contract {
        perp.margining = vike_model::Margining::Linear;
    }
    assert_eq!(
        kept_contract_value("okx", "BTCUSDT.P", Some(&linear)),
        Some(ContractValue::default())
    );
}

/// **BYB-4 and DER-4: a KEPT row's margining decides whether a contract is inverse**, the name only without a row:
/// bybit's spot `USDTUSD` (and the six other spot rows whose names end in `USD`) is a linear unit of one, not a
/// one-dollar inverse contract, by its row and by decision 0290's user grammar alike (a bare pair is spot); a row
/// stating inverse is inverse; and no user name whose `_YYMMDD` the calendar refuses is a dated inverse future
/// (`ETHUSD_260231`).
#[test]
fn a_kept_rows_margining_decides_whether_a_contract_is_inverse() {
    use super::{kept_contract_value, kept_is_inverse_contract};
    for spot in ["USDTUSD", "BTCUSD", "ETHUSD", "BTCXUSD", "BTCRLUSD", "XRPRLUSD", "ETHRLUSD"] {
        let row =
            vike_model::Instrument::new("bybit", spot, "", "USD", vike_model::Contract::CryptoSpot);
        assert!(!is_inverse_contract("bybit", spot), "the user grammar reads {spot} as a pair");
        assert!(!kept_is_inverse_contract("bybit", spot, Some(&row)), "its row is spot: {spot}");
        assert_eq!(kept_contract_value("bybit", spot, Some(&row)), Some(ContractValue::default()));
    }
    let inverse = kept("bybit", "BTCUSD.P", 0.0);
    assert!(kept_is_inverse_contract("bybit", "BTCUSD.P", Some(&inverse)));
    assert_eq!(
        kept_contract_value("bybit", "BTCUSD.P", Some(&inverse)),
        Some(ContractValue::inverse(1.0))
    );
    assert!(kept_is_inverse_contract("bybit", "ETHUSD.P", None), "no row: the name");
    for name in ["ETHUSD_260231", "BTCUSD_260229", "BTCUSD_260100"] {
        assert!(!is_inverse_contract("deribit", name), "{name}");
    }
    assert!(is_inverse_contract("deribit", "BTCUSD_280229"));
}
