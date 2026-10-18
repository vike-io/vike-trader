//! `IgFacts`: the per-epic keep times, the single flight, the fetch budget and the table bound. No
//! network: the fetch is a counting closure.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_gatewayhub_client::proto::{FACTS_BUSY, facts_refusal_is_transient};
use vike_ig::{IgFactsError, IgMarketFacts};
use vike_model::{ContractValue, SymbolProperties};

use super::{IgFacts, MAX_EPICS};

const HOUR: Duration = Duration::from_secs(3600);

fn facts() -> IgMarketFacts {
    IgMarketFacts {
        properties: SymbolProperties {
            tick_size: 0.0001,
            step_size: 0.1,
            min_qty: 0.1,
            ..SymbolProperties::default()
        },
        contract_value: ContractValue::linear(10_000.0),
    }
}

/// What a fetch answers.
type Answer = Result<IgMarketFacts, IgFactsError>;

fn counter() -> (Arc<AtomicUsize>, impl Fn(Answer) -> Answer) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    (calls, move |answer| {
        seen.fetch_add(1, Ordering::SeqCst);
        answer
    })
}

/// An answer carries the grid AND the contract value (IG's is `Some`, never the default), and is kept for
/// the TTL: the second ask is the first's answer and fetches nothing.
#[test]
fn an_answer_is_kept_and_carries_the_contract_value() {
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    let (calls, count) = counter();
    let first = ig.facts("CS.D.EURUSD.MINI.IP", || count(Ok(facts()))).expect("facts");
    assert_eq!(first.properties, facts().properties);
    assert_eq!(first.contract_value, Some(ContractValue::linear(10_000.0)));
    let second = ig.facts("CS.D.EURUSD.MINI.IP", || count(Ok(facts()))).expect("kept");
    assert_eq!(second, first);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ig.facts("CS.D.GBPUSD.MINI.IP", || count(Ok(facts()))).expect("another epic");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "the keep is per epic");
}

/// A stale answer is fetched again; a REFUSAL is kept only for the failed keep time.
#[test]
fn a_stale_answer_and_an_expired_refusal_are_asked_again() {
    let (calls, count) = counter();
    let stale = IgFacts::with_bounds(Duration::ZERO, HOUR, 100, HOUR);
    stale.facts("E", || count(Ok(facts()))).expect("first");
    stale.facts("E", || count(Ok(facts()))).expect("second");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a zero keep time keeps nothing");

    let (calls, count) = counter();
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    for _ in 0..3 {
        let why = ig
            .facts("E", || count(Err(IgFactsError::Refused("ig: no such epic".into()))))
            .expect_err("refused");
        assert_eq!(why, "ig: no such epic");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a refusal is remembered, not re-asked");
}

/// The node remembers a refusal and must NOT remember a "not now": only the second carries the marker the
/// node reads.
#[test]
fn a_failure_to_read_is_worded_as_not_now_and_a_no_is_not() {
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    let busy = ig
        .facts("A", || Err(IgFactsError::Unavailable("ig login: timeout".into())))
        .expect_err("down");
    assert!(busy.starts_with("ig: ") && busy.contains("ig login: timeout"), "{busy}");
    assert!(facts_refusal_is_transient(&busy), "{busy}");
    let no = ig.facts("B", || Err(IgFactsError::Refused("ig: B is an INDICES market".into())));
    assert!(!facts_refusal_is_transient(&no.expect_err("refused")));
}

/// A flood of distinct epics costs IG `budget` fetches per window and no more; the rest are "busy"
/// (not-now), a kept answer costs nothing, and the window rolls.
#[test]
fn the_fetch_budget_bounds_a_flood_of_made_up_epics() {
    let (calls, count) = counter();
    let ig = IgFacts::with_bounds(HOUR, HOUR, 2, Duration::from_millis(150));
    ig.facts("A", || count(Ok(facts()))).expect("A");
    ig.facts("B", || count(Ok(facts()))).expect("B");
    let busy = ig.facts("C", || count(Ok(facts()))).expect_err("over the budget");
    assert!(busy.contains(FACTS_BUSY) && facts_refusal_is_transient(&busy), "{busy}");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "the third never reached IG");
    ig.facts("A", || count(Ok(facts()))).expect("a kept answer costs no budget");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    std::thread::sleep(Duration::from_millis(250));
    ig.facts("C", || count(Ok(facts()))).expect("the window rolled");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// Two askers of one epic share ONE fetch: the second waits on the slot and reads the first's answer.
#[test]
fn concurrent_askers_of_one_epic_share_one_fetch() {
    let ig = Arc::new(IgFacts::with_bounds(HOUR, HOUR, 100, HOUR));
    let calls = Arc::new(AtomicUsize::new(0));
    let askers: Vec<_> = (0..4)
        .map(|_| {
            let (ig, calls) = (Arc::clone(&ig), Arc::clone(&calls));
            std::thread::spawn(move || {
                ig.facts("E", || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(100));
                    Ok(facts())
                })
            })
        })
        .collect();
    for asker in askers {
        asker.join().expect("asker").expect("facts");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// The table is bounded: at the cap a new epic is "busy" while every slot is fresh, and expired slots are
/// dropped to make room.
#[test]
fn the_table_of_epics_is_bounded_and_drops_the_expired() {
    let ig = IgFacts::with_bounds(HOUR, HOUR, usize::MAX, HOUR);
    for n in 0..MAX_EPICS {
        ig.facts(&format!("E{n}"), || Ok(facts())).expect("fits");
    }
    let busy = ig.facts("ONE.TOO.MANY", || Ok(facts())).expect_err("full");
    assert!(busy.contains(FACTS_BUSY), "{busy}");

    let ig = IgFacts::with_bounds(Duration::ZERO, Duration::ZERO, usize::MAX, HOUR);
    for n in 0..=MAX_EPICS {
        ig.facts(&format!("E{n}"), || Ok(facts())).expect("expired slots make room");
    }
}
