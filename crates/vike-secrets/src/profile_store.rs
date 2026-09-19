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
//! | no database at all (`crate::Backend::Files`) | [`Profiles::none`] | exactly what it does today |
//! | a database with no profile tables (every box migrated before this) | [`Profiles::none`] | exactly what it does today |
//! | a database whose profile tables hold BODIES and no `active` row | bodies, `active` `None` | exactly what it does today |
//!
//! Only the fourth state — a row that says `active = 1` — changes anything, and
//! [`plan_active_row`] is the one place that decides whether writing one is allowed to.
//!
//! # ⚠ THIS MODULE DOES NOT TOUCH `PRAGMA user_version`, AND THAT IS A SAFETY PROPERTY
//!
//! `crate::db`'s `READABLE_SCHEMA_VERSIONS` carries the argument in full: a binary that meets a
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
//! `crates/vike-ops/tests/profile_writer_gate.rs` pins the files that may construct one. 0057's
//! *EROFS constraint* is why the set is what it is: **no shipped unit grants `settings/db`**, so the
//! daemon's own namespace cannot write this store at all, and the GUI reaches the box only through
//! the daemon. The operator's CLI, running outside that namespace, is the writer.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::OptionalExtension;

use crate::db::{DbError, database_present, open_for_read, open_for_write};

// ---------------------------------------------------------------------------------------------
// The schema
// ---------------------------------------------------------------------------------------------

/// **The profile half of the settings store** — §4.4 `profile`, §4.5 `mount`, §4.6 `mount_param`
/// and §4.8 `profile_setting`, plus the two columns argued in this module's doc.
///
/// # The three deviations from the printed DDL, each argued where it is made
///
/// **1. `profile.active`, with a partial unique index per KIND.** The owner ruled the row wins; a
/// column is what a row needs to say so. `WHERE active = 1` is what makes "at most one live daemon
/// profile" a property of the SCHEMA rather than of whoever wrote last — and it is per `kind`,
/// because a daemon profile and a run profile are selected independently today (MEASURED: the
/// shipped unit's `ExecStart --config` chooses one and `VIKE_RUN_PROFILE` the other) and collapsing
/// them into one active row would invent a coupling the file world does not have.
///
/// **2. `mount.is_primary`, with a partial unique index per PROFILE.** See
/// [`StoredProfile::primary`] for what the column means and what its absence means.
///
/// **3. `kind` keeps §4.4's three-word CHECK, and `recorder` is refused in RUST.** 0057's verdict
/// for `recorder.toml` is **STAYS** — *"This is the NO, and it is a NO to the owner's question as
/// asked"* — so no code here may write one. Narrowing the CHECK to two words instead would have made
/// the schema disagree with the spec it is built from for a reason that is a POLICY rather than a
/// shape, and 0057's flip condition (the deploy-layout question being answered) would then need a
/// schema change to honour. [`ProfileKind`] is the refusal, and it is total.
///
/// **4. `mount.asset_class`, NOT NULL, with a `CHECK` whose words are a PARAMETER.** Phase 5 of
/// `docs/decisions/0061-an-instrument-names-its-kind.md`, ORDERED by the owner: *"we need to add
/// type if it is spot or perp or option or anything else bcz it will help us in future"* — a claim
/// about what a mount should MEAN, not about any current bug. The vocabulary is
/// `vike_catalog::AssetClass`, unchanged and unwidened.
///
/// **Required rather than nullable, also his call**, and the reason is measurable: nothing in this
/// tree writes a profile row yet (`crates/vike-ops/tests/profile_writer_gate.rs`'s `WRITER_CALLERS`
/// is EMPTY, and that is its own stated measurement), so no `mount` table exists on any box and the
/// migration that makes the column mandatory costs one value in one file. A nullable column would
/// instead make 0061's *"a missing claim becomes a legal value"* hazard permanent for this seam.
///
/// ⚠ **The word list is a PARAMETER because there is no crate that can hold it once.** `vike-secrets`
/// is a zero-`vike-*`-dependency leaf at layer 15 and `vike-catalog` is layer 20, so this module
/// cannot name `AssetClass` — and 0061 refuses a second hand-written list outright (*"two lists —
/// one in code, one in SQL — would be the fifth partial encoding this record refuses everywhere
/// else"*). So this module spells NO asset-class word anywhere: the caller hands over
/// `vike_catalog::AssetClass::SQL_WORDS`, which is itself generated from the enum's single
/// declaration, and the `CHECK` clause in the database is that declaration rendered. Interpolating
/// caller strings into SQL is safe for exactly the reason [`ProfileKind::sql_word`] gives for its
/// own — they are `&'static str`s off a closed enum, never operator input —
/// and [`profile_ddl`] REFUSES anything that is not a bare alphanumeric word rather than trusting
/// that.
///
/// **5. `recorder` + `subscription`, added 2026-09-16 when the owner overruled 0057's NO.** Six
/// decisions, each of which had an available alternative:
///
///   * **TYPED COLUMNS, not a `profile_setting` bag.** `[maintenance]` and `[alerting]` are five
///     and three named keys, fixed and small. As columns, SQLite refuses an `INSERT` naming a key
///     that does not exist — on EVERY write path including a hand `INSERT`, with no Rust
///     involved — which is `deny_unknown_fields` made STRONGER by the move rather than lost to it.
///     A `(path, value)` bag would have made a typo'd path a row that reads as nothing.
///   * **EVERY KNOB NULLABLE, and NULL means ABSENT — never "the default".** The defaults live in
///     `vike_recorder::config`'s `Default` impls and stay there; a column that stored the resolved
///     default would make a re-rendered document differ from the file it came from, and the
///     migration's whole fence is that the two are EQUAL. `store` is the one NOT NULL column,
///     because the file requires it too.
///   * **`CHECK` for the closed VOCABULARY only** (`backfill IN ('venue','archive','off')`), never
///     for a numeric bound. `min_parts >= 2`, `target_mb > 0`, `max_merge_rows > 0` and
///     `retention_days > 0` are all CONDITIONAL on `interval_secs > 0`, which a row-local `CHECK`
///     cannot express — and this module's own rule forbids restating a numeric bound in SQL
///     regardless. They stay in `vike_recorder::config::RecorderProfile::validate`, which a
///     row-loaded profile reaches through the same `from_toml` a file-loaded one does.
///   * **`venue` gets NO `CHECK`.** The set a build can record is a per-BUILD fact
///     (`vike_recorder::venues::supported`, `#[cfg(feature = …)]`) while this store is one file
///     shared by every binary on the box, so a `CHECK` would refuse at write time a row a
///     differently-featured build could record. The Rust refusal in
///     `crates/vike-datahub/src/recorder.rs`'s `load_and_check_profile` is the authority and is
///     unchanged.
///   * **`symbols` and `alert_webhooks` hold a TOML ARRAY RENDERING in one column**, the same
///     idiom `mount_param.value` and `profile_setting.value` already use for a composite. A child
///     table was the normalized alternative and buys nothing here: the whole list shares one
///     `backfill`, `FamilyAndSymbols` reasons about the subscription as ONE thing, and a child
///     table cannot be reached by the exclusivity check anyway.
///   * **`subscription.ord` EXISTS AND IS NOT `mount.ord`.** `mount`'s ordinal is an ATTRIBUTION
///     key (`coid_mount`, the `{idx}|` strategy-tag prefix), so reordering relabels a live ledger.
///     This one is a row identity that also preserves authoring order, which
///     `crates/vike-datahub/src/recorder.rs`'s `record` uses only to build feeds in the order the
///     operator wrote them — cosmetic, and stated here so nobody infers the other meaning. The
///     `DuplicateFamily` refusal rides the partial unique index instead, which is where a
///     uniqueness claim belongs; the `family`/`symbols` exclusivity stays in Rust, because a
///     `CHECK` over `(family IS NULL) != (symbols IS NULL)` would forbid the both-absent case
///     with the WRONG message (`ProfileError::Empty` names what it would record: nothing).
///
/// `ON DELETE CASCADE` throughout, and `PRAGMA foreign_keys` is set and VERIFIED by `crate::db`'s
/// `open_for_write` — without it every `REFERENCES` here would enforce nothing, which is the trap
/// that module's doc already records for the credential family.
///
/// # Errors
///
/// [`ProfileError::UnrenderableVocabulary`] when a supplied word is empty or is not bare ASCII
/// alphanumerics.
pub fn profile_ddl(asset_class_words: &[&str]) -> Result<String, ProfileError> {
    if asset_class_words.is_empty() {
        return Err(ProfileError::UnrenderableVocabulary { word: String::new() });
    }
    for word in asset_class_words {
        if word.is_empty() || !word.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(ProfileError::UnrenderableVocabulary { word: (*word).to_string() });
        }
    }
    let words = asset_class_words.iter().map(|w| format!("'{w}'")).collect::<Vec<_>>().join(", ");
    Ok(PROFILE_DDL_TEMPLATE.replace(ASSET_CLASS_WORDS_PLACEHOLDER, &words))
}

/// The token [`profile_ddl`] substitutes the caller's vocabulary for. Never reaches a database.
const ASSET_CLASS_WORDS_PLACEHOLDER: &str = "{{ASSET_CLASS_WORDS}}";

/// The schema with the asset-class vocabulary left as a hole — see [`profile_ddl`], which is the
/// only thing that may render it. Private on purpose: a caller that executed this verbatim would
/// create a `mount` table whose `CHECK` refuses every word there is.
const PROFILE_DDL_TEMPLATE: &str = "\
CREATE TABLE IF NOT EXISTS profile (
    name        TEXT PRIMARY KEY NOT NULL,
    kind        TEXT NOT NULL,
    active      INTEGER NOT NULL DEFAULT 0,
    note        TEXT,
    updated_utc INTEGER,
    updated_by  TEXT,
    CHECK (kind IN ('daemon', 'run', 'recorder')),
    CHECK (active IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS profile_one_active_per_kind
    ON profile (kind) WHERE active = 1;

CREATE TABLE IF NOT EXISTS mount (
    profile          TEXT NOT NULL REFERENCES profile(name) ON DELETE CASCADE,
    ord              INTEGER NOT NULL,
    is_primary       INTEGER NOT NULL DEFAULT 0,
    venue            TEXT NOT NULL,
    asset_class      TEXT NOT NULL,
    symbol           TEXT,
    token_id         TEXT,
    interval         TEXT,
    interval_ms      INTEGER,
    resolution_ts_ms INTEGER,
    qty              REAL,
    half_spread      REAL,
    tick_size        REAL,
    seed_cash        REAL,
    data_only        INTEGER,
    account          TEXT,
    strategy_name    TEXT,
    strategy_rhai    TEXT,
    note             TEXT,
    PRIMARY KEY (profile, ord),
    CHECK ((symbol IS NULL) != (token_id IS NULL)),
    CHECK (is_primary IN (0, 1)),
    CHECK (data_only IS NULL OR data_only IN (0, 1)),
    CHECK (asset_class IN ({{ASSET_CLASS_WORDS}}))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS mount_one_primary_per_profile
    ON mount (profile) WHERE is_primary = 1;

CREATE TABLE IF NOT EXISTS mount_param (
    profile   TEXT NOT NULL,
    mount_ord INTEGER NOT NULL,
    key       TEXT NOT NULL,
    value     TEXT NOT NULL,
    PRIMARY KEY (profile, mount_ord, key),
    FOREIGN KEY (profile, mount_ord) REFERENCES mount(profile, ord) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS profile_setting (
    profile     TEXT NOT NULL REFERENCES profile(name) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    value       TEXT NOT NULL,
    note        TEXT,
    updated_utc INTEGER,
    updated_by  TEXT,
    PRIMARY KEY (profile, path)
) STRICT;

CREATE TABLE IF NOT EXISTS recorder (
    profile             TEXT PRIMARY KEY NOT NULL REFERENCES profile(name) ON DELETE CASCADE,
    store               TEXT NOT NULL,
    interval_secs       INTEGER,
    min_parts           INTEGER,
    target_mb           INTEGER,
    max_merge_rows      INTEGER,
    retention_days      INTEGER,
    alert_webhooks      TEXT,
    alert_repeat_secs   INTEGER,
    alert_series_prefix TEXT,
    note                TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS subscription (
    profile  TEXT NOT NULL REFERENCES profile(name) ON DELETE CASCADE,
    ord      INTEGER NOT NULL,
    venue    TEXT NOT NULL,
    family   TEXT,
    symbols  TEXT,
    backfill TEXT,
    note     TEXT,
    PRIMARY KEY (profile, ord),
    CHECK (backfill IS NULL OR backfill IN ('venue', 'archive', 'off'))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS subscription_one_family_per_venue
    ON subscription (profile, venue, family) WHERE family IS NOT NULL;
";

/// The tables [`profile_ddl`] creates. Used by the presence probe so the read path can tell "this
/// store predates Phase 3" from "this store has an empty profile table", and by the tests so a new
/// table cannot be added without the presence probe learning about it.
pub const PROFILE_TABLES: [&str; 6] =
    ["profile", "mount", "mount_param", "profile_setting", "recorder", "subscription"];

/// The tables the PRESENCE PROBE requires, which is a SMALLER set and deliberately so.
///
/// ⚠ **Widening the probe to every table in [`PROFILE_TABLES`] would have been a silent
/// regression, and it is the first thing the recorder tables nearly broke.** `read_profiles` uses
/// the probe to tell "this store predates the profile phase" from "this store has an empty profile
/// table", and answers [`Profiles::none`] for the first. A store written before 2026-09-16 holds
/// exactly these four — so a probe demanding six would report every already-migrated box as
/// un-migrated, and its daemon and run profiles would vanish from the read path with no error
/// anywhere. Two boxes are in that state today.
///
/// So the probe keeps the four that define the phase, and [`read_recorder`] asks `sqlite_master`
/// for its own table instead of assuming it. `store_profile` and `ensure_tables` execute the whole
/// DDL (`CREATE TABLE IF NOT EXISTS`), so the first write upgrades an old store to all six.
const CORE_PROFILE_TABLES: [&str; 4] = ["profile", "mount", "mount_param", "profile_setting"];

// ---------------------------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------------------------

/// Which PROFILE DOCUMENT a row is a body of.
///
/// ⚠ **`Recorder` LANDED 2026-09-16, and this enum used to have TWO variants with a NAMED REFUSAL
/// where the third is.** `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`
/// answered `recorder.toml` with a NO, on the ground that it belongs to a different daemon and that
/// moving it would settle the deploy-layout question by accident. The owner OVERRULED that on
/// 2026-09-16, and what fired is the record's own stated reopener: the deploy-layout question was
/// answered the same day (ONE unit file per daemon, the project root a substitutable parameter).
/// The schema's `CHECK` already carried the word — [`profile_ddl`]'s own doc records that as
/// deliberate foresight — so the flip needed no column change and no migration of an existing
/// store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProfileKind {
    /// `settings/tradehub.toml` — `vike_tradehub::config::DaemonProfile`. The mount set.
    Daemon,
    /// `settings/run-live.toml` — `vike_core::RunProfile`. The `[risk]` ceilings and the sinks.
    Run,
    /// `settings/recorder.toml` — `vike_recorder::config::RecorderProfile`. The store root, the
    /// subscription list, and the maintenance/alerting knobs.
    ///
    /// ⚠ **Unlike the other two, this document decides WHICH VENUE FEEDS OPEN.** A wrong body is
    /// not a mis-read setting; it is a daemon subscribing to something nobody asked for. That is
    /// why the migration's fence is a ROUND-TRIP equality against the profile in force rather than
    /// [`plan_active_row`] alone — see `crates/vike-cli/src/cmd/config_mirror_recorder.rs`.
    Recorder,
}

impl ProfileKind {
    /// The `profile.kind` word. A `&'static str` from a closed enum — never operator input, which
    /// is what keeps it safe to compare against the CHECK.
    #[must_use]
    pub fn sql_word(self) -> &'static str {
        match self {
            ProfileKind::Daemon => "daemon",
            ProfileKind::Run => "run",
            ProfileKind::Recorder => "recorder",
        }
    }

    /// Parse a stored `kind`.
    ///
    /// ⚠ `recorder` was a NAMED refusal here, citing 0057's NO. That NO was overruled by the owner
    /// on 2026-09-16 and the word parses now; the refusal arm is DELETED rather than softened,
    /// because a word the CHECK admits and this function refuses is a row nothing can read.
    ///
    /// # Errors
    ///
    /// [`ProfileError::UnreadableKind`] for anything the CHECK would have refused, naming the word.
    pub fn parse(word: &str) -> Result<Self, ProfileError> {
        match word {
            "daemon" => Ok(ProfileKind::Daemon),
            "run" => Ok(ProfileKind::Run),
            "recorder" => Ok(ProfileKind::Recorder),
            other => Err(ProfileError::UnreadableKind { kind: other.to_string() }),
        }
    }
}

/// One `profile` row's own columns — the identity, not the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRow {
    /// The profile's NAME, which is its primary key and what an `active` selection names.
    pub name: String,
    /// Which document this is a body of.
    pub kind: ProfileKind,
    /// **The owner's ruling, as one bit.** Exactly one row per [`ProfileKind`] may carry it
    /// (`profile_one_active_per_kind`), and a kind with NO active row is the state every box is in
    /// today.
    pub active: bool,
    /// The operator's own note. Never read by any resolver.
    pub note: Option<String>,
}

/// One `mount` row — `vike_tradehub::config::MountCfg` as columns, plus the explicit primary.
///
/// Every field except [`Self::venue`] and [`Self::ord`] is optional, exactly as the TOML spelling
/// is: an omitted column and an omitted key must mean the same thing, or a round trip through the
/// store would silently freeze a default.
#[derive(Debug, Clone, PartialEq)]
pub struct MountRow {
    /// The row's ORDINAL — what a TOML array-of-tables has and a table does not. It is preserved
    /// because it is an OBSERVABLE: mount indices are attribution keys (`coid_mount` values, the
    /// `{idx}|` strategy-tag prefix), so re-ordering rows re-labels a live daemon's ledger.
    ///
    /// ⚠ **It is NOT the primary.** That is [`Self::is_primary`], and the split is the whole of
    /// 0057's *"the primary must become EXPLICIT"*.
    pub ord: i64,
    /// **THE DECLARED PRIMARY.** See [`StoredProfile::primary`].
    pub is_primary: bool,
    /// `MountCfg::venue`, with the profile's `"polymarket"` default already applied — a row always
    /// names its venue, because a NULL here could not be told from the default.
    pub venue: String,
    /// **WHAT PRODUCT this mount trades**, as the stored word of a `vike_catalog::AssetClass` —
    /// `"CryptoSpot"`, `"CryptoPerp"`, `"Option"`, … Phase 5 of
    /// `docs/decisions/0061-an-instrument-names-its-kind.md`, ORDERED by the owner: *"we need to add
    /// type if it is spot or perp or option or anything else bcz it will help us in future"*.
    ///
    /// ⚠ **NOT an `Option`, and that is the whole point.** Every other field here is optional
    /// because an omitted column and an omitted TOML key must mean the same thing; this one is
    /// REQUIRED because a mount that does not say which product it trades is under-specified, and a
    /// nullable column would make 0061's *"a missing claim becomes a legal value"* hazard permanent
    /// for this seam. The schema enforces it twice — `NOT NULL` and a `CHECK` over the vocabulary.
    ///
    /// ⚠ A `String` rather than a typed enum for the reason [`profile_ddl`]'s doc gives at length:
    /// this crate is a zero-`vike-*`-dependency leaf at layer 15 and cannot name a layer-20 type.
    /// The word is produced by `vike_catalog::AssetClass::sql_word` and read back by
    /// `AssetClass::from_sql_word`; a word the schema would have refused can only come from a
    /// hand-edited store, and it parses to `None` there rather than to a wrong variant.
    pub asset_class: String,
    /// `MountCfg::symbol` — exactly one of this and [`Self::token_id`] is set, which the schema's
    /// `CHECK ((symbol IS NULL) != (token_id IS NULL))` enforces rather than the loader.
    pub symbol: Option<String>,
    /// `MountCfg::token_id` — the Polymarket spelling of [`Self::symbol`].
    pub token_id: Option<String>,
    /// `MountCfg::interval`.
    pub interval: Option<String>,
    /// `MountCfg::interval_ms`.
    pub interval_ms: Option<i64>,
    /// `MountCfg::resolution_ts_ms`.
    pub resolution_ts_ms: Option<i64>,
    /// `MountCfg::qty`.
    pub qty: Option<f64>,
    /// `MountCfg::half_spread`.
    pub half_spread: Option<f64>,
    /// `MountCfg::tick_size`.
    pub tick_size: Option<f64>,
    /// `MountCfg::seed_cash`.
    pub seed_cash: Option<f64>,
    /// `MountCfg::data_only`.
    pub data_only: Option<bool>,
    /// `MountCfg::account` — the label, NULL meaning the venue's DEFAULT account, which is how
    /// `vike_model::account_keys::AccountLabel::Default` renders everywhere else in this tree.
    pub account: Option<String>,
    /// `StrategyCfg`'s registry NAME.
    pub strategy_name: Option<String>,
    /// `StrategyCfg`'s Rhai script PATH.
    pub strategy_rhai: Option<String>,
}

impl MountRow {
    /// A minimal row: an ordinal, a venue, an asset class, everything else absent. Every constructor
    /// in the tests starts here so a field added to [`MountRow`] cannot silently acquire a value in
    /// one.
    ///
    /// ⚠ `asset_class` is a PARAMETER rather than a default, which is the Rust half of the column's
    /// `NOT NULL`: there is no way to build a row that does not name its product, so the refusal
    /// happens at the type rather than at the database. Pass
    /// `vike_catalog::AssetClass::sql_word()`.
    #[must_use]
    pub fn new(ord: i64, venue: &str, asset_class: &str) -> Self {
        MountRow {
            ord,
            is_primary: false,
            venue: venue.to_string(),
            asset_class: asset_class.to_string(),
            symbol: None,
            token_id: None,
            interval: None,
            interval_ms: None,
            resolution_ts_ms: None,
            qty: None,
            half_spread: None,
            tick_size: None,
            seed_cash: None,
            data_only: None,
            account: None,
            strategy_name: None,
            strategy_rhai: None,
        }
    }

    /// The mount SYMBOL from whichever spelling the row used — the row twin of
    /// `vike_tradehub::config::DaemonProfile::mount_symbol`, and deliberately the same shape: the
    /// schema's exclusive CHECK has already refused a row that set neither or both.
    #[must_use]
    pub fn mount_symbol(&self) -> &str {
        self.symbol.as_deref().or(self.token_id.as_deref()).unwrap_or_default()
    }
}

/// **WHICH mount is the daemon's singular identity**, and how that was decided.
///
/// # Why this is an enum rather than a `usize`
///
/// MEASURED in `crates/vike-tradehub/src/tradehub_cli.rs`: the primary is `resolved[0]` — the first
/// mount row — and its own comment calls it *"the daemon's historical singular identity (summary
/// token, mode line, seed policy)"*. 0057 adds the half that comment does not: on a paper core
/// `crates/vike-run/src/lib.rs` derives ENGINE ORDER from first-appearance venue order, while on a
/// LIVE core row order decides nothing at all, because `crates/vike-run/src/node.rs`'s `build_node`
/// builds its engine list straight-line from `WIRED_MARKETS`. So *"row 0 silently means three things
/// on paper and nothing on live"*.
///
/// A table has no inherent order. Reproducing that from rows with an ordinal alone would carry the
/// accident forward and make it harder to see; declaring it makes the meaning readable and leaves
/// the accident named. Both arms exist because **the migration must not change the answer**: a
/// profile that declares nothing keeps the first row, byte for byte, and says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primary {
    /// A row carries `is_primary = 1`. The ordinal is its `ord`, which need NOT be the lowest —
    /// that is the entire point of the column.
    Declared(i64),
    /// No row declares one, so the LOWEST ordinal is the primary — today's rule, unchanged. A
    /// caller that reports the mount set should say *implicit* rather than print a bare number.
    ImplicitFirst(i64),
    /// The profile has no mounts at all. Nothing can be primary, and a live mount of this profile
    /// is a refusal somewhere above rather than a choice made here.
    NoMounts,
}

/// A whole profile as it sits in the store: the identity row, its mounts, and its non-mount leaves.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredProfile {
    /// The `profile` row.
    pub row: ProfileRow,
    /// The `mount` rows, **ordered by `ord`** — the read path sorts, so a caller never depends on
    /// SQLite's row order for a fact the schema says is in a column.
    pub mounts: Vec<MountRow>,
    /// The `mount_param` rows, keyed `(mount ord, key)` → the TOML scalar rendering of the value.
    pub params: BTreeMap<(i64, String), String>,
    /// The `profile_setting` rows, keyed by dotted path → the TOML scalar rendering of the value.
    pub settings: BTreeMap<String, String>,
    /// The `recorder` + `subscription` rows, present only on a [`ProfileKind::Recorder`] profile.
    ///
    /// `None` on every other kind and on every store written before 2026-09-16, which is the same
    /// shape [`Profiles::none`] gives the whole read path: a body that is not there behaves
    /// exactly as it did before the tables existed.
    pub recorder: Option<RecorderBody>,
}

/// A `recorder` profile's whole body — the scalars row plus its subscriptions, in `ord` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderBody {
    /// The one `recorder` row.
    pub row: RecorderRow,
    /// The `subscription` rows, **ordered by `ord`**.
    pub subscriptions: Vec<SubscriptionRow>,
}

/// The `recorder` table's columns for one profile.
///
/// ⚠ **Every `Option` here means ABSENT IN THE PROFILE, never "the default".** The defaults are
/// `vike_recorder::config`'s `Maintenance`/`Alerting` `Default` impls and they stay there; a row
/// that stored a resolved default would make [`render_recorder_toml`] emit a key the operator's
/// file did not carry, and the migration's fence is that the rendered document and the file PARSE
/// EQUAL. See [`profile_ddl`]'s decision 5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderRow {
    /// `store` — the profile's store root. Required, exactly as in the file.
    pub store: String,
    /// `[maintenance].interval_secs`.
    pub interval_secs: Option<i64>,
    /// `[maintenance].min_parts`.
    pub min_parts: Option<i64>,
    /// `[maintenance].target_mb`.
    pub target_mb: Option<i64>,
    /// `[maintenance].max_merge_rows`.
    pub max_merge_rows: Option<i64>,
    /// `[maintenance].retention_days`.
    pub retention_days: Option<i64>,
    /// `[alerting].webhooks`, as the TOML ARRAY rendering (`["telegram"]`) — never a token. The
    /// profile names TARGETS and the credential store holds what is behind a name.
    pub alert_webhooks: Option<String>,
    /// `[alerting].repeat_secs`.
    pub alert_repeat_secs: Option<i64>,
    /// `[alerting].series_prefix`.
    pub alert_series_prefix: Option<String>,
    /// The operator's own note. **Required by the design rather than optional decoration**: the CI box's
    /// live profile carries an inline measurement comment on `max_merge_rows` that is the only
    /// place on that box where the number's justification lives, and a migration that dropped it
    /// would be the largest unpriced loss in the move.
    pub note: Option<String>,
}

/// One `[[subscribe]]` entry as a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionRow {
    /// Row identity, and the order feeds are built in. ⚠ **NOT an attribution key** — see
    /// [`profile_ddl`]'s decision 5 for why that distinction from `mount.ord` is written down.
    pub ord: i64,
    /// `venue`. No `CHECK`: the recordable set is a per-BUILD fact.
    pub venue: String,
    /// `family` — a whole market family, recorded as ONE grouped series.
    pub family: Option<String>,
    /// `symbols`, as the TOML ARRAY rendering (`["BTCUSDT.P"]`) — recorded per-symbol.
    pub symbols: Option<String>,
    /// `backfill` — `venue` | `archive` | `off`. `None` = the key was absent.
    pub backfill: Option<String>,
    /// The operator's own note for this subscription — the inline comment a TOML file carries.
    pub note: Option<String>,
}

/// **Render a stored recorder body back into the TOML document `vike_recorder` parses.**
///
/// THE ONE RENDERER, and it is in this leaf crate deliberately: both consumers need it and they
/// cannot see each other. `crates/vike-datahub/src/recorder.rs` feeds the result to
/// `RecorderProfile::from_toml`, so every refusal that function performs — serde's
/// `deny_unknown_fields` on four structs, the missing-`store` parse error and all seven
/// `validate` rules — applies to a row-loaded profile with no second implementation.
/// `crates/vike-cli`'s migration renders the rows it is about to write and requires the result to
/// PARSE EQUAL to the file, which is the fence that stops a wrong body being stored.
///
/// ⚠ It emits ONLY the keys the rows carry. A `None` is an OMITTED key, not a rendered default —
/// see [`RecorderRow`]. `[maintenance]` and `[alerting]` headers are emitted only when at least one
/// of their keys is present, because an EMPTY table and an ABSENT one mean the same thing to
/// `vike_recorder::config` and the file being migrated has one or the other, never both.
///
/// ⚠ No value is escaped and none needs to be: `store`, `family`, `venue` and `series_prefix` are
/// rendered through [`toml_basic_string`], and `symbols`/`alert_webhooks` are stored ALREADY
/// RENDERED (the writer produced them with the same helper). A caller that hand-wrote a row with an
/// unbalanced array string gets a TOML parse error from `from_toml`, which is a refusal rather than
/// a silent misread.
#[must_use]
pub fn render_recorder_toml(body: &RecorderBody) -> String {
    let mut doc = String::new();
    doc.push_str(&format!("store = {}\n", toml_basic_string(&body.row.store)));
    for s in &body.subscriptions {
        doc.push_str("\n[[subscribe]]\n");
        doc.push_str(&format!("venue = {}\n", toml_basic_string(&s.venue)));
        if let Some(f) = &s.family {
            doc.push_str(&format!("family = {}\n", toml_basic_string(f)));
        }
        if let Some(syms) = &s.symbols {
            doc.push_str(&format!("symbols = {syms}\n"));
        }
        if let Some(b) = &s.backfill {
            doc.push_str(&format!("backfill = {}\n", toml_basic_string(b)));
        }
    }
    let m = &body.row;
    let maintenance =
        [m.interval_secs, m.min_parts, m.target_mb, m.max_merge_rows, m.retention_days];
    if maintenance.iter().any(Option::is_some) {
        doc.push_str("\n[maintenance]\n");
        for (key, value) in [
            ("interval_secs", m.interval_secs),
            ("min_parts", m.min_parts),
            ("target_mb", m.target_mb),
            ("max_merge_rows", m.max_merge_rows),
            ("retention_days", m.retention_days),
        ] {
            if let Some(v) = value {
                doc.push_str(&format!("{key} = {v}\n"));
            }
        }
    }
    if m.alert_webhooks.is_some()
        || m.alert_repeat_secs.is_some()
        || m.alert_series_prefix.is_some()
    {
        doc.push_str("\n[alerting]\n");
        if let Some(w) = &m.alert_webhooks {
            doc.push_str(&format!("webhooks = {w}\n"));
        }
        if let Some(v) = m.alert_repeat_secs {
            doc.push_str(&format!("repeat_secs = {v}\n"));
        }
        if let Some(p) = &m.alert_series_prefix {
            doc.push_str(&format!("series_prefix = {}\n", toml_basic_string(p)));
        }
    }
    doc
}

/// A TOML basic string, with the five escapes TOML requires. Pure, and the ONE place a value from
/// this store becomes document text — the writer and [`render_recorder_toml`] both go through it,
/// so a symbol carrying a quote round-trips instead of producing an unparseable document.
#[must_use]
pub fn toml_basic_string(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A TOML array of basic strings — the rendering `SubscriptionRow::symbols` and
/// `RecorderRow::alert_webhooks` are STORED as, so the column holds document text a renderer can
/// emit verbatim.
#[must_use]
pub fn toml_string_array(values: &[String]) -> String {
    let inner = values.iter().map(|v| toml_basic_string(v)).collect::<Vec<_>>().join(", ");
    format!("[{inner}]")
}

impl StoredProfile {
    /// **THE ONE PRIMARY RESOLUTION.** Every consumer asks this rather than indexing, so a table's
    /// lack of order cannot be answered two different ways in two places.
    #[must_use]
    pub fn primary(&self) -> Primary {
        if let Some(m) = self.mounts.iter().find(|m| m.is_primary) {
            return Primary::Declared(m.ord);
        }
        match self.mounts.iter().map(|m| m.ord).min() {
            Some(ord) => Primary::ImplicitFirst(ord),
            None => Primary::NoMounts,
        }
    }

    /// The mount row [`Self::primary`] names, if there is one.
    #[must_use]
    pub fn primary_mount(&self) -> Option<&MountRow> {
        let ord = match self.primary() {
            Primary::Declared(o) | Primary::ImplicitFirst(o) => o,
            Primary::NoMounts => return None,
        };
        self.mounts.iter().find(|m| m.ord == ord)
    }
}

// ⚠ THERE IS DELIBERATELY NO `refuse_unrunnable_primary` HERE, and the absence is the design.
//
// A first draft gave [`StoredProfile`] its own refusal for a primary naming a venue the node runs
// no engine for, in PR #1866's vocabulary. It had to go, for the reason #1866 itself is about:
// **two spellings of one refusal teach an operator to read two different faults into one
// situation.** This crate is a leaf with no `vike-*` dependency and no idea what a venue IS, so its
// copy could only ever be a second rendering of a sentence whose authority lives at the mount
// layer.
//
// The check survives and is stronger for the move: `vike_tradehub::profile_rows`'s
// `rows_to_daemon_profile` materialises stored rows back through `DaemonProfile::from_toml_str`, so
// a row-loaded profile reaches `vike_tradehub::config::DaemonProfile::refuse_unrunnable_primary` —
// the SAME function a file-loaded profile reaches, calling the SAME `no_engine_refusal` that
// `vike_tradehub::server::venue_refusal` calls on the order path. One sentence, one implementation,
// three call sites.

/// Every profile the store holds, plus the fact of whether it holds any at all.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Profiles {
    profiles: Vec<StoredProfile>,
    tables_present: bool,
}

impl Profiles {
    /// **The answer for every box that has not migrated, and for every box that has no database.**
    /// Distinct from "migrated and empty" only through [`Self::tables_present`]; every RESOLVER
    /// treats the two identically, which is the arming-preservation property.
    #[must_use]
    pub fn none() -> Self {
        Profiles { profiles: Vec::new(), tables_present: false }
    }

    /// Do the profile tables exist in this store? A reporting fact, never a resolution input.
    #[must_use]
    pub fn tables_present(&self) -> bool {
        self.tables_present
    }

    /// Every stored profile, in name order.
    #[must_use]
    pub fn all(&self) -> &[StoredProfile] {
        &self.profiles
    }

    /// **The ACTIVE profile of one kind, or `None`.** `None` is what every box answers today, and
    /// `None` is what every caller must treat as "nothing here selects anything".
    #[must_use]
    pub fn active(&self, kind: ProfileKind) -> Option<&StoredProfile> {
        self.profiles.iter().find(|p| p.row.kind == kind && p.row.active)
    }

    /// One profile by name, whatever its kind.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&StoredProfile> {
        self.profiles.iter().find(|p| p.row.name == name)
    }

    /// Assemble a `Profiles` from rows a caller already has — **for rendering tests, and it says
    /// `tables_present`.**
    ///
    /// ⚠ It exists because a consumer that RENDERS these rows (`vike-cli config recorder`) must be
    /// able to test its own wording without standing up a SQLite file, and the alternative was each
    /// such consumer growing a fixture that migrates a credential store to print one line. It
    /// cannot be mistaken for a read: nothing in this module calls it, no resolver takes a
    /// `Profiles` from anywhere but [`read_profiles`], and every field it fills is already `pub` on
    /// [`StoredProfile`].
    ///
    /// ⚠ It reports `tables_present() == true`, deliberately: the one thing a caller must NOT be
    /// able to fabricate through it is [`Profiles::none`]'s answer, which is the
    /// arming-preservation signal every box that has not migrated gives. `Profiles::none()` is the
    /// constructor for that and stays the only one.
    #[must_use]
    pub fn from_rows(profiles: Vec<StoredProfile>) -> Self {
        Profiles { profiles, tables_present: true }
    }
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// A profile-store failure. **Names a profile, a mount and a venue — never a credential**, which is
/// structural here rather than a promise: no statement in this module selects from `credential`,
/// `account` or `node_key`.
#[derive(Debug)]
pub enum ProfileError {
    /// The store itself could not be opened or read. Forwarded verbatim from `crate::db`.
    Db(DbError),
    /// A stored `kind` this code does not read — including `recorder`, which 0057 rules STAYS a
    /// file.
    UnreadableKind {
        /// The word found in the column.
        kind: String,
    },
    /// A write would have made a second row of one kind active. The schema refuses it too; this is
    /// the message an operator can act on.
    TwoActive {
        /// The kind that would have held two.
        kind: ProfileKind,
        /// The name that already holds it.
        held_by: String,
    },
    /// **A write was attempted against a store that does not exist, and creating one is REFUSED.**
    ///
    /// ⚠ This is a live-gate refusal, not tidiness. `crate::store::Backend` decides which store
    /// answers from ONE `is_file` on this path, so bringing an empty database into existence makes
    /// every credential read answer from it and the `secrets.env` beside it is never opened again —
    /// *"an empty map is not an error downstream; it is the LIVE GATE. Every venue drops to paper
    /// with `secrets.env` sitting on disk looking correct"* (`crate::db`'s module doc). A profile
    /// write has no business paying that price, so it refuses and names the one command that may
    /// create a store.
    NoStore {
        /// The database path that is absent.
        path: std::path::PathBuf,
    },
    /// A caller handed [`profile_ddl`] an asset-class word it will not render into a `CHECK`.
    ///
    /// Not a defensive flourish: this module spells NO asset-class word of its own (see
    /// [`profile_ddl`]'s doc for why it cannot), so the vocabulary arrives as caller data and the
    /// only thing standing between it and a SQL clause is this refusal. The legitimate caller hands
    /// over `vike_catalog::AssetClass::SQL_WORDS`, every member of which is a bare identifier off a
    /// closed enum; anything else is a bug in the caller, and rendering it would be a bug in the
    /// schema.
    UnrenderableVocabulary {
        /// The offending word, or empty when the whole vocabulary was empty.
        word: String,
    },
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::Db(e) => write!(f, "{e}"),
            ProfileError::UnreadableKind { kind } => write!(
                f,
                "the settings store holds a profile of kind {kind:?}, which this binary does not \
                 read. `recorder` is deliberate: docs/decisions/0057 rules that recorder.toml STAYS \
                 a file until the deploy-layout question is answered, so nothing here may load one"
            ),
            ProfileError::NoStore { path } => write!(
                f,
                "there is no settings database at {} and a profile write will not create one. \
                 Creating an empty store would make it the store that ANSWERS for credentials — \
                 every venue would silently drop to paper with secrets.env sitting on disk looking \
                 correct. Run `vike-cli secrets migrate` first; that is the one command that may \
                 bring this file into existence",
                path.display()
            ),
            ProfileError::TwoActive { kind, held_by } => write!(
                f,
                "profile kind `{}` already has an active row (`{held_by}`), and exactly one may be \
                 active. Deactivate that one first — an active profile is what this box trades, and \
                 two of them is not a state a resolver can be asked to break a tie in",
                kind.sql_word()
            ),
            ProfileError::UnrenderableVocabulary { word } if word.is_empty() => write!(
                f,
                "the mount schema's asset-class vocabulary arrived EMPTY. This module spells no \
                 asset-class word of its own by design — the caller hands over \
                 `vike_catalog::AssetClass::SQL_WORDS` — and an empty CHECK would refuse every \
                 mount row there is"
            ),
            ProfileError::UnrenderableVocabulary { word } => write!(
                f,
                "the asset-class word {word:?} is not a bare alphanumeric identifier and will not \
                 be rendered into a SQL CHECK. The vocabulary must be \
                 `vike_catalog::AssetClass::SQL_WORDS`, which is generated from that enum's single \
                 declaration"
            ),
        }
    }
}

impl std::error::Error for ProfileError {}

impl From<DbError> for ProfileError {
    fn from(e: DbError) -> Self {
        ProfileError::Db(e)
    }
}

// ---------------------------------------------------------------------------------------------
// THE RULING: selection
// ---------------------------------------------------------------------------------------------

/// Where a resolved profile selection CAME FROM.
///
/// It is reported rather than merely used, and that is §7 of the schema spec (`shadowed` — *"a file
/// could never report this, because a file cannot know it lost"*). With the row winning, an operator
/// whose `ExecStart --config` or whose `.env` `VIKE_RUN_PROFILE` has stopped deciding anything must
/// be told so at startup, or the ruling delivers the exact defect it was ruling against: positive
/// confirmation of something false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    /// A `profile` row with `active = 1`. **The winner, by the owner's ruling.**
    StoreRow,
    /// An explicit path the caller was given — `--config <path>` (which
    /// `crates/vike-tradehub/src/tradehub_cli.rs`'s `parse_args_from` makes REQUIRED) or
    /// `--profile <path>`.
    ExplicitArg,
    /// An environment variable — `VIKE_RUN_PROFILE`, which on the shipped deployment arrives from
    /// `EnvironmentFile=-<root>/.env`, an untracked file at the project root.
    EnvVar,
}

impl SelectionSource {
    /// The word a startup line prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            SelectionSource::StoreRow => "store row",
            SelectionSource::ExplicitArg => "explicit argument",
            SelectionSource::EnvVar => "environment variable",
        }
    }
}

/// One rung that held a value and LOST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadowed {
    /// Which rung.
    pub source: SelectionSource,
    /// What it held. A profile NAME or a path — never a secret.
    pub value: String,
}

/// The outcome of [`select`]: who won, with what, and who lost while still holding a value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selected {
    /// The winning value, or `None` when no rung held one.
    pub value: Option<String>,
    /// Which rung won. `None` exactly when [`Self::value`] is `None`.
    pub source: Option<SelectionSource>,
    /// Every rung that held a value and did not win, highest first.
    pub shadowed: Vec<Shadowed>,
}

impl Selected {
    /// Did a store row beat something that is still set? The one condition a startup line must
    /// WARN about rather than merely record.
    #[must_use]
    pub fn row_shadowed_something(&self) -> bool {
        self.source == Some(SelectionSource::StoreRow) && !self.shadowed.is_empty()
    }
}

/// **THE OWNER'S RULING, WRITTEN ONCE: the row wins.**
///
/// Precedence, highest first: an active `profile` row, then an explicit argument, then the
/// environment. Every rung that held a value and lost is reported in [`Selected::shadowed`], so a
/// caller can say what stopped mattering.
///
/// # ⚠ Why the ruling is safe to implement as stated, and where the safety actually lives
///
/// 0057 Question 3 warns that *"if the row wins, a migration that writes one ARMS a mount that a
/// missing line was holding on paper"*. That hazard is real and it is **not in this function**: this
/// function only decides which of the values it was handed wins. The hazard is in WRITING a row, and
/// [`plan_active_row`] is the only thing in this tree that may.
///
/// A blank or whitespace-only value is treated as ABSENT at every rung — the same reading
/// `vike_model::state_path`'s `VIKE_SETTINGS_DIR` handling gives an empty override, and the reason
/// is the same: `Environment=VIKE_RUN_PROFILE=` in a unit file is somebody unsetting it, not
/// somebody naming a profile called "".
#[must_use]
pub fn select(row: Option<&str>, explicit: Option<&str>, env: Option<&str>) -> Selected {
    let rungs: [(SelectionSource, Option<&str>); 3] = [
        (SelectionSource::StoreRow, row),
        (SelectionSource::ExplicitArg, explicit),
        (SelectionSource::EnvVar, env),
    ];
    let mut out = Selected::default();
    for (source, held) in rungs {
        let Some(v) = held.map(str::trim).filter(|v| !v.is_empty()) else { continue };
        if out.value.is_none() {
            out.value = Some(v.to_string());
            out.source = Some(source);
        } else {
            out.shadowed.push(Shadowed { source, value: v.to_string() });
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// THE FENCE: may a migration write an active row?
// ---------------------------------------------------------------------------------------------

/// What a migration proposes to do about ONE kind's active row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivePlan {
    /// Write `active = 1` on this profile. Only ever produced when doing so **reproduces the
    /// selection that is in force today**, so the resolved outcome is unchanged by construction.
    Write {
        /// The profile that is already in force, and which the row will name.
        name: String,
    },
    /// Write NO active row, and say why. The migration still lands the BODIES — which is 0057's own
    /// *"the store holds profile BODIES"* — and selection stays exactly where it is.
    Withhold {
        /// The operator-facing reason, which names what would have changed.
        reason: WithholdReason,
    },
}

/// Why [`plan_active_row`] refused to write an active row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WithholdReason {
    /// **The dangerous one.** Nothing selects a profile of this kind today. Writing a row would
    /// hand the resolver a value where it has none, which is the arming-by-migration 0057
    /// Question 3 is about. For a RUN profile this is not even a paper/live difference: with no run
    /// profile a live mount REFUSES TO START (`vike_mount::MountError::MissingRiskBudget` — see
    /// `vike_config::ceilings`'s `absent_means` for `max_notional_per_order`), so the row would turn
    /// a daemon that exits FAILURE into a daemon that trades.
    NothingSelectedToday,
    /// Something IS selected today, but the profile being migrated is not it. Writing the row would
    /// silently repoint the daemon at a different body.
    WouldRepoint {
        /// What is in force today.
        in_force: String,
        /// What the row would have named.
        proposed: String,
    },
    /// The store already holds an active row for this kind, and it names something else. This is
    /// not a migration's decision to overturn.
    AlreadyActive {
        /// The name the store already carries.
        held_by: String,
    },
}

impl std::fmt::Display for WithholdReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WithholdReason::NothingSelectedToday => write!(
                f,
                "no active row was written: nothing selects a profile of this kind on this box \
                 today, and a row that selected one would be this migration ARMING a mount that an \
                 absent selection was holding. The profile BODY was stored; activate it \
                 deliberately if that is what you want"
            ),
            WithholdReason::WouldRepoint { in_force, proposed } => write!(
                f,
                "no active row was written: `{in_force}` is what this box runs today and the row \
                 would have named `{proposed}`, which is a change of what the daemon trades rather \
                 than a change of where it is stored"
            ),
            WithholdReason::AlreadyActive { held_by } => write!(
                f,
                "no active row was written: the store already selects `{held_by}` for this kind, \
                 and a migration does not overturn a selection an operator made"
            ),
        }
    }
}

/// **THE ONE PLACE A MIGRATION MAY DECIDE TO ARM SOMETHING — and it says no unless it can prove it
/// is not arming anything.**
///
/// The rule, in one sentence: *write an active row only when the row names exactly what is already
/// in force, so the resolved selection before and after the migration is the same string.*
///
/// * `in_force` — what selects this kind TODAY, with the store's own rung removed: the `--config`
///   argument for a daemon profile, `--profile`/`VIKE_RUN_PROFILE` for a run profile, reduced to the
///   profile NAME the body was stored under. `None` means nothing selects one.
/// * `already_active` — what the store's `active` row holds for this kind before the migration.
/// * `proposed` — the name the migration is storing the body under.
///
/// # ⚠ Read the `None` arm twice
///
/// `in_force: None` is the state that LOOKS safest and is the one the record singles out. On a box
/// where `flags.tradehub_live` is on and no run profile is selected, today's outcome is a daemon
/// that refuses to start. Writing a row there does not "restore" anything: it starts a live mount
/// that has never run.
#[must_use]
pub fn plan_active_row(
    in_force: Option<&str>,
    already_active: Option<&str>,
    proposed: &str,
) -> ActivePlan {
    if let Some(held) = already_active {
        if held == proposed {
            return ActivePlan::Write { name: proposed.to_string() };
        }
        return ActivePlan::Withhold {
            reason: WithholdReason::AlreadyActive { held_by: held.to_string() },
        };
    }
    match in_force {
        None => ActivePlan::Withhold { reason: WithholdReason::NothingSelectedToday },
        Some(n) if n == proposed => ActivePlan::Write { name: proposed.to_string() },
        Some(n) => ActivePlan::Withhold {
            reason: WithholdReason::WouldRepoint {
                in_force: n.to_string(),
                proposed: proposed.to_string(),
            },
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// Do the profile tables exist in this store?
///
/// The probe that lets [`read_profiles`] answer [`Profiles::none`] for a store that predates
/// Phase 3 instead of erroring — which is the difference between "this box has not migrated" and
/// "this box is broken", and the second answer would be false.
fn profile_tables_present(conn: &rusqlite::Connection, path: &Path) -> Result<bool, DbError> {
    let found: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN \
             ('profile', 'mount', 'mount_param', 'profile_setting')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| DbError::sql(path, e))?;
    Ok(usize::try_from(found).unwrap_or(0) == CORE_PROFILE_TABLES.len())
}

/// Does this store carry the RECORDER tables? Asked rather than assumed — see
/// [`CORE_PROFILE_TABLES`] for why the main presence probe may not require them.
fn recorder_tables_present(conn: &rusqlite::Connection, path: &Path) -> Result<bool, DbError> {
    let found: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN \
             ('recorder', 'subscription')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| DbError::sql(path, e))?;
    Ok(found == 2)
}

/// **Every profile the store holds.** The whole read half of Phase 3, and it needs no unit change:
/// 0057's EROFS section measures that `ProtectSystem=strict` leaves reads untouched and that the
/// composition root already opens this store read-only at mount.
///
/// Three states collapse to [`Profiles::none`] — no database, no profile tables, no rows — because
/// the daemon must behave identically in all three and today.
///
/// # Errors
///
/// [`ProfileError::Db`] when the store exists and cannot be read (never when it is absent, which is
/// the ordinary unconfigured state), and [`ProfileError::UnreadableKind`] for a `recorder` row.
pub fn read_profiles(path: &Path) -> Result<Profiles, ProfileError> {
    if !database_present(path) {
        return Ok(Profiles::none());
    }
    let (conn, _version) = open_for_read(path)?;
    if !profile_tables_present(&conn, path)? {
        return Ok(Profiles::none());
    }
    let mut rows: Vec<ProfileRow> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT name, kind, active, note FROM profile ORDER BY name")
            .map_err(|e| DbError::sql(path, e))?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| DbError::sql(path, e))?;
        for row in mapped {
            let (name, kind, active, note) = row.map_err(|e| DbError::sql(path, e))?;
            rows.push(ProfileRow { name, kind: ProfileKind::parse(&kind)?, active, note });
        }
    }
    let mut out = Vec::new();
    for row in rows {
        let mounts = read_mounts(&conn, path, &row.name)?;
        let params = read_params(&conn, path, &row.name)?;
        let settings = read_settings(&conn, path, &row.name)?;
        let recorder = read_recorder(&conn, path, &row.name)?;
        out.push(StoredProfile { row, mounts, params, settings, recorder });
    }
    Ok(Profiles { profiles: out, tables_present: true })
}

/// The `recorder` row and its subscriptions, or `None` when this profile has no recorder body.
///
/// ⚠ A profile of any kind is asked, not just [`ProfileKind::Recorder`]: the absence is read off
/// the TABLE rather than inferred from the kind word, so a row written under the wrong kind is
/// visible to a caller instead of being silently unreadable.
fn read_recorder(
    conn: &rusqlite::Connection,
    path: &Path,
    profile: &str,
) -> Result<Option<RecorderBody>, DbError> {
    if !recorder_tables_present(conn, path)? {
        return Ok(None);
    }
    let row: Option<RecorderRow> = conn
        .query_row(
            "SELECT store, interval_secs, min_parts, target_mb, max_merge_rows, retention_days, \
             alert_webhooks, alert_repeat_secs, alert_series_prefix, note FROM recorder \
             WHERE profile = ?1",
            [profile],
            |r| {
                Ok(RecorderRow {
                    store: r.get(0)?,
                    interval_secs: r.get(1)?,
                    min_parts: r.get(2)?,
                    target_mb: r.get(3)?,
                    max_merge_rows: r.get(4)?,
                    retention_days: r.get(5)?,
                    alert_webhooks: r.get(6)?,
                    alert_repeat_secs: r.get(7)?,
                    alert_series_prefix: r.get(8)?,
                    note: r.get(9)?,
                })
            },
        )
        .optional()
        .map_err(|e| DbError::sql(path, e))?;
    let Some(row) = row else { return Ok(None) };
    let mut stmt = conn
        .prepare(
            "SELECT ord, venue, family, symbols, backfill, note FROM subscription \
             WHERE profile = ?1 ORDER BY ord",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let mapped = stmt
        .query_map([profile], |r| {
            Ok(SubscriptionRow {
                ord: r.get(0)?,
                venue: r.get(1)?,
                family: r.get(2)?,
                symbols: r.get(3)?,
                backfill: r.get(4)?,
                note: r.get(5)?,
            })
        })
        .map_err(|e| DbError::sql(path, e))?;
    let mut subscriptions = Vec::new();
    for s in mapped {
        subscriptions.push(s.map_err(|e| DbError::sql(path, e))?);
    }
    Ok(Some(RecorderBody { row, subscriptions }))
}

fn read_mounts(
    conn: &rusqlite::Connection,
    path: &Path,
    profile: &str,
) -> Result<Vec<MountRow>, DbError> {
    let mut stmt = conn
        .prepare(
            "SELECT ord, is_primary, venue, asset_class, symbol, token_id, interval, \
             interval_ms, resolution_ts_ms, qty, half_spread, tick_size, seed_cash, data_only, \
             account, strategy_name, strategy_rhai FROM mount WHERE profile = ?1 ORDER BY ord",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let mapped = stmt
        .query_map([profile], |r| {
            Ok(MountRow {
                ord: r.get(0)?,
                is_primary: r.get::<_, i64>(1)? != 0,
                venue: r.get(2)?,
                asset_class: r.get(3)?,
                symbol: r.get(4)?,
                token_id: r.get(5)?,
                interval: r.get(6)?,
                interval_ms: r.get(7)?,
                resolution_ts_ms: r.get(8)?,
                qty: r.get(9)?,
                half_spread: r.get(10)?,
                tick_size: r.get(11)?,
                seed_cash: r.get(12)?,
                data_only: r.get::<_, Option<i64>>(13)?.map(|v| v != 0),
                account: r.get(14)?,
                strategy_name: r.get(15)?,
                strategy_rhai: r.get(16)?,
            })
        })
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row.map_err(|e| DbError::sql(path, e))?);
    }
    Ok(out)
}

fn read_params(
    conn: &rusqlite::Connection,
    path: &Path,
    profile: &str,
) -> Result<BTreeMap<(i64, String), String>, DbError> {
    let mut stmt = conn
        .prepare(
            "SELECT mount_ord, key, value FROM mount_param WHERE profile = ?1 \
             ORDER BY mount_ord, key",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let mapped = stmt
        .query_map([profile], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = BTreeMap::new();
    for row in mapped {
        let (ord, key, value) = row.map_err(|e| DbError::sql(path, e))?;
        out.insert((ord, key), value);
    }
    Ok(out)
}

fn read_settings(
    conn: &rusqlite::Connection,
    path: &Path,
    profile: &str,
) -> Result<BTreeMap<String, String>, DbError> {
    let mut stmt = conn
        .prepare("SELECT path, value FROM profile_setting WHERE profile = ?1 ORDER BY path")
        .map_err(|e| DbError::sql(path, e))?;
    let mapped = stmt
        .query_map([profile], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = BTreeMap::new();
    for row in mapped {
        let (p, v) = row.map_err(|e| DbError::sql(path, e))?;
        out.insert(p, v);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Writing — the operator's own act
// ---------------------------------------------------------------------------------------------

/// **The capability every write here requires.**
///
/// It is not a permission check and it does not pretend to be one: a type cannot tell which process
/// is holding it. What it is, is the thing that makes "only the operator's CLI writes profile rows"
/// a fact a GATE can hold rather than a convention a reviewer has to remember —
/// `crates/vike-ops/tests/profile_writer_gate.rs` pins the files that may construct one, both
/// directions.
///
/// # ⚠ Why not a cargo feature, which is the obvious answer
///
/// MEASURED constraint, not a preference: a `profile-write` feature enabled only by `vike-cli` does
/// nothing under a workspace build. Resolver 2 unifies features across the whole selection, so
/// `cargo test --workspace` would enable it for every consumer at once and the daemon would compile
/// against the writers — the *light-consumers* failure class the root `CLAUDE.md` names, which has
/// already broken `main` once. A feature would make the seal LOOK structural in CI and be absent in
/// the one build that matters.
///
/// # What actually stops the daemon
///
/// Not this type. **The kernel does.** 0057 measures that no shipped unit grants `settings/db`:
/// `deploy/vike-tradehub.service`'s `ReadWritePaths` names `settings/state` alone under
/// `ProtectSystem=strict`, so the daemon's own mount namespace cannot write this file at all, and
/// the GUI reaches the box only through the daemon. This type is what keeps a future in-process
/// writer from being added by accident on the day that grant is paid.
#[derive(Debug, Clone)]
pub struct OperatorWrite {
    actor: String,
}

impl OperatorWrite {
    /// Claim the capability, naming the ACTOR the change journal will carry.
    ///
    /// `actor` is a human-meaningful label for who is writing (`"vike-cli profile activate"`), not
    /// a credential and not a user id — it lands in `updated_by` and is printed.
    #[must_use]
    pub fn claim(actor: &str) -> Self {
        OperatorWrite { actor: actor.to_string() }
    }

    /// The actor string this claim carries.
    #[must_use]
    pub fn actor(&self) -> &str {
        &self.actor
    }
}

/// **Refuse a write against a store that does not exist**, rather than letting `open_for_write`
/// create one.
///
/// Called first by every write in this module, and it is the single most important line here.
/// `crate::db`'s `open_for_write` CREATES the database when the path is absent — which is correct
/// for the migration that owns creating it and catastrophic for anything else, because
/// `crate::store::Backend` decides which store answers credentials from ONE `is_file` on this path.
/// An empty database brought into existence by a profile write would silently become the credential
/// store, the file beside it would stop being read, and every venue on the box would drop to paper
/// with `secrets.env` looking perfectly correct.
///
/// It is also the reason nothing here stamps `PRAGMA user_version`: a store that already exists is
/// already stamped, so there is no half-finished state for this module to create or to repair.
fn refuse_absent_store(path: &Path) -> Result<(), ProfileError> {
    if database_present(path) {
        return Ok(());
    }
    Err(ProfileError::NoStore { path: path.to_path_buf() })
}

/// Create the profile tables if they are absent. Idempotent, and it does **not** touch
/// `PRAGMA user_version` — see this module's doc for why that is a safety property rather than an
/// omission.
///
/// # Errors
///
/// [`ProfileError::Db`] when the store cannot be opened for writing — which on a deployed daemon's
/// box, run as the daemon, is exactly what should happen.
pub fn ensure_tables(
    path: &Path,
    _write: &OperatorWrite,
    asset_class_words: &[&str],
) -> Result<(), ProfileError> {
    refuse_absent_store(path)?;
    let (conn, _created, _version) = open_for_write(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| DbError::sql(path, e))?;
    Ok(())
}

/// Store ONE profile's body — the identity row, its mounts, its params and its settings — replacing
/// any body already stored under that name.
///
/// **It never sets `active`.** Storing a body is not selecting one, which is the separation 0057's
/// Phase 3 rests on and the reason a migration can run on a live box with nothing changing.
/// [`set_active`] is the separate, deliberate act.
///
/// # Errors
///
/// [`ProfileError::Db`] on any store failure. The schema's own CHECKs refuse a malformed mount (two
/// symbol spellings, two primaries) before a row lands.
pub fn store_profile(
    path: &Path,
    profile: &StoredProfile,
    write: &OperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), ProfileError> {
    refuse_absent_store(path)?;
    let (mut conn, _created, _version) = open_for_write(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| DbError::sql(path, e))?;
    // IMMEDIATE, not the default DEFERRED: this is a read-then-write (the `active` bit is preserved
    // from the row already there), and `crate::db`'s `BUSY_TIMEOUT` doc records why a deferred
    // promotion is the one case SQLite refuses without consulting the busy handler at all.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(path, e))?;
    // ⚠ PRESERVE the existing `active` bit across a body replacement. Re-storing a body is an edit
    // of WHAT a profile is, never of WHETHER it is the one running, and a re-store that silently
    // deactivated the live profile would be this module's own hazard wearing the other sign.
    let held: Option<i64> = tx
        .query_row("SELECT active FROM profile WHERE name = ?1", [&profile.row.name], |r| r.get(0))
        .optional()
        .map_err(|e| DbError::sql(path, e))?;
    let active = held.unwrap_or(i64::from(profile.row.active));
    tx.execute("DELETE FROM profile WHERE name = ?1", [&profile.row.name])
        .map_err(|e| DbError::sql(path, e))?;
    tx.execute(
        "INSERT INTO profile (name, kind, active, note, updated_utc, updated_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            profile.row.name,
            profile.row.kind.sql_word(),
            active,
            profile.row.note,
            now_utc,
            write.actor(),
        ],
    )
    .map_err(|e| DbError::sql(path, e))?;
    for m in &profile.mounts {
        tx.execute(
            "INSERT INTO mount (profile, ord, is_primary, venue, asset_class, symbol, token_id, \
             interval, interval_ms, resolution_ts_ms, qty, half_spread, tick_size, seed_cash, \
             data_only, account, strategy_name, strategy_rhai) VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
             ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            rusqlite::params![
                profile.row.name,
                m.ord,
                i64::from(m.is_primary),
                m.venue,
                m.asset_class,
                m.symbol,
                m.token_id,
                m.interval,
                m.interval_ms,
                m.resolution_ts_ms,
                m.qty,
                m.half_spread,
                m.tick_size,
                m.seed_cash,
                m.data_only.map(i64::from),
                m.account,
                m.strategy_name,
                m.strategy_rhai,
            ],
        )
        .map_err(|e| DbError::sql(path, e))?;
    }
    for ((ord, key), value) in &profile.params {
        tx.execute(
            "INSERT INTO mount_param (profile, mount_ord, key, value) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![profile.row.name, ord, key, value],
        )
        .map_err(|e| DbError::sql(path, e))?;
    }
    for (p, value) in &profile.settings {
        tx.execute(
            "INSERT INTO profile_setting (profile, path, value, updated_utc, updated_by) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![profile.row.name, p, value, now_utc, write.actor()],
        )
        .map_err(|e| DbError::sql(path, e))?;
    }
    // ⚠ The `DELETE FROM profile` above cascaded these away with everything else, so a body
    // replacement that drops a subscription genuinely drops it rather than leaving an orphan the
    // next read would feed to a venue.
    if let Some(body) = &profile.recorder {
        tx.execute(
            "INSERT INTO recorder (profile, store, interval_secs, min_parts, target_mb, \
             max_merge_rows, retention_days, alert_webhooks, alert_repeat_secs, \
             alert_series_prefix, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                profile.row.name,
                body.row.store,
                body.row.interval_secs,
                body.row.min_parts,
                body.row.target_mb,
                body.row.max_merge_rows,
                body.row.retention_days,
                body.row.alert_webhooks,
                body.row.alert_repeat_secs,
                body.row.alert_series_prefix,
                body.row.note,
            ],
        )
        .map_err(|e| DbError::sql(path, e))?;
        for s in &body.subscriptions {
            tx.execute(
                "INSERT INTO subscription (profile, ord, venue, family, symbols, backfill, note) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    profile.row.name,
                    s.ord,
                    s.venue,
                    s.family,
                    s.symbols,
                    s.backfill,
                    s.note,
                ],
            )
            .map_err(|e| DbError::sql(path, e))?;
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(())
}

/// **Select a profile: the deliberate act, and the only one that changes what this box trades.**
///
/// Clears the kind's previous active row in the same transaction, so the partial unique index can
/// never be the thing that reports a two-active state to an operator mid-edit.
///
/// # Errors
///
/// [`ProfileError::Db`] on a store failure, which includes the read-only refusal a process inside
/// the daemon's namespace gets.
pub fn set_active(
    path: &Path,
    kind: ProfileKind,
    name: &str,
    write: &OperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), ProfileError> {
    refuse_absent_store(path)?;
    let (mut conn, _created, _version) = open_for_write(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| DbError::sql(path, e))?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(path, e))?;
    tx.execute(
        "UPDATE profile SET active = 0, updated_utc = ?2, updated_by = ?3 WHERE kind = ?1",
        rusqlite::params![kind.sql_word(), now_utc, write.actor()],
    )
    .map_err(|e| DbError::sql(path, e))?;
    tx.execute(
        "UPDATE profile SET active = 1, updated_utc = ?2, updated_by = ?3 \
         WHERE name = ?1 AND kind = ?4",
        rusqlite::params![name, now_utc, write.actor(), kind.sql_word()],
    )
    .map_err(|e| DbError::sql(path, e))?;
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(())
}

/// Deselect whatever is active for one kind, returning the box to the state every box is in today.
///
/// # Errors
///
/// [`ProfileError::Db`] on a store failure.
pub fn clear_active(
    path: &Path,
    kind: ProfileKind,
    write: &OperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), ProfileError> {
    refuse_absent_store(path)?;
    let (conn, _created, _version) = open_for_write(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| DbError::sql(path, e))?;
    conn.execute(
        "UPDATE profile SET active = 0, updated_utc = ?2, updated_by = ?3 WHERE kind = ?1",
        rusqlite::params![kind.sql_word(), now_utc, write.actor()],
    )
    .map_err(|e| DbError::sql(path, e))?;
    Ok(())
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    /// The vocabulary a production caller hands over (`vike_catalog::AssetClass::SQL_WORDS`).
    ///
    /// ⚠ **A FIXTURE, never an authority** — this crate is a zero-`vike-*`-dependency leaf and cannot
    /// name that enum, which is the whole reason [`profile_ddl`] takes the list as a parameter. What
    /// is asserted here is the SCHEMA mechanism. The cross-crate pin that these words really are the
    /// enum's lives in `crates/vike-tradehub/tests/daemon/profile_rows.rs`, the one place that can
    /// see both crates.
    const WORDS: &[&str] = &[
        "Equity",
        "Etf",
        "CryptoSpot",
        "CryptoPerp",
        "CryptoFuture",
        "Option",
        "Fx",
        "Future",
        "Index",
        "PredictionMarket",
        "Cfd",
    ];

    fn schema() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory store");
        conn.execute_batch(&profile_ddl(WORDS).expect("renders")).expect("schema applies");
        conn.execute("INSERT INTO profile (name, kind, active) VALUES ('p', 'daemon', 0)", [])
            .expect("parent row");
        conn
    }

    /// **`mount.asset_class` is NOT NULL** — a mount that does not say which product it trades is
    /// refused by the schema. `MountRow` cannot express the absence at all (its constructor takes
    /// the class), so the database is the only place this refusal is observable.
    #[test]
    fn a_mount_row_with_no_asset_class_is_refused() {
        let conn = schema();
        let err = conn
            .execute(
                "INSERT INTO mount (profile, ord, is_primary, venue, asset_class, symbol) \
                 VALUES ('p', 0, 0, 'bybit', NULL, 'BTCUSDT')",
                [],
            )
            .expect_err("a NULL asset_class must be refused");
        assert!(
            err.to_string().to_uppercase().contains("NOT NULL"),
            "expected a NOT NULL violation, got: {err}"
        );
    }

    /// **The `CHECK` refuses a word outside the vocabulary**, however plausible it looks — which is
    /// what stops a hand-edited store or a future writer inventing a twelfth product.
    #[test]
    fn a_word_outside_the_vocabulary_is_refused_by_the_check() {
        let conn = schema();
        for (i, bad) in
            ["cryptospot", "Perp", "Spot", "CRYPTOSPOT", "", "Crypto Spot"].iter().enumerate()
        {
            let err = conn
                .execute(
                    "INSERT INTO mount (profile, ord, is_primary, venue, asset_class, symbol) \
                     VALUES ('p', ?1, 0, 'bybit', ?2, 'BTCUSDT')",
                    rusqlite::params![i as i64, bad],
                )
                .expect_err("a word outside the vocabulary must be refused");
            assert!(
                err.to_string().to_uppercase().contains("CHECK"),
                "expected a CHECK violation for {bad:?}, got: {err}"
            );
        }
    }

    /// ...and every word the vocabulary DOES carry is accepted. Without this half the test above
    /// would pass over a `CHECK` that refuses everything.
    #[test]
    fn every_word_in_the_vocabulary_is_accepted() {
        let conn = schema();
        for (i, word) in WORDS.iter().enumerate() {
            conn.execute(
                "INSERT INTO mount (profile, ord, is_primary, venue, asset_class, symbol) \
                 VALUES ('p', ?1, 0, 'bybit', ?2, 'BTCUSDT')",
                rusqlite::params![i as i64, word],
            )
            .unwrap_or_else(|e| panic!("`{word}` is in the vocabulary and must be accepted: {e}"));
        }
    }

    /// **The template spells no asset-class word of its own.** Rendered with a foreign vocabulary,
    /// the `CHECK` carries exactly that vocabulary and none of the real one — which is the property
    /// that makes the Rust enum the single declaration rather than one of two lists.
    #[test]
    fn the_schema_carries_only_the_vocabulary_it_was_given() {
        let ddl = profile_ddl(&["Alpha", "Beta"]).expect("renders");
        assert!(ddl.contains("CHECK (asset_class IN ('Alpha', 'Beta'))"), "{ddl}");
        for word in WORDS {
            assert!(
                !ddl.contains(&format!("'{word}'")),
                "the template carries a hardcoded asset-class word ({word}) — the vocabulary must \
                 arrive from the caller, or there are two lists"
            );
        }
        let real = profile_ddl(WORDS).expect("renders");
        for word in WORDS {
            assert!(real.contains(&format!("'{word}'")), "{word} missing from the rendered CHECK");
        }
        assert!(
            !real.contains(ASSET_CLASS_WORDS_PLACEHOLDER),
            "the placeholder must be fully substituted:\n{real}"
        );
    }

    /// A vocabulary this module will not render is REFUSED rather than interpolated. The only caller
    /// hands over `&'static str`s off a closed enum, so reaching this is a bug in the caller — but
    /// the thing standing between caller data and a SQL clause must be a refusal, not an assumption.
    #[test]
    fn an_unrenderable_vocabulary_is_refused() {
        for bad in
            [&["Crypto Spot"][..], &["it's"][..], &["a'); DROP TABLE mount;--"][..], &[""][..]]
        {
            let e = profile_ddl(bad)
                .expect_err("a non-identifier word must never be rendered into SQL");
            assert!(matches!(e, ProfileError::UnrenderableVocabulary { .. }), "{e}");
        }
        let e = profile_ddl(&[])
            .expect_err("an empty vocabulary would refuse every mount row there is");
        assert!(e.to_string().contains("EMPTY"), "{e}");
    }

    /// Every table the presence probe knows about is still created by the rendered DDL — the
    /// parameterisation must not have dropped one.
    #[test]
    fn the_rendered_schema_still_creates_every_declared_table() {
        let conn = schema();
        for table in PROFILE_TABLES {
            let found: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .expect("query sqlite_master");
            assert_eq!(found, 1, "`{table}` is not created by the rendered schema");
        }
    }
}
