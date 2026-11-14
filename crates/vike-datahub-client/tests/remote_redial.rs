//! A held connection the datahub closed: [`HistoryStoreDatahub`] dials again, ONCE, and repeats the read.
//! Everything else that fails stays a failure: a refusal from the datahub, a fresh connection that
//! dies, a second failure, a connection lost in the middle of a paged read.
//!
//! A scripted peer rather than the real `serve`: the real one closes an idle connection only after
//! `vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT_MIN` (two minutes), and only a script can say
//! which connection dies at which frame. Each scripted connection records what it received, so "the
//! read was repeated" and "nothing was repeated" are both a count.

use std::assert_matches;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_datahub_client::HistoryStoreDatahub;
use vike_datahub_client::proto::{FEATURE_SCAN_LIMIT, Request, Response, read_frame, write_frame};
use vike_datahub_client::test_support::accept_keyless;
use vike_marketdata::Bar;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
/// `properties_as_of` for this symbol is refused by the peer: a `Response::Error` on a healthy socket.
const REFUSED: &str = "REFUSED";

/// One scripted connection: what its `Welcome` advertises and how many frames it answers before it
/// hangs up.
struct Conn {
    features: Vec<String>,
    answers: usize,
}

/// A connection that advertises nothing, so a range read is ONE frame.
fn plain(answers: usize) -> Conn {
    Conn { features: Vec::new(), answers }
}

/// A connection that advertises the page cap, so a range read is pages up to an empty one.
fn paged(answers: usize) -> Conn {
    Conn { features: vec![FEATURE_SCAN_LIMIT.to_string()], answers }
}

fn bar(ts: i64) -> Bar {
    Bar {
        timestamp: ts,
        open: 100.0,
        high: 100.0,
        low: 100.0,
        close: 100.0,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn answer(request: &Request) -> Response {
    match request {
        Request::PropertiesAsOf { symbol, .. } if symbol == REFUSED => {
            Response::Error("the store refused this read".to_string())
        }
        Request::PropertiesAsOf { .. } => Response::Properties(None),
        // Every row is on the first page, so the page after it is the empty one that ends a read.
        Request::LoadBars { start: None, .. } => Response::Bars(vec![bar(1_000), bar(2_000)]),
        Request::LoadBars { .. } => Response::Bars(Vec::new()),
        other => panic!("the script expected no {other:?}"),
    }
}

/// Serve `script` in order, one connection each, and hand back the address and what each connection
/// received. Past the last one nothing listens, so a dial too many is a connect failure
/// ([`HistoryStoreError::Io`]) rather than a read that works.
fn spawn_script(script: Vec<Conn>) -> (String, Arc<Mutex<Vec<Vec<Request>>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port").to_string();
    let received: Arc<Mutex<Vec<Vec<Request>>>> = Arc::default();
    let seen = Arc::clone(&received);
    thread::spawn(move || {
        for conn in script {
            let (mut stream, _) = listener.accept().expect("accept the client");
            // Before the `Welcome`, so a client that has connected is already counted.
            let at = {
                let mut seen = seen.lock().unwrap();
                seen.push(Vec::new());
                seen.len() - 1
            };
            assert!(accept_keyless(&mut stream, conn.features), "the client opens with a Hello");
            for _ in 0..conn.answers {
                let Ok(request) = read_frame::<_, Request>(&mut stream) else { break };
                let reply = answer(&request);
                seen.lock().unwrap()[at].push(request);
                write_frame(&mut stream, &reply).expect("answer the frame");
            }
        }
    });
    (addr, received)
}

fn frames_per_connection(received: &Mutex<Vec<Vec<Request>>>) -> Vec<usize> {
    received.lock().unwrap().iter().map(Vec::len).collect()
}

fn read_properties(store: &HistoryStoreDatahub) -> Result<(), HistoryStoreError> {
    store.properties_as_of(VENUE, SYMBOL, 1).map(|_| ())
}

#[test]
fn a_connection_closed_while_it_was_held_is_dialled_again_and_the_read_repeated() {
    let (addr, received) = spawn_script(vec![plain(1), plain(1)]);
    let store = HistoryStoreDatahub::new(addr);

    read_properties(&store).expect("the first read dials");
    // The first connection has answered and hung up; the store still holds it.
    read_properties(&store).expect("the held connection is dead: this read dials again");

    assert_eq!(frames_per_connection(&received), [1, 1]);
}

#[test]
fn a_paged_read_on_a_closed_connection_is_repeated_whole() {
    let (addr, received) = spawn_script(vec![paged(2), paged(2)]);
    let store = HistoryStoreDatahub::new(addr);
    let read = || store.load_bars(VENUE, SYMBOL, "1m", TimestampRange::all());

    let stamps = |bars: Vec<Bar>| bars.iter().map(|b| b.timestamp).collect::<Vec<_>>();
    let first = stamps(read().expect("the first read dials"));
    let second = stamps(read().expect("the held connection is dead: this read dials again"));

    assert_eq!(first, [1_000, 2_000]);
    assert_eq!(second, first, "the same rows, none lost and none twice");
    assert_eq!(frames_per_connection(&received), [2, 2]);
}

/// A datahub restarted between two reads may have grown (or lost) the page cap. The repeat asks what
/// the NEW connection advertised, or it sends a server a cap it ignores, or none where one is due.
#[test]
fn the_repeat_reads_what_the_new_connection_advertises() {
    let (addr, received) = spawn_script(vec![plain(1), paged(2)]);
    let store = HistoryStoreDatahub::new(addr);
    let read = || store.load_bars(VENUE, SYMBOL, "1m", TimestampRange::all());

    read().expect("the first read dials");
    assert_eq!(read().expect("this read dials again").len(), 2);

    assert_eq!(frames_per_connection(&received), [1, 2]);
    let received = received.lock().unwrap();
    assert_matches!(received[0][0], Request::LoadBars { limit: None, .. });
    assert_matches!(received[1][0], Request::LoadBars { limit: Some(_), .. });
}

/// `Response::Error` leaves the socket healthy, so it is the datahub's answer and not a dead
/// connection. The read is not repeated; the connection is still dropped, as any error drops it.
#[test]
fn a_refusal_from_the_datahub_is_not_repeated() {
    let (addr, received) = spawn_script(vec![plain(2), plain(1)]);
    let store = HistoryStoreDatahub::new(addr);

    read_properties(&store).expect("the first read dials");
    let refused = store.properties_as_of(VENUE, REFUSED, 1).expect_err("the peer refuses it");
    assert_matches!(&refused, HistoryStoreError::Query(why) if why.contains("refused this read"));
    assert_eq!(frames_per_connection(&received), [2], "one frame of the refused read, one dial");

    read_properties(&store).expect("the read after a refusal dials");
    assert_eq!(frames_per_connection(&received), [2, 1]);
}

#[test]
fn the_repeat_happens_once() {
    let (addr, received) = spawn_script(vec![plain(1), plain(0)]);
    let store = HistoryStoreDatahub::new(addr);

    read_properties(&store).expect("the first read dials");
    let failed = read_properties(&store).expect_err("the new connection dies too");

    assert_matches!(failed, HistoryStoreError::Query(_), "a third dial would be a connect failure");
    assert_eq!(frames_per_connection(&received), [1, 0]);
}

/// A connection this read dialled itself was not idle, so it was not closed for being idle.
#[test]
fn a_fresh_connection_that_dies_is_not_repeated() {
    let (addr, received) = spawn_script(vec![plain(0), plain(1)]);
    let store = HistoryStoreDatahub::new(addr);

    read_properties(&store).expect_err("the first connection dies, and a repeat would work");
    assert_eq!(frames_per_connection(&received), [0]);

    read_properties(&store).expect("the next read dials");
    assert_eq!(frames_per_connection(&received), [0, 1]);
}

/// Only the first exchange on a held connection can meet one closed for being idle. Past a page it
/// answered, the connection was in use, so its loss is a real failure and it is reported.
#[test]
fn a_connection_lost_inside_a_paged_read_is_not_repeated() {
    let (addr, received) = spawn_script(vec![paged(3), paged(2)]);
    let store = HistoryStoreDatahub::new(addr);
    let read = || store.load_bars(VENUE, SYMBOL, "1m", TimestampRange::all());

    read().expect("the first read is two frames");
    let failed = read().expect_err("the third frame answered and the connection hung up");

    assert_matches!(failed, HistoryStoreError::Query(_));
    assert_eq!(frames_per_connection(&received), [3], "and no second dial");
}
