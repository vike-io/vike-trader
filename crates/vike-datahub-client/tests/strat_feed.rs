//! `StratFeed` and `StratOpener` over a real socket, against a SCRIPTED datahub (a fake peer that speaks the
//! lane's open and then the tube's frames, and that the test can kill, refuse and bring back at will). What
//! the real hub does on its side is `crates/vike-datahub/tests/strat_lane.rs`; what is asserted here is the
//! tradehub's half: the open it sends, the numbers it reads from the reply, the held set it replays, and
//! above all how a LOST LINK is disclosed to the sink (`src/strat.rs`'s module doc).

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataError, LiveDataSink, RecordingSink, SinkCall};
use vike_datahub_client::PROTO_VERSION;
use vike_datahub_client::proto::{
    DATA_PLANE_SENTINEL, FEATURE_AUTH, FEATURE_STRATEGY_LANE, FEATURE_STREAM_CLASS, Request,
    Response, SL_FRAME_CEILING_BYTES, SL_HEARTBEAT_MIN, SL_UP_FRAME_CEILING_BYTES, read_frame,
    strat_venue_feature, write_frame,
};
use vike_datahub_client::strat::{StratFeed, StratOpener, StratTimings};
use vike_hub_proto::auth::{DATAHUB_DOMAIN, HubKeys, Scope};
use vike_market_stream::frame::{read_capped, write_capped};
use vike_market_stream::opener::Opener;
use vike_market_stream::wire::{StreamDown, StreamUp, SubOp, WireDelivery, WireLane, WireStream};
use vike_marketdata::QuoteTick;
use vike_model::StreamStatus;
use vike_venues::link_deadman::{LinkSource, STRAT_LINK_STREAM};

const TRADE_KEY: &[u8] = b"strat-feed-test-trade-key";

fn trade_keys() -> HubKeys {
    HubKeys::new(b"o".to_vec(), b"c".to_vec()).with_trade(TRADE_KEY.to_vec())
}

fn timings() -> StratTimings {
    StratTimings {
        open_reply: Duration::from_secs(5),
        sub_reply: Duration::from_secs(5),
        stream_read_floor: Duration::from_secs(20),
        reconnect_base: Duration::from_millis(20),
        reconnect_max: Duration::from_millis(100),
    }
}

/// Poll `cond` for up to ten seconds.
fn eventually(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for: {what}");
}

// ---- the scripted datahub ------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Seen {
    /// A `StratOpen`, with whether the connection had authenticated under the Trade scope.
    Open { venue: String, network: Option<String>, trade: bool },
    /// A tube frame the client sent after the open.
    Up(StreamUp),
}

struct PeerCfg {
    keyed: bool,
    features: Vec<String>,
    heartbeat_ms: u64,
    refuse_open: Option<String>,
}

impl PeerCfg {
    fn lane() -> Self {
        Self {
            keyed: true,
            features: vec![
                FEATURE_AUTH.to_string(),
                DATA_PLANE_SENTINEL.to_string(),
                FEATURE_STRATEGY_LANE.to_string(),
                strat_venue_feature("binance"),
                FEATURE_STREAM_CLASS.to_string(),
            ],
            heartbeat_ms: 5_000,
            refuse_open: None,
        }
    }

    fn without(mut self, feature: &str) -> Self {
        self.features.retain(|f| f != feature);
        self
    }
}

struct Peer {
    addr: SocketAddr,
    /// Accepting and serving connections; `false` accepts and drops them at once (a daemon that is down).
    up: AtomicBool,
    /// Connections whose `Hello` was read.
    hellos: AtomicUsize,
    log: Mutex<Vec<Seen>>,
    /// `sid -> k` as acknowledged, across every connection.
    keys: Mutex<HashMap<u64, u32>>,
    /// The current lane connection's writer (also what `kill` shuts down).
    cur: Mutex<Option<Arc<Mutex<TcpStream>>>>,
    /// Whether to answer a `Sub` with a `SubOk` (a peer that withholds the ack models a stalled replay).
    ack: AtomicBool,
}

impl Peer {
    fn start(cfg: PeerCfg) -> Arc<Peer> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let peer = Arc::new(Peer {
            addr,
            up: AtomicBool::new(true),
            hellos: AtomicUsize::new(0),
            log: Mutex::default(),
            keys: Mutex::default(),
            cur: Mutex::default(),
            ack: AtomicBool::new(true),
        });
        let cfg = Arc::new(cfg);
        let p = Arc::clone(&peer);
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                if !p.up.load(Ordering::SeqCst) {
                    drop(conn);
                    continue;
                }
                let (p, cfg) = (Arc::clone(&p), Arc::clone(&cfg));
                thread::spawn(move || {
                    let _ = serve(&p, &cfg, conn);
                });
            }
        });
        peer
    }

    fn opener(&self) -> StratOpener {
        StratOpener::new(self.addr.to_string(), trade_keys(), timings())
    }

    fn seen(&self) -> Vec<Seen> {
        self.log.lock().unwrap().clone()
    }

    fn subs(&self) -> Vec<(u64, SubOp, Option<WireDelivery>)> {
        self.seen()
            .into_iter()
            .filter_map(|s| match s {
                Seen::Up(StreamUp::Sub { sid, op, class }) => Some((sid, op, class)),
                _ => None,
            })
            .collect()
    }

    fn opens(&self) -> Vec<Seen> {
        self.seen().into_iter().filter(|s| matches!(s, Seen::Open { .. })).collect()
    }

    fn k_of(&self, sid: u64) -> u32 {
        *self.keys.lock().unwrap().get(&sid).expect("that sid was acknowledged")
    }

    fn send(&self, frame: &StreamDown) {
        let w = self.cur.lock().unwrap().clone().expect("a lane connection is open");
        write_capped(&mut *w.lock().unwrap(), frame, SL_FRAME_CEILING_BYTES).expect("peer write");
    }

    /// Break the current lane connection the way a dead link does: both directions, at once.
    fn kill(&self) {
        if let Some(w) = self.cur.lock().unwrap().take() {
            let _ = w.lock().unwrap().shutdown(std::net::Shutdown::Both);
        }
    }
}

fn serve(p: &Peer, cfg: &PeerCfg, conn: TcpStream) -> io::Result<()> {
    let mut rd = conn.try_clone()?;
    let wr = Arc::new(Mutex::new(conn));
    let nonce = [7u8; 32];
    let mut authed = false;
    loop {
        let req = read_frame::<_, Request>(&mut rd)?;
        match req {
            Request::Hello { .. } => {
                p.hellos.fetch_add(1, Ordering::SeqCst);
                write_frame(
                    &mut *wr.lock().unwrap(),
                    &Response::Welcome {
                        proto_version: PROTO_VERSION,
                        features: cfg.features.clone(),
                        nonce: cfg.keyed.then_some(nonce),
                    },
                )?;
            }
            Request::Auth { scope, mac } => {
                let ok = trade_keys().verify(DATAHUB_DOMAIN, scope, &nonce, PROTO_VERSION, &mac);
                let resp = if ok {
                    Response::AuthOk { scope }
                } else {
                    Response::AuthDenied { reason: "no".into() }
                };
                authed = ok && scope == Scope::Trade;
                write_frame(&mut *wr.lock().unwrap(), &resp)?;
            }
            Request::StratOpen { venue, network } => {
                p.log.lock().unwrap().push(Seen::Open { venue, network, trade: authed });
                if let Some(text) = &cfg.refuse_open {
                    write_frame(&mut *wr.lock().unwrap(), &Response::Error(text.clone()))?;
                    continue;
                }
                *p.cur.lock().unwrap() = Some(Arc::clone(&wr));
                write_frame(
                    &mut *wr.lock().unwrap(),
                    &Response::StratOpened { session: 41, heartbeat_ms: cfg.heartbeat_ms },
                )?;
                return lane(p, &wr, &mut rd);
            }
            Request::Ping => write_frame(&mut *wr.lock().unwrap(), &Response::Pong)?,
            other => panic!("the scripted datahub got an unexpected positional frame: {other:?}"),
        }
    }
}

fn lane(p: &Peer, wr: &Arc<Mutex<TcpStream>>, rd: &mut TcpStream) -> io::Result<()> {
    let mut next_k = 1u32;
    loop {
        let up = read_capped::<_, StreamUp>(rd, SL_UP_FRAME_CEILING_BYTES)?;
        p.log.lock().unwrap().push(Seen::Up(up.clone()));
        if let StreamUp::Sub { sid, .. } = up
            && p.ack.load(Ordering::SeqCst)
        {
            let k = *p.keys.lock().unwrap().entry(sid).or_insert_with(|| {
                next_k += 1;
                next_k
            });
            write_capped(
                &mut *wr.lock().unwrap(),
                &StreamDown::SubOk { sid, k },
                SL_FRAME_CEILING_BYTES,
            )?;
        }
    }
}

// ---- the host's side -------------------------------------------------------------------------------------------

fn new_feed(peer: &Peer) -> (StratFeed, Arc<RecordingSink>) {
    let rec = Arc::new(RecordingSink::default());
    let sink: Arc<dyn LiveDataSink> = rec.clone();
    (StratFeed::new(peer.opener(), "binance", sink).expect("a thread"), rec)
}

/// Every `stream_status` the host's sink was handed: `(symbol, stream word, status)`.
fn statuses(rec: &RecordingSink) -> Vec<(String, String, StreamStatus)> {
    rec.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::StreamStatus { symbol, stream, status, .. } => Some((symbol, stream, status)),
            _ => None,
        })
        .collect()
}

fn link_statuses(rec: &RecordingSink, symbol: &str) -> Vec<StreamStatus> {
    statuses(rec)
        .into_iter()
        .filter(|(s, w, _)| s == symbol && w == STRAT_LINK_STREAM)
        .map(|(_, _, st)| st)
        .collect()
}

fn lane_statuses(rec: &RecordingSink) -> Vec<(String, String, StreamStatus)> {
    statuses(rec).into_iter().filter(|(_, w, _)| w != STRAT_LINK_STREAM).collect()
}

// ---- the open ---------------------------------------------------------------------------------------------------

/// **The open is `StratOpen{venue, network}` under the Trade scope, and what the reply states fills the
/// stream**: the session, epoch 0, the heartbeat clamped into the lane's range, the class from the feature
/// string, and the write timeout armed.
#[test]
fn the_open_is_stratopen_under_trade_and_the_reply_fills_the_stream() {
    let peer = Peer::start(PeerCfg::lane());
    let stream = peer.opener().open("binance", &|_| {}).expect("the lane opens");
    assert_eq!(stream.session, 41);
    assert_eq!(stream.epoch, 0);
    assert_eq!(stream.heartbeat, Duration::from_secs(5));
    assert!(stream.delivery_class, "the daemon advertised stream_class");
    assert!(stream.stream.write_timeout().unwrap().is_some(), "the write bound is armed");
    assert_eq!(
        peer.opens(),
        vec![Seen::Open { venue: "binance".into(), network: None, trade: true }]
    );
}

#[test]
fn the_class_follows_the_feature_string_and_the_heartbeat_is_clamped() {
    let mut cfg = PeerCfg::lane().without(FEATURE_STREAM_CLASS);
    cfg.heartbeat_ms = 1;
    let peer = Peer::start(cfg);
    let stream = peer.opener().open("binance", &|_| {}).expect("opens");
    assert!(!stream.delivery_class, "no stream_class advertised, none claimed");
    assert_eq!(stream.heartbeat, SL_HEARTBEAT_MIN, "a stated period under the floor is clamped up");
}

#[test]
fn a_network_rides_the_open() {
    let peer = Peer::start(PeerCfg::lane());
    let _ = peer.opener().on_network(Some("testnet")).open("binance", &|_| {}).expect("opens");
    assert_eq!(
        peer.opens(),
        vec![Seen::Open { venue: "binance".into(), network: Some("testnet".into()), trade: true }]
    );
}

/// No Trade key: refused before anything is dialled (a mac signed with the empty key would be sent
/// otherwise), and the refusal names the missing key.
#[test]
fn no_trade_key_dials_nothing() {
    let peer = Peer::start(PeerCfg::lane());
    let keyless = StratOpener::new(
        peer.addr.to_string(),
        HubKeys::new(b"o".to_vec(), b"c".to_vec()),
        timings(),
    );
    let e = keyless.open("binance", &|_| {}).expect_err("no Trade key");
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    assert!(e.to_string().contains("trade hub key"), "{e}");
    thread::sleep(Duration::from_millis(100));
    assert_eq!(peer.hellos.load(Ordering::SeqCst), 0, "nothing was dialled");
}

/// A key-less datahub authenticates nothing, so it can serve no lane: the connect that quietly degraded to
/// unauthenticated is refused, and no `StratOpen` is sent.
#[test]
fn a_keyless_datahub_serves_no_lane_and_nothing_is_opened() {
    let mut cfg = PeerCfg::lane().without(FEATURE_AUTH);
    cfg.keyed = false;
    let peer = Peer::start(cfg);
    let e = peer.opener().open("binance", &|_| {}).expect_err("keyless");
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    assert!(e.to_string().contains("serves no strategy lane"), "{e}");
    assert!(peer.opens().is_empty());
}

/// A lane or a venue the datahub does not advertise is refused LOCALLY naming the string: nothing is sent.
#[test]
fn an_unadvertised_lane_or_venue_is_refused_before_a_byte_is_sent() {
    let peer = Peer::start(PeerCfg::lane().without(FEATURE_STRATEGY_LANE));
    let e = peer.opener().open("binance", &|_| {}).expect_err("no lane");
    assert_eq!(e.kind(), io::ErrorKind::Unsupported);
    assert!(e.to_string().contains(FEATURE_STRATEGY_LANE), "{e}");
    assert!(peer.opens().is_empty());

    let peer = Peer::start(PeerCfg::lane());
    let e = peer.opener().open("bybit", &|_| {}).expect_err("no such venue");
    assert_eq!(e.kind(), io::ErrorKind::Unsupported);
    assert!(e.to_string().contains("bybit"), "{e}");
    assert!(peer.opens().is_empty());
}

/// **The lane check is askable WITHOUT opening one** (W12c): the tradehub's feed gate reads the advertisement of
/// the datahub it just authenticated against, for every mounted venue, before a core exists, so a datahub that
/// cannot serve a venue (an older or smaller build mid-deploy) is a held feed with the words and not a start that
/// dies after its core. It sends nothing: the peer never sees a `StratOpen`.
#[test]
fn the_lane_check_reads_the_advertisement_and_opens_nothing() {
    let peer = Peer::start(PeerCfg::lane());
    let client = vike_datahub_client::DatahubClient::connect_strat_lane(
        &peer.addr.to_string(),
        &trade_keys(),
        |_| {},
    )
    .expect("authenticates");
    assert!(client.check_strat_lane("binance").is_ok());
    let e = client.check_strat_lane("bybit").expect_err("not advertised");
    assert_eq!(e.kind(), io::ErrorKind::Unsupported);
    assert!(e.to_string().contains("bybit") && e.to_string().contains("binance"), "{e}");
    assert!(peer.opens().is_empty(), "nothing was opened");

    let no_lane = Peer::start(PeerCfg::lane().without(FEATURE_STRATEGY_LANE));
    let client = vike_datahub_client::DatahubClient::connect_strat_lane(
        &no_lane.addr.to_string(),
        &trade_keys(),
        |_| {},
    )
    .expect("authenticates");
    let e = client.check_strat_lane("binance").expect_err("no lane");
    assert!(e.to_string().contains(FEATURE_STRATEGY_LANE), "{e}");
}

/// The daemon's own refusal comes back as its text, unchanged.
#[test]
fn a_daemon_refusal_is_its_text() {
    let mut cfg = PeerCfg::lane();
    cfg.refuse_open = Some("this datahub's lane is full".into());
    let peer = Peer::start(cfg);
    let e = peer.opener().open("binance", &|_| {}).expect_err("refused");
    assert_eq!(e.to_string(), "this datahub's lane is full");
}

// ---- the contract of a DataClient ----------------------------------------------------------------------------------

/// Subscriptions are in-band `Sub` frames with the engine's ids; frames the peer streams reach the sink.
#[test]
fn subscriptions_are_in_band_and_frames_reach_the_sink() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, rec) = new_feed(&peer);
    let id = feed.subscribe_quotes("BTCUSDT").expect("subscribes");
    feed.subscribe_bars("ETHUSDT", "1m").expect("subscribes");
    assert_eq!(
        peer.subs(),
        vec![
            (1, SubOp::Quotes { symbol: "BTCUSDT".into() }, None),
            (2, SubOp::Bars { symbol: "ETHUSDT".into(), interval: "1m".into() }, None),
        ]
    );
    let quote = QuoteTick {
        ts: 5,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    };
    peer.send(&StreamDown::Quote { k: peer.k_of(id.0), seq: 1, quote });
    eventually("the quote reaches the sink", || rec.quotes().len() == 1);
    assert_eq!(rec.quotes()[0].0, "binance");
    assert_eq!(rec.quotes()[0].1, "BTCUSDT");
    assert!(feed.is_up());
}

/// The tick pump is the tube's inherent verb: it rides as `SubOp::Ticks`, and a grid the tube refuses is
/// an `Err` before any frame.
#[test]
fn the_tick_pump_rides_as_a_ticks_subscription() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, _rec) = new_feed(&peer);
    feed.subscribe_ticks("BTCUSDT", 0.1).expect("subscribes");
    assert_eq!(peer.subs()[0].1, SubOp::Ticks { symbol: "BTCUSDT".into(), tick_size: 0.1 });
    let bad = feed.subscribe_ticks("BTCUSDT", f64::NAN).expect_err("not a grid");
    assert!(matches!(bad, LiveDataError::Subscribe(_)), "{bad:?}");
    assert_eq!(peer.subs().len(), 1, "the refused grid sent nothing");
}

/// Only a depth subscription raises a lane's class: the floor rides the `Sub`. A daemon that does not
/// advertise the class cannot honour it: the subscription still runs, conflated.
#[test]
fn only_depth_raises_the_class() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, _rec) = new_feed(&peer);
    feed.subscribe_quotes("BTCUSDT").expect("quotes");
    feed.subscribe_trades("BTCUSDT").expect("trades");
    feed.subscribe_book("BTCUSDT").expect("book");
    feed.subscribe_depth("BTCUSDT").expect("depth");
    let classes: Vec<Option<WireDelivery>> = peer.subs().into_iter().map(|(_, _, c)| c).collect();
    assert_eq!(classes, vec![None, None, None, Some(WireDelivery::Lossless)]);

    let peer = Peer::start(PeerCfg::lane().without(FEATURE_STREAM_CLASS));
    let (mut feed, _rec) = new_feed(&peer);
    feed.subscribe_depth("BTCUSDT").expect("depth still subscribes");
    assert_eq!(peer.subs()[0].2, None, "no class is sent to a daemon that does not serve it");
}

/// A down link answers a transient `Err` and does not park the caller.
#[test]
fn a_down_link_is_an_err_not_a_wait() {
    let peer = Peer::start(PeerCfg::lane());
    peer.up.store(false, Ordering::SeqCst);
    let (mut feed, _rec) = new_feed(&peer);
    let t = Instant::now();
    let e = feed.subscribe_quotes("BTCUSDT").expect_err("the datahub is down");
    assert!(matches!(e, LiveDataError::Subscribe(_)), "{e:?}");
    assert!(t.elapsed() < Duration::from_secs(5), "the caller waited {:?}", t.elapsed());
}

/// `begin_shutdown` is a frame, and the feed is usable afterwards.
#[test]
fn begin_shutdown_is_a_frame_and_the_feed_stays_usable() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, _rec) = new_feed(&peer);
    feed.subscribe_quotes("BTCUSDT").expect("subscribes");
    feed.begin_shutdown();
    eventually("the frame arrives", || peer.seen().contains(&Seen::Up(StreamUp::BeginShutdown)));
    feed.subscribe_trades("BTCUSDT").expect("usable after the first phase");
}

// ---- loss ------------------------------------------------------------------------------------------------------------

/// **A lost link is disclosed under the strat-link word, once per held symbol, the instant it happens;
/// the venue's own open outage is neither closed nor duplicated by it; the replay under the original
/// ids closes it with the stamp it opened at.**
#[test]
fn a_lost_link_is_disclosed_under_its_own_word_and_closed_by_the_replay() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, rec) = new_feed(&peer);
    let q = feed.subscribe_quotes("BTCUSDT").expect("quotes");
    feed.subscribe_bars("ETHUSDT", "1m").expect("bars");
    // The venue's own socket is down at hop one, which the datahub forwards: that is the VENUE's outage.
    let venue_gap = StreamStatus::GapStart { at_ts_ms: 50 };
    peer.send(&StreamDown::Status {
        k: peer.k_of(q.0),
        stream: WireStream::Lane(WireLane::Quotes),
        status: venue_gap,
    });
    eventually("the forwarded venue status passes with its lane word", || {
        lane_statuses(&rec).len() == 1
    });

    peer.kill();
    eventually("both held symbols are disclosed under the link word", || {
        !link_statuses(&rec, "BTCUSDT").is_empty() && !link_statuses(&rec, "ETHUSDT").is_empty()
    });
    // ... and then the replay closes each, under original ids.
    eventually("both are closed by the replay", || {
        link_statuses(&rec, "BTCUSDT").len() == 2 && link_statuses(&rec, "ETHUSDT").len() == 2
    });
    for symbol in ["BTCUSDT", "ETHUSDT"] {
        let seq = link_statuses(&rec, symbol);
        let StreamStatus::GapStart { at_ts_ms } = seq[0] else {
            panic!("opens with a GapStart: {seq:?}")
        };
        assert_eq!(
            seq[1],
            StreamStatus::Live { gap_started_ts_ms: Some(at_ts_ms) },
            "{symbol}: the close echoes when the gap OPENED"
        );
    }
    assert_eq!(
        lane_statuses(&rec),
        vec![("BTCUSDT".to_string(), "quotes".to_string(), venue_gap)],
        "the link added no lane-word status: the venue's outage stays the venue's"
    );
    let subs = peer.subs();
    assert_eq!(subs.len(), 4, "two subscriptions, replayed once: {subs:?}");
    assert_eq!(
        subs[2..].iter().map(|s| s.0).collect::<Vec<_>>(),
        subs[..2].iter().map(|s| s.0).collect::<Vec<_>>(),
        "the held set replays under its ORIGINAL ids"
    );
    assert_eq!(feed.open_link_gaps(), 0);
}

/// **An unsubscribe during an outage still closes the gap**, at once and from the caller's thread, and the
/// reconnect then replays nothing for it. (Else the dead-man's window for the key stays open until it
/// trips.)
#[test]
fn an_unsubscribe_during_an_outage_closes_the_gap() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, rec) = new_feed(&peer);
    let q = feed.subscribe_quotes("BTCUSDT").expect("quotes");
    peer.up.store(false, Ordering::SeqCst);
    peer.kill();
    eventually("the loss is disclosed", || !link_statuses(&rec, "BTCUSDT").is_empty());
    assert_eq!(feed.open_link_gaps(), 1);

    feed.unsubscribe(q);
    let seq = link_statuses(&rec, "BTCUSDT");
    assert_eq!(seq.len(), 2, "closed before unsubscribe returned: {seq:?}");
    let StreamStatus::GapStart { at_ts_ms } = seq[0] else { panic!("{seq:?}") };
    assert_eq!(seq[1], StreamStatus::Live { gap_started_ts_ms: Some(at_ts_ms) });
    assert_eq!(feed.open_link_gaps(), 0);

    let hellos = peer.hellos.load(Ordering::SeqCst);
    peer.up.store(true, Ordering::SeqCst);
    eventually("the datahub is dialled again", || peer.hellos.load(Ordering::SeqCst) > hellos);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(peer.subs().len(), 1, "nothing is replayed for the unsubscribed key");
    assert_eq!(link_statuses(&rec, "BTCUSDT").len(), 2, "and nothing more is disclosed");
}

/// While the symbol still has another lane the gap stays open through one lane's unsubscribe, and the
/// surviving lane's replay closes it.
#[test]
fn a_symbol_with_a_surviving_lane_is_closed_by_its_replay() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, rec) = new_feed(&peer);
    let q = feed.subscribe_quotes("BTCUSDT").expect("quotes");
    feed.subscribe_trades("BTCUSDT").expect("trades");
    peer.up.store(false, Ordering::SeqCst);
    peer.kill();
    eventually("the loss is disclosed", || !link_statuses(&rec, "BTCUSDT").is_empty());
    feed.unsubscribe(q);
    assert_eq!(link_statuses(&rec, "BTCUSDT").len(), 1, "a lane is still held: still open");
    peer.up.store(true, Ordering::SeqCst);
    eventually("the replay closes it", || link_statuses(&rec, "BTCUSDT").len() == 2);
    assert_eq!(
        peer.subs().iter().filter(|(_, op, _)| matches!(op, SubOp::Quotes { .. })).count(),
        1,
        "the unsubscribed quotes lane was not replayed"
    );
}

/// The deliberate stop synthesizes no loss: a feed shut down is not a link that died.
#[test]
fn a_deliberate_shutdown_discloses_nothing() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, rec) = new_feed(&peer);
    feed.subscribe_quotes("BTCUSDT").expect("quotes");
    feed.shutdown();
    thread::sleep(Duration::from_millis(200));
    assert!(statuses(&rec).is_empty(), "{:?}", statuses(&rec));
}

// ---- W10b: windows left open ---------------------------------------------------------------------------------------

/// The dead-man's windows as the tradehub's `LinkDeadMan` keeps them, folded out of the statuses the host's
/// sink was handed: one per `(symbol, source)`, opened by a `GapStart`, closed ONLY by a `Live` of the SAME
/// source (`vike_venues::link_deadman::LinkSource::of`).
fn open_windows(rec: &RecordingSink) -> std::collections::BTreeSet<(String, LinkSource)> {
    let mut open = std::collections::BTreeSet::new();
    for (symbol, word, status) in statuses(rec) {
        let key = (symbol, LinkSource::of(&word));
        match status {
            StreamStatus::GapStart { .. } => {
                open.insert(key);
            }
            StreamStatus::Live { .. } => {
                open.remove(&key);
            }
            StreamStatus::Stale { .. } => {}
        }
    }
    open
}

/// **A Venue-source window opened before a datahub restart is closed by the joiner's `Live`** (review
/// finding 2). The pump disclosed `GapStart` on its `"ticks"` word (the venue blipped), then the datahub
/// restarted: the link's own loss and replay close ONLY the strat-link window, and the engine's replay
/// `Live` is withheld, so without a word from the venue side the Venue window stays open until the
/// dead-man trips on a healthy feed. The gatewayhub tells a joiner onto an UP pump `Live{None}` on the
/// pump's word (`crates/vike-gatewayhub/src/data/pump.rs`'s `Shared::add`); here the scripted datahub
/// plays that frame after the replay, and the window must close.
#[test]
fn a_venue_window_open_across_a_datahub_restart_closes_when_the_joiner_hears_live() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, rec) = new_feed(&peer);
    let pump = feed.subscribe_ticks("BTCUSDT", 0.5).expect("the pump");
    let k = peer.k_of(pump.0);
    peer.send(&StreamDown::Status {
        k,
        stream: WireStream::Link,
        status: StreamStatus::GapStart { at_ts_ms: 50 },
    });
    eventually("the venue's disconnect is forwarded", || !lane_statuses(&rec).is_empty());
    assert_eq!(
        open_windows(&rec),
        std::collections::BTreeSet::from([("BTCUSDT".to_string(), LinkSource::Venue)]),
        "the venue's window is open"
    );

    // The datahub restarts; the venue recovered meanwhile, and its `Live` went into the dead socket.
    peer.kill();
    eventually("the link's own window opens and closes with the replay", || {
        link_statuses(&rec, "BTCUSDT").len() == 2
    });
    assert_eq!(
        open_windows(&rec),
        std::collections::BTreeSet::from([("BTCUSDT".to_string(), LinkSource::Venue)]),
        "the replay closed the LINK's window and could not close the venue's: this is the stuck state"
    );

    // The new session joined an up pump; the gatewayhub says so.
    peer.send(&StreamDown::Status {
        k,
        stream: WireStream::Link,
        status: StreamStatus::Live { gap_started_ts_ms: None },
    });
    eventually("the venue window closes", || open_windows(&rec).is_empty());
    assert_eq!(
        lane_statuses(&rec).last(),
        Some(&(
            "BTCUSDT".to_string(),
            "ticks".to_string(),
            StreamStatus::Live { gap_started_ts_ms: None }
        )),
        "the Live reached the sink on the pump's own word, not withheld"
    );
}

/// A host sink that panics on a quote (a bug in a strategy's callback) and records the statuses it is told:
/// the engine ends the lane the callback killed (`is_dead`), which the feed's bookkeeping must notice.
struct PanicOnQuote(Arc<RecordingSink>);

impl LiveDataSink for PanicOnQuote {
    fn seed_bars(&self, v: &str, s: &str, i: &str, b: Vec<vike_marketdata::Bar>) {
        self.0.seed_bars(v, s, i, b);
    }

    fn close_bar(&self, v: &str, s: &str, i: &str, b: vike_marketdata::Bar) {
        self.0.close_bar(v, s, i, b);
    }

    fn forming_bar(&self, v: &str, s: &str, i: &str, b: vike_marketdata::Bar) {
        self.0.forming_bar(v, s, i, b);
    }

    fn mark_tick(&self, v: &str, s: &str, p: f64, t: i64) {
        self.0.mark_tick(v, s, p, t);
    }

    fn trade(&self, v: &str, s: &str, t: vike_marketdata::TradeTick) {
        self.0.trade(v, s, t);
    }

    fn book(&self, v: &str, s: &str, b: Arc<vike_marketdata::L2Book>) {
        self.0.book(v, s, b);
    }

    fn quote(&self, _venue: &str, _symbol: &str, _quote: QuoteTick) {
        panic!("a strategy callback that panics");
    }

    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.0.stream_status(venue, symbol, stream, status);
    }
}

/// **The last SERVED lane's unsubscribe closes the gap even when a sibling lane died** (review finding 5):
/// `held` still lists a lane the engine ended on its own, and counting it kept the symbol's strat-link
/// window open after the owner had given up everything that was alive.
#[test]
fn the_last_served_lane_closes_the_gap_though_a_dead_sibling_is_still_listed() {
    let peer = Peer::start(PeerCfg::lane());
    let rec = Arc::new(RecordingSink::default());
    let sink: Arc<dyn LiveDataSink> = Arc::new(PanicOnQuote(Arc::clone(&rec)));
    let mut feed = StratFeed::new(peer.opener(), "binance", sink).expect("a thread");
    let quotes = feed.subscribe_quotes("BTCUSDT").expect("quotes");
    let trades = feed.subscribe_trades("BTCUSDT").expect("trades");
    let quote = QuoteTick {
        ts: 5,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    };
    peer.send(&StreamDown::Quote { k: peer.k_of(quotes.0), seq: 1, quote });
    // The engine ends the lane the callback killed and discloses it as the lane's own `GapStart`.
    eventually("the panicking lane is ended by the engine", || {
        lane_statuses(&rec)
            .iter()
            .any(|(_, w, s)| w == "quotes" && matches!(s, StreamStatus::GapStart { .. }))
    });

    peer.up.store(false, Ordering::SeqCst);
    peer.kill();
    eventually("the loss is disclosed for the served lane", || {
        !link_statuses(&rec, "BTCUSDT").is_empty()
    });
    feed.unsubscribe(trades);
    assert_eq!(
        link_statuses(&rec, "BTCUSDT").len(),
        2,
        "nothing served is left, so the window closes: {:?}",
        link_statuses(&rec, "BTCUSDT")
    );
    assert_eq!(feed.open_link_gaps(), 0);
}

/// **Shutting a feed down closes the windows it still has open** (W6 DOUBT 3): a hot reload or a disarm
/// leaves no lane in doubt, and the window would otherwise trip the dead-man on a feed nobody reads.
#[test]
fn shutting_a_feed_down_closes_its_open_gaps() {
    let peer = Peer::start(PeerCfg::lane());
    let (mut feed, rec) = new_feed(&peer);
    feed.subscribe_quotes("BTCUSDT").expect("quotes");
    peer.up.store(false, Ordering::SeqCst);
    peer.kill();
    eventually("the loss is disclosed", || !link_statuses(&rec, "BTCUSDT").is_empty());
    assert_eq!(feed.open_link_gaps(), 1);
    feed.shutdown();
    let seq = link_statuses(&rec, "BTCUSDT");
    assert_eq!(seq.len(), 2, "closed by the shutdown: {seq:?}");
    let StreamStatus::GapStart { at_ts_ms } = seq[0] else { panic!("{seq:?}") };
    assert_eq!(seq[1], StreamStatus::Live { gap_started_ts_ms: Some(at_ts_ms) });
    assert!(open_windows(&rec).is_empty());
    drop(feed);
    assert_eq!(link_statuses(&rec, "BTCUSDT").len(), 2, "the drop after it says nothing more");
}
