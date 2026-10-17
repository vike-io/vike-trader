//! Unit tests of the pure fold: account rows and their fields, by FIELD, into tier cells.
//!
//! Every fixture is an in-memory `AccountDirectory` — the three store reads a root performs — so
//! nothing here opens a store. ⚠ No venue-prefixed NAME is spelled (a `src/` test region is swept by
//! the settings registry): stored names below are the writer's own `{venue}#{id}.{FIELD}` shape.

use std::collections::{BTreeMap, HashMap};

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, AccountKeys, Accounts};

use super::*;

fn row(id: i64, venue: &str, tier: AccountTier, label: Option<&str>, active: bool) -> Account {
    Account {
        id,
        venue: venue.to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
    }
}

fn dir(rows: Vec<Account>, book: CredentialBook) -> AccountDirectory {
    AccountDirectory::from_rows(Accounts::Known(rows), Some(BTreeMap::new())).with_credentials(book)
}

fn grids(rows: Vec<Account>, book: CredentialBook) -> AccountGrids {
    AccountGrids::from_directory(&dir(rows, book), &HashMap::new())
}

fn venue_row<'g>(grids: &'g AccountGrids, venue: &str) -> &'g VenueCredStatus {
    grids.default_grid().iter().find(|s| s.venue == venue).expect("a roster venue")
}

fn account(cell: &TierCell) -> &AccountCell {
    match cell {
        TierCell::Account(a) => a,
        other => panic!("not an account cell: {other:?}"),
    }
}

/// An empty account table: every cell of every venue has NO account row, the roster order is
/// the grid's, and the store ANSWERED — a measured absence, not an unknown.
#[test]
fn an_empty_account_table_is_every_cell_without_an_account() {
    let g = grids(Vec::new(), CredentialBook::new());
    assert!(g.health().is_readable());
    let names: Vec<&str> = g.default_grid().iter().map(|s| s.venue.as_str()).collect();
    assert_eq!(names, vike_model::VENUES.to_vec());
    for s in g.default_grid() {
        for tier in AccountTier::ALL {
            assert_eq!(s.cell(tier), &TierCell::NoAccount, "{} {tier}", s.venue);
        }
    }
    assert_eq!(g.labels().count(), 0);
    assert!(credentialed_venues(&g).is_empty());
}

/// A directory nobody read is NOT an empty one: its grid claims no measurement.
#[test]
fn an_unread_directory_is_unreadable_never_empty() {
    let g = AccountGrids::unread();
    assert!(!g.health().is_readable(), "nothing was read, so nothing was measured");
    assert!(credentialed_venues(&g).is_empty());
}

/// A store that would not open carries its fault verbatim into the grid's health.
#[test]
fn a_failed_read_is_the_grids_health() {
    let d = AccountDirectory::read(
        Err::<Accounts, _>("permission denied"),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>("permission denied"),
        Err::<CredentialBook, _>("permission denied"),
    );
    let g = AccountGrids::from_directory(&d, &HashMap::new());
    match g.health() {
        vike_secrets::credentials::StoreHealth::Unreadable(why) => {
            assert_eq!(why, "permission denied", "one fault, reported once")
        }
        other => panic!("{other:?}"),
    }
}

/// **Complete, partial, empty — by the venue's REQUIRED account fields.** binance requires
/// `API_KEY` and `API_SECRET`; the optional passphrase moves nothing.
#[test]
fn a_cells_presence_is_its_rows_required_fields() {
    let mut book = CredentialBook::new();
    book.insert_account(1, "API_KEY", "k");
    book.insert_account(1, "API_SECRET", "s");
    book.insert_account(2, "API_KEY", "k");
    book.insert_account(3, "API_PASSPHRASE", "p");
    let g = grids(
        vec![
            row(1, "binance", AccountTier::Live, None, true),
            row(2, "binance", AccountTier::Demo, None, true),
            row(3, "bybit", AccountTier::Demo, None, true),
        ],
        book,
    );
    let binance = venue_row(&g, "binance");
    assert_eq!(account(&binance.live).presence(), Presence::Complete);
    assert!(binance.is_complete(AccountTier::Live));
    let demo = account(&binance.demo);
    assert_eq!(demo.presence(), Presence::Partial);
    assert_eq!(demo.missing_required, vec!["API_SECRET"]);
    assert_eq!(binance.paper, TierCell::NoAccount);

    let bybit = account(&venue_row(&g, "bybit").demo);
    assert_eq!(bybit.presence(), Presence::Empty, "an optional field alone completes nothing");
    assert_eq!(credentialed_venues(&g), vec!["binance".to_string()]);
}

/// A blank value is ABSENT, exactly as every loader reads it.
#[test]
fn a_blank_field_is_not_held() {
    let mut book = CredentialBook::new();
    book.insert_account(1, "API_KEY", "  ");
    book.insert_account(1, "API_SECRET", "s");
    let g = grids(vec![row(1, "binance", AccountTier::Live, None, true)], book);
    assert_eq!(account(&venue_row(&g, "binance").live).missing_required, vec!["API_KEY"]);
}

/// **A label is an account ROW's**, and its cells are that row's — never the default account's.
#[test]
fn a_labelled_row_is_its_own_accounts_cell() {
    let mut book = CredentialBook::new();
    book.insert_account(9, "API_KEY", "k");
    book.insert_account(9, "API_SECRET", "s");
    let g = grids(vec![row(9, "binance", AccountTier::Demo, Some("ALT"), true)], book);
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    assert_eq!(g.labels().cloned().collect::<Vec<_>>(), vec![alt.clone()]);
    let grid = g.grid_for(&alt).expect("the label has a grid");
    let binance = grid.iter().find(|s| s.venue == "binance").expect("binance");
    assert_eq!(account(&binance.demo).id, 9);
    assert_eq!(venue_row(&g, "binance").demo, TierCell::NoAccount, "the default account has none");
    assert!(credentialed_venues(&g).is_empty(), "a labelled account is not the default's");
    assert!(g.grid_for(&AccountLabel::parse("NEW").expect("legal")).is_none());
    assert!(g.absent_grid().iter().all(|s| s.demo == TierCell::NoAccount));
}

/// An INACTIVE row is still its cell (it owns its credentials); two rows with no single active one
/// are AMBIGUOUS, and two with one active resolve to the active one.
#[test]
fn an_inactive_row_is_its_cell_and_two_rows_resolve_or_refuse() {
    let g = grids(vec![row(4, "fxcm", AccountTier::Demo, None, false)], CredentialBook::new());
    let cell = account(&venue_row(&g, "fxcm").demo);
    assert_eq!((cell.id, cell.active), (4, false));

    let g = grids(
        vec![
            row(5, "dukascopy", AccountTier::Demo, None, false),
            row(6, "dukascopy", AccountTier::Demo, None, false),
        ],
        CredentialBook::new(),
    );
    assert_eq!(venue_row(&g, "dukascopy").demo, TierCell::Ambiguous(vec![5, 6]));

    let g = grids(
        vec![
            row(5, "dukascopy", AccountTier::Demo, None, false),
            row(6, "dukascopy", AccountTier::Demo, None, true),
        ],
        CredentialBook::new(),
    );
    assert_eq!(account(&venue_row(&g, "dukascopy").demo).id, 6);
}

/// **Only a PUBLIC field's value is carried**, and a secret's never is — the security boundary.
#[test]
fn only_public_values_are_readable() {
    let mut book = CredentialBook::new();
    book.insert_account(1, "LOGIN", "login-1");
    book.insert_account(1, "PASSWORD", "pw-1");
    book.insert_venue("ctrader", "CLIENT_ID", "app-1");
    book.insert_venue("ctrader", "CLIENT_SECRET", "app-secret");
    let g = grids(vec![row(1, "dukascopy", AccountTier::Demo, None, true)], book);
    let held = g.account_fields(1).expect("row 1 holds fields");
    assert!(held.holds("LOGIN") && held.holds("PASSWORD"));
    assert_eq!(held.readable("LOGIN"), Some("login-1"));
    assert_eq!(held.readable("PASSWORD"), None, "a secret is never carried");
    let app = g.venue_fields("ctrader").expect("ctrader's application rows");
    assert_eq!(app.readable("CLIENT_ID"), Some("app-1"));
    assert_eq!(app.readable("CLIENT_SECRET"), None);
    assert_eq!(app.stored_name("CLIENT_ID"), None, "no listing names a venue's application rows");
}

/// **The stored NAME a Remove retires is the listing's own**, picked by the store's prefix split —
/// and a field the listing does not name exactly once has none.
#[test]
fn the_stored_name_is_picked_out_of_the_accounts_own_listing() {
    let mut book = CredentialBook::new();
    book.insert_account(3, "API_KEY", "k");
    book.insert_account(3, "RELAYER_API_KEY", "r");
    book.insert_account(3, "PRIVATE_KEY", "p");
    let keys = BTreeMap::from([(
        3,
        AccountKeys {
            prefixes: vec!["polymarket#3.".to_string()],
            names: vec![
                "polymarket#3.API_KEY".to_string(),
                "polymarket#3.RELAYER_API_KEY".to_string(),
            ],
        },
    )]);
    let d = AccountDirectory::from_rows(
        Accounts::Known(vec![row(3, "polymarket", AccountTier::Live, None, true)]),
        Some(keys),
    )
    .with_credentials(book);
    let g = AccountGrids::from_directory(&d, &HashMap::new());
    let held = g.account_fields(3).expect("row 3");
    assert_eq!(held.stored_name("API_KEY"), Some("polymarket#3.API_KEY"));
    assert_eq!(held.stored_name("RELAYER_API_KEY"), Some("polymarket#3.RELAYER_API_KEY"));
    assert_eq!(held.stored_name("PRIVATE_KEY"), None, "no listed name for it: no Remove");
}

/// The venue-wide attribution DEPLOYMENT rows are kept by name (they are public), and nothing else
/// of the caller's name map is.
#[test]
fn only_the_attribution_tail_is_kept_from_the_name_map() {
    let code = vike_venues::credential_keys::attribution_var_for("binance").expect("mechanised");
    let names = HashMap::from([
        (code.clone(), "tag-1".to_string()),
        ("SOME_OTHER_NAME".to_string(), "x".to_string()),
    ]);
    let g = AccountGrids::from_directory(&dir(Vec::new(), CredentialBook::new()), &names);
    assert_eq!(g.deployment_value(&code), Some("tag-1"));
    assert_eq!(g.deployment_value("SOME_OTHER_NAME"), None);
}
