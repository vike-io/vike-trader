//! The `account`-row fixture of `book_identity_table.rs` and `shared_book_report.rs`, each of which
//! `#[path]`-includes it (a bare `mod` would resolve beside the binary root, not here).

use vike_bridge_core::credentials::Account;

/// One ACTIVE `account` row. `book` is what the VENUE answered — or what an operator read off the
/// venue's own page and wrote with `vike-cli secrets set-book`.
pub fn acct(id: i64, venue: &str, tier: &str, label: Option<&str>, book: Option<&str>) -> Account {
    Account {
        id,
        venue: venue.to_string(),
        tier: tier.to_string(),
        label: label.map(str::to_string),
        venue_account_id: book.map(str::to_string),
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
    }
}
