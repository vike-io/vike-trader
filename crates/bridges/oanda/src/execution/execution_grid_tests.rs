//! The exec client writes each order on ITS INSTRUMENT'S OWN grid: the body builders on a JPY and a majors
//! grid, and the REAL `run_on` loop against a loopback OANDA double that serves the instrument list first.
//! The older `execution/execution_tests.rs` keeps its EUR_USD scripts through `run`, whose book is preloaded.
//!
//! ⚠ **Docs-shaped, not captured.** The instrument list below follows OANDA's documentation
//! (<https://developer.oanda.com/rest-live-v20/account-ep/>, `Instrument` at
//! <https://developer.oanda.com/rest-live-v20/primitives-df/>); no practice-account body was recorded, so
//! whether the venue accepts exactly the texts written here is not proven on the wire.

use std::assert_matches;
use std::net::SocketAddr;
use std::sync::mpsc::channel;

use vike_model::SymbolProperties;

use super::*;
use crate::execution::execution_tests::loopback::{Served, spawn_double, spawn_double_after};

/// An order on the USER name `symbol`; the tests' kept list is held first, so the loop finds its wire name.
fn request(symbol: &str, order_type: OrderType, side: i32, price: f64) -> OrderRequest {
    crate::test_support::kept_list_for_tests();
    OrderRequest {
        order_client_id: "order-client-id-1".into(),
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
    let b = build_order_body_on(&request("USDJPY", OrderType::Limit, 1, 150.123), "USD_JPY", &jpy);
    assert_eq!(b["order"]["instrument"], "USD_JPY");
    assert_eq!(b["order"]["price"], "150.123");
    assert_eq!(b["order"]["units"], "1000");
    let b = build_order_body_on(&request("EURJPY", OrderType::Stop, -1, 160.0), "EUR_JPY", &jpy);
    assert_eq!(b["order"]["type"], "STOP");
    assert_eq!(b["order"]["price"], "160.000", "a stop's trigger is written on the grid too");

    let eur = request("EURUSD", OrderType::Limit, 1, 1.09);
    let b = build_order_body_on(&eur, "EUR_USD", &grid(0.00001));
    assert_eq!(b["order"]["price"], "1.09000");
}

/// A limit price off the grid is rounded to the passive side before it is sent: a BUY down, a SELL up.
#[test]
fn an_off_grid_limit_is_rounded_to_the_passive_side_on_the_wire() {
    let jpy = grid(0.001);
    let limit = |side| request("GBPJPY", OrderType::Limit, side, 190.1234);
    let buy = build_order_body_on(&limit(1), "GBP_JPY", &jpy);
    let sell = build_order_body_on(&limit(-1), "GBP_JPY", &jpy);
    assert_eq!(buy["order"]["price"], "190.123");
    assert_eq!(sell["order"]["price"], "190.124");
    assert_eq!(sell["order"]["units"], "-1000");
}

/// A STOP's trigger off the grid goes AWAY from the market, a buy up and a sell down (the model's `snap_stop`
/// rule, the owner's G1), so it never fires before its level; the limit rule sent a buy stop down and a sell stop
/// up, both early. A market-if-touched take-profit keeps the limit's direction, and a replaced stop goes as a stop.
#[test]
fn an_off_grid_stop_trigger_goes_away_from_the_market_on_the_wire() {
    let jpy = grid(0.001);
    let stop = |side| {
        build_order_body_on(&request("GBPJPY", OrderType::Stop, side, 190.1234), "GBP_JPY", &jpy)
    };
    assert_eq!(stop(1)["order"]["price"], "190.124", "a buy stop goes up");
    assert_eq!(stop(-1)["order"]["price"], "190.123", "a sell stop goes down");
    let take_profit = |side| OrderRequest {
        price: None,
        ..request("GBPJPY", OrderType::TakeProfit, side, 190.1234)
    };
    assert_eq!(build_order_body_on(&take_profit(1), "GBP_JPY", &jpy)["order"]["price"], "190.123");
    assert_eq!(build_order_body_on(&take_profit(-1), "GBP_JPY", &jpy)["order"]["price"], "190.124");
    let resting = request("GBPJPY", OrderType::Stop, 1, 190.0);
    let body = build_replace_body_on(&resting, "GBP_JPY", None, Some(190.1234), || Ok(jpy))
        .expect("a resting stop replaces");
    assert_eq!(body["order"]["price"], "190.124", "a replaced buy stop goes up too");
}

/// A grid state is not a default: the older builders stay the majors' (the offline fixtures), and a replace on
/// a JPY grid is written on that grid.
#[test]
fn the_fixture_builders_are_the_majors_and_a_replace_follows_its_instruments_grid() {
    let r = request("EURUSD", OrderType::Limit, 1, 1.09);
    assert_eq!(build_order_body(&r), build_order_body_on(&r, "EUR_USD", &InstrumentGrid::MAJORS));
    assert_eq!(build_order_body(&r)["order"]["price"], "1.09000");

    let order = request("USDJPY", OrderType::Limit, 1, 150.123);
    let body = build_replace_body_on(&order, "USD_JPY", None, Some(150.5), || Ok(grid(0.001)))
        .expect("a resting limit replaces");
    assert_eq!(body["order"]["price"], "150.500");
    // the grid is asked AFTER the checks that need none: a market never reads the instrument list
    let market = request("USDJPY", OrderType::Market, 1, 150.0);
    let why = build_replace_body_on(&market, "USD_JPY", None, Some(150.5), || {
        panic!("no grid for a market")
    })
    .expect_err("a market cannot be replaced");
    assert!(why.contains("only a resting limit or stop"), "{why}");
    // an instrument without a grid is a refusal sentence, not a body
    let refused =
        build_replace_body_on(&order, "USD_JPY", None, Some(150.5), || Err("no grid".into()));
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

/// Drive the REAL `run_on` loop with an EMPTY book (so the instrument list is read from the double) and
/// `commands`; return every event it emitted.
fn drive(addr: SocketAddr, commands: Vec<ExecutionClientActorCommand>) -> Vec<Event> {
    let config = OandaConfig {
        api_token: vike_model::secret::Secret::new("tok".to_string()),
        account_id: "acct-1".to_string(),
        rest_base: format!("http://{addr}"),
        stream_base: format!("http://{addr}"),
    };
    let (events, mut ingest) = vike_execution::event_channel(64);
    let (tx, rx) = channel();
    let last_seen = Arc::new(AtomicU64::new(0));
    let symbols = submitted_symbols();
    let lane = thread::spawn(move || {
        let hedging = crate::account::modes::AccountHedging::stated(false);
        let fence = Arc::new(SubmitFence::new(events.clone()));
        run_on(config, events, &rx, last_seen, symbols, fence, GridCache::default(), hedging)
    });
    for c in commands {
        tx.send(c).expect("the loop is alive");
    }
    tx.send(ExecutionClientActorCommand::Shutdown).expect("the loop is alive");
    lane.join().expect("the loop returns on Shutdown");
    let mut out = Vec::new();
    while let Some(i) = ingest.blocking_recv() {
        match i {
            vike_execution::Ingest::Event(e) => out.push(e),
            other => panic!("expected Ingest::Event, got {other:?}"),
        }
    }
    out
}

fn submit(
    symbol: &str,
    order_client_id: &str,
    side: i32,
    price: f64,
) -> ExecutionClientActorCommand {
    let mut r = request(symbol, OrderType::Limit, side, price);
    r.order_client_id = order_client_id.into();
    ExecutionClientActorCommand::Submit(Box::new(r), std::time::Instant::now())
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
        vec![
            submit("USDJPY", "order-client-id-1", 1, 150.1234),
            submit("EURUSD", "order-client-id-2", -1, 1.090004),
        ],
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

/// **Fail closed**: an instrument whose row has no usable precision, or is not in the account's list, is REFUSED
/// with a sentence (the terminal `OrderRejected`) and nothing is posted; a name the KEPT list carries no row of is
/// refused by the translator before the list is even read (decision 0290: nothing is guessed from a name).
#[test]
fn an_instrument_without_a_grid_is_rejected_with_a_sentence_and_nothing_is_posted() {
    let (addr, server) = spawn_double(vec![(200, INSTRUMENTS)]);
    let evs = drive(
        addr,
        vec![
            submit("NEGPRUSD", "order-client-id-1", 1, 1.0),
            submit("NOPRUSD", "order-client-id-2", 1, 1.0),
            // kept (another account's list carries silver), not in this account's list
            submit("XAGUSD", "order-client-id-3", 1, 1.0),
            submit("NOPEUSD", "order-client-id-4", 1, 1.0),
        ],
    );
    assert_eq!(server.join().expect("double joins").len(), 1, "one list read, no POST, no summary");
    let reasons: Vec<(&str, &str)> = evs
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some((r.order_client_id.as_str(), &*r.reason)),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 4, "every refused order ends in a terminal event: {evs:?}");
    assert!(
        reasons[0].1.contains("NEGPR_USD") && reasons[0].1.contains("displayPrecision"),
        "{reasons:?}"
    );
    assert!(
        reasons[1].1.contains("NOPR_USD") && reasons[1].1.contains("displayPrecision"),
        "{reasons:?}"
    );
    assert!(
        reasons[2].1.contains("XAG_USD") && reasons[2].1.contains("not in the account's"),
        "{reasons:?}"
    );
    assert!(
        reasons[3].1.contains("NOPEUSD")
            && reasons[3].1.contains("kept list")
            && reasons[3].1.contains("nothing was sent"),
        "{reasons:?}"
    );
    assert!(
        evs.iter().filter(|e| matches!(e, Event::OrderSubmitted(_))).count() == 4,
        "each order was announced first: {evs:?}"
    );
}

/// An instrument list that cannot be read refuses the order with the venue's reason, even for EUR_USD: no
/// five-decimal default.
#[test]
fn an_unreadable_list_refuses_even_a_major() {
    let (addr, server) = spawn_double(vec![(503, r#"{"errorMessage":"down"}"#)]);
    let evs = drive(addr, vec![submit("EURUSD", "order-client-id-1", 1, 1.09)]);
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
    let modify = |order: OrderRequest, price: f64| ExecutionClientActorCommand::Modify {
        order: Box::new(OrderRequest { qty: 500.0, ..order }),
        new_qty: None,
        new_price: Some(price),
        new_trigger_price: None,
        queued_at: std::time::Instant::now(),
    };

    let (addr, server) = spawn_double(vec![(200, INSTRUMENTS), (201, REPLACED)]);
    let evs = drive(addr, vec![modify(order.clone(), 150.5)]);
    let served = server.join().expect("double joins");
    assert_eq!(served[1].0, "PUT /v3/accounts/acct-1/orders/@order-client-id-1 HTTP/1.1");
    assert_eq!(posted(&served, 1)["order"]["price"], "150.500");
    assert_matches!(&evs[..], [Event::OrderModified(m)] if m.order_client_id == "order-client-id-1");

    let (addr, server) = spawn_double(vec![(503, r#"{"errorMessage":"down"}"#)]);
    let evs = drive(addr, vec![modify(order, 150.5)]);
    assert_eq!(server.join().expect("double joins").len(), 1, "nothing was PUT");
    assert_matches!(
        &evs[..], [Event::OrderModifyRejected(r)] if r.reason.contains("guessed grid")
            && r.refusal_id.as_deref() == Some(OrderRefusalId::NotSent.word())
    );
}

/// **The grid read in front of the PUT spends what an amend had left**: this one is young at dequeue and past
/// `MAX_MODIFY_WAIT` when the instrument list arrives, so it is refused unsent and the venue hears no PUT.
#[test]
fn an_amend_that_outlives_its_wait_in_the_grid_read_is_not_sent() {
    use vike_bridge_core::exec_actor::MAX_MODIFY_WAIT;
    let nearly = std::time::Instant::now() - MAX_MODIFY_WAIT + std::time::Duration::from_secs(1);
    let order = request("USDJPY", OrderType::Limit, 1, 150.123);
    let (addr, server) =
        spawn_double_after(std::time::Duration::from_millis(1500), vec![(200, INSTRUMENTS)]);
    let evs = drive(
        addr,
        vec![ExecutionClientActorCommand::Modify {
            order: Box::new(OrderRequest { qty: 500.0, ..order }),
            new_qty: None,
            new_price: Some(150.5),
            new_trigger_price: None,
            queued_at: nearly,
        }],
    );
    assert_eq!(server.join().expect("double joins").len(), 1, "the read, and no PUT");
    assert_matches!(&evs[..], [Event::OrderModifyRejected(r)] if r.reason.contains("NOT sent"));
}
