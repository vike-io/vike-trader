//! The credential readers: the whole-table, scoped and present-names reads, each under the
//! handle's `crate::CredentialScope` (the no-live-keys predicate is applied in their statements).

use super::*;
use crate::store::CredentialScope;

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// Every row of one table, as the map the rest of the workspace speaks.
///
/// The WHOLE table, for the callers that need the SET: a live mount composes its key names from
/// `vike_venues::credential_keys`' grid, each venue's own `*_env_var_names` and an UNBOUNDED account
/// label, so no list it could hand a reader would be complete; and `vike-cli secrets list` /
/// `config show` have the store itself as their subject.
///
/// ⚠ **This doc said a per-key `SELECT` would be "a second shape for callers to reason about with
/// no reader that wants one", and that clause EXPIRED on 2026-09-16**, when the owner ruled that a
/// process must materialise only what it asked for. There is now a reader that wants one:
/// [`read_table_scoped`], reached through `crate::CredentialStore::read`, whose whole
/// subject is a caller needing one or two names and holding nothing else.
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` had already named that as the debt
/// the database left behind — *"Only the scoped read supplies it."*
///
/// No caller of it should move without a reason: the set is a legitimate need, and forcing a live
/// daemon through a hand-assembled name list would trade a bounded blast radius for a silent arming
/// defect.
///
/// # The credential scope is applied HERE, in the statement
///
/// `scope` is the handle's [`crate::CredentialScope`]. Under `Full` the statement is the unscoped
/// one, byte for byte, and the count is `0` (no count query runs). Under `NoLiveKeys` the `WHERE`
/// also carries `DEMO_SCOPE_WITHHELD_SQL` negated, so a withheld row is never selected and its
/// VALUE never becomes a Rust value in this process, and the second element is how many DISTINCT
/// names that predicate withheld (a `count(DISTINCT name)` that reads no value either). This is the
/// lowest read every `crate::CredentialStore` read goes through: there is no read of these rows
/// beside it that skips the scope.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn read_table(
    path: &Path,
    table: Table,
    scope: CredentialScope,
) -> Result<(SecretMap, usize), DbError> {
    let conn = open_for_read(path)?;
    read_table_on(path, &conn, table, scope)
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
/// (`ALPACA_SANDBOX_API_KEY` beside its `ALPACA_DEMO_API_KEY` twin, filed superseded because
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
/// `scope` is applied exactly as [`read_table`] applies it: under `NoLiveKeys` each per-name
/// statement carries the withheld-name predicate negated, and the count is how many DECLARED names
/// the store holds a readable row for that the predicate withheld.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn read_table_scoped(
    path: &Path,
    table: Table,
    keys: &crate::store::KeyScope,
    scope: CredentialScope,
) -> Result<(SecretMap, usize), DbError> {
    let conn = open_for_read(path)?;
    read_table_scoped_on(path, &conn, table, keys, scope)
}

/// The `WHERE` predicate (no `WHERE` keyword) that selects the rows a read may see, or `""`.
///
/// Extracted so [`read_table_on`] and [`read_table_scoped_on`] cannot spell it differently — see
/// [`read_table_scoped`] for what a divergence would cost. `crate::db::retire`'s `retire_row`
/// selects the row it retires through it too, so a delete retires exactly the row a reader answers
/// with.
///
/// # ⚠ A DELETED name answers with NOTHING (decision 0144)
///
/// The widened half (*a superseded row whose name has no live row*, the tier-alias case below)
/// would hand a deleted name straight back: retire its live row and the name has no live row, so
/// the retired row itself — or an older rollback copy or alias of the same name — would answer.
/// So the `NOT EXISTS` also counts a row carrying the delete's marker
/// (`crate::db::retire::RETIRED_MARK`, spelled here as the literal a `&'static str` needs, held
/// equal by `the_read_predicate_hides_the_mark_the_delete_writes`): **a name with a retired row and
/// no live row has no row a reader may see.** Re-added, the new LIVE row answers through the first
/// half and every older row of the name stays hidden.
///
/// ⚠ **Byte-identical over every store that holds no retired row**: the added disjunct is never
/// true there, so the predicate reduces to the one that shipped before the delete existed.
pub(super) fn live_predicate(table: Table) -> &'static str {
    match table {
        Table::HubKey => "",
        Table::Credential => {
            "superseded_at IS NULL \
               OR NOT EXISTS (SELECT 1 FROM credential live \
                              WHERE live.name = credential.name \
                                AND (live.superseded_at IS NULL \
                                     OR live.superseded_at = 'retired-by-the-operator'))"
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
fn order_clause(table: Table) -> &'static str {
    match table {
        Table::Credential => " ORDER BY name, id",
        Table::HubKey => " ORDER BY name",
    }
}

/// [`read_table_scoped`] on an open connection.
fn read_table_scoped_on(
    path: &Path,
    conn: &Connection,
    table: Table,
    keys: &crate::store::KeyScope,
    scope: CredentialScope,
) -> Result<(SecretMap, usize), DbError> {
    let mut out = BTreeMap::new();
    if keys.is_empty() {
        return Ok((SecretMap::new(out), 0));
    }
    let where_clause = per_name_where(table);
    // `table.sql_name()` is a `&'static str` from a closed enum — see `Table`. The NAME is a bound
    // parameter, so no caller-supplied text reaches the statement.
    let sql = format!(
        "SELECT value FROM {}{where_clause}{}{}",
        table.sql_name(),
        scope_conjunct(table, scope),
        order_clause(table)
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    for name in keys.as_set() {
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
    let withheld = match scope {
        CredentialScope::Full => 0,
        CredentialScope::NoLiveKeys | CredentialScope::NoVenueKeys => {
            // A declared name the store holds a readable row for, that the predicate withheld. The
            // statement selects a count, never a value.
            let count_sql = format!(
                "SELECT count(*) FROM {}{where_clause} AND {}",
                table.sql_name(),
                withheld_sql(table, scope)
            );
            let mut count = conn.prepare(&count_sql).map_err(|e| DbError::sql(path, e))?;
            let mut withheld = 0usize;
            for name in keys.as_set() {
                let rows: i64 = count
                    .query_row([name.as_str()], |r| r.get(0))
                    .map_err(|e| DbError::sql(path, e))?;
                if rows > 0 {
                    withheld += 1;
                }
            }
            withheld
        }
    };
    Ok((SecretMap::new(out), withheld))
}

/// The per-name `WHERE` ([`live_predicate`] behind a bound `name = ?1`) the scoped value read, its
/// withheld count and the presence read all share, so the three cannot select different rows.
fn per_name_where(table: Table) -> String {
    let live = live_predicate(table);
    if live.is_empty() {
        " WHERE name = ?1".to_string()
    } else {
        format!(" WHERE name = ?1 AND ({live})")
    }
}

/// The handle's scope as a conjunct appended to a `WHERE` that already has one, for a statement
/// that SELECTS A VALUE: nothing under [`CredentialScope::Full`], `AND NOT` the withheld-row
/// predicate under either narrower scope.
fn scope_conjunct(table: Table, scope: CredentialScope) -> String {
    match scope {
        CredentialScope::Full => String::new(),
        CredentialScope::NoLiveKeys | CredentialScope::NoVenueKeys => {
            format!(" AND NOT {}", withheld_sql(table, scope))
        }
    }
}

/// [`scope_conjunct`]'s twin for a statement that selects a BOOLEAN computed from the value and
/// never the value (the presence reads): the same conjunct under [`CredentialScope::NoLiveKeys`]
/// (a live key reads as absent), and NOTHING under [`CredentialScope::NoVenueKeys`], whose whole
/// subject is the value reaching Rust, which a boolean does not.
fn presence_conjunct(table: Table, scope: CredentialScope) -> String {
    match scope {
        CredentialScope::NoVenueKeys => String::new(),
        CredentialScope::Full | CredentialScope::NoLiveKeys => scope_conjunct(table, scope),
    }
}

/// The withheld-row predicate for `table` under `scope`: for `credential`, [`DEMO_SCOPE_WITHHELD_SQL`]
/// under [`CredentialScope::NoLiveKeys`] and `VENUE_OWNED_SQL` under
/// [`CredentialScope::NoVenueKeys`]; none for `hub_key` — a hub key belongs to no account and no
/// venue family, so no scope withholds any of them (the always-false `0` keeps every statement one
/// shape). Under [`CredentialScope::Full`] nothing is withheld and no statement asks.
fn withheld_sql(table: Table, scope: CredentialScope) -> String {
    match (table, scope) {
        (Table::HubKey, _) | (Table::Credential, CredentialScope::Full) => "(0)".to_string(),
        (Table::Credential, CredentialScope::NoLiveKeys) => DEMO_SCOPE_WITHHELD_SQL.to_string(),
        (Table::Credential, CredentialScope::NoVenueKeys) => {
            format!("({DEMO_SCOPE_WITHHELD_SQL} OR {VENUE_OWNED_SQL})")
        }
    }
}

/// **The rows [`CredentialScope::NoVenueKeys`] withholds, beyond [`DEMO_SCOPE_WITHHELD_SQL`]'s** —
/// every `credential` row filed against an ACCOUNT or a VENUE, by the row's own columns and never by
/// the shape of its name (decision 0156 item 6). What is left is the DEPLOYMENT rows (both columns
/// NULL): the pager's, a data provider's key, an attribution code.
///
/// The first half of the scope's union ([`withheld_sql`] ORs the two) is deliberate: a legacy
/// `ASTER_…` / `POLY_…` row filed as a deployment name is a venue secret too, so the scope that
/// withholds every venue value must withhold at least what the narrower one does.
const VENUE_OWNED_SQL: &str =
    "(credential.account_id IS NOT NULL OR credential.venue_id IS NOT NULL)";

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
/// present, because every reader treats a blank value as absent
/// (`crate::account_credentials::CredentialFields::get`'s `trim().is_empty()`), and a presence answer that
/// disagreed with the reader would tell an operator a key is armed that the lane then refuses.
///
/// ⚠ **One residual, declared:** SQLite's `trim` is given the ASCII whitespace set and Rust's
/// `str::trim` strips Unicode whitespace too, so a value made ONLY of non-ASCII whitespace reads
/// present here and absent to a reader. No real credential has that shape.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn read_present_names_scoped(
    path: &Path,
    table: Table,
    keys: &crate::store::KeyScope,
    scope: CredentialScope,
) -> Result<BTreeSet<String>, DbError> {
    let conn = open_for_read(path)?;
    let mut out = BTreeSet::new();
    if keys.is_empty() {
        return Ok(out);
    }
    let where_clause = per_name_where(table);
    // `table.sql_name()` is a `&'static str` from a closed enum and `BLANK_PADDING_SQL` a constant
    // of this file; the NAME is a bound parameter, so no caller-supplied text reaches the statement.
    // The selected column is an INTEGER (0 or 1), never the value — and a NULL value is not present.
    // Under `NoLiveKeys` a withheld name selects no row, so it is not present; under `NoVenueKeys` the
    // boolean is computed in SQLite and the value never reaches Rust, so nothing is withheld.
    let sql = format!(
        "SELECT coalesce(trim(value, {BLANK_PADDING_SQL}) <> '', 0) FROM {}{where_clause}{}{}",
        table.sql_name(),
        presence_conjunct(table, scope),
        order_clause(table)
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    for name in keys.as_set() {
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

/// [`read_table`] on an open connection.
///
/// # ⚠ The `WHERE`, and why it is load-bearing rather than tidy
///
/// The `credential` table can hold SUPERSEDED rows (§4.2's rollback copies), which carry the SAME
/// `name` as the live value that replaced them. Without the clause a superseded `ASTER_*` value and its live replacement would
/// collapse into one `BTreeMap` key and the winner would be decided by row order — i.e. a live
/// key on the one venue in this store that trades real money, chosen arbitrarily.
///
/// # ⚠ …and the one row that is superseded and STILL ANSWERS
///
/// The clause is *this name has a live row and this is not it*, not *superseded rows are invisible*,
/// and the difference is a whole key. A tier ALIAS — `ALPACA_SANDBOX_API_KEY` beside its
/// `ALPACA_DEMO_API_KEY` twin — is filed `superseded_at IS NOT NULL` because
/// `credential_one_live_value` admits ONE live row per `(account_id, field)` and the two spellings
/// derive the same one (the retired schema-2 fill filed the second as an alias). Its
/// NAME, though, appears on no live row at all. A flat `superseded_at IS NULL` therefore DROPPED it
/// from the map — a name the operator wrote, present in the database, absent from
/// `crate::CredentialStore::read_all` and from `vike-cli secrets list`, with nothing saying so. So the `WHERE`
/// admits a superseded row whose name no live row carries, which is exactly the set the flat clause
/// was reaching for.
///
/// The §4.2 rollback copies a store may carry are untouched by that widening and cannot be reached
/// by it: a rollback copy was only ever written for a name that HAS a live row, so the `NOT EXISTS`
/// is false for every one of them.
///
/// ⚠ **It is a DEVIATION from §11's printed read** (*"`SELECT name, value FROM credential WHERE
/// superseded_at IS NULL` for the 50"*), and it is stated here rather than edited into a signed
/// spec. §11 is describing the mirror-period RENDERER over a store in which every name has a live
/// row, which was true of every shape that spec enumerates; the tier alias is a name that does not,
/// and the flat clause would answer for it with nothing. The two agree everywhere §11 was looking.
fn read_table_on(
    path: &Path,
    conn: &Connection,
    table: Table,
    scope: CredentialScope,
) -> Result<(SecretMap, usize), DbError> {
    // `table.sql_name()` is a `&'static str` from a closed enum — see `Table`. No operator input
    // reaches this string, and the `WHERE` below is a literal chosen by a match on the table.
    // Both clauses come from the helpers the SCOPED reader also calls, so the two cannot drift —
    // `read_table_scoped`'s doc carries what a divergence would cost.
    let predicate = live_predicate(table);
    let order = order_clause(table);
    // `(live) AND ` before the scope's conjunct, or nothing on a table with no live predicate.
    let base = if predicate.is_empty() { String::new() } else { format!("({predicate}) AND ") };
    let sql = match scope {
        // The unscoped statement, exactly as it read before the scope moved into this reader.
        CredentialScope::Full => {
            let live =
                if predicate.is_empty() { String::new() } else { format!(" WHERE {predicate}") };
            format!("SELECT name, value FROM {}{live}{order}", table.sql_name())
        }
        // The exclusion is in the `WHERE`, so a withheld row is never selected.
        CredentialScope::NoLiveKeys | CredentialScope::NoVenueKeys => format!(
            "SELECT name, value FROM {} WHERE {base}NOT {}{order}",
            table.sql_name(),
            withheld_sql(table, scope)
        ),
    };
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (name, value) = row.map_err(|e| DbError::sql(path, e))?;
        out.insert(name, value);
    }
    let withheld = match scope {
        CredentialScope::Full => 0,
        // `count(DISTINCT name)` over the same predicate: it reads no value either.
        CredentialScope::NoLiveKeys | CredentialScope::NoVenueKeys => {
            let count_sql = format!(
                "SELECT count(DISTINCT name) FROM {} WHERE {base}{}",
                table.sql_name(),
                withheld_sql(table, scope)
            );
            let n: i64 =
                conn.query_row(&count_sql, [], |r| r.get(0)).map_err(|e| DbError::sql(path, e))?;
            usize::try_from(n).unwrap_or(0)
        }
    };
    Ok((SecretMap::new(out), withheld))
}

/// **The rows [`CredentialScope::NoLiveKeys`] withholds** — a `WHERE` predicate over the
/// unaliased `credential` table: a row it matches is never selected under that scope.
///
/// Two halves, and neither reads a tier out of a NAME (decision 0156 item 6: the tier is written
/// in ONE place, `account.tier`):
///
/// * **every credential of an account whose row says `live`** — the account's own tier, by its
///   number;
/// * **the two venue families whose DEMO spelling is still real money or a real account**, by the
///   venue the name opens with: aster (no testnet credentials are configured, so even a read signs
///   against the real account) and polymarket (live funds). A legacy name (`ASTER_…`, `POLY_…`),
///   an account row the writer named (`aster#…`) and a venue row (`aster.…`) all match; that is a
///   VENUE read off a name, never a tier.
///
/// ⚠ `EXISTS`, never `account_id IN (…)`: a deployment or venue row has a NULL `account_id`, and
/// `NULL IN (…)` is NULL once any live account exists, so `NOT (…)` would drop that row from every
/// read without counting it as withheld.
///
/// `GLOB`, not `LIKE`: `_` is a wildcard to `LIKE` and a literal to `GLOB`. `upper(name)` makes the
/// match case-insensitive, which can only widen what is withheld.
/// `crates/vike-secrets/tests/reads/demo_only_scope.rs` plants both halves and reads them back.
pub(super) const DEMO_SCOPE_WITHHELD_SQL: &str = "(EXISTS (SELECT 1 FROM account \
     WHERE account.id = credential.account_id AND account.tier = 'live') \
     OR upper(credential.name) GLOB 'ASTER_*' OR upper(credential.name) GLOB 'ASTER#*' \
     OR upper(credential.name) GLOB 'ASTER.*' \
     OR upper(credential.name) GLOB 'POLY_*' OR upper(credential.name) GLOB 'POLYMARKET#*' \
     OR upper(credential.name) GLOB 'POLYMARKET.*')";

/// **Every account's and every venue's live credential rows, keyed by FIELD** — the one read a
/// composition root hands the mount (`crate::CredentialStore::credential_book`), under `scope`.
///
/// `superseded_at IS NULL` and nothing wider: `credential_one_live_value` admits ONE live row per
/// `(account_id, field)`, and that row is the account's credential. A retired row, a rollback copy
/// and a second spelling filed superseded are none of them the account's value. A row filed
/// against neither an account nor a venue (a DEPLOYMENT row) is not selected: it is read by NAME.
/// `ORDER BY id`, so a later row of one `(venue, field)` deterministically wins the fold.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub(crate) fn read_credential_book(
    path: &Path,
    scope: CredentialScope,
) -> Result<crate::account_credentials::CredentialBook, DbError> {
    read_book(path, scope, None)
}

/// **One account's and one venue's credential rows, keyed by FIELD** — [`read_credential_book`]'s
/// narrow twin (`crate::CredentialStore::credential_book_for`), for a root that has already chosen
/// WHICH account it needs and must not materialise any other: the `WHERE` names the account number
/// and the venue, so every other account's rows (a live account's included) are never SELECTED, under
/// whatever `scope` the handle has. `account` is `None` for "no account chosen": only the venue's
/// application rows answer.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub(crate) fn read_credential_book_for(
    path: &Path,
    account: Option<i64>,
    venue: &str,
    scope: CredentialScope,
) -> Result<crate::account_credentials::CredentialBook, DbError> {
    read_book(path, scope, Some((account, venue)))
}

/// The one statement behind [`read_credential_book`] and [`read_credential_book_for`]: `only` adds
/// the account/venue filter to the `WHERE` as bound parameters (`?1` the account number, `?2` the
/// venue name), so the two cannot select different shapes of row.
fn read_book(
    path: &Path,
    scope: CredentialScope,
    only: Option<(Option<i64>, &str)>,
) -> Result<crate::account_credentials::CredentialBook, DbError> {
    let conn = open_for_read(path)?;
    let link = crate::schema::VenueLink::of(&conn, "credential", "credential")
        .map_err(|e| DbError::sql(path, e))?;
    let scoped = scope_conjunct(Table::Credential, scope);
    let only_sql = if only.is_some() {
        format!(
            " AND (credential.account_id = ?1 OR {})",
            crate::schema::venue_is("credential.venue_id", "?2")
        )
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT credential.account_id, {}, credential.field, credential.value FROM credential {} \
         WHERE credential.superseded_at IS NULL \
           AND (credential.account_id IS NOT NULL OR credential.venue_id IS NOT NULL)\
           {only_sql}{scoped} \
         ORDER BY credential.id",
        link.name, link.join
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    let map = |r: &rusqlite::Row<'_>| {
        Ok((
            r.get::<_, Option<i64>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    };
    let rows = match only {
        Some((account, venue)) => stmt.query_map((account, venue), map),
        None => stmt.query_map([], map),
    }
    .map_err(|e| DbError::sql(path, e))?;
    let mut book = crate::account_credentials::CredentialBook::new();
    for row in rows {
        let (account, venue, field, value) = row.map_err(|e| DbError::sql(path, e))?;
        match (account, venue) {
            (Some(id), _) => book.insert_account(id, field, value),
            (None, Some(venue)) => book.insert_venue(venue, field, value),
            // A venue number naming no `venue` row: no owner to file it under.
            (None, None) => {}
        }
    }
    Ok(book)
}

/// **ONE field of the ONE active account of `(venue, label, tier)`** — the narrowest credential read
/// there is, for a root that needs one account's one value and must materialise nothing else (the
/// datahub's OANDA history token): only that row's value is selected, under `scope`.
///
/// `label` is `None` for the unlabelled account. `None` when no active row of that `(venue, label,
/// tier)` exists, when MORE THAN ONE does (two unlabelled accounts answer one address, and picking
/// either would be a guess), or when the field is absent or blank once trimmed. Never a tier read
/// out of a name (decision 0156 item 6): the account row's own `tier` column is the filter.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub(crate) fn read_account_field(
    path: &Path,
    venue: &str,
    label: Option<&str>,
    tier: vike_model::AccountTier,
    field: &str,
    scope: CredentialScope,
) -> Result<Option<String>, DbError> {
    let conn = open_for_read(path)?;
    let Some(id) = the_one_active_account(path, &conn, venue, label, tier)? else {
        return Ok(None);
    };
    let sql = format!(
        "SELECT value FROM credential WHERE account_id = ?1 AND field = ?2 \
         AND superseded_at IS NULL{}",
        scope_conjunct(Table::Credential, scope)
    );
    let value: Option<String> = conn
        .query_row(&sql, (id, field), |r| r.get(0))
        .optional()
        .map_err(|e| DbError::sql(path, e))?;
    Ok(value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
}

/// [`read_account_field`]'s PRESENCE twin — whether that one field holds a non-blank value, as a
/// boolean SQLite computes: the value column is never returned, so the value never becomes a Rust
/// value in this process (the same property and the same ASCII-whitespace residual as
/// [`read_present_names_scoped`]).
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub(crate) fn read_account_field_present(
    path: &Path,
    venue: &str,
    label: Option<&str>,
    tier: vike_model::AccountTier,
    field: &str,
    scope: CredentialScope,
) -> Result<bool, DbError> {
    let conn = open_for_read(path)?;
    let Some(id) = the_one_active_account(path, &conn, venue, label, tier)? else {
        return Ok(false);
    };
    let sql = format!(
        "SELECT coalesce(trim(value, {BLANK_PADDING_SQL}) <> '', 0) FROM credential \
         WHERE account_id = ?1 AND field = ?2 AND superseded_at IS NULL{}",
        presence_conjunct(Table::Credential, scope)
    );
    let present: Option<bool> = conn
        .query_row(&sql, (id, field), |r| r.get(0))
        .optional()
        .map_err(|e| DbError::sql(path, e))?;
    Ok(present == Some(true))
}

/// The `account.id` of the ONE active row of `(venue, label, tier)`, or `None` for none or more
/// than one — the address [`read_account_field`] and its presence twin share.
fn the_one_active_account(
    path: &Path,
    conn: &Connection,
    venue: &str,
    label: Option<&str>,
    tier: vike_model::AccountTier,
) -> Result<Option<i64>, DbError> {
    let accounts = format!(
        "SELECT a.id FROM account a WHERE a.active = 1 AND {} AND a.tier = ?2 AND a.label IS ?3",
        crate::schema::venue_is("a.venue_id", "?1")
    );
    let mut stmt = conn.prepare(&accounts).map_err(|e| DbError::sql(path, e))?;
    let ids = stmt
        .query_map((venue, tier.as_str(), label), |r| r.get::<_, i64>(0))
        .map_err(|e| DbError::sql(path, e))?
        .collect::<rusqlite::Result<Vec<i64>>>()
        .map_err(|e| DbError::sql(path, e))?;
    Ok(match ids.as_slice() {
        [id] => Some(*id),
        _ => None,
    })
}

/// **The NAMES of the live DEPLOYMENT rows** — `credential` rows filed against neither an account
/// nor a venue (the pager token, a data provider's key, an attribution code written by name), sorted,
/// under `scope`. Names only: no value is selected. What a by-NAME writer may rotate, told apart from
/// an account's or a venue's row by the row's own columns, never by the shape of its name.
///
/// # Errors
/// [`DbError`] when the database will not open or is not a schema this code reads.
pub(crate) fn read_deployment_names(
    path: &Path,
    scope: CredentialScope,
) -> Result<BTreeSet<String>, DbError> {
    let conn = open_for_read(path)?;
    let sql = format!(
        "SELECT name FROM credential WHERE superseded_at IS NULL \
         AND account_id IS NULL AND venue_id IS NULL{} ORDER BY name",
        scope_conjunct(Table::Credential, scope)
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| DbError::sql(path, e))?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| DbError::sql(path, e))?
        .collect::<rusqlite::Result<BTreeSet<String>>>()
        .map_err(|e| DbError::sql(path, e))?;
    Ok(names)
}
