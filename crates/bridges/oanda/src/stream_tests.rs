use super::*;

#[test]
fn order_fill_transaction_dual_publishes_fill_and_wrap() {
    let v = serde_json::json!({
        "type": "ORDER_FILL", "id": "6373", "time": "1478012400.000000000",
        "orderID": "6372", "instrument": "EUR_USD", "units": "-1000", "price": "1.09300",
        "commission": "0.02", "clientExtensions": {"id": "coid-9"}
    });
    // Dual-publish contract: a fill yields BOTH the bare FillEvent (the Account folds
    // it into position/PnL) AND the OrderFilled wrap (the order FSM applies it), both
    // carrying the same FillEvent (same trade_id, so the two dedup sets key correctly).
    let evs = decode_transaction_events(&v);
    assert_eq!(evs.len(), 2, "fill must emit bare Fill + wrap");
    match &evs[0] {
        Event::Fill(fill) => {
            assert_eq!(fill.client_order_id, "coid-9"); // clientExtensions.id preferred
            assert_eq!(fill.trade_id, "6373");
            assert_eq!(fill.side, -1); // negative units → sell
            assert_eq!(fill.last_qty, 1000.0);
            assert_eq!(fill.last_px, 1.093);
            assert_eq!(fill.commission, 0.02);
            assert_eq!(fill.ts, 1_478_012_400_000);
        }
        other => panic!("expected bare Fill first, got {other:?}"),
    }
    match &evs[1] {
        Event::OrderFilled(of) => {
            assert_eq!(of.client_order_id, "coid-9");
            assert_eq!(of.fill.trade_id, "6373"); // same fill on the wrap
            assert_eq!(of.fill.side, -1);
        }
        other => panic!("expected OrderFilled wrap second, got {other:?}"),
    }
}

#[test]
fn order_cancel_transaction_maps_to_single_cancel() {
    let v = serde_json::json!({
        "type": "ORDER_CANCEL", "id": "700", "time": "1", "orderID": "6372", "reason": "CLIENT_REQUEST"
    });
    let evs = decode_transaction_events(&v);
    assert_eq!(evs.len(), 1);
    assert!(matches!(&evs[0], Event::OrderCanceled(c) if c.reason == "CLIENT_REQUEST"));
}

#[test]
fn heartbeat_is_ignored() {
    assert!(
        decode_transaction_events(&serde_json::json!({"type": "HEARTBEAT", "time": "1"}))
            .is_empty()
    );
}

// --- the rejected open: classified, logged, bounded ---------------------------------------
//
// These drive the REAL [`stream_transactions`] against a loopback server that answers a
// scripted status per open. They live here rather than in `tests/` because that is where the
// driver is reachable — this crate promotes only its PURE helpers to the crate root — and
// because the thing under test is the loop, not a decode.

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

/// How long a bounded lane is given to give up. Generous against the real budget
/// ([`MAX_TERMINAL_OPENS`] opens with [`RECONNECT_BACKOFF`] between them) so a slow runner
/// cannot flake it, and finite so the pre-fix behaviour — retry FOREVER — fails the test
/// instead of hanging the suite.
const LANE_GIVE_UP_BUDGET: Duration = Duration::from_secs(30);

/// A loopback HTTP server answering each open with the next status in `script`, then closing.
/// Its join value is how many opens it actually served — the count each test asserts on.
fn spawn_scripted_server(script: Vec<u16>) -> (SocketAddr, thread::JoinHandle<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let handle = thread::spawn(move || {
        let mut served = 0_usize;
        for status in script {
            let Ok((mut sock, _)) = listener.accept() else { break };
            let head = format!(
                "HTTP/1.1 {status} Rejected\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            let _ = sock.write_all(head.as_bytes());
            let _ = sock.flush();
            served += 1;
        }
        served
    });
    (addr, handle)
}

/// Run the REAL driver against `addr` on its own thread. The receiver fires exactly once
/// [`stream_transactions`] RETURNS, which is the property under test.
fn spawn_lane(addr: SocketAddr, stop: Arc<AtomicBool>) -> mpsc::Receiver<()> {
    let (done_tx, done_rx) = mpsc::channel();
    let config = OandaConfig {
        api_token: "bad-token".to_string(),
        account_id: "101-004-1234567-001".to_string(),
        rest_base: format!("http://{addr}"),
        stream_base: format!("http://{addr}"),
    };
    thread::spawn(move || {
        let (events, _ingest) = vike_exec::event_channel(16);
        stream_transactions(config, events, stop, Arc::new(AtomicU64::new(0)));
        let _ = done_tx.send(());
    });
    done_rx
}

/// The shared taxonomy answers, and the split this lane acts on: a rejection that re-issuing
/// cannot fix is terminal, everything transient keeps looping. Pinned as a unit so a widened
/// terminal set (which would abandon the lane over a rate limit) cannot land quietly.
#[test]
fn open_failures_classify_into_the_shared_taxonomy() {
    for status in [401, 403] {
        let kind = open_failure_kind(Some(status));
        assert_eq!(kind, ErrorKind::Auth, "HTTP {status} on a stream open is an auth failure");
        assert!(kind.is_terminal(), "a bad token answers the same way forever");
    }
    for status in [429, 500, 503] {
        assert!(
            !open_failure_kind(Some(status)).is_terminal(),
            "HTTP {status} is transient — this lane must keep retrying it"
        );
    }
    // No status at all: DNS/connect/TLS, the taxonomy's `code == 0` case. Retryable.
    assert_eq!(open_failure_kind(None), ErrorKind::Network);
    assert!(!open_failure_kind(None).is_terminal());
}

/// **THE TRAP, closed.** A venue that rejects the open outright used to be retried forever
/// with nothing logged. The lane now gives up after a bounded number of terminal rejections —
/// so this test TERMINATES, which is the whole assertion. (Before the fix it never returned.)
#[test]
fn a_permanently_rejected_open_gives_up_after_the_bounded_budget() {
    let (addr, server) = spawn_scripted_server(vec![401; MAX_TERMINAL_OPENS as usize]);
    let stop = Arc::new(AtomicBool::new(false));
    let done = spawn_lane(addr, stop.clone());

    done.recv_timeout(LANE_GIVE_UP_BUDGET)
        .expect("the lane must GIVE UP on a permanently rejected open, not retry forever");
    assert_eq!(
        server.join().expect("server thread joins"),
        MAX_TERMINAL_OPENS as usize,
        "the lane spends exactly its budget of opens before abandoning"
    );
    assert!(!stop.load(Ordering::Relaxed), "it gave up on its own, not because of a stop");
}

/// The COMPLEMENT, and the reason the split is not "abandon on any failed open": a transient
/// rejection must keep looping past the terminal budget. Serving one more 500 than
/// [`MAX_TERMINAL_OPENS`] proves the lane did not treat it as terminal.
#[test]
fn a_transient_rejection_keeps_retrying_past_the_terminal_budget() {
    let opens = MAX_TERMINAL_OPENS as usize + 1;
    let (addr, server) = spawn_scripted_server(vec![500; opens]);
    let stop = Arc::new(AtomicBool::new(false));
    let done = spawn_lane(addr, stop.clone());

    assert_eq!(
        server.join().expect("server thread joins"),
        opens,
        "a 5xx is retryable — the lane must still be dialling after the terminal budget"
    );
    // The listener is gone now, so further opens fail pre-send (also retryable): stop it.
    stop.store(true, Ordering::Relaxed);
    done.recv_timeout(LANE_GIVE_UP_BUDGET).expect("a stopped lane returns");
}

/// **The counter resets on a SUCCESSFUL open, not on any non-terminal one** — the rule
/// [`MAX_TERMINAL_OPENS`] states. Interleaving 5xx between auth rejections must NOT refill the
/// budget, or `401, 500, 401, 500, …` reopens the forever-loop this fix closes.
#[test]
fn an_interleaved_transient_failure_does_not_refill_the_terminal_budget() {
    let script = vec![401, 500, 401, 500, 401];
    let (addr, server) = spawn_scripted_server(script.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let done = spawn_lane(addr, stop.clone());

    done.recv_timeout(LANE_GIVE_UP_BUDGET)
        .expect("the third auth rejection must end the lane despite the 5xx between them");
    assert_eq!(
        server.join().expect("server thread joins"),
        script.len(),
        "it gave up on the THIRD terminal open, having also spent the two transient ones"
    );
}
