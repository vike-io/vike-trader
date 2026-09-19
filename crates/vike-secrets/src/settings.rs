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
//! | [`venue_arming`](crate::schema::DDL) | an arming CEILING — one row per roster venue, plus one per labelled account | POLICY (`policy.toml`'s `[venues]` / `[accounts]`) | **no — it can only ever REFUSE** |
//! | `venue_setting` | a BRIDGE's operational configuration, machine- or tier-scoped | the venue adapter | not an arming question at all |
//! | [`setting`](crate::schema::DDL) | anything keyed by a settings SECTION and a dotted key | `vike_config`'s four typed files | `config`/`preferences`/`flags` only; `policy` rows are sealed by the TYPE |
//! | [`profile_risk`](crate::schema::DDL) | one RUN PROFILE's `[risk]` table, one row per key | the run profile `VIKE_RUN_PROFILE` / `--profile` names | **it widens nothing, because nothing reads it** — see below |
//!
//! So: **a knob that can make a venue do MORE is an arming row and belongs in `venue_arming`; a
//! knob a bridge reads to talk to a venue at all is `venue_setting`; anything an operator names as
//! `<section>.<dotted.key>` in one of the four settings files is a `setting` row; anything inside a
//! RUN PROFILE's `[risk]` table is a `profile_risk` row.** The test to
//! apply when a new knob is ambiguous is the one `vike_config::venue_mode::VenueMode::cap` encodes:
//! if the value composes by `min` with another layer and can never raise anything, it is an arming
//! ceiling. If it configures rather than bounds, it is not.
//!
//! The fourth row is the one whose OWNER decides it rather than its shape: a `[risk]` key is a
//! pre-trade ceiling, so it looks exactly like an arming ceiling and is not one. `venue_arming`
//! is a property of the BOX, written in `policy.toml`, which no `--profile` flag swaps; a `[risk]`
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
//! gate that scans the tree: `crates/vike-ops/tests/profile_risk_readers_gate.rs` pins the files
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

use std::path::{Path, PathBuf};

use crate::db::{DbError, DbErrorKind, database_present, open_for_read, open_for_write};

/// The four settings SECTIONS a [`SettingRow`] may carry — `setting.section`'s `CHECK` list, and
/// the first segment of the dotted key an operator names (`policy.max_leverage`).
///
/// Stated here and in the DDL's `CHECK` because they are two different gates on the same
/// vocabulary: this one is what a Rust caller is refused by, that one is what a hand `INSERT` is
/// refused by. [`section_is_known`] is how the first is applied, so no caller re-spells the list.
pub const SETTINGS_SECTIONS: [&str; 4] = ["policy", "config", "preferences", "flags"];

/// Is `section` one of [`SETTINGS_SECTIONS`]?
#[must_use]
pub fn section_is_known(section: &str) -> bool {
    SETTINGS_SECTIONS.contains(&section)
}

/// One row of the `setting` table: a settings SECTION, a dotted key inside that section's file, and
/// **one JSON SCALAR**.
///
/// The value being a rendering rather than a typed column is 0057's *validate-on-load — PRESERVED,
/// and cheaply*: `vike_config::mirror` PARSES each row's value with `serde_json` and deserializes
/// the section's existing patch type from the assembled object, so no second validator is written,
/// every bound stays imported from `vike_model`, and a tombstoned key is refused with the message
/// it has always had.
///
/// ⚠ **It held a TOML rendering until 2026-09-18**, and the reader then spliced those renderings
/// back into a synthetic TOML document and parsed that — one scalar rendered as TOML, stored as
/// TOML, re-rendered as TOML and parsed twice. A JSON scalar is parsed once, by a parser that
/// decides the type. `vike_config::mirror`'s module doc carries what the change cost and what it
/// had to refuse to keep (`null`, a datetime, a non-finite float); the migration for a store
/// written before it is `vike-cli config mirror`, which re-derives every row from the files.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SettingRow {
    /// One of [`SETTINGS_SECTIONS`].
    pub section: String,
    /// The key's path INSIDE that section's file, dotted — `max_leverage`, `rate.max_utilization`.
    /// No section prefix: `policy.toml` holds `max_leverage`, not `policy.max_leverage`.
    pub key: String,
    /// One JSON scalar — `1.0`, `"warn"`, `true`.
    pub value: String,
}

/// One row of the `venue_arming` table: an arming CEILING for a venue, or for one labelled account
/// of a venue.
///
/// `label = None` is `policy.toml`'s `[venues]` row for that venue; `Some(label)` is its
/// `[accounts].<venue>.<label>` row. Both are ceilings and both can only ever refuse.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArmingRow {
    /// The roster venue id, lowercase — `vike_model::VENUES`' spelling.
    pub venue: String,
    /// The account label, or `None` for the venue-level ceiling.
    pub label: Option<String>,
    /// `paper` / `demo` / `live` — the DDL's `CHECK` list, and `VenueMode`'s own words.
    pub mode: String,
}

/// Everything the settings tables hold, as data. **No connection, no path, no handle** — see this
/// module's doc for why that is the whole point of the type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoredSettings {
    /// `setting` rows, sorted by `(section, key)`.
    pub settings: Vec<SettingRow>,
    /// `venue_arming` rows, sorted by `(venue, label)`.
    pub arming: Vec<ArmingRow>,
}

impl StoredSettings {
    /// Every row, sorted — the shape both the reader and the writer produce, so two
    /// [`StoredSettings`] built from the same facts compare equal whatever order they were built
    /// in. The mirror gate leans on exactly that.
    pub fn sorted(mut self) -> Self {
        self.settings.sort();
        self.arming.sort();
        self
    }

    /// `true` when there is nothing at all — a store whose tables exist and are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty() && self.arming.is_empty()
    }
}

/// What [`read_settings`] found, and **why it found nothing when it found nothing**.
///
/// An enum rather than an empty [`StoredSettings`] for the reason [`crate::Accounts`] is one: three
/// of the four answers below mean *this box has not mirrored yet*, which is the ordinary state, and
/// collapsing them into "the settings rows are empty" would let a caller report an authoritative
/// nothing about a store it cannot see. During the mirror period all four are equally harmless —
/// the files win — which is exactly why the distinction has to be carried NOW, before they do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsSource {
    /// The tables are there and these are their rows (possibly none).
    Rows(StoredSettings),
    /// [`crate::Backend::Files`] — no settings database on this box at all.
    NoDatabase {
        /// Where a database would be.
        path: PathBuf,
    },
    /// A database this binary can READ ([`crate::READABLE_SCHEMA_VERSIONS`]) that predates the
    /// settings tables — i.e. a box migrated before 0057 Phase 1 and not mirrored since.
    TablesAbsent {
        /// The database that was opened.
        path: PathBuf,
    },
}

impl SettingsSource {
    /// The rows, or `None` for every arm that has none. The shape a loader wants: an unmirrored box
    /// contributes no layer, exactly as an absent file contributes no layer.
    #[must_use]
    pub fn rows(&self) -> Option<&StoredSettings> {
        match self {
            SettingsSource::Rows(r) => Some(r),
            SettingsSource::NoDatabase { .. } | SettingsSource::TablesAbsent { .. } => None,
        }
    }
}

impl std::fmt::Display for SettingsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsSource::Rows(r) => write!(
                f,
                "{} setting rows and {} venue-arming rows",
                r.settings.len(),
                r.arming.len()
            ),
            SettingsSource::NoDatabase { path } => {
                write!(f, "no settings database at {} — the files answer alone", path.display())
            }
            SettingsSource::TablesAbsent { path } => write!(
                f,
                "the settings database {} carries no settings tables yet — it was migrated before \
                 they existed, and `vike-cli config mirror` is what writes them",
                path.display()
            ),
        }
    }
}

/// What [`write_settings`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SettingsWritten {
    /// `setting` rows after the write.
    pub settings: usize,
    /// `venue_arming` rows after the write.
    pub arming: usize,
    /// `true` when this call created the two tables (a store migrated before they existed).
    pub tables_created: bool,
}

/// **Read the settings rows from the database beside `settings_dir`.** Never creates anything.
///
/// The `_in` spelling of [`crate::backend_in`]'s convention: the caller holds the settings
/// DIRECTORY, the path is derived with [`crate::db_path_in`], and nothing here walks or reads an
/// environment.
pub fn read_settings_in(settings_dir: &Path) -> Result<SettingsSource, DbError> {
    read_settings(&crate::dotenv::db_path_in(settings_dir))
}

/// [`read_settings_in`] over the database's own path — the PURE root, so every arm is reachable
/// from a test without a walk.
pub fn read_settings(db: &Path) -> Result<SettingsSource, DbError> {
    if !database_present(db) {
        return Ok(SettingsSource::NoDatabase { path: db.to_path_buf() });
    }
    // Read-only, and the schema version is checked by the opener — a store at a version this binary
    // cannot read is LOUD here exactly as it is for a credential read, never a quiet empty layer.
    let (conn, _version) = open_for_read(db)?;
    if !table_exists(db, &conn, "setting")? || !table_exists(db, &conn, "venue_arming")? {
        return Ok(SettingsSource::TablesAbsent { path: db.to_path_buf() });
    }

    let mut settings = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT section, key, value FROM setting ORDER BY section, key")
            .map_err(|e| DbError::sql(db, e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SettingRow { section: r.get(0)?, key: r.get(1)?, value: r.get(2)? })
            })
            .map_err(|e| DbError::sql(db, e))?;
        for row in rows {
            settings.push(row.map_err(|e| DbError::sql(db, e))?);
        }
    }

    let mut arming = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT venue, label, mode FROM venue_arming ORDER BY venue, label")
            .map_err(|e| DbError::sql(db, e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ArmingRow { venue: r.get(0)?, label: r.get(1)?, mode: r.get(2)? })
            })
            .map_err(|e| DbError::sql(db, e))?;
        for row in rows {
            arming.push(row.map_err(|e| DbError::sql(db, e))?);
        }
    }

    Ok(SettingsSource::Rows(StoredSettings { settings, arming }.sorted()))
}

/// Does this database carry `name` as a TABLE?
///
/// The settings tables are deliberately NOT gated on [`crate::SCHEMA_VERSION`] — see
/// [`crate::schema::DDL`]'s note, where a bump is measured as the live gate for two already-migrated
/// boxes — so *is it there* is asked of `sqlite_master` rather than of a number. `name` is a
/// `&'static str` from this module's own call sites, never operator input.
fn table_exists(db: &Path, conn: &rusqlite::Connection, name: &str) -> Result<bool, DbError> {
    let found: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |r| r.get(0),
        )
        .map_err(|e| DbError::sql(db, e))?;
    Ok(found > 0)
}

/// **Replace the settings rows in the database beside `settings_dir`.** The `_in` spelling, as
/// [`read_settings_in`].
pub fn write_settings_in(
    settings_dir: &Path,
    rows: &StoredSettings,
) -> Result<SettingsWritten, DbError> {
    write_settings(&crate::dotenv::db_path_in(settings_dir), rows)
}

/// **Replace the settings rows** — the whole of both tables, in ONE transaction.
///
/// # Why a REPLACE rather than an upsert
///
/// The mirror's source is the four files, and a file that stops setting a key is a key that stops
/// being set. An upsert would leave the row behind, and a row nothing wrote is exactly the thing
/// 0057 forbids migrating: it reads as more authoritative than the file line it left. So a mirror
/// run makes the tables equal to the files or fails, and there is no third state — the transaction
/// is what buys that.
///
/// # ⚠ It refuses a project with no database
///
/// [`crate::DbErrorKind::NoSettingsDatabase`], and the reason is the live gate rather than tidiness:
/// see that variant. `vike-cli secrets migrate` is the one thing that may create the store.
///
/// The DDL batch runs inside the same transaction, `IF NOT EXISTS` throughout, so a store migrated
/// before these tables existed gains them here and a store born with them is untouched. No schema
/// version is stamped and none is read — again, see [`crate::schema::DDL`]'s note.
pub fn write_settings(db: &Path, rows: &StoredSettings) -> Result<SettingsWritten, DbError> {
    if !database_present(db) {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    for row in &rows.settings {
        debug_assert!(
            section_is_known(&row.section),
            "a caller offered the unknown settings section {:?}; the DDL's CHECK refuses it too",
            row.section
        );
    }

    let (mut conn, created, _version) = open_for_write(db)?;
    if created {
        // The file vanished between the probe above and the open, and this call has just re-created
        // it. Same hazard and same repair as `upsert_rows`' `VanishedDatabase`: an empty database
        // makes `Backend` answer `Database` for every process on the box forever.
        drop(conn);
        let _ = std::fs::remove_file(db);
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }

    let tables_created = {
        let existed = {
            let has_setting = table_exists(db, &conn, "setting")?;
            let has_arming = table_exists(db, &conn, "venue_arming")?;
            has_setting && has_arming
        };
        !existed
    };

    let tx = conn.transaction().map_err(|e| DbError::sql(db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(db, e))?;
    tx.execute("DELETE FROM setting", []).map_err(|e| DbError::sql(db, e))?;
    tx.execute("DELETE FROM venue_arming", []).map_err(|e| DbError::sql(db, e))?;
    {
        let mut stmt = tx
            .prepare("INSERT INTO setting (section, key, value) VALUES (?1, ?2, ?3)")
            .map_err(|e| DbError::sql(db, e))?;
        for row in &rows.settings {
            stmt.execute(rusqlite::params![row.section, row.key, row.value])
                .map_err(|e| DbError::sql(db, e))?;
        }
    }
    {
        let mut stmt = tx
            .prepare("INSERT INTO venue_arming (venue, label, mode) VALUES (?1, ?2, ?3)")
            .map_err(|e| DbError::sql(db, e))?;
        for row in &rows.arming {
            stmt.execute(rusqlite::params![row.venue, row.label, row.mode])
                .map_err(|e| DbError::sql(db, e))?;
        }
    }
    tx.commit().map_err(|e| DbError::sql(db, e))?;

    Ok(SettingsWritten { settings: rows.settings.len(), arming: rows.arming.len(), tables_created })
}

// ---------------------------------------------------------------------------------------------
// `profile_risk` — 0057 Phase 2. A DISCLOSURE copy; see this module's doc for what reads it.
// ---------------------------------------------------------------------------------------------

/// One row of the `profile_risk` table: a `[risk]` key of ONE run profile, and **the TOML RENDERING
/// of one scalar**.
///
/// The value is a rendering rather than a typed column for the same reason [`SettingRow::value`] is
/// — it round-trips through the same parse an operator's file goes through, so no second validator
/// is written. The key vocabulary is `vike_config::PROFILE_RISK_KEYS`, gated against
/// `vike_exec::ProfileRisk`'s own fields.
///
/// ⚠ **This column stayed TOML when [`SettingRow::value`] became JSON on 2026-09-18, and the
/// difference is a decision rather than an oversight.** `vike_config::profile_risk`'s
/// `ProfileRiskKey::accepts` states the rule this table is built on — *"the mirror must never be
/// STRICTER than the boot"* — and a JSON column would break it on exactly one shape: JSON has no
/// literal for `inf`, while `max_leverage = inf` is a run profile `vike_exec::ProfileRisk` parses
/// and `vike_core::RunProfile::validate` does not refuse. The settings column has no such shape,
/// because `vike_config::load` runs FIRST there and every `f64` field is finiteness-checked or
/// bounded. Every OTHER value this column can hold is valid JSON carrying the same value — the
/// roster admits floats, integers and bools and nothing else — so the two encodings differ on the
/// one case that forced them apart, and on nothing an operator's profile can otherwise produce.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProfileRiskRow {
    /// The `[risk]` key — `max_notional_per_order`, `max_leverage`, …. No `risk.` prefix: the
    /// table IS the `[risk]` table.
    pub key: String,
    /// The TOML rendering of one scalar — `250000.0`, `10`, `true`.
    pub value: String,
}

/// One run profile's whole `[risk]` table, as rows.
///
/// [`Self::profile`] is the profile's NAME, not a path. A path is a fact about one box's disk and
/// would put a box-specific string in a database that `vike-cli secrets`/`config` prints; the file
/// NAME is what an operator recognises and what the deployed unit's `VIKE_RUN_PROFILE` ends with.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct StoredProfileRisk {
    /// The profile's file name — `run-live.toml`.
    pub profile: String,
    /// Its `[risk]` rows, sorted by key.
    pub rows: Vec<ProfileRiskRow>,
}

/// What [`read_profile_risk`] found, and **why it found nothing when it found nothing** — the same
/// three-armed shape [`SettingsSource`] carries, and for the same reason: a box that has not
/// mirrored is the ordinary state, and an empty `Vec` would let a caller report an authoritative
/// nothing about a store it cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileRiskSource {
    /// The table is there and these are its profiles (possibly none), sorted by name.
    Rows(Vec<StoredProfileRisk>),
    /// [`crate::Backend::Files`] — no settings database on this box at all.
    NoDatabase {
        /// Where a database would be.
        path: PathBuf,
    },
    /// A database this binary can read that predates the `profile_risk` table — a box migrated
    /// before 0057 Phase 2 and not mirrored since.
    TableAbsent {
        /// The database that was opened.
        path: PathBuf,
    },
}

impl ProfileRiskSource {
    /// The profiles, or `None` for every arm that has none.
    #[must_use]
    pub fn profiles(&self) -> Option<&[StoredProfileRisk]> {
        match self {
            ProfileRiskSource::Rows(p) => Some(p),
            ProfileRiskSource::NoDatabase { .. } | ProfileRiskSource::TableAbsent { .. } => None,
        }
    }
}

impl std::fmt::Display for ProfileRiskSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileRiskSource::Rows(p) => write!(
                f,
                "{} mirrored run profile(s) carrying {} `[risk]` row(s)",
                p.len(),
                p.iter().map(|x| x.rows.len()).sum::<usize>()
            ),
            ProfileRiskSource::NoDatabase { path } => {
                write!(
                    f,
                    "no settings database at {} — the profile file answers alone",
                    path.display()
                )
            }
            ProfileRiskSource::TableAbsent { path } => write!(
                f,
                "the settings database {} carries no `profile_risk` table yet — `vike-cli config \
                 mirror --profile <file>` is what writes it",
                path.display()
            ),
        }
    }
}

/// What [`write_profile_risk`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProfileRiskWritten {
    /// Rows this profile has after the write.
    pub rows: usize,
    /// Rows this profile had BEFORE it — so a mirror that REMOVED keys can say so.
    pub replaced: usize,
    /// `true` when this call created the table (a store migrated before it existed).
    pub table_created: bool,
}

/// **Read every mirrored run profile's `[risk]` rows from the database beside `settings_dir`.**
pub fn read_profile_risk_in(settings_dir: &Path) -> Result<ProfileRiskSource, DbError> {
    read_profile_risk(&crate::dotenv::db_path_in(settings_dir))
}

/// [`read_profile_risk_in`] over the database's own path — the PURE root, so every arm is reachable
/// from a test without a walk.
///
/// ⚠ **This is a DISCLOSURE read and its callers are PINNED** —
/// `crates/vike-ops/tests/profile_risk_readers_gate.rs`. See this module's doc: the rows judge no
/// order, and a new caller that wanted them to would have to say so in that file.
pub fn read_profile_risk(db: &Path) -> Result<ProfileRiskSource, DbError> {
    if !database_present(db) {
        return Ok(ProfileRiskSource::NoDatabase { path: db.to_path_buf() });
    }
    let (conn, _version) = open_for_read(db)?;
    if !table_exists(db, &conn, "profile_risk")? {
        return Ok(ProfileRiskSource::TableAbsent { path: db.to_path_buf() });
    }

    let mut out: Vec<StoredProfileRisk> = Vec::new();
    let mut stmt = conn
        .prepare("SELECT profile, key, value FROM profile_risk ORDER BY profile, key")
        .map_err(|e| DbError::sql(db, e))?;
    let rows = stmt
        .query_map([], |r| {
            let profile: String = r.get(0)?;
            Ok((profile, ProfileRiskRow { key: r.get(1)?, value: r.get(2)? }))
        })
        .map_err(|e| DbError::sql(db, e))?;
    for row in rows {
        let (profile, row) = row.map_err(|e| DbError::sql(db, e))?;
        match out.last_mut() {
            Some(last) if last.profile == profile => last.rows.push(row),
            _ => out.push(StoredProfileRisk { profile, rows: vec![row] }),
        }
    }
    Ok(ProfileRiskSource::Rows(out))
}

/// **Replace ONE profile's `[risk]` rows** in the database beside `settings_dir`.
pub fn write_profile_risk_in(
    settings_dir: &Path,
    profile: &StoredProfileRisk,
) -> Result<ProfileRiskWritten, DbError> {
    write_profile_risk(&crate::dotenv::db_path_in(settings_dir), profile)
}

/// **Replace ONE profile's `[risk]` rows**, in ONE transaction.
///
/// # Scoped to the named profile, and deliberately so
///
/// [`write_settings`] replaces the WHOLE of its two tables because its source is the whole of the
/// four files. This writer's source is ONE file, so it deletes and re-inserts one profile's rows
/// and leaves every other profile alone — a mirror of `run-live.toml` must not silently drop the
/// rows of `run-paper.toml`. Within that profile the replace rule is the same and for the same
/// reason: a key the file stopped setting is a key that stops being set, and a row nothing wrote
/// reads as more authoritative than the file line it outlived.
///
/// # ⚠ It refuses a project with no database
///
/// [`crate::DbErrorKind::NoSettingsDatabase`], exactly as [`write_settings`], and the reason is the
/// live gate rather than tidiness: see that variant.
pub fn write_profile_risk(
    db: &Path,
    profile: &StoredProfileRisk,
) -> Result<ProfileRiskWritten, DbError> {
    if !database_present(db) {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    let (mut conn, created, _version) = open_for_write(db)?;
    if created {
        // Same hazard and same repair as `write_settings`: the file vanished between the probe and
        // the open, and an empty database makes `Backend` answer `Database` for every process on
        // the box forever.
        drop(conn);
        let _ = std::fs::remove_file(db);
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }

    let table_created = !table_exists(db, &conn, "profile_risk")?;
    let tx = conn.transaction().map_err(|e| DbError::sql(db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(db, e))?;
    let replaced = tx
        .execute("DELETE FROM profile_risk WHERE profile = ?1", rusqlite::params![profile.profile])
        .map_err(|e| DbError::sql(db, e))?;
    {
        let mut stmt = tx
            .prepare("INSERT INTO profile_risk (profile, key, value) VALUES (?1, ?2, ?3)")
            .map_err(|e| DbError::sql(db, e))?;
        for row in &profile.rows {
            stmt.execute(rusqlite::params![profile.profile, row.key, row.value])
                .map_err(|e| DbError::sql(db, e))?;
        }
    }
    tx.commit().map_err(|e| DbError::sql(db, e))?;

    Ok(ProfileRiskWritten { rows: profile.rows.len(), replaced, table_created })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database with the schema on it and the version stamped — what `secrets migrate` leaves
    /// behind, minus the credentials, so these tests never touch a credential path at all.
    fn planted(dir: &Path) -> PathBuf {
        let db = crate::dotenv::db_path_in(dir);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
        conn.execute_batch(crate::schema::DDL).unwrap();
        conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
        db
    }

    fn rows() -> StoredSettings {
        StoredSettings {
            settings: vec![
                SettingRow {
                    section: "policy".into(),
                    key: "max_leverage".into(),
                    value: "3.0".into(),
                },
                SettingRow {
                    section: "preferences".into(),
                    key: "log_file_level".into(),
                    value: "\"warn\"".into(),
                },
            ],
            arming: vec![
                ArmingRow { venue: "binance".into(), label: None, mode: "demo".into() },
                ArmingRow {
                    venue: "hyperliquid".into(),
                    label: Some("ALT".into()),
                    mode: "paper".into(),
                },
            ],
        }
    }

    #[test]
    fn a_project_with_no_database_reads_as_no_database_and_refuses_a_write() {
        let tmp = tempfile::tempdir().unwrap();
        let found = read_settings_in(tmp.path()).unwrap();
        assert!(matches!(found, SettingsSource::NoDatabase { .. }), "{found:?}");
        assert!(found.rows().is_none());

        let err = write_settings_in(tmp.path(), &rows()).unwrap_err();
        assert!(matches!(err.kind, DbErrorKind::NoSettingsDatabase), "{err}");
        // ...and it did not create one on the way past. This is the assertion that matters: a
        // settings write that created the store would take every venue to paper.
        assert!(
            !crate::dotenv::db_path_in(tmp.path()).exists(),
            "the refusal must leave NO database behind — its existence is the credential backend's \
             whole choice"
        );
    }

    #[test]
    fn a_round_trip_returns_exactly_what_was_written() {
        let tmp = tempfile::tempdir().unwrap();
        planted(tmp.path());
        let written = write_settings_in(tmp.path(), &rows()).unwrap();
        assert_eq!(written.settings, 2);
        assert_eq!(written.arming, 2);
        assert!(!written.tables_created, "a store planted from the full DDL already has them");

        let found = read_settings_in(tmp.path()).unwrap();
        assert_eq!(found.rows(), Some(&rows().sorted()));
    }

    /// A store migrated BEFORE these tables existed reads as [`SettingsSource::TablesAbsent`] and is
    /// carried by the first write — never as an empty settings layer, which is the answer a caller
    /// could not tell from "this box mirrored nothing on purpose".
    #[test]
    fn a_store_without_the_tables_says_so_and_the_first_write_creates_them() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::dotenv::db_path_in(tmp.path());
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
        // The credential half of the schema only — the shape a box migrated before Phase 1 carries.
        conn.execute_batch(
            "CREATE TABLE node_key (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
        drop(conn);

        let found = read_settings(&db).unwrap();
        assert!(matches!(found, SettingsSource::TablesAbsent { .. }), "{found:?}");
        assert!(found.rows().is_none(), "an unmirrored box contributes NO layer, not an empty one");

        let written = write_settings(&db, &rows()).unwrap();
        assert!(written.tables_created, "the first write is what creates them");
        assert_eq!(read_settings(&db).unwrap().rows(), Some(&rows().sorted()));
    }

    /// **A store that EXISTS and will not open is an ERROR, never `NoDatabase` and never empty
    /// rows** — the three answers must stay distinguishable, because each one asks a caller for
    /// something different.
    ///
    /// This is the INPUT to the degrade decision `vike_boot::boot` makes (an unopenable store is a
    /// warning; the files still answer), and that decision is only correct while this arm is an
    /// `Err`: collapsing it into `NoDatabase` would make a permissions bug on a mirrored box read
    /// exactly like a box that was never mirrored, with nothing anywhere saying so. The same
    /// posture `crate::store::resolve` already takes between an ABSENT credential store and an
    /// unreadable one.
    #[test]
    fn a_store_that_exists_and_will_not_open_is_an_error_not_an_absent_one() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::dotenv::db_path_in(tmp.path());
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"this is not a database").unwrap();

        let err = read_settings(&db).unwrap_err();
        assert_eq!(err.path, db);
        assert!(
            !matches!(err.kind, DbErrorKind::NoSettingsDatabase),
            "the file IS there — the refusal must not read as absence: {err}"
        );
    }

    /// A mirror run makes the tables EQUAL to its input — a key that stops being set stops being a
    /// row. An upsert would leave it behind, and a row nothing wrote reads as authoritative.
    #[test]
    fn a_second_write_replaces_rather_than_accumulates() {
        let tmp = tempfile::tempdir().unwrap();
        planted(tmp.path());
        write_settings_in(tmp.path(), &rows()).unwrap();

        let fewer = StoredSettings {
            settings: vec![SettingRow {
                section: "policy".into(),
                key: "max_leverage".into(),
                value: "1.0".into(),
            }],
            arming: Vec::new(),
        };
        write_settings_in(tmp.path(), &fewer).unwrap();
        assert_eq!(read_settings_in(tmp.path()).unwrap().rows(), Some(&fewer.sorted()));
    }

    /// The `CHECK` lists are gates, not comments — a hand `INSERT` of an unknown section or an
    /// unknown mode is refused by the engine, which is the half a Rust-side check can never cover.
    #[test]
    fn the_schema_refuses_an_unknown_section_and_an_unknown_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let db = planted(tmp.path());
        let conn = rusqlite::Connection::open(&db).unwrap();
        assert!(
            conn.execute(
                "INSERT INTO setting (section, key, value) VALUES ('secrets', 'k', '1')",
                []
            )
            .is_err(),
            "`section` carries a CHECK over the four settings sections"
        );
        assert!(
            conn.execute(
                "INSERT INTO venue_arming (venue, label, mode) VALUES ('binance', NULL, 'fat')",
                []
            )
            .is_err(),
            "`mode` carries a CHECK over paper/demo/live"
        );
    }

    /// The two partial indexes: one venue-level row per venue, one per (venue, label) — and the
    /// NULL label does not silently permit duplicates, which is what a single `UNIQUE (venue,
    /// label)` would have done.
    #[test]
    fn a_venue_gets_one_ceiling_row_and_each_label_gets_one_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let db = planted(tmp.path());
        let conn = rusqlite::Connection::open(&db).unwrap();
        let insert = |venue: &str, label: Option<&str>, mode: &str| {
            conn.execute(
                "INSERT INTO venue_arming (venue, label, mode) VALUES (?1, ?2, ?3)",
                rusqlite::params![venue, label, mode],
            )
        };
        insert("binance", None, "demo").unwrap();
        assert!(insert("binance", None, "live").is_err(), "one venue-level row per venue");
        insert("binance", Some("ALT"), "paper").unwrap();
        assert!(insert("binance", Some("ALT"), "live").is_err(), "one row per (venue, label)");
        // A DIFFERENT label is a different account and is allowed.
        insert("binance", Some("SUB"), "paper").unwrap();
    }

    // -----------------------------------------------------------------------------------------
    // `profile_risk` — 0057 Phase 2
    // -----------------------------------------------------------------------------------------

    fn live_profile() -> StoredProfileRisk {
        StoredProfileRisk {
            profile: "run-live.toml".into(),
            rows: vec![
                ProfileRiskRow { key: "max_leverage".into(), value: "3.0".into() },
                ProfileRiskRow { key: "max_notional_per_order".into(), value: "250.0".into() },
                ProfileRiskRow { key: "max_total_exposure".into(), value: "1000.0".into() },
            ],
        }
    }

    #[test]
    fn a_project_with_no_database_reads_as_no_database_and_refuses_a_profile_write() {
        let tmp = tempfile::tempdir().unwrap();
        let found = read_profile_risk_in(tmp.path()).unwrap();
        assert!(matches!(found, ProfileRiskSource::NoDatabase { .. }), "{found:?}");
        assert!(found.profiles().is_none());

        let err = write_profile_risk_in(tmp.path(), &live_profile()).unwrap_err();
        assert!(matches!(err.kind, DbErrorKind::NoSettingsDatabase), "{err}");
        assert!(
            !crate::dotenv::db_path_in(tmp.path()).exists(),
            "the refusal must leave NO database behind — its existence is the credential backend's \
             whole choice"
        );
    }

    #[test]
    fn a_profile_round_trips_and_a_re_mirror_replaces_only_its_own_rows() {
        let tmp = tempfile::tempdir().unwrap();
        planted(tmp.path());

        let live = live_profile();
        let paper = StoredProfileRisk {
            profile: "run-paper.toml".into(),
            rows: vec![ProfileRiskRow { key: "max_leverage".into(), value: "10.0".into() }],
        };
        let first = write_profile_risk_in(tmp.path(), &live).unwrap();
        assert_eq!((first.rows, first.replaced), (3, 0));
        write_profile_risk_in(tmp.path(), &paper).unwrap();

        let found = read_profile_risk_in(tmp.path()).unwrap();
        assert_eq!(found.profiles(), Some(&[live.clone(), paper.clone()][..]));

        // Re-mirroring `run-live.toml` with FEWER keys drops its removed rows...
        let shrunk = StoredProfileRisk {
            profile: "run-live.toml".into(),
            rows: vec![ProfileRiskRow { key: "max_leverage".into(), value: "2.0".into() }],
        };
        let again = write_profile_risk_in(tmp.path(), &shrunk).unwrap();
        assert_eq!((again.rows, again.replaced), (1, 3));
        // ...and leaves the OTHER profile exactly where it was, which is the whole reason this
        // writer is scoped to one profile rather than replacing the table.
        let found = read_profile_risk_in(tmp.path()).unwrap();
        assert_eq!(found.profiles(), Some(&[shrunk, paper][..]));
    }

    /// A store migrated before Phase 2 says so BY NAME rather than reading as "this profile sets
    /// no ceilings", which is the distinction the enum exists for.
    #[test]
    fn a_store_without_the_table_reads_as_table_absent_rather_than_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::dotenv::db_path_in(tmp.path());
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
        conn.execute_batch(
            "CREATE TABLE node_key (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
        drop(conn);

        let found = read_profile_risk_in(tmp.path()).unwrap();
        assert!(matches!(found, ProfileRiskSource::TableAbsent { .. }), "{found:?}");
        assert!(found.profiles().is_none());
        assert!(found.to_string().contains("config mirror --profile"), "{found}");

        // ...and a write CREATES it, on the same store, without a schema-version bump.
        let written = write_profile_risk_in(tmp.path(), &live_profile()).unwrap();
        assert!(written.table_created);
        assert_eq!(read_profile_risk_in(tmp.path()).unwrap().profiles().unwrap().len(), 1);
    }

    /// `UNIQUE (profile, key)` is a gate, not a comment: two rows for one key of one profile are
    /// refused by the engine, which is the half a Rust-side replace can never cover.
    #[test]
    fn the_schema_refuses_a_second_row_for_one_key_of_one_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let db = planted(tmp.path());
        let conn = rusqlite::Connection::open(&db).unwrap();
        let insert = |profile: &str, key: &str, value: &str| {
            conn.execute(
                "INSERT INTO profile_risk (profile, key, value) VALUES (?1, ?2, ?3)",
                rusqlite::params![profile, key, value],
            )
        };
        insert("run-live.toml", "max_leverage", "3.0").unwrap();
        assert!(insert("run-live.toml", "max_leverage", "9.0").is_err(), "one row per key");
        // A DIFFERENT profile is a different file and is allowed to carry the same key.
        insert("run-paper.toml", "max_leverage", "9.0").unwrap();
    }
}
