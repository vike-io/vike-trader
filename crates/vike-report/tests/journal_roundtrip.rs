//! End-to-end gate: a real `spawn_core` session journals a round-trip (buy then sell) through a
//! `TestExecutionClient`; `fills_from_journal` + `reconstruct_trades` recover exactly one closed
//! `Trade` with the expected PnL — the full live-tearsheet read path.
//!
//! Mirrors the spawn_core + TestExecutionClient + Command::Order harness in
//! `vike-core/tests/journal/journal_wiring.rs::paper_synthesized_fill_is_journaled` (the paper-synthesized
//! fills are pumped + journaled at teardown).

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::TestExecutionClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, RiskGate, RiskLimits,
};
use vike_model::OrderRequest;
use vike_report::{LiveTearsheet, fills_from_journal, reconstruct_trades};

/// A resting limit order on the sim venue. The `TestExecutionClient` fills at `request.price`.
fn order(coid: &str, side: i32, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(px),
        ..Default::default()
    }
}

#[test]
fn journal_roundtrip_reconstructs_one_trade() {
    // ⚠ BOUND for the whole test, and the binding is the point. This used to be a pid-keyed
    // directory under the OS temp directory, with a `remove_dir_all` at each end of the test,
    // which cleans up only when the test PASSES — and what it leaves behind on a failure is not a
    // stub directory: `CommandJournal::open` creates the tree AND `posix_fallocate`s a full segment
    // (`crates/vike-core/src/journal/segment.rs`'s `reserve_blocks`), which is the exact shape that
    // put 27,471 directories and 211 GB on the CI box's `/tmp`. `TempDir`'s `Drop` runs on the unwinding
    // path too. `tempfile` was already a declared dev-dependency of this crate for this test — see
    // `crates/vike-report/Cargo.toml`, whose comment names this round-trip — and was unused.
    //
    // ⚠ It was also INVISIBLE to `crates/vike-ops/tests/journal_scratch_gate.rs`'s tree rule, and
    // that is the more interesting half: the rule asks whether the file CREATES anything at the
    // path, matching five needles IN THE SAME FILE, and nothing here creates the directory — the
    // callee does, inside `crates/vike-core/src/journal/writer.rs`'s `CommandJournal::open`. So this
    // file matched no needle and was classified as one that "cannot leak". The blind spot is
    // declared and driven there now (`the_shapes_the_create_scan_cannot_see`); this is one of the
    // two files that measured it.
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
            file: vike_core::journal::JournalFileConfig {
                segment_bytes: 1024 * 1024,
                flush_every: 8,
            },
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

    // The full from_journal path yields a finite, one-trade tearsheet.
    let sheet = LiveTearsheet::from_journal(&dir, 10_000.0, 252.0).expect("tearsheet");
    assert_eq!(sheet.n_trades, 1);
    assert_eq!(sheet.net_profit, 10.0);
    assert!(sheet.final_equity.is_finite());

    // …and no `remove_dir_all` here: dropping `tmp` removes the tree, on the panic path too.
}
