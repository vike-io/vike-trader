//! **The credential half of an `account`-row fixture.** A credential belongs to its ACCOUNT ROW
//! (decision 0156 item 6: the store's `credential.account_id` + `field`), so a test that arms an
//! account plants the row AND that row's fields, BY FIELD — never a credential NAME, which no
//! reader composes any more. Shared by every binary that `#[path]`-includes it (a bare `mod` would
//! resolve beside the binary root, not here).
//!
//! ⚠ `vike_mount::MountPolicy::with_account` and `with_account_row` rebuild the directory with an
//! EMPTY credential book, so a fixture that plants a row after its credentials loses them. The
//! builders here re-file the book the policy already held, so rows and credentials can be planted
//! in any order.
#![allow(dead_code)]

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::credential_schema::{FieldOwner, schema_for};
use vike_mount::MountPolicy;
use vike_secrets::account_credentials::{AccountCredentials, CredentialBook};

/// Whether `field` is one the VENUE holds once for every account (cTrader's OAuth application
/// pair), by the venue's own row of `vike_model::credential_schema::VENUE_KEY_SCHEMAS` — every other
/// field is the account's.
pub fn is_venue_field(venue: &str, field: &str) -> bool {
    schema_for(venue).and_then(|s| s.field(field)).is_some_and(|f| f.owner == FieldOwner::Venue)
}

/// File `pairs` into `book` as account row `id`'s credentials of `venue` — each FIELD against the
/// owner the venue's schema row names.
pub fn file(book: &mut CredentialBook, venue: &str, id: i64, pairs: &[(&str, &str)]) {
    for (field, value) in pairs {
        if is_venue_field(venue, field) {
            book.insert_venue(venue, *field, *value);
        } else {
            book.insert_account(id, *field, *value);
        }
    }
}

/// A book holding `pairs` as account row `id`'s credentials of `venue`.
pub fn book(venue: &str, id: i64, pairs: &[(&str, &str)]) -> CredentialBook {
    let mut out = CredentialBook::new();
    file(&mut out, venue, id, pairs);
    out
}

/// The view a policy-less probe is handed (`vike_mount::venue_arming_under` and its wrappers): the
/// fields `book` holds for account row `id` of `venue`, and the venue's own.
pub fn creds<'a>(book: &'a CredentialBook, venue: &str, id: i64) -> AccountCredentials<'a> {
    book.view(venue, Some(id))
}

/// The credential book `policy`'s directory holds, or an empty one.
pub fn book_of(policy: &MountPolicy) -> CredentialBook {
    match policy.accounts.credentials() {
        Some(Ok(book)) => book.clone(),
        _ => CredentialBook::new(),
    }
}

/// `policy` with `book` as its directory's credentials.
pub fn with_book(policy: MountPolicy, book: CredentialBook) -> MountPolicy {
    MountPolicy { accounts: policy.accounts.with_credentials(book), ..policy }
}

/// The `account.id` the next row [`MountPolicy::with_account`] plants takes: one above the highest
/// planted so far.
pub fn next_id(policy: &MountPolicy) -> i64 {
    let highest = match policy.accounts.rows() {
        Some(Ok(accounts)) => {
            accounts.known().and_then(|rows| rows.iter().map(|a| a.id).max()).unwrap_or(0)
        }
        _ => 0,
    };
    highest + 1
}

/// Plant `row` AND `pairs` as its credentials, keeping every credential already planted.
pub fn with_keyed_row(
    policy: MountPolicy,
    row: vike_secrets::Account,
    pairs: &[(&str, &str)],
) -> MountPolicy {
    let mut kept = book_of(&policy);
    file(&mut kept, &row.venue, row.id, pairs);
    with_book(policy.with_account_row(row), kept)
}

/// Plant one ACTIVE row `(venue, label)` at `tier` holding `pairs` — the account model's "this
/// account trades at `tier` with these credentials".
pub fn with_keyed_account(
    policy: MountPolicy,
    venue: &str,
    label: &AccountLabel,
    tier: AccountTier,
    pairs: &[(&str, &str)],
) -> MountPolicy {
    let id = next_id(&policy);
    let mut kept = book_of(&policy);
    file(&mut kept, venue, id, pairs);
    with_book(policy.with_account(venue, label, tier), kept)
}

/// A policy holding exactly one ACTIVE row — `venue`'s DEFAULT account at `tier` (id 1) — and
/// `pairs` as its credentials.
pub fn keyed_policy(venue: &str, tier: AccountTier, pairs: &[(&str, &str)]) -> MountPolicy {
    with_keyed_account(MountPolicy::default(), venue, &AccountLabel::Default, tier, pairs)
}
