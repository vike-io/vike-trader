//! The `VenueRead` verb end-to-end (`vike-cli trade venue`): a real PAPER node
//! ([`vike_tradehub_core::build_paper_maker_core`]) + the real [`vike_tradehub::server`] whose publisher holds a
//! lease registry (a scripted [`SymbolFacts`], so the venue is a test's word), driven through the REAL client verb
//! ([`vike_tradehub_client::venue_read`]) under [`Scope::Read`] - the OBSERVE key, which is the point: an operator's
//! read of an account at its venue needs no key that can change the node (`server/venue_read.rs`'s module doc).
//!
//! What is proven:
//! - **Advertisement:** `Welcome.features` carries `"venue-read"`, so the client verb sends.
//! - **Observe suffices, Control is not needed:** both reads round-trip under the observe key, and a control peer
//!   is answered too (either scope, like `Directory`).
//! - **The account is addressed by its route key:** `DEFAULT` is the bare venue, a label is `venue#LABEL`, and the
//!   kind rides to the registry as asked.
//! - **A refusal is the operator's sentence**, an error and never an empty answer; a node with no lease registry,
//!   an unknown venue and a malformed account each say so and ask nothing.
//! - **A figure that is not a number is never written** (it would end the connection that decodes it).
//! - **The frames are pinned on the RAW bytes** the node writes, below the typed decode: the reply carries the
//!   figures and the order rows and nothing else (no credential, no key name, no funder address).
//! - **Pre-auth:** a peer that skips the handshake is refused, never answered a read.
//!
//! Grouped into the `daemon` binary: plain tests, no `#[ignore]`, no process-global mutation.

use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};

use vike_hub_proto::auth::{HubKeys, Scope};
use vike_model::{
    VenueAllowance, VenueBalance, VenueOpenOrder, VenuePosition, VenueRead, VenueReadKind,
};
use vike_tradehub::publish;
use vike_tradehub::server::symbol_admission::SymbolFacts;
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    FEATURE_VENUE_READ, FEATURE_VENUE_READ_POSITIONS, NODE_PROTO_VERSION, Request, Response,
    read_frame, read_frame_raw, write_frame,
};
use vike_tradehub_client::wire::{WireVenueRead, WireVenueReadKind};
use vike_tradehub_core::{MakerConfig, PaperMakerCore, build_paper_maker_core};

use crate::support::serve_on_loopback;

const OBSERVE_KEY: &[u8] = b"venue-read-observe-key";
const CONTROL_KEY: &[u8] = b"venue-read-control-key";

/// A lease registry that answers every route key with `answer` and remembers what it was asked.
struct Registry {
    answer: Result<VenueRead, String>,
    asked: Mutex<Vec<(String, VenueReadKind)>>,
}

impl SymbolFacts for Registry {
    fn admission(&self, _: &str, _: &str, _: &str) -> Result<vike_exec::SymbolAdmission, String> {
        Err("this suite asks for no symbol".into())
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Err("this suite asks for no price".into())
    }
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn venue_read(&self, route_key: &str, kind: VenueReadKind) -> Result<VenueRead, String> {
        self.asked.lock().unwrap().push((route_key.to_string(), kind));
        self.answer.clone()
    }
}

fn registry(answer: Result<VenueRead, String>) -> Arc<Registry> {
    Arc::new(Registry { answer, asked: Mutex::new(Vec::new()) })
}

/// A node serving `source` as its lease registry (`None`: a node with none), observe key always, control key
/// armed. The core stays alive with the returned mount.
fn spawn(source: Option<Arc<Registry>>) -> (PaperMakerCore, SocketAddr) {
    let cfg = MakerConfig::outcome_token("polymarket", "VENUE_READ_TOKEN", Some(3_000_000_000));
    let mount = build_paper_maker_core(&cfg);
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None)
        .with_symbol_facts(source.map(|s| s as Arc<dyn SymbolFacts>));
    let keys = HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    let addr = serve_on_loopback(publisher, keys, None, None, None);
    (mount, addr)
}

/// The wallet read of the 2026-10-27 order test.
fn balance() -> VenueRead {
    VenueRead::Balance(VenueBalance {
        asset: "pUSD".into(),
        balance: 20.0,
        allowances: vec![VenueAllowance {
            spender: "0xE111180000d2663C0091e4f400237545B87B996B".into(),
            name: "CTF exchange".into(),
            amount: 0.0,
            unlimited: true,
        }],
    })
}

fn open_orders() -> VenueRead {
    VenueRead::OpenOrders {
        orders: vec![VenueOpenOrder {
            venue_order_id: "0xabc".into(),
            client_order_id: None,
            symbol: "6710259892".into(),
            side: 1,
            price: Some(0.2),
            qty: 5.0,
            filled_qty: 0.0,
            status: "ACCEPTED".into(),
            ts_ms: 1_700_000_000_000,
        }],
        truncated: false,
    }
}

/// One held, resolved, losing leg of the funded wallet's real data-api body (the wire carries no address).
fn positions() -> VenueRead {
    VenueRead::Positions {
        positions: vec![VenuePosition {
            symbol: "1484359444963781501765067677755406951259550728919736807361564486189514136251"
                .into(),
            title: "Solana Up or Down - June 9, 3:40AM-3:45AM ET".into(),
            outcome: "Down".into(),
            qty: 11.11,
            avg_price: Some(0.2099),
            price: Some(0.0),
            value: Some(0.0),
            redeemable: true,
        }],
        truncated: false,
    }
}

/// A connection to `addr` authenticated under `scope`, the handshake done and nothing else sent.
fn authed_stream(addr: SocketAddr, scope: Scope, key: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, scope);
    write_frame(&mut stream, &Request::Auth { scope, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("auth reply") {
        Response::AuthOk { .. } => {}
        other => panic!("expected AuthOk, got {other:?}"),
    }
    stream
}

fn ask(venue: &str, account: &str, kind: WireVenueReadKind) -> Request {
    Request::VenueRead { venue: venue.into(), account: account.into(), kind }
}

#[test]
fn the_welcome_advertises_the_venue_read() {
    let (_mount, addr) = spawn(None);
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { features, .. } => {
            assert!(features.iter().any(|f| f == FEATURE_VENUE_READ), "{features:?}");
            assert!(features.iter().any(|f| f == FEATURE_VENUE_READ_POSITIONS), "{features:?}");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// All three reads, under the OBSERVE key, through the real client verb; the registry is asked under the
/// account's route key and the kind the operator asked.
#[test]
fn an_observe_peer_reads_a_balance_the_open_orders_and_the_positions() {
    for (answer, kind) in [
        (balance(), WireVenueReadKind::Balance),
        (open_orders(), WireVenueReadKind::OpenOrders),
        (positions(), WireVenueReadKind::Positions),
    ] {
        let source = registry(Ok(answer));
        let (_mount, addr) = spawn(Some(Arc::clone(&source)));
        let got =
            vike_tradehub_client::venue_read(addr, OBSERVE_KEY, "polymarket", "DEFAULT", kind)
                .expect("advertised and served");
        assert_eq!(got.kind(), kind);
        match got {
            WireVenueRead::Balance(b) => {
                assert_eq!((b.asset.as_str(), b.balance), ("pUSD", 20.0));
                assert!(b.allowances[0].unlimited && b.allowances[0].name == "CTF exchange");
            }
            WireVenueRead::OpenOrders { orders, truncated } => {
                assert!(!truncated);
                assert_eq!(orders.len(), 1);
                assert_eq!(
                    (orders[0].venue_order_id.as_str(), orders[0].price),
                    ("0xabc", Some(0.2))
                );
            }
            WireVenueRead::Positions { positions, truncated } => {
                assert!(!truncated);
                assert_eq!(positions.len(), 1);
                assert_eq!((positions[0].qty, positions[0].redeemable), (11.11, true));
            }
        }
        let asked = source.asked.lock().unwrap().clone();
        let want_kind = match kind {
            WireVenueReadKind::Balance => VenueReadKind::Balance,
            WireVenueReadKind::OpenOrders => VenueReadKind::OpenOrders,
            WireVenueReadKind::Positions => VenueReadKind::Positions,
        };
        assert_eq!(asked, [("polymarket".to_string(), want_kind)]);
    }
}

/// A labelled account is `venue#LABEL`: the registry is asked under the key its lease is filed by.
#[test]
fn a_labelled_account_is_asked_under_its_route_key() {
    let source = registry(Ok(balance()));
    let (_mount, addr) = spawn(Some(Arc::clone(&source)));
    vike_tradehub_client::venue_read(
        addr,
        OBSERVE_KEY,
        "hyperliquid",
        "TESTNET",
        WireVenueReadKind::Balance,
    )
    .expect("served");
    assert_eq!(
        source.asked.lock().unwrap().clone(),
        [("hyperliquid#TESTNET".to_string(), VenueReadKind::Balance)]
    );
}

/// A control peer is answered too: the verb needs no particular scope, only an authenticated one.
#[test]
fn a_control_peer_is_answered_too() {
    let (_mount, addr) = spawn(Some(registry(Ok(balance()))));
    let mut stream = authed_stream(addr, Scope::Write, CONTROL_KEY);
    write_frame(&mut stream, &ask("polymarket", "DEFAULT", WireVenueReadKind::Balance))
        .expect("ask");
    match read_frame::<_, Response>(&mut stream).expect("reply") {
        Response::VenueRead(read) => assert_eq!(read.kind(), WireVenueReadKind::Balance),
        other => panic!("expected VenueRead, got {other:?}"),
    }
}

/// A refusal reaches the client as the operator's sentence, verbatim, as an error from a node that ANSWERED.
#[test]
fn a_refusal_is_the_operators_sentence() {
    let sentence = "not offered by binance: its bridge has no account-wide balance read";
    let (_mount, addr) = spawn(Some(registry(Err(sentence.to_string()))));
    let err = vike_tradehub_client::venue_read(
        addr,
        OBSERVE_KEY,
        "binance",
        "DEFAULT",
        WireVenueReadKind::Balance,
    )
    .expect_err("a refusal is an error");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().ends_with(sentence), "{err}");
}

/// A node with no lease registry, an unknown venue and a malformed account each say so, and the registry is
/// asked nothing for the last two.
#[test]
fn a_node_without_a_registry_an_unknown_venue_and_a_bad_account_each_say_so() {
    let (_mount, addr) = spawn(None);
    let err = vike_tradehub_client::venue_read(
        addr,
        OBSERVE_KEY,
        "polymarket",
        "DEFAULT",
        WireVenueReadKind::Balance,
    )
    .expect_err("no registry");
    assert!(err.to_string().contains("no trade lease registry"), "{err}");

    let source = registry(Ok(balance()));
    let (_mount, addr) = spawn(Some(Arc::clone(&source)));
    let unknown = vike_tradehub_client::venue_read(
        addr,
        OBSERVE_KEY,
        "nosuchvenue",
        "DEFAULT",
        WireVenueReadKind::Balance,
    )
    .expect_err("unknown venue");
    assert!(unknown.to_string().contains("not a venue of this node's roster"), "{unknown}");
    let bad = vike_tradehub_client::venue_read(
        addr,
        OBSERVE_KEY,
        "polymarket",
        "not a label!",
        WireVenueReadKind::Balance,
    )
    .expect_err("bad account");
    assert!(bad.to_string().contains("not an account name"), "{bad}");
    assert!(source.asked.lock().unwrap().is_empty(), "nothing was asked of the registry");
}

/// A figure that is not a number is never written: it would end the connection that decodes it.
#[test]
fn a_figure_that_is_not_a_number_is_never_written() {
    let mut read = balance();
    if let VenueRead::Balance(b) = &mut read {
        b.balance = f64::INFINITY;
    }
    let (_mount, addr) = spawn(Some(registry(Ok(read))));
    let err = vike_tradehub_client::venue_read(
        addr,
        OBSERVE_KEY,
        "polymarket",
        "DEFAULT",
        WireVenueReadKind::Balance,
    )
    .expect_err("refused in words");
    assert!(err.to_string().contains("not a number"), "{err}");
}

/// **THE GOLDEN FRAMES, read below the typed decode**: the node's reply is exactly these bytes, so a typed
/// decode that silently dropped a field the node leaked could not hide it.
#[test]
fn the_reply_frames_are_exactly_the_pinned_bytes() {
    let (_mount, addr) = spawn(Some(registry(Ok(balance()))));
    let mut stream = authed_stream(addr, Scope::Read, OBSERVE_KEY);
    write_frame(&mut stream, &ask("polymarket", "DEFAULT", WireVenueReadKind::Balance))
        .expect("ask");
    let body = String::from_utf8(read_frame_raw(&mut stream).expect("frame")).expect("UTF-8");
    assert_eq!(
        body,
        r#"{"VenueRead":{"Balance":{"asset":"pUSD","balance":20.0,"allowances":[{"spender":"0xE111180000d2663C0091e4f400237545B87B996B","name":"CTF exchange","amount":0.0,"unlimited":true}]}}}"#
    );

    let (_mount, addr) = spawn(Some(registry(Ok(open_orders()))));
    let mut stream = authed_stream(addr, Scope::Read, OBSERVE_KEY);
    write_frame(&mut stream, &ask("polymarket", "DEFAULT", WireVenueReadKind::OpenOrders))
        .expect("ask");
    let body = String::from_utf8(read_frame_raw(&mut stream).expect("frame")).expect("UTF-8");
    assert_eq!(
        body,
        r#"{"VenueRead":{"OpenOrders":{"orders":[{"venue_order_id":"0xabc","client_order_id":null,"symbol":"6710259892","side":1,"price":0.2,"qty":5.0,"filled_qty":0.0,"status":"ACCEPTED","ts_ms":1700000000000}],"truncated":false}}}"#
    );

    // The positions reply: the rows and nothing else (no funder address, no slug, no icon: the wire type has
    // no field for them), and an empty wallet is an empty ANSWER.
    let (_mount, addr) = spawn(Some(registry(Ok(positions()))));
    let mut stream = authed_stream(addr, Scope::Read, OBSERVE_KEY);
    write_frame(&mut stream, &ask("polymarket", "DEFAULT", WireVenueReadKind::Positions))
        .expect("ask");
    let body = String::from_utf8(read_frame_raw(&mut stream).expect("frame")).expect("UTF-8");
    assert_eq!(
        body,
        r#"{"VenueRead":{"Positions":{"positions":[{"symbol":"1484359444963781501765067677755406951259550728919736807361564486189514136251","title":"Solana Up or Down - June 9, 3:40AM-3:45AM ET","outcome":"Down","qty":11.11,"avg_price":0.2099,"price":0.0,"value":0.0,"redeemable":true}],"truncated":false}}}"#
    );
    let empty = VenueRead::Positions { positions: Vec::new(), truncated: false };
    let (_mount, addr) = spawn(Some(registry(Ok(empty))));
    let mut stream = authed_stream(addr, Scope::Read, OBSERVE_KEY);
    write_frame(&mut stream, &ask("polymarket", "DEFAULT", WireVenueReadKind::Positions))
        .expect("ask");
    let body = String::from_utf8(read_frame_raw(&mut stream).expect("frame")).expect("UTF-8");
    assert_eq!(body, r#"{"VenueRead":{"Positions":{"positions":[],"truncated":false}}}"#);

    let (_mount, addr) = spawn(Some(registry(Err("not offered by binance".into()))));
    let mut stream = authed_stream(addr, Scope::Read, OBSERVE_KEY);
    write_frame(&mut stream, &ask("binance", "DEFAULT", WireVenueReadKind::Balance)).expect("ask");
    let body = String::from_utf8(read_frame_raw(&mut stream).expect("frame")).expect("UTF-8");
    assert_eq!(body, r#"{"Error":"not offered by binance"}"#);
}

/// A peer that skips the handshake is refused, never answered a read.
#[test]
fn an_unauthenticated_peer_is_never_answered_a_read() {
    let source = registry(Ok(balance()));
    let (_mount, addr) = spawn(Some(Arc::clone(&source)));
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &ask("polymarket", "DEFAULT", WireVenueReadKind::Balance))
        .expect("a read in place of Hello");
    match read_frame::<_, Response>(&mut stream) {
        Ok(Response::AuthDenied { .. }) | Err(_) => {}
        other => panic!("an unauthenticated read was answered: {other:?}"),
    }
    assert!(source.asked.lock().unwrap().is_empty(), "the registry was never asked");
}
