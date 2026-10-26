//! The bounds on IBKR's per-name lookups: keep, single flight, the budget, the table size, and how each
//! answer is worded for the node.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_gatewayhub_client::proto::{FACTS_BUSY, FACTS_LIST_UNAVAILABLE};
use vike_model::SymbolProperties;

use super::*;

fn stock(name: &str) -> StockFacts {
    StockFacts {
        symbol: name.into(),
        conid: 265_598,
        properties: SymbolProperties {
            tick_size: 0.01,
            step_size: 1.0,
            min_qty: 1.0,
            ..SymbolProperties::default()
        },
        price_magnifier: 1,
    }
}

const LONG: Duration = Duration::from_secs(3600);

/// **The plain path admits what it always did, whatever the price magnifier**: `facts` answers the same grid for a
/// contract the gateway reports with a magnifier of 0 (unset), 1 or 100, refuses nothing for it, and only RECORDS
/// the value ([`IbkrFacts::price_magnifier`]) for the converting path to judge a foreign stock by. The record
/// survives a grid served from the keep, and is not invented for a name never looked up.
#[test]
fn the_plain_path_admits_any_price_magnifier_and_records_it() {
    let facts = IbkrFacts::default();
    assert_eq!(facts.price_magnifier("AAPL.SMART.USD"), None, "never looked up: nothing recorded");
    let baseline = facts.facts("BASE.SMART.USD", || Ok(stock("BASE.SMART.USD"))).expect("granted");
    for (name, magnifier) in [("A0.SMART.USD", 0), ("A1.SMART.USD", 1), ("A100.SMART.USD", 100)] {
        let got = facts
            .facts(name, || Ok(StockFacts { price_magnifier: magnifier, ..stock(name) }))
            .expect("the plain path refuses nothing for a magnifier");
        assert_eq!(got, baseline, "{magnifier}: the same grid, byte for byte");
        assert_eq!(facts.price_magnifier(name), Some(magnifier));
        // Served from the keep: the fetch does not run, the record stands.
        let again = facts.facts(name, || panic!("{name}: the grid is kept")).expect("kept");
        assert_eq!(again, baseline);
        assert_eq!(facts.price_magnifier(name), Some(magnifier));
    }
}

/// A record older than the keep is gone with its answer: with a keep of no time nothing is ever read back.
#[test]
fn a_magnifier_record_does_not_outlive_the_keep() {
    let facts = IbkrFacts::with_bounds(Duration::ZERO, LONG, 100, LONG);
    facts.facts("AAPL.SMART.USD", || Ok(stock("AAPL.SMART.USD"))).expect("granted");
    assert_eq!(facts.price_magnifier("AAPL.SMART.USD"), None);
}

/// A granted answer is the grid with NO contract value (a share is linear 1.0), and is kept: the second ask
/// does not look the name up again.
#[test]
fn an_answer_is_kept_and_a_share_has_no_contract_value() {
    let facts = IbkrFacts::default();
    let calls = AtomicUsize::new(0);
    for _ in 0..3 {
        let got = facts
            .facts("AAPL.SMART.USD", || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(stock("AAPL.SMART.USD"))
            })
            .expect("granted");
        assert_eq!(got.properties.tick_size, 0.01);
        assert_eq!(got.contract_value, None);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// A refusal is the verdict's own words and is kept for the failure time; an unavailable gateway is worded as
/// the node's "not now" (so the node does not remember it) and is kept as a failure too.
#[test]
fn a_refusal_is_a_verdict_and_an_unavailable_gateway_is_not_now() {
    let facts = IbkrFacts::default();
    let why = facts
        .facts("DUAL.SMART.USD", || Err(FactsError::Refused("DUAL is ambiguous".into())))
        .expect_err("refused");
    assert_eq!(why, "DUAL is ambiguous");
    assert!(!vike_gatewayhub_client::proto::facts_refusal_is_transient(&why));

    let why = facts
        .facts("AAPL.SMART.USD", || Err(FactsError::Unavailable("no link".into())))
        .expect_err("unavailable");
    assert!(why.contains(FACTS_LIST_UNAVAILABLE) && why.contains("no link"), "{why}");
    assert!(vike_gatewayhub_client::proto::facts_refusal_is_transient(&why));
}

/// A failure is kept only for its (short) time, then the name is looked up again.
#[test]
fn a_failure_expires() {
    let facts = IbkrFacts::with_bounds(LONG, Duration::ZERO, 100, LONG);
    let calls = AtomicUsize::new(0);
    for _ in 0..2 {
        let _ = facts.facts("AAPL.SMART.USD", || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(FactsError::Unavailable("no link".into()))
        });
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// The budget: lookups past it are "busy" without calling the fetch; a kept answer costs none.
#[test]
fn the_account_lookup_budget_is_spent_by_lookups_not_by_kept_answers() {
    let facts = IbkrFacts::with_bounds(LONG, LONG, 2, LONG);
    for name in ["A.SMART.USD", "B.SMART.USD"] {
        assert!(facts.facts(name, || Ok(stock(name))).is_ok());
    }
    assert!(facts.facts("A.SMART.USD", || panic!("kept")).is_ok(), "a kept answer is free");
    let why = facts.facts("C.SMART.USD", || panic!("over budget")).expect_err("busy");
    assert!(why.contains(FACTS_BUSY) && why.contains("at most 2 lookups"), "{why}");
    assert!(vike_gatewayhub_client::proto::facts_refusal_is_transient(&why));
}

/// A full table answers busy for a new name when every slot is fresh.
#[test]
fn a_full_table_is_busy() {
    let facts = IbkrFacts::with_bounds(LONG, LONG, usize::MAX, LONG);
    for n in 0..MAX_NAMES {
        let name = format!("S{n}.SMART.USD");
        assert!(facts.facts(&name, || Ok(stock(&name))).is_ok());
    }
    let why = facts.facts("ONEMORE.SMART.USD", || panic!("busy")).expect_err("busy");
    assert!(why.contains(FACTS_BUSY) && why.contains("64 names"), "{why}");
}

/// Two askers of one name make ONE lookup: the second waits on the slot and reads the first's answer.
#[test]
fn concurrent_askers_of_one_name_share_one_lookup() {
    let facts = Arc::new(IbkrFacts::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let (facts, calls) = (Arc::clone(&facts), Arc::clone(&calls));
            std::thread::spawn(move || {
                facts.facts("AAPL.SMART.USD", || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    Ok(stock("AAPL.SMART.USD"))
                })
            })
        })
        .collect();
    for t in threads {
        assert!(t.join().expect("thread").is_ok());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
