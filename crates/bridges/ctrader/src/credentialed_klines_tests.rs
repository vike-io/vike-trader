//! The cTrader history lane's grant read, presence probe, startup line and connection sources, tested
//! where they can be named (`read_history_grant` is `pub(crate)`: it returns a connection config holding
//! the OAuth tokens, so no dependent crate may call it). Moved here from the gatewayhub with the lane; a
//! CONNECTION is one `connection_source`, and the engine's half (one source per connection, the cancel
//! before any slot, the dispatch) is the gatewayhub's `src/rest/credentialed_klines_tests.rs` and
//! `src/rest/lane_tests.rs`.
//!
//! The grant read is driven over hand-built `AccountDirectory` values (no database), the presence probe
//! through a real settings DATABASE planted in a temp directory (the property it claims is about SQLite,
//! so a fake would prove nothing), and the connection sources over a scripted `KlineSource` that asks its
//! provider the way the real source does when a session opens. Every credential value is an obviously
//! fake, distinctive string, so "no value reached a log line" is an assertion that could actually fail.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_bridge_core::credential_fixture::{under_capture, write_store};
use vike_data::source::SourceError;
use vike_marketdata::Bar;
use vike_secrets::account_credentials::CredentialRegistry;
use vike_secrets::{AccountRow, AccountRows};

use super::*;

const APP_ID: &str = "fake-app-id-3b6k";
const APP_SECRET: &str = "fake-app-secret-8v2n";
const DEMO_ACCESS: &str = "fake-demo-access-7q4z";
const DEMO_REFRESH: &str = "fake-demo-refresh-1h9d";
const LIVE_ACCESS: &str = "fake-live-access-5w3x";
const LIVE_REFRESH: &str = "fake-live-refresh-6c8m";
const PAPER_ACCESS: &str = "fake-paper-access-2j7t";
const PAPER_REFRESH: &str = "fake-paper-refresh-4y5r";

fn every_value() -> [&'static str; 8] {
    [
        APP_ID,
        APP_SECRET,
        DEMO_ACCESS,
        DEMO_REFRESH,
        LIVE_ACCESS,
        LIVE_REFRESH,
        PAPER_ACCESS,
        PAPER_REFRESH,
    ]
}

fn row(id: i64, tier: AccountTier) -> AccountRow {
    AccountRow {
        id,
        venue: VENUE.to_string(),
        tier,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: vike_secrets::fixture_account_symbol(VENUE).map(str::to_string),
        facts: Default::default(),
    }
}

/// The application pair, and a token pair per `(account id, access, refresh)`.
fn book(app: bool, accounts: &[(i64, &str, &str)]) -> CredentialRegistry {
    let mut book = CredentialRegistry::new();
    if app {
        book.insert_venue(VENUE, "CLIENT_ID", APP_ID);
        book.insert_venue(VENUE, "CLIENT_SECRET", APP_SECRET);
    }
    for &(id, access, refresh) in accounts {
        book.insert_account(id, ACCESS_TOKEN, access);
        book.insert_account(id, REFRESH_TOKEN, refresh);
    }
    book
}

fn directory(rows: Vec<AccountRow>, book: CredentialRegistry) -> AccountDirectory {
    AccountDirectory::from_rows(AccountRows::Known(rows), None).with_credentials(book)
}

/// A read of one fixed directory that counts the reads made of it.
struct Counting {
    directory: Mutex<AccountDirectory>,
    reads: AtomicUsize,
}

impl Counting {
    fn over(directory: AccountDirectory) -> Arc<Self> {
        Arc::new(Self { directory: Mutex::new(directory), reads: AtomicUsize::new(0) })
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    /// The directory, counted, as the spec's [`AccountDirectoryRead`] asks it.
    fn read(self: &Arc<Self>) -> AccountDirectoryRead {
        let me = Arc::clone(self);
        Arc::new(move |_venue: &str| {
            me.reads.fetch_add(1, Ordering::SeqCst);
            me.directory.lock().expect("directory lock").clone()
        })
    }
}

fn nowhere() -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(None, vike_secrets::CredentialScope::Full)
}

/// A scripted source that asks its provider for the config the way the real one does when a session
/// opens, and answers an empty window or the provider's refusal.
struct Asking {
    config: HistoryConfigProvider,
}

impl KlineSource for Asking {
    fn venue(&self) -> &str {
        VENUE
    }
    fn fetch(&self, _: &str, _: &str, _: i64, _: i64) -> Result<Vec<Bar>, SourceError> {
        match (self.config)() {
            Ok(_) => Ok(Vec::new()),
            Err(e) => Err(SourceError::Refused(e.carried())),
        }
    }
}

fn asking() -> SourceFor {
    Arc::new(|config: HistoryConfigProvider| -> Box<dyn KlineSource> {
        Box::new(Asking { config })
    })
}

fn real() -> SourceFor {
    Arc::new(|config: HistoryConfigProvider| -> Box<dyn KlineSource> {
        Box::new(KlineSourceCtrader::new(config))
    })
}

/// The release seed's ctrader rows: a box's `venue_url` table before an operator moves one.
fn seed() -> VenueUrls {
    VenueUrls::seed(VENUE)
}

fn spec(accounts: &Arc<Counting>, source_for: SourceFor) -> CredentialedKlinesSpecCtrader {
    CredentialedKlinesSpecCtrader::with_source(nowhere(), accounts.read(), seed(), source_for)
}

fn never() -> bool {
    false
}

fn fetch(source: &dyn CredentialedKlinesSource) -> Result<Vec<Bar>, SourceError> {
    source.fetch_until("EURUSD", "1m", 0, 60_000, &never)
}

// ------------------------------------------------------------------------------------------------
// the grant read
// ------------------------------------------------------------------------------------------------

/// **The grant is the unlabelled DEMO account's token pair with the venue's application pair, on the demo
/// host, and nothing else.** Planted beside it: a live account's pair and a paper account's.
#[test]
fn the_grant_is_the_demo_accounts_with_the_venues_application_pair() {
    let rows =
        vec![row(1, AccountTier::Demo), row(2, AccountTier::Live), row(3, AccountTier::Paper)];
    let book = book(
        true,
        &[
            (1, DEMO_ACCESS, DEMO_REFRESH),
            (2, LIVE_ACCESS, LIVE_REFRESH),
            (3, PAPER_ACCESS, PAPER_REFRESH),
        ],
    );
    let grant = read_history_grant(&directory(rows, book), &seed()).expect("a grant");
    let config = grant.expose();
    assert_eq!(config.access_token.expose(), DEMO_ACCESS);
    assert_eq!(config.refresh_token.as_ref().map(|t| t.expose().as_str()), Some(DEMO_REFRESH));
    assert_eq!(
        (config.client_id.as_str(), config.client_secret.expose().as_str()),
        (APP_ID, APP_SECRET)
    );
    let demo = ctrader_host(&seed(), AccountTier::Demo).expect("the seed's demo rows");
    assert_eq!(
        (&config.host, config.port),
        (&demo.host, demo.port),
        "the demo socket and no other"
    );
    assert_eq!(config.oauth_base, demo.oauth_base, "the demo login row");
    assert!(!config.no_tls, "a production session speaks TLS");
}

/// **No demo address row, no dial**: a whole grant on rows that cannot answer the demo addresses is
/// `AddressRefused`, the row named at `error!`, and no line carries a value.
#[test]
fn a_demo_address_the_rows_cannot_answer_is_refused_by_name() {
    let demo =
        directory(vec![row(1, AccountTier::Demo)], book(true, &[(1, DEMO_ACCESS, DEMO_REFRESH)]));
    let (out, lines) = under_capture(|| read_history_grant(&demo, &VenueUrls::default()).err());
    assert_eq!(out, Some(HistoryConfigError::AddressRefused));
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "logged once: {lines:?}");
    assert!(errors[0].contains("trade socket"), "the row is named: {}", errors[0]);
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(value)), "a value reached the log: {lines:?}");
    }
}

/// **Anything but the one active unlabelled demo account is NO grant** - a live account, the `paper`
/// stand-in the strategy feed falls back to, an account whose pair is incomplete, a venue without its
/// application pair and a store that holds nothing. Each is `NotConfigured`, never a guess.
#[test]
fn a_row_that_is_not_the_demo_account_with_a_whole_grant_is_no_grant() {
    let cases: Vec<(&str, AccountDirectory)> = vec![
        (
            "live only",
            directory(
                vec![row(2, AccountTier::Live)],
                book(true, &[(2, LIVE_ACCESS, LIVE_REFRESH)]),
            ),
        ),
        (
            "paper only (the feed's stand-in is not this lane's)",
            directory(
                vec![row(3, AccountTier::Paper)],
                book(true, &[(3, PAPER_ACCESS, PAPER_REFRESH)]),
            ),
        ),
        (
            "no application pair",
            directory(
                vec![row(1, AccountTier::Demo)],
                book(false, &[(1, DEMO_ACCESS, DEMO_REFRESH)]),
            ),
        ),
        ("no tokens", directory(vec![row(1, AccountTier::Demo)], book(true, &[]))),
        ("no accounts", directory(Vec::new(), book(true, &[]))),
        ("nothing read", AccountDirectory::unread()),
    ];
    for (what, directory) in cases {
        let out = read_history_grant(&directory, &seed()).err();
        assert_eq!(out, Some(HistoryConfigError::NotConfigured), "{what}");
    }

    // A pair with only one half stored is no pair either.
    let mut half = CredentialRegistry::new();
    half.insert_venue(VENUE, "CLIENT_ID", APP_ID);
    half.insert_venue(VENUE, "CLIENT_SECRET", APP_SECRET);
    half.insert_account(1, ACCESS_TOKEN, DEMO_ACCESS);
    assert_eq!(
        read_history_grant(&directory(vec![row(1, AccountTier::Demo)], half), &seed()).err(),
        Some(HistoryConfigError::NotConfigured),
        "an access token without its refresh token"
    );
}

/// **A store that exists and will not read is `StoreUnreadable`, never `NotConfigured`**, says so in an
/// `error!`, and no line carries a value.
#[test]
fn an_unreadable_store_is_not_an_absent_grant_and_no_line_carries_a_value() {
    let broken =
        AccountDirectory::from_rows(AccountRows::Known(vec![row(1, AccountTier::Demo)]), None)
            .with_read_credentials(Err::<CredentialRegistry, String>(
                "the file would not open".into(),
            ));
    let (out, lines) = under_capture(|| read_history_grant(&broken, &seed()).err());
    assert_eq!(out, Some(HistoryConfigError::StoreUnreadable));
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "logged once: {lines:?}");
    assert!(errors[0].contains("NOT the same as the grant being absent"), "{}", errors[0]);
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(value)), "a value reached the log: {lines:?}");
    }
}

// ------------------------------------------------------------------------------------------------
// the connection sources
// ------------------------------------------------------------------------------------------------

fn demo_directory() -> AccountDirectory {
    directory(vec![row(1, AccountTier::Demo)], book(true, &[(1, DEMO_ACCESS, DEMO_REFRESH)]))
}

/// **Read once per CONNECTION**: two fetches of one connection's source read the store once, the next
/// connection's source reads it again (a grant stored, rotated or removed between two requests is what
/// the next one sees).
#[test]
fn the_grant_is_read_once_per_connection_and_again_for_the_next() {
    let accounts = Counting::over(demo_directory());
    let spec = spec(&accounts, asking());
    let first = spec.connection_source();
    assert_eq!(fetch(first.as_ref()).expect("a fetch"), Vec::new());
    assert_eq!(fetch(first.as_ref()).expect("a fetch"), Vec::new());
    assert_eq!(accounts.reads(), 1, "the second fetch of one connection reads nothing");
    let second = spec.connection_source();
    fetch(second.as_ref()).expect("a fetch");
    assert_eq!(accounts.reads(), 2, "the next connection reads the store afresh");
}

/// An interval the venue does not serve is refused by the REAL source before a session opens, so the
/// grant is never read for it. The directory holds NO grant on purpose: this test builds the real
/// `KlineSourceCtrader`, and were the crate's own rule ever to stop refusing `1w`, a present grant would let it
/// dial the real cTrader demo host; with none, the provider is asked, finds nothing and the read count
/// below catches the regression with no socket.
#[test]
fn a_refusal_before_the_session_reads_no_grant() {
    let accounts = Counting::over(AccountDirectory::unread());
    let source = spec(&accounts, real()).connection_source();
    let out = source.fetch_until("EURUSD", "1w", 0, 60_000, &never);
    let calendar = crate::refusals::CtraderHistoryRefusal::CalendarWidth { interval: "1w".into() };
    assert!(matches!(&out, Err(SourceError::Refused(t)) if *t == calendar.carried()), "{out:?}");
    assert_eq!(accounts.reads(), 0);
}

/// **An absent grant is a refusal that names the fields, kept for the connection**: the venue was not
/// asked, the store was read once, and the second fetch of the connection is refused from the memo.
#[test]
fn an_absent_grant_is_a_refusal_naming_the_fields_and_is_read_once() {
    let accounts = Counting::over(AccountDirectory::unread());
    let source = spec(&accounts, real()).connection_source();
    for _ in 0..2 {
        let out = fetch(source.as_ref());
        assert!(
            matches!(&out, Err(SourceError::Refused(t))
                if *t == HistoryConfigError::NotConfigured.carried()),
            "{out:?}"
        );
    }
    assert_eq!(accounts.reads(), 1, "the refusal is kept for the connection");
}

// ------------------------------------------------------------------------------------------------
// the presence word
// ------------------------------------------------------------------------------------------------

fn pair(field: &str, value: &str) -> (String, String) {
    (field.to_string(), value.to_string())
}

fn at(dir: &std::path::Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
}

/// **The presence word, in every state a store can be in, and never a value.** The demo account's whole
/// token pair is `Present`; half a pair, a live account's pair and a store with nothing are `Absent`; a
/// store that exists and will not read is `Unreadable` (logged, and not `Absent`); no directory is `Absent`.
#[test]
fn the_presence_word_says_each_state_and_no_line_carries_a_value() {
    // ctrader rows, unlabelled (`vike_bridge_core::credential_fixture::write_store` plants them 0600 on
    // unix).
    let demo_pair = (
        VENUE,
        AccountTier::Demo,
        None,
        vec![pair(ACCESS_TOKEN, DEMO_ACCESS), pair(REFRESH_TOKEN, DEMO_REFRESH)],
    );
    let live_pair = (
        VENUE,
        AccountTier::Live,
        None,
        vec![pair(ACCESS_TOKEN, LIVE_ACCESS), pair(REFRESH_TOKEN, LIVE_REFRESH)],
    );
    let half = (VENUE, AccountTier::Demo, None, vec![pair(ACCESS_TOKEN, DEMO_ACCESS)]);

    let whole = tempfile::tempdir().expect("a temp dir");
    write_store(whole.path(), &[demo_pair.clone(), live_pair.clone()]);
    assert!(whole.path().join("db").join("vike.db").is_file(), "the fixture must be a database");
    let half_pair = tempfile::tempdir().expect("a temp dir");
    write_store(half_pair.path(), &[half]);
    let live_only = tempfile::tempdir().expect("a temp dir");
    write_store(live_only.path(), &[live_pair]);
    let empty = tempfile::tempdir().expect("a temp dir");
    write_store(empty.path(), &[]);
    let unreadable = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(unreadable.path().join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(unreadable.path()), b"not a sqlite database")
        .expect("plant the garbage");

    let (words, lines) = under_capture(|| {
        [
            ctrader_history_presence(&nowhere()),
            ctrader_history_presence(&at(whole.path())),
            ctrader_history_presence(&at(half_pair.path())),
            ctrader_history_presence(&at(live_only.path())),
            ctrader_history_presence(&at(empty.path())),
            ctrader_history_presence(&at(unreadable.path())),
        ]
    });
    assert_eq!(
        words,
        [
            CredentialPresence::Absent,
            CredentialPresence::Present,
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Unreadable,
        ]
    );
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
    assert!(errors[0].contains("NOT the same as the grant being absent"), "{}", errors[0]);
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(value)), "a value reached the log: {lines:?}");
    }
}

/// The startup line says each state in words, under the prefix an operator greps, and holds no value.
#[test]
fn the_startup_line_says_armed_not_armed_and_unknown() {
    let dir = std::path::Path::new("/some/settings");
    let armed = ctrader_history_lane_line(Some(dir), CredentialPresence::Present);
    let absent = ctrader_history_lane_line(Some(dir), CredentialPresence::Absent);
    let unknown = ctrader_history_lane_line(Some(dir), CredentialPresence::Unreadable);
    let nowhere = ctrader_history_lane_line(None, CredentialPresence::Absent);
    assert!(armed.starts_with("ctrader history lane: ARMED"), "{armed}");
    assert!(absent.starts_with("ctrader history lane: not armed"), "{absent}");
    assert!(absent.contains("ctrader_authorize") && absent.contains("CLIENT_ID"), "{absent}");
    assert!(unknown.starts_with("ctrader history lane: UNKNOWN"), "{unknown}");
    assert!(unknown.contains("not the same as the grant being absent"), "{unknown}");
    assert!(nowhere.contains("no settings directory"), "{nowhere}");
    for line in [armed, absent, unknown, nowhere] {
        for value in every_value() {
            assert!(!line.contains(value), "a value reached a startup line: {line}");
        }
    }
}
