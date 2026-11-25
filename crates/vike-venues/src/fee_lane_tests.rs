//! `fee_lane`'s per-shape lanes, pinned through the REAL path (`fee_lane` then
//! `crate::fee_schedule_for`). Moved here from `vike-catalog`'s `symbol_tests.rs` with the function
//! they test; the suffix-classification gate that reads the catalog's addressing table
//! (`uses_perp_suffix`) stays there.

use super::*;

// -----------------------------------------------------------------------------------------
// ASTER'S THIRD LANE: contract-class pricing on ONE perp order API.
//
// Aster charges three taker rates on `/fapi` — crypto 4 bps, equity/ETF/commodity 0.9 bps,
// USD1-settled 0.5 bps (live sweep, 2026-08-05; `crate::fees` carries the table). Only the
// USD1 class is decidable from a symbol, so only it is routed here. These tests pin BOTH
// halves: that the new lane resolves, and that adding it moved NOTHING else.
// -----------------------------------------------------------------------------------------

/// **The byte-identity gate.** Adding the USD1 lane must not have moved a single fee any caller
/// already resolved. Pinned through the REAL path (`fee_lane` → `fee_schedule_for`) against the
/// literal schedules that were in the table before the lane existed, so a future edit to either
/// crate that shifts one of them is loud here rather than silent in a backtest.
///
/// `BTCUSDT` is the specific symbol both aster rows were originally measured against, which is
/// why it is the anchor.
#[test]
fn the_usd1_lane_leaves_every_previously_resolved_aster_fee_unchanged() {
    use vike_model::FeeSchedule::PercentMakerTaker;
    // The two lanes that existed before, on the symbol they were measured on.
    assert_eq!(fee_lane("aster", "BTCUSDT"), "aster");
    assert_eq!(
        crate::fee_schedule_for(fee_lane("aster", "BTCUSDT")),
        PercentMakerTaker { maker_bps: 0.5, taker_bps: 4.0 },
        "aster SPOT BTCUSDT moved — measured live at 0.5/4 bps on 2026-08-05"
    );
    assert_eq!(fee_lane("aster", "BTCUSDT.P"), "aster-perp");
    assert_eq!(
        crate::fee_schedule_for(fee_lane("aster", "BTCUSDT.P")),
        PercentMakerTaker { maker_bps: 0.0, taker_bps: 4.0 },
        "aster PERP BTCUSDT.P moved — measured live at 0/4 bps on 2026-08-05"
    );
    // Every other crypto perp the sweep covered stays on the crypto lane too — including the
    // two shapes most likely to be caught by a sloppy suffix match.
    for symbol in ["ETHUSDT.P", "ASTERUSDT.P", "1000PEPEUSDT.P", "BTCDOMUSDT.P", "BTCU.P"] {
        assert_eq!(fee_lane("aster", symbol), "aster-perp", "{symbol} left the crypto perp lane");
    }
    // And the equity class — measured at 0.9 bps but deliberately NOT encoded — must still
    // resolve to the crypto row. This pins the documented GAP: if someone later routes these,
    // this test is where they must come and say so.
    for symbol in ["AAPLUSDT.P", "TSLAUSDT.P", "NVDAUSDT.P", "SPYUSDT.P", "XAUUSDT.P"] {
        assert_eq!(
            fee_lane("aster", symbol),
            "aster-perp",
            "{symbol} is an equity-class perp (true rate 0/0.9 bps, measured 2026-08-05) that this \
                 table deliberately charges the conservative crypto rate — see fee_schedule_for's \
                 `aster-perp` arm before changing this"
        );
    }
}

/// The other half: a USD1-settled perp resolves to a DIFFERENT lane with a DIFFERENT, cheaper
/// schedule. Without this the lane could be wired and silently never taken.
#[test]
fn a_usd1_settled_aster_perp_resolves_to_its_own_cheaper_lane() {
    use vike_model::FeeSchedule::PercentMakerTaker;
    for symbol in ["BTCUSD1.P", "ETHUSD1.P", "SOLUSD1.P"] {
        assert_eq!(fee_lane("aster", symbol), "aster-perp-usd1", "{symbol}");
        assert_eq!(
            crate::fee_schedule_for(fee_lane("aster", symbol)),
            PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.5 },
            "{symbol}: USD1-Perp is 0/0.005% (published) and measured 0/0.5 bps live 2026-08-05"
        );
        assert_ne!(
            crate::fee_schedule_for(fee_lane("aster", symbol)),
            crate::fee_schedule_for(fee_lane("aster", "BTCUSDT.P")),
            "{symbol} resolves to the SAME schedule as a USDT-settled perp — the 8x taker \
                 difference this lane exists for has been collapsed"
        );
    }
}

/// ⚠ The dangerous direction. This lane makes fees CHEAPER, so an over-broad match FLATTERS a
/// backtest — the failure mode the rest of this module works to avoid. The match is therefore a
/// suffix of the EXCHANGE symbol, and these are the cases that a `contains`, a base-asset match
/// or a pre-`.P`-strip match would each get wrong.
#[test]
fn usd1_lane_matches_settlement_not_substring() {
    // USD1 as the BASE asset, settled in USDT: a `contains("USD1")` would wrongly route it.
    assert_eq!(fee_lane("aster", "USD1USDT.P"), "aster-perp");
    // The SPOT USD1 pairs stay on the spot row entirely — the sweep measured all of them at the
    // flat spot rate (`BUSD1`, `ANUSD1` -> 0.5/4 bps), so a perp lane must not leak onto them.
    for symbol in ["BTCUSD1", "BUSD1", "ANUSD1", "USD1USDT"] {
        assert_eq!(fee_lane("aster", symbol), "aster", "{symbol} is a SPOT symbol");
    }
    // The suffix is tested AFTER the `.P` strip: a raw `ends_with` on the core symbol would
    // never match, silently disabling the lane.
    assert_ne!(fee_lane("aster", "BTCUSD1.P"), "aster-perp");
    // No OTHER venue grew the lane by accident — the arm is aster-gated.
    for v in ["binance", "bybit", "okx", "hyperliquid"] {
        assert_ne!(fee_lane(v, "BTCUSD1.P"), "aster-perp-usd1", "{v}");
    }
    // And the key is real, not a typo riding `fee_schedule_for`'s `_ => Free` fallback.
    assert_ne!(
        crate::fee_schedule_for("aster-perp-usd1"),
        vike_model::FeeSchedule::Free,
        "the USD1 lane key has no arm in vike_model::money::fees — every fee on it is silently zero"
    );
}

// -----------------------------------------------------------------------------------------
// THE SHAPE LANES: deribit's perpetuals (FEES-AUDIT 2026-10-26 §4.11), and the stated spot rows
// (bybit's, hyperliquid's and okx's `spot_pair_row`). Each fronts two books under one id and prices
// them apart, and the symbol's spelling decides which one a mount trades.
// -----------------------------------------------------------------------------------------

/// Every Deribit instrument that is not an option leaves the options row (commission audit
/// 2026-11-08: dated futures and `BTC_USDC` spot were charged the 3-bps options figure, 0.55-0.60 of
/// the measured 5 bps), by its USER name (decision 0290): a perpetual and a dated future (inverse or USDC) take the
/// futures lane, a spot pair the spot lane. An option (the venue's own name), the empty string, a name of no user
/// shape and an old venue name keep the bare (options) row.
#[test]
fn every_deribit_instrument_but_an_option_leaves_the_options_row() {
    for symbol in [
        "BTCUSD.P",
        "ETHUSD.P",
        "BTCUSDC.P",
        "GOLDUSDC.P",
        "ethusd.p",
        "BTCUSD_260327",
        "ETHUSD_261009",
        "BTCUSDC_261227",
    ] {
        assert_eq!(fee_lane("deribit", symbol), "deribit-futures", "{symbol}");
    }
    for symbol in ["BTCUSDC", "ETHUSDC", "STETHETH"] {
        assert_eq!(fee_lane("deribit", symbol), "deribit-spot", "{symbol}");
    }
    for symbol in [
        "BTC-27MAR26-80000-C",
        "SOL_USDC-12OCT26-96-P",
        "BTC-PERPETUAL",
        "BTC-27MAR26",
        "BTC_USDC",
        "BTCUSD_31FEB26",
        "_261227",
        "",
    ] {
        assert_eq!(fee_lane("deribit", symbol), "deribit", "{symbol} keeps the bare options row");
    }
}

/// An OKX SPOT pair's user name (`BTCUSDT`, the venue's `BTC-USDT`) takes the spot lane; its perpetual, a dated
/// future, an option and every other spelling keep the bare SWAP row.
#[test]
fn an_okx_spot_pair_takes_the_spot_lane() {
    for symbol in ["BTCUSDT", "ETHUSDC", "OKBUSDT"] {
        assert_eq!(fee_lane("okx", symbol), "okx-spot", "{symbol}");
    }
    for symbol in ["BTCUSDT.P", "BTCUSD_261225", "BTC-USD-261225-90000-C", "BTC-USDT", ""] {
        assert_eq!(fee_lane("okx", symbol), "okx", "{symbol} keeps the SWAP row");
    }
}

/// A binance dated contract's user name carries no `.P` (`BTCUSDT_261225`, `BTCUSD_261225`; decision 0290) and is
/// priced on the futures lane like the perpetual; its spot pair keeps the bare row.
#[test]
fn a_binance_dated_contract_is_priced_on_the_futures_lane() {
    for symbol in ["BTCUSDT_261225", "BTCUSD_261225", "BTCUSD.P", "BTCUSDT.P"] {
        assert_eq!(fee_lane("binance", symbol), "binance-perp", "{symbol}");
    }
    assert_eq!(fee_lane("binance", "BTCUSDT"), "binance");
}

/// cTrader (commission audit 2026-11-08): the measured broker charges its per-million commission on a
/// currency pair and a metal, nothing on an index, energy, share or crypto CFD. The bare row is the
/// first, the `ctrader-cfd` lane the second; the empty bare-venue lookup keeps the bare row.
#[test]
fn a_ctrader_cfd_takes_the_free_lane() {
    for symbol in
        ["EURUSD", "USDJPY", "eurgbp", "EUR/USD", "XAUUSD", "XAGUSD", "XPTUSD", "XAU/EUR", ""]
    {
        assert_eq!(fee_lane("ctrader", symbol), "ctrader", "{symbol} is FX or a metal");
    }
    for symbol in ["US500", "DE40", "XTIUSD", "XBRUSD", "BTCUSD", "ETHUSD", "AAPL.US", "EURUSDT"] {
        assert_eq!(fee_lane("ctrader", symbol), "ctrader-cfd", "{symbol} is a CFD");
    }
    for v in ["oanda", "fxcm", "ig", "dukascopy"] {
        assert_eq!(fee_lane(v, "US500"), v, "{v}");
    }
}

/// **With its KEPT row, a ctrader symbol is priced by the row's class** (an FX pair or a metal CFD: the bare row; any
/// other CFD: `ctrader-cfd`), whatever its name looks like; with no row, the name's answer above and the warning
/// flag; every other venue is `fee_lane` with no warning.
#[test]
fn a_kept_ctrader_row_prices_by_its_class_and_no_row_is_the_named_answer_and_a_warning() {
    use vike_model::{
        Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying,
        ContractFx, Instrument,
    };
    let cfd = |underlying_kind| {
        Contract::Cfd(ContractCfd {
            underlying_kind,
            underlying: None,
            product: ContractCfdProduct::Cfd,
            expiry: ContractCfdExpiry::Rolling,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        })
    };
    let row = |name: &str, contract| Instrument::new("ctrader", name, "", "", contract);
    for (name, contract, lane) in [
        ("EURUSD", Contract::Fx(ContractFx::default()), "ctrader"),
        ("XAUUSD", cfd(ContractCfdUnderlying::Metal), "ctrader"),
        ("BTCUSD", cfd(ContractCfdUnderlying::Crypto), "ctrader-cfd"),
        ("US500", cfd(ContractCfdUnderlying::Index), "ctrader-cfd"),
        // a name that LOOKS like a pair, typed a CFD by the broker: the row decides
        ("EURBBL", cfd(ContractCfdUnderlying::Bond), "ctrader-cfd"),
    ] {
        let kept = row(name, contract);
        assert_eq!(kept_fee_lane("ctrader", name, Some(&kept)), (lane, false), "{name}");
    }
    assert_eq!(
        kept_fee_lane("ctrader", "EURUSD", None),
        ("ctrader", true),
        "the name, and a warning"
    );
    assert_eq!(kept_fee_lane("ctrader", "US500", None), ("ctrader-cfd", true));
    assert_eq!(kept_fee_lane("ctrader", "", None), ("ctrader", false), "the bare-venue lookup");
    assert_eq!(kept_fee_lane("binance", "BTCUSDT.P", None), ("binance-perp", false));
}

/// IBKR (IBKR-3, decision 0290): a forex pair row and a future row each leave the US-stock row, by the KEPT row's
/// class; a stock row keeps it. A user name states no class, so with no row every name keeps the stock row and the
/// caller warns, and the old wire spellings are no lane's.
#[test]
fn an_ibkr_forex_pair_and_future_take_their_lanes_by_their_kept_row() {
    use vike_model::{
        Contract, ContractEquity, ContractFuture, ContractFutureDelivery, ContractFutureFamily,
        ContractFx, Instrument, Margining,
    };
    let future = Contract::Future(ContractFuture {
        margining: Margining::Linear,
        settle: "USD".into(),
        value_currency: Some("USD".into()),
        expiry_ms: 1_797_552_000_000,
        tenor: None,
        delivery: ContractFutureDelivery::Unknown,
        family: ContractFutureFamily::Traditional,
    });
    let stock = Contract::Equity(ContractEquity { exchange: None, currency: Some("USD".into()) });
    for (name, wire, base, quote, contract, lane) in [
        (
            "USDJPY",
            "USD.JPY.IDEALPRO",
            "USD",
            "JPY",
            Contract::Fx(ContractFx::default()),
            "ibkr-fx",
        ),
        ("ES_261218", "ES.CME.USD.FUT.20261218", "ES", "USD", future, "ibkr-futures"),
        ("AAPL", "AAPL.SMART.USD", "AAPL", "USD", stock, "ibkr"),
    ] {
        let row = Instrument::new("ibkr", wire, base, quote, contract);
        assert_eq!(kept_fee_lane("ibkr", name, Some(&row)), (lane, false), "{name}");
        assert_eq!(
            kept_fee_lane("ibkr", name, None),
            ("ibkr", true),
            "{name}: no row, the warning"
        );
        if lane != "ibkr" {
            assert_ne!(crate::fee_schedule_for(lane), crate::fee_schedule_for("ibkr"), "{lane}");
            assert_ne!(
                crate::fee_schedule_for(lane),
                vike_model::FeeSchedule::Free,
                "{lane} has an arm"
            );
        }
    }
    assert_eq!(kept_fee_lane("ibkr", "", None), ("ibkr", false), "the bare-venue lookup");
    for symbol in ["EUR.USD.IDEALPRO", "ES.CME.USD.FUT.20261218", "USDJPY", "AAPL", ""] {
        assert_eq!(fee_lane("ibkr", symbol), "ibkr", "{symbol}: a name states no ibkr lane");
    }
}

/// **deribit's `DDMMMYY`, read once** (moved here from vike-catalog's door, flag DER-4): one or two day digits, an
/// upper-case month, two year digits, and a day the calendar has. Every other shape is `None`, and so is a day its
/// month lacks.
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

/// **DER-4: the futures lane reads the ONE delivery-day rule** (a dated USER name's `_YYMMDD`, decision 0290): a day
/// the calendar lacks is no dated future (it keeps the options row), a real day in any case is.
#[test]
fn a_deribit_dated_future_needs_a_day_the_calendar_has() {
    for symbol in ["ETHUSD_260231", "BTCUSD_260100", "BTCUSD_260229", "BTCUSD_261332"] {
        assert_eq!(fee_lane("deribit", symbol), "deribit", "{symbol}");
        assert!(!is_delivery_day(&symbol[symbol.len() - 6..]), "{symbol}");
    }
    for symbol in ["ETHUSD_260228", "BTCUSD_280229", "btcusd_261227", "BTCUSDC_261103"] {
        assert_eq!(fee_lane("deribit", symbol), "deribit-futures", "{symbol}");
    }
    for day in ["26122", "2612255", "26-225", ""] {
        assert!(!is_delivery_day(day), "{day:?}");
    }
}

/// **BYB-3 and the row rule on every venue**: with a KEPT row, the row's class decides the lane whatever the name
/// reads (a bybit data-plane `BTCUSDT` whose row is SPOT is `bybit-spot`, not the perpetual's row); with none, the
/// name's lane as before.
#[test]
fn a_kept_row_decides_the_lane_on_every_venue() {
    use vike_model::{Contract, ContractPerp, Instrument, Margining};
    let perp = Contract::Perp(ContractPerp {
        margining: Margining::Linear,
        settle: "USDT".into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    });
    let spot =
        |venue: &str, name: &str| Instrument::new(venue, name, "", "USDT", Contract::CryptoSpot);
    let perp_row = |venue: &str, name: &str| Instrument::new(venue, name, "", "USDT", perp.clone());
    for (venue, name, row, lane) in [
        ("bybit", "BTCUSDT", spot("bybit", "BTCUSDT"), "bybit-spot"),
        ("bybit", "USDTUSD", spot("bybit", "USDTUSD"), "bybit-spot"),
        ("bybit", "BTCUSDT.P", perp_row("bybit", "BTCUSDT"), "bybit"),
        ("okx", "BTCUSDT", spot("okx", "BTC-USDT"), "okx-spot"),
        ("hyperliquid", "HYPEUSDC", spot("hyperliquid", "@107"), "hyperliquid-spot"),
        ("hyperliquid", "BTCUSDC.P", perp_row("hyperliquid", "BTC"), "hyperliquid"),
        ("deribit", "BTCUSD.P", perp_row("deribit", "BTC-PERPETUAL"), "deribit-futures"),
        ("deribit", "BTCUSDC", spot("deribit", "BTC_USDC"), "deribit-spot"),
        ("binance", "BTCUSDT", spot("binance", "BTCUSDT"), "binance"),
        ("binance", "BTCUSDT.P", perp_row("binance", "BTCUSDT.P"), "binance-perp"),
        ("aster", "BTCUSDT", spot("aster", "BTCUSDT"), "aster"),
        ("aster", "BTCUSDT.P", perp_row("aster", "BTCUSDT.P"), "aster-perp"),
    ] {
        assert_eq!(kept_fee_lane(venue, name, Some(&row)), (lane, false), "{venue} {name}");
    }
    assert_eq!(kept_fee_lane("bybit", "BTCUSDT.P", None), ("bybit", true), "no row: the name");
    assert_eq!(kept_fee_lane("oanda", "EURUSD", Some(&spot("oanda", "EUR_USD"))), ("oanda", false));
}

/// **ASTER-2: an aster stock perpetual is priced by its KEPT row's `underlyingSubType`** (0 / 0.9 bps, against the
/// crypto perpetual's 0 / 4): `STOCK`, `ETF`, `Commodities` and a blend; a crypto tag, none, or no row keep the
/// crypto lane, and a USD1 quote keeps its own.
#[test]
fn an_aster_stock_perpetual_takes_the_stock_lane_by_its_row() {
    use vike_model::{Contract, ContractPerp, FeeSchedule, Instrument, Margining};
    let row = |name: &str, quote: &str, tags: &[&str]| {
        let mut row = Instrument::new(
            "aster",
            name,
            "",
            quote,
            Contract::Perp(ContractPerp {
                margining: Margining::Linear,
                settle: quote.into(),
                value_currency: None,
                funding_interval_ms: None,
                settling_ms: None,
            }),
        );
        row.underlying_sub_type = tags.iter().map(|t| t.to_string()).collect();
        row
    };
    for (name, tags) in [
        ("AAPLUSDT.P", &["STOCK"][..]),
        ("NVDAUSDT.P", &["STOCK+Semiconductor"][..]),
        ("SPYUSDT.P", &["ETF"][..]),
        ("XAUUSDT.P", &["Commodities"][..]),
        ("QQQUSDT.P", &["Top", "etf"][..]),
    ] {
        let lane = kept_fee_lane("aster", name, Some(&row(name, "USDT", tags))).0;
        assert_eq!(lane, "aster-perp-stock", "{name}");
        assert_eq!(
            crate::fee_schedule_for(lane),
            FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.9 }
        );
    }
    for (name, tags) in
        [("BTCUSDT.P", &["Top"][..]), ("1000PEPEUSDT.P", &["Meme"][..]), ("TONUSDT.P", &[][..])]
    {
        assert_eq!(kept_fee_lane("aster", name, Some(&row(name, "USDT", tags))).0, "aster-perp");
    }
    assert_eq!(kept_fee_lane("aster", "AAPLUSDT.P", None).0, "aster-perp", "no row: the name");
    assert_eq!(
        kept_fee_lane("aster", "BTCUSD1.P", Some(&row("BTCUSD1.P", "USD1", &[]))).0,
        "aster-perp-usd1"
    );
}

/// A Bybit SPOT pair (decision 0290's user grammar: the unmarked `DOGEUSDT`, BYB-3) takes the spot lane; a perpetual
/// (`DOGEUSDT.P`, the inverse `BTCUSD.P`) and a dated contract (`ETHUSDT_261127`) stay the perpetual's row, as does the
/// bare-venue lookup; no other venue reads the bybit spelling.
#[test]
fn a_bybit_spot_pair_takes_the_spot_lane_by_its_unmarked_name() {
    assert_eq!(fee_lane("bybit", "DOGEUSDT"), "bybit-spot");
    assert_eq!(fee_lane("bybit", "USDTUSD"), "bybit-spot", "a pair quoted in USD is a pair");
    for derivative in ["DOGEUSDT.P", "BTCUSD.P", "BTCUSDC.P", "ETHUSDT_261127", "BTCUSD_261225", ""]
    {
        assert_eq!(fee_lane("bybit", derivative), "bybit", "{derivative}");
    }
    assert_eq!(
        fee_lane("okx", "DOGEUSDT"),
        "okx-spot",
        "okx spells its pair alike (decision 0290)"
    );
    assert_eq!(fee_lane("binance", "DOGEUSDT"), "binance");
}

/// **A bybit symbol's lane is its KEPT row's class** (BYB-3): a spot row is `bybit-spot` and every derivative row the
/// bare row, whatever the name; with no row the name's lane, and the warning.
#[test]
fn a_bybit_kept_row_decides_its_fee_lane() {
    use vike_model::{Contract, ContractPerp, Instrument, Margining};
    let perp = Contract::Perp(ContractPerp {
        margining: Margining::Linear,
        settle: "USDT".into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    });
    let spot = Instrument::new("bybit", "BTCUSDT", "BTC", "USDT", Contract::CryptoSpot);
    let linear = Instrument::new("bybit", "BTCUSDT.P", "BTC", "USDT", perp);
    assert_eq!(kept_fee_lane("bybit", "BTCUSDT", Some(&spot)), ("bybit-spot", false));
    assert_eq!(kept_fee_lane("bybit", "BTCUSDT.P", Some(&linear)), ("bybit", false));
    assert_eq!(
        kept_fee_lane("bybit", "BTCUSDT", None),
        ("bybit-spot", true),
        "the name, and a warning"
    );
    assert_eq!(kept_fee_lane("bybit", "BTCUSDT.P", None), ("bybit", true));
    assert_eq!(kept_fee_lane("bybit", "", None), ("bybit", false), "the bare-venue lookup");
}

/// A Hyperliquid SPOT pair resolves to the spot lane by its unmarked USER name (decision 0290: `HYPEUSDC`,
/// `PURRUSDC`); a perpetual (`BTCUSDC.P`, `KPEPEUSDC.P`), a builder-dex perpetual (`XYZ:TSLAUSDC.P`) and the empty
/// bare-venue lookup keep the perp row. The old pair spelling `HYPE/USDC` is no user name and no pair.
#[test]
fn a_hyperliquid_spot_pair_takes_the_spot_lane_by_its_unmarked_user_name() {
    for symbol in ["HYPEUSDC", "PURRUSDC", "HYPEUSDT0"] {
        assert_eq!(fee_lane("hyperliquid", symbol), "hyperliquid-spot", "{symbol}");
    }
    for symbol in ["BTCUSDC.P", "KPEPEUSDC.P", "XYZ:TSLAUSDC.P", "", "HYPE/USDC"] {
        assert_eq!(fee_lane("hyperliquid", symbol), "hyperliquid", "{symbol} keeps the perp row");
    }
}

/// THE REGRESSION GATE for both lanes, through the REAL path (`fee_lane` then
/// `crate::fee_schedule_for`): the shape lane is priced apart from the bare row, and its key has
/// a real arm (a typo would ride `fee_schedule_for`'s `_ => Free` fallback and zero every fee).
#[test]
fn a_shape_lane_prices_apart_from_the_bare_row() {
    use vike_model::FeeSchedule::PercentMakerTaker;
    for (venue, bare, lane) in [
        ("deribit", "BTC-27MAR26-80000-C", "ETHUSD.P"),
        ("deribit", "BTC-27MAR26-80000-C", "BTCUSDC"),
        ("hyperliquid", "BTCUSDC.P", "HYPEUSDC"),
        ("okx", "BTCUSDT.P", "BTCUSDT"),
    ] {
        let (a, b) = (fee_lane(venue, bare), fee_lane(venue, lane));
        assert_eq!(a, venue, "{venue}: {bare} is the bare row");
        assert_ne!(b, venue, "{venue}: {lane} is a lane");
        assert_ne!(crate::fee_schedule_for(a), crate::fee_schedule_for(b), "{venue}");
        assert_ne!(crate::fee_schedule_for(b), vike_model::FeeSchedule::Free, "{b} has an arm");
    }
    // the measured perpetual taker (5.0 bps of the 10 USD contract) and the published spot pair; a
    // dated future and deribit's spot pair at the same measured 5 bps
    for symbol in ["ETHUSD.P", "BTCUSD_261227", "BTCUSDC_261227", "BTCUSDC"] {
        assert_eq!(
            crate::fee_schedule_for(fee_lane("deribit", symbol)),
            PercentMakerTaker { maker_bps: 0.0, taker_bps: 5.0 },
            "{symbol}"
        );
    }
    assert_eq!(
        crate::fee_schedule_for(fee_lane("okx", "BTCUSDT")),
        PercentMakerTaker { maker_bps: 8.0, taker_bps: 10.0 }
    );
    // ctrader prices apart the other way round: its bare row is the FX commission and the lane is free
    assert_eq!(
        crate::fee_schedule_for(fee_lane("ctrader", "EURUSD")),
        PercentMakerTaker { maker_bps: 0.3, taker_bps: 0.3 }
    );
    assert_eq!(
        crate::fee_schedule_for(fee_lane("ctrader", "US500")),
        vike_model::FeeSchedule::Free
    );
    assert_eq!(
        crate::fee_schedule_for(fee_lane("hyperliquid", "HYPEUSDC")),
        PercentMakerTaker { maker_bps: 4.0, taker_bps: 7.0 }
    );
}
