//! The credentialed streams' account read, over a REAL settings database planted in a temp directory.
//!
//! Every fixture plants ACCOUNT ROWS and writes their FIELDS (decision 0156 item 6), and every VALUE is
//! an obviously fake, distinctive string, so "the live account's key did not come along" is an
//! assertion that could actually fail.

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::*;

/// One planted account: venue, tier, label and the `(field, value)` pairs filed against it.
type Planted = (&'static str, AccountTier, Option<&'static str>, Vec<(String, String)>);

fn pairs(fields: &[(&str, &str)]) -> Vec<(String, String)> {
    fields.iter().map(|&(f, v)| (f.to_string(), v.to_string())).collect()
}

/// Make the settings database hold exactly `accounts` (and, for ctrader, the venue's application pair
/// is planted by the caller through `venue_fields`).
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
}

fn at(dir: &std::path::Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
}

const DEMO_KEY: &str = "fake-demo-api-key-1a2b";
const LIVE_KEY: &str = "fake-live-api-key-9z8y";
const OTHER_VENUE_KEY: &str = "fake-binance-key-5c6d";

fn plant(dir: &std::path::Path) {
    write_store(
        dir,
        &[
            (
                "oanda",
                AccountTier::Demo,
                None,
                pairs(&[("API_KEY", DEMO_KEY), ("ACCOUNT_ID", "acct-1")]),
            ),
            (
                "oanda",
                AccountTier::Live,
                None,
                pairs(&[("API_KEY", LIVE_KEY), ("ACCOUNT_ID", "acct-2")]),
            ),
            (
                "oanda",
                AccountTier::Demo,
                Some("ALT"),
                pairs(&[("API_KEY", "fake-labelled-key-7e8f")]),
            ),
            ("binance", AccountTier::Live, None, pairs(&[("API_KEY", OTHER_VENUE_KEY)])),
        ],
    );
}

/// **The account is the SAME function the tradehub used**: the venue's default account at `demo`. The
/// directory the lane gets holds that account's two fields and nothing of its neighbours: not the live
/// account's, not a labelled demo account's, not another venue's.
#[test]
fn the_directory_holds_the_data_feed_account_and_none_of_its_neighbours() {
    let dir = tempfile::tempdir().expect("a temp dir");
    plant(dir.path());
    let source = StoreAccounts::source(at(dir.path()));
    let d = source.directory("oanda");

    let feed = d.data_feed_credentials("oanda");
    assert_eq!(feed.get("API_KEY"), Some(DEMO_KEY), "the default demo account's key");
    assert_eq!(feed.get("ACCOUNT_ID"), Some("acct-1"));

    // Nothing else came along: the other accounts resolve to empty views.
    let default = AccountLabel::Default;
    assert!(
        !d.credentials_for("oanda", &default, AccountTier::Live).holds_any(),
        "the live account's fields stayed in the store"
    );
    assert!(
        !d.credentials_for("binance", &default, AccountTier::Live).holds_any(),
        "another venue's account stayed in the store"
    );
    // The ROWS are the whole table (the choice is made by them), the values are one account's.
    assert!(d.account_row("oanda", &default, AccountTier::Live).is_some());
    let book = d.credentials().and_then(Result::ok).expect("a book");
    let held: Vec<_> = (1..=4).filter(|id| book.account(*id).is_some()).collect();
    assert_eq!(held.len(), 1, "exactly one account's fields are held: {held:?}");
}

/// A `data_only` venue has no `demo` row for its feed to log in as: its `paper` row holds the demo login.
#[test]
fn a_data_only_venue_logs_in_as_the_paper_account_behind_its_strategy() {
    let dir = tempfile::tempdir().expect("a temp dir");
    write_store(
        dir.path(),
        &[(
            "ig",
            AccountTier::Paper,
            None,
            pairs(&[("API_KEY", "fake-ig-key"), ("PASSWORD", "fake-pw")]),
        )],
    );
    let d = StoreAccounts::source(at(dir.path())).directory("ig");
    let feed = d.data_feed_credentials("ig");
    assert_eq!(feed.get("API_KEY"), Some("fake-ig-key"));
    assert_eq!(feed.get("PASSWORD"), Some("fake-pw"));
}

/// No such account, or a row holding nothing, is an EMPTY view: the live gate.
#[test]
fn no_account_for_the_venue_resolves_to_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    plant(dir.path());
    let d = StoreAccounts::source(at(dir.path())).directory("alpaca");
    assert!(!d.data_feed_credentials("alpaca").holds_any());
}

/// **Fresh on every open**: a key rotated between two opens is the second open's key.
#[test]
fn every_open_reads_the_store_afresh() {
    let dir = tempfile::tempdir().expect("a temp dir");
    plant(dir.path());
    let source = StoreAccounts::source(at(dir.path()));
    assert_eq!(
        source.directory("oanda").data_feed_credentials("oanda").get("API_KEY"),
        Some(DEMO_KEY)
    );
    write_store(
        dir.path(),
        &[(
            "oanda",
            AccountTier::Demo,
            None,
            pairs(&[("API_KEY", "fake-rotated-key"), ("ACCOUNT_ID", "acct-1")]),
        )],
    );
    assert_eq!(
        source.directory("oanda").data_feed_credentials("oanda").get("API_KEY"),
        Some("fake-rotated-key")
    );
}

/// A store that EXISTS and will not read is an error the directory carries, never an absence.
#[test]
fn an_unreadable_store_is_carried_as_an_error_and_not_as_no_credentials() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(dir.path()), b"not a sqlite database at all")
        .expect("plant the garbage");
    let d = StoreAccounts::source(at(dir.path())).directory("oanda");
    assert!(matches!(d.credentials(), Some(Err(_))), "the store failure must reach the refusal");
    assert!(!d.data_feed_credentials("oanda").holds_any());
}

/// **The feed's handle only ever NARROWS.** A `Full` daemon store is reopened over the same location under
/// `NoLiveKeys` (a data feed logs in as a `demo` or `paper` account, so a live key has no business in the
/// call); a handle that is already narrower is kept as it is, never widened.
#[test]
fn the_feed_reads_through_a_handle_that_only_narrows() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let open = |scope| {
        vike_secrets::CredentialStore::open(
            Some(vike_secrets::StoreLocation::new(dir.path())),
            scope,
        )
    };
    let full = feed_store(&open(vike_secrets::CredentialScope::Full));
    assert_eq!(full.scope(), vike_secrets::CredentialScope::NoLiveKeys);
    assert_eq!(full.location(), open(vike_secrets::CredentialScope::Full).location());
    for kept in
        [vike_secrets::CredentialScope::NoLiveKeys, vike_secrets::CredentialScope::NoVenueKeys]
    {
        assert_eq!(feed_store(&open(kept)).scope(), kept, "{kept:?} must not be widened");
    }
}

/// The venue's APPLICATION fields (cTrader's OAuth client) come along with the account's, and nothing
/// of any other venue's does.
#[test]
fn the_venues_application_fields_come_with_the_account() {
    let dir = tempfile::tempdir().expect("a temp dir");
    write_store(
        dir.path(),
        &[
            ("ctrader", AccountTier::Demo, None, pairs(&[("ACCESS_TOKEN", "fake-ctrader-token")])),
            ("ig", AccountTier::Demo, None, pairs(&[("API_KEY", "fake-ig-key-3k4l")])),
        ],
    );
    for (venue, field) in [("ctrader", "CLIENT_ID"), ("ig", "CLIENT_ID")] {
        vike_secrets::save_owned_credentials_unjournalled(
            dir.path(),
            &vike_secrets::CredentialOwner::Venue(venue.to_string()),
            &[(field.to_string(), format!("fake-{venue}-app"))],
            vike_secrets::UpsertIntent::Add,
        )
        .expect("write the venue's application field");
    }
    let d = StoreAccounts::source(at(dir.path())).directory("ctrader");
    let feed = d.data_feed_credentials("ctrader");
    assert_eq!(feed.get("ACCESS_TOKEN"), Some("fake-ctrader-token"));
    assert_eq!(feed.venue_get("CLIENT_ID"), Some("fake-ctrader-app"));
    let book = d.credentials().and_then(Result::ok).expect("a book");
    assert!(book.venue("ig").is_none(), "another venue's application fields stayed in the store");
}
