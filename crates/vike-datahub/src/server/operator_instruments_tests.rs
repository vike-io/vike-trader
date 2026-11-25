use super::*;

fn facts(tick: f64, quote: &str, class: &str, point_scale: f64) -> StatedInstrumentFacts {
    StatedInstrumentFacts {
        tick,
        quote: quote.into(),
        class: class.into(),
        point_scale,
        base: None,
        expiry: None,
    }
}

/// [`facts`] with the base its user name is spelled from, and a future's expiry.
fn named(class: &str, base: &str, quote: &str, expiry: Option<&str>) -> StatedInstrumentFacts {
    StatedInstrumentFacts {
        base: Some(base.into()),
        expiry: expiry.map(str::to_string),
        ..facts(0.01, quote, class, 100.0)
    }
}

/// **The IBKR rows an operator states** (decision 0290's O-10: IBKR lists nothing): a share, a forex pair and an
/// exchange future, each its wire name as the symbol with its class, base and quote, are rows the translator spells:
/// `AAPL`, `USDJPY` and `ES_261218`, each with the venue's own name as its wire. The exact line the runbook runs for
/// `AAPL` before accounts 12, 17 and 22 are renamed is `vike-cli data instruments add --venue ibkr AAPL.SMART.USD
/// --asset-class equity --base AAPL --quote USD --tick 0.01 --point-scale 100` (`docs/ops/operator-instruments.md`). A share
/// with no `--base` takes its symbol as its ticker (alpaca's `AAPL`).
#[test]
fn an_ibkr_stock_pair_and_future_are_rows_the_translator_spells() {
    for (wire, stated, user, class) in [
        (
            "AAPL.SMART.USD",
            named("equity", "AAPL", "USD", None),
            "AAPL",
            vike_model::AssetClass::Equity,
        ),
        (
            "BRK_B.SMART.USD",
            named("equity", "brk.b", "usd", None),
            "BRK.B",
            vike_model::AssetClass::Equity,
        ),
        ("USD.JPY.IDEALPRO", named("fx", "USD", "JPY", None), "USDJPY", vike_model::AssetClass::Fx),
        (
            "ES.CME.USD.FUT.20261218",
            named("future", "ES", "USD", Some("2026-12-18")),
            "ES_261218",
            vike_model::AssetClass::Future,
        ),
    ] {
        let row =
            vike_catalog::spelled(vec![stated_row("ibkr", wire, &stated).expect(wire)]).remove(0);
        assert_eq!(row.asset_class(), class, "{wire}");
        let names = vike_catalog::names_of_row(&row).expect(wire);
        assert_eq!((names.user.as_str(), names.wire.as_str()), (user, wire), "{wire}");
        assert_eq!(
            vike_catalog::kept_row_of_user_name(std::slice::from_ref(&row), "ibkr", user),
            Ok(&row)
        );
    }
    let share =
        stated_row("alpaca", "AAPL", &facts(0.01, "USD", "equity", 100.0)).expect("a share");
    assert_eq!(share.base, "AAPL", "a share with no --base: its symbol is its ticker");
    let future =
        StatedInstrumentFacts { base: None, ..named("future", "ES", "USD", Some("2026-12-18")) };
    for (bad, word) in [
        (named("equity", "", "USD", None), "--base"),
        (future, "--base"),
        (named("future", "ES", "USD", None), "--expiry"),
        (named("equity", "AAPL", "USD", Some("2026-12-18")), "--expiry"),
        (named("future", "ES", "USD", Some("2026-02-31")), "--expiry"),
        (named("equity", "AAPL USD", "USD", None), "--base"),
    ] {
        let why = stated_row("ibkr", "X", &bad).expect_err(word);
        assert!(why.contains(word), "{word}: {why}");
    }
}

/// Every stated fact is checked and none is defaulted: the tick, the point value that must be its reciprocal, the
/// quote and the class.
#[test]
fn a_stated_row_holds_its_facts_or_is_refused() {
    let row = stated_row("dukascopy", "AAPL.US/USD", &facts(0.001, "usd", "Share", 1000.0))
        .expect("a row");
    assert_eq!(
        (row.venue.as_str(), row.raw_symbol.as_str(), row.quote.as_str()),
        ("dukascopy", "AAPL.US/USD", "USD")
    );
    assert_eq!(row.properties.tick_size, 0.001);
    assert!(matches!(
        row.contract,
        Contract::Cfd(ContractCfd {
            underlying_kind: ContractCfdUnderlying::Share,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Rolling,
            ..
        })
    ));
    let pair = stated_row("dukascopy", "EURHUF", &facts(0.001, "HUF", "fx", 1000.0)).expect("fx");
    assert_eq!(pair.asset_class(), vike_model::AssetClass::Fx);

    for (bad, word) in [
        (facts(0.0, "USD", "share", 1000.0), "--tick"),
        (facts(f64::NAN, "USD", "share", 1000.0), "--tick"),
        (facts(0.001, "USD", "share", 100.0), "--point-scale"),
        (facts(0.001, "USD", "share", 0.0), "--point-scale"),
        (facts(0.001, "US", "share", 1000.0), "--quote"),
        (facts(0.001, "  ", "share", 1000.0), "--quote"),
        (facts(0.001, "USD1", "share", 1000.0), "--quote"),
        (facts(0.001, "USD", "", 1000.0), "--asset-class"),
        (facts(0.001, "USD", "stock", 1000.0), "--asset-class"),
        (facts(0.001, "USDT", "equity", 1000.0), "--quote"),
    ] {
        let why = stated_row("dukascopy", "X", &bad).expect_err(word);
        assert!(why.contains(word), "{why}");
    }
    // The class refusal lists every word the door takes, the CFD underlyings but the shadowed `fx` among them.
    let why =
        stated_row("dukascopy", "X", &facts(0.001, "USD", "stock", 1000.0)).expect_err("stock");
    assert!(
        why.ends_with(
            "is not one of fx, equity, future, crypto_spot, or a CFD's index, metal, commodity, share, crypto, \
             bond, other"
        ),
        "{why}"
    );
    // A CFD's underlying word, any case, is a rolling CFD of that underlying.
    let index = stated_row("dukascopy", "X", &facts(0.001, "USD", "Index", 1000.0)).expect("index");
    assert!(
        matches!(&index.contract, vike_model::Contract::Cfd(cfd)
            if cfd.underlying_kind == vike_model::ContractCfdUnderlying::Index),
        "{index:?}"
    );
}

/// **One instrument, one row** (DUK-8, respelled to decision 0290): a row is the instrument of a kept row when the two
/// have one USER name, or on dukascopy one `.bi5` dataset; a different instrument, and another venue's dataset-alike
/// names, are not. Two of IG's epics of one pair, and dukascopy's two listings of one share CFD, have ONE user name, so
/// they are one instrument here as they are to every reader (decision 0290: two rows for one name are refused).
#[test]
fn another_spelling_of_a_kept_row_is_the_same_instrument() {
    use vike_model::{
        Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying,
        ContractFx,
    };
    let pair = |venue: &str, raw: &str, base: &str, quote: &str| {
        Instrument::new(venue, raw, base, quote, Contract::Fx(ContractFx::default()))
    };
    let share = |venue: &str, raw: &str| {
        let cfd = Contract::Cfd(ContractCfd {
            underlying_kind: ContractCfdUnderlying::Share,
            underlying: None,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Rolling,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        });
        Instrument::new(venue, raw, "", "USD", cfd)
    };
    // The kept row stores its names (the list's writer spelled them); the added row is spelled once by the add.
    let same = |venue: &str, kept: &Instrument, added: &Instrument| {
        let kept = &vike_catalog::spelled(vec![kept.clone()])[0];
        let added_user =
            vike_catalog::user_name_of_row(&vike_catalog::spelled(vec![added.clone()])[0]).ok();
        same_instrument(venue, kept, added, added_user.as_deref())
    };
    for (venue, a, b) in [
        (
            "dukascopy",
            pair("dukascopy", "EURUSD", "EUR", "USD"),
            pair("dukascopy", "EUR/USD", "EUR", "USD"),
        ),
        ("dukascopy", share("dukascopy", "AAPL.US/USD"), share("dukascopy", "AAPLUSUSD")),
        ("dukascopy", share("dukascopy", "AAPL.US/USD"), share("dukascopy", "AAPL.DE/EUR")),
        ("fxcm", pair("fxcm", "EUR/USD", "EUR", "USD"), pair("fxcm", "EURUSD", "EUR", "USD")),
        (
            "ig",
            pair("ig", "CS.D.EURUSD.MINI.IP", "EUR", "USD"),
            pair("ig", "cs.d.eurusd.mini.ip", "EUR", "USD"),
        ),
        (
            "ig",
            pair("ig", "CS.D.EURUSD.MINI.IP", "EUR", "USD"),
            pair("ig", "CS.D.EURUSD.CFD.IP", "EUR", "USD"),
        ),
    ] {
        assert!(same(venue, &a, &b), "{venue}: {} and {}", a.raw_symbol, b.raw_symbol);
    }
    for (venue, a, b) in [
        (
            "dukascopy",
            pair("dukascopy", "EURUSD", "EUR", "USD"),
            pair("dukascopy", "GBPUSD", "GBP", "USD"),
        ),
        ("ig", share("ig", "AAPL.US/USD"), share("ig", "AAPLUSUSD")),
    ] {
        assert!(!same(venue, &a, &b), "{venue}: {} and {}", a.raw_symbol, b.raw_symbol);
    }
}

/// **A share and a coin of a venue with no list are rows of their own class** (alpaca, decision 0290's O-10), each
/// stating what the translator spells its user name from: the share `AAPL` is `AAPL`, the venue's pair `BTC/USD` is
/// the coin `BTCUSD` (its wire name the pair), a token quote `BTC/USDT` is `BTCUSDT`. A coin not written as the
/// venue's `BASE/QUOTE` pair of its stated quote is refused.
#[test]
fn a_stated_share_and_coin_are_named_by_their_class() {
    let share =
        stated_row("alpaca", "AAPL", &facts(0.01, "USD", "equity", 100.0)).expect("a share");
    assert_eq!(share.asset_class(), vike_model::AssetClass::Equity);
    let coin =
        stated_row("alpaca", "BTC/USD", &facts(0.01, "usd", "Crypto_Spot", 100.0)).expect("a coin");
    assert_eq!(coin.asset_class(), vike_model::AssetClass::CryptoSpot);
    let names = |row: &Instrument| {
        let spelled = vike_catalog::spelled(vec![row.clone()]);
        let names = vike_catalog::names_of_row(&spelled[0]).expect("a user name");
        (names.user.clone(), names.wire.clone())
    };
    assert_eq!(names(&share), ("AAPL".to_string(), "AAPL".to_string()));
    assert_eq!(names(&coin), ("BTCUSD".to_string(), "BTC/USD".to_string()));
    let tether = stated_row("alpaca", "BTC/USDT", &facts(0.01, "USDT", "crypto_spot", 100.0))
        .expect("a token's quote");
    assert_eq!(names(&tether).0, "BTCUSDT");
    for (symbol, quote) in [("BTCUSD", "USD"), ("BTC/USD", "EUR"), ("/USD", "USD")] {
        let why = stated_row("alpaca", symbol, &facts(0.01, quote, "crypto_spot", 100.0))
            .expect_err(symbol);
        assert!(why.contains("BASE/"), "{symbol}: {why}");
    }
}

/// The words a symbol may be: the venues' own spellings (`CS.D.EURUSD.MINI.IP`, `COPPER.CMD/USD`), nothing with a
/// space, a quote or a control byte, and nothing past the bound.
#[test]
fn a_symbol_is_bounded_at_the_door() {
    for good in ["EURUSD", "CS.D.EURUSD.MINI.IP", "COPPER.CMD/USD", "BRK_B", "ES-Z26"] {
        assert_eq!(symbol_refusal(good), None, "{good}");
    }
    let long = "X".repeat(SYMBOL_MAX_BYTES + 1);
    for bad in ["", "EUR USD", "EUR\"USD", "EUR\nUSD", long.as_str()] {
        assert!(symbol_refusal(bad).is_some(), "{bad:?}");
    }
}
