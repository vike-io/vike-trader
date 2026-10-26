//! The OANDA practice-token lane (decision 0097, amended by 0164): the startup line, and the
//! CONNECTION-SCOPED memo the one credential read lives in — through the real `VenueRest`, the real
//! server and the client's `RemoteKlines::credentialed`. The READS themselves (what the store answers,
//! the presence word, the permission finding) are `pub(crate)` - they return the plaintext token - and
//! are tested in the crate: `crates/vike-gatewayhub/src/rest/oanda_tests.rs`.
//!
//! Drift register row R7 held here: the token is read once per request and dropped with it
//! (a request is a connection: the client's source holds one), a second request reads the store
//! again, and an unreadable store is not an absent key. Moved from the datahub's
//! `datahub_cli_tests.rs` `oanda_history` module (the credential tests) and rewritten over the lane:
//! what only THIS root can get wrong is the startup line, and that nothing it builds holds the token
//! past its connection.
//!
//! Every fixture plants ACCOUNT ROWS and writes their FIELDS (decision 0156 item 6) — no credential
//! NAME is composed or spelled — and every VALUE is an obviously fake, distinctive string, so "no
//! value reached a log line" is an assertion that could actually fail. The real `OandaKlines` is never
//! built here: a present token would send CI to OANDA's practice host.

#![cfg(feature = "data")]

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;
#[path = "support/rest_server.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod rest_server;

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use vike_data::source::{KlineSource, SourceError};
use vike_gatewayhub::rest::lane::VenueRest;
use vike_gatewayhub::rest::oanda::{OandaLane, oanda_history_lane_line};
use vike_gatewayhub::session::ConnectionScope;
use vike_gatewayhub_client::credential::{CredentialPresence, credential_presence};
use vike_gatewayhub_client::history::RemoteKlines;
use vike_gatewayhub_client::proto::{FetchWhat, Request, Response};
use vike_hub_proto::auth::Scope;
use vike_marketdata::Bar;
use vike_model::AccountTier;
use vike_oanda::{HistoryTokenError, HistoryTokenProvider};

use harness::test_keys;
use raw_peer::Raw;
use rest_server::RestServer;

/// Every event's level, then every field — `message` first — as ` name=value`, one line per event:
/// the shape the assertions below read (`starts_with("ERROR")`, and "no line carries a value", which
/// needs EVERY field — an `error = %e` field is where a store error's text lands, and a value there
/// would be as much a leak as one in the message). Projected from `vike_log::capture::captured`, the
/// workspace's one scoped capture.
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

/// The token's FIELD on the practice account.
const TOKEN_FIELD: &str = vike_venues::credential_keys::API_KEY;
/// The oanda account's other field — the neighbour on the SAME account the read must leave.
const ACCOUNT_ID_FIELD: &str = "ACCOUNT_ID";

const PRACTICE_VALUE: &str = "fake-practice-token-7q4z";

/// One planted account: its venue, tier and label, and the `(field, value)` pairs filed against it.
type Planted = (&'static str, AccountTier, Option<&'static str>, Vec<(String, String)>);

/// A store's worth of accounts around the practice account, with the practice account's token set to
/// `token` (`None` = the account holds no token). The NEIGHBOURS the one-field read must leave
/// behind: the practice account's own account id, the LIVE oanda account's pair, the PAPER oanda
/// account's key, a LABELLED demo oanda account's key, and another venue's key. Every value
/// distinctive.
fn store_rows(token: Option<&str>) -> Vec<Planted> {
    let pair = |field: &str, value: &str| (field.to_string(), value.to_string());
    let mut practice = vec![pair(ACCOUNT_ID_FIELD, "fake-account-id-2k9d")];
    if let Some(token) = token {
        practice.push(pair(TOKEN_FIELD, token));
    }
    vec![
        ("oanda", AccountTier::Demo, None, practice),
        (
            "oanda",
            AccountTier::Live,
            None,
            vec![
                pair(TOKEN_FIELD, "fake-live-token-8m3x"),
                pair(ACCOUNT_ID_FIELD, "fake-live-account-5j1c"),
            ],
        ),
        ("oanda", AccountTier::Paper, None, vec![pair(TOKEN_FIELD, "fake-sim-token-3w7n")]),
        (
            "oanda",
            AccountTier::Demo,
            Some("ALT"),
            vec![pair(TOKEN_FIELD, "fake-labelled-token-9p5r")],
        ),
        ("binance", AccountTier::Live, None, vec![pair(TOKEN_FIELD, "fake-binance-key-4t8y")]),
    ]
}

/// Every neighbour, the practice account holding NO token.
fn neighbours() -> Vec<Planted> {
    store_rows(None)
}

/// The practice token plus every neighbour.
fn full_store_rows() -> Vec<Planted> {
    store_rows(Some(PRACTICE_VALUE))
}

/// Every planted value, the practice token included — what no log line may carry.
fn every_value() -> Vec<String> {
    let mut values: Vec<String> = store_rows(None)
        .into_iter()
        .flat_map(|(_, _, _, fields)| fields.into_iter().map(|(_, v)| v))
        .collect();
    values.push(PRACTICE_VALUE.to_string());
    values
}

/// Make the settings DATABASE — the only credential store — hold exactly `accounts`, 0600 on unix so
/// the permission finding is only what a test asks for. The store is created the one way a store
/// comes into being (`vike-cli secrets init`'s library half); each account is created with its tier
/// FIRST and its fields are then written against it by owner and field — the one way an account's
/// credential reaches a store. A store being REPLACED (a rotation, a removal) is rebuilt in this
/// test's own temp directory: the writer adds and replaces fields and never removes one.
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

/// Bytes that are not a database where the database belongs: a store that EXISTS and will not open
/// (`chmod 000` proves nothing as root, which CI is).
fn plant_unreadable_store(settings: &std::path::Path) {
    std::fs::create_dir_all(settings.join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(settings), b"not a sqlite database at all")
        .expect("plant the garbage");
}

/// The handle the boot would hand this root for a settings directory `dir` (the full scope).
fn at(dir: &std::path::Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
}

/// The startup line names the token by its ACCOUNT and FIELD and says what to do, and — taking a
/// `Result<(), _>` — can carry no value by construction. Four states, four texts.
#[test]
fn the_startup_line_names_the_key_and_what_to_do() {
    let key = format!("the {TOKEN_FIELD} of the ACTIVE unlabelled oanda account at tier `demo`");
    let dir = std::path::Path::new("settings");
    let armed = oanda_history_lane_line(Some(dir), Ok(()));
    assert!(armed.contains("ARMED") && armed.contains(&key), "{armed}");
    assert!(
        armed.contains("FetchCredentialed")
            && armed.contains("Control key")
            && armed.contains("only the Observe key never reaches the lane"),
        "the line speaks the gatewayhub's verb and keys: {armed}"
    );
    for datahub_word in ["Backfill", "Control-scope", "Observe verb"] {
        assert!(!armed.contains(datahub_word), "{datahub_word} is the datahub's: {armed}");
    }
    let absent = oanda_history_lane_line(Some(dir), Err(HistoryTokenError::NotConfigured));
    assert!(absent.contains(&key), "{absent}");
    assert!(
        absent.contains(&format!("`vike-cli secrets set --account <ID> {TOKEN_FIELD}`")),
        "the fix writes the field onto the account: {absent}"
    );
    assert!(
        absent.contains("`vike-cli secrets account add --venue oanda --tier demo --no-label`"),
        "…and names how the account is created: {absent}"
    );
    assert!(absent.contains("nothing to restart"), "{absent}");
    let unknown = oanda_history_lane_line(Some(dir), Err(HistoryTokenError::StoreUnreadable));
    assert!(unknown.contains("UNKNOWN") && unknown.contains("not the same"), "{unknown}");
    let no_dir = oanda_history_lane_line(None, Err(HistoryTokenError::NotConfigured));
    assert!(no_dir.contains("no settings directory"), "{no_dir}");
}

// ---- The memo, through the real server ------------------------------------------------------------

/// The provider type behind [`HistoryTokenProvider`]'s `Arc`, for a `Weak` to it.
type ProviderFn = dyn Fn() -> Result<String, HistoryTokenError> + Send + Sync;
type Seen = Arc<Mutex<Vec<String>>>;
type Providers = Arc<Mutex<Vec<Weak<ProviderFn>>>>;

/// A source that asks its provider on every fetch, the way `vike_oanda::OandaKlines` does, and
/// records what it was handed — so the test can see the VALUE flow and see it change — plus a `Weak`
/// to the provider it fetched through, so the test can see that provider (and the memo holding the
/// value inside it) GONE once the connection closes.
struct TokenEcho {
    token: HistoryTokenProvider,
    seen: Seen,
    providers: Providers,
}

impl KlineSource for TokenEcho {
    fn venue(&self) -> &str {
        "oanda"
    }

    fn fetch(
        &self,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        _end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        self.providers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::downgrade(&self.token));
        let token = (self.token)().map_err(|e| SourceError::Refused(e.to_string()))?;
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).push(token);
        Ok(vec![Bar {
            ts: start_ms,
            open: 1.25,
            high: 1.5,
            low: 1.0,
            close: 1.375,
            volume: 7.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }])
    }
}

/// A lane over `settings` whose source echoes the token it was handed.
fn echo_lane(settings: &std::path::Path, seen: &Seen, providers: &Providers) -> VenueRest {
    let (seen, providers) = (Arc::clone(seen), Arc::clone(providers));
    VenueRest::new(false).with_oanda(OandaLane::with_source(
        at(settings),
        Arc::new(move |token: HistoryTokenProvider| -> Box<dyn KlineSource> {
            Box::new(TokenEcho {
                token,
                seen: Arc::clone(&seen),
                providers: Arc::clone(&providers),
            })
        }),
    ))
}

fn seen_values(seen: &Seen) -> Vec<String> {
    seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Whether every provider any connection built has been dropped.
fn all_providers_gone(providers: &Providers) -> bool {
    providers.lock().unwrap_or_else(PoisonError::into_inner).iter().all(|p| p.upgrade().is_none())
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// **The store is read ONCE per connection however many days it fetches, a SECOND connection reads
/// again, and nothing holds the token past its connection** — proved by what a real store does to
/// the lane. A source per request holds one connection, so a chunked request is one connection: its
/// first fetch reads the store and its later fetches reuse the answer even after the key was rotated
/// under it; the next request (a new connection) is handed the rotated key; a key REMOVED makes the
/// request after that refuse with the teaching text; and when a connection closes its memo (token and
/// all) is gone. No line logged on the way carries any value.
#[test]
fn the_store_is_read_once_per_connection_and_a_new_connection_rereads() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &full_store_rows());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let providers: Providers = Arc::new(Mutex::new(Vec::new()));
    let server = RestServer::start(echo_lane(settings.path(), &seen, &providers));

    let rotated = "fake-rotated-token-1b6k";
    let (outcome, lines) = under_capture(|| {
        // One request: three day chunks on one connection.
        let first = RemoteKlines::credentialed(server.link(), "oanda");
        for day in 0..3 {
            first.fetch("EUR_USD", "5s", day * 86_400_000, day * 86_400_000 + 1).expect("a day");
        }
        assert_eq!(seen_values(&seen), [PRACTICE_VALUE; 3], "three days, ONE read of the store");

        // The key is rotated while that request is still running: it keeps what it read.
        write_store(settings.path(), &store_rows(Some(rotated)));
        first.fetch("EUR_USD", "5s", 3 * 86_400_000, 3 * 86_400_000 + 1).expect("a fourth day");
        assert_eq!(seen_values(&seen).last().map(String::as_str), Some(PRACTICE_VALUE));
        assert!(!all_providers_gone(&providers), "the request's provider was dropped while it ran");

        // The NEXT request is a new connection: it reads the store again and is handed the new key.
        let second = RemoteKlines::credentialed(server.link(), "oanda");
        second.fetch("EUR_USD", "5s", 0, 1).expect("the second request");
        assert_eq!(seen_values(&seen).last().map(String::as_str), Some(rotated));

        // The first connection closing takes ITS token with it; the second is still held.
        drop(first);
        drop(second);
        wait_until("a closed connection's token outlived it", || all_providers_gone(&providers));

        // A key REMOVED between requests disarms the lane for the next one, with no restart.
        write_store(settings.path(), &neighbours());
        let third = RemoteKlines::credentialed(server.link(), "oanda");
        third.fetch("EUR_USD", "5s", 0, 1)
    });
    let refused = outcome.expect_err("a removed key disarms the lane for the next request");
    assert_eq!(refused, SourceError::Refused(HistoryTokenError::NotConfigured.to_string()));
    let mut values = every_value();
    values.push(rotated.to_string());
    for value in values {
        assert!(!lines.iter().any(|l| l.contains(&value)), "a value reached the log: {lines:?}");
    }
}

/// **An unreadable store is NOT an absent key**, end to end: the fetch is refused with the store's
/// own words and the presence word is `Unreadable`; an absent key is refused with the teaching text
/// and the word is `Absent`.
#[test]
fn an_unreadable_store_is_not_an_absent_key_over_the_wire() {
    let unreadable = tempfile::tempdir().expect("a temp dir");
    plant_unreadable_store(unreadable.path());
    let absent = tempfile::tempdir().expect("a temp dir");
    write_store(absent.path(), &neighbours());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let providers: Providers = Arc::new(Mutex::new(Vec::new()));

    let broken = RestServer::start(echo_lane(unreadable.path(), &seen, &providers));
    let err = RemoteKlines::credentialed(broken.link(), "oanda")
        .fetch("EUR_USD", "5s", 0, 1)
        .expect_err("the store cannot be read");
    assert_eq!(err, SourceError::Refused(HistoryTokenError::StoreUnreadable.to_string()));
    assert_eq!(credential_presence(&broken.link(), "oanda"), CredentialPresence::Unreadable);

    let missing = RestServer::start(echo_lane(absent.path(), &seen, &providers));
    let err = RemoteKlines::credentialed(missing.link(), "oanda")
        .fetch("EUR_USD", "5s", 0, 1)
        .expect_err("the key is not stored");
    assert_eq!(err, SourceError::Refused(HistoryTokenError::NotConfigured.to_string()));
    assert_eq!(credential_presence(&missing.link(), "oanda"), CredentialPresence::Absent);
    assert!(seen_values(&seen).is_empty(), "no value flowed from a store that held none");
}

/// A key stored AFTER the daemon started is what the next connection sees, and the presence word
/// follows it — both uncached, so there is nothing to restart.
#[test]
fn a_key_stored_after_start_arms_the_lane_with_no_restart() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &neighbours());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let providers: Providers = Arc::new(Mutex::new(Vec::new()));
    let server = RestServer::start(echo_lane(settings.path(), &seen, &providers));
    assert_eq!(credential_presence(&server.link(), "oanda"), CredentialPresence::Absent);
    RemoteKlines::credentialed(server.link(), "oanda")
        .fetch("EUR_USD", "5s", 0, 1)
        .expect_err("no key yet");

    write_store(settings.path(), &full_store_rows());
    assert_eq!(credential_presence(&server.link(), "oanda"), CredentialPresence::Present);
    RemoteKlines::credentialed(server.link(), "oanda")
        .fetch("EUR_USD", "5s", 0, 1)
        .expect("the key is stored now");
    assert_eq!(seen_values(&seen), [PRACTICE_VALUE]);
}

/// A credentialed klines request as a raw peer sends it.
fn credentialed_request(rid: u64) -> Request {
    Request::FetchCredentialed {
        rid,
        what: FetchWhat::Klines {
            venue: "oanda".into(),
            symbol: "EUR_USD".into(),
            interval: "5s".into(),
            start_ms: 0,
            end_ms: 1,
        },
    }
}

/// **The DAEMON refuses a Read-scope peer the credentialed verb, and the lane is untouched.** The
/// credentialed verb is Write scope (0097's verdict 3, carried over the hop). A client built on a
/// Read-only link refuses LOCALLY and never dials (`GatewayClient::connect_observed`), so through the
/// client crate the server is never reached and "the lane was not touched" would be true whatever the
/// server did. This test therefore uses a RAW peer that CAN sign - it holds the Read key and
/// authenticates as Read - and sends the verb anyway: the daemon must answer a scope refusal naming
/// both scopes, keep the connection, and ask the lane for nothing (no provider built, the store not
/// read, the echo source never run). A Write peer sending the same bytes is the control: it reaches
/// the very same lane and reads the token, so the emptiness above is not a lane that cannot be reached.
#[test]
fn a_read_scope_peer_that_can_sign_is_refused_by_the_daemon_before_the_credentialed_lane() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &full_store_rows());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let providers: Providers = Arc::new(Mutex::new(Vec::new()));
    let server = RestServer::start(echo_lane(settings.path(), &seen, &providers));

    let mut read = Raw::open(server.addr, &test_keys(), Scope::Read);
    read.send(&credentialed_request(1));
    let Response::Error(text) = read.recv::<Response>() else {
        panic!("a Read peer's FetchCredentialed must be answered with the scope refusal")
    };
    assert!(
        text.contains("FetchCredentialed") && text.contains("control") && text.contains("observe"),
        "the refusal names the verb and both scopes: {text}"
    );
    read.send(&Request::Ping);
    assert!(
        matches!(read.recv::<Response>(), Response::Pong),
        "an over-scoped verb is a bad request, not a bad connection"
    );
    assert!(seen_values(&seen).is_empty(), "the lane ran for a Read peer");
    assert!(
        providers.lock().unwrap_or_else(PoisonError::into_inner).is_empty(),
        "a provider (and so the store read) was built for a Read peer"
    );

    // The control: the same bytes under the Write key reach the lane and spend the token.
    let mut write = Raw::open(server.addr, &test_keys(), Scope::Write);
    write.send(&credentialed_request(1));
    loop {
        match write.recv::<Response>() {
            Response::FetchProgress(_) | Response::FetchPage { .. } => {}
            Response::FetchDone { .. } => break,
            other => panic!("the Write peer's fetch did not run: {other:?}"),
        }
    }
    assert_eq!(seen_values(&seen), [PRACTICE_VALUE], "the Write peer is served by the same lane");

    // And the client crate refuses a Read-only link locally, before it dials at all.
    let before = seen_values(&seen).len();
    RemoteKlines::credentialed(server.read_link(), "oanda")
        .fetch("EUR_USD", "5s", 0, 1)
        .expect_err("a Read key may not spend the token");
    assert_eq!(seen_values(&seen).len(), before);
}

/// A daemon built without a credential store advertises no credentialed lane and says
/// `NotChecked` — nothing was read — never `Absent`.
#[test]
fn a_lane_with_no_way_to_look_says_not_checked_and_serves_no_credentialed_fetch() {
    let server = RestServer::start(VenueRest::new(false));
    assert_eq!(credential_presence(&server.link(), "oanda"), CredentialPresence::NotChecked);
    assert_eq!(credential_presence(&server.link(), "binance"), CredentialPresence::NotNeeded);
    RemoteKlines::credentialed(server.link(), "oanda")
        .fetch("EUR_USD", "5s", 0, 1)
        .expect_err("no credentialed lane is armed");
}

/// Nothing a lane or a scope prints holds the token: not the lane's `Debug`, not the scope's, not
/// the memo's — checked after a fetch has filled the slot.
#[test]
fn nothing_the_lane_or_the_scope_prints_holds_the_token() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &full_store_rows());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let providers: Providers = Arc::new(Mutex::new(Vec::new()));
    let lane = OandaLane::with_source(
        at(settings.path()),
        Arc::new({
            let (seen, providers) = (Arc::clone(&seen), Arc::clone(&providers));
            move |token: HistoryTokenProvider| -> Box<dyn KlineSource> {
                Box::new(TokenEcho {
                    token,
                    seen: Arc::clone(&seen),
                    providers: Arc::clone(&providers),
                })
            }
        }),
    );
    let scope = ConnectionScope::new();
    lane.fetch(&scope, "EUR_USD", "5s", 0, 1, &vike_gatewayhub::rest::FetchCtx::new())
        .expect("a fetch");
    assert_eq!(seen_values(&seen), [PRACTICE_VALUE]);
    for printed in [format!("{lane:?}"), format!("{scope:?}")] {
        for value in every_value() {
            assert!(!printed.contains(&value), "a value reached a Debug: {printed}");
        }
    }
}
