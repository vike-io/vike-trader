//! Tests of `crate::subscription`: what a status means, which rows the bridge subscribes, how the shim's answer
//! reads, and what a session remembers. The rows and the answers are the MEASURED ones
//! (`tests/fixtures/subscription_probe.json`, the demo login on 2026-10-26); nothing here has met a session.

use super::*;
use crate::home_conversion::parse_offers;
use serde_json::Value;

const PROBE: &str = include_str!("../tests/fixtures/subscription_probe.json");

fn demo_offers() -> Vec<Offer> {
    let fixture: Value = serde_json::from_str(PROBE).expect("the fixture is JSON");
    parse_offers(&fixture["offers"].to_string()).expect("the fixture's rows are the shim's shape")
}

fn row(name: &str, status: &str, kind: i32) -> Offer {
    let mut offer = demo_offers().into_iter().find(|o| o.instrument == "EUR/USD").expect("a row");
    offer.instrument = name.to_string();
    offer.subscription = status.to_string();
    offer.instrument_type = kind;
    offer.contract_currency = name.split('/').next().unwrap_or_default().to_string();
    offer
}

/// `T`, `V` and `D` are the venue's three words, and anything else is not guessed at.
#[test]
fn a_status_is_read_as_the_venue_states_it() {
    assert_eq!(Status::parse("T"), Status::Tradable);
    assert_eq!(Status::parse("V"), Status::ViewOnly);
    assert_eq!(Status::parse("D"), Status::Disabled);
    for odd in ["", "t", "TV", "X"] {
        assert_eq!(Status::parse(odd), Status::Other, "{odd:?}");
    }
    assert_eq!(Status::words("V"), "view only (`V`)");
    assert!(Status::words("Q").contains("does not know"));
}

/// The decision: a not-tradable regular forex pair needs the request; a tradable one, and anything the bridge would
/// refuse anyway (a metal, an index, a stock CFD), does not.
#[test]
fn only_a_not_tradable_regular_forex_pair_needs_subscribing() {
    let table = demo_offers();
    let status_of = |name: &str| table.iter().find(|o| o.instrument == name).expect("a row");
    assert!(needs_subscribing(status_of("EUR/USD")), "view only");
    assert!(needs_subscribing(status_of("CHF/JPY")), "disabled");
    assert!(!needs_subscribing(status_of("USD/JPY")), "already tradable");
    assert!(!needs_subscribing(&row("XAU/USD", "D", 2)), "a metal is refused by name further on");
    assert!(!needs_subscribing(&row("GER30", "D", 3)), "an index is not a pair");
    let mut odd = row("EUR/CHF", "D", 1);
    odd.contract_multiplier = 10.0;
    assert!(
        !needs_subscribing(&odd),
        "a forex row with a multiplier is refused by name further on"
    );
}

/// What the demo account needs, row by row: the symbol first, then its ONE conversion pair, nothing twice.
#[test]
fn the_rows_to_subscribe_are_the_symbol_and_its_conversion_pair() {
    let table = demo_offers();
    // EUR account, EUR/USD held view-only: the symbol IS its own conversion pair.
    assert_eq!(wanted(&table, "EURUSD", "EUR"), ["EUR/USD"]);
    // EUR account, USD/JPY already tradable, but its conversion pair EUR/JPY is view-only.
    assert_eq!(wanted(&table, "USDJPY", "EUR"), ["EUR/JPY"]);
    // EUR account, EUR/GBP: quoted in GBP, converted by itself.
    assert_eq!(wanted(&table, "EUR/GBP", "EUR"), ["EUR/GBP"]);
    // A USD account needs no conversion for a USD-quoted pair.
    assert_eq!(wanted(&table, "EURUSD", "USD"), ["EUR/USD"]);
    // A yen cross that is disabled and converts through EUR/JPY: both, symbol first.
    assert_eq!(wanted(&table, "CHFJPY", "EUR"), ["CHF/JPY", "EUR/JPY"]);
    // Not in the table: nothing to subscribe, the facts say so.
    assert!(wanted(&table, "NZDCAD", "EUR").is_empty());
}

/// A quote currency the ledger has no pair with is converted through USD (`crate::home_conversion::quote_rate`), so BOTH
/// legs must be tradable: the symbol first, then the quote currency's USD pair, then the ledger's. A leg FXCM does not
/// list is not invented, and a tradable one is not asked for.
#[test]
fn a_conversion_through_usd_subscribes_both_legs() {
    let rows = |hkd: &str, eur: &str| vec![row("USD/HKD", hkd, 1), row("EUR/USD", eur, 1)];
    // EUR ledger, USDHKD: the symbol IS the quote currency's leg, EUR/USD is the ledger's.
    assert_eq!(wanted(&rows("D", "V"), "USDHKD", "EUR"), ["USD/HKD", "EUR/USD"]);
    assert_eq!(wanted(&rows("T", "V"), "USDHKD", "EUR"), ["EUR/USD"]);
    assert_eq!(wanted(&rows("D", "T"), "USDHKD", "EUR"), ["USD/HKD"]);
    assert!(wanted(&rows("T", "T"), "USDHKD", "EUR").is_empty());
    // A symbol that is neither leg: three rows, in order, once each.
    let cross = vec![row("CHF/HKD", "D", 1), row("USD/HKD", "D", 1), row("EUR/USD", "V", 1)];
    assert_eq!(wanted(&cross, "CHFHKD", "EUR"), ["CHF/HKD", "USD/HKD", "EUR/USD"]);
    // A leg that is not listed is not invented (the facts then name it).
    assert_eq!(wanted(&[row("USD/HKD", "D", 1)], "USDHKD", "EUR"), ["USD/HKD"]);
    // A direct pair listed means no composition: only the symbol and that pair.
    let direct = vec![row("USD/JPY", "D", 1), row("EUR/JPY", "V", 1), row("EUR/USD", "V", 1)];
    assert_eq!(wanted(&direct, "USDJPY", "EUR"), ["USD/JPY", "EUR/JPY"]);
}

/// Everything already tradable, or two listed orientations of the conversion pair and neither tradable (the choice
/// is not made here), asks for nothing beyond the symbol.
#[test]
fn nothing_is_asked_when_nothing_is_needed_or_the_pair_is_ambiguous() {
    let all_tradable: Vec<Offer> = demo_offers()
        .into_iter()
        .map(|mut o| {
            o.subscription = "T".to_string();
            o
        })
        .collect();
    assert!(wanted(&all_tradable, "USDJPY", "EUR").is_empty());

    let both_ways = vec![row("USD/JPY", "T", 1), row("JPY/EUR", "D", 1), row("EUR/JPY", "D", 1)];
    assert!(wanted(&both_ways, "USDJPY", "EUR").is_empty(), "two orientations: no pick");
}

/// The shim's returns, as the operator reads them: FXCM's words verbatim for a refusal, the status read for a row
/// that did not settle, and who is to blame (a verdict, or an outcome nobody knows).
#[test]
fn the_shims_answer_reads_as_a_named_reason() {
    let fixture: Value = serde_json::from_str(PROBE).expect("JSON");
    let answers = fixture["answers"].as_array().expect("answers");
    assert_eq!(answers.len(), 4);
    let read = |i: usize, name: &str| {
        let a = &answers[i];
        answer_from_shim(
            name,
            a["code"].as_i64().expect("code") as i32,
            a["text"].as_str().expect("text"),
        )
    };
    assert_eq!(read(0, "EUR/GBP"), Ok(()));
    assert_eq!(read(1, "EUR/USD"), Ok(()));

    let unbuildable = read(2, "EUR/AUD").unwrap_err();
    assert!(!unbuildable.transient, "the factory refused the request: a verdict");
    assert!(unbuildable.why.contains("OfferID or Symbol is absent."), "{}", unbuildable.why);

    let unsettled = read(3, "NZD/JPY").unwrap_err();
    assert!(unsettled.transient, "the request may have applied: not a verdict");
    assert!(unsettled.why.contains("disabled (`D`)"), "{}", unsettled.why);

    let refused =
        answer_from_shim("EUR/USD", -6, " the account may not subscribe this instrument ")
            .unwrap_err();
    assert!(!refused.transient);
    assert_eq!(
        refused.why,
        "fxcm: FXCM refused to subscribe `EUR/USD` for trading; the venue said: the account may not subscribe this instrument"
    );
    assert!(
        answer_from_shim("X/Y", -3, "")
            .unwrap_err()
            .why
            .contains("not in the venue's Offers table")
    );
    assert!(answer_from_shim("X/Y", -8, "no answer").unwrap_err().transient);
    assert!(answer_from_shim("X/Y", -77, "boom").unwrap_err().why.contains("code -77"));
}

/// A bridge that subscribed a row and finds it undone says so, and does not call it a refusal of FXCM's.
#[test]
fn an_undone_subscription_is_named() {
    let undone = undone_words("EUR/USD", "D");
    assert!(undone.why.contains("changed at the venue since"), "{}", undone.why);
    assert!(undone.why.contains("disabled (`D`)"), "{}", undone.why);
    assert!(!undone.transient);
}

/// **Once per instrument per session, and no retry storm.** A success is final; a refusal is remembered for
/// [`REFUSED_KEEP`] and an unknown outcome for [`UNKNOWN_KEEP`], then asked again.
#[test]
fn a_session_asks_once_and_remembers_how_it_went() {
    let t0 = Instant::now();
    let sent = std::cell::Cell::new(0);
    // `result` is what the shim would answer IF it were asked.
    let go = |memory: &mut Attempts, at: Instant, result: Result<(), Failure>| {
        ensure(memory, "EUR/USD", at, || {
            sent.set(sent.get() + 1);
            result
        })
    };

    // A success is final.
    let mut memory = Attempts::default();
    assert_eq!(go(&mut memory, t0, Ok(())), Ok(()));
    let never = Err(Failure { why: "never asked".into(), transient: false });
    assert_eq!(go(&mut memory, t0 + Duration::from_secs(86_400), never), Ok(()));
    assert_eq!(sent.get(), 1);

    // A refusal is remembered for REFUSED_KEEP.
    let mut memory = Attempts::default();
    sent.set(0);
    let no = Failure { why: "FXCM said no".into(), transient: false };
    assert_eq!(go(&mut memory, t0, Err(no.clone())), Err(no.clone()));
    assert_eq!(go(&mut memory, t0 + REFUSED_KEEP - Duration::from_secs(2), Ok(())), Err(no));
    assert_eq!(sent.get(), 1, "not asked again inside the memory");
    assert_eq!(
        go(&mut memory, t0 + REFUSED_KEEP + Duration::from_secs(1), Ok(())),
        Ok(()),
        "asked again once it has run out"
    );
    assert_eq!(sent.get(), 2);

    // An unknown outcome is asked again much sooner.
    let mut memory = Attempts::default();
    sent.set(0);
    let unknown = Failure { why: "no answer".into(), transient: true };
    assert_eq!(go(&mut memory, t0, Err(unknown.clone())), Err(unknown.clone()));
    assert_eq!(go(&mut memory, t0 + UNKNOWN_KEEP - Duration::from_secs(2), Ok(())), Err(unknown));
    assert_eq!(go(&mut memory, t0 + UNKNOWN_KEEP + Duration::from_secs(1), Ok(())), Ok(()));
    assert_eq!(sent.get(), 2);
}

/// The request the shim builds is `Command`, `OfferID`, `SubscriptionStatus` and nothing else, and the bridge asks
/// for `T` only: both are pinned here (the first by `tests/fxcm_shim_subscription.rs`, which reads the C++).
#[test]
fn the_bridge_asks_for_tradable_and_only_that() {
    assert_eq!(TRADABLE, "T");
    assert_eq!(Status::parse(TRADABLE), Status::Tradable);
}

/// **The code mapping that decides who forgets a failure.** `-8` is the shim's code for EVERY failed lookup and for a
/// missing answer: never a verdict, so a reconnect blip on the first order for a symbol does not refuse it for
/// [`REFUSED_KEEP`] (10 minutes) while its row is `T`. `-4`/`-5` are the SEND stage's codes (the request could not be
/// built) and stay a verdict, as do `-3`, `-6` and `-14`. Every other code is unknown, not a verdict.
#[test]
fn each_shim_code_is_a_verdict_or_not_as_the_shim_means_it() {
    let transient =
        |code: i32| answer_from_shim("EUR/USD", code, "why").expect_err("a failure").transient;
    assert!(transient(-8), "a failed lookup or a lost answer is not a verdict");
    assert!(transient(-9), "a row that did not settle is not a verdict");
    assert!(transient(-77), "an unknown code is not a verdict");
    for verdict in [-3, -4, -5, -6, -14, -16] {
        assert!(!transient(verdict), "{verdict} is a verdict on the symbol");
    }
    assert!(answer_from_shim("EUR/USD", 0, "T").is_ok());
}

/// **An account that may not create orders on a pair is told so, by name, and the pair is not subscribed for nothing**
/// (`-16`, the shim's permission read; USD/HKD on the demo login, `tests/fixtures/order_permission_probe.json`). A verdict
/// the node remembers: ordering again would be refused again, and an order on it must NOT be placed "anyway" the way
/// an unknown outcome is.
#[test]
fn an_instrument_the_account_may_not_trade_is_refused_by_name() {
    let fixture: Value =
        serde_json::from_str(include_str!("../tests/fixtures/order_permission_probe.json"))
            .expect("JSON");
    let denied = fixture["rows_with_order_creation_disabled"][0].as_str().expect("a denied row");
    let failure = answer_from_shim(
        denied,
        -16,
        "FXCM's permission to create a market order and an entry order on it both read Disabled",
    )
    .expect_err("refused");
    assert!(!failure.transient, "a verdict, so the exec path ends the order here");
    assert!(
        failure.why.contains("the account may not trade `USD/HKD`")
            && failure.why.contains("both read Disabled"),
        "{}",
        failure.why
    );
    assert!(
        failure.why.contains("does not subscribe"),
        "it says why nothing was written: {}",
        failure.why
    );
    assert!(!failure.why.contains("Access Violation"), "no bare venue text: {}", failure.why);
}

/// A metal, an index or a CFD that is not tradable is refused in words that say the bridge subscribes regular forex
/// pairs only (`-14`, with the shim's type/multiplier/status text), a verdict the node remembers.
#[test]
fn a_non_forex_instrument_is_refused_by_name_not_subscribed() {
    let failure =
        answer_from_shim("XAU/USD", -14, "type 5, contract multiplier 100, subscription `D`")
            .expect_err("refused");
    assert!(!failure.transient);
    assert!(failure.why.contains("is not a regular forex instrument"), "{}", failure.why);
    assert!(failure.why.contains("type 5, contract multiplier 100"), "{}", failure.why);
}

/// A session writes a subscription on the `Demo` connection only, and only with a shim that can: the unvalidated
/// `venue.fxcm.demo.connection` setting can never make a `Real` session write. Either gap is said in words.
#[test]
fn a_subscription_is_written_on_the_demo_connection_with_a_capable_shim_only() {
    assert_eq!(subscribe_gap("Demo", None), None);
    let real = subscribe_gap("Real", None).expect("Real never writes");
    assert!(real.contains("`Real`") && real.contains("`Demo` only"), "{real}");
    for odd in ["demo", "DEMO", "", "Demo "] {
        assert!(subscribe_gap(odd, None).is_some(), "{odd:?} is not the Demo connection");
    }
    let old = subscribe_gap("Demo", Some("/var/lib/vike/lib/libfcshim.so")).expect("an older shim");
    assert!(
        old.contains("/var/lib/vike/lib/libfcshim.so")
            && old.contains("exports no `fc_set_subscription`"),
        "{old}"
    );
    assert!(
        old.contains(
            "deploy installs the release's shim once the root-owned deploy helper is reinstalled"
        ),
        "{old}"
    );
    // The connection gap comes first: a Real session with an old shim is told about the connection.
    assert!(subscribe_gap("Real", Some("x")).expect("gap").contains("`Real`"));
}
