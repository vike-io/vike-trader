//! The deribit RECON socket's lifecycle, over the local WebSocket stand-in in
//! `crate::fake_deribit_ws` — no venue, no credentials, no `#[ignore]`.
//!
//! The defect this pins (measured on the CI box, 2026-08-23): 795 identical
//! `vike_deribit::transport` "order-WS request failed: Trying to work with closed connection"
//! lines, each propagating to one `vike_core::reconcile` "mass-status report fetch failed"
//! tagged `venue=deribit`. Not 795 problems — ONE problem that could not heal.
//! `crates/bridges/deribit/src/transport.rs`'s `DeribitOrderTransport::call` propagates the
//! failure but leaves `self.socket = Some(dead_socket)`, and `DeribitOrderTransport::connect` is
//! called exactly ONCE, from `crates/bridges/deribit/src/recon_client.rs`'s
//! `DeribitReconClient::connect`, at mount time. Every other socket in this crate has a reconnect
//! lifecycle (`crates/bridges/deribit/src/dvol.rs`'s `spawn_deribit_dvol_feed`,
//! `crates/bridges/deribit/src/market_feed.rs`'s `Feeds`, `crates/bridges/deribit/src/exec.rs`'s
//! audit-A3 second socket); the recon socket inherited none, so the FIRST venue-side close
//! disabled deribit reconcile for the life of the daemon while the daemon reported healthy.
//!
//! The ORDER socket's twin behaviour — which may never re-send, only re-query — is
//! `exec_ambiguous_submit.rs`.

use std::sync::Arc;

use vike_deribit::client::DeribitRest;
use vike_deribit::DeribitReconClient;
use vike_exec::recon::ReconClient;

use crate::fake_deribit_ws::{rest_against, FakeDeribit, Session, ORDER_ID};

/// A recon client over the fake, built the way `DeribitReconClient::connect` builds the real one —
/// its OWN dedicated authed transport (never the exec side's). The `Arc<DeribitRest>` is handed
/// back too, because one test needs to reach `DeribitRest::detach`.
fn recon_client_against(fake: &FakeDeribit) -> (DeribitReconClient, Arc<DeribitRest>) {
    let rest = rest_against(fake);
    (DeribitReconClient::new(rest.clone()), rest)
}

/// THE REGRESSION. The venue closes the socket after one served read; the NEXT fetch must re-dial
/// and succeed rather than failing forever. Before the fix this fetch returned
/// `Err("[0] closed mid-request")` and every later one did too.
#[test]
fn a_venue_side_close_is_re_dialed_on_the_next_fetch() {
    let fake = FakeDeribit::spawn(vec![Session::OkThenClose(1), Session::OkForever]);
    let (client, _rest) = recon_client_against(&fake);

    let first = client.fetch_order_status_reports(0).expect("first fetch is served");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].client_order_id.as_deref(), Some("vike-1"));
    assert_eq!(fake.connections(), 1, "one dial so far");

    // ...the venue has closed the socket underneath us by now.
    let second = client
        .fetch_order_status_reports(0)
        .expect("the fetch after a venue-side close re-dials and succeeds");
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].venue_order_id.as_str(), ORDER_ID);
    assert_eq!(fake.connections(), 2, "the recon client re-dialed exactly once");

    // ...and the healed socket keeps serving: the re-dial is not a one-shot.
    assert_eq!(client.fetch_order_status_reports(0).expect("third fetch").len(), 1);
    assert_eq!(fake.connections(), 2, "a healthy socket is never re-dialed");
}

/// Every recon verb rides the same seam, so the heal is not specific to the order-report fetch.
#[test]
fn the_re_dial_covers_every_recon_fetch_not_just_orders() {
    let fake = FakeDeribit::spawn(vec![Session::OkThenClose(1), Session::OkForever]);
    let (client, _rest) = recon_client_against(&fake);

    client.fetch_order_status_reports(0).expect("first fetch is served");
    // `get_positions` answers with the same row shape here; `parse_positions` filters to the
    // mounted instrument, and the assertion that matters is that the CALL survived the dead
    // socket at all — before the fix this was an Err.
    let positions = client
        .fetch_position_status_reports()
        .expect("a position fetch after a venue-side close re-dials and succeeds");
    assert_eq!(positions.len(), 1, "the mounted instrument's row");
    assert_eq!(fake.connections(), 2);
}

/// A venue that is genuinely GONE must surface an error, not spin: ONE re-dial attempt per fetch,
/// no internal loop (the reconcile driver's own ~60s cadence is the retry cadence).
#[test]
fn a_dead_venue_surfaces_an_error_instead_of_spinning() {
    let fake = FakeDeribit::spawn(vec![Session::OkThenClose(1)]);
    let (client, _rest) = recon_client_against(&fake);

    client.fetch_order_status_reports(0).expect("first fetch is served");
    fake.await_listener_closed();

    let started = std::time::Instant::now();
    let err = client
        .fetch_order_status_reports(0)
        .expect_err("a refused re-dial must surface as an error");
    // ...and again: a second pass over a dead venue is still one bounded failure, not a spin.
    let err2 = client.fetch_order_status_reports(0).expect_err("still an error");
    let elapsed = started.elapsed();

    assert!(!err.is_empty() && !err2.is_empty());
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "two failed fetches took {elapsed:?} — a re-dial must be ONE bounded attempt"
    );
    assert_eq!(fake.connections(), 1, "the refused dials never reached an accept");
}

/// PRECISION. A JSON-RPC error object is the venue ANSWERING — the socket is alive and re-dialing
/// it would be pure churn against a zero-headroom credit pool
/// (`crates/bridges/deribit/src/ratelimit.rs`'s `order_ws_gate`), and would hide a real API
/// refusal behind a reconnect.
#[test]
fn a_venue_error_reply_does_not_re_dial() {
    // TWO sessions on purpose: a spurious re-dial would be ACCEPTED and show up in the connection
    // count. With a one-session script it would merely be refused, and this test would pass for
    // the wrong reason — the second session is what makes the count the real assertion.
    let fake = FakeDeribit::spawn(vec![Session::VenueErrorForever, Session::VenueErrorForever]);
    let (client, _rest) = recon_client_against(&fake);

    let err = client.fetch_order_status_reports(0).expect_err("the venue refused");
    assert!(err.contains("not_enough_funds"), "surfaced verbatim: {err}");
    assert!(err.contains("10009"), "the venue's own code survives: {err}");
    assert_eq!(fake.connections(), 1, "a live socket that answered is never re-dialed");

    // ...and the socket is still usable, which is the point.
    let again = client.fetch_order_status_reports(0).expect_err("the venue refuses again");
    assert!(again.contains("not_enough_funds"));
    assert_eq!(fake.connections(), 1);
}

/// A transport holding NO socket at all is exactly what a FAILED re-dial leaves behind —
/// `DeribitOrderTransport::connect` closes the prior socket BEFORE it dials, so a refused dial
/// returns with `socket: None`. If that state did not itself earn a re-dial, one unlucky
/// reconnect would strand the lane for the life of the process: the same never-heals defect,
/// wearing "DeribitOrderTransport.connect() not called" instead of a send failure. `detach()`
/// puts the transport into precisely that state on purpose.
#[test]
fn a_socketless_transport_re_dials_rather_than_stranding_the_lane() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever, Session::OkForever]);
    let (client, rest) = recon_client_against(&fake);

    client.fetch_order_status_reports(0).expect("first fetch is served");
    rest.detach(); // the transport now holds no socket — a failed re-dial's leftovers
    assert_eq!(fake.connections(), 1);

    let after = client
        .fetch_order_status_reports(0)
        .expect("a socketless transport is re-dialed, not reported forever");
    assert_eq!(after.len(), 1);
    assert_eq!(fake.connections(), 2, "the recon client dialled a second connection");
}
