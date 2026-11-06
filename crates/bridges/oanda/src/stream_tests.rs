use super::*;
use std::assert_matches;
use std::time::{Duration, Instant};
use vike_log::loopback::redirecting_origin;

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
    let evs = decode_transaction_events(&v, &Default::default());
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
    let evs = decode_transaction_events(&v, &Default::default());
    assert_eq!(evs.len(), 1);
    assert_matches!(&evs[0], Event::OrderCanceled(c) if c.reason == "CLIENT_REQUEST");
}

/// **The cancel half of an order REPLACE is not a cancel of the order.** The replacement keeps the
/// client id, so mapping this line to `OrderCanceled` would TERMINATE an order that is still
/// resting. Either marker alone suffices; a plain cancel carries neither and still maps.
#[test]
fn the_cancel_half_of_a_replace_ends_nothing() {
    let by_successor = serde_json::json!({
        "type": "ORDER_CANCEL", "id": "6401", "time": "1", "orderID": "6372",
        "clientOrderID": "coid-1", "reason": "CLIENT_REQUEST_REPLACED", "replacedByOrderID": "6402"
    });
    assert!(decode_transaction_events(&by_successor, &Default::default()).is_empty());
    let by_reason_only = serde_json::json!({
        "type": "ORDER_CANCEL", "id": "6401", "time": "1", "orderID": "6372",
        "clientOrderID": "coid-1", "reason": "CLIENT_REQUEST_REPLACED"
    });
    assert!(decode_transaction_events(&by_reason_only, &Default::default()).is_empty());
    let by_successor_only = serde_json::json!({
        "type": "ORDER_CANCEL", "id": "6401", "time": "1", "orderID": "6372",
        "clientOrderID": "coid-1", "replacedByOrderID": "6402"
    });
    assert!(decode_transaction_events(&by_successor_only, &Default::default()).is_empty());
    let plain = serde_json::json!({
        "type": "ORDER_CANCEL", "id": "6403", "time": "1", "orderID": "6402",
        "clientOrderID": "coid-1", "reason": "CLIENT_REQUEST"
    });
    assert_matches!(
        decode_transaction_events(&plain, &Default::default()).as_slice(),
        [Event::OrderCanceled(c)] if c.client_order_id == "coid-1"
    );
}

/// The fill of a REPLACEMENT order names the order by the carried-over client id, not by the new
/// venue order id, so it folds into the order the platform already knows.
#[test]
fn a_fill_of_the_replacement_order_carries_the_original_client_id() {
    let fill = serde_json::json!({
        "type": "ORDER_FILL", "id": "6410", "time": "2", "orderID": "6402",
        "clientOrderID": "coid-1", "instrument": "EUR_USD", "units": "500", "price": "1.095"
    });
    match decode_transaction_events(&fill, &Default::default()).as_slice() {
        [Event::Fill(f), Event::OrderFilled(w)] => {
            assert_eq!(f.client_order_id, "coid-1");
            assert_eq!(w.client_order_id, "coid-1");
        }
        other => panic!("expected the dual-publish pair, got {other:?}"),
    }
}

#[test]
fn heartbeat_is_ignored() {
    assert!(
        decode_transaction_events(
            &serde_json::json!({"type": "HEARTBEAT", "time": "1"}),
            &Default::default()
        )
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
/// ([`MAX_TERMINAL_OPENS`] opens with the exec row's fixed reconnect wait between them) so a slow
/// runner cannot flake it, and finite so the pre-fix behaviour — retry FOREVER — fails the test
/// instead of hanging the suite.
const LANE_GIVE_UP_BUDGET: Duration = Duration::from_secs(30);

/// The bounds `transactions_agent` builds are the exec lane's row, each on its own builder call —
/// observed on the BUILT agent (`agent.config().timeouts()`), not on a constant that could stop
/// being the thing the builder reads. `connect` and `send_request` are both 10 s today and are two
/// facts: the dial and the wait for the response head.
#[test]
fn the_transactions_agent_carries_the_exec_rows_bounds() {
    let timeouts = transactions_agent(venue_timings::OANDA.exec()).config().timeouts();
    assert_eq!(timeouts.connect, Some(Duration::from_secs(10)), "the dial bound");
    assert_eq!(timeouts.send_request, Some(Duration::from_secs(10)), "the response-head wait");
    assert_eq!(timeouts.recv_response, None, "a recv_response bound would cap the whole body");
    assert_eq!(timeouts.recv_body, Some(Duration::from_secs(20)), "the idle bound per body read");
}

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

/// Run the REAL driver against `addr` on its own thread, on the lane bounds `exec` (the production row
/// unless a test shortens a bound it would otherwise wait out). The receiver fires exactly once
/// [`stream_transactions_on`] RETURNS, which is the property under test.
fn spawn_lane(
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    exec: venue_timings::Lane,
) -> mpsc::Receiver<()> {
    spawn_reporting_lane(addr, stop, exec, StreamLink::default())
}

fn lane_config(addr: SocketAddr) -> OandaConfig {
    OandaConfig {
        api_token: "bad-token".to_string(),
        account_id: "101-004-1234567-001".to_string(),
        rest_base: format!("http://{addr}"),
        stream_base: format!("http://{addr}"),
    }
}

/// The head of one request, read off the socket; its first line comes back.
fn read_request_line(sock: &mut std::net::TcpStream) -> String {
    use std::io::Read;
    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = sock.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        head.extend_from_slice(&chunk[..n]);
    }
    String::from_utf8_lossy(&head).lines().next().unwrap_or("").to_string()
}

/// A REST host that states a floor and an empty gap to whoever asks: the lane of a test about its OPENS asks it for
/// both, and the script of opens never hears of it.
fn quiet_rest() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        for mut sock in listener.incoming().flatten() {
            read_request_line(&mut sock);
            let body = serde_json::json!({ "transactions": [], "lastTransactionID": "100",
                "account": { "lastTransactionID": "100" } })
            .to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(reply.as_bytes());
        }
    });
    format!("http://{addr}")
}

/// [`spawn_lane`] stating itself on `link`.
fn spawn_reporting_lane(
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    exec: venue_timings::Lane,
    link: StreamLink,
) -> mpsc::Receiver<()> {
    let (done_tx, done_rx) = mpsc::channel();
    let config = OandaConfig { rest_base: quiet_rest(), ..lane_config(addr) };
    thread::spawn(move || {
        let (events, _ingest) = vike_exec::event_channel(16);
        let last_seen = Arc::new(AtomicU64::new(0));
        stream_transactions_on(
            exec,
            AUTH_WAITS,
            config,
            events,
            stop,
            last_seen,
            Default::default(),
            link,
        );
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
/// The end is stated on the link for good: nothing will reopen this stream, and the doctor must say so.
///
/// It also pins the wait BETWEEN the opens from below: the exec row's reconnect is a fixed 1 s and
/// this lane reads only its `initial`, so the `MAX_TERMINAL_OPENS - 1` gaps cost at least that many
/// seconds (`sleep_unless_stopped` never sleeps short, so the bound cannot flake on a fast runner).
/// A row that stopped being fixed would stop being what this loop does, hence the `is_fixed` check.
#[test]
fn a_permanently_rejected_open_gives_up_after_the_bounded_budget() {
    let reconnect = venue_timings::OANDA.exec().reconnect();
    assert!(reconnect.is_fixed(), "stream_transactions reads `initial` and nothing else");
    assert_eq!(reconnect.initial, Duration::from_secs(1));

    let (addr, server) = spawn_scripted_server(vec![403; MAX_TERMINAL_OPENS as usize]);
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let link = StreamLink::default();
    let done = spawn_reporting_lane(addr, stop.clone(), venue_timings::OANDA.exec(), link.clone());

    done.recv_timeout(LANE_GIVE_UP_BUDGET)
        .expect("the lane must GIVE UP on a permanently rejected open, not retry forever");
    let down = link.down().expect("the lane's end is stated");
    assert_eq!(down.effect, GAVE_UP_EFFECT);
    assert_eq!(down.retry_in, Duration::ZERO, "no next try");
    assert!(down.why.contains("HTTP 403"), "{}", down.why);
    assert!(
        started.elapsed() >= Duration::from_secs(1) * (MAX_TERMINAL_OPENS - 1),
        "the opens were not spaced by the 1 s exec reconnect wait: gave up in {:?}",
        started.elapsed()
    );
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
    let done = spawn_lane(addr, stop.clone(), venue_timings::OANDA.exec());

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
/// budget, or `403, 500, 403, 500, …` reopens the forever-loop this fix closes.
#[test]
fn an_interleaved_transient_failure_does_not_refill_the_terminal_budget() {
    let script = vec![403, 500, 403, 500, 403];
    let (addr, server) = spawn_scripted_server(script.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let done = spawn_lane(addr, stop.clone(), venue_timings::OANDA.exec());

    done.recv_timeout(LANE_GIVE_UP_BUDGET)
        .expect("the third auth rejection must end the lane despite the 5xx between them");
    assert_eq!(
        server.join().expect("server thread joins"),
        script.len(),
        "it gave up on the THIRD terminal open, having also spent the two transient ones"
    );
}

// --- a 401 keeps the lane: OANDA says it now and then to a good token ----------------------

/// The lane's 401 waits made quick, the slow clock told apart from [`quick_opens`]' wait; the production values are
/// constants.
const QUICK_AUTH: AuthWaits =
    AuthWaits { reopen: Duration::from_millis(60), read: [Duration::from_millis(40); 2] };

/// The exec row with a quick wait between opens, so a test sits through neither clock.
fn quick_opens() -> venue_timings::Lane {
    venue_timings::Lane {
        reconnect: Some(venue_timings::Reconnect::fixed(
            Duration::from_millis(40),
            "a test's wait",
        )),
        ..venue_timings::OANDA.exec()
    }
}

/// One line of the stream the scripted venue pushes once it accepts an open.
fn stream_fill(id: &str) -> String {
    serde_json::json!({ "type": "ORDER_FILL", "id": id, "time": "1478012400.000000000", "orderID": "7",
        "instrument": "EUR_USD", "units": "100", "price": "1.1", "clientExtensions": { "id": "coid-7" } })
    .to_string()
}

/// What the scripted venue hands back: the request lines it served and the streams it left open.
type Served = (Vec<String>, Vec<std::net::TcpStream>);

/// A scripted OANDA, by request line, whose `/summary` states the floor 100.
fn spawn_scripted_venue(
    opens: Vec<u16>,
    backfills: Vec<(u16, String)>,
    line: String,
    ends: Option<Arc<AtomicBool>>,
) -> (SocketAddr, thread::JoinHandle<Served>) {
    spawn_venue_asked(Vec::new(), opens, backfills, line, ends)
}

/// A scripted OANDA, by request line: each `/summary` read answers the next `(status, body)` of `summaries` (then the
/// floor 100), each `sinceid` read the next of `backfills` (then an empty gap), and each stream open the next status of
/// `opens`. A 200 pushes `line` and then, with `ends`, raises that stop and closes; without it the stream stays open,
/// handed back in the join value beside every request line served, so it outlives the script. A 201 pushes `line` and
/// closes, and a 202 opens and ends at once: a stream that drops. It serves until the script is spent and the last open
/// has had its gap read.
fn spawn_venue_asked(
    summaries: Vec<(u16, String)>,
    opens: Vec<u16>,
    backfills: Vec<(u16, String)>,
    line: String,
    ends: Option<Arc<AtomicBool>>,
) -> (SocketAddr, thread::JoinHandle<Served>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let handle = thread::spawn(move || {
        let (mut opens, mut backfills) = (opens.into_iter(), backfills.into_iter());
        let mut summaries = summaries.into_iter();
        let mut served = Vec::new();
        let mut held = Vec::new();
        let mut gap_owed = false;
        let deadline = Instant::now() + LANE_GIVE_UP_BUDGET;
        while Instant::now() < deadline
            && (opens.len() > 0 || backfills.len() > 0 || summaries.len() > 0 || gap_owed)
        {
            let Ok((mut sock, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            sock.set_nonblocking(false).expect("blocking reads");
            sock.set_read_timeout(Some(Duration::from_secs(2))).expect("read timeout");
            let request = read_request_line(&mut sock);
            let answer = if request.contains("sinceid") {
                let answer = backfills.next().unwrap_or_else(|| gap_of(&[], "100"));
                gap_owed &= answer.0 != 200;
                Some(answer)
            } else if request.contains("/summary") {
                Some(summaries.next().unwrap_or_else(|| seed_at("100")))
            } else {
                opens.next().map(|status| {
                    gap_owed |= (200..300).contains(&status);
                    (status, String::new())
                })
            };
            served.push(request);
            let Some((status, body)) = answer else { continue };
            if matches!(status, 200 | 201) && body.is_empty() {
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{line}\n\r\n",
                    line.len() + 1
                );
                let _ = sock.write_all(reply.as_bytes());
                let _ = sock.flush();
                match &ends {
                    _ if status == 201 => {}
                    Some(stop) => stop.store(true, Ordering::Relaxed),
                    None => held.push(sock),
                }
                continue;
            }
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(reply.as_bytes());
            let _ = sock.flush();
        }
        (served, held)
    });
    (addr, handle)
}

/// A lane over the scripted venue with its ingest kept, from the watermark `last_seen`.
fn venue_lane(
    addr: SocketAddr,
    last_seen: u64,
    link: StreamLink,
) -> (Arc<AtomicBool>, mpsc::Receiver<()>, tokio::sync::mpsc::Receiver<vike_exec::Ingest>) {
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel();
    let (events, ingest) = vike_exec::event_channel(64);
    let config = lane_config(addr);
    let lane_stop = stop.clone();
    thread::spawn(move || {
        stream_transactions_on(
            quick_opens(),
            QUICK_AUTH,
            config,
            events,
            lane_stop,
            Arc::new(AtomicU64::new(last_seen)),
            Default::default(),
            link,
        );
        let _ = done_tx.send(());
    });
    (stop, done_rx, ingest)
}

/// The next fill's trade id on the ingest, or a failure after the budget.
fn next_fill(ingest: &mut tokio::sync::mpsc::Receiver<vike_exec::Ingest>) -> String {
    let deadline = Instant::now() + LANE_GIVE_UP_BUDGET;
    while Instant::now() < deadline {
        match ingest.try_recv() {
            Ok(vike_exec::Ingest::Event(Event::Fill(fill))) => return fill.trade_id.to_string(),
            Ok(_) => {}
            Err(_) => thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("no fill reached the engine's lane");
}

/// A `/summary` answer: the account's last transaction is `last`.
fn seed_at(last: &str) -> (u16, String) {
    let body =
        serde_json::json!({ "account": { "lastTransactionID": last }, "lastTransactionID": last });
    (200, body.to_string())
}

/// A `sinceid` answer: these fills, and the account's last transaction.
fn gap_of(fills: &[&str], last: &str) -> (u16, String) {
    let rows: Vec<serde_json::Value> = fills
        .iter()
        .map(|id| serde_json::from_str(&stream_fill(id)).expect("the fill is json"))
        .collect();
    (200, serde_json::json!({ "transactions": rows, "lastTransactionID": last }).to_string())
}

/// OANDA's passing refusal, on a REST read.
fn refused_read() -> (u16, String) {
    (401, serde_json::json!({ "errorMessage": "Insufficient authorization" }).to_string())
}

/// How many of the request lines the venue served contain `what`.
fn asked(lines: &[String], what: &str) -> usize {
    lines.iter().filter(|line| line.contains(what)).count()
}

/// The stream's first statement that it is down, or a failure after the budget.
fn wait_down(link: &StreamLink) -> StreamDown {
    let deadline = Instant::now() + LANE_GIVE_UP_BUDGET;
    loop {
        match link.down() {
            Some(down) => return down,
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
            None => panic!("the link never said the stream is down"),
        }
    }
}

/// **Three 401s in a row used to end resting fills and venue-side cancels until a restart, while the error blamed the
/// credentials.** The venue answers 401 past the old budget and then accepts: the lane is still there, the link says
/// the stream is down, on the slow clock, in words that do not call the orders refused, the stream that opens delivers,
/// and the link says it is up again.
#[test]
fn a_401_past_the_budget_keeps_the_lane_and_the_stream_comes_back() {
    let refusals = MAX_TERMINAL_OPENS as usize + 2;
    let mut opens = vec![401; refusals];
    opens.push(200);
    let (addr, server) = spawn_scripted_venue(opens, Vec::new(), stream_fill("6373"), None);
    let link = StreamLink::default();
    let (stop, done, mut ingest) = venue_lane(addr, 0, link.clone());

    let down = wait_down(&link);
    assert_eq!(down.effect, REOPENING_EFFECT);
    assert_eq!(down.retry_in, QUICK_AUTH.reopen, "the slow clock, not the 1 s row");
    assert!(down.failed_opens >= MAX_TERMINAL_OPENS, "{down:?}");
    assert!(down.why.contains("HTTP 401") && down.why.contains("good token"), "{}", down.why);

    assert_eq!(next_fill(&mut ingest), "6373", "the stream that opened delivers");
    assert!(done.try_recv().is_err(), "the lane did not end");
    assert_eq!(link.down(), None, "a stream that opened is up");

    let (lines, _open) = server.join().expect("the venue joins");
    assert_eq!(asked(&lines, "transactions/stream"), refusals + 1, "every 401: {lines:?}");
    stop.store(true, Ordering::Relaxed);
}

/// **One ERROR, said once and without telling the operator to fix a good key, and one line when it is back**, however
/// many opens the refusal takes. The lane runs on this thread, so its log is the capture's; the venue ends it.
#[test]
fn a_401_that_lasts_says_one_error_and_one_line_when_it_is_back() {
    let mut opens = vec![401; MAX_TERMINAL_OPENS as usize + 4];
    opens.push(200);
    let stop = Arc::new(AtomicBool::new(false));
    let (addr, server) =
        spawn_scripted_venue(opens, Vec::new(), stream_fill("6376"), Some(stop.clone()));
    let (events, _ingest) = vike_exec::event_channel(64);
    let ((), said) = vike_log::capture::captured(|| {
        stream_transactions_on(
            quick_opens(),
            QUICK_AUTH,
            lane_config(addr),
            events,
            stop,
            Arc::new(AtomicU64::new(0)),
            Default::default(),
            StreamLink::default(),
        );
    });
    server.join().expect("the venue joins");
    let errors: Vec<_> = said.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
    assert_eq!(errors.len(), 1, "{said:?}");
    assert!(errors[0].message.contains("good token"), "{}", errors[0].message);
    assert!(errors[0].message.contains("keeps asking"), "{}", errors[0].message);
    assert!(!errors[0].message.contains("Fix the account"), "{}", errors[0].message);
    let back = said.iter().filter(|e| e.message.contains("accepted again")).count();
    assert_eq!(back, 1, "{said:?}");
    let seeded = said.iter().filter(|e| e.message.contains("reads back from this floor")).count();
    assert_eq!(seeded, 1, "the seed is said once, whatever the opens do: {said:?}");
}

/// Fewer failed opens than the budget say nothing on the link, so a passing 401 does not reach the doctor.
#[test]
fn a_passing_401_does_not_call_the_stream_down() {
    let (addr, server) =
        spawn_scripted_venue(vec![401, 200], Vec::new(), stream_fill("6374"), None);
    let link = StreamLink::default();
    let (stop, _done, mut ingest) = venue_lane(addr, 0, link.clone());
    assert_eq!(next_fill(&mut ingest), "6374");
    assert_eq!(link.down(), None);
    assert_eq!(asked(&server.join().expect("the venue joins").0, "transactions/stream"), 2);
    stop.store(true, Ordering::Relaxed);
}

/// **A first open that was refused has a gap behind it too**: a resting order fills in that gap, so the first open that
/// works reads it back, from the lane's floor and with the watermark at 0, as production starts it. And the backfill's
/// own read outlasts a 401, which the refusal the stream just came out of can well answer.
#[test]
fn the_first_open_after_a_refusal_reads_the_gap_past_a_401() {
    let (addr, server) = spawn_venue_asked(
        vec![seed_at("6399")],
        vec![401, 200],
        vec![refused_read(), gap_of(&["6400"], "6400")],
        stream_fill("6401"),
        None,
    );
    let (stop, _done, mut ingest) = venue_lane(addr, 0, StreamLink::default());
    assert_eq!(next_fill(&mut ingest), "6400", "the gap's fill comes first");
    assert_eq!(next_fill(&mut ingest), "6401", "then the live line");
    let (lines, _open) = server.join().expect("the venue joins");
    let reads = lines.iter().filter(|l| l.contains("sinceid?id=6399")).count();
    assert_eq!(reads, 2, "the backfill asked again after the 401: {lines:?}");
    stop.store(true, Ordering::Relaxed);
}

// --- the lane's floor: the stream never reads past a fill it did not deliver ----------------

/// **The first way in: no watermark.** After a restart with an adopted limit and no new order nothing moved the watermark
/// (a heartbeat has no `id`), so a drop read nothing back and the fill stayed a held `MissingFill`. The floor is the
/// lane's own, asked once, and a drop reads from it with nothing else having happened.
#[test]
fn a_stream_that_drops_before_any_transaction_reads_the_gap_from_the_lanes_floor() {
    let (addr, server) = spawn_venue_asked(
        vec![seed_at("6399")],
        vec![202, 200],
        vec![gap_of(&[], "6399"), gap_of(&["6400"], "6400")],
        stream_fill("6401"),
        None,
    );
    let (stop, _done, mut ingest) = venue_lane(addr, 0, StreamLink::default());
    assert_eq!(next_fill(&mut ingest), "6400", "the fill in the gap");
    assert_eq!(next_fill(&mut ingest), "6401", "then the live line");
    let (lines, _open) = server.join().expect("the venue joins");
    assert_eq!(asked(&lines, "sinceid?id=6399 "), 2, "{lines:?}");
    assert_eq!(asked(&lines, "/summary"), 1, "seeded once: {lines:?}");
    stop.store(true, Ordering::Relaxed);
}

/// A fill that lands between the seed and the first open is on no stream (the stream starts when it is asked for) and
/// above the floor, so the FIRST open reads back too, and only once its stream is open: read the other way round, a fill
/// between the read and the open would be on neither.
#[test]
fn a_fill_between_the_seed_and_the_first_open_is_read_back() {
    let (addr, server) = spawn_venue_asked(
        vec![seed_at("6399")],
        vec![200],
        vec![gap_of(&["6400"], "6400")],
        stream_fill("6401"),
        None,
    );
    let (stop, _done, mut ingest) = venue_lane(addr, 0, StreamLink::default());
    assert_eq!(next_fill(&mut ingest), "6400");
    assert_eq!(next_fill(&mut ingest), "6401");
    let (lines, _open) = server.join().expect("the venue joins");
    let told = ["/summary", "transactions/stream", "sinceid?id=6399 "];
    assert!(told.iter().zip(&lines).all(|(what, line)| line.contains(what)), "{lines:?}");
    stop.store(true, Ordering::Relaxed);
}

/// **The second way in: a POST moves it.** The stream is down, a resting order fills, an order POST answers with a
/// `lastTransactionID` past that fill: the reopen still reads from the floor. The exec path's watermark, what an
/// ambiguous submit is scanned from, keeps its own rule: the stream's reads still advance it, and it never falls.
#[test]
fn an_order_post_moves_no_floor_and_the_exec_watermark_still_follows_the_stream() {
    for (posted, after) in [(9000_u64, 9000_u64), (5, 6400)] {
        let stop = Arc::new(AtomicBool::new(false));
        let (addr, server) = spawn_venue_asked(
            vec![seed_at("6399")],
            vec![401, 200],
            vec![gap_of(&["6400"], "6400")],
            stream_fill("6401"),
            Some(stop.clone()),
        );
        let last_seen = Arc::new(AtomicU64::new(0));
        let post = serde_json::json!({ "lastTransactionID": posted.to_string() });
        crate::exec::note_last_transaction_id(&last_seen, &post);
        let (events, mut ingest) = vike_exec::event_channel(64);
        stream_transactions_on(
            quick_opens(),
            QUICK_AUTH,
            lane_config(addr),
            events,
            stop,
            last_seen.clone(),
            Default::default(),
            StreamLink::default(),
        );
        assert_eq!(next_fill(&mut ingest), "6400", "the fill the POST's id lies past");
        let (lines, _open) = server.join().expect("the venue joins");
        assert_eq!(asked(&lines, "sinceid?id=6399 "), 1, "{lines:?}");
        assert_eq!(last_seen.load(Ordering::Relaxed), after);
    }
}

/// **The third way in: a failed backfill.** The read fails, so the response is dropped unread (the line it pushed moves
/// nothing), and the next open reads from the same floor. The line then arrives twice, once in the gap and once live,
/// under one trade id: `crates/bridges/oanda/tests/offline/engine_fill.rs` books that once.
#[test]
fn a_failed_backfill_is_a_failed_open_and_skips_nothing() {
    let boom = (500, serde_json::json!({ "errorMessage": "boom" }).to_string());
    let (addr, server) = spawn_venue_asked(
        vec![seed_at("6399")],
        vec![200, 200],
        vec![boom, gap_of(&["6400", "6402"], "6402")],
        stream_fill("6402"),
        None,
    );
    let (stop, _done, mut ingest) = venue_lane(addr, 0, StreamLink::default());
    for id in ["6400", "6402", "6402"] {
        assert_eq!(next_fill(&mut ingest), id);
    }
    let (lines, _open) = server.join().expect("the venue joins");
    assert_eq!(asked(&lines, "sinceid?id=6399 "), 2, "{lines:?}");
    assert_eq!(asked(&lines, "transactions/stream"), 2, "{lines:?}");
    stop.store(true, Ordering::Relaxed);
}

/// A read that outlasts its asks is a refused open: it stays on the 401's slow clock, the link says the stream is down
/// in the words that promise the fills, and they come. `REOPENING_EFFECT` is this test.
#[test]
fn a_backfill_refused_past_its_asks_keeps_the_stream_down_and_then_delivers() {
    let mut backfills = vec![refused_read(); 5 * 3]; // five opens, three asks each
    backfills.push(gap_of(&["6400"], "6400"));
    let (addr, server) = spawn_venue_asked(
        vec![seed_at("6399")],
        vec![200; 6],
        backfills,
        stream_fill("6401"),
        None,
    );
    let link = StreamLink::default();
    let (stop, done, mut ingest) = venue_lane(addr, 0, link.clone());
    let down = wait_down(&link);
    assert_eq!(down.effect, REOPENING_EFFECT);
    assert_eq!(down.retry_in, QUICK_AUTH.reopen, "the slow clock");
    assert!(down.why.contains("OANDA error 401"), "{}", down.why);
    assert_eq!(next_fill(&mut ingest), "6400", "before any line of the open that read it");
    assert_eq!(next_fill(&mut ingest), "6401");
    assert_eq!(link.down(), None, "the stream counts as up once its gap is read");
    assert!(done.try_recv().is_err(), "the lane did not end");
    let (lines, _open) = server.join().expect("the venue joins");
    assert_eq!(asked(&lines, "sinceid?id=6399 "), 16, "{lines:?}");
    stop.store(true, Ordering::Relaxed);
}

/// Only a rejected stream OPEN ends the lane. A REST read of the floor or of the gap answering 403 past the open's budget
/// says nothing about a stream the host has just accepted, and ending would stop what only a restart could restore.
#[test]
fn a_refused_rest_read_never_ends_the_lane() {
    let many = MAX_TERMINAL_OPENS as usize + 1;
    let refusal = (403, serde_json::json!({ "errorMessage": "no" }).to_string());
    let then = |last: (u16, String)| {
        let mut script = vec![refusal.clone(); many];
        script.push(last);
        script
    };
    for (summaries, backfills) in [
        (vec![seed_at("6399")], then(gap_of(&["6400"], "6400"))),
        (then(seed_at("6399")), vec![gap_of(&["6400"], "6400")]),
    ] {
        let (addr, server) =
            spawn_venue_asked(summaries, vec![200; many + 1], backfills, stream_fill("6401"), None);
        let (stop, done, mut ingest) = venue_lane(addr, 0, StreamLink::default());
        assert_eq!(next_fill(&mut ingest), "6400");
        assert!(done.try_recv().is_err(), "the lane did not end");
        server.join().expect("the venue joins");
        stop.store(true, Ordering::Relaxed);
    }
}

/// **A first seed that fails.** No floor means no line is read: a line would be the first fact the lane has, and the gap
/// behind it could never be asked for. The seed is asked again before each open (a 200 that states no id is no answer),
/// and until one answers the link does not promise what it cannot deliver: a fill before the first answer lies below
/// the floor, which is reconcile's to name.
#[test]
fn a_seed_that_fails_is_a_failed_open_until_one_answers() {
    let mut summaries = vec![(500, serde_json::json!({ "errorMessage": "boom" }).to_string()); 7];
    summaries.push((200, "{}".to_string()));
    summaries.push(seed_at("6399"));
    let tries = summaries.len();
    let (addr, server) = spawn_venue_asked(
        summaries,
        vec![200; tries],
        vec![gap_of(&["6400"], "6400")],
        stream_fill("6401"),
        None,
    );
    let link = StreamLink::default();
    let (stop, _done, mut ingest) = venue_lane(addr, 0, link.clone());
    assert_eq!(wait_down(&link).effect, NO_FLOOR_EFFECT);
    assert_eq!(next_fill(&mut ingest), "6400", "no line of an open with no floor was read");
    assert_eq!(next_fill(&mut ingest), "6401");
    assert_eq!(link.down(), None);
    let (lines, _open) = server.join().expect("the venue joins");
    assert_eq!(asked(&lines, "/summary"), tries, "{lines:?}");
    assert_eq!(asked(&lines, "sinceid"), 1, "{lines:?}");
    stop.store(true, Ordering::Relaxed);
}

/// A 200 with no array is not an empty gap: its `lastTransactionID` would move the floor past what it did not state.
#[test]
fn a_backfill_body_without_its_array_is_a_failed_open() {
    let (addr, server) = spawn_venue_asked(
        vec![seed_at("6399")],
        vec![200, 200],
        vec![
            (200, serde_json::json!({ "lastTransactionID": "9000" }).to_string()),
            gap_of(&["6400"], "6400"),
        ],
        stream_fill("6401"),
        None,
    );
    let (stop, _done, mut ingest) = venue_lane(addr, 0, StreamLink::default());
    assert_eq!(next_fill(&mut ingest), "6400");
    let (lines, _open) = server.join().expect("the venue joins");
    assert_eq!(asked(&lines, "sinceid?id=6399 "), 2, "{lines:?}");
    assert_eq!(asked(&lines, "sinceid?id=9000 "), 0, "{lines:?}");
    stop.store(true, Ordering::Relaxed);
}

/// The floor does move, so a reopen reads what it missed and no more: past a backfill that answered, and past a line read.
#[test]
fn the_floor_moves_on_a_backfill_that_answered_and_on_a_line_read() {
    let (addr, server) = spawn_venue_asked(
        vec![seed_at("6399")],
        vec![202, 201, 200],
        vec![gap_of(&[], "6450"), gap_of(&[], "6450"), gap_of(&[], "6455")],
        stream_fill("6455"),
        None,
    );
    let (stop, _done, mut ingest) = venue_lane(addr, 0, StreamLink::default());
    assert_eq!(next_fill(&mut ingest), "6455");
    assert_eq!(next_fill(&mut ingest), "6455");
    let (lines, _open) = server.join().expect("the venue joins");
    for floor in ["6399", "6450", "6455"] {
        assert_eq!(asked(&lines, &format!("sinceid?id={floor} ")), 1, "{floor}: {lines:?}");
    }
    stop.store(true, Ordering::Relaxed);
}

/// A lane starting on a cell an earlier adapter left down starts clean, before its first open has answered (the open
/// here hangs on a listener nobody accepts from), and one label is one cell.
#[test]
fn a_new_lane_clears_the_link_an_old_one_left_down() {
    let label = AccountLabel::parse("LINKS").expect("a legal label");
    let link = stream_link(&label);
    link.set(Some(StreamDown {
        why: "old".to_string(),
        effect: GAVE_UP_EFFECT,
        failed_opens: 3,
        retry_in: Duration::ZERO,
    }));
    assert!(stream_link(&label).down().is_some(), "one label, one cell");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (stop, _done, _ingest) = venue_lane(addr, 0, link);
    assert!(
        vike_log::poll::wait_until(Duration::from_secs(5), || stream_link(&label).down().is_none()),
        "the lane starts clean"
    );
    stop.store(true, Ordering::Relaxed);
}

// --- an opened stream outlives the response-head bound --------------------------------------

/// The exec row with the two bounds the heartbeat test waits out made five times shorter, each in its
/// production proportion (the head wait 10 s -> 2 s, the read-idle bound 20 s -> 4 s); the dial bound and the
/// wait between opens stay the row's. The production values are pinned by
/// `the_transactions_agent_carries_the_exec_rows_bounds`.
fn short_head_and_idle_bounds() -> venue_timings::Lane {
    venue_timings::Lane {
        response_timeout: Some(venue_timings::Timing::secs(2, "the head wait, 1/5 of the row's")),
        idle_threshold: Some(venue_timings::Timing::secs(
            4,
            "the read-idle bound, 1/5 of the row's",
        )),
        ..venue_timings::OANDA.exec()
    }
}

/// The heartbeat cadence of the server below: 1/5 of OANDA's ~3 s, as the bounds are.
const HEARTBEAT_EVERY: Duration = Duration::from_millis(600);

/// How long the heartbeating server keeps one stream open. It must clear both the head wait (the
/// bound that used to cap the body, see `transactions_agent`) and the idle bound of
/// [`short_head_and_idle_bounds`], which an unbroken heartbeat feed never trips: only a stream killed
/// by the HEAD bound fails this (26 s against the row's 10 s and 20 s, scaled by the same 1/5).
const HEARTBEAT_STREAM_LIFETIME: Duration = Duration::from_millis(5200);

/// **A healthy stream is not cut after the response-head bound.** The loopback server answers with
/// a chunked 200 and one HEARTBEAT every [`HEARTBEAT_EVERY`] for [`HEARTBEAT_STREAM_LIFETIME`]. Under
/// `timeout_recv_response` ureq ended the body one head wait after the head; the lane then redialled,
/// which shows here as a second connection waiting in the listener's backlog (a TCP connect completes
/// before `accept`, so the pending dial is observable without a second server loop). The wait is
/// real time on purpose (the bound under test is ureq's own clock), on bounds made short
/// ([`short_head_and_idle_bounds`]) so the test does not sit through the production seconds.
#[test]
fn a_heartbeating_stream_survives_past_the_response_head_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("the lane dials once");
        let head = "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\n\
                    transfer-encoding: chunked\r\n\r\n";
        sock.write_all(head.as_bytes()).expect("write the response head");
        let beat = "{\"type\":\"HEARTBEAT\",\"time\":\"1700000000.000000000\"}\n";
        let started = Instant::now();
        let mut writes_ok = true;
        while started.elapsed() < HEARTBEAT_STREAM_LIFETIME {
            let chunk = format!("{:x}\r\n{beat}\r\n", beat.len());
            writes_ok &= sock.write_all(chunk.as_bytes()).and_then(|()| sock.flush()).is_ok();
            thread::sleep(HEARTBEAT_EVERY);
        }
        // Stop first, then look for a second dial: the lane returns on its next line or, if it
        // has already left its read loop, on its next `while` check.
        server_stop.store(true, Ordering::Relaxed);
        listener.set_nonblocking(true).expect("non-blocking accept");
        let redialled = listener.accept().is_ok();
        (writes_ok, redialled)
    });
    let done = spawn_lane(addr, stop.clone(), short_head_and_idle_bounds());

    let (writes_ok, redialled) = server.join().expect("server thread joins");
    assert!(writes_ok, "the lane closed the connection while heartbeats were still arriving");
    assert!(
        !redialled,
        "the lane dropped a healthy stream and redialled before the server ended it"
    );
    done.recv_timeout(LANE_GIVE_UP_BUDGET).expect("a stopped lane returns");
}

/// **A 3xx on the credentialed transactions-stream open surfaces as a response and the `Location`
/// is never dialled.** ureq strips the Bearer token on a redirect, so this is defence in depth: no
/// redirect is followed from a credentialed open at all.
///
/// MUTATION PROOF: delete `.max_redirects(0)` from `transactions_agent` — the 302 is followed.
#[test]
fn the_transactions_agent_never_follows_a_redirect() {
    let (base, target_hits) = redirecting_origin();
    let resp = transactions_agent(venue_timings::OANDA.exec())
        .get(&format!("{base}/v3/accounts/101-004-1234567-001/transactions/stream"))
        .header("Authorization", "Bearer test-token")
        .call();
    assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "the target was dialled");
    assert_eq!(resp.expect("a 302 is a response").status().as_u16(), 302);
}

// --- a failed open names its cause: the timeout is not "a transport error" -----------------

/// **Every failed stream open is described by what failed, for both chunked-GET lanes** (the pricing
/// stream calls this helper with the market row, this lane with the exec row). A TIMEOUT used to share
/// the generic "transport error on stream open" with a reset or a TLS failure, which is how a stream
/// host that gave no response head hid behind that line for a whole evening. The number named is the
/// lane's configured bound (read off the row, so a retuned row cannot leave the text stale).
///
/// MUTATION PROOF: delete the `Timeout` arms of `describe_dial_error` — the head-wait and connect
/// assertions fall back to "transport error on stream open".
#[test]
fn a_failed_open_is_described_by_its_cause_on_both_lanes() {
    for lane in [venue_timings::OANDA.exec(), venue_timings::OANDA.market()] {
        let head = format!(
            "no response head within {} s (the OANDA stream host did not answer)",
            lane.response_timeout().as_secs_f64()
        );
        let dial = format!(
            "no TCP connection within {} s (the OANDA stream host did not accept)",
            lane.dial_timeout().as_secs_f64()
        );
        let said = |e| describe_dial_error(e, lane);
        // `timeout_send_request` is what both agents set, and ureq reports a silent host as it.
        assert_eq!(said(ureq::Error::Timeout(ureq::Timeout::SendRequest)), head);
        assert_eq!(said(ureq::Error::Timeout(ureq::Timeout::RecvResponse)), head);
        assert_eq!(said(ureq::Error::Timeout(ureq::Timeout::Connect)), dial);
        // Any other bound still says it WAS a timeout, and which.
        assert_eq!(
            said(ureq::Error::Timeout(ureq::Timeout::RecvBody)),
            "receive body timeout on stream open"
        );
        // The status arm and the generic arm are unchanged.
        assert_eq!(
            said(ureq::Error::StatusCode(403)),
            "HTTP 403 on stream open (401/403: bad or expired token / wrong account)"
        );
        assert_eq!(said(ureq::Error::HostNotFound), "transport error on stream open");
    }
}

/// The REAL error a silent host produces maps to the head-wait sentence, not to the generic line:
/// a loopback host that accepts and then says nothing, dialled by the agents' own shape (the head
/// wait on `timeout_send_request`) with a short bound so the test is quick. The sentence names the
/// exec row's bound, which is the lane the helper was handed.
#[test]
fn a_host_that_accepts_and_never_answers_reads_as_no_response_head() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (release, held) = mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (_socket, _) = listener.accept().expect("accept the dial");
        let _ = held.recv(); // hold the connection open and say nothing until the test is done
    });
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_send_request(Some(Duration::from_millis(200)))
        .build()
        .new_agent();
    let err = agent.get(&format!("http://{addr}/v3/accounts/a/pricing/stream")).call();
    drop(release);
    server.join().expect("the silent host exits once released");

    let err = err.expect_err("a host that never answers gives no response");
    assert!(matches!(err, ureq::Error::Timeout(_)), "expected a timeout, got {err:?}");
    let exec = venue_timings::OANDA.exec();
    assert_eq!(
        describe_dial_error(err, exec),
        format!(
            "no response head within {} s (the OANDA stream host did not answer)",
            exec.response_timeout().as_secs_f64()
        )
    );
}
