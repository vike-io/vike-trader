//! The exec client writes each order on ITS INSTRUMENT'S OWN grid: the body builders on a JPY and a majors
//! grid, and the REAL `run_on` loop against a loopback OANDA double that serves the instrument list first.
//! The older `exec_tests.rs` keeps its EUR_USD scripts through `run`, whose book is preloaded.
//!
//! ⚠ **Docs-shaped, not captured.** The instrument list below follows OANDA's documentation
//! (<https://developer.oanda.com/rest-live-v20/account-ep/>, `Instrument` at
//! <https://developer.oanda.com/rest-live-v20/primitives-df/>); no practice-account body was recorded, so
//! whether the venue accepts exactly the texts written here is not proven on the wire.

use std::assert_matches;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc::channel;

use vike_model::SymbolProperties;

use super::*;

fn request(symbol: &str, order_type: OrderType, side: i32, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "coid-1".into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        qty: 1000.0,
        price: Some(price),
        trigger_price: Some(price),
        order_type,
        ..Default::default()
    }
}

fn grid(tick: f64) -> InstrumentGrid {
    InstrumentGrid::from_properties(&SymbolProperties {
        tick_size: tick,
        step_size: 1.0,
        ..Default::default()
    })
    .expect("a power-of-ten grid")
}

#[test]
fn a_jpy_pair_is_written_with_three_decimals_and_a_major_with_five() {
    let jpy = grid(0.001);
    let b = build_order_body_on(&request("USDJPY", OrderType::Limit, 1, 150.123), &jpy);
    assert_eq!(b["order"]["instrument"], "USD_JPY");
    assert_eq!(b["order"]["price"], "150.123");
    assert_eq!(b["order"]["units"], "1000");
    let b = build_order_body_on(&request("EURJPY", OrderType::Stop, -1, 160.0), &jpy);
    assert_eq!(b["order"]["type"], "STOP");
    assert_eq!(b["order"]["price"], "160.000", "a stop's trigger is written on the grid too");

    let b = build_order_body_on(&request("EURUSD", OrderType::Limit, 1, 1.09), &grid(0.00001));
    assert_eq!(b["order"]["price"], "1.09000");
}

/// A limit price off the grid is rounded to the passive side before it is sent: a BUY down, a SELL up.
#[test]
fn an_off_grid_limit_is_rounded_to_the_passive_side_on_the_wire() {
    let jpy = grid(0.001);
    let buy = build_order_body_on(&request("GBPJPY", OrderType::Limit, 1, 190.1234), &jpy);
    let sell = build_order_body_on(&request("GBPJPY", OrderType::Limit, -1, 190.1234), &jpy);
    assert_eq!(buy["order"]["price"], "190.123");
    assert_eq!(sell["order"]["price"], "190.124");
    assert_eq!(sell["order"]["units"], "-1000");
}

/// A grid state is not a default: the older builders stay the majors' (the offline fixtures), and a replace on
/// a JPY grid is written on that grid.
#[test]
fn the_fixture_builders_are_the_majors_and_a_replace_follows_its_instruments_grid() {
    let r = request("EURUSD", OrderType::Limit, 1, 1.09);
    assert_eq!(build_order_body(&r), build_order_body_on(&r, &InstrumentGrid::MAJORS));
    assert_eq!(build_order_body(&r)["order"]["price"], "1.09000");

    let order = request("USDJPY", OrderType::Limit, 1, 150.123);
    let body = build_replace_body_on(&order, None, Some(150.5), || Ok(grid(0.001)))
        .expect("a resting limit replaces");
    assert_eq!(body["order"]["price"], "150.500");
    // the grid is asked AFTER the checks that need none: a market never reads the instrument list
    let market = request("USDJPY", OrderType::Market, 1, 150.0);
    let why = build_replace_body_on(&market, None, Some(150.5), || panic!("no grid for a market"))
        .expect_err("a market cannot be replaced");
    assert!(why.contains("only a resting limit or stop"), "{why}");
    // an instrument without a grid is a refusal sentence, not a body
    let refused = build_replace_body_on(&order, None, Some(150.5), || Err("no grid".into()));
    assert_eq!(refused, Err("no grid".to_string()));
}

// --- the REAL run loop against a loopback OANDA double ---------------------------------------

/// A docs-shaped list: two real-looking instruments and two garbage ones.
const INSTRUMENTS: &str = r#"{"instruments":[
  {"name":"EUR_USD","type":"CURRENCY","displayName":"EUR/USD","pipLocation":-4,
   "displayPrecision":5,"tradeUnitsPrecision":0,"minimumTradeSize":"1","maximumOrderUnits":"100000000"},
  {"name":"USD_JPY","type":"CURRENCY","displayName":"USD/JPY","pipLocation":-2,
   "displayPrecision":3,"tradeUnitsPrecision":0,"minimumTradeSize":"1","maximumOrderUnits":"100000000"},
  {"name":"NEGPR_USD","type":"CURRENCY","displayName":"NEG/USD","pipLocation":-4,
   "displayPrecision":-1,"tradeUnitsPrecision":0,"minimumTradeSize":"1"},
  {"name":"NOPR_USD","type":"CURRENCY","displayName":"NO/USD","pipLocation":-4,
   "tradeUnitsPrecision":0,"minimumTradeSize":"1"}
],"lastTransactionID":"6399"}"#;

const SUMMARY: &str = r#"{"account":{"currency":"USD"},"lastTransactionID":"6400"}"#;

const ACCEPTED: &str =
    r#"{"orderCreateTransaction":{"id":"6401","type":"LIMIT_ORDER"},"lastTransactionID":"6401"}"#;

const REPLACED: &str = r#"{"orderCancelTransaction":{"id":"6402"},
    "orderCreateTransaction":{"id":"6403","units":"500","price":"150.500"},"lastTransactionID":"6403"}"#;

type Served = Vec<(String, String)>;

/// A loopback HTTP server answering each request with the next `(status, body)` of `script`; its join value
/// is every request it served as `(request line, body)`. (The twin of `exec_tests`' `loopback` double.)
fn spawn_double(script: Vec<(u16, &'static str)>) -> (SocketAddr, thread::JoinHandle<Served>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let handle = thread::spawn(move || {
        let mut served = Vec::new();
        for (status, body) in script {
            let Ok((mut sock, _)) = listener.accept() else { break };
            let mut buf = Vec::new();
            let mut chunk = [0_u8; 1024];
            let (head_end, content_length) = loop {
                let n = sock.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break (buf.len(), 0);
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (p + 4, len);
                }
            };
            while buf.len() < head_end + content_length {
                let n = sock.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let text = String::from_utf8_lossy(&buf).to_string();
            let request_line = text.lines().next().unwrap_or_default().to_string();
            served.push((request_line, text[head_end.min(text.len())..].to_string()));
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(reply.as_bytes());
            let _ = sock.flush();
        }
        served
    });
    (addr, handle)
}

/// Drive the REAL `run_on` loop with an EMPTY book (so the instrument list is read from the double) and
/// `cmds`; return every event it emitted.
fn drive(addr: SocketAddr, cmds: Vec<ExecCommand>) -> Vec<Event> {
    let config = OandaConfig {
        api_token: "tok".to_string(),
        account_id: "acct-1".to_string(),
        rest_base: format!("http://{addr}"),
        stream_base: format!("http://{addr}"),
    };
    let (events, mut ingest) = vike_exec::event_channel(64);
    let (tx, rx) = channel();
    let last_seen = Arc::new(AtomicU64::new(0));
    let symbols = SubmittedSymbols::default();
    let lane = thread::spawn(move || {
        let hedging = crate::hedging::AccountHedging::stated(false);
        run_on(config, events, rx, last_seen, symbols, GridBook::default(), hedging)
    });
    for c in cmds {
        tx.send(c).expect("the loop is alive");
    }
    tx.send(ExecCommand::Shutdown).expect("the loop is alive");
    lane.join().expect("the loop returns on Shutdown");
    let mut out = Vec::new();
    while let Some(i) = ingest.blocking_recv() {
        match i {
            vike_exec::Ingest::Event(e) => out.push(e),
            other => panic!("expected Ingest::Event, got {other:?}"),
        }
    }
    out
}

fn submit(symbol: &str, coid: &str, side: i32, price: f64) -> ExecCommand {
    let mut r = request(symbol, OrderType::Limit, side, price);
    r.client_order_id = coid.into();
    ExecCommand::Submit(Box::new(r))
}

fn posted(served: &Served, at: usize) -> serde_json::Value {
    serde_json::from_str(&served[at].1).expect("an order body is JSON")
}

/// The instrument list is read ONCE, before the first order that needs a grid, and the JPY order goes out
/// with three decimals; a second order on another instrument costs no further read.
#[test]
fn the_loop_reads_the_instrument_list_once_and_writes_each_order_on_its_own_grid() {
    let (addr, server) =
        spawn_double(vec![(200, INSTRUMENTS), (200, SUMMARY), (201, ACCEPTED), (201, ACCEPTED)]);
    let evs = drive(
        addr,
        vec![submit("USDJPY", "coid-1", 1, 150.1234), submit("EURUSD", "coid-2", -1, 1.090004)],
    );
    let served = server.join().expect("double joins");
    let lines: Vec<&str> = served.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(
        lines,
        [
            "GET /v3/accounts/acct-1/instruments HTTP/1.1",
            "GET /v3/accounts/acct-1/summary HTTP/1.1",
            "POST /v3/accounts/acct-1/orders HTTP/1.1",
            "POST /v3/accounts/acct-1/orders HTTP/1.1",
        ]
    );
    let (jpy, eur) = (posted(&served, 2), posted(&served, 3));
    assert_eq!(jpy["order"]["instrument"], "USD_JPY");
    assert_eq!(jpy["order"]["price"], "150.123", "a BUY rounds down");
    assert_eq!(jpy["order"]["units"], "1000");
    assert_eq!(eur["order"]["instrument"], "EUR_USD");
    assert_eq!(eur["order"]["price"], "1.09001", "a SELL rounds up");
    assert_eq!(eur["order"]["units"], "-1000");
    assert_eq!(
        evs.iter().filter(|e| matches!(e, Event::OrderAccepted(_))).count(),
        2,
        "both orders were accepted: {evs:?}"
    );
}

/// **Fail closed**: an instrument whose row has no usable precision, or is not in the list, is REFUSED with a
/// sentence (the terminal `OrderRejected`) and nothing is posted.
#[test]
fn an_instrument_without_a_grid_is_rejected_with_a_sentence_and_nothing_is_posted() {
    let (addr, server) = spawn_double(vec![(200, INSTRUMENTS)]);
    let evs = drive(
        addr,
        vec![
            submit("NEGPR_USD", "coid-1", 1, 1.0),
            submit("NOPR_USD", "coid-2", 1, 1.0),
            submit("NOPE_USD", "coid-3", 1, 1.0),
        ],
    );
    assert_eq!(server.join().expect("double joins").len(), 1, "one list read, no POST, no summary");
    let reasons: Vec<(&str, &str)> = evs
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some((r.client_order_id.as_str(), &*r.reason)),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 3, "every refused order ends in a terminal event: {evs:?}");
    assert!(
        reasons[0].1.contains("NEGPR_USD") && reasons[0].1.contains("displayPrecision"),
        "{reasons:?}"
    );
    assert!(
        reasons[1].1.contains("NOPR_USD") && reasons[1].1.contains("displayPrecision"),
        "{reasons:?}"
    );
    assert!(
        reasons[2].1.contains("NOPE_USD") && reasons[2].1.contains("not in the account's"),
        "{reasons:?}"
    );
    assert!(
        evs.iter().filter(|e| matches!(e, Event::OrderSubmitted(_))).count() == 3,
        "each order was announced first: {evs:?}"
    );
}

/// An instrument list that cannot be read refuses the order with the venue's reason, even for EUR_USD: no
/// five-decimal default.
#[test]
fn an_unreadable_list_refuses_even_a_major() {
    let (addr, server) = spawn_double(vec![(503, r#"{"errorMessage":"down"}"#)]);
    let evs = drive(addr, vec![submit("EURUSD", "coid-1", 1, 1.09)]);
    assert_eq!(server.join().expect("double joins").len(), 1);
    let reason = evs
        .iter()
        .find_map(|e| match e {
            Event::OrderRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .expect("a terminal rejection");
    assert!(
        reason.contains("EUR_USD") && reason.contains("503") && reason.contains("guessed grid"),
        "{reason}"
    );
}

/// A replace reads the same grid: the JPY replacement is written with three decimals, and a replace whose
/// grid cannot be read is the NON-terminal `OrderModifyRejected` (the resting order keeps its terms).
#[test]
fn a_replace_follows_the_instruments_grid_and_a_missing_grid_is_a_modify_rejection() {
    let order = request("USDJPY", OrderType::Limit, 1, 150.123);
    let modify = |order: OrderRequest, price: f64| ExecCommand::Modify {
        order: Box::new(OrderRequest { qty: 500.0, ..order }),
        new_qty: None,
        new_price: Some(price),
    };

    let (addr, server) = spawn_double(vec![(200, INSTRUMENTS), (201, REPLACED)]);
    let evs = drive(addr, vec![modify(order.clone(), 150.5)]);
    let served = server.join().expect("double joins");
    assert_eq!(served[1].0, "PUT /v3/accounts/acct-1/orders/@coid-1 HTTP/1.1");
    assert_eq!(posted(&served, 1)["order"]["price"], "150.500");
    assert_matches!(&evs[..], [Event::OrderModified(m)] if m.client_order_id == "coid-1");

    let (addr, server) = spawn_double(vec![(503, r#"{"errorMessage":"down"}"#)]);
    let evs = drive(addr, vec![modify(order, 150.5)]);
    assert_eq!(server.join().expect("double joins").len(), 1, "nothing was PUT");
    assert_matches!(
        &evs[..], [Event::OrderModifyRejected(r)] if r.reason.contains("guessed grid")
    );
}
