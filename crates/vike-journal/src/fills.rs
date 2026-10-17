//! `fills_from_journal` — read the account-affecting `FillEvent`s out of a command journal
//! directory, preserving journal (chronological) order.
//!
//! The journal records every exec-lane message; only the bare `Ingest::Event(Event::Fill(_))`
//! records affect the account (this is exactly the filter `vike_exec::Account::fold` applies —
//! `OrderSubmitted`/`Accepted`/`Filled` lifecycle events are journaled too but carry no
//! position math, so they are dropped here). The extracted fills feed the tearsheet fold,
//! `vike_analytics::tearsheet_from_fills` with `FillOrder::AsHeld`: this crate does the READ and
//! names no analytics, the fold names no journal, and each caller composes the two (the tradehub
//! daemon's `crates/vike-tradehub/src/server/tearsheet.rs`, the `tearsheet` CLI in
//! `crates/vike/src/tearsheet_cli.rs`).
//!
//! The journal is the ONE source of fills on THIS door, and its directory is named by the caller:
//! the `tearsheet` CLI's `--journal DIR`, or the directory a daemon resolved from its
//! `config.journal_dir` row. There is no store FALLBACK here: `fills_from_store` and its row
//! converter were deleted on 2026-10-10 (`docs/decisions/0117-there-are-no-migrations.md`), as no
//! binary ever called them. This crate names no store at all; the desktop's Tearsheet reads the
//! datahub's `kind=exec_fill` rows itself, converts them with
//! `vike_data::ExecFillRow::to_fill_event` and folds them through
//! `vike_analytics::tearsheet_from_fills` with `FillOrder::ByTime`.
//!
//! ⚠ This was the `vike-report` crate's `journal_read.rs` until 2026-10-12 (`vike-report` was
//! dissolved; the reader lives in the journal crate itself).

use std::fmt;
use std::path::Path;

use vike_model::events::{Event, FillEvent};

use crate::{CommandJournal, JournalRecord};

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
            // `venue_event`, not an `Ingest::Event` pattern: a fill that came over a gateway's trade
            // link is journaled as an `Ingest::GatewayEvent` (v18) and is a fill all the same.
            JournalRecord::Cmd { msg, .. } => match msg.venue_event() {
                Some(Event::Fill(f)) => Some(f.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    Ok(fills)
}

#[path = "fills_tests.rs"]
#[cfg(test)]
mod fills_tests;
