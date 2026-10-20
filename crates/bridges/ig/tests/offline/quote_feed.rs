//! The quote lane over a SCRIPTED LIGHTSTREAMER DOUBLE (`ws://127.0.0.1`, never IG), end to end through the
//! real `Feeds`: the REST login is `fake_ig`, the TLCP server is a plain `TcpListener` + `tungstenite::accept`
//! answering `create_session` with a `CONOK` and the one `control` with a script.
//!
//! What it pins, all measured live on the demo gateway on 2026-10-19: IG no longer serves `MARKET:{epic}`
//! (every ask is `REQERR,1,21,Invalid group`), and serves `PRICE:{accountId}:{epic}` from the `Pricing` data
//! adapter. The bytes below are the production subscribe frame, so a regression to the dead item, or to the
//! wrong adapter (which IG ACKs and then leaves silent), fails here. The refusal half pins that a REQERR is
//! not retried at the shared pump's fixed 3 s for ever, and that the operator is TOLD (a `GapStart` on the
//! feed's stream-status channel, once, and the status line).

use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tungstenite::Message;
use tungstenite::handshake::server::{Request, Response};
use tungstenite::http::HeaderValue;
use vike_data::{DataClient, LiveDataSink};
use vike_ig::lightstreamer::LS_WS_SUBPROTOCOL;
use vike_ig::market_feed::Feeds;
use vike_marketdata::{Bar, QuoteTick};
use vike_model::StreamStatus;

use crate::fake_ig::{self, FakeIg, Route};

const EPIC: &str = "CS.D.EURGBP.MINI.IP";
const TICK: Duration = Duration::from_millis(50);

/// The production subscribe frame for `EPIC` on the account `ACC1` that `fake_ig::login_route` names.
const PRICE_CONTROL: &str = "control\r\nLS_reqId=1&LS_op=add&LS_subId=1&LS_data_adapter=Pricing\
    &LS_group=PRICE%3AACC1%3ACS.D.EURGBP.MINI.IP\
    &LS_schema=BIDPRICE1%20ASKPRICE1%20TIMESTAMP%20DLG_FLAG&LS_mode=MERGE&LS_snapshot=true";

/// A TLCP server that accepts WS sessions and answers the `control` with `reply`, line by line.
struct FakeLs {
    port: u16,
    sessions: Arc<AtomicUsize>,
    controls: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl FakeLs {
    fn controls(&self) -> Vec<String> {
        self.controls.lock().unwrap().clone()
    }
}

impl Drop for FakeLs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve_ls(reply: &'static [&'static str]) -> FakeLs {
    serve_script(vec![reply])
}

/// [`serve_ls`] with one reply per accepted session: the Nth connection gets `script[N]`, and the last
/// reply repeats for every connection after it.
fn serve_script(script: Vec<&'static [&'static str]>) -> FakeLs {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let port = listener.local_addr().expect("local addr").port();
    let sessions = Arc::new(AtomicUsize::new(0));
    let controls = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (count, seen, halt) = (sessions.clone(), controls.clone(), stop.clone());
    std::thread::spawn(move || {
        while !halt.load(Ordering::Relaxed) {
            let Ok((stream, _)) = listener.accept() else {
                std::thread::sleep(TICK);
                continue;
            };
            let nth = count.fetch_add(1, Ordering::Relaxed);
            let reply = script[nth.min(script.len() - 1)];
            let (seen, halt) = (seen.clone(), halt.clone());
            std::thread::spawn(move || session(stream, reply, &seen, &halt));
        }
    });
    FakeLs { port, sessions, controls, stop }
}

/// One accepted connection: `CONOK` for the `create_session`, then `reply` for the `control`, then silence
/// until the client leaves.
// tungstenite's `Callback` fixes the closure's `Err` type (`ErrorResponse`, an HTTP response), which
// nothing here can box.
#[expect(clippy::result_large_err)]
fn session(stream: TcpStream, reply: &[&str], controls: &Mutex<Vec<String>>, stop: &AtomicBool) {
    stream.set_nonblocking(false).expect("blocking accepted stream");
    // The client asks for the TLCP subprotocol, and the real server confirms it.
    let accept = |_: &Request, mut resp: Response| {
        resp.headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static(LS_WS_SUBPROTOCOL));
        Ok(resp)
    };
    let Ok(mut ws) = tungstenite::accept_hdr(stream, accept) else { return };
    let _ = ws.get_ref().set_read_timeout(Some(TICK));
    let mut step = 0;
    while !stop.load(Ordering::Relaxed) {
        let text = match ws.read() {
            Ok(Message::Text(t)) => t.to_string(),
            Ok(_) => continue,
            Err(tungstenite::Error::Io(ref e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                continue;
            }
            Err(_) => return, // the client hung up
        };
        step += 1;
        if step == 1 {
            // A reply that opens with a `CONERR` refuses the SESSION itself, in answer to `create_session`.
            if let Some(conerr) = reply.first().filter(|l| l.starts_with("CONERR")) {
                let _ = ws.send(Message::Text((*conerr).into()));
                return;
            }
            let _ = ws.send(Message::Text("CONOK,S1,50000,5000,*\r\nSERVNAME,fake".into()));
        } else if step == 2 {
            controls.lock().unwrap().push(text);
            for line in reply {
                let _ = ws.send(Message::Text((*line).into()));
            }
        }
    }
}

/// A `fake_ig` whose login hands out the double's Lightstreamer endpoint.
fn gateway(ls: &FakeLs) -> FakeIg {
    fake_ig::serve(vec![Route {
        key: "POST /session",
        status: 200,
        body: json!({
            "currentAccountId": "ACC1",
            "lightstreamerEndpoint": format!("http://127.0.0.1:{}", ls.port),
        })
        .to_string(),
        headers: &[("CST", "cst-1"), ("X-SECURITY-TOKEN", "xst-1")],
        delay: Duration::ZERO,
    }])
}

#[derive(Default)]
struct Sink {
    quotes: Mutex<Vec<(f64, f64)>>,
    statuses: Mutex<Vec<(String, StreamStatus)>>,
}

impl Sink {
    fn quotes(&self) -> Vec<(f64, f64)> {
        self.quotes.lock().unwrap().clone()
    }
    fn statuses(&self) -> Vec<(String, StreamStatus)> {
        self.statuses.lock().unwrap().clone()
    }
}

impl LiveDataSink for Sink {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _bars: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, venue: &str, symbol: &str, q: QuoteTick) {
        assert_eq!((venue, symbol), ("ig", EPIC), "a quote is filed under the venue and the epic");
        self.quotes.lock().unwrap().push((q.bid, q.ask));
    }
    fn trade(&self, _v: &str, _s: &str, _t: vike_marketdata::TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<vike_marketdata::L2Book>) {}
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        assert_eq!((venue, symbol), ("ig", EPIC));
        self.statuses.lock().unwrap().push((stream.to_string(), status));
    }
}

/// Poll `ready` every 20 ms for at most `within`; whether it became true.
fn wait_for(within: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    ready()
}

/// IG answers the quote subscription `REQERR,1,21,Invalid group` (what `MARKET:{epic}` has answered since the
/// decommissioning). Retrying the same bytes cannot cure that, so the lane must (1) ask for the item IG
/// serves, (2) not re-dial at the pump's fixed 3 s, and (3) say so once.
#[test]
fn a_refused_quote_subscription_is_disclosed_once_and_not_redialed_every_three_seconds() {
    let ls = serve_ls(&["REQERR,1,21,Invalid group"]);
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config());
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    // Every property is checked and reported together, so one run names everything that is broken.
    let mut broken = Vec::new();
    assert!(wait_for(Duration::from_secs(10), || !ls.controls().is_empty()), "no subscribe sent");
    if ls.controls()[0] != PRICE_CONTROL {
        broken.push(format!("not the production subscribe frame: {:?}", ls.controls()[0]));
    }

    // The operator is told, on the channel the other feeds use, even though the lane was never live.
    if !wait_for(Duration::from_secs(3), || !sink.statuses().is_empty()) {
        broken.push(
            "a refused lane discloses nothing: the node cannot tell it from no market".into(),
        );
    }
    let status = feeds.status.lock().unwrap().clone();
    if !(status.contains("refused") && status.contains("21")) {
        broken.push(format!("the status line does not say what IG said: {status}"));
    }

    // The shared pump waits 3 s between dials, so 5 s of quiet means the second dial is not ours to make
    // yet (before the policy the second session opened at ~3 s and the third at ~6 s).
    std::thread::sleep(Duration::from_secs(5));
    let sessions = ls.sessions.load(Ordering::Relaxed);
    let statuses = sink.statuses();
    feeds.unsubscribe(id);
    feeds.shutdown();

    if sessions != 1 {
        broken.push(format!(
            "a refused item was re-dialed at the pump's 3 s: {sessions} sessions in ~5 s"
        ));
    }
    if !sink.quotes().is_empty() {
        broken.push("a refused lane emitted quotes".into());
    }
    let one_gap =
        matches!(statuses.as_slice(), [(s, StreamStatus::GapStart { .. })] if s == "quotes");
    if !one_gap {
        broken.push(format!("expected ONE quotes GapStart, one per state change: {statuses:?}"));
    }
    assert!(broken.is_empty(), "{broken:#?}");
}

/// A served `PRICE` item: the same `sink.quote` (bid, ask) a `MARKET` item produced, the venue-padded
/// `DLG_FLAG` (`"DEAL         "`) read trimmed, and the lane going live exactly once.
#[test]
fn a_served_price_item_streams_quotes_and_goes_live() {
    let ls = serve_ls(&[
        "REQOK,1",
        "SUBOK,1,1,4",
        "U,1,1,0.84279|0.84288|1792433954526|DEAL         ",
        "U,1,1,||1792433956074|",
        "U,1,1,0.84280|0.84289|1792433958247|",
    ]);
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config());
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    assert!(wait_for(Duration::from_secs(10), || sink.quotes().len() == 3), "{:?}", sink.quotes());
    assert_eq!(ls.controls()[0], PRICE_CONTROL);
    assert_eq!(sink.quotes(), vec![(0.84279, 0.84288), (0.84279, 0.84288), (0.84280, 0.84289)]);
    let status = feeds.status.lock().unwrap().clone();
    assert_eq!(status, format!("IG live ({EPIC}@quotes)"));
    let statuses = sink.statuses();
    feeds.unsubscribe(id);
    feeds.shutdown();
    assert_eq!(statuses.len(), 1, "{statuses:?}");
    assert!(matches!(statuses[0].1, StreamStatus::Live { gap_started_ts_ms: None }));
}

/// A SUBOK is not data: IG ACKs `PRICE` on the wrong adapter and then sends nothing but `PROBE`, which the
/// shared pump takes for a healthy lane (its ack watchdog is satisfied by the SUBOK, its idle watchdog by
/// the `PROBE`s). A served item sends its snapshot behind the SUBOK, so a lane with no update at all after
/// the bound is DISCLOSED: a status line that says so, and one `GapStart` (the dropped-lane channel).
#[test]
fn a_lane_that_is_acknowledged_and_then_silent_is_disclosed_once() {
    let ls = serve_ls(&["REQOK,1", "SUBOK,1,1,4", "PROBE"]);
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config())
        .with_test_timing(
            Duration::from_millis(400),
            Duration::from_millis(50),
            Duration::from_secs(600),
        );
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    let mut broken = Vec::new();
    if !wait_for(Duration::from_secs(10), || !sink.statuses().is_empty()) {
        broken
            .push("an acknowledged lane that never sends an update discloses nothing".to_string());
    }
    // a second of further silence must not repeat the disclosure
    std::thread::sleep(Duration::from_secs(1));
    let status = feeds.status.lock().unwrap().clone();
    let statuses = sink.statuses();
    feeds.unsubscribe(id);
    feeds.shutdown();

    if !status.contains(vike_ig::market_feed::SILENT_STATUS) {
        broken.push(format!("the status line does not call the lane silent: {status}"));
    }
    let one_gap =
        matches!(statuses.as_slice(), [(s, StreamStatus::GapStart { .. })] if s == "quotes");
    if !one_gap {
        broken.push(format!("expected ONE quotes GapStart: {statuses:?}"));
    }
    assert!(broken.is_empty(), "{broken:#?}");
}

/// Refuse -> accept: IG refuses the subscribe once, then serves it. The refusal was disclosed as a gap and the
/// first quote closes it (`Live` echoing the gap's start), the status line goes live, and the lane dialed
/// exactly twice. (The pump's own 3 s redial is a shared row: this test costs about that.)
#[test]
fn a_refused_lane_that_is_then_served_closes_its_gap_and_goes_live() {
    let ls = serve_script(vec![
        &["REQERR,1,21,Invalid group"][..],
        &["REQOK,1", "SUBOK,1,1,4", "U,1,1,0.84279|0.84288|1792433954526|DEAL         "][..],
    ]);
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config())
        .with_test_timing(
            Duration::from_secs(60),
            Duration::from_millis(50),
            Duration::from_secs(600),
        );
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    // the `Live` status follows the status line's write, so waiting for it reads a settled line
    assert!(wait_for(Duration::from_secs(15), || sink.statuses().len() == 2), "never recovered");
    let status = feeds.status.lock().unwrap().clone();
    let (statuses, sessions) = (sink.statuses(), ls.sessions.load(Ordering::Relaxed));
    feeds.unsubscribe(id);
    feeds.shutdown();

    assert_eq!(sessions, 2, "one refusal, one served session");
    assert_eq!(status, format!("IG live ({EPIC}@quotes)"));
    let gap_at = match statuses.as_slice() {
        [
            (_, StreamStatus::GapStart { at_ts_ms }),
            (_, StreamStatus::Live { gap_started_ts_ms }),
        ] => {
            assert_eq!(
                *gap_started_ts_ms,
                Some(*at_ts_ms),
                "the recovery closes the gap it opened"
            );
            *at_ts_ms
        }
        other => panic!("expected GapStart then Live: {other:?}"),
    };
    assert!(gap_at > 0);
}

/// IG refuses the Lightstreamer SESSION (`CONERR`), every time. The shared pump redials at a fixed 3 s and the
/// lane used to throw its cached login away on every one, so a stuck lane spent a `POST /session` AND a session
/// every ~3 s for ever. Now the first CONERR is a free try (the common cause is a token that expired) and drops
/// the login once; the second asks for the stretched wait, and keeps the login it has: a stuck lane costs a
/// WebSocket handshake, not a login, and not every 3 s.
#[test]
fn a_lane_stuck_on_conerr_is_redialed_with_a_stretched_wait_and_logs_in_only_once_more() {
    let ls = serve_ls(&["CONERR,1,Wrong credentials"]);
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config())
        .with_test_timing(
            Duration::from_secs(60),
            Duration::from_millis(2500),
            Duration::from_secs(600),
        );
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    // the first CONERR is a free try: the pump's 3 s later, the second dial
    assert!(wait_for(Duration::from_secs(10), || ls.sessions.load(Ordering::Relaxed) >= 2));
    // The pump alone would dial again 3 s after that one (the lane used to); the stretched wait (2.5 s on
    // top of the pump's 3 s) says not yet.
    std::thread::sleep(Duration::from_secs(4));
    let (sessions, logins) = (ls.sessions.load(Ordering::Relaxed), ig.count("POST /session"));
    let (status, statuses) = (feeds.status.lock().unwrap().clone(), sink.statuses());
    feeds.unsubscribe(id);
    feeds.shutdown();

    let mut broken = Vec::new();
    if sessions != 2 {
        broken.push(format!("a CONERR was redialed at the pump's 3 s: {sessions} sessions"));
    }
    if logins != 2 {
        broken.push(format!(
            "expected the first login and ONE fresh one, got {logins} POST /session"
        ));
    }
    if !status.contains("CONERR") {
        broken.push(format!("the status line does not say what IG said: {status}"));
    }
    let one_gap =
        matches!(statuses.as_slice(), [(s, StreamStatus::GapStart { .. })] if s == "quotes");
    if !one_gap {
        broken.push(format!(
            "expected ONE quotes GapStart for a lane that never was live: {statuses:?}"
        ));
    }
    assert!(broken.is_empty(), "{broken:#?}");
}

/// IG answers the LOGIN with a credentials error (`401 error.security.invalid-details`: a bad or rotated
/// password; the documented lockout is `too-many-failed-attempts`). The same credentials get the same
/// answer, so the lane used to redial a failing `POST /session` every 3 s for ever, into the lockout. It
/// waits the longest wait before it tries again, and says so.
#[test]
fn a_login_refused_for_its_credentials_backs_off_instead_of_retrying_every_three_seconds() {
    let ig = fake_ig::serve(vec![Route::status(
        "POST /session",
        401,
        json!({ "errorCode": "error.security.invalid-details" }),
    )]);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config())
        .with_test_timing(
            Duration::from_secs(60),
            Duration::from_millis(50),
            Duration::from_secs(4),
        );
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    assert!(wait_for(Duration::from_secs(10), || ig.count("POST /session") >= 1));
    // The pump alone would log in again 3 s after the first; the wait (4 s on top of the pump's 3 s) says not yet.
    std::thread::sleep(Duration::from_secs(5));
    let logins = ig.count("POST /session");
    let (status, statuses) = (feeds.status.lock().unwrap().clone(), sink.statuses());
    feeds.unsubscribe(id);
    feeds.shutdown();

    let mut broken = Vec::new();
    if logins != 1 {
        broken.push(format!(
            "a refused login was retried at the pump's 3 s: {logins} POST /session in ~5 s"
        ));
    }
    if !(status.contains("IG login failed") && status.contains("error.security.invalid-details")) {
        broken
            .push(format!("the status line does not say the login was refused, and why: {status}"));
    }
    let one_gap =
        matches!(statuses.as_slice(), [(s, StreamStatus::GapStart { .. })] if s == "quotes");
    if !one_gap {
        broken.push(format!("expected ONE quotes GapStart: {statuses:?}"));
    }
    assert!(broken.is_empty(), "{broken:#?}");
}

/// CONERR, then success: the lane that was refused once is served on its next dial (the first CONERR is a free
/// try, so the pump's 3 s is all it waits), spends exactly one fresh login on it, closes the gap it opened and
/// goes live.
#[test]
fn a_lane_that_conerrs_once_and_is_then_served_closes_its_gap_and_goes_live() {
    let ls = serve_script(vec![
        &["CONERR,1,Wrong credentials"][..],
        &["REQOK,1", "SUBOK,1,1,4", "U,1,1,0.84279|0.84288|1792433954526|DEAL         "][..],
    ]);
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config())
        .with_test_timing(
            Duration::from_secs(60),
            Duration::from_millis(50),
            Duration::from_secs(600),
        );
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    assert!(wait_for(Duration::from_secs(15), || sink.statuses().len() == 2), "never recovered");
    let status = feeds.status.lock().unwrap().clone();
    let (statuses, sessions, logins) =
        (sink.statuses(), ls.sessions.load(Ordering::Relaxed), ig.count("POST /session"));
    feeds.unsubscribe(id);
    feeds.shutdown();

    assert_eq!(
        (sessions, logins),
        (2, 2),
        "one refused session, one served; the first login and one fresh"
    );
    assert_eq!(status, format!("IG live ({EPIC}@quotes)"));
    match statuses.as_slice() {
        [
            (_, StreamStatus::GapStart { at_ts_ms }),
            (_, StreamStatus::Live { gap_started_ts_ms }),
        ] => {
            assert_eq!(
                *gap_started_ts_ms,
                Some(*at_ts_ms),
                "the recovery closes the gap it opened"
            );
        }
        other => panic!("expected GapStart then Live: {other:?}"),
    }
}
