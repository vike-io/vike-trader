use super::*;
use std::collections::BTreeMap;

use vike_bridge_core::credentials::{Account, AccountKeys, Accounts};
use vike_dukascopy::DukascopyAccount;

fn row(id: i64, book: Option<&str>, label: Option<&str>, active: bool) -> Account {
    Account {
        id,
        venue: vike_dukascopy::recon_client::VENUE.to_string(),
        tier: "demo".to_string(),
        label: label.map(str::to_string),
        venue_account_id: book.map(str::to_string),
        parent_id: None,
        active,
        last_verified_at: None,
        armed: false,
    }
}

const SWISS_KEYS: &str = DukascopyAccount::Demo1.key_prefix();
const EU_KEYS: &str = DukascopyAccount::Demo2.key_prefix();

fn keys(rows: &[(i64, &[&str])]) -> BTreeMap<i64, AccountKeys> {
    rows.iter()
        .map(|(id, prefixes)| {
            (
                *id,
                AccountKeys {
                    prefixes: prefixes.iter().map(|p| (*p).to_string()).collect(),
                    names: Vec::new(),
                },
            )
        })
        .collect()
}

/// The live shape: two unlabelled demo rows, distinguishable only by their key prefixes, with
/// the books an operator wrote against them.
fn live_store() -> (Accounts, BTreeMap<i64, AccountKeys>) {
    (
        Accounts::Known(vec![
            row(7, Some("3709890"), None, true),
            row(8, Some("3716974"), None, true),
        ]),
        keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]),
    )
}

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// Every refusal renders a sentence that says PAPER, so no arm can be read as a startup failure
/// or as a silent fallback — and says WHY: one shared platform cache, and the way out (the second
/// account's own project folder).
#[test]
fn every_sidecar_refusal_says_it_stays_paper() {
    for refusal in [
        DukascopySidecarRefusal::SidecarHeldByAnother {
            label: "L".into(),
            holder: "dukascopy".into(),
        },
        DukascopySidecarRefusal::SidecarAlreadyClaimed { label: "L".into() },
    ] {
        let text = refusal.to_string();
        assert!(text.contains("PAPER"), "{refusal:?}: {text}");
        assert!(text.contains('L'), "{refusal:?} must name the account: {text}");
        assert!(text.len() > 60, "{refusal:?}: {text}");
        assert!(text.contains("platform cache"), "{refusal:?}: {text}");
        assert!(text.contains("own project folder"), "{refusal:?}: {text}");
    }
}

/// A store failure is reported as a STORE FAILURE — for BOTH halves of the read, and never as a
/// row that names no broker.
#[test]
fn a_store_that_will_not_open_is_not_reported_as_a_bad_row() {
    let unreadable = AccountDirectory::read(
        Err::<Accounts, _>("disk I/O error"),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>("disk I/O error"),
    );
    let err = resolve_in(&label("3716974"), &unreadable).expect_err("refused");
    assert!(matches!(err, DukascopyRefusal::StoreUnreadable { .. }), "{err}");
    assert!(err.to_string().contains("disk I/O error"), "{err}");
    // …and the DEFAULT account still mounts, because it never needed the table.
    assert_eq!(
        resolve_in(&AccountLabel::Default, &unreadable).expect("default").account,
        DukascopyAccount::Demo1
    );

    // ⚠ THE HALF THAT WAS SWALLOWED: rows fine, key NAMES unreadable. This used to resolve to
    // `UnmappableRow` — *this row's key names do not say which broker it is* — about a row
    // nobody could read the key names of.
    let (accounts, _) = live_store();
    let keys_failed = AccountDirectory::read(
        Ok::<_, &str>(accounts),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>("database is locked"),
    );
    let err = resolve_in(&label("3716974"), &keys_failed).expect_err("refused");
    assert!(
        matches!(err, DukascopyRefusal::StoreUnreadable { .. }),
        "a key-read failure must be a store failure, not a bad row: {err}"
    );
    let text = err.to_string();
    assert!(text.contains("database is locked"), "{text}");
    assert!(text.contains("STORE FAILURE"), "{text}");
}

/// A process that read NO store behaves exactly like a `Backend::Files` box: the default
/// account mounts the Swiss bank, every labelled one is refused.
#[test]
fn an_unread_store_is_a_files_box() {
    let unread = AccountDirectory::unread();
    let mount = resolve_in(&AccountLabel::Default, &unread).expect("default");
    assert_eq!(mount.account, DukascopyAccount::Demo1);
    assert_eq!(mount.row, None);
    let err = resolve_in(&label("3716974"), &unread).expect_err("refused");
    assert!(matches!(err, DukascopyRefusal::NoAccountTable { .. }), "{err}");
}

/// …and a directory a caller DID read resolves both accounts, through the same entry point the
/// mount uses.
#[test]
fn a_read_directory_resolves_each_account_through_the_mount_entry_point() {
    let (accounts, keys) = live_store();
    let dir = AccountDirectory::from_rows(accounts, Some(keys));
    assert_eq!(
        resolve_in(&label("3716974"), &dir).expect("row 8").account,
        DukascopyAccount::Demo2
    );
    assert_eq!(
        resolve_in(&AccountLabel::Default, &dir).expect("default").account,
        DukascopyAccount::Demo1
    );
}
