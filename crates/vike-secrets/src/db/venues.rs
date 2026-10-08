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
/// the file, and [`fill_into`]'s reshape branch only runs BELOW `SCHEMA_VERSION`, so a store already
/// at the current schema — which is the shape BOTH live boxes have been in since 2026-09-14 (see the
/// root `CLAUDE.md`) — reaches NEITHER path. Without this line, the `INSERT OR IGNORE` below would
/// fail outright with a bare `no such table: venue` the first time an operator added one credential
/// to an already-migrated store. `crate::settings`'s seven write sites and [`preview_rows`] already
/// re-run this exact batch for this exact reason (`IF NOT EXISTS` throughout, so it is a no-op
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
/// **Nothing breaks and nothing is to be fixed here.** The mark is a 64-bit integer, an id is an
/// ADDRESS rather than a count, and `crate::schema::rebuild_table_from_ddl` carries the large mark
/// across a rebuild correctly (measured with the rest). What it costs is a claim: a
/// design document called this step *"cheap, idempotent"* and it is idempotent in the ROWS only —
/// in the one number `crates/vike-secrets/tests/gates/sqlite_sequence/mod.rs` exists to protect. The
/// consequence a reader must carry is that **a venue added to the roster on a long-lived box gets
/// an arbitrarily large `venue.id`**, so nothing may present one as a small ordinal, derive a count
/// from a maximum, or size a column for it.
///
/// ⚠ Returns a bare `rusqlite::Result`, not `Result<_, DbError>`: its callers —
/// [`ensure_venue_id_columns`], [`crate::schema::reshape_into`] and [`preview_rows`] — are all in
/// that currency, as is [`crate::schema::write_rows`], called from [`fill_into`] alongside them,
/// and `DbError` needs a `path` this call has no cheap way to attach at this depth.
/// [`write_pending`] is where the whole fill is wrapped into `DbError::sql`, exactly as it already
/// wraps [`fill_into`]'s own result. (⚠ This named [`fill_into`] as the only caller until
/// 2026-09-23; that call MOVED when [`ensure_venue_id_columns`] took this over as its own first
/// statement — see that function's doc for why the guarantee became structural.)
///
/// ⚠ **The two other callers exist because of the venue-links flip.** `account.venue_id` is
/// `NOT NULL` since then, and both [`crate::schema::reshape_into`] (the schema-1 upgrade) and
/// [`preview_rows`] (the dry run of an already-current store) MINT account rows through
/// [`crate::schema::write_rows`] before any funnel runs. An account row takes its number from this
/// table in the same statement, so on the empty roster the bare `DDL` batch leaves, both refused
/// with `NOT NULL constraint failed: account.venue_id` — MEASURED across
/// `crates/vike-secrets/tests/migration/database/mod.rs`. Before the flip the same statement wrote a
/// NULL that the backfill filled later.
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

/// **Add `venue_id` where a store predates it, and fill it from the text column.** IDEMPOTENT.
///
/// `account`, `credential`, `venue_arming` and `venue_setting` each gained a nullable
/// `venue_id INTEGER REFERENCES venue(id)` column beside their text `venue` column —
/// [`crate::schema::DDL`]'s own doc names the four and the reason (a dual-write half with no
/// reader, until the venue-links plan made it the link every statement reads). That addition
/// reaches a store BORN after the change through `DDL`'s `CREATE TABLE IF NOT
/// EXISTS`, exactly like [`ensure_venue_rows`] above it, and reaches NOTHING born before it — the
/// same gap that function's own doc explains, for the same reason: `IF NOT EXISTS` does nothing to a
/// table that already exists. This function is the `ALTER TABLE` half for a store already at
/// [`SCHEMA_VERSION`].
///
/// ⚠ **Since the venue-links flip the shipped shape declares `venue_id` `NOT NULL` on `account`,
/// `venue_setting` and `venue_arming`, and this function is still where an existing store gets
/// there.** The `ALTER` below can only add the column NULLABLE (`ALTER TABLE` cannot add a
/// `NOT NULL` column with no `DEFAULT`), so the shape comes from a rebuild:
/// [`crate::schema::migrate_venue_links_onto_venue_id`], called near the end. The `ALTER` and
/// backfill now run BEFORE every rebuilding pass rather than after them, because each of those
/// passes copies into that `NOT NULL` shape — the comment at the loop carries the measurement.
///
/// ⚠ **A plain `ADD COLUMN` rather than the rebuild-copy `credential` needed elsewhere**, and the
/// reason is the same one `crate::settings::ensure_arming_columns` gives for `venue_arming.
/// max_exposure`: adding a NULLABLE column with no default is the case `ALTER TABLE` handles
/// directly, without rewriting a row and with nothing to interrupt halfway. SQLite permits a
/// `REFERENCES` clause on `ADD COLUMN` exactly while the default is NULL, which this is.
///
/// ⚠ **The backfill is an `UPDATE`, not a rewrite.** It sets the new column on rows that have a text
/// `venue` and leaves every other byte of the row alone — mirroring [`ensure_venue_rows`]'s own
/// `INSERT OR IGNORE` rather than a `DELETE`-then-reinsert, for the same reason: this runs on every
/// [`fill_into`] call, and a rewrite would cost every existing row on every write for a value that
/// hardly ever changes.
///
/// ⚠ **Calls [`ensure_venue_rows`] itself, as its FIRST statement — a STRUCTURAL guarantee now,
/// not a positional one.** This function's own DDL sub-selects and `ALTER TABLE … REFERENCES
/// venue(id)` clauses need the `venue` table to exist and, for the backfill to find anything, to be
/// POPULATED — both of which are exactly what [`ensure_venue_rows`] provides. This USED to be
/// enforced by ordering alone: [`fill_into`] called `ensure_venue_rows(tx)?;` immediately before
/// `ensure_venue_id_columns(tx)?;`, and every other reader of this function had to be trusted to do
/// the same. It was not: `edit_account`, [`move_pending_rows`], `crate::settings::write_settings`
/// and `crate::settings::set_venue_setting_in` each ran their own bare `tx.execute_batch
/// (crate::schema::DDL)` and then called this function directly — which creates `venue` EMPTY (`DDL`
/// is `CREATE TABLE IF NOT EXISTS`, and none of those four callers ever populate it) and never tops
/// it up. So on any one of those four paths, the backfill's sub-select
/// `(SELECT v.id FROM venue v WHERE v.name = …)` found no rows at all and silently set every
/// `venue_id` to NULL — MEASURED: `vike-cli config mirror` on a real box reaches exactly this
/// through `write_settings`. Calling [`ensure_venue_rows`] here, unconditionally, makes every one of
/// the five call sites correct BY CONSTRUCTION instead of by each remembering the same two-line
/// incantation in the right order; [`fill_into`] no longer calls [`ensure_venue_rows`] separately —
/// see that function's own updated comment.
///
/// ⚠ **Signature note for the next reader who compares this against a written plan**: a design
/// document for this change specified `fn ensure_venue_id_columns(db: &Path, tx: &Transaction<'_>)
/// -> Result<(), DbError>`, called as `ensure_venue_id_columns(&planned.db, tx)?`. That does not
/// compile: [`fill_into`] — the only caller — is in `rusqlite::Result` currency throughout (so are
/// its siblings [`crate::schema::reshape_into`] and [`crate::schema::write_rows`]), and `?` on a
/// `Result<(), DbError>` inside a `rusqlite::Result`-returning function has no `From` impl to use.
/// [`ensure_venue_rows`] above already made the identical correction for the identical reason; this
/// function matches its actual call site instead, dropping the `db: &Path` parameter (nothing here
/// needs it once nothing constructs a [`DbError`]) and returning a bare `rusqlite::Result<()>`. The
/// `DbError` wrap happens exactly once, where it already does for [`ensure_venue_rows`]'s errors:
/// [`write_pending`]'s `fill_into(...).map_err(|e| DbError::sql(&planned.db, e))?`.
///
/// [`crate::settings::has_column`] is reused rather than re-implemented — it answers in the same
/// `rusqlite::Result` currency for exactly this caller, and its two callers inside
/// `crate::settings` (which already hold a `db: &Path`) attach [`DbError`] themselves.
///
/// ⚠ **`pub(crate)`, not private, and for a second reason beyond [`fill_into`]'s own call.**
/// FOUR writers besides [`fill_into`] name `venue_id` in an `INSERT` column list and reach it
/// without ever passing through [`migrate`]/[`upsert_rows`] first: [`edit_account`]'s `Create` arm
/// and [`move_pending_rows`] (both in this module, called from here directly), and
/// `crate::settings::write_settings` and `crate::settings::set_venue_setting_in` (called through
/// the `crate::db::` path, which is what the `pub(crate)` is actually FOR — the two in this module
/// could see a private function). Without calling this first, a store that predates the column
/// (every store migrated before this change, until its next credential write) would answer a bare
/// `table … has no column named venue_id` the first time an operator ran `account create`,
/// `secrets move-venue-config`, `vike-cli config mirror`, or set a venue setting — turning a write
/// nothing here is expected to break into one that does. Each of the four ALSO runs its own
/// `tx.execute_batch(crate::schema::DDL)` immediately before calling this — redundant now that this
/// function calls [`ensure_venue_rows`] itself, but harmless (`IF NOT EXISTS` throughout) and left
/// alone rather than trimmed in the same change that fixed the correctness defect.
///
/// ⚠ **A table with no TEXT `venue` column has nothing for `venue_id` to shadow, and is skipped —**
/// **correct for schema 1's `credential` (`name TEXT PRIMARY KEY, value TEXT`: no `venue`, no
/// `account_id`, no `field`), even though NOTHING reaches this guard against a genuine schema-1
/// store TODAY.** Every non-`fill_into` caller of this function runs the WHOLE
/// `tx.execute_batch(crate::schema::DDL)` batch itself, immediately before calling here — a line
/// that predates this task — and that batch's `CREATE UNIQUE INDEX IF NOT EXISTS
/// credential_one_live_value ON credential (account_id, field) …` (part of the ORIGINAL 2026-09-14
/// schema-2 rollout, unrelated to `venue`) already fails to prepare against a genuine schema-1
/// `credential` with `no such column: account_id`, before this function is ever called. That is a
/// SEPARATE, PRE-EXISTING defect — `crates/vike-secrets/tests/migration/database/venue_id.rs`'s
/// `write_settings_still_refuses_a_genuine_schema_1_store_for_an_unrelated_pre_existing_reason`
/// pins the CURRENT failure so it is not confused with this guard — and this task does not fix it:
/// unrelated to `venue_id`, it predates this whole branch, and no live box has ever reached it
/// (both have been at schema 2 since before `write_settings` existed). What this guard DOES do is
/// what its own unit test proves directly rather than through a writer that cannot reach it today —
/// `ensure_venue_id_columns_skips_a_credential_table_with_no_venue_column`, this module's own
/// `db_tests` — and it is written for the shape [`READABLE_SCHEMA_VERSIONS`] says this binary must
/// still be able to READ, in case the DDL-ordering bug above is ever fixed and this path becomes
/// reachable in practice. Either way, a schema-1 `credential` is given `venue_id` for the first
/// time by [`crate::schema::reshape_into`]'s own wholesale rebuild (through `DDL`, which already
/// carries the column), never by an `ALTER` on its old shape.
///
/// ⚠ **Since the venue-links plan's second release that skip is also the ORDINARY path for three of
/// the four tables**: `account`, `credential` and `venue_setting` hold `venue_id` alone on a store
/// this release has carried, so the loop fills nothing there and only `venue_arming` (whose text
/// column stays until Plan B deletes the table) still gets a backfill. On a store not yet carried
/// the loop runs exactly as before, which is what lets the passes below drop the text after it.
pub(crate) fn ensure_venue_id_columns(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    ensure_venue_rows(tx)?;
    // ⚠ The `venue_id` ALTER and backfill come SECOND — after the roster, BEFORE every pass that
    // can rebuild a table. They sat below those passes until the venue-links flip, and the order
    // is load-bearing now. Every one of those passes copies into the SHIPPED shape, whose
    // `venue_id` is `NOT NULL` on `account`, `venue_setting` and `venue_arming`, so a copy that ran
    // first met rows the backfill had not filled yet — MEASURED: `NOT NULL constraint failed:
    // account_pre_paper_tier.venue_id` out of the §4.4 pass, on a store whose every row named a
    // roster venue — and on a store that predates the column it would find the column missing and
    // DECLINE (`crate::schema`'s `Rebuild::Skipped`). Nothing is lost by running it first: a
    // rebuild copies `venue_id` with every other column, so a value filled here survives it.
    for table in ["account", "credential", "venue_arming", "venue_setting"] {
        if !crate::settings::has_column(tx, table, "venue")? {
            continue;
        }
        if !crate::settings::has_column(tx, table, "venue_id")? {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN venue_id INTEGER REFERENCES venue(id);"
            ))?;
        }
        tx.execute_batch(&format!(
            "UPDATE {table} SET venue_id = (SELECT v.id FROM venue v WHERE v.name = {table}.venue) \
             WHERE venue IS NOT NULL AND venue_id IS NULL;"
        ))?;
    }
    // ⚠ §4.4's `sim` -> `paper` rename is the one post-freeze change `DDL` alone cannot deliver to
    // an EXISTING store: the CHECK it must replace belongs to a table `CREATE TABLE IF NOT EXISTS`
    // skips. It rides here rather than in `ensure_venue_rows` because this is the idempotent
    // REPAIR step every writer already funnels through. (⚠ This said it must run BEFORE the
    // `venue_id` backfill *"so the backfill's `UPDATE` lands on the rebuilt table rather than on
    // one that is about to be replaced"*; the backfill moved ABOVE it with the venue-links flip,
    // for the reason that loop's comment gives.) See `crate::schema::migrate_sim_tier_to_paper`.
    crate::schema::migrate_sim_tier_to_paper(tx)?;
    // ⚠ AFTER the rename above, and the order is load-bearing rather than tidy. This rebuild
    // re-creates each table from the SHIPPED `DDL`, whose `account` CHECK names `'paper'`, so a
    // store still holding `tier = 'sim'` rows would have them copied into a table that REFUSES
    // them — a `CHECK constraint failed` on an operator's credential write. The rename runs first
    // and there is nothing left for this one to refuse. See
    // `crate::schema::migrate_tables_onto_autoincrement`, and §4.1 of the settings-store-plane
    // design for why `AUTOINCREMENT` is a rebuild rather than an `ALTER`.
    crate::schema::migrate_tables_onto_autoincrement(tx)?;
    // §5.2 step 7 — `venue_setting.tier IS NULL` becomes `'any'`. The order is one of EFFICIENCY
    // rather than correctness, which is the point of where its carry lives: the NULL -> `'any'`
    // carry is applied by `crate::schema::rebuild_table_from_ddl` to every rebuild of that table,
    // so on a store old enough for §4.4's pass to rebuild `venue_setting` first the table already
    // arrives here on its new shape and this pass finds nothing to do. On both live boxes
    // (stage-4a/4b shape, no `'sim'`, already armed) this was the only pass that fired when step 7
    // shipped. (⚠ This also said *"BEFORE the `venue_id` backfill below"*; the backfill is above
    // every pass since the venue-links flip. And it said *"LAST of the four"*: the drop pass sat
    // above it until the venue-links plan's second release moved it below the venue-links pass.)
    // See `crate::schema::migrate_venue_setting_tier_to_any`.
    crate::schema::migrate_venue_setting_tier_to_any(tx)?;
    // The venue links become `venue_id` — AFTER the backfill above, so every roster venue's row
    // already carries its number and a row that does not is one naming no roster venue, which the
    // rebuild refuses by name (`crate::schema::rebuild_table_from_ddl`'s trap 7). AFTER §4.4's pass
    // for correctness: this pass rebuilds with no rewrite, so it would copy a `'sim'` row into the
    // `'paper'` CHECK. After the autoincrement and step-7 passes for efficiency only: each copies
    // into the same shipped shape, so a table one of them rebuilt arrives here already carried. On
    // both live boxes this is the pass that fired at the plan's first release.
    //
    // ⚠ The 0095 call below comes AFTER it in THIS function and that orders nothing beyond it.
    // On the path 0095 is really applied by —
    // `crate::live_means_mainnet::apply_live_means_mainnet`, which a boot's ceiling step and
    // `vike-cli config migrate-store` call — decision 0095's migration runs FIRST, on the store
    // exactly as it was found (pre-flip, maybe pre-backfill, maybe with no `venue_id` column at
    // all), and only then this whole function, whose own 0095 call then finds the store marked.
    // So that migration can never assume this pass has run: it reads `venue_arming` through
    // `crate::schema::VenueLink` — the number where a row has one, the text where it does not — and
    // rewrites exactly the rows it read, by rowid (`crate::live_means_mainnet`'s module doc). A
    // NAMED refusal from THIS function comes back as `DbErrorKind::RepairRefused` on that path as
    // on every writer's (`DbError::sql` classifies it), so the boot can tell it from a failure of
    // 0095's own step. See `crate::schema::migrate_venue_links_onto_venue_id`.
    crate::schema::migrate_venue_links_onto_venue_id(tx)?;
    // AFTER the venue-links pass, never before: a store still on the pre-flip shape carries its
    // venue only as text, and dropping the text column first would drop the only venue its rows
    // name. The flip moves the venue onto the number; this drop then removes the copy.
    //
    // ⚠ It is BESIDE the passes above, not inside one — this line was
    // `migrate_tables_onto_autoincrement`'s closing statement until 2026-09-23, only because the
    // task that wrote it owned `schema.rs` and not this file, and it sat one line after that pass
    // until the venue-links plan's second release moved it here. Their TRIGGERS have different
    // lifetimes: the autoincrement pass's goes false FOREVER once a store has been carried and it
    // never visits `venue_arming` at all, while this one's is *the store's table still HAS the
    // column*, which stays true afterwards. Nested, this looked like a phase of that, and the next
    // author deleting the outer pass as spent would take this with it. It is still AFTER that pass:
    // a table that pass rebuilt has already lost a dropped column through the intersection, so this
    // one finds nothing to do rather than rebuilding it twice. Moving `venue_arming.notes`' drop down
    // with it is harmless: nothing reads that column and the pass is idempotent. The pass refuses,
    // BY NAME and before it rebuilds anything, while a row's text venue has no number — trap 7
    // never asks a carried table (see `crate::schema::migrate_dropped_columns`).
    crate::schema::migrate_dropped_columns(tx)?;
    // Decision 0095's ceiling migration, LAST, and here so that no write can precede it: every
    // writer funnels through this function. A no-op once the store is marked. The journalled path
    // is `crate::live_means_mainnet::apply_live_means_mainnet`, which every booting root calls first.
    crate::live_means_mainnet::migrate_live_means_mainnet(tx)?;
    Ok(())
}
