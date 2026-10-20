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
    // Looked up by id, not by verb: the second test below also confirms (other quantities, so
    // other ids), and the capture is process-wide.
    let entry = audit_entry_for_recon_id(row.id).expect("the accepted confirm was audited");
    assert_eq!(entry.coid, "");
    assert_eq!(entry.reason.as_deref(), Some("venue shows it"));
    // …and the line names WHAT was confirmed: the id, the book, the kind and the description.
    assert_eq!(entry.recon_id, Some(row.id));
    assert_eq!(entry.recon_held, Some(true), "the published snapshot held the id");
    assert_eq!(entry.recon_account.as_deref(), Some(row.account.as_str()));
    assert_eq!(entry.recon_kind.as_deref(), Some("PositionOnlyExternal"));
    assert!(
        entry.recon_detail.as_deref().is_some_and(|d| d.contains("BTCUSDT")),
        "the description names the instrument: {:?}",
        entry.recon_detail
    );

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
    // The audit line still names the id that was asked for, and says the snapshot held no such row.
    let unknown = audit_entry_for_recon_id(999_999).expect("the unknown-id confirm was audited");
    assert_eq!(unknown.recon_held, Some(false));
    assert_eq!(unknown.recon_kind.as_deref(), Some(""));

    handle.shutdown_and_join();
}

/// One core under `quarantine`, served on the wire: `(handle, observe addr)`.
fn start_core() -> (vike_core::CoreHandle, SocketAddr) {
    let engine = ExecutionEngine::new(
        Account::new(1_000.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() });
    let addr = serve_core(&handle, HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);
    (handle, addr)
}

/// The two ways a confirm used to reach the wrong divergence, over the real wire.
///
/// (1) A held divergence a LATER pass no longer raises leaves the observe wire: the pass that saw
/// the venue's BTC position again cleared the phantom, so a confirm of its id folds nothing.
/// (2) The id is the divergence's identity, not a counter: a second core (the daemon after a
/// restart) that meets the divergences in the OPPOSITE order gives each the id it had before, so a
/// retried `confirm <id>` cannot land on another divergence.
#[test]
fn a_stale_row_leaves_the_wire_and_a_confirm_id_survives_a_restart() {
    test_init();
    let (handle, addr) = start_core();
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    let mut ctl = control_stream(addr, CONTROL_KEY);
    let row_of = |symbol: &str| {
        observer.snapshot().recon.iter().find(|r| r.detail.contains(symbol)).cloned()
    };

    // (1) the book holds BTC 0.25 (the venue's external position, confirmed) … (other quantities
    // than the first test, so other ids: the audit capture is process-wide)
    handle.send_command(quarantine_pass(vec![position("BTCUSDT", 0.25)]));
    assert!(wait_until(5, || row_of("BTCUSDT").is_some()), "the external position was never held");
    let adopt = row_of("BTCUSDT").expect("held");
    write_frame(&mut ctl, &confirm(adopt.id, None)).expect("confirm");
    let _ack: Response = read_frame(&mut ctl).expect("ack");
    assert!(wait_until(5, || observer.snapshot().positions.iter().any(|p| p.size == 0.25)));
    // … a pass that omits it holds the phantom …
    handle.send_command(quarantine_pass(vec![position("ETHUSDT", 2.0)]));
    assert!(
        wait_until(5, || observer.snapshot().recon.iter().any(|r| r.kind == "OrphanLocalPosition")),
        "the phantom was never held"
    );
    let phantom = observer
        .snapshot()
        .recon
        .iter()
        .find(|r| r.kind == "OrphanLocalPosition")
        .cloned()
        .expect("held");
    // … and the next pass reports BTC correctly: the phantom leaves the wire.
    handle.send_command(quarantine_pass(vec![position("BTCUSDT", 0.25), position("ETHUSDT", 2.0)]));
    assert!(
        wait_until(5, || observer.snapshot().recon.iter().all(|r| r.id != phantom.id)),
        "the cleared phantom is still listed: {:?}",
        observer.snapshot().recon
    );
    write_frame(&mut ctl, &confirm(phantom.id, None)).expect("confirm the cleared row");
    let _ack: Response = read_frame(&mut ctl).expect("edge ack");
    assert!(
        wait_until(5, || observer
            .snapshot()
            .recent_events
            .iter()
            .any(|l| l.contains(&format!("unknown alert id {}", phantom.id)))),
        "the stale confirm was not refused: {:?}",
        observer.snapshot().recent_events
    );
    assert!(
        observer.snapshot().positions.iter().any(|p| p.symbol == "BTCUSDT" && p.size == 0.25),
        "the stale confirm must not close the position the venue holds"
    );
    let eth_before = row_of("ETHUSDT").expect("the ETH row is still held");
    handle.shutdown_and_join();

    // (2) the restarted daemon meets ETH first, then BTC - the old counter gave ETH BTC's number.
    let (restarted, addr) = start_core();
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    restarted.send_command(quarantine_pass(vec![position("ETHUSDT", 2.0)]));
    assert!(wait_until(5, || !observer.snapshot().recon.is_empty()));
    restarted
        .send_command(quarantine_pass(vec![position("ETHUSDT", 2.0), position("BTCUSDT", 0.25)]));
    assert!(wait_until(5, || observer.snapshot().recon.len() == 2));
    let btc_after = observer
        .snapshot()
        .recon
        .iter()
        .find(|r| r.detail.contains("BTCUSDT"))
        .cloned()
        .expect("BTC held again");
    let eth_after = observer
        .snapshot()
        .recon
        .iter()
        .find(|r| r.detail.contains("ETHUSDT"))
        .cloned()
        .expect("ETH held");
    assert_eq!(btc_after.id, adopt.id, "the same divergence has the same id after a restart");
    assert_eq!(eth_after.id, eth_before.id);
    restarted.shutdown_and_join();
}
