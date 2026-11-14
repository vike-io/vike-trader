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
// THE SHAPE LANES: deribit's perpetuals and hyperliquid's spot pairs (FEES-AUDIT 2026-10-26
// §4.11). Neither venue uses the `.P` suffix; each fronts two books under one id and prices them
// apart, and the symbol's shape decides which one a mount trades.
// -----------------------------------------------------------------------------------------

/// Every Deribit instrument that is not an option leaves the options row (commission audit
/// 2026-11-08: dated futures and `BTC_USDC` spot were charged the 3-bps options figure, 0.55-0.60 of
/// the measured 5 bps): a perpetual, a dated future (inverse or USDC) and a future spread take the
/// futures lane, a spot pair the spot lane. An option, the empty string and a name of no deribit shape
/// keep the bare (options) row.
#[test]
fn every_deribit_instrument_but_an_option_leaves_the_options_row() {
    for symbol in [
        "BTC-PERPETUAL",
        "ETH-PERPETUAL",
        "BTC_USDC-PERPETUAL",
        "GOLD_USDC-PERPETUAL",
        "eth-perpetual",
        "BTC-27MAR26",
        "ETH-9OCT26",
        "BTC_USDC-27DEC26",
        "BTC-FS-5OCT26_PERP",
        "BTC-FS-25DEC26_20JUL26",
    ] {
        assert_eq!(fee_lane("deribit", symbol), "deribit-futures", "{symbol}");
    }
    for symbol in ["BTC_USDC", "ETH_USDC", "STETH_ETH"] {
        assert_eq!(fee_lane("deribit", symbol), "deribit-spot", "{symbol}");
    }
    for symbol in [
        "BTC-27MAR26-80000-C",
        "SOL_USDC-12OCT26-96-P",
        "BTC-CS-X",
        "BTC-PERPETUAL-X",
        "PERPETUAL",
        "-PERPETUAL",
        "-27MAR26",
        "_USDC",
        "BTC_",
        "",
        "BTCUSDT.P",
        "BTCUSDT",
    ] {
        assert_eq!(fee_lane("deribit", symbol), "deribit", "{symbol} keeps the bare options row");
    }
    // the shape is deribit's alone: another venue's `...-PERPETUAL` is not a lane (hyperliquid and okx
    // are not in this list: each one's own `BASE-QUOTE` spot shape reads a two-part hyphen name as a pair)
    for v in ["binance", "bybit", "ibkr"] {
        assert_eq!(fee_lane(v, "BTC-PERPETUAL"), v, "{v}");
        assert_eq!(fee_lane(v, "BTC-27MAR26"), v, "{v}");
        assert_eq!(fee_lane(v, "BTC_USDC"), v, "{v}");
    }
}

/// An OKX SPOT pair (`BTC-USDT`, the one-hyphen instId) takes the spot lane; its SWAP, a dated future,
/// an option and every other spelling keep the bare SWAP row, and no other venue reads the shape.
#[test]
fn an_okx_spot_pair_takes_the_spot_lane() {
    for symbol in ["BTC-USDT", "ETH-USDC", "OKB-USDT"] {
        assert_eq!(fee_lane("okx", symbol), "okx-spot", "{symbol}");
    }
    for symbol in ["BTC-USDT-SWAP", "BTC-USD-261225", "BTC-USD-261225-90000-C", "BTCUSDT", ""] {
        assert_eq!(fee_lane("okx", symbol), "okx", "{symbol} keeps the SWAP row");
    }
    for v in ["binance", "bybit", "aster", "ibkr", "deribit"] {
        assert_eq!(fee_lane(v, "BTC-USDT"), v, "{v}");
    }
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

/// IBKR: a forex pair and a future each leave the US-stock row, read as the bridge's
/// `parse_simplified` reads the name; a stock, an index, a crypto claim and an option keep it.
#[test]
fn an_ibkr_forex_pair_and_future_take_their_lanes() {
    for symbol in
        ["EUR.USD.IDEALPRO", "EUR.IDEALPRO.USD", "usd.jpy.idealpro", "EUR.IDEALPRO.USD.CASH"]
    {
        assert_eq!(fee_lane("ibkr", symbol), "ibkr-fx", "{symbol}");
    }
    for symbol in
        ["ES.CME.USD.FUT.20261218", "MES.CME.USD.FUT.20261218.5", "nq.cme.usd.fut.20261218"]
    {
        assert_eq!(fee_lane("ibkr", symbol), "ibkr-futures", "{symbol}");
    }
    for symbol in [
        "AAPL.SMART.USD",
        "VOD.LSE.GBP.STK",
        "SPX.CBOE.USD.IND",
        "BTC.PAXOS.USD.CRYPTO",
        "SPY.SMART.USD.OPT.20261218.500.C",
        "AAPL",
        "",
    ] {
        assert_eq!(fee_lane("ibkr", symbol), "ibkr", "{symbol} keeps the stock row");
    }
    assert_eq!(fee_lane("ibkr_cpapi", "EUR.USD.IDEALPRO"), "ibkr-fx");
    for v in ["oanda", "alpaca", "binance"] {
        assert_eq!(fee_lane(v, "EUR.USD.IDEALPRO"), v, "{v}");
        assert_eq!(fee_lane(v, "ES.CME.USD.FUT.20261218"), v, "{v}");
    }
}

/// A Bybit SPOT pair (`PAIR.S`, decision 0231) takes the spot lane; the bare name and a typed `.P` stay the
/// perpetual's row, and no other venue reads the suffix.
#[test]
fn a_bybit_spot_pair_takes_the_spot_lane_by_its_suffix() {
    assert_eq!(fee_lane("bybit", "DOGEUSDT.S"), "bybit-spot");
    assert_eq!(fee_lane("bybit", "DOGEUSDT"), "bybit");
    assert_eq!(fee_lane("bybit", "DOGEUSDT.P"), "bybit");
    assert_eq!(fee_lane("bybit", "ETHUSDT-27NOV26"), "bybit");
    assert_eq!(fee_lane("bybit", "BTCUSD"), "bybit");
    assert_eq!(fee_lane("okx", "DOGEUSDT.S"), "okx");
    assert_eq!(fee_lane("binance", "DOGEUSDT.S"), "binance");
}

/// A Hyperliquid SPOT pair resolves to the spot lane in BOTH spellings; a perp (a bare coin), a
/// builder-dex perp in either spelling, another venue's multi-hyphen name and a half-pair do not.
#[test]
fn a_hyperliquid_spot_pair_takes_the_spot_lane_in_both_spellings() {
    for symbol in ["HYPE/USDC", "HYPE-USDC", "PURR/USDC", "HYPE/USDT0"] {
        assert_eq!(fee_lane("hyperliquid", symbol), "hyperliquid-spot", "{symbol}");
    }
    for symbol in [
        "BTC",
        "kPEPE",
        "xyz:TSLA",
        "TSLA.d-xyz",
        "",
        "/USDC",
        "HYPE/",
        "A/B/C",
        "BTC-USDT-SWAP",
        "BTCUSDT.P",
    ] {
        assert_eq!(fee_lane("hyperliquid", symbol), "hyperliquid", "{symbol} keeps the perp row");
    }
    // the shape is hyperliquid's alone: alpaca writes pairs with a slash too, and keeps its row
    for v in ["alpaca", "binance", "okx", "deribit"] {
        assert_eq!(fee_lane(v, "HYPE/USDC"), v, "{v}");
    }
}

/// THE REGRESSION GATE for both lanes, through the REAL path (`fee_lane` then
/// `crate::fee_schedule_for`): the shape lane is priced apart from the bare row, and its key has
/// a real arm (a typo would ride `fee_schedule_for`'s `_ => Free` fallback and zero every fee).
#[test]
fn a_shape_lane_prices_apart_from_the_bare_row() {
    use vike_model::FeeSchedule::PercentMakerTaker;
    for (venue, bare, lane) in [
        ("deribit", "BTC-27MAR26-80000-C", "ETH-PERPETUAL"),
        ("deribit", "BTC-27MAR26-80000-C", "BTC_USDC"),
        ("hyperliquid", "BTC", "HYPE/USDC"),
        ("okx", "BTC-USDT-SWAP", "BTC-USDT"),
        ("ibkr", "AAPL.SMART.USD", "EUR.USD.IDEALPRO"),
        ("ibkr", "AAPL.SMART.USD", "ES.CME.USD.FUT.20261218"),
    ] {
        let (a, b) = (fee_lane(venue, bare), fee_lane(venue, lane));
        assert_eq!(a, venue, "{venue}: {bare} is the bare row");
        assert_ne!(b, venue, "{venue}: {lane} is a lane");
        assert_ne!(crate::fee_schedule_for(a), crate::fee_schedule_for(b), "{venue}");
        assert_ne!(crate::fee_schedule_for(b), vike_model::FeeSchedule::Free, "{b} has an arm");
    }
    // the measured perpetual taker (5.0 bps of the 10 USD contract) and the published spot pair; a
    // dated future and deribit's spot pair at the same measured 5 bps
    for symbol in ["ETH-PERPETUAL", "BTC-27DEC26", "BTC_USDC-27DEC26", "BTC_USDC"] {
        assert_eq!(
            crate::fee_schedule_for(fee_lane("deribit", symbol)),
            PercentMakerTaker { maker_bps: 0.0, taker_bps: 5.0 },
            "{symbol}"
        );
    }
    assert_eq!(
        crate::fee_schedule_for(fee_lane("okx", "BTC-USDT")),
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
        crate::fee_schedule_for(fee_lane("hyperliquid", "HYPE/USDC")),
        PercentMakerTaker { maker_bps: 4.0, taker_bps: 7.0 }
    );
}
