//! Dukascopy reconcile seam end-to-end: the fake bridge's `netting` mode replays the Java
//! sidecar's authoritative `position` push stream, and the [`DukascopyReconClient`] derived from
//! the running exec client must report EXACTLY the venue's net position through the whole netting
//! lifecycle — open, blend, partial net-close (the re-anchor case), and full close (a kept flat
//! row). No Java, no network; the fake bridge speaks the real stdio protocol.
//!
//! This is the position-only slice (see `src/recon_client.rs`): `fetch_order_status_reports` /
//! `fetch_fill_reports` are empty by design, so those are asserted empty here too.

use std::time::{Duration, Instant};

use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient, DukascopyReconClient};
use vike_exec::recon::ReconClient;
use vike_exec::{event_channel, ExecutionClient, Ingest};
use vike_model::{OrderRequest, PositionStatusReport};

const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
    }
}

/// A market order the netting fake fills at `px` (it reads `price` as the fill price).
fn order(coid: &str, side: i32, qty: f64, px: f64, ts: i64) -> OrderRequest {
    OrderRequest {
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: "market".into(),
        price: Some(px),
        trigger_price: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

/// Drain `n` events off the ingest lane (keeps the reader thread's bounded `blocking_send` moving
/// so it reaches the trailing `position` lines), failing loudly after 10s each.
fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, n: usize) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    for _ in 0..n {
        let ingest = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
            .expect("timed out waiting for event")
            .expect("ingest channel closed");
        assert!(matches!(ingest, Ingest::Event(_)), "expected Ingest::Event, got {ingest:?}");
    }
}

/// Poll the reconcile client until `symbol`'s reported net qty matches `want_qty` (the snapshot is
/// filled by the reader thread ASYNchronously from the `position` line that trails each fill — a
/// bounded poll is the honest race-free wait, mirroring the netting test's channel timeouts).
fn wait_for_qty(recon: &DukascopyReconClient, symbol: &str, want_qty: f64) -> PositionStatusReport {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reports = recon.fetch_position_status_reports().expect("position fetch");
        if let Some(r) = reports.iter().find(|r| r.symbol == symbol) {
            if (r.qty - want_qty).abs() < 1e-6 {
                return r.clone();
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {symbol} qty={want_qty}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn recon_client_tracks_venue_net_position_through_netting() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["netting".into()],
        &config(),
        events,
    )
    .expect("ready");
    let recon = client.recon_client();

    // The two empty-by-design fetches: honest `Ok(vec![])`, never an error (see recon_client.rs).
    assert_eq!(recon.fetch_order_status_reports(0), Ok(Vec::new()));
    assert_eq!(recon.fetch_fill_reports(0), Ok(Vec::new()));

    // 1) Short 1000 @ 100, then short 1000 @ 110 → venue net short 2000, signed-weighted avg 105.
    client.submit(&order("c1", -1, 1000.0, 100.0, 1));
    drain(&mut rx, 4); // Submitted, Accepted, Fill, OrderFilled
    client.submit(&order("c2", -1, 1000.0, 110.0, 2));
    drain(&mut rx, 4);
    let r = wait_for_qty(&recon, "EURUSD", -2000.0);
    assert_eq!(r.venue, "dukascopy");
    assert_eq!(r.avg_px, 105.0);

    // 2) Buy 1000 @ 108 nets against order A. Venue truth: remainder short 1000 at B's own basis
    //    110 (the exact netting attribution the position line carries + the re-anchor pair fold).
    client.submit(&order("c3", 1, 1000.0, 108.0, 3));
    drain(&mut rx, 6); // …+ 2 re-anchor legs
    let r = wait_for_qty(&recon, "EURUSD", -1000.0);
    assert_eq!(r.avg_px, 110.0);

    // 3) Buy 1000 @ 112 fully closes the remainder → the venue reports a FLAT row (size 0), which
    //    the seam KEEPS (diff needs it to detect a stale local position the venue has closed).
    client.submit(&order("c4", 1, 1000.0, 112.0, 4));
    drain(&mut rx, 4);
    let r = wait_for_qty(&recon, "EURUSD", 0.0);
    assert_eq!(r.avg_px, 0.0);

    client.detach();
}
