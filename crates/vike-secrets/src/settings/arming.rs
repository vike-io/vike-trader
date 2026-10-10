//! Table probes and the `account.armed` fold: `fold_arming_into_accounts` (spec section 9 stage 3).

use super::*;

/// Does this database carry `name` as a TABLE?
///
/// The settings tables are deliberately NOT gated on [`crate::SCHEMA_VERSION`] — see
/// [`crate::schema::DDL`]'s note, where a bump is measured as the live gate for two already-migrated
/// boxes — so *is it there* is asked of `sqlite_master` rather than of a number. `name` is a
/// `&'static str` from this module's own call sites, never operator input.
pub(super) fn table_exists(
    db: &Path,
    conn: &rusqlite::Connection,
    name: &str,
) -> Result<bool, DbError> {
    let found: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |r| r.get(0),
        )
        .map_err(|e| DbError::sql(db, e))?;
    Ok(found > 0)
}

/// **Does `table` carry `column`?** — asked of the live schema, never assumed from
/// [`crate::schema::DDL`].
///
/// ⚠ **The DDL cannot answer this and that is the whole reason the function exists.** Every table
/// there is `CREATE TABLE IF NOT EXISTS`, which does exactly nothing to a table that is already
/// present — so a column added to an existing table's DDL reaches a store BORN after the change and
/// no store born before it. Both boxes migrated on 2026-09-14 are stores born before, and their
/// `venue_arming` was itself created by a later `config mirror` run through that same `IF NOT
/// EXISTS` batch. A reader that selected an assumed column would fail on them with a SQL error
/// where the honest answer is `None`.
///
/// `pub(crate)` and in the bare `rusqlite::Result` currency rather than [`DbError`]:
/// [`crate::db::ensure_venue_rows`] needs this exact check and sits inside
/// [`crate::db::fill_into`]'s transaction, which is `rusqlite::Result` throughout — `DbError` needs
/// a `path` that call has no cheap way to attach at that depth, and the wrap belongs once at the
/// outer boundary, exactly as its errors are wrapped by [`crate::db`]'s `write_pending`. Both
/// callers of this function INSIDE this module already hold `db` and attach it themselves with
/// [`DbError::sql`].
pub(crate) fn has_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let name: String = r.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Add `venue_arming.max_exposure` where a store predates it. IDEMPOTENT, and a no-op on every
/// store born with the column.
///
/// ⚠ A plain `ADD COLUMN` rather than the rebuild-copy `credential` needed: that one had to change a
/// PRIMARY KEY, which SQLite's `ALTER TABLE` cannot do. Adding a NULLABLE column with no default is
/// the case `ALTER TABLE` handles directly and without rewriting a row, so there is nothing here to
/// interrupt halfway. The column's DATA needs no migration either — [`write_settings`] deletes and
/// re-inserts both tables on every run, so the next mirror fills it from the files.
pub(super) fn ensure_arming_columns(
    db: &Path,
    tx: &rusqlite::Transaction<'_>,
) -> Result<(), DbError> {
    if !has_column(tx, "venue_arming", "max_exposure").map_err(|e| DbError::sql(db, e))? {
        tx.execute_batch("ALTER TABLE venue_arming ADD COLUMN max_exposure REAL;")
            .map_err(|e| DbError::sql(db, e))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// `account.armed` — the arming rows, folded onto the rows they arm (spec §9 stage 3)
// ---------------------------------------------------------------------------------------------

/// `paper`, the mode word this crate may not import from `vike_config::VenueMode::as_str`.
const PAPER: &str = "paper";

/// **Does this store carry `table`?** — the `rusqlite::Result` twin of [`table_exists`], which
/// needs a `db: &Path` in order to build a [`DbError`] its callers want and this one has not got.
/// Same question, same `sqlite_master` probe, and deliberately not a second ANSWER: both ask the
/// live schema rather than assuming [`crate::schema::DDL`].
fn table_present(conn: &rusqlite::Connection, name: &str) -> rusqlite::Result<bool> {
    let found: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(found > 0)
}

/// **The arming vocabulary's ORDER, as a rank.** `paper < demo < live`.
///
/// ⚠ It exists because the words do NOT sort that way: lexically `'demo' < 'live' < 'paper'`, so a
/// `WHERE mode > 'paper'` — or a `.min()` over `&str` — arms NOTHING and reads as a clean pass.
/// The rank is the same order `vike_config::VenueMode`'s derived `Ord` carries, which is the
/// authority; this crate declares no edge to it (layer 15 against layer 20) and
/// `crates/vike-config/tests/no_ceiling_widens_across_the_migration.rs` is what holds the two
/// equal. An unrecognised word ranks as `paper`, which is the same disposition
/// `VenuePolicy::get` takes for a venue it does not carry — the safe direction, and unreachable
/// anyway while `crate::schema::DDL`'s `CHECK (mode IN ('paper', 'demo', 'live'))` stands.
fn mode_rank(mode: &str) -> u8 {
    match mode {
        "live" => 2,
        "demo" => 1,
        _ => 0,
    }
}

/// `vike_config::VenueMode::cap` over the stored words — *the effective tier is the LOWER of the
/// operator's ceiling and whatever was decided*, `min` and never `max`.
fn cap<'a>(ceiling: &'a str, stated: &'a str) -> &'a str {
    if mode_rank(stated) < mode_rank(ceiling) { stated } else { ceiling }
}

/// **One account's ceiling, reproduced from the stored arming rows** — the three arms of
/// `crates/vike-config/src/venue_mode.rs`'s `VenuePolicy::account`, which this crate cannot call.
///
/// ```text
/// (Some(stated), _)   => venue_ceiling.cap(stated)   a labelled account's own line, CAPPED
/// (None, default)     => venue_ceiling               the default account INHERITS
/// (None, labelled)    => paper                       a labelled one does NOT
/// ```
///
/// ⚠ **All three arms are load-bearing and this file has watched each of the other spellings arm
/// an account nobody armed.** Reading the venue row for a LABELLED account arms a
/// `{VENUE}_LIVE_API_KEY__ALT` that no `[accounts]` line ever named; reading the labelled row
/// WITHOUT the cap arms an `ALT = "live"` line that its venue's own `demo` line capped on read.
/// `crates/vike-config/tests/no_ceiling_widens_across_the_migration.rs` runs both as kill proofs
/// beside the shipped fold.
fn account_ceiling<'a>(
    venue_modes: &'a BTreeMap<String, String>,
    labelled: &'a BTreeMap<(String, String), String>,
    venue: &str,
    label: Option<&str>,
) -> &'a str {
    // An absent venue row is `paper`: `VenuePolicy::get`'s *"an id the roster does not carry
    // answers `VenueMode::Paper` — the safe answer"*, and the same answer a box with no `[venues]`
    // table already resolves.
    let venue_ceiling = venue_modes.get(venue).map_or(PAPER, String::as_str);
    let Some(label) = label else { return venue_ceiling };
    match labelled.get(&(venue.to_string(), label.to_string())) {
        Some(stated) => cap(venue_ceiling, stated),
        None => PAPER,
    }
}

/// **Derive `account.armed` from the `venue_arming` rows.** IDEMPOTENT, and a pure function of the
/// two tables it reads.
///
/// `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §9 stage 3, §5.2 step 5.
/// An account is ARMED exactly where the operator's own mode, resolved AT THAT ACCOUNT'S LEVEL of
/// the policy, names the tier the account already carries — so the new model's `armed ? tier :
/// paper` answers what the old model's `min(venue line, account line)` answered, and **it cannot
/// widen by construction**: an armed row comes out at a tier its own ceiling already allowed, and
/// a disarmed one comes out `paper`. It narrows exactly where a venue held several tiers and only
/// one was mounted; those rows were not trading before either, and `tier` is untouched, so
/// re-arming one is a policy line rather than a repair.
///
/// # ⚠ Every row is UPDATEd by `id`, never by its cells
///
/// Two dukascopy books share `(dukascopy, demo, NULL)` — SQLite's NULLs are distinct in an index,
/// so `UNIQUE (venue, tier, label)` admits both and that is the real shape of a real store. A
/// statement keyed on that tuple would write both rows from one account's answer. `id` is the
/// ruled identity, and here it is also the only key that separates them.
///
/// # ⚠ What this does NOT do: it does not drop `venue_arming`, and §3's *DELETED* is not this
/// stage
///
/// The table stays the SOURCE, and this column is its derived half — the shape stage 2 already
/// shipped for `venue_id` and the DDL's own doc called *a dual-write half with no reader yet*
/// (until the venue-links plan moved every reader onto it). **RULED here rather than assumed**,
/// because the spec's §5.2 step 5 reads as though the
/// table goes in one act:
///
/// * The settings mirror (`crates/vike-config/src/mirror.rs`, before `docs/decisions/0086` retired
///   its file-to-row direction) wrote one arming row per ROSTER venue whenever `[venues]` was
///   declared, and one per `[account_exposure]` figure. An `account` row exists only where a
///   CREDENTIAL minted one. So a venue an operator has declared and not
///   yet credentialled — and every `max_exposure` figure filed against one — has nowhere to live
///   in `account`, and **an absent figure means UNBOUNDED**: moving the exposure ceiling in this
///   stage would WIDEN it, in the one stage annotated as the one that must not widen, and §5.3's
///   gate compares venue MODES and would not see it.
/// * The same hole reaches the read path. `read_settings` is what `vike_config::apply_rows` builds
///   `VenuePolicy` from, so unfolding these rows back out of `account` alone would drop a
///   declared venue's line, `VenuePolicy::is_declared` with it on a box whose venues are all
///   uncredentialled, and the adoption seal's `arming_rows` ERASE detector would read the loss as
///   rows moved by some other route.
///
/// So `max_exposure` does NOT move in this stage and `venue_arming` is NOT dropped; the three
/// `table_exists(…, "venue_arming")` guards in this module are therefore correct as they stand.
/// What must land before the table can go is a home for a ceiling that names no account —
/// which is a question §3's schema does not answer today.
///
/// # Where it is called
///
/// Everywhere either input changes, which is what makes the column derived rather than stored:
/// [`write_settings`] (the mirror rewrites every arming row), `crate::db::fill_into` (the store's
/// creation) and `crate::db::commit_account_write` (the account verbs). Cheap by
/// construction — both tables are tens of rows on a real box.
pub(crate) fn fold_arming_into_accounts(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    // The column, where a store predates it. Same `ALTER TABLE` case `ensure_arming_columns`
    // argues for: SQLite adds a column with a NON-NULL default directly, without rewriting a row.
    // ⚠ The table-level `CHECK (armed IN (0, 1))` cannot ride an `ADD COLUMN`, so an older store
    // carries the column without it — see `crate::schema::DDL`'s own note.
    // ⚠ The `account` guard is DEFENSIVE and unreachable through any caller today — all three run
    // `crate::schema::DDL`'s `CREATE TABLE IF NOT EXISTS` batch first. It is here because the
    // failure it removes is not a refusal: without it, a caller that did not would reach the
    // `ALTER TABLE` below and take a bare `no such table: account` out of the engine, on a path
    // whose whole currency is `rusqlite::Result`.
    if !table_present(tx, "account")? {
        return Ok(());
    }
    if !has_column(tx, "account", "armed")? {
        tx.execute_batch("ALTER TABLE account ADD COLUMN armed INTEGER NOT NULL DEFAULT 0;")?;
    }
    // A store with no `venue_arming` table states no arming at all, and there is nothing to derive
    // FROM. Leaving every bit at its default `0` is the truthful reading — the same one
    // `read_settings` takes of a store whose tables predate it — and never a reason to error.
    if !table_present(tx, "venue_arming")? {
        return Ok(());
    }

    // Both tables are read by the venue's NUMBER (`crate::schema::VenueLink`), with the text only
    // for a `venue_arming` row whose number names no `venue` row; the link answers for one rather
    // than skipping it.
    let mut venue_modes: BTreeMap<String, String> = BTreeMap::new();
    let mut labelled: BTreeMap<(String, String), String> = BTreeMap::new();
    {
        let link = crate::schema::VenueLink::of(tx, "venue_arming", "t")?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {}, t.label, t.mode FROM venue_arming t {}",
            link.name, link.join
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (venue, label, mode) = row?;
            match label {
                None => {
                    venue_modes.insert(venue, mode);
                }
                Some(label) => {
                    labelled.insert((venue, label), mode);
                }
            }
        }
    }

    let accounts: Vec<(i64, String, String, Option<String>)> = {
        let link = crate::schema::VenueLink::of(tx, "account", "a")?;
        let mut stmt = tx.prepare(&format!(
            "SELECT a.id, {}, a.tier, a.label FROM account a {}",
            link.name, link.join
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out
    };

    let mut update = tx.prepare("UPDATE account SET armed = ?1 WHERE id = ?2")?;
    for (id, venue, tier, label) in accounts {
        let ceiling = account_ceiling(&venue_modes, &labelled, &venue, label.as_deref());
        // ⚠ A DIRECT comparison since §4.4's rename, and it was a mapped one — `mode_word_of_tier`,
        // deleted with this line's change. `account.tier` spelled "no real broker connection"
        // `sim` while `venue_arming.mode` spelled it `paper`, so a fold comparing the two columns
        // raw matched NOTHING while looking like a clean pass. One word for one idea (ruling 7)
        // removes the map rather than fixing it; `crate::schema::ACCOUNT_TIERS` and
        // `vike_config::VenueMode`'s vocabulary are now the same three words.
        let armed = ceiling == tier;
        update.execute(rusqlite::params![i64::from(armed), id])?;
    }
    Ok(())
}
