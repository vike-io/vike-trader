//! The bounds on IBKR's per-name lookups: keep, single flight, the budget, the table size, and how each
//! answer is worded for the node.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_gatewayhub_client::proto::{
    PROPERTIES_BUSY, PROPERTIES_LIST_UNAVAILABLE, SymbolPropertiesRefusalCode,
};
use vike_model::SymbolProperties;

use vike_ibkr::instrument::any_symbol::socket::IbkrPropertiesClass;
use vike_ibkr::instrument::any_symbol::{IbkrPropertiesError, IbkrPropertiesStock};
use vike_ibkr::refusals::IbkrSymbolRefusal;
use vike_ibkr::symbol_properties::{MAX_NAMES, SymbolPropertiesKeyedSpecIbkr};

use super::*;
use crate::trade::adapters::testutil::ibkr_stock;

const LONG: Duration = Duration::from_secs(3600);

/// **The plain path admits what it always did, whatever the price magnifier**: `facts` answers the same grid for a
/// contract the gateway reports with a magnifier of 0 (unset), 1 or 100, refuses nothing for it, and only RECORDS
/// the value (`SymbolPropertiesCacheKeyed::note`) for the converting path to judge a foreign stock by. The record
/// survives a grid served from the keep, and is not invented for a name never looked up.
#[test]
fn the_plain_path_admits_any_price_magnifier_and_records_it() {
    let facts = SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::default();
    assert_eq!(facts.note("AAPL.SMART.USD"), None, "never looked up: nothing recorded");
    let baseline =
        facts.facts("BASE.SMART.USD", || Ok(ibkr_stock("BASE.SMART.USD"))).expect("granted");
    for (name, magnifier) in [("A0.SMART.USD", 0), ("A1.SMART.USD", 1), ("A100.SMART.USD", 100)] {
        let got = facts
            .facts(name, || {
                Ok(IbkrPropertiesStock { price_magnifier: magnifier, ..ibkr_stock(name) })
            })
            .expect("the plain path refuses nothing for a magnifier");
        assert_eq!(got, baseline, "{magnifier}: the same grid, byte for byte");
        assert_eq!(facts.note(name), Some(magnifier));
        // Served from the keep: the fetch does not run, the record stands.
        let again = facts.facts(name, || panic!("{name}: the grid is kept")).expect("kept");
        assert_eq!(again, baseline);
        assert_eq!(facts.note(name), Some(magnifier));
    }
}

/// A record older than the keep is gone with its answer: with a keep of no time nothing is ever read back.
#[test]
fn a_magnifier_record_does_not_outlive_the_keep() {
    let facts = SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::with_bounds(
        Duration::ZERO,
        LONG,
        100,
        LONG,
    );
    facts.facts("AAPL.SMART.USD", || Ok(ibkr_stock("AAPL.SMART.USD"))).expect("granted");
    assert_eq!(facts.note("AAPL.SMART.USD"), None);
}

/// A granted answer is the grid with NO contract value (a share is linear 1.0), and is kept: the second ask
/// does not look the name up again.
#[test]
fn an_answer_is_kept_and_a_share_has_no_contract_value() {
    let facts = SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::default();
    let calls = AtomicUsize::new(0);
    for _ in 0..3 {
        let got = facts
            .facts("AAPL.SMART.USD", || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(ibkr_stock("AAPL.SMART.USD"))
            })
            .expect("granted");
        assert_eq!(got.properties.tick_size, 0.01);
        assert_eq!(got.contract_value, None);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The catalog's value of the user name over its kept row (what a paper book and a backtest read; decision 0290)
    // is this answer's linear 1.0.
    let rows = vike_ibkr::testing::operator_rows();
    let aapl = rows.iter().find(|row| row.raw_symbol == "AAPL.SMART.USD");
    assert_eq!(
        vike_catalog::kept_contract_value("ibkr", "AAPL", aapl),
        Some(vike_model::ContractValue::default())
    );
}

/// The bridge's "not now" of a gateway whose symbol search did not answer.
fn no_link() -> IbkrSymbolRefusal {
    IbkrSymbolRefusal::DidNotAnswer {
        what: "the symbol search for AAPL".into(),
        why: "no link".into(),
    }
}

/// A refusal is the verdict's own words and is kept for the failure time; an unavailable gateway is worded as
/// the node's "not now" (so the node does not remember it) and is kept as a failure too.
#[test]
fn a_refusal_is_a_verdict_and_an_unavailable_gateway_is_not_now() {
    let facts = SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::default();
    let no_contract = IbkrSymbolRefusal::NoContractForTicker {
        name: "DUAL.SMART.USD".into(),
        ticker: "DUAL".into(),
    };
    let why = facts
        .facts("DUAL.SMART.USD", || Err(IbkrPropertiesError::Refused(no_contract.clone())))
        .expect_err("refused");
    assert_eq!(why, super::super::symbol_facts::verdict(no_contract.carried()));

    let why = facts
        .facts("AAPL.SMART.USD", || Err(IbkrPropertiesError::Unavailable(no_link())))
        .expect_err("unavailable");
    assert!(
        why.reason.contains(PROPERTIES_LIST_UNAVAILABLE) && why.reason.contains("no link"),
        "{why}"
    );
    assert_eq!(why.code, SymbolPropertiesRefusalCode::ListUnavailable, "{why}");
}

/// A failure is kept only for its (short) time, then the name is looked up again.
#[test]
fn a_failure_expires() {
    let facts = SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::with_bounds(
        LONG,
        Duration::ZERO,
        100,
        LONG,
    );
    let calls = AtomicUsize::new(0);
    for _ in 0..2 {
        let _ = facts.facts("AAPL.SMART.USD", || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(IbkrPropertiesError::Unavailable(no_link()))
        });
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// The budget: lookups past it are "busy" without calling the fetch; a kept answer costs none.
#[test]
fn the_account_lookup_budget_is_spent_by_lookups_not_by_kept_answers() {
    let facts = SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::with_bounds(
        LONG, LONG, 2, LONG,
    );
    for name in ["A.SMART.USD", "B.SMART.USD"] {
        assert!(facts.facts(name, || Ok(ibkr_stock(name))).is_ok());
    }
    assert!(facts.facts("A.SMART.USD", || panic!("kept")).is_ok(), "a kept answer is free");
    let why = facts.facts("C.SMART.USD", || panic!("over budget")).expect_err("busy");
    assert!(
        why.reason.contains(PROPERTIES_BUSY) && why.reason.contains("at most 2 lookups"),
        "{why}"
    );
    assert_eq!(why.code, SymbolPropertiesRefusalCode::Busy, "{why}");
}

/// A full table answers busy for a new name when every slot is fresh.
#[test]
fn a_full_table_is_busy() {
    let facts = SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::with_bounds(
        LONG,
        LONG,
        usize::MAX,
        LONG,
    );
    for n in 0..MAX_NAMES {
        let name = format!("S{n}.SMART.USD");
        assert!(facts.facts(&name, || Ok(ibkr_stock(&name))).is_ok());
    }
    let why = facts.facts("ONEMORE.SMART.USD", || panic!("busy")).expect_err("busy");
    assert_eq!(why.code, SymbolPropertiesRefusalCode::Busy, "{why}");
    assert!(why.reason.contains(PROPERTIES_BUSY) && why.reason.contains("64 names"), "{why}");
}

/// Two askers of one name make ONE lookup: the second waits on the slot and reads the first's answer.
#[test]
fn concurrent_askers_of_one_name_share_one_lookup() {
    let facts = Arc::new(SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (facts, calls) = (Arc::clone(&facts), Arc::clone(&calls));
            std::thread::spawn(move || {
                facts.facts("AAPL.SMART.USD", || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    Ok(ibkr_stock("AAPL.SMART.USD"))
                })
            })
        })
        .collect();
    for t in threads {
        assert!(t.join().expect("thread").is_ok());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

fn future_facts(
    symbol: &str,
) -> vike_ibkr::instrument::any_symbol::forex_future::IbkrPropertiesFuture {
    vike_ibkr::instrument::any_symbol::forex_future::IbkrPropertiesFuture {
        symbol: symbol.into(),
        conid: 515_416_632,
        properties: SymbolProperties {
            tick_size: 0.25,
            step_size: 1.0,
            min_qty: 1.0,
            contract_size: 50.0,
            ..SymbolProperties::default()
        },
        contract_value: vike_model::ContractValue::linear(50.0),
        currency: "USD".into(),
        last_trade_ms: 1_797_552_000_000,
        margin_rate: Some(0.0632),
    }
}

/// **A future's answer carries its multiplier as the contract value and the venue's margin rate**; a forex pair's
/// carries neither (linear in units of its base, no margin rate stated). Both are for the name asked (no other
/// spelling is stated) and kept like a stock's.
#[test]
fn a_future_and_a_pair_are_answered_by_class_and_kept() {
    let facts = SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::default();
    let es = facts
        .facts_other("ES.CME.USD.FUT.20261218", || {
            Ok(IbkrPropertiesClass::Future(future_facts("ES.CME.USD.FUT.20261218")))
        })
        .expect("granted");
    assert_eq!(es.contract_value, Some(vike_model::ContractValue::linear(50.0)));
    assert_eq!(es.margin_rate, Some(0.0632));
    assert_eq!(es.symbol, None, "the asked spelling is the venue's: nothing to spell back");
    assert_eq!(es.properties.contract_size, 50.0);
    let kept = facts.facts_other("ES.CME.USD.FUT.20261218", || panic!("kept")).expect("kept");
    assert_eq!(kept, es);

    let pair = facts
        .facts_other("EUR.USD.IDEALPRO", || {
            Ok(IbkrPropertiesClass::Forex(
                vike_ibkr::instrument::any_symbol::forex_future::IbkrPropertiesForex {
                    symbol: "EUR.USD.IDEALPRO".into(),
                    conid: 12_087_792,
                    base: "EUR".into(),
                    quote: "USD".into(),
                    properties: SymbolProperties {
                        tick_size: 0.00005,
                        step_size: 0.01,
                        min_qty: 20_000.0,
                        ..SymbolProperties::default()
                    },
                },
            ))
        })
        .expect("granted");
    assert_eq!((pair.contract_value, pair.margin_rate, pair.symbol), (None, None, None));
    assert_eq!(pair.properties.min_qty, 20_000.0);
    // The catalog agrees where the kept row's class fixes the unit (a pair: linear 1.0, this answer's), and gives no
    // value for a future, whose multiplier only the venue's row states (decision 0290: by the row, never the name).
    let rows = vike_ibkr::testing::operator_rows();
    let row = |wire: &str| rows.iter().find(|row| row.raw_symbol == wire);
    assert_eq!(
        vike_catalog::kept_contract_value("ibkr", "EURUSD", row("EUR.USD.IDEALPRO")),
        Some(pair.contract_value.unwrap_or_default())
    );
    assert_eq!(
        vike_catalog::kept_contract_value("ibkr", "ES_261218", row("ES.CME.USD.FUT.20261218")),
        None
    );
}

/// A refusal and an unavailable lookup of a class keep their kinds.
#[test]
fn a_class_refusal_is_a_verdict_and_an_unavailable_lookup_is_not_now() {
    let facts = SymbolPropertiesCacheKeyed::<SymbolPropertiesKeyedSpecIbkr>::default();
    let refused = facts
        .facts_other("CL.NYMEX.USD.FUT.20261120", || {
            Err(IbkrPropertiesError::Refused(IbkrSymbolRefusal::RootNotCashSettled {
                name: "CL.NYMEX.USD.FUT.20261120".into(),
                root: "CL".into(),
                roots: "ES, MES".into(),
            }))
        })
        .expect_err("refused");
    assert_eq!(refused.code, SymbolPropertiesRefusalCode::Verdict, "{refused}");
    assert!(refused.reason.contains("settles by delivery"), "{refused}");
    let down = facts
        .facts_other("ES.CME.USD.FUT.20261218", || {
            Err(IbkrPropertiesError::Unavailable(IbkrSymbolRefusal::DidNotAnswer {
                what: "contract details of ES.CME.USD.FUT.20261218".into(),
                why: "gateway down".into(),
            }))
        })
        .expect_err("not now");
    assert_eq!(down.code, SymbolPropertiesRefusalCode::ListUnavailable, "{down}");
    assert!(down.reason.contains("gateway down"), "{down}");
}
