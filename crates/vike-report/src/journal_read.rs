//! `fills_from_journal` — read the account-affecting `FillEvent`s out of a vike-core command
//! journal directory, preserving journal (chronological) order.
//!
//! The journal records every exec-lane message; only the bare `Ingest::Event(Event::Fill(_))`
//! records affect the account (this is exactly the filter `vike_exec::Account::fold` applies —
//! `OrderSubmitted`/`Accepted`/`Filled` lifecycle events are journaled too but carry no
//! position math, so they are dropped here). The extracted fills feed
//! [`vike_analytics::reconstruct_trades`], and [`tearsheet_from_journal`] runs the whole door.
//!
//! ⚠ **This module was the whole reason the crate's `journal` feature existed**, and it is still
//! the one file here that names both vike-journal (the journal) and vike-exec (the `Ingest` the
//! records wrap). That closure is what `crates/vike-cli/src/cmd/report.rs` and
//! `crates/vike-tradehub-client/src/proto.rs`'s `Response::Tearsheet` once named as the reason they
//! would not link this crate, and the feature was how a renderer-only consumer declined it. Since
//! 2026-09-28 the renderer half lives in `vike-analytics` instead, so a consumer that only renders
//! names that crate and never this one — and with nothing left here to decline, the feature was
//! deleted rather than kept as a switch every build turns on.
//!
//! The journal is the ONE source of fills here, and its directory is named by the caller: the
//! `tearsheet` tool's `--journal DIR`, or the directory a daemon resolved from its
//! `config.journal_dir` row. `fills_from_store` — a read of the Tier-2 `kind=exec_fill` series
//! meant to be tried first, with this journal as its fallback for "the migration compat window" —
//! and its `exec_fill_to_event` row converter were deleted on 2026-10-10
//! (`docs/decisions/0117-there-are-no-migrations.md`): no binary ever called them.

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
/// from trade PnLs); when a stored `kind=equity` series exists, prefer
/// [`LiveTearsheet::from_result_parts`] with [`crate::equity_curve_from_store`] for a true
/// mark-to-market curve.
///
/// ⚠ **It was the inherent `LiveTearsheet::from_journal` until 2026-09-28**, and it is a FREE
/// function now for a language reason rather than a taste one: the document type moved to
/// `vike-analytics` with the rest of the renderer, and an inherent `impl` may only be written in
/// the crate that owns the type. Keeping the method would have meant keeping the journal read in
/// `vike-analytics`, which may not name `vike_journal` at all. The four steps are unchanged, and
/// `crate::tearsheet_cli`'s realized-only path inlines the same four so the curve stays in hand
/// for `--html`.
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
