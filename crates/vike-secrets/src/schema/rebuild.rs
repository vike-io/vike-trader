//! The rebuild: ONE spelling of copy-into-the-shipped-shape that every in-place upgrade step uses, its refusals, and the DDL introspection it rests on.

use rusqlite::Transaction;

use super::steps::any_tier::{carried_into_shipped_shape, carried_key_collisions};
use super::steps::venue_links::{dangling_references, unresolved_refusal, unresolved_venue_links};
use super::*;

// ---------------------------------------------------------------------------------------------
// The rebuild — ONE spelling, used by all five migrations above
// ---------------------------------------------------------------------------------------------

/// **A refusal the store's repair NAMES** — [`rebuild_table_from_ddl`]'s trap 5, its trap 7, or a
/// rebuild that left a dangling reference: rows an operator can find and repair with a SQLite
/// client, which no `vike-cli` verb can, because every one of them writes through this repair.
///
/// It travels INSIDE the engine error the funnel's passes return, and that is what makes it
/// distinguishable at every door at once: `crate::db::DbError::sql`, which every caller of the
/// funnel maps its error through, answers `crate::DbErrorKind::RepairRefused` for an error carrying
/// one, and leaves every OTHER engine failure — a full disk, a read-only store, a corrupt page, a
/// constraint the copy itself met — the `crate::DbErrorKind::Sqlite` it always was. A second error
/// type through the five passes was the alternative, and it would have changed the currency of
/// every pass and of every caller of the funnel to say one thing.
///
/// ⚠ **The carrier is `rusqlite::Error::ToSqlConversionFailure` for one reason only**: it is the one
/// variant boxing an arbitrary error that this crate's `rusqlite` features compile
/// (`UserFunctionError` sits behind the `functions` feature, which the workspace does not enable).
/// Nothing converts a value here. The variant is a box, its `Display` is this refusal's message
/// verbatim, and its `source()` is this refusal.
#[derive(Debug)]
pub(crate) struct NamedRefusal(String);

impl NamedRefusal {
    /// The refusal `message`, as the engine error the funnel's passes return.
    pub(super) fn into_error(message: String) -> rusqlite::Error {
        rusqlite::Error::ToSqlConversionFailure(Box::new(NamedRefusal(message)))
    }

    /// Whether `e` carries one — the question `crate::db::DbError::sql` asks of every error.
    pub(crate) fn is(e: &rusqlite::Error) -> bool {
        matches!(e, rusqlite::Error::ToSqlConversionFailure(inner) if inner.is::<NamedRefusal>())
    }
}

impl std::fmt::Display for NamedRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NamedRefusal {}

/// **How every [`NamedRefusal`] sends an operator into the store** — one spelling, so the three
/// cannot drift apart about the two things that come BEFORE any statement (final review M2).
///
/// * **The client version.** Every table here is `STRICT`, which SQLite reads from 3.37 on; an
///   older `sqlite3` cannot open the store at all, so the instruction would fail before it
///   started. Worded to be true whichever version a box carries.
/// * **`PRAGMA foreign_keys = ON;` first.** The `sqlite3` shell starts with foreign keys OFF, so a
///   parent row deleted there before the rows naming it is not refused — and the next write then
///   meets the dangling reference this file's last refusal names, with every write still refused.
///   Turned on, the engine itself refuses a premature delete.
pub(super) const SQLITE_CLIENT_REPAIR: &str = "open this database with a SQLite client — the `sqlite3` \
     shell, which a deployment may need to install; if your `sqlite3` is older than 3.37 it cannot \
     open this database (its tables are STRICT), so use a newer client — and run `PRAGMA \
     foreign_keys = ON;` first: the shell starts with foreign keys OFF, and nothing would then stop \
     a row being deleted while other rows still name it";

/// What [`rebuild_table_from_ddl`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Rebuild {
    /// The table now carries [`DDL`]'s shape, its old rows, and its indexes.
    Done,
    /// **Left exactly as it was**, because the store's table is missing a column the shipped shape
    /// declares `NOT NULL` with no `DEFAULT` — see [`required_columns_of`] for what that state is
    /// and why a skip is the right answer to it.
    ///
    /// ⚠ **The missing names ride along, and that is the whole point of the payload.** This variant
    /// was a unit, and the three call sites turned it into a bare `continue` — so a store that
    /// DECLINED the repair was indistinguishable from one that needed none. What the operator then
    /// meets is the next write refusing against the shape that was never repaired (a `'sim'` CHECK
    /// rejecting a `paper` tier, say) with nothing anywhere naming the cause. The columns are
    /// carried so the cause exists as data at the moment it is known rather than being computed
    /// and thrown away.
    ///
    /// # ⚠ NOTHING CAN REACH THIS VARIANT TODAY — stated plainly rather than implied
    ///
    /// No test constructs it through [`rebuild_table_from_ddl`]; the only coverage is
    /// `a_declined_rebuild_carries_the_reason_it_declined` in this module's own `schema_tests`,
    /// which builds the
    /// variant by hand, so what is proved is [`Rebuild::decline_note`]'s RENDERING and not the
    /// decline. And nothing in production can produce it, because [`required_columns_of`] derives
    /// its set from the SHIPPED [`DDL`] and every store that reaches this code was created by some
    /// version of that same `DDL`. The one older shape — schema 1 — dies EARLIER:
    /// `crate::db::ensure_venue_id_columns` opens with `crate::db`'s `ensure_venue_rows`, whose
    /// `tx.execute_batch(DDL)` cannot PREPARE `credential_one_live_value ON credential
    /// (account_id, field)` against it. MEASURED by the branch review of 2026-09-23 on a planted
    /// `account` with no `venue_account_id`: `no such column: venue_account_id`, out of that batch,
    /// with the rebuild log empty. That guard covers every required column an INDEX also names,
    /// which is most of them.
    ///
    /// **What would make it reachable**, and it is the state the variant exists for: `DDL` gaining
    /// a `NOT NULL`-with-no-`DEFAULT` column on an existing table. SQLite refuses to `ALTER TABLE
    /// … ADD COLUMN` one, so `DDL` alone cannot deliver it to a store already on disk, and on the
    /// day such a column ships every pre-existing store declines here. A column no index names
    /// reaches this branch rather than the `execute_batch` crash above it — `account.tier` and
    /// `credential.value` are two that are required and indexed by nothing.
    ///
    /// ⚠ **The venue-links flip SHIPPED such a column and still does not reach this branch**, for
    /// one reason that is load-bearing: `venue_id` is `NOT NULL` with no `DEFAULT` on `account`,
    /// `venue_setting` and `venue_arming`, and `crate::db::ensure_venue_id_columns` adds it
    /// (NULLABLE, which `ALTER TABLE` can do) and backfills it BEFORE any pass rebuilds a table. So
    /// the column a rebuild requires is always there to copy. Move that loop back below the passes
    /// and every store that predates `venue_id` declines here, on every pass.
    ///
    /// ⚠ **So the warnings channel [`Rebuild::decline_note`] records as OWED is owed for a branch
    /// nothing can reach**: it is not a hole in today's repair, and building it now would be
    /// building a reporting path with no producer. Build it with the first `DDL` change that can
    /// trigger a decline, and do not delete these branches in the meantime — they are the correct
    /// answer to a state this schema is one column away from.
    Skipped {
        /// Every `NOT NULL`-with-no-`DEFAULT` column the shipped shape declares and the store's
        /// table does not have — what [`Rebuild::decline_note`] renders.
        missing: Vec<String>,
    },
}

impl Rebuild {
    /// The operator-facing sentence for a decline, or `None` for a rebuild that ran.
    ///
    /// ⚠ **This crate has NOWHERE to send it yet, and that is a declared residual rather than an
    /// oversight.** `vike-secrets` carries no logging dependency by design (one external crate,
    /// `rusqlite`), and the three passes that call [`rebuild_table_from_ddl`] return
    /// `rusqlite::Result<()>` into `crate::db::ensure_venue_id_columns`, inside the caller's
    /// transaction. Turning a decline into a REFUSAL is ruled out on its own terms:
    /// [`required_columns_of`]'s doc states the design — *a repair that cannot run must decline
    /// rather than abort* — and a refusal there would take out the operator's credential write.
    /// So what is owed is a warnings channel out through that funnel, which is one signature in
    /// `crate::db`. Until it exists, this renders the note and the call sites name it.
    ///
    /// ⚠ **Read that debt with [`Rebuild::Skipped`]'s own reachability section beside it**: no
    /// store this binary can meet produces a decline today, so the channel is owed for a producer
    /// that does not exist yet. That is a reason to leave it unbuilt, not a reason to delete the
    /// renderer — the first `NOT NULL`-with-no-`DEFAULT` column `DDL` gains makes both real on the
    /// same day. (⚠ `venue_id` was such a column and did NOT, only because the funnel adds it
    /// before any rebuild: see [`Rebuild::Skipped`].)
    pub(super) fn decline_note(&self, table: &str) -> Option<String> {
        let Rebuild::Skipped { missing } = self else { return None };
        Some(format!(
            "`{table}` was left on its old shape: the store's table has no {}, which the shipped \
             DDL declares NOT NULL with no DEFAULT, so a rebuild would have nothing to put there. \
             Every later write this repair was meant to enable will be refused by the OLD \
             constraint until that column exists",
            missing.join(", ")
        ))
    }
}

/// **Rebuild `table` into [`DDL`]'s shape, carrying its rows, its ids and its `AUTOINCREMENT`
/// high-water mark** — the one spelling of the procedure [`migrate_sim_tier_to_paper`],
/// [`migrate_tables_onto_autoincrement`], [`migrate_dropped_columns`],
/// [`migrate_venue_setting_tier_to_any`] and [`migrate_venue_links_onto_venue_id`] all perform.
/// (⚠ This named the first two as *both* until 2026-09-26; §9 stage 4c's pass had already been a
/// third caller for three days.)
///
/// `select_expr` rewrites ONE column's value during the copy, returning `None` for a column that
/// travels unchanged. It exists because a value migration and a shape migration are the same act:
/// the rewrite must happen IN the `INSERT … SELECT`, since an `UPDATE` afterwards would run
/// against the new constraint the copy just passed.
///
/// # The seven traps this is written around
///
/// (Four until §5.2 step 7 added 5 and 6, and six until the venue-links flip added 7. Those three
/// are the only ones a caller cannot opt out of: they belong to the SHAPE being copied into, not to
/// the pass asking for the copy.)
///
/// 1. **The OLD table keeps the real name; the NEW one is built beside it and renamed in.** The
///    obvious spelling is the one [`reshape_into`] uses — rename the old table aside, re-run
///    [`DDL`], copy, drop — and it is WRONG for `account`, MEASURED rather than reasoned about.
///    `crates/vike-secrets/src/db.rs`'s open path sets and VERIFIES `PRAGMA foreign_keys = ON`,
///    and under that setting SQLite REWRITES every other table's `REFERENCES` clause to follow a
///    rename: `credential.account_id REFERENCES account(id)` silently becomes `REFERENCES
///    account_pre_paper_tier(id)` and survives the scratch table's own DROP as a clause pointing
///    at nothing. ⚠ **`PRAGMA legacy_alter_table` does NOT suppress that HERE, and the reason is
///    not the one this line used to give.** It said *it governs trigger and view bodies, while the
///    foreign-key rewrite is governed by `foreign_keys`*, and that mechanism is wrong — MEASURED on
///    2026-09-23 against SQLite 3.53.2, the matrix is in
///    `crates/vike-secrets/tests/gates/sqlite_sequence/engine_seam.rs`'s `rebuild_preserving_ids`. What the
///    engine actually does is: with `foreign_keys` OFF, `legacy_alter_table = ON` DOES suppress the
///    `REFERENCES` rewrite; with `foreign_keys` ON, nothing suppresses it. The conclusion for THIS
///    function is unchanged and is now load-bearing for the right reason — `PRAGMA foreign_keys` is
///    a NO-OP inside a transaction (measured: issued inside one it returns `Ok` and the engine
///    still answers `1`), so every caller here runs with foreign keys ON, which is exactly the
///    corner where `legacy_alter_table` cannot help. SQLite's own 12-step procedure ("turn the FKs
///    off for the rebuild") is unavailable for the same reason. The first draft of this used
///    `legacy_alter_table` and this function's own `pragma_foreign_key_check` refused it, naming
///    two dangling `credential` rows. Building the new table under the scratch name inverts the
///    problem away: the only rename left is `scratch -> {table}`, and NOTHING references the
///    scratch name, so no clause is rewritten and `credential` never stops naming `account`.
/// 2. **The indexes go with the dropped table, and the closing [`DDL`] pass is what puts them
///    back.** `account_one_account_per_book` belongs to the table being dropped, so its name is
///    free again and `CREATE … IF NOT EXISTS` re-creates it on the table that now carries the real
///    name. (⚠ This named `venue_setting`'s two partial indexes beside it until §5.2 step 7 retired
///    them; the same freeing is now what RETIRES them — the batch no longer declares them, so
///    nothing puts them back — and it is also why an OLDER binary's batch can: see
///    [`RETIRED_TIER_INDEXES`].) (Under the rename-aside
///    spelling this was a trap instead: an index follows a RENAME, keeping its name, so the
///    `IF NOT EXISTS` found the name taken, created nothing, and the drop then took the index.)
/// 3. **The column set is INTERSECTED, not assumed.** `venue_id` and `armed` reach an existing
///    store through `ALTER TABLE` (see [`DDL`]'s own doc), so the old table's columns are a subset
///    of the new one's on some stores and — for a store written by a NEWER binary and opened by
///    this one — could be a superset. `INSERT … SELECT` over the intersection needs no version
///    arithmetic. ⚠ **It is not equally CORRECT in the two directions, and this line used to claim
///    it was.** For the SUBSET direction it is correct: the columns the old table lacks are ones
///    the shipped shape can fill or leave null. For the SUPERSET direction it is silent DATA LOSS —
///    a column only the NEWER binary knows about is dropped on the floor by an older binary's
///    repair, with nothing said. No guard is built for it, deliberately: no column and no index has
///    ever been REMOVED from [`DDL`] (the whole history was read), so the superset case has never
///    occurred, and a guard over a shape nobody has produced would be a refusal written against a
///    guess. What this note buys is that whoever first removes a column knows they are the one
///    making it reachable. ⚠ **The parenthesis two sentences back is no longer true and is kept as
///    what the note said**: §9 stage 4c removed a column ([`DROPPED_COLUMNS`]) and §5.2 step 7
///    removed two indexes ([`RETIRED_TIER_INDEXES`]). Neither reaches the SUPERSET direction of
///    THIS function, which needs an older binary to REBUILD a newer table — and v0.1.34's three
///    triggers are all false on a step-7 store (no `'sim'`, `AUTOINCREMENT` present, no
///    `venue_arming.notes`), so it rebuilds nothing. What a removed INDEX does reach is different
///    and milder: an older batch re-creates it, which step 7's trigger answers. Any `id` the old
///    table has is copied EXPLICITLY, so no row changes
///    identity and `credential.account_id` still resolves; a table that never had one (`node_key`
///    before ruling 6's uniform shape) is simply handed fresh ids by the engine.
/// 4. **The `AUTOINCREMENT` high-water mark is CARRIED.** §4.1's hazard: the mark lives in a row of
///    `sqlite_sequence`, a dropped table takes its row with it, and a rebuild replaying the
///    SURVIVING rows with their ids leaves the mark at `max(id)` of what it replayed — a REWIND
///    whenever the top row had been removed, after which an id is REUSED and every reference to
///    the dead account silently names a live one. ⚠ **That carry stopped being a no-op the day
///    [`DDL`] armed these tables**, which is the same change that introduced
///    [`migrate_tables_onto_autoincrement`]; `crates/vike-secrets/tests/gates/sqlite_sequence/behaviour.rs`'s
///    `the_paper_tier_rebuild_does_not_rewind_the_account_marks` is the behaviour assertion that
///    now fails without it.
/// 5. **Rows the shipped `UNIQUE` cannot take are refused BY NAME before anything is built.**
///    [`carried_key_collisions`] asks, of the carried values, the question the copy is about to put
///    to the engine. Empty for every table but `venue_setting`, and empty there on every store this
///    tool wrote — [`migrate_venue_setting_tier_to_any`]'s doc is the proof. What it replaces is
///    the engine's own `UNIQUE constraint failed` naming the SCRATCH table's columns, which tells
///    an operator nothing they can act on. ⚠ **The refusal blocks EVERY write to the store, not
///    only a `venue_setting` one** — this runs inside `crate::db::ensure_venue_id_columns`, the
///    funnel every writer calls (credential rotation included), which is the precedent the other
///    rebuild refusals here already set. So the message is the whole of what the operator has to
///    act on, and it said *"delete all but one row per key named here and write again"* until
///    review, naming keys only in their STORED form while no shipped verb can delete a
///    `venue_setting` row. It now names each key by the credential name it answers to, lists the
///    colliding row ids, and says the repair is a SQLite client's `DELETE … WHERE id = …`. It
///    asks under BOTH keys — the number's and the text's — and says which a group collides under;
///    since the venue-links plan's second release the text one is asked only of an old table that
///    still carries the text, and [`carried_key_collisions`]' doc says why it is still asked.
/// 6. **The shipped shape's own CARRY is applied to every copy into it**
///    ([`carried_into_shipped_shape`]), outside the caller's rewrite. It is not a parameter because
///    it is not the caller's to remember: step 7's `NOT NULL` tier is violated by EVERY older
///    `venue_setting`, and on a pre-§4.4 store the pass that rebuilds that table first is
///    [`migrate_sim_tier_to_paper`], which has never heard of NULL.
/// 7. **Rows the shipped `venue_id NOT NULL` cannot take are refused BY NAME before anything is
///    built** — trap 5's twin for the venue-links flip. A row whose text `venue` names nothing the
///    `venue` table holds keeps a NULL `venue_id` through `crate::db::ensure_venue_id_columns`'
///    backfill, and copying it would fail with the engine's `NOT NULL constraint failed` naming the
///    SCRATCH table. [`unresolved_venue_links`] lists every such row in the store, by table, id and
///    venue string, and the refusal ([`unresolved_refusal`]) names the repair. It asks only while
///    the table's own `venue_id` still admits NULL and only where the shipped shape requires one,
///    so it never meets `credential` — which is why [`migrate_dropped_columns`] raises the same
///    refusal itself before it takes a text `venue` off a carried store (the venue-links plan's
///    second release) — and like trap 5 it is not the caller's to remember: on an older store the
///    first pass to rebuild a linked table is usually not [`migrate_venue_links_onto_venue_id`],
///    whose doc carries the measurement. ⚠ It blocks EVERY write, and no vike-cli verb can make
///    the repair, because every one of them writes through this funnel — `account remove`
///    included — so the message sends the operator to a SQLite client, exactly as trap 5's does.
///    ⚠ **It can also stop a daemon from STARTING**: while decision 0095's migration is pending, a
///    boot runs this funnel, and a root booted with `Ceilings::Interpret` refuses to start on this
///    refusal — passed through as `crate::DbErrorKind::RepairRefused`, so the operator reads this
///    message rather than one blaming 0095. `vike-cli config migrate-store` runs the same funnel
///    and fails the same way. [`migrate_venue_links_onto_venue_id`]'s doc carries the whole path.
///
/// ⚠ **The three refusals here — trap 5, trap 7 and the closing dangling-reference check — are
/// [`NamedRefusal`]s**, so every writer reports them as the store's repair refusing rather than as
/// a store that could not be read, and every one opens its repair the same way
/// ([`SQLITE_CLIENT_REPAIR`]: the client version, then foreign keys ON). Trap 7's offers the
/// CORRECTION before the delete, and the delete only after a row's values are copied out — a
/// `credential` row's value can be the only copy of a secret (final review M2).
///
/// The transaction is the caller's, so a failure anywhere leaves the store exactly as it was.
///
/// # Errors
/// The engine, a refusal naming carried-key collisions (trap 5), a refusal naming rows whose venue
/// the roster does not hold (trap 7), and a refusal naming every dangling reference the rebuild
/// left.
pub(super) fn rebuild_table_from_ddl(
    tx: &Transaction<'_>,
    table: &str,
    scratch_suffix: &str,
    select_expr: &dyn Fn(&str) -> Option<String>,
) -> rusqlite::Result<Rebuild> {
    let columns = table_columns(tx, table)?;
    let required = required_columns_of(table);
    let missing: Vec<String> =
        required.iter().filter(|&need| !columns.contains(need)).cloned().collect();
    if !missing.is_empty() {
        return Ok(Rebuild::Skipped { missing });
    }

    // Trap 5 — rows the shipped `UNIQUE` cannot take once carried, refused BY NAME before anything
    // is built. [`migrate_venue_setting_tier_to_any`]'s doc proves no store this code wrote holds
    // such a pair; this is the answer for one somebody edited by hand, and it is a REFUSAL rather
    // than a decline because the engine would refuse the copy anyway — only with a message naming
    // nothing an operator can act on.
    let collisions = carried_key_collisions(tx, table, select_expr)?;
    if !collisions.is_empty() {
        return Err(NamedRefusal::into_error(format!(
            "`{table}` holds rows that would share one key of the shipped UNIQUE once carried \
             onto it — {} — and picking one would hand a reader a value chosen by row order. \
             Nothing was committed, and EVERY write to this store (credentials included) is \
             refused until these rows are repaired. No store this tool wrote can hold such a \
             pair (the older shape's partial unique indexes refused it), so these rows were \
             written, or an index was dropped, outside it. No vike-cli verb deletes a \
             `{table}` row: {SQLITE_CLIENT_REPAIR}. Then keep the one row per key whose value is \
             right, remove the others with `DELETE FROM {table} WHERE id = <id>;`, and write \
             again",
            collisions.join("; ")
        )));
    }

    // Trap 7 — rows the shipped `venue_id NOT NULL` cannot take, refused BY NAME before anything is
    // built. Only for a table the shipped shape requires it on and whose own `venue_id` still
    // admits NULL, so `credential` (nullable by design) and an already-carried table never ask. The
    // list is the WHOLE store's, `credential` included, so one refusal names every row to repair
    // rather than one table's per round trip; the venue-links pass's own doc says why this lives
    // here rather than in that pass.
    if required.iter().any(|c| c == "venue_id") && column_is_nullable(tx, table, "venue_id")? {
        let unresolved = unresolved_venue_links(tx)?;
        if !unresolved.is_empty() {
            return Err(unresolved_refusal(&unresolved));
        }
    }

    let scratch = format!("{table}{scratch_suffix}");
    // Trap 4 — read BEFORE the drop, restored after the replay.
    let mark = autoincrement_mark(tx, table)?;

    // Trap 1 — the NEW table is built beside the old one under a scratch name, and the OLD one
    // keeps the real name until it is dropped.
    //
    // `defer_foreign_keys` is what makes the window legal: `DROP TABLE {table}` below performs an
    // implicit `DELETE FROM`, which fires every child row's foreign key, and the parent is only
    // put back by the rename on the next line. Deferring moves enforcement to COMMIT — and this
    // function re-checks the WHOLE database itself before returning, so nothing is merely
    // postponed. It is turned back OFF at the end, so the caller's transaction is left enforcing
    // exactly what it was.
    tx.execute_batch("PRAGMA defer_foreign_keys = ON;")?;
    // ⚠ A LEFTOVER SCRATCH TABLE IS OTHERWISE AN UNRECOVERABLE DEAD END, and this one statement is
    // the whole cure. `create_statement_under` spells a bare `CREATE TABLE`, not
    // `CREATE TABLE IF NOT EXISTS` — deliberately, because `IF NOT EXISTS` would silently REUSE a
    // stale scratch of the wrong shape and copy the rows into it. So without this drop, a scratch
    // table that ever survived would make every future credential write on that box refuse forever
    // with a bare `table account_pre_autoincrement already exists`: an error naming nothing an
    // operator can act on, on the binary that holds their venue keys. This rebuild is atomic (the
    // caller's transaction), so nothing in THIS code can leave one behind — the statement is here
    // for the states this code did not produce, which is the only kind a repair path ever meets.
    tx.execute_batch(&format!("DROP TABLE IF EXISTS {scratch};"))?;
    tx.execute_batch(&create_statement_under(table, &scratch)?)?;

    // Trap 3 — the intersection, with the caller's rewrite applied in the SELECT. Trap 6 — and the
    // shipped shape's own CARRY around it ([`carried_into_shipped_shape`]), which no caller may
    // forget because no caller spells it.
    let fresh = table_columns(tx, &scratch)?;
    let carried: Vec<&String> = columns.iter().filter(|c| fresh.contains(c)).collect();
    let selected: Vec<String> = carried
        .iter()
        .map(|c| {
            carried_into_shipped_shape(table, c, select_expr(c).unwrap_or_else(|| (*c).clone()))
        })
        .collect();
    let names: Vec<&str> = carried.iter().map(|c| c.as_str()).collect();
    tx.execute_batch(&format!(
        "INSERT INTO {scratch} ({}) SELECT {} FROM {table};",
        names.join(", "),
        selected.join(", ")
    ))?;

    // Trap 2 — the old table's NAMED INDEXES go with it, which is what makes the final `DDL` pass
    // necessary rather than decorative.
    //
    // ⚠ The drop is its OWN statement and its own call. `crates/vike-secrets/tests/
    // sqlite_sequence_gate.rs`'s `table_drops` scan classifies every table drop this crate's
    // source performs, keyed on the token that follows `DROP TABLE`, and `TABLE_DROP_PIN` carries
    // this one's row. Keeping the statement alone keeps the token the pin names readable here.
    tx.execute_batch(&format!("DROP TABLE {table};"))?;
    tx.execute_batch(&format!("ALTER TABLE {scratch} RENAME TO {table};"))?;
    tx.execute_batch(DDL)?;

    // Trap 4's other half: the replay above set the mark to `max(id)` OF WHAT IT REPLAYED, which
    // is a REWIND whenever the top row had been removed. Put back what was there.
    if let Some(seq) = mark {
        tx.execute("DELETE FROM sqlite_sequence WHERE name = ?1", [table])?;
        tx.execute("INSERT INTO sqlite_sequence (name, seq) VALUES (?1, ?2)", (table, seq))?;
    }
    tx.execute_batch("PRAGMA defer_foreign_keys = OFF;")?;

    // Deferring enforcement is not skipping it — this is [`reshape_into`]'s own closing move, and
    // it asks about the WHOLE database rather than about the statements just run. ⚠ It NAMES each
    // reference it finds: the refusal blocks every write until a human repairs the rows, and it
    // used to say only how many there were (final review M2).
    let dangling = dangling_references(tx)?;
    if !dangling.is_empty() {
        return Err(NamedRefusal::into_error(format!(
            "rebuilding `{table}` from the shipped DDL left {} dangling reference(s) — {} — rows \
             naming a row that does not exist, which only an edit made with foreign keys off \
             leaves behind. Nothing was committed, and EVERY write to this store (credentials \
             included) is refused until they are repaired. No vike-cli verb can make the repair, \
             because every one of them runs this same check: {SQLITE_CLIENT_REPAIR}. Then, for \
             each row, point it at a row that exists (`UPDATE <table> SET <column> = <id> WHERE \
             rowid = <rowid>;`), or DELETE it (`DELETE FROM <table> WHERE rowid = <rowid>;`) only \
             after copying out every value it holds: a `credential` row's value is a secret this \
             store may hold the only copy of. Then write again",
            dangling.len(),
            dangling.join("; ")
        )));
    }
    Ok(Rebuild::Done)
}

/// **Every column [`DDL`]'s `table` declares `NOT NULL` with no `DEFAULT`** — the columns a rebuild
/// must be able to fill FROM THE OLD TABLE, because the copy is over the intersection and a column
/// the old table does not have is handed nothing at all.
///
/// ⚠ **This guard has exactly one reachable subject and it is the one that matters: a schema-1
/// `credential`.** That table is `(name TEXT PRIMARY KEY, value TEXT)` — no `field` — so a rebuild
/// into the shipped shape would `INSERT` rows whose `field` is NULL against a `NOT NULL` column
/// and abort the operator's whole write. [`crate::db::fill_into`] runs [`reshape_into`] before it
/// reaches here, and the four other callers of [`crate::db::ensure_venue_id_columns`] already fail
/// on such a store for a separate PRE-EXISTING reason (their own `DDL` batch's
/// `credential_one_live_value` index, `no such column: account_id`) — so nothing reaches this
/// guard today. It is written because the alternative is a repair step whose failure mode on an
/// old store is *the credential write was refused*, and a repair that cannot run must decline
/// rather than abort.
///
/// It is DERIVED from the shipped batch rather than written down, so a new `NOT NULL` column is
/// covered by the edit that adds it.
pub(super) fn required_columns_of(table: &str) -> Vec<String> {
    ddl_column_decls(table)
        .into_iter()
        .filter(|(_, decl)| decl.contains("NOT NULL") && !decl.contains("DEFAULT"))
        .map(|(name, _)| name)
        .collect()
}

/// `(column name, the rest of its declaration)` for every COLUMN of [`DDL`]'s `table`.
///
/// Table-level constraints are NOT columns and are dropped: an entry whose first word is one of
/// [`TABLE_CONSTRAINT_WORDS`] is skipped. Entries are split on commas at PAREN DEPTH ZERO, so the
/// commas inside `CHECK (tier IN ('paper', 'demo', 'live'))` and `UNIQUE (venue, tier, label)` do
/// not split anything.
pub(super) fn ddl_column_decls(table: &str) -> Vec<(String, String)> {
    let head = format!("CREATE TABLE IF NOT EXISTS {table} (");
    let Some(at) = DDL.find(&head) else { return Vec::new() };
    let rest = &DDL[at + head.len()..];
    let Some(end) = rest.find(") STRICT") else { return Vec::new() };

    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut entry = String::new();
    for c in rest[..end].chars() {
        match c {
            '(' => {
                depth += 1;
                entry.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                entry.push(c);
            }
            ',' if depth == 0 => {
                push_column_decl(&mut out, &entry);
                entry.clear();
            }
            _ => entry.push(c),
        }
    }
    push_column_decl(&mut out, &entry);
    out
}

/// The words that open a TABLE-level constraint rather than a column.
const TABLE_CONSTRAINT_WORDS: [&str; 5] = ["CHECK", "UNIQUE", "PRIMARY", "FOREIGN", "CONSTRAINT"];

fn push_column_decl(out: &mut Vec<(String, String)>, entry: &str) {
    let entry = entry.trim();
    let Some((name, decl)) = entry.split_once(char::is_whitespace) else { return };
    if TABLE_CONSTRAINT_WORDS.contains(&name.to_uppercase().as_str()) {
        return;
    }
    out.push((name.to_string(), decl.trim().to_string()));
}

/// One table's `CREATE TABLE` text as the engine stored it, or `None` when the table is absent.
pub(super) fn table_sql(tx: &Transaction<'_>, table: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1", [table], |r| {
        r.get::<_, Option<String>>(0)
    })
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

/// One table's column names, in declaration order.
pub(super) fn table_columns(tx: &Transaction<'_>, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    rows.collect()
}

/// **[`DDL`]'s `CREATE TABLE` for `table`, re-pointed at `under`** — the statement that builds the
/// new shape beside the old one.
///
/// The head is rewritten and the BODY is taken verbatim, so the rebuilt table is [`DDL`]'s own
/// definition rather than a second spelling of it: a column added to the batch reaches a migrated
/// store through this function with no edit here. A head that cannot be found is an ERROR rather
/// than a skip — it would mean this function and [`RETIER_TABLES`] disagree about what the batch
/// contains, and the quiet version of that is a store left on the old vocabulary.
pub(super) fn create_statement_under(table: &str, under: &str) -> rusqlite::Result<String> {
    let head = format!("CREATE TABLE IF NOT EXISTS {table} (");
    let Some(at) = DDL.find(&head) else {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!("the shipped DDL declares no `{head}…` to rebuild `{table}` from")),
        ));
    };
    let rest = &DDL[at + head.len()..];
    let Some(end) = rest.find(";") else {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!("the shipped DDL's `{table}` statement is unterminated")),
        ));
    };
    Ok(format!("CREATE TABLE {under} ({};", &rest[..end]))
}

/// One table's `AUTOINCREMENT` high-water mark, or `None` when it declares none.
///
/// `sqlite_sequence` does not exist at all in a database with no `AUTOINCREMENT` table anywhere,
/// so its absence is asked of `sqlite_master` first rather than allowed to arrive as a `no such
/// table` error that would be indistinguishable from a real one.
fn autoincrement_mark(tx: &Transaction<'_>, table: &str) -> rusqlite::Result<Option<i64>> {
    let present: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence'",
        [],
        |r| r.get(0),
    )?;
    if present == 0 {
        return Ok(None);
    }
    tx.query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| r.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
}
