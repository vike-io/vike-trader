//! Headless-daemon LIFECYCLE test over the PAPER exchange (headless two-layer plan, Layer 2, PR-9).
//!
//! Drives the SAME lifecycle `main.rs` runs, but as library calls (a bin crate has no importable
//! surface, so the integration test exercises the deps `main.rs` composes): build the node over the
//! paper `ExecutionClient` via [`vike_run::build_paper_maker_core`], drive an operator
//! [`vike_exec::Command`] in through the EXACT path the stdio control uses
//! ([`vike_core::CoreHandle::send_command`], the command round-tripped through JSON to prove the
//! newline-JSON decode is faithful), assert the published snapshot reflects it, then assert the
//! bounded teardown ([`vike_ops::shutdown::run_with_deadline`] wrapping `shutdown_and_join`)
//! completes `Graceful` inside the deadline.
//!
//! No network, no creds, no geo access, no `polymarket` feature — the paper mount fills nothing
//! without a feed (a resting limit far from any market simply rests), which is exactly what makes the
//! snapshot assertion deterministic: the ONLY order in the book is the operator's.

use std::time::{Duration, Instant};

use vike_exec::{Command, OrderIntent};
use vike_model::OrderRequest;
use vike_ops::shutdown::{ShutdownOutcome, run_with_deadline};
use vike_run::{MakerMountConfig, build_paper_maker_core};

const TOKEN: &str = "HEADLESS_LIFECYCLE_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the vike-run offline mount test).
const RESOLUTION_TS: i64 = 3_000_000_000;

/// Poll `cond` up to `secs`, returning whether it became true — the core folds on its own thread and
/// publishes coalesced snapshots, so this just waits for the publish.
fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn paper_daemon_builds_drives_a_stdio_command_and_shuts_down_bounded() {
    vike_log::test_init();

    // 1. Build the node over the PAPER ExecutionClient — the SAME mount main.rs builds.
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);

    // The snapshot is readable immediately (node built, core live).
    let seq0 = mount.handle.snapshot().seq;

    // 2. Drive an operator Command through the EXACT path the stdio control uses: a newline of JSON →
    //    vike_exec::Command → CoreHandle::send_command. Round-trip through JSON so the assertion also
    //    proves the stdio decode is faithful. A resting limit far from any market never fills without
    //    a feed, so it stays working and deterministic.
    let coid = "op-1";
    let req = OrderRequest {
        client_order_id: coid.to_string(),
        venue: cfg.venue.clone(),
        symbol: cfg.token_id.clone(),
        side: 1,
        qty: cfg.qty,
        order_type: "limit".to_string(),
        price: Some(0.40),
        ..Default::default()
    };
    let json = serde_json::to_string(&Command::Order(OrderIntent::Submit(Box::new(req))))
        .expect("serialize the operator command");
    let cmd: Command = serde_json::from_str(&json).expect("stdio newline-JSON decode");
    mount.handle.send_command(cmd);

    // 3. The published snapshot must reflect the operator order (wait for the coalesced publish).
    assert!(
        wait_until(5, || mount.handle.snapshot().order(coid).is_some()),
        "the snapshot never reflected the stdio-driven operator order"
    );
    let snap = mount.handle.snapshot();
    let ov = snap.order(coid).expect("the operator order is present in the snapshot");
    assert_eq!(ov.symbol, cfg.token_id, "the order routed to the mount symbol");
    assert_eq!(ov.side, 1, "the order side survived the newline-JSON round-trip");
    assert!(snap.seq >= seq0, "the snapshot seq advanced after the command");
    assert!(snap.fault.is_none(), "the core must not have faulted: {:?}", snap.fault);
    assert_eq!(snap.rejected_commands, 0, "the command lane accepted the operator command");

    // 4. The bounded teardown — the EXACT main.rs shutdown primitive — completes Graceful within the
    //    deadline. No venue feeds on a paper mount, so `tasks` is empty and `shutdown_and_join` is
    //    the sequential tail.
    let deadline = Duration::from_millis(5_000);
    let join_core = move || mount.handle.shutdown_and_join();
    let outcome = run_with_deadline(Vec::new(), Box::new(join_core), deadline);
    assert_eq!(outcome, ShutdownOutcome::Graceful, "the bounded shutdown must complete gracefully");
}

/// A second, smaller invariant: even with NO command driven in, the node builds, the snapshot is
/// readable, and the bounded shutdown is Graceful within the deadline — the minimum lifecycle the
/// daemon guarantees headless.
#[test]
fn paper_daemon_bare_build_and_bounded_shutdown_is_graceful() {
    vike_log::test_init();

    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);

    let snap = mount.handle.snapshot();
    assert!(snap.fault.is_none(), "a freshly built node must not be faulted");
    assert!(snap.orders.is_empty(), "no orders before any command");

    let deadline = Duration::from_millis(5_000);
    let join_core = move || mount.handle.shutdown_and_join();
    let outcome = run_with_deadline(Vec::new(), Box::new(join_core), deadline);
    assert_eq!(outcome, ShutdownOutcome::Graceful);
}
