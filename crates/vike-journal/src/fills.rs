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
    let mut fills = Vec::new();
    let mut fees = FeeCorrections::default();
    for r in records {
        // `venue_event`, not an `Ingest::Event` pattern: a fill that came over a gateway's trade
        // link is journaled as an `Ingest::GatewayEvent` (v18) and is a fill all the same.
        if let JournalRecord::Cmd { msg, .. } = r {
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

/// The fee corrections a journal holds (`Event::FeeAdjustment`, decision 0238), by the trade id of the fill they
/// restate, the LAST one of an execution winning (each states the execution's total over its fill). The tearsheet
/// reads a fill's `commission`; a fill booked with its fee unknown, or one IB later revised, would otherwise carry a
/// fee the account no longer holds.
#[derive(Default)]
struct FeeCorrections(std::collections::HashMap<String, vike_model::events::FeeAdjustmentEvent>);

impl FeeCorrections {
    fn note(&mut self, a: &vike_model::events::FeeAdjustmentEvent) {
        self.0.insert(a.trade_id.to_string(), a.clone());
    }

    /// Add each correction to the fill it restates; a fill that stated no fee currency takes the correction's.
    fn fold_into(&self, fills: &mut [FillEvent]) {
        if self.0.is_empty() {
            return;
        }
        for f in fills {
            if let Some(a) = self.0.get(f.trade_id.as_str()) {
                f.commission += a.extra_commission;
                if f.commission_asset.is_empty() {
                    f.commission_asset = a.commission_asset;
                }
            }
        }
    }
}

/// [`fills_from_journal`]'s fills AND the ledgers the journal's snapshots record that are not the dollar's, in
/// ONE read of the segments (a journal is read whole): what a tearsheet door needs to refuse to add a euro fill
/// to a dollar one (decision 0183, item 13).
///
/// The second element is `(venue, currency)` for every engine of a `Snap` record whose `quote_asset` is a
/// currency the node told it (`ExecutionEngine::set_account_ccy`) that is not `USD` or `USDT` (the dollar group,
/// `ExecutionEngine::ledger_ccy_key`'s own rule), first-seen order, each pair once. ⚠ It is a statement about
/// ENGINES of a venue, not about a fill: a fill carries no account (the journal reader drops the gateway's
/// route key), so a venue that runs a dollar account beside a euro one is listed and every fill of that venue is
/// the door's to treat as possibly euro. A journal with no `Snap` record (none was ever written) lists nothing.
// The pair of lists is the whole answer; a named type for it would be a second spelling of two plain `Vec`s.
#[expect(clippy::type_complexity)]
pub fn fills_and_ledgers_from_journal(
    dir: &Path,
) -> Result<(Vec<FillEvent>, Vec<(String, String)>), JournalReadError> {
    let records = CommandJournal::read_all(dir)?;
    let mut fills = Vec::new();
    let mut fees = FeeCorrections::default();
    let mut ledgers: Vec<(String, String)> = Vec::new();
    for r in records {
        match r {
            JournalRecord::Cmd { msg, .. } => match msg.venue_event() {
                Some(Event::Fill(f)) => fills.push(f.clone()),
                Some(Event::FeeAdjustment(a)) => fees.note(a),
                _ => {}
            },
            JournalRecord::Snap { engines, .. } => {
                for e in &engines {
                    let code = e.quote_asset.trim();
                    if code.is_empty()
                        || code.eq_ignore_ascii_case("USD")
                        || code.eq_ignore_ascii_case("USDT")
                    {
                        continue;
                    }
                    let pair = (e.venue.clone(), code.to_ascii_uppercase());
                    if !ledgers.contains(&pair) {
                        ledgers.push(pair);
                    }
                }
            }
            _ => {}
        }
    }
    fees.fold_into(&mut fills);
    Ok((fills, ledgers))
}

#[path = "fills_tests.rs"]
#[cfg(test)]
mod fills_tests;
