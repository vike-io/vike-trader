//! `fills_from_journal` — read the account-affecting `FillEvent`s out of a command journal
//! directory, preserving journal (chronological) order.
//!
//! The journal records every exec-lane message; only the bare `Ingest::Event(Event::Fill(_))`
//! records affect the account (this is exactly the filter `vike_execution::Account::fold` applies —
//! `OrderSubmitted`/`Accepted`/`Filled` lifecycle events are journaled too but carry no
//! position math, so they are dropped here). The extracted fills feed the tearsheet fold,
//! `vike_analytics::tearsheet_from_fills` with `TearsheetFillOrder::AsHeld`: this crate does the READ and
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
//! `vike_data::ExecFillRow::fill_events` and folds them through
//! `vike_analytics::tearsheet_from_fills` with `TearsheetFillOrder::ByTime`. The two doors fold a fee correction
//! through one `vike_model::events::FeeCorrections` and keep `position_side`, so one journal gives one
//! sheet (`crates/vike-datahub/tests/default_build/execution_fills_two_doors.rs`).
//!
//! ⚠ This was the `vike-report` crate's `journal_read.rs` until 2026-10-12 (`vike-report` was
//! dissolved; the reader lives in the journal crate itself).

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use vike_model::ContractValue;
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

/// [`fills_from_journal`]'s fills AND the ledgers and contract values the journal's snapshots record that a sheet
/// cannot add, in ONE read of the segments (a journal is read whole): what a tearsheet door needs to refuse to add a
/// euro fill to a dollar one (decision 0183, item 13) or to read an inverse contract as a linear one (decision 0243,
/// item 6).
///
/// The second element is `(venue, currency)` for every engine of a `Snapshot` record whose `quote_asset` is a
/// currency the node told it (`Engine::set_account_currency`) that is not `USD` or `USDT` (the dollar group,
/// `Engine::ledger_currency_key`'s own rule), first-seen order, each pair once. ⚠ It is a statement about
/// ENGINES of a venue, not about a fill: a fill carries no account (the journal reader drops the gateway's
/// route key), so a venue that runs a dollar account beside a euro one is listed and every fill of that venue is
/// the door's to treat as possibly euro. A journal with no `Snapshot` record (none was ever written) lists nothing.
///
/// The third is `(venue, symbol, value)` for every row of an engine's `EngineSnapshotAccount::mult` and
/// `EngineSnapshotAccount::inverse` that is not the default (`ContractValue::is_default`), first-seen order, each
/// `(venue, symbol)` once. The same statement about a venue: an account of the venue that values the symbol so lists
/// it for all of them. `mult_default` is not read, because every engine a node builds passes 1.0 and lists the rest.
/// ⚠ What the snapshots record: a symbol admitted since the last `Snapshot` is not listed until the next one.
// The lists are the whole answer; a named type for them would be a second spelling of plain `Vec`s.
#[expect(clippy::type_complexity)]
pub fn fills_and_ledgers_from_journal(
    dir: &Path,
) -> Result<
    (Vec<FillEvent>, Vec<(String, String)>, Vec<(String, String, ContractValue)>),
    JournalReadError,
> {
    let records = Journal::read_all(dir)?;
    let mut fills = Vec::new();
    let mut fees = vike_model::events::FeeCorrections::default();
    let mut ledgers: Vec<(String, String)> = Vec::new();
    let mut contracts: Vec<(String, String, ContractValue)> = Vec::new();
    let mut listed: HashSet<(String, String)> = HashSet::new();
    for r in records {
        match r {
            JournalRecord::Command { message: msg, .. } => match msg.venue_event() {
                Some(Event::Fill(f)) => fills.push(f.clone()),
                Some(Event::FeeAdjustment(a)) => fees.note(a),
                _ => {}
            },
            JournalRecord::Snapshot { engines, .. } => {
                for e in &engines {
                    let linear = e.account.mult.iter().map(|(s, u)| (s, ContractValue::linear(*u)));
                    let inverse =
                        e.account.inverse.iter().map(|(s, u)| (s, ContractValue::inverse(*u)));
                    for (symbol, value) in linear.chain(inverse) {
                        // `folded`: the value `Account::restore` reads this row as.
                        let value = value.folded();
                        if !value.is_default() && listed.insert((e.venue.clone(), symbol.clone())) {
                            contracts.push((e.venue.clone(), symbol.clone(), value));
                        }
                    }
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
    Ok((fills, ledgers, contracts))
}

#[path = "fills_tests.rs"]
#[cfg(test)]
mod fills_tests;
