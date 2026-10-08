//! §4.1: `AUTOINCREMENT`, and the uniform `id` shape (ruling 6).

use rusqlite::Transaction;

use crate::schema::DDL;
use crate::schema::rebuild::{rebuild_table_from_ddl, table_sql};

// ---------------------------------------------------------------------------------------------
// §4.1 — `AUTOINCREMENT`, and the uniform `id` shape (ruling 6)
// ---------------------------------------------------------------------------------------------

/// Suffix for the scratch name §4.1's rebuild builds the new shape under.
///
/// Distinct from [`RETIER_SCRATCH_SUFFIX`] deliberately: the two rebuilds run in the same
/// transaction on the same tables, and a shared scratch name would make a failure of one look like
/// a leftover of the other.
const AUTOINCREMENT_SCRATCH_SUFFIX: &str = "_pre_autoincrement";

/// **Carry an EXISTING store onto ruling 6's uniform `id` shape** — `id INTEGER PRIMARY KEY
/// AUTOINCREMENT` on every table [`DDL`] arms, and `node_key`'s surrogate `id` beside the `name`
/// that used to be its PRIMARY KEY. A no-op on a store born after the rebuild, and on any table
/// that already declares it.
///
/// # What it buys, in the spec's own words
///
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §2.4: there WAS no
/// `AUTOINCREMENT` anywhere, so SQLite hands a new row `max(rowid) + 1`, and
/// [`crate::db::edit_account`]'s `DELETE` frees the top id — *"remove account 16, add an account,
/// and the new one IS account 16."* §4.1 rules the cure is the ENGINE's job rather than a number
/// computed in application code, and notes what makes it a REBUILD rather than an `ALTER`:
/// ⚠ **`AUTOINCREMENT` cannot be added by `ALTER TABLE` at all.** (The past tense is load-bearing
/// and was a present tense until this was swept: it describes the PRE-state of the store this
/// function repairs, which is what the heading and the summary above both frame, and stage 4a has
/// landed — that spec section's own heading now ends *"— CLOSED by stage 4a"*.)
///
/// # ⚠ Why this is not gated on [`crate::SCHEMA_VERSION`]
///
/// For the reason [`DDL`]'s own doc has recorded since the four post-freeze tables landed, and
/// which Task 5 re-ruled for `account.armed`: [`crate::READABLE_SCHEMA_VERSIONS`] is
/// `[1, SCHEMA_VERSION]`, so a bump to 3 silently DROPS 2 — the version both live boxes hold — and
/// their credential stores read as unreadable, which is every venue on paper with nothing
/// erroring. So the trigger is the SHAPE the store actually has, asked of `sqlite_master`, exactly
/// as [`migrate_sim_tier_to_paper`] asks whether the old tier word is still in the CHECK.
/// [`crate::ACCOUNT_TABLE_SCHEMA`] does not move either — it answers *is this store old enough
/// that the `account` table does not exist*, which an `id` column's shape does not change.
///
/// # ⚠ What makes the mark CORRECT on this particular rebuild
///
/// §4.1 again: *"On THIS migration there is no prior mark to lose (the constraint is being
/// introduced), so copying rows with their ids sets the sequence to `max(id)`, which is correct."*
/// [`rebuild_table_from_ddl`] reads the mark before and restores it after regardless, which is
/// `None` here and a no-op — and is the statement that stops being a no-op for every FUTURE
/// rebuild of these tables, including [`migrate_sim_tier_to_paper`]'s, now that they are armed.
/// `crates/vike-secrets/tests/gates/sqlite_sequence/mod.rs` is the gate.
///
/// # ⚠ It no longer runs [`migrate_dropped_columns`], and the move is the point
///
/// §9 stage 4c's delivery pass used to be this function's closing statement, because this was the
/// one schema-owned entry `crate::db::ensure_venue_id_columns` already called and the task that
/// wrote it did not own `db.rs`. It then sat BESIDE this call in that funnel, one line after it,
/// which is where both docs always said it belonged. (Since the venue-links plan's second release
/// it sits further down, after the venue-links pass — [`migrate_dropped_columns`]' doc says why —
/// and still after this one.)
///
/// Worth keeping rather than deleting, because the nesting was actively misleading: the two
/// repairs have DIFFERENT triggers and different lifetimes. This one's goes false FOREVER once a
/// store has been carried, and it never visits `venue_arming` at all; that one's is *the store's
/// table still HAS the column*, which stays true afterwards. Nested, the second reads as a phase
/// of the first — and the next author who deletes this pass on the grounds that it is spent takes
/// the other one with it.
///
/// # Errors
/// The engine and [`rebuild_table_from_ddl`]'s own refusals.
pub(crate) fn migrate_tables_onto_autoincrement(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    for table in autoincrement_tables() {
        let Some(sql) = table_sql(tx, &table)? else { continue };
        if sql.to_uppercase().contains("AUTOINCREMENT") {
            continue;
        }
        // ⚠ The decline is NAMED rather than being a bare `continue` — see
        // `Rebuild::decline_note`, and the same ⚠ at `migrate_sim_tier_to_paper`'s call site.
        let rebuilt = rebuild_table_from_ddl(tx, &table, AUTOINCREMENT_SCRATCH_SUFFIX, &|_| None)?;
        if rebuilt.decline_note(&table).is_some() {
            continue;
        }

        // The POSITIVE check, the same one §4.4's rebuild performs against its own vocabulary:
        // assert the NEW shape is on the table, never merely that the old one is gone. A rebuild
        // that silently produced the old body would pass every other statement here.
        let rebuilt = table_sql(tx, &table)?.unwrap_or_default();
        if !rebuilt.to_uppercase().contains("AUTOINCREMENT") {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "the rebuilt `{table}` does not declare AUTOINCREMENT, so its `id` is still \
                     reusable; nothing was committed"
                )),
            ));
        }
    }

    // ⚠ [`migrate_dropped_columns`] USED TO BE CALLED HERE and is now called below this function
    // in `crate::db::ensure_venue_id_columns`, which is the funnel both repairs belong to and what
    // both docs always named as its home — after the venue-links pass, since the venue-links plan's
    // second release. It must stay AFTER this one: a table the loop above rebuilt has already lost
    // a dropped column through the intersection, so that pass then finds nothing to do rather than
    // rebuilding it twice.
    Ok(())
}

/// Every table the shipped [`DDL`] arms with `AUTOINCREMENT`, DERIVED from the batch rather than
/// written down — so a table that joins the uniform shape joins this migration by the same edit
/// that arms it, and one that leaves it stops being rebuilt without a second edit here.
///
/// ⚠ The two tables deliberately NOT in the answer are not in it because [`DDL`] does not arm them,
/// and each has its own reason recorded at its `CREATE TABLE`: `settings_adoption` is ruling 6's
/// one stated exception (*"a singleton whose `id` is a seal rather than a surrogate"*), and
/// `venue_arming` is a table §3 spells DELETED, which a surrogate key would be work spent on
/// something scheduled to go.
pub(crate) fn autoincrement_tables() -> Vec<String> {
    let mut out = Vec::new();
    for chunk in DDL.split("CREATE TABLE IF NOT EXISTS ").skip(1) {
        let name: String = chunk.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let Some(end) = chunk.find(") STRICT") else { continue };
        if chunk[..end].to_uppercase().contains("AUTOINCREMENT") {
            out.push(name);
        }
    }
    out
}
