//! The publisher's READ half: where its records come from, which of them the stream carries and how each becomes a
//! wire body, the removed mounts a strategy code names, and the order client id sessions the journal's `Snapshot`s
//! name.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vike_datahub_client::execution_stream::{
    ExecutionForecastWire, ExecutionPositionSnapshotAccountWire, ExecutionPositionSnapshotWire,
    ExecutionRecordBodyWire, ExecutionReportWire, ExecutionUnverifiedWire,
};
use vike_execution::Ingest;
use vike_journal::{
    EXECUTION_STREAM_HOLD_MS, JournalDurableWatch, JournalRecord, read_closed_since,
    read_durable_since, record_now_ms, record_sequence,
};
use vike_model::accounts::account_keys::label_of_route_key;
use vike_model::events::Event;
use vike_model::orders::client_id::OrderClientIdStrategyCode;
use vike_model::orders::{
    ExecutionOwnership, ExecutionReport, ExecutionReportFeeCorrection, ExecutionReportFill,
};

/// The most records one read asks the journal for: a catch-up after an outage holds at most this many at once.
pub(super) const READ_LIMIT: usize = 512;

/// How often one session's row is raised again while its sequence moves: a `Snapshot` can come every few seconds on a
/// busy node, and the row only has to be roughly current when the session is left.
/// shortcut: ids minted in the last minute before a journal is moved aside read foreign after it, until a past
/// session's last sequence comes from the core itself.
const SESSION_RAISE_EVERY: Duration = Duration::from_secs(60);

/// Where a stream's records come from.
pub(super) enum JournalRecords<'a> {
    /// The open journal: only what its sync thread has made durable.
    Live { dir: &'a Path, watch: &'a JournalDurableWatch },
    /// A journal moved aside: nothing writes it any more, so what [`MovedAside::scan`] found of it holds.
    MovedAside(&'a MovedAside),
}

impl JournalRecords<'_> {
    /// The records past `after` (all of them for `None`), at most `limit`, in sequence order.
    pub(super) fn read(&self, after: Option<u64>, limit: usize) -> io::Result<Vec<JournalRecord>> {
        match self {
            JournalRecords::Live { dir, watch } => read_durable_since(dir, after, watch, limit),
            JournalRecords::MovedAside(moved) => moved.read(after, limit),
        }
    }

    /// The first record's sequence and time.
    pub(super) fn oldest(&self) -> io::Result<Option<(u64, i64)>> {
        match self {
            JournalRecords::Live { .. } => {
                Ok(self.read(None, 1)?.first().map(|r| (record_sequence(r), record_now_ms(r))))
            }
            JournalRecords::MovedAside(moved) => Ok(moved.first),
        }
    }

    /// The newest record's sequence, read on from `known` (a head read before; the reader resumes where it stopped).
    pub(super) fn head(
        &self,
        known: Option<u64>,
        sessions: &mut SessionRows,
    ) -> io::Result<Option<u64>> {
        if let JournalRecords::MovedAside(moved) = self {
            return Ok(moved.end());
        }
        let mut head = known;
        loop {
            let batch = self.read(head, READ_LIMIT)?;
            for record in batch.iter() {
                sessions.note(record);
            }
            match batch.last() {
                Some(last) => head = Some(record_sequence(last)),
                None => return Ok(head),
            }
            if batch.len() < READ_LIMIT {
                return Ok(head);
            }
        }
    }

    /// The last sequence this source will ever hold; `None` for the open journal, which grows.
    pub(super) fn end(&self) -> Option<Option<u64>> {
        match self {
            JournalRecords::Live { .. } => None,
            JournalRecords::MovedAside(moved) => Some(moved.end()),
        }
    }

    pub(super) fn watch(&self) -> Option<&JournalDurableWatch> {
        match self {
            JournalRecords::Live { watch, .. } => Some(watch),
            JournalRecords::MovedAside(_) => None,
        }
    }
}

/// **A journal moved aside, as the stream sends it.** It can hold a whole uptime's records (the open journal is pruned
/// only at boot, and a moved directory never), so it is never held in memory: [`Self::scan`] walks it once, a page at
/// a time, for its ends, and [`Self::read`] pages through it as [`read_durable_since`] pages the open journal. It
/// sends:
/// - the leading run of records already older than `EXECUTION_STREAM_HOLD_MS` when it is walked, as ONE
///   `ExecutionReportsUnverified`: the open journal's prune would have dropped them, so the stream owes no more of a
///   moved journal than of the open one; the datahub records the window as a gap and both ends say it at ERROR;
/// - every other record it still reads;
/// - when its read HALTED (what a moved-aside journal is), one `ExecutionReportsUnverified` under its own `journal_id`
///   for everything past the halt, numbered one past its last readable record. The executions there cannot be read,
///   so they are said lost, never silently skipped.
///
/// The directory itself is kept as it is: nothing here deletes a moved journal.
pub(super) struct MovedAside {
    pub(super) journal_id: String,
    path: PathBuf,
    /// Its first and last readable records, `(sequence, now_ms)`.
    first: Option<(u64, i64)>,
    last: Option<(u64, i64)>,
    /// The leading run older than the hold: `(the first one's now_ms, the last one's sequence, its now_ms)`.
    expired: Option<(i64, u64, i64)>,
    /// Why nothing past its halt reads; `None` when it ends clean.
    halted: Option<String>,
}

impl MovedAside {
    /// Walk the journal at `path` once, [`READ_LIMIT`] records at a time, for its first and last readable records,
    /// where its read halts, and the leading run older than the hold at `now_ms`.
    pub(super) fn scan(journal_id: &str, path: &Path, now_ms: i64) -> io::Result<Self> {
        let mut moved = MovedAside {
            journal_id: journal_id.to_string(),
            path: path.to_path_buf(),
            first: None,
            last: None,
            expired: None,
            halted: None,
        };
        let mut expiring = true;
        loop {
            let (page, halt) = read_closed_since(path, moved.last.map(|(s, _)| s), READ_LIMIT)?;
            for record in &page {
                let at = (record_sequence(record), record_now_ms(record));
                moved.first = moved.first.or(Some(at));
                expiring &= now_ms.saturating_sub(at.1) > EXECUTION_STREAM_HOLD_MS;
                if expiring {
                    moved.expired =
                        Some((moved.expired.map_or(at.1, |(from, ..)| from), at.0, at.1));
                }
                moved.last = Some(at);
            }
            if page.len() < READ_LIMIT {
                moved.halted = halt.map(|h| {
                    format!(
                        "the journal moved aside to {} does not read past byte {} of {} ({}): the executions \
                         journaled after it are lost to the stream",
                        path.display(),
                        h.offset,
                        h.segment.display(),
                        h.cause.as_str()
                    )
                });
                return Ok(moved);
            }
        }
    }

    /// The last sequence the stream sends of it: the unverified rest's, else its last record's.
    fn end(&self) -> Option<u64> {
        if self.halted.is_some() { Some(self.rest_sequence()) } else { self.last.map(|(s, _)| s) }
    }

    /// One past its last readable record: the sequence of the unverified rest.
    fn rest_sequence(&self) -> u64 {
        self.last.map_or(0, |(s, _)| s + 1)
    }

    /// The records the stream sends past `after`, at most `limit`, read off the files a page at a time.
    fn read(&self, after: Option<u64>, limit: usize) -> io::Result<Vec<JournalRecord>> {
        let mut out = Vec::new();
        let mut after = after;
        if let Some((first_ms, through, to_ms)) = self.expired
            && limit > 0
            && after.is_none_or(|a| a < through)
        {
            // the window opens at the first record past `after`
            let (next, _) = read_closed_since(&self.path, after, 1)?;
            let from_ms = next.first().map_or(first_ms, record_now_ms);
            let reason = format!(
                "the journal moved aside to {} holds executions older than the execution stream's hold ({} days) \
                 that the datahub never stored: they are not sent, as the open journal's prune would have dropped \
                 them",
                self.path.display(),
                EXECUTION_STREAM_HOLD_MS / (24 * 60 * 60 * 1000)
            );
            tracing::error!(
                journal_id = %self.journal_id,
                after_sequence = ?after,
                through_sequence = through,
                from_ms,
                to_ms,
                "execution stream: {reason}"
            );
            out.push(self.unverified(
                (through, to_ms),
                (after.unwrap_or(0), through),
                (Some(from_ms), Some(to_ms)),
                reason,
            ));
            after = Some(through);
        }
        if out.len() < limit {
            out.extend(read_closed_since(&self.path, after, limit - out.len())?.0);
        }
        let read_to = out.last().map(record_sequence).or(after);
        let all_read = self.last.is_none_or(|(last, _)| read_to.is_some_and(|r| r >= last));
        let rest = self.rest_sequence();
        if let Some(reason) = &self.halted
            && out.len() < limit
            && all_read
            && read_to.is_none_or(|r| r < rest)
        {
            out.push(self.unverified(
                (rest, self.last.map_or(0, |(_, ms)| ms)),
                // everything after: the halt hides how far the journal went
                (self.last.map_or(0, |(s, _)| s), u64::MAX),
                (None, None),
                reason.clone(),
            ));
        }
        Ok(out)
    }

    /// An `ExecutionReportsUnverified` of this journal: `(sequence, now_ms)`, `(after, through)`, `(from, to)`.
    fn unverified(
        &self,
        (sequence, now_ms): (u64, i64),
        (after_sequence, through_sequence): (u64, u64),
        (from_ms, to_ms): (Option<i64>, Option<i64>),
        reason: String,
    ) -> JournalRecord {
        JournalRecord::ExecutionReportsUnverified {
            sequence,
            now_ms,
            journal_id: self.journal_id.clone(),
            after_sequence,
            through_sequence,
            from_ms,
            to_ms,
            order_client_ids: Vec::new(),
            reason,
        }
    }
}

/// **What the stream says of one journal record**, or `None` for a record that is no execution. One exhaustive match:
/// a variant added to `JournalRecord` must be classified here.
///
/// # Errors
/// The settings store exists and cannot be read for a removed mount's strategy code: the record waits (never sent
/// without its mount).
pub(super) fn body_of(
    record: &JournalRecord,
    mounts: &mut StrategyCodeMounts,
) -> Result<Option<ExecutionRecordBodyWire>, String> {
    let report = |r: &ExecutionReport| {
        Some(ExecutionRecordBodyWire::Report(Box::new(ExecutionReportWire::from(r))))
    };
    Ok(match record {
        JournalRecord::ExecutionReport { report: r, .. } => match removed_mount(r, mounts)? {
            Some(mount_id) => report(&ExecutionReport { mount_id: Some(mount_id), ..r.clone() }),
            None => report(r),
        },
        JournalRecord::Command { sequence, message, .. } => {
            foreign_execution(*sequence, message).as_ref().and_then(report)
        }
        JournalRecord::Forecast { mount_id, venue, symbol, forecast, .. } => {
            Some(ExecutionRecordBodyWire::Forecast(ExecutionForecastWire {
                mount_id: mount_id.clone(),
                venue: venue.clone(),
                symbol: symbol.clone(),
                value: forecast.value(),
                horizon_ms: forecast.horizon_ms(),
                confidence: forecast.confidence(),
            }))
        }
        JournalRecord::ExecutionReportsUnverified {
            journal_id,
            after_sequence,
            through_sequence,
            from_ms,
            to_ms,
            order_client_ids,
            reason,
            ..
        } => Some(ExecutionRecordBodyWire::Unverified(ExecutionUnverifiedWire {
            journal_id: journal_id.clone(),
            after_sequence: *after_sequence,
            through_sequence: *through_sequence,
            from_ms: *from_ms,
            to_ms: *to_ms,
            order_client_ids: order_client_ids.clone(),
            reason: reason.clone(),
        })),
        JournalRecord::Snapshot { .. }
        | JournalRecord::StrategySubmit { .. }
        | JournalRecord::MintedSubmit { .. }
        | JournalRecord::PortfolioSnapshot { .. }
        | JournalRecord::OrderEmulationArmed { .. }
        | JournalRecord::OrderEmulationFire { .. }
        | JournalRecord::OrderEmulationDisarmed { .. }
        | JournalRecord::MarginCallLiquidate { .. }
        | JournalRecord::GtdExpire { .. }
        | JournalRecord::ScheduleFire { .. }
        | JournalRecord::SharedCloseTimeout { .. }
        | JournalRecord::StrategyFileMounted { .. } => None,
        // A count of denials the gate repeated: the first of its key went out as its own report, the repeats are no
        // execution of their own (format 35).
        JournalRecord::DeniedRepeat { .. } => None,
        JournalRecord::PositionSnapshot { accounts, .. } => {
            Some(ExecutionRecordBodyWire::PositionSnapshot(ExecutionPositionSnapshotWire {
                accounts: accounts
                    .iter()
                    .map(|a| ExecutionPositionSnapshotAccountWire {
                        venue: a.venue.clone(),
                        account: a.account.clone(),
                        // The venue a position names is the account's, so it does not ride.
                        positions: a
                            .positions
                            .iter()
                            .cloned()
                            .map(vike_model::AccountVenuePosition::without_venue)
                            .collect(),
                        unknown: a.unknown.clone(),
                    })
                    .collect(),
            }))
        }
    })
}

/// The mount a report of OURS names by its strategy code when the core had no mount carrying that code any more (the
/// mount was removed or re-pointed before the event): the durable `strategy_code` row's.
fn removed_mount(
    report: &ExecutionReport,
    mounts: &mut StrategyCodeMounts,
) -> Result<Option<String>, String> {
    match (report.ownership, &report.mount_id, report.strategy_code) {
        (ExecutionOwnership::Ours, None, Some(code)) => mounts.mount_of(code),
        _ => Ok(None),
    }
}

/// **A venue fill or fee correction that names no order**: the OMS journals no report for it (the latency gate's
/// path), so it is stated from its own `Command`, FOREIGN, with the account whose ring carried it and nothing else.
fn foreign_execution(sequence: u64, message: &Ingest) -> Option<ExecutionReport> {
    let ring = match message {
        Ingest::GatewayEvent(g) => Some(g.route_key.as_str()),
        _ => None,
    };
    let (venue, symbol, at_ms, fill, fee_correction) = match message.venue_event()? {
        Event::Fill(f) if f.order_client_id.is_empty() => {
            (f.venue, f.symbol, f.timestamp, Some(ExecutionReportFill::of_fill(f)), None)
        }
        Event::FeeAdjustment(a) if a.order_client_id.is_empty() => (
            a.venue,
            a.symbol,
            a.timestamp,
            None,
            Some(ExecutionReportFeeCorrection::of_adjustment(a)),
        ),
        // an event that names an order is reported by the OMS itself; any other event is no execution
        _ => return None,
    };
    Some(ExecutionReport {
        caused_by_sequence: sequence,
        at_ms,
        ownership: ExecutionOwnership::Foreign,
        account: ring.and_then(|key| label_of_route_key(&venue, key)).map(|l| l.to_string()),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        order_client_id: String::new(),
        venue_order_id: None,
        mount_id: None,
        strategy_code: None,
        order: None,
        fill,
        fee_correction,
        reason: None,
    })
}

/// The mounts the durable `strategy_code` table names, by code. A code is never re-pointed (the table is insert-only),
/// so an answer is kept for the life of the thread.
pub(super) struct StrategyCodeMounts {
    settings_dir: Option<PathBuf>,
    named: HashMap<i64, Option<String>>,
}

impl StrategyCodeMounts {
    pub(super) fn new(settings_dir: Option<PathBuf>) -> Self {
        StrategyCodeMounts { settings_dir, named: HashMap::new() }
    }

    fn mount_of(&mut self, code: OrderClientIdStrategyCode) -> Result<Option<String>, String> {
        let Some(dir) = &self.settings_dir else { return Ok(None) };
        if let Some(named) = self.named.get(&code.id()) {
            return Ok(named.clone());
        }
        let named = vike_secrets::strategy_code::strategy_name_for(dir, code.id())
            .map_err(|e| {
                format!("strategy code {code} could not be named from the settings store: {e}")
            })?
            .map(|row| row.mount_id);
        self.named.insert(code.id(), named.clone());
        Ok(named)
    }
}

/// The order client id sessions the journal's `Snapshot`s name, raised into `vike.db` (`vike_secrets`'s
/// `order_client_id_sessions`) to the sequence a `Snapshot` states plus [`super::ORDER_CLIENT_ID_SESSION_LEASE`]: on
/// first sight, then at most once per [`SESSION_RAISE_EVERY`] while its sequence moves. The boot wrote the session's
/// first lease before the core minted under it; this keeps the lease ahead of a session that mints past it.
pub(super) struct SessionRows {
    settings_dir: Option<PathBuf>,
    written: HashMap<String, (u64, Instant)>,
}

impl SessionRows {
    pub(super) fn new(settings_dir: Option<PathBuf>) -> Self {
        SessionRows { settings_dir, written: HashMap::new() }
    }

    pub(super) fn note(&mut self, record: &JournalRecord) {
        let JournalRecord::Snapshot {
            order_client_id_session: session,
            order_client_id_sequence: sequence,
            ..
        } = record
        else {
            return;
        };
        let Some(dir) = &self.settings_dir else { return };
        let due = match self.written.get(session) {
            None => !session.is_empty(),
            Some(&(written, at)) => *sequence > written && at.elapsed() >= SESSION_RAISE_EVERY,
        };
        if !due {
            return;
        }
        match vike_secrets::order_client_id_sessions::raise_order_client_id_session(
            dir,
            session,
            sequence.saturating_add(super::ORDER_CLIENT_ID_SESSION_LEASE),
            vike_model::now_ms(),
        ) {
            Ok(()) => {
                self.written.insert(session.clone(), (*sequence, Instant::now()));
            }
            Err(e) => tracing::warn!(
                session = %session,
                error = %e,
                "execution stream: this install's order client id session could not be recorded in the settings \
                 store; a late fill on one of its ids may read as foreign after the next restart"
            ),
        }
    }
}
