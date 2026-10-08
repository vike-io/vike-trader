//! §4.4: the `sim` -> `paper` value AND constraint migration.

use rusqlite::Transaction;

use crate::schema::rebuild::{rebuild_table_from_ddl, table_sql};
use crate::schema::tiers::{PAPER_TIER, SIM_TIER_WORD};

// ---------------------------------------------------------------------------------------------
// §4.4 — the `sim` -> `paper` value AND constraint migration
// ---------------------------------------------------------------------------------------------

/// The two tables whose `tier` column carried the old word. Ordered for readability only — each is
/// rebuilt independently and nothing links them.
const RETIER_TABLES: [&str; 2] = ["account", "venue_setting"];

/// Suffix for the scratch name a table is renamed aside to while it is rebuilt.
const RETIER_SCRATCH_SUFFIX: &str = "_pre_paper_tier";

/// **Carry an EXISTING store onto [`ACCOUNT_TIERS`]' `paper`** — the rows AND the `CHECK` that
/// refuses them, in one idempotent step. A no-op on a store born after the rename.
///
/// # Why this is not an `UPDATE`
///
/// An `UPDATE account SET tier = 'paper' WHERE tier = 'sim'` runs against the table's OWN check
/// constraint, which on an already-migrated store still reads `CHECK (tier IN ('sim','demo',
/// 'live'))` — [`DDL`] is `CREATE TABLE IF NOT EXISTS` and changes nothing about a table that is
/// already there. So the `UPDATE` is refused, and so is every later credential write that mints a
/// `paper` row. SQLite has no `ALTER TABLE … DROP CONSTRAINT`; the documented cure is a rebuild,
/// which is what [`reshape_into`] already does for its own reason (*"the rebuild is a COPY,
/// because `ALTER TABLE` cannot do it"*). Rewriting the rows and replacing the constraint are
/// therefore the SAME act, and doing one without the other leaves a store that either refuses its
/// own vocabulary or holds a word nothing reads.
///
/// # What it does
///
/// For each of [`RETIER_TABLES`] whose `sqlite_master` entry still names the old word, it calls
/// [`rebuild_table_from_ddl`] with a SELECT rewrite that maps the value, then asserts the NEW
/// vocabulary is in the rebuilt constraint.
///
/// ⚠ **The four traps that rebuild is written around are documented THERE, not here** — this
/// function used to carry them and stage 4's `AUTOINCREMENT` needed the identical procedure, so
/// the body moved rather than being spelled a second time. Trap 4 is the one whose meaning changed
/// with that move: the mark carry was a no-op while no table declared `AUTOINCREMENT`, and
/// [`migrate_tables_onto_autoincrement`] is what made it load-bearing.
///
/// The transaction is the caller's, so a failure anywhere leaves the store exactly as it was.
///
/// # Errors
/// The engine.
pub(crate) fn migrate_sim_tier_to_paper(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for table in RETIER_TABLES {
        let Some(sql) = table_sql(tx, table)? else { continue };
        if !sql.contains(&format!("'{SIM_TIER_WORD}'")) {
            continue;
        }
        // Traps 1–4 all live in [`rebuild_table_from_ddl`], which is the one spelling of this
        // procedure. The only thing peculiar to §4.4 is the SELECT rewrite below: `tier` is
        // rewritten IN the copy rather than afterwards, because an `UPDATE` after the copy would
        // run against the new CHECK the copy just passed.
        let rebuilt = rebuild_table_from_ddl(tx, table, RETIER_SCRATCH_SUFFIX, &|column| {
            (column == "tier").then(|| {
                format!("CASE tier WHEN '{SIM_TIER_WORD}' THEN '{PAPER_TIER}' ELSE tier END")
            })
        })?;
        if rebuilt.decline_note(table).is_some() {
            // ⚠ A DECLINE IS NOT NOTHING-TO-DO, and this `continue` is where the difference is
            // still lost. The store keeps its `'sim'` CHECK and refuses every later `paper` write
            // with nothing naming the cause. `Rebuild::decline_note` renders that cause and its
            // doc names the one change owed — a warnings channel out of `crate::db::
            // ensure_venue_id_columns`. ⚠ Do NOT "fix" this by returning an `Err`:
            // `required_columns_of`'s doc rules that out, and it would take the operator's
            // credential write down with the repair.
            continue;
        }

        // ⚠ The POSITIVE check, and it is the one this file's own programme insists on: assert the
        // NEW vocabulary is in the shipped constraint, never merely that the old one is gone. A
        // rebuild that produced a table with no CHECK at all would pass the second and fail this.
        let rebuilt = table_sql(tx, table)?.unwrap_or_default();
        if !rebuilt.contains(&format!("'{PAPER_TIER}'")) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "the rebuilt `{table}` does not constrain `tier` against '{PAPER_TIER}'; \
                     nothing was committed"
                )),
            ));
        }
    }
    Ok(())
}
