//! A fee adjustment (`Event::FeeAdjustment`, decision 0238) is journaled as an ordinary `Cmd` record and replays
//! deterministically, with the fee ledger riding the `Snap`; a journal that never carried one replays exactly as it did.
//!
//! The statement is an event tag inside the existing record, so the segment is stamped as any event segment is (no
//! lazy format step), and `replay_offline` re-pumps the tail through a real core and fences the reproduced
//! `state_hash` against the source's last `Snap`. Three shapes, as `unadmit_symbol_replay.rs` has: the corrections
//! AFTER the replay base (the replay re-applies them, the repeat among them books nothing), BEFORE it (the base `Snap`
//! carries what was booked, and a later total books only the difference: without the ledger in the `Snap` the replay
//! would book the whole total again and the fence would fail), and none at all (the journal of every box until now).

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
use vike_journal::{CommandJournal, JournalFileConfig};
use vike_model::events::{Event, FeeAdjustmentEvent};
use vike_model::{Clock, RiskLimits};

use crate::kit::events::sim_bare_fill;

fn adjustment(tid: &str, extra: f64) -> Event {
    Event::FeeAdjustment(FeeAdjustmentEvent {
        trade_id: vike_model::events::TradeId::new(tid).expect("non-empty"),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        last_px: 100.0,
        extra_commission: extra,
        commission_asset: "USD".into(),
        ts: 2,
    })
}

/// `snapshot_every: 4`: the 4th journaled record trips the cadence `Snap` that becomes the replay base.
fn build(dir: &Path, shape: Shape) {
    let limits = RiskLimits { tick_size: Some(0.1), lot_size: Some(0.001), ..RiskLimits::new() };
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
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
    let send = |ev: Event| sender.blocking_send(ev).unwrap();
    match shape {
        Shape::Tail => {
            for tid in ["t0", "t1", "t2", "t3"] {
                send(sim_bare_fill(tid, 1.0, 100.0)); // record 4: the base Snap
            }
            send(adjustment("t0", 1.25));
            send(adjustment("t0", 1.25)); // the repeat: books nothing
            send(adjustment("t0", 1.40)); // a changed total: books the difference
        }
        Shape::Base => {
            send(sim_bare_fill("t0", 1.0, 100.0));
            send(adjustment("t0", 1.25));
            send(sim_bare_fill("t1", 1.0, 100.0));
            send(sim_bare_fill("t2", 1.0, 100.0)); // record 4: the base Snap, holding the ledger
            send(adjustment("t0", 1.40)); // after the base: only the 0.15 difference
        }
        Shape::None => {
            for tid in ["t0", "t1", "t2", "t3", "t4"] {
                send(sim_bare_fill(tid, 1.0, 100.0));
            }
        }
    }
    handle.shutdown_and_join();
}

#[derive(Clone, Copy)]
enum Shape {
    Tail,
    Base,
    None,
}

fn replayed(tag: &str, shape: Shape) -> vike_core::replay::ReplayOutcome {
    let dir = Scratch::reserved(tag);
    build(&dir, shape);
    // The fence is the proof: the replay's `state_hash` equals the one the live session recorded in its last Snap.
    let out = vike_core::replay::replay_offline(&dir).expect("the fence holds");
    assert!(out.records > 0 && out.snaps_compared >= 1);
    // No format step: the segment keeps the version any event segment carries (the journal's base version).
    assert_eq!(CommandJournal::latest_segment_version(&dir).unwrap(), Some(18), "{tag}");
    out
}

#[test]
fn corrections_after_the_replay_base_are_re_applied_and_fenced() {
    let out = replayed("fee-adjustment-replay-tail", Shape::Tail);
    let account = &out.engines[0].account;
    assert!(
        (account.fees_paid - 1.40).abs() < 1e-12,
        "booked once, then the difference: {}",
        account.fees_paid
    );
    assert!((account.balance + 1.40).abs() < 1e-12, "{}", account.balance);
    assert_eq!(account.fee_adjusted.len(), 1, "one corrected execution");
}

#[test]
fn a_correction_before_the_replay_base_rides_the_snapshot() {
    let out = replayed("fee-adjustment-replay-base", Shape::Base);
    let account = &out.engines[0].account;
    assert!(
        (account.fees_paid - 1.40).abs() < 1e-12,
        "the later total booked only the difference over what the Snap held: {}",
        account.fees_paid
    );
    assert_eq!(account.fee_adjusted.len(), 1);
}

/// A journal that never carried a correction replays as before, and its canonical snapshot has no such key.
#[test]
fn a_journal_without_a_correction_replays_unchanged() {
    let out = replayed("fee-adjustment-replay-none", Shape::None);
    let account = &out.engines[0].account;
    assert!(account.fee_adjusted.is_empty());
    assert_eq!(account.fees_paid, 0.0);
    let json = serde_json::to_string(&out.engines[0]).unwrap();
    assert!(!json.contains("fee_adjusted"), "{json}");
}
