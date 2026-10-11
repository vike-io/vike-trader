//! **The settings rows** — `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`
//! Phase 1's store half, and the one place this crate knows anything about settings.
//!
//! ⚠ **This opened *"Phase 1 is MIRRORED: the store is written and the files still win"*, and that
//! stopped being true on 2026-09-26** (`docs/decisions/0086-settings-live-only-in-the-database.md`
//! points 1 and 4): there are no settings files any more, as source, fallback or export, so no row
//! is derived from one and nothing sits above the rows. `vike_config`'s loader reads the rows; a
//! key with no row resolves to its compiled-in default; a write changes exactly one row through
//! [`write_setting_row_in`]. What Phase 1 bought survives unchanged: the read-back path
//! (`vike-cli config show` answers with the daemon down and with no `sqlite3` binary on the box),
//! the provenance word, and a row materialising through the SAME typed patch that refuses an
//! unknown key.
//!
//! # ⚠ The boundary between the venue- and account-scoped homes, written down
//!
//! 0057 asked for this sentence before a second venue-scoped table was added, because one database
//! holds several of them with different shapes and owners, and *"the next author to add a venue
//! knob picks whichever table they read about last"*. The rule, and it is keyed on WHO OWNS the
//! value rather than on what it looks like:
//!
//! | home | holds | owner | may it ever widen a permission? |
//! |---|---|---|---|
//! | the `account` row's `tier` + `active` | the mode ONE account trades at: `active ? tier : paper` | the operator, through the account verbs (`crate::db::edit_account`) | **yes — it is the arming itself (decision 0119)** |
//! | [`account.max_exposure`](crate::Account::max_exposure) | ONE account's own exposure ceiling | the operator, through `AccountEdit::SetMaxExposure` | **no — it composes by `min` and can only ever REFUSE** |
//! | `venue_setting` | a BRIDGE's operational configuration, machine- or tier-scoped | the venue adapter | not an arming question at all |
//! | [`setting`](crate::schema::DDL) | anything keyed by a settings SECTION and a dotted key | `vike_config`'s four typed sections | `config`/`preferences`/`flags` only; `policy` rows are sealed by the TYPE |
//! | [`profile_risk`](crate::schema::DDL) | one RUN PROFILE's `[risk]` table, one row per key | a run-profile FILE, copied by the retired `[risk]` mirror (no binary reads a run-profile file since decision 0111) | **it widens nothing, because nothing reads it** — see below |
//!
//! So: **a per-account mode or a per-account ceiling is a column of that account's row; a knob a
//! bridge reads to talk to a venue at all is `venue_setting`; anything an operator names as
//! `<section>.<dotted.key>` is a `setting` row; anything inside a RUN PROFILE's `[risk]` table is a
//! `profile_risk` row.** The test to apply when a new knob is ambiguous: if the value composes by
//! `min` with another layer and can never raise anything, it is a ceiling. If it configures rather
//! than bounds, it is not.
//!
//! The `profile_risk` row is the one whose OWNER decides it rather than its shape: a `[risk]` key is
//! a pre-trade ceiling, so it looks exactly like an account ceiling and is not one. An account row is
//! a property of the BOX, which no `--profile` flag swaps; a `[risk]` key travels with the RUN.
//! `vike_config::ceilings::PRE_TRADE_CEILINGS`' `CeilingHome` is that split already written down.
//!
//! ⚠ An older store still holds a `venue_arming` table and an `account.armed` column. Nothing here
//! reads, writes or drops either (see [`crate::schema::DDL`]'s note).
//!
//! # ⚠ `profile_risk` is a DISCLOSURE copy, and that is the whole of what Phase 2 is
//!
//! 0057's Phase 2 is *"the ceilings become readable from `config show` with the daemon down"*, and
//! it buys exactly that and nothing else. **No mount, no core, no engine and no binary that signs
//! an order reads these rows**, so a `profile_risk` row cannot arm a venue, cannot raise a ceiling
//! and cannot satisfy `vike_mount::require_live_risk_budget` — which is the refusal that stops a box
//! starting live without `max_notional_per_order` and `max_total_exposure`.
//!
//! ⚠ **This paragraph also said a profile FILE is the only thing that builds a
//! `vike_model::ProfileRisk`, and that is no longer true.** The mirror is RETIRED: a run profile's
//! whole body lives on the profile plane (`crate::profile_store`), and `vike-tradehub` builds its
//! `ProfileRisk` from the ACTIVE `run` row and from nothing else — no file, since decision 0111
//! (`crates/vike-tradehub/src/tradehub_cli/profile.rs`, through
//! `crates/vike-tradehub/src/profile_rows.rs`'s `rows_to_run_profile`). So run-profile ceilings
//! bind from the database — on that plane, never from this table.
//!
//! That property is a claim about the whole tree rather than about this module, so it is held by a
//! gate that scans the tree: `crates/vike-ops/tests/settings_secrets/profile_risk_readers_gate.rs` pins the files
//! that may call [`read_profile_risk`] and [`read_profile_risk_in`], with the reason each may.
//! A future phase that gives the rows a READER has to edit that pin, which is the point — it makes
//! the widening an act somebody performs rather than one that happens.
//!
//! **What WRITES them: nothing in production.** [`write_profile_risk`] survives with no caller
//! outside this crate — `crates/vike-ops/tests/settings_secrets/profile_risk_readers_gate.rs`'s `WRITERS`
//! is that statement as a measurement — because `vike-cli config mirror --profile`, its one writer,
//! now stores the run profile's whole BODY on the profile plane instead.
//!
//! ⚠ **This paragraph used to argue that a live pre-trade ceiling could be changed only by an
//! operator shell OUTSIDE the daemon's mount namespace** — the daemons' settings directory
//! read-only, *"no `.service` under `deploy/` grants `settings/db`"*, no control-channel verb — **and
//! every premise of that argument is gone:**
//!
//! * `deploy/vike-tradehub.service`'s `ReadWritePaths=` names `<settings>/db` since the owner's
//!   ruling of 2026-09-18 (*"it has to have access"*), and its own comment block says that the same
//!   file holds the `policy` ceiling rows, so a filesystem grant cannot guard them.
//! * `docs/decisions/0086-settings-live-only-in-the-database.md` point 6 (2026-09-26) REVERSED the
//!   rule that the daemon writes no settings row: an accepted `WireCommand::SetSetting` reaches
//!   `crates/vike-tradehub/src/server/settings.rs`'s `apply_set_setting`, which calls
//!   `vike_config::write_setting_row` — the same one-row planner `vike-cli config set` calls — so an
//!   authenticated peer can change a `policy.*` ceiling without an SSH session. Point 7 deleted the
//!   retype confirm.
//!
//! The section below states what guards those writes now.
//!
//! ⚠ **`setting` carries no precedence column, and must never acquire one.** See
//! [`crate::schema::DDL`]'s own note: the seal that makes a ceiling a ceiling is a property of
//! `vike_config`'s TYPE — `Policy` implements neither of that crate's two sealed override traits,
//! so an override is a compile error — and a `precedence` column here would put that seal one
//! `UPDATE` away from gone.
//!
//! # ⚠ This module hands out ROWS, never a handle
//!
//! 0057 names the hazard explicitly: `vike-config` declares its edge to this crate with the words
//! *"this crate never opens the store, and must not"*, and under one database a `vike-config` that
//! opened the store to read settings would hold a handle that also reaches the `credential` table —
//! from the crate whose boot disclosure goes into a file shipped with bug reports. So the split is
//! the one 0057 offers first: **`vike-secrets` hands `vike-config` only the settings rows.**
//! [`StoredSettings`] is plain data with no connection in it, `vike-config` takes it as a PARAMETER
//! exactly as it takes the environment map, and the widening is not argued because it is not taken.
//!
//! # Reads need no grant; writes come from the operator's CLI AND the daemon's control channel
//!
//! [`read_settings`] opens READ-ONLY through the same opener the credential read uses, which is why
//! the entire read half is reachable on a deployed box with no unit change — 0057's most
//! under-stated fact.
//!
//! ⚠ **This heading said *"writes are an operator act"* and named [`write_settings`] as reached
//! from `vike-cli`, an operator shell OUTSIDE the daemon's mount namespace. Both halves are
//! obsolete.** [`write_settings`] — the whole-table replace — has no production caller since 0086
//! deleted `config mirror`'s settings half; it **refuses a project with no database** rather than
//! creating one (see [`crate::DbErrorKind::NoSettingsDatabase`], where the reason is the live gate)
//! and plants rows for tests. The production writer is [`write_setting_row_in`], reached through
//! `vike_config::write_setting_row` from `vike-cli config set` on the box AND from the trading
//! daemon's own control channel (0086 point 6, above), and it touches exactly one row in `setting`
//! (plus the seal it moves) — never a profile table, never an `account` row and never
//! `profile_risk`.
//!
//! **NOTE — protection of a ceiling against a write by the daemon now rests on CODE, not on the
//! filesystem or the unit.** What a `policy.*` write over the wire must pass,
//! each piece pointable-to:
//!
//! 1. **The handshake's scope.** Only a peer authenticated at `Scope::Write` reaches the command arm,
//!    and a node holding no control key refuses that scope outright
//!    (`crates/vike-tradehub/src/server/handshake.rs`'s `scope_admission`).
//! 2. **The one acceptance path**, `crates/vike-tradehub/src/server/control.rs`'s `accept_command`:
//!    the per-connection rate token (`ControlLimits::vet`), then the rationale sanitizer, then the
//!    write, then the audit record.
//! 3. **The row validator**, `crates/vike-config/src/write.rs`'s `validate_row_write`, run inside the
//!    write's own transaction: a store that claims a seal must already boot clean (a write must not
//!    re-bless an erased ceiling — `crates/vike-config/src/mirror.rs`'s `seal_refusal`), and the
//!    candidate must resolve through the loader and change no key but the one named. Any refusal
//!    rolls back and leaves the database byte-identical. Before the database is opened at all, the
//!    same planner refuses a credential-shaped key (`crates/vike-config/src/write.rs`'s
//!    `refuse_credential_key`), so a credential cannot travel as a settings row over the wire.
//! 4. **No hot apply.** `crates/vike-tradehub/src/hot_reload.rs`'s `classify` answers `Restart` for
//!    every `policy` key before its table is consulted, so a written ceiling binds at the next boot,
//!    never in the running process.
//! 5. **The audit trail.** `crates/vike-tradehub/src/audit.rs`'s `record_settings_write` records the
//!    key and *old → new* to the log line and to the change journal, whose row names the
//!    fingerprint of the key that authenticated. A REFUSED wire write is recorded nowhere durable,
//!    which `accept_command` declares at its own site.
//!
//! What does NOT guard it: the filesystem sandbox (the unit grants `settings/db`), any unit-level
//! restriction, and any "there is no verb" argument (there is one). Nothing here distinguishes a
//! stolen control key from its owner — the residual
//! `docs/decisions/0068-the-ceiling-bounds-mistakes-not-an-adversary.md` names and leaves open.
//!
//! A run profile's `[risk]` ceilings are NOT reachable by that verb: `RowChange` has only the
//! `Setting` shape, and the profile plane's writers need
//! `crate::profile_store::OperatorWrite`, whose constructing files
//! `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs` pins under `crates/vike-cli/src/` — a
//! gate, not the kernel, for the trading daemon.

use std::path::{Path, PathBuf};

use crate::db::{DbError, DbErrorKind, database_present, open_for_read, open_for_write};

mod model;
mod probe;
mod profile_risk;
mod read;
mod row_write;
mod venue_setting_write;

pub use model::{
    Adoption, SETTINGS_SECTIONS, SettingRow, SettingsSource, SettingsWritten, StoredSettings,
    VenueSettingRow, section_is_known,
};
pub use profile_risk::{
    ProfileRiskRow, ProfileRiskSource, ProfileRiskWritten, StoredProfileRisk, read_profile_risk,
    read_profile_risk_in, write_profile_risk, write_profile_risk_in,
};
pub use read::{read_settings, read_settings_in};
pub use row_write::{
    RowChange, RowWriteError, RowWritten, write_setting_row_in, write_settings, write_settings_in,
};
#[cfg(feature = "test-support")]
pub use row_write::{hold_write_lock, plant_settings_rows};
pub use venue_setting_write::{
    VenueSettingRefusal, set_venue_setting_in, set_venue_setting_in_journalled,
};

pub(crate) use probe::has_column;

use probe::table_exists;
use read::{read_adoption, read_rows};

#[cfg(test)]
mod profile_risk_tests;
#[cfg(test)]
mod row_write_tests;

/// **The `venue_setting` table survives a MIRROR, and nothing else in this module does.**
///
/// ⚠ This is the most expensive thing in the file to get wrong, and the shape of the code argues
/// FOR the mistake: [`write_settings`] clears `setting` and re-inserts it from the caller's
/// rows, so a second `DELETE FROM venue_setting` beside it reads as the obvious
/// tidy-up. It would not be one. Venue settings have no file to be re-derived FROM — that is the
/// whole reason they are a table instead of a `config` key — so the delete would simply remove
/// them: one whole-table write and a box loses its JForex server, its IBKR gateway host and its
/// polymarket egress, silently, with the call reporting success.
#[cfg(test)]
mod mirror_tests;

/// **The operator's writer for a moved venue setting**, and the DDL constraint that is the whole
/// reason these values are a table rather than a field on `Config`.
#[cfg(test)]
mod venue_setting_write_tests;
