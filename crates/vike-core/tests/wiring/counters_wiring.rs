//! Audit co9 wiring gate: the core MIRRORS its key health counters into the opt-in mmap file at
//! publish cadence, an external reader (`vike_core::counters::read`) sees them, and the mirror is
//! OFF by default (no file opened, publish byte-identical). Mirrors the journal wiring test's shape:
//! the counters file defaults off, so this is the only core test that opens one.

use crate::scratch::Scratch;

use vike_core::{CoreConfig, spawn_core};
use vike_exec::testing::TestExecutionClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::events::{Event, OrderCanceled};

fn engine() -> ExecutionEngine<TestExecutionClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    )
}

/// An `OrderCanceled` for a coid that was never registered — the engine folds it, drops it (unknown
/// coid), and increments `dropped_unknown_coid` (audit C2). A deterministic, public-API way to move
/// a counter the mirror must then surface.
fn ghost_cancel(coid: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.into(),
        reason: String::new().into(),
        ts: 0,
    })
}

#[test]
fn counters_file_mirrors_runtime_counters() {
    // The mmap file lives inside a scratch directory this test OWNS: `dir`'s drop removes it and
    // the `.bin` in it, on the unwind path too — so the assertions below need no cleanup arm. ⚠
    // Hold `dir` past `shutdown_and_join`: the core writes into this file until then. The old
    // spelling (`env::temp_dir().join(format!("vmc-wire-{pid}.bin"))` + a `remove_file` pre-clean)
    // leaked one file per run and could still collide with the other the CI box user's file at the same
    // PID — see `vike_core::counters`'s `tmp_path` for the measurement.
    let dir = Scratch::created("counters-wire");
    let path = dir.join("vmc-wire.bin");

    // struct-update (not field-reassign) — the codebase clippy gate forbids reassigning fields on a
    // `Default::default()` value (see journal_wiring.rs / runtime_smoke.rs::test_config).
    let cfg = CoreConfig { counters_path: Some(path.clone()), ..CoreConfig::default() };
    let handle = spawn_core(engine(), cfg);
    let sender = handle.event_sender();
    for i in 0..3 {
        sender.blocking_send(ghost_cancel(&format!("ghost{i}"))).unwrap();
    }
    // Lossless shutdown folds every queued event, then the teardown publish mirrors the final
    // counters into the mmap file.
    handle.shutdown_and_join();

    let report = vike_core::counters::read(&path).expect("read mmap counters file");
    assert_eq!(report.version, 2, "version header round-trips (v2 appended dropped_nonfinite)");
    assert_eq!(report.counter_count, 8, "all eight named counters declared");
    assert!(report.clean, "no writer contention post-shutdown ⇒ a clean seqlock read");
    assert_eq!(report.seq % 2, 0, "a completed write leaves seq even");
    assert_eq!(
        report.counters.dropped_unknown_coid, 3,
        "the three unknown-coid cancels are mirrored"
    );
    // The counters we did not touch stay zero — the mirror is not inventing values.
    assert_eq!(report.counters.rejected_commands, 0);
    assert_eq!(report.counters.stranded_terminal_drops, 0);
    assert_eq!(
        report.counters.dropped_nonfinite, 0,
        "nothing here sent a non-finite number, so the hostile-venue counter stays zero"
    );
}

#[test]
fn counters_disabled_by_default_opens_no_file() {
    // The default config leaves the mirror off — nothing is opened, the publish path is byte-identical.
    assert!(CoreConfig::default().counters_path.is_none(), "mirror is opt-in (default None)");

    // A path inside a scratch directory this test owns, so the final `!path.exists()` asserts on a
    // location NOTHING has ever written to — a stronger guarantee than the `remove_file` pre-clean
    // this replaces, which only cleared a leftover from a previous run at the same PID (and could
    // not clear one owned by the other the CI box user at all). `dir` is held to the end of the test.
    let dir = Scratch::created("counters-off");
    let path = dir.join("vmc-off.bin");

    let handle = spawn_core(engine(), CoreConfig::default()); // counters_path stays None
    let sender = handle.event_sender();
    sender.blocking_send(ghost_cancel("ghost")).unwrap();
    handle.shutdown_and_join();

    assert!(!path.exists(), "no counters file is created when counters_path is None");
}
