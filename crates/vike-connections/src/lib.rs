//! The venues x Sim/Demo/Live credential-status grid — the `vike-connections` tool: which venues
//! have API keys configured in the workspace `.env`, plus in-app editing of those keys, plus
//! (foundation, see [`vike_model::feed_status`]) a live per-venue connection-status column.
//!
//! `status` is the pure credential enumerator (unit-tested); the pure live connection-state model +
//! string parser ([`vike_model::feed_status::ConnectionState`]/
//! [`vike_model::feed_status::parse_feed_status`], unit-tested; binance-only source today, every
//! other venue reports `Unknown` until it gets a producer wired in) lives DOWN in
//! [`vike_model::feed_status`] and every consumer names it there — its other consumer,
//! `vike_ops::reconcile_config::health_from_feed_status`, is called by the HEADLESS daemon, which
//! may not reach through this (egui) crate to get at it, and this crate in turn no longer reaches
//! through `vike-ops` (and so `vike-core`/`vike-data`/`vike-alerting`) to get at 142
//! dependency-free lines; `env_write` is the pure `.env` upsert +
//! atomic write (unit/integration-tested, no secrets in tests); `summary` is the pure fold that
//! decides what a dot MEANS and what a count counts; `view` is the egui render (mounted by
//! `vike-desktop` as the Connections tool's Credentials tab) — a venue RAIL, a per-venue DETAIL
//! pane spelling every key name out, and the masked inline add/edit form. The panel never
//! displays an existing secret's plaintext (presence stays a MARK — ●/○ for the two measurements,
//! `·`/`?` for the two states that are not one); edit fields are masked
//! (`egui::TextEdit::password`) and start empty; no secret value is ever logged.
//! `crates/vike-connections/tests/connections_a11y.rs`'s
//! `no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor` is the
//! end-to-end proof of that sentence, over a store seeded with a real value.
//!
//! ⚠ **This crate reads two DIFFERENT facts and must never render them through one glyph.** The
//! rail's dots are CREDENTIAL PRESENCE in the store; the detail pane's `Status` row is the LIVE
//! FEED's connection state, which comes from a different producer and is absent for most of the
//! roster. [`summary`]'s module doc carries the argument and [`summary::FeedFact`] is the type
//! that keeps "no producer in this build" apart from "the producer has not reported".
//!
//! ⚠ **…and it renders a THIRD thing that is not a fact: a store nobody could open.** The
//! credential loader is infallible by design — an unopenable store logs and returns an EMPTY map,
//! indistinguishable from an absent one — so every count and every dot folded from it would be a
//! measurement of a file that was never read. [`StoreHealth`] is that input, [`TierState::Unknown`]
//! the per-cell answer and [`CredentialSummary::configured`] an `Option`. [`summary`]'s module doc
//! carries the rule this obeys.

//!
//! # More than one account per venue
//!
//! [`status::credential_status_for_account`] is the per-account read, and [`status::AccountGrids`]
//! is the ENUMERATION built on it: the default account's grid plus one per labelled account the
//! store holds. `view` renders one of them at a time, picked in a chip strip above the grid, and
//! composes every key name it writes through `vike_model::account_keys::account_key`. On a box with
//! no labelled account the enumeration is empty, the strip offers one chip, and every name is
//! returned unchanged — so that box's panel is the one that shipped.

pub mod env_write;
pub mod status;
pub mod summary;
pub mod view;

pub use env_write::{
    CredentialHome, CredentialWrite, edit_account_journalled, save_credentials_journalled,
};
pub use status::{
    AccountGrids, VenueCredStatus, credential_status, credential_status_for_account,
    credentialed_venues,
};
pub use summary::{
    CredentialSummary, FeedFact, StoreHealth, TIERS, TierState, credential_summary, tier_state,
};
pub use view::{connections_ui, key_family, shown_account};
