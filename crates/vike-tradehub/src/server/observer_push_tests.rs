//! The shaped observe stream ([`super`]) against the real publisher and the real server on a loopback socket, over a
//! test-owned snapshot cell the tests step like a node: what the defaults leave byte for byte, what the cap sends, what
//! compression sends, what an older client is sent, and (ignored, a lane's) how many bytes a second each costs.

use std::collections::{BTreeMap, VecDeque};
use std::net::TcpListener;
use std::sync::Mutex;

use arc_swap::ArcSwap;
use vike_execution::{CoreSnapshot, OrderView, PositionView};
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_hub_protocol::frame::{
    MSGPACK_BODY_MAGIC, read_frame, write_frame, write_frame_as, write_frame_raw,
};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_marketdata::Bar;
use vike_marketdata::test_support::flat_bar_unit_volume;
use vike_model::{OrderStatus, TradingState};
use vike_tradehub_client::auth;
use vike_tradehub_client::observe_stream::{
    COMPRESSED_BODY_MARKER, MSGPACK_DEFLATE_BODY_MAGIC, ObserveReceiver, Received, decode_response,
};
use vike_tradehub_client::proto::{NODE_PROTO_VERSION, Topic};
use vike_tradehub_client::wire::CoreSnapshotWire;

use super::*;
use crate::publish::{self, ObserverStream};
use crate::server::control::ControlLimitsConfig;
use crate::server::serve_with_link_policy;

const OBSERVE_KEY: &[u8] = b"observe-key-for-the-observer-push-tests";
const CONTROL_KEY: &[u8] = b"control-key-for-the-observer-push-tests";
/// The longest any wait here waits: far over every interval a test asks for, so a loaded box fails nothing on time alone.
const PATIENCE: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------------------------
// A node's state, stepped: the shape the stream was measured at (2026-11-18 on the CI box: up to 16 series of 300 bars).
// ---------------------------------------------------------------------------------------------

/// One series of [`Synthetic`]: its key, its closed bars and its forming one.
type SyntheticSeries = (vike_execution::SeriesKey, Vec<Bar>, Bar);

/// A synthetic node: `series` bar series of 300 closed bars and a forming one, a 64-line event ring, an order every few
/// steps that fills a few steps later and stays in the registry, a position that grows with each fill.
struct Synthetic {
    sequence: u64,
    ring: VecDeque<Arc<str>>,
    series: Vec<SyntheticSeries>,
    orders: Vec<OrderView>,
    position: f64,
    /// A bar closes in every series each this many steps.
    close_every: u64,
    halted: bool,
}

impl Synthetic {
    fn new(series: usize, close_every: u64) -> Self {
        let series = (0..series)
            .map(|n| {
                let key = ("binance".to_string(), format!("SYMBOL{n}USDT"), "1m".to_string());
                let closed = (0..300).map(|i| bar(i, n)).collect();
                (key, closed, bar(300, n))
            })
            .collect();
        Synthetic {
            sequence: 0,
            ring: VecDeque::new(),
            series,
            orders: Vec::new(),
            position: 0.0,
            close_every,
            halted: false,
        }
    }

    /// The line every step `seq` adds to the ring, which a capped subscriber must not lose.
    fn line(seq: u64) -> String {
        match seq % 3 {
            0 => format!("OrderAccepted order-{seq}"),
            1 => format!("FillEvent order-{seq} 0.25@{}", 100 + seq % 50),
            _ => format!("OrderRejected order-{seq} (venue=binance)"),
        }
    }

    /// One publish of the node.
    fn step(&mut self) -> CoreSnapshot {
        self.sequence += 1;
        let seq = self.sequence;
        self.ring.push_back(Arc::from(Self::line(seq)));
        while self.ring.len() > 64 {
            self.ring.pop_front();
        }
        for (_, closed, forming) in &mut self.series {
            if seq.is_multiple_of(self.close_every) {
                closed.remove(0);
                closed.push(forming.clone());
                *forming = Bar { timestamp: forming.timestamp + 60_000, ..forming.clone() };
            }
            forming.close += 0.5;
            forming.high = forming.high.max(forming.close);
            forming.volume += 1.0;
        }
        if seq.is_multiple_of(5) {
            self.orders.push(order(&format!("order-{seq}"), OrderStatus::Accepted));
        }
        if seq % 5 == 3 && seq > 5 {
            let id = format!("order-{}", seq - 3);
            if let Some(o) = self.orders.iter_mut().find(|o| o.order_client_id == id) {
                o.status = OrderStatus::Filled;
                o.filled_qty = o.qty;
                o.average_fill_price = 100.0;
                self.position += o.qty;
            }
        }
        self.snapshot()
    }

    /// `count` orders that filled before the run, as the published list held them before it let an ended order go after
    /// an hour (the CI box on 2026-11-19: 2,021 FILLED of 2,023 after 3 h 20 m).
    fn with_ended_orders(mut self, count: usize) -> Self {
        self.orders.extend((0..count).map(|n| {
            let mut ended = order(&format!("ended-{n}"), OrderStatus::Filled);
            ended.filled_qty = ended.qty;
            ended.average_fill_price = 100.0;
            ended.venue_order_id = Some(format!("venue-{n}"));
            ended
        }));
        self
    }

    fn snapshot(&self) -> CoreSnapshot {
        let mut snap = CoreSnapshot::empty("binance", "SYMBOL0USDT");
        snap.sequence = self.sequence;
        snap.recent_events = self.ring.iter().cloned().collect();
        snap.orders = self.orders.iter().cloned().map(Arc::new).collect();
        snap.positions = vec![position(self.position)];
        snap.trading_state = if self.halted { TradingState::Halted } else { TradingState::Active };
        for (key, closed, forming) in &self.series {
            snap.bars.insert(
                key.clone(),
                vike_execution::BarSeries {
                    closed: Arc::new(closed.clone()),
                    forming: Some(forming.clone()),
                },
            );
        }
        snap
    }
}

fn bar(i: i64, series: usize) -> Bar {
    let price = 100.0 + series as f64 + (i % 17) as f64 * 0.25;
    Bar {
        high: price + 1.0,
        low: price - 1.0,
        ..flat_bar_unit_volume(1_700_000_000_000 + i * 60_000, price)
    }
}

fn order(order_client_id: &str, status: OrderStatus) -> OrderView {
    OrderView {
        data_delay: None,
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        account: None,
        symbol: "SYMBOL0USDT".into(),
        side: 1,
        qty: 0.25,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }
}

fn position(size: f64) -> PositionView {
    PositionView {
        venue: "binance".into(),
        symbol: "SYMBOL0USDT".into(),
        position_side: vike_model::events::PositionSide::Long,
        size,
        average_price: 100.0,
        unrealized: size * 0.5,
        mark_source: Some(vike_execution::PriceSource::Mark),
        leverage: 1.0,
        liquidation_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

// ---------------------------------------------------------------------------------------------
// The node and a raw subscriber.
// ---------------------------------------------------------------------------------------------

/// The real publisher over `cell`, handed `stream`, served by the real server on an ephemeral loopback port.
fn spawn_node(
    cell: Arc<ArcSwap<CoreSnapshot>>,
    stream: ObserverStream,
) -> (PublisherHandle, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(cell, None).with_observer_stream(stream);
    let keys = HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    let served = publisher.clone();
    thread::spawn(move || {
        let _ = serve_with_link_policy(
            listener,
            served,
            keys,
            None,
            ControlLimitsConfig::default(),
            None,
            None,
            None,
            LinkPolicy::PRODUCTION,
        );
    });
    (publisher, addr)
}

/// A subscriber that reads every frame raw: the handshake under `Scope::Read`, then `subscribe`.
struct RawSubscriber {
    stream: TcpStream,
    /// What it writes its subscription and its words in: msgpack is a client of this build, JSON one before it.
    codec: BodyCodec,
    /// Write `FrameRead` after each snapshot (it subscribed for the newest).
    says_read: bool,
    /// What its patched frames rebuild.
    receiver: ObserveReceiver,
    /// Drop the next patch frame unread, as a reader that missed it would.
    drop_next_patch: bool,
    /// The patch frames it was sent, whole (`base` absent) or not.
    patches: Vec<Option<u64>>,
    /// How many bytes it read, prefixes included.
    bytes: u64,
}

/// The handshake under `Scope::Read`, the stream authed and read under [`PATIENCE`]. The node advertises the msgpack
/// body, as every node of this build does.
fn authed(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &HandshakeFrame::Hello { proto_version: NODE_PROTO_VERSION })
        .expect("hello");
    let nonce = match read_frame::<_, HandshakeReply>(&mut stream).expect("welcome") {
        HandshakeReply::Welcome { nonce: Some(nonce), features, .. } => {
            assert!(features.iter().any(|f| f == FEATURE_WIRE_MSGPACK), "{features:?}");
            nonce
        }
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(OBSERVE_KEY, &nonce, NODE_PROTO_VERSION, Scope::Read);
    write_frame(&mut stream, &HandshakeFrame::Auth { scope: Scope::Read, mac }).expect("auth");
    match read_frame::<_, HandshakeReply>(&mut stream).expect("authok") {
        HandshakeReply::AuthOk { .. } => {}
        other => panic!("expected AuthOk, got {other:?}"),
    }
    stream.set_read_timeout(Some(PATIENCE)).expect("read bound");
    stream
}

impl RawSubscriber {
    /// A client from before msgpack: it subscribes, and says its words, in JSON.
    fn connect(addr: SocketAddr, subscribe: &Request) -> Self {
        Self::connect_in(addr, subscribe, BodyCodec::Json)
    }

    fn connect_in(addr: SocketAddr, subscribe: &Request, codec: BodyCodec) -> Self {
        let mut stream = authed(addr);
        write_frame_as(&mut stream, codec, subscribe).expect("subscribe");
        let says_read = match subscribe {
            Request::SubscribeNewest { .. } => true,
            Request::SubscribeWith(s) => s.newest,
            _ => false,
        };
        RawSubscriber {
            stream,
            codec,
            says_read,
            receiver: ObserveReceiver::default(),
            drop_next_patch: false,
            patches: Vec::new(),
            bytes: 0,
        }
    }

    /// One of the reader's words to the node, in its form.
    fn say(&mut self, word: &Request) {
        write_frame_as(&mut self.stream, self.codec, word).expect("a word");
    }

    /// The next frame that holds a state: its whole bytes (prefix included) and the state, decoded or rebuilt. A
    /// heartbeat is passed over, and so is a patch while a whole frame is awaited; a gap asks for a whole frame.
    fn next(&mut self) -> (Vec<u8>, CoreSnapshotWire) {
        loop {
            let body = read_frame_raw(&mut self.stream).expect("a frame");
            self.bytes += 4 + body.len() as u64;
            let mut whole = (body.len() as u32).to_be_bytes().to_vec();
            whole.extend_from_slice(&body);
            let response = decode_response(&body, self.codec).expect("decodes");
            let is_frame =
                matches!(response, Response::SnapshotFrame(_) | Response::SnapshotPatch(_));
            if let Response::SnapshotPatch(patch) = &response {
                self.patches.push(patch.base);
            }
            let received = match response {
                Response::SnapshotPatch(_) if self.drop_next_patch => {
                    self.drop_next_patch = false;
                    Received::Nothing
                }
                response => self.receiver.receive(response),
            };
            if matches!(received, Received::Resync) {
                self.say(&Request::ObserveResync);
            }
            if self.says_read && is_frame {
                self.say(&Request::FrameRead);
            }
            if let Received::Snapshot(wire) = received {
                return (whole, *wire);
            }
        }
    }

    /// Frames until one carries `sequence` (or later).
    fn until(&mut self, sequence: u64) -> Vec<(Vec<u8>, CoreSnapshotWire)> {
        let deadline = Instant::now() + PATIENCE;
        let mut seen = Vec::new();
        loop {
            assert!(Instant::now() < deadline, "no frame reached {sequence}");
            let frame = self.next();
            let reached = frame.1.sequence >= sequence;
            seen.push(frame);
            if reached {
                return seen;
            }
        }
    }
}

fn plain() -> Request {
    Request::Subscribe { topics: vec![Topic::All] }
}

fn asked(newest: bool, compressed: bool, patches: bool) -> Request {
    Request::SubscribeWith(ObserveSubscriptionWire {
        topics: vec![Topic::All],
        newest,
        compressed,
        patches,
        series: None,
    })
}

/// Step `node` into `cell` once, wait for the publisher to frame it and record the bytes it framed under its sequence.
fn publish_recorded(
    cell: &ArcSwap<CoreSnapshot>,
    publisher: &PublisherHandle,
    node: &mut Synthetic,
    framed: &mut BTreeMap<u64, Vec<u8>>,
) {
    cell.store(Arc::new(node.step()));
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(latest) = publisher.latest_frame()
            && latest.wire.sequence == node.sequence
        {
            framed.insert(node.sequence, latest.bytes(BodyCodec::Json).to_vec());
            return;
        }
        assert!(Instant::now() < deadline, "the publisher never framed {}", node.sequence);
        thread::sleep(Duration::from_millis(2));
    }
}

/// Step `node` into `cell` `steps` times, one every `every`, and return its last sequence.
fn drive(cell: &ArcSwap<CoreSnapshot>, node: &mut Synthetic, steps: u64, every: Duration) -> u64 {
    for _ in 0..steps {
        cell.store(Arc::new(node.step()));
        thread::sleep(every);
    }
    node.sequence
}

// ---------------------------------------------------------------------------------------------
// The properties.
// ---------------------------------------------------------------------------------------------

/// **With both rows off, nothing changes byte for byte.** An older client's `Subscribe` and this build's `SubscribeWith`
/// are each written the publisher's own bytes for every frame they get, and those are the bytes of the frame `project`
/// makes of that snapshot.
#[test]
fn with_the_rows_off_every_subscriber_is_written_the_publishers_own_bytes() {
    vike_log::test_init();
    let mut node = Synthetic::new(2, 4);
    let cell = Arc::new(ArcSwap::from_pointee(node.step()));
    let (publisher, addr) = spawn_node(Arc::clone(&cell), ObserverStream::default());
    let mut older = RawSubscriber::connect(addr, &plain());
    let mut newer = RawSubscriber::connect(addr, &asked(true, true, false));
    let mut framed: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    for _ in 0..12 {
        publish_recorded(&cell, &publisher, &mut node, &mut framed);
    }
    for subscriber in [&mut older, &mut newer] {
        let frames = subscriber.until(node.sequence);
        assert!(frames.len() >= 2, "{} frames", frames.len());
        for (bytes, wire) in frames {
            if let Some(expected) = framed.get(&wire.sequence) {
                assert_eq!(
                    &bytes, expected,
                    "frame {} differs from the publisher's",
                    wire.sequence
                );
                assert_ne!(bytes[4], COMPRESSED_BODY_MARKER);
            }
        }
    }
    publisher.shutdown();
}

/// **With the cap on, a subscriber gets at most N frames a second, the LATEST state, and every event line.** The node
/// publishes about a hundred times a second, one event line each (an ack, a fill, a refusal), and halts at the end; a
/// four-a-second subscriber (an OLDER client's `Subscribe`, so the cap reaches it too) is sent no more than the cap
/// allows, its last frame is the node's last state (the halt, every fill in the order registry), and every line the
/// node ever put in its 64-line ring reached it in some frame.
#[test]
fn the_cap_sends_at_most_n_frames_a_second_the_latest_state_and_every_event_line() {
    vike_log::test_init();
    let mut node = Synthetic::new(1, 10);
    let cell = Arc::new(ArcSwap::from_pointee(node.step()));
    let (publisher, addr) =
        spawn_node(Arc::clone(&cell), ObserverStream { frames_per_second: 4, compression: false });
    let mut older = RawSubscriber::connect(addr, &plain());
    let started = Instant::now();
    let reader = thread::spawn(move || {
        let mut frames: Vec<(Instant, CoreSnapshotWire)> = Vec::new();
        loop {
            let (_, wire) = older.next();
            let halted = wire.trading_state == TradingState::Halted;
            frames.push((Instant::now(), wire));
            if halted {
                return frames;
            }
        }
    });
    let last = drive(&cell, &mut node, 300, Duration::from_millis(10));
    let publishing = started.elapsed();
    node.halted = true;
    cell.store(Arc::new(node.step()));
    let frames = reader.join().expect("the reader");

    let within: Vec<_> =
        frames.iter().filter(|(at, _)| at.duration_since(started) <= publishing).collect();
    let allowed = (publishing.as_secs_f64() * 4.0).ceil() as usize + 1;
    assert!(
        within.len() <= allowed,
        "{} frames in {publishing:?}, the cap allows {allowed}",
        within.len()
    );
    assert!(within.len() >= 2, "frames flowed: {}", within.len());
    for pair in frames.windows(2) {
        let gap = pair[1].0.duration_since(pair[0].0);
        assert!(gap >= Duration::from_millis(200), "two frames {gap:?} apart under a 250 ms cap");
    }

    let (_, newest) = frames.last().expect("a frame");
    assert_eq!(newest.sequence, last + 1, "the last frame is the node's last state");
    assert_eq!(newest.trading_state, TradingState::Halted);
    let filled = newest.orders.iter().filter(|o| o.status == "FILLED").count();
    assert_eq!(filled, node.orders.iter().filter(|o| o.status == OrderStatus::Filled).count());

    let heard: std::collections::HashSet<&str> =
        frames.iter().flat_map(|(_, w)| w.recent_events.iter().map(String::as_str)).collect();
    let lost: Vec<String> =
        (1..=node.sequence).map(Synthetic::line).filter(|l| !heard.contains(l.as_str())).collect();
    assert!(
        lost.is_empty(),
        "{} event lines never reached the subscriber: {:?}",
        lost.len(),
        &lost[..lost.len().min(5)]
    );
    publisher.shutdown();
}

/// **With compression on, a subscriber that asked is sent compressed bodies that decode to the frames the publisher
/// framed, and an older client is sent the plain bytes.** The real client handle reads the same state.
#[test]
fn compression_reaches_only_a_subscriber_that_asked_and_decodes_to_the_same_frames() {
    vike_log::test_init();
    let mut node = Synthetic::new(4, 3);
    let cell = Arc::new(ArcSwap::from_pointee(node.step()));
    let (publisher, addr) =
        spawn_node(Arc::clone(&cell), ObserverStream { frames_per_second: 0, compression: true });
    let mut older = RawSubscriber::connect(addr, &plain());
    let mut newer = RawSubscriber::connect(addr, &asked(true, true, false));
    let handle = vike_tradehub_client::TradehubCoreHandle::connect(addr, OBSERVE_KEY)
        .expect("the real client");
    let last = drive(&cell, &mut node, 20, Duration::from_millis(20));

    let plain_frames = older.until(last);
    for (bytes, _) in &plain_frames {
        assert_eq!(bytes[4], b'{', "an older client reads a plain body");
    }
    let by_sequence: BTreeMap<u64, CoreSnapshotWire> =
        plain_frames.into_iter().map(|(_, w)| (w.sequence, w)).collect();
    let compressed = newer.until(last);
    let mut compared = 0;
    for (bytes, wire) in &compressed {
        assert_eq!(
            bytes[4], COMPRESSED_BODY_MARKER,
            "a subscriber that asked reads a compressed body"
        );
        if let Some(plain) = by_sequence.get(&wire.sequence) {
            assert_eq!(wire, plain, "frame {} decodes to another state", wire.sequence);
            compared += 1;
        }
    }
    assert!(compared >= 1, "frames were compared");
    let deadline = Instant::now() + PATIENCE;
    while handle.snapshot().sequence != last {
        assert!(Instant::now() < deadline, "the client handle never read frame {last}");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*handle.snapshot(), by_sequence[&last], "the client handle reads the same state");
    publisher.shutdown();
}

/// The builder alone: with the rows off it hands back the publisher's own `Arc`; with the cap on it carries the lines
/// a frame it skipped held, ahead of the newest frame's ring, and only once.
#[test]
fn the_builder_carries_the_skipped_lines_only_under_the_cap() {
    let wire = |sequence: u64, events: &[&str]| {
        let mut wire = CoreSnapshotWire::empty();
        wire.sequence = sequence;
        wire.recent_events = events.iter().map(|e| e.to_string()).collect();
        wire
    };
    let log = ["a", "b", "c", "d", "e"];
    let missed = |from: u64, to: u64| (from..to).map(|s| log[s as usize].to_string()).collect();
    let first = Arc::new(PublishedFrame::new(wire(1, &["a", "b"]), 2));
    let later = Arc::new(PublishedFrame::new(wire(3, &["d", "e"]), 5));

    let mut off = ObserverPush {
        carry: false,
        codec: BodyCodec::Json,
        deflate: false,
        sent_events_end: None,
        patching: None,
        series: None,
    };
    assert!(Arc::ptr_eq(&off.frame(&first, missed).expect("framed"), first.bytes(BodyCodec::Json)));
    assert!(
        Arc::ptr_eq(&off.frame(&later, missed).expect("framed"), later.bytes(BodyCodec::Json)),
        "nothing carried"
    );

    let mut capped = ObserverPush {
        carry: true,
        codec: BodyCodec::Json,
        deflate: false,
        sent_events_end: None,
        patching: None,
        series: None,
    };
    assert!(Arc::ptr_eq(
        &capped.frame(&first, missed).expect("framed"),
        first.bytes(BodyCodec::Json)
    ));
    let carried = capped.frame(&later, missed).expect("framed");
    let Response::SnapshotFrame(back) =
        read_frame::<_, Response>(&mut &carried[..]).expect("a frame")
    else {
        panic!("not a frame")
    };
    assert_eq!(back.recent_events, ["c", "d", "e"], "the skipped line rides ahead of the ring");
    assert!(
        Arc::ptr_eq(&capped.frame(&later, missed).expect("framed"), later.bytes(BodyCodec::Json)),
        "and only once"
    );
}

/// `response` as the JSON frame a node writes it in: what a rebuilt state is held to.
fn json_frame(response: &Response) -> Vec<u8> {
    let mut frame = Vec::new();
    write_frame(&mut frame, response).expect("frames");
    frame
}

/// The publisher's frame of `snap`, as `run_publisher` makes it, its event serial supplied.
fn published_of(snap: &CoreSnapshot, events_end: u64) -> Arc<PublishedFrame> {
    Arc::new(PublishedFrame::new(publish::project(snap, None, &[]), events_end))
}

/// **Patches rebuild the very state a whole frame carries, bit for bit, over a long run.** Two thousand publishes of a
/// node whose four series close a bar every seven steps (the forming bar of each moving every step), whose orders arrive
/// and fill, whose position grows and whose event ring rolls; the writer is handed one publish in `k` (`k` from 1 to 4,
/// as the cap and the newest skip), and after each frame the reader's rebuilt snapshot serializes to exactly the bytes of
/// the publisher's whole frame of that publish. After the first, every frame is a patch, and far smaller than the whole.
#[test]
fn patches_rebuild_the_very_state_a_whole_frame_carries_over_a_long_run() {
    let mut node = Synthetic::new(4, 7);
    let shape = PushShape { shaped: true, newest: true, patches: true, ..PushShape::default() };
    let mut push = ObserverPush {
        carry: false,
        codec: BodyCodec::Json,
        deflate: false,
        sent_events_end: None,
        patching: shape.patches.then(Patching::default),
        series: None,
    };
    let mut receiver = ObserveReceiver::default();
    let (mut whole, mut patched, mut frames) = (0usize, 0usize, 0usize);
    for step in 0..2_000u64 {
        let snap = node.step();
        if !step.is_multiple_of(1 + step % 4) {
            continue;
        }
        let published = published_of(&snap, node.sequence);
        let frame = push.frame(&published, |_, _| Vec::new()).expect("framed");
        let body = read_frame_raw(&mut &frame[..]).expect("a whole frame");
        let response = decode_response(&body, BodyCodec::Json).expect("decodes");
        let Response::SnapshotPatch(sent) = &response else {
            panic!("not a patch frame: {response:?}")
        };
        if frames == 0 {
            assert_eq!(sent.base, None, "the first frame is whole");
        } else {
            assert_eq!(sent.base, Some(sent.frame - 1), "each patch names the frame before it");
            patched = patched.max(frame.len());
        }
        whole = whole.max(published.bytes(BodyCodec::Json).len());
        frames += 1;
        let Received::Snapshot(rebuilt) = receiver.receive(response) else {
            panic!("frame {frames} did not rebuild a state")
        };
        let mut again = Vec::new();
        write_frame(&mut again, &Response::SnapshotFrame(rebuilt)).expect("frames");
        assert_eq!(
            again,
            **published.bytes(BodyCodec::Json),
            "the rebuilt state of publish {} differs",
            snap.sequence
        );
    }
    assert!(frames > 500, "{frames} frames");
    assert!(
        patched * 10 < whole,
        "the largest patch frame {patched} B against a whole frame of {whole} B"
    );
}

/// **A patched subscriber rebuilds the publisher's own state, a gap brings a whole frame, and an older client beside it
/// is written the publisher's bytes.** The patched reader drops one patch unread, as a reader that missed it would: the
/// next patch names a frame it does not hold, it asks for a whole frame, and the node's next frame is whole.
#[test]
fn a_patched_subscriber_rebuilds_the_state_and_a_gap_brings_a_whole_frame() {
    vike_log::test_init();
    let mut node = Synthetic::new(3, 5);
    let cell = Arc::new(ArcSwap::from_pointee(node.step()));
    let (publisher, addr) = spawn_node(Arc::clone(&cell), ObserverStream::default());
    let mut older = RawSubscriber::connect(addr, &plain());
    let mut patched = RawSubscriber::connect(addr, &asked(true, false, true));
    let mut framed: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let check = |frames: Vec<(Vec<u8>, CoreSnapshotWire)>, framed: &BTreeMap<u64, Vec<u8>>| {
        for (_, wire) in frames {
            let mut again = Vec::new();
            write_frame(&mut again, &Response::SnapshotFrame(Box::new(wire.clone())))
                .expect("frames");
            if let Some(expected) = framed.get(&wire.sequence) {
                assert_eq!(&again, expected, "the rebuilt state of {} differs", wire.sequence);
            }
        }
    };
    for _ in 0..5 {
        publish_recorded(&cell, &publisher, &mut node, &mut framed);
    }
    check(patched.until(node.sequence), &framed);
    assert_eq!(patched.patches.first(), Some(&None), "the first frame is whole");
    assert!(
        patched.patches[1..].iter().all(Option::is_some),
        "then patches: {:?}",
        patched.patches
    );

    patched.drop_next_patch = true;
    let before_gap = patched.patches.len();
    for _ in 0..8 {
        publish_recorded(&cell, &publisher, &mut node, &mut framed);
        thread::sleep(Duration::from_millis(30));
    }
    check(patched.until(node.sequence), &framed);
    let after_gap = &patched.patches[before_gap..];
    assert!(
        after_gap.iter().skip(1).any(Option::is_none),
        "the gap brought a whole frame: {after_gap:?}"
    );

    for (bytes, wire) in older.until(node.sequence) {
        if let Some(expected) = framed.get(&wire.sequence) {
            assert_eq!(&bytes, expected, "the older client's frame {} differs", wire.sequence);
        }
    }
    publisher.shutdown();
}

/// **A subscriber that comes after a spell with none is sent the current state whole, then patches, and a second one
/// beside it a stream of its own.** The node publishes to nobody first, so nothing is framed and no frame can be the base
/// of a patch nobody was sent; a patched subscriber then comes, and its first frame is whole and the node's state as it
/// stands; a second patched one comes later and starts whole too. Every state either rebuilds is the publisher's own
/// frame of that publish, byte for byte, and after each one's first frame every frame is a patch.
#[test]
fn a_late_subscriber_is_sent_the_current_state_whole_then_patches_and_a_second_its_own_stream() {
    vike_log::test_init();
    let mut node = Synthetic::new(3, 5);
    let cell = Arc::new(ArcSwap::from_pointee(node.step()));
    let (publisher, addr) = spawn_node(Arc::clone(&cell), ObserverStream::default());
    drive(&cell, &mut node, 20, Duration::from_millis(5));
    thread::sleep(Duration::from_millis(100));
    assert!(publisher.latest_frame().is_none(), "nobody listens: nothing is framed");

    let mut framed: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let check = |frames: &[(Vec<u8>, CoreSnapshotWire)], framed: &BTreeMap<u64, Vec<u8>>| {
        for (_, wire) in frames {
            let mut again = Vec::new();
            write_frame(&mut again, &Response::SnapshotFrame(Box::new(wire.clone())))
                .expect("frames");
            assert_eq!(framed.get(&wire.sequence), Some(&again), "the state of {}", wire.sequence);
        }
    };
    let mut first = RawSubscriber::connect(addr, &asked(false, false, true));
    let joined = first.next();
    assert_eq!(joined.1.sequence, node.sequence, "the state as it stands, not an older one");
    let latest = publisher.latest_frame().expect("framed for the subscriber");
    framed.insert(latest.wire.sequence, latest.bytes(BodyCodec::Json).to_vec());
    check(&[joined], &framed);
    for _ in 0..5 {
        publish_recorded(&cell, &publisher, &mut node, &mut framed);
    }
    check(&first.until(node.sequence), &framed);

    let mut second = RawSubscriber::connect(addr, &asked(false, false, true));
    let joined = second.next();
    assert_eq!(joined.1.sequence, node.sequence, "the second one's state as it stands");
    check(&[joined], &framed);
    for _ in 0..5 {
        publish_recorded(&cell, &publisher, &mut node, &mut framed);
    }
    for subscriber in [&mut first, &mut second] {
        check(&subscriber.until(node.sequence), &framed);
        assert_eq!(subscriber.patches.first(), Some(&None), "the first frame is whole");
        assert!(
            subscriber.patches[1..].iter().all(Option::is_some),
            "then patches: {:?}",
            subscriber.patches
        );
    }
    publisher.shutdown();
}

fn key(symbol: &str) -> SeriesKeyWire {
    SeriesKeyWire { venue: "binance".into(), symbol: symbol.into(), interval: "1m".into() }
}

/// **A subscription filters BARS and nothing else.** A patched reader that shows one series of three is sent that
/// series' bars and the other two by their keys with no bar, and its orders, fills, positions, events and trading state
/// (a halt included) are the very ones an older client is sent beside it; saying it shows every series again brings the
/// whole state back, value for value.
#[test]
fn a_subscription_filters_the_bars_and_never_the_orders_fills_positions_or_halts() {
    vike_log::test_init();
    let mut node = Synthetic::new(3, 4);
    let cell = Arc::new(ArcSwap::from_pointee(node.step()));
    let (publisher, addr) = spawn_node(Arc::clone(&cell), ObserverStream::default());
    let mut older = RawSubscriber::connect(addr, &plain());
    let shown = Request::SubscribeWith(ObserveSubscriptionWire {
        topics: vec![Topic::All],
        newest: true,
        compressed: false,
        patches: true,
        series: Some(vec![key("SYMBOL1USDT")]),
    });
    let mut one = RawSubscriber::connect(addr, &shown);
    let mut framed = BTreeMap::new();
    for _ in 0..20 {
        publish_recorded(&cell, &publisher, &mut node, &mut framed);
    }
    node.halted = true;
    publish_recorded(&cell, &publisher, &mut node, &mut framed);

    let whole: BTreeMap<u64, CoreSnapshotWire> =
        older.until(node.sequence).into_iter().map(|(_, w)| (w.sequence, w)).collect();
    let (_, filtered) = one.until(node.sequence).pop().expect("a frame");
    let full = &whole[&filtered.sequence];
    assert_eq!(filtered.trading_state, TradingState::Halted, "the halt flows");
    assert_eq!(filtered.orders, full.orders, "every order and fill flows");
    assert_eq!(filtered.positions, full.positions, "the positions flow");
    assert_eq!(filtered.recent_events, full.recent_events, "the events flow");
    assert_eq!(filtered.venues, full.venues, "the accounts flow");
    let keys = |w: &CoreSnapshotWire| w.bars.iter().map(|s| s.symbol.clone()).collect::<Vec<_>>();
    assert_eq!(keys(&filtered), keys(full), "every series is still named");
    for (series, whole_series) in filtered.bars.iter().zip(&full.bars) {
        if series.symbol == "SYMBOL1USDT" {
            assert_eq!(series, whole_series, "the series shown carries its bars");
            assert_eq!(series.closed.len(), 300);
        } else {
            assert!(
                series.closed.is_empty() && series.forming.is_none(),
                "{} sends no bar",
                series.symbol
            );
        }
    }

    // Every series again: the next frame rebuilds the whole state.
    write_frame(&mut one.stream, &Request::ObserveSeries { series: None }).expect("say it");
    node.halted = false;
    for _ in 0..3 {
        publish_recorded(&cell, &publisher, &mut node, &mut framed);
        thread::sleep(Duration::from_millis(30));
    }
    let (_, again) = one.until(node.sequence).pop().expect("a frame");
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &Response::SnapshotFrame(Box::new(again.clone()))).expect("frames");
    assert_eq!(Some(&bytes), framed.get(&again.sequence), "every series, value for value");
    for (bytes, wire) in older.until(node.sequence) {
        if let Some(expected) = framed.get(&wire.sequence) {
            assert_eq!(&bytes, expected, "the older client's frame {} differs", wire.sequence);
        }
    }
    publisher.shutdown();
}

/// The patch builder this writer had before the typed diff (until 2026-11-19): the tree of each snapshot, diffed against
/// the tree it sent last. The reference the typed builder's bytes are held to.
#[derive(Default)]
struct TreePatching {
    sent: Option<serde_json::Value>,
    frame: u64,
}

impl TreePatching {
    fn next(&mut self, wire: &CoreSnapshotWire) -> Response {
        let state = serde_json::to_value(wire).expect("a tree");
        let (base, patch) = match self.sent.take() {
            Some(sent) => (
                Some(self.frame),
                observe_patch::diff(&sent, &state).unwrap_or_else(|| serde_json::json!({})),
            ),
            None => (None, state.clone()),
        };
        self.frame += 1;
        self.sent = Some(state);
        Response::SnapshotPatch(Box::new(SnapshotPatchWire { frame: self.frame, base, patch }))
    }
}

/// `wire` as a subscriber showing `shown` is sent it: every other series by its key, with no bar (a copy, as the writer
/// before the typed diff made one; `None` shows every series and borrows).
fn shown_only<'a>(
    wire: &'a CoreSnapshotWire,
    shown: Option<&[SeriesKeyWire]>,
) -> Cow<'a, CoreSnapshotWire> {
    let Some(shown) = shown else { return Cow::Borrowed(wire) };
    let mut wire = wire.clone();
    for series in &mut wire.bars {
        let key = SeriesKeyWire {
            venue: series.venue.clone(),
            symbol: series.symbol.clone(),
            interval: series.interval.clone(),
        };
        if !shown.contains(&key) {
            series.closed = Vec::new();
            series.forming = None;
        }
    }
    Cow::Owned(wire)
}

/// **The typed patch frame is the tree patch frame, byte for byte, over a long run**: the run of
/// [`patches_rebuild_the_very_state_a_whole_frame_carries_over_a_long_run`] over a node holding fifty ended orders, one
/// publish in `k` handed to each writer (JSON and msgpack, every series or one shown), and every frame the typed writer
/// writes is the very frame the tree builder ([`TreePatching`]) writes for the same publish: a whole one first, then
/// patches where orders arrive, fill and shift, bars close and form, the ring rolls and the position grows. MUTATION
/// PROOF: make `observe_patch`'s typed diff skip the orders (or the closed bars) and this fails.
#[test]
fn the_typed_patch_frame_is_the_tree_patch_frame_over_a_long_run() {
    let mut node = Synthetic::new(4, 7).with_ended_orders(50);
    let one = [key("SYMBOL1USDT")];
    let mut writers = Vec::new();
    for codec in [BodyCodec::Json, BodyCodec::Msgpack] {
        for shown in [None, Some(&one[..])] {
            let push = ObserverPush {
                carry: false,
                codec,
                deflate: false,
                sent_events_end: None,
                patching: Some(Patching::default()),
                series: shown.map(|keys| keys.iter().cloned().collect()),
            };
            writers.push((push, TreePatching::default(), shown));
        }
    }
    let mut frames = 0;
    for step in 0..1_000u64 {
        let snap = node.step();
        if !step.is_multiple_of(1 + step % 4) {
            continue;
        }
        let published = published_of(&snap, node.sequence);
        for (push, tree, shown) in &mut writers {
            let typed = push.frame(&published, |_, _| Vec::new()).expect("framed");
            let reference =
                framed(push.codec, false, &tree.next(&shown_only(&published.wire, *shown)))
                    .expect("framed");
            assert!(
                *typed == reference,
                "{:?}, shown {shown:?}: the frame of publish {} differs",
                push.codec,
                snap.sequence
            );
        }
        frames += 1;
    }
    assert!(frames > 250, "{frames} frames");
}

/// **The measurement of a patch frame on a node up for hours** (a lane's, `--release --ignored --nocapture`): 16 series
/// of 300 bars and 2,000 ended orders (the CI box on 2026-11-19 held 2,023), the time a patched msgpack writer takes to build
/// one frame, the tree builder of before ([`TreePatching`]: the snapshot's tree, its diff, the body) against the typed one
/// ([`ObserverPush::frame`]), every series and one shown (the desktop's). Each frame of the two is the same bytes.
/// Medians over 200 publishes, the first (whole) frame not counted.
#[test]
#[ignore = "a measurement: a few seconds of framing; run on a lane with --release --nocapture"]
fn measure_patch_frame_build_with_ended_orders() {
    let mut node = Synthetic::new(16, 900).with_ended_orders(2_000);
    let published: Vec<Arc<PublishedFrame>> =
        (0..201).map(|_| published_of(&node.step(), node.sequence)).collect();
    let whole = published[0].bytes(BodyCodec::Msgpack).len();
    println!(
        "a patch frame, msgpack: 16 series x 300 bars, {} orders ({whole} B whole frame); median of 200, µs",
        published[0].wire.orders.len()
    );
    let one = [key("SYMBOL0USDT")];
    for (name, shown) in [("every series", None), ("one series shown", Some(&one[..]))] {
        let mut push = ObserverPush {
            carry: false,
            codec: BodyCodec::Msgpack,
            deflate: false,
            sent_events_end: None,
            patching: Some(Patching::default()),
            series: shown.map(|keys| keys.iter().cloned().collect()),
        };
        let mut tree = TreePatching::default();
        let (mut before, mut after, mut bytes) = (Vec::new(), Vec::new(), 0);
        for (n, publish) in published.iter().enumerate() {
            let started = Instant::now();
            let reference =
                framed(BodyCodec::Msgpack, false, &tree.next(&shown_only(&publish.wire, shown)))
                    .expect("framed");
            let tree_took = started.elapsed();
            let started = Instant::now();
            let typed = push.frame(publish, |_, _| Vec::new()).expect("framed");
            let typed_took = started.elapsed();
            assert!(*typed == reference, "{name}: the frame of publish {n} differs");
            if n > 0 {
                before.push(tree_took);
                after.push(typed_took);
                bytes = bytes.max(typed.len());
            }
        }
        let median = |mut runs: Vec<Duration>| {
            runs.sort();
            runs[runs.len() / 2].as_secs_f64() * 1e6
        };
        let (before, after) = (median(before), median(after));
        println!(
            "{name:<18} | tree builder (before) {before:>9.1} µs | typed (after) {after:>8.1} µs | {:>6.1}x | largest patch {bytes} B",
            before / after
        );
    }
}

/// **A msgpack reader rebuilds the very state a JSON reader rebuilds, over a long recorded run**: the run of
/// [`patches_rebuild_the_very_state_a_whole_frame_carries_over_a_long_run`] (two thousand publishes, one in `k` handed
/// to the writers), written by four writers per connection form (whole frames or patches, plain or compressed). After
/// every frame each reader's state, framed as JSON, is the publisher's own JSON frame of that publish, byte for byte. A
/// msgpack writer's body is msgpack or, compressed, msgpack deflated behind its own magic, every frame, whole or patch; a
/// JSON writer's is JSON or deflated JSON, as before. MUTATION PROOF: send a compressed msgpack subscriber the codec's
/// LZ4 form, or deflate a JSON one behind the msgpack magic, and this fails.
///
/// The eight writers share nothing but the publishes, so each runs on its own thread, handed every publish in order
/// (one writer alone took 70-83 s of a CI run's last minute, when it ran them one after another).
#[test]
fn msgpack_rebuilds_the_very_state_json_rebuilds_over_a_long_run() {
    let frames = std::thread::scope(|scope| {
        let mut lanes = Vec::new();
        for connection in [BodyCodec::Json, BodyCodec::Msgpack] {
            for (patches, compressed) in
                [(false, false), (false, true), (true, false), (true, true)]
            {
                let (lane, publishes) =
                    std::sync::mpsc::sync_channel::<(u64, Arc<PublishedFrame>)>(16);
                lanes.push(lane);
                scope.spawn(move || {
                    let (codec, deflate) = ObserverPush::form(connection, compressed);
                    let mut push = ObserverPush {
                        carry: false,
                        codec,
                        deflate,
                        sent_events_end: None,
                        patching: patches.then(Patching::default),
                        series: None,
                    };
                    let mut receiver = ObserveReceiver::default();
                    for (sequence, published) in publishes {
                        let frame = push.frame(&published, |_, _| Vec::new()).expect("framed");
                        let body = read_frame_raw(&mut &frame[..]).expect("a whole frame");
                        let form = BodyCodec::of(&body);
                        let deflated_msgpack = body.starts_with(&MSGPACK_DEFLATE_BODY_MAGIC);
                        let fits = match (connection, compressed) {
                            (BodyCodec::Json, false) => form == BodyCodec::Json && body[0] == b'{',
                            (BodyCodec::Json, true) => {
                                body[0] == COMPRESSED_BODY_MARKER && !deflated_msgpack
                            }
                            (_, false) => form == BodyCodec::Msgpack,
                            (_, true) => deflated_msgpack,
                        };
                        assert!(
                            fits,
                            "{connection:?} compressed={compressed}: a body opening {:?}",
                            &body[..4]
                        );
                        let response = decode_response(&body, connection).expect("decodes");
                        let Received::Snapshot(rebuilt) = receiver.receive(response) else {
                            panic!("{connection:?}: publish {sequence} rebuilt no state")
                        };
                        assert_eq!(
                            json_frame(&Response::SnapshotFrame(rebuilt)),
                            **published.bytes(BodyCodec::Json),
                            "{connection:?} compressed={compressed}: the rebuilt state of publish {sequence} differs",
                        );
                    }
                });
            }
        }
        let mut node = Synthetic::new(4, 7);
        let mut frames = 0;
        for step in 0..2_000u64 {
            let snap = node.step();
            if !step.is_multiple_of(1 + step % 4) {
                continue;
            }
            let published = published_of(&snap, node.sequence);
            for lane in &lanes {
                // A writer that panicked has hung up; the scope fails the test with its panic.
                let _ = lane.send((snap.sequence, Arc::clone(&published)));
            }
            frames += 1;
        }
        frames
    });
    assert!(frames > 500, "{frames} frames");
}

/// **Over a real stream, an old client keeps JSON and a client of this build reads msgpack, and both hold the node's
/// state.** Compression on: a client from before the shaped subscription is written the publisher's plain JSON bytes; a
/// desktop of 2026-11-18 (a JSON `SubscribeWith`, patches, compressed) is written deflate; this build's subscriber (the
/// same subscription in msgpack) is written deflated msgpack; the real client handle speaks msgpack to this node. Every
/// state each one reads, framed as JSON, is the publisher's own frame of that publish.
#[test]
fn over_a_real_stream_an_old_client_keeps_json_and_a_new_one_reads_msgpack() {
    vike_log::test_init();
    let mut node = Synthetic::new(3, 4);
    let cell = Arc::new(ArcSwap::from_pointee(node.step()));
    let compressed = ObserverStream { frames_per_second: 0, compression: true };
    let (publisher, addr) = spawn_node(Arc::clone(&cell), compressed);
    let mut older = RawSubscriber::connect(addr, &plain());
    let mut json = RawSubscriber::connect(addr, &asked(true, true, true));
    let mut msgpack = RawSubscriber::connect_in(addr, &asked(true, true, true), BodyCodec::Msgpack);
    let handle = vike_tradehub_client::TradehubCoreHandle::connect(addr, OBSERVE_KEY)
        .expect("the real client");
    let mut framed: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    for _ in 0..20 {
        publish_recorded(&cell, &publisher, &mut node, &mut framed);
    }
    let readers = [("older", &mut older), ("json", &mut json), ("msgpack", &mut msgpack)];
    for (who, subscriber) in readers {
        let frames = subscriber.until(node.sequence);
        assert!(frames.len() >= 2, "{who}: {} frames", frames.len());
        for (bytes, wire) in frames {
            let body = &bytes[4..];
            let deflated_msgpack = body.starts_with(&MSGPACK_DEFLATE_BODY_MAGIC);
            let fits = match who {
                "older" => body[0] == b'{',
                "json" => body[0] == COMPRESSED_BODY_MARKER && !deflated_msgpack,
                _ => deflated_msgpack,
            };
            assert!(fits, "{who}: a body opening {:#04x}", body[0]);
            let sequence = wire.sequence;
            let again = json_frame(&Response::SnapshotFrame(Box::new(wire)));
            if let Some(expected) = framed.get(&sequence) {
                assert_eq!(&again, expected, "{who}: the state of {sequence}");
            }
        }
    }
    let deadline = Instant::now() + PATIENCE;
    while handle.snapshot().sequence != node.sequence {
        assert!(Instant::now() < deadline, "the handle never read {}", node.sequence);
        thread::sleep(Duration::from_millis(5));
    }
    let held = json_frame(&Response::SnapshotFrame(Box::new((*handle.snapshot()).clone())));
    assert_eq!(Some(&held), framed.get(&node.sequence), "the client handle holds the node's state");
    publisher.shutdown();
}

/// **Each request is answered in the form it came in, on one connection**: a msgpack `Snapshot` gets a msgpack frame of
/// the node's state, a JSON `Ping` after it the JSON `Pong` it always got, a msgpack `Ping` a msgpack one, an LZ4-form
/// `Snapshot` an LZ4 frame (and an LZ4-form `Ping` a plain msgpack `Pong`, which LZ4 would not shrink), and a
/// well-framed msgpack body that is no request a msgpack refusal, the connection kept.
#[test]
fn a_request_is_answered_in_the_codec_it_came_in() {
    vike_log::test_init();
    let mut node = Synthetic::new(1, 4);
    let cell = Arc::new(ArcSwap::from_pointee(node.step()));
    let (publisher, addr) = spawn_node(Arc::clone(&cell), ObserverStream::default());
    let mut stream = authed(addr);
    let ask = |stream: &mut TcpStream, codec: BodyCodec, request: &Request| {
        write_frame_as(stream, codec, request).expect("ask");
        read_frame_raw(stream).expect("an answer")
    };
    let body = ask(&mut stream, BodyCodec::Msgpack, &Request::Snapshot);
    assert_eq!(BodyCodec::of(&body), BodyCodec::Msgpack);
    let Ok(Response::SnapshotFrame(wire)) = decode_body(&body) else { panic!("not a frame") };
    assert_eq!(wire.sequence, node.sequence, "the node's state");
    let body = ask(&mut stream, BodyCodec::Json, &Request::Ping);
    assert_eq!(body, br#""Pong""#, "JSON, byte for byte as before");
    let body = ask(&mut stream, BodyCodec::Msgpack, &Request::Ping);
    assert_eq!(BodyCodec::of(&body), BodyCodec::Msgpack);
    assert_eq!(decode_body::<Response>(&body).ok(), Some(Response::Pong));
    // The thin-link form is answered in kind where LZ4 shrinks the answer.
    let body = ask(&mut stream, BodyCodec::MsgpackLz4, &Request::Snapshot);
    assert_eq!(BodyCodec::of(&body), BodyCodec::MsgpackLz4);
    let Ok(Response::SnapshotFrame(wire)) = decode_body(&body) else { panic!("not a frame") };
    assert_eq!(wire.sequence, node.sequence, "the node's state, in LZ4");
    let body = ask(&mut stream, BodyCodec::MsgpackLz4, &Request::Ping);
    assert_eq!(BodyCodec::of(&body), BodyCodec::Msgpack, "a Pong LZ4 cannot shrink");
    // `{"Bad": nil}`: a well-framed msgpack body naming no request.
    let mut bad = MSGPACK_BODY_MAGIC.to_vec();
    bad.extend_from_slice(&[0x81, 0xa3, b'B', b'a', b'd', 0xc0]);
    write_frame_raw(&mut stream, &bad).expect("write");
    let body = read_frame_raw(&mut stream).expect("a refusal");
    assert_eq!(BodyCodec::of(&body), BodyCodec::Msgpack);
    let refusal: Response = decode_body(&body).expect("decodes");
    assert!(matches!(&refusal, Response::Error(e) if e.contains("undecodable")), "{refusal:?}");
    let body = ask(&mut stream, BodyCodec::Json, &Request::Ping);
    assert_eq!(decode_body::<Response>(&body).ok(), Some(Response::Pong), "the connection kept");
    publisher.shutdown();
}

/// **The measurement** (a lane's, `--ignored --nocapture`): bytes a second to one subscriber of a synthetic node shaped
/// like the CI box's on 2026-11-18 (16 series of 300 one-minute bars, every forming bar moving, an event line, an order
/// every few frames, about 15 frames a second), per settings rows and per client: today's (`SubscribeNewest`), and the
/// shaped subscriptions without patches, with patches, and with patches showing one series, each in JSON (a desktop from
/// before msgpack) and in msgpack (this build). Steady state: the first second (the whole first frame) is not counted.
#[test]
#[ignore = "a measurement: about 40 s of a synthetic node at 15 frames a second; run on a lane with --nocapture"]
fn measure_observer_stream_bytes_per_second() {
    const WINDOW: Duration = Duration::from_secs(6);
    const WARM_UP: Duration = Duration::from_secs(1);
    let node = Arc::new(Mutex::new(Synthetic::new(16, 900)));
    let first = node.lock().expect("node").step();
    let cell = Arc::new(ArcSwap::from_pointee(first));
    let driving = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let driver = {
        let (node, cell, driving) = (Arc::clone(&node), Arc::clone(&cell), Arc::clone(&driving));
        thread::spawn(move || {
            while driving.load(std::sync::atomic::Ordering::Relaxed) {
                let snap = node.lock().expect("node").step();
                cell.store(Arc::new(snap));
                thread::sleep(Duration::from_millis(66));
            }
        })
    };
    let shown = Request::SubscribeWith(ObserveSubscriptionWire {
        topics: vec![Topic::All],
        newest: true,
        compressed: true,
        patches: true,
        series: Some(vec![key("SYMBOL0USDT")]),
    });
    let clients: [(&str, Request, BodyCodec); 7] = [
        (
            "today's frames (SubscribeNewest)",
            Request::SubscribeNewest { topics: vec![Topic::All] },
            BodyCodec::Json,
        ),
        ("whole frames, JSON", asked(true, true, false), BodyCodec::Json),
        ("whole frames, msgpack", asked(true, true, false), BodyCodec::Msgpack),
        ("patches, JSON", asked(true, true, true), BodyCodec::Json),
        ("patches, msgpack", asked(true, true, true), BodyCodec::Msgpack),
        ("patches + one series, JSON", shown.clone(), BodyCodec::Json),
        ("patches + one series, msgpack", shown, BodyCodec::Msgpack),
    ];
    let rows = [
        ("defaults (both off)", ObserverStream::default()),
        ("cap 2/s", ObserverStream { frames_per_second: 2, compression: false }),
        ("cap 4/s", ObserverStream { frames_per_second: 4, compression: false }),
        ("compression on", ObserverStream { frames_per_second: 0, compression: true }),
        ("cap 2/s + compression", ObserverStream { frames_per_second: 2, compression: true }),
    ];
    let mut table = Vec::new();
    for (row, stream) in rows {
        let (publisher, addr) = spawn_node(Arc::clone(&cell), stream);
        let readers: Vec<_> = clients
            .iter()
            .map(|(client, subscribe, codec)| {
                let mut subscriber = RawSubscriber::connect_in(addr, subscribe, *codec);
                let client = *client;
                thread::spawn(move || {
                    let started = Instant::now();
                    let (mut counted, mut frames) = (0u64, 0u64);
                    loop {
                        let before = subscriber.bytes;
                        subscriber.next();
                        let at = started.elapsed();
                        if at > WARM_UP + WINDOW {
                            break;
                        }
                        if at >= WARM_UP {
                            counted += subscriber.bytes - before;
                            frames += 1;
                        }
                    }
                    let seconds = WINDOW.as_secs_f64();
                    (client, counted as f64 / seconds, frames as f64 / seconds)
                })
            })
            .collect();
        for reader in readers {
            let (client, rate, frames) = reader.join().expect("a reader");
            table.push(format!(
                "{row:<24} | {client:<34} | {rate:>12.0} B/s | {frames:>5.1} frames/s"
            ));
        }
        publisher.shutdown();
    }
    driving.store(false, std::sync::atomic::Ordering::Relaxed);
    driver.join().expect("the driver");
    println!("observer stream, synthetic node: 16 series x 300 bars, ~15 publishes/s");
    for line in table {
        println!("{line}");
    }
}

/// **The measurement of ONE publish** (a lane's, `--release --ignored --nocapture`), JSON against msgpack, on the
/// node of [`measure_observer_stream_bytes_per_second`] (16 series of 300 bars): the bytes and the time of each step a
/// publish costs, per form: JSON and deflated JSON (a desktop of 2026-11-18), msgpack, msgpack deflated (what this
/// stream sends a compressed msgpack subscriber) and the codec's LZ4 form of msgpack (what it does not, measured beside it
/// to say why). `whole frame`: the publisher's one framing of the publish in that form ([`PublishedFrame::bytes`],
/// [`PublishedFrame::deflated_msgpack`]; a deflate row times the deflate of the plain frame the pass before framed);
/// `reader's decode`: a reader's decode of that whole frame; `patch`: a patched writer's frame of the next publish (the
/// tree, the diff, the body) and the reader's decode and apply of it. Medians over 100 publishes.
#[test]
#[ignore = "a measurement: a few seconds of framing; run on a lane with --release --nocapture"]
fn measure_bytes_and_time_per_publish() {
    let mut node = Synthetic::new(16, 900);
    let published: Vec<Arc<PublishedFrame>> =
        (0..201).map(|_| published_of(&node.step(), node.sequence)).collect();
    let median = |mut runs: Vec<(Duration, usize)>| {
        runs.sort();
        let (time, bytes) = runs[runs.len() / 2];
        (time.as_secs_f64() * 1e6, bytes)
    };
    let timed = |run: &mut dyn FnMut() -> usize| {
        let started = Instant::now();
        let bytes = run();
        (started.elapsed(), bytes)
    };
    println!("one publish, synthetic node: 16 series x 300 bars (median of 100; µs, bytes)");
    // (the form written, the connection it is read on, whether it is deflated)
    let forms = [
        (BodyCodec::Json, BodyCodec::Json, false),
        (BodyCodec::Json, BodyCodec::Json, true),
        (BodyCodec::Msgpack, BodyCodec::Msgpack, false),
        (BodyCodec::Msgpack, BodyCodec::Msgpack, true),
        (BodyCodec::MsgpackLz4, BodyCodec::Msgpack, false),
    ];
    for (codec, connection, deflate) in forms {
        let (mut frame, mut decode) = (Vec::new(), Vec::new());
        for publish in &published[..100] {
            // A JSON deflate runs on each subscriber's writer; a msgpack one once per publish, shared.
            frame.push(timed(&mut || match (deflate, codec) {
                (true, BodyCodec::Json) => compress_body(&publish.bytes(codec)[4..]).len(),
                (true, _) => publish.deflated_msgpack().len() - 4,
                (false, _) => publish.bytes(codec).len() - 4,
            }));
            let body = match (deflate, codec) {
                (true, BodyCodec::Json) => compress_body(&publish.bytes(codec)[4..]),
                (true, _) => publish.deflated_msgpack()[4..].to_vec(),
                (false, _) => publish.bytes(codec)[4..].to_vec(),
            };
            decode.push(timed(&mut || {
                decode_response(&body, connection).expect("decodes");
                body.len()
            }));
        }
        let mut push = ObserverPush {
            carry: false,
            codec,
            deflate,
            sent_events_end: None,
            patching: Some(Patching::default()),
            series: None,
        };
        let mut receiver = ObserveReceiver::default();
        let (mut patch, mut apply) = (Vec::new(), Vec::new());
        for (n, publish) in published[100..].iter().enumerate() {
            let mut sent = Vec::new();
            let made = timed(&mut || {
                sent = push.frame(publish, |_, _| Vec::new()).expect("framed").to_vec();
                sent.len()
            });
            let applied = timed(&mut || {
                let response = decode_response(&sent[4..], connection).expect("decodes");
                assert!(matches!(receiver.receive(response), Received::Snapshot(_)));
                sent.len()
            });
            // The first frame of a patched stream is whole.
            if n > 0 {
                patch.push(made);
                apply.push(applied);
            }
        }
        let name = if deflate { format!("{codec:?}+deflate") } else { format!("{codec:?}") };
        let row = |what: &str, (microseconds, bytes): (f64, usize)| {
            println!("{name:<15} | {what:<30} | {bytes:>9} B | {microseconds:>9.1} µs");
        };
        row(if deflate { "deflate of the plain frame" } else { "whole frame" }, median(frame));
        row("reader's decode of the frame", median(decode));
        row("patch frame (tree, diff, body)", median(patch));
        row("reader's decode and apply", median(apply));
    }
}
