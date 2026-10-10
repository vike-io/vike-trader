//! The settings rows as plain data: `SettingRow`, `ArmingRow`, `StoredSettings`, `Adoption` and the sources.

use super::*;

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
    /// world. It is also `vike-cli secrets init`'s fresh store, whose `setting` and
    /// `venue_arming` tables are created EMPTY by the shared DDL batch — which is exactly why the
    /// seal cannot be a table probe or a row count. See [`Adoption`].
    Rows {
        /// The two tables' rows.
        rows: StoredSettings,
        /// The seal, or `None` when this box has not crossed.
        adopted: Option<Adoption>,
    },

    /// [`crate::Backend::Absent`] — no settings database on this box at all.
    NoDatabase {
        /// Where a database would be.
        path: PathBuf,
    },
    /// A database at [`crate::SCHEMA_VERSION`] that predates the settings tables — i.e. a box
    /// migrated before 0057 Phase 1 and not mirrored since.
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
