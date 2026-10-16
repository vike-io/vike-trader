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
//! | [`a_declared_name_of_an_account_row_reads_as_absent`] | the declared-name read around the scope |
//! | [`the_book_and_the_one_field_read_answer_nothing`] | a directory whose `credentials_for` still arms a venue |
//! | [`presence_names_and_rows_stay_readable`] | the account plane losing what it lists, or `HeldCredentials` lying |
//! | [`no_environment_word_selects_the_scope`] | a ceiling raisable from the environment (decision 0111) |

use std::collections::BTreeSet;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{CredentialScope, KeyScope, Lookup, Source};

use crate::support::{self, Fixture};

/// The planted store: a LIVE and a DEMO account with keys, a venue application row, an aster demo
/// account, and deployment rows (a data provider's key, the pager's, an attribution code and one
/// legacy aster-named name). `held` is every name an account or a venue owns, `deployment_kept`
/// the deployment names the scope must answer.
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
        .chain(&["VIKE_ALERT_TELEGRAM_TOKEN", "OKX_BROKER_CODE"])
        .map(|n| (*n).to_string())
        .collect();
    // A legacy aster-named deployment row is a venue secret misfiled: withheld, as `NoLiveKeys`
    // withholds it, so the narrower scope is a subset of this one.
    let deployment_withheld: BTreeSet<String> = ["ASTER_DEMO_API_KEY"].map(String::from).into();
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
    assert!(matches!(narrow.source, Source::Database(_)), "the database must answer");
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

/// **The declared-name read goes around nothing**: an account row's name reads as ABSENT (the live
/// gate's answer), a deployment name is present, and the withheld count says how many declared
/// names the scope took.
#[test]
fn a_declared_name_of_an_account_row_reads_as_absent() {
    let p = planted();
    let held = p.probe.clone();
    let provider = vike_venues::credential_keys::FINNHUB_API_KEY;
    let scope = KeyScope::of([held.as_str(), provider]);
    let read = no_venue_keys(&p).read(&scope).expect("opens");
    assert!(matches!(read.get(&held), Lookup::AbsentFromStore), "an account value was read");
    assert!(matches!(read.get(provider), Lookup::Present(_)), "the provider key is a deployment");
    assert_eq!(read.withheld, 1);
    // The control.
    let full = support::store_in(p.fx.dir()).read(&scope).expect("opens");
    assert!(matches!(full.get(&held), Lookup::Present(_)), "the control holds the account value");
}

/// **The credential book and the one-field read answer nothing**, so an `AccountDirectory` built
/// the way a composition root builds one hands out no credential for ANY account: the live one, the
/// demo one, the aster one — and the data-feed credentials are empty too.
#[test]
fn the_book_and_the_one_field_read_answer_nothing() {
    let p = planted();
    let narrow = no_venue_keys(&p);
    let book = narrow.credential_book().expect("opens");
    for id in [p.live, p.demo, p.fx.id_of("aster", "demo")] {
        assert!(book.account(id).is_none(), "account {id}'s fields reached the book");
    }
    assert!(book.venue("ctrader").is_none(), "a venue application row reached the book");
    // The control: the same store, unscoped, holds them.
    let full = support::store_in(p.fx.dir()).credential_book().expect("opens");
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
    let scope = KeyScope::of([held.as_str(), "NOT_STORED_ANYWHERE"]);
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
