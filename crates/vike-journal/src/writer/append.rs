//! The `append_*` verbs of [`CommandJournal`]: one per record variant a live core journals, each
//! serializing the borrowed write twin of the record and handing the payload to `write_framed`.
//!
//! Split out of `writer.rs` by concern, bodies verbatim. Every verb has the same shape on purpose —
//! read `self.seq`, serialize, `write_framed`, return the seq — and `write_framed` (the framing, the
//! roll trigger, the sync cadence) stays in the parent with the state it mutates. These verbs are
//! the entry points the fold calls, so the `p99` core-hop gate covers them: add no work here.

use std::io;

use vike_exec::Ingest;

use super::CommandJournal;
use crate::record::{
    ConditionalRecord, JournalRecordRef, PortfolioSample, SnapConditional, SnapContingency,
    SnapMountAttr,
};

impl CommandJournal {
    /// Journal one exec-lane ingest message (borrowed — no clone). Returns the assigned seq.
    pub fn append_cmd(&mut self, now_ms: i64, msg: &Ingest) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::Cmd { seq, now_ms, msg })
            .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one mounted strategy's order intent at the `drain_broker` boundary (borrowed — no
    /// clone), write-ahead of the `apply_intent` call it records (mirrors `append_cmd`). Returns
    /// the assigned seq.
    pub fn append_strategy_submit(
        &mut self,
        now_ms: i64,
        mount_id: &str,
        intent: &vike_exec::OrderIntent,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload =
            serde_json::to_vec(&JournalRecordRef::StrategySubmit { seq, now_ms, mount_id, intent })
                .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal a server-minted submit's RESOLVED request (borrowed — no clone), from inside
    /// `apply_intent` right AFTER the coid is minted for an incoming EMPTY-coid submit. This closes
    /// the minted-coid `exec_order` gap: the write-ahead `Cmd` for this order carried an empty coid,
    /// so without this record the materializer could never tie the minted coid to its
    /// `(venue, symbol, qty, …)` for an order that terminalizes without filling. See
    /// [`crate::JournalRecord::MintedSubmit`]. Returns the assigned seq.
    ///
    /// `route_key` is the RESOLVED routing key of the engine this submit was lowered onto, or
    /// `None` when that engine is the request's venue's sole/default account — the shape
    /// `vike_exec::ReconcileReports::route_key` established, so a single-account box's journal
    /// bytes do not move. See [`crate::JournalRecord::MintedSubmit`] for why the record carries it.
    pub fn append_minted_submit(
        &mut self,
        now_ms: i64,
        req: &vike_model::OrderRequest,
        route_key: Option<&str>,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload =
            serde_json::to_vec(&JournalRecordRef::MintedSubmit { seq, now_ms, req, route_key })
                .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one periodic compact portfolio observation (borrowed — no clone). See
    /// [`crate::JournalRecord::PortfolioSnap`]. Returns the assigned seq.
    pub fn append_portfolio_snap(
        &mut self,
        now_ms: i64,
        sample: &PortfolioSample,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::PortfolioSnap { seq, now_ms, sample })
            .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one emulated conditional's RESOLVED terms right after its `arm_id` is minted
    /// (borrowed — no clone). See [`crate::JournalRecord::ConditionalArmed`]. Returns the assigned seq.
    pub fn append_conditional_armed(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        resolved: &ConditionalRecord,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::ConditionalArmed {
            seq,
            now_ms,
            arm_id,
            resolved,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one emulated conditional's FIRE, write-ahead of the release it causes (borrowed —
    /// no clone). See [`crate::JournalRecord::ConditionalFire`]. Returns the assigned seq.
    pub fn append_conditional_fire(
        &mut self,
        now_ms: i64,
        arm_id: &str,
        trigger_px: f64,
        req: &vike_model::OrderRequest,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::ConditionalFire {
            seq,
            now_ms,
            arm_id,
            trigger_px,
            req,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one emulated conditional's DISARM, write-ahead of the book mutation it records.
    /// See [`crate::JournalRecord::ConditionalDisarmed`]. Returns the assigned seq.
    pub fn append_conditional_disarmed(&mut self, now_ms: i64, arm_id: &str) -> io::Result<u64> {
        let seq = self.seq;
        let payload =
            serde_json::to_vec(&JournalRecordRef::ConditionalDisarmed { seq, now_ms, arm_id })
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
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::MarginCallLiquidate {
            seq,
            now_ms,
            req,
            mount_id,
            route_key,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal one MANAGED GTD/Day expiry DECISION (borrowed coid — no clone), write-ahead of the
    /// `cancel_order` it records. See [`crate::JournalRecord::GtdExpire`] for why this is an audit marker
    /// rather than a replayable command. Returns the assigned seq.
    pub fn append_gtd_expire(&mut self, now_ms: i64, coid: &str, engine: usize) -> io::Result<u64> {
        let seq = self.seq;
        let payload =
            serde_json::to_vec(&JournalRecordRef::GtdExpire { seq, now_ms, coid, engine })
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
        let seq = self.seq;
        let payload =
            serde_json::to_vec(&JournalRecordRef::ScheduleFire { seq, now_ms, mount_id, tag })
                .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }

    /// Journal a full-state snapshot-as-command (borrowed slices — no clone). `arm_seq` is the
    /// runtime's emulated-conditional arm-id counter, stamped next to `coid_seq`; `conditionals`
    /// is the resting conditional books, in fire order; `contingencies` is the resting OTO/OCO
    /// book (held exits carry their request); `mount_attr` is the per-mount attribution ledgers —
    /// see the [`crate::JournalRecord::Snap`] field docs for why all four ride the Snap. Returns the seq.
    pub fn append_snap(
        &mut self,
        now_ms: i64,
        engines: &[vike_exec::EngineSnapshot],
        coid_session: &str,
        coid_seq: u64,
        arm_seq: u64,
        conditionals: &[SnapConditional],
        contingencies: &[SnapContingency],
        mount_attr: &[SnapMountAttr],
        hash: u64,
    ) -> io::Result<u64> {
        let seq = self.seq;
        let payload = serde_json::to_vec(&JournalRecordRef::Snap {
            seq,
            now_ms,
            engines,
            coid_session,
            coid_seq,
            arm_seq,
            conditionals,
            contingencies,
            mount_attr,
            hash,
        })
        .expect("JournalRecordRef serializes");
        self.write_framed(&payload)?;
        Ok(seq)
    }
}
