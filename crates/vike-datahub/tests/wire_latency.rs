//! A request/response round trip on loopback costs microseconds, not a delayed ACK.
//!
//! `crates/vike-node-proto/tests/frame_write.rs` pins the MECHANISM (one write per frame); this
//! pins the PROPERTY at the real server, so a second frame writer, a wrapper that splits a write,
//! or a socket option elsewhere cannot bring the stall back unseen. The budget is half of Linux's
//! 40 ms delayed-ACK floor per round trip: a stalling codec pays at least one per frame (MEASURED
//! 82 ms per round trip), a healthy one measured 0.028 ms, so the margin either side is two orders
//! of magnitude and box load cannot move a healthy run across it.
//!
//! ⚠ The clients here are RAW `TcpStream`s, deliberately not `DatahubClient`: a raw socket keeps
//! Nagle on, as a client that never went through `vike_node_proto::frame::configure_node_stream`
//! does, so the first test still sees a codec that splits a frame — the server's own `TCP_NODELAY`
//! cannot hide a stall on the CLIENT'S writes.

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use vike_data::{HistStore, MemHistStore};
use vike_datahub_client::proto::{Request, Response, read_frame, write_frame};

const ROUND_TRIPS: u32 = 200;
const BUDGET: Duration = Duration::from_millis(20 * ROUND_TRIPS as u64);

fn spawn() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = vike_datahub::server::serve(listener, store);
    });
    addr
}

fn pong(s: &mut TcpStream) {
    match read_frame::<_, Response>(s).expect("read") {
        Response::Pong => {}
        other => panic!("a Ping must be answered with Pong, got {other:?}"),
    }
}

fn ping(s: &mut TcpStream) {
    write_frame(s, &Request::Ping).expect("write");
    pong(s);
}

#[test]
fn two_hundred_round_trips_cost_no_delayed_ack() {
    let mut s = TcpStream::connect(spawn()).expect("connect");
    ping(&mut s); // connection setup is not what this measures
    let started = Instant::now();
    for _ in 0..ROUND_TRIPS {
        ping(&mut s);
    }
    let took = started.elapsed();
    assert!(
        took < BUDGET,
        "{ROUND_TRIPS} loopback round trips took {took:?} (budget {BUDGET:?}): a frame is waiting \
         for the peer's delayed ACK — see vike_node_proto::frame::write_frame's doc"
    );
}

/// Two requests PIPELINED in one write, so the server writes two answers back to back: the second
/// is written while the first is still unacknowledged, which is the one shape one write per frame
/// does not cover — Nagle holds a small segment while an earlier one is in flight, and the client,
/// blocked reading, ACKs the first only when its delayed-ACK timer fires. The server's
/// `TCP_NODELAY` (`vike_node_proto::frame::configure_node_stream`, armed on every accepted socket)
/// is what sends the second at once; it is the same shape as a push stream's consecutive frames and
/// as the last segment of a frame larger than one segment.
///
/// Same budget, same argument as the round-trip test above, per pipelined pair.
#[test]
fn back_to_back_answers_cost_no_delayed_ack() {
    let mut s = TcpStream::connect(spawn()).expect("connect");
    ping(&mut s); // connection setup is not what this measures
    let mut two = Vec::new();
    write_frame(&mut two, &Request::Ping).expect("frame the first Ping");
    write_frame(&mut two, &Request::Ping).expect("frame the second Ping");
    let started = Instant::now();
    for _ in 0..ROUND_TRIPS {
        s.write_all(&two).expect("write both requests in ONE write");
        pong(&mut s);
        pong(&mut s);
    }
    let took = started.elapsed();
    assert!(
        took < BUDGET,
        "{ROUND_TRIPS} pipelined pairs took {took:?} (budget {BUDGET:?}): the server's second \
         answer is waiting for the client's delayed ACK of its first — the accepted socket is \
         missing TCP_NODELAY (vike_node_proto::frame::configure_node_stream)"
    );
}
