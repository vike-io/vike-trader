//! Tests of `crate::reads`: the account's currency decided once, the facts built from one Offers read on
//! the told currency, and the registry the gatewayhub's lane reaches a running session through. The
//! session itself is SCRIPTED (docs-shaped answers; nothing here has met a ForexConnect session).

use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A session that answers a script and counts how often it was asked.
struct Scripted {
    props: PropertiesRead,
    offers: Result<String, String>,
    property_reads: AtomicUsize,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Scripted {
    fn new(props: PropertiesRead, offers: Result<String, String>) -> Arc<Self> {
        Arc::new(Self { props, offers, property_reads: AtomicUsize::new(0), log: Arc::default() })
    }
}

impl SessionReads for Scripted {
    fn system_properties(&self) -> PropertiesRead {
        self.property_reads.fetch_add(1, Ordering::SeqCst);
        self.log.lock().unwrap().push("properties");
        self.props.clone()
    }
    fn offers(&self) -> Result<String, String> {
        self.log.lock().unwrap().push("offers");
        self.offers.clone()
    }
}

fn row(name: &str, bid: f64, ask: f64, digits: i64) -> serde_json::Value {
    json!({
        "instrument": name, "bid": bid, "ask": ask, "digits": digits, "instrument_type": 1,
        "contract_currency": name.split('/').next().unwrap_or_default(), "contract_multiplier": 1.0,
        "subscription": "T", "trading_status": "O", "bid_tradable": "T", "ask_tradable": "T",
        "base_unit": 1000, "quote_age_ms": 0
    })
}

fn table() -> String {
    serde_json::Value::Array(vec![
        row("EUR/USD", 1.0998, 1.1002, 5),
        row("USD/JPY", 150.08, 150.12, 3),
        row("EUR/JPY", 164.98, 165.02, 3),
        row("GBP/USD", 1.3498, 1.3502, 5),
    ])
    .to_string()
}

fn stated(code: &str) -> PropertiesRead {
    PropertiesRead::Json(json!({ "BASE_CRNCY": code }).to_string())
}

/// The currency is read from the session ONCE and kept: a rate and a start's `account_ccy` are therefore
/// expressed in the same currency, and the venue is not asked per request.
#[test]
fn the_currency_is_decided_once_and_kept() {
    let session = Scripted::new(stated("EUR"), Ok(table()));
    let reads = AccountReads::new(session.clone());
    let first = reads.account_currency().expect("a currency");
    assert_eq!(first.code, "EUR");
    assert!(!first.is_assumed());
    assert_eq!(reads.account_currency().unwrap(), first);
    reads.quoted_facts("EURUSD", &|| 1).expect("facts");
    assert_eq!(session.property_reads.load(Ordering::SeqCst), 1, "asked the session once");
}

/// **An API-reported EUR account is converted in EUR: no fallback.** USD-quoted EUR/USD on a EUR account is
/// `1 / 1.10` EUR per dollar, and a yen cross is `1 / 165`.
#[test]
fn a_reported_eur_account_is_converted_in_eur() {
    let reads = AccountReads::new(Scripted::new(stated("EUR"), Ok(table())));
    let usd_quoted = reads.quoted_facts("EURUSD", &|| 5).unwrap().quote.expect("a rate");
    assert_eq!(usd_quoted.quote_ccy, "USD");
    assert!((usd_quoted.account_per_quote.get() - 1.0 / 1.10).abs() < 1e-12);
    let yen = reads.quoted_facts("EUR/JPY", &|| 5).unwrap().quote.expect("a rate");
    assert_eq!(yen.quote_ccy, "JPY");
    assert!((yen.account_per_quote.get() - 1.0 / 165.0).abs() < 1e-12);
}

/// **The API is silent: USD, ASSUMED, and a yen pair still converts through the conversion pair** with the
/// orientation intact.
#[test]
fn a_silent_api_assumes_usd_and_a_yen_pair_still_converts() {
    let session = Scripted::new(PropertiesRead::Json("{}".to_string()), Ok(table()));
    let reads = AccountReads::new(session);
    let told = reads.account_currency().unwrap();
    assert_eq!((told.code.as_str(), told.is_assumed()), ("USD", true));
    let facts = reads.quoted_facts("USDJPY", &|| 5).unwrap();
    let quote = facts.quote.expect("a rate");
    assert_eq!(quote.quote_ccy, "JPY");
    assert!((quote.account_per_quote.get() - 1.0 / 150.10).abs() < 1e-15);
    assert!((facts.properties.tick_size - 0.001).abs() < 1e-15);
}

/// The rate is stamped with the moment it was ASKED: the clock is read BEFORE the request goes out, so the
/// stamp is never newer than the answer.
#[test]
fn the_rate_is_stamped_before_the_table_is_read() {
    let session = Scripted::new(stated("USD"), Ok(table()));
    let log = Arc::clone(&session.log);
    let reads = AccountReads::new(session);
    reads.account_currency().unwrap();
    log.lock().unwrap().clear();
    let clock_log = Arc::clone(&log);
    let ask = move || {
        clock_log.lock().unwrap().push("clock");
        1_234
    };
    let quote = reads.quoted_facts("USDJPY", &ask).unwrap().quote.unwrap();
    assert_eq!(quote.as_of_ms, 1_234);
    assert_eq!(*log.lock().unwrap(), ["clock", "offers"], "the clock is read first");
}

/// A table that cannot be read, or a statement that is not a currency, refuses in the venue's words.
#[test]
fn an_unreadable_table_or_a_malformed_currency_refuses() {
    let down =
        AccountReads::new(Scripted::new(stated("USD"), Err("the session is gone".to_string())));
    let gone = down.quoted_facts("USDJPY", &|| 1).unwrap_err();
    assert_eq!(gone.to_string(), "the session is gone");
    assert!(
        gone.is_rate(),
        "an unreadable table is the rate's failure, not a verdict on the symbol"
    );

    let garbled = AccountReads::new(Scripted::new(stated("USD"), Ok("not json".to_string())));
    assert!(garbled.quoted_facts("USDJPY", &|| 1).unwrap_err().to_string().contains("not JSON"));

    let bad = Scripted::new(stated("US$"), Ok(table()));
    let reads = AccountReads::new(bad.clone());
    let refused = reads.quoted_facts("USDJPY", &|| 1).unwrap_err();
    assert!(refused.to_string().contains("three-letter") && refused.is_rate(), "{refused}");
    // A malformed statement is remembered as a refusal for a while ([`MALFORMED_KEEP`]): a venue that states
    // `US$` states it again, and every start re-reading it would run under the adapter lock.
    let _ = reads.account_currency();
    assert_eq!(bad.property_reads.load(Ordering::SeqCst), 1);
}

/// Asked again once the refusal's memory has passed (the venue may have been fixed), and never cached forever.
#[test]
fn a_malformed_currency_is_remembered_for_a_while_and_then_asked_again() {
    let bad = Scripted::new(stated("US$"), Ok(table()));
    let remembers = AccountReads::new(bad.clone());
    let first = remembers.account_currency().unwrap_err();
    for _ in 0..3 {
        assert_eq!(remembers.account_currency().unwrap_err(), first);
    }
    assert_eq!(bad.property_reads.load(Ordering::SeqCst), 1, "four asks, one read");

    let forgets = AccountReads::with_keeps(bad.clone(), Duration::ZERO, UNANSWERED_KEEP);
    let _ = forgets.account_currency();
    let _ = forgets.account_currency();
    assert_eq!(bad.property_reads.load(Ordering::SeqCst), 3, "a lapsed memory asks again");
}

/// A session that answers its properties one script entry at a time.
struct Sequenced {
    answers: Mutex<Vec<PropertiesRead>>,
    reads: AtomicUsize,
}

impl SessionReads for Sequenced {
    fn system_properties(&self) -> PropertiesRead {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.answers.lock().unwrap().remove(0)
    }
    fn offers(&self) -> Result<String, String> {
        Err("no table".to_string())
    }
}

fn sequenced(answers: Vec<PropertiesRead>) -> Arc<Sequenced> {
    Arc::new(Sequenced { answers: Mutex::new(answers), reads: AtomicUsize::new(0) })
}

/// **A session that did not ANSWER decides nothing.** A timeout is not the API's silence, so it is not the
/// ruling's assumed USD (which would then be kept for the registration's life even if the venue states EUR):
/// it is a refusal, remembered briefly so a stuck session costs one wait and not one per start, and the next
/// ask after that gets the venue's real word.
#[test]
fn an_unanswered_ask_is_not_an_assumption_and_the_next_ask_gets_the_real_word() {
    let timed_out = || PropertiesRead::Failed("did not answer within 15 s".to_string());

    let session = sequenced(vec![timed_out(), stated("EUR")]);
    let reads = AccountReads::new(session.clone());
    let why = reads.account_currency().unwrap_err();
    assert!(why.contains("did not answer") && why.contains("not assumed"), "{why}");
    assert_eq!(reads.account_currency().unwrap_err(), why, "remembered for a moment");
    assert_eq!(session.reads.load(Ordering::SeqCst), 1, "a stuck session costs one wait");

    let session = sequenced(vec![timed_out(), stated("EUR")]);
    let reads = AccountReads::with_keeps(session.clone(), MALFORMED_KEEP, Duration::ZERO);
    assert!(reads.account_currency().is_err());
    let told = reads.account_currency().expect("the second ask is answered");
    assert_eq!((told.code.as_str(), told.is_assumed()), ("EUR", false));

    // The facts path is the same refusal, and it is the RATE's failure (the node forgets it).
    let session = sequenced(vec![timed_out()]);
    let facts = AccountReads::new(session).quoted_facts("USDJPY", &|| 1).unwrap_err();
    assert!(facts.is_rate(), "{facts}");
}

/// What the API cannot state is still the ruling's USD, ASSUMED, decided once (an old shim never learns the
/// export mid-registration).
#[test]
fn an_api_that_cannot_state_it_is_still_assumed_usd_once() {
    let session = sequenced(vec![PropertiesRead::Unreadable("the shim exports no export".into())]);
    let reads = AccountReads::new(session.clone());
    for _ in 0..3 {
        let told = reads.account_currency().unwrap();
        assert_eq!((told.code.as_str(), told.is_assumed()), ("USD", true));
    }
    assert_eq!(session.reads.load(Ordering::SeqCst), 1);
}

/// A session whose property read parks until the test lets it go: a mute session.
struct Parked {
    entered: AtomicUsize,
    open: (Mutex<bool>, std::sync::Condvar),
}

impl SessionReads for Parked {
    fn system_properties(&self) -> PropertiesRead {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let mut open = self.open.0.lock().unwrap();
        while !*open {
            open = self.open.1.wait(open).unwrap();
        }
        stated("EUR")
    }
    fn offers(&self) -> Result<String, String> {
        Err("parked".to_string())
    }
}

/// **The `told` lock is not held across the session's wait.** With a session that never answers, a second asker
/// must still reach the session (and, in production, its own deadline) instead of queueing on a lock behind the
/// first for however long the first waits.
#[test]
fn a_mute_session_does_not_hold_the_currency_lock_across_its_wait() {
    let session = Arc::new(Parked {
        entered: AtomicUsize::new(0),
        open: (Mutex::new(false), std::sync::Condvar::new()),
    });
    let reads = Arc::new(AccountReads::new(session.clone()));
    let askers: Vec<_> = (0..2)
        .map(|_| {
            let reads = Arc::clone(&reads);
            std::thread::spawn(move || reads.account_currency())
        })
        .collect();
    let give_up = Instant::now() + Duration::from_secs(10);
    while session.entered.load(Ordering::SeqCst) < 2 && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(5));
    }
    let entered = session.entered.load(Ordering::SeqCst);
    // Let go BEFORE asserting, so a failure cannot leave the askers parked for ever.
    *session.open.0.lock().unwrap() = true;
    session.open.1.notify_all();
    for asker in askers {
        assert_eq!(asker.join().unwrap().unwrap().code, "EUR", "every asker is answered");
    }
    assert_eq!(entered, 2, "the second asker reached the session while the first was waiting");
}

/// **The words tell "the venue stated none" from "the venue was never asked"** (finding 6 of the second review).
/// With reconciliation off no session is registered, nothing asks the venue, and the code goes straight to the
/// assumption: saying "not reported by the venue" there would claim an answer nobody sought.
#[test]
fn the_assumption_says_whether_the_venue_was_asked() {
    let asked = assumed_words("USD", "the venue's system properties carry no BASE_CRNCY", true);
    assert!(
        asked.contains("not reported by the venue") && asked.contains("assuming USD"),
        "{asked}"
    );
    assert!(!asked.contains("NOT ASKED"), "{asked}");

    let unasked =
        assumed_words("USD", "no reconcile session is running: reconciliation is off", false);
    assert!(unasked.contains("NOT ASKED") && unasked.contains("never asked"), "{unasked}");
    assert!(
        unasked.contains("reconciliation is off") && unasked.contains("USD is assumed"),
        "{unasked}"
    );
    assert!(!unasked.contains("not reported by the venue"), "{unasked}");
    for words in [&asked, &unasked] {
        assert!(
            words.contains("policy.max_account_exposure"),
            "the consequence is stated: {words}"
        );
    }
}

// ---- the registry (unique keys: the registry is process-wide and the tests run in parallel) -----------

#[test]
fn an_account_with_no_session_is_assumed_usd_and_has_no_rate() {
    let told = told_account_currency("reads-test/none").expect("the ruling's fallback");
    assert_eq!((told.code.as_str(), told.is_assumed()), ("USD", true));
    let why = quoted_facts("reads-test/none", "USDJPY").unwrap_err();
    assert!(why.to_string().contains("no reconcile session") && why.is_rate(), "{why}");
}

#[test]
fn a_registered_session_answers_for_its_account_only() {
    let eur = register("reads-test/eur", Scripted::new(stated("EUR"), Ok(table())));
    let usd = register("reads-test/usd", Scripted::new(stated("USD"), Ok(table())));
    assert_eq!(told_account_currency("reads-test/eur").unwrap().code, "EUR");
    assert_eq!(told_account_currency("reads-test/usd").unwrap().code, "USD");
    assert!(quoted_facts("reads-test/eur", "EURUSD").is_ok());
    unregister("reads-test/eur", &eur);
    unregister("reads-test/usd", &usd);
    assert!(account_reads("reads-test/eur").is_none());
    assert!(
        told_account_currency("reads-test/eur").unwrap().is_assumed(),
        "gone: back to the ruling"
    );
}

/// A restart registers its successor BEFORE the old session is dropped: the old session's unregister must
/// not take the new one with it.
#[test]
fn an_old_sessions_unregister_leaves_its_successor() {
    let old = register("reads-test/restart", Scripted::new(stated("USD"), Ok(table())));
    let new = register("reads-test/restart", Scripted::new(stated("EUR"), Ok(table())));
    unregister("reads-test/restart", &old);
    assert_eq!(told_account_currency("reads-test/restart").unwrap().code, "EUR");
    unregister("reads-test/restart", &new);
    assert!(account_reads("reads-test/restart").is_none());
}

/// **A session that opens LATE does not take its successor's place.** The gatewayhub joins a replaced
/// adapter for 10 s and a login takes up to 30 s, so a retry of an OLD start can open after the NEW start
/// registered. Its generation is the older, so the registry refuses it, the successor keeps its entry, and
/// the old client's later drop (which unregisters only its own entry) cannot empty the registry under the
/// live session. A start that is genuinely newer still replaces, and an empty registry accepts anyone.
#[test]
fn a_late_session_of_an_older_start_never_replaces_a_newer_one() {
    let key = "reads-test/generation";
    let (older, newer) = (next_generation(), next_generation());
    assert!(older < newer, "generations only grow");

    let live = register_generation(key, Scripted::new(stated("GBP"), Ok(table())), newer)
        .expect("an empty registry accepts a start");
    let late = register_generation(key, Scripted::new(stated("USD"), Ok(table())), older);
    assert!(late.is_none(), "the older start's late session must not replace the newer one");
    assert_eq!(told_account_currency(key).unwrap().code, "GBP", "the successor still answers");

    let newest =
        register_generation(key, Scripted::new(stated("EUR"), Ok(table())), next_generation())
            .expect("a newer start replaces");
    assert_eq!(told_account_currency(key).unwrap().code, "EUR");
    unregister(key, &live); // the replaced one's drop leaves its successor
    assert_eq!(told_account_currency(key).unwrap().code, "EUR");
    unregister(key, &newest);
    assert!(account_reads(key).is_none());
    let alone = register_generation(key, Scripted::new(stated("USD"), Ok(table())), older)
        .expect("an old start may register when nobody newer holds the account");
    unregister(key, &alone);
}

// ---- the instrument subscription (the bridge makes a symbol tradable itself) -----------------------

/// A session whose Offers table CHANGES when it is asked to subscribe, as FXCM's does (measured: the row reads `T` at
/// once, on every session of the login).
struct Subscribing {
    rows: Mutex<Vec<serde_json::Value>>,
    answer: Mutex<Result<(), Failure>>,
    asked: Mutex<Vec<String>>,
}

impl Subscribing {
    fn new(rows: Vec<serde_json::Value>, answer: Result<(), Failure>) -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(rows),
            answer: Mutex::new(answer),
            asked: Mutex::default(),
        })
    }
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
    /// Someone changes a row at the venue (the FXCM platform).
    fn set(&self, name: &str, status: &str) {
        for r in self.rows.lock().unwrap().iter_mut().filter(|r| r["instrument"] == name) {
            r["subscription"] = json!(status);
        }
    }
}

impl SessionReads for Subscribing {
    fn system_properties(&self) -> PropertiesRead {
        stated("EUR")
    }
    fn offers(&self) -> Result<String, String> {
        Ok(serde_json::Value::Array(self.rows.lock().unwrap().clone()).to_string())
    }
    fn subscribe(&self, instrument: &str) -> Result<(), Failure> {
        self.asked.lock().unwrap().push(instrument.to_string());
        let answer = self.answer.lock().unwrap().clone();
        if answer.is_ok() {
            self.set(instrument, "T");
        }
        answer
    }
}

fn with_status(name: &str, status: &str) -> serde_json::Value {
    let mut r = row(name, 1.1, 1.1002, 5);
    r["subscription"] = json!(status);
    r
}

fn refusal(why: &str, transient: bool) -> Failure {
    Failure { why: why.to_string(), transient }
}

/// **The measured demo case**: a EUR account, EUR/USD held view-only. The symbol is subscribed BEFORE it is judged
/// and the facts come out on the table that shows it; a second ask sends nothing.
#[test]
fn a_view_only_symbol_is_subscribed_then_admitted() {
    let session = Subscribing::new(vec![with_status("EUR/USD", "V")], Ok(()));
    let reads = AccountReads::new(session.clone());
    let facts = reads.quoted_facts("EURUSD", &|| 5).expect("facts after the subscription");
    assert_eq!(facts.quote.expect("USD is not EUR").quote_ccy, "USD");
    assert_eq!(session.asked(), ["EUR/USD"]);
    reads.quoted_facts("EURUSD", &|| 6).expect("facts again");
    assert_eq!(session.asked(), ["EUR/USD"], "once per instrument per session");
}

/// A symbol quoted in another currency also needs its CONVERSION pair tradable: `USD/JPY` on a EUR account
/// converts through `EUR/JPY`, held view-only, and that is subscribed too.
#[test]
fn the_conversion_pair_is_subscribed_with_the_symbol() {
    let session = Subscribing::new(
        vec![with_status("USD/JPY", "D"), with_status("EUR/JPY", "V"), with_status("EUR/USD", "T")],
        Ok(()),
    );
    let reads = AccountReads::new(session.clone());
    reads.quoted_facts("USDJPY", &|| 5).expect("facts");
    assert_eq!(
        session.asked(),
        ["USD/JPY", "EUR/JPY"],
        "the symbol first, then its conversion pair"
    );
}

/// **The measured defect (the EUR demo, v0.1.93)**: `USDHKD` on a EUR ledger was refused ("fxcm lists no HKD/EUR or
/// EUR/HKD pair"). FXCM lists no euro pair of the Hong Kong dollar, so its rate is made through USD, and both legs
/// (USD/HKD, the symbol itself, and EUR/USD, which FXCM holds view-only on a EUR login) are subscribed first.
#[test]
fn a_pair_whose_quote_has_no_pair_of_the_ledger_is_admitted_through_usd() {
    let leg = |name: &str, bid: f64, ask: f64, status: &str| {
        let mut r = row(name, bid, ask, 5);
        r["subscription"] = json!(status);
        r
    };
    let session = Subscribing::new(
        vec![leg("USD/HKD", 7.8442, 7.8446, "D"), leg("EUR/USD", 1.1178, 1.1182, "V")],
        Ok(()),
    );
    let reads = AccountReads::new(session.clone());
    let facts = reads.quoted_facts("USDHKD", &|| 5).expect("facts once both legs are tradable");
    let quote = facts.quote.expect("HKD is not the ledger's EUR");
    assert_eq!(quote.quote_ccy, "HKD");
    assert!((quote.account_per_quote.get() - (1.0 / 7.8444) * (1.0 / 1.118)).abs() < 1e-12);
    assert_eq!(facts.account_ccy, "EUR");
    assert_eq!(session.asked(), ["USD/HKD", "EUR/USD"], "the symbol first, then the ledger's leg");
    reads.quoted_facts("USDHKD", &|| 6).expect("facts again");
    assert_eq!(session.asked().len(), 2, "once per instrument per session");
}

/// **A refusal is NAMED**: FXCM's words ride on the verdict instead of the bare "not subscribed", the verdict
/// stays a verdict on the symbol, and the request is not repeated.
#[test]
fn a_refusal_carries_what_fxcm_said_and_is_not_repeated() {
    let session = Subscribing::new(
        vec![with_status("EUR/USD", "D")],
        Err(refusal(
            "fxcm: FXCM refused to subscribe `EUR/USD` for trading; the venue said: not permitted",
            false,
        )),
    );
    let reads = AccountReads::new(session.clone());
    let Err(FactsError::Symbol(why)) = reads.quoted_facts("EURUSD", &|| 5) else {
        panic!("a refusal of the symbol")
    };
    assert!(why.contains("is not subscribed for trading"), "{why}");
    assert!(why.contains("the venue said: not permitted"), "{why}");
    let again = reads.quoted_facts("EURUSD", &|| 6).unwrap_err();
    assert!(again.message().contains("the venue said: not permitted"), "{again}");
    assert_eq!(session.asked().len(), 1, "no retry storm");
}

/// An UNKNOWN outcome (no answer, a row that did not settle) is not a verdict on the symbol: the node forgets it.
#[test]
fn an_unknown_outcome_is_a_rate_failure_the_node_forgets() {
    let session = Subscribing::new(
        vec![with_status("EUR/USD", "V")],
        Err(refusal(
            "fxcm: the subscription of `EUR/USD` is not known to have applied: no answer",
            true,
        )),
    );
    let reads = AccountReads::new(session);
    let error = reads.quoted_facts("EURUSD", &|| 5).unwrap_err();
    assert!(error.is_rate(), "{error}");
    assert!(error.message().contains("not known to have applied"), "{error}");
}

/// A session that cannot subscribe (the default) leaves the refusal in words, as before.
#[test]
fn a_session_that_cannot_subscribe_keeps_the_refusal_in_words() {
    struct Plain(Mutex<Vec<serde_json::Value>>);
    impl SessionReads for Plain {
        fn system_properties(&self) -> PropertiesRead {
            stated("EUR")
        }
        fn offers(&self) -> Result<String, String> {
            Ok(serde_json::Value::Array(self.0.lock().unwrap().clone()).to_string())
        }
    }
    let reads = AccountReads::new(Arc::new(Plain(Mutex::new(vec![with_status("EUR/USD", "V")]))));
    let error = reads.quoted_facts("EURUSD", &|| 5).unwrap_err();
    assert!(error.message().contains("not subscribed for trading"), "{error}");
    assert!(error.message().contains("cannot subscribe"), "{error}");
}

/// **A person who un-subscribes what the bridge subscribed is not fought**: the row reads not tradable again,
/// nothing is sent a second time, and the words say it was changed at the venue.
#[test]
fn a_row_changed_at_the_venue_afterwards_is_not_subscribed_again() {
    let session = Subscribing::new(vec![with_status("EUR/USD", "V")], Ok(()));
    let reads = AccountReads::new(session.clone());
    reads.quoted_facts("EURUSD", &|| 5).expect("subscribed");
    session.set("EUR/USD", "D");
    let error = reads.quoted_facts("EURUSD", &|| 6).unwrap_err();
    assert!(error.message().contains("changed at the venue since"), "{error}");
    assert_eq!(session.asked(), ["EUR/USD"]);
}

/// A CFD (a metal, an index, a stock) is refused by name further on, and no venue-side write is made for it.
#[test]
fn an_instrument_the_bridge_would_refuse_anyway_is_never_subscribed() {
    let mut metal = with_status("XAU/USD", "D");
    metal["instrument_type"] = json!(2);
    let session = Subscribing::new(vec![metal], Ok(()));
    let reads = AccountReads::new(session.clone());
    let error = reads.quoted_facts("XAUUSD", &|| 5).unwrap_err();
    assert!(error.message().contains("not subscribed for trading"), "{error}");
    assert!(error.message().contains("not a regular forex instrument"), "{error}");
    assert!(session.asked().is_empty(), "no write for an instrument that is refused anyway");
}

/// The PRICE lane's read subscribes the symbols it is asked to price, and returns the table that shows them.
#[test]
fn the_price_lane_subscribes_the_symbols_it_is_asked_for() {
    let session =
        Subscribing::new(vec![with_status("USD/CHF", "D"), with_status("GBP/USD", "T")], Ok(()));
    let reads = AccountReads::new(session.clone());
    let offers =
        reads.priced_offers(&["USDCHF".to_string(), "GBPUSD".to_string()]).expect("a table");
    assert_eq!(session.asked(), ["USD/CHF"], "only the one that was not tradable");
    assert!(offers.iter().all(|o| o.subscription == "T"));
}

/// A session whose Offers table is read through a SEQUENCE of answers (the last repeats): the first read is what an
/// asker saw BEFORE another asker's subscription landed, the next what it sees AFTER.
struct Overtaken {
    tables: Vec<Vec<serde_json::Value>>,
    reads: AtomicUsize,
    asked: Mutex<Vec<String>>,
}

impl SessionReads for Overtaken {
    fn system_properties(&self) -> PropertiesRead {
        stated("EUR")
    }
    fn offers(&self) -> Result<String, String> {
        let n = self.reads.fetch_add(1, Ordering::SeqCst).min(self.tables.len() - 1);
        Ok(serde_json::Value::Array(self.tables[n].clone()).to_string())
    }
    fn subscribe(&self, instrument: &str) -> Result<(), Failure> {
        self.asked.lock().unwrap().push(instrument.to_string());
        Ok(())
    }
}

/// **The race: another asker subscribes a row between this asker's table read and its turn at the memory.** This asker
/// holds a table that says `V`, finds the instrument already DONE, and used to call it "changed at the venue since"
/// from its stale table, a definite refusal of a row that is `T`. It now reads the table once more and judges the
/// FRESH row, so a subscription made a moment ago is not mistaken for a person's un-subscribe.
#[test]
fn a_subscription_another_asker_made_after_this_table_was_read_is_not_called_undone() {
    let session = Arc::new(Overtaken {
        tables: vec![vec![with_status("EUR/USD", "V")], vec![with_status("EUR/USD", "T")]],
        reads: AtomicUsize::new(0),
        asked: Mutex::default(),
    });
    let reads = AccountReads::new(session.clone());
    // Asker A's subscription, recorded in the memory before this asker takes its turn.
    reads.attempts.lock().unwrap().record("EUR/USD", &Ok(()), Instant::now());
    let facts =
        reads.quoted_facts("EURUSD", &|| 5).expect("the facts on the fresh table, not a refusal");
    assert_eq!(facts.properties.step_size, 1000.0);
    assert!(session.asked.lock().unwrap().is_empty(), "nothing is sent a second time");
    assert_eq!(
        session.reads.load(Ordering::SeqCst),
        2,
        "one read, and ONE more because the row looked undone"
    );
}

/// The memory is NEVER locked across the ask: a subscription that waits for the session thread (up to
/// `SUBSCRIBE_WAIT`) would otherwise hold up the price lane's poll and the control door behind it.
#[test]
fn the_memory_is_not_locked_while_a_subscription_is_in_flight() {
    struct Probing {
        reads: std::sync::OnceLock<Arc<AccountReads>>,
        free_during_the_ask: Mutex<Option<bool>>,
        table: Mutex<Vec<serde_json::Value>>,
    }
    impl SessionReads for Probing {
        fn system_properties(&self) -> PropertiesRead {
            stated("EUR")
        }
        fn offers(&self) -> Result<String, String> {
            Ok(serde_json::Value::Array(self.table.lock().unwrap().clone()).to_string())
        }
        fn subscribe(&self, _instrument: &str) -> Result<(), Failure> {
            let reads = self.reads.get().expect("registered");
            *self.free_during_the_ask.lock().unwrap() = Some(reads.attempts.try_lock().is_ok());
            self.table.lock().unwrap().iter_mut().for_each(|r| r["subscription"] = json!("T"));
            Ok(())
        }
    }
    let session = Arc::new(Probing {
        reads: std::sync::OnceLock::new(),
        free_during_the_ask: Mutex::new(None),
        table: Mutex::new(vec![with_status("EUR/USD", "V")]),
    });
    let reads = Arc::new(AccountReads::new(session.clone()));
    assert!(session.reads.set(Arc::clone(&reads)).is_ok());
    reads.quoted_facts("EURUSD", &|| 5).expect("facts");
    assert_eq!(
        *session.free_during_the_ask.lock().unwrap(),
        Some(true),
        "the memory was locked across the ask"
    );
}
