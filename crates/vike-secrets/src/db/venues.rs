//! The `venue` table as a projection of the roster: the title seed and the two top-ups.

use super::*;

/// **Each roster venue as the venue spells itself — the SEED of the one migration.** The owner
/// ruled on 2026-09-30 that venue names live in the settings database rather than in code. This
/// table takes effect in exactly two moments of a store's life, and the result is that a title
/// already in the database is never rewritten:
/// - when [`ensure_venue_rows`] adds the `title` column to a store born before it;
/// - when it inserts a roster venue the store does not hold yet.
#[rustfmt::skip]
pub(super) const VENUE_TITLES: &[(&str, &str)] = &[
    ("binance", "Binance"),
    ("bybit", "Bybit"),
    ("okx", "OKX"),
    ("deribit", "Deribit"),
    ("oanda", "OANDA"),
    ("ig", "IG"),
    ("fxcm", "FXCM"),
    ("dukascopy", "Dukascopy"),
    ("polymarket", "Polymarket"),
    ("ibkr", "Interactive Brokers"),
    ("ctrader", "cTrader"),
    ("alpaca", "Alpaca"),
    ("aster", "Aster"),
    ("hyperliquid", "Hyperliquid"),
    // vike:new-venue:row ("{venue}", "TODO(new-venue: {venue}): the venue's own spelling"),
];

/// The seed spelling of one roster venue. `None` for a venue [`VENUE_TITLES`] does not name, which
/// `crates/vike-secrets/tests/accounts/venue_titles.rs` refuses.
pub(super) fn venue_title_seed(venue: &str) -> Option<&'static str> {
    VENUE_TITLES.iter().find(|(name, _)| *name == venue).map(|(_, title)| *title)
}

/// **Insert every `vike_model::VENUES` id this store does not already hold.** IDEMPOTENT.
///
/// It also carries the venue's own spelling, `title`, written once per row; see [`VENUE_TITLES`].
///
/// ⚠ A SEED would not be enough, and the difference is the whole reason this is a function rather
/// than a line in `DDL`: the roster GROWS. Without a top-up on every write-open, adding a venue to
/// `vike_model::VENUES` would make its credentials unstorable until somebody inserted a row
/// by hand — the foreign key would refuse the account, and the message would name a constraint
/// rather than the cause.
///
/// The `venue` table is a PROJECTION of that roster and never a second roster: nothing here invents
/// a venue KEY (the `name` column holds only the roster's own ids), and a venue this store holds
/// that the roster has dropped is left alone rather than deleted, because a live `account` row may
/// still reference it. The one other text this function writes is `title`, the owner's spelling
/// from [`VENUE_TITLES`], once per row — see above.
///
/// ⚠ **Re-runs the WHOLE `DDL` batch first, and that is not redundant — it is the only thing that
/// creates this table on a store already at [`SCHEMA_VERSION`].** `venue` joined `DDL` AFTER the
/// schema-2 freeze, exactly like `setting`/`venue_arming`/`profile_risk`/`settings_adoption` before
/// it — see `crate::schema::DDL`'s own module doc: *"An already-migrated store simply has no such
/// table until a writer runs this batch again."* [`open_for_write`] only runs `DDL` when it CREATES
/// the file, so a store already at the current schema — which is the shape BOTH live boxes have been
/// in since 2026-09-14 (see the root `CLAUDE.md`) — never reaches it there. Without this line, the
/// `INSERT OR IGNORE` below would fail outright with a bare `no such table: venue` the first time an
/// operator added one credential to an already-migrated store. `crate::settings`'s seven write sites
/// already re-run this exact batch for this exact reason (`IF NOT EXISTS` throughout, so it is a no-op
/// everywhere the table already exists); this follows that convention rather than inventing a
/// narrower one. Do not "simplify" this away as dead weight — `db_tests`' own
/// `ensure_venue_rows_creates_the_table_on_a_store_that_predates_it` is the regression test that
/// pins it, and the scenario it plants is not hypothetical.
///
/// # ⚠ IDEMPOTENT IN THE ROWS, NOT IN THE IDS — and stage 4a is what made that distinction real
///
/// MEASURED (branch review, 2026-09-23): on an `AUTOINCREMENT` table SQLite advances the
/// `sqlite_sequence` mark for an `INSERT OR IGNORE` **even when the row is ignored**, and the same
/// for `INSERT … ON CONFLICT DO UPDATE`. So the top-up below burns one `venue` id per roster venue
/// per pass through the funnel — **13 → 1313 over 100 ordinary writes on a 13-venue probe, with
/// the row count unchanged**. `crate::settings::set_venue_setting_in`'s `venue_setting` upsert and
/// [`upsert_rows`]' `node_key` upsert do the same to their own tables, all three armed by §4.1's
/// `AUTOINCREMENT`, which is the change that turned a no-op into a climb.
///
/// **Nothing breaks and nothing is to be fixed here.** The mark is a 64-bit integer and an id is an
/// ADDRESS rather than a count. What it costs is a claim: a design document called this step
/// *"cheap, idempotent"* and it is idempotent in the ROWS only — in the one number
/// `crates/vike-secrets/tests/gates/sqlite_sequence/mod.rs` exists to protect. The consequence a
/// reader must carry is that **a venue added to the roster on a long-lived box gets an arbitrarily
/// large `venue.id`**, so nothing may present one as a small ordinal, derive a count from a maximum,
/// or size a column for it.
///
/// # ⚠ EVERY writer calls it, at the start of its own transaction
///
/// It is the write funnel: [`fill_into`], [`upsert_rows`], [`edit_account`],
/// `crate::settings::write_settings`, `crate::settings::write_setting_row_in`
/// and `crate::settings::set_venue_setting_in` each call it before their first statement that names
/// a `venue_id`, because that statement looks the number up in this table. On an EMPTY roster (what
/// the bare `DDL` batch leaves) a `NOT NULL` `venue_id` is refused (`NOT NULL constraint failed`),
/// and a nullable one is silently written NULL.
///
/// ⚠ Returns a bare `rusqlite::Result`, not `Result<_, DbError>`: [`fill_into`] is in that
/// currency, and `DbError` needs a `path` this call has no cheap way to attach at this depth. A
/// caller holding a path wraps it with `DbError::sql`.
pub(crate) fn ensure_venue_rows(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(crate::schema::DDL)?;
    // The owner's ruling of 2026-09-30: venue names live here. A store born before the column gains
    // it ONCE, and the rows it already holds get their seed spelling in the same transaction. From
    // then on nothing rewrites a title: the insert below touches only a venue the store lacks.
    if !crate::settings::has_column(tx, "venue", "title")? {
        tx.execute_batch("ALTER TABLE venue ADD COLUMN title TEXT;")?;
        let mut fill = tx.prepare("UPDATE venue SET title = ?2 WHERE name = ?1")?;
        for (name, title) in VENUE_TITLES {
            fill.execute([*name, *title])?;
        }
    }
    let mut stmt = tx.prepare("INSERT OR IGNORE INTO venue (name, title) VALUES (?1, ?2)")?;
    for venue in vike_model::VENUES {
        stmt.execute((*venue, venue_title_seed(venue)))?;
    }
    Ok(())
}
