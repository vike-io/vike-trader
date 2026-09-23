//! **Schema 2 — the account is a ROW.** The DDL of
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4 and §6, the derivation that
//! fills it, and the in-place reshape that carries a schema-1 store into it.
//!
//! Schema 1 was two flat `(name, value)` tables. What it could not say is the thing §1 of that spec
//! is about: WHICH ACCOUNT a credential belongs to. The account lived in the key NAME
//! (`DUKASCOPY_DEMO1_LOGIN` bakes an account INDEX into the tier token), so an account could go
//! stale with nothing in the store able to contradict it. Schema 2 gives the account a row, a
//! permanent opaque `id`, and a place for the venue's own answer (`venue_account_id`).
//!
//! # ⚠ What this module does NOT do, and why that is the spec's own rule rather than a shortcut
//!
//! §12 states it as a hard sequencing requirement rather than a follow-up:
//!
//! > Ruling 10 moves ten values out from under the readers that find them today; §6.2's ordering
//! > (A) pays for that with the renderer and ordering (B) with four bridge PRs, but NEITHER is
//! > written and until one is, **THE ROWS MAY NOT MOVE.**
//!
//! So §11's steps **3 and 4 are not performed here**. The ten book keys are NOT folded into
//! `account.venue_account_id` and the ten config-shaped keys are NOT moved to `venue_setting`;
//! every one of them stays a `credential` row carrying its legacy `name`, and
//! [`Classification::pending_move`] is how each one says so BY NAME in the migration's report. The
//! consequence is the property this whole change rests on: **`credential` still holds every live
//! name**, so [`crate::db::read_table`] answers with the same map before and after, and no renderer
//! is needed to make that true. `venue_setting` is created EMPTY for the same reason — a table is a
//! SHAPE, and a ROW nothing reads is the defect `vike_config::CONSUMPTION` exists to refuse.
//!
//! It also writes no `venue_account_id`, on either dukascopy row. See [`DDL`]'s `label` note.
//!
//! # The classification is INJECTED, exactly like `is_node_key`
//!
//! `field`, `account_id` and `venue` are derived from the key NAME, and that derivation needs
//! `vike_model::account_keys` and the venue roster. **This crate declares no `vike-*` dependency**
//! (`crates/vike-secrets/Cargo.toml`'s layer-15 `leaf` tier, and two consumers rest on it), so the
//! derivation arrives as a closure returning [`Classification`] — the same seam, for the same
//! reason, that [`crate::db::migrate`] already takes `is_node_key` through.
//! `vike_bridge_core::credentials::classify_credential_name` is the production implementation.

use std::collections::BTreeMap;

use rusqlite::Transaction;

/// **The tier vocabulary of [`AccountKey::tier`]** — the arming ceiling's spelling, lowercase.
///
/// §4's DDL comment names it *"paper | demo | live (the arming ceiling's vocabulary)"*, i.e.
/// `vike_config::VenueMode`'s, and the column is CHECK-constrained against this list because
/// `STRICT` constrains TYPES and not values — so without it the first reader to join
/// `account.tier` against a `VenueMode` would be comparing `"DEMO"` with `"demo"` and nothing in
/// the store would have objected.
///
/// Two deliberate differences from that comment, each a consequence of what a credential IS:
///
/// * **`paper` is absent.** A paper venue loads no credential (`vike_mount::make_engine` returns
///   the paper client having read none), so a paper account has no credential and therefore no
///   row. A CHECK admitting `paper` would be admitting a state nothing can reach.
/// * **`sim` is present.** `vike_model::credential_keys::CREDENTIAL_TIERS` is
///   `["SIM", "DEMO", "LIVE"]`, so a `{VENUE}_SIM_*` key is a credential this migration can be
///   handed today, and a CHECK without it would refuse a store that is already legal.
pub const ACCOUNT_TIERS: [&str; 3] = ["sim", "demo", "live"];

/// **Which account a credential name belongs to**, as the classifier reports it.
///
/// `venue`/`tier`/`label` are `vike_model::account_keys::AccountRef`'s three fields — §4.1's
/// `UNIQUE (venue, tier, label)` is that tuple and not an invention — carried as owned `String`s
/// because this crate cannot name that type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountKey {
    /// A `vike_model::VENUES` id.
    pub venue: String,
    /// One of [`ACCOUNT_TIERS`].
    pub tier: String,
    /// The operator's name for the ROLE. **`None` is the ordinary answer and is what the migration
    /// writes for every account it creates**, because `AccountLabel::Default` renders as ABSENT
    /// everywhere else in this workspace — its serde impl skips it, and
    /// `AccountLabel::parse("DEFAULT")` REFUSES the reserved spelling outright, so the literal
    /// string would not round-trip.
    pub label: Option<String>,
    /// ⚠ **Derivation-time only — it is stored in NO column.**
    ///
    /// Two accounts of one venue at one tier are indistinguishable by `(venue, tier, label)` when
    /// both labels are absent, and dukascopy is exactly that case: `DEMO1` and `DEMO2` are two
    /// accounts (ruling 1) and the owner ruled that neither gets a label. This field lets the
    /// classifier's HAND-MAP say *these two names belong to different accounts* without putting the
    /// index token into a column — which is the defect §1 is about, and §11.1's own rejected
    /// alternative. `None` for every other venue.
    pub discriminator: Option<String>,
}

/// Where a credential row sits — §5.1's three cases, as the pair `(account_id, venue)` expresses
/// them. **The pair IS the classification; there is no separate kind column to keep in step.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// `account_id` set, `venue` NULL — the venue is on the account row. `OKX_DEMO_API_SECRET`.
    Account(AccountKey),
    /// `account_id` NULL, `venue` set — an APPLICATION credential shared by every account of the
    /// venue. `CTRADER_CLIENT_ID`. ⚠ `venue` here means *belongs to this venue's plane*, not
    /// *issued by this venue*.
    Venue(String),
    /// Both NULL — the deployment's own credentials. `CLOUDFLARE_API_TOKEN`.
    Infrastructure,
}

/// A row this change deliberately does not move, and the spec section that will.
///
/// Reported BY NAME so the next change takes its work-list out of a run rather than out of prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PendingMove {
    /// §7 — this value IS the book, and folds into `account.venue_account_id` once the map renderer
    /// can re-synthesize its legacy name.
    BookIdentifier,
    /// §6 / ruling 10 — machine- or tier-scoped configuration that holds no secret, and moves to
    /// `venue_setting` behind the same renderer.
    VenueSetting,
}

impl std::fmt::Display for PendingMove {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PendingMove::BookIdentifier => "would fold into account.venue_account_id (spec 7)",
            PendingMove::VenueSetting => "would move to venue_setting (spec 6, ruling 10)",
        })
    }
}

/// **What one credential NAME is**, as the injected classifier answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// §5.1's three cases.
    pub placement: Placement,
    /// §4.4 — the name with its OWNER PREFIX removed; the WHOLE name for an infrastructure row,
    /// whose owner is the deployment.
    pub field: String,
    /// `false` for the rows §6 MEASURED as holding no secret. `true` everywhere else: a value
    /// wrongly marked non-secret is a worse error than one wrongly marked secret.
    pub secret: bool,
    /// `false` when the classifier could not place this name and fell back to
    /// [`Placement::Infrastructure`]. The row is still written with `name` and `value` VERBATIM —
    /// **a name the parse cannot classify is never dropped and never guessed at** — and it is
    /// REPORTED by name (§11.1).
    pub recognised: bool,
    /// Set when this row is one §7 or §6 will move later. See [`PendingMove`].
    pub pending_move: Option<PendingMove>,
}

impl Classification {
    /// The unrecognised fallback — §11 step 6, and the shape a classifier returns when it has
    /// nothing to say about a name.
    #[must_use]
    pub fn unrecognised(name: &str) -> Classification {
        Classification {
            placement: Placement::Infrastructure,
            field: name.to_string(),
            secret: true,
            recognised: false,
            pending_move: None,
        }
    }

    /// The OWNER PREFIX this classification implies — the name with [`Classification::field`]
    /// removed from its end.
    ///
    /// ⚠ **This is the account's re-derivable identity ACROSS RUNS**, and it is why the migration
    /// needs no stored discriminator. Every credential row keeps its legacy `name` (§4.1: *"`name`
    /// is the only record"*) and its `field`, so an account's owner prefix — `DUKASCOPY_DEMO1_` —
    /// is recoverable from any one of its own rows. A later run therefore finds the account an
    /// earlier run created, without either of them putting the index token in a column.
    ///
    /// ⚠ `None` for a LABELLED account, and that is not a failure: a labelled key's `field` is not
    /// a suffix of its name (the `__LABEL` sits after it), and a labelled account does not need
    /// this lookup — `(venue, tier, label)` is unique for it, so [`AccountResolver`]'s SECOND
    /// lookup answers. The prefix exists for the one case nothing else can answer: an UNLABELLED
    /// account that shares `(venue, tier, label)` with another, i.e. dukascopy's pair.
    ///
    /// ⚠ It is ALSO `None` when a classifier contradicts itself by naming a `field` that is no
    /// part of the name's end — and **that is refused only where it is not survivable**, which is
    /// the discriminated-unlabelled shape above. This doc said it "IS refused" flatly, and
    /// [`SchemaRefusal::FieldIsNotASuffix`]'s own doc said the same; both were wider than
    /// [`write_rows`], which raises that refusal under
    /// `Classification::needs_owner_prefix` alone. A blanket refusal is not
    /// available and never was: a LABELLED key's `field` is legitimately not a suffix of its name,
    /// so refusing every non-suffix would refuse the whole labelled grammar.
    #[must_use]
    pub fn owner_prefix<'a>(&self, name: &'a str) -> Option<&'a str> {
        name.strip_suffix(self.field.as_str())
    }

    /// Is this an account-scoped row whose account can ONLY be found by its owner prefix?
    ///
    /// True exactly when the account is unlabelled AND the classifier offered a discriminator —
    /// the dukascopy shape, where `(venue, tier, label)` has two answers and the discriminator
    /// reaches no column. For every other row a missing prefix is survivable.
    fn needs_owner_prefix(&self) -> bool {
        matches!(
            &self.placement,
            Placement::Account(key) if key.label.is_none() && key.discriminator.is_some()
        )
    }
}

/// **The schema-2 DDL — §4's two tables, §6's third, and `node_key` unchanged.**
///
/// Stated once, here, and applied both by a fresh create and by [`reshape_into`], so a store BORN
/// at 2 and a store CARRIED to 2 cannot end up different shapes.
///
/// # Three deviations from §4's printed DDL, each argued
///
/// **1. `label` is NULLABLE, where §4 prints `label TEXT NOT NULL`.** The signature at the head of
/// that spec rules that the provisional `DEMO1`/`DEMO2` labels *"are NOT written at all (labels are
/// informative and optional, `id` is the identity…)"*. That ruling and a `NOT NULL` label cannot
/// both hold: §11.1's surviving rule derives `RESERVED_DEFAULT_LABEL` for every account, dukascopy
/// has TWO accounts at `(dukascopy, demo)`, and `UNIQUE (venue, tier, label)` would then refuse the
/// sixteenth row — §4.1 says exactly that in its own words (*"Adding `tier` fixes hyperliquid and
/// does NOT fix dukascopy"*), and the signature removed §11.1's answer without supplying a
/// replacement. Nullable is also the FAITHFUL mirror rather than a workaround: everywhere else in
/// this workspace `AccountLabel::Default` is rendered as ABSENT, and
/// `vike_model::account_keys::AccountLabel::parse` REFUSES the literal `"DEFAULT"` as reserved, so
/// a stored `'DEFAULT'` string would not parse back.
///
/// ⚠ **The cost is stated rather than hidden.** With every migrated label NULL,
/// `UNIQUE (venue, tier, label)` constrains NOTHING (NULLs are distinct in a SQLite index), so what
/// keeps a re-run from inserting a duplicate account is [`AccountResolver`]'s name- and
/// prefix-keyed lookup plus `credential_one_live_name`, not this index. The index starts biting the
/// moment an account is LABELLED, which is the state it was written for.
///
/// **2. `CHECK (tier IN …)`** — §4's comment names the vocabulary and `STRICT` cannot enforce it.
/// See [`ACCOUNT_TIERS`] for why the list is neither the comment's three words nor
/// `CREDENTIAL_TIERS`' three. The two `IN (0, 1)` checks are the same reasoning applied to the two
/// boolean columns, which `STRICT` types as `INTEGER` and would otherwise let hold `7`.
///
/// **3. `venue_setting` is created EMPTY** — see this module's doc. The table is the SHAPE the
/// signature accepted; the step that FILLS it is §11 step 4, which §12 forbids until the map
/// renderer exists.
///
/// **4. `account`, `credential`, `venue_arming` and `venue_setting` each carry a nullable
/// `venue_id INTEGER REFERENCES venue(id)` beside their text `venue` column** — a DUAL-WRITE half
/// with no reader yet. ⚠ **Nothing reads it; the text column stays authoritative.** It exists so a
/// later change can reshape onto the typed link without a flag day: EVERY writer of a row carrying a
/// text `venue` fills it beside its text sibling — [`AccountResolver::resolve`]'s and
/// [`crate::db::edit_account`]'s account INSERTs, [`write_rows`]'s two `credential` INSERTs that can
/// ever carry a non-NULL `venue` (its other two sit inside the `Placement::Account` branch, where
/// `venue` is provably `None` — see the comment at that branch's own `if let`), and
/// `write_settings`'s/`set_venue_setting_in`'s/[`crate::db::move_pending_rows`]'s `venue_arming`/
/// `venue_setting` INSERTs — [`crate::db`]'s `ensure_venue_id_columns` backfills a store that
/// predates it (and is ALSO called directly by the four writers that never pass through
/// [`crate::db::fill_into`], so none of them can hit a missing column on an existing store), and
/// `venue_id_agrees_with_the_text_column_on_every_row` (`crates/vike-secrets/tests/venue_table.rs`)
/// pins that the two never disagree on any row, across all four tables.
/// `crates/vike-secrets/tests/store_link_gate.rs` does not count these four as untyped links (its
/// `observed()` skips any column carrying `REFERENCES`), so its pin stays at the four TEXT `venue`
/// columns it already named.
///
/// Everything else is §4 and §6 verbatim, including both partial indexes on `credential` (§4.1
/// argues why each must carry its `WHERE` — a plain `name … UNIQUE` would refuse the very
/// superseded rows §4.2 exists to keep), `venue_setting`'s two (NULLs are distinct, so ONE index
/// over `(venue, tier, field)` would say nothing at all about the machine-scoped rows), and the
/// `CHECK (account_id IS NULL OR venue IS NULL)` that refuses the fourth of §5.1's four
/// combinations.
///
/// `IF NOT EXISTS` throughout, for the reason schema 1's batch already gives: a database created by
/// a concurrent first run is JOINED rather than fought.
///
/// # ⚠ The last three tables are NOT the credential schema, and the boundary is [`crate::settings`]'s
///
/// `setting` and `venue_arming` arrive with
/// `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 1 and belong to
/// a different owner from everything above them. They are stated HERE for the reason this const's
/// opening line gives — one statement of shape, applied by every create path, so a store BORN
/// complete and a store CARRIED here cannot end up different — and the sentence that says which
/// table a new venue-scoped knob belongs in lives in [`crate::settings`]'s module doc, which is the
/// authority. ⚠ Neither is added to [`SCHEMA_VERSION`](crate::SCHEMA_VERSION): a bump would drop 2
/// out of [`READABLE_SCHEMA_VERSIONS`](crate::READABLE_SCHEMA_VERSIONS) and make both already-
/// migrated boxes' credential stores unreadable, which is the LIVE GATE. An already-migrated store
/// simply has no such table until a writer runs this batch again, and
/// [`crate::settings::read_settings`] answers [`crate::settings::SettingsSource::TablesAbsent`] for
/// it rather than pretending the rows are empty.
///
/// **`setting`'s `CHECK` is a closed VOCABULARY, never a bound.** `section IN (…)` is the same
/// reasoning as `tier IN (…)` above — four words a `STRICT` `TEXT` column cannot otherwise refuse.
/// A `CHECK` restating a NUMERIC bound (`max_leverage <= …`) is forbidden and deliberately absent:
/// `vike_config`'s typed model imports every bound from `vike_model`, and a second copy in SQL is
/// the split-brain that model exists to refuse.
///
/// **`venue_arming` carries no precedence column, no `overridable` flag and no per-row layer
/// field**, and that is a decision rather than an omission — 0057's `policy.toml` verdict in as
/// many words. A precedence column is how a ceiling acquires an override, and the seal that makes a
/// ceiling a ceiling is a property of the TYPE (`vike_config::layers`'s sealed `EnvOverride` /
/// `CliOverride`, which `Policy` does not implement), not of the carrier underneath the loader.
///
/// **`label` is NULLABLE and the two partial indexes are why**, exactly as `venue_setting`'s pair
/// above: NULLs are distinct in a SQLite index, so one `UNIQUE (venue, label)` would constrain
/// nothing about the venue-level rows. A NULL label is the `[venues]` row for that venue; a
/// non-NULL one is an `[accounts]` row for that venue's labelled account.
///
/// **`profile_risk` is 0057's Phase 2** — a RUN PROFILE's `[risk]` table, one row per key, and it
/// is a THIRD owner rather than a variant of either table above it. The boundary is spelled in
/// [`crate::settings`]'s module doc beside the other three; the two properties worth carrying at
/// the DDL itself:
///
/// * **`profile` is part of the key and is not optional**, because *which* profile is live is
///   0057's open Question 3 and this table must not answer it by accident. `VIKE_RUN_PROFILE` (or
///   `--profile`) names a PATH, a box may hold several, and a table keyed on `key` alone would be
///   "the risk budget" — i.e. an active-profile row written sideways. `UNIQUE (profile, key)` says
///   instead: these are the ceilings of the profile that goes by this name, and nothing here
///   selects one.
/// * **Nothing on the mount path reads it**, so no `CHECK` restates a numeric bound here either
///   (the rule the `setting` paragraph above states). The rows are a DISCLOSURE copy: the file the
///   boot resolves is still the only thing that builds a `vike_exec::ProfileRisk`, and
///   `crates/vike-ops/tests/profile_risk_readers_gate.rs` is what holds that true as the tree
///   changes. `vike_config::PROFILE_RISK_KEYS` is the key vocabulary, and it is a Rust roster
///   rather than a `CHECK` list for the reason `setting.key` has none: it is gated against
///   `vike_exec::ProfileRisk`'s own fields, and a SQL copy would be a second authority that drifts.
///
/// **`settings_adoption` is the PROBE, and it is a SEAL rather than a preference.**
///
/// One row, `CHECK (id = 1)`, written by `vike-cli config adopt` and by nothing else. Its presence
/// is the whole of the question *does this box resolve its settings from the rows or from the four
/// files* — decided ONCE per run, before any key is looked up, exactly as [`crate::Backend`]
/// decides which store answers for a credential NAME.
///
/// ⚠ **Why a row and not a count, a table probe or a `precedence` column**, each of which was
/// available and each of which is refused:
///
/// * A DATABASE-EXISTS or TABLES-EXIST probe is dead on arrival, and that is MEASURED rather than
///   argued: [`crate::db::open_for_write`] runs this whole batch on the `created` branch, so
///   `vike-cli secrets migrate` — a CREDENTIAL command — leaves `setting` and `venue_arming`
///   present and EMPTY on every fresh box. A probe shaped like either one would flip a box that
///   has mirrored nothing into resolving from zero rows, which is every ceiling absent and every
///   venue `paper` with `is_declared()` false.
/// * A ROW COUNT is a data fact with no declared intent behind it, so an accident moves the
///   crossing in both directions.
/// * A `precedence` COLUMN is refused for the reason the `venue_arming` paragraph above gives in
///   0057's own words: it puts the type's seal one `UPDATE` away from gone.
///
/// The columns are not disclosure. `venues_declared` is `VenuePolicy::is_declared` AT ADOPTION and
/// is the UNMOUNT detector — a `policy.toml` with no `[venues]` table mirrors to ZERO arming rows
/// deliberately, so *no arming rows* alone cannot be told from *the rows were erased*.
/// `setting_rows`/`arming_rows` are the ERASE detector: every sanctioned write updates them in its
/// own transaction, so a mismatch in either direction means rows moved by some other route.
/// `files_present` says what the box had when it crossed, which is what a later deletion is
/// deleting.
///
/// Like the three tables above it, NOT added to [`SCHEMA_VERSION`](crate::SCHEMA_VERSION) — same
/// live gate, and with a second property this one needs: an OLDER binary meeting an adopted store
/// does not see the table at all and resolves from the FILES, which is the safe direction for a
/// rollback.
///
/// Like the two before it, `profile_risk` is NOT added to [`SCHEMA_VERSION`](crate::SCHEMA_VERSION)
/// — the same live gate, measured the same way: a bump drops 2 out of
/// [`READABLE_SCHEMA_VERSIONS`](crate::READABLE_SCHEMA_VERSIONS) and makes both already-migrated
/// boxes' CREDENTIAL stores unreadable. Presence is asked of `sqlite_master`.
pub const DDL: &str = "\
CREATE TABLE IF NOT EXISTS node_key (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;

CREATE TABLE IF NOT EXISTS venue (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE
) STRICT;

CREATE TABLE IF NOT EXISTS account (
    id               INTEGER PRIMARY KEY,
    venue            TEXT    NOT NULL,
    venue_id         INTEGER REFERENCES venue(id),
    tier             TEXT    NOT NULL,
    label            TEXT,
    venue_account_id TEXT,
    parent_id        INTEGER REFERENCES account(id),
    active           INTEGER NOT NULL DEFAULT 1,
    last_verified_at TEXT,
    notes            TEXT,
    UNIQUE (venue, tier, label),
    CHECK (tier IN ('sim', 'demo', 'live')),
    CHECK (active IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS account_one_account_per_book
    ON account (venue, venue_account_id)
    WHERE active = 1 AND venue_account_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS credential (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER REFERENCES account(id),
    venue         TEXT,
    venue_id      INTEGER REFERENCES venue(id),
    field         TEXT    NOT NULL,
    value         TEXT    NOT NULL,
    name          TEXT    NOT NULL,
    secret        INTEGER NOT NULL DEFAULT 1,
    superseded_at TEXT,
    notes         TEXT,
    CHECK (account_id IS NULL OR venue IS NULL),
    CHECK (secret IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_value
    ON credential (account_id, field) WHERE superseded_at IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_name
    ON credential (name) WHERE superseded_at IS NULL;

CREATE TABLE IF NOT EXISTS venue_setting (
    id       INTEGER PRIMARY KEY,
    venue    TEXT NOT NULL,
    venue_id INTEGER REFERENCES venue(id),
    tier     TEXT,
    field    TEXT NOT NULL,
    value    TEXT NOT NULL,
    notes    TEXT,
    CHECK (tier IS NULL OR tier IN ('sim', 'demo', 'live'))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS venue_setting_one_per_tier
    ON venue_setting (venue, tier, field) WHERE tier IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS venue_setting_one_per_machine
    ON venue_setting (venue, field) WHERE tier IS NULL;

CREATE TABLE IF NOT EXISTS setting (
    id      INTEGER PRIMARY KEY,
    section TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    notes   TEXT,
    UNIQUE (section, key),
    CHECK (section IN ('policy', 'config', 'preferences', 'flags'))
) STRICT;

CREATE TABLE IF NOT EXISTS venue_arming (
    id       INTEGER PRIMARY KEY,
    venue    TEXT NOT NULL,
    venue_id INTEGER REFERENCES venue(id),
    label    TEXT,
    mode     TEXT NOT NULL,
    notes    TEXT,
    max_exposure REAL,
    CHECK (mode IN ('paper', 'demo', 'live')),
    CHECK (max_exposure IS NULL OR max_exposure > 0.0)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS venue_arming_one_per_venue
    ON venue_arming (venue) WHERE label IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS venue_arming_one_per_account
    ON venue_arming (venue, label) WHERE label IS NOT NULL;

CREATE TABLE IF NOT EXISTS settings_adoption (
    id              INTEGER PRIMARY KEY,
    adopted_at      TEXT    NOT NULL,
    tool_version    TEXT    NOT NULL,
    files_present   TEXT    NOT NULL,
    venues_declared INTEGER NOT NULL,
    setting_rows    INTEGER NOT NULL,
    arming_rows     INTEGER NOT NULL,
    notes           TEXT,
    CHECK (id = 1),
    CHECK (venues_declared IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS profile_risk (
    id      INTEGER PRIMARY KEY,
    profile TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    notes   TEXT,
    UNIQUE (profile, key)
) STRICT;
";

/// The schema-1 `credential` table, re-created by [`reshape_into`] under a scratch name so the
/// rebuild is a copy rather than an in-place `ALTER`. SQLite cannot add or drop a PRIMARY KEY with
/// `ALTER TABLE`, and schema 1's `name TEXT PRIMARY KEY` has to become schema 2's surrogate `id`.
const RESHAPE_SCRATCH: &str = "credential_schema1";

// ---------------------------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------------------------

/// One reason a schema-2 write refused. **Names a KEY, never a value.**
///
/// Same contract as [`crate::Ambiguity`], and for the same reason: an operator has to be able to
/// read a refusal out of a log without the log becoming a credential.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchemaRefusal {
    /// The classifier returned a `field` that is not a suffix of the NAME **for a row whose
    /// account can ONLY be found by its owner prefix** (`Classification::needs_owner_prefix` —
    /// unlabelled, and discriminated, i.e. dukascopy's shape), so §4.4's derivation (*the name with
    /// its owner prefix removed*) did not hold and there is no second way to reach the account.
    /// A classifier bug, refused rather than papered over with a guessed prefix.
    ///
    /// ⚠ **It is NOT raised for every non-suffix `field`, and this doc said it was.** A LABELLED
    /// key's `field` is legitimately not a suffix of its name (the `__LABEL` sits after it), so a
    /// blanket refusal would refuse the whole labelled grammar; every other row survives a missing
    /// prefix because `(venue, tier, label)` still answers for it. See
    /// [`Classification::owner_prefix`], whose own doc carried the same overstatement.
    FieldIsNotASuffix {
        /// The key name.
        key: String,
        /// What the classifier said `field` was.
        field: String,
    },
    /// The classifier named a tier outside [`ACCOUNT_TIERS`]. Refused HERE rather than left to the
    /// `CHECK`, so the message names the KEY rather than an opaque constraint failure.
    UnknownTier {
        /// The key name.
        key: String,
        /// The tier the classifier answered.
        tier: String,
    },
    /// More than one unlabelled `account` row already exists for a `(venue, tier)` the classifier
    /// offered no discriminator for, so *which account is this key's?* has more than one answer and
    /// nothing here may pick. See [`AccountKey::discriminator`].
    AmbiguousAccount {
        /// The key name.
        key: String,
        /// The venue.
        venue: String,
        /// The tier.
        tier: String,
    },
    /// A commented-out `#KEY=VALUE` line names a key with no LIVE row in this store, so there is
    /// nothing it can be the superseded value OF. The line is REPORTED and NOT written — see
    /// [`FileComments`].
    SupersededKeyIsNotInTheStore {
        /// The key name. Never the commented value.
        key: String,
    },
    /// **Two credential NAMES resolve to one `(account_id, field)` and their VALUES DISAGREE.**
    ///
    /// The live case is a tier ALIAS: `vike_model::account_keys` normalizes the legacy `MAINNET`
    /// tier onto `LIVE`, so `{VENUE}_LIVE_API_KEY` and `{VENUE}_MAINNET_API_KEY` classify to the
    /// same account and — the store's own tier token being what §4.4 removes — to the same `field`.
    /// One credential, two spellings. `credential_one_live_value` says a live `(account, field)`
    /// has ONE value, and when the two spellings carry DIFFERENT values there is no answer here
    /// that is not a guess: whichever row were made live would silently decide which key a venue
    /// signs orders with.
    ///
    /// So neither is written, BOTH names ride the refusal, and every other key in the run lands.
    /// The repair is an operator edit — delete one of the two lines — which is exactly the shape
    /// every other per-key refusal in this module takes. When the two values are IDENTICAL nothing
    /// is refused at all: see [`RowReport::aliases`].
    CollidingLiveValues {
        /// The key name this row was being written for. Never its value.
        key: String,
        /// The name already holding the live row for the same `(account_id, field)`. Never its
        /// value.
        other: String,
        /// The `field` both of them derive.
        field: String,
    },
}

impl SchemaRefusal {
    /// The key NAME this refusal is about. Every variant has one; none of them has a value.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            SchemaRefusal::FieldIsNotASuffix { key, .. }
            | SchemaRefusal::UnknownTier { key, .. }
            | SchemaRefusal::AmbiguousAccount { key, .. }
            | SchemaRefusal::SupersededKeyIsNotInTheStore { key }
            | SchemaRefusal::CollidingLiveValues { key, .. } => key,
        }
    }
}

impl std::fmt::Display for SchemaRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaRefusal::FieldIsNotASuffix { key, field } => write!(
                f,
                "{key}: the classifier answered field {field:?}, which is not the end of that \
                 name — the owner prefix a schema-2 row is keyed on cannot be taken from it, and \
                 nothing here will guess one"
            ),
            SchemaRefusal::UnknownTier { key, tier } => write!(
                f,
                "{key}: the classifier answered tier {tier:?}, which is not one of {:?} — an \
                 account row carrying it could not be joined to a venue arming ceiling",
                ACCOUNT_TIERS
            ),
            SchemaRefusal::AmbiguousAccount { key, venue, tier } => write!(
                f,
                "{key}: more than one unlabelled {venue} account already exists at tier {tier} and \
                 the classifier offered no discriminator, so which one this key belongs to has \
                 more than one answer — nothing here will pick"
            ),
            SchemaRefusal::SupersededKeyIsNotInTheStore { key } => write!(
                f,
                "{key} appears as a commented-out assignment in the credential file but has no \
                 live row in this store, so it is not the superseded value OF anything — the line \
                 is reported and nothing was written from it"
            ),
            SchemaRefusal::CollidingLiveValues { key, other, field } => write!(
                f,
                "{key} and {other} are two spellings of ONE credential — they resolve to the same \
                 account and the same field {field:?} — and they carry DIFFERENT values. A live \
                 (account, field) has one value, and nothing here will pick which of the two a \
                 venue signs orders with. NEITHER was written; delete one of the two lines and \
                 run again"
            ),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The comment scan — spec 4.2 and 4.3
// ---------------------------------------------------------------------------------------------

/// **What the credential FILE's comment lines carry** — the one thing `parse_dotenv` throws away.
///
/// ⚠ **This is a SECOND read of the credential file, and the property that makes it safe is that it
/// produces no LIVE value.** [`crate::db::migrate`]'s step 1 reads through `crate::store::resolve`
/// — *"the one parser, so the migration cannot disagree with the reader about what a line means"* —
/// and `crate::dotenv::parse_dotenv` skips every line beginning `#`. §4.2's two superseded
/// `ASTER_*` values and §4.3's provenance comments are therefore unreachable through it. This scan
/// looks ONLY at lines the one parser skipped, so the two can never disagree about anything that
/// reaches the credential map: this one contributes `superseded_at IS NOT NULL` rows and `notes`,
/// and no live row at all.
///
/// The file is opened READ-ONLY and is never written, moved or normalised — the rule that outranks
/// everything in `crate::db`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileComments {
    /// `#KEY=VALUE` lines — §4.2's rollback copies, as `(name, value)`. The value is a CREDENTIAL
    /// and is never printed by anything in this crate.
    pub superseded: Vec<(String, String)>,
    /// Prose comment blocks, keyed by the `KEY=` line they sit immediately above — §4.3's
    /// provenance. **Nothing ever reads these back**; they exist so that retiring the file does not
    /// destroy what the operator wrote in it.
    pub notes: BTreeMap<String, String>,
    /// Prose comment lines that sit above no key (a file header, a trailing note, anything
    /// separated from the next key by a blank line). COUNTED and never attached, because a note
    /// attached to the wrong row is worse than one nobody kept.
    pub unattached_prose_lines: usize,
}

/// Read [`FileComments`] out of a credential file's TEXT.
///
/// The rules, stated because §11 step 5 says a line is *classified, never guessed at*:
///
/// * a line matching `#[ ]*KEY=VALUE` (optional spaces after the `#`, a legal env-var name, an `=`)
///   is a SUPERSEDED VALUE;
/// * any other `#` line is PROSE;
/// * a run of prose lines **immediately** above a `KEY=` line — no blank line and no superseded
///   line between — is that key's note. Anything else is unattached and COUNTED.
///
/// ⚠ A `#` inside a VALUE is not a comment: this scan only considers lines whose first
/// non-whitespace byte is `#`, which is exactly `crate::dotenv::parse_dotenv`'s own skip condition,
/// so the two agree about which lines are comments by construction.
#[must_use]
pub fn scan_comments(text: &str) -> FileComments {
    let mut out = FileComments::default();
    let mut block: Vec<&str> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            out.unattached_prose_lines += block.len();
            block.clear();
            continue;
        }
        if let Some(body) = line.strip_prefix('#') {
            let body = body.trim_start();
            match split_assignment(body) {
                Some((name, value)) => {
                    // ⚠ **A `#KEY=VALUE` line ENDS the prose block**, and it did not until a
                    // reviewer measured §4.2's own file shape. The provenance line an operator
                    // writes above a rollback copy —
                    // `# superseded 2026-07-29 (kept for rollback)` — was left in the block, so it
                    // was carried PAST the rollback line and attached to the NEXT key in the file,
                    // whose note then claimed a rollback that was somebody else's. That is exactly
                    // what [`FileComments::unattached_prose_lines`] exists to prevent, and this
                    // function's own doc says a note is attached only to a key it sits
                    // IMMEDIATELY above.
                    //
                    // The block is COUNTED rather than attached because [`FileComments::superseded`]
                    // has no note column to put it in: a rollback copy carries the same `name` as
                    // the live row, so filing its provenance under that name would overwrite the
                    // live row's own note.
                    out.unattached_prose_lines += block.len();
                    block.clear();
                    out.superseded.push((name, value));
                }
                None => block.push(raw.trim_end()),
            }
            continue;
        }
        match split_assignment(line) {
            Some((name, _)) if !block.is_empty() => {
                out.notes.insert(name, block.join("\n"));
                block.clear();
            }
            _ => {
                out.unattached_prose_lines += block.len();
                block.clear();
            }
        }
    }
    out.unattached_prose_lines += block.len();
    out
}

/// `NAME=VALUE` with a legal credential-key name, or `None`.
///
/// The name grammar is deliberately the narrow one — an ASCII **UPPERCASE** letter or `_`, then
/// uppercase letters, digits and `_` — so a prose comment that merely contains an `=` is prose and
/// not a mangled superseded credential.
///
/// ⚠ **The uppercase requirement is a FENCE, and the example this doc used to give for it was
/// wrong.** It read: §4.3 quotes a real comment from the live store — *"polydata.live: key VALID
/// but FREE tier => data_access_days=0"* — and *"with a case-insensitive name grammar that tail
/// parses as an assignment and the line is read as a superseded value of a key called
/// `data_access_days`"*, described as MEASURED. It is not, and it cannot be: [`str::split_once`]
/// takes the FIRST `=`, which in that line is the one inside `=>`, so the candidate name is
/// `polydata.live: key VALID but FREE tier` — spaces, a dot and a colon — and no grammar that
/// admits an env-var name admits it, in any case. That line was never at risk and a
/// case-insensitive grammar would not have mis-read it.
///
/// What the rule actually fences is a comment whose WHOLE BODY is a lowercase assignment —
/// `# data_access_days=0`, the note left behind when somebody pastes the tail of that same sentence
/// onto its own line. THAT parses cleanly as `data_access_days = 0` under a case-insensitive
/// grammar and is read as a superseded credential. The `SupersededKeyIsNotInTheStore` safety net
/// catches it — there is no live row by that name, so nothing is written — but it catches it as a
/// REFUSAL an operator then has to read and dismiss on every run. Every credential key this store
/// holds is uppercase; a lowercase left-hand side is prose.
fn split_assignment(line: &str) -> Option<(String, String)> {
    let (name, value) = line.split_once('=')?;
    let name = name.trim();
    let mut bytes = name.bytes();
    let first = bytes.next()?;
    if !(first.is_ascii_uppercase() || first == b'_') {
        return None;
    }
    if !bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
        return None;
    }
    Some((name.to_string(), value.trim().to_string()))
}

// ---------------------------------------------------------------------------------------------
// The account resolver
// ---------------------------------------------------------------------------------------------

/// **Which `account` row a credential name belongs to, resolved once per run and never guessed.**
///
/// TWO lookups, in order, and the order is the whole design:
///
/// 1. **By OWNER PREFIX.** A name this store has never seen — `OKX_DEMO_API_PASSPHRASE` added to a
///    box that already holds `OKX_DEMO_API_KEY` — resolves through the prefix its siblings imply.
///    See [`Classification::owner_prefix`] for why this is a DERIVATION from stored data rather
///    than a heuristic, and why it is what lets dukascopy's two accounts survive a re-run with no
///    label and no discriminator column.
/// 2. **By `(venue, tier, label)`.** The last resort, and it is what unifies a LEGACY tier spelling
///    with its canonical one: `ASTER_MAINNET_API_KEY` and `ASTER_LIVE_API_KEY` have DIFFERENT owner
///    prefixes and are one account, because `AccountRef::tier` normalizes `MAINNET` onto `LIVE`.
///    ⚠ That unification is also what makes the two names collide at one `(account_id, field)` —
///    [`RowReport::aliases`] is the disposition, and it belongs to the WRITER rather than to this
///    resolver, which is doing exactly what it should here.
///    ⚠ It REFUSES ([`SchemaRefusal::AmbiguousAccount`]) rather than picking when the classifier
///    offered no [`AccountKey::discriminator`] and more than one unlabelled row matches — the
///    state dukascopy would put it in if its hand-map ever stopped discriminating.
///
/// ⚠ **This said "THREE lookups" and put a lookup BY NAME first, and there is no such lookup here.**
/// The by-name answer is real but it is [`write_rows`]' `existing` set, which SKIPS a name the
/// store already holds before a resolver is ever consulted — so the account is not re-resolved, it
/// is not touched at all. That is what makes a second run a no-op, and reading it as a first
/// lookup here sends anybody debugging idempotence into the wrong function.
pub struct AccountResolver {
    by_prefix: BTreeMap<String, i64>,
    by_key: BTreeMap<(String, String, Option<String>, Option<String>), i64>,
    /// `(venue, tier)` pairs that ALREADY hold more than one UNLABELLED account row.
    ///
    /// ⚠ **The one thing lookup 3 must never answer for.** `by_key` is a map, so two unlabelled
    /// rows of one `(venue, tier)` collapse into one entry and the second silently wins — which is
    /// precisely the dukascopy shape, and precisely the class of silent wrong answer §1 of the
    /// spec is about. This set is how lookup 3 knows to REFUSE instead of picking, and it is
    /// counted at LOAD rather than asked per row because the map has already lost the evidence by
    /// the time a lookup happens.
    ambiguous_unlabelled: std::collections::BTreeSet<(String, String)>,
    /// Every `(venue, tier)` that holds at least one UNLABELLED account row — the state
    /// [`AccountResolver::ambiguous_unlabelled`] is the SECOND sighting of. Kept alongside it so a
    /// run that CREATES the second one (dukascopy's pair, on migration day) reaches the same
    /// verdict as the run that merely finds them, rather than refusing only from the next run on.
    unlabelled_seen: std::collections::BTreeSet<(String, String)>,
    created: Vec<(i64, String, String)>,
}

impl AccountResolver {
    /// Build the resolver from what the database already holds.
    ///
    /// Reads every live credential row's `(name, field, account_id)` and reconstructs each
    /// account's owner prefix from it, which is the state lookup 2 rests on.
    fn load(tx: &Transaction<'_>) -> rusqlite::Result<AccountResolver> {
        let mut by_prefix: BTreeMap<String, i64> = BTreeMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT name, field, account_id FROM credential \
                 WHERE superseded_at IS NULL AND account_id IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })?;
            for row in rows {
                let (name, field, id) = row?;
                if let Some(prefix) = name.strip_suffix(field.as_str()) {
                    by_prefix.insert(prefix.to_string(), id);
                }
            }
        }
        let mut by_key = BTreeMap::new();
        let mut ambiguous_unlabelled = std::collections::BTreeSet::new();
        let mut unlabelled_seen = std::collections::BTreeSet::new();
        {
            let mut stmt = tx.prepare("SELECT id, venue, tier, label FROM account")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            for row in rows {
                let (id, venue, tier, label) = row?;
                let unlabelled = label.is_none();
                // The discriminator is derivation-time only and is stored nowhere, so an existing
                // row joins this map under `None` — lookup 3's shape, which is the only one that
                // consults it.
                by_key.insert((venue.clone(), tier.clone(), label, None), id);
                if unlabelled && !unlabelled_seen.insert((venue.clone(), tier.clone())) {
                    // ⚠ A SECOND unlabelled row of this `(venue, tier)`. The map just lost one of
                    // them; record the pair so lookup 3 refuses rather than answering with
                    // whichever row happened to be read last. This is dukascopy after a migration,
                    // and it is not an error — it is the schema working.
                    ambiguous_unlabelled.insert((venue, tier));
                }
            }
        }
        Ok(AccountResolver {
            by_prefix,
            by_key,
            ambiguous_unlabelled,
            unlabelled_seen,
            created: Vec::new(),
        })
    }

    /// The `account.id` for `key`, creating the row when nothing answers.
    fn resolve(
        &mut self,
        tx: &Transaction<'_>,
        name: &str,
        key: &AccountKey,
        owner_prefix: Option<&str>,
        note: Option<&str>,
    ) -> Result<i64, ResolveError> {
        if let Some(id) = owner_prefix.and_then(|p| self.by_prefix.get(p)) {
            return Ok(*id);
        }
        let map_key =
            (key.venue.clone(), key.tier.clone(), key.label.clone(), key.discriminator.clone());
        if let Some(id) = self.by_key.get(&map_key) {
            // ⚠ **THE REFUSAL, and it guards lookup 3 rather than sitting after it.** A hit here on
            // an UNLABELLED account whose `(venue, tier)` already holds more than one such row is
            // the one answer nothing may give: `by_key` is a map, so it is holding whichever of the
            // two was read last, and returning it would file this key against an account picked by
            // row order. Dukascopy's pair after a migration is exactly that state, and it is
            // reached by any key whose prefix is NEW while its `(venue, tier, label)` is not — a
            // canonical-tier `DUKASCOPY_DEMO_LOGIN` beside the two indexed sets, say.
            //
            // A DISCRIMINATED key never arrives here, because the hand-map's discriminator is part
            // of `map_key` and the table's rows joined `by_key` under `None`; its account is found
            // by the owner prefix above, which is what that prefix exists for.
            if key.label.is_none()
                && key.discriminator.is_none()
                && self.ambiguous_unlabelled.contains(&(key.venue.clone(), key.tier.clone()))
            {
                return Err(ResolveError::Refused(SchemaRefusal::AmbiguousAccount {
                    key: name.to_string(),
                    venue: key.venue.clone(),
                    tier: key.tier.clone(),
                }));
            }
            if let Some(p) = owner_prefix {
                self.by_prefix.insert(p.to_string(), *id);
            }
            return Ok(*id);
        }
        tx.execute(
            "INSERT INTO account (venue, venue_id, tier, label, notes) \
             VALUES (?1, (SELECT id FROM venue WHERE name = ?1), ?2, ?3, ?4)",
            (&key.venue, &key.tier, &key.label, note),
        )
        .map_err(ResolveError::Sql)?;
        let id = tx.last_insert_rowid();
        if let Some(p) = owner_prefix {
            self.by_prefix.insert(p.to_string(), id);
        }
        self.by_key.insert(map_key, id);
        if key.label.is_none() && key.discriminator.is_some() {
            // ⚠ **THE SAME-RUN / LATER-RUN PARITY, and it needs this line as well as the flag
            // below.** [`AccountResolver::load`] joins every row of the `account` table under a
            // `None` discriminator — the column does not exist, so it cannot do otherwise — while
            // a row CREATED in this run joins under the discriminator that created it. So an
            // UNDISCRIMINATED key arriving after a discriminated one (`DUKASCOPY_DEMO_LOGIN` after
            // `DUKASCOPY_DEMO1_LOGIN`) MISSED in the same run and HIT on the next, and the two
            // runs then did different things to a store neither of them had changed: the first
            // created a THIRD dukascopy account for it, the second either attached it to an
            // existing one or refused it. Registering the discriminator-less shape here is what
            // makes the two agree. `or_insert`, so the first account created keeps the entry —
            // which of the two it is decides nothing once the pair is flagged ambiguous below.
            self.by_key.entry((key.venue.clone(), key.tier.clone(), None, None)).or_insert(id);
        }
        if key.label.is_none()
            && !self.unlabelled_seen.insert((key.venue.clone(), key.tier.clone()))
        {
            // The SECOND unlabelled account of this `(venue, tier)` — dukascopy, on migration day.
            // Recorded now so a later key in the SAME run reaches the same refusal a later RUN
            // would, rather than the two disagreeing about a store neither of them changed.
            self.ambiguous_unlabelled.insert((key.venue.clone(), key.tier.clone()));
        }
        self.created.push((id, key.venue.clone(), key.tier.clone()));
        Ok(id)
    }
}

/// The two ways [`AccountResolver::resolve`] can fail: the engine, or a refusal that names a key.
enum ResolveError {
    Sql(rusqlite::Error),
    Refused(SchemaRefusal),
}

// ---------------------------------------------------------------------------------------------
// Writing classified rows
// ---------------------------------------------------------------------------------------------

/// **What a schema-2 fill DID** — counts and NAMES, never a value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowReport {
    /// `account` rows this run created, as `(id, venue, tier)`.
    ///
    /// ⚠ The `id` is carried because `(venue, tier)` is NOT a key: dukascopy has TWO accounts at
    /// `(dukascopy, demo)` and neither carries a label, so a report keyed on the pair alone would
    /// fold ruling 1's whole point into one row — which it did, until a test said so. The id is the
    /// identity the owner's signature names, and it is not a secret.
    pub accounts_created: Vec<(i64, String, String)>,
    /// `credential` rows INSERTED with `superseded_at IS NULL`.
    ///
    /// ⚠ It counts INSERTS, and an ALIAS row is not one of them — it is inserted
    /// `superseded_at IS NOT NULL` by construction and counts under [`RowReport::alias_rows`]. The
    /// two together are the rows a fill CARRIED, which is what [`reshape_into`]'s guard compares.
    /// A row this fill inserted live and then DEMOTED (the canonical spelling arriving after its
    /// alias) is counted here, once, because that is what the fill did to it; the demotion is an
    /// `UPDATE` and is counted by neither field.
    pub live_rows: usize,
    /// `credential` rows INSERTED as the rollback copy of a name already holding the live row —
    /// see [`RowReport::aliases`], which NAMES them. The other half of *rows carried*.
    pub alias_rows: usize,
    /// **Two NAMES of one credential, as `(the alias, the name holding the live row)`.**
    ///
    /// Sorted, and never a value. The live case is the legacy tier spelling: a store holding both
    /// `{VENUE}_LIVE_API_KEY` and `{VENUE}_MAINNET_API_KEY` holds ONE credential under two names,
    /// because `vike_model::account_keys` normalizes `MAINNET` onto `LIVE` and §4.4 removes the
    /// store's own tier token from `field`. `credential_one_live_value` then admits exactly one of
    /// the two as live, so the other is filed `superseded_at IS NOT NULL` — which is what that
    /// column is for (§4.2: a rollback copy and the value that replaced it coexist) and is the
    /// only disposition that keeps BOTH `name` rows.
    ///
    /// Keeping both is the compatibility contract rather than tidiness: `crate::db::read_table`
    /// answers for an aliased name out of its superseded row (no live row carries it), so
    /// `crate::resolve_project` returns the same map before and after the upgrade and
    /// `vike-cli secrets list` still prints the operator's own spelling.
    ///
    /// ⚠ This is the IDENTICAL-value case. Two spellings carrying DIFFERENT values are refused by
    /// name instead — [`SchemaRefusal::CollidingLiveValues`].
    pub aliases: Vec<(String, String)>,
    /// **The key NAMES this fill WROTE**, sorted — live rows and superseded ones alike.
    ///
    /// ⚠ It exists because a SCHEMA UPGRADE inserts every row in the store while INSERTING NO NEW
    /// KEY, so a caller that records what a migration did from the pending set alone would journal
    /// nothing at all for the one act that rewrites the whole table and cannot be undone.
    ///
    /// ⚠ **`crate::db::migrate` performs that fold, into [`crate::Migration::inserted_keys`]** —
    /// this doc named `crates/vike-cli/src/cmd/secrets.rs`'s `record_migration` as the folder,
    /// which it is not: that function READS `inserted_keys` and hands the names to the change
    /// journal, and by the time it sees a `Migration` the fold has already happened. The
    /// distinction matters because the fold is what makes the CLI's guard (`if
    /// done.inserted_keys.is_empty() { return }`) record an upgrade at all; a reader who believed
    /// the CLI owned it would look for the bug in the wrong crate.
    pub written_names: Vec<String>,
    /// Superseded `credential` rows written from §4.2's commented-out assignments, by NAME.
    pub superseded_rows: Vec<String>,
    /// `notes` attached from §4.3's prose comments.
    pub notes_attached: usize,
    /// Comment lines that sat above no key and were therefore attached to nothing.
    pub unattached_prose_lines: usize,
    /// **Names the classifier could not place** — written verbatim as infrastructure rows and
    /// reported here (§11.1). Not an error; the store legitimately holds names no venue grammar
    /// covers.
    pub unrecognised: Vec<String>,
    /// **Rows §7 and §6 will move and this change deliberately did not** — by name, with the
    /// section that will take them. This is the next change's work-list.
    pub pending_moves: Vec<(String, PendingMove)>,
    /// Per-KEY refusals. The run still succeeded and every other row landed — the same disposition
    /// `crate::Ambiguity::DisagreesWithDatabase` already takes.
    pub refused: Vec<SchemaRefusal>,
}

impl RowReport {
    fn sort(&mut self) {
        self.accounts_created.sort();
        self.aliases.sort();
        self.aliases.dedup();
        self.superseded_rows.sort();
        self.written_names.sort();
        self.written_names.dedup();
        self.unrecognised.sort();
        self.pending_moves.sort();
        self.refused.sort();
        self.refused.dedup();
    }

    /// Is there anything worth printing? A clean fill on a store with no oddities reports nothing
    /// but its counts.
    ///
    /// ⚠ An ALIAS counts as something worth printing: a name the operator wrote has stopped being
    /// the live row for its credential, and a store that does that in silence is the defect §1 is
    /// about wearing a smaller hat.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.unrecognised.is_empty()
            && self.refused.is_empty()
            && self.pending_moves.is_empty()
            && self.aliases.is_empty()
    }

    /// Fold a second fill's findings into this one — a run that RESHAPES and then carries new file
    /// keys in the same transaction does both, and the operator wants ONE report.
    pub fn absorb(&mut self, other: RowReport) {
        self.accounts_created.extend(other.accounts_created);
        self.live_rows += other.live_rows;
        self.alias_rows += other.alias_rows;
        self.aliases.extend(other.aliases);
        self.superseded_rows.extend(other.superseded_rows);
        self.written_names.extend(other.written_names);
        self.notes_attached += other.notes_attached;
        self.unrecognised.extend(other.unrecognised);
        self.pending_moves.extend(other.pending_moves);
        self.refused.extend(other.refused);
        // ⚠ **A `SupersededKeyIsNotInTheStore` the OTHER half then wrote is not a finding**, and
        // leaving it in printed one on a run that did everything right. BOTH halves are handed the
        // same [`FileComments`], and a commented-out rollback line whose live key is arriving from
        // the FILE in this very run is seen twice: the reshape half runs first, over the OLD
        // table, where that key has no live row — so it refuses — and the fill half then inserts
        // the live row and rescues the same comment successfully. The refusal's own claim ("has no
        // live row in this store") is false by the time the transaction commits, and the evidence
        // is in this very report: the name is in `superseded_rows`. Dropped here rather than
        // suppressed at the source, because each half is individually right about the store it saw.
        let rescued: std::collections::BTreeSet<&str> =
            self.superseded_rows.iter().map(String::as_str).collect();
        self.refused.retain(|r| {
            !matches!(r, SchemaRefusal::SupersededKeyIsNotInTheStore { key }
                if rescued.contains(key.as_str()))
        });
        // `unattached_prose_lines` is a property of the FILE, not of a fill, so both halves saw the
        // same number and adding them would double it.
        self.sort();
    }
}

impl std::fmt::Display for RowReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} account row(s), {} live credential row(s)",
            self.accounts_created.len(),
            self.live_rows
        )?;
        if !self.superseded_rows.is_empty() {
            write!(
                f,
                ", {} superseded value(s) rescued from commented-out lines: {}",
                self.superseded_rows.len(),
                self.superseded_rows.join(", ")
            )?;
        }
        if !self.aliases.is_empty() {
            write!(
                f,
                "\n  {} name(s) are a SECOND SPELLING of a credential this store already holds — \
                 one account, one field, one value, two names. The live row is the canonical \
                 spelling's and the other is filed as its rollback copy; BOTH names still answer:",
                self.aliases.len()
            )?;
            for (alias, live) in &self.aliases {
                write!(f, "\n    - {alias} -> the live row is {live}'s")?;
            }
        }
        if self.notes_attached > 0 || self.unattached_prose_lines > 0 {
            write!(
                f,
                "\n  {} comment block(s) kept as notes; {} comment line(s) sat above no key and \
                 were attached to nothing",
                self.notes_attached, self.unattached_prose_lines
            )?;
        }
        if !self.unrecognised.is_empty() {
            write!(
                f,
                "\n  {} name(s) the classifier could not place — written VERBATIM as \
                 deployment-level rows, nothing dropped and nothing guessed: {}",
                self.unrecognised.len(),
                self.unrecognised.join(", ")
            )?;
        }
        if !self.pending_moves.is_empty() {
            write!(
                f,
                "\n  ⚠ {} row(s) are classified but NOT MOVED by this change — they keep their \
                 legacy name in `credential`, so every reader still finds them. The move waits on \
                 the map renderer (spec 6.2/12):",
                self.pending_moves.len()
            )?;
            for (name, mv) in &self.pending_moves {
                write!(f, "\n    - {name}: {mv}")?;
            }
        }
        if !self.refused.is_empty() {
            write!(
                f,
                "\n  ⚠ {} key(s) were REFUSED and nothing was written for them:",
                self.refused.len()
            )?;
            for r in &self.refused {
                write!(f, "\n    - {r}")?;
            }
        }
        Ok(())
    }
}

/// The `superseded_at` marker an ALIAS row carries.
///
/// Same shape and same reasoning as §4.2's `'superseded-before-schema-2'`: the column records THAT
/// a row is not the live one, and the reason rather than a date — nothing in this store knows when
/// the operator added the second spelling, and §4.3's rule forbids parsing prose for a fact code
/// uses. It is a distinct string from the rollback marker so the two can be told apart by eye in a
/// `SELECT`, which is the only way anybody will ever look at them.
const ALIAS_MARK: &str = "alias-of-the-canonical-tier-spelling";

/// The LIVE row at `(account_id, field)`, as `(id, name, value)`, or `None`.
///
/// `credential_one_live_value` admits at most one, so this is a lookup and not a scan.
fn live_row_at(
    tx: &Transaction<'_>,
    account_id: i64,
    field: &str,
) -> rusqlite::Result<Option<(i64, String, String)>> {
    let mut stmt = tx.prepare(
        "SELECT id, name, value FROM credential \
         WHERE account_id = ?1 AND field = ?2 AND superseded_at IS NULL",
    )?;
    let mut rows = stmt.query((account_id, field))?;
    match rows.next()? {
        Some(r) => Ok(Some((r.get(0)?, r.get(1)?, r.get(2)?))),
        None => Ok(None),
    }
}

/// **Does this NAME spell the tier its classification resolved to?**
///
/// The tiebreak when two names collide at one `(account, field)` with the same value, and the one
/// signal available here that is not row order. The classifier NORMALIZES a tier
/// (`vike_model::account_keys::AccountRef::tier`: the legacy `MAINNET` resolves to `LIVE`), so the
/// name that still carries the canonical token in its owner prefix is the canonical spelling and
/// the one that does not is the alias. `ASTER_LIVE_API_KEY` answers `true` here and
/// `ASTER_MAINNET_API_KEY` answers `false`, which is the whole of the decision.
///
/// The match is on `_{TIER}_` rather than on the bare token, so `DUKASCOPY_DEMO1_` does not read as
/// spelling `DEMO`. When NEITHER name spells the tier — every hand-mapped family, whose store token
/// is by definition not the canonical one (`ALPACA_SANDBOX_`) — the answer is `false` for both and
/// the row already written keeps the live value, i.e. sorted order decides. That is arbitrary and
/// is stated as such: it is reached only by a collision this store has never produced, and the
/// report NAMES both spellings either way.
fn spells_its_tier(name: &str, field: &str, tier: &str) -> bool {
    // An EMPTY tier would make the needle `__`, which a labelled name contains — so it is refused
    // outright rather than left to produce an answer from no evidence. Unreachable today (the
    // caller has a `Placement::Account`, whose tier is a non-empty `ACCOUNT_TIERS` member by the
    // time this runs), and cheap enough to state.
    if tier.is_empty() {
        return false;
    }
    let Some(prefix) = name.strip_suffix(field) else { return false };
    format!("_{prefix}").contains(&format!("_{}_", tier.to_ascii_uppercase()))
}

/// **Fill a schema-2 store's `account` and `credential` tables from a name→value map.**
///
/// The one derivation, used by BOTH a fresh create and [`reshape_into`], so a store born at 2 and a
/// store carried to 2 cannot be classified differently. It writes no `venue_setting` row and no
/// `venue_account_id` — see this module's doc for the sequencing rule that forbids both.
///
/// Rows already present under the same live NAME are left alone, which is what makes a second run a
/// no-op.
///
/// # Errors
/// The engine. Per-KEY refusals ride [`RowReport::refused`] and leave the run successful.
pub fn write_rows(
    tx: &Transaction<'_>,
    rows: &BTreeMap<String, String>,
    comments: &FileComments,
    classify: &dyn Fn(&str) -> Classification,
) -> rusqlite::Result<RowReport> {
    let mut report =
        RowReport { unattached_prose_lines: comments.unattached_prose_lines, ..Default::default() };
    let mut resolver = AccountResolver::load(tx)?;

    let existing: std::collections::BTreeSet<String> = {
        let mut stmt = tx.prepare("SELECT name FROM credential WHERE superseded_at IS NULL")?;
        let names = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::BTreeSet::new();
        for n in names {
            out.insert(n?);
        }
        out
    };

    for (name, value) in rows {
        if existing.contains(name) {
            continue;
        }
        let class = classify(name);
        let note = comments.notes.get(name).map(String::as_str);
        let owner_prefix = class.owner_prefix(name);
        if owner_prefix.is_none() && class.needs_owner_prefix() {
            report.refused.push(SchemaRefusal::FieldIsNotASuffix {
                key: name.clone(),
                field: class.field.clone(),
            });
            continue;
        }
        let (account_id, venue) = match &class.placement {
            Placement::Account(key) => {
                if !ACCOUNT_TIERS.contains(&key.tier.as_str()) {
                    report.refused.push(SchemaRefusal::UnknownTier {
                        key: name.clone(),
                        tier: key.tier.clone(),
                    });
                    continue;
                }
                match resolver.resolve(tx, name, key, owner_prefix, note) {
                    Ok(id) => (Some(id), None),
                    Err(ResolveError::Sql(e)) => return Err(e),
                    Err(ResolveError::Refused(r)) => {
                        report.refused.push(r);
                        continue;
                    }
                }
            }
            Placement::Venue(v) => (None, Some(v.clone())),
            Placement::Infrastructure => (None, None),
        };
        // ⚠ **TWO NAMES, ONE `(account_id, field)`** — the one collision this store can actually
        // produce, and the reason this block exists rather than letting the engine answer. See
        // [`RowReport::aliases`] and [`SchemaRefusal::CollidingLiveValues`]; `spells_its_tier`
        // decides which of the two spellings keeps the live row.
        //
        // ⚠ Neither `INSERT` in this whole block names `venue_id`, and that is not an omission: both
        // fire only when `account_id` is `Some`, which happens only on the `Placement::Account` arm
        // above — and that arm always pairs `account_id = Some(id)` with `venue = None`. So `venue`
        // is provably `None` on every row either statement below can write; there is nothing for
        // `venue_id` to agree with. The two `credential` inserts further down, reached when
        // `account_id` is `None` (`Placement::Venue`/`Placement::Infrastructure`), are the ones that
        // can carry a non-NULL `venue` and are the ones that fill `venue_id`.
        if let Some(id) = account_id
            && let Some((other_id, other_name, other_value)) = live_row_at(tx, id, &class.field)?
        {
            if &other_value != value {
                report.refused.push(SchemaRefusal::CollidingLiveValues {
                    key: name.clone(),
                    other: other_name,
                    field: class.field.clone(),
                });
                continue;
            }
            let tier = match &class.placement {
                Placement::Account(key) => key.tier.as_str(),
                // Unreachable: `account_id` is `Some` only on the `Placement::Account` arm above.
                _ => "",
            };
            if spells_its_tier(name, &class.field, tier)
                && !spells_its_tier(&other_name, &class.field, tier)
            {
                // The NEWCOMER is the canonical spelling and the row already holding the live value
                // is the alias. Demote that one — an `UPDATE`, counted by neither counter — and let
                // this one land LIVE, which is an insert and counts as one.
                tx.execute(
                    "UPDATE credential SET superseded_at = ?2 WHERE id = ?1",
                    (other_id, ALIAS_MARK),
                )?;
                report.aliases.push((other_name, name.clone()));
                tx.execute(
                    "INSERT INTO credential \
                     (account_id, venue, field, value, name, secret, notes) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    (account_id, &venue, &class.field, value, name, i64::from(class.secret), note),
                )?;
                report.live_rows += 1;
            } else {
                // The ordinary direction: the row already written keeps the live value and THIS
                // name is filed as its rollback copy, so both names survive and `read_table`
                // answers for either.
                tx.execute(
                    "INSERT INTO credential \
                     (account_id, venue, field, value, name, secret, notes, superseded_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    (
                        account_id,
                        &venue,
                        &class.field,
                        value,
                        name,
                        i64::from(class.secret),
                        note,
                        ALIAS_MARK,
                    ),
                )?;
                report.aliases.push((name.clone(), other_name));
                report.alias_rows += 1;
            }
            report.written_names.push(name.clone());
            if note.is_some() {
                report.notes_attached += 1;
            }
            if let Some(mv) = class.pending_move {
                report.pending_moves.push((name.clone(), mv));
            }
            continue;
        }
        tx.execute(
            "INSERT INTO credential (account_id, venue, venue_id, field, value, name, secret, \
             notes) \
             VALUES (?1, ?2, (SELECT id FROM venue WHERE name = ?2), ?3, ?4, ?5, ?6, ?7)",
            (account_id, &venue, &class.field, value, name, i64::from(class.secret), note),
        )?;
        report.live_rows += 1;
        report.written_names.push(name.clone());
        if note.is_some() {
            report.notes_attached += 1;
        }
        if !class.recognised {
            report.unrecognised.push(name.clone());
        }
        if let Some(mv) = class.pending_move {
            report.pending_moves.push((name.clone(), mv));
        }
    }

    // §4.2 — the commented-out rollback copies, AFTER the live rows so the "is there a live row"
    // test below sees this run's own inserts.
    for (name, value) in &comments.superseded {
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential WHERE name = ?1 AND superseded_at IS NULL)",
            (name,),
            |r| r.get(0),
        )?;
        if !live {
            // ⚠ A commented assignment whose key has no live row is not a SUPERSEDED value — it is
            // a disabled key, and writing it would introduce a credential the store does not
            // otherwise hold, out of a line the one parser skips. Reported, not written.
            report.refused.push(SchemaRefusal::SupersededKeyIsNotInTheStore { key: name.clone() });
            continue;
        }
        let already: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential WHERE name = ?1 AND superseded_at IS NOT NULL)",
            (name,),
            |r| r.get(0),
        )?;
        if already {
            continue;
        }
        let class = classify(name);
        let owner_prefix = class.owner_prefix(name);
        let (account_id, venue) = match &class.placement {
            Placement::Account(key) => match resolver.resolve(tx, name, key, owner_prefix, None) {
                Ok(id) => (Some(id), None),
                Err(ResolveError::Sql(e)) => return Err(e),
                Err(ResolveError::Refused(r)) => {
                    report.refused.push(r);
                    continue;
                }
            },
            Placement::Venue(v) => (None, Some(v.clone())),
            Placement::Infrastructure => (None, None),
        };
        // `superseded_at` records that the value WAS replaced, not when — the file's comment is the
        // only evidence of a date and §4.3's rule forbids parsing prose for a fact code uses. The
        // marker is the migration that rescued it.
        tx.execute(
            "INSERT INTO credential \
             (account_id, venue, venue_id, field, value, name, secret, superseded_at) \
             VALUES (?1, ?2, (SELECT id FROM venue WHERE name = ?2), ?3, ?4, ?5, ?6, \
             'superseded-before-schema-2')",
            (account_id, &venue, &class.field, value, name, i64::from(class.secret)),
        )?;
        report.superseded_rows.push(name.clone());
        report.written_names.push(name.clone());
    }

    report.accounts_created = std::mem::take(&mut resolver.created);
    report.sort();
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// The reshape
// ---------------------------------------------------------------------------------------------

/// **Carry a schema-1 `credential` table into schema 2, IN PLACE, inside the caller's transaction.**
///
/// # ⚠ Why this is one transaction and not a sequence of steps
///
/// MEASURED in `db_tests::the_schema_stamp_and_the_ddl_are_both_transactional`: `PRAGMA
/// user_version` and DDL are BOTH rolled back with the transaction that set them. So the whole
/// reshape — the new table, the copy, the drop, the rename and the stamp — commits or does not, and
/// there is no state in between for a reader to find. That matters more here than it did for schema
/// 1's create, and in the opposite direction: schema 2's `credential` still has `name` and `value`
/// columns, so `SELECT name, value FROM credential` — the exact statement a schema-1 binary runs —
/// **is still valid SQL against the schema-2 shape**. A half-applied reshape stamped 1 over
/// schema-2 tables would therefore be ACCEPTED by an older binary and answered from silently.
/// Atomicity is what removes that state rather than documents it.
///
/// It follows that a reshape which fails leaves a working schema-1 store, re-runnable, with the
/// credential files beside it untouched — the same disposition `crate::db::migrate` takes about a
/// database it could not finish creating.
///
/// # The rebuild is a COPY, because `ALTER TABLE` cannot do it
///
/// Schema 1's `credential` is keyed `name TEXT PRIMARY KEY`; schema 2's is keyed on a surrogate
/// `id` with `name` merely unique among LIVE rows (§4.1 — a superseded row carries the same name as
/// the value that replaced it, which is what makes it a rollback copy). SQLite's `ALTER TABLE` can
/// add a column but cannot add or drop a PRIMARY KEY, so the old table is RENAMED aside, the new
/// one created from [`DDL`], the rows classified across, and the old one dropped.
///
/// ⚠ **No `VACUUM` afterwards.** It writes a full temp copy of the database — i.e. plaintext venue
/// credentials — into `SQLITE_TMPDIR` under the umask, which is the exact hazard `crate::db`'s
/// modes section exists to prevent and which its `preview` already refused once as an
/// implementation shortcut. The freed pages are reused by the next write; that is cheaper than a
/// second plaintext copy.
///
/// # Errors
/// The engine. Per-KEY refusals ride [`RowReport::refused`].
pub fn reshape_into(
    tx: &Transaction<'_>,
    comments: &FileComments,
    classify: &dyn Fn(&str) -> Classification,
) -> rusqlite::Result<RowReport> {
    let rows: BTreeMap<String, String> = {
        let mut stmt = tx.prepare("SELECT name, value FROM credential ORDER BY name")?;
        let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = BTreeMap::new();
        for row in it {
            let (n, v) = row?;
            out.insert(n, v);
        }
        out
    };

    tx.execute_batch(&format!("ALTER TABLE credential RENAME TO {RESHAPE_SCRATCH};"))?;
    tx.execute_batch(DDL)?;
    let report = write_rows(tx, &rows, comments, classify)?;

    // ⚠ **EVERY ROW MUST HAVE CARRIED, and a shortfall fails the RUN rather than one key.**
    //
    // This is the one place a per-key refusal is not survivable, and the difference from
    // `crate::db::migrate`'s is the SOURCE. There, a refused key is still in `secrets.env`, the run
    // carries its neighbours, and the operator re-runs after fixing one line. Here the source is
    // the table about to be DROPPED: a refusal that merely skipped a row would commit a schema-2
    // store missing a credential that exists nowhere else in the database, stamped at the current
    // version so every reader accepts it. A venue would silently drop to paper with nothing to say
    // why.
    //
    // The guard is a COUNT rather than an inspection of `refused`, deliberately: it catches any
    // future path that drops a row for a reason nobody has thought of yet, not just the three
    // refusals that exist today. The refused NAMES ride the message, because a count alone is not
    // something an operator can act on. Returning `Err` here rolls the whole transaction back — the
    // rename, the new tables and the stamp with it — so what is left on disk is the working
    // schema-1 store this call started from.
    //
    // ⚠ **ALIAS rows are added to the count**, and leaving them out turned the one collision this
    // store can produce into a whole-run failure. A `{VENUE}_MAINNET_*` key beside its
    // `{VENUE}_LIVE_*` twin is CARRIED — it is written, both names answer, and the report says so
    // — but it is written `superseded_at IS NOT NULL`, so it is not a LIVE row and `live_rows`
    // does not count it. The guard is about rows that VANISHED, and an alias did not.
    if report.live_rows + report.alias_rows != rows.len() {
        // ⚠ **The whole REFUSAL, not just its key.** This listed `SchemaRefusal::key` alone, which
        // is enough for the three refusals whose repair is obvious from the name and is NOT enough
        // for `CollidingLiveValues`, whose whole content is that this key collides with ANOTHER
        // one: an operator told only *the rows it could not carry: ASTER_MAINNET_API_KEY* has to
        // guess which other line in their file it disagrees with. Every variant's `Display` names a
        // key and never a value, so printing the refusal costs nothing the key did not.
        let missing: Vec<String> = report.refused.iter().map(ToString::to_string).collect();
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "the schema upgrade read {} credential row(s) and could classify only {} — \
                 NOTHING WAS WRITTEN and this store is still at its old schema, which still \
                 reads. The rows it could not carry: {}. Every one of them exists only in this \
                 database, so dropping the old table would have destroyed them.",
                rows.len(),
                report.live_rows + report.alias_rows,
                if missing.is_empty() {
                    "(none named)".to_string()
                } else {
                    format!("\n    - {}", missing.join("\n    - "))
                },
            )),
        ));
    }

    tx.execute_batch(&format!("DROP TABLE {RESHAPE_SCRATCH};"))?;
    // The FK columns schema 2 introduces are checked before the caller commits, so a reshape that
    // produced a dangling `account_id` is an error rather than a store nobody notices is broken.
    // `PRAGMA foreign_keys` only enforces NEW statements; this asks about the whole database.
    let violations: i64 =
        tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
    if violations > 0 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "the reshaped credential store has {violations} dangling account reference(s); \
                 nothing was committed"
            )),
        ));
    }
    Ok(report)
}
