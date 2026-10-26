//! Offline gate on how the live FXCM smokes find the demo login (`tests/common/mod.rs`'s `demo_config`): every smoke
//! must be able to run while the daemon's own FXCM account is DEACTIVATED, because a second session of one login
//! kicks the first and the lane run therefore deactivates it. It places nothing and opens no session.
//!
//! Two halves. The lookup's own behaviour over a directory held in memory (the active row, the one inactive demo row,
//! and the cases that must stay a skip), and a SOURCE PIN: no top-level test file may read the credential store by
//! itself (the active-only read self-skipped two smokes in the v0.1.93 lane run), and every `*smoke*` file must
//! go through the helper.

mod common;

use std::path::Path;

use common::demo_config_from;
use vike_model::AccountTier;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::venue_setting::VenueSettings;
use vike_secrets::{Account, Accounts};

fn row(id: i64, tier: AccountTier, label: Option<&str>, active: bool) -> Account {
    Account {
        id,
        venue: "fxcm".to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
    }
}

/// A directory of `rows` where each id in `logins` holds a `USER`/`PASSWORD` pair named after it.
fn directory(rows: Vec<Account>, logins: &[i64]) -> AccountDirectory {
    let mut book = CredentialBook::new();
    for id in logins {
        book.insert_account(*id, "USER", format!("user{id}"));
        book.insert_account(*id, "PASSWORD", "not-a-secret");
    }
    AccountDirectory::from_rows(Accounts::Known(rows), None).with_credentials(book)
}

fn user(dir: &AccountDirectory) -> Option<String> {
    demo_config_from(dir, &VenueSettings::default()).map(|c| c.user)
}

#[test]
fn the_active_default_demo_row_answers() {
    let dir = directory(vec![row(9, AccountTier::Demo, None, true)], &[9]);
    assert_eq!(user(&dir).as_deref(), Some("user9"));
}

/// The lane run: account 9 deactivated so the smoke's session does not kick the daemon's.
#[test]
fn the_one_inactive_demo_row_answers() {
    let dir = directory(vec![row(9, AccountTier::Demo, None, false)], &[9]);
    assert_eq!(user(&dir).as_deref(), Some("user9"));
}

#[test]
fn the_active_row_wins_over_an_inactive_one() {
    let dir = directory(
        vec![row(9, AccountTier::Demo, None, true), row(10, AccountTier::Demo, None, false)],
        &[9, 10],
    );
    assert_eq!(user(&dir).as_deref(), Some("user9"));
}

#[test]
fn several_inactive_demo_rows_stay_a_skip_never_a_guess() {
    let dir = directory(
        vec![row(9, AccountTier::Demo, None, false), row(10, AccountTier::Demo, None, false)],
        &[9, 10],
    );
    assert_eq!(user(&dir), None);
}

#[test]
fn an_inactive_live_or_labelled_row_is_not_the_demo_login() {
    let dir = directory(
        vec![row(9, AccountTier::Live, None, false), row(10, AccountTier::Demo, Some("B"), false)],
        &[9, 10],
    );
    assert_eq!(user(&dir), None);
}

#[test]
fn a_row_with_no_login_is_a_skip() {
    let dir = directory(vec![row(9, AccountTier::Demo, None, false)], &[]);
    assert_eq!(user(&dir), None);
}

/// The top-level `tests/*.rs` files of this crate except this one, as `(file name, text)`.
fn test_sources() -> Vec<(String, String)> {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&tests).expect("the crate's tests directory") {
        let path = entry.expect("a directory entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if path.extension().is_some_and(|e| e == "rs") && name != "fxcm_smoke_login.rs" {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{} does not read: {e}", path.display()));
            found.push((name, text));
        }
    }
    found
}

#[test]
fn no_live_smoke_reads_only_the_active_row() {
    let sources = test_sources();
    let smokes: Vec<&str> =
        sources.iter().filter(|(n, _)| n.contains("smoke")).map(|(n, _)| n.as_str()).collect();
    assert!(smokes.len() >= 6, "the smoke files were not found: {smokes:?}");

    let reads_alone: Vec<&str> = sources
        .iter()
        .filter(|(_, t)| t.contains("smoke_directory") || t.contains("credentials_for("))
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(
        reads_alone.is_empty(),
        "{reads_alone:?} read the credential store themselves: that answers from the ACTIVE row only, so the \
         file self-skips in a lane run that deactivated the daemon's account. Call `common::demo_config()`."
    );

    let skipping: Vec<&str> = sources
        .iter()
        .filter(|(n, t)| n.contains("smoke") && !t.contains("common::demo_config()"))
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(
        skipping.is_empty(),
        "{skipping:?} never ask `common::demo_config()` for the demo login"
    );
}
