//! End-to-end CLI gate for the `tearsheet` bin: a real `spawn_core` session journals two
//! round-trips (one win, one loss — so every metric stays finite, incl. `profit_factor`), then the
//! `tearsheet` binary is invoked over that journal directory and its stdout is asserted for both
//! the human table and `--json` output. Also pins the missing-`--journal` usage error (exit 2).

use std::process::Command as OsCommand;

use vike_core::{spawn_core, CoreConfig, JournalConfig};
use vike_exec::testing::TestExecutionClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, RiskGate, RiskLimits,
};
use vike_model::OrderRequest;

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

/// Write a journal with two closed round-trips: +10 (buy 100 → sell 110) then -10 (buy 110 →
/// sell 100). One win + one loss keeps `gross_loss` non-zero so `profit_factor` (and thus `--json`)
/// is finite.
///
/// Returns the journal directory **and the guard that owns it**, and the guard must be BOUND by the
/// caller for as long as the directory is wanted. Returning the path alone would drop the `TempDir`
/// at the end of this function and delete the journal before `env!("CARGO_BIN_EXE_tearsheet")` ever
/// opened it — the tearsheet binary would then be handed an empty directory on every run.
///
/// ⚠ It used to return the path alone — a pid-keyed directory under the OS temp directory, with a
/// `remove_dir_all` at each end of the test. That cleans up only when the test
/// PASSES, and what it left behind on a failure was a `posix_fallocate`d journal segment
/// (`crates/vike-core/src/journal/segment.rs`'s `reserve_blocks`) — the shape that put 211 GB on
/// the CI box's `/tmp`. It was also invisible to `crates/vike-ops/tests/journal_scratch_gate.rs`'s tree
/// rule, which matches create needles in the SAME file while the directory here is created by a
/// callee (`crates/vike-core/src/journal/writer.rs`'s `CommandJournal::open`); that blind spot is
/// declared and driven there now, and this file is one of the two that measured it. `tempfile` was
/// already a declared, unused dev-dependency of this crate for exactly this pair of tests.
fn build_two_trade_journal() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("scratch dir");
    // NOT the tempdir root: `CommandJournal::open` creating the directory and its segments is part
    // of what this harness exercises, so it must not exist yet.
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
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(order("b1", 1, 100.0)))));
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(order("s1", -1, 110.0)))));
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(order("b2", 1, 110.0)))));
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(order("s2", -1, 100.0)))));
    handle.shutdown_and_join();
    (tmp, dir)
}

#[test]
fn tearsheet_bin_prints_table_and_json() {
    // ⚠ `_scratch` is BOUND, not discarded. Binding it to a bare `_` drops the guard immediately
    // and deletes the journal before the binary below is spawned.
    let (_scratch, dir) = build_two_trade_journal();
    let bin = env!("CARGO_BIN_EXE_tearsheet");

    // --- default human table ---
    let out = OsCommand::new(bin)
        .args(["--journal", dir.to_str().unwrap(), "--name", "cli-test"])
        .output()
        .expect("run tearsheet bin");
    assert!(out.status.success(), "bin exited non-zero: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Live Tearsheet: cli-test"), "labeled header missing:\n{stdout}");
    assert!(stdout.contains("trades:              2"), "expected 2 trades:\n{stdout}");
    assert!(stdout.contains("net_profit:"), "net_profit row missing:\n{stdout}");
    assert!(stdout.contains("sharpe:"), "sharpe row missing:\n{stdout}");

    // --- --json ---
    let out = OsCommand::new(bin)
        .args(["--journal", dir.to_str().unwrap(), "--json"])
        .output()
        .expect("run tearsheet bin --json");
    assert!(out.status.success(), "--json exited non-zero: {:?}", out.status);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(json["n_trades"], 2);
    // net profit = +10 - 10 = 0 over the two round-trips
    assert_eq!(json["net_profit"].as_f64().unwrap(), 0.0);
    assert!(json["profit_factor"].as_f64().unwrap().is_finite(), "profit_factor must be finite");

    // …and no `remove_dir_all` here: dropping `_scratch` removes the tree, on the panic path too.
}

#[test]
fn tearsheet_bin_requires_journal_flag() {
    // No --journal and no $VIKE_JOURNAL_DIR → usage error, exit code 2. Clear the env var in case
    // the runner has it set (the test must not depend on ambient config).
    let out = OsCommand::new(env!("CARGO_BIN_EXE_tearsheet"))
        .env_remove("VIKE_JOURNAL_DIR")
        .output()
        .expect("run tearsheet bin with no args");
    assert_eq!(out.status.code(), Some(2), "missing --journal must exit 2");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("--journal"), "usage message must name --journal:\n{stderr}");
}
