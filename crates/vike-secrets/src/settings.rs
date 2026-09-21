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
// ⚠ `Eq`/`Ord` were dropped when `max_exposure` landed — `Ord` requires `Eq`, and `Eq` over an
// `f64` is a claim this type cannot honour. Nothing needed the total order except
// [`StoredSettings::sorted`], which now sorts on the row's KEY explicitly: `(venue, label)` is the
// table's own unique key and the reader's `ORDER BY`, so it settles every comparison the derive
// settled and the figure never breaks a tie.
/// One row of the `venue_setting` table: a BRIDGE's operational configuration — the JForex server a
/// dukascopy account connects to, the IBKR gateway's host and port, the polymarket egress proxy.
///
/// ⚠ **`value` is a PLAIN STRING, not a JSON scalar, and that is the point of the table.**
/// [`SettingRow::value`] holds one JSON scalar because a typed schema deserializes it;
/// nothing deserializes these, so there is no encoding contract to get wrong. Writing a bare string
/// into `setting.value` is exactly what broke the CI box on 2026-09-21 — this table removes the class
/// rather than encoding around it.
///
/// ⚠ `tier` is `None` for a MACHINE-SCOPED value. The polymarket proxy is the family that takes
/// that shape: `crates/bridges/polymarket/src/egress.rs`'s `PROXY_KEYS` is a fixed allow-list read
/// once and cached, so it is not account-aware and cannot become so without that cache changing
/// shape. The DDL's two partial unique indexes keep the two scopes apart.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VenueSettingRow {
    /// The roster venue id, lowercase — `vike_model::VENUES`' spelling.
    pub venue: String,
    /// `sim` / `demo` / `live`, or `None` for a machine-scoped value. The DDL `CHECK`s this list.
    pub tier: Option<String>,
    /// The field name as the legacy credential key spelled it, upper-case — `SERVER`, `PROXY_HOST`.
    pub field: String,
    /// The value, verbatim. No encoding, no quoting.
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArmingRow {
    /// The roster venue id, lowercase — `vike_model::VENUES`' spelling.
    pub venue: String,
    /// The account label, or `None` for the venue-level ceiling.
    pub label: Option<String>,
    /// `paper` / `demo` / `live` — the DDL's `CHECK` list, and `VenueMode`'s own words.
    pub mode: String,
    /// **This account's own exposure ceiling**, or `None` where its line named no figure — which is
    /// every row on every box that has not written `policy.toml`'s `[account_exposure]` table.
    ///
    /// ⚠ It rides THIS table rather than one of its own because it passes the test
    /// [this module's doc](self) states for an ambiguous knob: it composes by `min` and can never
    /// raise anything, so it is an arming ceiling. The column is nullable and the DDL's second
    /// `CHECK` refuses a non-positive figure, which is the store's half of the refusal
    /// `vike_config`'s loader already performs on the file.
    ///
    /// ⚠ **A store written before this column existed does not have it**, and neither the reader
    /// nor the writer may assume it does — [`crate::schema::DDL`] is `CREATE TABLE IF NOT EXISTS`,
    /// which creates nothing on a table that is already there. Both paths go through
    /// [`has_column`].
    pub max_exposure: Option<f64>,
}

/// Everything the settings tables hold, as data. **No connection, no path, no handle** — see this
/// module's doc for why that is the whole point of the type.
// ⚠ `Eq` dropped with `ArmingRow`'s — see that type's note. Every comparison in the tree is
// `PartialEq` (two loaded settings in a test, the mirror gate's round trip), and nothing uses this
// as a map or set key.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StoredSettings {
    /// `setting` rows, sorted by `(section, key)`.
    pub settings: Vec<SettingRow>,
    /// `venue_arming` rows, sorted by `(venue, label)`.
    pub arming: Vec<ArmingRow>,
    /// `venue_setting` rows, sorted by `(venue, tier, field)`.
    ///
    /// ⚠ **A THIRD table rather than a `config` key, and the reason is `deny_unknown_fields`.**
    /// Ruling 10 first filed these ten values as `setting` rows under `config.venue.<venue>…`, and
    /// that shape cannot work: `vike_config::Config` carries `#[serde(deny_unknown_fields)]` and
    /// has no `venue` field, so the whole `config` section failed to deserialize — MEASURED on
    /// the CI box 2026-09-21, where it took the arming ceiling with it and every venue read `paper`.
    /// Giving `Config` a map field would fix that by punching a permanent hole in the one property
    /// that schema exists for: at sixteen roster venues it would be the largest subtree in the
    /// settings model where a typo is accepted in silence.
    ///
    /// ⚠ **The earlier ruling's stated reason did not survive measurement either.** It chose a
    /// `section.key` path because *"`CONSUMPTION` refuses it if nothing reads it"* — but that table
    /// is a fixed list of literal keys and `is_consumed` answers `true` for a key it does not
    /// carry, so an unread `config.venue.*` key was never going to be gated under either shape.
    /// What a column table buys instead is real: the DDL's `CHECK (tier IN ('sim','demo','live'))`
    /// refuses a mistyped tier at write time, and the two partial unique indexes encode that a
    /// machine-scoped row and a tier-scoped one are different namespaces rather than two spellings
    /// nobody compares.
    pub venue: Vec<VenueSettingRow>,
}

impl StoredSettings {
    /// Every row, sorted — the shape both the reader and the writer produce, so two
    /// [`StoredSettings`] built from the same facts compare equal whatever order they were built
    /// in. The mirror gate leans on exactly that.
    pub fn sorted(mut self) -> Self {
        self.settings.sort();
        // Keyed rather than derived — see [`ArmingRow`]'s own note on why that type carries no
        // total order any more. `(venue, label)` is the table's unique key, so this is a total
        // order over the rows even though it is a partial one over the struct.
        self.venue.sort();
        self.arming.sort_by(|a, b| {
            (a.venue.as_str(), a.label.as_deref()).cmp(&(b.venue.as_str(), b.label.as_deref()))
        });
        self
    }

    /// `true` when there is nothing at all — a store whose tables exist and are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty() && self.arming.is_empty() && self.venue.is_empty()
    }
}

/// What [`read_settings`] found, and **why it found nothing when it found nothing**.
///
/// **The ADOPTION SEAL — the one probe that decides which source answers for every settings key.**
///
/// Present means: this box resolves its settings from the ROWS, and the four files are not opened
/// for resolution at all. Absent means: the files answer exactly as they always have. There is no
/// third state and no per-key fallback — `crates/vike-secrets/src/store.rs`'s [`crate::Backend`]
/// decides which store answers for a credential NAME on one probe before any lookup, and this is
/// the same shape one table over. A key missing from the source that answered is MISSING and
/// resolves to its compiled-in default; it does not go looking in a file.
///
/// Written by `vike-cli config adopt` and by nothing else. See [`crate::schema::DDL`] for why the
/// probe is this row rather than the database's existence, the tables' existence or a row count —
/// each of which was available and each of which is refused there, one of them on a measurement.
///
/// ⚠ **Three of these columns are MECHANISM, not disclosure**, and a reader who treats them as
/// decoration will delete the only defences this crossing has:
///
/// * [`venues_declared`](Adoption::venues_declared) is the UNMOUNT detector. `VenuePolicy`'s own
///   `is_declared` is a fact about whether an operator EVER STATED an arming, and a `policy.toml`
///   with no `[venues]` table mirrors to ZERO arming rows deliberately — so once the rows are the
///   only layer, *nobody ever stated one* and *the rows were erased* are the same empty table. This
///   column is what tells them apart.
/// * [`setting_rows`](Adoption::setting_rows) / [`arming_rows`](Adoption::arming_rows) are the
///   ERASE detector. Every sanctioned write updates them inside the transaction that changes the
///   tables, so a disagreement in EITHER direction means rows arrived or left by some route other
///   than the writer.
///
/// They are COUNTS and deliberately not a digest. A digest would make the store tamper-evident and
/// would also refuse a boot over a hand-edited `preferences.log_file_level` — the JSON incident
/// with a different value in the column. Counts are the largest check that cannot brick a box over
/// a legal value, and what they leave silent is stated where it can be acted on:
/// `vike_config::mirror`'s integrity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    /// RFC 3339 UTC, as `config adopt` observed it. Disclosure.
    pub adopted_at: String,
    /// The `vike-cli` version that performed the adoption. Disclosure.
    pub tool_version: String,
    /// Which of the four settings files existed at adoption, comma-separated in
    /// `vike_config::SECTION_FILES` order, or empty for none. Scopes the drift report, and tells a
    /// later deletion PR what it is deleting.
    pub files_present: String,
    /// `VenuePolicy::is_declared` at adoption — see this type's doc. The UNMOUNT detector.
    pub venues_declared: bool,
    /// `setting` rows at adoption. The ERASE detector.
    pub setting_rows: usize,
    /// `venue_arming` rows at adoption. The ERASE detector.
    pub arming_rows: usize,
}

/// An enum rather than an empty [`StoredSettings`] for the reason [`crate::Accounts`] is one: three
/// of the four answers below mean *this box has not mirrored yet*, which is the ordinary state, and
/// collapsing them into "the settings rows are empty" would let a caller report an authoritative
/// nothing about a store it cannot see. During the mirror period all four are equally harmless —
/// the files win — which is exactly why the distinction has to be carried NOW, before they do not.
// ⚠ `Eq` dropped with `ArmingRow`'s — see that type's note; this enum carries `StoredSettings`.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsSource {
    /// The tables are there and these are their rows (possibly none), together with the ADOPTION
    /// SEAL read from the same open.
    ///
    /// ⚠ **`adopted: None` is the ordinary answer and is not a degraded one.** It is every box that
    /// has mirrored but not crossed, which on the day `config adopt` ships is every box in the
    /// world. It is also `vike-cli secrets migrate`'s fresh store, whose `setting` and
    /// `venue_arming` tables are created EMPTY by the shared DDL batch — which is exactly why the
    /// seal cannot be a table probe or a row count. See [`Adoption`].
    Rows {
        /// The two tables' rows.
        rows: StoredSettings,
        /// The seal, or `None` when this box has not crossed.
        adopted: Option<Adoption>,
    },

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
            SettingsSource::Rows { rows, .. } => Some(rows),
            SettingsSource::NoDatabase { .. } | SettingsSource::TablesAbsent { .. } => None,
        }
    }

    /// The ADOPTION SEAL, or `None` for every arm that carries none.
    ///
    /// ⚠ **This is the probe, and it is asked of the SOURCE rather than of the rows**, because
    /// three of the states a caller can be in have rows and no seal and one has neither. A caller
    /// that reaches for [`SettingsSource::rows`] alone gets exactly the `Option` that threw this
    /// distinction away for the whole of the mirror period — see [`Adoption`].
    #[must_use]
    pub fn adoption(&self) -> Option<&Adoption> {
        match self {
            SettingsSource::Rows { adopted, .. } => adopted.as_ref(),
            SettingsSource::NoDatabase { .. } | SettingsSource::TablesAbsent { .. } => None,
        }
    }
}

impl std::fmt::Display for SettingsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsSource::Rows { rows, adopted: Some(a) } => write!(
                f,
                "{} setting rows and {} venue-arming rows — ADOPTED {} by {}, so the rows answer \
                 and the settings files are not read",
                rows.settings.len(),
                rows.arming.len(),
                a.adopted_at,
                a.tool_version
            ),
            SettingsSource::Rows { rows, adopted: None } => write!(
                f,
                "{} setting rows and {} venue-arming rows — NOT adopted, so the settings files \
                 answer and these rows are read below them",
                rows.settings.len(),
                rows.arming.len()
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
            // ⚠ The column is SELECTED only where it exists — see [`has_column`]. A store written
            // before it was added still answers, with `None` for every row, which is the truthful
            // reading: that box states no per-account figure because it could not have.
            .prepare(match has_column(db, &conn, "venue_arming", "max_exposure")? {
                true => {
                    "SELECT venue, label, mode, max_exposure FROM venue_arming \
                         ORDER BY venue, label"
                }
                false => "SELECT venue, label, mode, NULL FROM venue_arming ORDER BY venue, label",
            })
            .map_err(|e| DbError::sql(db, e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ArmingRow {
                    venue: r.get(0)?,
                    label: r.get(1)?,
                    mode: r.get(2)?,
                    max_exposure: r.get(3)?,
                })
            })
            .map_err(|e| DbError::sql(db, e))?;
        for row in rows {
            arming.push(row.map_err(|e| DbError::sql(db, e))?);
        }
    }

    // ⚠ The `venue_setting` rows, read the same way and guarded the same way. A store written
    // before this table existed answers with nothing to read, which is the truthful reading: that
    // box has moved no venue configuration out of `credential` because it could not have.
    let mut venue = Vec::new();
    if table_exists(db, &conn, "venue_setting")? {
        let mut stmt = conn
            .prepare(
                "SELECT venue, tier, field, value FROM venue_setting ORDER BY venue, tier, field",
            )
            .map_err(|e| DbError::sql(db, e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(VenueSettingRow {
                    venue: r.get(0)?,
                    tier: r.get(1)?,
                    field: r.get(2)?,
                    value: r.get(3)?,
                })
            })
            .map_err(|e| DbError::sql(db, e))?;
        for row in rows {
            venue.push(row.map_err(|e| DbError::sql(db, e))?);
        }
    }

    // ...and the SEAL, from the same open. It is read LAST and it is read unconditionally: a store
    // whose `settings_adoption` table is absent is a store written before the seal existed, which
    // is `None` — the same answer as an adopted-then-undone box, and the right one for both.
    let adopted = if table_exists(db, &conn, "settings_adoption")? {
        read_adoption(db, &conn)?
    } else {
        None
    };

    Ok(SettingsSource::Rows { rows: StoredSettings { settings, arming, venue }.sorted(), adopted })
}

/// The ADOPTION row, over a connection the caller already holds. At most one — `CHECK (id = 1)`.
fn read_adoption(db: &Path, conn: &rusqlite::Connection) -> Result<Option<Adoption>, DbError> {
    let mut stmt = conn
        .prepare(
            "SELECT adopted_at, tool_version, files_present, venues_declared, setting_rows, \
             arming_rows FROM settings_adoption WHERE id = 1",
        )
        .map_err(|e| DbError::sql(db, e))?;
    let mut rows = stmt
        .query_map([], |r| {
            Ok(Adoption {
                adopted_at: r.get(0)?,
                tool_version: r.get(1)?,
                files_present: r.get(2)?,
                venues_declared: r.get::<_, i64>(3)? != 0,
                setting_rows: r.get::<_, i64>(4)?.max(0) as usize,
                arming_rows: r.get::<_, i64>(5)?.max(0) as usize,
            })
        })
        .map_err(|e| DbError::sql(db, e))?;
    match rows.next() {
        Some(row) => Ok(Some(row.map_err(|e| DbError::sql(db, e))?)),
        None => Ok(None),
    }
}

/// **Write the ADOPTION SEAL** — the act that makes the rows answer for every settings key on this
/// box, and the only thing in this crate that can.
///
/// Reached from `vike-cli config adopt` and from nothing else. That command re-compares the two
/// resolutions and re-reads every row through the boot's own reader BEFORE calling this, which is
/// what LICENSES the dispositions that change on the far side of the seal — above all `vike_config`
/// inverting an unreadable row from a degrade into a refusal. The seal is positive evidence that
/// every row was readable at a moment somebody chose, not a preference.
///
/// The counts are taken from the tables INSIDE this transaction rather than from the caller, so the
/// erase detector can never be sealed against a number nobody measured.
///
/// ⚠ Refuses a store with no `setting` table rather than creating one: an adoption over tables that
/// do not exist would seal a box into resolving from nothing, which is every ceiling absent and
/// every venue `paper`. `vike-cli config mirror` is what creates them.
pub fn write_adoption(
    db: &Path,
    adopted_at: &str,
    tool_version: &str,
    files_present: &str,
    venues_declared: bool,
) -> Result<Adoption, DbError> {
    if !database_present(db) {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    let (mut conn, created, _version) = open_for_write(db)?;
    if created {
        drop(conn);
        let _ = std::fs::remove_file(db);
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    if !table_exists(db, &conn, "setting")? || !table_exists(db, &conn, "venue_arming")? {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsTables });
    }

    let tx = conn.transaction().map_err(|e| DbError::sql(db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(db, e))?;
    let setting_rows: i64 = tx
        .query_row("SELECT count(*) FROM setting", [], |r| r.get(0))
        .map_err(|e| DbError::sql(db, e))?;
    let arming_rows: i64 = tx
        .query_row("SELECT count(*) FROM venue_arming", [], |r| r.get(0))
        .map_err(|e| DbError::sql(db, e))?;
    tx.execute("DELETE FROM settings_adoption", []).map_err(|e| DbError::sql(db, e))?;
    tx.execute(
        "INSERT INTO settings_adoption \
         (id, adopted_at, tool_version, files_present, venues_declared, setting_rows, arming_rows) \
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            adopted_at,
            tool_version,
            files_present,
            i64::from(venues_declared),
            setting_rows,
            arming_rows
        ],
    )
    .map_err(|e| DbError::sql(db, e))?;
    tx.commit().map_err(|e| DbError::sql(db, e))?;

    Ok(Adoption {
        adopted_at: adopted_at.to_string(),
        tool_version: tool_version.to_string(),
        files_present: files_present.to_string(),
        venues_declared,
        setting_rows: setting_rows.max(0) as usize,
        arming_rows: arming_rows.max(0) as usize,
    })
}

/// **Delete the ADOPTION SEAL** — `vike-cli config adopt --undo`, the rollback.
///
/// The rows are left exactly where they are and the four files answer again. That is the whole of
/// the rollback: ONE row, and no redeploy of a trading daemon. It is also why the crossing is safe
/// to rehearse — nothing about the data is undone, because nothing about the data was changed.
///
/// `Ok(false)` when there was no seal (an already-unadopted box is not an error to un-adopt).
pub fn clear_adoption(db: &Path) -> Result<bool, DbError> {
    if !database_present(db) {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    let (conn, created, _version) = open_for_write(db)?;
    if created {
        drop(conn);
        let _ = std::fs::remove_file(db);
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    if !table_exists(db, &conn, "settings_adoption")? {
        return Ok(false);
    }
    let removed =
        conn.execute("DELETE FROM settings_adoption", []).map_err(|e| DbError::sql(db, e))?;
    Ok(removed > 0)
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

/// **Does `table` carry `column`?** — asked of the live schema, never assumed from
/// [`crate::schema::DDL`].
///
/// ⚠ **The DDL cannot answer this and that is the whole reason the function exists.** Every table
/// there is `CREATE TABLE IF NOT EXISTS`, which does exactly nothing to a table that is already
/// present — so a column added to an existing table's DDL reaches a store BORN after the change and
/// no store born before it. Both boxes migrated on 2026-09-14 are stores born before, and their
/// `venue_arming` was itself created by a later `config mirror` run through that same `IF NOT
/// EXISTS` batch. A reader that selected an assumed column would fail on them with a SQL error
/// where the honest answer is `None`.
fn has_column(
    db: &Path,
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> Result<bool, DbError> {
    let mut stmt =
        conn.prepare(&format!("PRAGMA table_info({table})")).map_err(|e| DbError::sql(db, e))?;
    let mut rows = stmt.query([]).map_err(|e| DbError::sql(db, e))?;
    while let Some(r) = rows.next().map_err(|e| DbError::sql(db, e))? {
        let name: String = r.get(1).map_err(|e| DbError::sql(db, e))?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Add `venue_arming.max_exposure` where a store predates it. IDEMPOTENT, and a no-op on every
/// store born with the column.
///
/// ⚠ A plain `ADD COLUMN` rather than the rebuild-copy `credential` needed: that one had to change a
/// PRIMARY KEY, which SQLite's `ALTER TABLE` cannot do. Adding a NULLABLE column with no default is
/// the case `ALTER TABLE` handles directly and without rewriting a row, so there is nothing here to
/// interrupt halfway. The column's DATA needs no migration either — [`write_settings`] deletes and
/// re-inserts both tables on every run, so the next mirror fills it from the files.
fn ensure_arming_columns(db: &Path, tx: &rusqlite::Transaction<'_>) -> Result<(), DbError> {
    if !has_column(db, tx, "venue_arming", "max_exposure")? {
        tx.execute_batch("ALTER TABLE venue_arming ADD COLUMN max_exposure REAL;")
            .map_err(|e| DbError::sql(db, e))?;
    }
    Ok(())
}

/// **Upsert ONE `venue_setting` row**, returning the value it replaced — `None` where the row is
/// new. The operator's writer for the values `secrets move-venue-config` relocated.
///
/// ⚠ **Without it those ten values are read-only, which is the defect this workspace spent a day
/// removing for credentials.** `secrets set` writes the `credential` table, so after the move it
/// would not update a venue setting — it would create a SHADOW row, which the fold then reports as
/// a collision and resolves in the credential's favour. The operator would see their new value
/// ignored and no error anywhere.
///
/// ⚠ It touches ONE row. It does not clear the table, and nothing in this module may —
/// [`write_settings`]'s own ⚠ carries what a `DELETE FROM venue_setting` would cost.
///
/// # Errors
/// The engine, a store at an unreadable schema, or a `CHECK` the DDL enforces — a `tier` outside
/// `sim`/`demo`/`live` is refused HERE, by the database, which is the validation a `config` map
/// field could never have given.
pub fn set_venue_setting_in(
    settings_dir: &Path,
    venue: &str,
    tier: Option<&str>,
    field: &str,
    value: &str,
) -> Result<Option<String>, DbError> {
    let db = crate::dotenv::db_path_in(settings_dir);
    let (mut conn, _created, _version) = crate::db::open_for_write(&db)?;
    let tx = conn.transaction().map_err(|e| DbError::sql(&db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(&db, e))?;

    // The row it replaces, read inside the same transaction so the report cannot describe a value
    // some other writer changed in between.
    let previous: Option<String> = tx
        .query_row(
            "SELECT value FROM venue_setting WHERE venue = ?1 AND field = ?2 \
             AND tier IS ?3",
            (venue, field, tier),
            |r| r.get(0),
        )
        .ok();

    tx.execute(
        "INSERT INTO venue_setting (venue, tier, field, value) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT DO UPDATE SET value = excluded.value",
        (venue, tier, field, value),
    )
    .map_err(|e| DbError::sql(&db, e))?;
    tx.commit().map_err(|e| DbError::sql(&db, e))?;
    Ok(previous)
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
    // …and the one column the DDL above cannot add to a table that already exists.
    ensure_arming_columns(db, &tx)?;
    tx.execute("DELETE FROM setting", []).map_err(|e| DbError::sql(db, e))?;
    tx.execute("DELETE FROM venue_arming", []).map_err(|e| DbError::sql(db, e))?;
    // ⚠⚠ **`venue_setting` IS DELIBERATELY NOT CLEARED HERE, AND ADDING IT WOULD DESTROY DATA.**
    // This function re-derives every row FROM THE FILES, and venue settings have no file to be
    // derived from — that is the whole reason they are a table rather than a `config` key
    // ([`VenueSettingRow`] carries the argument). A `DELETE FROM venue_setting` beside the two
    // above would therefore not re-write those rows, it would simply remove them: one
    // `vike-cli config mirror` and a box loses its JForex server, its IBKR gateway and its
    // polymarket egress, silently, with the command reporting success.
    //
    // `the_mirror_does_not_touch_venue_settings` is the gate. It is a real risk rather than a
    // theoretical one: every other table this function knows about IS cleared, so the symmetry
    // argues for the deletion and only this note argues against it.
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
            .prepare(
                "INSERT INTO venue_arming (venue, label, mode, max_exposure) VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(|e| DbError::sql(db, e))?;
        for row in &rows.arming {
            stmt.execute(rusqlite::params![row.venue, row.label, row.mode, row.max_exposure])
                .map_err(|e| DbError::sql(db, e))?;
        }
    }
    // ⚠ **The SEAL's counts move with the tables, in the SAME transaction.** Without this line a
    // sanctioned mirror run on an adopted box would trip its own erase detector at the next boot,
    // which would teach an operator that the detector is noise — and a detector people have learned
    // to work around is worse than none. `UPDATE` rather than upsert: a box with no seal gets no
    // row, because writing one here would ADOPT the box from a mirror command.
    tx.execute(
        "UPDATE settings_adoption SET setting_rows = ?1, arming_rows = ?2 WHERE id = 1",
        rusqlite::params![rows.settings.len() as i64, rows.arming.len() as i64],
    )
    .map_err(|e| DbError::sql(db, e))?;
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
            venue: Vec::new(),
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
                ArmingRow {
                    venue: "binance".into(),
                    label: None,
                    mode: "demo".into(),
                    max_exposure: None,
                },
                ArmingRow {
                    venue: "hyperliquid".into(),
                    label: Some("ALT".into()),
                    mode: "paper".into(),
                    max_exposure: None,
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
            venue: Vec::new(),
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

    // --------------------------------------------------------------------------------------------
    // `venue_arming.max_exposure` — the column added to a table that already exists on two live
    // boxes. Every test here is about that asymmetry, because the DDL cannot express it: every
    // statement in it is `CREATE TABLE IF NOT EXISTS`, which does nothing at all to a table that is
    // already there.
    // --------------------------------------------------------------------------------------------

    /// A store planted the way both migrated boxes actually look: the settings tables exist, built
    /// by an earlier `config mirror`, and `venue_arming` has NO `max_exposure` column because the
    /// code that created it predates one.
    ///
    /// ⚠ The DDL here is the OLD shape, spelled out rather than derived, for the reason
    /// `plant_schema_1` gives about approximations: a fixture that built the old table by editing
    /// the new one would stop being the old one the day the new one changes again.
    fn planted_without_the_column(dir: &Path) -> PathBuf {
        let db = crate::dotenv::db_path_in(dir);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
        conn.execute_batch(
            "CREATE TABLE node_key (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
             CREATE TABLE venue_arming (
                 id    INTEGER PRIMARY KEY,
                 venue TEXT NOT NULL,
                 label TEXT,
                 mode  TEXT NOT NULL,
                 notes TEXT,
                 CHECK (mode IN ('paper', 'demo', 'live'))
             ) STRICT;
             CREATE TABLE setting (
                 id      INTEGER PRIMARY KEY,
                 section TEXT NOT NULL,
                 key     TEXT NOT NULL,
                 value   TEXT NOT NULL,
                 CHECK (section IN ('policy', 'config', 'preferences', 'flags'))
             ) STRICT;
             INSERT INTO venue_arming (venue, label, mode) VALUES ('binance', NULL, 'demo');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
        db
    }

    /// ⚠ **THE ONE THAT PROTECTS THE TWO MIGRATED BOXES.** A reader that selected an assumed column
    /// would fail with a SQL error on every store written before it existed — which is both boxes —
    /// and a settings read that ERRORS is not a degraded answer, it is a daemon that will not boot.
    /// The honest answer is `None`: that box states no per-account figure because it could not have.
    #[test]
    fn a_store_without_the_column_reads_back_with_no_figure() {
        let tmp = tempfile::tempdir().unwrap();
        let db = planted_without_the_column(tmp.path());

        let source = read_settings(&db).expect("a store predating the column must still READ");
        let SettingsSource::Rows { rows, .. } = source else {
            panic!("a planted store answers with rows");
        };
        assert_eq!(rows.arming.len(), 1);
        assert_eq!(rows.arming[0].venue, "binance");
        assert_eq!(rows.arming[0].max_exposure, None, "no column means no figure, never an error");
    }

    /// …and WRITING to that same store adds the column rather than failing — idempotently, so the
    /// second mirror run is a no-op. Without this the figure would be unwritable on exactly the
    /// boxes that already exist.
    #[test]
    fn writing_to_a_store_without_the_column_adds_it_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let db = planted_without_the_column(tmp.path());

        let mut with_figure = rows();
        with_figure.arming[0].max_exposure = Some(5000.0);

        write_settings(&db, &with_figure).expect("the first write adds the column");
        write_settings(&db, &with_figure).expect("the second write must be a no-op, not a failure");

        let SettingsSource::Rows { rows, .. } = read_settings(&db).expect("read back") else {
            panic!("rows");
        };
        let binance = rows.arming.iter().find(|r| r.venue == "binance").expect("the binance row");
        assert_eq!(binance.max_exposure, Some(5000.0));
    }

    /// The round trip on a store born WITH the column — the ordinary case, and the one that says the
    /// figure is carried rather than merely accepted.
    #[test]
    fn a_figure_survives_the_write_and_the_read() {
        let tmp = tempfile::tempdir().unwrap();
        let db = planted(tmp.path());

        let mut written = rows();
        written.arming[0].max_exposure = Some(50000.0);
        written.arming[1].max_exposure = Some(5000.0);
        write_settings(&db, &written).expect("write");

        let SettingsSource::Rows { rows, .. } = read_settings(&db).expect("read") else {
            panic!("rows");
        };
        assert_eq!(rows.sorted(), written.sorted(), "the rows must come back exactly as written");
    }

    /// ⚠ **The store refuses a non-positive figure too**, and that second gate is not redundant with
    /// the loader's: this one is what a hand `INSERT` meets. A ceiling of zero would become the
    /// BINDING one under the mount's `min` fold and refuse every order on that account.
    #[test]
    fn the_store_refuses_a_non_positive_figure() {
        let tmp = tempfile::tempdir().unwrap();
        let db = planted(tmp.path());
        let conn = rusqlite::Connection::open(&db).unwrap();
        for bad in ["0.0", "-1.0"] {
            let err = conn
                .execute_batch(&format!(
                    "INSERT INTO venue_arming (venue, label, mode, max_exposure) \
                     VALUES ('okx', NULL, 'demo', {bad});"
                ))
                .expect_err("the DDL's CHECK must refuse it");
            assert!(
                err.to_string().to_lowercase().contains("check"),
                "refused by the CHECK rather than by accident ({bad}): {err}"
            );
        }
    }
}

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
mod mirror_leaves_venue_settings_alone {
    use super::*;

    /// The rows a mirror would carry: everything EXCEPT venue settings, which no file can spell.
    fn from_files() -> StoredSettings {
        StoredSettings {
            settings: vec![SettingRow {
                section: "policy".into(),
                key: "max_leverage".into(),
                value: "3.0".into(),
            }],
            arming: Vec::new(),
            venue: Vec::new(),
        }
    }

    #[test]
    fn a_mirror_does_not_delete_the_moved_venue_rows() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::dotenv::db_path_in(tmp.path());

        // A box that has run the move: one tier-scoped row and one machine-scoped one.
        {
            let (conn, _created, _v) = crate::db::open_for_write(&db).expect("open");
            conn.execute_batch(crate::schema::DDL).expect("ddl");
            conn.execute(
                "INSERT INTO venue_setting (venue, tier, field, value) VALUES (?1, ?2, ?3, ?4)",
                ("dukascopy", Some("demo"), "SERVER", "https://example.invalid/x.jnlp"),
            )
            .expect("tier-scoped row");
            conn.execute(
                "INSERT INTO venue_setting (venue, tier, field, value) \
                 VALUES (?1, NULL, ?2, ?3)",
                ("polymarket", "PROXY_HOST", "127.0.0.1"),
            )
            .expect("machine-scoped row");
            // The reader refuses an unstamped store, and rightly — a fixture that skips this is
            // testing the version check rather than the mirror.
            conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).expect("stamp");
        }

        // The fixture has to actually be there, or the assertion below proves nothing.
        let before = match read_settings(&db).expect("read") {
            SettingsSource::Rows { rows, .. } => rows.venue,
            other => panic!("expected rows, got {other:?}"),
        };
        assert_eq!(before.len(), 2, "the fixture did not land: {before:?}");

        // THE MIRROR — rows derived from files, carrying no venue settings at all.
        write_settings_in(tmp.path(), &from_files()).expect("mirror");

        let after = match read_settings(&db).expect("read back") {
            SettingsSource::Rows { rows, .. } => rows,
            other => panic!("expected rows, got {other:?}"),
        };
        assert_eq!(
            after.venue, before,
            "a mirror DELETED venue settings — a box just lost its JForex server, its IBKR \
             gateway and its polymarket egress, and `config mirror` reported success"
        );
        // …and the control: the mirror really did run, so the survival above is the guard working
        // rather than the writer having done nothing at all.
        assert_eq!(after.settings.len(), 1, "the mirror wrote no setting rows: {after:?}");
        assert_eq!(after.settings[0].key, "max_leverage");
    }
}

/// **The operator's writer for a moved venue setting**, and the DDL constraint that is the whole
/// reason these values are a table rather than a field on `Config`.
#[cfg(test)]
mod venue_setting_writer {
    use super::*;

    fn empty_store() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::dotenv::db_path_in(tmp.path());
        let (conn, _c, _v) = crate::db::open_for_write(&db).expect("open");
        conn.execute_batch(crate::schema::DDL).expect("ddl");
        conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).expect("stamp");
        drop(conn);
        (tmp, db)
    }

    fn rows_of(db: &std::path::Path) -> Vec<VenueSettingRow> {
        match read_settings(db).expect("read") {
            SettingsSource::Rows { rows, .. } => rows.venue,
            other => panic!("expected rows, got {other:?}"),
        }
    }

    /// A new row reports `None`; writing it again reports what it replaced and does NOT duplicate.
    #[test]
    fn a_write_upserts_and_reports_what_it_replaced() {
        let (tmp, db) = empty_store();

        let first = set_venue_setting_in(tmp.path(), "dukascopy", Some("demo"), "SERVER", "first")
            .expect("write");
        assert_eq!(first, None, "a new row replaced nothing");

        let second =
            set_venue_setting_in(tmp.path(), "dukascopy", Some("demo"), "SERVER", "second")
                .expect("rewrite");
        assert_eq!(second.as_deref(), Some("first"), "the replaced value is reported");

        let rows = rows_of(&db);
        assert_eq!(rows.len(), 1, "the upsert DUPLICATED the row: {rows:?}");
        assert_eq!(rows[0].value, "second");
    }

    /// ⚠ **THE ARGUMENT FOR THE TABLE, AS BEHAVIOUR.** A mistyped tier is refused by the DATABASE.
    ///
    /// This is what a `venue` map field on `vike_config::Config` could never have given: that shape
    /// accepts any map key, so `venue.ibkr.demoo.backend` would have been stored, read by nothing,
    /// and silent. `Config` carries `#[serde(deny_unknown_fields)]` precisely so an unknown key is
    /// refused by name, and a map field would have made the venue subtree the one place in the
    /// settings model where that stops being true — at sixteen roster venues, the largest
    /// unguarded surface in it.
    #[test]
    fn a_mistyped_tier_is_refused_by_the_database() {
        let (tmp, db) = empty_store();

        let bad = set_venue_setting_in(tmp.path(), "ibkr", Some("demoo"), "BACKEND", "cpapi");
        assert!(bad.is_err(), "a tier outside the DDL's CHECK was accepted: {bad:?}");
        assert!(rows_of(&db).is_empty(), "a refused write left a row behind");

        // …and the control: the same call with a REAL tier lands, so the refusal above is the
        // CHECK biting rather than the writer being broken for every input.
        set_venue_setting_in(tmp.path(), "ibkr", Some("demo"), "BACKEND", "cpapi").expect("good");
        assert_eq!(rows_of(&db).len(), 1);
    }

    /// ⚠ A MACHINE-SCOPED row (`tier = NULL`) and a tier-scoped one are different namespaces, which
    /// the two partial unique indexes encode. The polymarket proxy is the family that takes the
    /// first shape.
    #[test]
    fn machine_scoped_and_tier_scoped_are_separate_rows() {
        let (tmp, db) = empty_store();

        set_venue_setting_in(tmp.path(), "polymarket", None, "PROXY_HOST", "127.0.0.1")
            .expect("machine-scoped");
        set_venue_setting_in(tmp.path(), "polymarket", Some("live"), "PROXY_HOST", "<host>")
            .expect("tier-scoped");

        let rows = rows_of(&db);
        assert_eq!(rows.len(), 2, "the two scopes collapsed onto one row: {rows:?}");

        // …and re-writing the machine-scoped one updates IT, not its tier-scoped neighbour.
        let was = set_venue_setting_in(tmp.path(), "polymarket", None, "PROXY_HOST", "127.0.0.2")
            .expect("rewrite");
        assert_eq!(was.as_deref(), Some("127.0.0.1"), "it replaced the machine-scoped value");
        let rows = rows_of(&db);
        assert_eq!(rows.len(), 2, "still two rows");
        let tiered = rows.iter().find(|r| r.tier.is_some()).expect("the tier-scoped row survives");
        assert_eq!(tiered.value, "<host>", "the neighbour was overwritten");
    }
}
