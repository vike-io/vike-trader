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

use std::collections::BTreeMap;
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
    /// No section prefix: the `policy` section holds `max_leverage`, not `policy.max_leverage`.
    pub key: String,
    /// One JSON scalar — `1.0`, `"warn"`, `true`.
    pub value: String,
}

/// One row of the `venue_arming` table: an arming CEILING for a venue, or for one labelled account
/// of a venue.
///
/// `label = None` is the `policy.venues.<venue>` row for that venue; `Some(label)` is its
/// `policy.accounts.<venue>.<LABEL>` row. Both are ceilings and both can only ever refuse.
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
/// that shape: one process has one declared egress (`vike_polymarket::declare_egress`), so it is not
/// account-aware and cannot become so without that declaration changing shape. The DDL's total
/// `UNIQUE (venue, tier, field)` keeps the two scopes apart, because the
/// table stores that `None` as the word `'any'` — a real value a `UNIQUE` constrains. (⚠ Until §5.2
/// step 7 this read *"The DDL's two partial unique indexes keep the two scopes apart"*: the table
/// stored SQL NULL, which a `UNIQUE` does not constrain, so one index per scope was needed and the
/// NULL decided which applied — the shape owner ruling 2 refuses.)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VenueSettingRow {
    /// The roster venue id, lowercase — `vike_model::VENUES`' spelling.
    pub venue: String,
    /// `paper` / `demo` / `live`, or `None` for a machine-scoped value. The DDL `CHECK`s this
    /// list. ⚠ `paper` spelled `sim` before the 2026-09-23 rename (`crate::ACCOUNT_TIERS`);
    /// `crate::schema::migrate_sim_tier_to_paper` rewrites a stored row and
    /// `crate::venue_setting::venue_setting_names` still renders its legacy `_SIM_` key name.
    ///
    /// ⚠ **`None` is the RUST spelling only.** The column stores `'any'` for it since §5.2 step 7
    /// (SQL NULL before), and nothing outside `crate::schema::stored_venue_setting_tier` and
    /// `crate::schema::venue_setting_tier_of_stored` may translate between the two: a stored
    /// `'any'` that reached this field would render the legacy name `{HEAD}_ANY_{FIELD}`, which no
    /// store holds, and the row would stop answering with nothing to say why.
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
    /// every row on every box that has stated no `policy.account_exposure` figure.
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
    /// What a column table buys instead is real: the DDL's
    /// `CHECK (tier IN ('any','paper','demo','live'))` refuses a mistyped tier at write time, and
    /// its `UNIQUE (venue, tier, field)` encodes that a machine-scoped row (`'any'`) and a
    /// tier-scoped one are different rows rather than two spellings nobody compares. (⚠ Until §5.2
    /// step 7 this quoted `CHECK (tier IN ('paper','demo','live'))` and credited *"the two partial
    /// unique indexes"* with the second half — the NULL-discriminated shape step 7 retired.)
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
///   `is_declared` is a fact about whether an operator EVER STATED an arming, and a box with no
///   `policy.venues.<venue>` rows holds ZERO arming rows deliberately — so once the rows are the
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
    /// Which of the four settings files existed at adoption, comma-separated in the order
    /// `vike_config::SECTION_FILES` listed them, or empty for none. Scopes the drift report, and
    /// tells a later deletion PR what it is deleting. ⚠ That constant was deleted with the files on
    /// 2026-09-27 (#2210, docs/decisions/0086), and the in-tree writers pass this empty since; it
    /// was cited as a live order here until 2026-09-28.
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
                "{} setting rows and {} venue-arming rows — sealed {} by {}",
                rows.settings.len(),
                rows.arming.len(),
                a.adopted_at,
                a.tool_version
            ),
            SettingsSource::Rows { rows, adopted: None } => write!(
                f,
                "{} setting rows and {} venue-arming rows — no integrity seal yet (`docs/decisions/\
                 0086`: there is no second source for these rows to answer below any more)",
                rows.settings.len(),
                rows.arming.len()
            ),

            SettingsSource::NoDatabase { path } => {
                write!(
                    f,
                    "no settings database at {} — every key resolves to its compiled-in default",
                    path.display()
                )
            }
            SettingsSource::TablesAbsent { path } => write!(
                f,
                "the settings database {} carries no settings tables yet — it was migrated before \
                 they existed, and the first `vike-cli config set` creates them",
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

    let rows = read_rows(&conn, db)?;

    // ...and the SEAL, from the same open. It is read LAST and it is read unconditionally: a store
    // whose `settings_adoption` table is absent is a store written before the seal existed, which
    // is `None` — the same answer as an adopted-then-undone box, and the right one for both.
    let adopted = if table_exists(db, &conn, "settings_adoption")? {
        read_adoption(db, &conn)?
    } else {
        None
    };

    Ok(SettingsSource::Rows { rows, adopted })
}

/// **The three settings tables' rows, over a connection the caller already holds** — the shared body
/// [`read_settings`] wraps (read-only, with the `NoDatabase`/`TablesAbsent` guards and the seal) and
/// [`write_setting_row_in`] calls directly (write-path, INSIDE its own transaction, where the tables
/// are guaranteed present by the DDL batch that runs ahead of it).
///
/// Takes `&rusqlite::Connection` rather than a generic so a `&rusqlite::Transaction<'_>` reaches it
/// by the ordinary `Deref` coercion every other reader/writer pair in this module already relies on.
fn read_rows(conn: &rusqlite::Connection, db: &Path) -> Result<StoredSettings, DbError> {
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
        // The venue by its NUMBER (`crate::schema::VenueLink`); the text answers only for a row
        // with none, which the read-only half of this function can meet on a store no writer has
        // carried yet.
        let link = crate::schema::VenueLink::of(conn, "venue_arming", "t")
            .map_err(|e| DbError::sql(db, e))?;
        // ⚠ The column is SELECTED only where it exists — see [`has_column`]. A store written
        // before it was added still answers, with `None` for every row, which is the truthful
        // reading: that box states no per-account figure because it could not have.
        let exposure = match has_column(conn, "venue_arming", "max_exposure")
            .map_err(|e| DbError::sql(db, e))?
        {
            true => "t.max_exposure",
            false => "NULL",
        };
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {}, t.label, t.mode, {exposure} FROM venue_arming t {} ORDER BY 1, 2",
                link.name, link.join
            ))
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
    if table_exists(db, conn, "venue_setting")? {
        let link = crate::schema::VenueLink::of(conn, "venue_setting", "t")
            .map_err(|e| DbError::sql(db, e))?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {}, t.tier, t.field, t.value FROM venue_setting t {} ORDER BY 1, 2, 3",
                link.name, link.join
            ))
            .map_err(|e| DbError::sql(db, e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(VenueSettingRow {
                    venue: r.get(0)?,
                    // ⚠ §5.2 step 7's READ boundary, and the one place a stored tier becomes a
                    // Rust one. `'any'` AND a not-yet-migrated NULL both read as `None`; the
                    // function's own doc carries why an unmapped `'any'` is a silent outage.
                    tier: crate::schema::venue_setting_tier_of_stored(r.get(1)?),
                    field: r.get(2)?,
                    value: r.get(3)?,
                })
            })
            .map_err(|e| DbError::sql(db, e))?;
        for row in rows {
            venue.push(row.map_err(|e| DbError::sql(db, e))?);
        }
    }

    Ok(StoredSettings { settings, arming, venue }.sorted())
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
///
/// `pub(crate)` and in the bare `rusqlite::Result` currency rather than [`DbError`]: `crate::db`'s
/// `ensure_venue_id_columns` needs this exact check and sits inside [`crate::db::fill_into`]'s
/// transaction, which is `rusqlite::Result` throughout — `DbError` needs a `path` that call has no
/// cheap way to attach at that depth, and the wrap belongs once at the outer boundary, exactly as
/// [`crate::db::ensure_venue_rows`]'s errors are wrapped by [`crate::db`]'s `write_pending`. Both
/// callers of this function INSIDE this module already hold `db` and attach it themselves with
/// [`DbError::sql`].
pub(crate) fn has_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let name: String = r.get(1)?;
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
    if !has_column(tx, "venue_arming", "max_exposure").map_err(|e| DbError::sql(db, e))? {
        tx.execute_batch("ALTER TABLE venue_arming ADD COLUMN max_exposure REAL;")
            .map_err(|e| DbError::sql(db, e))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// `account.armed` — the arming rows, folded onto the rows they arm (spec §9 stage 3)
// ---------------------------------------------------------------------------------------------

/// `paper`, the mode word this crate may not import from `vike_config::VenueMode::as_str`.
const PAPER: &str = "paper";

/// **Does this store carry `table`?** — the `rusqlite::Result` twin of [`table_exists`], which
/// needs a `db: &Path` in order to build a [`DbError`] its callers want and this one has not got.
/// Same question, same `sqlite_master` probe, and deliberately not a second ANSWER: both ask the
/// live schema rather than assuming [`crate::schema::DDL`].
fn table_present(conn: &rusqlite::Connection, name: &str) -> rusqlite::Result<bool> {
    let found: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(found > 0)
}

/// **The arming vocabulary's ORDER, as a rank.** `paper < demo < live`.
///
/// ⚠ It exists because the words do NOT sort that way: lexically `'demo' < 'live' < 'paper'`, so a
/// `WHERE mode > 'paper'` — or a `.min()` over `&str` — arms NOTHING and reads as a clean pass.
/// The rank is the same order `vike_config::VenueMode`'s derived `Ord` carries, which is the
/// authority; this crate declares no edge to it (layer 15 against layer 20) and
/// `crates/vike-config/tests/no_ceiling_widens_across_the_migration.rs` is what holds the two
/// equal. An unrecognised word ranks as `paper`, which is the same disposition
/// `VenuePolicy::get` takes for a venue it does not carry — the safe direction, and unreachable
/// anyway while `crate::schema::DDL`'s `CHECK (mode IN ('paper', 'demo', 'live'))` stands.
fn mode_rank(mode: &str) -> u8 {
    match mode {
        "live" => 2,
        "demo" => 1,
        _ => 0,
    }
}

/// `vike_config::VenueMode::cap` over the stored words — *the effective tier is the LOWER of the
/// operator's ceiling and whatever was decided*, `min` and never `max`.
fn cap<'a>(ceiling: &'a str, stated: &'a str) -> &'a str {
    if mode_rank(stated) < mode_rank(ceiling) { stated } else { ceiling }
}

/// **One account's ceiling, reproduced from the stored arming rows** — the three arms of
/// `crates/vike-config/src/venue_mode.rs`'s `VenuePolicy::account`, which this crate cannot call.
///
/// ```text
/// (Some(stated), _)   => venue_ceiling.cap(stated)   a labelled account's own line, CAPPED
/// (None, default)     => venue_ceiling               the default account INHERITS
/// (None, labelled)    => paper                       a labelled one does NOT
/// ```
///
/// ⚠ **All three arms are load-bearing and this file has watched each of the other spellings arm
/// an account nobody armed.** Reading the venue row for a LABELLED account arms a
/// `{VENUE}_LIVE_API_KEY__ALT` that no `[accounts]` line ever named; reading the labelled row
/// WITHOUT the cap arms an `ALT = "live"` line that its venue's own `demo` line capped on read.
/// `crates/vike-config/tests/no_ceiling_widens_across_the_migration.rs` runs both as kill proofs
/// beside the shipped fold.
fn account_ceiling<'a>(
    venue_modes: &'a BTreeMap<String, String>,
    labelled: &'a BTreeMap<(String, String), String>,
    venue: &str,
    label: Option<&str>,
) -> &'a str {
    // An absent venue row is `paper`: `VenuePolicy::get`'s *"an id the roster does not carry
    // answers `VenueMode::Paper` — the safe answer"*, and the same answer a box with no `[venues]`
    // table already resolves.
    let venue_ceiling = venue_modes.get(venue).map_or(PAPER, String::as_str);
    let Some(label) = label else { return venue_ceiling };
    match labelled.get(&(venue.to_string(), label.to_string())) {
        Some(stated) => cap(venue_ceiling, stated),
        None => PAPER,
    }
}

/// **Derive `account.armed` from the `venue_arming` rows.** IDEMPOTENT, and a pure function of the
/// two tables it reads.
///
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §9 stage 3, §5.2 step 5.
/// An account is ARMED exactly where the operator's own mode, resolved AT THAT ACCOUNT'S LEVEL of
/// the policy, names the tier the account already carries — so the new model's `armed ? tier :
/// paper` answers what the old model's `min(venue line, account line)` answered, and **it cannot
/// widen by construction**: an armed row comes out at a tier its own ceiling already allowed, and
/// a disarmed one comes out `paper`. It narrows exactly where a venue held several tiers and only
/// one was mounted; those rows were not trading before either, and `tier` is untouched, so
/// re-arming one is a policy line rather than a repair.
///
/// # ⚠ Every row is UPDATEd by `id`, never by its cells
///
/// Two dukascopy books share `(dukascopy, demo, NULL)` — SQLite's NULLs are distinct in an index,
/// so `UNIQUE (venue, tier, label)` admits both and that is the real shape of a real store. A
/// statement keyed on that tuple would write both rows from one account's answer. `id` is the
/// ruled identity, and here it is also the only key that separates them.
///
/// # ⚠ What this does NOT do: it does not drop `venue_arming`, and §3's *DELETED* is not this
/// stage
///
/// The table stays the SOURCE, and this column is its derived half — the shape stage 2 already
/// shipped for `venue_id` and the DDL's own doc called *a dual-write half with no reader yet*
/// (until the venue-links plan moved every reader onto it). **RULED here rather than assumed**,
/// because the spec's §5.2 step 5 reads as though the
/// table goes in one act:
///
/// * The settings mirror (`crates/vike-config/src/mirror.rs`, before `docs/decisions/0086` retired
///   its file-to-row direction) wrote one arming row per ROSTER venue whenever `[venues]` was
///   declared, and one per `[account_exposure]` figure. An `account` row exists only where a
///   CREDENTIAL minted one. So a venue an operator has declared and not
///   yet credentialled — and every `max_exposure` figure filed against one — has nowhere to live
///   in `account`, and **an absent figure means UNBOUNDED**: moving the exposure ceiling in this
///   stage would WIDEN it, in the one stage annotated as the one that must not widen, and §5.3's
///   gate compares venue MODES and would not see it.
/// * The same hole reaches the read path. `read_settings` is what `vike_config::apply_rows` builds
///   `VenuePolicy` from, so unfolding these rows back out of `account` alone would drop a
///   declared venue's line, `VenuePolicy::is_declared` with it on a box whose venues are all
///   uncredentialled, and the adoption seal's `arming_rows` ERASE detector would read the loss as
///   rows moved by some other route.
///
/// So `max_exposure` does NOT move in this stage and `venue_arming` is NOT dropped; the three
/// `table_exists(…, "venue_arming")` guards in this module are therefore correct as they stand.
/// What must land before the table can go is a home for a ceiling that names no account —
/// which is a question §3's schema does not answer today.
///
/// # Where it is called
///
/// Everywhere either input changes, which is what makes the column derived rather than stored:
/// [`write_settings`] (the mirror rewrites every arming row), `crate::db::fill_into` (a credential
/// write mints account rows) and `crate::db::commit_account_write` (the account verbs). Cheap by
/// construction — both tables are tens of rows on a real box.
///
/// ⚠ **It therefore also runs inside `crate::db::preview_rows`' in-memory REPLICA, where its
/// answer is meaningless and read by nothing.** That replica is rebuilt from `(name, value)`
/// credentials, so its `venue_arming` table is EMPTY however many rows the real store holds, and
/// every bit comes out `false`. Nothing in `crate::schema::RowReport` carries an `armed` bit, and
/// the transaction is rolled back — the same class as that function's own warning that a predicted
/// `account.id` is not the id the apply will assign.
pub(crate) fn fold_arming_into_accounts(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    // The column, where a store predates it. Same `ALTER TABLE` case `ensure_arming_columns`
    // argues for: SQLite adds a column with a NON-NULL default directly, without rewriting a row.
    // ⚠ The table-level `CHECK (armed IN (0, 1))` cannot ride an `ADD COLUMN`, so an older store
    // carries the column without it — see `crate::schema::DDL`'s own note.
    // ⚠ The `account` guard is DEFENSIVE and unreachable through any caller today — all three run
    // `crate::schema::DDL`'s `CREATE TABLE IF NOT EXISTS` batch first. It is here because the
    // failure it removes is not a refusal: without it, a caller that did not would reach the
    // `ALTER TABLE` below and take a bare `no such table: account` out of the engine, on a path
    // whose whole currency is `rusqlite::Result`.
    if !table_present(tx, "account")? {
        return Ok(());
    }
    if !has_column(tx, "account", "armed")? {
        tx.execute_batch("ALTER TABLE account ADD COLUMN armed INTEGER NOT NULL DEFAULT 0;")?;
    }
    // A store with no `venue_arming` table states no arming at all, and there is nothing to derive
    // FROM. Leaving every bit at its default `0` is the truthful reading — the same one
    // `read_settings` takes of a store whose tables predate it — and never a reason to error.
    if !table_present(tx, "venue_arming")? {
        return Ok(());
    }

    // ⚠ Both tables are read by the venue's NUMBER (`crate::schema::VenueLink`), with the text
    // only for a row that has none. Not a formality: decision 0095's boot-path migration calls this
    // fold on the store AS FOUND, before the funnel has carried it
    // (`crate::live_means_mainnet::migrate_live_means_mainnet`), so this function can meet rows
    // with no number and tables with no `venue_id` column; every other caller has run the funnel.
    let mut venue_modes: BTreeMap<String, String> = BTreeMap::new();
    let mut labelled: BTreeMap<(String, String), String> = BTreeMap::new();
    {
        let link = crate::schema::VenueLink::of(tx, "venue_arming", "t")?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {}, t.label, t.mode FROM venue_arming t {}",
            link.name, link.join
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (venue, label, mode) = row?;
            match label {
                None => {
                    venue_modes.insert(venue, mode);
                }
                Some(label) => {
                    labelled.insert((venue, label), mode);
                }
            }
        }
    }

    let accounts: Vec<(i64, String, String, Option<String>)> = {
        let link = crate::schema::VenueLink::of(tx, "account", "a")?;
        let mut stmt = tx.prepare(&format!(
            "SELECT a.id, {}, a.tier, a.label FROM account a {}",
            link.name, link.join
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out
    };

    let mut update = tx.prepare("UPDATE account SET armed = ?1 WHERE id = ?2")?;
    for (id, venue, tier, label) in accounts {
        let ceiling = account_ceiling(&venue_modes, &labelled, &venue, label.as_deref());
        // ⚠ A DIRECT comparison since §4.4's rename, and it was a mapped one — `mode_word_of_tier`,
        // deleted with this line's change. `account.tier` spelled "no real broker connection"
        // `sim` while `venue_arming.mode` spelled it `paper`, so a fold comparing the two columns
        // raw matched NOTHING while looking like a clean pass. One word for one idea (ruling 7)
        // removes the map rather than fixing it; `crate::schema::ACCOUNT_TIERS` and
        // `vike_config::VenueMode`'s vocabulary are now the same three words.
        let armed = ceiling == tier;
        update.execute(rusqlite::params![i64::from(armed), id])?;
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
/// `tier` is `None` for a machine-scoped row; the table stores that as `'any'` since §5.2 step 7,
/// and this function is one of the two writers that translate
/// (`crate::schema::stored_venue_setting_tier` is the translation). ⚠ `Some("any")` is NOT a Rust
/// tier and no production caller can produce one — `crate::venue_setting::parse_venue_setting_key`
/// classifies
/// by `crate::schema::account_tier_named`, which does not know the word — and it is REFUSED here,
/// before the store is opened. ⚠ This paragraph used to end *"Were one passed, it would land on the
/// machine-scoped row, which is what the stored word means, never on a second row"*, which was
/// what the code did and contradicted the `# Errors` section below: step 7 added `'any'` to the
/// `CHECK`, so the database no longer refused the word, and the write overwrote the machine-scoped
/// row and reported its old value with nothing erroring. Review found the two disagreeing; the
/// refusal is `stored_venue_setting_tier`'s, so the `# Errors` promise is true again.
///
/// # Errors
/// The engine, a store at an unreadable schema, or a `CHECK` the DDL enforces — a `tier` outside
/// `paper`/`demo`/`live` is refused HERE, by the database, which is the validation a `config` map
/// field could never have given. `Some("any")` — the one word the `CHECK` admits that is not a
/// tier — is refused by the write boundary instead, and nothing is opened or migrated for it.
pub fn set_venue_setting_in(
    settings_dir: &Path,
    venue: &str,
    tier: Option<&str>,
    field: &str,
    value: &str,
) -> Result<Option<String>, DbError> {
    let db = crate::dotenv::db_path_in(settings_dir);
    // ⚠ §5.2 step 7's WRITE boundary — the tier is bound as the STORED word, in the lookup and the
    // upsert below alike, and it is resolved BEFORE the store is opened so a refused `Some("any")`
    // opens nothing and migrates nothing. The funnel below carries the table onto
    // `tier TEXT NOT NULL`, so the `Option` itself would bind SQL NULL: refused outright by the
    // INSERT, and — the quiet half — matched by NOTHING in the lookup, which is well-formed either
    // way and would report `None` for a machine-scoped row that plainly holds a value. (This lookup
    // read `tier IS ?3` over the raw `Option` until step 7.)
    let stored_tier =
        crate::schema::stored_venue_setting_tier(tier).map_err(|e| DbError::sql(&db, e))?;
    let (mut conn, _created, _version) = crate::db::open_for_write(&db)?;
    let tx = conn.transaction().map_err(|e| DbError::sql(&db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(&db, e))?;
    // ⚠ This writer names `venue_id` in its own `venue_setting` INSERT below — see
    // `crate::db::ensure_venue_id_columns`'s own doc for why a store that predates the column needs
    // this call rather than the bare DDL batch above.
    crate::db::ensure_venue_id_columns(&tx).map_err(|e| DbError::sql(&db, e))?;

    // The row it replaces, read inside the same transaction so the report cannot describe a value
    // some other writer changed in between. Found by the venue's NUMBER: the funnel above has
    // carried the store, so every row holds one (`crate::schema::venue_is`).
    let previous: Option<String> = tx
        .query_row(
            &format!(
                "SELECT value FROM venue_setting WHERE {} AND field = ?2 AND tier = ?3",
                crate::schema::venue_is("venue_id", "?1")
            ),
            (venue, field, stored_tier),
            |r| r.get(0),
        )
        .ok();

    // ⚠ `venue_id = excluded.venue_id` on the conflict path too, not only `value` — DEFENSIVE, and a
    // DECLARED BLIND SPOT rather than a covered branch, in the same spirit
    // `crates/vike-secrets/tests/store_link_gate.rs` declares `venue_arming.label` as a link its own
    // rules cannot see rather than pretending otherwise.
    //
    // It makes this statement correct ON ITS OWN, rather than by depending on
    // `ensure_venue_id_columns` above (in this same function) having already filled every NULL —
    // i.e. on a call in ANOTHER function rather than on this one.
    //
    // ⚠ **It cannot fire today, and here is why**: `crate::schema::DDL`'s `UNIQUE (venue, tier,
    // field)` keys on `venue` FIRST, so `venue` sits INSIDE this table's conflict key. (⚠ This named
    // the two partial indexes `venue_setting_one_per_tier` and `venue_setting_one_per_machine` until
    // §5.2 step 7 collapsed them into that one total `UNIQUE`; both keyed on `venue` first as well,
    // so the argument survives the change unaltered.) A row can only ever conflict
    // with an incoming row naming the SAME `venue` text, and `venue_id` is a pure function of that
    // text (the sub-select above) — so a conflicting row's existing `venue_id`, if it was ever
    // correctly set, is BY CONSTRUCTION the same value `excluded.venue_id` would carry. There is no
    // sequence of calls to this function that can make an existing row's `venue_id` need to CHANGE
    // on a conflict. Since the venue-links flip `venue_id` sits inside a conflict key too — the
    // second total `UNIQUE (venue_id, tier, field)` — and a conflict on THAT key is a row holding
    // the same `venue_id` already, so the clause sets it to itself and the argument holds for both.
    // (The untargeted `ON CONFLICT` is what lets one `DO UPDATE` answer whichever of the two
    // fires.) ⚠ Since the plan's second release the shipped table has no text `venue` and no
    // text-keyed `UNIQUE`, so on a carried store the number's `UNIQUE` is the only conflict key and
    // that last argument is the whole of it; the text half above still describes a store no writer
    // of that release has carried yet.
    //
    // What WOULD make it fire: a writer that leaves a STALE, non-NULL `venue_id` on a row — nothing
    // in this crate does. `ensure_venue_id_columns` heals only `venue_id IS NULL`; it would NOT
    // correct a stale-but-non-NULL value, so if such a writer ever existed, this clause is what would
    // catch up behind it.
    //
    // ⚠ **This is therefore NOT regression-guarded by any test, deliberately, and a mutation of this
    // clause will NOT redden the suite** — MEASURED: `every_writer_names_its_venue_by_number_on_every_row`
    // (`venue_id_agrees_with_the_text_column_on_every_row` until the plan's second release)
    // exercises this exact `ON CONFLICT` branch (a second `set_venue_setting_in` call for the same
    // key) and stays green with this clause removed, for precisely the reason above. Do not delete
    // this line after finding no test fails for it; that absence is the point being documented, not
    // evidence the line is dead code.
    let link = crate::schema::VenueLink::of(&tx, "venue_setting", "t")
        .map_err(|e| DbError::sql(&db, e))?;
    tx.execute(
        &format!(
            "INSERT INTO venue_setting ({}, tier, field, value) VALUES ({}, ?2, ?3, ?4) \
             ON CONFLICT DO UPDATE SET value = excluded.value, venue_id = excluded.venue_id",
            link.columns,
            link.values("?1")
        ),
        (venue, stored_tier, field, value),
    )
    .map_err(|e| DbError::sql(&db, e))?;
    tx.commit().map_err(|e| DbError::sql(&db, e))?;
    Ok(previous)
}

/// What a SECRET venue field's value is written as in the change journal.
const REDACTED: &str = "<secret>";

/// **[`set_venue_setting_in_journalled`]'s `Err` — the write's own refusal, paired with whether
/// recording THAT refusal in the change journal also succeeded.** Boxed: clippy's
/// `result_large_err` correctly refuses a bare `(DbError, Option<JournalAppendError>)` inline in
/// every `Result` this function's callers propagate (`DbError` alone stays unboxed everywhere
/// else in this crate; it is the PAIR that crosses the threshold), and `type_complexity` asks for
/// the tuple to be named rather than spelled at the return type. A type alias over a raw tuple
/// rather than a named struct: the two cells have no name of their own beyond "the write's error"
/// and "the journal's", which a struct's field names would only restate.
pub type VenueSettingRefusal = Box<(DbError, Option<crate::JournalAppendError>)>;

/// **[`set_venue_setting_in`], plus its `set_setting` record in the change journal** — the
/// venue-setting twin of [`crate::save_credentials_to_store_journalled`], and the ONE journalled
/// `venue_setting` writer (`vike-cli config set venue.*` and the desktop both reach it).
///
/// The journal is the EXISTING change journal every settings write uses
/// (`vike_model::change_journal`, beside the store: `<settings_dir>/state/changes`), with `file` =
/// `venue` and the operator's dotted key. BOTH outcomes are recorded, as `vike-cli`'s
/// `settings_write` records them: a refused write carries its reason. ⚠ A `secret` field's value —
/// old and new — is recorded as `<secret>`, never verbatim.
///
/// A journal failure cannot fail the CALL — the underlying write's own outcome (applied or
/// refused) is always reported unchanged — but it must never be swallowed either, on EITHER
/// outcome: it comes back as `Some(JournalAppendError)`, paired with whichever half of the result
/// it belongs to, for the caller to report.
///
/// ⚠ **This is the shape `crates/vike-cli/src/cmd/settings_write.rs`'s
/// `set_setting_journalled_within` already uses for the identical problem, carried here as
/// RETURNED DATA rather than as an `eprintln!`** — this crate carries no logging dependency and
/// hands findings to its caller instead (see the crate doc's Redaction section, and
/// [`crate::JournalAppendError`]'s own doc). A prior version of this function paired the journal
/// outcome with the result via `result.map(|previous| (previous, journal_error))`, which is a
/// no-op on `Err` — so a REFUSED write whose OWN refusal record also failed to append reported
/// only the refusal, and the caller had no way to learn the ledger did not record it either.
/// Found in review before any downstream task depended on the signature; the `Err` arm now carries
/// the same pair the `Ok` arm always has, rather than silently dropping half of it.
///
/// ⚠ **It CREATES NO DATABASE**: a box with no settings database is refused with
/// `DbErrorKind::NoDatabase`, exactly as [`crate::edit_account_in`] refuses one — the database's
/// mere existence is the credential store's per-run backend choice, so creating one from a
/// venue-setting write would stop `secrets.env` being read (`docs/decisions/0036`, reason 1). That
/// refusal carries no journal outcome at all (`Err((e, None))`): nothing beside a nonexistent
/// database has a journal directory to open either, and no change was attempted worth recording.
///
/// # Errors
/// Every refusal [`set_venue_setting_in`] states, and `NoDatabase` — each paired with the outcome
/// of recording THAT refusal in the change journal (`None` for the `NoDatabase` case above).
pub fn set_venue_setting_in_journalled(
    settings_dir: &Path,
    venue: &str,
    tier: Option<&str>,
    field: &str,
    value: &str,
    secret: bool,
    journal: crate::AccountJournal,
) -> Result<(Option<String>, Option<crate::JournalAppendError>), VenueSettingRefusal> {
    use vike_model::change_journal::{Change, Outcome};
    let db = crate::dotenv::db_path_in(settings_dir);
    if !database_present(&db) {
        return Err(Box::new((
            DbError {
                path: db,
                kind: crate::db::DbErrorKind::NoDatabase {
                    file: crate::dotenv::secrets_path_in(settings_dir),
                },
            },
            None,
        )));
    }
    let key = crate::venue_setting::venue_setting_key(venue, tier, field);
    let shown = |v: &str| if secret { REDACTED.to_string() } else { v.to_string() };
    let result = set_venue_setting_in(settings_dir, venue, tier, field, value);
    let change = match &result {
        Ok(previous) => Change::set_setting(
            Outcome::AppliedPendingRestart,
            journal.actor,
            "venue",
            &key,
            previous.as_deref().map(shown).as_deref(),
            &shown(value),
        ),
        Err(e) => {
            Change::set_setting(Outcome::Refused, journal.actor, "venue", &key, None, &shown(value))
                .with_reason(Some(&e.to_string()))
        }
    };
    let cj = crate::store::journal_beside(settings_dir, journal.proc);
    let journal_error = cj
        .append(journal.now_ms, &change)
        .err()
        .map(|source| crate::JournalAppendError { dir: cj.dir().to_path_buf(), source });
    match result {
        Ok(previous) => Ok((previous, journal_error)),
        Err(e) => Err(Box::new((e, journal_error))),
    }
}

// ---------------------------------------------------------------------------------------------
// The one-row writer (`docs/decisions/0086`)
// ---------------------------------------------------------------------------------------------

/// **One change [`write_setting_row_in`] can make — exactly one row, in exactly one table.**
///
/// The two shapes a settings write can take: an ordinary `setting` row
/// (`policy.max_notional_per_order`, `config.tradehub_addr`, …) or one `venue_arming` ceiling
/// (`policy.venues.<venue>` when `label` is `None`, `policy.accounts.<venue>.<LABEL>` when it names
/// one). Turning a dotted key into this enum is `vike-config`'s job (layer 20, which alone knows the
/// key grammar); this crate only ever executes the row change it is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowChange {
    /// Upsert one `setting` row.
    Setting {
        /// One of [`SETTINGS_SECTIONS`].
        section: String,
        /// The key's path inside that section — no section prefix, as [`SettingRow::key`].
        key: String,
        /// One JSON scalar, as [`SettingRow::value`].
        value: String,
    },
    /// Upsert one `venue_arming` row's `mode` — never its `max_exposure`, which this door does not
    /// touch: a row already carrying one keeps it, and a brand-new row gets `NULL`, exactly as every
    /// arming row does before an operator has stated a figure for it.
    Arming {
        /// The roster venue id, lowercase.
        venue: String,
        /// The account label, or `None` for the venue-level ceiling.
        label: Option<String>,
        /// `paper` / `demo` / `live`.
        mode: String,
    },
}

/// What [`write_setting_row_in`] did, once its transaction committed.
#[derive(Debug, Clone, PartialEq)]
pub struct RowWritten {
    /// The row's own value before this write — `None` for a brand-new row.
    pub old_value: Option<String>,
    /// The full settings rows AFTER this write landed — the committed CANDIDATE, so a caller builds
    /// its `old -> new` report and its journal record from what actually happened rather than from
    /// the request.
    pub rows: StoredSettings,
}

/// Why [`write_setting_row_in`] refused. Every variant's `Display` is operator-facing.
#[derive(Debug)]
pub enum RowWriteError {
    /// No settings database on this box. Nothing was written and nothing was created.
    NoDatabase {
        /// Where a database would be.
        path: PathBuf,
    },
    /// **Another writer holds the store** and the busy budget ran out. NOTHING was written.
    Busy,
    /// **This box has no arming rows at all**, so a `policy.accounts.*` statement has no venue-level
    /// row to refine and a `policy.venues.*` statement would be the FIRST — which this door refuses
    /// rather than silently declaring a partial roster (the old mirror filled every roster venue's
    /// row the moment `[venues]` was declared at all; a one-row writer cannot do that and still
    /// change only the row it names). What creates the first arming row is an open question this
    /// primitive does not answer — see `docs/decisions/0086`.
    ArmingRosterEmpty,
    /// The caller's validator refused: either the CURRENT store does not boot clean (a write must
    /// not re-bless an erased ceiling), or the CANDIDATE — the current rows plus this one change —
    /// does not boot clean, or would resolve a key other than the one named. The validator's own
    /// message says which.
    Rejected(String),
    /// The engine, mid-transaction. Every statement here runs inside one transaction, so a failure
    /// at any point rolls the whole write back — there is no partial state to repair.
    Sql(DbError),
}

impl std::fmt::Display for RowWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RowWriteError::NoDatabase { path } => {
                write!(f, "no settings database at {} — nothing was written", path.display())
            }
            RowWriteError::Busy => write!(
                f,
                "another process is writing the settings database — waited and gave up; NOTHING \
                 was written. Re-run the command."
            ),
            RowWriteError::ArmingRosterEmpty => write!(
                f,
                "this box has no arming rows at all — a single-venue arming write would leave a \
                 partial roster, which this writer refuses; nothing was written"
            ),
            RowWriteError::Rejected(why) => write!(f, "{why}"),
            RowWriteError::Sql(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RowWriteError {}

impl From<DbError> for RowWriteError {
    fn from(e: DbError) -> Self {
        if matches!(e.kind, DbErrorKind::StoreBusy) {
            RowWriteError::Busy
        } else {
            RowWriteError::Sql(e)
        }
    }
}

/// **Change exactly ONE row, inside one `BEGIN IMMEDIATE` transaction** — the row-native writer
/// `docs/decisions/0086` designs: every settings write, from
/// `vike-cli config set` to the daemon's own control channel, lands through this one primitive.
///
/// `validate` is handed `(current, current's seal, candidate)` — the store's rows before this write,
/// the [`Adoption`] those rows were sealed under (`None` on a store that has never been sealed), and
/// the rows after this write — and answers whether the write may land. It lives here as a CLOSURE
/// rather than as code in this crate because only `vike-config` can resolve rows into a typed
/// `Settings` and fold them through `apply_rows`/`differing_keys`, and this crate must not
/// depend upward on that one (`vike-secrets` is layer 15, `vike-config` is layer 20). It is called
/// exactly once, with the transaction still open and before any row is touched, so a refusal — the
/// current store failing its own boot check, the candidate failing its, or the candidate resolving a
/// key other than the one named — leaves the database BYTE-IDENTICAL: nothing between the read and
/// the rollback can have moved it.
///
/// `busy` is the caller's own wait budget for the write lock — a GUI frame, a daemon connection
/// thread and an interactive CLI all wait different amounts for the same lock, exactly as
/// `vike_config::write`'s module doc argues for the settings FILE's own lock.
///
/// # What this does NOT do
///
/// It never DELETEs a row, and it touches exactly one row in `setting` or `venue_arming` — plus the
/// derived `account.armed` fold and the adoption seal's counts, both RECOMPUTED from the same
/// candidate rather than taken from the caller, never a second independent write. A refusal, for any
/// reason including the engine's own [`RowWriteError::Sql`], rolls the whole transaction back.
///
/// # Errors
/// See [`RowWriteError`].
pub fn write_setting_row_in(
    settings_dir: &Path,
    change: RowChange,
    busy: std::time::Duration,
    validate: impl FnOnce(&StoredSettings, Option<&Adoption>, &StoredSettings) -> Result<(), String>,
) -> Result<RowWritten, RowWriteError> {
    let db = crate::dotenv::db_path_in(settings_dir);
    if !database_present(&db) {
        return Err(RowWriteError::NoDatabase { path: db });
    }
    let (mut conn, created, _version) = crate::db::open_for_write_within(&db, busy)?;
    if created {
        // The file vanished between the probe above and the open — the same hazard [`write_settings`]
        // and [`write_adoption`] both guard, and the same repair: never leave an EMPTY database
        // behind for `crate::store::Backend` to mistake for a real store.
        drop(conn);
        let _ = std::fs::remove_file(&db);
        return Err(RowWriteError::NoDatabase { path: db });
    }

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(&db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(&db, e))?;
    ensure_arming_columns(&db, &tx)?;
    crate::db::ensure_venue_id_columns(&tx).map_err(|e| DbError::sql(&db, e))?;

    let current = read_rows(&tx, &db)?;
    // The seal AS IT STOOD before this write — over the SAME open, so the caller's "does the
    // current store boot clean" check cannot race a sibling write. `None` for a store that has
    // never been sealed, which is the ordinary pre-adoption state and not itself a refusal.
    let current_adoption =
        if table_exists(&db, &tx, "settings_adoption")? { read_adoption(&db, &tx)? } else { None };

    let (candidate, old_value) = match &change {
        RowChange::Setting { section, key, value } => {
            let mut candidate = current.clone();
            let existing =
                candidate.settings.iter_mut().find(|r| &r.section == section && &r.key == key);
            let old = match existing {
                Some(row) => {
                    let old = row.value.clone();
                    row.value = value.clone();
                    Some(old)
                }
                None => {
                    candidate.settings.push(SettingRow {
                        section: section.clone(),
                        key: key.clone(),
                        value: value.clone(),
                    });
                    None
                }
            };
            (candidate.sorted(), old)
        }
        RowChange::Arming { venue, label, mode } => {
            if current.arming.is_empty() {
                return Err(RowWriteError::ArmingRosterEmpty);
            }
            let mut candidate = current.clone();
            let existing = candidate
                .arming
                .iter_mut()
                .find(|r| &r.venue == venue && r.label.as_ref() == label.as_ref());
            let old = match existing {
                Some(row) => {
                    let old = row.mode.clone();
                    row.mode = mode.clone();
                    Some(old)
                }
                None => {
                    candidate.arming.push(ArmingRow {
                        venue: venue.clone(),
                        label: label.clone(),
                        mode: mode.clone(),
                        max_exposure: None,
                    });
                    None
                }
            };
            (candidate.sorted(), old)
        }
    };

    if let Err(why) = validate(&current, current_adoption.as_ref(), &candidate) {
        return Err(RowWriteError::Rejected(why));
    }

    match &change {
        RowChange::Setting { section, key, value } => {
            if old_value.is_some() {
                tx.execute(
                    "UPDATE setting SET value = ?1 WHERE section = ?2 AND key = ?3",
                    rusqlite::params![value, section, key],
                )
            } else {
                tx.execute(
                    "INSERT INTO setting (section, key, value) VALUES (?1, ?2, ?3)",
                    rusqlite::params![section, key, value],
                )
            }
            .map_err(|e| DbError::sql(&db, e))?;
        }
        RowChange::Arming { venue, label, mode } => {
            // Both arms name the venue by its NUMBER, the way `read_rows` found (or did not find)
            // the row above: the funnel has carried the store, so every row holds one.
            if old_value.is_some() {
                tx.execute(
                    &format!(
                        "UPDATE venue_arming SET mode = ?1 WHERE {} AND label IS ?3",
                        crate::schema::venue_is("venue_id", "?2")
                    ),
                    rusqlite::params![mode, venue, label],
                )
            } else {
                let link = crate::schema::VenueLink::of(&tx, "venue_arming", "t")
                    .map_err(|e| DbError::sql(&db, e))?;
                tx.execute(
                    &format!(
                        "INSERT INTO venue_arming ({}, label, mode, max_exposure) \
                         VALUES ({}, ?2, ?3, NULL)",
                        link.columns,
                        link.values("?1")
                    ),
                    rusqlite::params![venue, label, mode],
                )
            }
            .map_err(|e| DbError::sql(&db, e))?;
            fold_arming_into_accounts(&tx).map_err(|e| DbError::sql(&db, e))?;
        }
    }

    // The seal moves by the SAME delta the write just made: created where none exists yet (every
    // store this primitive has ever written to before it existed), moved where one does. Unlike the
    // old `vike-cli config adopt` ceremony, `venues_declared` is not frozen at a past moment — it is
    // recomputed on every write, because there is no separate ceremony left to freeze it at.
    let venues_declared = !candidate.arming.is_empty();
    tx.execute(
        "INSERT INTO settings_adoption \
         (id, adopted_at, tool_version, files_present, venues_declared, setting_rows, arming_rows) \
         VALUES (1, datetime('now'), 'vike-secrets row-writer', '', ?1, ?2, ?3) \
         ON CONFLICT (id) DO UPDATE SET \
         venues_declared = excluded.venues_declared, \
         setting_rows = excluded.setting_rows, \
         arming_rows = excluded.arming_rows",
        rusqlite::params![
            i64::from(venues_declared),
            candidate.settings.len() as i64,
            candidate.arming.len() as i64
        ],
    )
    .map_err(|e| DbError::sql(&db, e))?;

    tx.commit().map_err(|e| DbError::sql(&db, e))?;
    Ok(RowWritten { old_value, rows: candidate })
}

/// **Plant a [`StoredSettings`] straight into the database beside `settings_dir`**, creating the
/// store first where none exists — the fixture every OTHER crate's row-writer test wants, behind
/// `test-support` so nothing outside a test can reach it.
///
/// ⚠ It goes through [`write_settings`] (the whole-table REPLACE), not through
/// [`write_setting_row_in`] — a fixture that planted rows one call at a time would need the arming
/// table non-empty before it could plant the FIRST arming row, which is exactly the refusal
/// [`RowWriteError::ArmingRosterEmpty`] states. A fixture is allowed to start from a state
/// [`write_setting_row_in`] itself could never reach in one call.
///
/// # Errors
/// The engine, or the filesystem.
#[cfg(feature = "test-support")]
pub fn plant_settings_rows(settings_dir: &Path, rows: &StoredSettings) -> Result<(), DbError> {
    let db = crate::dotenv::db_path_in(settings_dir);
    if !database_present(&db) {
        // ⚠ NOT `open_for_write` — that call's OWN doc says it deliberately does not stamp
        // `PRAGMA user_version` on the file it creates (the caller stamps it, after its own
        // transaction commits), and [`write_settings`] refuses a database it just found freshly
        // created as the "vanished between the probe and the open" hazard. So a fixture that opened
        // the file that way and then called `write_settings` would always hit
        // `DbErrorKind::SchemaVersion { found: 0, .. }` — MEASURED, the first version of this
        // function did exactly that. This is the same plant-a-finished-store sequence
        // `crate::settings::tests::planted` already uses successfully: create, set the pragma, run
        // the DDL, stamp the version — all before `write_settings` ever opens it.
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| DbError::io(&db, e))?;
        }
        let conn = rusqlite::Connection::open(&db).map_err(|e| DbError::sql(&db, e))?;
        conn.execute_batch("PRAGMA journal_mode = DELETE;").map_err(|e| DbError::sql(&db, e))?;
        conn.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(&db, e))?;
        conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION)
            .map_err(|e| DbError::sql(&db, e))?;
    }
    write_settings(&db, rows).map(|_| ())
}

/// **Hold the settings database's write lock**, for a test proving [`write_setting_row_in`]'s
/// [`RowWriteError::Busy`] disposition. `BEGIN IMMEDIATE` on a second connection, released by
/// dropping the returned one — a rollback of a transaction that wrote nothing, so it leaves no trace.
///
/// # Panics
/// If the database cannot be opened or the lock cannot be taken — a test fixture, not a production
/// path, so it fails loudly rather than returning a `Result` nothing would check.
#[cfg(feature = "test-support")]
#[must_use]
pub fn hold_write_lock(settings_dir: &Path) -> rusqlite::Connection {
    let db = crate::dotenv::db_path_in(settings_dir);
    let conn = rusqlite::Connection::open(&db).expect("open the settings database");
    conn.execute_batch("BEGIN IMMEDIATE;").expect("take the write lock");
    conn
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
    // …and the columns the DDL above cannot add to a table that already exists.
    ensure_arming_columns(db, &tx)?;
    // ⚠ This writer names `venue_id` in its own `venue_arming` INSERT below, and a store that
    // predates the column has no other path back through `crate::db::fill_into` before reaching
    // here — see `crate::db::ensure_venue_id_columns`'s own doc for why this call is required rather
    // than merely tidy.
    crate::db::ensure_venue_id_columns(&tx).map_err(|e| DbError::sql(db, e))?;
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
        let link = crate::schema::VenueLink::of(&tx, "venue_arming", "t")
            .map_err(|e| DbError::sql(db, e))?;
        let mut stmt = tx
            .prepare(&format!(
                "INSERT INTO venue_arming ({}, label, mode, max_exposure) VALUES ({}, ?2, ?3, ?4)",
                link.columns,
                link.values("?1")
            ))
            .map_err(|e| DbError::sql(db, e))?;
        for row in &rows.arming {
            stmt.execute(rusqlite::params![row.venue, row.label, row.mode, row.max_exposure])
                .map_err(|e| DbError::sql(db, e))?;
        }
    }
    // ⚠ **AFTER the arming rows land, in the SAME transaction, and the order is the whole of it.**
    // `account.armed` is DERIVED from the rows just written — see [`fold_arming_into_accounts`] —
    // so folding before the `INSERT` loop above would derive every bit from the rows this run
    // DELETED. A mirror is the ordinary way an operator states an arming, so this is the call that
    // actually fills the column on a live box.
    fold_arming_into_accounts(&tx).map_err(|e| DbError::sql(db, e))?;
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
            // ⚠ It named the RETIRED `profile_risk` table until the run profile's body moved onto
            // the profile plane. Its one constructor is now `vike-cli config show`'s projection of
            // `profile_store`, so the absence this reports is the PROFILE TABLES' — a store
            // written before they existed, which is a different fact from "this profile sets no
            // ceiling" and must not read as one.
            ProfileRiskSource::TableAbsent { path } => write!(
                f,
                "the settings database {} carries no profile tables yet — `vike-cli config \
                 mirror --profile <file>` is what creates them",
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

#[path = "settings_tests.rs"]
#[cfg(test)]
mod settings_tests;

/// **The `venue_setting` table survives a MIRROR, and nothing else in this module does.**
///
/// ⚠ This is the most expensive thing in the file to get wrong, and the shape of the code argues
/// FOR the mistake: [`write_settings`] clears `setting` and `venue_arming` and re-inserts both from
/// the caller's rows, so a third `DELETE FROM venue_setting` beside them reads as the obvious
/// tidy-up. It would not be one. Venue settings have no file to be re-derived FROM — that is the
/// whole reason they are a table instead of a `config` key — so the delete would simply remove
/// them: one `vike-cli config mirror` and a box loses its JForex server, its IBKR gateway host and
/// its polymarket egress, silently, with the command reporting success.
#[path = "mirror_leaves_venue_settings_alone.rs"]
#[cfg(test)]
mod mirror_leaves_venue_settings_alone;

/// **The operator's writer for a moved venue setting**, and the DDL constraint that is the whole
/// reason these values are a table rather than a field on `Config`.
#[path = "venue_setting_writer.rs"]
#[cfg(test)]
mod venue_setting_writer;
