//! **`stock_split`: the share splits of the stored series, one row per split of one series**
//! (`vike_model::StockSplit`; `docs/decisions/0289-a-backtest-split-adjusts-a-share-series-from-the-operators-split-rows.md`).
//!
//! Two writers fill it. `vike-cli data splits add|rm|ls` writes and reads the OPERATOR's rows on the box whose settings
//! database it booted from; the datahub's refresh (`vike-cli data catalog refresh`) writes the rows the VENDORS give
//! (Yahoo's list, FMP's confirmation: the owner's rulings of 2026-11-16, the decision's amendment), each row's `source`
//! naming which (`operator`, `yahoo`, `yahoo+fmp`). The datahub
//! serves every row with its kept instrument lists (`Request::InstrumentLists`), read at each ask, so a row is seen by
//! the next backtest without a restart. A backtest reading the series adjusts its prices before each split. Nothing is
//! compiled in.
//!
//! # What this module may write
//!
//! * **The operator's: one UPSERT or one DELETE of ONE row, and nothing else**, addressed by venue, series symbol and
//!   date; it links by `venue_id`, as every link in this store does. The operator's add takes over a vendor's row of
//!   the same date (its `source` becomes [`OPERATOR_SOURCE`]).
//! * **The vendors': the rows of ONE series whose `source` is not the operator's** ([`replace_sourced_splits`]), in
//!   one transaction: those the refresh's verdict no longer carries are deleted, the verdict's are inserted or their
//!   ratio or source updated. ⚠ **An operator's row is never touched**: a date the operator holds keeps the operator's
//!   ratio.
//! * **A store that does not exist is refused, never created**, and a row that is no split
//!   ([`StockSplit::effective_ms`]) is refused before the store is opened.
//! * **The operator's ticker renames: one UPSERT of ONE `ticker_rename` row** ([`write_ticker_rename`]), by venue and
//!   ticker: `FB` on `dukascopy` is listed as `META` now, so the refresh asks the vendors for `META`.
//! * **No version stamp and no change-journal record**, `crate::operator_instrument`'s argument: a split is not a
//!   credential, an account or a setting.

use std::path::Path;

use rusqlite::params;
use vike_model::StockSplit;

use crate::db::{StoreError, database_present, open_for_read};
use crate::operator_instrument::write_one;
use crate::store_locator::db_path_in;

/// The `source` of a row the operator typed in, and of every row a store held before the column existed.
pub const OPERATOR_SOURCE: &str = "operator";

/// **Add one split**, replacing the ratio of the row of the same venue, symbol and date, and taking it over as the
/// operator's when a vendor wrote it.
///
/// # Errors
/// A row that is no split, [`crate::StoreErrorKind::NoSettingsDatabase`] (none is created), a venue the store has no
/// `venue` row for, a busy store, or one at another schema. Nothing was written.
pub fn write_stock_split(settings_dir: &Path, split: &StockSplit) -> Result<(), StoreError> {
    split
        .effective_ms()
        .map_err(|why| StoreError::io(&db_path_in(settings_dir), std::io::Error::other(why)))?;
    write_one(settings_dir, |tx| {
        tx.execute(
            "INSERT INTO stock_split (venue_id, symbol, date, ratio, source) \
             VALUES ((SELECT id FROM venue WHERE name = ?1), ?2, ?3, ?4, ?5) \
             ON CONFLICT (venue_id, symbol, date) DO UPDATE SET ratio = excluded.ratio, source = excluded.source",
            params![split.venue, split.symbol, split.date, split.ratio, OPERATOR_SOURCE],
        )
    })
    .map(drop)
}

/// **Remove the one split of `symbol` on `venue` dated `date`** (`YYYY-MM-DD`), whoever wrote it. `Ok(false)` when
/// there is none. A vendor's row removed here is written again at the next refresh that reads it.
///
/// # Errors
/// As [`write_stock_split`].
pub fn delete_stock_split(
    settings_dir: &Path,
    venue: &str,
    symbol: &str,
    date: &str,
) -> Result<bool, StoreError> {
    write_one(settings_dir, |tx| {
        tx.execute(
            "DELETE FROM stock_split WHERE venue_id = (SELECT id FROM venue WHERE name = ?1) \
             AND symbol = ?2 AND date = ?3",
            params![venue, symbol, date],
        )
    })
    .map(|n| n > 0)
}

/// **Make the VENDOR rows of ONE series (`symbol` on `venue`) exactly `splits`**, the refresh's whole verdict for it,
/// each split with the vendor `source` it is written under (`yahoo`, `yahoo+fmp`), in one transaction:
/// `(written, removed)`, the rows inserted or whose ratio or source changed and the vendor rows the verdict no longer
/// carries. ⚠ An [`OPERATOR_SOURCE`] row is never written or removed, so a date the operator holds keeps the
/// operator's ratio; an empty verdict removes every vendor row of the series.
///
/// # Errors
/// A source that is blank or [`OPERATOR_SOURCE`], a split that is no split or names another series (each refused
/// before the store is opened), or as [`write_stock_split`]. Nothing was written.
pub fn replace_sourced_splits(
    settings_dir: &Path,
    venue: &str,
    symbol: &str,
    splits: &[(StockSplit, &str)],
) -> Result<(usize, usize), StoreError> {
    let refuse =
        |why: String| StoreError::io(&db_path_in(settings_dir), std::io::Error::other(why));
    for (split, source) in splits {
        if source.trim().is_empty() || *source == OPERATOR_SOURCE {
            return Err(refuse(format!(
                "`{source}` is no vendor: the operator's rows are written one at a time"
            )));
        }
        split.effective_ms().map_err(refuse)?;
        if split.venue != venue || split.symbol != symbol {
            return Err(refuse(format!(
                "{} {} is not the series {venue} {symbol} being replaced",
                split.venue, split.symbol
            )));
        }
    }
    let mut removed = 0;
    let written = write_one(settings_dir, |tx| {
        let held: Vec<String> = tx
            .prepare(
                "SELECT date FROM stock_split WHERE venue_id = (SELECT id FROM venue WHERE name = ?1) \
                 AND symbol = ?2 AND source <> ?3",
            )?
            .query_map(params![venue, symbol, OPERATOR_SOURCE], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for date in held.iter().filter(|d| !splits.iter().any(|(s, _)| s.date == **d)) {
            removed += tx.execute(
                "DELETE FROM stock_split WHERE venue_id = (SELECT id FROM venue WHERE name = ?1) \
                 AND symbol = ?2 AND date = ?3 AND source <> ?4",
                params![venue, symbol, date, OPERATOR_SOURCE],
            )?;
        }
        let mut written = 0;
        for (split, source) in splits {
            written += tx.execute(
                "INSERT INTO stock_split (venue_id, symbol, date, ratio, source) \
                 VALUES ((SELECT id FROM venue WHERE name = ?1), ?2, ?3, ?4, ?5) \
                 ON CONFLICT (venue_id, symbol, date) DO UPDATE SET ratio = excluded.ratio, source = excluded.source \
                 WHERE stock_split.source <> ?6 \
                 AND (stock_split.ratio <> excluded.ratio OR stock_split.source <> excluded.source)",
                params![venue, symbol, split.date, split.ratio, source, OPERATOR_SOURCE],
            )?;
        }
        Ok(written)
    })?;
    Ok((written, removed))
}

/// **Record that `ticker` on `venue` is listed as `renamed` now** (`FB` is `META`), replacing the row of the same
/// ticker: the split refresh asks Yahoo and FMP for `renamed` (decision 0289's amendment of 2026-11-17). A row naming
/// the ticker itself (`FB` as `FB`) renames nothing. Both are tickers as the vendors spell them: capitals, digits, `-`
/// and `.`, at most 20, since each rides a vendor's URL.
///
/// # Errors
/// A ticker outside that spelling (refused before the store is opened), or as [`write_stock_split`].
pub fn write_ticker_rename(
    settings_dir: &Path,
    venue: &str,
    ticker: &str,
    renamed: &str,
) -> Result<(), StoreError> {
    let spelled = |t: &str| {
        (1..=20).contains(&t.len())
            && t.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    };
    if let Some(bad) = [ticker, renamed].into_iter().find(|t| !spelled(t)) {
        let why = format!("`{bad}` is no ticker: capitals, digits, `-` and `.`, at most 20");
        return Err(StoreError::io(&db_path_in(settings_dir), std::io::Error::other(why)));
    }
    write_one(settings_dir, |tx| {
        tx.execute(
            "INSERT INTO ticker_rename (venue_id, ticker, renamed) \
             VALUES ((SELECT id FROM venue WHERE name = ?1), ?2, ?3) \
             ON CONFLICT (venue_id, ticker) DO UPDATE SET renamed = excluded.renamed",
            params![venue, ticker, renamed],
        )
    })
    .map(drop)
}

/// **Every ticker rename the store holds**, `(venue, ticker, renamed)`, ordered by venue and ticker. Empty for a box
/// with no store and for a store no writer has carried this table into.
///
/// # Errors
/// A store that exists and cannot be read: never an empty answer in its place, since the refresh would then ask for
/// the old ticker, which may name another share now.
pub fn read_ticker_renames(
    settings_dir: &Path,
) -> Result<Vec<(String, String, String)>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db)? {
        return Ok(Vec::new());
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Vec<(String, String, String)>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ticker_rename')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(Vec::new());
        }
        conn.prepare(
            "SELECT v.name, t.ticker, t.renamed FROM ticker_rename t JOIN venue v ON v.id = t.venue_id \
             ORDER BY v.name, t.ticker",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
    };
    query().map_err(|e| StoreError::sql(&db, e))
}

/// **Every split the store holds**, every venue, ordered by venue, symbol and date. Empty for a box with no store
/// and for a store no writer has carried this table into. Rows are returned as stored; each reader judges one with
/// [`StockSplit::effective_ms`].
///
/// # Errors
/// A store that exists and cannot be read: never an empty answer in its place, since a backtest would then run
/// unadjusted over a series it holds a split for.
pub fn read_stock_splits(settings_dir: &Path) -> Result<Vec<StockSplit>, StoreError> {
    read_sourced_stock_splits(settings_dir).map(|rows| rows.into_iter().map(|(s, _)| s).collect())
}

/// [`read_stock_splits`] with each row's `source`: [`OPERATOR_SOURCE`] or the vendor a refresh read it from (a store
/// no writer has carried the column into reads [`OPERATOR_SOURCE`], what its rows are).
///
/// # Errors
/// As [`read_stock_splits`].
pub fn read_sourced_stock_splits(
    settings_dir: &Path,
) -> Result<Vec<(StockSplit, String)>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db)? {
        return Ok(Vec::new());
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Vec<(StockSplit, String)>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'stock_split')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(Vec::new());
        }
        // A READ-ONLY reader selects the column only where a writer carried it in (`crate::db::ensure_venue_rows`).
        let source = if crate::settings::has_column(&conn, "stock_split", "source")? {
            "s.source".to_string()
        } else {
            format!("'{OPERATOR_SOURCE}'")
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT v.name, s.symbol, s.date, s.ratio, {source} FROM stock_split s JOIN venue v ON v.id = s.venue_id \
             ORDER BY v.name, s.symbol, s.date"
        ))?;
        stmt.query_map([], |r| {
            Ok((
                StockSplit {
                    venue: r.get(0)?,
                    symbol: r.get(1)?,
                    date: r.get(2)?,
                    ratio: r.get(3)?,
                },
                r.get(4)?,
            ))
        })?
        .collect()
    };
    query().map_err(|e| StoreError::sql(&db, e))
}
