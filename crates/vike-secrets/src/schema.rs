//! **The account is a ROW.** The DDL of `docs/superpowers/specs/2026-09-14-the-credential-schema.md`
//! §4 and §6, and the derivation that fills it.
//!
//! What a flat `(name, value)` table cannot say is the thing §1 of that spec is about: WHICH
//! ACCOUNT a credential belongs to. The account lived in the key NAME (`DUKASCOPY_DEMO1_LOGIN`
//! bakes an account INDEX into the tier token), so an account could go stale with nothing in the
//! store able to contradict it. This schema gives the account a row, a
//! permanent opaque `id`, and a place for the venue's own answer (`venue_account_id`).
//!
//! # What this module does NOT do
//!
//! It re-files nothing (`docs/decisions/0117-there-are-no-migrations.md`). A book key
//! (`IG_DEMO_IDENTIFIER`, `POLY_FUNDER`, ...) stays a `credential` row under its own `name` and is
//! never folded into `account.venue_account_id`, so [`crate::db::read_table`] answers for every
//! name the store holds. It writes no `venue_account_id`, on either dukascopy row. See [`DDL`]'s
//! `label` note.
//!
//! # The classification is INJECTED
//!
//! `field`, `account_id` and `venue` are derived from the key NAME, and the production
//! implementation of that derivation is `vike_bridge_core::credentials::classify_credential_name`
//! — a function of a crate this one **cannot name, in two independent ways**. `vike-bridge-core`
//! declares `layer = 25` where this crate declares `15`, and dependency direction is DOWN ONLY and
//! machine-checked (`crates/vike-ops/tests/architecture/layer_gate.rs`); and that crate declares `vike-secrets`
//! as a normal dependency, so the reverse edge is a cycle as well as a layer violation. The
//! derivation therefore arrives as a closure returning [`Classification`].
//!
//! ⚠ **The reason above REPLACES the one this paragraph used to give, whose premise expired.** It
//! read *"**This crate declares no `vike-*` dependency** (`crates/vike-secrets/Cargo.toml`'s
//! layer-15 `leaf` tier, and two consumers rest on it)"*, and that is false since
//! `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
//! 2026-09-20): the manifest declares `vike-model` as a normal dependency and
//! `crates/vike-secrets/src/db/venues.rs`'s `ensure_venue_rows` iterates `vike_model::VENUES`
//! outright. ⚠ Read 0072's *"Layers 15 → 10, strictly down"* as the EDGE's direction rather than as
//! this crate moving: `crates/vike-secrets/Cargo.toml` still declares `layer = 15`, and `10` is
//! `vike-model`'s.
//!
//! ⚠ **That is not licence to collapse the seam.** 0072 admitted ONE edge, on the ground that it
//! costs nothing in any graph, and ruled nothing about injected seams. The layer bound above is why
//! this one stands whatever the manifest gains next — and it is a DIFFERENT argument from the one
//! that expired, not a restatement of it.

mod classify;
mod ddl;
mod refusal;
mod resolver;
mod rows;
mod tiers;
mod venue_link;

pub use classify::{AccountKey, Classification, Placement};
pub use ddl::DDL;
pub use refusal::SchemaRefusal;
pub(crate) use resolver::AccountResolver;
pub use rows::RowReport;
pub(crate) use rows::write_rows;
pub use tiers::{
    ACCOUNT_TIERS, PAPER_TIER, SIM_KEY_TOKEN, account_tier_named, account_tier_of_key_token,
    key_token_of_account_tier,
};

pub(crate) use tiers::{stored_venue_setting_tier, venue_setting_tier_of_stored};
pub(crate) use venue_link::{VenueLink, venue_is};

// ---------------------------------------------------------------------------------------------
// The vocabulary's own properties
// ---------------------------------------------------------------------------------------------

/// **§4.4's rename, held to its own terms** — the two maps, the one fact this crate spells twice,
/// and the `'any'` word's two boundaries.
#[cfg(test)]
mod tests;
