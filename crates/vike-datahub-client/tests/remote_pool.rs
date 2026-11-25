//! [`HistoryStoreDatahub`]'s connection POOL: reads of one handle from several threads are in flight
//! together, never more than [`HistoryStoreDatahub::POOL_CONNECTIONS`] connections exist, a connection
//! that failed is closed and its slot freed, and a dead idle one is dialled again.
//!
//! COUNTED, not timed: a scripted peer releases its LoadBars answers in GROUPS, each answer waiting
//! until its whole group has arrived, and records how many requests were in flight at once. A pool
//! that serialized its readers would never fill a group, so `most_in_flight` would read 1 (each wait
//! ends at the peer's hang guard instead, which only makes such a failure slow).

use std::assert_matches;
use std::net::TcpListener;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_datahub_client::HistoryStoreDatahub;
use vike_datahub_client::proto::{Request, Response};
use vike_datahub_client::test_support::accept_keyless;
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_marketdata::Bar;

/// How long one answer waits for its group before going anyway: a hang guard, never the assertion.
const GROUP_GUARD: Duration = Duration::from_secs(20);

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

#[derive(Default)]
struct Seen {
    accepted: usize,
    arrived: usize,
    in_flight: usize,
    most_in_flight: usize,
}

/// The scripted peer: it advertises no page cap, so a range read is ONE frame.
struct Peer {
    seen: Mutex<Seen>,
    moved: Condvar,
    group: usize,
}

impl Peer {
    /// Count one arrived request and hold it until the group it belongs to has arrived whole.
    fn hold_for_group(&self) {
        let mut seen = self.seen.lock().unwrap();
        seen.arrived += 1;
        seen.in_flight += 1;
        seen.most_in_flight = seen.most_in_flight.max(seen.in_flight);
        let release = seen.arrived.div_ceil(self.group) * self.group;
        self.moved.notify_all();
        let deadline = Instant::now() + GROUP_GUARD;
        while seen.arrived < release {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            seen = self.moved.wait_timeout(seen, left).unwrap().0;
        }
    }

    fn leave(&self) {
        self.seen.lock().unwrap().in_flight -= 1;
    }

    fn counts(&self) -> (usize, usize) {
        let seen = self.seen.lock().unwrap();
        (seen.accepted, seen.most_in_flight)
    }
}

/// Serve every connection on its own thread, as the datahub does (`dh-conn`). Connection `i` (in
/// accept order) answers `answers(i)` frames and then hangs up; `0` is a connection that dies right
/// after its `Welcome`.
fn spawn_peer(group: usize, answers: fn(usize) -> usize) -> (String, Arc<Peer>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port").to_string();
    let peer = Arc::new(Peer { seen: Mutex::default(), moved: Condvar::new(), group });
    let shared = Arc::clone(&peer);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.expect("accept the client");
            let peer = Arc::clone(&shared);
            let at = {
                let mut seen = peer.seen.lock().unwrap();
                seen.accepted += 1;
                seen.accepted - 1
            };
            thread::spawn(move || {
                if !accept_keyless(&mut stream, Vec::new()) {
                    return;
                }
                for _ in 0..answers(at) {
                    let Ok(request) = read_frame::<_, Request>(&mut stream) else { return };
                    assert_matches!(request, Request::LoadBars { .. });
                    peer.hold_for_group();
                    let sent =
                        write_frame(&mut stream, &Response::Bars(vec![bar(1_000), bar(2_000)]));
                    peer.leave();
                    if sent.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, peer)
}

fn read(store: &HistoryStoreDatahub) -> Result<Vec<i64>, HistoryStoreError> {
    let bars = store.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all())?;
    Ok(bars.iter().map(|b| b.timestamp).collect())
}

/// `n` reads of ONE shared handle, each on its own thread, all joined.
fn read_from_threads(
    store: &Arc<HistoryStoreDatahub>,
    n: usize,
) -> Vec<Result<Vec<i64>, HistoryStoreError>> {
    let readers: Vec<_> = (0..n)
        .map(|_| {
            let store = Arc::clone(store);
            thread::spawn(move || read(&store))
        })
        .collect();
    readers.into_iter().map(|r| r.join().expect("a reader thread")).collect()
}

#[test]
fn parallel_reads_of_one_handle_are_in_flight_together() {
    const READERS: usize = 4;
    let (addr, peer) = spawn_peer(READERS, |_| usize::MAX);
    let store = Arc::new(HistoryStoreDatahub::new(addr));

    for answer in read_from_threads(&store, READERS) {
        assert_eq!(answer.expect("every read is answered"), [1_000, 2_000]);
    }
    assert_eq!(peer.counts(), (READERS, READERS), "one connection each, all in flight at once");
}

#[test]
fn the_pool_never_holds_more_than_its_cap_and_reuses_what_comes_back() {
    let cap = HistoryStoreDatahub::POOL_CONNECTIONS;
    let (addr, peer) = spawn_peer(cap, |_| usize::MAX);
    let store = Arc::new(HistoryStoreDatahub::new(addr));

    for answer in read_from_threads(&store, 2 * cap) {
        assert_eq!(answer.expect("every read is answered"), [1_000, 2_000]);
    }
    // The second half waited at the cap and took the connections the first half returned.
    assert_eq!(peer.counts(), (cap, cap));
}

/// Every connection that failed is closed and frees its slot: a pool that leaked them would park the
/// read after the cap-th failure for good, so the reads run on a thread a deadline watches.
#[test]
fn a_failed_connection_frees_its_slot_and_the_next_read_dials_again() {
    let cap = HistoryStoreDatahub::POOL_CONNECTIONS;
    let (addr, peer) =
        spawn_peer(1, |at| if at < HistoryStoreDatahub::POOL_CONNECTIONS { 0 } else { usize::MAX });
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let store = HistoryStoreDatahub::new(addr);
        let answers: Vec<_> = (0..=cap).map(|_| read(&store)).collect();
        tx.send(answers).expect("hand the answers back");
    });
    let answers = rx.recv_timeout(Duration::from_secs(60)).expect("no read parks at a leaked cap");

    for failed in &answers[..cap] {
        assert_matches!(
            failed,
            Err(HistoryStoreError::Query(_)),
            "a fresh connection that dies fails"
        );
    }
    assert_eq!(answers[cap].as_deref().expect("the read past the failures dials"), [1_000, 2_000]);
    assert_eq!(peer.counts().0, cap + 1);
}

/// Two idle connections the datahub closed while they sat in the pool: two reads in flight together
/// each take one, find it dead, dial again ONCE and repeat (`remote_redial.rs` holds the one-connection
/// rules this keeps).
#[test]
fn each_dead_idle_connection_is_dialled_again() {
    let (addr, peer) = spawn_peer(2, |at| if at < 2 { 1 } else { usize::MAX });
    let store = Arc::new(HistoryStoreDatahub::new(addr));

    for answer in read_from_threads(&store, 2) {
        assert_eq!(answer.expect("the first two reads dial"), [1_000, 2_000]);
    }
    for answer in read_from_threads(&store, 2) {
        assert_eq!(
            answer.expect("each held connection is dead: each read dials again"),
            [1_000, 2_000]
        );
    }
    assert_eq!(peer.counts(), (4, 2));
}
