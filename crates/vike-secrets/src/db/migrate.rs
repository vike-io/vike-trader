//! The migration: `migrate` (the one act that creates the database), `write_pending`, `fill_into`.

use super::open::stamp_schema_version;
use super::*;

mod carry;
mod move_rows;
/// **The move and the fold — ruling 10's last two prerequisites, driven over a real store.**
#[cfg(test)]
mod move_rows_tests;
mod outcome;
mod planner;
mod preview;
mod upsert;

pub(crate) use carry::read_credential_file;
pub use move_rows::{MovedRows, move_pending_rows};
pub use outcome::{
    Ambiguity, MigrateError, Migration, MigrationOutcome, SourceReport, WhenNothingToCarry,
};
use planner::{Plan, plan};
#[cfg(feature = "test-support")]
pub use preview::SCHEMA_1;
pub use preview::{MigrationPlan, PlannedOutcome, preview};
pub(crate) use upsert::upsert_rows;

/// Declare the database FINISHED — the last write of a run that created one.
///
/// Separated from [`open_for_write`] deliberately: see that function's crash section. Until this
/// runs, `PRAGMA user_version` is `0` and every reader refuses the file loudly, so the window
/// between "the schema exists" and "the rows are committed" cannot be mistaken for a configured box.
/// The write half of [`migrate`], in its own function so the connection is DROPPED before its
/// caller can unlink the file — on Windows an open handle refuses the removal, so a cleanup written
/// inline would silently leave exactly the database it was added to remove.
///
/// Returns whether this call CREATED the database. ⚠ That answer comes from the OPEN and not from
/// the plan's read-only probe, and the two can genuinely differ: a sibling process may have created
/// the file in between. The open is the authority because it is the call that would have done the
/// creating, so the stamp below is decided by what actually happened rather than by what was
/// predicted — which is also exactly what a preview cannot promise, see [`preview`].
/// # ⚠ THREE acts, and which transaction each one is in
///
/// * **the RESHAPE** (schema 1 → 2) and **the STAMP** ride the SAME transaction as the inserts.
///   MEASURED in `db_tests::the_schema_stamp_and_the_ddl_are_both_transactional`: `PRAGMA
///   user_version` and DDL are both rolled back with the transaction that set them, so a box is
///   either fully at 2 or fully at 1 and there is no state in between. That is stricter than
///   schema 1 needed and the reason is the shape of schema 2: `SELECT name, value FROM credential`
///   — the exact statement a schema-1 binary runs — is STILL VALID SQL against the schema-2 table,
///   so a half-applied reshape stamped 1 over schema-2 tables would be ACCEPTED by an older binary
///   and answered from. Atomicity removes that state rather than documenting it.
/// * **the CREATE's stamp** also moved inside the transaction, and the crash story it was written
///   for is UNCHANGED. [`open_for_write`] still applies the DDL outside the transaction and still
///   does not stamp, so a kill between the schema batch and the commit still leaves
///   `user_version = 0` — the loud, unresumable state [`check_schema_version`] explains. What the
///   move removes is the window that used to sit AFTER the commit, in which the rows were durable
///   and the version was not.
fn write_pending(
    planned: &Plan,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
) -> Result<(bool, Option<crate::schema::RowReport>), DbError> {
    let (mut conn, created, version) = open_for_write(&planned.db)?;
    let tx = conn.transaction().map_err(|e| DbError::sql(&planned.db, e))?;

    let report = fill_into(&tx, planned, classify, version, created)
        .map_err(|e| DbError::sql(&planned.db, e))?;

    // ⚠ INSIDE the transaction — see this function's doc. On a create the version is still withheld
    // by `open_for_write`, so the unfinished-store story is unchanged.
    if created || version != SCHEMA_VERSION {
        // ONE spelling of the stamp, reached through the transaction — `Transaction` derefs to
        // `Connection`, so this is the same call `db_tests` makes and there is no second way to
        // declare a database finished.
        stamp_schema_version(&planned.db, &tx)?;
    }
    tx.commit().map_err(|e| DbError::sql(&planned.db, e))?;
    Ok((created, report))
}

/// **Everything a run WRITES into an open transaction** — the reshape when one is due, the node
/// rows, and the schema-2 fill — and the one place it is spelled.
///
/// ⚠ It is a free function over a `Transaction` rather than a step inside [`write_pending`] for the
/// same reason [`plan`] is a free function: [`preview`] has to perform it too, against a database
/// that is not the operator's, and a second spelling here would be a dry run describing a
/// classification the apply does not make. [`preview_rows`] is that caller. The STAMP and the
/// COMMIT stay with [`write_pending`], because those are the two acts a preview must not have.
fn fill_into(
    tx: &Transaction<'_>,
    planned: &Plan,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
    version: i64,
    created: bool,
) -> rusqlite::Result<Option<crate::schema::RowReport>> {
    let mut report = if version < SCHEMA_VERSION && !created {
        Some(crate::schema::reshape_into(tx, &planned.comments, classify)?)
    } else {
        None
    };

    // ⚠ AFTER the reshape branch above, not before it: on a store still at schema 1, `venue` does
    // not exist until `reshape_into`'s own `DDL` batch creates it. On a fresh create it already
    // exists (`open_for_write`'s create branch ran the same `DDL`), and on a store already at the
    // current schema this is the top-up's only chance to run at all — see this function's call site
    // in `write_pending` and [`ensure_venue_rows`]'s own doc for why it is unconditional rather than
    // guarded on `created`/`version`.
    //
    // ⚠ This used to be TWO calls, `ensure_venue_rows(tx)?;` then `ensure_venue_id_columns(tx)?;`,
    // ordered by hand and explained by a comment at each end. [`ensure_venue_id_columns`] now calls
    // [`ensure_venue_rows`] itself as its own first statement — a STRUCTURAL guarantee rather than a
    // positional one, because the four OTHER callers of `ensure_venue_id_columns`
    // (`edit_account`, [`move_pending_rows`], `crate::settings::write_settings`,
    // `crate::settings::set_venue_setting_in`) never called `ensure_venue_rows` at all and were
    // silently leaving `venue` empty on every write. See [`ensure_venue_id_columns`]'s own doc.
    ensure_venue_id_columns(tx)?;

    // The node table is `(name, value)` in every schema — 0051's pair is not an account's — so it
    // is written exactly as it always was.
    let mut credentials: BTreeMap<String, String> = BTreeMap::new();
    for ((table, name), value) in &planned.pending {
        match table {
            Table::NodeKey => {
                tx.execute("INSERT INTO node_key (name, value) VALUES (?1, ?2)", (name, value))?;
            }
            Table::Credential => {
                credentials.insert(name.clone(), value.clone());
            }
        }
    }
    if !credentials.is_empty() || report.is_none() {
        let fill = crate::schema::write_rows(tx, &credentials, &planned.comments, classify)?;
        match &mut report {
            Some(r) => r.absorb(fill),
            None => report = Some(fill),
        }
    }

    // ⚠ LAST, because it is DERIVED from the rows everything above just wrote. `write_rows` MINTS
    // account rows, and `crate::settings::fold_arming_into_accounts` answers per account — so a
    // fold before it would leave every newly minted row at the `DEFAULT 0` the DDL gives it, which
    // on an adopted box is the only copy of the operator's decision reading as "not stated". It is
    // also where the column reaches an already-migrated store at all: this is the only path a
    // CREDENTIAL write takes, and a box that never runs `config mirror` still runs this one.
    crate::settings::fold_arming_into_accounts(tx)?;
    Ok(report)
}

/// **Fill the database from the files.** Idempotent, non-destructive, and refusing rather than
/// half-finishing.
///
/// `settings_dir` is [`crate::SETTINGS_DIR_ENV`]'s value — the same PARAMETER every other resolver
/// in this crate takes, so this performs no environment read and resolves its three paths through
/// the ONE walk ([`crate::workspace_dotenv_path_from`], [`crate::workspace_node_path_from`],
/// [`crate::workspace_db_path_from`]).
///
/// `is_node_key` decides WHICH TABLE a name belongs to, and it is a parameter. ⚠ **The reason it
/// used to give is FALSE** — *"because this crate declares no `vike-*` dependency and so cannot see
/// `vike_model::credential_keys`"*. That edge was admitted by
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
/// 2026-09-20), and `vike_model::credential_keys::is_platform_key` IS reachable from here — the
/// same crate `ensure_venue_rows` already iterates `vike_model::VENUES` out of. ⚠ Read
/// 0072's *"Layers 15 → 10, strictly down"* as the EDGE's direction rather than as this crate
/// moving: `crates/vike-secrets/Cargo.toml` still declares `layer = 15`, and rank 10 is INSIDE
/// tier 15's floor — the band's rule is *nothing above rank 10*
/// (`crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s
/// `every_tier_15_crate_names_nothing_above_the_vocabulary`), not *no `vike-*` dependency at all*.
///
/// ⚠ **RULED 2026-09-26: the parameter STAYS, and it is the only seam in this crate that keeps
/// its shape WITHOUT a layer bound underneath it.** 0072 admitted one edge and ruled nothing about
/// injected seams, so the question it stranded here is answered rather than left to be
/// re-derived; the seam beside this one (`classify`) has a rank-25 bound that survives the edge
/// outright, and this one has nothing of the kind. What it has instead:
///
///   * **The shape is not this function's to remove.** [`crate::store::resolve_node_keys`] takes
///     the SAME predicate, and THAT seam is not collapsible at all: its correct value is
///     per-CALLER (`vike_model::credential_keys::is_tradehub_node_key` /
///     `is_datahub_node_key`), a WIDE one is a MEASURED defect — a `node.env` holding only the
///     datahub pair answered for a tradehub caller and dropped its working pair, producing a
///     silent `bad mac` — and `crates/vike-ops/tests/settings_secrets/node_key_store_gate.rs` polices it with a row
///     per production probe. So `impl Fn(&str) -> bool` stays on this crate's surface whatever
///     happens here; collapsing this one removes a parameter, not a concept.
///   * **The crate's own migration test passes ONE predicate to BOTH.**
///     `crates/vike-secrets/tests/migration/database/fixture.rs`'s `is_node_key` reaches this function and
///     `resolve_node_keys` alike, so a collapse would not retire the fixture — it would leave it
///     in place and stop half its call sites from taking it.
///   * **That fixture is a MEASUREMENT, and its independence is the point.** It is the node file's
///     names as read off one live box on one day — four, where `PLATFORM_KEYS` holds five since
///     the admin name joined. Importing the production table would make the migration's
///     node-vs-credential PARTITION track the table the test exists to be able to disagree with,
///     which is the same principle that makes `classify`'s fixture *"deliberately a SIMPLER rule
///     than the production one"*.
///
/// ⚠ **The residuals, stated because this ruling does not dissolve them.** The hazard in the
/// paragraph below — a per-service predicate reaching THIS call — is held off by prose and by
/// there being exactly one production caller, not by construction. And the third reason is
/// PROSPECTIVE: the two predicates are observationally identical over today's fixture, because no
/// test plants the fifth platform name, so it has never yet caught anything. **What would reopen
/// the collapse: a second production caller of this verb** — one call site is what makes a
/// hand-passed predicate auditable, and two is what makes it a choice somebody can get wrong.
///
/// Pass the UNION
/// predicate (`is_platform_key`, the four-name table), not a per-service one: 0051's narrowing is
/// about which FILE answers for one service's pair at READ time, whereas this is a CLASSIFICATION of
/// every name in the store, and a per-service predicate here would file the other service's pair as
/// a venue credential.
///
/// # What it does
///
/// 1. Reads both files through `crate::db::read_credential_file` — the one carry reader, so the migration cannot
///    disagree with the reader about what a line means. An ABSENT file contributes nothing; an
///    unreadable one is an error.
/// 2. Classifies: everything in the node file is a node key (and it REFUSES if the predicate
///    disagrees — [`Ambiguity::UnexpectedNameInNodeFile`]); in the credential file, a name the
///    predicate claims goes to `node_key` and every other name to `credential`. That second arm is
///    what DRAINS 0051's legacy home rather than stacking a third level on it.
/// 3. Compares against what is already stored, collecting EVERY finding before writing anything —
///    the three that make the RUN undecidable (returned as [`MigrateError::Ambiguous`], nothing
///    written) and the one that makes a single KEY undecidable (reported on [`Migration::refused`],
///    that key not written and every other key landed).
/// 4. Writes the pending rows in ONE transaction, then stamps [`SCHEMA_VERSION`] — or opens no write
///    connection at all when there is nothing pending, which is how *twice is the same as once* is
///    structural here rather than hoped for.
///
///    ⚠ **With nothing pending and NO DATABASE, it creates none**
///    ([`MigrationOutcome::NothingToMigrate`]). Creating an empty one is the harmful act: from that
///    moment `crate::store::backend_at` answers `Database` for every process on the box and the
///    credential file written afterwards is never read again.
///
///    ⚠ **Unless the caller ASKED for exactly that** — `when_empty` =
///    [`WhenNothingToCarry::CreateEmptyStore`], the operator's statement that this box starts fresh
///    (`vike-cli secrets migrate --init`). Then that one arm, and no other, creates the EMPTY store
///    ([`MigrationOutcome::Initialised`]) through the same [`write_pending`] a one-key migration
///    takes, so the store is the same shape. The guard is the plan itself: the arm is reached only
///    with `pending` EMPTY and no database, so a file carrying a key makes the run an ordinary
///    [`MigrationOutcome::Created`] that CARRIES it, a refused file makes it an `Err`, and an
///    existing database makes it the ordinary no-op — the flag can never produce an empty store
///    that shadows a credential file, and can never re-create, truncate or rewrite an existing one.
///
/// ⚠ **Steps 1, 2, 3 and the report are [`plan`], verbatim and shared with [`preview`]** — this
/// function is that plan plus step 4. It is one code path rather than two so that a dry run cannot
/// describe a migration different from the one that follows it.
///
/// # What it never does
///
/// Open either file for writing, in any branch. See the module doc.
/// ⚠ **Step 0, added with schema 2: it also UPGRADES a store it finds at an older schema**, in the
/// same transaction as everything else.
///
/// That is a widening of this verb and it is deliberate. The alternative was a second verb, and a
/// second verb would mean an operator whose box is at schema 1 runs `migrate`, is told *already
/// complete*, and is not told that every credential write on that box will now be refused. One
/// verb whose name already means *bring this store to the current schema*, with the `--dry-run`
/// this act has always deserved in front of it, is the smaller surface. `crate::schema`'s
/// `reshape_into` carries the atomicity argument.
///
/// `classify` is the second injected decision, beside `is_node_key` — but NOT for the same reason
/// any more, and the doc said it was. It says which ACCOUNT a credential name belongs to, and the
/// production implementation is `vike_bridge_core::credentials::classify_credential_name`: a
/// function of a crate this one cannot name in two independent ways — `vike-bridge-core` declares
/// `layer = 25` where this crate declares `15`, and tier 15's rule is *nothing above rank 10*,
/// machine-checked by `crates/vike-ops/tests/architecture/layer_gate/tiers.rs`'s
/// `every_tier_15_crate_names_nothing_above_the_vocabulary`, so rank 25 is refused by the BAND and
/// not merely by the down-only direction rule; and that crate declares `vike-secrets` as a normal
/// dependency, so the reverse edge is a cycle as well. That bound is untouched by
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`, which admitted one
/// edge AT the floor. ⚠ What WAS false in the old wording is the clause *"which
/// needs `vike_model::accounts::account_keys` … and this crate declares no `vike-*` dependency"*:
/// `vike_model::accounts::account_keys` is reachable from here, so it is the layer-25 half — and only that
/// half — carrying the seam. `crates/vike-secrets/src/schema.rs`'s module doc is the long form of
/// the surviving argument, and says in its own words that this is *"not licence to collapse the
/// seam."*
pub fn migrate(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
    when_empty: WhenNothingToCarry,
) -> Result<Migration, MigrateError> {
    let planned = plan(settings_dir, is_node_key)?;
    // The NAMES behind the counts, taken from the plan rather than read back out of the store
    // afterwards — see `Migration::inserted_keys`. A `BTreeMap` keyed `(table, name)` yields them
    // sorted, and the classification puts each name in exactly one table, so there are no
    // duplicates to fold.
    //
    // ⚠ A SCHEMA UPGRADE adds to this set, below, and that is not double-counting: it re-inserts
    // every credential row in the store while inserting no new KEY, so a ledger built from
    // `pending` alone would record NOTHING for the one act that rewrites the whole table and
    // cannot be undone. `crate::schema::RowReport::written_names` is that half.
    let mut inserted_keys: Vec<String> = planned.pending.keys().map(|(_, n)| n.clone()).collect();

    // 4. Write — or do not open for writing at all.
    //
    // ⚠ The `pending.is_empty() && !exists` arm is the one that must NOT open a write connection,
    // because opening one CREATES the database. See `MigrationOutcome::NothingToMigrate`, which
    // carries what that cost.
    // Step 0 — an existing store at an older schema is UPGRADED even when the files carry nothing
    // new, because "nothing to carry" and "this store is the shape this binary writes" are
    // different questions and only the second one decides whether a credential write will land.
    let must_upgrade = planned.version.is_some_and(|v| v != SCHEMA_VERSION);
    let schema_before = planned.version;
    // The ONE arm `when_empty` decides, and it is the `NothingToMigrate` state exactly: nothing
    // pending AND no database. A file carrying a key (`pending` non-empty) or a database already
    // there (`exists`) never reaches it, so the flag can neither shadow a credential file with an
    // empty store nor touch an existing one. See this function's step 4.
    let initialise = when_empty == WhenNothingToCarry::CreateEmptyStore
        && planned.pending.is_empty()
        && !planned.exists;

    let mut rows = None;
    let outcome = if planned.pending.is_empty() && !must_upgrade && !initialise {
        if planned.exists {
            if planned.refused.is_empty() {
                MigrationOutcome::AlreadyComplete
            } else {
                MigrationOutcome::NothingNewButRefused
            }
        } else {
            MigrationOutcome::NothingToMigrate
        }
    } else {
        // ⚠ `created` comes from the OPEN and not from `planned.exists`, and the two can genuinely
        // differ: a sibling process may have created the database between the plan's read-only
        // probe and this open. The open is the authority because it is the call that would have
        // done the creating, so the stamp below is decided by what actually happened rather than by
        // what was predicted. That difference is also exactly what a preview cannot promise — see
        // [`preview`].
        // ⚠ **A RUN THAT FAILS LEAVES NO DATABASE BEHIND, and that is a DIFFERENT case from the
        // unfinished one this module already argues.** Draw the line before reading further:
        //
        // * **The process is KILLED** between the commit and [`stamp_schema_version`] — nothing can
        //   prevent that, the rows sit at `user_version = 0`, and every fallible reader refuses
        //   LOUDLY with the recipe ([`check_schema_version`], pinned by
        //   `the_unfinished_database_says_it_cannot_be_resumed_and_names_the_way_out`). Unchanged.
        // * **This function RETURNS `Err`** — a path we control. Leaving the file then converts a
        //   working box into one whose credentials are unreadable until a human deletes a file by
        //   hand, in exchange for nothing: the credential FILES are untouched, so re-running is the
        //   whole repair. `upsert_rows` refuses the same shape and argues it; this is that rule for
        //   the one function whose JOB is to create the file.
        //
        // The guard is measured BEFORE the open, not taken from `open_for_write`'s `created`,
        // because `open_for_write` can fail AFTER `create_file_private` — at `Connection::open`, at
        // the `journal_mode` verification, or in `execute_batch(SCHEMA)` — and on those paths there
        // is no `created` to take. The unlink is `let _ =` deliberately: a cleanup may not replace
        // the operator's real error with a worse one, and it runs only over a path that did not
        // exist moments earlier and has never held a row belonging to anybody.
        let we_created_it = !planned.db.exists();
        match write_pending(&planned, classify) {
            Ok((created, report)) => {
                rows = report;
                if created && initialise {
                    MigrationOutcome::Initialised
                } else if created {
                    MigrationOutcome::Created
                } else if initialise {
                    // A sibling process created the database between the plan's read-only probe
                    // and this open (see `created`'s note above): nothing was pending, so this run
                    // added nothing to it, and it is the ordinary no-op rather than a creation.
                    MigrationOutcome::AlreadyComplete
                } else if must_upgrade {
                    MigrationOutcome::SchemaUpgraded
                } else {
                    MigrationOutcome::Updated
                }
            }
            Err(e) => {
                if we_created_it {
                    let _ = std::fs::remove_file(&planned.db);
                }
                return Err(e.into());
            }
        }
    };

    if outcome == MigrationOutcome::SchemaUpgraded
        && let Some(r) = &rows
    {
        inserted_keys.extend(r.written_names.iter().cloned());
        inserted_keys.sort();
        inserted_keys.dedup();
    }

    Ok(Migration {
        db: planned.db,
        outcome,
        schema_before,
        schema_now: SCHEMA_VERSION,
        rows,
        sources: planned.sources,
        doubly_claimed: planned.doubly_claimed,
        refused: planned.refused,
        inserted_keys,
    })
}
