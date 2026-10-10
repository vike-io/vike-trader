//! `profile_store` — **the daemon profile, its mount rows, and WHICH profile is live, as rows.**
//!
//! Phase 3 of `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`, built to the
//! schema `docs/superpowers/specs/2026-09-13-settings-store-schema-design.md` §4.4–4.8 prints, with
//! the two columns that spec deliberately did NOT ship added here because the owner has since ruled
//! on the question that was holding them:
//!
//! * `profile.active` — §4.4 ships without it (*"⚠ with NO `active` column"*) and hands §6's
//!   selection contradiction to the owner. **He ruled: the ROW wins.** [`select`] is that ruling,
//!   written once.
//! * `mount.is_primary` — nothing in §4.5 carries the primary at all, because in the file world it
//!   is the array's first element. **A table has no inherent order**, so reproducing today's
//!   behaviour from rows needs either an ordinal or a declaration, and 0057's *tradehub.toml*
//!   verdict says which of the two to take: *"the honest move is to declare the MEANING instead of
//!   the position"*.
//!
//! # ⚠ THE PROPERTY THIS WHOLE MODULE IS BUILT AROUND: absence and emptiness both resolve to today
//!
//! 0057's Question 3 states the hazard in one sentence — a migration that writes an active profile
//! row **ARMS A MOUNT that a missing line was holding on paper**. Every read path here is therefore
//! written so that three different states give the SAME answer, and that answer is the one this box
//! gives today:
//!
//! | state | [`read_profiles`] answers | what the daemon does |
//! |---|---|---|
//! | no database at all (`crate::Backend::Absent`) | [`Profiles::none`] | exactly what it does today |
//! | a database with no profile tables (every box migrated before this) | [`Profiles::none`] | exactly what it does today |
//! | a database whose profile tables hold BODIES and no `active` row | bodies, `active` `None` | exactly what it does today |
//!
//! Only the fourth state — a row that says `active = 1` — changes anything, and
//! [`plan_active_row`] is the one place that decides whether writing one is allowed to.
//!
//! # ⚠ THIS MODULE DOES NOT TOUCH `PRAGMA user_version`, AND THAT IS A SAFETY PROPERTY
//!
//! `crate::db`'s `SCHEMA_VERSION` carries the argument in full: a binary that meets a
//! `user_version` it does not know refuses the store, and
//! `vike_bridge_core::credentials::load_workspace_secrets_at` — the infallible wrapper every
//! composition root reaches through — turns that refusal into an EMPTY credential map, which is not
//! an error downstream but **the live gate**. Every venue silently drops to paper.
//!
//! Bumping the version for a table nothing has read yet would spend exactly that outage to announce
//! a feature. So [`profile_ddl`] is `CREATE TABLE IF NOT EXISTS` throughout and the stamp is left
//! alone: an OLD binary meeting a store these tables were added to reads its credentials unchanged,
//! because it selects from `credential` and `node_key` and has never asked what else is in the file.
//! What the version guards is the shape of the tables whose ABSENCE is the live gate, and this
//! module adds none of those.
//!
//! # Who may write
//!
//! [`OperatorWrite`] is required by every write function here, and
//! `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs` pins the files that may construct one. The
//! operator's CLI (`vike-cli`) is the writer; the GUI reaches the box only through a daemon. (This
//! said the CLI was the writer because it ran *"outside every daemon's mount namespace"* — a
//! location argument the paragraph below retires for the trading daemon.)
//!
//! ⚠ **This said "no shipped unit grants `settings/db`" and stopped being true on 2026-09-18**,
//! when the owner ruled the trading daemon must be able to write the database
//! (*"why doesn't the daemon have access to write to the db? it has to have access"*) and
//! `deploy/vike-tradehub.service` gained the grant. MEASURED over `deploy/*.service`: that unit is
//! the one that has it. `deploy/vike-datahub.service` — the daemon whose recorder profile this
//! module's [`ProfileKind::Recorder`] half serves — grants `settings/state/logs` and its data root
//! and nothing else, which is the measurement
//! `docs/decisions/0081-a-recorded-subscription-is-a-write-verb.md` rests on.
//!
//! **So the kernel is no longer the whole seal, and this type is load-bearing rather than
//! anticipatory** — [`OperatorWrite`]'s own doc predicted exactly this day and says what it now
//! carries.
//!
//! **NOTE — protection of a ceiling against a write by the daemon now rests on CODE, not on the
//! filesystem or the unit.** That daemon DOES write settings rows since
//! `docs/decisions/0086-settings-live-only-in-the-database.md` point 6 (2026-09-26), over
//! `WireCommand::SetSetting`, but that path plans only a `setting` or `venue_arming` row
//! (`crates/vike-secrets/src/settings/row_write.rs`'s `RowChange`) and so never reaches a table
//! here; what keeps a run profile's `[risk]` ceilings off it is [`OperatorWrite`] and the gate
//! above. What guards the `policy.*` and arming ceilings it CAN write is listed in
//! `crates/vike-secrets/src/settings.rs`'s module doc.

mod ddl;
mod error;
mod model;
mod profiles;
mod read;
mod render;
mod select;
mod write;

pub use ddl::{PROFILE_TABLES, profile_ddl};
pub use error::ProfileError;
pub use model::{
    MountRow, Primary, ProfileKind, ProfileRow, RecorderBody, RecorderRow, StoredProfile,
    SubscriptionRow,
};
pub use profiles::{ActiveProfile, Profiles};
pub use read::read_profiles;
pub use render::{
    render_daemon_toml, render_recorder_toml, render_run_toml, toml_basic_string, toml_string_array,
};
pub use select::{
    ActivePlan, Selected, SelectionSource, Shadowed, WithholdReason, plan_active_row, select,
};
pub use write::{OperatorWrite, clear_active, ensure_tables, set_active, store_profile};

#[cfg(test)]
mod ddl_tests;

#[cfg(test)]
mod profiles_tests;

#[cfg(test)]
mod render_tests;
