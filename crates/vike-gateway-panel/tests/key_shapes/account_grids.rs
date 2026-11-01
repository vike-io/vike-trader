//! **The panel's account enumeration and its cells, read through a REAL store handle**
//! (`vike_gateway_panel::AccountGrids::read`).
//!
//! The store's `account` rows ARE the accounts (decision 0156 item 6): a label is in the strip
//! when a row carries it, and a (venue, label, tier) cell is that row — its credentials read by
//! FIELD out of `vike_secrets::CredentialStore::credential_book`, the name a Remove retires out of
//! `account_keys`' listing. `crates/vike-gateway-panel/src/status_tests.rs` folds in-memory
//! directories; this member proves the three real reads land where the fold expects them.

use std::collections::HashMap;

use vike_gateway_panel::{AccountGrids, Presence, TierCell, VenueCredStatus, credentialed_venues};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_paths::scratch::ScratchDir;
use vike_secrets::credentials::StoreHealth;
use vike_secrets::{CredentialOwner, CredentialScope};

use crate::support::{account, fields, grids, grids_scoped, store};

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

fn venue_row<'g>(rows: &'g [VenueCredStatus], venue: &str) -> &'g VenueCredStatus {
    rows.iter().find(|s| s.venue == venue).expect("a roster venue")
}

fn presence(cell: &TierCell) -> Presence {
    match cell {
        TierCell::Account(a) => a.presence(),
        other => panic!("not an account cell: {other:?}"),
    }
}

/// An EMPTY store answers, and its answer is "no account anywhere" — a measurement, not a fault.
#[test]
fn an_empty_store_answers_no_account_anywhere() {
    let dir = store("vike-gateway-panel-grids-empty");
    let g = grids(&dir);
    assert_eq!(g.health(), &StoreHealth::Readable);
    assert_eq!(g.labels().count(), 0);
    for s in g.default_grid() {
        for tier in AccountTier::ALL {
            assert_eq!(s.cell(tier), &TierCell::NoAccount, "{} {tier}", s.venue);
        }
    }
    assert!(credentialed_venues(&g).is_empty());
}

/// A box with NO settings database is not a fault either: there is no store, so there is no
/// account — the same answer the mount gives it.
#[test]
fn a_box_with_no_database_has_no_account_and_no_fault() {
    let dir = ScratchDir::create_in(&std::env::temp_dir(), "vike-gateway-panel-no-db")
        .expect("scratch root");
    let g = grids_scoped(dir.path(), CredentialScope::Full);
    assert_eq!(g.health(), &StoreHealth::Readable, "an absent store is a real answer");
    assert!(g.default_grid().iter().all(|s| s.demo == TierCell::NoAccount));
}

/// ⚠ **A store that EXISTS and would not open is a FAULT, never an absence** — the grid's health
/// says so, so the panel can claim no measurement.
#[test]
fn a_store_that_will_not_open_is_unreadable_never_empty() {
    let dir = ScratchDir::create_in(&std::env::temp_dir(), "vike-gateway-panel-bad-db")
        .expect("scratch root");
    let db = vike_secrets::db_path_in(dir.path());
    std::fs::create_dir_all(db.parent().expect("the database sits in a directory"))
        .expect("db dir");
    std::fs::write(&db, b"this is not a sqlite database").expect("plant a broken database");
    let g = grids_scoped(dir.path(), CredentialScope::Full);
    assert!(!g.health().is_readable(), "a broken store is not an empty one: {:?}", g.health());
    assert!(credentialed_venues(&g).is_empty());
}

/// **A label is a ROW's.** A labelled row enumerates its label, and its cells are ITS; the default
/// (unlabelled) account's cells are the unlabelled rows', never borrowed.
#[test]
fn a_labelled_row_enumerates_its_label_and_owns_its_cells() {
    let dir = store("vike-gateway-panel-grids-labelled");
    let alt = account(&dir, "binance", AccountTier::Demo, Some("ALT"));
    let default_live = account(&dir, "binance", AccountTier::Live, None);
    fields(&dir, &CredentialOwner::Account(alt), &[("API_KEY", "k"), ("API_SECRET", "s")]);

    let g = grids(&dir);
    assert_eq!(g.labels().cloned().collect::<Vec<_>>(), vec![label("ALT")]);

    let alt_grid = g.grid_for(&label("ALT")).expect("a row carries ALT");
    let alt_binance = venue_row(alt_grid, "binance");
    assert!(matches!(&alt_binance.demo, TierCell::Account(a) if a.id == alt));
    assert_eq!(presence(&alt_binance.demo), Presence::Complete);
    assert_eq!(alt_binance.live, TierCell::NoAccount, "ALT has no live row");

    let default_binance = venue_row(g.default_grid(), "binance");
    assert_eq!(default_binance.demo, TierCell::NoAccount, "ALT's row is not the default's");
    assert!(matches!(&default_binance.live, TierCell::Account(a) if a.id == default_live));
    assert_eq!(presence(&default_binance.live), Presence::Empty);

    assert!(g.grid_for(&label("NEW")).is_none(), "a label no row carries has no grid");
    assert!(
        credentialed_venues(&g).is_empty(),
        "the default account's live row holds nothing, and ALT's is not the default's"
    );
}

/// **Presence is the row's REQUIRED fields, read out of the store**: complete, partial, empty —
/// and the optional passphrase moves nothing.
#[test]
fn a_cells_presence_is_read_from_the_stores_fields() {
    let dir = store("vike-gateway-panel-grids-presence");
    let complete = account(&dir, "binance", AccountTier::Live, None);
    let partial = account(&dir, "binance", AccountTier::Demo, None);
    let empty = account(&dir, "bybit", AccountTier::Demo, None);
    fields(&dir, &CredentialOwner::Account(complete), &[("API_KEY", "k"), ("API_SECRET", "s")]);
    fields(&dir, &CredentialOwner::Account(partial), &[("API_KEY", "k")]);
    fields(&dir, &CredentialOwner::Account(empty), &[("API_PASSPHRASE", "p")]);

    let g = grids(&dir);
    let binance = venue_row(g.default_grid(), "binance");
    assert_eq!(presence(&binance.live), Presence::Complete);
    assert_eq!(presence(&binance.demo), Presence::Partial);
    match &binance.demo {
        TierCell::Account(a) => assert_eq!(a.missing_required, vec!["API_SECRET"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        presence(&venue_row(g.default_grid(), "bybit").demo),
        Presence::Empty,
        "bybit's passphrase is OPTIONAL, so holding it alone holds none of the required fields"
    );
    assert_eq!(credentialed_venues(&g), vec!["binance".to_string()]);
}

/// **The NAME a Remove retires is the store's own** — the name the writer stored the row under,
/// picked out of `account_keys`' listing, never composed by the panel.
#[test]
fn the_stored_name_is_the_name_the_writer_stored() {
    let dir = store("vike-gateway-panel-grids-names");
    let id = account(&dir, "polymarket", AccountTier::Live, None);
    let names = fields(
        &dir,
        &CredentialOwner::Account(id),
        &[("PRIVATE_KEY", "p"), ("API_KEY", "k"), ("RELAYER_API_KEY", "r")],
    );
    let g = grids(&dir);
    let held = g.account_fields(id).expect("the row holds fields");
    assert_eq!(held.stored_name("PRIVATE_KEY"), Some(names[0].as_str()));
    assert_eq!(held.stored_name("API_KEY"), Some(names[1].as_str()));
    assert_eq!(
        held.stored_name("RELAYER_API_KEY"),
        Some(names[2].as_str()),
        "a field whose word ends another field's word still picks its own row"
    );
}

/// **Only a PUBLIC value is carried**: a login is readable, a password never is — read through the
/// real credential book, the boundary the panel's prefill stands on.
#[test]
fn only_a_public_fields_value_reaches_the_grid() {
    let dir = store("vike-gateway-panel-grids-readable");
    let id = account(&dir, "dukascopy", AccountTier::Demo, Some("EU"));
    fields(&dir, &CredentialOwner::Account(id), &[("LOGIN", "login-1"), ("PASSWORD", "pw-1")]);
    let g = grids(&dir);
    let held = g.account_fields(id).expect("the row holds fields");
    assert!(held.holds("LOGIN") && held.holds("PASSWORD"));
    assert_eq!(held.readable("LOGIN"), Some("login-1"));
    assert_eq!(held.readable("PASSWORD"), None);
}

/// **A venue's APPLICATION fields are the venue's** — cTrader's client pair, written against the
/// venue and read back as the venue's, never as any account's.
#[test]
fn a_venues_application_fields_are_the_venues() {
    let dir = store("vike-gateway-panel-grids-app");
    let id = account(&dir, "ctrader", AccountTier::Demo, None);
    fields(
        &dir,
        &CredentialOwner::Venue("ctrader".to_string()),
        &[("CLIENT_ID", "app-1"), ("CLIENT_SECRET", "app-secret")],
    );
    let g = grids(&dir);
    let app = g.venue_fields("ctrader").expect("the venue's application rows");
    assert!(app.holds("CLIENT_ID") && app.holds("CLIENT_SECRET"));
    assert_eq!(app.readable("CLIENT_ID"), Some("app-1"));
    assert_eq!(app.readable("CLIENT_SECRET"), None);
    assert_eq!(
        presence(&venue_row(g.default_grid(), "ctrader").demo),
        Presence::Empty,
        "the account's own grant is what its cell measures; the app pair is the venue's"
    );
    assert!(g.account_fields(id).is_none(), "nothing was written to the account");
}

/// ⚠ **The live-key scope binds the grid too**: under `CredentialScope::NoLiveKeys` a LIVE
/// account's fields are not read, so its cell holds nothing, while a demo account's still reads.
#[test]
fn the_no_live_keys_scope_withholds_a_live_accounts_fields() {
    let dir = store("vike-gateway-panel-grids-scope");
    let live = account(&dir, "binance", AccountTier::Live, None);
    let demo = account(&dir, "binance", AccountTier::Demo, None);
    for id in [live, demo] {
        fields(&dir, &CredentialOwner::Account(id), &[("API_KEY", "k"), ("API_SECRET", "s")]);
    }
    let g = grids_scoped(dir.path(), CredentialScope::NoLiveKeys);
    let binance = venue_row(g.default_grid(), "binance");
    assert_eq!(presence(&binance.live), Presence::Empty, "the live row's fields were withheld");
    assert_eq!(presence(&binance.demo), Presence::Complete);
    let full = grids(&dir);
    assert_eq!(presence(&venue_row(full.default_grid(), "binance").live), Presence::Complete);
}

/// The panel's grid is the same fold whatever handle read it: `read` is `from_directory` over the
/// three reads, and a grid built from them by hand answers identically.
#[test]
fn read_is_the_fold_over_the_three_store_reads() {
    let dir = store("vike-gateway-panel-grids-read");
    let id = account(&dir, "fxcm", AccountTier::Demo, None);
    fields(&dir, &CredentialOwner::Account(id), &[("USER", "u"), ("PASSWORD", "p")]);
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir.path())),
        CredentialScope::Full,
    );
    let by_hand = AccountGrids::from_directory(
        &vike_secrets::account_directory::AccountDirectory::read(
            store.accounts(),
            store.account_keys(),
            store.credential_book(),
        ),
        &HashMap::new(),
    );
    assert_eq!(AccountGrids::read(&store, &HashMap::new()), by_hand);
}
