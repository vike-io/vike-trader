//! `IgFacts`: the per-epic keep times, the single flight, the fetch budget and the table bound. No
//! network: the fetch is a counting closure.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_gatewayhub_client::proto::{
    FACTS_BUSY, FACTS_RATE_UNAVAILABLE, facts_refusal_is_transient,
};
use vike_ig::{IgFactsError, IgMarketFacts, IgQuotedFacts, QuoteRate, Reading};
use vike_model::{ContractValue, SymbolProperties};

use super::super::keyed_facts::{NO_RATE_KEEP, RATE_READ_FAILED_KEEP, RateMemory};
use super::super::symbol_facts::SESSION_FAILED_TTL;
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
        margin_rate: None,
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

/// IG's own margin rate for an epic rides the wire answer of both asks (the plain one and the converting one), and an
/// epic that states none sends none (the node then margins it at the engine's rate, as before).
///
/// MUTATION PROOF: drop `margin_rate: facts.margin_rate` from `wire` - both asks answer `None`.
#[test]
fn the_epics_own_margin_rate_rides_the_wire() {
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    let stated = IgMarketFacts { margin_rate: Some(0.0333), ..facts() };
    let plain = ig.facts("CS.D.EURUSD.MINI.IP", || Ok(stated)).expect("facts");
    assert_eq!(plain.margin_rate, Some(0.0333));
    let quoted = ig
        .facts_quoted("CS.D.GBPUSD.MINI.IP", || {
            Ok(IgQuotedFacts { facts: stated, quote: None, account_ccy: "USD".into() })
        })
        .expect("quoted facts");
    assert_eq!(quoted.margin_rate, Some(0.0333));
    let silent = ig.facts("CS.D.USDCAD.MINI.IP", || Ok(facts())).expect("facts");
    assert_eq!(silent.margin_rate, None, "an epic that states none sends none");
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

// ---- a symbol quoted in another currency than the account's (decision 0183) ----------------------------

fn yen() -> IgQuotedFacts {
    IgQuotedFacts {
        facts: facts(),
        quote: Some(QuoteRate {
            quote_ccy: "JPY".to_string(),
            account_per_quote: 1.0 / 150.0,
            as_of_ms: 1_700_000_000_123,
            reading: Reading::QuoteRow,
        }),
        account_ccy: "USD".to_string(),
    }
}

/// **The wire carries the rate in the engine's orientation**: a yen is a FRACTION of a dollar, with the moment
/// the market details were asked, beside the grid and IG's contract value. A dollar quote carries no currency
/// and no rate (the node reads that as "the account's own").
#[test]
fn a_quoted_answer_carries_the_currency_the_rate_and_the_moment() {
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    let wire = ig.facts_quoted("CS.D.USDJPY.MINI.IP", || Ok(yen())).expect("converted");
    assert_eq!(wire.quote_ccy.as_deref(), Some("JPY"));
    assert_eq!(wire.quote_rate, Some(1.0 / 150.0));
    assert!(wire.quote_rate.is_some_and(|r| r < 0.01), "dollars per yen, never yen per dollar");
    assert_eq!(wire.rate_as_of_ms, Some(1_700_000_000_123));
    assert_eq!(wire.contract_value, Some(ContractValue::linear(10_000.0)));

    let plain = IgQuotedFacts { facts: facts(), quote: None, account_ccy: "USD".to_string() };
    let wire = ig.facts_quoted("CS.D.EURUSD.MINI.IP", || Ok(plain)).expect("not converted");
    assert_eq!((wire.quote_ccy, wire.quote_rate, wire.rate_as_of_ms), (None, None, None));
}

/// **Every converted answer states the ledger it is expressed in** (`account_ccy`), the account's own money
/// whatever it is (a pound account's yen epic is `GBP` per yen), so the node refuses it against an engine anchored
/// to another currency; the PLAIN ask (a node that does not convert) states none, as it never did.
///
/// MUTATION PROOF: drop `account_ccy` from `wire`'s literal - the converted ask answers `None` and the node's
/// door cannot tell a pound answer from the dollar engine it was never built for.
#[test]
fn a_quoted_answer_states_its_ledger_and_the_plain_one_does_not() {
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    let pounds = IgQuotedFacts { account_ccy: "GBP".to_string(), ..yen() };
    let wire = ig.facts_quoted("CS.D.USDJPY.MINI.IP", || Ok(pounds)).expect("converted");
    assert_eq!(wire.account_ccy.as_deref(), Some("GBP"));
    let own = IgQuotedFacts { facts: facts(), quote: None, account_ccy: "EUR".to_string() };
    let wire =
        ig.facts_quoted("CS.D.EURGBP.MINI.IP", || Ok(own)).expect("in the account's own money");
    assert_eq!((wire.quote_ccy, wire.account_ccy.as_deref()), (None, Some("EUR")));
    let plain = ig.facts("CS.D.EURUSD.MINI.IP", || Ok(facts())).expect("plain");
    assert_eq!(plain.account_ccy, None, "the plain ask states no ledger");
}

/// **A rate is never served from the listing's keep time**: with a keep of an hour a second ask inside the
/// rate's own moment shares the first read (a look and its commit are one read), but with no keep at all every
/// ask reads again.
#[test]
fn a_rate_is_kept_for_a_moment_only() {
    let calls = Arc::new(AtomicUsize::new(0));
    let read = |calls: &Arc<AtomicUsize>| -> Result<IgQuotedFacts, IgFactsError> {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(yen())
    };
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    ig.facts_quoted("Y", || read(&calls)).expect("first");
    ig.facts_quoted("Y", || read(&calls)).expect("a moment later");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "inside the rate keep: one read");

    calls.store(0, Ordering::SeqCst);
    let ig = IgFacts::with_bounds(Duration::ZERO, HOUR, 100, HOUR);
    for _ in 0..3 {
        ig.facts_quoted("Y", || read(&calls)).expect("always asked again");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3, "no keep time keeps no rate");
}

/// The plain ask and the quoted ask of ONE epic are two answers: a node that does not convert is refused
/// the yen epic and a node that does is not, over the same account.
#[test]
fn the_plain_and_the_quoted_answers_of_one_epic_never_answer_for_each_other() {
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    let refused = ig
        .facts("CS.D.USDJPY.MINI.IP", || {
            Err(IgFactsError::Refused(
                "ig: quoted in JPY but this account's currency is USD".into(),
            ))
        })
        .expect_err("the fence");
    assert!(refused.contains("quoted in JPY"));
    ig.facts_quoted("CS.D.USDJPY.MINI.IP", || Ok(yen())).expect("the kept refusal does not answer");
    let again = ig.facts("CS.D.USDJPY.MINI.IP", || panic!("kept")).expect_err("still the fence");
    assert_eq!(again, refused, "...and the converted answer does not unfence the plain ask");
}

/// **A rate IG cannot give, or that no price confirms, is "not now" about the RATE**: the node must not
/// remember it as a verdict on the epic (`FACTS_RATE_UNAVAILABLE` is a marker the node reads), and a hard
/// refusal is not transient.
#[test]
fn no_rate_is_worded_as_a_failure_of_the_rate_and_a_refusal_is_not() {
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    let no_rate = ig
        .facts_quoted("Y", || Err(IgFactsError::NoRate("ig: no reading of IG's table".into())))
        .expect_err("no rate");
    assert!(no_rate.starts_with("ig: ") && no_rate.contains(FACTS_RATE_UNAVAILABLE), "{no_rate}");
    assert!(no_rate.contains("no reading of IG's table"), "{no_rate}");
    assert!(facts_refusal_is_transient(&no_rate), "{no_rate}");
    let no = ig
        .facts_quoted("Z", || Err(IgFactsError::Refused("ig: Z is an INDICES market".into())))
        .expect_err("refused");
    assert!(!facts_refusal_is_transient(&no), "{no}");
}

// ---- a failure on the RATE path is remembered for seconds, never minutes ---------------------------------

/// A quoted read that counts itself and answers `answer`.
fn read_quoted(
    calls: &Arc<AtomicUsize>,
    answer: Result<IgQuotedFacts, IgFactsError>,
) -> Result<IgQuotedFacts, IgFactsError> {
    calls.fetch_add(1, Ordering::SeqCst);
    answer
}

fn brief() -> RateMemory {
    RateMemory { read_failed: Duration::from_millis(40), no_rate: Duration::from_millis(400) }
}

/// **A blip on a rate read does not outlive the node's retry**: a 503 (or a timeout, a closed pair) was served
/// from memory for `failed_ttl` (five minutes) without asking IG again, while the node retried every 10 s and
/// the engine denied opening orders once the rate was 120 s old: about three minutes of denied orders per epic
/// after one blip. Now a failed read is remembered for a moment (a burst of orders shares one failing read),
/// and the retry after it asks IG again and is answered.
#[test]
fn a_failed_rate_read_is_asked_again_after_a_moment_and_not_after_minutes() {
    let calls = Arc::new(AtomicUsize::new(0));
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR).with_rate_memory(brief());
    let down = ig
        .facts_quoted("Y", || {
            read_quoted(
                &calls,
                Err(IgFactsError::Unavailable("ig GET /markets/Y: IG error 503".into())),
            )
        })
        .expect_err("503");
    assert!(down.contains("503") && facts_refusal_is_transient(&down), "{down}");
    ig.facts_quoted("Y", || read_quoted(&calls, Ok(yen())))
        .expect_err("a moment: the same failure");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "inside the memory, IG is not asked again");

    std::thread::sleep(Duration::from_millis(100));
    let ok = ig.facts_quoted("Y", || read_quoted(&calls, Ok(yen()))).expect("IG answers now");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "after the memory, IG is asked again");
    assert_eq!(ok.quote_ccy.as_deref(), Some("JPY"));

    // The plain ask of the same account still remembers a failed read for the failed keep time: there a
    // failure is a login that failed, and repeating it is what that keep time protects.
    let plain = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let seen = Arc::clone(&plain);
        ig.facts("P", || {
            seen.fetch_add(1, Ordering::SeqCst);
            Err(IgFactsError::Unavailable("ig login: timeout".into()))
        })
        .expect_err("down");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(plain.load(Ordering::SeqCst), 1, "the plain path keeps its failure for failed_ttl");
}

/// **A rate nobody can give is remembered longer than a failed read, and still only for seconds**: no listed
/// conversion pair, a shut market that cannot witness the rate, or a table no reading of which agrees cannot
/// change within a moment, but a weekend answer must not hold the Sunday open back for the failed keep time.
#[test]
fn a_rate_nobody_can_give_is_remembered_longer_than_a_failed_read_but_not_for_minutes() {
    let calls = Arc::new(AtomicUsize::new(0));
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR).with_rate_memory(brief());
    let no_rate =
        || read_quoted(&calls, Err(IgFactsError::NoRate("ig: the market is CLOSED".into())));
    let why = ig.facts_quoted("N", no_rate).expect_err("shut");
    assert!(why.contains(FACTS_RATE_UNAVAILABLE), "{why}");
    std::thread::sleep(Duration::from_millis(100));
    ig.facts_quoted("N", no_rate).expect_err("still remembered (longer than a failed read)");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    std::thread::sleep(Duration::from_millis(400));
    ig.facts_quoted("N", || read_quoted(&calls, Ok(yen()))).expect("the market opened");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "asked again once the memory ran out");
}

/// A definitive refusal (not an epic, a fence) keeps the long failed keep time: only the rate path is brief.
#[test]
fn a_refusal_on_the_quoted_door_keeps_the_long_time() {
    let calls = Arc::new(AtomicUsize::new(0));
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR).with_rate_memory(brief());
    for _ in 0..2 {
        ig.facts_quoted("Z", || {
            read_quoted(&calls, Err(IgFactsError::Refused("ig: Z is an INDICES market".into())))
        })
        .expect_err("refused");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// **A failure only a CHANGE cures keeps the long failed time on the quoted door, and a blip still does not**:
/// a refused login (wrong details, a disabled key or account), an account row IG does not list, a currency it
/// does not state. Before, every `Unavailable` of the quoted door was a 5 s "not now", so a caller that kept
/// asking made one fresh login per 5 s per epic (capped at 6-12 a minute), against an account IG may lock on
/// repeated failed logins. The transient classes (a timeout, a 5xx, a 429) are still asked again after a moment.
#[test]
fn a_failure_only_a_change_cures_keeps_the_long_time_on_the_quoted_door_and_a_blip_does_not() {
    for (what, error, reads) in [
        (
            "a refused login",
            IgFactsError::Account("ig login: IG error 401: error.security.invalid-details".into()),
            1,
        ),
        (
            "no account row",
            IgFactsError::Account("ig: GET /accounts lists no account ``".into()),
            1,
        ),
        ("a timeout", IgFactsError::Unavailable("ig login: network error: timed out".into()), 2),
        ("a 503", IgFactsError::Unavailable("ig GET /markets/Y: IG error 503: x".into()), 2),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR).with_rate_memory(brief());
        for _ in 0..2 {
            let why =
                ig.facts_quoted("C", || read_quoted(&calls, Err(error.clone()))).expect_err(what);
            assert!(
                facts_refusal_is_transient(&why),
                "{what}: the node does not remember it: {why}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(calls.load(Ordering::SeqCst), reads, "{what}");
    }
    // the plain door words the same class the same way it always worded a failed login
    let ig = IgFacts::with_bounds(HOUR, HOUR, 100, HOUR);
    let why = ig
        .facts("P", || Err(IgFactsError::Account("ig login: IG error 401: x".into())))
        .expect_err("refused");
    assert!(facts_refusal_is_transient(&why), "{why}");
}

/// The production memory is seconds, strictly below the node's own freshness (a rate is called stale at 120 s)
/// and the long keep time, and a failed read is remembered no longer than the node's retry interval.
#[test]
fn the_default_rate_memory_is_seconds() {
    let memory = RateMemory::default();
    assert_eq!(memory, RateMemory { read_failed: RATE_READ_FAILED_KEEP, no_rate: NO_RATE_KEEP });
    assert!(memory.read_failed <= Duration::from_secs(10), "{memory:?}: the node retries at 10 s");
    assert!(memory.read_failed < memory.no_rate, "{memory:?}");
    assert!(
        memory.no_rate <= Duration::from_secs(10),
        "{memory:?}: the node retries a rate at 10 s (RATE_RETRY_AFTER); a longer memory refuses past the retry"
    );
    assert!(memory.no_rate < SESSION_FAILED_TTL);
}

// ---- the budget keeps a reserve for the refresh of a held position's rate --------------------------------

/// **A flood of first looks cannot starve the refresh**: of a budget of 4, first looks at new epics may spend 2,
/// and the re-read of an epic that has answered with a rate may spend the rest. (The node re-reads every held
/// rate about once a minute; the engine denies opening orders once one is 120 s old.)
#[test]
fn the_refresh_of_a_rated_epic_has_a_reserve_a_flood_of_first_looks_cannot_spend() {
    // a rate is kept for the shorter of the listing keep and `RATE_KEEP`: 30 ms here
    let ig = IgFacts::with_bounds(Duration::from_millis(30), HOUR, 4, HOUR).with_refresh_reserve(2);
    let calls = Arc::new(AtomicUsize::new(0));
    let ask = |epic: &str| ig.facts_quoted(epic, || read_quoted(&calls, Ok(yen())));
    ask("A").expect("first look 1");
    ask("B").expect("first look 2");
    let busy = ask("C").expect_err("a third first look: the reserve is not its to spend");
    assert!(busy.contains(FACTS_BUSY) && facts_refusal_is_transient(&busy), "{busy}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    std::thread::sleep(Duration::from_millis(60));
    ask("A").expect("the refresh of a rated epic spends the reserve");
    ask("B").expect("...both of them");
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    std::thread::sleep(Duration::from_millis(60));
    let busy = ask("A").expect_err("the whole budget is now spent");
    assert!(busy.contains(FACTS_BUSY), "{busy}");
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

/// With no reserve (`with_bounds`, the tests' and every other venue's) the budget is one pool, as it was.
#[test]
fn without_a_reserve_the_budget_is_one_pool() {
    let ig = IgFacts::with_bounds(HOUR, HOUR, 2, HOUR);
    ig.facts_quoted("A", || Ok(yen())).expect("1");
    ig.facts_quoted("B", || Ok(yen())).expect("2");
    ig.facts_quoted("C", || Ok(yen())).expect_err("spent");
}

/// The production default keeps the reserve for the refresh, and half the budget at most.
#[test]
fn the_default_budget_reserves_half_for_the_refresh() {
    use super::{FETCH_BUDGET, RATE_REFRESH_RESERVE};
    assert_eq!(FETCH_BUDGET, 12);
    assert_eq!(RATE_REFRESH_RESERVE, 6);
    let ig = IgFacts::default();
    for n in 0..RATE_REFRESH_RESERVE {
        ig.facts_quoted(&format!("E{n}"), || Ok(yen())).expect("a first look under the reserve");
    }
    ig.facts_quoted("ONE.MORE", || Ok(yen())).expect_err("the other half is the refresh's");
}
