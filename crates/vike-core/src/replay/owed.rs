//! **What a boot owes the execution stream** (package 2's C.1b): no execution report a crash cut off is lost
//! silently. A report is journaled right after the fold it states, before any `Snapshot` its cause can be followed
//! by, and a crash cuts only the journal's TAIL (decision 0279). So a report can be owed only for a cause after the
//! latest `Snapshot` and at or after the newest durable report's cause (that cause may have owed several, and only
//! some were written): the [`ReportWindow`]. Each owed report is either
//!
//! - **regenerated**: the restore's refold folded the tail through replay cores that state their folds like the live
//!   core (`crates/vike-core/src/replay/refold.rs`'s `replayed_reports`), and every report of a window cause beyond
//!   the ones the journal already holds (counted per `(caused_by_sequence, order_client_id)`) is journaled again by
//!   the next core, before its boot `Snapshot` ([`OwedExecutionReports::reports`]); its account is read off its cause
//!   record's gateway ring, and a mount its replay core did not hold is named by its strategy code in the journal's own
//!   rows, never from replay state; or
//! - **recorded as unverified**: one [`ExecutionReportsUnverifiedWindow`] names the window, the orders known to be
//!   affected and why, when the refold did not run (snap-only, a torn tail it could not refold, no checkpoint, a
//!   broken journal), skipped a watchdog tick in the window, regenerated a report on an order outside its base
//!   (ended before the `Snapshot`'s window of ended orders: its state after the event is not known), or withheld the
//!   reports of an order whose pre-trade decision read market data the journal does not hold ([`MARKET_DATA_UNVERIFIED`]:
//!   a replay has its base `Snapshot`'s marks only, so it may accept what the live core denied, or the other way round,
//!   and a regenerated ACCEPTED or DENIED must never be wrong).
//!
//! A second boot owes nothing: the regenerated reports are counted as written, and the unverified record is a floor.
//!
//! **The journal's identity and its moved-aside predecessors** live in `<journal_dir>/execution_stream.json`, the
//! publisher's file, read and written through `vike_journal::JournalExecutionStream`'s `load` and `store`, its id
//! minted by `JournalExecutionStream::mint_journal_id`. A broken journal moved aside becomes the fresh journal's
//! predecessor there ([`record_predecessor`]), so the publisher still sends its readable executions. A file that
//! exists and does not read refuses the boot (`crate::replay::restore_for_boot`): nothing is moved and no file is
//! written over it, because a fresh one would lose the journal's id, its floor and its undrained predecessors.

use std::collections::HashMap;
use std::path::Path;

use vike_journal::{
    JournalExecutionStream, JournalExecutionStreamPredecessor, JournalRecord, record_now_ms,
};
use vike_model::accounts::account_keys::label_of_route_key;
use vike_model::orders::client_id::{
    OrderClientIdReading, OrderClientIdSource, OrderClientIdStrategyCode, parse_order_client_id,
};
use vike_model::orders::{ExecutionOwnership, ExecutionReport};

use super::boot::RestoreOutcome;
use super::fold::tail_facts;
use super::refold::JournalReplayReports;

/// The reason a REPLAY core withholds an order's reports (its `ExecutionReportsUnverified` marker, `vike_core`'s
/// `CoreThread::journal_execution_report`): the order's pre-trade decision read market data the journal does not hold,
/// so the replay may have accepted what the live core denied, or the other way round. The boot's unverified record
/// says it of those orders.
pub(crate) const MARKET_DATA_UNVERIFIED: &str =
    "decided on market data the journal does not hold (a mark, a price or a book)";

/// What a boot owes the execution stream, as `crate::CoreConfig::owed_execution_reports` holds it: set by
/// `RestoreReport::apply_to` alone, journaled once by `assemble_core` right after the journal opens.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OwedExecutionReports {
    /// `(now_ms, report)`: each report a crash cut off that the refold regenerated, under its cause's sequence and
    /// clock, journaled again under a sequence of its own.
    pub reports: Vec<(i64, ExecutionReport)>,
    /// The records whose reports could not be regenerated, said once at ERROR.
    pub unverified: Option<ExecutionReportsUnverifiedWindow>,
}

/// The fields of one `vike_journal::JournalRecord::ExecutionReportsUnverified`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionReportsUnverifiedWindow {
    /// The boot's clock.
    pub now_ms: i64,
    /// The journal the window is of: its stream file's id, the moved-aside journal's for a broken one. Always a real
    /// id (`JournalExecutionStream::mint_journal_id`'s, minted once): a journal with no stream file yet is given its
    /// file by the boot that owes the window, and the publisher then streams under that same id.
    pub journal_id: String,
    pub after_sequence: u64,
    pub through_sequence: u64,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub order_client_ids: Vec<String>,
    pub reason: String,
}

/// The records whose reports a crash can have cut off, read off the journal before the refold consumes it: every
/// record after the latest `Snapshot` and the newest `ExecutionReportsUnverified`, from the newest durable report's
/// cause on. `None` when none of them is a cause ([`is_report_cause`]).
pub(super) struct ReportWindow {
    /// The first sequence of the window (a record's sequences strictly rise through a journal).
    from_sequence: u64,
    through_sequence: u64,
    from_ms: i64,
    to_ms: i64,
    /// The order client ids its records name: minted, named by a command, or by a venue event.
    order_client_ids: Vec<String>,
    /// Its watchdog ticks: the refold drops them, so what their sweeps decided is not regenerated.
    wakers: usize,
    /// The ring of each gateway event in it, by sequence: an owed report's account is its cause's.
    rings: HashMap<u64, String>,
    /// How many reports of each `(caused_by_sequence, order_client_id)` of a window cause the journal holds.
    reported: HashMap<(u64, String), usize>,
    /// The mount each strategy code names in the journal's durable rows ([`mounts_by_code`]).
    mounts_by_code: Vec<(OrderClientIdStrategyCode, String)>,
}

/// **The mount each strategy code names, from the journal alone** (C.1a: a late event's strategy comes from its id,
/// never from replay state): the code a `Snapshot`'s mount row carries, then the code of every v2 strategy id the
/// journal says a mount minted (its `StrategySubmit` provenance). A code two mounts claim names none.
fn mounts_by_code(records: &[JournalRecord]) -> Vec<(OrderClientIdStrategyCode, String)> {
    let mut claims: Vec<(OrderClientIdStrategyCode, String)> = Vec::new();
    // The orders a `Snapshot` says its mounts own count as minted (decision 0304: the boot reads from the newest
    // `Snapshot` on, so their mints are below it).
    let mut owned: Vec<(String, String)> = Vec::new();
    for rec in records {
        if let JournalRecord::Snapshot { mount_attribution, order_client_id_mounts, .. } = rec {
            for row in mount_attribution.iter().filter(|r| r.venue.is_none()) {
                if let Some(code) = row.order_client_id_code {
                    claims.push((code, row.mount_id.clone()));
                }
            }
            owned.extend(order_client_id_mounts.iter().cloned());
        }
    }
    owned.extend(super::fold_order_client_id_mounts(records));
    for (order_client_id, mount_id) in owned {
        if let Some(OrderClientIdReading::V2(parts)) = parse_order_client_id(&order_client_id)
            && parts.src == OrderClientIdSource::Strategy
        {
            claims.push((parts.strat, mount_id));
        }
    }
    // ONE pass: a claim per order a strategy minted, so a pairwise check was quadratic in a whole journal's orders
    // (833 260 orders of one mount on the CI box, 2026-11-24: about an hour of it at a boot). A code keeps its first mount
    // until another mount claims it.
    let mut mount_of: HashMap<OrderClientIdStrategyCode, Option<&str>> = HashMap::new();
    let mut codes: Vec<OrderClientIdStrategyCode> = Vec::new();
    for (code, mount_id) in &claims {
        let mount = mount_of.entry(*code).or_insert_with(|| {
            codes.push(*code);
            Some(mount_id.as_str())
        });
        if *mount != Some(mount_id.as_str()) {
            *mount = None;
        }
    }
    codes.into_iter().filter_map(|c| Some((c, mount_of[&c]?.to_string()))).collect()
}

impl ReportWindow {
    pub(super) fn of(records: &[JournalRecord]) -> Option<Self> {
        use vike_journal::record_sequence;
        let from_sequence = records.iter().fold(0u64, |from, r| {
            let at = match r {
                JournalRecord::Snapshot { sequence, .. }
                | JournalRecord::ExecutionReportsUnverified { sequence, .. } => *sequence + 1,
                JournalRecord::ExecutionReport { report, .. } => report.caused_by_sequence,
                // every other record: no floor
                _ => return from,
            };
            from.max(at)
        });
        let start = records.partition_point(|r| record_sequence(r) < from_sequence);
        let window = &records[start..];
        if !window.iter().any(is_report_cause) {
            return None;
        }
        let (first, last) = (&window[0], &window[window.len() - 1]);
        let mut order_client_ids: Vec<String> =
            tail_facts(window).used.into_iter().map(|(id, _)| id).collect();
        let mut wakers = 0usize;
        let mut rings: HashMap<u64, String> = HashMap::new();
        let mut reported: HashMap<(u64, String), usize> = HashMap::new();
        for r in window {
            match r {
                JournalRecord::Command { sequence, message, .. } => {
                    if matches!(message, vike_execution::Ingest::Watchdog) {
                        wakers += 1;
                    }
                    if let vike_execution::Ingest::GatewayEvent(g) = message {
                        rings.insert(*sequence, g.route_key.to_string());
                    }
                    if let Some(id) =
                        message.venue_event().and_then(vike_model::events::Event::order_client_id)
                        && !id.is_empty()
                    {
                        order_client_ids.push(id.to_string());
                    }
                }
                JournalRecord::ExecutionReport { report, .. } => {
                    *reported
                        .entry((report.caused_by_sequence, report.order_client_id.clone()))
                        .or_default() += 1;
                }
                // every other record: nothing this window reads
                _ => {}
            }
        }
        Some(ReportWindow {
            from_sequence: record_sequence(first),
            through_sequence: record_sequence(last),
            from_ms: record_now_ms(first),
            to_ms: record_now_ms(last),
            order_client_ids: distinct(order_client_ids),
            wakers,
            rings,
            reported,
            mounts_by_code: mounts_by_code(records),
        })
    }

    /// The reports `replayed` states for window causes beyond the ones the journal holds, each with its cause's ring
    /// account (the live core's rule: the ring's account, else what the fold knew for ours, else none for foreign),
    /// and, for one of ours whose replay core held no mount of its strategy code, the mount the journal's rows name.
    fn owed(&self, placed: Vec<(i64, ExecutionReport)>) -> Vec<(i64, ExecutionReport)> {
        let mut seen: HashMap<(u64, String), usize> = HashMap::new();
        let mut owed = Vec::new();
        for (now_ms, mut report) in placed {
            if report.caused_by_sequence < self.from_sequence {
                continue;
            }
            let key = (report.caused_by_sequence, report.order_client_id.clone());
            let n = seen.entry(key.clone()).or_default();
            *n += 1;
            if *n <= self.reported.get(&key).copied().unwrap_or(0) {
                continue; // written before the crash
            }
            if let Some(ring) = self.rings.get(&report.caused_by_sequence) {
                match label_of_route_key(&report.venue, ring) {
                    Some(label) => report.account = Some(label.to_string()),
                    None if report.ownership == ExecutionOwnership::Foreign => {
                        report.account = None
                    }
                    None => {}
                }
            }
            if report.ownership == ExecutionOwnership::Ours
                && report.mount_id.is_none()
                && let Some(code) = report.strategy_code
            {
                report.mount_id =
                    self.mounts_by_code.iter().find(|(c, _)| *c == code).map(|(_, m)| m.clone());
            }
            owed.push((now_ms, report));
        }
        owed
    }

    fn unverified(
        &self,
        order_client_ids: Vec<String>,
        reasons: &[String],
        now_ms: i64,
    ) -> ExecutionReportsUnverifiedWindow {
        ExecutionReportsUnverifiedWindow {
            now_ms,
            journal_id: String::new(),
            // "since N" is exclusive; a window from a journal's first record (sequence 0) says 0
            after_sequence: self.from_sequence.saturating_sub(1),
            through_sequence: self.through_sequence,
            from_ms: Some(self.from_ms),
            to_ms: Some(self.to_ms),
            order_client_ids: distinct(order_client_ids),
            reason: reasons.join("; "),
        }
    }
}

/// **What the boot owes** after a restore that went `outcome` deep: the regenerated reports when the refold ran
/// (`replayed`), and the unverified window for what it could not regenerate (the module doc). `now_ms` is the boot's.
pub(super) fn owed_execution_reports(
    outcome: &RestoreOutcome,
    window: Option<ReportWindow>,
    replayed: Option<JournalReplayReports>,
    now_ms: i64,
) -> OwedExecutionReports {
    let Some(window) = window else { return OwedExecutionReports::default() };
    let Some(replayed) = replayed else {
        let reason = match outcome {
            RestoreOutcome::Off => return OwedExecutionReports::default(),
            RestoreOutcome::SnapOnly { why } => format!("the restore refolded nothing ({why})"),
            RestoreOutcome::TornTail { .. } => {
                "the torn journal's readable records were not refolded".to_string()
            }
            RestoreOutcome::Fresh { .. } => {
                "the journal holds no checkpoint to refold from".to_string()
            }
            RestoreOutcome::Broken { cause, .. } => {
                format!("the journal was moved aside broken ({})", cause.as_str())
            }
            RestoreOutcome::Full => "the restore regenerated nothing".to_string(),
        };
        let ids = window.order_client_ids.clone();
        return OwedExecutionReports {
            reports: Vec::new(),
            unverified: Some(window.unverified(ids, &[reason], now_ms)),
        };
    };
    let reports = window.owed(replayed.placed);
    let outside: Vec<String> = reports
        .iter()
        .filter(|(_, r)| r.ownership == ExecutionOwnership::Ours && r.order.is_none())
        .map(|(_, r)| r.order_client_id.clone())
        .collect();
    let mut reasons = Vec::new();
    if window.wakers > 0 {
        reasons.push(format!("{} watchdog tick(s) the restore does not replay", window.wakers));
    }
    if !outside.is_empty() {
        reasons.push(format!(
            "{} report(s) on orders outside the restore's base, whose state after the event is not known",
            outside.len()
        ));
    }
    if !replayed.unplaced.is_empty() {
        reasons.push(format!(
            "{} report(s) the refold could not place in the journal",
            replayed.unplaced.len()
        ));
    }
    // the orders a replay core withheld, of a cause the crash may have cut a report of
    let market: Vec<String> = distinct(
        replayed
            .market_dependent
            .into_iter()
            .filter(|(cause, _)| *cause >= window.from_sequence)
            .map(|(_, id)| id)
            .collect(),
    );
    if !market.is_empty() {
        reasons.push(format!("{} order(s) {MARKET_DATA_UNVERIFIED}", market.len()));
    }
    let unverified = (!reasons.is_empty()).then(|| {
        let ids = outside.into_iter().chain(replayed.unplaced).chain(market).collect();
        window.unverified(ids, &reasons, now_ms)
    });
    OwedExecutionReports { reports, unverified }
}

/// Whether `rec`'s fold can owe a report: every record but a checkpoint, an observation and a report itself. A new
/// variant counts as a cause until it says otherwise (the loud direction).
fn is_report_cause(rec: &JournalRecord) -> bool {
    !matches!(
        rec,
        JournalRecord::Snapshot { .. }
            | JournalRecord::PortfolioSnapshot { .. }
            | JournalRecord::Forecast { .. }
            | JournalRecord::StrategyFileMounted { .. }
            | JournalRecord::ExecutionReport { .. }
            | JournalRecord::ExecutionReportsUnverified { .. }
            | JournalRecord::PositionSnapshot { .. }
            | JournalRecord::DeniedRepeat { .. }
    )
}

/// `ids` without repeats, in first-seen order.
fn distinct(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter().filter(|id| seen.insert(id.clone())).collect()
}

/// A stream file for a journal that has none yet, under a fresh id: THE identity of that journal from here on, so a
/// caller stores it before it hands the id out.
pub(super) fn fresh_execution_stream() -> JournalExecutionStream {
    JournalExecutionStream {
        journal_id: JournalExecutionStream::mint_journal_id(),
        acknowledged_sequence: None,
        acknowledged_ms: None,
        predecessors: Vec::new(),
    }
}

/// **A broken journal moved aside is still drained** (C.1b): the journal now at `moved_to`, whose stream file was
/// `stream` (read before the move; a journal with none gets one, under a fresh id, so its records have an identity
/// to be sent under), becomes the PREDECESSOR of the fresh journal at `dir`: a new stream file there, under a fresh
/// id, lists it with its floor, after the predecessors it had not drained yet. Returns the moved journal's id, which
/// is a real one even when a write failed, and whether both files were written.
pub(super) fn record_predecessor(
    dir: &Path,
    moved_to: &Path,
    stream: Option<JournalExecutionStream>,
) -> (String, std::io::Result<()>) {
    let (moved, written) = match stream {
        Some(stream) => (stream, Ok(())),
        None => {
            let fresh = fresh_execution_stream();
            let written = fresh.store(moved_to);
            (fresh, written)
        }
    };
    let mut predecessors: Vec<JournalExecutionStreamPredecessor> =
        moved.predecessors.into_iter().filter(|p| !p.done).collect();
    predecessors.push(JournalExecutionStreamPredecessor {
        journal_id: moved.journal_id.clone(),
        path: moved_to.to_path_buf(),
        acknowledged_sequence: moved.acknowledged_sequence,
        done: false,
    });
    let listed = JournalExecutionStream { predecessors, ..fresh_execution_stream() };
    (moved.journal_id, written.and_then(|()| listed.store(dir)))
}

#[cfg(test)]
mod window_tests {
    use std::time::{Duration, Instant};

    use vike_model::orders::client_id::{
        OrderClientIdAccountCode, OrderClientIdGenerator, OrderClientIdIntent, OrderClientIdLabels,
        OrderClientIdPlan, OrderClientIdShape,
    };

    use super::*;

    /// `n` orders mount `mount_id` minted under strategy code `code` from sequence `seq`: its `StrategySubmit`, then
    /// one `MintedSubmit` per order (ownership stays open across them).
    fn mints(seq: u64, mount_id: &str, code: &str, n: u64) -> Vec<JournalRecord> {
        let mut ids = OrderClientIdGenerator::resume("W00000000abcd".into(), seq);
        let plan =
            OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(3));
        let labels = OrderClientIdLabels::new(
            OrderClientIdSource::Strategy,
            OrderClientIdStrategyCode::parse(code).expect("a code"),
            OrderClientIdIntent::OpenLong,
        );
        let submit = JournalRecord::StrategySubmit {
            sequence: seq,
            now_ms: seq as i64,
            mount_id: mount_id.into(),
            intent: vike_execution::OrderIntent::Submit(Box::new(
                vike_execution::testing::resting_limit(""),
            )),
            route_key: None,
        };
        std::iter::once(submit)
            .chain((1..=n).map(|i| JournalRecord::MintedSubmit {
                sequence: seq + i,
                now_ms: (seq + i) as i64,
                request: vike_execution::testing::resting_limit(&plan.mint(&mut ids, labels)),
                route_key: None,
                replaces: None,
            }))
            .collect()
    }

    /// A code one mount claims names it once, in first-seen order; a code two mounts claim names none.
    #[test]
    fn a_code_names_its_one_mount_and_a_code_two_mounts_claim_names_none() {
        let records: Vec<JournalRecord> = [
            mints(0, "maker", "0001", 3),
            mints(10, "taker", "0002", 2),
            mints(20, "other", "0001", 1),
            mints(30, "taker", "0002", 1),
        ]
        .into_iter()
        .flatten()
        .collect();
        let code = |c: &str| OrderClientIdStrategyCode::parse(c).expect("a code");
        assert_eq!(mounts_by_code(&records), vec![(code("0002"), "taker".to_string())]);
    }

    /// **The boot's window is linear in the journal's orders**: a day of a strategy's quotes is hundreds of thousands
    /// of mints, and a pairwise check of their claims (`mounts_by_code` before 2026-11-24) kept a boot on such a
    /// journal in `restore_for_boot` for about an hour. 100 000 orders of one mount, in a debug build: under a second
    /// now; the pairwise check took minutes.
    #[test]
    fn the_report_window_of_a_hundred_thousand_strategy_orders_reads_in_linear_time() {
        let records = mints(0, "maker", "0001", 100_000);
        let started = Instant::now();
        let window = ReportWindow::of(&records).expect("the mints are report causes");
        let took = started.elapsed();
        assert_eq!(window.order_client_ids.len(), 100_000);
        assert_eq!(window.mounts_by_code.len(), 1);
        assert!(took < Duration::from_secs(10), "the report window took {took:?}");
    }
}
