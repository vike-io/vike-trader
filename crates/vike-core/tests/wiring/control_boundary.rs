//! The control boundary end-to-end: a Command::Order in via the handle, a CoreSnapshot query out.
use vike_core::control::{Command, CoreHandle, OrderIntent};
use vike_core::{spawn_core, CoreConfig};
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
/// apply.rs. (Cancels/confirms are also issued by the panic safe-state sweep in mod.rs, so they are
/// NOT part of this invariant — only order CREATION must be single-site, which is what guarantees
/// mint + RiskGate.) A cheap scan over the PRODUCTION prefix (before `#[cfg(test)]`) of each module.
#[test]
fn order_submission_lives_only_in_apply_rs() {
    let files = [
        "mod.rs",
        "broker.rs",
        "handle.rs",
        "strategy_drive.rs",
        "timers.rs",
        "watchdog.rs",
        "publish.rs",
    ]; // every runtime module EXCEPT apply.rs (order-submission single-site) + safe_state_tests.rs (test-only)
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime/");
    for f in files {
        let src = std::fs::read_to_string(format!("{dir}{f}")).unwrap();
        let prod = src.split("#[cfg(test)]").next().unwrap(); // ignore in-module unit tests
        for verb in [".submit_order(", ".submit_order_batch("] {
            assert!(
                !prod.contains(verb),
                "{f} submits orders outside apply.rs (`{verb}`) — submission must be single-site"
            );
        }
    }
}
