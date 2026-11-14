//! **What a quiet socket adds to an order**: how long a command waits in the actor's queue before it is on the wire, at
//! the timings production runs at (`ConnectionConfig::new`, never `for_test`). The exec socket carries no spots, so between an
//! order's last frame and the next heartbeat nothing wakes its read. `crates/bridges/ctrader/CLAUDE.md`'s "A quiet exec
//! socket" says what it was and what it is.

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use vike_ctrader::execution::ExecutionClientCtrader;
use vike_ctrader::test_support::{self, FakeCtrader, NoopSink, market_buy, wait_open_filled};
use vike_ctrader::transport::connection::{
    ConnectionConfig, connect_and_auth_execution, fetch_symbols,
};
use vike_execution::{ExecutionClient, Ingest, event_channel};

/// Well above the poll plus a loaded lane's scheduling, well below the whole second these tests exist to keep out.
const WORST_ALLOWED: Duration = Duration::from_millis(250);

/// How long after an order's last frame the next one is submitted, so each lands somewhere else in the read's wait.
const QUIET_FOR_MS: [u64; 6] = [0, 2, 5, 60, 250, 700];

/// The production constructor, spoken plainly to the fake: only the TLS differs from what the CI box runs.
fn production_config(server: &FakeCtrader) -> ConnectionConfig {
    let addr = server.addr();
    let mut config =
        ConnectionConfig::new(addr.ip().to_string(), addr.port(), "cid", "secret", "token");
    config.no_tls = true;
    config
}

fn execution_on(
    server: &FakeCtrader,
) -> (ExecutionClientCtrader, tokio::sync::mpsc::Receiver<Ingest>) {
    let (events, ingest) = event_channel(256);
    let handle =
        connect_and_auth_execution(production_config(server), Arc::new(NoopSink), events.clone())
            .expect("auth ok");
    (
        test_support::execution_with_no_halt(handle, events, &std::env::temp_dir(), module_path!()),
        ingest,
    )
}

/// Submit one order per [`QUIET_FOR_MS`] and time each from `submit` to the fake holding its frame.
fn submit_to_wire(
    server: &FakeCtrader,
    exec: &mut ExecutionClientCtrader,
    ingest: &mut tokio::sync::mpsc::Receiver<Ingest>,
) -> Vec<Duration> {
    let on_the_wire = server.count_seen("NEW_ORDER_REQ");
    let mut waits = Vec::new();
    for (i, quiet_ms) in QUIET_FOR_MS.into_iter().enumerate() {
        let order_client_id = format!("c-quiet-{i}");
        thread::sleep(Duration::from_millis(quiet_ms));
        let submitted = Instant::now();
        exec.submit(&market_buy(&order_client_id));
        while server.count_seen("NEW_ORDER_REQ") <= on_the_wire + i {
            assert!(
                submitted.elapsed() < Duration::from_secs(5),
                "{order_client_id} never reached the wire"
            );
            thread::sleep(Duration::from_micros(100));
        }
        waits.push(submitted.elapsed());
        wait_open_filled(ingest, &order_client_id);
    }
    waits
}

/// **The order path.** The worst of the samples is the one submitted just after a read began.
#[test]
fn an_order_on_a_silent_socket_reaches_the_wire_without_waiting_out_a_read() {
    let server = FakeCtrader::start_scripted();
    let (mut exec, mut ingest) = execution_on(&server);

    let waits = submit_to_wire(&server, &mut exec, &mut ingest);

    println!("submit to wire, quiet for {QUIET_FOR_MS:?} ms: {waits:?}");
    let worst = waits.iter().max().copied().unwrap_or_default();
    assert!(worst < WORST_ALLOWED, "an order waited {worst:?} on a silent socket: {waits:?}");
}

/// The socket a reconnect opens has to read as shortly as the first one: the handshake leaves it on the long read.
#[test]
fn an_order_after_a_reconnect_reaches_the_wire_without_waiting_out_a_read() {
    let server = FakeCtrader::start_reconnect_after_handshake();
    let (mut exec, mut ingest) = execution_on(&server);
    server.wait_until_count_at_least("RECONCILE_REQ", 1, Duration::from_secs(10));
    // The replay's own round trip is answered; give the actor its way back to the loop.
    thread::sleep(Duration::from_millis(200));

    let waits = submit_to_wire(&server, &mut exec, &mut ingest);

    println!("submit to wire after a reconnect, quiet for {QUIET_FOR_MS:?} ms: {waits:?}");
    let worst = waits.iter().max().copied().unwrap_or_default();
    assert!(worst < WORST_ALLOWED, "an order waited {worst:?} on a new socket: {waits:?}");
}

/// ⚠ **A TLS handshake gets the long read.** rustls drives it inside the first write, and a read that times out there is an
/// error that ends the attempt, not a turn of a loop: at the actor's poll every handshake over a real network would fail.
/// So a venue that takes 300 ms to answer the `ClientHello` (here: to close) is still being waited for after 300 ms.
#[test]
fn a_tls_handshake_waits_out_a_slow_venue_and_is_not_cut_short_by_the_poll() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let slow = Duration::from_millis(300);
    let venue = thread::spawn(move || {
        let (socket, _peer) = listener.accept().expect("accept");
        thread::sleep(slow);
        drop(socket);
    });

    let started = Instant::now();
    let ended = fetch_symbols(ConnectionConfig::new("127.0.0.1", port, "cid", "secret", "token"));

    assert!(ended.is_err(), "the venue never answered");
    assert!(started.elapsed() >= slow, "gave up after {:?}", started.elapsed());
    venue.join().expect("venue");
}
