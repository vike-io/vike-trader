//! Task 7 gate: the `replay_journal` support CLI, invoked as a real child process — the
//! `fake_jforex_bridge` pattern (`env!("CARGO_BIN_EXE_replay_journal")` + `std::process::Command`).
//!
//! The journal-building scenario is duplicated from `replay_fence.rs` (tests can't share helpers
//! across files without a common module; the Task 7 brief calls duplication fine here). Exercises
//! all three CLI exit paths: 0 (fence verified), 1 (`replay_offline` returns `Err`), 2 (usage
//! error — no argv).

use crate::scratch::Scratch;
use std::path::Path;
use std::process::Command as OsCommand;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, RiskGate, RiskLimits,
};
use vike_model::events::{Event, FillEvent};
use vike_model::{Clock, OrderRequest};

/// Path of the `replay_journal` bin (cargo builds crate bins for integration tests).
const REPLAY_JOURNAL: &str = env!("CARGO_BIN_EXE_replay_journal");

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("cli-{tag}"))
}

/// A bare external fill on the sim venue (empty coid) — the journaled `Ingest::Event` that folds
/// into position/pnl without any client involvement, so replay reproduces it exactly.
fn fill(tid: &'static str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: tid.into(),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    })
}

fn limit(coid: &str, side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: "limit".into(),
        price: Some(px),
        ..Default::default()
    }
}

/// Run the source scenario, producing a journal at `dir` that spans a cadence Snap + the exit
/// Snap (duplicated from `replay_fence.rs::build_source_journal`). `snapshot_every = 4`, 7
/// exec-lane messages: 4 bare fills (the 4th trips the cadence Snap), then a `Submit` + a bare
/// fill + a `Cancel` in the tail.
fn build_source_journal(dir: &Path) {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    // self-advancing clock: message k is dispatched with now_ms == k
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
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)
    handle
        .send_command(Command::Order(OrderIntent::Submit(Box::new(limit("ord1", 1, 1.0, 100.0)))));
    sender.blocking_send(fill("t5", 1.0, 100.0)).unwrap();
    handle.send_command(Command::Order(OrderIntent::Cancel("ord1".into())));
    handle.shutdown_and_join();
}

/// Copy only the `.vjl` segment files (not any temp/lock artifacts) — duplicated from
/// `replay_fence.rs::copy_journal_files`.
fn copy_journal_files(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|s| s.to_str()) == Some("vjl") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
}

/// POSITIVE: a valid journal replays clean through the real bin — exit 0, stdout carries the
/// summary lines (records/snaps/hash + the one engine's venue/symbol/order/position/balance).
#[test]
fn replay_journal_reports_success_on_valid_journal() {
    let dir = unique_dir("ok");
    build_source_journal(&dir);

    let out = OsCommand::new(REPLAY_JOURNAL).arg(&dir).output().expect("spawn replay_journal");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("journal records"), "stdout missing records line: {stdout}");
    assert!(stdout.contains("snaps verified"), "stdout missing snaps line: {stdout}");
    assert!(stdout.contains("final state hash"), "stdout missing hash line: {stdout}");
    assert!(stdout.contains("sim/BTCUSDT"), "stdout missing per-engine line: {stdout}");
}

/// NEGATIVE: a journal whose LAST Snap carries a corrupted hash fails the determinism fence
/// inside `replay_offline` — the CLI must surface that as exit 1 + a `REPLAY FAILED` stderr line
/// (doctoring pattern duplicated from `replay_fence.rs::corrupted_final_hash_fails_the_fence`).
#[test]
fn replay_journal_exits_nonzero_on_corrupted_journal() {
    let good = unique_dir("bad-src");
    build_source_journal(&good);
    let bad = unique_dir("bad-dst");
    copy_journal_files(&good, &bad);

    let recs = CommandJournal::read_all(&good).unwrap();
    let (engines, session, seq, good_hash) = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { engines, coid_session, coid_seq, hash, .. } => {
                Some((engines.clone(), coid_session.clone(), *coid_seq, *hash))
            }
            _ => None,
        })
        .unwrap();
    let wrong_hash = good_hash.wrapping_add(1);
    {
        let mut j = CommandJournal::open(
            &bad,
            JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
        )
        .unwrap();
        j.append_snap(999, &engines, &session, seq, 0, &[], &[], &[], wrong_hash).unwrap();
        j.flush().unwrap();
    }

    let out = OsCommand::new(REPLAY_JOURNAL).arg(&bad).output().expect("spawn replay_journal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got {:?}; stderr: {stderr}",
        out.status
    );
    assert!(out.stdout.is_empty(), "Err path must not print the success summary to stdout");
    assert!(stderr.contains("REPLAY FAILED"), "stderr missing failure line: {stderr}");
    assert!(stderr.contains("HashMismatch"), "stderr should surface the fence variant: {stderr}");
}

/// NEGATIVE (Io path): a nonexistent journal dir also fails — exercises `ReplayError::Io`/`Empty`
/// rather than `HashMismatch`, still surfacing as exit 1 + `REPLAY FAILED`.
#[test]
fn replay_journal_exits_nonzero_on_missing_dir() {
    let missing = unique_dir("missing"); // never created
    let out = OsCommand::new(REPLAY_JOURNAL).arg(&missing).output().expect("spawn replay_journal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1, got {:?}; stderr: {stderr}",
        out.status
    );
    assert!(stderr.contains("REPLAY FAILED"), "stderr missing failure line: {stderr}");
}

/// Usage error: no journal-dir argv → exit 2, nothing on stdout.
#[test]
fn replay_journal_usage_error_without_argv() {
    let out = OsCommand::new(REPLAY_JOURNAL).output().expect("spawn replay_journal");
    assert_eq!(out.status.code(), Some(2), "expected exit 2, got {:?}", out.status);
    assert!(out.stdout.is_empty(), "usage error must not print to stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage"), "stderr missing usage line: {stderr}");
}
