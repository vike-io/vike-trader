//! The `append_*` verbs of [`Journal`]: one per record variant a live core journals, each
//! building the borrowed write twin of the record and handing it to [`Journal::append`].
//!
//! Every verb has the same shape on purpose: [`Journal::append`] settles a `Snapshot` the write thread is
//! encoding (so the sequence it reads is the next one), raises the header the record needs, encodes the
//! record (msgpack, `crate::frame`'s `encode_into`) into the ONE payload buffer the journal keeps and frames it;
//! the framing, the roll trigger and the sync cadence stay in the parent with the state they mutate. An
//! execution report alone frames LAZILY (`append_execution_report`), and a cadence `Snapshot` is ENCODED on the
//! write thread (`append_snap_deferred`). These verbs are the entry points the fold calls, so the `p99` core-hop
//! gate covers them: add no work here.

use std::io;

use vike_execution::{EngineSnapshot, Ingest};

use super::Journal;
use crate::format::{
    intent_version_needed, release_version_needed, snap_version_needed, version_needed,
};
use crate::record::{
    JournalRecordRef, JournalSnapshotBooks, JournalSnapshotParts, OrderEmulationRecord,
    PortfolioSample,
};

/// The `Snapshot` record over borrowed parts: the ONE spelling the synchronous [`Journal::append_snap`] and the write
/// thread's encode of [`Journal::append_snap_deferred`] share, so both write the same bytes.
fn snapshot_record<'a>(
    sequence: u64,
    now_ms: i64,
    engines: &'a [EngineSnapshot],
    order_client_id_session: &'a str,
    order_client_id_sequence: u64,
    arm_sequence: u64,
    books: JournalSnapshotParts<'a>,
    hash: u64,
) -> JournalRecordRef<'a> {
    JournalRecordRef::Snapshot {
        sequence,
        now_ms,
        engines,
        order_client_id_session,
        order_client_id_sequence,
        arm_sequence,
        order_emulation_arms: books.order_emulation_arms,
        contingencies: books.contingencies,
        mount_attribution: books.mount_attribution,
        gateway_cursors: books.gateway_cursors,
        held_releases: books.held_releases,
        recent_order_client_ids: books.recent_order_client_ids,
        emulated: books.emulated,
        closed_trades: books.closed_trades,
        awaiting_engines: books.awaiting_engines,
        order_client_id_mounts: books.order_client_id_mounts,
        order_client_id_past_sessions: books.order_client_id_past_sessions,
        hash,
    }
}

/// One write-ahead record of a strategy submit, HELD by [`Journal::defer_submits`] until it is framed or dropped:
/// encoded at once under the sequence it takes when framed (nothing else frames before it), so the core lends it
/// nothing it must keep.
pub(super) struct DeferredSubmit {
    sequence: u64,
    needed: u32,
    payload: Vec<u8>,
}

impl Journal {
    /// The one append: settle a `Snapshot` the write thread is encoding (it takes the sequence it was handed, so it
    /// is framed BEFORE this record), frame what [`Self::defer_submits`] holds, raise the header to `needed`, encode
    /// `record(sequence)` into the kept payload buffer (no allocation once it has grown) and frame it, eager or
    /// `lazy`. Returns the sequence.
    fn append<'r>(
        &mut self,
        needed: u32,
        lazy: bool,
        record: impl FnOnce(u64) -> JournalRecordRef<'r>,
    ) -> io::Result<u64> {
        self.settle_snapshot()?;
        if self.deferred.is_some() {
            self.end_deferred_submits()?;
        }
        self.require_version(needed);
        let sequence = self.sequence;
        let mut payload = std::mem::take(&mut self.payload);
        payload.clear();
        crate::frame::encode_into(&mut payload, &record(sequence));
        let framed = self.frame_into_stage(&payload, lazy);
        self.payload = payload;
        framed.map(|()| sequence)
    }

    /// Journal one exec-lane ingest message (borrowed — no clone). Returns the assigned seq.
    pub fn append_command(&mut self, now_ms: i64, msg: &Ingest) -> io::Result<u64> {
        // The place a record can need a newer header than the segment carries (`AdmitSymbol`, v19;
        // an `AdmitSymbol` that names a quote currency, v20; `append_snap` has the `Snapshot` twin): the
        // stamp rises here, before the frame, so a journal that never admits a symbol (or never
        // trades a foreign-quoted one) stays readable by the build it replaced.
        self.append(version_needed(msg), false, |sequence| JournalRecordRef::Command {
            sequence,
            now_ms,
            message: msg,
        })
    }

    /// Raise the segment to the order-list format (`crate::format::ORDER_LIST_VERSION`) before the
    /// core acts on a lone `OUO` leg it ACCEPTED: the leg's write-ahead `Submit` parses in every build,
    /// so only the accept can say it holds one (an order list's own frame stamps itself). A no-op once
    /// raised.
    pub fn stamp_order_list_version(&mut self) {
        self.require_version(crate::format::ORDER_LIST_VERSION);
    }

    /// Journal one mounted strategy's order intent at the `drain_broker` boundary (borrowed — no
    /// clone), write-ahead of the `apply_intent` call it records (mirrors `append_command`). Returns
    /// the assigned seq.
    ///
    /// `route_key` is the route key of the engine the mount lowers the intent onto, or `None` on a core
    /// whose engines an order's venue can tell apart (so its frame bytes do not move). See
    /// [`crate::JournalRecord::StrategySubmit`].
    pub fn append_strategy_submit(
        &mut self,
        now_ms: i64,
        mount_id: &str,
        intent: &vike_execution::OrderIntent,
        route_key: Option<&str>,
    ) -> io::Result<u64> {
        // an order list's frame needs the header a build that cannot parse it refuses (v22)
        self.append_or_hold(intent_version_needed(intent), |sequence| {
            JournalRecordRef::StrategySubmit { sequence, now_ms, mount_id, intent, route_key }
        })
    }

    /// Journal a server-minted submit's RESOLVED request (borrowed — no clone), from inside
    /// `apply_intent` right AFTER the order client id is minted for an incoming EMPTY-order-client-id submit. This closes
    /// the minted-order-client-id `execution_order` gap: the write-ahead `Command` for this order carried an empty order client id,
    /// so without this record a reader of the journal could never tie the minted order client id to its
    /// `(venue, symbol, qty, …)` for an order that terminalizes without filling. See
    /// [`crate::JournalRecord::MintedSubmit`]. Returns the assigned seq.
    ///
    /// `route_key` is the RESOLVED routing key of the engine this submit was lowered onto, or
    /// `None` when that engine is the request's venue's sole/default account — the shape
    /// `vike_execution::ReconcileReports::route_key` established, so a single-account box's journal
    /// bytes do not move. See [`crate::JournalRecord::MintedSubmit`] for why the record carries it.
    pub fn append_minted_submit(
        &mut self,
        now_ms: i64,
        req: &vike_model::OrderRequest,
        route_key: Option<&str>,
    ) -> io::Result<u64> {
        self.append_minted_submit_replacing(now_ms, req, route_key, None)
    }

    /// [`Self::append_minted_submit`] naming the order this mint `replaces` (a cancel-then-new
    /// amend's new order); `None` writes exactly the frame `append_minted_submit` writes.
    pub fn append_minted_submit_replacing(
        &mut self,
        now_ms: i64,
        req: &vike_model::OrderRequest,
        route_key: Option<&str>,
        replaces: Option<&str>,
    ) -> io::Result<u64> {
        // the belt for a release that names a price venue (v24): its write-ahead frame stamped it
        self.append_or_hold(release_version_needed(req), |sequence| {
            JournalRecordRef::MintedSubmit { sequence, now_ms, request: req, route_key, replaces }
        })
    }

    /// Journal one periodic compact portfolio observation (borrowed — no clone). See
    /// [`crate::JournalRecord::PortfolioSnapshot`]. Returns the assigned seq.
    pub fn append_portfolio_snap(
        &mut self,
        now_ms: i64,
        sample: &PortfolioSample,
    ) -> io::Result<u64> {
        self.append(crate::format::BASE_VERSION, false, |sequence| {
            JournalRecordRef::PortfolioSnapshot { sequence, now_ms, sample }
        })
    }

    /// Journal one emulated conditional's RESOLVED terms right after its `arm_id` is minted
    /// (borrowed — no clone). See [`crate::JournalRecord::OrderEmulationArmed`]. Returns the assigned seq.
    pub fn append_conditional_armed(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        resolved: &OrderEmulationRecord,
    ) -> io::Result<u64> {
        self.append(crate::format::BASE_VERSION, false, |sequence| {
            JournalRecordRef::OrderEmulationArmed { sequence, now_ms, arm_id, resolved }
        })
    }

    /// Journal one emulated conditional's FIRE, write-ahead of the release it causes (borrowed —
    /// no clone). See [`crate::JournalRecord::OrderEmulationFire`]. Returns the assigned seq.
    ///
    /// `route_key` is the route key of the engine the arm fires onto, with
    /// [`Self::append_strategy_submit`]'s absence rule.
    pub fn append_conditional_fire(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        trigger_px: f64,
        req: &vike_model::OrderRequest,
        route_key: Option<&str>,
    ) -> io::Result<u64> {
        self.append(crate::format::BASE_VERSION, false, |sequence| {
            JournalRecordRef::OrderEmulationFire {
                sequence,
                now_ms,
                arm_id,
                trigger_price: trigger_px,
                request: req,
                route_key,
            }
        })
    }

    /// Journal one emulated conditional's DISARM, write-ahead of the book mutation it records.
    /// See [`crate::JournalRecord::OrderEmulationDisarmed`]. Returns the assigned seq.
    pub fn append_conditional_disarmed(&mut self, now_ms: i64, arm_id: &str) -> io::Result<u64> {
        self.append(crate::format::BASE_VERSION, false, |sequence| {
            JournalRecordRef::OrderEmulationDisarmed { sequence, now_ms, arm_id }
        })
    }

    /// Journal one auto-liquidation's RELEASED reduce-only MARKET order (borrowed — no clone),
    /// write-ahead of the `apply_intent` release it records.
    ///
    /// `mount_id` is `None` for the ACCOUNT-wide margin-call sweep and `Some(id)` for the per-mount
    /// budget latch's flatten, which ONE mount owns — see [`crate::JournalRecord::MarginCallLiquidate`] for
    /// why that distinction has to survive to disk. Returns the assigned seq.
    ///
    /// `route_key` is the RESOLVED routing key of the BREACHING engine — the one whose book this
    /// liquidation closes — or `None` when that engine is the request's venue's sole/default
    /// account. Same additive shape and same byte-identity argument as
    /// [`Self::append_minted_submit`]'s.
    pub fn append_margin_call_liquidate(
        &mut self,
        now_ms: i64,
        req: &vike_model::OrderRequest,
        mount_id: Option<&str>,
        route_key: Option<&str>,
    ) -> io::Result<u64> {
        self.append(crate::format::BASE_VERSION, false, |sequence| {
            JournalRecordRef::MarginCallLiquidate {
                sequence,
                now_ms,
                request: req,
                mount_id,
                route_key,
            }
        })
    }

    /// Journal one MANAGED GTD/Day expiry DECISION (borrowed order client id — no clone), write-ahead of the
    /// `cancel_order` it records. See [`crate::JournalRecord::GtdExpire`] for why this is an audit marker
    /// rather than a replayable command. Returns the assigned seq.
    pub fn append_gtd_expire(
        &mut self,
        now_ms: i64,
        order_client_id: &str,
        engine: usize,
    ) -> io::Result<u64> {
        self.append(crate::format::BASE_VERSION, false, |sequence| JournalRecordRef::GtdExpire {
            sequence,
            now_ms,
            order_client_id,
            engine,
        })
    }

    /// Journal one WALL-CLOCK SCHEDULE FIRE DECISION (borrowed `mount_id`/`tag` — no clone),
    /// write-ahead of the `Strategy::on_schedule` it drives. See [`crate::JournalRecord::ScheduleFire`] for
    /// why this is a replay-neutral audit marker (the on_schedule ORDERS journal on their own as
    /// `StrategySubmit` records that replay re-applies). Returns the assigned seq.
    pub fn append_schedule_fire(
        &mut self,
        now_ms: i64,
        mount_id: &str,
        tag: &str,
    ) -> io::Result<u64> {
        self.append(crate::format::BASE_VERSION, false, |sequence| JournalRecordRef::ScheduleFire {
            sequence,
            now_ms,
            mount_id,
            tag,
        })
    }

    /// Journal one SHARED CLOSE whose grace ran out, write-ahead of the strategy step it releases. See
    /// [`crate::JournalRecord::SharedCloseTimeout`]: a replay-neutral decision marker, and the first one
    /// raises the header to `crate::format::SHARED_CLOSE_VERSION`. Returns the assigned seq.
    pub fn append_shared_close_timeout(
        &mut self,
        now_ms: i64,
        mount_id: &str,
        close_ms: i64,
        input: &str,
    ) -> io::Result<u64> {
        self.append(crate::format::SHARED_CLOSE_VERSION, false, |sequence| {
            JournalRecordRef::SharedCloseTimeout { sequence, now_ms, mount_id, close_ms, input }
        })
    }

    /// Journal that mount `mount_id` opened on `strategy_file`, once, as the mount opens (never per event).
    /// See [`crate::JournalRecord::StrategyFileMounted`]: replay-neutral, and the first one raises the header
    /// to `crate::format::STORED_WORDS_VERSION`. Returns the assigned seq.
    pub fn append_strategy_file_mounted(
        &mut self,
        now_ms: i64,
        mount_id: &str,
        strategy_file: &crate::JournalStrategyFile,
    ) -> io::Result<u64> {
        self.append(crate::format::STORED_WORDS_VERSION, false, |sequence| {
            JournalRecordRef::StrategyFileMounted { sequence, now_ms, mount_id, strategy_file }
        })
    }

    /// Journal one FORECAST mount `mount_id`'s strategy published at `now_ms` (its decision time) for `symbol` on
    /// `venue`. See [`crate::JournalRecord::Forecast`]: replay-neutral, and the first one raises the header to
    /// `crate::format::FORECAST_VERSION`. Returns the assigned seq.
    pub fn append_forecast(
        &mut self,
        now_ms: i64,
        mount_id: &str,
        venue: &str,
        symbol: &str,
        forecast: vike_model::Forecast,
    ) -> io::Result<u64> {
        self.append(crate::format::FORECAST_VERSION, false, |sequence| JournalRecordRef::Forecast {
            sequence,
            now_ms,
            mount_id,
            venue,
            symbol,
            forecast,
        })
    }

    /// Journal one EXECUTION REPORT, what the OMS states right after a fold (borrowed — no clone). See
    /// [`crate::JournalRecord::ExecutionReport`]: replay-neutral, and the first one raises the header to
    /// `crate::format::EXECUTION_REPORT_VERSION`. Returns the assigned seq.
    ///
    /// The one LAZY frame: no order waits for a report, so it reaches the file with the next record's hand-off,
    /// or once the write thread has nothing else to do (`crate::handoff`'s module doc).
    pub fn append_execution_report(
        &mut self,
        now_ms: i64,
        report: &vike_model::orders::ExecutionReport,
    ) -> io::Result<u64> {
        self.append(crate::format::EXECUTION_REPORT_VERSION, true, |sequence| {
            JournalRecordRef::ExecutionReport { sequence, now_ms, report }
        })
    }

    /// Journal that the reports owed for `(after_sequence, through_sequence]` of journal `journal_id` could not be
    /// regenerated, and why. See [`crate::JournalRecord::ExecutionReportsUnverified`]; the first one raises the
    /// header to `crate::format::EXECUTION_REPORT_VERSION`. Returns the assigned seq.
    pub fn append_execution_reports_unverified(
        &mut self,
        now_ms: i64,
        journal_id: &str,
        (after_sequence, through_sequence): (u64, u64),
        (from_ms, to_ms): (Option<i64>, Option<i64>),
        order_client_ids: &[String],
        reason: &str,
    ) -> io::Result<u64> {
        self.append(crate::format::EXECUTION_REPORT_VERSION, false, |sequence| {
            JournalRecordRef::ExecutionReportsUnverified {
                sequence,
                now_ms,
                journal_id,
                after_sequence,
                through_sequence,
                from_ms,
                to_ms,
                order_client_ids,
                reason,
            }
        })
    }

    /// Journal every account's open positions as the core holds them, or why it cannot know them. See
    /// [`crate::JournalRecord::PositionSnapshot`]: replay-neutral, once per boot, on the
    /// `crate::format::EXECUTION_REPORT_VERSION` step. Returns the assigned seq.
    pub fn append_position_snapshot(
        &mut self,
        now_ms: i64,
        accounts: &[crate::JournalPositionSnapshotAccount],
    ) -> io::Result<u64> {
        self.append(crate::format::EXECUTION_REPORT_VERSION, false, |sequence| {
            JournalRecordRef::PositionSnapshot { sequence, now_ms, accounts }
        })
    }

    /// Journal a count of repeated denials and the order client id range their ids came from. See
    /// [`crate::JournalRecord::DeniedRepeat`]: replay-neutral, and the first one raises the header to
    /// `crate::format::DENIED_REPEAT_VERSION`. Returns the assigned seq.
    pub fn append_denied_repeat(
        &mut self,
        now_ms: i64,
        repeat: &crate::JournalDeniedRepeat,
    ) -> io::Result<u64> {
        self.append(crate::format::DENIED_REPEAT_VERSION, false, |sequence| {
            JournalRecordRef::DeniedRepeat { sequence, now_ms, repeat }
        })
    }

    /// **Hold the write-ahead records of the ONE strategy submit about to be lowered** ([`Self::append_strategy_submit`]
    /// and [`Self::append_minted_submit_replacing`] keep a copy instead of framing it) until the core knows whether the
    /// pre-trade gate denies it as it denied the same key before, which it then counts in a
    /// [`crate::JournalRecord::DeniedRepeat`] and drops ([`Self::drop_deferred_submits`]). Anything else frames them, in
    /// order and before itself: any other append, a `Snapshot`, [`Self::flush`], the stop, or
    /// [`Self::end_deferred_submits`], which the core calls before the order can leave the process and once it is
    /// lowered. The sequence numbers are taken at the frame, so a dropped pair leaves no gap.
    pub fn defer_submits(&mut self) {
        if self.deferred.is_none() {
            self.deferred = Some(Vec::with_capacity(2));
        }
    }

    /// How many records [`Self::defer_submits`] holds, `None` while it holds none and is not holding.
    pub fn deferred_submits(&self) -> Option<usize> {
        self.deferred.as_ref().map(Vec::len)
    }

    /// Frame what [`Self::defer_submits`] holds, in order, and stop holding. Nothing to do while nothing is held.
    pub fn end_deferred_submits(&mut self) -> io::Result<()> {
        let Some(held) = self.deferred.take() else { return Ok(()) };
        self.settle_snapshot()?;
        for DeferredSubmit { sequence, needed, payload } in held {
            // can't-happen guard: every other frame frames what is held first
            if sequence != self.sequence {
                return Err(io::Error::other(format!(
                    "a held strategy submit was numbered {sequence} and the journal is at {}",
                    self.sequence
                )));
            }
            self.require_version(needed);
            self.frame_into_stage(&payload, false)?;
        }
        Ok(())
    }

    /// [`Self::append`] for a strategy submit's write-ahead record, or, while [`Self::defer_submits`] holds, its
    /// frame kept (encoded under the sequence it will take) and that sequence returned.
    fn append_or_hold<'r>(
        &mut self,
        needed: u32,
        record: impl FnOnce(u64) -> JournalRecordRef<'r>,
    ) -> io::Result<u64> {
        if self.deferred.is_none() {
            return self.append(needed, false, record);
        }
        let sequence = self.next_sequence();
        let mut payload = Vec::new();
        crate::frame::encode_into(&mut payload, &record(sequence));
        if let Some(held) = self.deferred.as_mut() {
            held.push(DeferredSubmit { sequence, needed, payload });
        }
        Ok(sequence)
    }

    /// Drop, unwritten, what [`Self::defer_submits`] holds and stop holding: the order was denied and the denial is
    /// counted ([`crate::JournalRecord::DeniedRepeat`]). Returns how many records were dropped.
    pub fn drop_deferred_submits(&mut self) -> usize {
        self.deferred.take().map_or(0, |held| held.len())
    }

    /// Journal a full-state snapshot-as-command (borrowed slices — no clone). `arm_sequence` is the
    /// runtime's emulated-conditional arm-id counter, stamped next to `order_client_id_sequence`; `conditionals`
    /// is the resting conditional books, in fire order; `contingencies` is the resting OTO/OCO
    /// book (held exits carry their request); `mount_attribution` is the per-mount attribution ledgers;
    /// `gateway_cursors` is each gateway ring's resume point, one `(route key, cursor)` per account —
    /// the four ride together as [`JournalSnapshotParts`]; see the [`crate::JournalRecord::Snapshot`] field docs for
    /// why each rides the Snapshot. Returns the seq.
    pub fn append_snap(
        &mut self,
        now_ms: i64,
        engines: &[vike_execution::EngineSnapshot],
        order_client_id_session: &str,
        order_client_id_sequence: u64,
        arm_sequence_value: u64,
        books: JournalSnapshotParts<'_>,
        hash: u64,
    ) -> io::Result<u64> {
        // A `Snapshot` holding a foreign-quoted symbol's rate table needs the header a build that reads
        // it as base units cannot pass (v20), and one whose contingency book holds an OUO leg the
        // header a build that reads it as OCO cannot pass (v22), and one holding a release that names a
        // price venue the header a build that cannot parse it refuses (v24): normally already raised by
        // the record that made it, and carried through every roll; this is one `any` over each per `Snapshot`.
        let needed =
            snap_version_needed(engines, books.contingencies, books.held_releases, books.emulated);
        self.append(needed, false, |sequence| {
            snapshot_record(
                sequence,
                now_ms,
                engines,
                order_client_id_session,
                order_client_id_sequence,
                arm_sequence_value,
                books,
                hash,
            )
        })
    }

    /// **The cadence `Snapshot` of a live core, ENCODED ON THE WRITE THREAD** (decision 0279, amended 2026-11-19):
    /// [`Self::append_snap`] for parts the core hands over by value, with the hash
    /// (`vike_execution::state_hash` of `engines`), the msgpack and the frame check computed off the core thread.
    /// The core thread only moves the parts into a job here (and later copies the encoded frame into its stage).
    ///
    /// The `Snapshot` takes the sequence the next record would have taken and the place it would have taken: until
    /// it is framed nothing else is, because every append, [`Self::flush`] and `Drop` settle it first: frame it once
    /// the write thread has encoded it, wait while it is encoding it (CPU, never I/O), or encode it right there when
    /// the write thread has not taken it up yet. The core's idle point ([`Self::settle_pending_snapshot`]) frames it
    /// once it is encoded and never waits for the encode; past its bound it encodes one the write thread has not taken
    /// up ([`Self::settle_snapshot_unless_encoding`]). So the file holds the bytes [`Self::append_snap`] would have written for the same parts, at the
    /// same position, and a restore reads them as it always did. What is NOT the same: until it is settled the
    /// `Snapshot` is in no page cache, which is decision 0279's accepted window for a non-order record; and an
    /// order's [`crate::WriteFence`] never waits for it (it is no record of the order). The checkpoint sync is
    /// asked for when it is framed. Returns its sequence.
    pub fn append_snap_deferred(
        &mut self,
        now_ms: i64,
        engines: Vec<EngineSnapshot>,
        order_client_id_session: String,
        (order_client_id_sequence, arm_sequence_value): (u64, u64),
        books: JournalSnapshotBooks,
    ) -> io::Result<u64> {
        self.settle_snapshot()?;
        self.end_deferred_submits()?;
        self.require_version(snap_version_needed(
            &engines,
            &books.contingencies,
            &books.held_releases,
            &books.emulated,
        ));
        let sequence = self.sequence;
        self.hand_snapshot(Box::new(move || {
            let hash = vike_execution::state_hash(&engines);
            let mut payload = Vec::new();
            let record = snapshot_record(
                sequence,
                now_ms,
                &engines,
                &order_client_id_session,
                order_client_id_sequence,
                arm_sequence_value,
                books.parts(),
                hash,
            );
            crate::frame::encode_into(&mut payload, &record);
            payload
        }));
        Ok(sequence)
    }
}
