//! **Today's answers for listed symbols, pinned BEFORE the hand tables went** (owner 2026-11-13, "remove all hardcoded
//! symbols"): what the door, the converting lane, the conversion pair, the history scale and the catalog answer for a
//! set of instruments the account lists, one or more of each class. Written and run green on the code that read the
//! committed SDK table (commit `93b0105833`), then kept, so the list-fed code (here over the recorded-shape list,
//! `crate::test_support::listed`) is held to the same answers. A value that had to change is a correction (a server
//! figure, or a measured scale) and says so where it is pinned, with the old value beside it.

use std::assert_matches;

use vike_model::{
    AccountPerQuoteLeg, Contract, ContractCfdUnderlying, ConversionRoute, QuoteRateError,
};

use crate::account::quote_rate::conversion_route;
use crate::instrument::any_symbol::{
    DukascopyPropertiesBasis, facts_basis, instrument_facts_quoted, instrument_facts_refused,
};
use crate::instrument::grid::{JforexInstrument, history_point_value};
use crate::refusals::{DukascopyPropertiesRefusal, DukascopyPropertiesRefusalId};
use crate::test_support::listed;

/// The account's list every pin here reads.
fn rows() -> Vec<JforexInstrument> {
    listed()
}

/// What the door makes of the wire name `symbol`: `pair`, `terms <wire spelling>`, or `refused` (the cause is checked by
/// the caller). CHANGED by decision 0290: a fact is asked by the kept row's wire name exactly, so the slashed spelling
/// of a pair or a metal (`eur/usd`, `XAG/USD`) and the enum-style name of a CFD (`COPPERCMDUSD`), each answered or refused
/// by name before, are on no row of the list now.
fn basis(symbol: &str) -> Result<String, DukascopyPropertiesRefusal> {
    match facts_basis(&rows(), symbol)? {
        DukascopyPropertiesBasis::Static => Ok("pair".to_string()),
        terms @ DukascopyPropertiesBasis::VenueTerms(_) => {
            Ok(format!("terms {}", terms.wire_symbol().unwrap_or_default()))
        }
    }
}

#[test]
fn the_basis_of_each_listed_symbol() {
    for (symbol, want) in [
        ("EURUSD", "pair"),
        ("USDJPY", "pair"),
        ("GBPUSD", "pair"),
        ("EURGBP", "pair"),
        ("EURHUF", "pair"),
        ("EURCZK", "pair"),
        ("EURTHB", "pair"),
        ("USDTRY", "pair"),
        ("XAUUSD", "terms XAUUSD"),
        ("XAGUSD", "terms XAGUSD"),
        ("COPPER.CMD/USD", "terms COPPER.CMD/USD"),
        ("BRENT.CMD/USD", "terms BRENT.CMD/USD"),
        ("DEU.IDX/EUR", "terms DEU.IDX/EUR"),
        ("BMW.DE/EUR", "terms BMW.DE/EUR"),
    ] {
        assert_eq!(basis(symbol).as_deref(), Ok(want), "{symbol}");
    }
    for (symbol, cause) in [
        ("BTCUSD", DukascopyPropertiesRefusalId::Coin),
        ("ETHUSD", DukascopyPropertiesRefusalId::Coin),
        ("LSE.GB/GBX", DukascopyPropertiesRefusalId::MinorUnit),
        // CHANGED (was "write the instrument `COPPER.CMD/USD`"): no other spelling of a row is read (the doc of `basis`)
        ("COPPERCMDUSD", DukascopyPropertiesRefusalId::NotOnList),
        ("USTBOND.TR/USD", DukascopyPropertiesRefusalId::NotOnList),
    ] {
        let why = basis(symbol).expect_err(symbol);
        assert_eq!(why.id(), cause, "{symbol}: {why}");
    }
}

#[test]
fn the_grid_and_quote_of_each_listed_pair() {
    let usd = |s: &str| {
        instrument_facts_refused(&rows(), s, "USD").map(|p| (p.tick_size, p.step_size, p.min_qty))
    };
    for symbol in ["EURUSD", "AUDUSD", "NZDUSD", "GBPUSD"] {
        assert_eq!(usd(symbol), Ok((0.00001, 1000.0, 1000.0)), "{symbol}");
    }
    for (symbol, quote) in [("USDJPY", "JPY"), ("EURGBP", "GBP"), ("EURHUF", "HUF")] {
        let why = usd(symbol).expect_err(symbol);
        assert_matches!(
            &why,
            DukascopyPropertiesRefusal::QuoteNotLedger { quote: q, .. } if q == quote,
            "{symbol}: {why}"
        );
    }
    let quoted =
        |s: &str| instrument_facts_quoted(&rows(), s).map(|(p, q)| (p.tick_size, p.min_qty, q));
    for (symbol, tick, quote) in [
        ("EURUSD", 0.00001, "USD"),
        ("USDJPY", 0.001, "JPY"),
        ("GBPUSD", 0.00001, "USD"),
        ("EURGBP", 0.00001, "GBP"),
        ("EURHUF", 0.001, "HUF"),
        ("EURCZK", 0.0001, "CZK"),
        // CORRECTED (was 0.0001, the SDK's predefined pip 0.001): the server states EUR/THB's pip as 0.01, and the SDK
        // overrode its constant at the demo login of 2026-11-13; the hand table's grid was ten times too fine
        ("EURTHB", 0.001, "THB"),
        ("USDTRY", 0.00001, "TRY"),
    ] {
        assert_eq!(quoted(symbol), Ok((tick, 1000.0, quote.to_string())), "{symbol}");
    }
    for symbol in ["XAUUSD", "COPPER.CMD/USD", "DEU.IDX/EUR"] {
        let why = usd(symbol).expect_err(symbol);
        assert_eq!(why.id(), DukascopyPropertiesRefusalId::NeedsVenueTerms, "{symbol}: {why}");
        assert!(instrument_facts_quoted(&rows(), symbol).is_err(), "{symbol}");
    }
}

#[test]
fn the_conversion_pair_of_each_quote_currency() {
    use AccountPerQuoteLeg::{AccountIsBase, QuoteIsBase};
    for (quote, pair, leg) in [
        ("JPY", "USDJPY", AccountIsBase),
        ("GBP", "GBPUSD", QuoteIsBase),
        ("EUR", "EURUSD", QuoteIsBase),
        ("CHF", "USDCHF", AccountIsBase),
        ("HUF", "USDHUF", AccountIsBase),
        ("CZK", "USDCZK", AccountIsBase),
        ("THB", "USDTHB", AccountIsBase),
    ] {
        let Ok(ConversionRoute::Direct(found)) = conversion_route(&rows(), quote, "USD") else {
            panic!("{quote}: no direct pair")
        };
        assert_eq!((found.pair.as_str(), found.orientation), (pair, leg), "{quote}");
    }
    let Ok(ConversionRoute::Direct(eurjpy)) = conversion_route(&rows(), "JPY", "EUR") else {
        panic!("JPY on EUR: no direct pair")
    };
    assert_eq!(eurjpy.pair, "EURJPY");
    assert!(matches!(conversion_route(&rows(), "GBX", "USD"), Err(QuoteRateError::NoPair(_))));
}

/// The HTTP lane's `.bi5` point value: the row's `10^tick_scale`. CORRECTED where the old suffix rule (`1e3` for a name
/// ending in JPY, else `1e5`) disagreed with the tick scale: the metals (measured: XAU/USD's hour of 2024-01-03 14:00 UTC
/// decodes to 2041.28 at 1,000 and to 20.41 at the old 100,000; XAG/USD to 23.06 against 0.23) and the HUF and CZK
/// crosses (the old 100,000 against the rows' 1,000 and 10,000; `crates/bridges/dukascopy/tests/bi5_agrees_with_the_bridge.rs`'s
/// live probe).
#[test]
fn the_history_scale_of_each_listed_symbol() {
    for (symbol, points, was) in [
        ("EURUSD", 1e5, 1e5),
        ("USDJPY", 1e3, 1e3),
        ("GBPUSD", 1e5, 1e5),
        ("EURHUF", 1e3, 1e5),
        ("EURCZK", 1e4, 1e5),
        ("XAUUSD", 1e3, 1e5),
        ("XAGUSD", 1e3, 1e5),
    ] {
        assert_eq!(history_point_value(&rows(), symbol), Ok(points), "{symbol} (was {was})");
    }
}

/// The catalog's row, as the gatewayhub hands the pushed list to the datahub. The HUF and CZK crosses now carry the pip
/// their row states, where the hand table stated none (its scale was doubtful).
#[test]
fn the_catalog_row_of_each_listed_symbol() {
    crate::catalog::hold(&rows());
    let rows = vike_catalog::held_list::held("dukascopy").expect("held");
    let row = |s: &str| rows.iter().find(|i| i.raw_symbol == s).cloned().expect(s);
    for (symbol, base, quote, pip) in [
        ("EURUSD", "EUR", "USD", Some(0.0001)),
        ("USDJPY", "USD", "JPY", Some(0.01)),
        ("GBPUSD", "GBP", "USD", Some(0.0001)),
        ("EURHUF", "EUR", "HUF", Some(0.01)),  // was None
        ("EURCZK", "EUR", "CZK", Some(0.001)), // was None
    ] {
        let r = row(symbol);
        assert_eq!(
            (r.venue.as_str(), r.base.as_str(), r.quote.as_str()),
            ("dukascopy", base, quote)
        );
        match r.contract {
            Contract::Fx(fx) => assert_eq!(fx.pip_size, pip, "{symbol}"),
            other => panic!("{symbol}: {other:?}"),
        }
    }
    for symbol in ["XAUUSD", "XAGUSD"] {
        match row(symbol).contract {
            Contract::Cfd(cfd) => assert_eq!(cfd.underlying_kind, ContractCfdUnderlying::Metal),
            other => panic!("{symbol}: {other:?}"),
        }
    }
}
