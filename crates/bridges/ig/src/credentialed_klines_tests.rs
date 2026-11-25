//! The IG history lane, tested where it can be named: the credential READ (`pub(crate)`, it returns the
//! demo account's password), the presence word, the startup line, the allowance LEDGER and the spec's
//! connection sources (a CONNECTION is one `connection_source`; the engine's half — one source per
//! connection, the cancel and the admission before any slot — is the gatewayhub's
//! `src/rest/credentialed_klines_tests.rs`). Moved here from the gatewayhub with the lane; the gatewayhub's
//! `tests/lanes/ig_credential.rs` still drives the lane through the real server and the client's real
//! `KlineSourceGateway::credentialed`. The store writer and the log capture both sides use are
//! `vike_bridge_core::credential_fixture`'s, as the OANDA lane's are.
//!
//! Every fixture plants ACCOUNT ROWS and writes their FIELDS (decision 0156 item 6) — no credential
//! NAME is composed or spelled — and every VALUE is an obviously fake, distinctive string, so "no value
//! reached a log line" is an assertion that could actually fail. The real `KlineSourceIg` is built here only
//! over a scripted gateway session (`KlineSourceIg::with_login`): a test must never log in to IG's demo host.

use std::cell::Cell as StdCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use vike_bridge_core::transport::http_error::VenueApiHttpError;

use super::*;
use crate::history::klines::{PricesApi, reset_sentence};
use crate::refusals::IgHistoryRefusalId;
use vike_bridge_core::credential_fixture::{
    Planted, plant_unreadable_store, under_capture, write_store,
};

const API_KEY_VALUE: &str = "fake-ig-api-key-5d2f";
const IDENTIFIER_VALUE: &str = "fake-ig-identifier-8h1q";
const PASSWORD_VALUE: &str = "fake-ig-password-3v9x";

fn pair(field: &str, value: &str) -> (String, String) {
    (field.to_string(), value.to_string())
}

/// The demo gateway as the lane looks it up over the release's ig rows.
fn demo_gateway() -> String {
    ig_rest_base(&VenueUrls::seed(VENUE), DEMO_TIER).expect("the seed's row").to_string()
}

/// **A missing gateway row fails every fetch as `Fetch`, naming the row**, before the store is read, as
/// on every venue (owner ruling on H-D7): the lane over an empty table answers the lookup's words, and
/// never a login.
#[test]
fn a_missing_gateway_row_fails_the_fetch_naming_the_row() {
    let spec = CredentialedKlinesSpecIg::new(nowhere(), &VenueUrls::default());
    let memo = Arc::new(ConfigMemo::default());
    let provider = spec.config_provider(&memo);
    let Err(SourceError::Fetch(why)) = provider() else { panic!("an empty table fails the fetch") };
    assert!(why.text().contains("venue_url"), "{why}");
}

/// A loopback IG whose login (`POST /session`) is answered once with `reply`: its base URL, and the
/// server thread, which answers the request line it read.
fn login_answering(reply: &'static str) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
    let base = format!("http://{}", listener.local_addr().expect("its address"));
    let server = std::thread::spawn(move || {
        let (sock, _) = listener.accept().expect("the login arrives");
        let mut reader = BufReader::new(sock);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).expect("the request line");
        let mut length = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).expect("a header");
            if header.trim().is_empty() {
                break;
            }
            if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0_u8; length];
        reader.read_exact(&mut body).expect("the body");
        reader.get_mut().write_all(reply.as_bytes()).expect("the answer");
        request_line
    });
    (base, server)
}

/// **Only IG refusing a login holds the next one off** (owner ruling on H-D7): a missing address row is a
/// `Fetch` that asked nothing of IG and holds nothing off, so the next request, with the row restored, logs
/// in at once; IG answering that login with a 401 holds the next one off for five minutes, as before.
#[test]
fn a_missing_row_holds_no_login_off_and_an_ig_refusal_holds_five_minutes() {
    let ledger = Arc::new(ledger());
    let gate = Arc::clone(&ledger) as Arc<dyn AllowanceGate>;
    let (epic, start) = ("CS.D.EURUSD.MINI.IP", 1_792_368_000_000_i64); // 2026-10-19T00:00:00Z
    let end = start + 3 * 3_600_000 - 1;

    let missing = CredentialedKlinesSpecIg::new(nowhere(), &VenueUrls::default());
    let source = KlineSourceIg::new(
        missing.config_provider(&Arc::new(ConfigMemo::default())),
        Arc::clone(&gate),
        &SessionCache::new(),
    );
    let Err(SourceError::Fetch(why)) = source.fetch_until(epic, "1h", start, end, &never) else {
        panic!("a missing row fails the fetch")
    };
    assert!(why.text().contains("venue_url"), "{why}");
    assert_eq!(ledger.precheck(1, "x"), Ok(()), "a missing row holds no login off");

    let (base, server) = login_answering(
        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: 46\r\n\
         connection: close\r\n\r\n{\"errorCode\":\"error.security.invalid-details\"}",
    );
    let restored: ConfigProvider = Arc::new(move || {
        Ok(IgConfig {
            api_key: vike_model::secret::Secret::new("k".to_string()),
            identifier: vike_model::secret::Secret::new("i".to_string()),
            password: vike_model::secret::Secret::new("p".to_string()),
            rest_base: base.clone(),
            account_pin: None,
            fx_clock: None,
        })
    });
    let source = KlineSourceIg::new(restored, Arc::clone(&gate), &SessionCache::new());
    let Err(SourceError::Fetch(why)) = source.fetch_until(epic, "1h", start, end, &never) else {
        panic!("IG refused the login")
    };
    assert!(server.join().unwrap().starts_with("POST "), "the login was asked at once");
    assert!(why.text().contains("401"), "{why}");
    set_now(T0 + 299_999);
    let SourceError::Fetch(held) = ledger.precheck(1, "x").expect_err("held off") else {
        panic!("a Fetch")
    };
    assert_eq!(
        held,
        IgHistoryRefusal::LoginHeld { why: why.text().to_owned(), minutes: 5 }.carried()
    );
    set_now(T0 + 300_000);
    assert_eq!(ledger.precheck(1, "x"), Ok(()), "five minutes have passed");
}

/// The demo account's trio, leaving out `without` (a field name) and blanking `blank`.
fn demo_fields(without: Option<&str>, blank: Option<&str>) -> Vec<(String, String)> {
    [("API_KEY", API_KEY_VALUE), ("IDENTIFIER", IDENTIFIER_VALUE), ("PASSWORD", PASSWORD_VALUE)]
        .into_iter()
        .filter(|(field, _)| Some(*field) != without)
        .map(|(field, value)| pair(field, if Some(field) == blank { "   " } else { value }))
        .collect()
}

/// A store's worth of accounts around the demo account: its LIVE sibling's trio, its PAPER sibling's
/// key, a LABELLED demo account's trio and another venue's key under the same field name. The
/// NEIGHBOURS the three-field read must leave behind. Every value distinctive.
fn store_rows(demo: Vec<(String, String)>) -> Vec<Planted> {
    vec![
        ("ig", AccountTier::Demo, None, demo),
        (
            "ig",
            AccountTier::Live,
            None,
            vec![
                pair("API_KEY", "fake-ig-live-key-1m4z"),
                pair("IDENTIFIER", "fake-ig-live-identifier-6c8b"),
                pair("PASSWORD", "fake-ig-live-password-9t2w"),
            ],
        ),
        ("ig", AccountTier::Paper, None, vec![pair("API_KEY", "fake-ig-paper-key-7n3r")]),
        (
            "ig",
            AccountTier::Demo,
            Some("ALT"),
            vec![
                pair("API_KEY", "fake-ig-alt-key-2k6y"),
                pair("IDENTIFIER", "fake-ig-alt-identifier-4p1d"),
                pair("PASSWORD", "fake-ig-alt-password-8x5s"),
            ],
        ),
        ("oanda", AccountTier::Demo, None, vec![pair("API_KEY", "fake-oanda-key-3q7h")]),
    ]
}

/// Every planted value outside the demo trio — what no read may answer in its place.
fn neighbour_values() -> Vec<&'static str> {
    vec![
        "fake-ig-live-key-1m4z",
        "fake-ig-live-identifier-6c8b",
        "fake-ig-live-password-9t2w",
        "fake-ig-paper-key-7n3r",
        "fake-ig-alt-key-2k6y",
        "fake-ig-alt-identifier-4p1d",
        "fake-ig-alt-password-8x5s",
        "fake-oanda-key-3q7h",
    ]
}

fn every_value() -> Vec<&'static str> {
    let mut values = neighbour_values();
    values.extend([API_KEY_VALUE, IDENTIFIER_VALUE, PASSWORD_VALUE]);
    values
}

fn at(dir: &std::path::Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
}

fn nowhere() -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(None, vike_secrets::CredentialScope::Full)
}

// ── the credential ─────────────────────────────────────────────────────────────────────────────

/// **The read returns the demo account's THREE fields and nothing else.** The trio is planted among its
/// live, paper and labelled siblings and another venue's key under the same field name; the read
/// answers the demo account's own values and builds the demo gateway's config. With the demo account
/// short of one field no neighbour answers in its place, and the error names the field by position.
#[test]
fn the_ig_history_read_holds_the_demo_trio_and_nothing_else() {
    let dir = tempfile::tempdir().expect("a temp dir");
    write_store(dir.path(), &store_rows(demo_fields(None, None)));
    assert!(dir.path().join("db").join("vike.db").is_file(), "a real database store");

    let config =
        read_ig_history_config(&at(dir.path()), &demo_gateway()).expect("the trio is stored");

    assert_eq!(
        (
            config.api_key.expose().as_str(),
            config.identifier.expose().as_str(),
            config.password.expose().as_str()
        ),
        (API_KEY_VALUE, IDENTIFIER_VALUE, PASSWORD_VALUE)
    );
    assert!(config.rest_base.contains("demo-api.ig.com"), "the demo gateway: {}", config.rest_base);
    let shown = format!("{config:?}");
    for value in every_value() {
        assert!(!shown.contains(value), "the config prints a value: {shown}");
    }

    for (without, flags) in [
        ("API_KEY", [true, false, false]),
        ("IDENTIFIER", [false, true, false]),
        ("PASSWORD", [false, false, true]),
    ] {
        write_store(dir.path(), &store_rows(demo_fields(Some(without), None)));
        let got = read_ig_history_config(&at(dir.path()), &demo_gateway());
        assert_eq!(got.err(), Some(IgCredentialError::NotConfigured(flags)), "without {without}");
    }
}

/// **Every state is its own answer, and no value reaches any line logged on the way.** No settings
/// directory, no store, a store whose demo account is short of a field, one holding a field BLANK and one
/// holding the trio; and a store that exists and will not read is `StoreUnreadable`, logged once.
#[test]
fn the_ig_history_read_answers_every_state_and_no_line_carries_a_value() {
    let absent = tempfile::tempdir().expect("a temp dir");
    let short = tempfile::tempdir().expect("a temp dir");
    write_store(short.path(), &store_rows(demo_fields(Some("IDENTIFIER"), None)));
    let blank = tempfile::tempdir().expect("a temp dir");
    write_store(blank.path(), &store_rows(demo_fields(None, Some("PASSWORD"))));
    let full = tempfile::tempdir().expect("a temp dir");
    write_store(full.path(), &store_rows(demo_fields(None, None)));
    let unreadable = tempfile::tempdir().expect("a temp dir");
    plant_unreadable_store(unreadable.path());

    let gateway = demo_gateway();
    let (answers, lines) = under_capture(|| {
        [
            read_ig_history_config(&nowhere(), &gateway).err(),
            read_ig_history_config(&at(absent.path()), &gateway).err(),
            read_ig_history_config(&at(short.path()), &gateway).err(),
            read_ig_history_config(&at(blank.path()), &gateway).err(),
            read_ig_history_config(&at(full.path()), &gateway).err(),
            read_ig_history_config(&at(unreadable.path()), &gateway).err(),
        ]
    });

    assert_eq!(
        answers,
        [
            Some(IgCredentialError::NotConfigured([true; 3])),
            Some(IgCredentialError::NotConfigured([true; 3])),
            Some(IgCredentialError::NotConfigured([false, true, false])),
            Some(IgCredentialError::NotConfigured([false, false, true])),
            None,
            Some(IgCredentialError::StoreUnreadable),
        ]
    );
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
    assert!(errors[0].contains("NOT the same as the fields being absent"), "{}", errors[0]);
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(value)), "a value reached the log: {lines:?}");
    }
}

/// The refusal an absent or unreadable credential earns names the fix and the FIELDS missing, by name.
#[test]
fn the_refusal_names_the_missing_fields_and_never_a_value() {
    let missing = |missing: &str| IgHistoryRefusal::CredentialMissing {
        all: "API_KEY, IDENTIFIER, PASSWORD".to_string(),
        account: demo_account_words(),
        missing: missing.to_string(),
    };
    assert_eq!(
        refusal_words(IgCredentialError::NotConfigured([false, true, false])),
        missing("IDENTIFIER")
    );
    assert_eq!(
        refusal_words(IgCredentialError::NotConfigured([true; 3])),
        missing("API_KEY, IDENTIFIER, PASSWORD")
    );
    assert_eq!(
        refusal_words(IgCredentialError::StoreUnreadable),
        IgHistoryRefusal::CredentialStoreUnreadable {
            account: demo_account_words(),
            all: "API_KEY, IDENTIFIER, PASSWORD".to_string(),
        }
    );
}

/// **The presence word needs ALL THREE fields**: one short or blank is `Absent`, and the word agrees
/// with the lane's own read on every store; a store that will not read is `Unreadable`, logged once;
/// no value reaches a line.
#[test]
fn the_presence_word_needs_all_three_fields() {
    let absent = tempfile::tempdir().expect("a temp dir");
    let short = tempfile::tempdir().expect("a temp dir");
    write_store(short.path(), &store_rows(demo_fields(Some("API_KEY"), None)));
    let blank = tempfile::tempdir().expect("a temp dir");
    write_store(blank.path(), &store_rows(demo_fields(None, Some("IDENTIFIER"))));
    let full = tempfile::tempdir().expect("a temp dir");
    write_store(full.path(), &store_rows(demo_fields(None, None)));
    let unreadable = tempfile::tempdir().expect("a temp dir");
    plant_unreadable_store(unreadable.path());

    let (answers, lines) = under_capture(|| {
        [
            ig_history_presence(&nowhere()),
            ig_history_presence(&at(absent.path())),
            ig_history_presence(&at(short.path())),
            ig_history_presence(&at(blank.path())),
            ig_history_presence(&at(full.path())),
            ig_history_presence(&at(unreadable.path())),
        ]
    });

    assert_eq!(
        answers,
        [
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Present,
            CredentialPresence::Unreadable,
        ]
    );
    for dir in [absent.path(), short.path(), blank.path(), full.path()] {
        assert_eq!(
            read_ig_history_config(&at(dir), &demo_gateway()).is_ok(),
            ig_history_presence(&at(dir)) == CredentialPresence::Present,
            "the presence word and the lane's read disagree about {}",
            dir.display()
        );
    }
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(value)), "a value reached the log: {lines:?}");
    }
}

/// The startup line says every state in its own words under the greppable prefix, and the armed line
/// states the three limits the ledger enforces.
#[test]
fn the_startup_line_says_every_state() {
    let dir = std::path::Path::new("/settings");
    let armed = ig_history_lane_line(Some(dir), CredentialPresence::Present);
    for part in [
        "ig history lane: ARMED",
        "API_KEY, IDENTIFIER, PASSWORD",
        "DEMO gateway",
        "20 percent",
        "50 percent",
        "one every 6 seconds",
        "decision 0194",
    ] {
        assert!(armed.contains(part), "{part:?} in {armed}");
    }
    let absent = ig_history_lane_line(Some(dir), CredentialPresence::Absent);
    assert!(
        absent.starts_with("ig history lane: not armed") && absent.contains("secrets set"),
        "{absent}"
    );
    let unknown = ig_history_lane_line(Some(dir), CredentialPresence::Unreadable);
    assert!(
        unknown.starts_with("ig history lane: UNKNOWN") && unknown.contains("not the same as"),
        "{unknown}"
    );
    let no_dir = ig_history_lane_line(None, CredentialPresence::Absent);
    assert!(no_dir.contains("no settings directory"), "{no_dir}");
    for line in [&armed, &absent, &unknown, &no_dir] {
        for value in every_value() {
            assert!(!line.contains(value));
        }
    }
}

// ── the ledger ─────────────────────────────────────────────────────────────────────────────────

thread_local! {
    /// The ledger's clock in these tests: one per test thread, so tests do not share a "now".
    static NOW: StdCell<i64> = const { StdCell::new(0) };
}

fn now() -> i64 {
    NOW.with(StdCell::get)
}

fn set_now(ms: i64) {
    NOW.with(|n| n.set(ms));
}

/// 2026-10-23T17:22:23Z, the instant of the probe's first answer.
const T0: i64 = 1_792_776_143_000;

/// A ledger with no pace on a controllable clock.
fn ledger() -> AllowanceLedger {
    set_now(T0);
    AllowanceLedger::with(Duration::ZERO, now)
}

fn heard(remaining: u32, resets_in_s: u64) -> Allowance {
    Allowance { remaining, total: 10_000, resets_in_s }
}

fn never() -> bool {
    false
}

/// **A chunk that would leave less than the reserve is refused BEFORE it is asked**, to the point: 2500
/// left, 2000 kept, so 500 is the last chunk that goes. The sentence carries the numbers and the reset.
#[test]
fn a_chunk_that_would_leave_less_than_the_reserve_is_refused_before_it_is_asked() {
    let ledger = ledger();
    ledger.learn(heard(2_500, 604_799));

    assert_eq!(ledger.admit(500, "1h bars from 2026-09-01 to 2026-09-21", &never), Ok(()));
    let SourceError::Fetch(text) = ledger
        .admit(501, "1h bars from 2026-09-01 to 2026-09-21", &never)
        .expect_err("501 would leave 1999")
    else {
        panic!("a Fetch")
    };
    let refusal = IgHistoryRefusal::ReserveReached {
        chunk: "1h bars from 2026-09-01 to 2026-09-21".to_string(),
        est_points: 501,
        remaining: 2_500,
        total: 10_000,
        reserve: 2_000,
        reset: reset_sentence(Some((T0, 604_799))),
    };
    assert_eq!(text, refusal.carried());
}

/// With nothing heard yet there is nothing to refuse on: the first read (one point) goes, and its
/// answer teaches the ledger.
#[test]
fn with_nothing_heard_the_first_read_is_admitted_and_its_answer_teaches() {
    let ledger = ledger();
    assert_eq!(ledger.last_seen(), None);
    assert_eq!(ledger.summary(), None);

    assert_eq!(ledger.admit(1, "the first daily read", &never), Ok(()));
    ledger.learn(heard(9_999, 604_798));

    assert_eq!(ledger.last_seen(), Some((heard(9_999, 604_798), T0)));
}

/// **Every answer replaces the belief**: spending by anything else (a manual run, the reconcile reads)
/// is repaired by the next answer, in either direction.
#[test]
fn a_later_answer_replaces_the_belief_in_both_directions() {
    let ledger = ledger();
    ledger.learn(heard(5_000, 600_000));
    assert_eq!(ledger.admit(150, "x", &never), Ok(()));

    ledger.learn(heard(2_100, 599_000));
    assert!(ledger.admit(150, "x", &never).is_err(), "1950 would be under the reserve");

    ledger.learn(heard(9_000, 598_000));
    assert_eq!(ledger.admit(150, "x", &never), Ok(()));
}

/// A window that has passed is a full one: the allowance refills at the reset, whatever the last answer
/// said, and the next answer says so for certain.
#[test]
fn a_window_that_has_reset_is_a_full_one() {
    let ledger = ledger();
    ledger.learn(heard(2_100, 100));

    set_now(T0 + 99_000);
    assert!(ledger.admit(200, "x", &never).is_err(), "still the old window");
    set_now(T0 + 100_000);
    assert_eq!(ledger.admit(5_000, "x", &never), Ok(()), "the window reset");
}

/// **A wall IG answered with is remembered.** The weekly historical-data allowance blocks every later
/// request locally until the window resets, with IG's own code in the words; a per-minute ceiling blocks
/// for a minute and a margin; any answer clears it.
#[test]
fn a_403_blocks_the_next_requests_until_the_allowance_refills() {
    let ledger = ledger();
    ledger.learn(heard(5_000, 3_600));

    let words = ledger.exhausted("error.public-api.exceeded-account-historical-data-allowance");
    assert!(
        matches!(&words, IgHistoryRefusal::AllowanceSpent { when, .. }
            if when == &reset_sentence(Some((T0, 3_600)))),
        "the week waits for its reset: {words:?}"
    );
    set_now(T0 + 3_599_999);
    assert_eq!(ledger.admit(1, "x", &never), Err(SourceError::Fetch(words.carried())));
    set_now(T0 + 3_600_000);
    assert_eq!(ledger.admit(1, "x", &never), Ok(()), "the window reset, and so did the block");

    set_now(T0);
    let minute = ledger.exhausted("error.public-api.exceeded-api-key-allowance");
    assert_eq!(minute.id(), IgHistoryRefusalId::AllowanceSpent, "{minute}");
    set_now(T0 + 64_999);
    assert!(ledger.admit(1, "x", &never).is_err());
    set_now(T0 + 65_000);
    assert_eq!(ledger.admit(1, "x", &never), Ok(()));

    set_now(T0);
    ledger.exhausted("error.public-api.exceeded-api-key-allowance");
    ledger.learn(heard(4_000, 3_000));
    assert_eq!(ledger.admit(1, "x", &never), Ok(()), "an answer arrived: the wall is gone");
}

/// IG's account id keys the ledger: another account starts clean, the same one keeps what it knew.
#[test]
fn binding_another_account_forgets_what_was_known() {
    let ledger = ledger();
    ledger.bind("ABC12");
    ledger.learn(heard(2_100, 600_000));
    assert!(ledger.admit(200, "x", &never).is_err());

    ledger.bind("ABC12");
    assert!(ledger.admit(200, "x", &never).is_err(), "the same account keeps its belief");
    ledger.bind("XYZ99");
    assert_eq!(ledger.last_seen(), None);
    assert_eq!(ledger.admit(200, "x", &never), Ok(()), "another account is a fresh pool");
}

/// **A precheck refuses what is already known, without taking a turn**: the reserve rule, to the point
/// (2100 left, 2000 kept), and a remembered wall. It never waits for the pace and never spends a slot,
/// so the request that follows it still leaves at once.
#[test]
fn a_precheck_refuses_a_known_wall_and_a_drained_week_without_taking_a_turn() {
    let ledger = AllowanceLedger::with(Duration::from_secs(30), now);
    set_now(T0);
    ledger.learn(heard(2_100, 604_799));
    let started = Instant::now();

    let SourceError::Fetch(text) =
        ledger.precheck(150, "1h bars from 2026-09-01 to 2026-09-21").expect_err("1950 is under")
    else {
        panic!("a Fetch")
    };
    let refusal = IgHistoryRefusal::ReserveReached {
        chunk: "1h bars from 2026-09-01 to 2026-09-21".to_string(),
        est_points: 150,
        remaining: 2_100,
        total: 10_000,
        reserve: 2_000,
        reset: reset_sentence(Some((T0, 604_799))),
    };
    assert_eq!(text, refusal.carried());
    assert_eq!(ledger.precheck(100, "x"), Ok(()), "2000 left is the last that goes");
    ledger.admit(0, "the first request", &never).expect("no slot was taken by the prechecks");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());

    let words = ledger.exhausted("error.public-api.exceeded-api-key-allowance");
    assert_eq!(
        ledger.precheck(1, "x"),
        Err(SourceError::Fetch(words.carried())),
        "a remembered wall"
    );
}

/// **A login IG refused holds the next one off for five minutes** (IG documents a lockout on repeated
/// failed logins, and the exec lane's own re-login shares the account), in a sentence with IG's own code
/// and the wait; a login that worked clears it, and so does the time.
#[test]
fn a_refused_login_holds_the_next_one_off_for_five_minutes() {
    let ledger = ledger();
    ledger.login_failed(
        "ig: the history lane could not log in (IG error 401: error.security.invalid-details)",
    );

    set_now(T0 + 299_999);
    let SourceError::Fetch(text) = ledger.precheck(1, "x").expect_err("held off") else {
        panic!("a Fetch")
    };
    let refusal = IgHistoryRefusal::LoginHeld {
        why: "ig: the history lane could not log in (IG error 401: error.security.invalid-details)"
            .to_string(),
        minutes: 5,
    };
    assert_eq!(text, refusal.carried());
    set_now(T0 + 300_000);
    assert_eq!(ledger.precheck(1, "x"), Ok(()), "five minutes have passed");

    set_now(T0);
    ledger.login_failed("ig: the history lane could not log in (IG error 401)");
    assert!(ledger.precheck(1, "x").is_err());
    ledger.bind("ABC12"); // the next login worked
    assert_eq!(ledger.precheck(1, "x"), Ok(()), "a login that worked clears the hold");
}

/// The closing line says how many points are left and when the week resets, in words.
#[test]
fn the_summary_says_what_is_left_and_when_it_resets() {
    let ledger = ledger();
    ledger.learn(heard(7_412, 441_000));

    let line = ledger.summary().expect("an answer was heard");

    assert_eq!(
        line,
        "ig history request ended. IG allowance left: 7412 of 10000 points. The allowance resets \
         in 5 days 2 hours (about 2026-10-28T19:52Z)."
    );
}

/// **The pace spaces requests in turn**: the first leaves at once, the next waits a pace for it.
#[test]
fn the_pace_spaces_requests_one_pace_apart() {
    let ledger = AllowanceLedger::with(Duration::from_millis(150), now);
    set_now(T0);
    let started = Instant::now();

    ledger.admit(0, "first", &never).expect("the first");
    let first = started.elapsed();
    ledger.admit(0, "second", &never).expect("the second");
    let second = started.elapsed();

    assert!(first < Duration::from_millis(100), "the first request waits for nothing: {first:?}");
    assert!(second >= Duration::from_millis(140), "the second waits a pace: {second:?}");
}

/// A cancel is seen while the pace is waited out, in well under the pace, and says so.
#[test]
fn a_cancel_during_the_wait_returns_at_once() {
    let ledger = AllowanceLedger::with(Duration::from_secs(30), now);
    set_now(T0);
    ledger.admit(0, "first", &never).expect("the first leaves at once");
    let cancelled = AtomicBool::new(false);
    let started = Instant::now();

    let outcome = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(60));
            cancelled.store(true, Ordering::SeqCst);
        });
        ledger.admit(0, "second", &|| cancelled.load(Ordering::SeqCst))
    });

    assert_eq!(outcome, Err(SourceError::Fetch(IgHistoryRefusal::CancelledWaiting.carried())));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the wait did not end: {:?}",
        started.elapsed()
    );
}

// ── the connection sources ─────────────────────────────────────────────────────────────────────

/// A source that asks its provider on every fetch, the way `KlineSourceIg` does on its login, records the
/// API key it was handed, and spends the gate with a cost taken from the symbol.
struct Echo {
    config: ConfigProvider,
    gate: Arc<dyn AllowanceGate>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl CredentialedKlinesSource for Echo {
    fn fetch_until(
        &self,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        _end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError> {
        let config = (self.config)()?;
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).push(config.api_key.into_inner());
        self.gate.admit(1, "an echo", stop)?;
        Ok(vec![Bar {
            timestamp: start_ms,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }])
    }
}

fn echo_spec(
    settings: &std::path::Path,
    seen: &Arc<Mutex<Vec<String>>>,
) -> CredentialedKlinesSpecIg {
    let seen = Arc::clone(seen);
    CredentialedKlinesSpecIg::with_source(
        at(settings),
        &VenueUrls::seed(VENUE),
        Arc::new(
            move |config: ConfigProvider,
                  gate: Arc<dyn AllowanceGate>|
                  -> Box<dyn CredentialedKlinesSource> {
                Box::new(Echo { config, gate, seen: Arc::clone(&seen) })
            },
        ),
    )
    .with_ledger(Arc::new(AllowanceLedger::with(Duration::ZERO, vike_model::now_ms)))
}

fn seen_keys(seen: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// **The three fields are read ONCE per connection**: a second fetch of one connection's source reuses
/// the answer even after the store changed, the next connection's source reads again, and a field
/// removed between requests disarms the lane for the next one with no restart — and no line logged on
/// the way carries a value.
#[test]
fn the_fields_are_read_once_per_connection_and_a_new_connection_rereads() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &store_rows(demo_fields(None, None)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let spec = echo_spec(settings.path(), &seen);

    let (removed_field_err, lines) = under_capture(|| {
        let first = spec.connection_source();
        first.fetch_until("CS.D.EURUSD.MINI.IP", "1h", 0, 1, &never).expect("the first chunk");
        let rotated = vec![pair("API_KEY", "fake-ig-rotated-key-6z1j")];
        let mut rows = store_rows(demo_fields(None, None));
        rows[0].3 = rotated.into_iter().chain(demo_fields(Some("API_KEY"), None)).collect();
        write_store(settings.path(), &rows);
        first.fetch_until("CS.D.EURUSD.MINI.IP", "1h", 2, 3, &never).expect("the second chunk");
        assert_eq!(
            seen_keys(&seen),
            [API_KEY_VALUE; 2],
            "ONE read for the connection's two chunks"
        );

        let second = spec.connection_source();
        second.fetch_until("CS.D.EURUSD.MINI.IP", "1h", 4, 5, &never).expect("a new connection");
        assert_eq!(seen_keys(&seen).last().map(String::as_str), Some("fake-ig-rotated-key-6z1j"));

        write_store(settings.path(), &store_rows(demo_fields(Some("PASSWORD"), None)));
        let third = spec.connection_source();
        third
            .fetch_until("CS.D.EURUSD.MINI.IP", "1h", 6, 7, &never)
            .expect_err("a field was removed")
    });

    assert_eq!(
        removed_field_err,
        SourceError::Refused(
            refusal_words(IgCredentialError::NotConfigured([false, false, true])).carried()
        )
    );
    for value in every_value().into_iter().chain(["fake-ig-rotated-key-6z1j"]) {
        assert!(!lines.iter().any(|l| l.contains(value)), "a value reached the log: {lines:?}");
    }
}

/// **The lane serves only the measured intervals**, whoever asks: a minute or a four-hour series is
/// refused in words by the admission, which the engine asks before the store is read, the connection's
/// slot is made or any source is built.
#[test]
fn the_lane_refuses_an_unmeasured_interval_before_it_reads_anything() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &store_rows(demo_fields(None, None)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let spec = echo_spec(settings.path(), &seen);

    for interval in ["1m", "5m", "30m", "4h"] {
        let out = spec.admit(interval);
        let Err(SourceError::Refused(text)) = out else { panic!("{interval}: a Refused: {out:?}") };
        assert!(text.text().contains("unmeasured") && text.text().contains(interval), "{text}");
    }
    assert!(seen_keys(&seen).is_empty(), "the store was not read");
    spec.admit("1d").expect("a measured one goes");
    spec.connection_source()
        .fetch_until("CS.D.EURUSD.MINI.IP", "1d", 0, 1, &never)
        .expect("and is served");
}

/// **One ledger serves every connection**: a chunk of one connection teaches it and the chunk of ANOTHER
/// connection is refused by what the first one learnt.
#[test]
fn one_ledger_serves_every_connection() {
    struct Spender {
        gate: Arc<dyn AllowanceGate>,
        remaining: u32,
    }
    impl CredentialedKlinesSource for Spender {
        fn fetch_until(
            &self,
            _s: &str,
            _i: &str,
            _a: i64,
            _b: i64,
            stop: &dyn Fn() -> bool,
        ) -> Result<Vec<Bar>, SourceError> {
            self.gate.admit(200, "a chunk of 200 points", stop)?;
            self.gate.learn(Allowance {
                remaining: self.remaining,
                total: 10_000,
                resets_in_s: 5 * 86_400,
            });
            Ok(Vec::new())
        }
    }
    set_now(T0);
    let spec = CredentialedKlinesSpecIg::with_source(
        nowhere(),
        &VenueUrls::seed(VENUE),
        Arc::new(
            |_config: ConfigProvider,
             gate: Arc<dyn AllowanceGate>|
             -> Box<dyn CredentialedKlinesSource> {
                Box::new(Spender { gate, remaining: 2_100 })
            },
        ),
    )
    .with_ledger(Arc::new(AllowanceLedger::with(Duration::ZERO, now)));

    spec.connection_source()
        .fetch_until("CS.D.EURUSD.MINI.IP", "1h", 0, 1, &never)
        .expect("the first connection spends 200 of an unknown week");
    let refused = spec
        .connection_source()
        .fetch_until("CS.D.EURUSD.MINI.IP", "1h", 0, 1, &never)
        .expect_err("the second connection sees what the first learnt");

    let SourceError::Fetch(text) = refused else { panic!("a Fetch") };
    assert!(text.text().contains("2100 of this week's 10000 points are left"), "{text}");
    assert!(spec.ledger().summary().is_some());
}

/// **A request ends when its connection closes, and says what the week has left, ONCE**: dropping a
/// connection's source logs the ledger's closing line at `info`, however many chunks it fetched.
#[test]
fn a_closed_connection_says_what_the_week_has_left_once() {
    struct Learner {
        gate: Arc<dyn AllowanceGate>,
    }
    impl CredentialedKlinesSource for Learner {
        fn fetch_until(
            &self,
            _s: &str,
            _i: &str,
            _a: i64,
            _b: i64,
            _stop: &dyn Fn() -> bool,
        ) -> Result<Vec<Bar>, SourceError> {
            self.gate.learn(heard(7_412, 441_000));
            Ok(Vec::new())
        }
    }
    let spec = CredentialedKlinesSpecIg::with_source(
        nowhere(),
        &VenueUrls::seed(VENUE),
        Arc::new(
            |_config: ConfigProvider,
             gate: Arc<dyn AllowanceGate>|
             -> Box<dyn CredentialedKlinesSource> { Box::new(Learner { gate }) },
        ),
    )
    .with_ledger(Arc::new(ledger()));

    let ((), lines) = under_capture(|| {
        let source = spec.connection_source();
        for _ in 0..2 {
            source.fetch_until("CS.D.EURUSD.MINI.IP", "1h", 0, 1, &never).expect("a chunk");
        }
        drop(source);
    });

    let ended: Vec<&String> =
        lines.iter().filter(|l| l.contains("ig history request ended")).collect();
    assert_eq!(ended.len(), 1, "one line for the one connection: {lines:?}");
    assert!(
        ended[0].starts_with("INFO") && ended[0].contains("7412 of 10000 points"),
        "{}",
        ended[0]
    );
}

// ── the real source over a scripted gateway, behind the real ledger ──────────────────────────────

/// A session that answers from a queue and records what it is asked.
struct Scripted {
    calls: Mutex<Vec<(String, String)>>,
    replies: Mutex<VecDeque<Result<Value, VenueApiHttpError>>>,
}

impl PricesApi for Scripted {
    fn account_id(&self) -> String {
        "ABC12".to_string()
    }
    fn get(&self, path: &str, _version: &str, query: &str) -> Result<Value, VenueApiHttpError> {
        self.calls.lock().unwrap().push((path.to_string(), query.to_string()));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("an unscripted request: {path}?{query}"))
    }
}

fn price_row(utc: &str, local: &str, close: (f64, f64)) -> Value {
    let side = |(bid, ask): (f64, f64)| json!({"bid": bid, "ask": ask, "lastTraded": null});
    json!({
        "snapshotTime": local, "snapshotTimeUTC": utc, "lastTradedVolume": 10,
        "openPrice": side(close), "highPrice": side(close), "lowPrice": side(close),
        "closePrice": side(close),
    })
}

fn answer(rows: Vec<Value>, remaining: u32) -> Value {
    let n = rows.len();
    json!({
        "prices": rows,
        "metadata": {
            "allowance": {"allowanceExpiry": 604799, "remainingAllowance": remaining, "totalAllowance": 10000},
            "pageData": {"pageNumber": 1, "pageSize": n, "totalPages": 1},
            "size": n
        }
    })
}

/// **The real `KlineSourceIg` behind the real ledger stops at the reserve**: the first read teaches the
/// ledger (2100 left of 10000, 2000 kept), a three-row chunk goes and is answered, and the next chunk,
/// which could use 100, is refused in words WITHOUT being asked of the gateway.
#[test]
fn the_real_source_over_a_scripted_gateway_stops_at_the_reserve() {
    let epic = "CS.D.EURUSD.MINI.IP";
    let eur_snapshot = json!({
        "instrument": {"onePipMeans": "0.0001 USD/EUR"},
        "snapshot": {"marketStatus": "TRADEABLE", "bid": 1.11927, "offer": 1.11933, "scalingFactor": 10000}
    });
    let hours = [
        ("2026-10-19T00:00:00", "2026/10/19 08:00:00"),
        ("2026-10-19T01:00:00", "2026/10/19 09:00:00"),
        ("2026-10-19T02:00:00", "2026/10/19 10:00:00"),
    ];
    let chunk = answer(hours.iter().map(|(u, l)| price_row(u, l, (1.1, 1.1002))).collect(), 2_097);
    let first_read = answer(
        vec![price_row("2026-10-23T16:00:00", "2026/10/24 00:00:00", (1.11927, 1.11933))],
        2_100,
    );
    let api = Arc::new(Scripted {
        calls: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([Ok(first_read), Ok(eur_snapshot), Ok(chunk)])),
    });
    let login_api = Arc::clone(&api);
    let spec = CredentialedKlinesSpecIg::with_source(
        nowhere(),
        &VenueUrls::seed(VENUE),
        Arc::new(
            move |_config: ConfigProvider,
                  gate: Arc<dyn AllowanceGate>|
                  -> Box<dyn CredentialedKlinesSource> {
                let api = Arc::clone(&login_api);
                let login: crate::history::klines::LoginFn =
                    Arc::new(move || Ok(Arc::clone(&api) as Arc<dyn PricesApi>));
                Box::new(KlineSourceIg::with_login(login, gate).with_clock(|| 4_000_000_000_000))
            },
        ),
    )
    .with_ledger(Arc::new(AllowanceLedger::with(Duration::ZERO, now)));
    set_now(T0);
    let source = spec.connection_source();
    let day = 86_400_000_i64;
    let start = 1_792_368_000_000; // 2026-10-19T00:00:00Z

    let bars = source
        .fetch_until(epic, "1h", start, start + 3 * 3_600_000 - 1, &never)
        .expect("chunk one");
    // Five days of hours could use 120 points: 2097 - 120 is under the 2000 kept.
    let refused = source
        .fetch_until(epic, "1h", start + day, start + 6 * day - 1, &never)
        .expect_err("chunk two");

    assert_eq!(bars.len(), 3);
    let SourceError::Fetch(text) = refused else { panic!("a Fetch") };
    for part in
        ["were NOT requested", "up to 120 points", "2097 of this week's 10000 points are left"]
    {
        assert!(text.text().contains(part), "{part:?} in {text}");
    }
    assert_eq!(
        api.calls.lock().unwrap().len(),
        3,
        "the first read, the snapshot and chunk one only"
    );
    assert_eq!(
        spec.ledger().last_seen().map(|(a, _)| a.remaining),
        Some(2_097),
        "the chunk's answer is what the ledger believes"
    );
}
