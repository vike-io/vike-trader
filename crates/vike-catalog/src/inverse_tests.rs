use super::{contract_value, is_inverse_contract, option_underlyings};
use vike_model::ContractValue;

#[test]
fn the_inverse_names_of_each_venue() {
    for (venue, symbol) in [
        ("deribit", "BTC-PERPETUAL"),
        ("deribit", "ETH-PERPETUAL"),
        ("deribit", "BTC-9OCT26"),
        ("deribit", "BTC-27DEC26"),
        ("bybit", "BTCUSD"),
        ("bybit", "BTCUSD.P"),
        ("bybit", "HYPEUSD"),
        ("bybit", "BTCUSDZ26"),
        ("bybit", "ETHUSDH27"),
        ("binance", "BTCUSD_PERP"),
        ("binance", "BTCUSD_PERP.P"),
        ("binance", "ETHUSD_PERP.P"),
        ("binance", "BTCUSD_261225"),
        ("binance", "BTCUSD_261225.P"),
        ("okx", "BTC-USD-SWAP"),
        ("okx", "BTC-USD-261030"),
        // A typed lower case names the same contract.
        ("deribit", "btc-perpetual"),
    ] {
        assert!(is_inverse_contract(venue, symbol), "{venue} {symbol}");
    }
}

#[test]
fn the_linear_names_of_each_venue() {
    for (venue, symbol) in [
        ("deribit", "BTC_USDC-PERPETUAL"),
        ("deribit", "SOL_USDC-PERPETUAL"),
        ("deribit", "BTC_USDC-27DEC26"),
        ("deribit", "BTC_USDC"),
        ("deribit", "BTC-27DEC26-60000-C"),
        ("deribit", "SOL_USDC-27DEC26-150-P"),
        ("deribit", ""),
        ("bybit", "BTCUSDT"),
        ("bybit", "BTCUSDT.P"),
        ("bybit", "BTCUSDC"),
        ("bybit", "BTCPERP"),
        ("bybit", "ETHUSDT-27NOV26"),
        ("bybit", "BTC-27DEC26"),
        ("bybit", "DOGEUSDT.S"),
        ("bybit", "USD"),
        ("bybit", "UZ2"),
        ("bybit", ""),
        ("binance", "BTCUSDT"),
        ("binance", "BTCUSDT.P"),
        ("binance", "BTCUSDT_261225.P"),
        ("binance", "BTCUSDC.P"),
        ("binance", "USD_PERP"),
        ("okx", "BTC-USDT-SWAP"),
        ("okx", "BTC-USDC-SWAP"),
        ("okx", "BTC-USDT-261225"),
        ("okx", "BTC-USD_UM-261127"),
        ("okx", "BTC-USD"),
        ("okx", "BTC-USD-261012-75000-C"),
    ] {
        assert!(!is_inverse_contract(venue, symbol), "{venue} {symbol}");
    }
}

/// One name, two venues: the reason the venue is an argument.
#[test]
fn the_same_name_can_be_inverse_on_one_venue_and_linear_on_another() {
    assert!(is_inverse_contract("bybit", "BTCUSD"));
    for venue in ["ctrader", "oanda", "ig", "ibkr", "binance", "okx", "deribit"] {
        assert!(!is_inverse_contract(venue, "BTCUSD"), "{venue}");
    }
    assert!(is_inverse_contract("deribit", "BTC-27DEC26"));
    assert!(!is_inverse_contract("bybit", "BTC-27DEC26"));
}

/// One dollar a unit where the venue fixes it (deribit, bybit), whatever the symbol; where only the row
/// states it (binance COIN-M `contractSize`, okx `ctVal`), a committed row's face or no answer; and none
/// where the venue lists no inverse contract.
#[test]
fn the_face_value_is_the_venues_or_a_committed_rows() {
    assert_eq!(super::inverse_face_value("deribit", "BTC-PERPETUAL"), Some(1.0));
    assert_eq!(super::inverse_face_value("bybit", "ADAUSD"), Some(1.0));
    assert_eq!(super::inverse_face_value("binance", "ETHUSD_PERP.P"), Some(10.0));
    assert_eq!(super::inverse_face_value("okx", "BTC-USD-SWAP"), Some(100.0));
    for (venue, symbol) in [
        ("binance", "SOLUSD_PERP.P"),
        ("okx", "ETH-USD-SWAP"),
        ("hyperliquid", "BTC"),
        ("oanda", "BTCUSD_PERP"),
        ("", "BTC-USD-SWAP"),
    ] {
        assert_eq!(super::inverse_face_value(venue, symbol), None, "{venue} {symbol}");
    }
}

/// Every committed face names a contract the name rule reads as inverse on its venue, once, at a
/// positive face: a row the rule misread would never be looked up, and a duplicate could disagree.
#[test]
fn every_committed_face_is_an_inverse_name_once() {
    for (venue, faces) in
        [("binance", super::BINANCE_COIN_M_FACES), ("okx", super::OKX_INVERSE_FACES)]
    {
        for (i, &(name, face)) in faces.iter().enumerate() {
            assert!(is_inverse_contract(venue, name), "{venue} {name}");
            assert!(face.is_finite() && face > 0.0, "{venue} {name}: {face}");
            assert_eq!(name, name.to_ascii_uppercase(), "{venue} {name}: the venue's spelling");
            assert!(faces[..i].iter().all(|(n, _)| *n != name), "{venue} {name} twice");
        }
    }
}

/// The whole value of an inverse name: the face where the venue fixes it (deribit, bybit), the face a
/// COMMITTED row states where only the row does (binance COIN-M `contractSize`, okx `ctVal`), and no
/// answer for a name no committed row states, never the common 100.
#[test]
fn an_inverse_name_is_valued_at_its_face_or_not_at_all() {
    assert_eq!(contract_value("deribit", "BTC-PERPETUAL"), Some(ContractValue::inverse(1.0)));
    assert_eq!(contract_value("deribit", "BTC-27DEC26"), Some(ContractValue::inverse(1.0)));
    assert_eq!(contract_value("bybit", "BTCUSD.P"), Some(ContractValue::inverse(1.0)));
    for (venue, symbol, face) in [
        ("binance", "BTCUSD_PERP.P", 100.0),
        ("binance", "btcusd_perp", 100.0),
        ("binance", "ETHUSD_PERP.P", 10.0),
        ("binance", "BTCUSD_261225.P", 100.0),
        ("okx", "BTC-USD-SWAP", 100.0),
        ("okx", "btc-usd-261030", 100.0),
    ] {
        let want = Some(ContractValue::inverse(face));
        assert_eq!(contract_value(venue, symbol), want, "{venue} {symbol}");
    }
    for (venue, symbol) in [
        ("binance", "SOLUSD_PERP.P"),
        ("binance", "ETHUSD_261225.P"),
        ("okx", "ETH-USD-SWAP"),
        ("okx", "BTC-USD-261225"),
    ] {
        assert_eq!(contract_value(venue, symbol), None, "no committed row: {venue} {symbol}");
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
    assert_eq!(contract_value("deribit", "BTC_USDC-PERPETUAL"), Some(ContractValue::default()));
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

/// A deribit USDC option is written on its book's perpetual and spot pair; no other name names an underlying.
#[test]
fn a_deribit_usdc_option_names_its_underlyings_and_nothing_else_does() {
    assert_eq!(
        option_underlyings("deribit", "BTC_USDC-27NOV26-80000-C"),
        Some(["BTC_USDC-PERPETUAL".to_string(), "BTC_USDC".to_string()])
    );
    assert_eq!(
        option_underlyings("deribit", "sol_usdc-12oct26-96-p"),
        Some(["SOL_USDC-PERPETUAL".to_string(), "SOL_USDC".to_string()])
    );
    for none in ["BTC-27DEC26-60000-C", "BTC_USDC-PERPETUAL", "BTC_USDC", "BTC-27DEC26"] {
        assert_eq!(option_underlyings("deribit", none), None, "{none}");
    }
    assert_eq!(option_underlyings("okx", "BTC_USDC-27NOV26-80000-C"), None);
}

/// The ibkr and fxcm shapes whose name fixes one unit, and the ones whose unit only the row states.
#[test]
fn the_ibkr_and_fxcm_names_that_fix_one_unit() {
    for one in [
        "AAPL.SMART.USD",
        "BRK_B.SMART.USD",
        "AAPL.SMART.USD.STK.265598",
        "VOD.LSE.GBP.STK",
        "EUR.USD.IDEALPRO",
        "EUR.IDEALPRO.USD",
        "eur.idealpro.usd.cash",
        "BTC.PAXOS.USD.CRYPTO",
    ] {
        assert_eq!(contract_value("ibkr", one), Some(ContractValue::default()), "{one}");
    }
    for row_only in [
        "ES.CME.USD.FUT.20261218",
        "MES.CME.USD.FUT.20261218.5",
        "SPY.SMART.USD.OPT.20261218.500.C",
        "SPX.CBOE.USD.IND",
        "ES.GLOBEX.USD",
        "AAPL.NASDAQ.USD",
        "ES",
        "",
    ] {
        assert_eq!(contract_value("ibkr", row_only), None, "{row_only}");
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

/// The settle coin of an inverse name is its base coin on every venue's grammar; a linear name (or a venue that
/// lists no inverse contract) has none.
#[test]
fn an_inverse_name_settles_in_its_base_coin() {
    for (venue, symbol, coin) in [
        ("deribit", "BTC-PERPETUAL", "BTC"),
        ("deribit", "eth-27dec26", "ETH"),
        ("bybit", "BTCUSD", "BTC"),
        ("bybit", "ETHUSD.P", "ETH"),
        ("bybit", "BTCUSDZ26", "BTC"),
        ("bybit", "HYPEUSD", "HYPE"),
        ("binance", "BTCUSD_PERP.P", "BTC"),
        ("binance", "ETHUSD_261225", "ETH"),
        ("okx", "BTC-USD-SWAP", "BTC"),
        ("okx", "ETH-USD-261225", "ETH"),
    ] {
        let got = super::inverse_settle_coin(venue, symbol);
        assert_eq!(got.as_deref(), Some(coin), "{venue} {symbol}");
    }
    for (venue, symbol) in [
        ("deribit", "BTC_USDC-PERPETUAL"),
        ("bybit", "BTCUSDT"),
        ("binance", "BTCUSDT.P"),
        ("okx", "BTC-USDT-SWAP"),
        ("ctrader", "BTCUSD"),
    ] {
        assert_eq!(super::inverse_settle_coin(venue, symbol), None, "{venue} {symbol}");
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
/// size, the answer is the name's; the venues that fix one unit and every linear name ignore the row.
#[test]
fn a_kept_row_states_the_face_only_its_row_can() {
    use super::kept_contract_value;
    let eth_swap = kept("okx", "ETH-USD-SWAP", 10.0);
    assert_eq!(contract_value("okx", "ETH-USD-SWAP"), None, "no committed row: no face");
    assert_eq!(
        kept_contract_value("okx", "ETH-USD-SWAP", Some(&eth_swap)),
        Some(ContractValue::inverse(10.0))
    );
    let coin_m = kept("binance", "ETHUSD_PERP.P", 10.0);
    assert_eq!(
        kept_contract_value("binance", "ETHUSD_PERP.P", Some(&coin_m)),
        Some(ContractValue::inverse(10.0))
    );
    for (venue, symbol) in
        [("okx", "BTC-USD-SWAP"), ("binance", "BTCUSD_PERP.P"), ("okx", "ETH-USD-SWAP")]
    {
        let no_size = kept(venue, symbol, 0.0);
        assert_eq!(
            kept_contract_value(venue, symbol, Some(&no_size)),
            contract_value(venue, symbol),
            "a row with no size: the name's answer, {venue} {symbol}"
        );
        assert_eq!(kept_contract_value(venue, symbol, None), contract_value(venue, symbol));
    }
    // deribit's row size is the LOT, not the unit; bybit's states none: both keep the venue's dollar.
    let lot = kept("deribit", "BTC-PERPETUAL", 10.0);
    assert_eq!(
        kept_contract_value("deribit", "BTC-PERPETUAL", Some(&lot)),
        Some(ContractValue::inverse(1.0))
    );
    let linear = kept("okx", "BTC-USDT-SWAP", 0.01);
    assert_eq!(
        kept_contract_value("okx", "BTC-USDT-SWAP", Some(&linear)),
        Some(ContractValue::default())
    );
}
