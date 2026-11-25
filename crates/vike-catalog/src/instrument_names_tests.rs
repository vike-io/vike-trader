//! Decision 0290's translator, venue by venue and class by class: each kept row's user, store and wire names, and
//! the round trip back to the row from the user name and from the store name.

use vike_model::{
    AssetClass, Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct,
    ContractCfdUnderlying, ContractFuture, ContractFutureDelivery, ContractFutureFamily,
    Instrument, InstrumentVenueIds, Margining,
};

use super::*;
use crate::test_support::test_row;

/// A row of `class` (`crate::test_support::contract_for`'s contract).
fn row(venue: &str, raw: &str, base: &str, quote: &str, class: AssetClass) -> Instrument {
    test_row(venue, raw, base, quote, class)
}

/// A CFD row on an underlying of `kind`.
fn cfd(venue: &str, raw: &str, base: &str, quote: &str, kind: ContractCfdUnderlying) -> Instrument {
    Instrument::new(
        venue,
        raw,
        base,
        quote,
        Contract::Cfd(ContractCfd {
            underlying_kind: kind,
            underlying: None,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Unknown,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        }),
    )
}

/// A dated future of `family` expiring at 08:00 UTC on `(year, month, day)`.
fn future(
    venue: &str,
    raw: &str,
    base: &str,
    quote: &str,
    family: ContractFutureFamily,
    (year, month, day): (i64, u32, u32),
) -> Instrument {
    let expiry_ms =
        vike_utility::calendar::days_from_civil(year, month, day) * 86_400_000 + 8 * 3_600_000;
    Instrument::new(
        venue,
        raw,
        base,
        quote,
        Contract::Future(ContractFuture {
            margining: Margining::Linear,
            settle: quote.into(),
            value_currency: None,
            expiry_ms,
            tenor: None,
            delivery: ContractFutureDelivery::Unknown,
            family,
        }),
    )
}

/// `row` carrying hyperliquid's coin beside its name.
fn coin(row: Instrument, coin: &str) -> Instrument {
    row.with_wire(InstrumentVenueIds { coin: Some(coin.into()), ..InstrumentVenueIds::default() })
}

/// The table: each kept row and its (user, store, wire) names.
fn table() -> Vec<(Instrument, [&'static str; 3])> {
    use AssetClass::{
        CryptoPerp as Perp, CryptoSpot as Spot, Equity, Fx, Option, PredictionMarket,
    };
    use ContractCfdUnderlying::{Commodity, Crypto, Index, Metal, Share};
    use ContractFutureFamily::{Crypto as CryptoDated, Traditional};
    let token = "67102598924726349989742049657609063097299442010642912942032681022413402597017";
    vec![
        (row("binance", "BTCUSDT", "BTC", "USDT", Spot), ["BTCUSDT", "BTCUSDT", "BTCUSDT"]),
        (row("binance", "BTCUSDT.P", "BTC", "USDT", Perp), ["BTCUSDT.P", "BTCUSDT.P", "BTCUSDT"]),
        (
            row("binance", "BTCUSD_PERP.P", "BTC", "USD", Perp),
            ["BTCUSD.P", "BTCUSD.P", "BTCUSD_PERP"],
        ),
        (
            future("binance", "BTCUSDT_261225.P", "BTC", "USDT", CryptoDated, (2026, 12, 25)),
            ["BTCUSDT_261225", "BTCUSDT_261225", "BTCUSDT_261225"],
        ),
        (row("aster", "BTCUSD1.P", "BTC", "USD1", Perp), ["BTCUSD1.P", "BTCUSD1.P", "BTCUSD1"]),
        (row("bybit", "BTCUSDT", "BTC", "USDT", Spot), ["BTCUSDT", "BTCUSDT", "BTCUSDT"]),
        (row("bybit", "BTCUSDT.P", "BTC", "USDT", Perp), ["BTCUSDT.P", "BTCUSDT.P", "BTCUSDT"]),
        (row("bybit", "BTCPERP.P", "BTC", "USDC", Perp), ["BTCUSDC.P", "BTCUSDC.P", "BTCPERP"]),
        (row("bybit", "BTCUSD.P", "BTC", "USD", Perp), ["BTCUSD.P", "BTCUSD.P", "BTCUSD"]),
        (
            future("bybit", "ETHUSDT-06NOV26", "ETH", "USDT", CryptoDated, (2026, 11, 6)),
            ["ETHUSDT_261106", "ETHUSDT_261106", "ETHUSDT-06NOV26"],
        ),
        (
            future("bybit", "BTCUSDZ26", "BTC", "USD", CryptoDated, (2026, 12, 25)),
            ["BTCUSD_261225", "BTCUSD_261225", "BTCUSDZ26"],
        ),
        (row("okx", "BTC-USDT", "BTC", "USDT", Spot), ["BTCUSDT", "BTCUSDT", "BTC-USDT"]),
        (
            row("okx", "BTC-USDT-SWAP", "BTC", "USDT", Perp),
            ["BTCUSDT.P", "BTCUSDT.P", "BTC-USDT-SWAP"],
        ),
        (
            future("okx", "BTC-USDT-261225", "BTC", "USDT", CryptoDated, (2026, 12, 25)),
            ["BTCUSDT_261225", "BTCUSDT_261225", "BTC-USDT-261225"],
        ),
        (
            row("deribit", "BTC-PERPETUAL", "BTC", "USD", Perp),
            ["BTCUSD.P", "BTCUSD.P", "BTC-PERPETUAL"],
        ),
        (
            row("deribit", "BTC_USDC-PERPETUAL", "BTC", "USDC", Perp),
            ["BTCUSDC.P", "BTCUSDC.P", "BTC_USDC-PERPETUAL"],
        ),
        (row("deribit", "BTC_USDC", "BTC", "USDC", Spot), ["BTCUSDC", "BTCUSDC", "BTC_USDC"]),
        (
            future("deribit", "BTC-3NOV26", "BTC", "USD", CryptoDated, (2026, 11, 3)),
            ["BTCUSD_261103", "BTCUSD_261103", "BTC-3NOV26"],
        ),
        (
            // deribit's own list upper-cases a decimal strike; the venue and the door spell it `2d5`.
            row("deribit", "XRP_USDC-27MAR26-2D5-C", "XRP", "USDC", Option),
            ["XRP_USDC-27MAR26-2d5-C", "XRP_USDC-27MAR26-2d5-C", "XRP_USDC-27MAR26-2d5-C"],
        ),
        (
            coin(row("hyperliquid", "BTC", "BTC", "USDC", Perp), "BTC"),
            ["BTCUSDC.P", "BTCUSDC.P", "BTC"],
        ),
        (
            coin(row("hyperliquid", "kPEPE", "kPEPE", "USDC", Perp), "kPEPE"),
            ["KPEPEUSDC.P", "KPEPEUSDC.P", "kPEPE"],
        ),
        (
            coin(row("hyperliquid", "HYPE/USDC", "HYPE", "USDC", Spot), "@107"),
            ["HYPEUSDC", "HYPEUSDC", "@107"],
        ),
        (
            coin(row("hyperliquid", "xyz:TSLA", "TSLA", "USDC", Perp), "xyz:TSLA"),
            ["XYZ:TSLAUSDC.P", "XYZ-TSLAUSDC.P", "xyz:TSLA"],
        ),
        (
            coin(row("hyperliquid", "cash:TSLA", "TSLA", "USDT0", Perp), "cash:TSLA"),
            ["CASH:TSLAUSDT0.P", "CASH-TSLAUSDT0.P", "cash:TSLA"],
        ),
        (
            coin(row("hyperliquid", "flx:TSLA", "TSLA", "USDH", Perp), "flx:TSLA"),
            ["FLX:TSLAUSDH.P", "FLX-TSLAUSDH.P", "flx:TSLA"],
        ),
        (row("alpaca", "AAPL", "AAPL", "USD", Equity), ["AAPL", "AAPL", "AAPL"]),
        (row("alpaca", "BRK.B", "BRK.B", "USD", Equity), ["BRK.B", "BRK.B", "BRK.B"]),
        (row("alpaca", "BTC/USD", "BTC", "USD", Spot), ["BTCUSD", "BTCUSD", "BTC/USD"]),
        (row("ibkr", "AAPL.SMART.USD", "AAPL", "USD", Equity), ["AAPL", "AAPL", "AAPL.SMART.USD"]),
        (
            row("ibkr", "USD/JPY/CASH/IDEALPRO", "USD", "JPY", Fx),
            ["USDJPY", "USDJPY", "USD/JPY/CASH/IDEALPRO"],
        ),
        (
            future("ibkr", "ES.CME.USD.FUT.20261218", "ES", "USD", Traditional, (2026, 12, 18)),
            ["ES_261218", "ES_261218", "ES.CME.USD.FUT.20261218"],
        ),
        (
            row("ig", "CS.D.USDJPY.MINI.IP", "USD", "JPY", Fx),
            ["USDJPY", "USDJPY", "CS.D.USDJPY.MINI.IP"],
        ),
        (
            cfd("ig", "IX.D.DAX.IFD.IP", "DE40", "EUR", Index),
            ["DE40.CFD", "DE40.CFD", "IX.D.DAX.IFD.IP"],
        ),
        (row("oanda", "USD_JPY", "USD", "JPY", Fx), ["USDJPY", "USDJPY", "USD_JPY"]),
        (cfd("oanda", "XAU_USD", "XAU", "USD", Metal), ["XAUUSD", "XAUUSD", "XAU_USD"]),
        (
            cfd("oanda", "SPX500_USD", "US500", "USD", Index),
            ["US500.CFD", "US500.CFD", "SPX500_USD"],
        ),
        (row("fxcm", "USD/JPY", "USD", "JPY", Fx), ["USDJPY", "USDJPY", "USD/JPY"]),
        (cfd("fxcm", "XAU/USD", "XAU", "USD", Metal), ["XAUUSD", "XAUUSD", "XAU/USD"]),
        (row("dukascopy", "USDJPY", "USD", "JPY", Fx), ["USDJPY", "USDJPY", "USDJPY"]),
        (cfd("dukascopy", "XAUUSD", "XAU", "USD", Metal), ["XAUUSD", "XAUUSD", "XAUUSD"]),
        (
            cfd("dukascopy", "AAPL.US/USD", "", "USD", Share),
            ["AAPL.CFD", "AAPL.CFD", "AAPL.US/USD"],
        ),
        (
            cfd("dukascopy", "BRKB.US/USD", "", "USD", Share),
            ["BRKB.CFD", "BRKB.CFD", "BRKB.US/USD"],
        ),
        (row("ctrader", "EURUSD", "EUR", "USD", Fx), ["EURUSD", "EURUSD", "EURUSD"]),
        (cfd("ctrader", "US500", "US500", "USD", Index), ["US500.CFD", "US500.CFD", "US500"]),
        (cfd("ctrader", "XTIUSD", "WTI", "USD", Commodity), ["WTI.CFD", "WTI.CFD", "XTIUSD"]),
        (cfd("ctrader", "BTCUSD", "BTC", "USD", Crypto), ["BTCUSD.CFD", "BTCUSD.CFD", "BTCUSD"]),
        (row("polymarket", token, "Yes", "USDC", PredictionMarket), [token, token, token]),
    ]
}

/// **Every venue and class: a row's three names, and both lookups find that row again** (the user name in any
/// case, the store name exactly).
#[test]
fn each_kept_row_has_its_three_names_and_both_names_find_it_again() {
    let table = table();
    let rows = spelled(table.iter().map(|(r, _)| r.clone()).collect());
    for (at, (row, [user, store, wire])) in table.iter().enumerate() {
        let id = format!("{} `{}`", row.venue, row.raw_symbol);
        let names = names_of_row(&rows[at]).unwrap_or_else(|e| panic!("{id}: {e}"));
        let stored = vike_paths::store_path::store_name_of_user_name(&names.user);
        assert_eq!(
            (names.user.as_str(), stored.as_str(), names.wire.as_str()),
            (*user, *store, *wire),
            "{id}"
        );
        let found =
            kept_row_of_user_name(&rows, &row.venue, user).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert!(std::ptr::eq(found, &rows[at]), "{id}: the user name finds another row");
        let lower = user.to_ascii_lowercase();
        let found = kept_row_of_user_name(&rows, &row.venue, &format!(" {lower} "))
            .unwrap_or_else(|e| panic!("{id} in lower case: {e}"));
        assert!(std::ptr::eq(found, &rows[at]), "{id}: the lower-case user name finds another row");
        let found = kept_row_of_store_name(&rows, &row.venue, store)
            .unwrap_or_else(|e| panic!("{id}: {e}"));
        assert!(std::ptr::eq(found, &rows[at]), "{id}: the store name finds another row");
        assert_eq!(names_of_user_name(&rows, &row.venue, &lower), Ok(names.clone()), "{id}");
        assert_eq!(names_of_store_name(&rows, &row.venue, store), Ok(names.clone()), "{id}");
        // The separators `vike_paths::store_path::refuse_a_path_hostile_symbol` refuses first (a path level, a drive).
        assert!(
            !store.contains(['/', '\\', ':']),
            "{id}: the store name `{store}` is no path the store may hold"
        );
    }
}

/// **A name is never read for an instrument without its row**: no list, no row, two rows for one name, and the
/// old spellings decision 0290 retired are each refused in words.
#[test]
fn no_list_no_row_or_two_rows_is_a_refusal_and_an_old_spelling_names_nothing() {
    let rows = spelled(table().into_iter().map(|(r, _)| r).collect());
    let refused = |venue: &str, name: &str| {
        kept_row_of_user_name(&rows, venue, name)
            .expect_err(&format!("{venue} `{name}` is refused"))
    };
    assert!(refused("aster", "ETHUSDT.P").contains("carries no row"));
    let none: Vec<Instrument> = Vec::new();
    let why = kept_row_of_user_name(&none, "oanda", "EURUSD").expect_err("no list");
    assert!(why.contains(&crate::no_kept_list_refusal("oanda")), "the fix is named: {why}");
    for (venue, old) in [
        ("oanda", "USD_JPY"),
        ("dukascopy", "AAPL.US/USD"),
        ("dukascopy", "AAPLUSUSD"),
        ("bybit", "BTCUSDT.S"),
        ("deribit", "BTC-PERPETUAL"),
        ("okx", "BTC-USDT-SWAP"),
        ("ig", "CS.D.USDJPY.MINI.IP"),
        ("ibkr", "AAPL.SMART.USD"),
        ("hyperliquid", "BTC"),
        ("hyperliquid", "HYPE-USDC"),
    ] {
        assert!(refused(venue, old).contains("carries no row"), "{venue} `{old}`");
    }
    // bybit's bare `BTCUSDT` is the SPOT pair now (it was the perpetual's exec name): the one old spelling that
    // still names a row, and another one.
    let spot = kept_row_of_user_name(&rows, "bybit", "BTCUSDT").expect("the spot pair");
    assert_eq!(spot.asset_class(), AssetClass::CryptoSpot);

    let twice = spelled(vec![
        coin(row("hyperliquid", "kPEPE", "kPEPE", "USDC", AssetClass::CryptoPerp), "kPEPE"),
        coin(row("hyperliquid", "KPEPE", "KPEPE", "USDC", AssetClass::CryptoPerp), "KPEPE"),
        future("okx", "BTC-USD-261127", "BTC", "USD", ContractFutureFamily::Crypto, (2026, 11, 27)),
        future(
            "okx",
            "BTC-USD_UM-261127",
            "BTC",
            "USD",
            ContractFutureFamily::Crypto,
            (2026, 11, 27),
        ),
    ]);
    for (venue, name) in [("hyperliquid", "KPEPEUSDC.P"), ("okx", "BTCUSD_261127")] {
        let why = kept_row_of_user_name(&twice, venue, name).expect_err("two rows");
        assert!(why.contains("names two"), "{venue} `{name}`: {why}");
        let why = kept_row_of_store_name(&twice, venue, name).expect_err("two rows");
        assert!(why.contains("names two"), "{venue} `{name}`: {why}");
    }
}

/// **A row whose class has no grammar, or that does not state what its grammar spells, has no user name**: an
/// option off deribit, deribit's coin-settled option (its door refuses it), an index CFD stating no base, an index
/// itself, a future with no expiry; and a lookup simply skips such a row.
#[test]
fn a_row_its_grammar_cannot_spell_has_no_user_name() {
    let refusals = [
        row("okx", "SOL-USD_UM-261113-70-C", "SOL", "USD", AssetClass::Option),
        row("deribit", "BTC-27MAR26-90000-C", "BTC", "USD", AssetClass::Option),
        cfd("ig", "IX.D.SPTRD.IFMM.IP", "", "USD", ContractCfdUnderlying::Index),
        row("ig", "IX.D.FTSE.IFD.IP", "FTSE", "GBP", AssetClass::Index),
        row("binance", "BTCUSDT_000000.P", "BTC", "USDT", AssetClass::CryptoFuture),
    ];
    let mut rows = spelled(refusals.to_vec());
    for refused in &rows {
        let id = format!("{} `{}`", refused.venue, refused.raw_symbol);
        assert!(matches!(refused.naming, Some(InstrumentNaming::Unnamed(_))), "{id}: stores why");
        assert_eq!(
            names_of_row(refused).map(|_| ()),
            user_name_of_row(refused).map(|_| ()),
            "{id}"
        );
    }
    let why = user_name_of_row(&rows[0]).unwrap_err();
    assert!(why.contains("deribit only"), "the stored words: {why}");
    rows.extend(spelled(vec![row("okx", "SOL-USDT", "SOL", "USDT", AssetClass::CryptoSpot)]));
    assert!(kept_row_of_user_name(&rows, "okx", "SOLUSDT").is_ok());
}

/// **A broker's suffixed name is its wire name only**: ctrader's `EURUSDm` is the user's `EURUSD`.
#[test]
fn a_brokers_suffixed_name_is_only_the_wire() {
    let rows = spelled(vec![row("ctrader", "EURUSDm", "EUR", "USD", AssetClass::Fx)]);
    let names = names_of_user_name(&rows, "ctrader", "EURUSD").expect("the row");
    assert_eq!((names.user.as_str(), names.wire.as_str()), ("EURUSD", "EURUSDm"));
}

/// **A reader reads the STORED user name and never spells** (the owner's ruling «Б», 2026-11-16, simplified): a row
/// whose stored user name differs from what the grammar would spell answers the stored one, in every reader and lookup
/// (its store name is the store's rule over it), and the grammar's spelling names nothing. A row that already states
/// its naming keeps it through [`spelled`].
#[test]
fn a_reader_answers_the_stored_names_never_the_grammars() {
    let mut stored = row("oanda", "EUR_USD", "EUR", "USD", AssetClass::Fx);
    let grammar = spell_naming_of_row(&stored);
    assert_eq!(grammar, InstrumentNaming::User("EURUSD".into()), "the grammar spells `EURUSD`");
    stored.naming = Some(InstrumentNaming::User("STORED:EURUSD".into()));
    let rows = spelled(vec![stored]);
    let names = InstrumentNames { user: "STORED:EURUSD".into(), wire: "EUR_USD".into() };
    assert_eq!(names_of_row(&rows[0]), Ok(names.clone()), "spelled keeps what a row stores");
    assert_eq!(user_name_of_row(&rows[0]).as_deref(), Ok("STORED:EURUSD"));
    assert_eq!(wire_name_of_row(&rows[0]), "EUR_USD", "the wire is the row's own name");
    assert!(std::ptr::eq(
        kept_row_of_user_name(&rows, "oanda", "stored:eurusd").expect("the stored user name"),
        &rows[0]
    ));
    // The store name is the store's rule over the STORED user name.
    assert_eq!(names_of_store_name(&rows, "oanda", "STORED-EURUSD"), Ok(names));
    let why = kept_row_of_user_name(&rows, "oanda", "EURUSD").expect_err("the grammar's name");
    assert!(why.contains("carries no row"), "{why}");
    assert!(kept_row_of_store_name(&rows, "oanda", "EURUSD").is_err());
}

/// **A row that stores no names (a kept file written before names were stored) is refused in the refresh's words**:
/// by the row's readers, and by every lookup over a list holding one, even a lookup another row of it answers.
#[test]
fn a_row_that_stores_no_names_is_refused_with_the_refresh_words() {
    let words =
        "the bybit list predates symbol names: run `vike-cli data catalog refresh --venue bybit`";
    let old = row("bybit", "BTCUSDT.P", "BTC", "USDT", AssetClass::CryptoPerp);
    assert!(old.naming.is_none());
    for why in [names_of_row(&old).map(|_| ()), user_name_of_row(&old).map(|_| ())] {
        assert!(why.as_ref().is_err_and(|why| why.contains(words)), "{why:?}");
    }
    let mut rows = spelled(vec![row("bybit", "BTCUSDT", "BTC", "USDT", AssetClass::CryptoSpot)]);
    rows.push(old);
    for why in [
        kept_row_of_user_name(&rows, "bybit", "BTCUSDT").map(|_| ()),
        kept_row_of_store_name(&rows, "bybit", "BTCUSDT").map(|_| ()),
        names_of_user_name(&rows, "bybit", "BTCUSDT.P").map(|_| ()),
    ] {
        assert!(why.as_ref().is_err_and(|why| why.contains(words)), "{why:?}");
    }
}
