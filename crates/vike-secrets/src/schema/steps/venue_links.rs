//! Ruling 3: the venue links become `venue_id` (the venue-links plan's first release), and `VenueLink`, the one spelling of reading and writing one.

use rusqlite::Transaction;

use super::any_tier::column_is_nullable;
use crate::schema::rebuild::{
    NamedRefusal, SQLITE_CLIENT_REPAIR, rebuild_table_from_ddl, table_columns,
};

// ---------------------------------------------------------------------------------------------
// Ruling 3 — the venue links become `venue_id` (the venue-links plan's first release)
// ---------------------------------------------------------------------------------------------

/// The tables whose venue link this pass rebuilds, in rebuild order. `credential` is absent: its
/// `venue_id` stays nullable in this release (account-scoped and infrastructure rows carry none),
/// so its shape does not change. `crate::venue_links::apply_venue_links`' read-only probe asks the
/// same list, so the probe and the pass cannot disagree about what a carry is.
pub(crate) const VENUE_LINK_TABLES: [&str; 3] = ["account", "venue_setting", "venue_arming"];

/// The four tables a text `venue` column links to the roster.
const VENUE_LINKED_TABLES: [&str; 4] = ["account", "credential", "venue_setting", "venue_arming"];

/// Suffix for the scratch name this pass's rebuild builds the new shape under.
///
/// Distinct from the four above for the reason [`AUTOINCREMENT_SCRATCH_SUFFIX`] gives about
/// [`RETIER_SCRATCH_SUFFIX`]: every rebuild runs in one transaction over overlapping tables, and a
/// shared scratch name makes a failure of one look like a leftover of another.
const VENUE_LINK_SCRATCH_SUFFIX: &str = "_pre_venue_link";

/// **Rebuild every table whose venue link is still a nullable `venue_id` onto the shipped shape,
/// where `venue_id` is `NOT NULL` and every venue uniqueness keys on it.** IDEMPOTENT.
///
/// The trigger is the SHAPE, asked of the engine, exactly as stage 4a's and step 7's passes ask:
/// a table whose `venue_id` admits NULL has not been carried. Once carried, the trigger is false
/// forever, and an older binary's batch cannot make it true again. `CREATE TABLE IF NOT EXISTS`
/// does not touch a table that exists, and the three re-keyed indexes keep their names, so an older
/// `CREATE … IF NOT EXISTS` finds the name taken and creates nothing. (An older binary's own
/// REBUILD can make it true again — step 7's pass in a binary that predates this one, after a
/// still older batch put the retired tier indexes back — and then this pass carries that table
/// again on the next write. The trigger is the shape, so nothing has to remember it.)
///
/// ⚠ **It needs `crate::db::ensure_venue_id_columns`' `ALTER` and backfill to have run, and so does
/// every other pass in that funnel.** Only after the backfill does every row whose text venue is on
/// the roster carry its `venue_id`, so a row still NULL names a venue the `venue` table does not
/// hold. Copying it would fail with the engine's `NOT NULL constraint failed` on a scratch table,
/// which names nothing an operator can act on, so [`rebuild_table_from_ddl`] refuses the write
/// naming it — its trap 7, built on [`unresolved_venue_links`].
///
/// ⚠ **Neither the refusal nor the backfill is spelled in this function, and that is the point.**
/// Every pass that rebuilds one of these tables copies into the same shipped shape, whose
/// `venue_id` is `NOT NULL` now, and on an older store the first such pass is usually NOT this one:
/// [`migrate_sim_tier_to_paper`] on a store still holding `'sim'`,
/// [`migrate_tables_onto_autoincrement`] on one predating §4.1,
/// [`migrate_venue_setting_tier_to_any`] on one predating step 7. ([`migrate_dropped_columns`], on
/// one still carrying `venue_arming.notes`, was in that list until the venue-links plan's second
/// release moved it below this pass.) MEASURED with the backfill still below those passes:
/// `crates/vike-secrets/tests/migration/paper_tier.rs` failed `NOT NULL constraint failed:
/// account_pre_paper_tier.venue_id` on a store whose every row the backfill would have filled. So
/// the backfill runs FIRST in the funnel, and the refusal lives in the rebuild, where no pass can
/// skip it — the same reasoning that put step 7's NULL carry there
/// ([`carried_into_shipped_shape`]).
///
/// # ⚠ A BOOT can run this pass, and its refusal then stops a daemon from starting
///
/// The funnel is not only reached by writes an operator asks for. While decision 0095's ceiling
/// migration is PENDING on a store, `crate::live_means_mainnet::apply_live_means_mainnet` runs it
/// and then the whole of `crate::db::ensure_venue_id_columns` in the same transaction — this pass
/// and trap 7 included — and two callers reach it:
///
/// * **every booting root that reads the ceilings** (`vike_boot`'s step 3½). A root booted with
///   `Ceilings::Interpret` — the daemon, `vike-backend venues` — that meets trap 7's refusal
///   REFUSES TO START; one booted with `Ceilings::InterpretOrMark` (`vike-cli`, the desktop)
///   carries it as a mark. The refusal reaches the root as `crate::DbErrorKind::RepairRefused`,
///   so `crates/vike-boot/src/lib.rs`'s `ceiling_migration_refusal` passes trap 7's own message
///   through, saying that this is not decision 0095 failing and that `vike-cli config
///   migrate-store` is not the repair. (⚠ Until the venue-links plan's reader task, every failure
///   on that path was wrapped as *"this box's settings store predates decision 0095"*, naming
///   `vike-cli config migrate-store` as the repair, with trap 7's message in its parentheses.);
/// * **`vike-cli config migrate-store` itself** (`crates/vike-cli/src/cmd/config/migrate.rs`),
///   which calls the same function and so fails the same way — and which, after decision 0095's
///   step, carries the store on purpose through `crate::venue_links::apply_venue_links`, meeting
///   this same refusal there. Its `refusal` reports either one without the *run this as the user
///   that owns the store* hint it gives every other failure: that hint was appended to this
///   refusal too until the venue-links plan's final fix wave, and the message before it
///   contradicted it by naming that very verb among the ones that cannot make the repair.
///
/// Neither repairs anything: the repair is the SQLite-client one trap 7's message names. ⚠ **The
/// same path makes merely STARTING a binary that carries this pass a WRITE**: started against a
/// store whose 0095 migration is pending, it migrates that store, so a lane-built daemon pointed
/// at a live store writes it exactly as a lane-built `vike-cli` does.
///
/// # Errors
/// The engine, and [`rebuild_table_from_ddl`]'s own refusals (trap 7 above among them). All of
/// them leave the caller's transaction uncommitted, so the store is exactly as it was.
pub(crate) fn migrate_venue_links_onto_venue_id(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut due: Vec<&str> = Vec::new();
    for table in VENUE_LINK_TABLES {
        if column_is_nullable(tx, table, "venue_id")? {
            due.push(table);
        }
    }
    for table in due {
        // ⚠ The decline is NAMED rather than being a bare `continue` — see `Rebuild::decline_note`,
        // and the same ⚠ at `migrate_sim_tier_to_paper`'s call site. Unreachable here while the
        // funnel adds `venue_id` before any rebuild: see `Rebuild::Skipped`.
        let rebuilt = rebuild_table_from_ddl(tx, table, VENUE_LINK_SCRATCH_SUFFIX, &|_| None)?;
        if rebuilt.decline_note(table).is_some() {
            continue;
        }
        // The POSITIVE check every migration in this file performs, asked of the NEW shape by the
        // engine rather than of the statement text.
        if column_is_nullable(tx, table, "venue_id")? {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "the rebuilt `{table}` still lets `venue_id` be NULL; nothing was committed"
                )),
            ));
        }
    }
    Ok(())
}

/// Every row, in every linked table that still HAS a text `venue`, whose text venue is set but
/// whose `venue_id` is not. After the backfill, those are exactly the rows naming a venue that the
/// `venue` table does not hold. Rendered `table id N (venue 'x')`, in table order and then id
/// order.
pub(crate) fn unresolved_venue_links(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    let mut out = Vec::new();
    for table in VENUE_LINKED_TABLES {
        let columns = table_columns(tx, table)?;
        if !columns.iter().any(|c| c == "venue") || !columns.iter().any(|c| c == "venue_id") {
            continue;
        }
        let mut stmt = tx.prepare(&format!(
            "SELECT id, venue FROM {table} WHERE venue IS NOT NULL AND venue_id IS NULL ORDER BY id"
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, venue) = row?;
            out.push(format!("`{table}` id {id} (venue '{venue}')"));
        }
    }
    Ok(out)
}

/// **The refusal for rows whose text venue has no number** — `rows` as [`unresolved_venue_links`]
/// renders them, inside a [`NamedRefusal`], so every writer reports it as
/// `crate::DbErrorKind::RepairRefused`.
///
/// ⚠ ONE spelling, two callers: [`rebuild_table_from_ddl`]'s trap 7, for a table whose `venue_id`
/// still admits NULL, and [`migrate_dropped_columns`], before it takes a text `venue` off a store
/// trap 7 is never asked of. It was trap 7's inline text until the venue-links plan's second release
/// needed it twice.
pub(crate) fn unresolved_refusal(rows: &[String]) -> rusqlite::Error {
    NamedRefusal::into_error(format!(
        "rows name a venue the store's roster does not hold — {} — so their link to a venue \
         cannot become a number. Nothing was committed, and EVERY write to this store \
         (credentials included) is refused until they are repaired; a daemon that boots while \
         decision 0095's migration is pending refuses to start on it. No vike-cli verb can make \
         the repair, because every one of them runs this same check (`vike-cli secrets account \
         remove` and `vike-cli config migrate-store` included): {SQLITE_CLIENT_REPAIR}. Then, for \
         each row, CORRECT its venue where only the spelling is wrong (`UPDATE <table> SET venue = \
         '<venue>' WHERE id = <id>;`), unless another row of that table already holds that venue \
         for the same tier and label (or field): the engine does not refuse every such duplicate, \
         and for an unlabelled `account` a second one at one venue and tier makes every later \
         credential write for that tier refuse as ambiguous. DELETE a row (`DELETE FROM <table> \
         WHERE id = <id>;`) only when it should not exist at all, and only after copying out \
         every value it holds: a `credential` row's value is a secret this store may hold the only \
         copy of, and an `account` row goes only after every `credential` row naming it in \
         `account_id` and every `account` row naming it in `parent_id`, which the foreign keys \
         turned on above enforce. Then write again",
        rows.join("; ")
    ))
}

/// Every row the WHOLE database holds that names a row which does not exist, from the engine's own
/// `pragma_foreign_key_check`, rendered `` `table` row N (its `parent` row is missing) `` in table
/// and then rowid order — the listing [`rebuild_table_from_ddl`]'s last refusal names, so the repair
/// starts from the rows rather than from a count.
pub(crate) fn dangling_references(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        tx.prepare("SELECT \"table\", rowid, parent FROM pragma_foreign_key_check ORDER BY 1, 2")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, String>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (table, rowid, parent) = row?;
        let row = rowid.map_or_else(|| "a row".to_string(), |id| format!("row {id}"));
        out.push(format!("`{table}` {row} (its `{parent}` row is missing)"));
    }
    Ok(out)
}

/// **How one table's link to a venue is READ and WRITTEN on this store** — the one spelling every
/// statement in this crate uses, so the move from text to number happened in one place and the
/// second release's contraction happens in one place too.
///
/// Three shapes exist in the field:
///
/// | the table has | reads | writes |
/// |---|---|---|
/// | `venue_id` and the text `venue` | the number, through `venue`; the text only for a row whose number names no `venue` row | both |
/// | `venue_id` only | the number | the number |
/// | the text `venue` only (a backup older than stage 2) | the text: there is no number to read | never reached — the funnel carries the store forward before any writer runs |
///
/// (Since the second release: `account`, `credential`, `venue_setting` hold the second row's shape;
/// `venue_arming` holds the first until Plan B deletes it.) The first row is also what every linked
/// table of a store the second release has not yet carried still holds, which is why no statement
/// here assumes a shape: each asks [`VenueLink::of`] for the one the store has.
///
/// ⚠ **The text fallback is not a second source.** A row reaches it only when its `venue_id` names
/// no `venue` row: a NULL, which a carried store cannot hold, since `venue_id` is `NOT NULL` there,
/// or a number left dangling by a hand edit with foreign keys off, which the LEFT join below keeps
/// (⚠ this said "only when its `venue_id` is NULL" until the venue-links plan's final fix wave,
/// which is the case a carried store cannot reach and not the only one). It exists for the
/// statements that can meet a store no writer has carried yet, where the text is the only answer
/// the store holds for that row: a read-only reader; decision 0095's ceiling migration, which a
/// boot runs on the store AS FOUND before the funnel (`crate::live_means_mainnet`'s module doc);
/// and `crate::db::set_venue_account_id`, which writes one row and runs no funnel at all. Where
/// one of them FILTERS by venue (that last one's book-holder check) it uses [`VenueLink::named`];
/// a statement that runs after the funnel filters with [`venue_is`], on the number alone.
///
/// ⚠ **The join is a LEFT join in both shapes that carry a number**, where the plan's first
/// spelling had an inner one for the second release. An inner join DROPS a row whose number is
/// NULL or names no `venue` row, and `credential.venue_id` is NULL by design on every
/// account-scoped row: a reader that ever read `credential` through this link would lose those rows
/// without a word, and an arming row lost that way is a `max_exposure` figure lost, which means
/// UNBOUNDED. A LEFT join keeps the row and hands its reader a NULL name, which a `String` column
/// refuses loudly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VenueLink {
    /// A SELECT expression naming the venue of the aliased row, e.g. `COALESCE(v.name, a.venue)`.
    pub name: String,
    /// The join clause that expression needs; empty when it needs none.
    pub join: String,
    /// The INSERT column list for the link: `venue, venue_id` or `venue_id`.
    pub columns: &'static str,
    has_text: bool,
}

impl VenueLink {
    /// Ask the store which shape `table` has. `alias` is the alias the caller's statement gives
    /// the table (`a`, `t`, …), and the venue join uses `v`.
    pub fn of(
        conn: &rusqlite::Connection,
        table: &str,
        alias: &str,
    ) -> rusqlite::Result<VenueLink> {
        let has_id = crate::settings::has_column(conn, table, "venue_id")?;
        let has_text = crate::settings::has_column(conn, table, "venue")?;
        let by_number = format!("LEFT JOIN venue v ON v.id = {alias}.venue_id");
        let (name, join) = match (has_id, has_text) {
            (true, true) => (format!("COALESCE(v.name, {alias}.venue)"), by_number),
            (true, false) => ("v.name".to_string(), by_number),
            (false, _) => (format!("{alias}.venue"), String::new()),
        };
        let columns = if has_text { "venue, venue_id" } else { "venue_id" };
        Ok(VenueLink { name, join, columns, has_text })
    }

    /// The VALUES fragment matching [`VenueLink::columns`] for a venue NAME bound at `param`
    /// (`?1`, …). The number is looked up in the same statement, so a writer never holds a
    /// `venue_id` it could get wrong.
    pub fn values(&self, param: &str) -> String {
        let id = format!("(SELECT id FROM venue WHERE name = {param})");
        if self.has_text { format!("{param}, {id}") } else { id }
    }

    /// The filter for rows of the venue NAMED at `param`, for a statement that can meet a store no
    /// writer has carried (the type's doc names the three). It compares the very expression
    /// [`VenueLink::name`] selects, so it finds exactly the rows a reader names that venue: by the
    /// number where a row carries one, by the text only where it does not. The statement must
    /// carry [`VenueLink::join`].
    pub fn named(&self, param: &str) -> String {
        format!("{} = {param}", self.name)
    }
}

/// The filter a statement uses to find rows of ONE venue by its name bound at `param`, keyed on the
/// number. `column` is the table's `venue_id` column as the statement spells it.
///
/// ⚠ Only for a statement that runs AFTER the funnel (`crate::db::ensure_venue_id_columns`) in its
/// own transaction, where the column exists and holds a number on every row of `account`,
/// `venue_setting` and `venue_arming`. A statement that can meet an uncarried store filters with
/// [`VenueLink::named`] instead.
pub(crate) fn venue_is(column: &str, param: &str) -> String {
    format!("{column} = (SELECT id FROM venue WHERE name = {param})")
}
