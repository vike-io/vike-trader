//! The venues x Paper/Demo/Live credential panel — the `vike-connections` tool: which venues' ACCOUNT
//! ROWS hold their credentials in the workspace credential store, in-app masked editing of those
//! credentials and of the account rows, and — in the detail pane, never in the rail's dots — a
//! per-venue LIVE feed-status row. The connection-state model itself lives DOWN in
//! [`vike_model::feed_status`]: the headless daemon reads it there and may not reach through this
//! (egui) crate to get at it.
//!
//! # A credential belongs to its ACCOUNT (decision 0156 item 6)
//!
//! The tier is written in ONE place, `account.tier`, by the operator. A credential is the store's
//! `credential.account_id` + `field`: a tier cell of this panel is the account row of
//! `(venue, the selected label, tier)`, its form writes that row's FIELDS, and no reader here
//! composes or parses a credential NAME. A cell with no account row offers no form — a credential
//! write never creates an account — and says how the row is created.
//!
//! # The modules, by role
//!
//! They depend on each other in one direction only: `keys` ← `status` ← `summary` ← `view`
//! (`env_write` stands beside them). A pure module never reaches up into the render module.
//!
//! * [`keys`] — PURE, no egui: the per-venue FORMS by FIELD ([`keys::form_fields`], read off
//!   `vike_venues::credential_schema`, plus the LIVE form's venue-wide attribution tail) and the
//!   [`keys::Sensitivity`] classifier, the security boundary that decides which stored values the
//!   editor may read back.
//! * [`status`] — PURE: the READ side. [`AccountGrids`] folds the store's account rows, their key
//!   names and their credentials (`vike_secrets::CredentialStore::accounts` / `account_keys` /
//!   `credential_book`) into one [`TierCell`] per (venue, label, tier), carrying the health of
//!   those reads.
//! * [`summary`] — the pure fold that decides what a dot MEANS and what a count counts
//!   ([`CredentialSummary`], [`TierState`], [`FeedFact`]).
//! * [`env_write`] — the editor's WRITE CONTEXT ([`CredentialHome`], [`CredentialWrite`]); the
//!   store writes themselves are `vike_secrets`' journalled writers.
//! * [`view`] — the egui render, mounted by `vike-desktop` as the Data Manager's Credentials
//!   destination: a venue RAIL (`view::rail`), a per-venue DETAIL pane spelling every field out
//!   with its presence (`view::detail`), the account strip (`view::strip`), and the masked inline
//!   form plus the account-row acts (`view::editor`). `view::editor` holds **every store write this
//!   panel can perform**, on purpose, so the workspace's credential-writer gate stays one row. The
//!   layout contract and the security contract are in `view`'s own module doc.
//!
//! The panel never displays an existing secret's plaintext (presence stays a MARK — ●/○ for the two
//! measurements, `·`/`?` for the two states that are not one); edit fields are masked
//! (`egui::TextEdit::password`) and start empty; no secret value is ever logged.
//! `crates/vike-connections/tests/panel/a11y_secrets.rs`'s
//! `no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor` is the
//! end-to-end proof of that sentence, over a store seeded with a real value.
//!
//! ⚠ **This crate reads two DIFFERENT facts and must never render them through one glyph.** The
//! rail's dots are CREDENTIAL PRESENCE in the store; the detail pane's `Status` row is the LIVE
//! FEED's connection state, which comes from a different producer and is absent for most of the
//! roster. [`summary`]'s module doc carries the argument and [`summary::FeedFact`] is the type
//! that keeps "no producer in this build" apart from "the producer has not reported".
//!
//! ⚠ **…and it renders a THIRD thing that is not a fact: a store nobody could open.** A store that
//! exists and would not open answers no rows and no fields — indistinguishable from an empty one —
//! so every count and every dot folded from it would be a measurement of a file that was never
//! read. [`AccountGrids::health`] carries the verdict of the very reads the grid was folded from,
//! [`TierState::Unknown`] is the per-cell answer and [`CredentialSummary::configured`] an `Option`.
//! [`summary`]'s module doc carries the rule this obeys.
//!
//! # More than one account per venue
//!
//! [`AccountGrids`] carries the default (unlabelled) account's grid plus one per LABEL the store's
//! account rows carry. `view` renders one of them at a time, picked in a chip strip above the grid.

#![warn(unreachable_pub)]

pub mod env_write;
pub mod keys;
pub mod status;
pub mod summary;
pub mod view;

pub use env_write::{CredentialHome, CredentialWrite};
pub use status::{
    AccountCell, AccountGrids, HeldFields, Presence, TierCell, VenueCredStatus, credentialed_venues,
};
pub use summary::{CredentialSummary, FeedFact, TIERS, TierState, credential_summary, tier_state};
pub use view::{connections_ui, field_family, shown_account};
