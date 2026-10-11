//! **Which venue BOOK does one account actually trade?** — the per-venue capability table behind
//! the shared-book warning `vike_config::shared_books` states the rule for.
//!
//! # Why a table and not a match arm
//!
//! No defensible default: an EIP-712 venue derives a wallet address from the stored private key
//! (offline, exact); a key/secret venue's store names no account. A `_ => None` arm would make a
//! forgotten venue look like one where "cannot tell" is the truth. So: one row per
//! `vike_model::VENUES` id (like `vike_model::venues::venue_tif`), citing its loader, with a
//! completeness test (`crates/vike-tradehub/tests/book_identity_table.rs`). The `BOOK_IDENTITY`
//! rows live in each bridge's `VenueDeclaration::book_identity` (docs/decisions/0096);
//! [`book_identity_for`] reads the registry.
//!
//! ⚠ Every row's key names come from that venue's OWN loader, tier tokens included: alpaca spells
//! its demo tier `SANDBOX` and aster its non-live tier `TESTNET`; a row assuming `DEMO` reads no
//! key, answers `None`, and silently downgrades a determinable venue to "cannot tell".
//!
//! # ⚠ What a row may name, and the family this table deliberately declines
//!
//! A [`BookIdentity::Named`] row may only name keys whose VALUES are safe in a log line (an
//! address, a login, a numeric account id): fingerprinting the API SECRET would put a secret one
//! `{:?}` away from a log. **A declared residual:** on binance/bybit/okx/deribit two labels holding
//! the SAME key pair are one book and nothing is said — a duplicate CREDENTIAL, narrower than the
//! agent-key case the warning exists for, and visible to the credential editor.
//!
//! # ⚠ Where the book cannot be determined, NOTHING is said
//!
//! [`effective_book`] answers `None`, `vike_mount::make_engine_accounts` builds no
//! `vike_config::ArmedBook`, and no pair forms: an unprovable suspicion is not a finding, and a
//! placeholder would compare equal to every other unknown and manufacture a pair.
//!
//! ## ⚠ …but an `Undeterminable` ROW is NOT the last word on that venue any more
//!
//! This table answers OFFLINE, but [`book_of_account`] reads [`recorded_book`]
//! (`vike_secrets::Account::venue_account_id`) FIRST, so such a venue still forms an `ArmedBook`
//! once `vike-cli secrets set-book` or a mount handshake ([`confirmation_for_account`] →
//! `vike_model::accounts::account_confirmation::park` → `vike-cli secrets confirm`) recorded what
//! the venue answered. `Undeterminable` means "the store's KEYS do not name it", not "never a
//! shared book" (`vike_mount::shared_books_for`'s doc).

use std::collections::HashMap;

use vike_bridge_core::account_directory::AccountDirectory;
use vike_bridge_core::credentials::account_var;
use vike_bridge_core::venue_mount::BookIdentity;
use vike_config::VenueMode;
use vike_model::accounts::account_keys::AccountLabel;

mod record;
pub use record::{confirmation_for_account, record_authenticated_account, record_confirmation};

/// The [`BookIdentity`] declared for `venue`, from its registry row — or `None` for a string the
/// registry does not carry (a test's `"sim"`, a paper id: no loader, one default account).
#[must_use]
pub fn book_identity_for(
    registry: &'static [crate::VenueRow],
    venue: &str,
) -> Option<BookIdentity> {
    match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => Some(row.declaration().book_identity),
        // No bridge in this build, so no declaration to read: the generic answer.
        Some(crate::VenueRow::FeatureAbsent { .. }) => Some(crate::registry::ABSENT_BOOK),
        None => None,
    }
}

/// **The canonical form every book identity is compared in**: trimmed, lowercased ASCII. Both
/// shapes are case-insensitive (an EVM address; an IBKR `DUQ186573` is `duq186573`). Normalizing at
/// the PRODUCER keeps `vike_config::shared_books` a plain string comparison with no second copy.
#[must_use]
pub fn normalize_book(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// **THE offline resolution: which book does account `label` of `venue` trade at its resolved
/// tier?** `None` = "not determinable from this store", which emits no warning. `mode` is the
/// RESOLVED tier (`account_arming_under`'s answer), not the account's, so the keys read are the ones
/// the mount signs with: [`VenueMode::Paper`] answers `None` (no venue book); [`VenueMode::Demo`] /
/// [`VenueMode::Live`] select the row's own tier tokens, in order.
///
/// ⚠ **Only the resolved tier's keys are read.** A store holding BOTH a demo and a live key set has
/// two books; the union would pair A's live book with B's demo book — two different ledgers.
#[must_use]
pub fn effective_book(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    vars: &HashMap<String, String>,
) -> Option<String> {
    if mode == VenueMode::Paper {
        return None;
    }
    let BookIdentity::Named { prefix, demo_tiers, live_tiers, name_suffixes, evm_key_suffixes } =
        book_identity_for(registry, venue)?
    else {
        return None;
    };
    let tiers = if mode == VenueMode::Live { live_tiers } else { demo_tiers };
    // The NAMED key first (the venue's own statement), then the derivation (an inference).
    for tier in tiers {
        for suffix in name_suffixes {
            if let Some(v) = account_var(vars, &format!("{prefix}_{tier}_{suffix}"), label) {
                return Some(normalize_book(v));
            }
        }
    }
    for tier in tiers {
        for suffix in evm_key_suffixes {
            let Some(key) = account_var(vars, &format!("{prefix}_{tier}_{suffix}"), label) else {
                continue;
            };
            // An unparseable key is not a finding: signer construction refuses it anyway.
            if let Ok(addr) = vike_bridge_core::eip712::eth_address_from_private_key(key) {
                return Some(normalize_book(&addr));
            }
        }
    }
    None
}

/// **The book the STORE has RECORDED for one account** — `vike_secrets::Account::venue_account_id`,
/// from the composition root's snapshot ([`crate::MountPolicy::accounts`]). The half
/// [`effective_book`] cannot answer: for the five [`BookIdentity::Undeterminable`] venues the
/// credential store holds nothing to derive FROM (an HMAC key or a login names no account).
///
/// # The join is `(venue, tier, label)`, and every term is load-bearing
///
/// * **`venue`** — the only term the row states in the shape the caller holds it.
/// * **`tier`** — demo and live accounts of one venue are TWO books. `mode` is the RESOLVED tier;
///   [`VenueMode::as_str`] spells all three words as `vike_secrets::schema::ACCOUNT_TIERS` does.
/// * **`label`** — `vike_model::accounts::account_keys::AccountLabel::text` is `None` for the
///   default account and `account.label` is `NULL`, so they compare equal. ⚠ A `NULL` label matches
///   the DEFAULT account and nothing else: a labelled account never gets the default's book.
///
/// # ⚠ More than one candidate answers `None`, and that is the dukascopy case
///
/// dukascopy's `(dukascopy, demo, NULL)` pair matches twice under the default label; only the
/// credential-key OWNER PREFIX separates them, which `crates/bridges/dukascopy/src/account.rs`'s
/// `resolve_account` reads and this general resolver does not restate. So the caller falls through
/// to the derivation — the first row's book would file one broker's account number against the
/// other's. A still-`None` column contributes nothing either: *not yet known* is not *no book*.
#[must_use]
pub fn recorded_book(
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    directory: &AccountDirectory,
) -> Option<String> {
    let row = recorded_row(venue, label, mode, directory)?;
    // ⚠ NORMALIZED, not cosmetic: `vike_secrets::normalized_venue_account_id` preserves CASE, so a
    // stored `0xAbC` and a derived `0xabc` are one account, and `vike_config::shared_books` is a
    // plain string comparison.
    row.venue_account_id.as_deref().map(normalize_book)
}

/// **THE book resolution the mount reports on: what was RECORDED, else what can be DERIVED.**
///
/// [`recorded_book`] first, [`effective_book`] second, and the order is the point: a recorded id is
/// what the VENUE said (a handshake) or an OPERATOR read off the venue (`vike-cli secrets
/// set-book`); a derivation is an INFERENCE, and where they can disagree the inference is wrong — a
/// hyperliquid agent key with no `_ACCOUNT_ADDRESS` derives the AGENT's address
/// (`crates/bridges/hyperliquid/src/user_role.rs`'s module doc). A disagreement is the writer's
/// finding, not this reader's pick: `vike_secrets::set_venue_account_id` REFUSES to overwrite a
/// different book (`DbErrorKind::BookAlreadyKnown`).
#[must_use]
pub fn book_of_account(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    vars: &HashMap<String, String>,
    directory: &AccountDirectory,
) -> Option<String> {
    recorded_book(venue, label, mode, directory)
        .or_else(|| effective_book(registry, venue, label, mode, vars))
}

/// **THE JOIN** — the one `account` row that IS account `label` of `venue` at the tier it resolved
/// to, or `None` when no single row can be said to be it.
///
/// ONE join for the BOOK ([`recorded_book`]) and the row `id` ([`confirmation_for_account`]'s
/// `observed_row`, the `row` [`record_confirmation`] logs beside its `set-book --id` line), so the
/// line an operator runs cannot address a different row from the one the mount read. The terms
/// are argued at [`recorded_book`]; **more than one candidate answers `None`** (dukascopy's pair).
fn recorded_row<'a>(
    venue: &str,
    label: &AccountLabel,
    mode: VenueMode,
    directory: &'a AccountDirectory,
) -> Option<&'a vike_bridge_core::credentials::Account> {
    // A paper account touches no venue book: a recorded column outlives a demotion to paper and
    // must not be reported as a book being traded.
    if mode == VenueMode::Paper {
        return None;
    }
    let rows = directory.rows()?.ok()?.known()?;
    let tier = mode.as_str();
    let mut matched = rows.iter().filter(|r| {
        r.active && r.venue == venue && r.tier == tier && r.label.as_deref() == label.text()
    });
    let row = matched.next()?;
    if matched.next().is_some() {
        return None;
    }
    Some(row)
}

#[path = "book_identity_tests.rs"]
#[cfg(test)]
mod book_identity_tests;
