//! §9 stage 4c: a column that LEFT the batch, delivered to a store that still carries it.

use rusqlite::Transaction;

use super::venue_links::{unresolved_refusal, unresolved_venue_links};
use crate::schema::rebuild::{rebuild_table_from_ddl, table_columns};

// ---------------------------------------------------------------------------------------------
// §9 stage 4c — a column that LEFT the batch, delivered to a store that still carries it
// ---------------------------------------------------------------------------------------------

/// Suffix for the scratch name a table is rebuilt under when a dropped column is being taken off
/// it.
///
/// Distinct from the two above for the reason [`AUTOINCREMENT_SCRATCH_SUFFIX`] gives about
/// [`RETIER_SCRATCH_SUFFIX`]: all three rebuilds run in one transaction over overlapping tables,
/// and a shared scratch name makes a failure of one look like a leftover of another.
const DEAD_COLUMN_SCRATCH_SUFFIX: &str = "_pre_column_drop";

/// **Every column stage 4c took OUT of [`DDL`], as `(table, column, the measurement that proved it
/// dead)`.**
///
/// ⚠ **A row here is a licence to DESTROY data on a live box**, so the bar is the one §9 stage 4c
/// sets and [`DDL`]'s own *dead columns* section spells per candidate: **zero readers AND zero
/// writers across the whole tree**. A column whose only writer is [`write_rows`] is WRITE-ONLY and
/// does not qualify — it has a value on disk that nothing would put back.
///
/// The set is deliberately NOT derived by comparing the shipped batch against a store: that
/// comparison also answers YES for a column a NEWER binary added, and an older binary running that
/// rule would delete it on the next write. Only a column this batch deliberately dropped may be
/// dropped, so it is named.
///
/// This module's own `the_dropped_columns_are_absent_from_the_batch` is the anti-vacuity half:
/// every row must name a table [`DDL`] still declares and a column it does NOT, so a column put
/// back into the batch reddens rather than being silently deleted from every store on the next
/// write.
///
/// # ⚠ A row's `why` may not spell the name of the mirror writer, and that is a GATE
///
/// `crates/vike-ops/tests/settings/settings_row_writer_gate.rs` pins **which files may write a settings
/// ROW**, and it measures by looking for that writer's identifier in the COMMENT-STRIPPED source
/// of every `src/` file. A doc comment is stripped; **a string literal is not**. So the
/// measurement behind the one row below is written HERE, in the doc, and the row itself carries the
/// short form — spelling the identifier inside the `why` string reports this file as an unpinned
/// writer of the live ceilings, which is the same shape as the rule against writing a whole
/// credential-key literal in a `src/` file. MEASURED: it did, on the first push.
///
/// **`venue_arming.notes`, re-measured 2026-09-23.** The mirror writer in
/// `crates/vike-secrets/src/settings.rs` INSERTs `(venue, venue_id, label, mode, max_exposure)`;
/// the three statements that READ that table select `(venue, label, mode[, max_exposure])`; and no
/// statement anywhere else in the workspace touches the table at all — it is confined to that one
/// file. The same writer DELETEs and re-inserts every row on each mirror, so there is no historical
/// value to lose either. It is also the one `notes` column §3 does not declare, because §3 spells
/// the whole TABLE deleted — which is why this column can go and its six siblings cannot.
///
/// # ⚠ The three text `venue` columns meet a DIFFERENT bar, and it is stated rather than stretched
///
/// The venue-links plan's second release dropped `account.venue`, `credential.venue` and
/// `venue_setting.venue`. They were not dead: every writer filled them through the first release,
/// and they were READ where a row's number is missing — the funnel's backfill derives the number
/// from them, trap 7 names the rows it could not number, [`VenueLink`]'s fallback answers for a row
/// whose `venue_id` names no `venue` row, and trap 5 groups `venue_setting` by them as well as by
/// the number. What makes them droppable is that they are REDUNDANT: a copy of the venue `venue_id`
/// names, which every reader answers from while the number names one. Two things carry that from
/// a claim to a guarantee. On the three tables whose `venue_id` is `NOT NULL` since the first
/// release, every row's number names a `venue` row (the foreign key). On `credential`, whose
/// `venue_id` stays nullable, a venue-scoped row filed for a venue the roster lacks keeps its text
/// and a NULL number, so [`migrate_dropped_columns`] refuses BY NAME, before it rebuilds anything,
/// while any such row exists ([`unresolved_venue_links`], with trap 7's own message). A row whose
/// text DISAGREES with its number (only a hand edit makes one) loses the disagreeing spelling, which
/// no reader consulted while the number named a venue. the CI box's store held no such row at the first
/// release's dry run (the design's as-built block for that release: every row with a text venue
/// carried its number, none disagreeing with it).
pub(crate) const DROPPED_COLUMNS: [(&str, &str, &str); 4] = [
    (
        "venue_arming",
        "notes",
        "§2.7, re-measured 2026-09-23: zero writers, zero readers, and the table is confined to \
         one file — the measurement is at this table's own doc, which is where it can name the \
         mirror writer without this file being reported as one. §3 declares no such column, \
         because it spells the whole table deleted.",
    ),
    (
        "account",
        "venue",
        "the venue-links plan's second release: `venue_id` has been the link since the first, and \
         the text column was written for the rollback window only — a copy no reader answers \
         from while a row's number names a venue, which the drop pass makes sure of before it \
         drops anything",
    ),
    (
        "credential",
        "venue",
        "the same, for venue-scoped credential rows — the one table whose `venue_id` stays \
         nullable, so the one where the drop pass's refusal can fire on a carried store",
    ),
    ("venue_setting", "venue", "the same"),
];

/// **Carry an EXISTING store onto a [`DDL`] that has DROPPED a column** — the delivery half of
/// §9 stage 4c, and a no-op on a store born after the drop.
///
/// # ⚠ Why this needs a trigger of its own rather than riding the rebuild above
///
/// [`rebuild_table_from_ddl`] copies the INTERSECTION of the two column sets (its trap 3), so a
/// dropped column is already taken off any table it rebuilds — **on the one occasion it rebuilds
/// it**. That is not a delivery path, for two independent reasons:
///
/// * [`migrate_tables_onto_autoincrement`]'s trigger is *this table does not yet declare
///   `AUTOINCREMENT`*, which goes false FOREVER after the first rebuild. A column dropped from the
///   batch afterwards reaches a store that has already been carried — both live boxes, the moment
///   stage 4a ships — never.
/// * It only visits [`autoincrement_tables`], and `venue_arming` is deliberately not one of them.
///
/// So the trigger here is the state that is still true: **the store's table still HAS the column**.
/// Idempotent by construction, and the rebuild that answers it is the same one, so this adds no
/// second spelling of the procedure and no second `DROP TABLE` for
/// `crates/vike-secrets/tests/gates/sqlite_sequence/mod.rs`'s source scan to classify.
///
/// # ⚠ Where it is CALLED from
///
/// `crate::db::ensure_venue_id_columns` — the idempotent repair funnel every writer passes through
/// — AFTER [`migrate_venue_links_onto_venue_id`], beside its siblings rather than nested inside one
/// of them. It was nested in [`migrate_tables_onto_autoincrement`] until 2026-09-23, because the
/// task that wrote this owned `schema.rs` and not `db.rs`, and it then sat one line after that
/// pass until the venue-links plan's second release moved it below the venue-links pass. Both
/// orders matter:
///
/// * **After the autoincrement pass**, for efficiency: a table that pass rebuilds already loses a
///   dropped column through the intersection, and this one then finds nothing to do rather than
///   rebuilding it a second time.
/// * **After the venue-links pass**, for correctness, since the text `venue` columns joined
///   [`DROPPED_COLUMNS`]: a store still on the pre-flip shape carries some venues only as text, and
///   the flip is what moves them onto the number. Dropping the text first would drop the only venue
///   those rows name.
///
/// Nesting was not merely untidy. The two triggers have different LIFETIMES — that one's goes
/// false forever once a store has been carried, this one's does not — so the nested reading makes
/// this pass look like a phase of that one, and the next author who deletes the outer pass as
/// spent takes this with it.
///
/// # ⚠ It refuses before it drops a text venue that has no number
///
/// Trap 7 ([`rebuild_table_from_ddl`]) refuses a row whose text venue has no number only while the
/// table being rebuilt still lets `venue_id` be NULL, and it never meets `credential`, whose
/// `venue_id` stays nullable by design. So it is never asked of a store the first release carried —
/// and on such a store a venue-scoped `credential` row filed for a venue the roster lacks keeps its
/// text and a NULL number, which this pass would erase. So before any table loses a text `venue`,
/// this pass asks [`unresolved_venue_links`] of the WHOLE store and refuses with trap 7's own
/// message ([`unresolved_refusal`]) while anything is listed: the refusal is REQUIRED here, not
/// redundant, and it commits nothing.
///
/// # Errors
/// The engine, the unresolved-venue refusal above, [`rebuild_table_from_ddl`]'s own refusals, and
/// a refusal when a rebuild ran and the column survived it.
pub(crate) fn migrate_dropped_columns(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    // Before ANY rebuild, so a refusal leaves every table on the shape it had: the text `venue` is
    // the only venue a row with no number names, and the drop below would erase it.
    if !text_venue_tables(tx)?.is_empty() {
        let unresolved = unresolved_venue_links(tx)?;
        if !unresolved.is_empty() {
            return Err(unresolved_refusal(&unresolved));
        }
    }
    for (table, column, _why) in DROPPED_COLUMNS {
        // `PRAGMA table_info` answers with NO ROWS for a table that is not there, so an absent
        // table is a skip without a second probe for it.
        if !table_columns(tx, table)?.iter().any(|have| have == column) {
            continue;
        }
        // ⚠ The decline is NAMED rather than being a bare `continue` — see
        // `Rebuild::decline_note`, and the same ⚠ at `migrate_sim_tier_to_paper`'s call site.
        let rebuilt = rebuild_table_from_ddl(tx, table, DEAD_COLUMN_SCRATCH_SUFFIX, &|_| None)?;
        if rebuilt.decline_note(table).is_some() {
            continue;
        }

        // The POSITIVE check both migrations above perform, asked of the NEW shape: assert the
        // column is gone from the table that now carries the real name, never merely that a
        // rebuild was attempted. A rebuild that silently produced the old body would pass
        // everything else here.
        if table_columns(tx, table)?.iter().any(|have| have == column) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!(
                    "the rebuilt `{table}` still carries `{column}`, which the shipped DDL no \
                     longer declares; nothing was committed"
                )),
            ));
        }
    }
    Ok(())
}

/// **Every table whose text `venue` [`DROPPED_COLUMNS`] drops and the store still holds** — the
/// venue-links plan's second release still owed to this store, in [`DROPPED_COLUMNS`] order and
/// EMPTY once it has been contracted (or was born contracted). Asked of `pragma_table_info`, so it is
/// the table the engine holds; a table the store does not hold is not listed.
///
/// ⚠ ONE spelling of "this store is not contracted yet", for its two askers:
/// [`migrate_dropped_columns`], which refuses before it drops a text venue that has no number, and
/// `crate::venue_links::apply_venue_links`' read-only probe, which decides whether
/// `vike-cli config migrate-store` has anything to do. It takes a `Connection` so the probe can ask
/// it outside any transaction; a `Transaction` derefs to one.
pub(crate) fn text_venue_tables(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<Vec<&'static str>> {
    let mut out = Vec::new();
    for (table, column, _why) in DROPPED_COLUMNS {
        if column == "venue" && crate::settings::has_column(conn, table, column)? {
            out.push(table);
        }
    }
    Ok(out)
}
