//! **Schema 2 — the account is a ROW.** The DDL of
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4 and §6, the derivation that
//! fills it, and the in-place reshape that carries a schema-1 store into it.
//!
//! Schema 1 was two flat `(name, value)` tables. What it could not say is the thing §1 of that spec
//! is about: WHICH ACCOUNT a credential belongs to. The account lived in the key NAME
//! (`DUKASCOPY_DEMO1_LOGIN` bakes an account INDEX into the tier token), so an account could go
//! stale with nothing in the store able to contradict it. Schema 2 gives the account a row, a
//! permanent opaque `id`, and a place for the venue's own answer (`venue_account_id`).
//!
//! # ⚠ What this module does NOT do, and why that is the spec's own rule rather than a shortcut
//!
//! §12 states it as a hard sequencing requirement rather than a follow-up:
//!
//! > Ruling 10 moves ten values out from under the readers that find them today; §6.2's ordering
//! > (A) pays for that with the renderer and ordering (B) with four bridge PRs, but NEITHER is
//! > written and until one is, **THE ROWS MAY NOT MOVE.**
//!
//! So §11's steps **3 and 4 are not performed here**. The ten book keys are NOT folded into
//! `account.venue_account_id` and the ten config-shaped keys are NOT moved to `venue_setting`;
//! every one of them stays a `credential` row carrying its legacy `name`, and
//! [`Classification::pending_move`] is how each one says so BY NAME in the migration's report. The
//! consequence is the property this whole change rests on: **`credential` still holds every live
//! name**, so [`crate::db::read_table`] answers with the same map before and after, and no renderer
//! is needed to make that true. `venue_setting` is created EMPTY for the same reason — a table is a
//! SHAPE, and a ROW nothing reads is the defect `vike_config::CONSUMPTION` exists to refuse.
//!
//! It also writes no `venue_account_id`, on either dukascopy row. See [`DDL`]'s `label` note.
//!
//! # The classification is INJECTED, exactly like `is_node_key`
//!
//! `field`, `account_id` and `venue` are derived from the key NAME, and the production
//! implementation of that derivation is `vike_bridge_core::credentials::classify_credential_name`
//! — a function of a crate this one **cannot name, in two independent ways**. `vike-bridge-core`
//! declares `layer = 25` where this crate declares `15`, and dependency direction is DOWN ONLY and
//! machine-checked (`crates/vike-ops/tests/arch/layer_gate.rs`); and that crate declares `vike-secrets`
//! as a normal dependency, so the reverse edge is a cycle as well as a layer violation. The
//! derivation therefore arrives as a closure returning [`Classification`] — the same SEAM that
//! [`crate::db::migrate`] already takes `is_node_key` through, though ⚠ **no longer for the same
//! reason**: that one lost its layer bound to 0072 and was ruled to stay on other grounds
//! (2026-09-26, in that function's own doc), while this one's bound is untouched. The heading
//! above says *exactly like* about the SHAPE; do not read it as a shared argument.
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
mod comments;
mod ddl;
mod rebuild;
mod refusal;
mod reshape;
mod resolver;
mod rows;
mod steps;
mod tiers;

pub use classify::{AccountKey, Classification, PendingMove, Placement};
pub use comments::{FileComments, scan_comments};
pub use ddl::DDL;
pub use refusal::SchemaRefusal;
pub use reshape::reshape_into;
pub use resolver::AccountResolver;
pub use rows::{RowReport, write_rows};
pub use tiers::{
    ACCOUNT_TIERS, PAPER_TIER, SIM_KEY_TOKEN, account_tier_named, account_tier_of_key_token,
    key_token_of_account_tier,
};

pub(crate) use rebuild::NamedRefusal;
pub(crate) use steps::any_tier::{column_is_nullable, migrate_venue_setting_tier_to_any};
pub(crate) use steps::autoincrement::migrate_tables_onto_autoincrement;
pub(crate) use steps::dropped_columns::{migrate_dropped_columns, text_venue_tables};
pub(crate) use steps::sim_to_paper::migrate_sim_tier_to_paper;
pub(crate) use steps::venue_links::{
    VENUE_LINK_TABLES, VenueLink, migrate_venue_links_onto_venue_id, venue_is,
};
pub(crate) use tiers::{stored_venue_setting_tier, venue_setting_tier_of_stored};

// ---------------------------------------------------------------------------------------------
// The vocabulary's own properties
// ---------------------------------------------------------------------------------------------

/// **§4.4's rename, held to its own terms** — the two maps, and the one fact this crate spells
/// twice. The MIGRATION's proof is `crates/vike-secrets/tests/migration/paper_tier.rs`, which drives a real
/// pre-rename store through a public writer; these are the pure halves.
#[cfg(test)]
mod tests;
