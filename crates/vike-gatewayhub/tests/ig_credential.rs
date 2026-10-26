//! The IG history lane (decision 0194), through the real `VenueRest`, the real server and the client's
//! `RemoteKlines::credentialed`: the demo account's three fields are read ONCE per connection and
//! dropped with it, a field removed between requests disarms the next one with no restart, the presence
//! word needs all three, and the allowance LEDGER is one for the whole process however many connections
//! ask. The reads themselves (`pub(crate)`, they return the password) and the ledger's rules are tested
//! in the crate: `crates/vike-gatewayhub/src/rest/ig_tests.rs`.
//!
//! The real `IgKlines` is never built here over a present credential: CI would log in to IG's demo
//! host. The lane is built through `IgLane::with_source` over scripted sources, as the OANDA suite does.
//! Every VALUE planted is an obviously fake, distinctive string, so "no value reached a log line" is an
//! assertion that could actually fail.

#![cfg(feature = "data")]

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/rest_server.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod rest_server;

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use vike_data::source::{KlineSource, SourceError};
use vike_gatewayhub::rest::ig::{AllowanceLedger, IgHistory, IgLane};
use vike_gatewayhub::rest::lane::VenueRest;
use vike_gatewayhub_client::credential::{CredentialPresence, credential_presence};
use vike_gatewayhub_client::history::RemoteKlines;
use vike_ig::{Allowance, AllowanceGate, ConfigProvider, IgConfig};
use vike_marketdata::Bar;
use vike_model::AccountTier;

use rest_server::RestServer;

/// Every event's level, then every field — `message` first — as ` name=value`, one line per event.
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

const EPIC: &str = "CS.D.EURUSD.MINI.IP";
const API_KEY_VALUE: &str = "fake-ig-api-key-5d2f";
const IDENTIFIER_VALUE: &str = "fake-ig-identifier-8h1q";
const PASSWORD_VALUE: &str = "fake-ig-password-3v9x";
const ROTATED_VALUE: &str = "fake-ig-rotated-key-6z1j";

/// One planted account: its venue, tier and label, and the `(field, value)` pairs filed against it.
type Planted = (&'static str, AccountTier, Option<&'static str>, Vec<(String, String)>);

/// The demo account's trio with `api_key` as its key, leaving out the field named `without`; and the
/// LIVE sibling's trio beside it, which no read may answer in its place.
fn store_rows(api_key: &str, without: Option<&str>) -> Vec<Planted> {
    let pair = |field: &str, value: &str| (field.to_string(), value.to_string());
    let demo =
        [("API_KEY", api_key), ("IDENTIFIER", IDENTIFIER_VALUE), ("PASSWORD", PASSWORD_VALUE)]
            .into_iter()
            .filter(|(field, _)| Some(*field) != without)
            .map(|(field, value)| pair(field, value))
            .collect();
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
    ]
}

fn every_value() -> [&'static str; 6] {
    [
        API_KEY_VALUE,
        IDENTIFIER_VALUE,
        PASSWORD_VALUE,
        ROTATED_VALUE,
        "fake-ig-live-key-1m4z",
        "fake-ig-live-password-9t2w",
    ]
}

/// Make the settings DATABASE hold exactly `accounts`, 0600 on unix. A store being REPLACED (a rotation,
/// a removal) is rebuilt in this test's own temp directory.
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
        vike_secrets::save_owned_credentials_unjournalled(
            settings,
            &vike_secrets::CredentialOwner::Account(id),
            fields,
            vike_secrets::UpsertIntent::Add,
        )
        .expect("write the account's fields");
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

/// The provider type behind [`ConfigProvider`]'s `Arc`, for a `Weak` to it.
type ProviderFn = dyn Fn() -> Result<IgConfig, SourceError> + Send + Sync;
type Seen = Arc<Mutex<Vec<String>>>;
type Providers = Arc<Mutex<Vec<Weak<ProviderFn>>>>;

/// A source that asks its provider on every fetch, the way `IgKlines` does when it logs in, and records
/// the API key it was handed — so the test can see the VALUE flow and see it change — plus a `Weak` to
/// the provider it fetched through, so the test can see that provider (and the memo holding the three
/// fields inside it) GONE once the connection closes.
struct Echo {
    config: ConfigProvider,
    seen: Seen,
    providers: Providers,
}

impl IgHistory for Echo {
    fn fetch_until(
        &self,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        _end_ms: i64,
        _stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError> {
        self.providers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::downgrade(&self.config));
        let config = (self.config)()?;
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).push(config.api_key);
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

/// A lane over `settings` whose source echoes the key it was handed.
fn echo_lane(settings: &std::path::Path, seen: &Seen, providers: &Providers) -> VenueRest {
    let (seen, providers) = (Arc::clone(seen), Arc::clone(providers));
    VenueRest::new(false).with_ig(IgLane::with_source(
        at(settings),
        Arc::new(
            move |config: ConfigProvider, _gate: Arc<dyn AllowanceGate>| -> Box<dyn IgHistory> {
                Box::new(Echo {
                    config,
                    seen: Arc::clone(&seen),
                    providers: Arc::clone(&providers),
                })
            },
        ),
    ))
}

fn seen_values(seen: &Seen) -> Vec<String> {
    seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

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

/// **The three fields are read ONCE per connection however many chunks it fetches, a SECOND connection
/// reads again, and nothing holds them past their connection** — proved by what a real store does to the
/// lane. A request is one connection: its first fetch reads the store and its later fetches reuse the
/// answer even after the key was rotated under it; the next request is handed the rotated key; a field
/// REMOVED makes the request after that refuse, naming the field; and when a connection closes its memo
/// is gone. No line logged on the way carries any value.
#[test]
fn the_fields_are_read_once_per_connection_and_a_new_connection_rereads() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &store_rows(API_KEY_VALUE, None));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let providers: Providers = Arc::new(Mutex::new(Vec::new()));
    let server = RestServer::start(echo_lane(settings.path(), &seen, &providers));

    let (outcome, lines) = under_capture(|| {
        let first = RemoteKlines::credentialed(server.link(), "ig");
        for day in 0..3 {
            first.fetch(EPIC, "1h", day * 86_400_000, day * 86_400_000 + 1).expect("a chunk");
        }
        assert_eq!(seen_values(&seen), [API_KEY_VALUE; 3], "three chunks, ONE read of the store");

        write_store(settings.path(), &store_rows(ROTATED_VALUE, None));
        first.fetch(EPIC, "1h", 3 * 86_400_000, 3 * 86_400_000 + 1).expect("a fourth chunk");
        assert_eq!(seen_values(&seen).last().map(String::as_str), Some(API_KEY_VALUE));
        assert!(!all_providers_gone(&providers), "the request's provider was dropped while it ran");

        let second = RemoteKlines::credentialed(server.link(), "ig");
        second.fetch(EPIC, "1h", 0, 1).expect("the second request");
        assert_eq!(seen_values(&seen).last().map(String::as_str), Some(ROTATED_VALUE));

        drop(first);
        drop(second);
        wait_until("a closed connection's fields outlived it", || all_providers_gone(&providers));

        write_store(settings.path(), &store_rows(ROTATED_VALUE, Some("PASSWORD")));
        RemoteKlines::credentialed(server.link(), "ig").fetch(EPIC, "1h", 0, 1)
    });

    let SourceError::Refused(text) = outcome.expect_err("a removed field disarms the lane") else {
        panic!("a Refused")
    };
    assert!(text.contains("lacks: PASSWORD."), "{text}");
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(value)), "a value reached the log: {lines:?}");
    }
}

/// **The presence word follows the store with no restart, and needs ALL THREE fields**; a store that
/// exists and will not read is `Unreadable`, never `Absent`, and the fetch is refused in the store's own
/// words.
#[test]
fn the_presence_word_follows_the_store_and_an_unreadable_one_is_not_an_absent_field() {
    let settings = tempfile::tempdir().expect("a temp dir");
    write_store(settings.path(), &store_rows(API_KEY_VALUE, Some("IDENTIFIER")));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let providers: Providers = Arc::new(Mutex::new(Vec::new()));
    let server = RestServer::start(echo_lane(settings.path(), &seen, &providers));
    assert_eq!(credential_presence(&server.link(), "ig"), CredentialPresence::Absent);

    write_store(settings.path(), &store_rows(API_KEY_VALUE, None));
    assert_eq!(credential_presence(&server.link(), "ig"), CredentialPresence::Present);
    RemoteKlines::credentialed(server.link(), "ig").fetch(EPIC, "1h", 0, 1).expect("armed now");

    let broken = tempfile::tempdir().expect("a temp dir");
    plant_unreadable_store(broken.path());
    let broken_server = RestServer::start(echo_lane(broken.path(), &seen, &providers));
    assert_eq!(credential_presence(&broken_server.link(), "ig"), CredentialPresence::Unreadable);
    let SourceError::Refused(text) = RemoteKlines::credentialed(broken_server.link(), "ig")
        .fetch(EPIC, "1h", 0, 1)
        .expect_err("the store cannot be read")
    else {
        panic!("a Refused")
    };
    assert!(text.contains("not the same as the fields being absent"), "{text}");
}

/// A source that spends 200 points of the gate and tells the ledger what is left.
struct Spender {
    gate: Arc<dyn AllowanceGate>,
}

impl IgHistory for Spender {
    fn fetch_until(
        &self,
        _symbol: &str,
        _interval: &str,
        _start_ms: i64,
        _end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError> {
        self.gate.admit(200, "a chunk of 200 points", stop)?;
        self.gate.learn(Allowance { remaining: 2_100, total: 10_000, resets_in_s: 5 * 86_400 });
        Ok(Vec::new())
    }
}

/// **ONE ledger serves every connection through the real server**: the chunk of one request teaches
/// it, and the next REQUEST (a new connection, a new source) is stopped by what the first learnt, in
/// the sentence the operator reads, without the source being asked of its venue.
#[test]
fn the_allowance_ledger_is_one_for_every_connection() {
    let lane = IgLane::with_source(
        at(tempfile::tempdir().expect("a temp dir").path()),
        Arc::new(|_config: ConfigProvider, gate: Arc<dyn AllowanceGate>| -> Box<dyn IgHistory> {
            Box::new(Spender { gate })
        }),
    )
    .with_ledger(Arc::new(AllowanceLedger::with(Duration::ZERO, vike_model::now_ms)));
    let server = RestServer::start(VenueRest::new(false).with_ig(lane));

    RemoteKlines::credentialed(server.link(), "ig")
        .fetch(EPIC, "1d", 0, 1)
        .expect("the first request spends 200 points of a week it knew nothing of");
    let err = RemoteKlines::credentialed(server.link(), "ig")
        .fetch(EPIC, "1d", 0, 1)
        .expect_err("the second request sees what the first learnt");

    let SourceError::Fetch(text) = err else { panic!("a Fetch") };
    for part in ["2100 of this week's 10000 points are left", "2000 (20 percent)", "5 days 0 hours"]
    {
        assert!(text.contains(part), "{part:?} in {text}");
    }
}
