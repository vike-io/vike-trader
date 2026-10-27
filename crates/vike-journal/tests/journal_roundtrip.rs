//! End-to-end gate: a real `spawn_core` session journals a round-trip (buy then sell) through a
//! `TestExecutionClient`; `fills_from_journal` + `reconstruct_trades` recover exactly one closed
//! `Trade` with the expected PnL, and `tearsheet_from_fills` folds the same fills (as held) into a
//! one-trade document — the full live-tearsheet read path. (It was `vike-report`'s test until that
//! crate was dissolved on 2026-10-12; it sits here for `fills_from_journal`.)
//!
//! Mirrors the spawn_core + TestExecutionClient + Command::Order harness in
//! `vike-core/tests/journal/journal_wiring.rs::paper_synthesized_fill_is_journaled` (the paper-synthesized
//! fills are pumped + journaled at teardown).

use vike_analytics::reconstruct_trades;
use vike_analytics::{FillOrder, tearsheet_from_fills};
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::TestExecutionClient;
use vike_exec::{Account, BalanceMode, Command, ExecutionEngine, OrderIntent, RiskGate};
use vike_journal::fills_from_journal;
use vike_model::OrderRequest;
use vike_model::RiskLimits;

/// A resting limit order on the sim venue. The `TestExecutionClient` fills at `request.price`.
fn order(coid: &str, side: i32, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(px),
        ..Default::default()
    }
}

#[test]
fn journal_roundtrip_reconstructs_one_trade() {
    // ⚠ BOUND for the whole test: `TempDir`'s `Drop` removes the tree on the panic path too, where
    // a `remove_dir_all` at the end would not run. `CommandJournal::open` creates the tree AND
    // `posix_fallocate`s a full segment (`crates/vike-journal/src/segment.rs`'s `reserve_blocks`),
    // so a leaked scratch dir is not a stub. The create happens in that callee, which
    // `crates/vike-ops/tests/hygiene/journal_scratch_gate.rs`'s same-file needle scan cannot see
    // (its `the_shapes_the_create_scan_cannot_see` names this file), so the binding is the guard.
    let tmp = tempfile::tempdir().expect("scratch dir");
    // NOT the tempdir root: the journal directory must not exist yet, because `CommandJournal::open`
    // creating it (and its segments) is part of what this test drives.
    let dir = tmp.path().join("journal");

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::default()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(vike_model::TestClock::new(1_000)),
        coid_session: Some(("deadbeef".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.clone(),
            file: vike_journal::JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 1_000,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);

    // Round trip: buy 1 @ 100, then sell 1 @ 110.
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(order("buy1", 1, 100.0)))));
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(order("sell1", -1, 110.0)))));
    handle.shutdown_and_join();

    // Read the fill stream back and reconstruct.
    let fills = fills_from_journal(&dir).expect("read journal");
    // Only the two account-affecting Fills are extracted (lifecycle events filtered out).
    assert_eq!(fills.len(), 2, "expected exactly the buy + sell fills, got {}", fills.len());

    let trades = reconstruct_trades(&fills);
    assert_eq!(trades.len(), 1, "one closed round-trip");
    let t = &trades[0];
    assert_eq!(t.entry_price, 100.0);
    assert_eq!(t.exit_price, 110.0);
    assert_eq!(t.size, 1.0);
    assert_eq!(t.pnl, 10.0, "(110 - 100) * 1");
    assert!(t.is_long);

    // The full journal door yields a finite, one-trade tearsheet. Read by CATALOG ID rather
    // than by field: `LiveTearsheet` is keyed on `vike_analytics::metric_catalog::METRICS` now, so
    // `metric(id)` is the accessor and `None` means the document does not carry that number.
    let sheet = tearsheet_from_fills(fills, FillOrder::AsHeld, 10_000.0, 252.0).0;
    assert_eq!(sheet.metric("n_trades"), Some(1.0));
    assert_eq!(sheet.metric("net_profit"), Some(10.0));
    assert!(sheet.metric("final_equity").expect("a compact scalar always answers").is_finite());
    // The document declares its shape, which is what lets a reader branch before it reads a value.
    assert_eq!(sheet.schema, vike_analytics::TEARSHEET_SCHEMA);

    // …and no `remove_dir_all` here: dropping `tmp` removes the tree, on the panic path too.
}
