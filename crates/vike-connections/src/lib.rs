//! The venues x Sim/Demo/Live credential-status panel — the `vike-connections` tool: which venues
//! have API keys configured in the workspace credential store, in-app masked editing of those keys
//! and of the account rows, and — in the detail pane, never in the rail's dots — a per-venue LIVE
//! feed-status row. The connection-state model itself lives DOWN in [`vike_model::feed_status`]:
//! the headless daemon reads it there and may not reach through this (egui) crate to get at it.
//!
//! # The modules, by role
//!
//! They depend on each other in one direction only: `keys` ← `status` ← `summary` ← `view`
//! (`env_write` stands beside them). A pure module never reaches up into the render module.
//!
//! * [`keys`] — PURE, no egui: the per-venue WRITE tables (which key names each tier cell's form
//!   composes: `expected_key_name`, `edit_fields`, `account_fields`) and the [`keys::Sensitivity`]
//!   classifier, the security boundary that decides which stored values the editor may read back.
//! * [`status`] — PURE: the READ side, which keys light a dot. `status::arms` holds one arm per venue
//!   whose bridge reads a bespoke key shape (each reads the exact names that bridge's own loader
//!   reads), `status::accounts` the [`AccountGrids`] enumeration. What a form WRITES ([`keys`]) and
//!   what a dot READS (`status::arms`) are the same facts spelled twice ON PURPOSE, held together by
//!   two independent frozen baselines under `tests/key_shapes/`: unifying them would remove the
//!   independence those tests compare against.
//! * [`summary`] — the pure fold that decides what a dot MEANS and what a count counts
//!   ([`CredentialSummary`], [`TierState`], [`FeedFact`]).
//! * [`env_write`] — the editor's WRITE CONTEXT ([`CredentialHome`], [`CredentialWrite`]); the safe
//!   store upsert itself is `vike_secrets::save_credentials`.
//! * [`view`] — the egui render, mounted by `vike-desktop` as the Connections tool's Credentials tab:
//!   a venue RAIL (`view::rail`), a per-venue DETAIL pane spelling every key name out
//!   (`view::detail`), the account strip (`view::strip`), and the masked inline form plus the
//!   account-row acts (`view::editor`). `view::editor` holds **every store write this panel can
//!   perform**, on purpose, so the workspace's credential-writer gate stays one row. The layout
//!   contract and the security contract are in `view`'s own module doc.
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
//! [`keys`] composes every key name it writes through
//! `vike_model::accounts::account_keys::account_key`. On a box with no labelled account the
//! enumeration is empty, the strip offers one chip, and every name is returned unchanged — so that
//! box's panel is the one that shipped.

pub mod env_write;
pub mod keys;
pub mod status;
pub mod summary;
pub mod view;

pub use env_write::{CredentialHome, CredentialWrite};
pub use status::{
    AccountGrids, VenueCredStatus, credential_status, credential_status_for_account,
    credentialed_venues,
};
pub use summary::{
    CredentialSummary, FeedFact, StoreHealth, TIERS, TierState, credential_summary, tier_state,
};
pub use view::{connections_ui, key_family, shown_account};
