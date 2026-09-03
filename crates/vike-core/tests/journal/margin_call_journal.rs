//! Emulator PR-4: the margin-call auto-liquidation is journaled write-ahead so a fired session
//! replays to an IDENTICAL `state_hash`, and a crash mid-liquidation recovers the released order
//! exactly once.
//!
//! Before this, `replay.rs`'s module doc named `sweep_margin_call_engine` as an unjournaled
//! residual: the sweep fires reduce-only MARKET liquidations off a CLOSED BAR against the account's
//! marks + equity, and neither the bar nor the equity is journaled, so a re-fold reproduced the
//! divergence as `ReplayError::HashMismatch`. `margin_call_session_replays_deterministically` is
//! that scenario, green; `an_unjournaled_liquidation_would_have_mismatched` documents WHY it used
//! to fail by fencing the same journal with the liquidation record filtered out. The design mirrors
//! the conditional-FIRE precedent (PR-1) record-for-record: capture the OUTPUT (the released order),
//! never the inputs (marks/equity are market state, deliberately never journaled), and replay
//! re-applies the recorded `Submit` through the SAME `apply_intent` site — the breach is never
//! re-evaluated, so a liquidation can neither double nor vanish.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use vike_core::journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{spawn_core, CoreConfig, JournalConfig};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, BarUpdate, ExecutionEngine, MarginCallConfig, RiskGate, RiskLimits,
};
use vike_model::events::{Event, FillEvent};
use vike_model::{Bar, Clock};

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("mc-{tag}"))
}

/// A bare external LONG fill (empty coid) — the journaled `Ingest::Event` that folds a position
/// with no client involvement, so replay reproduces it exactly. No mark is set (fills carry none
/// into the account); the CLOSED BAR sets the mark the margin sweep values against.
fn long_fill(tid: &'static str, qty: f64, px: f64) -> Event {
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

fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts,
        open,
        high,
        low,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

/// `seed_cash = 40` with a 10-unit long marked at 100 puts the account UNDER maintenance: the LEAN
/// model computes margin_used = 10·100·0.05 = 50 > equity 40 (unrealized 0 at mark == entry), and
/// 50 > 40·1.10 = 44 with remaining -10 ⇒ liquidate the excess (10 / 5 = 2 units). The sweep runs
/// per closed bar (`margin_call: Some(..)`), so the ONE bar below fires exactly one liquidation.
/// Self-advancing clock (message k dispatches at now_ms == k) so a wrong QueueClock replay of the
/// released order's hashed `created_ms` would break the fence. `snapshot_every = 4` ⇒ the 4th warmup
/// fill trips the cadence Snap = the replay BASE, with the position already built and NO mark yet.
fn core_config_margin(dir: &Path) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    CoreConfig {
        seed_cash: 40.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        margin_call: Some(MarginCallConfig::default()),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    }
}

/// 4 warmup long fills of 2.5 @ 100 build a 10-unit @ 100 position (the 4th trips the cadence Snap =
/// BASE), then ONE closed bar at 100 marks the position and the per-bar margin sweep fires a
/// reduce-only MARKET sell of 2 units through `apply_intent` — journaled write-ahead as a
/// `MarginCallLiquidate`.
fn build_liquidated_journal(dir: &Path) {
    let handle = spawn_core(engine(), core_config_margin(dir));
    let sender = handle.event_sender();
    sender.blocking_send(long_fill("t0", 2.5, 100.0)).unwrap();
    sender.blocking_send(long_fill("t1", 2.5, 100.0)).unwrap();
    sender.blocking_send(long_fill("t2", 2.5, 100.0)).unwrap();
    sender.blocking_send(long_fill("t3", 2.5, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)

    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bar: bar(60_000, 100.0, 100.0, 100.0, 100.0), // marks the position -> margin breach
        })
        .unwrap();
    handle.shutdown_and_join();
}

fn records(dir: &Path) -> Vec<JournalRecord> {
    CommandJournal::read_all(dir).unwrap()
}

/// The record shape: the sweep writes exactly one `MarginCallLiquidate` carrying the released
/// reduce-only MARKET with its coid still EMPTY (recorded PRE-mint), and the follow-on
/// `MintedSubmit` `apply_intent` writes ties that release to a NON-empty minted coid.
#[test]
fn the_liquidation_is_journaled_with_its_released_request() {
    let dir = unique_dir("liq-record");
    build_liquidated_journal(&dir);
    let recs = records(&dir);

    let liq: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::MarginCallLiquidate { req, .. } => Some(req),
            _ => None,
        })
        .collect();
    assert_eq!(liq.len(), 1, "the one breaching bar fired exactly one liquidation");
    assert_eq!(liq[0].order_type, "market");
    assert!(liq[0].reduce_only, "a margin liquidation is always reduce-only");
    assert_eq!(liq[0].side, -1, "closing a long -> sell");
    assert!((liq[0].qty - 2.0).abs() < 1e-12, "the LEAN excess: 10 / (100·0.05) = 2 units");
    assert!(
        liq[0].client_order_id.is_empty(),
        "the release is recorded PRE-mint, so replay re-mints the identical coid"
    );

    // the `apply_intent` mint follows write-ahead of the release, tying it to a real coid.
    let minted: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::MintedSubmit { req, .. } => Some(req),
            _ => None,
        })
        .collect();
    assert_eq!(minted.len(), 1, "the liquidation minted exactly one coid");
    assert!(!minted[0].client_order_id.is_empty(), "the minted coid is non-empty");
    assert!(minted[0].reduce_only, "same released reduce-only order, now with its coid");

    let _ = std::fs::remove_dir_all(&dir);
}

/// THE gate: the fence that used to fail. A session whose only tail write is a margin-call
/// liquidation's release re-folds to a bit-identical `state_hash` — coid-identical by construction
/// (the fence hashes the registry, which carries the re-minted order).
#[test]
fn margin_call_session_replays_deterministically() {
    let dir = unique_dir("liq-replay");
    build_liquidated_journal(&dir);
    let out = replay_offline(&dir).expect("a liquidated session must replay deterministically");
    assert_eq!(out.snaps_compared, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The negative control that proves the test above is load-bearing: strip the `MarginCallLiquidate`
/// records out of the journal (exactly the pre-PR-4 shape — the release was never recorded) and the
/// SAME fence rejects it. Rebuilt through a fresh `CommandJournal` so the frames stay valid; only
/// the liquidation record is dropped (its follow-on `MintedSubmit` is kept, mirroring the pre-PR-4
/// world where `apply_intent` still minted but no write-ahead record existed).
#[test]
fn an_unjournaled_liquidation_would_have_mismatched() {
    let src = unique_dir("liq-negative-src");
    build_liquidated_journal(&src);

    let stripped = unique_dir("liq-negative-stripped");
    let mut j = CommandJournal::open(&stripped, JournalFileConfig::default()).unwrap();
    for r in records(&src) {
        match r {
            JournalRecord::MarginCallLiquidate { .. } => {} // the pre-PR-4 hole
            JournalRecord::GtdExpire { .. } => {}
            JournalRecord::ScheduleFire { now_ms, mount_id, tag, .. } => {
                j.append_schedule_fire(now_ms, &mount_id, &tag).unwrap();
            }
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
                hash,
                ..
            } => {
                j.append_snap(
                    now_ms,
                    &engines,
                    &coid_session,
                    coid_seq,
                    arm_seq.unwrap_or(0),
                    &conditionals,
                    &contingencies,
                    &mount_attr,
                    hash,
                )
                .unwrap();
            }
            JournalRecord::MintedSubmit { now_ms, req, .. } => {
                j.append_minted_submit(now_ms, &req).unwrap();
            }
            JournalRecord::StrategySubmit { now_ms, mount_id, intent, .. } => {
                j.append_strategy_submit(now_ms, &mount_id, &intent).unwrap();
            }
            JournalRecord::ConditionalArmed { now_ms, arm_id, resolved, .. } => {
                j.append_conditional_armed(now_ms, &arm_id, &resolved).unwrap();
            }
            JournalRecord::ConditionalFire { now_ms, arm_id, trigger_px, req, .. } => {
                j.append_conditional_fire(now_ms, &arm_id, trigger_px, &req).unwrap();
            }
            JournalRecord::ConditionalDisarmed { now_ms, arm_id, .. } => {
                j.append_conditional_disarmed(now_ms, &arm_id).unwrap();
            }
            JournalRecord::PortfolioSnap { now_ms, sample, .. } => {
                j.append_portfolio_snap(now_ms, &sample).unwrap();
            }
        }
    }
    j.flush().unwrap();
    drop(j);

    assert!(
        replay_offline(&stripped).is_err(),
        "without the liquidation record the release vanishes from the replay — the residual PR-4 closes"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&stripped);
}

/// WRITE-AHEAD proof: truncate the journal right AFTER the `MarginCallLiquidate` record (dropping
/// the `MintedSubmit` + exit Snap that came after) — a crash between the write-ahead record and the
/// order's durable fold. A restart restore re-folds the crash tail and recovers the released order
/// EXACTLY ONCE (not lost, not doubled), with the IDENTICAL coid the full run minted.
#[test]
fn a_restart_mid_liquidation_recovers_the_order_exactly_once() {
    let full = unique_dir("liq-writeahead-full");
    build_liquidated_journal(&full);
    let full_recs = records(&full);

    // The coid the full run minted for the liquidation (its follow-on MintedSubmit) — the restore
    // must reproduce this exact id from the resumed generator.
    let minted_coid = full_recs
        .iter()
        .find_map(|r| match r {
            JournalRecord::MintedSubmit { req, .. } => Some(req.client_order_id.clone()),
            _ => None,
        })
        .expect("the full run minted a coid for the liquidation");
    assert!(!minted_coid.is_empty());

    // Rebuild a TORN journal ending at the first MarginCallLiquidate: keep the warmup fills + the
    // cadence Snap (the restore base) + the write-ahead liquidation record, drop everything after.
    let liq_idx = full_recs
        .iter()
        .position(|r| matches!(r, JournalRecord::MarginCallLiquidate { .. }))
        .expect("the full run wrote a liquidation record");
    let torn = unique_dir("liq-writeahead-torn");
    let mut j = CommandJournal::open(&torn, JournalFileConfig::default()).unwrap();
    for r in &full_recs[..=liq_idx] {
        match r {
            JournalRecord::Cmd { now_ms, msg, .. } => {
                j.append_cmd(*now_ms, msg).unwrap();
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
                hash,
                ..
            } => {
                j.append_snap(
                    *now_ms,
                    engines,
                    coid_session,
                    *coid_seq,
                    arm_seq.unwrap_or(0),
                    conditionals,
                    contingencies,
                    mount_attr,
                    *hash,
                )
                .unwrap();
            }
            JournalRecord::MarginCallLiquidate { now_ms, req, mount_id, .. } => {
                j.append_margin_call_liquidate(*now_ms, req, mount_id.as_deref()).unwrap();
            }
            other => panic!("unexpected pre-liquidation record in the torn prefix: {other:?}"),
        }
    }
    j.flush().unwrap();
    drop(j);

    // Restore: base = the cadence Snap, crash tail = the lone MarginCallLiquidate, re-applied once.
    let state = restore_from_journal(&torn).unwrap().expect("the torn journal has a Snap");
    assert_eq!(state.engines.len(), 1, "v1 single-engine restore");
    let liq_orders: Vec<_> = state.engines[0]
        .registry
        .iter()
        .filter(|(_, mo)| mo.request.reduce_only && mo.request.order_type == "market")
        .collect();
    assert_eq!(
        liq_orders.len(),
        1,
        "the write-ahead liquidation is recovered EXACTLY ONCE — not lost, not doubled"
    );
    let (coid, mo) = liq_orders[0];
    assert_eq!(*coid, minted_coid, "restore re-mints the IDENTICAL coid the full run assigned");
    assert_eq!(mo.request.side, -1);
    assert!((mo.request.qty - 2.0).abs() < 1e-12);

    let _ = std::fs::remove_dir_all(&full);
    let _ = std::fs::remove_dir_all(&torn);
}
