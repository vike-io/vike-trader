//! `fills_from_journal` — read the account-affecting `FillEvent`s out of a command journal
//! directory, preserving journal (chronological) order.
//!
//! The journal records every exec-lane message; only the bare `Ingest::Event(Event::Fill(_))`
//! records affect the account (this is exactly the filter `vike_execution::Account::fold` applies —
//! `OrderSubmitted`/`Accepted`/`Filled` lifecycle events are journaled too but carry no
//! position math, so they are dropped here). The extracted fills feed the tearsheet fold,
//! `vike_analytics::trading_results_table_from_fills` with `TradingResultsTableFillOrder::AsHeld`: this crate does the READ and
//! names no analytics, the fold names no journal, and each caller composes the two. ⚠ No shipped binary
//! calls it since 2026-11-24: its last two doors, the `tearsheet` CLI and the tradehub's live-journal report
//! verb, are retired (a live account's report reads the datahub, `vike_client::account_report`), and what reads
//! it now is the two-doors pin (`crates/vike-datahub/tests/default_build/execution_fills_two_doors.rs`).
//!
//! The journal is the ONE source of fills on THIS door, and its directory is named by the caller:
//! the `tearsheet` CLI's `--journal DIR`, or the directory a daemon resolved from its
//! `config.journal_dir` row. There is no store FALLBACK here: `fills_from_store` and its row
//! converter were deleted on 2026-10-10 (`docs/decisions/0117-there-are-no-migrations.md`), as no
//! binary ever called them. This crate names no store at all; the desktop's Tearsheet reads the
//! datahub's `kind=exec_fill` rows itself, converts them with
//! `vike_data::ExecFillRow::fill_events` and folds them through
//! `vike_analytics::trading_results_table_from_fills` with `TradingResultsTableFillOrder::ByTime`. The two doors fold a fee correction
//! through one `vike_model::events::FeeCorrections` and keep `position_side`, so one journal gives one
//! sheet (`crates/vike-datahub/tests/default_build/execution_fills_two_doors.rs`).
//!
//! ⚠ This was the `vike-report` crate's `journal_read.rs` until 2026-10-12 (`vike-report` was
//! dissolved; the reader lives in the journal crate itself).

use std::fmt;
use std::path::Path;

use vike_model::events::{Event, FillEvent};

use crate::{Journal, JournalRecord};

/// Error reading fills from a journal directory.
#[derive(Debug)]
pub enum JournalReadError {
    /// The underlying [`Journal::read_all`] I/O / format error.
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
/// `Journal::read_all`'s contract) — the fills up to the tear are returned.
pub fn fills_from_journal(dir: &Path) -> Result<Vec<FillEvent>, JournalReadError> {
    let records = Journal::read_all(dir)?;
    let mut fills = Vec::new();
    let mut fees = vike_model::events::FeeCorrections::default();
    for r in records {
        // `venue_event`, not an `Ingest::Event` pattern: a fill that came over a gateway's trade
        // link is journaled as an `Ingest::GatewayEvent` (v18) and is a fill all the same.
        if let JournalRecord::Command { message: msg, .. } = r {
            match msg.venue_event() {
                Some(Event::Fill(f)) => fills.push(f.clone()),
                Some(Event::FeeAdjustment(a)) => fees.note(a),
                _ => {}
            }
        }
    }
    fees.fold_into(&mut fills);
    Ok(fills)
}

// The fold of the corrections is `vike_model::events::FeeCorrections`, shared with the stored door.

#[path = "fills_tests.rs"]
#[cfg(test)]
mod fills_tests;
