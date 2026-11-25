//! The `CredentialsChanged` RELAY on a real server (decision 0287): an operator's credential verb tells the datahub
//! an account's row id, and the datahub tells the gatewayhub. Every outcome is an answer in words: relayed, a
//! gatewayhub that is down, one that serves no trade lane, and a datahub with no link at all. Loopback only.

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::server::{
    CredentialsRelay, ServerLanes, ServerTimings, credentials_relay, serve_with_lanes,
};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::gateway_status::CredentialsChangedReply;
use vike_datahub_client::proto::FEATURE_CREDENTIALS_CHANGED;
use vike_gatewayhub::testkit::data::DataLaneScript;
use vike_gatewayhub::testkit::gateway::{self, LoopbackGateway};
use vike_gatewayhub::testkit::rest::RestLaneScript;
use vike_gatewayhub_client::client::GatewayLink;

/// A real server relaying through `relay`, on an ephemeral loopback port.
fn serve(relay: Option<CredentialsRelay>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let lanes = ServerLanes { credentials_relay: relay, ..ServerLanes::default() };
        let timings = ServerTimings::PRODUCTION;
        let _ = serve_with_lanes(listener, store, None, None, None, None, lanes, None, timings);
    });
    addr
}

fn told(addr: SocketAddr, account: i64) -> CredentialsChangedReply {
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(
        client.features().iter().any(|f| f == FEATURE_CREDENTIALS_CHANGED),
        "{:?}",
        client.features()
    );
    client.credentials_changed(account).expect("answered")
}

#[test]
fn the_row_id_reaches_the_gatewayhub_and_its_answer_comes_back_whole() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&asked);
    let relay: CredentialsRelay = Arc::new(move |account| {
        seen.lock().unwrap_or_else(PoisonError::into_inner).push(account);
        CredentialsChangedReply {
            reached: true,
            reconnected: vec!["dukascopy".to_string()],
            error: None,
        }
    });
    let reply = told(serve(Some(relay)), 7);
    assert_eq!(*asked.lock().unwrap_or_else(PoisonError::into_inner), [7]);
    assert_eq!(
        reply,
        CredentialsChangedReply {
            reached: true,
            reconnected: vec!["dukascopy".to_string()],
            error: None
        }
    );
}

#[test]
fn a_gatewayhub_that_is_down_is_an_answer_in_the_gateway_text_words() {
    // Port 1: nothing listens, so the dial is refused at once.
    let dead = "127.0.0.1:1";
    let reply = told(serve(Some(credentials_relay(GatewayLink::new(dead, gateway::keys())))), 7);
    assert!(!reply.reached, "{reply:?}");
    assert!(reply.reconnected.is_empty());
    let words = reply.error.expect("the words");
    assert!(words.starts_with(&format!("gatewayhub {dead}: cannot connect")), "{words}");
}

/// A gatewayhub that runs no trade lane (here: a data-only build) runs no session to reconnect, and says so by
/// not advertising the verb: the relay refuses locally, naming the string, and sends nothing.
#[test]
fn a_gatewayhub_with_no_trade_lane_is_reached_and_named() {
    let gw = LoopbackGateway::start(DataLaneScript::new(&["binance"]), RestLaneScript::new());
    let reply = told(serve(Some(credentials_relay(gw.link()))), 7);
    assert!(reply.reached, "{reply:?}");
    assert!(reply.reconnected.is_empty());
    let words = reply.error.expect("the words");
    assert!(words.contains("gateway_credentials_changed"), "{words}");
}

#[test]
fn a_datahub_with_no_gatewayhub_link_says_so() {
    let reply = told(serve(None), 7);
    assert!(!reply.reached, "{reply:?}");
    assert!(reply.error.expect("the words").contains("no gatewayhub link"));
}
