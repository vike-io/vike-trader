//! The credentialed stream lane: which account it logs in as, what it refuses, and that no value
//! travels. The four real constructors are exercised where they are network-free (alpaca, ig and oanda
//! construct without dialling; ctrader's handshake is synchronous and is never built here).

use std::sync::Mutex;

use vike_data::RecordingSink;
use vike_model::AccountTier;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, Accounts};

use super::*;

/// A source that hands out a fixed directory.
struct Fixed(Mutex<Option<AccountDirectory>>);

impl AccountSource for Fixed {
    fn directory(&self, _venue: &str) -> AccountDirectory {
        self.0.lock().unwrap().clone().expect("a directory")
    }
}

fn fixed(directory: AccountDirectory) -> Arc<dyn AccountSource> {
    Arc::new(Fixed(Mutex::new(Some(directory))))
}

fn row(id: i64, venue: &str, tier: AccountTier) -> Account {
    Account {
        id,
        venue: venue.to_string(),
        tier,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
    }
}

fn directory(rows: Vec<Account>, book: CredentialBook) -> AccountDirectory {
    AccountDirectory::from_rows(Accounts::Known(rows), None).with_credentials(book)
}

fn sink() -> Arc<dyn LiveDataSink> {
    Arc::new(RecordingSink::default())
}

/// The roster the lane advertises is the registry's, in order.
#[test]
fn the_lane_serves_the_registry_roster() {
    let lane = CredentialedLane::over(
        fixed(AccountDirectory::unread()),
        Box::new(|_, _, _| Err("never built".into())),
    );
    assert_eq!(lane.venues(), owned(Lane::CredentialedStream));
    assert_eq!(lane.venues(), ["alpaca", "ctrader", "ig", "oanda"]);
}

/// **The account choice is `data_feed_credentials`**: the lane hands the builder a directory and the
/// builder resolves through it, so the demo row wins, a `paper` row is the `data_only` fallback, and a
/// `live` row is never the feed's login.
#[test]
fn the_builder_resolves_the_demo_row_then_the_paper_row_and_never_the_live_row() {
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let record = Arc::clone(&seen);
    let builder: CredClientBuilder = Box::new(move |venue, _sink, dir| {
        record
            .lock()
            .unwrap()
            .push(dir.data_feed_credentials(venue).get("API_KEY").map(str::to_string));
        Err("recorded".into())
    });

    let mut book = CredentialBook::new();
    book.insert_account(1, "API_KEY", "demo-key");
    book.insert_account(2, "API_KEY", "live-key");
    let both = directory(
        vec![row(1, "oanda", AccountTier::Demo), row(2, "oanda", AccountTier::Live)],
        book,
    );
    let lane = CredentialedLane::over(fixed(both), builder);
    assert_eq!(lane.open("oanda", sink()).err().as_deref(), Some("recorded"));

    let mut book = CredentialBook::new();
    book.insert_account(3, "API_KEY", "paper-key");
    let paper_only = directory(vec![row(3, "oanda", AccountTier::Paper)], book);
    let builder: CredClientBuilder = {
        let record = Arc::clone(&seen);
        Box::new(move |venue, _s, dir| {
            record
                .lock()
                .unwrap()
                .push(dir.data_feed_credentials(venue).get("API_KEY").map(str::to_string));
            Err("recorded".into())
        })
    };
    let lane = CredentialedLane::over(fixed(paper_only), builder);
    let _ = lane.open("oanda", sink());

    let mut book = CredentialBook::new();
    book.insert_account(4, "API_KEY", "live-only-key");
    let live_only = directory(vec![row(4, "oanda", AccountTier::Live)], book);
    let builder: CredClientBuilder = {
        let record = Arc::clone(&seen);
        Box::new(move |venue, _s, dir| {
            record
                .lock()
                .unwrap()
                .push(dir.data_feed_credentials(venue).get("API_KEY").map(str::to_string));
            Err("recorded".into())
        })
    };
    let _ = CredentialedLane::over(fixed(live_only), builder).open("oanda", sink());

    assert_eq!(
        *seen.lock().unwrap(),
        vec![Some("demo-key".to_string()), Some("paper-key".to_string()), None],
        "demo wins, paper stands behind a data_only venue, a live row is never the feed's login"
    );
}

/// **Absent credentials refuse the open, naming the fields and the verbs that write them** (the
/// tradehub's `venue_plan.rs` refusals, moved), and carry no value.
#[test]
fn absent_credentials_refuse_each_venue_naming_its_fields() {
    let empty = directory(Vec::new(), CredentialBook::new());
    for (venue, fields) in [
        ("alpaca", &["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"][..]),
        ("ctrader", &["CLIENT_ID", "CLIENT_SECRET", "ACCESS_TOKEN", "REFRESH_TOKEN"][..]),
        ("ig", &["API_KEY", "IDENTIFIER", "PASSWORD"][..]),
        ("oanda", &["API_KEY", "ACCOUNT_ID"][..]),
    ] {
        let err = build_credentialed(venue, sink(), &empty, None)
            .err()
            .unwrap_or_else(|| panic!("{venue}: absent credentials built a client"));
        assert!(err.contains("CREDENTIALED"), "{venue}: {err}");
        assert!(err.contains("DEFAULT"), "{venue}: {err}");
        for field in fields {
            assert!(err.contains(field), "{venue}: the refusal must name {field}: {err}");
        }
        assert!(
            err.contains(&format!("--venue {venue}")) && err.contains("--tier demo"),
            "{venue}: the refusal names the verbs that write the fields: {err}"
        );
    }
}

/// A venue outside the table is refused naming the table, never built.
#[test]
fn a_venue_outside_the_roster_has_no_credentialed_stream() {
    let err = build_credentialed("binance", sink(), &AccountDirectory::unread(), None)
        .err()
        .expect("binance is keyless");
    assert!(err.contains("alpaca, ctrader, ig, oanda"), "{err}");
}

/// The three construct-without-dialling venues build a client from planted credentials, and a partial
/// set (one field missing) still refuses; the refusal carries no value.
#[test]
fn alpaca_ig_and_oanda_build_without_dialling_and_a_partial_set_refuses_without_a_value() {
    let mut book = CredentialBook::new();
    for (id, field, value) in [
        (1, "CLIENT_ID", "fake-alpaca-id"),
        (1, "CLIENT_SECRET", "fake-alpaca-secret"),
        (1, "ACCOUNT_ID", "fake-alpaca-acct"),
        (2, "API_KEY", "fake-ig-key"),
        (2, "IDENTIFIER", "fake-ig-ident"),
        (2, "PASSWORD", "fake-ig-pass"),
        (3, "API_KEY", "fake-oanda-token"),
        (3, "ACCOUNT_ID", "fake-oanda-acct"),
    ] {
        book.insert_account(id, field, value);
    }
    let d = directory(
        vec![
            row(1, "alpaca", AccountTier::Demo),
            row(2, "ig", AccountTier::Demo),
            row(3, "oanda", AccountTier::Demo),
        ],
        book,
    );
    for venue in ["alpaca", "ig", "oanda"] {
        let mut client = build_credentialed(venue, sink(), &d, None)
            .unwrap_or_else(|e| panic!("{venue} did not build: {e}"));
        client.begin_shutdown();
        client.shutdown();
    }

    // An oanda account with its token but no account id: the pair is incomplete.
    let mut partial = CredentialBook::new();
    partial.insert_account(9, "API_KEY", "fake-secret-token-xyz");
    let d = directory(vec![row(9, "oanda", AccountTier::Demo)], partial);
    let err = build_credentialed("oanda", sink(), &d, None).err().expect("incomplete");
    assert!(!err.contains("fake-secret-token-xyz"), "a value reached a refusal: {err}");
}

/// An unreadable store is a store FAULT in the refusal's own words, and the builder is never asked.
#[test]
fn an_unreadable_store_refuses_as_a_fault_and_never_reaches_the_builder() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(dir.path()), b"not a sqlite database at all")
        .expect("plant the garbage");
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir.path())),
        vike_secrets::CredentialScope::Full,
    );
    let lane = CredentialedLane::over(
        super::super::accounts::StoreAccounts::source(store),
        Box::new(|_, _, _| panic!("the builder must not be asked of an unreadable store")),
    );
    let (result, events) = vike_log::capture::captured(|| lane.open("oanda", sink()));
    let err = result.err().expect("a refusal");
    assert!(err.contains("store fault") && err.contains("not an absent credential"), "{err}");
    assert!(
        events.iter().any(|e| e.level.to_string().to_ascii_uppercase().contains("ERROR")),
        "an unreadable store is logged at error"
    );
}

/// **Structural pins on the table's own text** (what a built client cannot show): one arm per roster
/// venue and no other, every arm resolves through the shared `creds`, the tier is the one constant, and
/// ctrader's handshake sits inside the arm (synchronous: the open's answer is the handshake's).
#[test]
fn the_table_has_one_arm_per_roster_venue_resolved_through_the_directory() {
    let src = include_str!("credentialed.rs");
    let body = &src[src.find("pub fn build_credentialed").expect("the fn")..];
    let body = &body[..body.find("#[cfg(test)]").expect("the tests")];
    let mut arms: Vec<&str> =
        body.lines().filter_map(|l| l.strip_prefix("        \"")?.split('"').next()).collect();
    arms.sort_unstable();
    let mut roster_sorted = roster(Lane::CredentialedStream).to_vec();
    roster_sorted.sort_unstable();
    assert_eq!(arms, roster_sorted, "the arms and the registry roster must be the same set");
    assert!(
        body.contains("directory.data_feed_credentials(venue)"),
        "the account choice moved whole"
    );
    assert_eq!(body.matches("FEED_TIER").count(), 4, "every arm logs in at the one tier constant");
    let ctrader =
        &body[body.find("\"ctrader\" =>").expect("the arm")..body.find("\"ig\" =>").expect("next")];
    assert!(ctrader.contains("connect_and_auth("), "ctrader's handshake is inside its arm");
    assert!(ctrader.contains("with_rotation_home"), "ctrader rotates into the declared home");
    assert_eq!(FEED_TIER, AccountTier::Demo);
}

/// **A synchronous open is bounded, so the gatewayhub's words are what the datahub's client reads.** ctrader's
/// handshake runs inside `DataOpen` (several replies, each up to the bridge's 15 s wait); past the budget
/// the open answers in words instead of leaving the caller to a bare "reply timed out" and a redial that
/// starts the handshake again. An answer in time comes through whole; a late one is dropped with its channel.
#[test]
fn a_synchronous_open_is_bounded_and_a_late_answer_is_dropped() {
    use std::time::{Duration, Instant};
    assert_eq!(bounded(Duration::from_secs(5), || 7), Ok(7));
    let started = Instant::now();
    let late = bounded(Duration::from_millis(100), || {
        std::thread::sleep(Duration::from_millis(800));
        8
    });
    assert_eq!(
        late,
        Err(NoAnswer::TimedOut),
        "the open did not wait for a handshake past its budget"
    );
    assert!(started.elapsed() < Duration::from_millis(600), "{:?}", started.elapsed());
    // The budget sits under the wait of the client that asked (the protocol constant says so too).
    assert!(
        CTRADER_HANDSHAKE_BUDGET < vike_gatewayhub_client::proto::LOOKUP_REPLY_TIMEOUT,
        "the handshake budget must sit under the datahub client's open-reply wait"
    );
}

/// **A handshake that crashes is not "slow"**: the thread panicking reads as `Panicked`, which the arm words as a
/// bug to report, where it used to be the same "did not finish within 12 s" a slow venue gets.
#[test]
fn a_handshake_that_panics_is_not_a_timeout() {
    use std::time::Duration;
    // The panic is expected and reported by the default hook on stderr; the answer is what is asserted.
    let r: Result<u8, NoAnswer> = bounded(Duration::from_secs(5), || panic!("a scripted crash"));
    assert_eq!(r, Err(NoAnswer::Panicked));
}

/// **Abandoned handshakes cannot pile up**: past the cap a new open is refused in words (with how many are
/// still running) instead of starting another thread and a concurrent login on the account, and a place is given
/// back when its handshake ends. On a gauge of its own, so the process-wide count is not shared with other tests.
#[test]
fn abandoned_handshakes_are_capped_and_give_their_place_back() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};
    static GAUGE: AtomicUsize = AtomicUsize::new(0);
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(Barrier::new(3));
    for _ in 0..2 {
        let (release, started) = (Arc::clone(&release), Arc::clone(&started));
        let r = bounded_in(&GAUGE, 2, Duration::from_millis(30), move || {
            started.wait();
            while !release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        assert_eq!(r, Err(NoAnswer::TimedOut), "abandoned after its budget");
    }
    started.wait(); // both handshake threads are running now
    assert_eq!(GAUGE.load(Ordering::SeqCst), 2);
    assert_eq!(
        bounded_in(&GAUGE, 2, Duration::from_secs(1), || 1),
        Err(NoAnswer::TooMany(2)),
        "the third is refused, naming how many run"
    );
    assert_eq!(GAUGE.load(Ordering::SeqCst), 2, "a refusal takes no place");
    release.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(10);
    while GAUGE.load(Ordering::SeqCst) != 0 {
        assert!(Instant::now() < deadline, "the places were never given back");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(bounded_in(&GAUGE, 2, Duration::from_secs(5), || 9), Ok(9));
}
