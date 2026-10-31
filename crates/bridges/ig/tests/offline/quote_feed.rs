//! The quote lane over a SCRIPTED LIGHTSTREAMER DOUBLE (`ws://127.0.0.1`, never IG), end to end through the
//! real `Feeds`: the REST login is `fake_ig`, the TLCP server is a plain `TcpListener` + `tungstenite::accept`
//! answering `create_session` with a `CONOK` and every `control` `add` with a script.
//!
//! What it pins, all measured live on the demo gateway on 2026-10-19: IG no longer serves `MARKET:{epic}`
//! (every ask is `REQERR,1,21,Invalid group`), and serves `PRICE:{accountId}:{epic}` from the `Pricing` data
//! adapter. The bytes below are the production subscribe frame, so a regression to the dead item, or to the
//! wrong adapter (which IG ACKs and then leaves silent), fails here. The refusal half pins that a REQERR is
//! not retried at the shared pump's fixed 3 s for ever, and that the operator is TOLD (a `GapStart` on the
//! feed's stream-status channel, once, and the status line).
//!
//! The last half pins the one-login, one-socket design (`crates/bridges/ig/src/ls_link.rs`): every lane of a
//! `Feeds` rides ONE Lightstreamer session, so a refused or stalled subscription is a `delete` and a new `add`
//! on that session (the double counts `add`s, not only sessions), a connection that ends is dialed once for all
//! its lanes, and the login is kept. The scripts spell subscription `1`; the double rewrites a reply for the id
//! of the `add` it answers, as the real server would have addressed it.

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

/// A TLCP server that accepts WS sessions and answers every `control` `add` with its session's reply.
struct FakeLs {
    port: u16,
    sessions: Arc<AtomicUsize>,
    /// Every `control` any session received, in order (the `add`s and the `delete`s).
    controls: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    /// Bumped by [`FakeLs::end_all`]: every session born before the bump ends with an `END`.
    kill: Arc<AtomicUsize>,
}

impl FakeLs {
    fn controls(&self) -> Vec<String> {
        self.controls.lock().unwrap().clone()
    }

    /// The `control`s that asked for a subscription: a lane asking IG for its item.
    fn adds(&self) -> Vec<String> {
        self.controls().into_iter().filter(|c| c.contains("LS_op=add")).collect()
    }

    /// The `control`s that ended a subscription.
    fn deletes(&self) -> Vec<String> {
        self.controls().into_iter().filter(|c| c.contains("LS_op=delete")).collect()
    }

    /// End every open session the way IG ends one (`END`); sessions accepted afterwards live.
    fn end_all(&self) {
        self.kill.fetch_add(1, Ordering::Relaxed);
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
/// reply repeats for every connection after it. A reply may hold SEVERAL answers, split by a `"--"` line: the
/// Nth `add` of a session gets the Nth answer, and the last answer repeats (a lane IG refused asks again on
/// the same session, where it used to dial a new one).
fn serve_script(script: Vec<&'static [&'static str]>) -> FakeLs {
    serve_script_tail(script, None)
}

/// A line the double repeats on every session after its reply, until the client leaves: the server's
/// keepalive `PROBE`, which IG sends while it has no data (CONOK announced 5000 ms; the tests send it far
/// faster than the pump's 2 s read poll, so no read ever times out and the pump's `on_tick` never runs).
type Tail = Option<(&'static str, Duration)>;

/// [`serve_script`] whose every session keeps sending `tail` after its reply.
fn serve_script_tail(script: Vec<&'static [&'static str]>, tail: Tail) -> FakeLs {
    serve_plans(script.into_iter().map(|reply| Plan { tail, ..Plan::replying(reply) }).collect())
}

/// Items by group, and an update for every live subscription each `ticks`: the double for several lanes on one
/// session. An `add` whose group holds a key of `items` is answered with that item's reply.
fn serve_items(items: Items, ticks: Duration) -> FakeLs {
    serve_plans(vec![Plan {
        items,
        ticks: Some(ticks),
        ..Plan::replying(&["REQOK,1", "SUBOK,1,1,4"])
    }])
}

/// Replies by item: `(a part of the `LS_group`, the reply lines)`; the first key the group holds answers.
type Items = &'static [(&'static str, &'static [&'static str])];

/// What one accepted session does.
#[derive(Clone, Copy)]
struct Plan {
    /// The answers to its `add`s (split by `"--"`, the last repeating); a first line that is a `CONERR` refuses
    /// the SESSION instead, in answer to `create_session`.
    reply: &'static [&'static str],
    /// Repeated after the first `add`, until the client leaves.
    tail: Tail,
    /// An `add` whose group holds one of these keys is answered by that entry instead of `reply`.
    items: Items,
    /// A `U,<sub>,1,||<ts>|` (an unchanged quote) for every subscription whose `add` was answered with a
    /// snapshot, at this cadence.
    ticks: Option<Duration>,
}

impl Plan {
    fn replying(reply: &'static [&'static str]) -> Self {
        Plan { reply, tail: None, items: &[], ticks: None }
    }
}

fn serve_plans(plans: Vec<Plan>) -> FakeLs {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let port = listener.local_addr().expect("local addr").port();
    let sessions = Arc::new(AtomicUsize::new(0));
    let controls = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let kill = Arc::new(AtomicUsize::new(0));
    let (count, seen, halt, ended) =
        (sessions.clone(), controls.clone(), stop.clone(), kill.clone());
    std::thread::spawn(move || {
        while !halt.load(Ordering::Relaxed) {
            let Ok((stream, _)) = listener.accept() else {
                std::thread::sleep(TICK);
                continue;
            };
            let nth = count.fetch_add(1, Ordering::Relaxed);
            let plan = plans[nth.min(plans.len() - 1)];
            let (seen, halt, ended) = (seen.clone(), halt.clone(), ended.clone());
            let born = ended.load(Ordering::Relaxed);
            std::thread::spawn(move || session(stream, plan, &seen, &halt, (&*ended, born)));
        }
    });
    FakeLs { port, sessions, controls, stop, kill }
}

/// The value of `key` in a `control` request's form body.
fn field<'a>(control: &'a str, key: &str) -> Option<&'a str> {
    control.split(['&', '\n']).find_map(|kv| kv.trim_end().strip_prefix(key)?.strip_prefix('='))
}

/// A scripted reply line as the lane that asked reads it: the scripts spell subscription `1` (`SUBOK,1,..`,
/// `U,1,..`), and a connection numbers its subscriptions 1, 2, 3 ..., so the line is rewritten for the `sub`
/// of the `add` it answers (which is also that `add`'s request id).
fn addressed(line: &str, sub: u32) -> String {
    let mut parts = line.splitn(3, ',');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(tag @ ("U" | "SUBOK" | "REQOK" | "REQERR" | "EOS")), Some("1"), Some(rest)) => {
            format!("{tag},{sub},{rest}")
        }
        (Some(tag @ ("REQOK" | "EOS")), Some("1"), None) => format!("{tag},{sub}"),
        _ => line.to_string(),
    }
}

/// One accepted connection: `CONOK` for the `create_session`, then the plan's answer to every `add` (and
/// `REQOK` + `UNSUB` for a `delete`), the tail and the ticks, until the client leaves or is ended.
// tungstenite's `Callback` fixes the closure's `Err` type (`ErrorResponse`, an HTTP response), which
// nothing here can box.
#[expect(clippy::result_large_err)]
fn session(
    stream: TcpStream,
    plan: Plan,
    controls: &Mutex<Vec<String>>,
    stop: &AtomicBool,
    kill: (&AtomicUsize, usize),
) {
    stream.set_nonblocking(false).expect("blocking accepted stream");
    // The client asks for the TLCP subprotocol, and the real server confirms it.
    let accept = |_: &Request, mut resp: Response| {
        resp.headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static(LS_WS_SUBPROTOCOL));
        Ok(resp)
    };
    let Ok(mut ws) = tungstenite::accept_hdr(stream, accept) else { return };
    let _ = ws.get_ref().set_read_timeout(Some(TICK));
    let answers: Vec<&[&str]> = plan.reply.split(|line| *line == "--").collect();
    let mut step = 0;
    let (mut last_tail, mut last_tick) = (Instant::now(), Instant::now());
    let (mut adds, mut live) = (0_usize, Vec::<u32>::new());
    while !stop.load(Ordering::Relaxed) {
        if kill.0.load(Ordering::Relaxed) != kill.1 {
            let _ = ws.send(Message::Text("END,41,killed".into()));
            return;
        }
        if let Some((line, every)) = plan.tail
            && step >= 2
            && last_tail.elapsed() >= every
        {
            last_tail = Instant::now();
            if ws.send(Message::Text(line.into())).is_err() {
                return; // the client hung up
            }
        }
        if let Some(every) = plan.ticks
            && last_tick.elapsed() >= every
        {
            last_tick = Instant::now();
            for sub in &live {
                if ws.send(Message::Text(format!("U,{sub},1,||1792433960000|").into())).is_err() {
                    return; // the client hung up
                }
            }
        }
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
            if let Some(conerr) = plan.reply.first().filter(|l| l.starts_with("CONERR")) {
                let _ = ws.send(Message::Text((*conerr).into()));
                return;
            }
            let _ = ws.send(Message::Text("CONOK,S1,50000,5000,*\r\nSERVNAME,fake".into()));
            continue;
        }
        controls.lock().unwrap().push(text.clone());
        let sub: u32 = field(&text, "LS_subId").and_then(|s| s.parse().ok()).unwrap_or(1);
        match field(&text, "LS_op") {
            Some("add") => {
                let group = field(&text, "LS_group").unwrap_or_default();
                let by_item = plan.items.iter().find(|(key, _)| group.contains(*key));
                let lines =
                    by_item.map_or(answers[adds.min(answers.len() - 1)], |(_, lines)| *lines);
                adds += 1;
                for line in lines {
                    let _ = ws.send(Message::Text(addressed(line, sub).into()));
                }
                // Only an item that delivered a snapshot goes on ticking (a refused or a mute one does not).
                if lines.iter().any(|line| line.starts_with("U,")) {
                    live.push(sub);
                }
            }
            Some("delete") => {
                live.retain(|s| *s != sub);
                let request = field(&text, "LS_reqId").unwrap_or("0");
                let _ = ws.send(Message::Text(format!("REQOK,{request}").into()));
                let _ = ws.send(Message::Text(format!("UNSUB,{sub}").into()));
            }
            _ => {}
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
    let (sessions, adds) = (ls.sessions.load(Ordering::Relaxed), ls.adds().len());
    let statuses = sink.statuses();
    feeds.unsubscribe(id);
    feeds.shutdown();

    // One session however often the lane asks: a refused item is a new `add` on the shared session, not a dial.
    if (sessions, adds) != (1, 1) {
        broken.push(format!(
            "a refused item was re-asked at the pump's 3 s: {sessions} sessions and {adds} adds in ~5 s"
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
/// first quote closes it (`Live` echoing the gap's start), the status line goes live, and the lane asked
/// exactly twice, on ONE session (a refused subscription is a `delete` and a new `add`, no new socket).
/// (The pump's own 3 s redial is a shared row: this test costs about that.)
#[test]
fn a_refused_lane_that_is_then_served_closes_its_gap_and_goes_live() {
    let ls = serve_ls(&[
        "REQERR,1,21,Invalid group",
        "--",
        "REQOK,1",
        "SUBOK,1,1,4",
        "U,1,1,0.84279|0.84288|1792433954526|DEAL         ",
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
    let (statuses, sessions, adds) =
        (sink.statuses(), ls.sessions.load(Ordering::Relaxed), ls.adds().len());
    feeds.unsubscribe(id);
    feeds.shutdown();

    assert_eq!((sessions, adds), (1, 2), "one refusal, one served add, on one session");
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

/// The snapshot a (re)subscribe delivers behind the SUBOK, and a live update after it.
const SNAPSHOT: &str = "U,1,1,0.84279|0.84288|1792433954526|DEAL         ";
const LIVE_UPDATE: &str = "U,1,1,0.84280|0.84289|1792433955526|";

/// IG keeps sending `PROBE` (its keepalive, which satisfies the shared pump's 60 s idle watchdog) and stops
/// sending updates. The lane used to stay `LIVE · IG` with no quotes, no reconnect and no disclosure; the
/// price tap aged the price out, and nothing told the operator. Now a quote lane whose updates have been
/// flowing and then stop, while its `DLG_FLAG` says the market is open, is called stalled, DISCLOSED (a
/// status line, a `GapStart`, the `warn` of the session-error arm) and redialed for a fresh snapshot, which
/// closes the gap.
///
/// ⚠ The `PROBE`s here come every 100 ms, far faster than the pump's 2 s read poll: no read ever times out,
/// so the pump's dataless-tick hook (`on_tick`) NEVER runs. A watch that lived there would pass every test
/// that goes quiet and fail this one; the check rides the frame itself.
#[test]
fn a_lane_whose_updates_stop_while_probes_keep_coming_is_disclosed_and_redialed() {
    let ls = serve_script_tail(
        vec![&["REQOK,1", "SUBOK,1,1,4", SNAPSHOT, LIVE_UPDATE][..]],
        Some(("PROBE", Duration::from_millis(100))),
    );
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config())
        .with_test_watch_timing(Duration::from_millis(600), Duration::from_secs(1800));
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    // Live, then (about 0.7 s after the last update) the stall is disclosed on the dropped-lane channel, and the
    // status line says what happened (it is written before the GapStart is sent, and stands until the redial's
    // first quote about 3 s later: seen on EVERY poll, so a descheduled test thread cannot miss it)
    let mut said_quiet = false;
    assert!(
        wait_for(Duration::from_secs(10), || {
            said_quiet |= feeds.status.lock().unwrap().contains("quiet");
            sink.statuses().len() >= 2
        }),
        "a lane that goes quiet behind its PROBEs discloses nothing: {:?}",
        sink.statuses()
    );
    // ... then the redial's snapshot closes the gap, and its second update follows
    let recovered = wait_for(Duration::from_secs(10), || {
        said_quiet |= feeds.status.lock().unwrap().contains("quiet");
        sink.statuses().len() >= 3 && sink.quotes().len() >= 4
    });
    let (statuses, sessions, adds, quotes) = (
        sink.statuses(),
        ls.sessions.load(Ordering::Relaxed),
        ls.adds().len(),
        sink.quotes().len(),
    );
    feeds.unsubscribe(id);
    feeds.shutdown();

    let mut broken = Vec::new();
    if !said_quiet {
        broken.push("the status line never said the quote stream went quiet".to_string());
    }
    if !recovered {
        broken.push(format!("the redial never closed the gap: {statuses:?}"));
    }
    if adds < 2 {
        broken.push(format!("a stalled lane was not resubscribed: {adds} add(s)"));
    }
    // A stalled epic costs a `delete` and a new `add` on the shared session: not a socket (the whole point of
    // the one-socket link is that one epic's stall does not tear down the others).
    if sessions != 1 {
        broken.push(format!("a stalled lane redialed the socket: {sessions} sessions"));
    }
    if quotes < 4 {
        broken.push(format!(
            "expected the snapshot and one update from each of two sessions: {quotes}"
        ));
    }
    match statuses.as_slice() {
        [
            (_, StreamStatus::Live { gap_started_ts_ms: None }),
            (_, StreamStatus::GapStart { at_ts_ms }),
            (_, StreamStatus::Live { gap_started_ts_ms }),
            ..,
        ] => {
            if *gap_started_ts_ms != Some(*at_ts_ms) {
                broken.push(format!("the recovery does not close the gap it opened: {statuses:?}"));
            }
        }
        other => broken.push(format!("expected Live, GapStart, Live: {other:?}")),
    }
    assert!(broken.is_empty(), "{broken:#?}");
}

/// A market the flag says is CLOSED legitimately sends nothing for hours (the shared pump's own row says
/// so): nothing but `PROBE` behind a `CLOSED` snapshot is not a stall, and the lane is neither redialed nor
/// disclosed. (The guard on the other side of the test above: a watch that fires here would redial a closed
/// market every bound for ever.)
#[test]
fn a_closed_market_that_sends_only_probes_is_not_redialed() {
    let ls = serve_script_tail(
        vec![&["REQOK,1", "SUBOK,1,1,4", "U,1,1,0.84279|0.84288|1792433954526|CLOSED       "][..]],
        Some(("PROBE", Duration::from_millis(100))),
    );
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config())
        .with_test_watch_timing(Duration::from_millis(300), Duration::from_secs(1800));
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    assert!(wait_for(Duration::from_secs(10), || !ls.controls().is_empty()), "no subscribe sent");
    // several bounds of PROBEs behind the CLOSED snapshot, and past the pump's 3 s redial
    std::thread::sleep(Duration::from_secs(4));
    let (sessions, statuses, quotes) =
        (ls.sessions.load(Ordering::Relaxed), sink.statuses(), sink.quotes());
    feeds.unsubscribe(id);
    feeds.shutdown();

    assert_eq!(sessions, 1, "a closed market was redialed");
    assert!(statuses.is_empty(), "a closed market was disclosed as a gap: {statuses:?}");
    assert!(quotes.is_empty(), "a CLOSED flag emits no quote");
}

/// A run only ends on a delivered update, so a lane refused for longer than a token lives used to keep an
/// EXPIRED pair after its one drop: every later dial answered `CONERR 1` until the process restarted. A login
/// older than the guard (30 minutes in production, 2 s here) is dropped again by a later `CONERR` of the same
/// run, once, and the replacement is young: `POST /session` stays bounded to one per guard, not one per dial.
#[test]
fn a_lane_stuck_on_conerr_replaces_a_login_that_outlived_the_guard_once() {
    let ls = serve_ls(&["CONERR,1,Wrong credentials"]);
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config())
        .with_test_timing(
            Duration::from_secs(60),
            Duration::from_millis(50),
            Duration::from_secs(600),
        )
        .with_test_watch_timing(Duration::from_secs(60), Duration::from_secs(2));
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    // Dial 1 (t=0): login 1, CONERR, the run's one drop. Dial 2 (~3 s, the pump's redial): login 2, CONERR on a
    // login a moment old: kept. Dial 3 (~6 s): that login is now ~3 s old, past the 2 s guard: its CONERR drops
    // it. Dial 4 (~9 s): login 3. Dial 5 would be ~12 s: its CONERR keeps login 3.
    let dialed_four =
        wait_for(Duration::from_secs(14), || ls.sessions.load(Ordering::Relaxed) >= 4);
    // login 3 was made before dial 4 was accepted; half a second is time enough for a second login to show,
    // and leaves 2.5 s before dial 5 (~3 s after dial 4) for a descheduled test thread
    std::thread::sleep(Duration::from_millis(500));
    let (sessions, logins) = (ls.sessions.load(Ordering::Relaxed), ig.count("POST /session"));
    feeds.unsubscribe(id);
    feeds.shutdown();

    assert!(dialed_four, "the lane stopped dialing: {sessions} sessions");
    assert_eq!(
        (sessions, logins),
        (4, 3),
        "four dials: the first login, the run's one drop, and ONE more once the second login outlived the guard"
    );
}

/// A session that ends for any reason BUT a stall (here an `END`, as the 60 s idle watchdog, a reset or a
/// `LOOP` would) leaves the lane's last update behind, and the redial comes more than the quiet bound later
/// (always: the pump waits 3 s, the bound here is 0.6 s). The new session's first frames (`REQOK`, before its
/// `SUBOK`) used to be judged against that old update: "the quote stream went quiet" on a perfectly good
/// session, one wasted redial and a misleading status after every outage. The second session must be SERVED:
/// it closes the gap the `END` opened, and the lane has dialed exactly twice.
///
/// Every session after the `END` streams an update every 100 ms, so none of them is ever really quiet.
#[test]
fn a_redial_after_an_end_is_not_called_quiet_on_its_first_frames() {
    let ls = serve_script_tail(
        vec![
            &["REQOK,1", "SUBOK,1,1,4", SNAPSHOT, LIVE_UPDATE, "END,31,closed"][..],
            &["REQOK,1", "SUBOK,1,1,4", SNAPSHOT, LIVE_UPDATE][..],
        ],
        Some(("U,1,1,||1792433960000|", Duration::from_millis(100))),
    );
    let ig = gateway(&ls);
    let sink = Arc::new(Sink::default());
    let mut feeds = Feeds::new(sink.clone() as Arc<dyn LiveDataSink>, || {}, ig.config())
        .with_test_timing(
            Duration::from_secs(60),
            Duration::from_millis(50),
            Duration::from_secs(600),
        )
        .with_test_watch_timing(Duration::from_millis(600), Duration::from_secs(1800));
    let id = feeds.subscribe_quotes(EPIC).expect("subscribe_quotes");

    // Live (session 1), the END's GapStart, and the redial's first quote closing it
    let recovered = wait_for(Duration::from_secs(10), || sink.statuses().len() >= 3);
    let (statuses, sessions) = (sink.statuses(), ls.sessions.load(Ordering::Relaxed));
    let status = feeds.status.lock().unwrap().clone();
    feeds.unsubscribe(id);
    feeds.shutdown();

    assert!(recovered, "the redial never closed the gap: {statuses:?}");
    assert_eq!(
        sessions, 2,
        "the second session was killed as quiet before it was served: {statuses:?}"
    );
    assert!(!status.contains("quiet"), "{status}");
    assert!(
        matches!(
            statuses.as_slice(),
            [
                (_, StreamStatus::Live { gap_started_ts_ms: None }),
                (_, StreamStatus::GapStart { .. }),
                (_, StreamStatus::Live { gap_started_ts_ms: Some(_) }),
                ..
            ]
        ),
        "{statuses:?}"
    );
}

// ---- One login and one socket for every lane of a client (`crates/bridges/ig/src/ls_link.rs`) ----

const EPIC2: &str = "CS.D.GBPUSD.MINI.IP";

/// What each item of the shared-session double answers: a snapshot with a price of its own, so a quote that
/// reached the wrong lane is seen. `CHART` comes first: a bar group names the same epic as a quote group.
const ITEMS: Items = &[
    ("CHART", &["REQOK,1", "SUBOK,1,1,7"]),
    ("EURGBP", &["REQOK,1", "SUBOK,1,1,4", "U,1,1,1.1111|1.1113|1792433954526|DEAL         "]),
    ("GBPUSD", &["REQOK,1", "SUBOK,1,1,4", "U,1,1,2.2221|2.2223|1792433954526|DEAL         "]),
];

/// [`ITEMS`] with the first epic refused (`REQERR 21`) and the second served.
const ONE_REFUSED: Items = &[
    ("EURGBP", &["REQERR,1,21,Invalid group"]),
    ("GBPUSD", &["REQOK,1", "SUBOK,1,1,4", "U,1,1,2.2221|2.2223|1792433954526|DEAL         "]),
];

/// A sink that files every quote and status under the symbol it came for (a client's lanes name several).
#[derive(Default)]
struct Tape {
    quotes: Mutex<Vec<(String, f64)>>,
    statuses: Mutex<Vec<(String, StreamStatus)>>,
}

impl Tape {
    fn bids(&self, symbol: &str) -> Vec<f64> {
        let quotes = self.quotes.lock().unwrap();
        quotes.iter().filter(|(s, _)| s == symbol).map(|(_, bid)| *bid).collect()
    }

    fn statuses(&self, symbol: &str) -> Vec<StreamStatus> {
        let all = self.statuses.lock().unwrap();
        all.iter().filter(|(s, _)| s == symbol).map(|(_, status)| *status).collect()
    }
}

impl LiveDataSink for Tape {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _bars: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, venue: &str, symbol: &str, q: QuoteTick) {
        assert_eq!(venue, "ig");
        self.quotes.lock().unwrap().push((symbol.to_string(), q.bid));
    }
    fn trade(&self, _v: &str, _s: &str, _t: vike_marketdata::TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<vike_marketdata::L2Book>) {}
    fn stream_status(&self, venue: &str, symbol: &str, _stream: &str, status: StreamStatus) {
        assert_eq!(venue, "ig");
        self.statuses.lock().unwrap().push((symbol.to_string(), status));
    }
}

/// Three subscriptions of one client (the quotes of two epics and the 1-minute bars of the first) used to be
/// three `POST /session` and three Lightstreamer sockets of the same API key, which IG's FAQ warns may get the
/// key suspended. They share ONE login and ONE session, each under an id of its own, and each reads only its
/// own item's lines.
#[test]
fn the_lanes_of_a_client_share_one_login_and_one_socket() {
    let ls = serve_items(ITEMS, Duration::from_millis(100));
    let ig = gateway(&ls);
    let tape = Arc::new(Tape::default());
    let mut feeds = Feeds::new(tape.clone() as Arc<dyn LiveDataSink>, || {}, ig.config());
    let ids = [
        feeds.subscribe_quotes(EPIC).expect("first epic"),
        feeds.subscribe_quotes(EPIC2).expect("second epic"),
        feeds.subscribe_bars(EPIC, "1m").expect("bars of the first epic"),
    ];

    let streaming = wait_for(Duration::from_secs(10), || {
        tape.bids(EPIC).len() >= 3 && tape.bids(EPIC2).len() >= 3 && ls.adds().len() == 3
    });
    let (logins, sessions) = (ig.count("POST /session"), ls.sessions.load(Ordering::Relaxed));
    let (adds, first, second) = (ls.adds(), tape.bids(EPIC), tape.bids(EPIC2));
    for id in ids {
        feeds.unsubscribe(id);
    }
    feeds.shutdown();

    assert!(streaming, "the three lanes never streamed: {first:?} {second:?} {adds:?}");
    assert_eq!((logins, sessions), (1, 1), "three lanes, one login and one socket");
    let mut subs: Vec<_> = adds.iter().filter_map(|add| field(add, "LS_subId")).collect();
    subs.sort_unstable();
    assert_eq!(subs, ["1", "2", "3"], "each lane has an id of its own on the shared session");
    assert!(
        first.iter().all(|bid| *bid == 1.1111),
        "the first epic read another lane's quote: {first:?}"
    );
    assert!(
        second.iter().all(|bid| *bid == 2.2221),
        "the second epic read another lane's quote: {second:?}"
    );
}

/// One lane ending (an `unsubscribe`) is a `delete` of ITS subscription on the shared session: the socket and
/// the other lane stay.
#[test]
fn ending_one_subscription_deletes_it_and_the_others_keep_streaming_on_the_same_socket() {
    let ls = serve_items(ITEMS, Duration::from_millis(100));
    let ig = gateway(&ls);
    let tape = Arc::new(Tape::default());
    let mut feeds = Feeds::new(tape.clone() as Arc<dyn LiveDataSink>, || {}, ig.config());
    let first = feeds.subscribe_quotes(EPIC).expect("first epic");
    let second = feeds.subscribe_quotes(EPIC2).expect("second epic");
    assert!(
        wait_for(Duration::from_secs(10), || tape.bids(EPIC).len() >= 2
            && tape.bids(EPIC2).len() >= 2),
        "never streaming"
    );

    feeds.unsubscribe(first);
    let deleted = wait_for(Duration::from_secs(5), || ls.deletes().len() == 1);
    let before = tape.bids(EPIC2).len();
    let still_streaming = wait_for(Duration::from_secs(5), || tape.bids(EPIC2).len() >= before + 3);
    let (logins, sessions) = (ig.count("POST /session"), ls.sessions.load(Ordering::Relaxed));
    let (adds, deletes) = (ls.adds(), ls.deletes());
    feeds.unsubscribe(second);
    feeds.shutdown();

    assert!(deleted, "the lane's subscription was never ended on the shared session: {deletes:?}");
    assert!(still_streaming, "the other lane stopped streaming when its neighbour left");
    assert_eq!((logins, sessions), (1, 1), "and the socket and the login stayed");
    let first_sub =
        adds.iter().find(|add| add.contains("EURGBP")).and_then(|add| field(add, "LS_subId"));
    let deleted_sub = deletes.first().and_then(|delete| field(delete, "LS_subId"));
    assert!(first_sub.is_some() && first_sub == deleted_sub, "{first_sub:?} vs {deleted_sub:?}");
}

/// The shared session ends (IG sends `END`): every lane reads it, takes its free first try, and the FIRST to ask
/// dials the new session; the others find it. One new socket however many lanes, and no new login (the kept
/// pair is only dropped by a `CONERR`).
#[test]
fn a_connection_that_ends_is_dialed_once_for_all_its_lanes_and_the_login_is_kept() {
    let ls = serve_items(ITEMS, Duration::from_millis(100));
    let ig = gateway(&ls);
    let tape = Arc::new(Tape::default());
    let mut feeds = Feeds::new(tape.clone() as Arc<dyn LiveDataSink>, || {}, ig.config());
    let ids = [
        feeds.subscribe_quotes(EPIC).expect("first epic"),
        feeds.subscribe_quotes(EPIC2).expect("second epic"),
        feeds.subscribe_bars(EPIC, "1m").expect("bars of the first epic"),
    ];
    assert!(
        wait_for(Duration::from_secs(10), || {
            tape.bids(EPIC).len() >= 2 && tape.bids(EPIC2).len() >= 2 && ls.adds().len() == 3
        }),
        "never streaming"
    );

    ls.end_all();
    // the pump's 3 s redial is a shared row: this costs about that
    let redialed = wait_for(Duration::from_secs(15), || {
        ls.sessions.load(Ordering::Relaxed) >= 2 && ls.adds().len() >= 6
    });
    let (first, second) = (tape.bids(EPIC).len(), tape.bids(EPIC2).len());
    let streaming_again = wait_for(Duration::from_secs(5), || {
        tape.bids(EPIC).len() >= first + 3 && tape.bids(EPIC2).len() >= second + 3
    });
    let (logins, sessions, adds) =
        (ig.count("POST /session"), ls.sessions.load(Ordering::Relaxed), ls.adds().len());
    for id in ids {
        feeds.unsubscribe(id);
    }
    feeds.shutdown();

    assert!(redialed, "the lanes never came back: {sessions} sessions, {adds} adds");
    assert!(streaming_again, "the lanes came back but do not stream");
    assert_eq!(
        (logins, sessions, adds),
        (1, 2, 6),
        "one new socket for three lanes, the login kept"
    );
}

/// IG refuses ONE item (`REQERR 21`). That lane is disclosed and waits out its own ladder, and neither the
/// socket nor the healthy lane beside it is touched: before the link, a refusal cost the lane a whole session
/// per try.
#[test]
fn a_refused_subscription_neither_costs_a_socket_nor_interrupts_the_healthy_lane() {
    let ls = serve_items(ONE_REFUSED, Duration::from_millis(100));
    let ig = gateway(&ls);
    let tape = Arc::new(Tape::default());
    let mut feeds = Feeds::new(tape.clone() as Arc<dyn LiveDataSink>, || {}, ig.config());
    let refused = feeds.subscribe_quotes(EPIC).expect("refused epic");
    let healthy = feeds.subscribe_quotes(EPIC2).expect("healthy epic");
    assert!(
        wait_for(Duration::from_secs(10), || {
            tape.bids(EPIC2).len() >= 3 && !tape.statuses(EPIC).is_empty()
        }),
        "the healthy lane never streamed, or the refusal was not disclosed"
    );

    // Past the pump's 3 s redial: the refused lane's own wait (6 s on top) says not yet.
    let before = tape.bids(EPIC2).len();
    std::thread::sleep(Duration::from_secs(4));
    let (logins, sessions, adds) =
        (ig.count("POST /session"), ls.sessions.load(Ordering::Relaxed), ls.adds().len());
    let (after, quotes_of_refused) = (tape.bids(EPIC2).len(), tape.bids(EPIC));
    let (refused_says, healthy_says) = (tape.statuses(EPIC), tape.statuses(EPIC2));
    feeds.unsubscribe(refused);
    feeds.unsubscribe(healthy);
    feeds.shutdown();

    assert_eq!((logins, sessions, adds), (1, 1, 2), "one login, one socket, one add each");
    assert!(after >= before + 10, "the healthy lane was interrupted: {before} -> {after} quotes");
    assert!(quotes_of_refused.is_empty(), "a refused lane emitted quotes: {quotes_of_refused:?}");
    assert!(
        matches!(refused_says.as_slice(), [StreamStatus::GapStart { .. }]),
        "the refusal is disclosed once: {refused_says:?}"
    );
    assert!(
        matches!(healthy_says.as_slice(), [StreamStatus::Live { gap_started_ts_ms: None }]),
        "the healthy lane saw a gap: {healthy_says:?}"
    );
}
