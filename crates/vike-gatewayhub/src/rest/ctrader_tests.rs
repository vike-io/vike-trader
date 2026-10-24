//! The cTrader history lane's grant read, presence probe, startup line and per-connection memo, tested
//! where they can be named (`read_history_grant` is `pub(crate)`: it returns a connection config holding
//! the OAuth tokens, so no dependent crate may call it).
//!
//! The grant read is driven through a scripted `AccountSource` over hand-built `AccountDirectory` values
//! (no database), the presence probe through a real settings DATABASE planted in a temp directory (the
//! property it claims is about SQLite, so a fake would prove nothing), and the lane over a scripted
//! `KlineSource` that asks its provider the way the real source does when a session opens. Every
//! credential value is an obviously fake, distinctive string, so "no value reached a log line" is an
//! assertion that could actually fail.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::source::SourceError;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, Accounts};

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

fn row(id: i64, tier: AccountTier) -> Account {
    Account {
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
    }
}

/// The application pair, and a token pair per `(account id, access, refresh)`.
fn book(app: bool, accounts: &[(i64, &str, &str)]) -> CredentialBook {
    let mut book = CredentialBook::new();
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

fn directory(rows: Vec<Account>, book: CredentialBook) -> AccountDirectory {
    AccountDirectory::from_rows(Accounts::Known(rows), None).with_credentials(book)
}

/// A source of one fixed directory that counts the reads made of it.
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
}

impl AccountSource for Counting {
    fn directory(&self, _venue: &str) -> AccountDirectory {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.directory.lock().expect("directory lock").clone()
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
            Err(e) => Err(SourceError::Refused(e.to_string())),
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
        Box::new(CtraderKlines::new(config))
    })
}

fn lane(accounts: &Arc<Counting>, source_for: SourceFor) -> CtraderLane {
    CtraderLane::with_source(nowhere(), Arc::clone(accounts) as Arc<dyn AccountSource>, source_for)
}

fn fetch(lane: &CtraderLane, scope: &ConnectionScope) -> Result<Vec<Bar>, FetchFailure> {
    lane.fetch(scope, "EURUSD", "1m", 0, 60_000, &FetchCtx::new())
}

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
    let accounts = Counting::over(directory(rows, book));
    let grant = read_history_grant(accounts.as_ref()).expect("a grant");
    let cfg = grant.expose();
    assert_eq!(cfg.access_token, DEMO_ACCESS);
    assert_eq!(cfg.refresh_token.as_deref(), Some(DEMO_REFRESH));
    assert_eq!((cfg.client_id.as_str(), cfg.client_secret.as_str()), (APP_ID, APP_SECRET));
    assert_eq!(cfg.host, "demo.ctraderapi.com", "the demo host and no other");
    assert!(!cfg.no_tls, "a production session speaks TLS");
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
        let accounts = Counting::over(directory);
        let out = read_history_grant(accounts.as_ref()).err();
        assert_eq!(out, Some(HistoryConfigError::NotConfigured), "{what}");
    }

    // A pair with only one half stored is no pair either.
    let mut half = CredentialBook::new();
    half.insert_venue(VENUE, "CLIENT_ID", APP_ID);
    half.insert_venue(VENUE, "CLIENT_SECRET", APP_SECRET);
    half.insert_account(1, ACCESS_TOKEN, DEMO_ACCESS);
    let accounts = Counting::over(directory(vec![row(1, AccountTier::Demo)], half));
    assert_eq!(
        read_history_grant(accounts.as_ref()).err(),
        Some(HistoryConfigError::NotConfigured),
        "an access token without its refresh token"
    );
}

/// **A store that exists and will not read is `StoreUnreadable`, never `NotConfigured`**, says so in an
/// `error!`, and no line carries a value.
#[test]
fn an_unreadable_store_is_not_an_absent_grant_and_no_line_carries_a_value() {
    let broken =
        AccountDirectory::from_rows(Accounts::Known(vec![row(1, AccountTier::Demo)]), None)
            .with_read_credentials(Err::<CredentialBook, String>("the file would not open".into()));
    let accounts = Counting::over(broken);
    let (out, lines) = under_capture(|| read_history_grant(accounts.as_ref()).err());
    assert_eq!(out, Some(HistoryConfigError::StoreUnreadable));
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "logged once: {lines:?}");
    assert!(errors[0].contains("NOT the same as the grant being absent"), "{}", errors[0]);
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(value)), "a value reached the log: {lines:?}");
    }
}

// ------------------------------------------------------------------------------------------------
// the lane
// ------------------------------------------------------------------------------------------------

fn demo_directory() -> AccountDirectory {
    directory(vec![row(1, AccountTier::Demo)], book(true, &[(1, DEMO_ACCESS, DEMO_REFRESH)]))
}

/// **Read once per CONNECTION**: two fetches of one connection read the store once, the next connection
/// reads it again (a grant stored, rotated or removed between two requests is what the next one sees).
#[test]
fn the_grant_is_read_once_per_connection_and_again_for_the_next() {
    let accounts = Counting::over(demo_directory());
    let lane = lane(&accounts, asking());
    let first = ConnectionScope::new();
    assert_eq!(fetch(&lane, &first).expect("a fetch"), Vec::new());
    assert_eq!(fetch(&lane, &first).expect("a fetch"), Vec::new());
    assert_eq!(accounts.reads(), 1, "the second fetch of one connection reads nothing");
    let second = ConnectionScope::new();
    fetch(&lane, &second).expect("a fetch");
    assert_eq!(accounts.reads(), 2, "the next connection reads the store afresh");
}

/// An interval the venue does not serve is refused by the REAL source before a session opens, so the
/// grant is never read for it. The directory holds NO grant on purpose: this test builds the real
/// `CtraderKlines`, and were the crate's own rule ever to stop refusing `1w`, a present grant would let it
/// dial the real cTrader demo host; with none, the provider is asked, finds nothing and the read count
/// below catches the regression with no socket.
#[test]
fn a_refusal_before_the_session_reads_no_grant() {
    let accounts = Counting::over(AccountDirectory::unread());
    let lane = lane(&accounts, real());
    let scope = ConnectionScope::new();
    let out = lane.fetch(&scope, "EURUSD", "1w", 0, 60_000, &FetchCtx::new());
    assert!(
        matches!(&out, Err(FetchFailure::Refused(t)) if t.contains("calendar-width")),
        "{out:?}"
    );
    assert_eq!(accounts.reads(), 0);
}

/// **An absent grant is a refusal that names the fields, kept for the connection**: the venue was not
/// asked, the store was read once, and the second fetch of the connection is refused from the memo.
#[test]
fn an_absent_grant_is_a_refusal_naming_the_fields_and_is_read_once() {
    let accounts = Counting::over(AccountDirectory::unread());
    let lane = lane(&accounts, real());
    let scope = ConnectionScope::new();
    for _ in 0..2 {
        let out = fetch(&lane, &scope);
        assert!(
            matches!(&out, Err(FetchFailure::Refused(t))
                if t.contains("CLIENT_ID") && t.contains(ACCESS_TOKEN) && t.contains("demo")),
            "{out:?}"
        );
    }
    assert_eq!(accounts.reads(), 1, "the refusal is kept for the connection");
}

/// A fetch cancelled before the venue was asked reads nothing and creates no slot.
#[test]
fn a_cancelled_fetch_reads_nothing_and_creates_no_slot() {
    let accounts = Counting::over(demo_directory());
    let lane = lane(&accounts, asking());
    let scope = ConnectionScope::new();
    let ctx = FetchCtx::new();
    ctx.cancel();
    let out = lane.fetch(&scope, "EURUSD", "1m", 0, 60_000, &ctx);
    assert!(matches!(&out, Err(FetchFailure::Fetch(t)) if t.contains("cancelled")), "{out:?}");
    assert_eq!((accounts.reads(), scope.is_empty()), (0, true));
}

// ------------------------------------------------------------------------------------------------
// the presence word
// ------------------------------------------------------------------------------------------------

/// One planted account: its tier and the `(field, value)` pairs filed against it.
type Planted = (AccountTier, Vec<(String, String)>);

fn pair(field: &str, value: &str) -> (String, String) {
    (field.to_string(), value.to_string())
}

/// Make the settings DATABASE hold exactly `accounts` (ctrader rows, unlabelled), 0600 on unix. The store
/// is created the one way a store comes into being; each account is created with its tier FIRST and its
/// fields are then written against it by owner and field.
fn write_store(settings: &std::path::Path, accounts: &[Planted]) {
    vike_secrets::create_store(Some(settings.to_str().expect("a utf-8 temp path")))
        .expect("create the empty store");
    for (tier, fields) in accounts {
        let created = vike_secrets::edit_account_in_unjournalled(
            settings,
            vike_secrets::AccountEdit::Create { venue: VENUE, tier: *tier, label: None },
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
    let demo_pair = (
        AccountTier::Demo,
        vec![pair(ACCESS_TOKEN, DEMO_ACCESS), pair(REFRESH_TOKEN, DEMO_REFRESH)],
    );
    let live_pair = (
        AccountTier::Live,
        vec![pair(ACCESS_TOKEN, LIVE_ACCESS), pair(REFRESH_TOKEN, LIVE_REFRESH)],
    );
    let half = (AccountTier::Demo, vec![pair(ACCESS_TOKEN, DEMO_ACCESS)]);

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

// ------------------------------------------------------------------------------------------------
// the dispatch
// ------------------------------------------------------------------------------------------------

/// **A credentialed fetch for ctrader reaches the ctrader lane and no other**, through the real
/// `VenueRest` dispatch: the lane armed alone answers its own venue's klines and refuses every other
/// venue's (the venue was not asked), and the answer is the lane's `Vec<Bar>` as `FetchRows::Bars`.
#[test]
fn a_credentialed_fetch_for_ctrader_reaches_the_ctrader_lane_only() {
    use vike_gatewayhub_client::proto::{FetchRows, FetchWhat};

    use crate::rest::RestLane;
    use crate::rest::lane::VenueRest;

    let accounts = Counting::over(demo_directory());
    let rest = VenueRest::new(false).with_ctrader(lane(&accounts, asking()));
    let klines = |venue: &str| FetchWhat::Klines {
        venue: venue.to_string(),
        symbol: "EURUSD".to_string(),
        interval: "1m".to_string(),
        start_ms: 0,
        end_ms: 60_000,
    };
    let scope = ConnectionScope::new();
    let served = rest.fetch(&scope, &klines(VENUE), true, &FetchCtx::new());
    assert!(matches!(&served, Ok(FetchRows::Bars(bars)) if bars.is_empty()), "{served:?}");
    assert_eq!(accounts.reads(), 1, "the lane read the grant for the connection that asked");
    for other in ["oanda", "ig"] {
        let refused = rest.fetch(&scope, &klines(other), true, &FetchCtx::new());
        assert!(matches!(refused, Err(FetchFailure::Refused(_))), "{other}: {refused:?}");
    }
    assert_eq!(accounts.reads(), 1, "a refusal for another venue read nothing");
}
