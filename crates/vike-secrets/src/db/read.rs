//! The credential readers: the whole-table, scoped, present-names and demo-only reads.

use super::*;

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// Every row of one table, as the map the rest of the workspace speaks.
///
/// The WHOLE table, for the callers that need the SET: a live mount composes its key names from
/// `vike_model::credential_keys`' grid, each venue's own `*_env_var_names` and an UNBOUNDED account
/// label, so no list it could hand a reader would be complete; and `vike-cli secrets list` /
/// `config show` have the store itself as their subject.
///
/// ⚠ **This doc said a per-key `SELECT` would be "a second shape for callers to reason about with
/// no reader that wants one", and that clause EXPIRED on 2026-09-16**, when the owner ruled that a
/// process must materialise only what it asked for. There is now a reader that wants one:
/// [`read_table_scoped`], reached through `crate::store::resolve_project_scoped`, whose whole
/// subject is a caller needing one or two names and holding nothing else.
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` had already named that as the debt
/// the database left behind — *"Only the scoped read supplies it."*
///
/// This function did not change and no caller of it should move without a reason: the set is a
/// legitimate need, and forcing a live daemon through a hand-assembled name list would trade a
/// bounded blast radius for a silent arming defect.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub fn read_table(path: &Path, table: Table) -> Result<SecretMap, DbError> {
    let (conn, version) = open_for_read(path)?;
    read_table_on(path, &conn, table, version)
}

/// **Only the DECLARED names of one table** — the scoped twin of [`read_table`], and the one place
/// the narrowing is a genuinely narrower QUERY rather than a filter.
///
/// Owner ruling, 2026-09-16: restrict what a process materialises. A row outside `scope` is never
/// selected, so it never enters this process's address space at all — which is the difference
/// between this arm and the retired scoped file arm, where the whole file must be
/// parsed before anything can be dropped (that type's own ⚠ section says so).
///
/// # ⚠ It must answer EXACTLY as [`read_table`] does for the names it was given
///
/// One statement per declared name, carrying the SAME `WHERE` predicate and the SAME ordering
/// [`read_table_on`] builds — from the same two helpers, so the two cannot drift. That is not
/// tidiness: the widened `superseded_at` predicate is what keeps a TIER ALIAS
/// (`{VENUE}_MAINNET_API_KEY` beside its `_LIVE_` twin, filed superseded because
/// `crate::schema`'s `credential_one_live_value` admits one live row per `(account_id, field)`)
/// in the map, and the `id` ordering is what makes the last-wins fold deterministic. A scoped
/// `SELECT` that spelled a flat `superseded_at IS NULL` would silently drop a key the operator
/// wrote — the exact defect that predicate was widened to fix, reintroduced one function over.
/// `crates/vike-secrets/tests/reads/scoped_read.rs`'s
/// `a_scoped_read_answers_exactly_like_the_whole_table_read_name_for_name` folds both readers over
/// the same planted store and compares, rather than asserting it.
///
/// Per-name rather than one `name IN (…)`: the scopes this serves are one to six names, the
/// `credential_one_live_name` index makes each lookup a seek, and a bound list has a parameter
/// ceiling this would then have to chunk around. The name is a BOUND parameter either way — only
/// `table.sql_name()`, a `&'static str` from a closed enum, is interpolated.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub fn read_table_scoped(
    path: &Path,
    table: Table,
    scope: &crate::store::KeyScope,
) -> Result<SecretMap, DbError> {
    let (conn, version) = open_for_read(path)?;
    read_table_scoped_on(path, &conn, table, version, scope)
}

/// The `WHERE` predicate (no `WHERE` keyword) that selects the rows a read may see, or `""`.
///
/// Extracted so [`read_table_on`] and [`read_table_scoped_on`] cannot spell it differently — see
/// [`read_table_scoped`] for what a divergence would cost.
fn live_predicate(table: Table, version: i64) -> &'static str {
    match (table, version) {
        // Schema 1 has no `superseded_at` column, so naming it would be a prepare-time error.
        (_, 1) | (Table::NodeKey, _) => "",
        (Table::Credential, _) => {
            "superseded_at IS NULL \
               OR NOT EXISTS (SELECT 1 FROM credential live \
                              WHERE live.name = credential.name AND live.superseded_at IS NULL)"
        }
    }
}

/// ⚠ `ORDER BY name, id` and not `name` alone. The rows are folded into a `BTreeMap`, so the LAST
/// row of a name wins, and [`live_predicate`]'s widened clause can return more than one row for a
/// name that has no live one. `id` is monotonic per INSERT, so the newest row answers —
/// deterministic on every box and on every run, which row order is not.
///
/// The scoped reader binds one name at a time, so this reduces to `ORDER BY id` there; it is the
/// same string rather than a second one for the reason [`live_predicate`] is shared.
fn order_clause(table: Table, version: i64) -> &'static str {
    match table {
        Table::Credential if version != 1 => " ORDER BY name, id",
        _ => " ORDER BY name",
    }
}

/// [`read_table_scoped`] on an open connection.
fn read_table_scoped_on(
    path: &Path,
    conn: &Connection,
    table: Table,
    version: i64,
    scope: &crate::store::KeyScope,
) -> Result<SecretMap, DbError> {
    let mut out = BTreeMap::new();
    if scope.is_empty() {
        return Ok(SecretMap::new(out));
    }
    let live = live_predicate(table, version);
    let where_clause = if live.is_empty() {
        " WHERE name = ?1".to_string()
    } else {
        format!(" WHERE name = ?1 AND ({live})")
    };
    // `table.sql_name()` is a `&'static str` from a closed enum — see `Table`. The NAME is a bound
    // parameter, so no caller-supplied text reaches the statement.
    let sql = format!(
        "SELECT value FROM {}{where_clause}{}",
        table.sql_name(),
        order_clause(table, version)
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    for name in scope.as_set() {
        let rows = stmt
            .query_map([name.as_str()], |r| r.get::<_, String>(0))
            .map_err(|e| DbError::sql(path, e))?;
        // LAST wins, matching the `BTreeMap` fold `read_table_on` performs over the same ordering.
        let mut last = None;
        for row in rows {
            last = Some(row.map_err(|e| DbError::sql(path, e))?);
        }
        if let Some(value) = last {
            out.insert(name.clone(), value);
        }
    }
    Ok(SecretMap::new(out))
}

/// The bytes a stored value may be padded with and still count as BLANK — the ASCII half of what
/// Rust's `str::trim` strips, written as the second argument of SQLite's `trim(X, Y)`. See
/// [`read_present_names_scoped`] for the residual this leaves.
const BLANK_PADDING_SQL: &str = "' ' || char(9, 10, 11, 12, 13)";

/// **Which DECLARED names of one table hold a non-blank value — the names, never the values.**
/// The presence twin of [`read_table_scoped`], for a caller whose question is "is it stored" and
/// who must not hold the answer to "what is it".
///
/// It exists for the datahub's history-channels verb
/// (`docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §2.4): an OBSERVE request
/// learns whether the operator stored the OANDA practice token, and on a settings DATABASE that
/// question is now answered without the token becoming a Rust value in that process at all. The
/// statement selects a boolean computed by SQLite from the row; the value column is never returned.
/// ⚠ That is a claim about this process's VALUES, not about its pages: SQLite reads the page that
/// holds the row into its own cache, as it reads every page a query touches.
///
/// # ⚠ It must answer exactly as a value reader followed by a blank check would
///
/// The SAME `WHERE` predicate and the SAME ordering [`read_table_scoped_on`] builds, from the same
/// two helpers, and the LAST row of a name decides — so a tier alias and a superseded row answer
/// here exactly as they answer there. A value that is empty or ASCII whitespace only is NOT
/// present, because every venue reader treats a blank value as absent
/// (`vike_bridge_core::credentials::account_var`'s `trim().is_empty()`), and a presence answer that
/// disagreed with the reader would tell an operator a key is armed that the lane then refuses.
///
/// ⚠ **One residual, declared:** SQLite's `trim` is given the ASCII whitespace set and Rust's
/// `str::trim` strips Unicode whitespace too, so a value made ONLY of non-ASCII whitespace reads
/// present here and absent to a reader. No real credential has that shape.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub fn read_present_names_scoped(
    path: &Path,
    table: Table,
    scope: &crate::store::KeyScope,
) -> Result<BTreeSet<String>, DbError> {
    let (conn, version) = open_for_read(path)?;
    let mut out = BTreeSet::new();
    if scope.is_empty() {
        return Ok(out);
    }
    let live = live_predicate(table, version);
    let where_clause = if live.is_empty() {
        " WHERE name = ?1".to_string()
    } else {
        format!(" WHERE name = ?1 AND ({live})")
    };
    // `table.sql_name()` is a `&'static str` from a closed enum and `BLANK_PADDING_SQL` a constant
    // of this file; the NAME is a bound parameter, so no caller-supplied text reaches the statement.
    // The selected column is an INTEGER (0 or 1), never the value — and a NULL value is not present.
    let sql = format!(
        "SELECT coalesce(trim(value, {BLANK_PADDING_SQL}) <> '', 0) FROM {}{where_clause}{}",
        table.sql_name(),
        order_clause(table, version)
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    for name in scope.as_set() {
        let rows = stmt
            .query_map([name.as_str()], |r| r.get::<_, bool>(0))
            .map_err(|e| DbError::sql(path, e))?;
        // LAST wins, the fold `read_table_scoped_on` performs over the same ordering.
        let mut last = None;
        for row in rows {
            last = Some(row.map_err(|e| DbError::sql(path, e))?);
        }
        if last == Some(true) {
            out.insert(name.clone());
        }
    }
    Ok(out)
}

/// # ⚠ The version branch, and why the READERS CANNOT TELL THE TWO APART
///
/// Schema 2 keeps `credential.name` and `credential.value` and — because §11's steps 3 and 4 are
/// deliberately not performed (`crate::schema`'s module doc argues the sequencing rule that forbids
/// them) — it still holds a row for EVERY live name. So the two spellings below select the same set
/// over the same store, and that is the whole of the "the readers must not notice" claim:
/// `crates/vike-secrets/tests/migration/database/schema2.rs`'s
/// `a_reshaped_store_answers_byte_for_byte_like_the_flat_one_it_came_from` compares the two maps
/// directly rather than asserting it.
///
/// The ONE difference is the `WHERE`, and it is load-bearing rather than tidy: schema 2 can hold
/// SUPERSEDED rows (§4.2's rollback copies), which carry the SAME `name` as the live value that
/// replaced them. Without the clause a superseded `ASTER_*` value and its live replacement would
/// collapse into one `BTreeMap` key and the winner would be decided by row order — i.e. a MAINNET
/// key on the one venue in this store that trades real money, chosen arbitrarily.
///
/// # ⚠ …and the one row that is superseded and STILL ANSWERS
///
/// The clause is *this name has a live row and this is not it*, not *superseded rows are invisible*,
/// and the difference is a whole key. A tier ALIAS — `{VENUE}_MAINNET_API_KEY` beside its
/// `{VENUE}_LIVE_API_KEY` twin — is filed `superseded_at IS NOT NULL` because
/// `credential_one_live_value` admits ONE live row per `(account_id, field)` and the two spellings
/// derive the same one (`crate::schema::RowReport::aliases` carries that argument in full). Its
/// NAME, though, appears on no live row at all. A flat `superseded_at IS NULL` therefore DROPPED it
/// from the map — a name the operator wrote in `secrets.env`, present in the database, absent from
/// `crate::resolve_project` and from `vike-cli secrets list`, with nothing saying so. So the `WHERE`
/// admits a superseded row whose name no live row carries, which is exactly the set the flat clause
/// was reaching for.
///
/// The §4.2 rollback copies are untouched by that widening and cannot be reached by it: a rollback
/// copy is only ever written for a name that HAS a live row (`crate::schema::write_rows` refuses
/// the rest — `SchemaRefusal::SupersededKeyIsNotInTheStore`), so the `NOT EXISTS` is false for
/// every one of them.
///
/// ⚠ **It is a DEVIATION from §11's printed read** (*"`SELECT name, value FROM credential WHERE
/// superseded_at IS NULL` for the 50"*), and it is stated here rather than edited into a signed
/// spec. §11 is describing the mirror-period RENDERER over a store in which every name has a live
/// row, which was true of every shape that spec enumerates; the tier alias is a name that does not,
/// and the flat clause would answer for it with nothing. The two agree everywhere §11 was looking.
pub(super) fn read_table_on(
    path: &Path,
    conn: &Connection,
    table: Table,
    version: i64,
) -> Result<SecretMap, DbError> {
    // `table.sql_name()` is a `&'static str` from a closed enum — see `Table`. No operator input
    // reaches this string, and the `WHERE` below is a literal chosen by a match on an integer.
    // Both clauses come from the helpers the SCOPED reader also calls, so the two cannot drift —
    // `read_table_scoped`'s doc carries what a divergence would cost.
    let predicate = live_predicate(table, version);
    let live = if predicate.is_empty() { String::new() } else { format!(" WHERE {predicate}") };
    let order = order_clause(table, version);
    let sql = format!("SELECT name, value FROM {}{live}{order}", table.sql_name());
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (name, value) = row.map_err(|e| DbError::sql(path, e))?;
        out.insert(name, value);
    }
    Ok(SecretMap::new(out))
}

/// The SQL twin of [`crate::store::withheld_by_demo_scope`]: a `credential` row whose NAME this
/// matches is never selected under the demo-only scope.
///
/// `GLOB`, not `LIKE`: `_` is a wildcard to `LIKE` and a literal to `GLOB`, and every token here is
/// spelled with underscores. `upper(name)` makes the match case-insensitive, which can only widen
/// what is withheld. `crates/vike-secrets/tests/reads/demo_only_scope.rs` holds the two spellings equal
/// over a planted name set, so neither can drift from the other in silence.
const DEMO_SCOPE_WITHHELD_SQL: &str = "(upper(name) GLOB '*_LIVE_*' OR upper(name) GLOB '*_LIVE' \
     OR upper(name) GLOB '*_MAINNET_*' OR upper(name) GLOB '*_MAINNET' \
     OR upper(name) GLOB 'ASTER_*' OR upper(name) GLOB 'POLY_*')";

/// **The `credential` table under the DEMO-ONLY scope** — every row [`read_table`] would return
/// except those [`crate::store::withheld_by_demo_scope`] names, plus HOW MANY distinct names were
/// withheld.
///
/// ⚠ The exclusion is in the `WHERE`, so a withheld row is never selected: its VALUE never becomes a
/// Rust value in this process. The count is a `count(DISTINCT name)` over the same predicate and
/// reads no value either. Same read-only open as every other reader here ([`open_for_read`]).
///
/// # Errors
/// [`DbError`] when the database will not open or is not this schema — the loud arm, as for
/// [`read_table`].
pub fn read_credentials_demo_only(path: &Path) -> Result<(SecretMap, usize), DbError> {
    let (conn, version) = open_for_read(path)?;
    let table = Table::Credential;
    let live = live_predicate(table, version);
    let base = if live.is_empty() { String::new() } else { format!("({live}) AND ") };
    let order = order_clause(table, version);
    let sql = format!(
        "SELECT name, value FROM {} WHERE {base}NOT {DEMO_SCOPE_WITHHELD_SQL}{order}",
        table.sql_name()
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (name, value) = row.map_err(|e| DbError::sql(path, e))?;
        out.insert(name, value);
    }
    let count_sql = format!(
        "SELECT count(DISTINCT name) FROM {} WHERE {base}{DEMO_SCOPE_WITHHELD_SQL}",
        table.sql_name()
    );
    let withheld: i64 =
        conn.query_row(&count_sql, [], |r| r.get(0)).map_err(|e| DbError::sql(path, e))?;
    Ok((SecretMap::new(out), usize::try_from(withheld).unwrap_or(0)))
}
