//! The `GatewayStatus` verb on a real server with a real gatewayhub client: a gatewayhub that is up, one that
//! is down, one that stalls, one older than the status verb, and what a read loop costs. Loopback only: a scripted
//! gatewayhub (`vike_gatewayhub::testkit`) or a socket that answers as the test says. No venue.

use std::io::Read as _;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::server::{
    GATEWAY_STATUS_WAIT, GatewayStatusLane, ServerLanes, ServerTimings, serve_with_lanes,
};
use vike_datahub::test_support::no_gateway;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::gateway_status::GatewayStatusReply;
use vike_datahub_client::proto::FEATURE_GATEWAY_STATUS;
use vike_gatewayhub::testkit::data::DataLaneScript;
use vike_gatewayhub::testkit::gateway::{self, LoopbackGateway};
use vike_gatewayhub::testkit::rest::RestLaneScript;
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::proto::{
    GATEWAYHUB_PLANE_SENTINEL, GATEWAYHUB_PROTO_VERSION, Request, Response, read_frame, write_frame,
};
use vike_hub_protocol::auth::FEATURE_AUTH;

fn lane(link: GatewayLink) -> Arc<GatewayStatusLane> {
    Arc::new(GatewayStatusLane::dialling(link))
}

/// A real server mounting `lane`, on an ephemeral loopback port.
fn serve(lane: Option<Arc<GatewayStatusLane>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let lanes = ServerLanes { gateway_status: lane, ..ServerLanes::default() };
        let timings = ServerTimings::PRODUCTION;
        let _ =
            serve_with_lanes(listener, store, None, None, None, None, lanes, None, false, timings);
    });
    addr
}

#[test]
fn a_gatewayhub_that_is_up_is_reached_and_its_report_comes_through_the_wire() {
    let gw = LoopbackGateway::start(DataLaneScript::new(&["binance"]), RestLaneScript::new());
    let addr = serve(Some(lane(gw.read_link())));
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.serves_gateway_status(), "{:?}", client.features());
    let reply = client.gateway_status().expect("answered");
    assert!(reply.reached, "{reply:?}");
    assert_eq!(reply.error, None);
    let report = reply.report.expect("a report");
    assert!(report.market_data_venues.iter().any(|v| v == "binance"), "{report:?}");
    assert!(report.budget.is_some(), "the daemon's budget rows are carried: {report:?}");
}

#[test]
fn a_gatewayhub_that_is_down_is_an_answer_in_the_gateway_text_words() {
    // Port 1: nothing listens, so the dial is refused at once, and no test's listener can be handed it
    // (a port bound and released can be).
    let dead = "127.0.0.1:1";
    let link = GatewayLink::new(dead, gateway::read_only_keys());
    let addr = serve(Some(lane(link)));
    let reply =
        DatahubClient::connect(addr).expect("handshake").gateway_status().expect("answered");
    assert!(!reply.reached, "{reply:?}");
    assert_eq!(reply.report, None, "never an empty report");
    let words = reply.error.expect("the words");
    assert!(words.starts_with(&format!("gatewayhub {dead}: cannot connect")), "{words}");
}

/// A box with no gatewayhub pair is told so, and nothing is dialled.
#[test]
fn a_link_with_no_key_says_so_and_dials_nothing() {
    let reply = lane(no_gateway()).answer();
    assert!(!reply.reached, "{reply:?}");
    let words = reply.error.expect("the words");
    assert!(words.contains("nothing was dialled"), "{words}");
}

/// ⚠ A gatewayhub that accepts and says nothing holds the ask for the handshake's whole deadline, and a reader is
/// answered inside the lane's own wait. The second read, with the first ask still out, dials nothing more.
#[test]
fn a_gatewayhub_that_stalls_is_answered_within_the_wait_and_dialled_once() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let stalled = listener.local_addr().expect("addr").to_string();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&accepted);
    thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            counted.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    let lane = lane(GatewayLink::new(stalled.clone(), gateway::read_only_keys()));
    for _ in 0..2 {
        let started = Instant::now();
        let reply = lane.answer();
        let took = started.elapsed();
        assert!(took < GATEWAY_STATUS_WAIT + Duration::from_secs(2), "{took:?}");
        assert!(!reply.reached, "{reply:?}");
        let words = reply.error.expect("the words");
        assert!(words.starts_with(&format!("gatewayhub {stalled}: no answer within")), "{words}");
    }
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "one ask at a time");
}

/// A daemon that takes the handshake and advertises no `gateway_status` is REACHED: an older gatewayhub is not one
/// the datahub cannot find, and the words say what to do. The client refuses locally, so nothing more is sent.
#[test]
fn a_gatewayhub_older_than_the_status_verb_is_reached_and_gives_no_report() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let _: Request = read_frame(&mut stream).expect("Hello");
        let features = vec![GATEWAYHUB_PLANE_SENTINEL.to_string(), FEATURE_AUTH.to_string()];
        let welcome =
            Response::Welcome { proto_version: GATEWAYHUB_PROTO_VERSION, features, nonce: [7; 32] };
        write_frame(&mut stream, &welcome).expect("Welcome");
        let scope = match read_frame::<_, Request>(&mut stream).expect("Auth") {
            Request::Auth { scope, .. } => scope,
            other => panic!("expected Auth, got {other:?}"),
        };
        write_frame(&mut stream, &Response::AuthOk { scope }).expect("AuthOk");
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).expect("read to the client's close");
        rest
    });
    let reply = lane(GatewayLink::new(addr, gateway::read_only_keys())).answer();
    assert!(reply.reached, "{reply:?}");
    assert_eq!(reply.report, None);
    assert!(reply.error.expect("the words").contains("gateway_status"));
    assert!(peer.join().expect("the peer").is_empty(), "the status verb was not sent");
}

/// A lane over a scripted ask, and the count of asks it made.
fn counting_lane(ttl: Duration) -> (Arc<GatewayStatusLane>, Arc<AtomicUsize>) {
    let asks = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asks);
    let lane = GatewayStatusLane::with_probe(
        "127.0.0.1:1",
        move || {
            counted.fetch_add(1, Ordering::SeqCst);
            GatewayStatusReply { reached: true, error: None, report: None }
        },
        ttl,
        Duration::from_secs(30),
    );
    (Arc::new(lane), asks)
}

/// ⚠ **A read loop is not a dial loop**: however many connections read at once, one ask per TTL.
#[test]
fn a_read_loop_is_one_ask_while_the_answer_is_fresh() {
    let (lane, asks) = counting_lane(Duration::from_secs(600));
    let readers: Vec<_> = (0..8)
        .map(|_| {
            let lane = Arc::clone(&lane);
            thread::spawn(move || {
                for _ in 0..20 {
                    assert!(lane.answer().reached);
                }
            })
        })
        .collect();
    for reader in readers {
        reader.join().expect("a reader");
    }
    assert_eq!(asks.load(Ordering::SeqCst), 1, "the cache served the loop");
}

#[test]
fn an_answer_past_its_ttl_is_asked_again() {
    let (lane, asks) = counting_lane(Duration::ZERO);
    assert!(lane.answer().reached && lane.answer().reached);
    assert_eq!(asks.load(Ordering::SeqCst), 2);
}

/// An ask that panics must not leave the lane "asking" for ever: that would call a live daemon stalled until a restart.
#[test]
fn a_panicking_ask_is_an_answer_and_the_next_read_asks_again() {
    let asks = AtomicUsize::new(0);
    let lane = Arc::new(GatewayStatusLane::with_probe(
        "127.0.0.1:1",
        move || {
            assert!(asks.fetch_add(1, Ordering::SeqCst) > 0, "scripted panic");
            GatewayStatusReply { reached: true, error: None, report: None }
        },
        Duration::ZERO,
        Duration::from_secs(30),
    ));
    let first = lane.answer();
    assert!(!first.reached, "{first:?}");
    assert!(first.error.expect("the words").contains("panicked"));
    assert!(lane.answer().reached, "the lane asked again");
}

/// A build fact: a server with no link still advertises and answers, in words, so it is never read as an old one.
#[test]
fn a_laneless_server_advertises_and_answers_that_it_holds_no_link() {
    let mut client = DatahubClient::connect(serve(None)).expect("handshake");
    assert!(client.features().iter().any(|f| f == FEATURE_GATEWAY_STATUS));
    let reply = client.gateway_status().expect("answered");
    assert!(!reply.reached, "{reply:?}");
    assert!(reply.error.expect("the words").contains("no gatewayhub link"));
}
