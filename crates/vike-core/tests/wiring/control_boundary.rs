//! The control boundary end-to-end: a Command::Order in via the handle, a CoreSnapshot query out.
use vike_core::control::{Command, OrderIntent};
use vike_core::{CoreConfig, CoreHandle, spawn_core};
use vike_exec::testing::TestExecutionClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::OrderRequest;

fn handle() -> CoreHandle {
    let engine = ExecutionEngine::new(
        Account::new(1_000.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    spawn_core(engine, CoreConfig { seed_cash: 1_000.0, ..Default::default() })
}

#[test]
fn command_in_snapshot_query_out() {
    let h = handle();
    let req = OrderRequest {
        client_order_id: "cli-1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        ..Default::default()
    };
    h.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
    // let the fold + paper fill settle, then query THROUGH the accessors
    std::thread::sleep(std::time::Duration::from_millis(50));
    let snap = h.snapshot();
    assert!(snap.order("cli-1").is_some(), "the order is visible via the query accessor");
    assert_eq!(snap.orders_for("BTCUSDT").count(), 1);
    h.shutdown_and_join();
}

/// The "one submission path" invariant: engine ORDER-SUBMISSION verbs are called ONLY inside
/// apply.rs. (Cancels/confirms are also issued by the panic safe-state sweep,
/// `crates/vike-core/src/runtime/watchdog/orders.rs`'s `enter_safe_state`, so they are NOT part of
/// this invariant — only order CREATION must be single-site, which is what guarantees mint +
/// RiskGate.) A cheap scan over the PRODUCTION prefix (before `#[cfg(test)]`) of each module.
///
/// ⚠ This read a HAND-WRITTEN list of seven files until the vike-core layout split, and that list was
/// already short of its own comment ("every runtime module EXCEPT apply.rs"): `deadman.rs`,
/// `link_deadman.rs` and `recon_held.rs` were never in it. Splitting `mod.rs`, `strategy_drive.rs` and
/// `watchdog.rs` into folders would have left every new child in no list at all, and the invariant
/// would have gone on passing while covering a fraction of the code. It WALKS `src/runtime/` now:
/// every production `.rs` file except `apply.rs` and its `apply/` children (the single site) and the
/// white-box tests (`tests/` and any `*_tests.rs`).
#[test]
fn order_submission_lives_only_in_apply_rs() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if name == "apply" || name == "tests" || name.ends_with("_tests") {
                    continue;
                }
                walk(&path, out);
            } else if name.ends_with(".rs") && name != "apply.rs" && !name.ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime");
    let mut files = Vec::new();
    walk(&dir, &mut files);
    // NON-VACUITY: a walk that stopped finding files would pass this test while checking nothing, and
    // a split that adds a folder the walk skips would do the same. 31 production files at the split.
    assert!(
        files.len() >= 25,
        "the runtime walk found only {} production files — it has stopped reaching the tree: {files:?}",
        files.len()
    );
    for must_reach in [
        ["strategy_drive", "broker_drain.rs"],
        ["watchdog", "orders.rs"],
        ["reconcile.rs", ""],
        ["run_loop.rs", ""],
    ] {
        let tail: std::path::PathBuf = must_reach.iter().filter(|s| !s.is_empty()).collect();
        assert!(
            files.iter().any(|f| f.ends_with(&tail)),
            "the runtime walk must reach `{}` (a child the layout split created): {files:?}",
            tail.display()
        );
    }
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        let prod = src.split("#[cfg(test)]").next().unwrap(); // ignore in-module unit tests
        for verb in [".submit_order(", ".submit_order_batch("] {
            assert!(
                !prod.contains(verb),
                "{} submits orders outside apply.rs (`{verb}`) — submission must be single-site",
                f.display()
            );
        }
    }
}
