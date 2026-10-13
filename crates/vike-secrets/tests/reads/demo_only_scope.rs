//! **The NO-LIVE-KEYS scope withholds by the ACCOUNT'S TIER and by two VENUE families — never by a
//! token in a credential's name.**
//!
//! `crates/vike-secrets/src/db/read.rs`'s `DEMO_SCOPE_WITHHELD_SQL` is the rule, applied inside the
//! database's own `WHERE` so a withheld row's value is never selected. Since decision 0156 item 6
//! the tier is written in ONE place, `account.tier`, so the rule has two halves and no third:
//!
//! * every credential of an account whose row says `live`, WHATEVER its rows are called — a legacy
//!   name spelling `DEMO` on a live account is withheld, and one spelling `LIVE` on a demo account
//!   is kept;
//! * every row of the aster and polymarket families (a legacy `ASTER_`/`POLY_` name, an account row
//!   the writer named `aster#…`/`polymarket#…`, a venue row `aster.…`/`polymarket.…`),
//!   case-insensitively, whatever the account's tier.
//!
//! This plants one store with a row on every edge of that rule and holds:
//!
//! 1. the DATABASE arm of a `NoLiveKeys` handle's whole-map read against the hand-written
//!    expectation, with the unscoped read of the same store as the control;
//! 2. the scope's reach: the account credential BOOK, the one-field read, the account KEY NAMES and
//!    the node keys are read under it too;
//! 3. that the tier is the ROW'S: moving an account to `live` withholds rows it held a moment ago.

use std::assert_matches;
use std::collections::BTreeSet;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::CredentialFields;
use vike_secrets::{AccountEdit, CredentialScope, Source};

use crate::support::{self, Fixture};

/// The planted store, and what the scope must keep and withhold of it — NAMES, each written the way
/// production writes it (or, for a legacy name, the way an older store holds it).
struct Planted {
    fx: Fixture,
    kept: BTreeSet<String>,
    withheld: BTreeSet<String>,
    demo: i64,
    live: i64,
}

fn planted() -> Planted {
    let fx = Fixture::initialised();
    let mut kept = BTreeSet::new();
    let mut withheld = BTreeSet::new();

    // A DEMO account: kept — its writer-named rows, and a LEGACY row whose NAME spells `LIVE`.
    let demo = fx.create_account("binance", AccountTier::Demo, None);
    kept.extend(fx.attach(demo, &["API_KEY", "API_SECRET"]));
    fx.plant_legacy_credential(demo, "BINANCE_LIVE_API_PASSPHRASE", "API_PASSPHRASE");
    kept.insert("BINANCE_LIVE_API_PASSPHRASE".to_string());

    // A LIVE account: withheld — every row, a legacy name spelling `DEMO` included.
    let live = fx.create_account("bybit", AccountTier::Live, None);
    withheld.extend(fx.attach(live, &["API_KEY"]));
    fx.plant_legacy_credential(live, "BYBIT_DEMO_API_SECRET", "API_SECRET");
    withheld.insert("BYBIT_DEMO_API_SECRET".to_string());

    // A PAPER account is not live: kept.
    let paper = fx.create_account("okx", AccountTier::Paper, None);
    kept.extend(fx.attach(paper, &["API_KEY"]));

    // The two families, at a DEMO tier: withheld by venue, account rows and venue rows alike.
    let aster = fx.create_account("aster", AccountTier::Demo, None);
    withheld.extend(fx.attach(aster, &["API_KEY"]));
    let poly = fx.create_account("polymarket", AccountTier::Demo, None);
    withheld.extend(fx.attach(poly, &["PRIVATE_KEY"]));
    withheld.extend(fx.attach_venue("aster", &["BUILDER_CODE"]));
    withheld.extend(fx.attach_venue("polymarket", &["RELAYER_API_KEY"]));
    // Another venue's application row carries no tier: kept.
    kept.extend(fx.attach_venue("ctrader", &["CLIENT_ID"]));

    // DEPLOYMENT rows, by name.
    let deployment_kept = [
        // ⚠ A name carrying a LIVE or MAINNET token is no longer judged by it: no account owns
        // these rows, so no tier withholds them (decision 0156 item 6).
        "BINANCE_LIVE_API_KEY",
        "HYPERLIQUID_MAINNET_PRIVATE_KEY",
        // Attribution carries no tier.
        "OKX_BROKER_CODE",
        // Near misses: `ASTER_`/`POLY_` are whole-segment PREFIXES.
        "MASTER_SWITCH_TOKEN",
        "ASTERISK_NOTE",
        "POLYGON_RPC_URL",
    ];
    let deployment_withheld = [
        "ASTER_DEMO_API_KEY",
        "POLY_PRIVATE_KEY",
        // Case can only WIDEN what is withheld.
        "aster_live_private_key",
        "Poly_Funder",
    ];
    fx.write(
        support::fake_rows(deployment_kept.iter().chain(&deployment_withheld)),
        support::is_node_key,
    );
    kept.extend(deployment_kept.iter().map(|n| (*n).to_string()));
    withheld.extend(deployment_withheld.iter().map(|n| (*n).to_string()));

    Planted { fx, kept, withheld, demo, live }
}

#[test]
fn the_database_arm_withholds_by_account_tier_and_venue_family_never_by_a_name_token() {
    let p = planted();

    // The CONTROL: the unscoped read of the same database returns every planted name.
    let all = support::store_in(p.fx.dir()).read_all().expect("unscoped read");
    assert_matches!(all.source, Source::Database(_), "the database must answer: {:?}", all.source);
    let every: BTreeSet<String> = p.kept.union(&p.withheld).cloned().collect();
    assert_eq!(support::key_names(&all.secrets), every, "the control lost or invented names");

    let scoped = support::scoped_store_in(p.fx.dir(), CredentialScope::NoLiveKeys)
        .read_all()
        .expect("no-live-keys read");
    assert_matches!(scoped.source, Source::Database(_), "same store: {:?}", scoped.source);
    assert_eq!(scoped.scope, CredentialScope::NoLiveKeys);
    // ⚠ A row NO account owns (a deployment row, a venue's application row) has a NULL
    // `account_id`, and the store holds a live account beside it — the case a `account_id IN
    // (SELECT …)` half reads as NULL rather than false, so `NOT (…)` drops the row unseen and
    // uncounted. Such a row carries no tier and must be KEPT.
    for n in ["OKX_BROKER_CODE", "BINANCE_LIVE_API_KEY", "ctrader.CLIENT_ID"] {
        assert!(
            scoped.secrets.keys().any(|k| k == n),
            "`{n}` belongs to no account, so no tier can withhold it — the predicate must read a \
             NULL `account_id` as `not a live account's`"
        );
    }
    assert_eq!(
        support::key_names(&scoped.secrets),
        p.kept,
        "the DATABASE arm's SQL predicate kept a live-tier or family row, or withheld another"
    );
    assert_eq!(scoped.withheld, p.withheld.len(), "the withheld count is wrong");
    let map = scoped.secrets.into_map();
    for n in &p.kept {
        assert_eq!(
            map.get(n),
            Some(&support::fake_value(n)),
            "`{n}` came back with the wrong value"
        );
    }
}

#[test]
fn an_absent_store_is_the_live_gate_under_the_scope_too() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let scoped =
        support::scoped_store_in(&tmp.path().join("settings"), CredentialScope::NoLiveKeys)
            .read_all()
            .expect("absent");
    assert_eq!(scoped.source, Source::None);
    assert!(scoped.secrets.is_empty());
    assert_eq!(scoped.withheld, 0);
}

/// **The scope binds the account credential BOOK and the one-field read** — the reads a mount and a
/// one-value root take. The live account answers nothing under it, the demo account answers in
/// full, and an aster DEMO account answers nothing (its family is withheld at every tier).
#[test]
fn the_scope_binds_the_credential_book_and_the_one_field_read() {
    let p = planted();
    let full = support::store_in(p.fx.dir()).credential_book().expect("opens");
    let narrow = support::scoped_store_in(p.fx.dir(), CredentialScope::NoLiveKeys)
        .credential_book()
        .expect("opens");

    // The control: the unscoped book holds the live account's fields.
    assert!(full.account(p.live).is_some_and(|f| f.get("API_KEY").is_some()), "the control");
    assert!(
        narrow.account(p.live).is_none_or(CredentialFields::is_empty),
        "a LIVE account's credential reached a no-live-keys book"
    );
    assert_eq!(narrow.account(p.demo), full.account(p.demo), "the demo account is untouched");
    assert!(
        narrow.account(p.demo).is_some_and(|f| f.get("API_PASSPHRASE").is_some()),
        "…its legacy row spelling LIVE included"
    );
    let aster = p.fx.id_of("aster", "demo");
    assert!(full.account(aster).is_some(), "the control holds the aster account's row");
    assert!(narrow.account(aster).is_none(), "the aster family is withheld at a demo tier too");
    assert!(narrow.venue("aster").is_none() && narrow.venue("polymarket").is_none());
    assert_eq!(narrow.venue("ctrader"), full.venue("ctrader"), "another venue's row is kept");

    // The narrowest read obeys the same rule.
    let field = |scope, venue: &str, tier| {
        support::scoped_store_in(p.fx.dir(), scope)
            .account_field(venue, &AccountLabel::Default, tier, "API_KEY")
            .expect("opens")
            .is_some()
    };
    assert!(field(CredentialScope::Full, "bybit", AccountTier::Live), "the control");
    assert!(!field(CredentialScope::NoLiveKeys, "bybit", AccountTier::Live), "a live field leaked");
    assert!(
        field(CredentialScope::NoLiveKeys, "binance", AccountTier::Demo),
        "a demo field is kept"
    );
}

/// **The tier is the ROW'S, read at every read**: an account moved to `live` has its rows withheld
/// from then on, and moved back they are kept again — no row was renamed, and none had to be.
#[test]
fn moving_an_account_to_live_withholds_its_rows_whatever_they_are_called() {
    let p = planted();
    let names = |fx: &Fixture| {
        support::key_names(
            &support::scoped_store_in(fx.dir(), CredentialScope::NoLiveKeys)
                .read_all()
                .expect("opens")
                .secrets,
        )
    };
    let demo_rows: BTreeSet<String> = support::store_in(p.fx.dir())
        .account_keys()
        .expect("opens")
        .expect("keyed")
        .remove(&p.demo)
        .expect("the demo account owns rows")
        .names
        .into_iter()
        .collect();
    assert!(demo_rows.is_subset(&names(&p.fx)), "the precondition: a demo account is kept");

    p.fx.edit(AccountEdit::SetTier { id: p.demo, tier: AccountTier::Live }).expect("re-tier");
    assert!(
        names(&p.fx).is_disjoint(&demo_rows),
        "the account is live now: every row it owns is withheld, whatever its name spells"
    );

    p.fx.edit(AccountEdit::SetTier { id: p.demo, tier: AccountTier::Demo }).expect("back");
    assert!(demo_rows.is_subset(&names(&p.fx)), "…and kept again once it is not");
}

/// **The scope reaches the account KEY NAMES** — a credential row's name is a credential row too.
/// The shared fixture's binance LIVE account owns one name; under the scope that account answers
/// with no names (absent from the map), while the demo accounts keep theirs.
#[test]
fn the_scope_binds_the_account_key_names() {
    let fx = Fixture::seeded();
    let full = support::store_in(fx.dir()).account_keys().expect("opens").expect("keyed");
    let live_id = fx.id_of("binance", "live");
    let demo_id = fx.id_of("binance", "demo");
    let live_key = fx.key("binance", "live", "API_KEY");
    assert!(
        full.get(&live_id).is_some_and(|k| k.names.contains(&live_key)),
        "the control: the live account owns its key name: {full:?}"
    );

    let narrow = support::scoped_store_in(fx.dir(), CredentialScope::NoLiveKeys)
        .account_keys()
        .expect("opens")
        .expect("keyed");
    assert!(
        narrow.get(&live_id).is_none_or(|k| k.names.is_empty()),
        "a live account's key NAME reached a no-live-keys process: {narrow:?}"
    );
    assert_eq!(narrow.get(&demo_id), full.get(&demo_id), "the demo account is untouched");
    // The account ROWS hold no credential: the scope leaves them alone.
    assert_eq!(
        support::scoped_store_in(fx.dir(), CredentialScope::NoLiveKeys).accounts().expect("opens"),
        support::store_in(fx.dir()).accounts().expect("opens"),
    );
}

/// The node-key read is under the scope too: a node key belongs to no account and no venue family,
/// so the pair reads identically, and the read reports nothing withheld.
#[test]
fn the_node_key_read_is_under_the_scope_and_withholds_no_node_key() {
    let names: &[&str] = &vike_venues::credential_keys::PLATFORM_KEYS;
    let fx = Fixture::seeded_with(support::fake_rows(names.iter()), support::node_keys_in(names));
    let full = support::store_in(fx.dir()).node_keys(|_| true).expect("opens");
    let narrow = support::scoped_store_in(fx.dir(), CredentialScope::NoLiveKeys)
        .node_keys(|_| true)
        .expect("opens");
    assert!(!full.secrets.is_empty(), "the control holds the node keys");
    assert_eq!(narrow.secrets, full.secrets);
    assert_eq!(narrow.withheld, 0);
}
