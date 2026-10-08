//! §5.2 step 7: `venue_setting.tier IS NULL` becomes `'any'`.

use std::collections::BTreeMap;

use rusqlite::Transaction;

use super::venue_links::VenueLink;
use crate::schema::rebuild::{rebuild_table_from_ddl, table_sql};
use crate::schema::tiers::{ANY_TIER, ANY_TIER_TABLE, venue_setting_tier_of_stored};

// ---------------------------------------------------------------------------------------------
// §5.2 step 7 — `venue_setting.tier IS NULL` becomes `'any'`
// ---------------------------------------------------------------------------------------------

/// Suffix for the scratch name step 7's rebuild builds the new shape under.
///
/// Distinct from the three above for the reason [`AUTOINCREMENT_SCRATCH_SUFFIX`] gives about
/// [`RETIER_SCRATCH_SUFFIX`]: every rebuild runs in one transaction over overlapping tables, and a
/// shared scratch name makes a failure of one look like a leftover of another.
const ANY_TIER_SCRATCH_SUFFIX: &str = "_pre_any_tier";

/// **The two partial indexes step 7 RETIRES** — the NULL discriminator itself, as the shipped
/// `DDL` spelled it up to and including v0.1.34.
///
/// ⚠ **NAMED rather than derived, and they are the second half of the trigger for a reason that
/// was measured, not guessed.** The rebuild drops them with the table, and the shipped [`DDL`] no
/// longer declares them — but an OLDER binary's batch still does, as `CREATE UNIQUE INDEX IF NOT
/// EXISTS`, and the rebuild freed their names. So the first write an older binary makes to a
/// migrated store PUTS THEM BACK (MEASURED on a copy of the CI box's store with v0.1.34 — the spec's
/// step-7 as-built block carries it). On a `NOT NULL` column they constrain nothing new and refuse
/// nothing the total `UNIQUE` admits, so they are harmless; they are also exactly the shape ruling
/// 2 refuses, and a store should converge on the shipped shape when this binary next writes rather
/// than keep a rollback's residue forever. A rule that derived *indexes the batch does not declare*
/// would also delete one a NEWER binary added, which is the thing [`DROPPED_COLUMNS`]' own doc
/// refuses for columns.
///
/// ⚠ **Since the venue-links plan's second release that can happen only on a store still carrying
/// the text `venue`.** Both indexes key on it, and this binary's first write takes it out of
/// `venue_setting`, so on a store this binary has written an older batch's `CREATE` is refused
/// (`no such column: venue`) rather than putting them back. A store the plan's first release carried
/// and this one has not written yet still takes them — and there this trigger is FUNCTIONALLY
/// REDUNDANT: the drop pass rebuilds `venue_setting` on that store anyway (its text `venue` is one of
/// [`DROPPED_COLUMNS`]), and any rebuild retires the indexes. It is kept because it is the precise
/// statement of what step 7 owes and costs nothing on a store with no such index, not because a test
/// can fail without it: `crates/vike-secrets/tests/migration/venue_setting_any_tier.rs`'s
/// `a_legacy_index_an_older_binary_puts_back_is_retired_on_the_next_write` holds the end state and
/// says the same.
pub(crate) const RETIRED_TIER_INDEXES: [&str; 2] =
    ["venue_setting_one_per_tier", "venue_setting_one_per_machine"];

/// **Carry an EXISTING store onto step 7's `venue_setting`** — `tier TEXT NOT NULL` with
/// [`ANY_TIER`] where SQL NULL used to mean "applies to any tier", and ONE total
/// `UNIQUE (venue, tier, field)` where two partial indexes split the table on that NULL. A no-op on
/// a store born after step 7, and on one this pass already carried.
///
/// # ⚠ Why this is not gated on [`crate::SCHEMA_VERSION`]
///
/// The reason [`migrate_tables_onto_autoincrement`]'s own doc gives, which stage 4a set as the
/// precedent: [`crate::READABLE_SCHEMA_VERSIONS`] is `[1, SCHEMA_VERSION]`, so a bump to 3 silently
/// DROPS 2 — the version both live boxes hold — and every venue falls to paper with nothing
/// erroring. The trigger is the SHAPE, asked of the engine: **`tier` still admits NULL, or a
/// [`RETIRED_TIER_INDEXES`] index is still on the table.** The first goes false forever once the
/// table is rebuilt; the second can come BACK, after a rollback write, which is why it is here.
///
/// # ⚠ The NULL -> `'any'` carry is NOT spelled in this function
///
/// It lives in [`rebuild_table_from_ddl`] ([`carried_into_shipped_shape`]), because this is not the
/// only pass that can rebuild this table and every rebuild copies INTO the shipped shape, whose
/// `tier` is `NOT NULL`. On a store old enough to still hold `'sim'`, [`migrate_sim_tier_to_paper`]
/// rebuilds `venue_setting` FIRST — its rewrite knows about `'sim'` and nothing about NULL — and a
/// carry that lived only here would have that pass copy a NULL into a `NOT NULL` column and take
/// the operator's write down with it before this function ever ran. MEASURED: that is exactly how
/// `crates/vike-secrets/tests/migration/paper_tier.rs` failed with the carry absent (`NOT NULL constraint
/// failed: venue_setting_pre_paper_tier.tier`). With the carry in the rebuild, the order of the
/// passes in `crate::db::ensure_venue_id_columns` decides nothing about correctness, and this
/// function's own rebuild passes no rewrite at all.
///
/// # ⚠ The collision the new `UNIQUE` could meet, PROVED absent and refused BY NAME anyway
///
/// Collapsing two partial indexes into one total `UNIQUE (venue, tier, field)` would fail the copy
/// if two surviving rows mapped onto one key. No store this code wrote can hold such a pair:
///
/// * two tier-scoped rows cannot share `(venue, tier, field)` — `venue_setting_one_per_tier` was
///   exactly that constraint over every non-NULL row;
/// * two machine-scoped rows cannot share `(venue, field)` — `venue_setting_one_per_machine`, over
///   every NULL row, so they cannot share `(venue, 'any', field)` after the carry;
/// * a machine-scoped row cannot collide with a tier-scoped one, because no row could ALREADY hold
///   `'any'` — the old `CHECK` was `tier IS NULL OR tier IN ('paper', 'demo', 'live')` (or `'sim'`
///   before §4.4), which refuses the word;
/// * and both indexes arrived in the SAME commit as the table (`e3b669126`, schema 2), and every
///   write re-runs the batch that declares them, so there has never been a store with the table
///   and without them — short of somebody dropping one by hand.
///
/// That last case is the one [`rebuild_table_from_ddl`] refuses BY NAME through
/// [`carried_key_collisions`] rather than letting the engine's bare `UNIQUE constraint failed`
/// surface: picking either row would hand a bridge one of two values chosen by row order, and the
/// refusal names each colliding KEY (never a value) — by the credential name it answers to and by
/// its row ids — with the transaction left uncommitted.
/// `crates/vike-secrets/tests/migration/venue_setting_any_tier.rs`'s
/// `colliding_rows_refuse_the_write_by_name_and_nothing_is_committed` plants it.
///
/// # ⚠ What an OLDER binary sees afterwards — a rollback cost, stated where the change is
///
/// v0.1.34 reads `'any'` as a TIER, so its renderer answers `{HEAD}_ANY_{FIELD}` and every
/// machine-scoped row goes unreachable for THAT binary (the polymarket proxy family among them),
/// and it cannot write a machine-scoped row at all (it binds NULL, which `NOT NULL` refuses).
/// Tier-scoped rows read and write normally. The spec's step-7 as-built block carries the measured
/// operator view.
///
/// ⚠ **The polymarket proxy family is the one where "unreachable" changes behaviour, and it does
/// not fail quietly.** `crates/bridges/polymarket/src/egress.rs`'s `proxy_url_with` defaults the
/// proxy ON when `POLY_PROXY_ENABLED` answers nothing, so an older process whose store row said
/// `false` dials a SOCKS proxy on its built-in default instead — and where nothing listens there,
/// every Polymarket connection is refused and a recording daemon stops taking that venue's tape.
/// The remedy is the PROCESS environment (egress reads it before the store) of every unit that
/// builds a Polymarket client, set before the older binary starts; which unit that is, is a
/// property of the box, and the spec's block names it for the one measured. (This section said
/// only "unreachable" until review found the consequence was an outage, not a misread.)
///
/// ⚠ **Nothing here waits for a release.** This pass rides the write funnel, so the FIRST write by
/// ANY binary containing it migrates the store it is pointed at — a release is one such binary, a
/// hand-built `vike-cli` is another. Such a write against a store whose installed daemons are still
/// older puts them in exactly the rollback position above at their next start.
///
/// # Errors
/// The engine, [`rebuild_table_from_ddl`]'s own refusals (a dangling reference, a carried-key
/// collision), and a refusal when a rebuild ran and the old shape survived it.
pub(crate) fn migrate_venue_setting_tier_to_any(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if table_sql(tx, ANY_TIER_TABLE)?.is_none() {
        return Ok(());
    }
    if !column_is_nullable(tx, ANY_TIER_TABLE, "tier")?
        && retired_tier_indexes_present(tx)?.is_empty()
    {
        return Ok(());
    }
    // ⚠ The decline is NAMED rather than being a bare `continue` — see `Rebuild::decline_note`, and
    // the same ⚠ at `migrate_sim_tier_to_paper`'s call site.
    let rebuilt = rebuild_table_from_ddl(tx, ANY_TIER_TABLE, ANY_TIER_SCRATCH_SUFFIX, &|_| None)?;
    if rebuilt.decline_note(ANY_TIER_TABLE).is_some() {
        return Ok(());
    }

    // The POSITIVE checks every migration in this file performs, asked of the NEW shape: `tier` is
    // refused NULL by the ENGINE, the stored `CHECK` admits the new word, and no retired index is
    // left beside the total `UNIQUE` — a total `UNIQUE` with one partial index still next to it is
    // ruling 2's defect wearing one index instead of two.
    let sql = table_sql(tx, ANY_TIER_TABLE)?.unwrap_or_default();
    let left = retired_tier_indexes_present(tx)?;
    if column_is_nullable(tx, ANY_TIER_TABLE, "tier")?
        || !sql.contains(&format!("'{ANY_TIER}'"))
        || !left.is_empty()
    {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "the rebuilt `{ANY_TIER_TABLE}` still lets a NULL decide a row's kind (tier \
                 nullable: {}, '{ANY_TIER}' admitted: {}, retired indexes left: {left:?}); nothing \
                 was committed",
                column_is_nullable(tx, ANY_TIER_TABLE, "tier")?,
                sql.contains(&format!("'{ANY_TIER}'")),
            )),
        ));
    }
    Ok(())
}

/// Whether the ENGINE lets `table.column` hold a NULL — asked of `pragma_table_info`, i.e. of the
/// table the engine holds, not of the statement text, so a respelling of the `CREATE` cannot answer
/// for it. `false` when the table or the column is absent: there is nothing for a pass to carry, and
/// what covers that case is each caller's own (step 7's: [`rebuild_table_from_ddl`]'s decline; the
/// venue links': the funnel's `ALTER`, which adds `venue_id` before any pass runs).
///
/// ⚠ ONE spelling for every nullability this crate asks of the engine. Step 7 asked it of
/// `venue_setting.tier` and the venue-links pass of each table's `venue_id`, through two copies of
/// this query that differed only in the names, and `crate::venue_links::apply_venue_links`' probe
/// spelled a third (until the venue-links plan's final fix wave and its first review). It takes a
/// `Connection` so the read-only probe can ask it outside any transaction; a `Transaction` derefs to
/// one.
pub(crate) fn column_is_nullable(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<bool> {
    if !crate::settings::has_column(conn, table, column)? {
        return Ok(false);
    }
    conn.query_row(
        &format!("SELECT \"notnull\" FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |r| r.get::<_, i64>(0),
    )
    .map(|notnull| notnull == 0)
}

/// Every [`RETIRED_TIER_INDEXES`] index the engine still holds, sorted.
fn retired_tier_indexes_present(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name IN (?1, ?2) ORDER BY name",
    )?;
    let rows = stmt.query_map(RETIRED_TIER_INDEXES, |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// **What a copy INTO the shipped shape must do to one column's value, beyond the caller's own
/// rewrite** — `expr` (the caller's rewrite, or the bare column) wrapped, or handed back unchanged.
///
/// One case today, and it is step 7's: `venue_setting.tier` becomes `COALESCE(expr, 'any')`,
/// because the shipped column is `NOT NULL` and every older shape used NULL for "any tier". It is
/// applied by [`rebuild_table_from_ddl`] to EVERY rebuild of that table, whichever pass asked for
/// it — [`migrate_venue_setting_tier_to_any`]'s doc carries the measurement that made that a
/// requirement rather than tidiness. Composed OUTSIDE the caller's rewrite, so §4.4's
/// `CASE tier WHEN 'sim' THEN 'paper' ELSE tier END` still maps its word and a NULL still falls
/// through it to the carry.
pub(crate) fn carried_into_shipped_shape(table: &str, column: &str, expr: String) -> String {
    if table == ANY_TIER_TABLE && column == "tier" {
        format!("COALESCE({expr}, '{ANY_TIER}')")
    } else {
        expr
    }
}

/// **Every key of the shipped `UNIQUE (venue_id, tier, field)` and `UNIQUE (venue, tier, field)`
/// that two of `table`'s rows would share once carried** — one entry per colliding group, sorted,
/// and EMPTY for every table without a [`carried_into_shipped_shape`] carry (none of them can gain
/// a collision by being copied).
///
/// The tier it groups by is the CARRIED one — the caller's rewrite with the carry around it — so
/// the check asks exactly the question the copy is about to put to the engine. It names KEYS and
/// never a value: a venue setting's value is an operator's configuration and can be a URL with
/// something in it.
///
/// ⚠ **Each entry names the key THREE ways, and it named it one way until review.** It rendered
/// `venue/tier/field` alone — the STORED form, `polymarket/any/PROXY_HOST`, a word the operator
/// never typed — while the refusal around it told them to delete rows no shipped verb can delete.
/// Now each entry leads with the credential name(s) the row answers to (through
/// [`venue_setting_tier_of_stored`] and `crate::venue_setting::venue_setting_names`, the reader's
/// own path, so it cannot name a key the reader would not), then the stored key, then the colliding
/// ROW IDS — the repair is a delete by id, and a key alone does not say which rows to look at. The
/// ids are `rowid`, which is `id` on every shape of this table (`id INTEGER PRIMARY KEY` since it
/// was created); grouping happens here rather than in a `group_concat`, whose order SQLite does not
/// promise. Calling the renderer makes this file one of its pinned callers —
/// `crates/vike-ops/tests/settings_secrets/smoke_store_parity_gate.rs`'s `RENDERER_CALLERS` carries the row, as a
/// LABEL for a refusal rather than a second fold.
///
/// # ⚠ BOTH keys the shipped shape enforces, and each entry says which one it collides under
///
/// The shipped `venue_setting` carries `UNIQUE (venue_id, tier, field)` AND the text-keyed
/// `UNIQUE (venue, tier, field)` until the venue-links plan's second release drops the text, so the
/// copy asks both questions. This check asked only the NUMBER's until the plan's final fix wave,
/// and two rows SPELLED alike whose numbers name different venues — a lie only a hand edit makes —
/// reached the copy and met the engine's bare `UNIQUE constraint failed` on the scratch table.
/// So the rows are grouped by each key the table has,
/// and an entry names the key or keys it collides under — `venue_id`, the text `venue`, or both,
/// which is the ordinary case of a store whose two spellings agree — so the rows are named however
/// an operator searches for them.
///
/// ⚠ **Since the plan's second release the shipped shape enforces the number's key ALONE** — the
/// text column and its `UNIQUE` are gone from it — and the text half is asked anyway, of the OLD
/// table, while that table still carries a text `venue` (a store this release has not carried yet;
/// the trigger is the old table's column, not the shipped batch). Kept deliberately rather than
/// trimmed to what the copy would refuse: two rows spelled alike whose numbers differ are a lie
/// only a hand edit makes, and copying them would silently keep each number's reading and drop
/// the spelling that disagrees. Refusing names both rows instead.
/// `crates/vike-secrets/tests/migration/venue_setting_any_tier.rs`'s
/// `a_collision_under_the_text_key_alone_is_refused_by_name` holds it. The refusal's own sentence
/// still calls the key *"one key of the shipped UNIQUE"*, which for that half is now the key the
/// first release shipped.
pub(crate) fn carried_key_collisions(
    tx: &Transaction<'_>,
    table: &str,
    select_expr: &dyn Fn(&str) -> Option<String>,
) -> rusqlite::Result<Vec<String>> {
    if table != ANY_TIER_TABLE {
        return Ok(Vec::new());
    }
    let tier = carried_into_shipped_shape(
        table,
        "tier",
        select_expr("tier").unwrap_or_else(|| "tier".into()),
    );
    // The venue by its NUMBER ([`VenueLink`]), the key the shipped `UNIQUE (venue_id, tier, field)`
    // asks about; a row whose number is still NULL keys on its text, which is all it has. And by
    // its TEXT, the key `UNIQUE (venue, tier, field)` asks about. `tier` stays bare, exactly as the
    // caller's rewrite and the carry spell it: the joined `venue` table has no column of that name.
    // `rowid` is qualified because `venue` has one too.
    let mut keyed: Vec<(&str, String)> = Vec::new();
    if crate::settings::has_column(tx, table, "venue_id")? {
        let link = VenueLink::of(tx, table, "t")?;
        keyed.push((
            "`venue_id`",
            format!(
                "SELECT {}, {tier}, t.field, t.rowid FROM {table} t {} ORDER BY 1, 2, 3, 4",
                link.name, link.join
            ),
        ));
    }
    if crate::settings::has_column(tx, table, "venue")? {
        keyed.push((
            "the text `venue`",
            format!("SELECT t.venue, {tier}, t.field, t.rowid FROM {table} t ORDER BY 1, 2, 3, 4"),
        ));
    }
    // One entry per colliding group, merged across the two keys: a group both keys find (the
    // spellings agree) is ONE entry naming both, so the ordinary refusal does not list a pair twice.
    let mut found: BTreeMap<CollidingGroup, Vec<&str>> = BTreeMap::new();
    for (key, sql) in &keyed {
        for group in colliding_groups(tx, sql)? {
            found.entry(group).or_default().push(*key);
        }
    }
    Ok(found
        .into_iter()
        .map(|(CollidingGroup { venue, tier, field, ids }, keys)| {
            let rust_tier = venue_setting_tier_of_stored(Some(tier.clone()));
            let names =
                crate::venue_setting::venue_setting_names(&venue, rust_tier.as_deref(), &field);
            let ids: Vec<String> = ids.iter().map(i64::to_string).collect();
            format!(
                "`{}` ({venue}/{tier}/{field}: rows {}, by {})",
                names.join("` / `"),
                ids.join(", "),
                keys.join(" and by ")
            )
        })
        .collect())
}

/// One group of rows that would share a `venue_setting` key once carried: the `(venue, tier,
/// field)` key, as one of [`carried_key_collisions`]' two `SELECT`s reads it, and the rows' ids in
/// order. Ordered field by field, so the two keys' findings merge in a `BTreeMap`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CollidingGroup {
    venue: String,
    tier: String,
    field: String,
    ids: Vec<i64>,
}

/// Every `(venue, tier, field)` group of more than one row that `sql` — a `SELECT venue, tier,
/// field, rowid` ordered by those four — returns, with its row ids in order.
fn colliding_groups(tx: &Transaction<'_>, sql: &str) -> rusqlite::Result<Vec<CollidingGroup>> {
    let mut stmt = tx.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut groups: Vec<CollidingGroup> = Vec::new();
    for row in rows {
        let (venue, tier, field, id) = row?;
        let same_key =
            groups.last().is_some_and(|g| g.venue == venue && g.tier == tier && g.field == field);
        if !same_key {
            groups.push(CollidingGroup { venue, tier, field, ids: Vec::new() });
        }
        if let Some(last) = groups.last_mut() {
            last.ids.push(id);
        }
    }
    groups.retain(|g| g.ids.len() > 1);
    Ok(groups)
}
