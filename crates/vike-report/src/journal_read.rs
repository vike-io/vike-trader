//! `fills_from_journal` — read the account-affecting `FillEvent`s out of a vike-core command
//! journal directory, preserving journal (chronological) order.
//!
//! The journal records every exec-lane message; only the bare `Ingest::Event(Event::Fill(_))`
//! records affect the account (this is exactly the filter `vike_exec::Account::fold` applies —
//! `OrderSubmitted`/`Accepted`/`Filled` lifecycle events are journaled too but carry no
//! position math, so they are dropped here). The extracted fills feed
//! [`vike_analytics::reconstruct_trades`], and [`tearsheet_from_journal`] runs the whole door.
//!
//! The journal is the ONE source of fills on THIS door, and its directory is named by the caller:
//! the `tearsheet` CLI's `--journal DIR` (`crates/vike/src/tearsheet_cli.rs`), or the directory a
//! daemon resolved from its `config.journal_dir` row. There is no store FALLBACK here:
//! `fills_from_store` and its row converter were deleted on 2026-10-10
//! (`docs/decisions/0117-there-are-no-migrations.md`), as no binary ever called them. This crate
//! names no store at all; the desktop's Tearsheet reads the datahub's `kind=exec_fill` rows itself,
//! converts them with `vike_data::ExecFillRow::to_fill_event` and folds them through
//! [`tearsheet_from_fills`].

use std::fmt;
use std::path::Path;

use vike_analytics::{LiveTearsheet, equity_curve_from_trades, reconstruct_trades};
use vike_exec::Ingest;
use vike_journal::{CommandJournal, JournalRecord};
use vike_model::events::{Event, FillEvent};

/// Error reading fills from a journal directory.
#[derive(Debug)]
pub enum JournalReadError {
    /// The underlying [`CommandJournal::read_all`] I/O / format error.
    Io(std::io::Error),
}

impl fmt::Display for JournalReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalReadError::Io(e) => write!(f, "reading command journal: {e}"),
        }
    }
}

impl std::error::Error for JournalReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JournalReadError::Io(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for JournalReadError {
    fn from(e: std::io::Error) -> Self {
        JournalReadError::Io(e)
    }
}

/// Read every account-affecting `FillEvent` from the journal segments under `dir`, in journal
/// (chronological) order. Non-fill records (lifecycle events, snapshots, strategy submits,
/// commands) are skipped. A torn journal tail stops the read cleanly (that is
/// `CommandJournal::read_all`'s contract) — the fills up to the tear are returned.
pub fn fills_from_journal(dir: &Path) -> Result<Vec<FillEvent>, JournalReadError> {
    let records = CommandJournal::read_all(dir)?;
    let fills = records
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::Cmd { msg: Ingest::Event(Event::Fill(f)), .. } => Some(f),
            _ => None,
        })
        .collect();
    Ok(fills)
}

/// Read a command journal directory, reconstruct trades from its fill stream, build the
/// realized-only fallback equity curve from `seed`, and assemble the tearsheet.
///
/// This is the primary live-tearsheet entry point. The equity curve is realized-only (derived
/// from trade PnLs); when a stored `kind=equity` series exists, the `tearsheet` CLI's `--datahub` path
/// (`crates/vike/src/tearsheet_cli.rs`) reads it instead, for a true
/// mark-to-market curve.
///
/// A free function, not a `LiveTearsheet` method: an inherent `impl` may only be written in the
/// crate that owns the type (`vike-analytics`). `crates/vike/src/tearsheet_cli.rs`'s realized-only
/// path inlines the same four steps so the curve stays in hand for `--html`.
pub fn tearsheet_from_journal(
    dir: &Path,
    seed: f64,
    periods_per_year: f64,
) -> Result<LiveTearsheet, JournalReadError> {
    let fills = fills_from_journal(dir)?;
    let trades = reconstruct_trades(&fills);
    let (equity, ts) = equity_curve_from_trades(seed, &trades);
    Ok(LiveTearsheet::from_result_parts(None, trades, equity, ts, periods_per_year))
}

/// The tearsheet over fills a caller already holds — the desktop's, converted from the stored
/// `kind=exec_fill` rows it read from the datahub (`vike_data::ExecFillRow::to_fill_event`; the row
/// shape is vike-data's and this crate names no store). Put in time order first (a reader that holds
/// them newest-first, as the GUI does, hands them over as it holds them), then folded exactly as
/// [`tearsheet_from_journal`] folds a journal's: realized-only equity from `seed`, every metric from
/// `vike_analytics`.
pub fn tearsheet_from_fills(
    mut fills: Vec<FillEvent>,
    seed: f64,
    periods_per_year: f64,
) -> LiveTearsheet {
    fills.sort_by_key(|f| f.ts);
    let trades = reconstruct_trades(&fills);
    let (equity, ts) = equity_curve_from_trades(seed, &trades);
    LiveTearsheet::from_result_parts(None, trades, equity, ts, periods_per_year)
}

#[path = "journal_read_tests.rs"]
#[cfg(test)]
mod journal_read_tests;
