//! **The family's listen-key stream, pinned at its production entry**: `vike_binance::family::execution::user_data`'s
//! `spawn_user_data_with_resync`, the one spawn binance perp (USDS-M and COIN-M) and both aster lanes run, on a loopback
//! venue. What the venue hears, what the account's lane, its stream cell and its link history receive, and every line the
//! shared user-stream engine says, with the timings between them. A live order path: a difference here is a behaviour
//! change, never a refactor.
//!
//! # The pinned steps and timings
//!
//! - **Open**: one listenKey POST, then the socket `<ws_base><ws_path>/<key>`; INFO `user-data WS connected`, the cell
//!   open, a link-history row opened on the pump's thread name `{lane}-userdata-{symbol}`.
//! - **The key expires** (the venue's `listenKeyExpired` frame): the session ends in the words "the venue said the
//!   listenKey expired" (INFO `user-data WS closed mid-session — reconnecting`: a reopen, `Connecting`, which the order
//!   path's level rule says at `info`; the history row closed in those words),
//!   the re-open waits the engine's first backoff (1 s, spread by up to half), mints a NEW key (a second POST) and says INFO
//!   `user-data WS reconnected`; the replay runs on the `{lane}-resync-{symbol}` thread at least the settle (1 s) after
//!   that, through the fence, drops a row older than the spawn, and fires the reconcile trigger once.
//! - **The socket cannot open** (the POST works, the dial does not): a failed open per wait of the ladder, 1 s then 2 s,
//!   each spread by up to half of it (the row's ladder, `vike_venues::reconnect`'s module doc), each a fresh POST; the cell down with the dial's words (never the key) and the count; ONE WARN
//!   `user-data WS transport error — reconnecting` with `next_line_in_s = 300` (the paced failed-open line), then quiet;
//!   no history row.
//! - **The venue refuses the key's create** (HTTP 401): a failed open like any other, retried, never given up.
//! - **Silence**: no frame for the idle threshold ends the session (WARN `user-data WS silent stall — reconnecting`, the
//!   history row closed "no frame from the venue for N s"), and the re-open follows after the threshold and the first
//!   backoff; the venue's own pings keep a session alive.
//! - **The socket keepalive**: a venue that asks for it hears a ping and a pong on that cadence; no PUT before the key's
//!   renewal is due; binance perp's socket hears nothing of ours.
//! - **Stop**: INFO `user-data pump stopped`, the history row closed as ours, the cell gone. A stop during a backoff says
//!   nothing.
//!
//! The listenKey PUT cadence (30 min), its failed-PUT retry and the key-at-risk end are `crates/bridges/binance/src/family/execution/user_data.rs`'s
//! own unit tests (`listenkey_tests`), on cadences a test can wait for.
//!
//! # Why its own binary, and ONE test
//!
//! The engine says its lines on threads the spawn owns, which only `vike_log::capture::global` sees, and
//! `vike_bridge_core::link_history::install` takes the process's one sink: both are process-wide, so this file is a
//! binary of its own, and its scenarios run one after another in one test, each reading the lines said after its start.
#![cfg(feature = "execution")]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracing::Level;
use tungstenite::Message;
use vike_binance::family::execution::user_data::{
    ListenKeyAuthentication, WebsocketSessionUserSpecBinance, spawn_user_data_with_resync,
};
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::link_history::{self, ENDED_BY_US, VenueLinkSinks};
use vike_bridge_core::venue_link::{VenueLink, VenueLinkState, venue_link};
use vike_bridge_core::websocket_session::user::WebsocketSessionUserResyncHandle;
use vike_execution::{EventSender, Ingest};
use vike_log::capture::{CapturedEvent, global};
use vike_model::events::Event;
use vike_secrets::link_sessions::{LinkEvent, PLANE_TRADE};

/// The tracing target of every line the shared user-stream engine says.
const TARGET: &str = "vike_bridge_core::websocket_session::user";
/// The key the loopback venue mints, long enough that hiding it can touch no other word.
const KEY: &str = "thePinnedListenKeyReadsThePrivateStream";
const SYMBOL: &str = "BTCUSDT";
/// The threads' prefix the spec names.
const LANE: &str = "pin";
/// The pump's thread name, which is also the history rows' lane.
const PUMP_THREAD: &str = "pin-userdata-BTCUSDT";
/// Bounds a BROKEN run only: every wait below ends the moment its condition holds.
const PATIENCE: Duration = Duration::from_secs(20);
/// The engine's first reconnect wait and its post-reconnect settle, as the literals they are.
const SECOND: Duration = Duration::from_secs(1);
/// What an upper bound allows past the engine's own waits, for a loaded box.
const SLACK: Duration = Duration::from_secs(3);
/// How the pump's down reading begins while it keeps reopening by itself (its effect words; a key it gave up on
/// says other words).
const REOPENING: &str = "orders still go out on their own path";

/// What one socket of the loopback venue does, session by session (the last one repeats).
#[derive(Clone, Copy)]
enum Session {
    /// One fill frame, then the venue's own word that the key expired.
    FillThenExpired,
    /// Nothing at all.
    Silent,
    /// A server ping on this cadence, nothing else.
    Pings(Duration),
    /// An empty frame every few ms, so the pump's read returns at once and its hook keeps its own cadence.
    Chatter,
}

/// What the loopback venue heard.
#[derive(Default)]
struct Heard {
    posts: Mutex<Vec<Instant>>,
    puts: AtomicUsize,
    sessions: Mutex<Vec<Instant>>,
    pings: Mutex<Vec<Instant>>,
    pongs: AtomicUsize,
    /// When the venue said the key expired.
    expired_at: Mutex<Option<Instant>>,
}

/// One loopback venue: its script and what it heard.
struct Venue {
    sessions: Vec<Session>,
    refuse_posts: bool,
    heard: Heard,
}

impl Venue {
    fn new(sessions: Vec<Session>) -> Self {
        Self { sessions, refuse_posts: false, heard: Heard::default() }
    }
}

/// One loopback connection: a listenKey call is counted and answered, a socket follows the script.
fn serve(mut stream: TcpStream, venue: &Venue) {
    let mut head = [0u8; 4];
    loop {
        match stream.peek(&mut head) {
            Ok(4) => break,
            Ok(0) | Err(_) => return,
            Ok(_) => std::thread::yield_now(),
        }
    }
    if &head == b"GET " {
        return socket(stream, venue);
    }
    let refused = if &head == b"POST" {
        venue.heard.posts.lock().unwrap().push(Instant::now());
        venue.refuse_posts
    } else {
        venue.heard.puts.fetch_add(1, Ordering::Relaxed);
        false
    };
    let mut request = [0u8; 4096];
    let _ = std::io::Read::read(&mut stream, &mut request);
    let (status, body) = if refused {
        ("401 Unauthorized", r#"{"code":-2015,"msg":"Invalid API-key"}"#.to_string())
    } else {
        ("200 OK", format!(r#"{{"listenKey":"{KEY}"}}"#))
    };
    let answer = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = std::io::Write::write_all(&mut stream, answer.as_bytes());
}

/// One socket session of the venue, by its script.
fn socket(stream: TcpStream, venue: &Venue) {
    let mut ws = tungstenite::accept(stream).expect("the upgrade");
    ws.get_ref().set_read_timeout(Some(Duration::from_millis(5))).unwrap();
    let index = {
        let mut sessions = venue.heard.sessions.lock().unwrap();
        sessions.push(Instant::now());
        sessions.len() - 1
    };
    let session = venue.sessions[index.min(venue.sessions.len() - 1)];
    let (mut said, mut last_ping) = (false, Instant::now());
    loop {
        match ws.read() {
            Ok(Message::Ping(_)) => venue.heard.pings.lock().unwrap().push(Instant::now()),
            Ok(Message::Pong(_)) => {
                venue.heard.pongs.fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {}
            Err(e) if vike_transport::websocket::is_timeout(&e) => {
                let sent = match session {
                    Session::FillThenExpired if !said => {
                        said = true;
                        let fill = ws.send(Message::Text(r#"{"e":"pinFill"}"#.into()));
                        *venue.heard.expired_at.lock().unwrap() = Some(Instant::now());
                        fill.and_then(|()| {
                            ws.send(Message::Text(r#"{"e":"listenKeyExpired","E":1}"#.into()))
                        })
                    }
                    Session::Pings(every) if last_ping.elapsed() >= every => {
                        last_ping = Instant::now();
                        ws.send(Message::Ping(Vec::new().into()))
                    }
                    Session::Chatter => ws.send(Message::Text("{}".into())),
                    _ => Ok(()),
                };
                if sent.is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

/// The venue on a loopback port.
fn loopback(venue: Venue) -> (Arc<Venue>, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let venue = Arc::new(venue);
    let served = Arc::clone(&venue);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let venue = Arc::clone(&served);
            std::thread::spawn(move || serve(stream, &venue));
        }
    });
    (venue, addr)
}

/// Binance's auth shape: the apiKey header, nothing signed.
struct HeaderOnly;

impl ListenKeyAuthentication for HeaderOnly {
    fn query(&self, _method: &str) -> String {
        String::new()
    }
    fn header(&self) -> Option<(&'static str, String)> {
        Some(("X-MBX-APIKEY", "PIN".to_string()))
    }
}

/// One fill, stamped `ts`.
fn fill(trade_id: &str, ts: i64) -> Event {
    Event::Fill(
        serde_json::from_value(json!({
            "trade_id": trade_id, "order_client_id": "pin", "venue": "loopback", "symbol": SYMBOL,
            "side": 1, "last_qty": 1.0, "last_price": 100.0, "commission": 0.0,
            "commission_asset": "USDT", "timestamp": ts
        }))
        .unwrap(),
    )
}

/// A running stream and what a scenario reads of it.
struct Stream {
    feed: WebsocketSessionUserResyncHandle,
    link: VenueLink,
    lane: Receiver<Ingest>,
    reconciles: Receiver<()>,
    fetches: Arc<Mutex<Vec<Instant>>>,
}

/// The production spawn for the account `route`, on `rest`'s listenKey endpoint and the socket host `websocket_base`. The decoder
/// turns the venue's `pinFill` frame into a fill; the replay returns one row older than the spawn and one after it, once.
fn spawn(
    rest: SocketAddr,
    websocket_base: &str,
    route: &str,
    idle_threshold: Option<Duration>,
    socket_keepalive: Option<Duration>,
) -> Stream {
    let (events, ingest) = vike_execution::event_channel(64);
    let events: EventSender = events.routed(route);
    // The engine's lane, onto a channel this file can name.
    let (to_lane, lane) = mpsc::channel();
    let mut ingest = ingest;
    std::thread::spawn(move || {
        while let Some(event) = ingest.blocking_recv() {
            let _ = to_lane.send(event);
        }
    });
    let spec = WebsocketSessionUserSpecBinance {
        venue: "loopback",
        lane: LANE,
        rest_url: format!("http://{rest}"),
        websocket_base_url: websocket_base.to_string(),
        websocket_path: "/ws",
        listenkey_path: "/listenKey",
        idle_threshold,
        socket_keepalive,
        history_lag: None,
        reconnect: vike_venues::venue_timings::BINANCE_PERP.exec().reconnect(),
    };
    let fetches = Arc::new(Mutex::new(Vec::new()));
    let fetched = Arc::clone(&fetches);
    let now = vike_model::now_ms();
    let mut rows = Some(vec![fill("e_old", now - 3_600_000), fill("e_new", now + 3_600_000)]);
    let replay = move || {
        fetched.lock().unwrap().push(Instant::now());
        rows.take().unwrap_or_default()
    };
    let (reconcile, reconciles) = mpsc::channel();
    let link = venue_link(route);
    let fence = Arc::new(SubmitFence::new(events.clone()));
    let feed = spawn_user_data_with_resync(
        spec,
        SYMBOL.to_string(),
        HeaderOnly,
        |frame: &Value| match frame["e"].as_str() {
            Some("pinFill") => vec![fill("t-live", vike_model::now_ms())],
            _ => Vec::new(),
        },
        events,
        fence,
        replay,
        Some(reconcile),
    );
    Stream { feed, link, lane, reconciles, fetches }
}

impl Stream {
    /// Stop it, and every trade id the lane received, in order.
    fn shutdown(self) -> (Vec<String>, VenueLink, Receiver<()>, Vec<Instant>) {
        let Stream { feed, link, lane, reconciles, fetches } = self;
        feed.shutdown().expect("a listen-key stream never gives up");
        let mut fills = Vec::new();
        loop {
            match lane.recv_timeout(PATIENCE) {
                Ok(Ingest::Event(Event::Fill(f))) => fills.push(f.trade_id.to_string()),
                Ok(other) => panic!("only fills reach this lane: {other:?}"),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("a sender outlived the stream"),
            }
        }
        let fetches = fetches.lock().unwrap().clone();
        (fills, link, reconciles, fetches)
    }
}

/// Wait until `done`, or fail naming `what`.
fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The engine's lines from `from` on, as (level, sentence).
fn said(from: usize) -> Vec<(Level, String)> {
    global(TARGET)[from..].iter().map(|line| (line.level, line.message.clone())).collect()
}

/// The engine's lines from `from` on.
fn lines(from: usize) -> Vec<CapturedEvent> {
    global(TARGET)[from..].to_vec()
}

/// The history rows of `route` received so far, as (opened or how it closed, ours). The routes are a roster venue's
/// (`binance#…`): a key no roster venue minted writes no row (`vike_bridge_core::link_history`'s `trade_key`).
fn rows(history: &Receiver<LinkEvent>, kept: &mut Vec<LinkEvent>, route: &str) -> Vec<String> {
    kept.extend(history.try_iter());
    kept.iter()
        .filter_map(|row| match row {
            LinkEvent::Opened { key, .. } if key.route == route => {
                assert_eq!(
                    (key.venue.as_str(), key.plane, key.lane.as_str()),
                    ("binance", PLANE_TRADE, PUMP_THREAD)
                );
                Some("opened".to_string())
            }
            LinkEvent::Closed { key, how, ours, .. } if key.route == route => {
                assert_eq!(*ours, how == ENDED_BY_US, "{how}");
                Some(how.clone())
            }
            _ => None,
        })
        .collect()
}

fn info(text: &str) -> (Level, String) {
    (Level::INFO, text.to_string())
}

fn warn(text: &str) -> (Level, String) {
    (Level::WARN, text.to_string())
}

/// The key expires: the session ends, a new key is minted after the first backoff, and the replay fills the hole.
fn the_key_expires_and_the_reopen_replays_the_hole(
    history: &Receiver<LinkEvent>,
    kept: &mut Vec<LinkEvent>,
) {
    let from = global(TARGET).len();
    let (venue, addr) = loopback(Venue::new(vec![Session::FillThenExpired, Session::Silent]));
    let stream = spawn(addr, &format!("ws://{addr}"), "binance#EXPIRY", None, None);
    assert_eq!(stream.feed.pump.thread().name(), Some(PUMP_THREAD));
    assert_eq!(stream.feed.resync.thread().name(), Some("pin-resync-BTCUSDT"));
    stream.reconciles.recv_timeout(PATIENCE).expect("the reconnect's reconcile");
    assert_eq!(stream.link.state(), VenueLinkState::Up);
    let (fills, link, reconciles, fetches) = stream.shutdown();

    assert_eq!(
        fills,
        ["t-live", "e_new"],
        "the live fill, then the replay's, the older row dropped"
    );
    assert!(reconciles.try_recv().is_err(), "one reconcile per re-open");
    assert_eq!(link.state(), VenueLinkState::Unknown);
    let heard = &venue.heard;
    let posts = heard.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 2, "a new key for the re-open");
    assert_eq!(heard.sessions.lock().unwrap().len(), 2);
    assert_eq!(heard.puts.load(Ordering::Relaxed), 0, "no renewal was due");
    assert!(heard.pings.lock().unwrap().is_empty(), "binance perp's socket hears nothing of ours");
    assert_eq!(heard.pongs.load(Ordering::Relaxed), 0);

    let expired = heard.expired_at.lock().unwrap().expect("the venue said it");
    let reopen = posts[1] - expired;
    assert!(reopen >= SECOND && reopen < SECOND + SLACK, "re-open {reopen:?} after the expiry");
    assert_eq!(fetches.len(), 1, "one replay");
    let replay = fetches[0] - posts[1];
    assert!(replay >= SECOND && replay < SECOND * 2 + SLACK, "replay {replay:?} after the re-open");

    assert_eq!(
        said(from),
        [
            info("user-data WS connected"),
            info("user-data WS closed mid-session — reconnecting"),
            info("user-data WS reconnected"),
            info("user-data pump stopped"),
        ]
    );
    assert_eq!(lines(from)[1].field("error"), Some("the venue said the listenKey expired"));
    assert_eq!(
        rows(history, kept, "binance#EXPIRY"),
        ["opened", "the venue said the listenKey expired", "opened", ENDED_BY_US]
    );
}

/// The socket's host is down: a failed open per wait of the ladder, each with a fresh key, one paced line.
fn a_socket_that_cannot_open_retries_on_the_ladder(
    history: &Receiver<LinkEvent>,
    kept: &mut Vec<LinkEvent>,
) {
    let from = global(TARGET).len();
    let (venue, addr) = loopback(Venue::new(vec![Session::Silent]));
    // Port 1: nothing listens, and no test's listener can be handed it.
    let stream = spawn(addr, "ws://127.0.0.1:1", "binance#DIAL", None, None);
    let link = stream.link.clone();
    wait_for(
        "the third failed open",
        || matches!(link.state(), VenueLinkState::Down(down) if down.failed_restarts >= 3),
    );
    let VenueLinkState::Down(down) = link.state() else { unreachable!() };
    assert_eq!(down.failed_restarts, 3);
    assert!(down.why.starts_with("connect: "), "{}", down.why);
    // secret-absent: the listen key reads the private stream
    assert!(!down.why.contains(KEY), "{}", down.why);
    assert!(down.effect.starts_with(REOPENING), "{}", down.effect);
    let retry_in = down.retry_in;
    // The row's ladder spreads each step by up to half of it, never past its ceiling (`vike_venues::reconnect`'s
    // module doc), and the cell counts the wait down: the third is 4 s to 6 s, at most.
    assert!(retry_in <= SECOND * 6, "the third wait is the ladder's 4 s, spread: {retry_in:?}");
    let (fills, link, _reconciles, fetches) = stream.shutdown();

    assert!(fills.is_empty() && fetches.is_empty(), "no session, no replay");
    assert_eq!(link.state(), VenueLinkState::Unknown);
    let posts = venue.heard.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 3, "every open mints a key");
    assert!(venue.heard.sessions.lock().unwrap().is_empty());
    assert!(posts[1] - posts[0] >= SECOND, "{:?}", posts[1] - posts[0]);
    assert!(posts[2] - posts[1] >= SECOND * 2, "{:?}", posts[2] - posts[1]);

    let lines = lines(from);
    assert_eq!(said(from), [warn("user-data WS transport error — reconnecting")]);
    let line = &lines[0];
    // secret-absent: the listen key reads the private stream
    assert!(line.field("error").is_some_and(|e| e.starts_with("connect: ") && !e.contains(KEY)));
    let first_wait: u64 =
        line.field("backoff_ms").and_then(|ms| ms.parse().ok()).expect("a wait in ms");
    assert!((1000..=1500).contains(&first_wait), "the ladder's first 1 s, spread: {first_wait} ms");
    assert_eq!(line.field("next_line_in_s"), Some("300"), "the failed-open line is paced");
    assert!(rows(history, kept, "binance#DIAL").is_empty(), "a failed open is no session");
}

/// The venue refuses to create a key: a failed open like any other, retried, never given up.
fn a_refused_create_is_retried_and_never_given_up(
    history: &Receiver<LinkEvent>,
    kept: &mut Vec<LinkEvent>,
) {
    let from = global(TARGET).len();
    let (venue, addr) = loopback(Venue { refuse_posts: true, ..Venue::new(vec![Session::Silent]) });
    let stream = spawn(addr, &format!("ws://{addr}"), "binance#REFUSED", None, None);
    let link = stream.link.clone();
    wait_for(
        "the second failed open",
        || matches!(link.state(), VenueLinkState::Down(down) if down.failed_restarts >= 2),
    );
    let VenueLinkState::Down(down) = link.state() else { unreachable!() };
    assert!(down.why.starts_with("listenKey create: venue error 401: "), "{}", down.why);
    assert!(down.effect.starts_with(REOPENING), "a refused key is retried: {}", down.effect);
    stream.shutdown();

    assert_eq!(venue.heard.posts.lock().unwrap().len(), 2);
    let lines = lines(from);
    assert_eq!(said(from), [warn("user-data WS transport error — reconnecting")]);
    assert!(
        lines[0]
            .field("error")
            .is_some_and(|e| e.starts_with("listenKey create: venue error 401: "))
    );
    assert!(rows(history, kept, "binance#REFUSED").is_empty());
}

/// A silent socket ends at the idle threshold and opens again after the first backoff; the venue's pings keep one alive.
fn silence_ends_a_session_and_pings_keep_one(
    history: &Receiver<LinkEvent>,
    kept: &mut Vec<LinkEvent>,
) {
    let idle = Duration::from_millis(1500);
    let from = global(TARGET).len();
    let (venue, addr) = loopback(Venue::new(vec![Session::Silent]));
    let stream = spawn(addr, &format!("ws://{addr}"), "binance#SILENT", Some(idle), None);
    wait_for("the re-open after the stall", || {
        said(from).contains(&info("user-data WS reconnected"))
    });
    stream.shutdown();
    let sessions = venue.heard.sessions.lock().unwrap().clone();
    assert_eq!(sessions.len(), 2);
    let gap = sessions[1] - sessions[0];
    // The stall is seen at the read's 1 s poll, so it trips at the first poll past the threshold.
    assert!(gap >= idle + SECOND && gap < SECOND * 3 + SLACK, "{gap:?}");
    assert_eq!(venue.heard.posts.lock().unwrap().len(), 2);
    assert_eq!(
        said(from),
        [
            info("user-data WS connected"),
            warn("user-data WS silent stall — reconnecting"),
            info("user-data WS reconnected"),
            info("user-data pump stopped"),
        ]
    );
    assert_eq!(lines(from)[1].field("idle_ms"), Some("1500"));
    assert_eq!(
        rows(history, kept, "binance#SILENT"),
        ["opened", "no frame from the venue for 1 s", "opened", ENDED_BY_US]
    );

    let from = global(TARGET).len();
    let (venue, addr) = loopback(Venue::new(vec![Session::Pings(Duration::from_millis(300))]));
    let stream = spawn(addr, &format!("ws://{addr}"), "binance#PINGED", Some(idle), None);
    std::thread::sleep(idle * 2 + SECOND / 2);
    stream.shutdown();
    assert_eq!(venue.heard.sessions.lock().unwrap().len(), 1, "the pings kept it");
    assert!(venue.heard.pongs.load(Ordering::Relaxed) >= 1, "each ping is answered");
    assert_eq!(said(from), [info("user-data WS connected"), info("user-data pump stopped")]);
    assert_eq!(rows(history, kept, "binance#PINGED"), ["opened", ENDED_BY_US]);
}

/// A venue that asks for a socket keepalive hears a ping and a pong on that cadence, and no PUT before the key is due.
fn the_socket_keepalive_keeps_its_cadence(
    history: &Receiver<LinkEvent>,
    kept: &mut Vec<LinkEvent>,
) {
    let every = Duration::from_millis(250);
    let from = global(TARGET).len();
    let (venue, addr) = loopback(Venue::new(vec![Session::Chatter]));
    let stream = spawn(addr, &format!("ws://{addr}"), "binance#KEEPALIVE", None, Some(every));
    let heard = &venue.heard;
    wait_for("four keepalives", || {
        heard.pings.lock().unwrap().len() >= 4 && heard.pongs.load(Ordering::Relaxed) >= 4
    });
    stream.shutdown();
    let opened = heard.sessions.lock().unwrap()[0];
    let fourth = heard.pings.lock().unwrap()[3] - opened;
    assert!(fourth >= every * 4, "never faster than the cadence: the fourth at {fourth:?}");
    assert_eq!(heard.puts.load(Ordering::Relaxed), 0, "the key's renewal is 30 min away");
    assert_eq!(heard.posts.lock().unwrap().len(), 1);
    assert_eq!(heard.sessions.lock().unwrap().len(), 1);
    assert_eq!(said(from), [info("user-data WS connected"), info("user-data pump stopped")]);
    assert_eq!(rows(history, kept, "binance#KEEPALIVE"), ["opened", ENDED_BY_US]);
}

/// ONE test: the line capture and the history sink are the process's, so the scenarios run one after another.
#[test]
fn the_listen_key_stream_keeps_its_steps_timings_lines_and_rows() {
    global(TARGET);
    let (trade, history) = mpsc::sync_channel(1024);
    let (price, _prices) = mpsc::sync_channel(16);
    link_history::install(VenueLinkSinks { trade, price });
    let mut kept = Vec::new();
    the_key_expires_and_the_reopen_replays_the_hole(&history, &mut kept);
    a_socket_that_cannot_open_retries_on_the_ladder(&history, &mut kept);
    a_refused_create_is_retried_and_never_given_up(&history, &mut kept);
    silence_ends_a_session_and_pings_keep_one(&history, &mut kept);
    the_socket_keepalive_keeps_its_cadence(&history, &mut kept);
}
