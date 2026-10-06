//! What a migration answers with: `Ambiguity`, `MigrateError`, `SourceReport`, `Migration`.

use super::*;

// ---------------------------------------------------------------------------------------------
// The migration
// ---------------------------------------------------------------------------------------------

/// One reason [`migrate`] refused. **Names a KEY, never a value.**
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ambiguity {
    /// The node file carries a name the caller's predicate does not call a node key. The migration
    /// will not guess whether that is a venue credential filed in the wrong place or a node key the
    /// predicate has not heard of — either guess writes a credential into a namespace nothing will
    /// look for it in.
    UnexpectedNameInNodeFile {
        /// The key name.
        key: String,
    },
    /// The same name is in both files with DIFFERENT values: a half-migrated box, exactly the shape
    /// `crate::store::resolve_node_keys` refuses to merge. Choosing a side here produces a
    /// mismatched pair, whose symptom at the node is an opaque `bad mac`.
    DisagreeingFiles {
        /// The key name.
        key: String,
    },
    /// The file's value differs from the row already in the database. Nothing here can tell which is
    /// newer — the file may have been hand-edited after the move, or the row may have been written
    /// through software — so it refuses rather than clobbering a key an order is signed with.
    ///
    /// ⚠ **This one is a PER-KEY refusal and is reported on [`Migration::refused`], not through
    /// [`MigrateError::Ambiguous`].** The other three arms make the RUN undecidable; this one makes
    /// one KEY undecidable, and a whole-run refusal over it meant an operator who added a brand-new
    /// key in the same edit that left a stale line behind got neither key migrated. The refusal is
    /// unchanged in what it protects — the stored value is never overwritten, and the name is always
    /// printed — it just no longer takes its neighbours with it.
    DisagreesWithDatabase {
        /// The key name.
        key: String,
        /// Which table holds the differing row.
        table: Table,
    },
    /// The name is already in the OTHER table. This is the static predicate being enforced: a name
    /// has one home, and a run under a differently-scoped `is_node_key` may not give it a second.
    WrongTable {
        /// The key name.
        key: String,
        /// Where it already is.
        found_in: Table,
        /// Where this run wanted to put it.
        wanted: Table,
    },
}

impl std::fmt::Display for Ambiguity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ambiguity::UnexpectedNameInNodeFile { key } => write!(
                f,
                "{key} is in {} but the node-key predicate does not claim it — move it to {} if it \
                 is a venue credential, or widen the predicate if it is a node key",
                crate::dotenv::NODE_FILE,
                crate::dotenv::SECRETS_FILE
            ),
            Ambiguity::DisagreeingFiles { key } => write!(
                f,
                "{key} is in BOTH {} and {} with different values — this box is half-migrated; \
                 remove the stale line before migrating",
                crate::dotenv::SECRETS_FILE,
                crate::dotenv::NODE_FILE
            ),
            Ambiguity::DisagreesWithDatabase { key, table } => write!(
                f,
                "{key} already sits in the {table} table with a DIFFERENT value than the file's — \
                 nothing here can tell which is newer, so neither is overwritten"
            ),
            Ambiguity::WrongTable { key, found_in, wanted } => write!(
                f,
                "{key} is already in the {found_in} table and this run wants it in {wanted} — a \
                 name has ONE home (decision 0051), and nothing here will give it two"
            ),
        }
    }
}

/// [`migrate`] did not finish, and NOTHING was written.
#[derive(Debug)]
pub enum MigrateError {
    /// One of the two files exists and could not be read. (An ABSENT file is not this — it is an
    /// empty contribution, the same live gate `crate::store::resolve` implements.)
    Store(SecretsError),
    /// The database could not be opened, is not this schema, or refused `journal_mode=DELETE`.
    Db(DbError),
    /// The inputs do not determine an answer. EVERY finding is reported, not just the first, so one
    /// run tells the operator everything they have to fix.
    ///
    /// ⚠ **WHOLE-RUN findings only** — the three that make the run itself undecidable. A value that
    /// merely disagrees with a row already stored is a per-KEY refusal and rides
    /// [`Migration::refused`] on an otherwise successful run; see
    /// [`Ambiguity::DisagreesWithDatabase`].
    Ambiguous(Vec<Ambiguity>),
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::Store(e) => write!(f, "{e}"),
            MigrateError::Db(e) => write!(f, "{e}"),
            MigrateError::Ambiguous(list) => {
                write!(
                    f,
                    "refusing to migrate — {} ambiguous key(s), nothing written:",
                    list.len()
                )?;
                for a in list {
                    write!(f, "\n  - {a}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for MigrateError {}

impl From<SecretsError> for MigrateError {
    fn from(e: SecretsError) -> Self {
        MigrateError::Store(e)
    }
}

impl From<DbError> for MigrateError {
    fn from(e: DbError) -> Self {
        MigrateError::Db(e)
    }
}

/// What one file contributed to one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReport {
    /// The file that was READ. It was not written, moved, truncated or re-rendered.
    pub file: PathBuf,
    /// Where its keys went.
    pub table: Table,
    /// Names read out of this file and destined for this table.
    pub read: usize,
    /// Rows this run actually INSERTED. Zero on every run after the first — that is idempotence.
    pub inserted: usize,
    /// Rows already present with an identical value, so this run left them alone.
    pub already_present: usize,
}

/// **What a [`migrate`] run DID, including the two ways it did nothing.**
///
/// The distinction that matters is the last variant, and it is the reason this is an enum rather
/// than the `bool` it replaced: *the files held nothing, so no database was created*. That case used
/// to be indistinguishable from *the database already had everything*, and the code took the
/// harmful branch on it — it opened a write connection, which CREATES the store. See
/// [`MigrationOutcome::NothingToMigrate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// The database did not exist and this run created it and filled it.
    Created,
    /// The database existed and this run added rows to it.
    Updated,
    /// **The database existed at an OLDER schema and this run brought it to [`SCHEMA_VERSION`]**,
    /// in place, in one transaction — with or without new rows from the files beside it.
    ///
    /// It is its own outcome rather than folded into [`Self::Updated`] because it is the fact an
    /// operator most needs after running the verb: until it happens, a box whose binary reads
    /// schema 1 happily REFUSES every credential write to it
    /// ([`DbErrorKind::WriteToOlderSchema`]), and *already existed* would have read as *nothing to
    /// do*.
    SchemaUpgraded,
    /// The database existed and already held every key the files carry. Nothing was written and no
    /// connection was opened for writing — that is how *twice is the same as once* is structural
    /// here rather than hoped for.
    ///
    /// ⚠ This variant asserts COMPLETENESS, so it may only be chosen when
    /// [`Migration::refused`] is empty — see [`Self::NothingNewButRefused`].
    AlreadyComplete,
    /// **The database existed, nothing new was pending — and a key was REFUSED.** Not complete, and
    /// the distinction is not pedantry: a file value that disagrees with a stored row is held back
    /// (`Ambiguity::DisagreesWithDatabase`) rather than overwritten, so the store is missing a
    /// value the files carry and the operator has two values for one name.
    ///
    /// ⚠ It exists because [`Self::AlreadyComplete`] was chosen on `pending.is_empty()` ALONE, which
    /// printed *already existed, already complete* directly above a refusal line saying the run had
    /// not carried everything. A runbook or deploy check grepping stdout for `complete` would call
    /// that box converged.
    NothingNewButRefused,
    /// **There was nothing to migrate and NO DATABASE WAS CREATED.**
    ///
    /// Neither file carries a key (both absent, or both empty), and no database exists. The old code
    /// opened a write connection here and left behind a schema-stamped, zero-row `vike.db` — and
    /// from that moment `crate::store::backend_at` answers `Database` for every process on the box,
    /// so the credential file the operator writes afterwards is never read again.
    /// `crate::store::resolve_project` then returns an EMPTY map, which downstream is not an error
    /// but the LIVE GATE: every venue silently on paper while `secrets.env` sits on disk looking
    /// correct.
    ///
    /// **Creating the store IS the harmful act**, so this arm performs none of it: no directory, no
    /// file, no connection. A CLI verb should say *nothing to migrate* and exit successfully.
    NothingToMigrate,
}

impl std::fmt::Display for MigrationOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MigrationOutcome::Created => "created",
            MigrationOutcome::Updated => "already existed",
            MigrationOutcome::SchemaUpgraded => {
                "already existed at an OLDER schema and was upgraded in place"
            }
            MigrationOutcome::AlreadyComplete => "already existed, already complete",
            MigrationOutcome::NothingNewButRefused => {
                "already existed; nothing new to carry, and a key was REFUSED"
            }
            MigrationOutcome::NothingToMigrate => "NOT created — there was nothing to migrate",
        })
    }
}

/// What [`migrate`] did. Its `Display` is the operator's report: how many keys, from which file,
/// into which table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// The database this run was about. ⚠ It does not necessarily EXIST — see
    /// [`MigrationOutcome::NothingToMigrate`], the one outcome on which this path was deliberately
    /// left untouched.
    pub db: PathBuf,
    /// What happened. See [`MigrationOutcome`].
    pub outcome: MigrationOutcome,
    /// The schema the database carried BEFORE this run, or `None` when there was none.
    pub schema_before: Option<i64>,
    /// The schema it carries now — always [`SCHEMA_VERSION`] on a run that wrote anything.
    pub schema_now: i64,
    /// **What the schema-2 fill DID** — accounts created, rows classified, names the classifier
    /// could not place, and the rows §7 and §6 will move that this change deliberately did not.
    /// `None` on a run that wrote nothing. See [`crate::schema::RowReport`].
    pub rows: Option<crate::schema::RowReport>,
    /// One row per (file, table) pair that could have contributed — including the pairs that
    /// contributed nothing, because an empty node file is a fact worth printing.
    pub sources: Vec<SourceReport>,
    /// Names the credential file AND the node file both carry with an IDENTICAL value.
    ///
    /// ⚠ They are counted under the node file's row and under no other, because the node file is
    /// their home and one row is one key. That makes the credential file's `read` count LOWER than
    /// the number of `KEY=` lines in it — on precisely the half-migrated box where an operator most
    /// wants to reconcile the report against a `grep -c`. So the names are reported here as well,
    /// and the arithmetic closes: the credential file's rows plus this list is what that file holds.
    pub doubly_claimed: Vec<String>,
    /// **Keys this run REFUSED without refusing the run** — see [`Ambiguity::DisagreesWithDatabase`].
    ///
    /// A file value that disagrees with a row already stored is never overwritten and is always
    /// NAMED; what changed is that it no longer takes the rest of the edit down with it. A brand-new
    /// key added in the same edit LANDS, and this list is what the operator has left to reconcile.
    ///
    /// ⚠ A non-empty list means the run did not carry everything the files hold, even though it
    /// returned `Ok`. `MigrateError::Ambiguous` is still the WHOLE-RUN refusal, and still carries
    /// the three findings that make the run itself undecidable (a name the predicate does not claim,
    /// two files disagreeing with each other, a name that would acquire a second namespace).
    pub refused: Vec<Ambiguity>,
    /// **The key NAMES this run INSERTED** — not what the store holds, and not what was read.
    ///
    /// Sorted, and never a value. It exists because a CALLER that records the migration has to name
    /// what the migration did: the ledger record `vike-cli secrets migrate` appends takes key names,
    /// and the only other way to obtain them is to read the whole table back — which on a run that
    /// added one key to a sixty-seven-key store would name all sixty-eight and claim this run wrote
    /// them. That is an append-only record asserting something false, which is worse than one
    /// asserting nothing.
    ///
    /// A key NAME is not a secret: `vike-cli secrets list` prints names by an explicit decision in
    /// the root `CLAUDE.md`, and `vike_model::change_journal`'s `credential_write` records them for
    /// exactly the same reason.
    ///
    /// ⚠ **`inserted_keys.len()` is [`Migration::inserted`] ON EVERY OUTCOME BUT ONE, and this doc
    /// stated it without the exception.** On [`MigrationOutcome::SchemaUpgraded`] the two are
    /// deliberately different and the gap is the size of the store: `inserted()` sums
    /// [`Migration::sources`], which counts what the FILES contributed, and an upgrade contributes
    /// none of those while re-inserting every row in the table — so this list is the union of the
    /// pending names and `crate::schema::RowReport::written_names`, and is the larger of the two by
    /// however many keys the store already held. That is the whole reason the fold exists (see
    /// that field), so the invariant could never have held on the path it was added for. On every
    /// other outcome the per-file breakdown is a different decomposition of the same rows and
    /// `crates/vike-secrets/tests/migration/database/mod.rs` pins them equal.
    pub inserted_keys: Vec<String>,
}

impl Migration {
    /// Total rows this run inserted. `0` means the run was a no-op, which is what a second run is.
    #[must_use]
    pub fn inserted(&self) -> usize {
        self.sources.iter().map(|s| s.inserted).sum()
    }

    /// Did this run CREATE the database? (Was a public `bool` field; the four states it flattened
    /// are now [`MigrationOutcome`], and two of them mean "no database was written".)
    #[must_use]
    pub fn created(&self) -> bool {
        matches!(self.outcome, MigrationOutcome::Created)
    }

    /// Does a database EXIST at [`Migration::db`] as a result of this run having succeeded?
    ///
    /// False on exactly one outcome — [`MigrationOutcome::NothingToMigrate`] — and that is the
    /// invariant the existence-only backend probe rests on: **a database exists ⇒ a migration
    /// finished.**
    #[must_use]
    pub fn database_exists(&self) -> bool {
        !matches!(self.outcome, MigrationOutcome::NothingToMigrate)
    }

    /// Total names read out of the files, across both tables. (Named `keys_read` rather than `read`
    /// so it cannot be mistaken for a file-opening call by the tree scanners that key on that name —
    /// `crates/vike-ops/tests/settings/settings_registry/credential_store_scan.rs`'s `FILE_OPENERS`.)
    #[must_use]
    pub fn keys_read(&self) -> usize {
        self.sources.iter().map(|s| s.read).sum()
    }
}

impl std::fmt::Display for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "settings database {} ({})", self.db.display(), self.outcome)?;
        if self.outcome == MigrationOutcome::SchemaUpgraded {
            write!(
                f,
                "\n  schema {} -> {}: the account is a ROW now. Nothing was lost and every key \
                 name still answers exactly as it did — the `account`, `credential` and \
                 `venue_setting` tables are the new shape, and the credential file beside this \
                 database was READ ONLY, as always.\n  ⚠ ONE-WAY: a binary that predates schema \
                 {} will REFUSE this store, which downstream is an EMPTY credential map and \
                 therefore every venue on paper. Roll the BINARY back only together with this \
                 store.",
                self.schema_before.unwrap_or(0),
                self.schema_now,
                self.schema_now,
            )?;
        }
        if let Some(rows) = &self.rows {
            write!(f, "\n  {rows}")?;
        }
        for s in &self.sources {
            write!(
                f,
                "\n  {} -> {}: {} key(s) read, {} inserted, {} already present",
                s.file.display(),
                s.table,
                s.read,
                s.inserted,
                s.already_present
            )?;
        }
        if !self.refused.is_empty() {
            write!(
                f,
                "\n  ⚠ {} key(s) were REFUSED and nothing was overwritten — every other key above \
                 landed:",
                self.refused.len()
            )?;
            for a in &self.refused {
                write!(f, "\n    - {a}")?;
            }
        }
        if !self.doubly_claimed.is_empty() {
            write!(
                f,
                "\n  {} name(s) are in BOTH source files with an identical value and are counted \
                 under the node file's row above, so the credential file holds that many more \
                 `KEY=` lines than its row reports: {}",
                self.doubly_claimed.len(),
                self.doubly_claimed.join(", ")
            )?;
        }
        f.write_str("\n  the source files were READ ONLY — nothing was moved, rewritten or deleted")
    }
}
