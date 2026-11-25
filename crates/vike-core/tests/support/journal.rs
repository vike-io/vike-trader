//! Journal helpers: reading a journal back, copying its segments, re-framing a doctored payload,
//! and the shared source scenario the replay suites fence.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::{Command, EngineSnapshot, OrderIntent};
use vike_journal::{Journal, JournalFileConfig, JournalRecord, JournalSnapshotParts};
use vike_model::Clock;

use crate::kit::engines::sim_engine;
use crate::kit::events::sim_bare_fill;
use crate::kit::requests::sim_limit;

/// The journal knobs a scenario runs with: `segment_bytes` per segment (1 MiB unless a test needs
/// them to roll), flushed every 8 records, a cadence Snapshot every `snapshot_every` records. Both knobs
/// are the caller's, so the value a test reads stays at its own site.
pub(crate) fn journal_at(dir: &Path, segment_bytes: u64, snapshot_every: u64) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes, flush_every: 8 },
        snapshot_every,
    }
}

/// The archive pass over a journal nothing writes any more, with no keep window and no archive limit: every
/// segment its newest `Snapshot` superseded (and the execution stream's floor lets go) moves to `<dir>/archive/`.
pub(crate) fn prune_closed(dir: &Path) -> vike_journal::JournalArchivePass {
    let everything = vike_journal::JournalArchivePolicy {
        keep_ms: 0,
        max_bytes: u64::MAX,
        max_age_ms: i64::MAX,
        at_limit: vike_model::JournalArchiveFull::Keep,
    };
    let closed = vike_journal::JournalDurableMark { segment_index: u64::MAX, offset: usize::MAX };
    Journal::prune_before_latest_snap(dir, &everything, i64::MAX, closed).expect("the archive pass")
}

/// A self-advancing clock (message `k` dispatches at `now_ms == k`).
pub(crate) fn counter_clock() -> Box<dyn Clock + Send> {
    let t = Arc::new(AtomicI64::new(0));
    Box::new(move || t.fetch_add(1, Ordering::Relaxed))
}

/// A clock that reads `now`, which the test moves by hand.
pub(crate) fn clock(now: &Arc<AtomicI64>) -> Box<dyn Clock + Send> {
    let now = Arc::clone(now);
    Box::new(move || now.load(Ordering::Relaxed))
}

/// Every record of the journal at `dir`, in order.
pub(crate) fn records(dir: &Path) -> Vec<JournalRecord> {
    Journal::read_all(dir).unwrap()
}

/// Every `Snapshot` of `recs`, in order, as `(hash, engines)`.
pub(crate) fn snaps(recs: &[JournalRecord]) -> Vec<(u64, Vec<EngineSnapshot>)> {
    recs.iter()
        .filter_map(|r| match r {
            JournalRecord::Snapshot { hash, engines, .. } => Some((*hash, engines.clone())),
            _ => None,
        })
        .collect()
}

/// Write `recs` verbatim into a fresh journal at `dst` (valid frames, current header version).
/// The doctoring tests transform a REAL session's records (drop one kind, edit one field) and
/// re-emit them through this, so only what the test changed differs from a journal a live core
/// would have written.
pub(crate) fn rewrite_journal(dst: &Path, recs: Vec<JournalRecord>) {
    let mut j = Journal::open(dst, JournalFileConfig::default()).unwrap();
    for r in recs {
        match r {
            JournalRecord::Command { now_ms, message: msg, .. } => {
                j.append_command(now_ms, &msg).unwrap();
            }
            JournalRecord::Snapshot {
                now_ms,
                engines,
                order_client_id_session,
                order_client_id_sequence,
                arm_sequence: arm_sequence_value,
                order_emulation_arms: conditionals,
                contingencies,
                mount_attribution: mount_attribute,
                gateway_cursors,
                held_releases,
                recent_order_client_ids,
                emulated,
                closed_trades,
                awaiting_engines,
                order_client_id_mounts,
                order_client_id_past_sessions,
                hash,
                ..
            } => {
                j.append_snap(
                    now_ms,
                    &engines,
                    &order_client_id_session,
                    order_client_id_sequence,
                    arm_sequence_value.unwrap_or(0),
                    JournalSnapshotParts {
                        order_emulation_arms: &conditionals,
                        contingencies: &contingencies,
                        mount_attribution: &mount_attribute,
                        gateway_cursors: &gateway_cursors,
                        held_releases: &held_releases,
                        recent_order_client_ids: &recent_order_client_ids,
                        emulated: &emulated,
                        closed_trades: &closed_trades,
                        awaiting_engines: &awaiting_engines,
                        order_client_id_mounts: &order_client_id_mounts,
                        order_client_id_past_sessions: &order_client_id_past_sessions,
                    },
                    hash,
                )
                .unwrap();
            }
            // an amend's replacement keeps the order it `replaces`: a refold places it there
            JournalRecord::MintedSubmit { now_ms, request: req, replaces, .. } => {
                j.append_minted_submit_replacing(now_ms, &req, None, replaces.as_deref()).unwrap();
            }
            JournalRecord::StrategySubmit { now_ms, mount_id, intent, .. } => {
                j.append_strategy_submit(now_ms, &mount_id, &intent, None).unwrap();
            }
            JournalRecord::OrderEmulationArmed { now_ms, arm_id, resolved, .. } => {
                j.append_conditional_armed(now_ms, &arm_id, &resolved).unwrap();
            }
            JournalRecord::OrderEmulationDisarmed { now_ms, arm_id, .. } => {
                j.append_conditional_disarmed(now_ms, &arm_id).unwrap();
            }
            JournalRecord::OrderEmulationFire {
                now_ms,
                arm_id,
                trigger_price: trigger_px,
                request: req,
                ..
            } => {
                j.append_conditional_fire(now_ms, &arm_id, trigger_px, &req, None).unwrap();
            }
            JournalRecord::PortfolioSnapshot { now_ms, sample, .. } => {
                j.append_portfolio_snap(now_ms, &sample).unwrap();
            }
            JournalRecord::MarginCallLiquidate { now_ms, request: req, mount_id, .. } => {
                j.append_margin_call_liquidate(now_ms, &req, mount_id.as_deref(), None).unwrap();
            }
            JournalRecord::GtdExpire { now_ms, order_client_id, engine, .. } => {
                j.append_gtd_expire(now_ms, &order_client_id, engine).unwrap();
            }
            JournalRecord::ScheduleFire { now_ms, mount_id, tag, .. } => {
                j.append_schedule_fire(now_ms, &mount_id, &tag).unwrap();
            }
            JournalRecord::SharedCloseTimeout { now_ms, mount_id, close_ms, input, .. } => {
                j.append_shared_close_timeout(now_ms, &mount_id, close_ms, &input).unwrap();
            }
            JournalRecord::StrategyFileMounted { now_ms, mount_id, strategy_file, .. } => {
                j.append_strategy_file_mounted(now_ms, &mount_id, &strategy_file).unwrap();
            }
            JournalRecord::Forecast { now_ms, mount_id, venue, symbol, forecast, .. } => {
                j.append_forecast(now_ms, &mount_id, &venue, &symbol, forecast).unwrap();
            }
            JournalRecord::ExecutionReport { now_ms, report, .. } => {
                j.append_execution_report(now_ms, &report).unwrap();
            }
            JournalRecord::ExecutionReportsUnverified {
                now_ms,
                journal_id,
                after_sequence,
                through_sequence,
                from_ms,
                to_ms,
                order_client_ids,
                reason,
                ..
            } => {
                j.append_execution_reports_unverified(
                    now_ms,
                    &journal_id,
                    (after_sequence, through_sequence),
                    (from_ms, to_ms),
                    &order_client_ids,
                    &reason,
                )
                .unwrap();
            }
            JournalRecord::PositionSnapshot { now_ms, accounts, .. } => {
                j.append_position_snapshot(now_ms, &accounts).unwrap();
            }
            JournalRecord::DeniedRepeat { now_ms, repeat, .. } => {
                j.append_denied_repeat(now_ms, &repeat).unwrap();
            }
        }
    }
    j.flush().unwrap();
    drop(j);
}

/// **A crash copy of a journal a core is still writing** (`kill -9` as it stands): [`copy_journal_files`] once the files
/// at `src` hold what `holds` waits for (`what`, said if it never comes). The journal's write thread lands a record
/// after the core has published what it folded, so a copy taken at the publish alone can miss it.
pub(crate) fn copy_journal_files_once(
    src: &Path,
    dst: &Path,
    what: &str,
    holds: impl Fn(&[JournalRecord]) -> bool,
) {
    crate::kit::handle::poll_within(std::time::Duration::from_secs(10), what, || {
        Journal::read_all(src).is_ok_and(|records| holds(&records))
    });
    copy_journal_files(src, dst);
}

/// Copy only the `.vjl` segment files (not any temp/lock artifacts) from `src` into `dst`.
pub(crate) fn copy_journal_files(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|s| s.to_str()) == Some("vjl") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
}

/// Run the source scenario, producing a journal at `dir` that spans a cadence Snapshot + the exit
/// Snapshot. `snapshot_every = 4`, 7 exec-lane messages: 4 bare fills (the 4th trips the cadence
/// Snapshot = the replay BASE), then a `Submit` + a bare fill + a `Cancel` in the tail (so the tail
/// is non-empty AND carries both an `Ingest::Event` and an `Ingest::Command`).
pub(crate) fn build_source_journal(dir: &Path) {
    let engine = sim_engine();
    // self-advancing clock: message k is dispatched with now_ms == k (distinct per message, so a
    // wrong JournalReplayQueueClock replay of the Submit's created_ms would break the hash)
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        order_client_id_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snapshot (BASE)
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(sim_limit(
        "ord1", 1, 1.0, 100.0,
    )))));
    sender.blocking_send(sim_bare_fill("t5", 1.0, 100.0)).unwrap();
    handle.send_command(Command::Order(OrderIntent::Cancel("ord1".into())));
    handle.shutdown_and_join();
}
