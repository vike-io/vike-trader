//! **The NO-VENUE-KEYS scope keeps every venue credential VALUE out of the process** — decision
//! 0146's second reopener, taken by the two hubs that hold no venue connection (decision 0164).
//!
//! `crates/vike-secrets/src/db/read.rs`'s `withheld_sql` is the rule, applied inside the database's
//! own `WHERE`: a `credential` row filed against an ACCOUNT or a VENUE is never selected, and the
//! DEPLOYMENT rows (the pager's, a data provider's key) and the hub keys stay readable. Unlike
//! `NoLiveKeys` it asks nothing of the account's tier, so a store holding a LIVE key set and a demo
//! one withholds both. What it must NOT withhold is just as much the contract: the `account` rows,
//! the key NAMES an account owns, and every PRESENCE boolean (the tradehub's `HeldCredentials`
//! answers from them), because none of those is a value.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_whole_map_holds_no_account_or_venue_value`] | a hub holding a venue key in its address space |
//! | [`a_deployment_row_under_a_venue_name_is_withheld`] | a flat `BYBIT_API_KEY` or an IBKR gateway login in the tradehub's whole-map read |
//! | [`a_declared_name_of_an_account_row_reads_as_absent`] | the declared-name read around the scope |
//! | [`the_book_and_the_one_field_read_answer_nothing`] | a directory whose `credentials_for` still arms a venue |
//! | [`presence_names_and_rows_stay_readable`] | the account plane losing what it lists, or `HeldCredentials` lying |
//! | [`no_environment_word_selects_the_scope`] | a ceiling raisable from the environment (decision 0111) |
//! | [`a_key_replaced_in_place_moves_nothing_this_scope_can_read`] | a tradehub taught to compare row ids believing it sees a rotation |

use std::collections::BTreeSet;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{CredentialKeyScope, CredentialLookup, CredentialScope, CredentialSource};

use crate::support::{self, Fixture};

/// The planted store: a LIVE and a DEMO account with keys, a venue application row, an aster demo
/// account, and deployment rows (a data provider's key, the pager's, and two venue-named names).
/// `held` is every name an account or a venue owns, `deployment_kept` the deployment names the
/// scope must answer.
struct Planted {
    fx: Fixture,
    held: BTreeSet<String>,
    /// A name that ONLY the venue-owned half withholds (a demo binance account's key): the aster and
    /// polymarket families are withheld by `NoLiveKeys`' own predicate too, so they cannot show it.
    probe: String,
    deployment_kept: BTreeSet<String>,
    deployment_withheld: BTreeSet<String>,
    live: i64,
    demo: i64,
}

fn planted() -> Planted {
    let fx = Fixture::initialised();
    let mut held = BTreeSet::new();
    let demo = fx.create_account("binance", AccountTier::Demo, None);
    held.extend(fx.attach(demo, &["API_KEY", "API_SECRET"]));
    let probe = vike_secrets::account_credential_name("binance", demo, "API_KEY");
    assert!(held.contains(&probe), "the probe is a name the demo account owns");
    let live = fx.create_account("bybit", AccountTier::Live, None);
    held.extend(fx.attach(live, &["API_KEY", "API_SECRET"]));
    let aster = fx.create_account("aster", AccountTier::Demo, None);
    held.extend(fx.attach(aster, &["API_KEY"]));
    held.extend(fx.attach_venue("ctrader", &["CLIENT_ID"]));

    // DEPLOYMENT rows: no account, no venue. The provider keys are the datahub's own.
    let deployment_kept: BTreeSet<String> = vike_venues::credential_keys::DATA_PROVIDER_KEYS
        .iter()
        .chain(&["VIKE_ALERT_TELEGRAM_TOKEN"])
        .map(|n| (*n).to_string())
        .collect();
    // A venue-named deployment row is the venue's, filed by name: withheld. The aster one as
    // `NoLiveKeys` withholds it, so the narrower scope is a subset of this one; the attribution code
    // by its venue's name family, read by the mount in the gatewayhub and by no hub.
    let deployment_withheld: BTreeSet<String> =
        ["ASTER_DEMO_API_KEY", "OKX_BROKER_CODE"].map(String::from).into();
    fx.write(
        support::fake_rows(deployment_kept.iter().chain(&deployment_withheld)),
        support::is_hub_key,
    );
    Planted { fx, held, probe, deployment_kept, deployment_withheld, live, demo }
}

fn no_venue_keys(p: &Planted) -> vike_secrets::CredentialStore {
    support::scoped_store_in(p.fx.dir(), CredentialScope::NoVenueKeys)
}

/// **The whole-map read holds the deployment rows and not one account or venue row** — live,
/// demo, an aster demo and a venue application row alike — and counts what it withheld.
#[test]
fn the_whole_map_holds_no_account_or_venue_value() {
    let p = planted();

    // The CONTROL: the unscoped read holds everything planted.
    let all = support::store_in(p.fx.dir()).read_all().expect("unscoped read");
    let every: BTreeSet<String> =
        p.held.iter().chain(&p.deployment_kept).chain(&p.deployment_withheld).cloned().collect();
    assert_eq!(support::key_names(&all.secrets), every, "the control lost or invented names");

    let narrow = no_venue_keys(&p).read_all().expect("no-venue-keys read");
    assert_eq!(narrow.scope, CredentialScope::NoVenueKeys);
    assert!(matches!(narrow.source, CredentialSource::Database(_)), "the database must answer");
    assert_eq!(
        support::key_names(&narrow.secrets),
        p.deployment_kept,
        "a venue credential reached the process, or a deployment row was withheld"
    );
    assert_eq!(
        narrow.withheld,
        p.held.len() + p.deployment_withheld.len(),
        "the withheld count is wrong"
    );
    let map = narrow.secrets.into_map();
    for n in &p.deployment_kept {
        assert_eq!(map.get(n), Some(&support::fake_value(n)), "`{n}` came back with a wrong value");
    }
}

/// **A DEPLOYMENT row under a roster venue's name is withheld, whatever the columns say**: the
/// tradehub's boot reads the whole map, so the columns alone would hand it a legacy flat
/// `BYBIT_API_KEY`, or an IBKR gateway login `crates/vike-cli/src/cmd/secrets/set.rs`'s `name_owner`
/// files by name on a fresh box. Every deployment name measured on the live box on 2026-11-07 stays
/// readable, the near misses too, and the PRESENCE read answers as the unscoped reader does: it
/// returns a boolean, and the tradehub's `HeldCredentials` marks an attribution code from it.
#[test]
fn a_deployment_row_under_a_venue_name_is_withheld() {
    let mut venue_named: BTreeSet<String> = vike_model::VENUES
        .iter()
        .flat_map(|venue| {
            let upper = venue.to_uppercase();
            [
                format!("{upper}_API_KEY"),
                // Case can only WIDEN what is withheld.
                format!("{venue}_api_secret"),
                format!("{venue}.API_KEY"),
                format!("{venue}#7.API_KEY"),
            ]
        })
        .collect();
    venue_named.extend(
        [
            "IBKR_DEMO_USERNAME",
            "IBKR_DEMO_PASSWORD",
            "IBKR_LIVE_USERNAME",
            "IBKR_LIVE_PASSWORD",
            "OKX_BROKER_CODE",
            "HYPERLIQUID_BUILDER_FEE_TENTHS_BP",
            "ASTER_BUILDER_FEE_RATE",
        ]
        .map(String::from),
    );
    let kept: BTreeSet<String> = [
        "DATAFORSEO_LOGIN",
        "DATAFORSEO_PASSWORD",
        "FINNHUB_API_KEY",
        "FMP_API_KEY",
        "PMDATA_API_KEY",
        "POLYDATA_API_KEY",
        "VIKE_ALERT_TELEGRAM_CHAT_ID",
        "VIKE_ALERT_TELEGRAM_TOKEN",
        "VIKE_API_KEY",
        "VIKE_ARCHIVE_API_KEY",
        "VIKE_STRATEGY_BUILDER_KEY",
        "VIKE_TELEGRAM_ALLOWED_CHAT_IDS",
        "VIKE_TELEGRAM_BOT_TOKEN",
        // Near misses: a family is the venue AND its separator, from the name's first letter.
        "IGNITE_API_KEY",
        "BINANCEDATA_API_KEY",
        "OKXLIKE_API_KEY",
        "POLYMARKETS_NOTE",
        "POLYGON_RPC_URL",
        "MY_BYBIT_API_KEY",
    ]
    .map(String::from)
    .into();
    let every: BTreeSet<String> = kept.union(&venue_named).cloned().collect();
    let fx = Fixture::seeded_with(support::fake_rows(&every), support::is_hub_key);
    let full = support::store_in(fx.dir());
    let narrow = support::scoped_store_in(fx.dir(), CredentialScope::NoVenueKeys);

    // The CONTROL: every planted row is a DEPLOYMENT row, so the columns withhold none of them.
    assert_eq!(full.deployment_names().expect("opens"), every, "a planted row has an owner");

    let map = narrow.read_all().expect("no-venue-keys read");
    assert_eq!(support::key_names(&map.secrets), kept, "the whole map holds a venue-named row");
    assert_eq!(map.withheld, venue_named.len(), "the withheld count is wrong");
    let values = map.secrets.into_map();
    for n in &kept {
        assert_eq!(values.get(n), Some(&support::fake_value(n)), "`{n}` lost its value");
    }

    let declared = narrow.read(&CredentialKeyScope::of(&every)).expect("opens");
    assert_eq!(declared.withheld, venue_named.len(), "the declared read's count is wrong");
    for n in &venue_named {
        assert!(
            matches!(declared.get(n), CredentialLookup::AbsentFromStore),
            "`{n}` was read by name"
        );
    }
    for n in &kept {
        assert!(
            matches!(declared.get(n), CredentialLookup::Present(_)),
            "`{n}` must stay readable"
        );
    }
    assert_eq!(narrow.deployment_names().expect("opens"), kept, "the name list holds a venue row");

    let scope = CredentialKeyScope::of(&venue_named);
    let present = narrow.present_names(&scope).expect("opens");
    assert_eq!(present, venue_named, "presence is a boolean, so it withholds nothing");
    assert_eq!(present, full.present_names(&scope).expect("opens"), "…as it answers unscoped");
}

/// **The archive download's AWS key pair answers a DECLARED read, and only that** (decision 0100's 2026-11-13
/// amendment): deployment rows under the approved `DUKASCOPY_ARCHIVE_AWS_…` names, which the datahub reads one name
/// at a time. The whole map and the name list (the tradehub's boot) still withhold them, and a near miss in the same
/// family (another name, the same name in lower case) stays withheld in every read.
#[test]
fn the_archive_key_pair_answers_only_a_declared_read() {
    let pair: BTreeSet<String> =
        vike_venues::credential_keys::ARCHIVE_DOWNLOAD_KEYS.map(String::from).into();
    let near: BTreeSet<String> = [
        "DUKASCOPY_ARCHIVE_AWS_SESSION_TOKEN".to_string(),
        vike_venues::credential_keys::ARCHIVE_DOWNLOAD_KEYS[0].to_ascii_lowercase(),
    ]
    .into();
    let every: BTreeSet<String> = pair.union(&near).cloned().collect();
    let fx = Fixture::seeded_with(support::fake_rows(&every), support::is_hub_key);
    let narrow = support::scoped_store_in(fx.dir(), CredentialScope::NoVenueKeys);
    assert_eq!(
        support::store_in(fx.dir()).deployment_names().expect("opens"),
        every,
        "the control: all four are deployment rows"
    );

    let declared = narrow.read(&CredentialKeyScope::of(&every)).expect("opens");
    for n in &pair {
        let want = support::fake_value(n);
        assert!(
            matches!(declared.get(n), CredentialLookup::Present(v) if v == want),
            "`{n}` must reach the datahub's declared read"
        );
    }
    for n in &near {
        assert!(
            matches!(declared.get(n), CredentialLookup::AbsentFromStore),
            "`{n}` is the venue's family"
        );
    }
    assert_eq!(declared.withheld, near.len(), "only the near misses are withheld");

    let map = narrow.read_all().expect("no-venue-keys read");
    assert!(support::key_names(&map.secrets).is_empty(), "the whole map holds the pair");
    assert_eq!(map.withheld, every.len());
    assert!(narrow.deployment_names().expect("opens").is_empty(), "the name list holds the pair");
}

/// **The declared-name read goes around nothing**: an account row's name reads as ABSENT (the live
/// gate's answer), a deployment name is present, and the withheld count says how many declared
/// names the scope took.
#[test]
fn a_declared_name_of_an_account_row_reads_as_absent() {
    let p = planted();
    let held = p.probe.clone();
    let provider = vike_venues::credential_keys::FINNHUB_API_KEY;
    let scope = CredentialKeyScope::of([held.as_str(), provider]);
    let read = no_venue_keys(&p).read(&scope).expect("opens");
    assert!(
        matches!(read.get(&held), CredentialLookup::AbsentFromStore),
        "an account value was read"
    );
    assert!(
        matches!(read.get(provider), CredentialLookup::Present(_)),
        "the provider key is a deployment"
    );
    assert_eq!(read.withheld, 1);
    // The control.
    let full = support::store_in(p.fx.dir()).read(&scope).expect("opens");
    assert!(
        matches!(full.get(&held), CredentialLookup::Present(_)),
        "the control holds the account value"
    );
}

/// **The credential book and the one-field read answer nothing**, so an `AccountDirectory` built
/// the way a composition root builds one hands out no credential for ANY account: the live one, the
/// demo one, the aster one — and the data-feed credentials are empty too.
#[test]
fn the_book_and_the_one_field_read_answer_nothing() {
    let p = planted();
    let narrow = no_venue_keys(&p);
    let book = narrow.credential_registry().expect("opens");
    for id in [p.live, p.demo, p.fx.id_of("aster", "demo")] {
        assert!(book.account(id).is_none(), "account {id}'s fields reached the book");
    }
    assert!(book.venue("ctrader").is_none(), "a venue application row reached the book");
    // The control: the same store, unscoped, holds them.
    let full = support::store_in(p.fx.dir()).credential_registry().expect("opens");
    assert!(full.account(p.live).is_some() && full.venue("ctrader").is_some(), "the control");

    let one = |tier, venue| {
        narrow.account_field(venue, &AccountLabel::Default, tier, "API_KEY").expect("opens")
    };
    assert!(one(AccountTier::Live, "bybit").is_none(), "a live field was read");
    assert!(one(AccountTier::Demo, "binance").is_none(), "a demo field was read");

    let dir =
        AccountDirectory::read(narrow.accounts(), narrow.account_keys(), Ok::<_, String>(book));
    let default = AccountLabel::Default;
    assert!(!dir.credentials_for("bybit", &default, AccountTier::Live).holds_any());
    assert!(!dir.credentials_for("binance", &default, AccountTier::Demo).holds_any());
    assert!(!dir.data_feed_credentials("binance").holds_any());
    assert!(
        dir.account_row("bybit", &default, AccountTier::Live).is_some(),
        "the ROW is still there: the scope withholds values, not accounts"
    );
}

/// **A key replaced in place moves nothing a hub with no venue keys can read**: the write funnel UPDATEs the live
/// row (`crates/vike-secrets/src/db/upsert.rs`'s `upsert_rows`), so the id, the name and the directory all stay. Only
/// the process that holds the values can tell (`crates/vike-tradehub/src/account_follow.rs`'s `follow_key_changes`).
#[test]
fn a_key_replaced_in_place_moves_nothing_this_scope_can_read() {
    let p = planted();
    let narrow = no_venue_keys(&p);
    let directory = || {
        AccountDirectory::read(
            narrow.accounts(),
            narrow.account_keys(),
            narrow.credential_registry(),
        )
    };
    let row_id = || -> i64 {
        p.fx.conn()
            .query_row("SELECT id FROM credential WHERE name = ?1", [&p.probe], |r| r.get(0))
            .expect("the row is there")
    };
    let (rows_before, id_before) = (directory(), row_id());
    let read_full = || {
        support::store_in(p.fx.dir()).credential_registry().expect("opens").account(p.demo).cloned()
    };
    let full_before = read_full();

    p.fx.write([(p.probe.as_str(), "the-corrected-key")], support::is_hub_key);

    assert_ne!(read_full(), full_before, "the control: the value did change");
    assert_eq!(row_id(), id_before, "the row was replaced in place, not retired and added");
    assert_eq!(directory(), rows_before, "nothing this scope reads moved");
}

/// **What is not a value stays readable.** The `account` rows, the key NAMES each account owns, the
/// presence booleans (by name and by account field) and the hub keys answer exactly as they do
/// unscoped — the tradehub's account plane lists them and `HeldCredentials` answers from them.
#[test]
fn presence_names_and_rows_stay_readable() {
    let p = planted();
    let narrow = no_venue_keys(&p);
    let full = support::store_in(p.fx.dir());

    assert_eq!(narrow.accounts().expect("opens"), full.accounts().expect("opens"), "rows");
    assert_eq!(narrow.account_keys().expect("opens"), full.account_keys().expect("opens"), "names");

    let held = p.probe.clone();
    let scope = CredentialKeyScope::of([held.as_str(), "NOT_STORED_ANYWHERE"]);
    let present = narrow.present_names(&scope).expect("opens");
    assert_eq!(present, BTreeSet::from([held]), "presence answers from the scoped statement");
    assert_eq!(present, full.present_names(&scope).expect("opens"), "…as it answers unscoped");
    assert!(
        narrow
            .account_field_present("bybit", &AccountLabel::Default, AccountTier::Live, "API_KEY")
            .expect("opens"),
        "a live account's field is PRESENT: a boolean computed in SQLite, no value returned"
    );

    let names: &[&str] = &vike_venues::credential_keys::PLATFORM_KEYS;
    let hubs = Fixture::seeded_with(support::fake_rows(names.iter()), support::hub_keys_in(names));
    let scoped = support::scoped_store_in(hubs.dir(), CredentialScope::NoVenueKeys)
        .hub_keys(|_| true)
        .expect("opens");
    let control = support::store_in(hubs.dir()).hub_keys(|_| true).expect("opens");
    assert!(!control.secrets.is_empty(), "the control holds the hub keys");
    assert_eq!(scoped.secrets, control.secrets, "a hub key is no account's and no venue's");
    assert_eq!(scoped.withheld, 0);
}

/// **No environment word selects this scope** (decision 0111: a ceiling you can raise from the
/// environment is not a ceiling, and the same goes for lowering one into a new spelling). The
/// parse of `VIKE_NO_LIVE_KEYS` still answers `Full` or `NoLiveKeys` for every word, and a word
/// naming this scope is refused like any other.
#[test]
fn no_environment_word_selects_the_scope() {
    for word in ["true", "TRUE", "false", "False"] {
        let parsed = CredentialScope::parse(Some(word)).expect("a legal word");
        assert_ne!(parsed, CredentialScope::NoVenueKeys, "{word}");
    }
    for word in ["no-venue-keys", "NoVenueKeys", "no_venue_keys", "venue", "2"] {
        assert!(CredentialScope::parse(Some(word)).is_err(), "{word} must be refused");
    }
}
