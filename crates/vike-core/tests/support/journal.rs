//! Journal helpers: reading a journal back, copying its segments, re-framing a doctored payload,
//! and the shared source scenario the replay suites fence.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::{Command, EngineSnapshot, OrderIntent};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord, SnapBooks};
use vike_model::Clock;

use crate::kit::engines::sim_engine;
use crate::kit::events::sim_bare_fill;
use crate::kit::requests::sim_limit;

/// The journal knobs a scenario runs with: `segment_bytes` per segment (1 MiB unless a test needs
/// them to roll), flushed every 8 records, a cadence Snap every `snapshot_every` records. Both knobs
/// are the caller's, so the value a test reads stays at its own site.
pub(crate) fn journal_at(dir: &Path, segment_bytes: u64, snapshot_every: u64) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes, flush_every: 8 },
        snapshot_every,
    }
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
    CommandJournal::read_all(dir).unwrap()
}

/// Every `Snap` of `recs`, in order, as `(hash, engines)`.
pub(crate) fn snaps(recs: &[JournalRecord]) -> Vec<(u64, Vec<EngineSnapshot>)> {
    recs.iter()
        .filter_map(|r| match r {
            JournalRecord::Snap { hash, engines, .. } => Some((*hash, engines.clone())),
            _ => None,
        })
        .collect()
}

/// Write `recs` verbatim into a fresh journal at `dst` (valid frames, current header version).
/// The doctoring tests transform a REAL session's records (drop one kind, edit one field) and
/// re-emit them through this, so only what the test changed differs from a journal a live core
/// would have written.
pub(crate) fn rewrite_journal(dst: &Path, recs: Vec<JournalRecord>) {
    let mut j = CommandJournal::open(dst, JournalFileConfig::default()).unwrap();
    for r in recs {
        match r {
            JournalRecord::Cmd { now_ms, msg, .. } => {
                j.append_cmd(now_ms, &msg).unwrap();
            }
            JournalRecord::Snap {
                now_ms,
                engines,
                coid_session,
                coid_seq,
                arm_seq,
                conditionals,
                contingencies,
                mount_attr,
                gateway_cursors,
                hash,
                ..
            } => {
                j.append_snap(
                    now_ms,
                    &engines,
                    &coid_session,
                    coid_seq,
                    arm_seq.unwrap_or(0),
                    SnapBooks {
                        conditionals: &conditionals,
                        contingencies: &contingencies,
                        mount_attr: &mount_attr,
                        gateway_cursors: &gateway_cursors,
                    },
                    hash,
                )
                .unwrap();
            }
            JournalRecord::MintedSubmit { now_ms, req, .. } => {
                j.append_minted_submit(now_ms, &req, None).unwrap();
            }
            JournalRecord::StrategySubmit { now_ms, mount_id, intent, .. } => {
                j.append_strategy_submit(now_ms, &mount_id, &intent, None).unwrap();
            }
            JournalRecord::ConditionalArmed { now_ms, arm_id, resolved, .. } => {
                j.append_conditional_armed(now_ms, &arm_id, &resolved).unwrap();
            }
            JournalRecord::ConditionalDisarmed { now_ms, arm_id, .. } => {
                j.append_conditional_disarmed(now_ms, &arm_id).unwrap();
            }
            JournalRecord::ConditionalFire { now_ms, arm_id, trigger_px, req, .. } => {
                j.append_conditional_fire(now_ms, &arm_id, trigger_px, &req, None).unwrap();
            }
            JournalRecord::PortfolioSnap { now_ms, sample, .. } => {
                j.append_portfolio_snap(now_ms, &sample).unwrap();
            }
            JournalRecord::MarginCallLiquidate { now_ms, req, mount_id, .. } => {
                j.append_margin_call_liquidate(now_ms, &req, mount_id.as_deref(), None).unwrap();
            }
            JournalRecord::GtdExpire { now_ms, coid, engine, .. } => {
                j.append_gtd_expire(now_ms, &coid, engine).unwrap();
            }
            JournalRecord::ScheduleFire { now_ms, mount_id, tag, .. } => {
                j.append_schedule_fire(now_ms, &mount_id, &tag).unwrap();
            }
        }
    }
    j.flush().unwrap();
    drop(j);
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

/// Run the source scenario, producing a journal at `dir` that spans a cadence Snap + the exit
/// Snap. `snapshot_every = 4`, 7 exec-lane messages: 4 bare fills (the 4th trips the cadence
/// Snap = the replay BASE), then a `Submit` + a bare fill + a `Cancel` in the tail (so the tail
/// is non-empty AND carries both an `Ingest::Event` and an `Ingest::Command`).
pub(crate) fn build_source_journal(dir: &Path) {
    let engine = sim_engine();
    // self-advancing clock: message k is dispatched with now_ms == k (distinct per message, so a
    // wrong QueueClock replay of the Submit's created_ms would break the hash)
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(sim_limit(
        "ord1", 1, 1.0, 100.0,
    )))));
    sender.blocking_send(sim_bare_fill("t5", 1.0, 100.0)).unwrap();
    handle.send_command(Command::Order(OrderIntent::Cancel("ord1".into())));
    handle.shutdown_and_join();
}
