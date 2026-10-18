//! The operator's RECONCILE verb (`trade recon ls` / `trade recon confirm`) over the real wire.
//!
//! A held reconcile divergence used to be unresolvable from outside the process
//! (`Command::ConfirmRecon` had no production constructor). Here a REAL core holds two divergences
//! under `quarantine` - a venue position the book lacks, then a local position the venue stopped
//! reporting - and a `Scope::Write` peer resolves them with `WireCommand::ReconConfirm`, reading
//! the ids off the pushed `WireSnapshot::recon`. What is pinned end to end: the held rows reach the
//! observe wire; a `Preview` of the verb decides nothing; a confirm Acks, is audited with its
//! rationale and folds into the book; an unknown id is a note, not an error.

use super::*;

use vike_core::{CoreConfig, spawn_core};
use vike_exec::recon::{BalanceTol, ReconMode, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, Command, ExecutionEngine, ReconcileReports, RiskGate};
use vike_model::RiskLimits;
use vike_model::events::PositionSide;

/// A venue position row on the engine's venue.
fn position(symbol: &str, qty: f64) -> vike_model::PositionStatusReport {
    vike_model::PositionStatusReport {
        venue: "binance".into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        qty,
        avg_px: 100.0,
        ts: 1_000,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    }
}

/// One pass under `quarantine` (the policy a live box runs): everything is held, nothing folds.
fn quarantine_pass(positions: Vec<vike_model::PositionStatusReport>) -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "binance".into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions,
        policy: ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: BalanceTol::default(),
        route_key: None,
        admitted_symbols: None,
    }))
}

fn confirm(id: u64, reason: Option<&str>) -> Request {
    Request::Command { cmd: WireCommand::ReconConfirm { id }, reason: reason.map(str::to_string) }
}

#[test]
fn recon_confirm_over_the_wire_adopts_a_venue_position_then_drops_a_phantom() {
    test_init();
    let engine = ExecutionEngine::new(
        Account::new(1_000.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() });
    let addr = serve_core(&handle, HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    let mut ctl = control_stream(addr, CONTROL_KEY);
    let book_holds = |symbol: &str, size: f64| {
        observer.snapshot().positions.iter().any(|p| p.symbol == symbol && p.size == size)
    };

    // (1) THE VENUE HOLDS +0.5 BTC THE BOOK DOES NOT: held, and listed on the observe wire.
    handle.send_command(quarantine_pass(vec![position("BTCUSDT", 0.5)]));
    assert!(
        wait_until(5, || !observer.snapshot().recon.is_empty()),
        "the held divergence never reached the observe wire"
    );
    let row = observer.snapshot().recon[0].clone();
    assert_eq!(row.kind, "PositionOnlyExternal");
    assert!(!book_holds("BTCUSDT", 0.5), "held, not folded");

    // (2) A PREVIEW decides the edge's verdict and does nothing: the row is still held after it.
    let (accepted, reason) =
        preview_command(addr, CONTROL_KEY, &WireCommand::ReconConfirm { id: row.id })
            .expect("preview handshake");
    assert_eq!((accepted, reason), (true, None), "the edge lowers the verb");
    thread::sleep(Duration::from_millis(200));
    assert_eq!(observer.snapshot().recon.len(), 1, "a preview must not confirm anything");

    // (3) THE CONFIRM: Ack, audited with the rationale, the book adopts the venue's position.
    write_frame(&mut ctl, &confirm(row.id, Some("venue shows it"))).expect("confirm");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "", "account-wide-verb convention"),
        other => panic!("expected Ack for recon_confirm, got {other:?}"),
    }
    assert!(wait_until(5, || book_holds("BTCUSDT", 0.5)), "the confirm never adopted the position");
    assert!(wait_until(5, || observer.snapshot().recon.is_empty()), "the confirmed row stays");
    let entry = audit_entry_with_kind("recon_confirm").expect("the accepted confirm was audited");
    assert_eq!(entry.coid, "");
    assert_eq!(entry.reason.as_deref(), Some("venue shows it"));

    // (4) THE VENUE CLOSED IT: a later pass reports another symbol only, so the book's BTC is a
    // phantom, held as an OrphanLocalPosition. Confirming it DROPS the local position.
    handle.send_command(quarantine_pass(vec![position("ETHUSDT", 3.0)]));
    assert!(
        wait_until(5, || observer.snapshot().recon.iter().any(|r| r.kind == "OrphanLocalPosition")),
        "the phantom was never held"
    );
    let orphan = observer
        .snapshot()
        .recon
        .iter()
        .find(|r| r.kind == "OrphanLocalPosition")
        .cloned()
        .expect("held");
    write_frame(&mut ctl, &confirm(orphan.id, None)).expect("confirm the phantom");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { .. } => {}
        other => panic!("expected Ack for the phantom's confirm, got {other:?}"),
    }
    assert!(
        wait_until(5, || !observer
            .snapshot()
            .positions
            .iter()
            .any(|p| p.symbol == "BTCUSDT" && p.size != 0.0)),
        "the phantom position is still in the book: {:?}",
        observer.snapshot().positions
    );

    // (5) AN UNKNOWN ID is an Ack and a note on the node, never an error and never a fold.
    write_frame(&mut ctl, &confirm(999_999, None)).expect("confirm an unknown id");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { .. } => {}
        other => panic!("an unknown id is edge-Ack'd, got {other:?}"),
    }
    assert!(
        wait_until(5, || observer
            .snapshot()
            .recent_events
            .iter()
            .any(|l| l.contains("RECON confirm: unknown alert id 999999"))),
        "the core's unknown-id note never surfaced: {:?}",
        observer.snapshot().recent_events
    );

    handle.shutdown_and_join();
}
