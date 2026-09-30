//! **Decision 0095's store migration — a `live` ceiling stops meaning "permitted" and starts
//! meaning MAINNET.**
//!
//! Before decision 0095, `live` on binance, bybit, okx and hyperliquid meant DEMO unless the process
//! exported `{VENUE}_MAINNET=1`; the four switches are deleted and the ceiling alone chooses the
//! network. So every `live` row of those four venues — the venue line and every account line — is
//! rewritten to `demo`, which is what it traded on every box that never set the switch (every box
//! measured). Aster and polymarket rows are untouched: their `live` already meant real money.
//!
//! # When it runs, and the one rule
//!
//! It must be applied before ANY binary reads a ceiling under the new meaning, and before any write
//! (so an operator's deliberate `live`, written after it, can never be undone by it). Two paths:
//!
//! * [`apply_live_means_mainnet`] — explicit, journalled. `vike_boot::boot` calls it for every root
//!   that reads the ceilings, before step 4 reads them; `vike-cli config migrate-store` calls it
//!   when an operator is told to.
//! * [`migrate_live_means_mainnet`] — the same pass inside the write funnel
//!   (`crate::db::ensure_venue_id_columns`), so no write can precede it even from a writer that
//!   never booted. That path records the rewrite in the `store_migration` row, not in the change
//!   journal (it has no process identity to stamp); every production writer boots first, so the
//!   journalled path is the one that runs.
//!
//! It runs ONCE per store: the `store_migration` row named [`LIVE_MEANS_MAINNET`] is its marker. The
//! table is this module's own DDL (the `crate::profile_store` precedent), not `crate::schema::DDL`,
//! so the schema version and its readers are unchanged and a pre-0095 binary still opens the store.
//!
//! ⚠ **Declared residual:** a pre-0095 binary that WRITES a `live` ceiling for one of the four
//! venues into a migrated store writes old-meaning `live`; the next post-0095 boot reads it as
//! mainnet. Only a rollback plus a hand write reaches it.

use std::path::Path;

use rusqlite::{Connection, Transaction};
use vike_model::change_journal::{Actor, Change, Outcome, Proc};

use crate::db::DbError;

/// The migration's name — its row in `store_migration`.
pub const LIVE_MEANS_MAINNET: &str = "0095-live-means-mainnet";

/// The venues whose `live` meant demo unless `{VENUE}_MAINNET=1` was exported. Spelled here because
/// this crate (layer 15) cannot name `vike-bridge-core`, where the switch table lived.
pub const SWITCHED_VENUES: [&str; 4] = ["binance", "bybit", "okx", "hyperliquid"];

/// The reason every journal record of this migration carries.
pub const REASON: &str = "decision 0095: a `live` ceiling now means MAINNET for binance, bybit, okx \
     and hyperliquid; before it, `live` there meant demo unless {VENUE}_MAINNET=1 was exported, so \
     this row is rewritten to `demo`, what it traded. To trade mainnet on purpose: `vike-cli config \
     set <this key> live`";

const MIGRATION_DDL: &str = "CREATE TABLE IF NOT EXISTS store_migration (
    name       TEXT PRIMARY KEY,
    applied_ms INTEGER NOT NULL,
    reason     TEXT NOT NULL,
    rewrites   TEXT NOT NULL
) STRICT;";

/// One ceiling row the migration rewrote from `live` to `demo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CeilingRewrite {
    /// The roster venue.
    pub venue: String,
    /// The account label, or `None` for the venue line.
    pub label: Option<String>,
}

impl CeilingRewrite {
    /// The dotted settings key an operator types for this row.
    #[must_use]
    pub fn key(&self) -> String {
        match &self.label {
            None => format!("policy.venues.{}", self.venue),
            Some(label) => format!("policy.accounts.{}.{label}", self.venue),
        }
    }
}

/// What [`apply_live_means_mainnet`] did.
#[derive(Debug)]
pub enum LiveMeansMainnet {
    /// No settings database on this box.
    NoDatabase,
    /// Already applied, or nothing on this store for it to rewrite.
    NotPending,
    /// Applied now. `journal_error` is `Some` when the store changed and the journal did not.
    Applied { rewrites: Vec<CeilingRewrite>, journal_error: Option<crate::JournalAppendError> },
}

fn table_present(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

fn marked(conn: &Connection) -> rusqlite::Result<bool> {
    if !table_present(conn, "store_migration")? {
        return Ok(false);
    }
    conn.query_row(
        "SELECT COUNT(*) FROM store_migration WHERE name = ?1",
        [LIVE_MEANS_MAINNET],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

fn live_rows(conn: &Connection) -> rusqlite::Result<Vec<CeilingRewrite>> {
    if !table_present(conn, "venue_arming")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT venue, label FROM venue_arming WHERE mode = 'live' ORDER BY venue, label",
    )?;
    let rows = stmt.query_map([], |r| Ok(CeilingRewrite { venue: r.get(0)?, label: r.get(1)? }))?;
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if SWITCHED_VENUES.contains(&row.venue.as_str()) {
            out.push(row);
        }
    }
    out.sort_by_key(CeilingRewrite::key);
    Ok(out)
}

/// Whether this store holds a `live` row of a switched venue that decision 0095's migration has not
/// yet rewritten. READ-ONLY: a binary that cannot write the store can still ask.
///
/// # Errors
/// A store that exists and cannot be read.
pub fn live_means_mainnet_pending(settings_dir: &Path) -> Result<bool, DbError> {
    let db = crate::dotenv::db_path_in(settings_dir);
    if !crate::db::database_present(&db) {
        return Ok(false);
    }
    let (conn, _version) = crate::db::open_for_read(&db)?;
    let pending = marked(&conn).and_then(|m| Ok(!m && !live_rows(&conn)?.is_empty()));
    pending.map_err(|e| DbError::sql(&db, e))
}

/// The pass itself, inside the caller's transaction. A no-op on a marked store. Recomputes
/// `account.armed` when it rewrote anything, since that column is derived from these rows.
pub(crate) fn migrate_live_means_mainnet(
    tx: &Transaction<'_>,
) -> rusqlite::Result<Vec<CeilingRewrite>> {
    tx.execute_batch(MIGRATION_DDL)?;
    if marked(tx)? {
        return Ok(Vec::new());
    }
    let rewrites = live_rows(tx)?;
    // `live_rows` above already returned early when the table is absent, so a plain guard (rather
    // than an `or_else` probing again per venue) is enough here.
    if table_present(tx, "venue_arming")? {
        for venue in SWITCHED_VENUES {
            tx.execute(
                "UPDATE venue_arming SET mode = 'demo' WHERE mode = 'live' AND venue = ?1",
                [venue],
            )?;
        }
    }
    let detail = rewrites.iter().map(CeilingRewrite::key).collect::<Vec<_>>().join(",");
    tx.execute(
        "INSERT INTO store_migration (name, applied_ms, reason, rewrites) \
         VALUES (?1, CAST(strftime('%s', 'now') AS INTEGER) * 1000, ?2, ?3)",
        (LIVE_MEANS_MAINNET, REASON, detail),
    )?;
    if !rewrites.is_empty() {
        crate::settings::fold_arming_into_accounts(tx)?;
    }
    Ok(rewrites)
}

/// **Apply decision 0095's migration, journalled** — one `set_setting` record per rewritten row, with
/// [`REASON`], in the change journal beside the store.
///
/// Probes read-only first, so a store with nothing pending is never opened for writing: a binary
/// whose sandbox cannot write `settings/db` still boots on a migrated store.
///
/// # Errors
/// A store that cannot be read, or — when a migration is pending — cannot be written.
pub fn apply_live_means_mainnet(
    settings_dir: &Path,
    actor: Actor,
    proc: Proc,
    now_ms: i64,
) -> Result<LiveMeansMainnet, DbError> {
    let db = crate::dotenv::db_path_in(settings_dir);
    if !crate::db::database_present(&db) {
        return Ok(LiveMeansMainnet::NoDatabase);
    }
    if !live_means_mainnet_pending(settings_dir)? {
        return Ok(LiveMeansMainnet::NotPending);
    }
    // `_created`: `database_present` above already confirmed the file exists, so a fresh CREATE
    // here means it vanished between that probe and this open. Unlike `set_venue_account_id`'s
    // guard against the same race, this migration is safe either way — an accidentally-created,
    // empty store has no `live` rows to rewrite, so it is marked applied against nothing rather
    // than corrupted — so this reads the flag and does not act on it.
    let (mut conn, _created, _version) = crate::db::open_for_write(&db)?;
    // IMMEDIATE, not the default DEFERRED: `migrate_live_means_mainnet` below reads
    // (`marked`/`live_rows`) BEFORE it writes, inside this one transaction — the exact DEFERRED
    // read-then-write shape `crate::db::BUSY_TIMEOUT`'s own doc names as the case a busy timeout
    // cannot cover, because SQLite refuses a SHARED-to-RESERVED promotion with `SQLITE_BUSY`
    // immediately rather than consulting the busy handler. Two roots booting onto the same
    // pre-0095 store at once is the ordinary case this migration exists for, not a rare one.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(&db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(&db, e))?;
    // FIRST, so this call sees the rows it rewrites; the funnel below then finds the marker.
    let rewrites = migrate_live_means_mainnet(&tx).map_err(|e| DbError::sql(&db, e))?;
    crate::db::ensure_venue_id_columns(&tx).map_err(|e| DbError::sql(&db, e))?;
    tx.commit().map_err(|e| DbError::sql(&db, e))?;

    let journal = crate::store::journal_beside(settings_dir, proc);
    let mut journal_error = None;
    for r in &rewrites {
        let change = Change::set_setting(
            Outcome::Applied,
            actor.clone(),
            "policy",
            &r.key(),
            Some("live"),
            "demo",
        )
        .with_reason(Some(REASON));
        if let Err(source) = journal.append(now_ms, &change) {
            journal_error =
                Some(crate::JournalAppendError { dir: journal.dir().to_path_buf(), source });
            break;
        }
    }
    Ok(LiveMeansMainnet::Applied { rewrites, journal_error })
}

/// Test support: remove the migration's marker, so a planted store reads as one a PRE-0095 binary
/// left (a fixture plants rows through the write funnel, which applies the migration first).
///
/// # Panics
/// On any store error — a fixture, not a production path.
#[cfg(feature = "test-support")]
pub fn unmark_live_means_mainnet(settings_dir: &Path) {
    let db = crate::dotenv::db_path_in(settings_dir);
    let conn = Connection::open(&db).expect("open the settings database");
    conn.execute_batch(MIGRATION_DDL).expect("the marker table");
    conn.execute("DELETE FROM store_migration WHERE name = ?1", [LIVE_MEANS_MAINNET])
        .expect("unmark");
}
