//! Emulator PR-4: the margin-call auto-liquidation is journaled write-ahead so a fired session
//! replays to an IDENTICAL `state_hash`, and a crash mid-liquidation recovers the released order
//! exactly once.
//!
//! Before this, `replay.rs`'s module doc named `sweep_margin_call_engine` as an unjournaled
//! residual: the sweep fires reduce-only MARKET liquidations off a CLOSED BAR against the account's
//! marks + equity, and neither the bar nor the equity is journaled, so a re-fold reproduced the
//! divergence as `JournalReplayError::HashMismatch`. `margin_call_session_replays_deterministically` is
//! that scenario, green; `an_unjournaled_liquidation_would_have_mismatched` documents WHY it used
//! to fail by fencing the same journal with the liquidation record filtered out. The design mirrors
//! the conditional-FIRE precedent (PR-1) record-for-record: capture the OUTPUT (the released order),
//! never the inputs (marks/equity are market state, deliberately never journaled), and replay
//! re-applies the recorded `Submit` through the SAME `apply_intent` site — the breach is never
//! re-evaluated, so a liquidation can neither double nor vanish.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{CoreConfig, JournalConfig, LiveBroker, PriceOnlyConfig, spawn_core};
use vike_execution::BarUpdate;
use vike_journal::{Journal, JournalFileConfig, JournalRecord, JournalSnapshotParts};
use vike_model::{Clock, MarginCallConfig, Strategy};

use crate::kit::engines::sim_engine;
use crate::kit::events::{ohlc_bar, sim_bare_fill, sim_quote};
use crate::kit::journal::{records, rewrite_journal};
use vike_core::testkit::mount_of;

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("mc-{tag}"))
}

/// `seed_cash = 40` with a 10-unit long marked at 100 puts the account UNDER maintenance: the LEAN
/// model computes margin_used = 10·100·0.05 = 50 > equity 40 (unrealized 0 at mark == entry), and
/// 50 > 40·1.10 = 44 with remaining -10 ⇒ liquidate the excess (10 / 5 = 2 units). The sweep runs
/// per closed bar (`margin_call: Some(..)`), so the ONE bar below fires exactly one liquidation.
/// Self-advancing clock (message k dispatches at now_ms == k) so a wrong JournalReplayQueueClock replay of the
/// released order's hashed `created_ms` would break the fence. `snapshot_every = 4` ⇒ the 4th warmup
/// fill trips the cadence Snapshot = the replay BASE, with the position already built and NO mark yet.
fn core_config_margin(dir: &Path) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    CoreConfig {
        seed_cash: 40.0,
        clock,
        order_client_id_session: Some(("cafef00d".into(), 0)),
        margin_call: Some(MarginCallConfig::default()),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    }
}

/// 4 warmup long fills of 2.5 @ 100 build a 10-unit @ 100 position (the 4th trips the cadence Snapshot =
/// BASE), then ONE closed bar at 100 marks the position and the per-bar margin sweep fires a
/// reduce-only MARKET sell of 2 units through `apply_intent` — journaled write-ahead as a
/// `MarginCallLiquidate`.
fn build_liquidated_journal(dir: &Path) {
    let handle = spawn_core(sim_engine(), core_config_margin(dir));
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 2.5, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 2.5, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 2.5, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 2.5, 100.0)).unwrap(); // 4th record -> cadence Snapshot (BASE)

    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bar: ohlc_bar(60_000, 100.0, 100.0, 100.0, 100.0), // marks the position -> margin breach
        })
        .unwrap();
    handle.shutdown_and_join();
}

/// The record shape: the sweep writes exactly one `MarginCallLiquidate` carrying the released
/// reduce-only MARKET with its order client id still EMPTY (recorded PRE-mint), and the follow-on
/// `MintedSubmit` `apply_intent` writes ties that release to a NON-empty minted order client id.
#[test]
fn the_liquidation_is_journaled_with_its_released_request() {
    let dir = unique_dir("liq-record");
    build_liquidated_journal(&dir);
    let recs = records(&dir);

    let liquidation: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::MarginCallLiquidate { request: req, .. } => Some(req),
            _ => None,
        })
        .collect();
    assert_eq!(liquidation.len(), 1, "the one breaching bar fired exactly one liquidation");
    assert_eq!(liquidation[0].order_type, vike_model::OrderType::Market);
    assert!(liquidation[0].reduce_only, "a margin liquidation is always reduce-only");
    assert_eq!(liquidation[0].side, -1, "closing a long -> sell");
    assert!((liquidation[0].qty - 2.0).abs() < 1e-12, "the LEAN excess: 10 / (100·0.05) = 2 units");
    assert!(
        liquidation[0].order_client_id.is_empty(),
        "the release is recorded PRE-mint, so replay re-mints the identical order client id"
    );

    // the `apply_intent` mint follows write-ahead of the release, tying it to a real order client id.
    let minted: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::MintedSubmit { request: req, .. } => Some(req),
            _ => None,
        })
        .collect();
    assert_eq!(minted.len(), 1, "the liquidation minted exactly one order client id");
    assert!(!minted[0].order_client_id.is_empty(), "the minted order client id is non-empty");
    assert!(minted[0].reduce_only, "same released reduce-only order, now with its order client id");

    let _ = std::fs::remove_dir_all(&dir);
}

/// THE gate: the fence that used to fail. A session whose only tail write is a margin-call
/// liquidation's release re-folds to a bit-identical `state_hash` — order-client-id-identical by construction
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
/// SAME fence rejects it. Rebuilt through a fresh `Journal` so the frames stay valid; only
/// the liquidation record is dropped (its follow-on `MintedSubmit` is kept, mirroring the pre-PR-4
/// world where `apply_intent` still minted but no write-ahead record existed).
#[test]
fn an_unjournaled_liquidation_would_have_mismatched() {
    let src = unique_dir("liq-negative-src");
    build_liquidated_journal(&src);

    let stripped = unique_dir("liq-negative-stripped");
    rewrite_journal(
        &stripped,
        records(&src)
            .into_iter()
            // the pre-PR-4 hole (no liquidation record); `GtdExpire` was never re-emitted either
            .filter(|r| {
                !matches!(
                    r,
                    JournalRecord::MarginCallLiquidate { .. } | JournalRecord::GtdExpire { .. }
                )
            })
            .collect(),
    );

    assert!(
        replay_offline(&stripped).is_err(),
        "without the liquidation record the release vanishes from the replay — the residual PR-4 closes"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&stripped);
}

/// WRITE-AHEAD proof: truncate the journal right AFTER the `MarginCallLiquidate` record (dropping
/// the `MintedSubmit` + exit Snapshot that came after) — a crash between the write-ahead record and the
/// order's durable fold. A restart restore re-folds the crash tail and recovers the released order
/// EXACTLY ONCE (not lost, not doubled), with the IDENTICAL order client id the full run minted.
#[test]
fn a_restart_mid_liquidation_recovers_the_order_exactly_once() {
    let full = unique_dir("liq-writeahead-full");
    build_liquidated_journal(&full);
    let full_recs = records(&full);

    // The order client id the full run minted for the liquidation (its follow-on MintedSubmit) — the restore
    // must reproduce this exact id from the resumed generator.
    let minted_order_client_id = full_recs
        .iter()
        .find_map(|r| match r {
            JournalRecord::MintedSubmit { request: req, .. } => Some(req.order_client_id.clone()),
            _ => None,
        })
        .expect("the full run minted an order client id for the liquidation");
    assert!(!minted_order_client_id.is_empty());

    // Rebuild a TORN journal ending at the first MarginCallLiquidate: keep the warmup fills + the
    // cadence Snapshot (the restore base) + the write-ahead liquidation record, drop everything after.
    let liquidation_index = full_recs
        .iter()
        .position(|r| matches!(r, JournalRecord::MarginCallLiquidate { .. }))
        .expect("the full run wrote a liquidation record");
    let torn = unique_dir("liq-writeahead-torn");
    let mut j = Journal::open(&torn, JournalFileConfig::default()).unwrap();
    for r in &full_recs[..=liquidation_index] {
        match r {
            JournalRecord::Command { now_ms, message: msg, .. } => {
                j.append_command(*now_ms, msg).unwrap();
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
                hash,
                ..
            } => {
                j.append_snap(
                    *now_ms,
                    engines,
                    order_client_id_session,
                    *order_client_id_sequence,
                    arm_sequence_value.unwrap_or(0),
                    JournalSnapshotParts {
                        order_emulation_arms: conditionals,
                        contingencies,
                        mount_attribution: mount_attribute,
                        gateway_cursors,
                        held_releases,
                        recent_order_client_ids,
                        emulated,
                        closed_trades,
                    },
                    *hash,
                )
                .unwrap();
            }
            JournalRecord::MarginCallLiquidate { now_ms, request: req, mount_id, .. } => {
                j.append_margin_call_liquidate(*now_ms, req, mount_id.as_deref(), None).unwrap();
            }
            other => panic!("unexpected pre-liquidation record in the torn prefix: {other:?}"),
        }
    }
    j.flush().unwrap();
    drop(j);

    // Restore: base = the cadence Snapshot, crash tail = the lone MarginCallLiquidate, re-applied once.
    let state = restore_from_journal(&torn).unwrap().expect("the torn journal has a Snapshot");
    assert_eq!(state.engines.len(), 1, "v1 single-engine restore");
    let liquidation_orders: Vec<_> = state.engines[0]
        .registry
        .iter()
        .filter(|(_, mo)| {
            mo.request.reduce_only && mo.request.order_type == vike_model::OrderType::Market
        })
        .collect();
    assert_eq!(
        liquidation_orders.len(),
        1,
        "the write-ahead liquidation is recovered EXACTLY ONCE — not lost, not doubled"
    );
    let (order_client_id, mo) = liquidation_orders[0];
    assert_eq!(
        *order_client_id, minted_order_client_id,
        "restore re-mints the IDENTICAL order client id the full run assigned"
    );
    assert_eq!(mo.request.side, -1);
    assert!((mo.request.qty - 2.0).abs() < 1e-12);

    let _ = std::fs::remove_dir_all(&full);
    let _ = std::fs::remove_dir_all(&torn);
}

/// A mount that trades nothing: the 4-hour series it listens to never closes in the test below.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

/// Owner ruling 2026-10-25: the margin call runs on a CLOCK for a MOUNTED engine too, whatever its bar
/// interval. The same session as `build_liquidated_journal`, with the closed bar replaced by the clock: the
/// sim engine carries a mount on a 4-HOUR series (no bar ever arrives) and the node has no price-only venue
/// (`venues` empty: the config is only the clock). The injected clock is stepped by the test, so the sweep
/// is due when the test says and the waker thread (a real-time tick of half the interval) never lands.
/// The release is journaled write-ahead exactly like the bar-driven one, and replays to the identical
/// `state_hash`: the breach is never re-decided, so the liquidation can neither double nor vanish.
#[test]
fn a_clock_swept_liquidation_of_a_mounted_engine_replays_deterministically() {
    let dir = unique_dir("liq-clock");
    let now = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = {
        let now = Arc::clone(&now);
        Box::new(move || now.load(Ordering::Relaxed))
    };
    let handle = spawn_core(
        sim_engine(),
        CoreConfig {
            seed_cash: 40.0,
            clock,
            order_client_id_session: Some(("cafef00d".into(), 0)),
            margin_call: Some(MarginCallConfig::default()),
            strategy: Some(mount_of("sim", "BTCUSDT", "4h", Box::new(Idle))),
            price_only: Some(PriceOnlyConfig {
                venues: std::collections::BTreeSet::new(),
                sweep_interval: std::time::Duration::from_secs(3_600),
                max_price_age: std::time::Duration::from_secs(60),
                price_source: std::collections::BTreeMap::new(),
            }),
            journal: Some(JournalConfig {
                dir: dir.to_path_buf(),
                file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
                snapshot_every: 4,
            }),
            ..CoreConfig::default()
        },
    );
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(sim_bare_fill(tid, 2.5, 100.0)).unwrap(); // the 4th trips the cadence Snapshot
    }

    // The position is folded before the clock moves: a sweep that found no quote would re-arm an hour on.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !handle
        .snapshot()
        .portfolio
        .venues
        .iter()
        .flat_map(|v| v.positions.iter())
        .any(|p| p.size == 10.0)
    {
        assert!(std::time::Instant::now() < deadline, "the four fills never folded");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // An hour and a minute on: the sweep armed at an hour is due, and the quote below is fresh to it.
    now.store(3_660_000, Ordering::Relaxed);
    handle.tick_sender().quote(sim_quote(100.0, 3_660_000)).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while handle.snapshot().orders.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the clock never liquidated the mounted engine"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    handle.shutdown_and_join();

    let liquidation: Vec<_> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::MarginCallLiquidate { request: req, .. } => Some(req),
            _ => None,
        })
        .collect();
    assert_eq!(liquidation.len(), 1, "the clock fired exactly one liquidation");
    assert!(
        liquidation[0].reduce_only && liquidation[0].side == -1,
        "a reduce-only close of the long"
    );
    assert!((liquidation[0].qty - 2.0).abs() < 1e-12, "the LEAN excess: 10 / (100·0.05) = 2 units");
    assert!(
        liquidation[0].order_client_id.is_empty(),
        "recorded PRE-mint, so replay re-mints the same order client id"
    );

    let out =
        replay_offline(&dir).expect("a clock-liquidated session must replay deterministically");
    assert_eq!(out.snaps_compared, 1);
    let _ = std::fs::remove_dir_all(&dir);
}
