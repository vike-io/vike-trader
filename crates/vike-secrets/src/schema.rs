//! **The account is a ROW.** The DDL of `docs/superpowers/specs/2026-09-14-the-credential-schema.md`
//! §4 and §6, the tier's column conversions, and the one spelling of a row's venue link.
//!
//! What a flat `(name, value)` table cannot say is the thing §1 of that spec is about: WHICH
//! ACCOUNT a credential belongs to. This schema gives the account a row, a permanent opaque `id`,
//! a tier column the operator writes (decision 0156 item 6: the ONE place the tier is written), and
//! a place for the venue's own answer (`venue_account_id`). A credential row names its account
//! (`credential.account_id`) and its `field`; its `name` is a LABEL that nothing reads a tier,
//! an account or a field out of.
//!
//! # What this module does NOT do
//!
//! It re-files nothing (`docs/decisions/0117-there-are-no-migrations.md`), and it derives nothing
//! from a credential NAME: the classifier that once filed a new name under the account its tier
//! token implied — and minted the account row when none answered — is gone with decision 0156. An
//! account is created by the operator, with its tier (`crate::AccountEdit::Create`), and a
//! credential is written against it by id (`crate::save_owned_credentials`).

mod ddl;
mod tiers;
mod venue_link;

pub use ddl::DDL;

pub(crate) use tiers::{stored_venue_setting_tier, tier_from_column, venue_setting_tier_of_stored};
pub(crate) use venue_link::{VenueLink, venue_is};

// ---------------------------------------------------------------------------------------------
// The vocabulary's own properties
// ---------------------------------------------------------------------------------------------

/// The tier column's conversions and the `'any'` word's two boundaries.
#[cfg(test)]
mod tests;
