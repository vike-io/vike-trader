//! `CtraderReconClient`'s idle-revive, end-to-end against the in-process fake cTrader server
//! (plaintext, no TLS) — the MUST-FIX-BEFORE-MOUNT gate the client's module doc used to carry.
//!
//! The dedicated reconcile connection sends no heartbeats and sits idle between passes, and
//! cTrader closes idle sockets; at the default 60s `ReconDriver` cadence it was therefore dead
//! before the second pass. `FakeCtrader::start_close_after_each_reconcile` reproduces exactly that
//! server behavior — every connection serves the handshake plus ONE `RECONCILE_RES` and then
//! closes — so the second pass is forced to survive a closed socket rather than a simulated timer.
//! No live venue, and no real idle wait: the revive age is injected
//! (`connect_with_idle_revive`) instead of slept out.

mod common;

use std::time::Duration;

use vike_ctrader::conn::ConnConfig;
use vike_ctrader::recon_client::CtraderReconClient;
use vike_exec::recon::ReconClient;
use vike_model::events::PositionSide;

use common::{FakeCtrader, RECONCILE_PENDING_COID, RECONCILE_PENDING_ORDER_ID};

/// The fix: with revive armed, a SECOND reconcile pass over a connection the server already closed
/// succeeds, because the client re-runs the handshake before the request.
#[test]
fn second_pass_succeeds_after_the_server_closes_the_idle_connection() {
    let server = FakeCtrader::start_close_after_each_reconcile();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    // ZERO = "always consider it idle", so the revive path runs deterministically on every call
    // without waiting out a real IDLE_REVIVE_AFTER.
    let client = CtraderReconClient::connect_with_idle_revive(&cfg, "EURUSD", Duration::ZERO)
        .expect("recon client connects");

    // Pass 1 — over the connection `connect` opened. The server answers, then closes.
    client.fetch_order_status_reports(0).expect("first pass");
    // Pass 2 — that socket is now dead. Without the revive this is the failure the module doc
    // predicted; with it, the client re-handshakes onto a fresh connection and completes.
    client.fetch_order_status_reports(0).expect("second pass must survive the idle close");
    // Pass 3 — the revive is not a one-shot.
    client.fetch_order_status_reports(0).expect("third pass");

    // Each pass ran a FULL handshake of its own: three passes => three ApplicationAuth requests.
    // This is what distinguishes a genuine re-handshake from a reply that merely happened to work.
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 3, Duration::from_secs(10));
}

/// The regression pin: the same scenario with revive effectively disabled (an age no test run can
/// reach) still fails on the second pass. Without this, the test above could pass for reasons
/// unrelated to the fix — e.g. if the fake server stopped closing connections.
#[test]
fn without_revive_the_second_pass_fails_on_the_closed_socket() {
    let server = FakeCtrader::start_close_after_each_reconcile();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let client =
        CtraderReconClient::connect_with_idle_revive(&cfg, "EURUSD", Duration::from_secs(86_400))
            .expect("recon client connects");

    client.fetch_order_status_reports(0).expect("first pass");
    assert!(
        client.fetch_order_status_reports(0).is_err(),
        "second pass must fail without a revive — otherwise this test proves nothing",
    );
}

/// End-to-end over the socket, a scripted `RECONCILE_RES` maps to the EXPECTED report CONTENT (not
/// just the right shape/symbol the other tests here assert): the fake answers `RECONCILE_REQ` with
/// ONE pending LIMIT order (`RECONCILE_PENDING_COID` -> `RECONCILE_PENDING_ORDER_ID`, EURUSD, BUY,
/// 100 centi-units, `ORDER_STATUS_ACCEPTED`) and a FLAT position list. This proves the full
/// `fetch -> reconcile -> parse_orders/parse_positions` path the `ReconClient` drives — the pure
/// parsers are unit-tested separately in `recon_client_parse.rs`. `Duration::ZERO` (revive on every
/// call) is used only so the orders fetch and the positions fetch each get a fresh socket from the
/// close-after-each-reconcile fake, exactly like `reports_stay_correct_across_a_revive` below.
#[test]
fn scripted_reconcile_res_maps_to_expected_reports() {
    let server = FakeCtrader::start_close_after_each_reconcile();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let client = CtraderReconClient::connect_with_idle_revive(&cfg, "EURUSD", Duration::ZERO)
        .expect("recon client connects");

    let orders = client.fetch_order_status_reports(0).expect("order reports");
    assert_eq!(orders.len(), 1, "the fake scripts exactly one pending order");
    let o = &orders[0];
    assert_eq!(o.venue, "ctrader");
    assert_eq!(o.symbol, "EURUSD");
    assert_eq!(o.venue_order_id.as_str(), RECONCILE_PENDING_ORDER_ID.to_string());
    assert_eq!(o.client_order_id.as_deref(), Some(RECONCILE_PENDING_COID));
    assert_eq!(o.side, 1, "BUY -> +1");
    assert_eq!(o.order_type, "limit");
    assert_eq!(o.qty, 1.0, "100 centi-units -> 1.00 units");
    assert_eq!(o.filled_qty, 0.0);
    assert_eq!(o.status, "ACCEPTED", "ORDER_STATUS_ACCEPTED with no fill progress -> ACCEPTED");

    // The scripted position list is empty, so the client synthesizes the flat zero row cTrader omits.
    let positions = client.fetch_position_status_reports().expect("position reports");
    assert_eq!(positions.len(), 1, "empty position list -> one synthesized flat row");
    let p = &positions[0];
    assert_eq!(p.venue, "ctrader");
    assert_eq!(p.symbol, "EURUSD");
    assert_eq!(p.qty, 0.0);
    assert_eq!(p.avg_px, 0.0);
    assert_eq!(p.position_side, PositionSide::Both, "a synthesized flat row is side-agnostic");
}

/// A revive re-resolves the handshake-derived fields, not just the socket: the reports coming back
/// after one still carry the correct symbol, so a fetch can never mix a fresh connection with a
/// previous handshake's symbol map.
#[test]
fn reports_stay_correct_across_a_revive() {
    let server = FakeCtrader::start_close_after_each_reconcile();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let client = CtraderReconClient::connect_with_idle_revive(&cfg, "EURUSD", Duration::ZERO)
        .expect("recon client connects");

    let first = client.fetch_position_status_reports().expect("first pass");
    let second = client.fetch_position_status_reports().expect("second pass across a revive");
    assert_eq!(first.len(), second.len(), "revive must not change the report shape");
    for r in second {
        assert_eq!(r.symbol, "EURUSD");
        assert_eq!(r.venue, "ctrader");
    }
}
