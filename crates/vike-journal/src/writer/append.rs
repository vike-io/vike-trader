//! The `append_*` verbs of [`Journal`]: one per record variant a live core journals, each
//! serializing the borrowed write twin of the record and handing the payload to `write_framed`.
//!
//! Split out of `writer.rs` by concern, bodies verbatim. Every verb has the same shape on purpose —
//! read `self.seq`, serialize, `write_framed`, return the seq — and `write_framed` (the framing, the
//! roll trigger, the sync cadence) stays in the parent with the state it mutates. These verbs are
//! the entry points the fold calls, so the `p99` core-hop gate covers them: add no work here.

use std::io;

use vike_execution::Ingest;

use super::Journal;
use crate::format::{
    intent_version_needed, release_version_needed, snap_version_needed, version_needed,
};
use crate::record::{
    JournalRecordRef, JournalSnapshotParts, OrderEmulationRecord, PortfolioSample,
};

impl Journal {
    /// Journal one exec-lane ingest message (borrowed — no clone). Returns the assigned seq.
    pub fn append_command(&mut self, now_ms: i64, msg: &Ingest) -> io::Result<u64> {
        // The place a record can need a newer header than the segment carries (`AdmitSymbol`, v19;
        // an `AdmitSymbol` that names a quote currency, v20; `append_snap` has the `Snapshot` twin): the
        // stamp rises here, before the frame, so a journal that never admits a symbol (or never
        // trades a foreign-quoted one) stays readable by the build it replaced.
        self.require_version(version_needed(msg));
        let seq = self.sequence;
        let payload =
            serde_json::to_vec(&JournalRecordRef::Command { sequence: seq, now_ms, message: msg })
                .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
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
        self.require_version(intent_version_needed(intent));
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::StrategySubmit {
            sequence: seq,
            now_ms,
            mount_id,
            intent,
            route_key,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal a server-minted submit's RESOLVED request (borrowed — no clone), from inside
    /// `apply_intent` right AFTER the order client id is minted for an incoming EMPTY-order-client-id submit. This closes
    /// the minted-order-client-id `execution_order` gap: the write-ahead `Command` for this order carried an empty order client id,
    /// so without this record the materializer could never tie the minted order client id to its
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
        self.require_version(release_version_needed(req));
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::MintedSubmit {
            sequence: seq,
            now_ms,
            request: req,
            route_key,
            replaces,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one periodic compact portfolio observation (borrowed — no clone). See
    /// [`crate::JournalRecord::PortfolioSnapshot`]. Returns the assigned seq.
    pub fn append_portfolio_snap(
        &mut self,
        now_ms: i64,
        sample: &PortfolioSample,
    ) -> io::Result<u64> {
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::PortfolioSnapshot {
            sequence: seq,
            now_ms,
            sample,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one emulated conditional's RESOLVED terms right after its `arm_id` is minted
    /// (borrowed — no clone). See [`crate::JournalRecord::OrderEmulationArmed`]. Returns the assigned seq.
    pub fn append_conditional_armed(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        resolved: &OrderEmulationRecord,
    ) -> io::Result<u64> {
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::OrderEmulationArmed {
            sequence: seq,
            now_ms,
            arm_id,
            resolved,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
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
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::OrderEmulationFire {
            sequence: seq,
            now_ms,
            arm_id,
            trigger_price: trigger_px,
            request: req,
            route_key,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one emulated conditional's DISARM, write-ahead of the book mutation it records.
    /// See [`crate::JournalRecord::OrderEmulationDisarmed`]. Returns the assigned seq.
    pub fn append_conditional_disarmed(&mut self, now_ms: i64, arm_id: &str) -> io::Result<u64> {
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::OrderEmulationDisarmed {
            sequence: seq,
            now_ms,
            arm_id,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
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
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::MarginCallLiquidate {
            sequence: seq,
            now_ms,
            request: req,
            mount_id,
            route_key,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
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
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::GtdExpire {
            sequence: seq,
            now_ms,
            order_client_id,
            engine,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
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
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::ScheduleFire {
            sequence: seq,
            now_ms,
            mount_id,
            tag,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
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
        self.require_version(crate::format::SHARED_CLOSE_VERSION);
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::SharedCloseTimeout {
            sequence: seq,
            now_ms,
            mount_id,
            close_ms,
            input,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
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
        self.require_version(crate::format::STORED_WORDS_VERSION);
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::StrategyFileMounted {
            sequence: seq,
            now_ms,
            mount_id,
            strategy_file,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
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
        self.require_version(snap_version_needed(
            engines,
            books.contingencies,
            books.held_releases,
            books.emulated,
        ));
        let seq = self.sequence;
        let payload = serde_json::to_vec(&JournalRecordRef::Snapshot {
            sequence: seq,
            now_ms,
            engines,
            order_client_id_session,
            order_client_id_sequence,
            arm_sequence: arm_sequence_value,
            order_emulation_arms: books.order_emulation_arms,
            contingencies: books.contingencies,
            mount_attribution: books.mount_attribution,
            gateway_cursors: books.gateway_cursors,
            held_releases: books.held_releases,
            recent_order_client_ids: books.recent_order_client_ids,
            emulated: books.emulated,
            closed_trades: books.closed_trades,
            hash,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }
}
