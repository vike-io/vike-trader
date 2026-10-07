//! The `account` table as a value: `Account`, `Accounts`, `VenueRow`, `AccountKeys` and readers.

use super::*;

// ---------------------------------------------------------------------------------------------
// Reading the ACCOUNT table
// ---------------------------------------------------------------------------------------------

/// **The schema that first carried an `account` table.**
///
/// Named rather than spelled `2` at the one branch that needs it, because the branch is not *is
/// this the current schema* — [`SCHEMA_VERSION`] already answers that — but *is this old enough
/// that the table does not exist*. [`READABLE_SCHEMA_VERSIONS`] deliberately keeps a schema-1 store
/// readable, so the two questions have different answers on a box that has not run
/// `vike-cli secrets migrate` yet, and conflating them is how [`read_accounts`] would have handed
/// that box a raw `no such table: account` out of the engine.
pub const ACCOUNT_TABLE_SCHEMA: i64 = 2;

/// **One account of one venue, as the `account` table holds it** — the row that replaced the key
/// NAME as the answer to *which account is this*.
///
/// The columns are [`crate::schema::DDL`]'s, minus `notes`, and the omission is
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.3's rule rather than an
/// oversight: *notes are for humans, and code NEVER reads them — the moment code parses a note it
/// is not a note*, it is an undeclared schema with no validation and no gate. A reader that
/// RETURNED the column would be the invitation to parse it, and a fact code needs gets a column of
/// its own. A human-facing renderer that prints provenance adds it back as a display-only field and
/// argues for itself; nothing on the arming side needs it.
///
/// ⚠ **No value, no secret, no credential — by construction rather than by care.** An account row
/// carries identity and nothing else; the values live in `credential`, which [`read_accounts`]
/// never selects from. That is what makes `Debug` here safe to log verbatim, the same contract
/// [`DbError`] holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Account {
    /// **The identity** — opaque, and stable **for the life of ONE database file**. The owner's
    /// ruling at the spec's signature is that this is what identifies an account;
    /// [`Account::label`] is not.
    ///
    /// ⚠ **"Permanent, never reused" is the guarantee WITHIN a file, and this doc used to state it
    /// without the qualifier.** The scope is not a technicality — it is the difference between a
    /// number an operator may write down and one they may not:
    ///
    /// * **within a file it is permanent until something REMOVES a row — and since the account
    ///   lifecycle landed, something can.** This bullet used to read *"genuinely permanent"* and
    ///   justified it with *"nothing in this crate ever `UPDATE`s `account.id` or deletes a row"*.
    ///   The first half still holds; the second stopped being true the day [`AccountEdit::Remove`]
    ///   shipped, and the sentence did not move with it. What is left: `crate::schema`'s
    ///   `AccountResolver::resolve` takes its ids from `last_insert_rowid()`, nothing ever `UPDATE`s
    ///   an id, and a re-run of the migration RESOLVES existing rows (by owner prefix, then by
    ///   `(venue, tier, label)`) rather than re-creating them — so an id is stable across restarts,
    ///   re-migrations of the SAME file, and every other write this crate performs.
    ///
    ///   ⚠ **This bullet used to end with a REUSE, and stage 4 closed it.** It read: *"`account.id
    ///   INTEGER PRIMARY KEY` carries no `AUTOINCREMENT` (the schema has none anywhere), so SQLite
    ///   hands a new row `max(rowid) + 1`. Delete the row holding the LARGEST id and the next `Add`
    ///   is handed that same number. Concretely: remove account 16, add an account, and the new one
    ///   IS account 16."* The column is
    ///   `INTEGER PRIMARY KEY AUTOINCREMENT` now — the spec's §4.1, landed by §9's stage 4 — so the
    ///   engine keeps a high-water mark in `sqlite_sequence` and a freed id is never handed out
    ///   again for the life of this file. ⚠ **The guarantee rests on every REBUILD of this table
    ///   carrying that mark**, which `crate::schema::rebuild_table_from_ddl` does and
    ///   `crates/vike-secrets/tests/gates/sqlite_sequence/mod.rs` is the gate for. The verbs still echo
    ///   the row's credential KEY NAMES before acting (`echo_row`) rather than trusting the number,
    ///   because the bullet below is unchanged;
    /// * **across a RE-CREATED file it is not**, and `AUTOINCREMENT` does not reach this case at
    ///   all: the mark lives in the database FILE, so a file that was deleted takes it with it. A
    ///   fresh migration assigns ids in the order `AccountResolver` meets credential names —
    ///   `BTreeMap` order over the store's key names. Add a key, remove one, or rename a venue, and
    ///   the same accounts come back numbered differently.
    ///
    /// ⚠ **That second case is REACHABLE, and this tree documents the route.** The unrecoverable
    /// half-migration ([`DbErrorKind::SchemaVersion`] with `found == 0`) has exactly one repair and
    /// it is *delete the database and run the migration again*. A `venue_account_id` an operator
    /// wrote against the OLD ids is not carried across that — the column is filled by
    /// [`set_venue_account_id`] and by nothing a migration performs — so after a re-migration the
    /// books are absent, and if they are re-entered from a note that says *account 2 is 1234567*,
    /// account 2 may now be the other broker. **Re-read `vike-cli secrets accounts` and identify
    /// each row again after any re-migration**; the row's own credential key names
    /// ([`read_account_keys`]) are what identify it, never a remembered id.
    pub id: i64,
    /// A `vike_model::VENUES` id.
    pub venue: String,
    /// One of [`crate::schema::ACCOUNT_TIERS`] — the arming ceiling's vocabulary, lowercase.
    ///
    /// ⚠ **What this account CAN reach, never whether it MAY.** It is a fact about the credential
    /// set that minted the row, and [`Account::armed`] is the operator's decision about it; the two
    /// are separate axes and `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`
    /// §3.2 composes them as `armed ? tier : paper`. Nothing in a migration rewrites this column.
    pub tier: String,
    /// **Whether the operator ALLOWS this account to be used** — §3.2's permission half, and a
    /// DERIVED value rather than one anybody types.
    ///
    /// `crate::settings::fold_arming_into_accounts` is its only writer, and it recomputes the whole
    /// column from the `venue_arming` rows whenever either table changes: an account is armed
    /// exactly where the operator's own mode, resolved at THAT ACCOUNT'S level of the policy,
    /// names the tier the row already carries. So this column answers what
    /// `vike_config::VenuePolicy::account` answers, materialized — and
    /// `crates/vike-config/tests/no_ceiling_widens_across_the_migration.rs` is what holds the two
    /// equal, since this crate cannot name that function.
    ///
    /// ⚠ **`false` IS NOT *the operator disarmed this*, and a renderer must not print it as one.**
    /// Three different stores answer `false` for every row and only one of them is a decision: a
    /// store whose `account` table predates the column ([`read_accounts`] selects it only where it
    /// exists, the same shape `crate::settings::read_settings` takes for
    /// `venue_arming.max_exposure`); a store whose `venue_arming` table is EMPTY, which is every
    /// box that has not run `vike-cli config mirror`; and a store whose arming rows genuinely name
    /// no account's tier. The column cannot tell the first two from the third, which is why
    /// `vike-cli secrets accounts` does NOT render it — a `DISARMED` cell on an unmirrored box
    /// would be a confident answer about something this column cannot see, which is the shape
    /// [`Accounts`]' own three-state enum exists to refuse.
    ///
    /// ⚠ **The mount does not read this column at all yet**: the ceiling it consults is still
    /// `venue_arming` through `vike_config::VenuePolicy`, so a `false` here changes nothing a venue
    /// does. `crates/vike-secrets/src/settings/arming.rs`'s `fold_arming_into_accounts` carries why the
    /// table has not moved.
    pub armed: bool,
    /// The operator's name for the ROLE, and **`None` is the ordinary answer**.
    ///
    /// ⚠ A reader may not synthesise one. Every account a migration creates carries `NULL` here —
    /// `crate::schema`'s `AccountResolver` writes whatever the classifier supplied, and the owner
    /// refused the provisional `DEMO1`/`DEMO2` spellings outright at signature: *labels are
    /// informative and optional, `id` is the identity*. Rendering `"DEFAULT"` for a `None` would
    /// put back the string `vike_model::accounts::account_keys::AccountLabel::parse` refuses as reserved, and
    /// rendering an index token would put an identity back into the one column this schema exists
    /// to stop carrying one.
    pub label: Option<String>,
    /// **The BOOK, as the venue names it** — `None` until the venue has been asked.
    ///
    /// ⚠ It is `None` on every row a MIGRATION writes, not merely on dukascopy's two.
    /// [`crate::schema`]'s module doc states why: §11's steps 3 and 4 are not performed, so the ten
    /// book keys are still `credential` rows carrying their legacy names and nothing folds them
    /// here. A caller that needs one of those books today must read the legacy name; a caller that
    /// reads this column must treat `None` as *not yet known* and never as *this account has no
    /// book*.
    ///
    /// ⚠ **It is no longer `None` on every row of every store**, and the difference is one source
    /// out of three. [`set_venue_account_id`] fills it for a book an OPERATOR supplies, which is
    /// the only way the two dukascopy demo accounts can be told apart at all — their numbers are in
    /// no store and derivable from nothing in one. The other two sources of §4.5 are still owed and
    /// still unwritten: the migration's fold of the ten stored book keys (§11 step 3, sequenced
    /// behind the map renderer §7 requires) and the venue's own handshake (§12). So a `Some` here
    /// means *somebody or something told this store the book*, and this column does not record
    /// which — [`Account::last_verified_at`] is the column that does, and [`BookSource`] is how
    /// [`set_venue_account_id`] is told which of the two it is performing.
    pub venue_account_id: Option<String>,
    /// A sub-account's master. `None` is the ordinary answer and means *not a sub-account*.
    pub parent_id: Option<i64>,
    /// `false` = the operator no longer uses this account. See [`Accounts::active_for_venue`].
    pub active: bool,
    /// **Set by a successful authenticated SESSION**, and by nothing else. `None` on every migrated
    /// row, and on every row an operator has written a book onto by hand.
    ///
    /// ⚠ This doc read *"this column has no writer in the tree today"* until 2026-09-15, and that
    /// is the gap [`BookSource::Handshake`] closes: the writer is [`set_venue_account_id`] under
    /// that arm, reached by a fold of what a venue's own handshake answered. [`BookSource::Operator`]
    /// leaves it untouched, deliberately — see that enum, which carries the whole argument.
    ///
    /// The value is an RFC 3339 instant the CALLER formats
    /// (`vike_model::time::epoch_ms_to_utc_timestamp`); this crate carries no time dependency and
    /// parses nothing here. ⚠ The instant is the HANDSHAKE's, never the fold's — the column answers
    /// *when did this credential last authenticate*, and a fold two days later stamping its own
    /// `now` would answer *when was the CLI run* while looking like the first.
    pub last_verified_at: Option<String>,
}

/// **Why a store cannot answer the account question AT ALL** — which is a different answer from
/// *this store holds no accounts*.
///
/// The distinction is the whole reason [`Accounts`] is an enum. [`crate::store::Backend`] is a
/// per-RUN choice and a box that has not migrated has no `account` table on it; collapsing that
/// into an empty list would hand every caller the sentence *this venue has no accounts* about a
/// store that is holding its credentials perfectly well under their legacy names. That is the
/// shape of the failure §1 of the spec is about — a store answering confidently about something it
/// cannot see — and it is the same posture this crate already takes between an ABSENT store (the
/// live gate, an empty map) and one that EXISTS and will not open ([`SecretsError`], loud).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoAccountTable {
    /// [`crate::store::Backend::Files`] — there is no settings database on this box, so the
    /// credential FILE answers and the accounts live in the key NAMES.
    /// `vike_model::accounts::account_keys::accounts_in_store` is the reader that applies to that store.
    FileStore {
        /// The credential file that is answering instead.
        file: PathBuf,
    },
    /// A settings database at a schema OLDER than [`ACCOUNT_TABLE_SCHEMA`] — readable (that is what
    /// [`READABLE_SCHEMA_VERSIONS`] buys) and carrying no such table. `vike-cli secrets migrate` is
    /// what moves it.
    OlderSchema {
        /// The database that was opened.
        path: PathBuf,
        /// The schema it carries, as `PRAGMA user_version` reported it.
        found: i64,
    },
}

impl std::fmt::Display for NoAccountTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoAccountTable::FileStore { file } => write!(
                f,
                "the credential store {} is a FILE, which carries no account table — the accounts \
                 of a file store are in the key names",
                file.display()
            ),
            NoAccountTable::OlderSchema { path, found } => write!(
                f,
                "the settings database {} is at schema {found}, which predates the account table \
                 (schema {ACCOUNT_TABLE_SCHEMA}) — `vike-cli secrets migrate` is what moves it",
                path.display()
            ),
        }
    }
}

/// **What a store can say about the accounts it holds.**
///
/// Two arms and not an `Option<Vec<_>>`, because the caller that merges them is the caller this
/// type exists to stop: [`Accounts::Known`] with an empty vector is *the table is there and holds
/// nothing*, and [`Accounts::Unanswerable`] is *this store cannot be asked*. See
/// [`NoAccountTable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accounts {
    /// The store carries an `account` table and these are its rows, **`id`-ordered** — every row,
    /// inactive ones included, because filtering is the caller's decision and a silent one here
    /// would retire an account nobody asked to retire.
    Known(Vec<Account>),
    /// The store has no `account` table. Carries WHICH store, and why.
    Unanswerable(NoAccountTable),
}

impl Accounts {
    /// The rows, or `None` when the store could not be asked.
    #[must_use]
    pub fn known(&self) -> Option<&[Account]> {
        match self {
            Accounts::Known(rows) => Some(rows),
            Accounts::Unanswerable(_) => None,
        }
    }

    /// Why the store could not be asked, or `None` when it answered.
    #[must_use]
    pub fn unanswerable(&self) -> Option<&NoAccountTable> {
        match self {
            Accounts::Known(_) => None,
            Accounts::Unanswerable(why) => Some(why),
        }
    }

    /// **Every ACTIVE account of one venue**, `id`-ordered — or `None` when the store could not be
    /// asked.
    ///
    /// ⚠ `Some(&[])` and `None` are DIFFERENT ANSWERS and a caller may not merge them: the first
    /// is *this store knows its accounts and this venue has none*, the second is *ask the key
    /// names instead*. The `active` filter is in the NAME rather than behind a flag, so a call site
    /// cannot arm a retired account by forgetting an argument; [`Accounts::Known`] is where a
    /// caller that genuinely wants the inactive rows goes.
    #[must_use]
    pub fn active_for_venue(&self, venue: &str) -> Option<Vec<&Account>> {
        Some(self.known()?.iter().filter(|a| a.active && a.venue == venue).collect())
    }
}

/// **Every row of the `account` table** — the reader that asks the DATABASE which accounts exist,
/// rather than parsing them out of credential key names.
///
/// The twin of [`read_table`] for the third table, and deliberately the same shape: the WHOLE
/// table, because a per-venue `SELECT` would be a second query shape for callers to reason about
/// and the table is sixteen rows on the live box. Filtering is [`Accounts::active_for_venue`], over
/// the rows this returned.
///
/// # What it never does
///
/// * **It never touches `credential`.** No value of any kind is selected, so no error, no `Debug`
///   and no log line reachable from here can become a credential.
/// * **It never writes.** [`open_for_read`] opens `SQLITE_OPEN_READ_ONLY` and creates nothing —
///   which matters more here than usual, because a reader that could create the file would turn
///   *this box has no database* into *this box has an empty database*, and under
///   [`crate::store::Backend`] that is the difference between the credential file answering and
///   every venue silently dropping to paper.
/// * **It never answers `Known(vec![])` for a store that has no table.** See [`NoAccountTable`].
///
/// # The version branch
///
/// A schema-1 database is READABLE by design ([`READABLE_SCHEMA_VERSIONS`]) and has no `account`
/// table, so the `SELECT` below would fail at PREPARE with the engine's own `no such table`. That
/// arrives as [`DbErrorKind::Sqlite`] — an opaque malfunction — for a store that is simply older
/// than the table. The branch turns it into the classification it actually is.
pub fn read_accounts(path: &Path) -> Result<Accounts, DbError> {
    let (conn, version) = open_for_read(path)?;
    if version < ACCOUNT_TABLE_SCHEMA {
        return Ok(Accounts::Unanswerable(NoAccountTable::OlderSchema {
            path: path.to_path_buf(),
            found: version,
        }));
    }
    // ⚠ `ORDER BY id` and not by `(venue, tier)`: `id` is the identity (the owner's ruling), it is
    // monotonic per INSERT, and it is the one column guaranteed present and distinct on every row —
    // so the order is stable across boxes and across runs, which a nullable `label` is not.
    // ⚠ The statement head is [`account_select`]'s, which is where `armed`'s absence on a store
    // older than that column, and the venue link's shape, are handled — once, for all three
    // readers. This one is the reader `vike-cli secrets accounts` reaches, on a box that may never
    // have written since migrating, so it may meet a store no writer has carried onto `venue_id`.
    let select = account_select(&conn).map_err(|e| DbError::sql(path, e))?;
    let mut stmt =
        conn.prepare(&format!("{select} ORDER BY a.id")).map_err(|e| DbError::sql(path, e))?;
    let rows = stmt.query_map([], account_from_row).map_err(|e| DbError::sql(path, e))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| DbError::sql(path, e))?);
    }
    Ok(Accounts::Known(out))
}

/// One row of the `venue` table: the roster venue's identity in this store, its key, and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueRow {
    /// `venue.id` — the number every venue link in this store names.
    pub id: i64,
    /// The roster key (`binance`, `ctrader`) — the ADDRESS a human types and the wire carries.
    pub name: String,
    /// The venue's own spelling (`Binance`, `cTrader`). `None` on a store no writer has carried
    /// since the column was added; a reader shows [`VenueRow::name`] then.
    ///
    /// ⚠ **A NULL stays NULL.** The seed spelling is written ONCE per row — when the column is added,
    /// and when a roster venue's row is inserted — and no later write refills it, so a title somebody
    /// cleared stays cleared and the reader shows the key. That is deliberate (a refill would be the
    /// rewrite-on-every-write the owner ruled out), and
    /// `crates/vike-secrets/tests/accounts/venue_titles.rs`'s `a_title_the_operator_cleared_is_never_refilled`
    /// pins it.
    pub title: Option<String>,
}

/// **Every row of the `venue` table**, id-ordered. Read-only. An absent database, or one that
/// predates the table, answers an empty list: a node with no store has no venues to name.
pub fn read_venues(path: &Path) -> Result<Vec<VenueRow>, DbError> {
    if !database_present(path) {
        return Ok(Vec::new());
    }
    let (conn, _version) = open_for_read(path)?;
    let present: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'venue')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| DbError::sql(path, e))?;
    if !present {
        return Ok(Vec::new());
    }
    let title = match crate::settings::has_column(&conn, "venue", "title")
        .map_err(|e| DbError::sql(path, e))?
    {
        true => "title",
        false => "NULL",
    };
    let mut stmt = conn
        .prepare(&format!("SELECT id, name, {title} FROM venue ORDER BY id"))
        .map_err(|e| DbError::sql(path, e))?;
    let rows = stmt
        .query_map([], |r| Ok(VenueRow { id: r.get(0)?, name: r.get(1)?, title: r.get(2)? }))
        .map_err(|e| DbError::sql(path, e))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| DbError::sql(path, e))
}

/// **What tells one account row from another when the row itself does not** — the credential key
/// NAMES it owns, and the owner PREFIXES those names imply.
///
/// # Why a listing needs this at all
///
/// `(id, venue, tier, label, venue_account_id)` is the whole of an [`Account`], and for the pair
/// this schema's hardest case is about — dukascopy's two demo rows, `(dukascopy, demo, NULL)`
/// twice over, both books `NULL` until somebody writes them — every one of those cells is
/// IDENTICAL except `id`. A listing built from [`Account`] alone therefore renders the two rows the
/// operator must tell apart as two indistinguishable lines differing by an opaque integer, and
/// [`set_venue_account_id`] addresses a row by exactly that integer. There is no way to choose.
///
/// The discriminating fact is already in the store and is not in the `account` table: each row's
/// own `credential` rows carry their LEGACY NAMES (§4.1 of
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md`: *`name` is the only record*), so
/// `DUKASCOPY_DEMO1_LOGIN` belongs to one row and `DUKASCOPY_DEMO2_LOGIN` to the other. That is
/// what an operator recognises, and it is what this type carries.
///
/// # ⚠ NAMES ONLY — never a value, and structurally so
///
/// [`read_account_keys`] selects `name` and `field` and nothing else. `credential.value` is not in
/// the statement, so no row, no `Debug`, no error and no rendering reachable from this type can be
/// a credential — the same construction [`Account`] holds by never touching the table at all.
/// A caller may print every field of this verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountKeys {
    /// The OWNER PREFIXES — each credential name with its own `field` suffix removed, deduplicated
    /// and sorted. `DUKASCOPY_DEMO1_`, `BINANCE_LIVE_`.
    ///
    /// The same derivation `crate::schema`'s `AccountResolver::load` performs to build `by_prefix`,
    /// and it is deliberately the same one: that map is how a re-run of the migration finds the
    /// account an earlier run created, so a prefix here is the identity the STORE itself uses.
    ///
    /// ⚠ **More than one is normal, not a fault.** A legacy tier spelling and its canonical twin
    /// are one account with two prefixes (`ASTER_MAINNET_` and `ASTER_LIVE_`, unified because
    /// `AccountRef::tier` normalizes `MAINNET` onto `LIVE`), and both belong on the row.
    ///
    /// Empty when no live credential row names this account — an account row can outlive the last
    /// key that created it, and an empty list says so rather than pretending.
    pub prefixes: Vec<String>,
    /// Every live credential key NAME filed against this account, sorted.
    ///
    /// The evidence behind [`AccountKeys::prefixes`], for the case where a prefix is not enough —
    /// and the thing an operator actually recognises from their own `secrets.env`.
    pub names: Vec<String>,
}

/// **The credential key NAMES each `account` row owns** — the discriminator [`read_accounts`]
/// cannot return, keyed by [`Account::id`].
///
/// See [`AccountKeys`] for what this is for and why a listing is unusable without it. A row with no
/// live credential rows is simply absent from the map; a caller renders that as an empty
/// [`AccountKeys`] (which is its [`Default`]).
///
/// # What it never does
///
/// * **It never selects a value.** The statement is `SELECT name, field, account_id FROM
///   credential`, so a credential value is not merely omitted from the result — it is not read.
/// * **It never writes**, and it never creates: [`open_for_read`]'s `SQLITE_OPEN_READ_ONLY`, for
///   the reason spelled at [`read_accounts`].
/// * **It skips SUPERSEDED rows** (`superseded_at IS NULL`), exactly as `AccountResolver::load`
///   does — a name that has been rotated out is not evidence about who the row is today.
///
/// A schema older than [`ACCOUNT_TABLE_SCHEMA`] has no `account` table and therefore no
/// `credential.account_id`; it answers with an EMPTY map rather than an error, because its caller
/// has already been told the real answer by [`read_accounts`] — which returns
/// [`Accounts::Unanswerable`] for that store and is the authority on it.
pub fn read_account_keys(path: &Path) -> Result<BTreeMap<i64, AccountKeys>, DbError> {
    let (conn, version) = open_for_read(path)?;
    if version < ACCOUNT_TABLE_SCHEMA {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT name, field, account_id FROM credential \
             WHERE superseded_at IS NULL AND account_id IS NOT NULL ORDER BY name",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })
        .map_err(|e| DbError::sql(path, e))?;

    let mut out: BTreeMap<i64, AccountKeys> = BTreeMap::new();
    let mut prefixes: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        let (name, field, id) = row.map_err(|e| DbError::sql(path, e))?;
        // The SAME derivation `AccountResolver::load` performs — see `AccountKeys::prefixes`. A
        // classifier whose `field` is not a suffix of the name yields no prefix (the LABELLED
        // grammar, legitimately), and the NAME below still carries the evidence.
        if let Some(prefix) = name.strip_suffix(field.as_str()) {
            prefixes.entry(id).or_default().insert(prefix.to_string());
        }
        out.entry(id).or_default().names.push(name);
    }
    for (id, set) in prefixes {
        out.entry(id).or_default().prefixes = set.into_iter().collect();
    }
    Ok(out)
}

/// **The `account` statement HEAD every [`Account`] reader selects through — projection, table
/// alias and venue join together — and the ONE place `armed`'s absence and the venue link's shape
/// are handled.** A caller appends only its `WHERE`/`ORDER BY` tail, over the alias `a`.
///
/// ⚠ `crate::schema::DDL` is `CREATE TABLE IF NOT EXISTS`, so `armed` reaches an ALREADY-MIGRATED
/// store through `crate::settings::fold_arming_into_accounts`' `ALTER TABLE` and not before — and
/// two of this module's three readers run INSIDE a write transaction whose fold has not happened
/// yet, so "a writer will have added it" is not true even there. A reader naming the column
/// unconditionally answers a bare `no such column: armed` on both live boxes, for a store that is
/// simply older than it.
///
/// ⚠ **The venue is read by its NUMBER** (`crate::schema::VenueLink`), and that is why this is a
/// whole statement head rather than the column list it used to be: the number needs a join, and a
/// projection a caller could select from without the join is a reader that silently falls back to
/// the text. The text answers only for a row whose number is NULL, which a store no writer has
/// carried yet can hold — [`read_accounts`] is read-only and [`set_venue_account_id`] runs no
/// funnel, so both can meet one.
///
/// One function rather than three `match`es because the three statements differ only in their
/// `WHERE`/`ORDER BY` tail: a per-site copy is a second answer to *is the column there*, and the
/// one that got copied wrong would be the one inside the account VERBS.
pub(super) fn account_select(conn: &Connection) -> rusqlite::Result<String> {
    let armed =
        if crate::settings::has_column(conn, "account", "armed")? { "a.armed" } else { "0" };
    let link = crate::schema::VenueLink::of(conn, "account", "a")?;
    Ok(format!(
        "SELECT a.id, {}, a.tier, a.label, a.venue_account_id, a.parent_id, a.active, \
         a.last_verified_at, {armed} FROM account a {}",
        link.name, link.join
    ))
}

/// One row of [`account_select`], as an [`Account`]. The mapper and the projection move together.
pub(super) fn account_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        venue: r.get(1)?,
        tier: r.get(2)?,
        label: r.get(3)?,
        venue_account_id: r.get(4)?,
        parent_id: r.get(5)?,
        // `STRICT` types both flags INTEGER and the DDL's `CHECK (… IN (0, 1))` is what makes
        // these comparisons exact rather than a truthiness convention.
        active: r.get::<_, i64>(6)? != 0,
        last_verified_at: r.get(7)?,
        armed: r.get::<_, i64>(8)? != 0,
    })
}
