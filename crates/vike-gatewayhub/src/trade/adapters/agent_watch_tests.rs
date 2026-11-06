//! **The approval watch tells once, tells again when it gets worse, never alarms on a silent venue, and no
//! credential reaches a line it writes.** The venue is a scripted checker, the pager a recording closure, the
//! clock a number; the one thread test drives the real watcher thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::channel;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, Accounts};

use super::*;
use crate::trade::adapters::bridge_trade::BridgeTrade;
use crate::trade::adapters::snapshot::TradeSnapshot;
use crate::trade::lane::TradeTarget;

const NOW: i64 = 1_790_000_000_000;
const SIGNER: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MASTER: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// The SDK vector key every hyperliquid signing test uses: a published test value, never an operator's.
const KEY: &str = "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e";

fn subject() -> AgentSubject {
    AgentSubject {
        network: vike_hyperliquid::base::config::Network::Testnet,
        signer: SIGNER.into(),
        master: Some(MASTER.into()),
    }
}

fn watched(still_runs: bool) -> Watched {
    Watched {
        key: "hyperliquid/default/demo".into(),
        account: "default".into(),
        subject: subject(),
        still_runs: Box::new(move || still_runs),
    }
}

fn expired() -> AgentStatus {
    AgentStatus::Expired { valid_until_ms: NOW - DAY_MS }
}

fn soon(days: i64) -> AgentStatus {
    AgentStatus::ExpiresSoon { valid_until_ms: NOW + days * DAY_MS, urgent: days <= 3 }
}

fn ok() -> AgentStatus {
    AgentStatus::Ok { valid_until_ms: Some(NOW + 100 * DAY_MS) }
}

fn quiet() -> AgentStatus {
    AgentStatus::CouldNotCheck("userRole: http 500".into())
}

#[test]
fn a_status_that_needs_the_operator_is_told_once_a_day_and_at_once_when_it_gets_worse() {
    let mut seen = Seen::default();
    // First sight: told. The same status an hour later and 23 hours later: silent.
    assert!(due(&mut seen, &soon(10), NOW));
    assert!(!due(&mut seen, &soon(10), NOW + 3_600_000));
    assert!(!due(&mut seen, &soon(10), NOW + 23 * 3_600_000));
    // A day after the last telling: told again.
    assert!(due(&mut seen, &soon(10), NOW + DAY_MS));
    // Worse within the day (14d -> 3d -> expired): told at once, each time.
    assert!(due(&mut seen, &soon(2), NOW + DAY_MS + 1));
    assert!(due(&mut seen, &expired(), NOW + DAY_MS + 2));
    assert!(!due(&mut seen, &expired(), NOW + DAY_MS + 3));
    // Expired and missing are the same class: one telling.
    assert!(!due(&mut seen, &AgentStatus::Missing, NOW + DAY_MS + 4));
}

#[test]
fn a_re_approval_resets_the_memory_and_a_silent_venue_changes_nothing() {
    let mut seen = Seen::default();
    assert!(due(&mut seen, &expired(), NOW));
    // The venue not answering is not a recovery and not a new incident.
    assert!(!due(&mut seen, &quiet(), NOW + 1));
    assert!(!due(&mut seen, &expired(), NOW + 2), "still the same incident");
    // Re-approved: nothing to say; the next lapse is told at once.
    assert!(!due(&mut seen, &ok(), NOW + 3));
    assert!(due(&mut seen, &expired(), NOW + 4));
    // Ok / master key / could-not-check are never told.
    for s in [ok(), AgentStatus::MasterKey, quiet()] {
        assert!(!due(&mut Seen::default(), &s, NOW), "{s:?}");
    }
}

/// The pager and the log, driven by `report` on this thread: the alert names the master, the signer and the
/// cure; the log line is a `warn`; and neither holds anything but public addresses.
#[test]
fn an_expired_approval_is_a_loud_warn_and_one_alert_naming_the_cure() {
    let alerts = Mutex::new(Vec::<FiredAlert>::new());
    let notify = |a: &FiredAlert| alerts.lock().unwrap().push(a.clone());
    let mut seen = Seen::default();
    let w = watched(true);
    let ((), events) = vike_log::capture::captured(|| {
        report(&mut seen, &w, &expired(), NOW, &notify);
        report(&mut seen, &w, &expired(), NOW + 3_600_000, &notify); // an hour later: quiet
    });
    let alerts = alerts.into_inner().unwrap();
    assert_eq!(alerts.len(), 1, "once, not every hour");
    let a = &alerts[0];
    assert!(
        a.title.contains("CANNOT SIGN") && a.title.contains("default (testnet)"),
        "{}",
        a.title
    );
    for needle in [MASTER, SIGNER, "app.hyperliquid-testnet.xyz/API", "EXPIRED", "warning only"] {
        assert!(a.body.contains(needle), "{needle} missing from: {}", a.body);
    }
    let warns: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::WARN).collect();
    assert_eq!(warns.len(), 1, "one loud line, the repeat is debug: {events:?}");
    assert!(
        warns[0].message.contains("EXPIRED")
            && warns[0].message.contains("app.hyperliquid-testnet.xyz/API")
    );
    assert_eq!(warns[0].field("signer"), Some(SIGNER));
    assert_eq!(warns[0].field("master"), Some(MASTER));
}

#[test]
fn a_venue_that_did_not_answer_is_info_never_warn_and_never_an_alert() {
    let alerts = AtomicUsize::new(0);
    let notify = |_: &FiredAlert| {
        alerts.fetch_add(1, Ordering::SeqCst);
    };
    let mut seen = Seen::default();
    let ((), events) = vike_log::capture::captured(|| {
        report(&mut seen, &watched(true), &quiet(), NOW, &notify);
    });
    assert_eq!(alerts.load(Ordering::SeqCst), 0);
    assert!(events.iter().all(|e| e.level >= tracing::Level::INFO), "{events:?}");
    assert!(events.iter().any(|e| e.message.contains("could not check")), "{events:?}");
}

#[test]
fn an_account_that_no_longer_runs_is_dropped_without_a_check() {
    let checks = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&checks);
    let parts = Parts {
        checker: Arc::new(move |_: &AgentSubject, _: i64| {
            counted.fetch_add(1, Ordering::SeqCst);
            AgentStatus::Missing
        }),
        notify: Arc::new(|_: &FiredAlert| {}),
        clock: Arc::new(|| NOW),
        every: CHECK_EVERY,
    };
    let mut entry = Entry { watched: watched(false), seen: Seen::default() };
    assert!(!check_one(&parts, &mut entry), "dropped");
    assert_eq!(checks.load(Ordering::SeqCst), 0, "a deactivated account is not asked about");
    let mut entry = Entry { watched: watched(true), seen: Seen::default() };
    assert!(check_one(&parts, &mut entry));
    assert_eq!(checks.load(Ordering::SeqCst), 1);
}

#[test]
fn a_panicking_check_reads_as_could_not_check() {
    let alerts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&alerts);
    let parts = Parts {
        checker: Arc::new(|_: &AgentSubject, _: i64| -> AgentStatus {
            panic!("a venue read blew up")
        }),
        notify: Arc::new(move |_: &FiredAlert| {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
        clock: Arc::new(|| NOW),
        every: CHECK_EVERY,
    };
    let mut entry = Entry { watched: watched(true), seen: Seen::default() };
    assert!(check_one(&parts, &mut entry), "the entry survives");
    assert_eq!(alerts.load(Ordering::SeqCst), 0, "a panic is 'could not check', never an alarm");
}

/// The REAL thread: a registration is checked at once (not an hour later), a second registration of the
/// same account replaces the first and does not alert again inside the day, and the pass repeats on its period.
#[test]
fn the_watcher_thread_checks_at_registration_and_on_its_period() {
    let (checked_tx, checked_rx) = channel();
    let (alert_tx, alert_rx) = channel();
    let clock = Arc::new(AtomicI64::new(NOW));
    let clock_for_watch = Arc::clone(&clock);
    let watch = AgentWatch::with_parts(
        Arc::new(move |subject: &AgentSubject, now: i64| {
            let _ = checked_tx.send((subject.signer.clone(), now));
            AgentStatus::Missing
        }),
        Arc::new(move |a: &FiredAlert| {
            let _ = alert_tx.send(a.title.clone());
        }),
        Arc::new(move || clock_for_watch.load(Ordering::SeqCst)),
        Duration::from_millis(80),
    );
    watch.watch(watched(true));
    let wait = Duration::from_secs(10);
    let (signer, at) = checked_rx.recv_timeout(wait).expect("checked at registration");
    assert_eq!((signer.as_str(), at), (SIGNER, NOW));
    assert!(alert_rx.recv_timeout(wait).expect("told").contains("CANNOT SIGN"));
    // The period elapses: checked again (more than once), told no second time within the day.
    checked_rx.recv_timeout(wait).expect("checked again on the period");
    checked_rx.recv_timeout(wait).expect("and again");
    watch.watch(watched(true));
    checked_rx.recv_timeout(wait).expect("a re-registration is checked at once");
    assert!(alert_rx.try_recv().is_err(), "the same incident, inside the day: one alert");
    // A day later the same incident is told again.
    clock.store(NOW + DAY_MS, Ordering::SeqCst);
    assert!(alert_rx.recv_timeout(wait).is_ok(), "re-alerted after a day");
}

/// A started hyperliquid account is put under the watch with the signer ADDRESS and the configured master
/// and nothing else; an account with no key is not watched, and a lane with no watch watches nothing.
#[test]
fn a_started_account_is_registered_by_address_and_the_key_never_travels() {
    let row = |id: i64, label: Option<&str>| Account {
        id,
        venue: "hyperliquid".to_string(),
        tier: AccountTier::Demo,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
    };
    let mut book = CredentialBook::new();
    book.insert_account(1, "PRIVATE_KEY", KEY);
    book.insert_account(1, "ACCOUNT_ADDRESS", MASTER);
    let accounts =
        AccountDirectory::from_rows(Accounts::Known(vec![row(1, None), row(2, Some("ALT"))]), None)
            .with_credentials(book);
    let snapshot = TradeSnapshot { accounts, ..TradeSnapshot::default() };

    let (seen_tx, seen_rx) = channel();
    let watch = Arc::new(AgentWatch::with_parts(
        Arc::new(move |subject: &AgentSubject, _: i64| {
            let _ = seen_tx.send(format!("{subject:?}"));
            AgentStatus::MasterKey
        }),
        Arc::new(|_: &FiredAlert| {}),
        Arc::new(|| NOW),
        CHECK_EVERY,
    ));
    let lane = BridgeTrade::with_snapshot(&[], HashMap::new(), snapshot)
        .with_agent_watch(Arc::clone(&watch));
    let target = |account: Option<&str>| TradeTarget {
        venue: "hyperliquid".into(),
        account: account.map(str::to_string),
        tier: AccountTier::Demo,
    };

    lane.watch_agent(&target(None), &AccountLabel::Default);
    let seen =
        seen_rx.recv_timeout(Duration::from_secs(10)).expect("the default account is watched");
    assert!(seen.contains(MASTER), "{seen}");
    assert!(!seen.contains(KEY), "the key never reaches the watch: {seen}");
    let signer = vike_hyperliquid::signing::Signer::from_private_key(
        KEY,
        vike_hyperliquid::base::config::Network::Testnet,
    )
    .expect("the vector key");
    assert!(seen.contains(signer.address()), "{seen}");

    // ALT holds no key: nothing registered.
    lane.watch_agent(&target(Some("ALT")), &AccountLabel::parse("ALT").unwrap());
    assert!(seen_rx.recv_timeout(Duration::from_millis(300)).is_err());
    // A lane with no watch is a quiet no-op.
    let bare = BridgeTrade::with_snapshot(&[], HashMap::new(), TradeSnapshot::default());
    bare.watch_agent(&target(None), &AccountLabel::Default);
}

/// **The directory the watcher's predicate (and the ibkr link's) keeps for the life of the process holds the
/// account ROWS and no credential book.** `watch_agent` and `link_watch` capture `without_credentials()`, and
/// `account_tier` reads rows only, so the stripped directory answers exactly as the full one while a rotated
/// key is no longer reachable from a closure that outlives the rotation.
#[test]
fn the_directory_a_long_lived_predicate_keeps_has_the_rows_and_no_credential_book() {
    let row = Account {
        id: 1,
        venue: "hyperliquid".to_string(),
        tier: AccountTier::Demo,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
    };
    let mut book = CredentialBook::new();
    book.insert_account(1, "PRIVATE_KEY", KEY);
    book.insert_venue("hyperliquid", "ACCOUNT_ADDRESS", MASTER);
    let full = AccountDirectory::from_rows(Accounts::Known(vec![row]), None).with_credentials(book);
    let kept = full.without_credentials();

    assert!(
        matches!(full.credentials(), Some(Ok(_))),
        "the premise: the full directory has a book"
    );
    assert!(
        kept.credentials().is_none(),
        "the kept directory holds no book of any account or venue"
    );
    assert_eq!(kept.rows(), full.rows(), "the rows are what it keeps");
    let tier = |directory: &AccountDirectory| {
        vike_config::account_arming::account_tier(
            Some(directory),
            "hyperliquid",
            &AccountLabel::Default,
        )
    };
    assert_eq!(tier(&kept), Ok(AccountTier::Demo));
    assert_eq!(tier(&kept), tier(&full), "the predicate answers as before");
}
