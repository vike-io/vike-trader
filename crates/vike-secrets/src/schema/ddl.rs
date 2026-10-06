//! `DDL`: the one schema-2 DDL string, applied by every create path and by every rebuild.

#[cfg(doc)]
use super::{
    rebuild::{create_statement_under, ddl_column_decls},
    steps::dropped_columns::DROPPED_COLUMNS,
    tiers::ANY_TIER,
    *,
};

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
/// **4. `account`, `credential`, `venue_arming` and `venue_setting` each carry a
/// `venue_id INTEGER REFERENCES venue(id)` beside their text `venue` column.** (Since the
/// venue-links plan's second release, only `venue_arming` keeps the text column beside it: see the
/// ⚠ paragraph after the next.)
///
/// ⚠ **Since the venue-links plan's first release, `venue_id` IS the link.** (The plan is
/// `2026-09-30-settings-store-links-by-number`, and it builds ruling 3 of
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`.) It is `NOT NULL`
/// wherever a row must name a venue, and every venue uniqueness keys on it. The text `venue`
/// column is WRITTEN for one more release, so the release before this one can read a store this
/// one migrated; the second release drops it. **Every statement READS and FILTERS by `venue_id`**,
/// through [`VenueLink`] and [`venue_is`], the one spelling. [`VenueLink`] falls back to the text
/// column for a row whose number names no `venue` row — a NULL, which a store no writer has carried
/// yet can still hold, or a number a hand edit left dangling (the join is a LEFT one) — and three
/// kinds of statement can meet such a row: a read-only reader, decision 0095's boot-path
/// migration, and `crate::db::set_venue_account_id`, which runs no funnel ([`VenueLink`]'s doc).
/// ⚠ **Outside that fallback the text is still READ, on purpose, and this said it was not**: by the
/// write funnel's own repair (the backfill that derives a row's number from it, trap 7's listing of
/// rows whose text names no roster venue, and trap 5's check under the text-keyed `UNIQUE`, which
/// the shipped `venue_setting` still carries — all in `rebuild_table_from_ddl` and the funnel
/// before it), and by decision 0095's migration, which rewrites a `live` row when either its number
/// or its text names a switched venue, because a rollback reads the text.
/// (⚠ This said the read half was still to land, with the readers, the guards and 0095's `UPDATE`
/// on the TEXT column, until the venue-links plan's reader task, which landed in the same
/// release.) `credential` is the one table whose `venue_id` stays nullable, because an
/// account-scoped or infrastructure row names no venue. An EXISTING store is carried onto this
/// shape by [`migrate_venue_links_onto_venue_id`].
///
/// ⚠ **The plan's second release DROPPED the text column from `account`, `credential` and
/// `venue_setting`** ([`DROPPED_COLUMNS`], taken to an existing store at its first write by
/// [`migrate_dropped_columns`], after the venue-links pass). Those three carry `venue_id` alone,
/// the text-keyed `UNIQUE`s went with the column, and `credential`'s scope check is
/// `CHECK (account_id IS NULL OR venue_id IS NULL)`. [`VenueLink`] answers each table by the shape
/// the store holds, so a writer names only the number there. `venue_arming` keeps its text column,
/// and everything the paragraph above says about the text, until Plan B deletes that table; the
/// fallback and the funnel's reads of the text answer for a store no writer of this release has
/// carried yet.
///
/// Until then, all four carried it NULLABLE — a DUAL-WRITE half with no reader yet. ⚠ **Nothing
/// read it then, and the text column was authoritative** (this read in the present tense,
/// *"Nothing reads it; the text column stays authoritative"*, for a release after the venue-links
/// plan made it false). It existed so a later change could reshape onto the typed link without a
/// flag day, which that plan's first release did: EVERY writer of a row carrying a
/// text `venue` fills it beside its text sibling — [`AccountResolver::resolve`]'s and
/// [`crate::db::edit_account`]'s account INSERTs, [`write_rows`]'s two `credential` INSERTs that can
/// ever carry a non-NULL `venue` (its other two sit inside the `Placement::Account` branch, where
/// `venue` is provably `None` — see the comment at that branch's own `if let`), and
/// `write_settings`'s/`set_venue_setting_in`'s/[`crate::db::move_pending_rows`]'s `venue_arming`/
/// `venue_setting` INSERTs — [`crate::db`]'s `ensure_venue_id_columns` backfills a store that
/// predates it (and is ALSO called directly by the four writers that never pass through
/// `crate::db::fill_into`, so none of them can hit a missing column on an existing store), and
/// `every_writer_names_its_venue_by_number_on_every_row` (`crates/vike-secrets/tests/accounts/venue_table.rs`,
/// `venue_id_agrees_with_the_text_column_on_every_row` until the second release) pins that every
/// writer files the number of the venue it was handed, and that the two spellings never disagree
/// where a table still has both. `crates/vike-secrets/tests/gates/store_link.rs` does not count
/// these four as untyped links (its `observed()` skips any column carrying `REFERENCES`); its pin
/// held the four TEXT `venue` columns until the second release dropped three of them.
///
/// Everything else is §4 and §6 verbatim, including both partial indexes on `credential` (§4.1
/// argues why each must carry its `WHERE` — a plain `name … UNIQUE` would refuse the very
/// superseded rows §4.2 exists to keep) and the scope `CHECK` that refuses the fourth of §5.1's
/// four combinations — `CHECK (account_id IS NULL OR venue_id IS NULL)` since the venue-links
/// plan's second release, over the text `venue` until then.
///
/// ⚠ **`venue_setting` is NOT §6 verbatim any more, and this paragraph used to say it was.** It
/// listed *"`venue_setting`'s two (NULLs are distinct, so ONE index over `(venue, tier, field)`
/// would say nothing at all about the machine-scoped rows)"* — true of the shape §6 designed, and
/// the reason that shape is ruling 2's defect: the NULL decided which of two uniqueness rules
/// applied. §5.2 step 7 of `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`
/// replaces it: `tier` is `NOT NULL`, the machine scope is the stored word `'any'`
/// ([`ANY_TIER`]), and ONE total `UNIQUE (venue, tier, field)` constrains every row. It keys on the
/// TEXT `venue` rather than §3's `venue_id` for the reason `account`'s address constraint does:
/// `venue_id` is still nullable (a dual-write half), and a `UNIQUE` over a nullable column lets
/// every NULL through. An EXISTING store reaches the new shape through
/// [`migrate_venue_setting_tier_to_any`] — `CREATE TABLE IF NOT EXISTS` changes nothing about a
/// table already there, which is the whole of stage 4's lesson. ⚠ The two sentences before that
/// one describe the store BEFORE the venue-links flip (deviation 4 above): `venue_id` is
/// `NOT NULL` now, so `UNIQUE (venue_id, tier, field)` stood beside the text one, and `account`
/// gained `UNIQUE (venue_id, tier, label)` the same way. The plan's second release then dropped the
/// text column from both tables and the text-keyed `UNIQUE` with it, so the number's is the only
/// key either carries.
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
/// **`label` is NULLABLE and the two partial indexes are why**: NULLs are distinct in a SQLite
/// index, so one `UNIQUE (venue, label)` would constrain nothing about the venue-level rows. A NULL
/// label is the `[venues]` row for that venue; a non-NULL one is an `[accounts]` row for that
/// venue's labelled account. ⚠ This said *"exactly as `venue_setting`'s pair above"* until §5.2
/// step 7 retired that pair; this one is now the LAST NULL discriminator in the batch, and it goes
/// with the table (§3 spells `venue_arming` DELETED) rather than by the same cure, because a venue
/// line naming no account has no row of `account` to become.
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
///   `crates/vike-ops/tests/settings/profile_risk_readers_gate.rs` is what holds that true as the tree
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
/// is the UNMOUNT detector — a box with no `policy.venues.<venue>` rows holds ZERO arming rows
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
///
/// **`account.armed` is a DERIVED column — the operator's PERMISSION, materialized.**
///
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §9 stage 3. `tier` is
/// what an account CAN reach (its credential set); `armed` is whether the operator allows it, and
/// §3.2 composes the two as `armed ? tier : paper`. The column is filled by
/// [`crate::settings::fold_arming_into_accounts`] and by NOTHING else — it is a pure function of
/// the `venue_arming` rows and the `account` rows, recomputed wholesale whenever either changes,
/// so there is no state here that can drift from the ceiling the mount actually reads.
///
/// ⚠ **`venue_arming` is NOT dropped by that fold, and §3's *DELETED* has not happened yet.** The
/// stage-2 shape is repeated deliberately: this is the dual-written half of a column whose READER
/// has not moved — the shape `venue_id` had until the venue-links plan's first release gave it its
/// readers (deviation 4 above: *a dual-write half with no reader yet*, which `venue_id` no longer
/// is; ⚠ this said that this file still named `venue_id` that way until the plan's final fix
/// wave). What the reader still needs and `account` cannot yet
/// answer is a `[venues]` line for a venue that has NO account row — a venue with no credentials
/// has no row to hang a ceiling or a `max_exposure` figure on, and losing a figure means
/// UNBOUNDED. `crates/vike-secrets/src/settings/arming.rs`'s `fold_arming_into_accounts` carries the
/// whole ruling.
///
/// ⚠ **Like every column added after the schema-2 freeze, this one reaches an EXISTING store
/// through an `ALTER TABLE`, not through this batch** — `CREATE TABLE IF NOT EXISTS` changes
/// nothing about a table that is already there. The `ALTER` cannot carry the table-level
/// `CHECK (armed IN (0, 1))` above, so a store born before this column has the column without the
/// constraint until stage 4's rebuild; the fold is the only writer and it writes `0`/`1`, which is
/// the same asymmetry `venue_id`'s nullable-only `REFERENCES` already carries.
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
/// # ⚠ `tier IN ('paper', …)` is the ONE post-freeze change an `ALTER TABLE` cannot carry at all
///
/// The paragraph above is about a column an existing store LACKS. The `sim` → `paper` rename
/// (§4.4, ruling 7) is the opposite shape and is strictly worse: the column is already there,
/// `CREATE TABLE IF NOT EXISTS` skips the table, and the CHECK an already-migrated store holds
/// still reads `('sim', 'demo', 'live')`. So on BOTH live boxes this batch would leave a table
/// that REFUSES the very word the classifier now produces — a `{VENUE}_SIM_*` credential write
/// failing with a bare `CHECK constraint failed` on a store that worked yesterday. SQLite has no
/// `ALTER TABLE … DROP CONSTRAINT`, so the repair is a table REBUILD, and
/// [`migrate_sim_tier_to_paper`] is it: one step that rewrites the ROWS and the CONSTRAINT
/// together, because they are the same migration.
///
/// ⚠ **MEASURED 2026-09-23: neither live box can reach that failure today** — the CI box and the dev
/// box each hold `0` `_SIM_` credential keys and `0` `tier = 'sim'` rows, so no `paper` row is
/// minted there and the old CHECK never fires. That is why the rename is safe to make now; it is
/// NOT why the migration can be skipped, because a store elsewhere may hold one and the failure it
/// would take is a write refusal, not a wrong answer.
///
/// # ⚠ The dead columns (§9 stage 4c) — ONE was dropped, and the others are NOT dead
///
/// §9's stage-4 row says *"the dead columns"* in the plural and §2.7 names exactly one:
/// `venue_arming.notes`. That is also the only one the sweep could prove — **zero readers AND zero
/// writers across the whole tree**, not merely across this crate — so it is the only DEAD one
/// dropped. [`DROPPED_COLUMNS`] carries it with its measurement, and [`migrate_dropped_columns`] is
/// what takes the drop to a store that already exists; this batch alone would reach a NEWBORN store
/// only. (⚠ The venue-links plan's second release dropped three more through the same pass — the
/// text `venue` of `account`, `credential` and `venue_setting` — which were REDUNDANT rather than
/// dead; [`DROPPED_COLUMNS`]' doc states that bar separately. `venue_arming.venue` stays until Plan
/// B deletes the table.)
///
/// Every other candidate stays, and **the note is the deliverable for a column that was not
/// dropped** — a reader who finds one of these unnamed in any `SELECT` must not conclude it is
/// spare. These are in the doc rather than in the SQL because the batch is a string literal whose
/// BODY is taken verbatim by [`create_statement_under`] and split on commas by
/// [`ddl_column_decls`]: an SQL comment inside it becomes part of every rebuilt table's stored
/// text and a bogus entry in that split.
///
/// * **`account.notes` and `credential.notes` are WRITE-ONLY, and write-only is the opposite of
///   dead.** [`write_rows`] fills both — its `account` INSERT and three of its four `credential`
///   INSERTs name the column, the fourth being the pre-schema-2 rollback copy, which has no
///   comment to attach — from [`FileComments::notes`], which is §4.3's harvest of the operator's own
///   comment blocks out of their `secrets.env`. Nothing SELECTs either one, and that is a RULE
///   rather than an omission: [`crate::db::Account`]'s own doc quotes it (*notes are for humans,
///   and code NEVER reads them — the moment code parses a note it is not a note*). Dropping them
///   destroys the only copy of that prose.
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
///   (`crates/vike-ops/tests/settings/settings_store_ddl_gate/pinned_debt.rs`'s `GROWTH_GUIDANCE` states the order).
///   `venue_arming.notes` is not in that company for the one reason that makes it droppable: §3
///   spells that whole TABLE deleted, so there is no design-side column to contradict.
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
    armed            INTEGER NOT NULL DEFAULT 0,
    label            TEXT,
    venue_account_id TEXT,
    parent_id        INTEGER REFERENCES account(id),
    active           INTEGER NOT NULL DEFAULT 1,
    last_verified_at TEXT,
    notes            TEXT,
    UNIQUE (venue_id, tier, label),
    CHECK (tier IN ('paper', 'demo', 'live')),
    CHECK (armed IN (0, 1)),
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

CREATE TABLE IF NOT EXISTS venue_arming (
    id       INTEGER PRIMARY KEY,
    venue    TEXT NOT NULL,
    venue_id INTEGER NOT NULL REFERENCES venue(id),
    label    TEXT,
    mode     TEXT NOT NULL,
    max_exposure REAL,
    CHECK (mode IN ('paper', 'demo', 'live')),
    CHECK (max_exposure IS NULL OR max_exposure > 0.0)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS venue_arming_one_per_venue
    ON venue_arming (venue_id) WHERE label IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS venue_arming_one_per_account
    ON venue_arming (venue_id, label) WHERE label IS NOT NULL;

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
