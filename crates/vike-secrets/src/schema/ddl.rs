//! `DDL`: the one schema-2 DDL string, applied by every create path and by every writer's funnel.

#[cfg(doc)]
use super::{tiers::ANY_TIER, *};

/// **The schema-2 DDL — §4's two tables, §6's third, and `node_key` unchanged.**
///
/// Stated once, here, and applied both by a fresh create and by every writer's funnel
/// (`crate::db::ensure_venue_rows`), so a store BORN complete and a store a writer tops up cannot
/// end up different shapes.
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
/// `vike_model::accounts::account_keys::AccountLabel::parse` REFUSES the literal `"DEFAULT"` as reserved, so
/// a stored `'DEFAULT'` string would not parse back.
///
/// ⚠ **The cost is stated rather than hidden.** With every migrated label NULL,
/// `UNIQUE (venue, tier, label)` constrains NOTHING (NULLs are distinct in a SQLite index), so what
/// keeps a re-run from inserting a duplicate account is [`AccountResolver`]'s name- and
/// prefix-keyed lookup plus `credential_one_live_name`, not this index. The index starts biting the
/// moment an account is LABELLED, which is the state it was written for.
///
/// **2. `CHECK (tier IN …)`** — §4's comment names the vocabulary and `STRICT` cannot enforce it.
/// See [`ACCOUNT_TIERS`] for why the list IS the comment's three words since the 2026-09-23 rename
/// and is still not `CREDENTIAL_TIERS`' three. The two `IN (0, 1)` checks are the same reasoning
/// applied to the two boolean columns, which `STRICT` types as `INTEGER` and would otherwise let
/// hold `7`.
///
/// **3. `venue_setting` is created EMPTY** — see this module's doc. The table is the SHAPE the
/// signature accepted; the step that FILLS it is §11 step 4, which §12 forbids until the map
/// renderer exists.
///
/// **4. Every link to a venue is `venue_id INTEGER REFERENCES venue(id)`.** `account`,
/// `credential` and `venue_setting` carry it alone. It is `NOT NULL` wherever a row must name a
/// venue, and every venue uniqueness keys on it. **Every statement READS and FILTERS by
/// `venue_id`**, through [`VenueLink`] and [`venue_is`], the one spelling. `credential` is the one
/// table whose `venue_id` stays nullable, because an account-scoped or infrastructure row names no
/// venue, and its scope check is `CHECK (account_id IS NULL OR venue_id IS NULL)`. Every writer
/// names the number of the venue it was handed — [`AccountResolver::resolve`]'s and
/// [`crate::db::edit_account`]'s account INSERTs, [`write_rows`]'s `credential` INSERTs, and
/// `set_venue_setting_in`'s `venue_setting` INSERTs — and
/// `every_writer_names_its_venue_by_number_on_every_row`
/// (`crates/vike-secrets/tests/accounts/venue_table.rs`) pins it.
/// `crates/vike-secrets/tests/gates/store_link.rs` does not count these columns as untyped links
/// (its `observed()` skips any column carrying `REFERENCES`).
///
/// Everything else is §4 and §6 verbatim, including both partial indexes on `credential` (§4.1
/// argues why each must carry its `WHERE` — a plain `name … UNIQUE` would refuse the very
/// superseded rows §4.2 exists to keep) and the scope `CHECK` that refuses the fourth of §5.1's
/// four combinations.
///
/// ⚠ **`venue_setting` is NOT §6 verbatim any more, and this paragraph used to say it was.** It
/// listed *"`venue_setting`'s two (NULLs are distinct, so ONE index over `(venue, tier, field)`
/// would say nothing at all about the machine-scoped rows)"* — true of the shape §6 designed, and
/// the reason that shape is ruling 2's defect: the NULL decided which of two uniqueness rules
/// applied. §5.2 step 7 of `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`
/// replaces it: `tier` is `NOT NULL`, the machine scope is the stored word `'any'`
/// ([`ANY_TIER`]), and ONE total `UNIQUE (venue_id, tier, field)` constrains every row.
///
/// `IF NOT EXISTS` throughout: a database created by a concurrent first run is JOINED rather than
/// fought, and a writer re-running the batch over a finished store changes nothing.
///
/// # ⚠ The last three tables are NOT the credential schema, and the boundary is [`crate::settings`]'s
///
/// `setting` arrives with
/// `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 1 and belongs to
/// a different owner from everything above it. It is stated HERE for the reason this const's
/// opening line gives — one statement of shape, applied by every create path, so a store BORN
/// complete and a store a writer tops up cannot end up different — and the sentence that says which
/// table a new venue-scoped knob belongs in lives in [`crate::settings`]'s module doc, which is the
/// authority. ⚠ It is not added to [`SCHEMA_VERSION`](crate::SCHEMA_VERSION): a bump would make
/// every existing credential store unreadable to the new binary, which is the LIVE GATE. An
/// already-migrated store simply has no such table until a writer runs this batch again, and
/// [`crate::settings::read_settings`] answers [`crate::settings::SettingsSource::TablesAbsent`] for
/// it rather than pretending the rows are empty.
///
/// **`setting`'s `CHECK` is a closed VOCABULARY, never a bound.** `section IN (…)` is the same
/// reasoning as `tier IN (…)` above — four words a `STRICT` `TEXT` column cannot otherwise refuse.
/// A `CHECK` restating a NUMERIC bound (`max_leverage <= …`) is forbidden and deliberately absent:
/// `vike_config`'s typed model imports every bound from `vike_model`, and a second copy in SQL is
/// the split-brain that model exists to refuse.
///
/// **`setting` carries no precedence column, no `overridable` flag and no per-row layer field**,
/// and that is a decision rather than an omission — 0057's `policy.toml` verdict in as many words.
/// A precedence column is how a ceiling acquires an override, and the seal that makes a ceiling a
/// ceiling is a property of the TYPE (`vike_config::layers`'s sealed `EnvOverride` /
/// `CliOverride`, which `Policy` does not implement), not of the carrier underneath the loader.
///
/// **`profile_risk` is 0057's Phase 2** — a RUN PROFILE's `[risk]` table, one row per key, and it
/// is a THIRD owner rather than a variant of either table above it. The boundary is spelled in
/// [`crate::settings`]'s module doc beside the other three; the two properties worth carrying at
/// the DDL itself:
///
/// * **`profile` is part of the key and is not optional**, because *which* profile is live is
///   0057's open Question 3 and this table must not answer it by accident. A profile FILE (the
///   retired `VIKE_RUN_PROFILE` or `--profile` named one) is a PATH, a box may hold several, and a
///   table keyed on `key` alone would be
///   "the risk budget" — i.e. an active-profile row written sideways. `UNIQUE (profile, key)` says
///   instead: these are the ceilings of the profile that goes by this name, and nothing here
///   selects one.
/// * **Nothing on the mount path reads it**, so no `CHECK` restates a numeric bound here either
///   (the rule the `setting` paragraph above states). The rows are a DISCLOSURE copy: the file the
///   boot resolves is still the only thing that builds a `vike_model::ProfileRisk`, and
///   `crates/vike-ops/tests/settings_secrets/profile_risk_readers_gate.rs` is what holds that true as the tree
///   changes. The key vocabulary is `vike_model::ProfileRisk::keys()` (serde's own field
///   list, which `vike_config::check_risk_key` judges each row by), not a `CHECK` list, for the
///   reason `setting.key` has none: a SQL copy would be a second authority that drifts.
///
/// **`settings_adoption` is the integrity SEAL of the `setting` rows.**
///
/// One row, `CHECK (id = 1)`, created by the first [`crate::settings::write_setting_row_in`] and
/// moved by every sanctioned write after it. `setting_rows` is the ERASE detector: every sanctioned
/// write updates it in its own transaction, so a mismatch in either direction means rows moved by
/// some other route. `files_present` is disclosure from the retired file crossing.
///
/// ⚠ **`venues_declared` and `arming_rows` are dead since decision 0119**, which made an account's
/// own `tier` + `active` its mode and deleted the venue arming ceiling they counted. They stay
/// DECLARED because an existing store's columns are `NOT NULL` without a default, so the row writer
/// must still fill them on its INSERT: it inserts `0`, never updates them, and nothing reads them.
/// Dropping them from a fresh store's DDL would split that INSERT into two shapes.
///
/// Like `setting`, NOT added to [`SCHEMA_VERSION`](crate::SCHEMA_VERSION) — same live gate. Nor is
/// `profile_risk`: a bump makes every existing CREDENTIAL store unreadable to the new binary.
/// Presence is asked of `sqlite_master`.
///
/// **`account.max_exposure` is the account's own exposure ceiling** — `NULL` is UNBOUNDED, and the
/// column's `CHECK` refuses a figure that is not `> 0.0`. It is the LAST column, so a fresh store and
/// an `ALTER`ed one agree on its position relative to every column this batch names. Its writer is
/// [`crate::db::edit_account`]'s `AccountEdit::SetMaxExposure`; its type is
/// [`crate::db::MaxExposure`].
///
/// ⚠ **Like every column added after the schema-2 freeze, it reaches an EXISTING store through an
/// `ALTER TABLE`, not through this batch** — `CREATE TABLE IF NOT EXISTS` changes nothing about a
/// table that is already there. `crate::db`'s `ensure_venue_rows` runs that `ALTER`, with the same
/// column `CHECK` character for character, and a READ-ONLY reader that meets a store no writer has
/// carried selects `NULL` in its place (`crate::db`'s `account_select`). The probe reads the store's
/// SHAPE, never old data: it converts nothing (`docs/decisions/0117-there-are-no-migrations.md`).
///
/// ⚠ **An older store still carries two things this batch no longer declares**: the `account.armed`
/// column and the whole `venue_arming` table, with their rows. Nothing reads, writes or drops either
/// — a `DROP` would be a migration — and every reader selects `account` by column NAME, so the dead
/// column in the middle of an old row changes nothing.
///
/// **`venue.title` is the venue's own spelling, and it follows the same rule.** The owner ruled on
/// 2026-09-30 that venue names live in this database, and §3's `venue` block carries the column. It
/// is nullable with no `CHECK` (a spelling is free text, so there is nothing to constrain), reaches
/// an EXISTING store through the `ALTER TABLE venue ADD COLUMN title TEXT` that `crate::db`'s
/// `ensure_venue_rows` runs rather than through this batch, and is written ONCE per row from that
/// function's `VENUE_TITLES` seed — never rewritten. A NULL stays NULL: a store no writer has
/// carried since the column was added, and a title somebody cleared, both read as `None`, and a
/// reader shows the venue's key (`crate::db::read_venues`).
///
/// # ⚠ The dead columns (§9 stage 4c) — ONE was dropped, and the others are NOT dead
///
/// §9's stage-4 row says *"the dead columns"* in the plural and §2.7 names exactly one:
/// `venue_arming.notes`, dropped before decision 0119 removed that whole table from this batch.
/// That was also the only one the sweep could prove — **zero readers AND zero writers across the
/// whole tree**, not merely across this crate — so it was the only DEAD one dropped.
///
/// Every other candidate stays, and **the note is the deliverable for a column that was not
/// dropped** — a reader who finds one of these unnamed in any `SELECT` must not conclude it is
/// spare. These are in the doc rather than in the SQL because the batch is a string literal that
/// `crates/vike-secrets/tests/settings_store_ddl_gate.rs` reads as TEXT, and an SQL comment inside
/// it would become part of every created table's stored statement.
///
/// * **`account.notes` and `credential.notes` hold prose a store may already carry, and that is
///   the opposite of dead.** §4.3's harvest of the operator's own comment blocks landed there when
///   their credentials were carried in; [`write_rows`] binds the column NULL on every row it writes
///   today. Nothing SELECTs either one, and that is a RULE rather than an omission:
///   [`crate::db::Account`]'s own doc quotes it (*notes are for humans, and code NEVER reads them —
///   the moment code parses a note it is not a note*). Dropping them destroys the only copy of that
///   prose.
/// * **`account.last_verified_at` has a writer AND a reader**, and the sentence that says
///   otherwise is stale. `crates/vike-model/src/accounts/account_confirmation.rs`'s module doc still reads
///   *"`account.last_verified_at` has no writer anywhere in the tree"*; it was true when written
///   and stopped being true on 2026-09-15. The writer is [`crate::db::set_venue_account_id`] under
///   [`crate::db::BookSource::Handshake`] (an `UPDATE account SET last_verified_at`, and a second
///   one folded into its `COALESCE` form); the readers are [`crate::db::Account::last_verified_at`]
///   — selected by this module's sibling `account_select` on every account read — and
///   `vike-cli secrets accounts`, which renders it as the *(never)* column an operator looks at.
/// * **`setting.notes`, `venue_setting.notes`, `profile_risk.notes` and `settings_adoption.notes`
///   have neither a reader nor a writer today**, and they still stay: §3 DECLARES every one of
///   them, so they are UNWRITTEN rather than dead, and dropping a column the signed design carries
///   would be a divergence to argue at the spec rather than a debt to pay here
///   (`crates/vike-secrets/tests/settings_store_ddl_gate/pinned_debt.rs`'s `GROWTH_GUIDANCE` states the order).
/// * Two more came out of the same sweep and are named so the next reader does not re-measure
///   them: **`account.parent_id` is READ-ONLY** (the projection above selects it into
///   [`crate::db::Account::parent_id`]; nothing writes it yet, and `crate::db::edit_account`'s own
///   comment says so), and **`credential.secret` is WRITE-ONLY** ([`write_rows`] again). Neither
///   is dead and §3 declares both.
pub const DDL: &str = "\
CREATE TABLE IF NOT EXISTS node_key (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    name  TEXT NOT NULL UNIQUE,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS venue (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    name  TEXT NOT NULL UNIQUE,
    title TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS account (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    venue_id         INTEGER NOT NULL REFERENCES venue(id),
    tier             TEXT    NOT NULL,
    label            TEXT,
    venue_account_id TEXT,
    parent_id        INTEGER REFERENCES account(id),
    active           INTEGER NOT NULL DEFAULT 1,
    last_verified_at TEXT,
    notes            TEXT,
    max_exposure     REAL CHECK (max_exposure IS NULL OR max_exposure > 0.0),
    UNIQUE (venue_id, tier, label),
    CHECK (tier IN ('paper', 'demo', 'live')),
    CHECK (active IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS account_one_account_per_book
    ON account (venue_id, venue_account_id)
    WHERE active = 1 AND venue_account_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS credential (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id    INTEGER REFERENCES account(id),
    venue_id      INTEGER REFERENCES venue(id),
    field         TEXT    NOT NULL,
    value         TEXT    NOT NULL,
    name          TEXT    NOT NULL,
    secret        INTEGER NOT NULL DEFAULT 1,
    superseded_at TEXT,
    notes         TEXT,
    CHECK (account_id IS NULL OR venue_id IS NULL),
    CHECK (secret IN (0, 1))
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_value
    ON credential (account_id, field) WHERE superseded_at IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_name
    ON credential (name) WHERE superseded_at IS NULL;

CREATE TABLE IF NOT EXISTS venue_setting (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    venue_id INTEGER NOT NULL REFERENCES venue(id),
    tier     TEXT NOT NULL,
    field    TEXT NOT NULL,
    value    TEXT NOT NULL,
    notes    TEXT,
    UNIQUE (venue_id, tier, field),
    CHECK (tier IN ('any', 'paper', 'demo', 'live'))
) STRICT;

CREATE TABLE IF NOT EXISTS setting (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    section TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    notes   TEXT,
    UNIQUE (section, key),
    CHECK (section IN ('policy', 'config', 'preferences', 'flags'))
) STRICT;

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
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    profile TEXT NOT NULL,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL,
    notes   TEXT,
    UNIQUE (profile, key)
) STRICT;
";
