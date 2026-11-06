//! The REAL price stream on the REAL `ibapi` client (`LazyQuotes` over `IbkrFeeds`), against a loopback double of the IB
//! Gateway that speaks the API handshake, records every request it is sent and writes the ticks it is told to. What
//! these tests read is what a gateway would see: how many TCP connections, and which requests in which order.
//!
//! Nothing here reaches a real gateway. What the double answers is `ibapi` 5.2.0's own wire (`connection/common.rs`'s
//! handshake, the protobuf frames of `market_data/realtime/common/encoders.rs`), so what a real gateway answers to the
//! same requests is still the first attended run's to say.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink};
use vike_model::DataDelay;

use crate::config::IbkrConfig;
use crate::data_door::test_support::{Call, RecSink};
use crate::quote_handle::LazyQuotes;
use crate::transport::socket::socket_tests::silent_gateway;

const AAPL: &str = "AAPL.SMART.USD";
const AAPL_PINNED: &str = "AAPL.SMART.USD.STK.265598";
const MSFT: &str = "MSFT.SMART.USD";
const TSLA: &str = "TSLA.SMART.USD";

/// How long a test waits for something the client does on its own threads.
const WAIT: Duration = Duration::from_secs(20);

/// The offset of a protobuf frame's message id (`ibapi`'s `PROTOBUF_MSG_ID`).
const PROTOBUF: i32 = 200;
/// `ibapi`'s `OutgoingMessages`: what the gateway is asked.
const REQ_MKT_DATA: i32 = 1;
const CANCEL_MKT_DATA: i32 = 2;
const REQ_MKT_DATA_TYPE: i32 = 59;
/// `ibapi`'s `IncomingMessages`: what the gateway answers.
const TICK_PRICE: i32 = 1;
const NEXT_VALID_ID: i32 = 9;
const MANAGED_ACCOUNTS: i32 = 15;
const MARKET_DATA_TYPE: i32 = 58;
/// IB's tick types.
const BID: i32 = 1;
const ASK: i32 = 2;
const DELAYED_BID: i32 = 66;
const DELAYED_ASK: i32 = 67;
/// IB's market-data types, on the wire both ways.
const FROZEN: i32 = 2;
const DELAYED: i32 = 3;

/// One request a client sent: the message and its first field (the request id, or the market-data type).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Request {
    kind: i32,
    first: i32,
}

fn write_frame(socket: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    let length = u32::try_from(body.len()).expect("a small frame").to_be_bytes();
    socket.write_all(&length)?;
    socket.write_all(body)
}

fn read_frame(socket: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    socket.read_exact(&mut length)?;
    let mut body = vec![0u8; u32::from_be_bytes(length) as usize];
    socket.read_exact(&mut body)?;
    Ok(body)
}

fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// A protobuf int32 field.
fn int_field(tag: u8, value: i32, out: &mut Vec<u8>) {
    out.push(tag << 3);
    varint(u64::try_from(value).expect("a positive field"), out);
}

/// A protobuf frame of message `kind`.
fn frame(kind: i32, proto: &[u8]) -> Vec<u8> {
    let mut body = (kind + PROTOBUF).to_be_bytes().to_vec();
    body.extend_from_slice(proto);
    body
}

/// A request's first field: the protobuf int32 at tag 1 that `reqMktData`, `cancelMktData` and `reqMarketDataType` each
/// lead with, 0 for a request that leads with anything else.
fn first_int(proto: &[u8]) -> i32 {
    if proto.first() != Some(&0x08) {
        return 0;
    }
    let (mut value, mut shift) = (0u64, 0);
    for byte in &proto[1..] {
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    i32::try_from(value).expect("an int32")
}

/// What the double remembers, shared with its threads.
#[derive(Default)]
struct Seen {
    accepted: AtomicUsize,
    requests: Mutex<Vec<Request>>,
    /// The write half of the newest connection, for the ticks a test tells it to send.
    down: Mutex<Option<TcpStream>>,
}

/// See the module doc.
struct FakeGateway {
    port: u16,
    seen: Arc<Seen>,
}

impl FakeGateway {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("bound").port();
        let seen = Arc::new(Seen::default());
        let accepting = Arc::clone(&seen);
        std::thread::spawn(move || {
            for socket in listener.incoming().flatten() {
                accepting.accepted.fetch_add(1, Ordering::SeqCst);
                let serving = Arc::clone(&accepting);
                std::thread::spawn(move || {
                    let _ = serve(socket, &serving);
                });
            }
        });
        Self { port, seen }
    }

    /// A config whose socket gateway is this double.
    fn cfg(&self) -> IbkrConfig {
        let mut cfg = silent_gateway().1;
        cfg.port = self.port;
        cfg
    }

    fn accepted(&self) -> usize {
        self.seen.accepted.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Request> {
        self.seen.requests.lock().expect("requests").clone()
    }

    /// The request ids of every request of `kind` so far.
    fn ids_of(&self, kind: i32) -> Vec<i32> {
        self.requests().into_iter().filter(|r| r.kind == kind).map(|r| r.first).collect()
    }

    /// The `n`th `reqMktData`'s request id, once it has arrived.
    fn market_data_request(&self, n: usize) -> i32 {
        until("the market-data request", || self.ids_of(REQ_MKT_DATA).get(n).copied())
    }

    /// Every `cancelMktData`'s request id, once `n` have arrived.
    fn cancels(&self, n: usize) -> Vec<i32> {
        until("the cancels", || {
            let cancels = self.ids_of(CANCEL_MKT_DATA);
            (cancels.len() >= n).then_some(cancels)
        })
    }

    fn write(&self, body: &[u8]) {
        let mut down = self.seen.down.lock().expect("down");
        let socket = down.as_mut().expect("a client is connected");
        write_frame(socket, body).expect("the client is connected");
    }

    /// One `tickPrice` of request `id`.
    fn tick(&self, id: i32, tick_type: i32, price: f64) {
        let mut proto = Vec::new();
        int_field(1, id, &mut proto);
        int_field(2, tick_type, &mut proto);
        proto.push((3 << 3) | 1);
        proto.extend_from_slice(&price.to_le_bytes());
        self.write(&frame(TICK_PRICE, &proto));
    }

    /// IB's `marketDataType` message of request `id`.
    fn data_type(&self, id: i32, market_data_type: i32) {
        let mut proto = Vec::new();
        int_field(1, id, &mut proto);
        int_field(2, market_data_type, &mut proto);
        self.write(&frame(MARKET_DATA_TYPE, &proto));
    }
}

/// One connection: the handshake, then every request recorded until the client goes.
fn serve(mut socket: TcpStream, seen: &Seen) -> std::io::Result<()> {
    let mut api = [0u8; 4];
    socket.read_exact(&mut api)?;
    assert_eq!(&api, b"API\0", "the handshake's prefix");
    let _versions = read_frame(&mut socket)?;
    write_frame(&mut socket, b"226\x0020261106 12:00:00 UTC\0")?;
    let _start_api = read_frame(&mut socket)?;

    let mut accounts = vec![0x0a, 9];
    accounts.extend_from_slice(b"DU0000000");
    write_frame(&mut socket, &frame(MANAGED_ACCOUNTS, &accounts))?;
    let mut next_id = Vec::new();
    int_field(1, 100, &mut next_id);
    write_frame(&mut socket, &frame(NEXT_VALID_ID, &next_id))?;

    *seen.down.lock().expect("down") = Some(socket.try_clone()?);
    loop {
        let body = read_frame(&mut socket)?;
        let (id, proto) = body.split_first_chunk::<4>().expect("a message id");
        seen.requests
            .lock()
            .expect("requests")
            .push(Request { kind: i32::from_be_bytes(*id) - PROTOBUF, first: first_int(proto) });
    }
}

/// Poll `found` until it answers, or fail naming `what`.
fn until<T>(what: &str, mut found: impl FnMut() -> Option<T>) -> T {
    let began = Instant::now();
    loop {
        if let Some(value) = found() {
            return value;
        }
        assert!(began.elapsed() < WAIT, "waited {WAIT:?} for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stream_over(cfg: &IbkrConfig, sink: &Arc<RecSink>) -> LazyQuotes {
    let sink: Arc<dyn LiveDataSink> = sink.clone();
    LazyQuotes::for_config(cfg.clone(), sink)
}

/// Everything the sink has been told so far, once `done` holds of it.
fn told_until(sink: &RecSink, what: &str, done: impl Fn(&[Call]) -> bool) -> Vec<Call> {
    let mut calls = Vec::new();
    until(what, || {
        calls.extend(sink.take());
        done(&calls).then_some(())
    });
    calls
}

fn quote_of(calls: &[Call], symbol: &str) -> bool {
    calls.iter().any(|call| matches!(call, Call::Quote(s, _) if s == symbol))
}

/// The labels `symbol`'s quote lane was told, in order, with whether a quote had gone out before each.
fn labels_of(calls: &[Call], symbol: &str) -> Vec<(DataDelay, bool)> {
    let mut quoted = false;
    let mut labels = Vec::new();
    for call in calls {
        match call {
            Call::Quote(s, _) if s == symbol => quoted = true,
            Call::Delay(s, lane, delay) if s == symbol => {
                assert_eq!(lane, "quotes", "the lane the label names");
                labels.push((*delay, quoted));
            }
            _ => {}
        }
    }
    labels
}

/// **One TCP connection under `data_client_id`, however many hold it** (decision 0193, the design's "one data client
/// per gateway"; IB answers a second client under an id in use with error 326). Building a stream dials nothing, N
/// symbols are one dial, and a second stream of the same gateway session (a restarted datahub's, over a session that
/// has not gone yet) joins the connection. A recorder in the datahub is less than either: it is a hold on the broker's
/// one client and reaches this client not at all (`crates/vike-datahub/src/feeds/venues.rs`'s sharing row).
#[test]
fn every_holder_of_the_gateway_session_rides_one_connection_under_the_data_client_id() {
    let gateway = FakeGateway::start();
    let cfg = gateway.cfg();
    let sink = RecSink::arc();
    let mut first = stream_over(&cfg, &sink);
    let mut second = stream_over(&cfg, &sink);
    assert_eq!(gateway.accepted(), 0, "building a stream dials nothing");

    first.subscribe_quotes(AAPL).expect("the first symbol dials");
    first.subscribe_quotes(MSFT).expect("the second rides it");
    second.subscribe_quotes(TSLA).expect("the second stream joins it");
    gateway.market_data_request(2);
    assert_eq!(gateway.accepted(), 1, "one connection for three streams of two clients");

    drop(first);
    assert_eq!(gateway.cancels(2).len(), 2, "the leaving client's two streams");
    second.subscribe_quotes(AAPL).expect("the connection outlives the client that dialled it");
    gateway.market_data_request(3);
    assert_eq!(gateway.accepted(), 1, "still the one connection");
}

/// **THE READING THE SHARING ROW RESTS ON** (decision 0193 verdict 3): releasing one quote key ends exactly that key's
/// market-data request. Another symbol's stream goes on, and so does another key of the SAME contract (a pinned
/// spelling is a request of its own at IB), on the same connection, with no gap told to anybody.
#[test]
fn releasing_one_quote_key_cancels_exactly_its_market_data_request_and_no_other() {
    let gateway = FakeGateway::start();
    let sink = RecSink::arc();
    let mut stream = stream_over(&gateway.cfg(), &sink);
    let aapl = stream.subscribe_quotes(AAPL).expect("subscribed");
    let released = gateway.market_data_request(0);
    stream.subscribe_quotes(AAPL_PINNED).expect("subscribed");
    let pinned = gateway.market_data_request(1);
    stream.subscribe_quotes(MSFT).expect("subscribed");
    let msft = gateway.market_data_request(2);

    stream.unsubscribe(aapl);
    assert_eq!(gateway.cancels(1), [released], "exactly the released key's request");

    let mut calls = Vec::new();
    for (id, symbol) in [(pinned, AAPL_PINNED), (msft, MSFT)] {
        gateway.tick(id, DELAYED_BID, 10.0);
        gateway.tick(id, DELAYED_ASK, 10.5);
        calls.extend(told_until(&sink, symbol, |calls| quote_of(calls, symbol)));
    }
    assert_eq!(gateway.ids_of(CANCEL_MKT_DATA), [released], "nobody else's request");
    assert_eq!(gateway.accepted(), 1, "the same connection");
    assert!(
        !calls.iter().any(|call| matches!(call, Call::Status(..))),
        "a release discloses nothing: {calls:?}"
    );
}

/// **The market-data type is a setting of the gateway SESSION, so it goes out before every quote subscribe** (decision
/// 0220): on this box stocks are delayed-only, and a subscribe on a session that lost the type ends with `[354]`.
#[test]
fn the_market_data_type_goes_out_right_before_every_quote_subscribe() {
    let gateway = FakeGateway::start();
    let sink = RecSink::arc();
    let mut stream = stream_over(&gateway.cfg(), &sink);
    for (n, symbol) in [AAPL, MSFT, TSLA].into_iter().enumerate() {
        stream.subscribe_quotes(symbol).expect("subscribed");
        gateway.market_data_request(n);
    }
    let requests = gateway.requests();
    for (at, request) in requests.iter().enumerate().filter(|(_, r)| r.kind == REQ_MKT_DATA) {
        assert_eq!(
            requests[at - 1],
            Request { kind: REQ_MKT_DATA_TYPE, first: DELAYED },
            "before request {request:?}: {requests:?}"
        );
    }
}

/// **Where the socket's label comes from** (decision 0140): the tick's own type and the subscription's `marketDataType`
/// message, not what was asked for, and always ahead of the first quote it labels. A delayed stream (what this box's
/// stocks are) says so before its first quote, a real-time one says that, and a frozen message relabels a stream with
/// no new price. The recorder's rule stands on exactly this order (`crates/vike-datahub/src/feeds/routing.rs`'s module
/// doc): a label that came after its quote would let that quote be stored.
#[test]
fn a_stream_states_its_label_ahead_of_its_first_quote_off_what_the_gateway_sent() {
    let gateway = FakeGateway::start();
    let sink = RecSink::arc();
    let mut stream = stream_over(&gateway.cfg(), &sink);
    stream.subscribe_quotes(AAPL).expect("subscribed");
    let delayed = gateway.market_data_request(0);
    stream.subscribe_quotes(MSFT).expect("subscribed");
    let real_time = gateway.market_data_request(1);

    gateway.data_type(delayed, DELAYED);
    gateway.tick(delayed, DELAYED_BID, 190.10);
    gateway.tick(delayed, DELAYED_ASK, 190.12);
    gateway.tick(real_time, BID, 410.00);
    gateway.tick(real_time, ASK, 410.02);
    let mut calls =
        told_until(&sink, "both quotes", |calls| quote_of(calls, AAPL) && quote_of(calls, MSFT));
    assert_eq!(labels_of(&calls, AAPL), [(DataDelay::DELAYED, false)], "{calls:?}");
    assert_eq!(labels_of(&calls, MSFT), [(DataDelay::RealTime, false)], "{calls:?}");

    gateway.data_type(real_time, FROZEN);
    calls.extend(told_until(&sink, "the frozen label", |calls| !labels_of(calls, MSFT).is_empty()));
    assert_eq!(
        labels_of(&calls, MSFT),
        [(DataDelay::RealTime, false), (DataDelay::Frozen, true)],
        "{calls:?}"
    );
}
