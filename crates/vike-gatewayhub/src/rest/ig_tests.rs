//! The IG history lane, tested where it can be named: the credential READ (`pub(crate)`, it returns the
//! demo account's password), the presence word, the startup line, the allowance LEDGER and the lane's
//! dispatch. `tests/ig_credential.rs` drives the lane through the real server and the client's real
//! `RemoteKlines::credentialed`; the fixtures are therefore written twice, once per side of the crate
//! boundary, as `oanda_tests.rs`'s are.
//!
//! Every fixture plants ACCOUNT ROWS and writes their FIELDS (decision 0156 item 6) — no credential
//! NAME is composed or spelled — and every VALUE is an obviously fake, distinctive string, so "no value
//! reached a log line" is an assertion that could actually fail. The real `IgKlines` is built here only
//! over a scripted gateway session (`IgKlines::with_login`): a test must never log in to IG's demo host.

use std::cell::Cell as StdCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use vike_ig::{IgApiError, PricesApi};

use super::*;

/// Every event's level, then every field — `message` first — as ` name=value`, one line per event:
/// the shape the assertions below read. Projected from `vike_log::capture::captured`.
fn under_capture<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    let (out, events) = vike_log::capture::captured(f);
    let mut lines = Vec::with_capacity(events.len());
    for e in &events {
        let mut line = format!("{} message={}", e.level, e.message);
        for (k, v) in &e.fields {
            line.push_str(&format!(" {k}={v}"));
        }
        lines.push(line);
    }
    (out, lines)
}

const API_KEY_VALUE: &str = "fake-ig-api-key-5d2f";
const IDENTIFIER_VALUE: &str = "fake-ig-identifier-8h1q";
const PASSWORD_VALUE: &str = "fake-ig-password-3v9x";

/// One planted account: its venue, tier and label, and the `(field, value)` pairs filed against it.
type Planted = (&'static str, AccountTier, Option<&'static str>, Vec<(String, String)>);

fn pair(field: &str, value: &str) -> (String, String) {
    (field.to_string(), value.to_string())
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

/// Make the settings DATABASE hold exactly `accounts`, 0600 on unix. The store is created the one way a
/// store comes into being; each account is created with its tier FIRST and its fields are then written
/// against it by owner and field. A store being REPLACED is rebuilt in this test's own temp directory.
fn write_store(settings: &std::path::Path, accounts: &[Planted]) {
    let db_dir = settings.join("db");
    if db_dir.exists() {
        std::fs::remove_dir_all(&db_dir).expect("drop this test's own previous store");
    }
    vike_secrets::create_store(Some(settings.to_str().expect("a utf-8 temp path")))
        .expect("create the empty store");
    for &(venue, tier, label, ref fields) in accounts {
        let created = vike_secrets::edit_account_in_unjournalled(
            settings,
            vike_secrets::AccountEdit::Create { venue, tier, label },
        )
        .expect("create the account");
        let id = created.after.expect("a create returns its row").id;
        if !fields.is_empty() {
            vike_secrets::save_owned_credentials_unjournalled(
                settings,
                &vike_secrets::CredentialOwner::Account(id),
                fields,
                vike_secrets::UpsertIntent::Add,
            )
            .expect("write the account's fields");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            vike_secrets::db_path_in(settings),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("chmod");
    }
}

/// Bytes that are not a database where the database belongs: a store that EXISTS and will not open.
fn plant_unreadable_store(settings: &std::path::Path) {
    std::fs::create_dir_all(settings.join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(settings), b"not a sqlite database at all")
        .expect("plant the garbage");
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

    let config = read_ig_history_config(&at(dir.path())).expect("the trio is stored");

    assert_eq!(
        (config.api_key.as_str(), config.identifier.as_str(), config.password.as_str()),
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
        let got = read_ig_history_config(&at(dir.path()));
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

    let (answers, lines) = under_capture(|| {
        [
            read_ig_history_config(&nowhere()).err(),
            read_ig_history_config(&at(absent.path())).err(),
            read_ig_history_config(&at(short.path())).err(),
            read_ig_history_config(&at(blank.path())).err(),
            read_ig_history_config(&at(full.path())).err(),
            read_ig_history_config(&at(unreadable.path())).err(),
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
    let one = refusal_words(IgCredentialError::NotConfigured([false, true, false]));
    assert!(one.contains("lacks: IDENTIFIER."), "{one}");
    assert!(
        one.contains("API_KEY, IDENTIFIER, PASSWORD") && one.contains("`vike-cli secrets set"),
        "{one}"
    );
    assert!(one.contains("Nothing was asked of IG"), "{one}");
    let all = refusal_words(IgCredentialError::NotConfigured([true; 3]));
    assert!(all.contains("lacks: API_KEY, IDENTIFIER, PASSWORD."), "{all}");
    let broken = refusal_words(IgCredentialError::StoreUnreadable);
    assert!(broken.contains("not the same as the fields being absent"), "{broken}");
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
            read_ig_history_config(&at(dir)).is_ok(),
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
    for part in [
        "1h bars from 2026-09-01 to 2026-09-21 were NOT requested",
        "up to 501 points",
        "2500 of this week's 10000 points are left",
        "2000 (20 percent) are kept in reserve",
        "resets in 6 days 23 hours (about 2026-10-30T17:22Z)",
        "Run the same command again after the reset",
    ] {
        assert!(text.contains(part), "{part:?} in {text}");
    }
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
        words.contains("historical-data allowance is spent") && words.contains("resets in 1 hour"),
        "{words}"
    );
    set_now(T0 + 3_599_999);
    assert_eq!(ledger.admit(1, "x", &never), Err(SourceError::Fetch(words.clone())));
    set_now(T0 + 3_600_000);
    assert_eq!(ledger.admit(1, "x", &never), Ok(()), "the window reset, and so did the block");

    set_now(T0);
    let minute = ledger.exhausted("error.public-api.exceeded-api-key-allowance");
    assert!(minute.contains("per-minute ceiling"), "{minute}");
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
    assert!(text.contains("were NOT requested") && text.contains("up to 150 points"), "{text}");
    assert_eq!(ledger.precheck(100, "x"), Ok(()), "2000 left is the last that goes");
    ledger.admit(0, "the first request", &never).expect("no slot was taken by the prechecks");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());

    let words = ledger.exhausted("error.public-api.exceeded-api-key-allowance");
    assert_eq!(ledger.precheck(1, "x"), Err(SourceError::Fetch(words)), "a remembered wall");
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
    for part in ["refused a login", "5 minutes", "invalid-details", "locks"] {
        assert!(text.contains(part), "{part:?} in {text}");
    }
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

    let SourceError::Fetch(text) = outcome.expect_err("cancelled") else { panic!("a Fetch") };
    assert!(text.contains("cancelled"), "{text}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the wait did not end: {:?}",
        started.elapsed()
    );
}

// ── the lane ───────────────────────────────────────────────────────────────────────────────────

/// A source that asks its provider on every fetch, the way `IgKlines` does on its login, records the
/// API key it was handed, and spends the gate with a cost taken from the symbol.
struct Echo {
    config: ConfigProvider,
    gate: Arc<dyn AllowanceGate>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl IgHistory for Echo {
    fn fetch_until(
        &self,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        _end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError> {
        let config = (self.config)()?;
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).push(config.api_key);
        self.gate.admit(1, "an echo", stop)?;
        Ok(vec![Bar {
            ts: start_ms,
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

fn echo_lane(settings: &std::path::Path, seen: &Arc<Mutex<Vec<String>>>) -> IgLane {
    let seen = Arc::clone(seen);
    IgLane::with_source(
        at(settings),
        Arc::new(
            move |config: ConfigProvider, gate: Arc<dyn AllowanceGate>| -> Box<dyn IgHistory> {
                Box::new(Echo { config, gate, seen: Arc::clone(&seen) })
            },
        ),
    )
    .with_ledger(Arc::new(AllowanceLedger::with(Duration::ZERO, vike_model::now_ms)))
}

fn seen_keys(seen: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// **The three fields are read ONCE per connection**: a second fetch on the same scope reuses the
/// answer even after the store changed, a new scope reads again, and a field removed between requests
/// disarms the lane for the next one with no restart — and no line logged on the way carries a value.
#[test]
fn the_fields_are_read_once_per_connection_and_a_new_connection_rereads() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &store_rows(demo_fields(None, None)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let lane = echo_lane(settings.path(), &seen);
    let ctx = FetchCtx::new();

    let (removed_field_err, lines) = under_capture(|| {
        let first = ConnectionScope::new();
        lane.fetch(&first, "CS.D.EURUSD.MINI.IP", "1h", 0, 1, &ctx).expect("the first chunk");
        let rotated = vec![pair("API_KEY", "fake-ig-rotated-key-6z1j")];
        let mut rows = store_rows(demo_fields(None, None));
        rows[0].3 = rotated.into_iter().chain(demo_fields(Some("API_KEY"), None)).collect();
        write_store(settings.path(), &rows);
        lane.fetch(&first, "CS.D.EURUSD.MINI.IP", "1h", 2, 3, &ctx).expect("the second chunk");
        assert_eq!(
            seen_keys(&seen),
            [API_KEY_VALUE; 2],
            "ONE read for the connection's two chunks"
        );

        let second = ConnectionScope::new();
        lane.fetch(&second, "CS.D.EURUSD.MINI.IP", "1h", 4, 5, &ctx).expect("a new connection");
        assert_eq!(seen_keys(&seen).last().map(String::as_str), Some("fake-ig-rotated-key-6z1j"));

        write_store(settings.path(), &store_rows(demo_fields(Some("PASSWORD"), None)));
        let third = ConnectionScope::new();
        lane.fetch(&third, "CS.D.EURUSD.MINI.IP", "1h", 6, 7, &ctx)
            .expect_err("a field was removed")
    });

    let FetchFailure::Refused(text) = removed_field_err else { panic!("a Refused") };
    assert!(text.contains("lacks: PASSWORD."), "{text}");
    for value in every_value().into_iter().chain(["fake-ig-rotated-key-6z1j"]) {
        assert!(!lines.iter().any(|l| l.contains(value)), "a value reached the log: {lines:?}");
    }
}

/// A cancelled fetch is seen before the venue is asked and never creates the connection's slot.
#[test]
fn a_cancelled_fetch_never_creates_the_connections_slot() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &store_rows(demo_fields(None, None)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let lane = echo_lane(settings.path(), &seen);
    let scope = ConnectionScope::new();
    let ctx = FetchCtx::new();
    ctx.cancel();

    let out = lane.fetch(&scope, "CS.D.EURUSD.MINI.IP", "1h", 0, 1, &ctx);

    assert!(matches!(out, Err(FetchFailure::Fetch(_))), "{out:?}");
    assert!(scope.is_empty());
    assert!(seen_keys(&seen).is_empty());
}

/// **The lane serves only the measured intervals**, whoever asks: a minute or a four-hour series is
/// refused in words before the store is read, the connection's slot is made or any source is built.
#[test]
fn the_lane_refuses_an_unmeasured_interval_before_it_reads_anything() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &store_rows(demo_fields(None, None)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let lane = echo_lane(settings.path(), &seen);
    let scope = ConnectionScope::new();
    let ctx = FetchCtx::new();

    for interval in ["1m", "5m", "30m", "4h"] {
        let out = lane.fetch(&scope, "CS.D.EURUSD.MINI.IP", interval, 0, 1, &ctx);
        let Err(FetchFailure::Refused(text)) = out else {
            panic!("{interval}: a Refused: {out:?}")
        };
        assert!(text.contains("unmeasured") && text.contains(interval), "{text}");
    }
    assert!(scope.is_empty(), "no slot was made");
    assert!(seen_keys(&seen).is_empty(), "the store was not read");
    lane.fetch(&scope, "CS.D.EURUSD.MINI.IP", "1d", 0, 1, &ctx).expect("a measured one goes");
}

/// **One ledger serves every connection**: a chunk of one connection teaches it and the chunk of ANOTHER
/// connection is refused by what the first one learnt.
#[test]
fn one_ledger_serves_every_connection() {
    struct Spender {
        gate: Arc<dyn AllowanceGate>,
        remaining: u32,
    }
    impl IgHistory for Spender {
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
    let lane = IgLane::with_source(
        nowhere(),
        Arc::new(|_config: ConfigProvider, gate: Arc<dyn AllowanceGate>| -> Box<dyn IgHistory> {
            Box::new(Spender { gate, remaining: 2_100 })
        }),
    )
    .with_ledger(Arc::new(AllowanceLedger::with(Duration::ZERO, now)));
    let ctx = FetchCtx::new();

    lane.fetch(&ConnectionScope::new(), "CS.D.EURUSD.MINI.IP", "1h", 0, 1, &ctx)
        .expect("the first connection spends 200 of an unknown week");
    let refused = lane
        .fetch(&ConnectionScope::new(), "CS.D.EURUSD.MINI.IP", "1h", 0, 1, &ctx)
        .expect_err("the second connection sees what the first learnt");

    let FetchFailure::Fetch(text) = refused else { panic!("a Fetch") };
    assert!(text.contains("2100 of this week's 10000 points are left"), "{text}");
    assert!(lane.ledger().summary().is_some());
}

// ── the real source over a scripted gateway, behind the real ledger ──────────────────────────────

/// A session that answers from a queue and records what it is asked.
struct Scripted {
    calls: Mutex<Vec<(String, String)>>,
    replies: Mutex<VecDeque<Result<Value, IgApiError>>>,
}

impl PricesApi for Scripted {
    fn account_id(&self) -> String {
        "ABC12".to_string()
    }
    fn get(&self, path: &str, _version: &str, query: &str) -> Result<Value, IgApiError> {
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

/// **The real `IgKlines` behind the real ledger stops at the reserve**: the first read teaches the
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
    let lane = IgLane::with_source(
        nowhere(),
        Arc::new(
            move |_config: ConfigProvider, gate: Arc<dyn AllowanceGate>| -> Box<dyn IgHistory> {
                let api = Arc::clone(&login_api);
                let login: vike_ig::LoginFn =
                    Arc::new(move || Ok(Arc::clone(&api) as Arc<dyn PricesApi>));
                Box::new(IgKlines::with_login(login, gate).with_clock(|| 4_000_000_000_000))
            },
        ),
    )
    .with_ledger(Arc::new(AllowanceLedger::with(Duration::ZERO, now)));
    set_now(T0);
    let scope = ConnectionScope::new();
    let ctx = FetchCtx::new();
    let day = 86_400_000_i64;
    let start = 1_792_368_000_000; // 2026-10-19T00:00:00Z

    let bars =
        lane.fetch(&scope, epic, "1h", start, start + 3 * 3_600_000 - 1, &ctx).expect("chunk one");
    // Five days of hours could use 120 points: 2097 - 120 is under the 2000 kept.
    let refused = lane
        .fetch(&scope, epic, "1h", start + day, start + 6 * day - 1, &ctx)
        .expect_err("chunk two");

    assert_eq!(bars.len(), 3);
    let FetchFailure::Fetch(text) = refused else { panic!("a Fetch") };
    for part in
        ["were NOT requested", "up to 120 points", "2097 of this week's 10000 points are left"]
    {
        assert!(text.contains(part), "{part:?} in {text}");
    }
    assert_eq!(
        api.calls.lock().unwrap().len(),
        3,
        "the first read, the snapshot and chunk one only"
    );
    assert_eq!(
        lane.ledger().last_seen().map(|(a, _)| a.remaining),
        Some(2_097),
        "the chunk's answer is what the ledger believes"
    );
}
