//! **The settings rows** — `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`
//! Phase 1's store half, and the one place this crate knows anything about settings.
//!
//! Phase 1 is MIRRORED: the store is written and **the files still win**. Nothing here changes an
//! effective value on any box — the rows are DERIVED from the files by `vike_config::mirror`, and
//! `vike_config`'s loader applies them BELOW the file layer. What the mirror buys before the files
//! retire is the read-back path (`vike-cli config show` can answer with the daemon down and with no
//! `sqlite3` binary on the box), the provenance word, and a proof that a row materialises back
//! through the SAME typed patch that refuses an unknown key in a file.
//!
//! # ⚠ The boundary between the three venue-scoped tables, written down
//!
//! 0057 asked for this sentence before a second venue-scoped table was added, because one database
//! now holds three of them with three shapes and three owners, and *"the next author to add a venue
//! knob picks whichever table they read about last"*. The rule, and it is keyed on WHO OWNS the
//! value rather than on what it looks like:
//!
//! | table | holds | owner | may it ever widen a permission? |
//! |---|---|---|---|
//! | [`venue_arming`](crate::schema::DDL) | an arming CEILING — one row per roster venue, plus one per labelled account | POLICY (the `policy.venues.<venue>` / `policy.accounts.<venue>.<LABEL>` rows) | **no — it can only ever REFUSE** |
//! | `venue_setting` | a BRIDGE's operational configuration, machine- or tier-scoped | the venue adapter | not an arming question at all |
//! | [`setting`](crate::schema::DDL) | anything keyed by a settings SECTION and a dotted key | `vike_config`'s four typed files | `config`/`preferences`/`flags` only; `policy` rows are sealed by the TYPE |
//! | [`profile_risk`](crate::schema::DDL) | one RUN PROFILE's `[risk]` table, one row per key | the run profile `VIKE_RUN_PROFILE` / `--profile` names | **it widens nothing, because nothing reads it** — see below |
//! | [`account.armed`](crate::Account::armed) | the same decision, PER ACCOUNT ROW | DERIVED from `venue_arming` by [`fold_arming_into_accounts`] | **no — it is a projection of the row above, and nothing reads it yet** |
//!
//! ⚠ **The last row is not a fifth table and is not a second AUTHORITY.** It is `venue_arming`
//! resolved onto the rows it arms, recomputed on every write — the spec's §9 stage 3. The table
//! above it is still the source, still what `vike_config::apply_rows` builds a `VenuePolicy` from,
//! and still the only home for an arming decision naming a venue that has no account row.
//!
//! So: **a knob that can make a venue do MORE is an arming row and belongs in `venue_arming`; a
//! knob a bridge reads to talk to a venue at all is `venue_setting`; anything an operator names as
//! `<section>.<dotted.key>` in one of the four settings files is a `setting` row; anything inside a
//! RUN PROFILE's `[risk]` table is a `profile_risk` row.** The test to
//! apply when a new knob is ambiguous is the one `vike_config::VenueMode::cap` encodes:
//! if the value composes by `min` with another layer and can never raise anything, it is an arming
//! ceiling. If it configures rather than bounds, it is not.
//!
//! The fourth row is the one whose OWNER decides it rather than its shape: a `[risk]` key is a
//! pre-trade ceiling, so it looks exactly like an arming ceiling and is not one. `venue_arming`
//! is a property of the BOX, written as `policy` rows, which no `--profile` flag swaps; a `[risk]`
//! key travels with the RUN. `vike_config::ceilings::PRE_TRADE_CEILINGS`'
//! `CeilingHome` is that split already written down, and this table simply gives its second home a
//! carrier.
//!
//! # ⚠ `profile_risk` is a DISCLOSURE copy, and that is the whole of what Phase 2 is
//!
//! 0057's Phase 2 is *"the ceilings become readable from `config show` with the daemon down"*, and
//! it buys exactly that and nothing else. **No mount, no core, no engine and no binary that signs
//! an order reads these rows.** The file `vike_core::resolve_profile` opens is still the only thing
//! that builds a `vike_exec::ProfileRisk`, so a row cannot arm a venue, cannot raise a ceiling and
//! cannot satisfy `vike_mount::require_live_risk_budget` — which is the refusal that stops a box
//! starting live without `max_notional_per_order` and `max_total_exposure`. Mirroring a profile
//! changes no verdict about any order.
//!
//! That property is a claim about the whole tree rather than about this module, so it is held by a
//! gate that scans the tree: `crates/vike-ops/tests/settings/profile_risk_readers_gate.rs` pins the files
//! that may call [`read_profile_risk`] and [`read_profile_risk_in`], with the reason each may.
//! A future phase that gives the rows a READER has to edit that pin, which is the point — it makes
//! the widening an act somebody performs rather than one that happens.
//!
//! **What WRITES them:** [`write_profile_risk`], reached from `vike-cli config mirror --profile`,
//! i.e. an operator shell OUTSIDE the daemon's mount namespace. Both shipped daemons have the
//! settings directory read-only in their own namespaces (MEASURED: no `.service` under `deploy/`
//! grants `settings/db`), the GUI has no writer for them, and the control channel has no verb. So
//! the set of things that can change a live pre-trade ceiling is exactly what it was before this
//! table existed: an editor, on the profile FILE.
//!
//! ⚠ **`venue_arming` carries no precedence column, and must never acquire one.** See
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
//! # Reads need no grant; writes are an operator act
//!
//! [`read_settings`] opens READ-ONLY through the same opener the credential read uses, which is why
//! the entire read half is reachable on a deployed box with no unit change — 0057's most
//! under-stated fact. [`write_settings`] is reached from `vike-cli` (an operator shell OUTSIDE the
//! daemon's mount namespace) and **refuses a project with no database** rather than creating one:
//! see [`crate::DbErrorKind::NoSettingsDatabase`], where the reason is the live gate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::db::{DbError, DbErrorKind, database_present, open_for_read, open_for_write};

mod arming;
mod model;
mod profile_risk;
mod read;
mod row_write;
mod venue_setting_write;

pub use model::{
    Adoption, ArmingRow, SETTINGS_SECTIONS, SettingRow, SettingsSource, SettingsWritten,
    StoredSettings, VenueSettingRow, section_is_known,
};
pub use profile_risk::{
    ProfileRiskRow, ProfileRiskSource, ProfileRiskWritten, StoredProfileRisk, read_profile_risk,
    read_profile_risk_in, write_profile_risk, write_profile_risk_in,
};
pub use read::{clear_adoption, read_settings, read_settings_in, write_adoption};
pub use row_write::{
    RowChange, RowWriteError, RowWritten, write_setting_row_in, write_settings, write_settings_in,
};
#[cfg(feature = "test-support")]
pub use row_write::{hold_write_lock, plant_settings_rows};
pub use venue_setting_write::{
    VenueSettingRefusal, set_venue_setting_in, set_venue_setting_in_journalled,
};

pub(crate) use arming::{fold_arming_into_accounts, has_column};

use arming::{ensure_arming_columns, table_exists};
use read::{read_adoption, read_rows};

#[cfg(test)]
mod arming_tests;
#[cfg(test)]
mod profile_risk_tests;
#[cfg(test)]
mod row_write_tests;

/// **The `venue_setting` table survives a MIRROR, and nothing else in this module does.**
///
/// ⚠ This is the most expensive thing in the file to get wrong, and the shape of the code argues
/// FOR the mistake: [`write_settings`] clears `setting` and `venue_arming` and re-inserts both from
/// the caller's rows, so a third `DELETE FROM venue_setting` beside them reads as the obvious
/// tidy-up. It would not be one. Venue settings have no file to be re-derived FROM — that is the
/// whole reason they are a table instead of a `config` key — so the delete would simply remove
/// them: one `vike-cli config mirror` and a box loses its JForex server, its IBKR gateway host and
/// its polymarket egress, silently, with the command reporting success.
#[cfg(test)]
mod mirror_tests;

/// **The operator's writer for a moved venue setting**, and the DDL constraint that is the whole
/// reason these values are a table rather than a field on `Config`.
#[cfg(test)]
mod venue_setting_write_tests;
