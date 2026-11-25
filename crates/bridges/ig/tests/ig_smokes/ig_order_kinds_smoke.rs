//! Live demo smoke: the order kinds IG RESTS, a LIMIT and a STOP on each side, placed through the bridge's own exec
//! client far from the market, read back on IG's working-order list at their level, moved and cancelled.
//!
//!     cargo test -p vike-ig --test ig_smokes -- --ignored --nocapture ig_order_kinds_smoke::
//!
//! IG's row (`crates/vike-venues/src/venue_caps.rs`'s `IG`) lists three kinds: `market`, `limit` and `stop`. A MARKET
//! order fills at once and cannot be placed far from the market, so its open and close is
//! `ig_close_position_smoke.rs`'s; this file is the two kinds that rest. A reduce-only limit or stop is refused by the
//! bridge before the wire (`CLOSE_MARKET_ONLY`, `tests/offline/close_limit_never_rests.rs`), so none is sent here.
//!
//! ⚠ **This PLACES REAL ORDERS on the IG DEMO account**: four working orders of the minimum size, none of which can
//! fill: a BUY LIMIT and a SELL STOP [`AWAY`] below the bid, a SELL LIMIT and a BUY STOP [`AWAY`] above the offer. Each
//! is accepted, found on `GET /workingorders` at its level, moved [`FURTHER`] away by price (`OrderModified`, the PUT
//! the node's amend takes) and cancelled (`OrderCanceled`, confirmed by IG). Then a sweep deletes every working order
//! of the epic this run added, on every exit path (a failed step included), and the run asserts the list and the open
//! deals are back to what they were.
//!
//! It drives the PRODUCTION client, `vike_ig::spawn_execution_client`: its exec thread builds the bodies (`build_request`),
//! reads `/confirms` and keeps the order client id to dealId map the cancel and the amend address. The REST session of this file
//! only reads the market and the working-order list, and deletes in the sweep. The client watches a HALT sentinel this
//! file owns and never creates, as a start hands one.
//!
//! Gated like `ig_close_position_smoke.rs`: no demo account credentials, a failed demo login, or a market that is not
//! TRADEABLE (FX is shut at the weekend) is a skip, never a red. It opens three sessions of the key (this file's, the
//! exec thread's and the trade stream's): run it alone, one probe at a time (`crates/bridges/ig/CLAUDE.md`). Its
//! orders carry no origin tag, so a node reconcile pass on the same account that lands while one rests holds it as an
//! `UnknownOrder` under `quarantine`.

use crate::common;

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{EPIC, SYMBOL, config};
use serde_json::Value;
use vike_bridge_core::exec_actor::ExecutionClientActor;
use vike_bridge_core::execution_drive::drain;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_ig::{IgSession, spawn_execution_client};
use vike_math::powers::power_of_ten;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

/// How far from the touch each order rests: a 5% move of EUR/USD inside the run is not a market this smoke trades in.
const AWAY: f64 = 0.05;

/// How much further the amend moves it.
const FURTHER: f64 = 0.01;

/// The smallest dealable size on the demo gateway's EUR/USD mini when the market body states none (read live on
/// 2026-08-21, `ig_close_position_smoke.rs`'s `SIZE`).
const MIN_SIZE: f64 = 0.1;

/// How long the client has to answer one command: the exec thread's first login, the `/confirms` ladder (about 2.4 s)
/// and IG's own latency.
const ANSWER: Duration = Duration::from_secs(45);

/// The HALT sentinel the client watches: this file owns it and never creates it.
const HALT_SENTINEL: &str = "ig-order-kinds-smoke-never-engaged/HALT";

/// The market as this smoke needs it, off `GET /markets/{epic}` v3.
struct Market {
    bid: f64,
    offer: f64,
    min_size: f64,
    decimals: i32,
}

/// The market, or why it cannot be traded now.
fn market(session: &IgSession) -> Result<Market, String> {
    let m = session.get(&format!("/markets/{EPIC}"), "3", "").map_err(|e| e.to_string())?;
    let status = m.pointer("/snapshot/marketStatus").and_then(Value::as_str).unwrap_or("");
    if status != "TRADEABLE" {
        return Err(format!("{EPIC} is {status}, not TRADEABLE (FX is shut at the weekend)"));
    }
    let price = |p: &str| m.pointer(p).and_then(Value::as_f64).filter(|v| *v > 0.0);
    let (Some(bid), Some(offer)) = (price("/snapshot/bid"), price("/snapshot/offer")) else {
        return Err(format!("{EPIC} states no bid or no offer"));
    };
    let min_size = price("/dealingRules/minDealSize/value").unwrap_or(MIN_SIZE);
    let decimals = m
        .pointer("/snapshot/decimalPlacesFactor")
        .and_then(Value::as_i64)
        .map_or(5, |d| d.clamp(0, 5) as i32);
    Ok(Market { bid, offer, min_size, decimals })
}

/// `level` on the market's own grid.
fn on_grid(level: f64, decimals: i32) -> f64 {
    let scale = power_of_ten(decimals).expect("the decimals are clamped to 0..=5");
    (level * scale).round() / scale
}

/// One working order of `EPIC` as IG lists it: `(dealId, orderType, direction, orderLevel)`.
type Row = (String, String, String, f64);

/// The working orders of `EPIC` right now (`GET /workingorders` v2).
fn working_orders(session: &IgSession) -> Vec<Row> {
    let body = session.get("/workingorders", "2", "").expect("GET /workingorders");
    let text = |r: &Value, p: &str| r.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
    body.get("workingOrders")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|r| r.pointer("/marketData/epic").and_then(Value::as_str) == Some(EPIC))
                .map(|r| {
                    (
                        text(r, "/workingOrderData/dealId"),
                        text(r, "/workingOrderData/orderType"),
                        text(r, "/workingOrderData/direction"),
                        r.pointer("/workingOrderData/orderLevel")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// How many open deals the account holds in `EPIC`.
fn deals_open(session: &IgSession) -> usize {
    let body = session.get("/positions", "2", "").expect("GET /positions");
    body.get("positions").and_then(Value::as_array).map_or(0, |rows| {
        rows.iter()
            .filter(|r| r.pointer("/market/epic").and_then(Value::as_str) == Some(EPIC))
            .count()
    })
}

/// The row of `deal_id` once IG lists it as `want` says (a list read lags the confirm by a moment), else the last read.
fn row_of(session: &IgSession, deal_id: &str, want: impl Fn(Option<&Row>) -> bool) -> Option<Row> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let row = working_orders(session).into_iter().find(|r| r.0 == deal_id);
        if want(row.as_ref()) || Instant::now() >= deadline {
            return row;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Every event off the lane until one satisfies `answer`, or [`ANSWER`] passes; each is printed.
fn until(
    next: &mut impl FnMut() -> Option<Ingest>,
    answer: impl Fn(&Event) -> bool,
) -> Option<Event> {
    let deadline = Instant::now() + ANSWER;
    while Instant::now() < deadline {
        for event in drain(&mut *next, 1, Duration::from_millis(250)) {
            println!("     <- {event:?}");
            if answer(&event) {
                return Some(event);
            }
        }
    }
    None
}

/// One resting kind on one side: `below` rests it under the bid, else over the offer.
struct Case {
    kind: OrderType,
    side: i32,
    below: bool,
}

const CASES: [Case; 4] = [
    Case { kind: OrderType::Limit, side: 1, below: true },
    Case { kind: OrderType::Stop, side: -1, below: true },
    Case { kind: OrderType::Limit, side: -1, below: false },
    Case { kind: OrderType::Stop, side: 1, below: false },
];

/// The order of `case` at `level`: a LIMIT carries it as its price, a STOP as its trigger (`build_request` reads each).
fn order(order_client_id: &str, case: &Case, qty: f64, level: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "ig".into(),
        // the USER name; the exec client writes the epic of its kept row (decision 0290)
        symbol: SYMBOL.into(),
        side: case.side,
        qty,
        order_type: case.kind.clone(),
        price: (case.kind == OrderType::Limit).then_some(level),
        trigger_price: (case.kind == OrderType::Stop).then_some(level),
        timestamp: vike_model::now_ms(),
        ..Default::default()
    }
}

/// Place, read back, move and cancel one case; panics on the first step IG or the client gets wrong.
fn one_case(
    client: &mut ExecutionClientActor,
    next: &mut impl FnMut() -> Option<Ingest>,
    session: &IgSession,
    m: &Market,
    case: &Case,
    run: i64,
) {
    let touch = if case.below { m.bid } else { m.offer };
    let factor = |d: f64| if case.below { 1.0 - d } else { 1.0 + d };
    let away = |d: f64| on_grid(touch * factor(d), m.decimals);
    let (level, moved) = (away(AWAY), away(AWAY + FURTHER));
    let (word, dir) = (
        if case.kind == OrderType::Limit { "LIMIT" } else { "STOP" },
        if case.side > 0 { "BUY" } else { "SELL" },
    );
    let order_client_id =
        format!("igkind-{run}-{}-{}", word.to_ascii_lowercase(), dir.to_ascii_lowercase());
    let req = order(&order_client_id, case, m.min_size, level);
    println!(
        "{dir} {word} {} @ {level} (bid {} offer {}), order client id {order_client_id}:",
        m.min_size, m.bid, m.offer
    );

    client.submit(&req);
    let accepted = until(next, |e| match e {
        Event::OrderAccepted(a) => a.order_client_id == order_client_id,
        Event::OrderRejected(r) => r.order_client_id == order_client_id,
        _ => false,
    });
    let deal_id = match accepted {
        Some(Event::OrderAccepted(a)) => a
            .venue_order_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| panic!("{order_client_id}: accepted with no dealId")),
        other => panic!("{order_client_id}: IG did not accept the {dir} {word}: {other:?}"),
    };
    let rests = row_of(session, &deal_id, |r| r.is_some());
    println!("     rests: {rests:?}");
    let (_, otype, odir, olevel) =
        rests.unwrap_or_else(|| panic!("{order_client_id}: {deal_id} is not on the list"));
    assert_eq!(
        (otype.as_str(), odir.as_str()),
        (word, dir),
        "{order_client_id}: IG rests it as another order"
    );
    assert!(
        (olevel - level).abs() < 1e-9,
        "{order_client_id}: IG rests it at {olevel}, not {level}"
    );

    println!("  move it to {moved}:");
    client.modify(&req, None, Some(moved));
    let modified = until(next, |e| match e {
        Event::OrderModified(x) => x.order_client_id == order_client_id,
        Event::OrderModifyRejected(x) => x.order_client_id == order_client_id,
        _ => false,
    });
    assert!(
        matches!(&modified, Some(Event::OrderModified(x)) if x.new_price == Some(moved)),
        "{order_client_id}: the amend to {moved} did not land: {modified:?}"
    );
    let moved_row = row_of(session, &deal_id, |r| r.is_some_and(|r| (r.3 - moved).abs() < 1e-9));
    println!("     after the move: {moved_row:?}");
    assert!(
        moved_row.as_ref().is_some_and(|r| (r.3 - moved).abs() < 1e-9),
        "{order_client_id}: IG does not list it at {moved}: {moved_row:?}"
    );

    println!("  cancel it:");
    client.cancel(&order_client_id);
    let canceled = until(next, |e| match e {
        Event::OrderCanceled(x) => x.order_client_id == order_client_id,
        Event::OrderCancelRejected(x) => x.order_client_id == order_client_id,
        _ => false,
    });
    assert!(
        matches!(canceled, Some(Event::OrderCanceled(_))),
        "{order_client_id}: not cancelled: {canceled:?}"
    );
    let gone = row_of(session, &deal_id, |r| r.is_none());
    assert!(gone.is_none(), "{order_client_id}: IG still lists it after the cancel: {gone:?}");
    println!("  OK: {dir} {word} rested, moved and cancelled");
}

#[test]
#[ignore = "live DEMO; PLACES REAL ORDERS (four far working orders, each cancelled) - needs a demo ig account's \
            credentials and an open FX market"]
fn ig_limit_and_stop_rest_move_and_cancel_on_each_side() {
    let Some(c) = config() else {
        eprintln!("skip: no demo ig account credentials in the credential store");
        return;
    };
    let Ok(session) = IgSession::login(&c) else {
        eprintln!("skip: IG demo login failed (rotated password?) - not a red");
        return;
    };
    println!("logged in: account {}", session.account_id);
    let m = match market(&session) {
        Ok(m) => m,
        Err(why) => {
            eprintln!("skip: {why} - not a red");
            return;
        }
    };
    let before = working_orders(&session);
    let deals_before = deals_open(&session);
    println!(
        "before: {} working order(s) in {EPIC} {before:?}, {deals_before} open deal(s)",
        before.len()
    );

    common::hold_kept_list();
    let (events, mut rx) = event_channel(256);
    let mut client =
        spawn_execution_client(c, events, None).with_halt_path(PathBuf::from(HALT_SENTINEL));
    let mut next = || rx.try_recv().ok();
    let run = vike_model::now_ms();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        for case in &CASES {
            one_case(&mut client, &mut next, &session, &m, case, run);
        }
    }));

    // ---- whatever happened above: delete every working order this run added -------------------------------------
    for (deal_id, otype, dir, level) in working_orders(&session) {
        if before.iter().any(|r| r.0 == deal_id) {
            continue;
        }
        println!("sweeping {deal_id} ({dir} {otype} @ {level})");
        match session.delete(&format!("/workingorders/otc/{deal_id}"), "2") {
            Ok(reply) => println!("     cancel reply: {reply}"),
            Err(e) => println!("     cancel FAILED: status={} body={}", e.status, e.message),
        }
    }
    client.detach();
    let after = working_orders(&session);
    let deals_after = deals_open(&session);
    println!(
        "after: {} working order(s) in {EPIC} {after:?}, {deals_after} open deal(s)",
        after.len()
    );
    let added: Vec<&Row> = after.iter().filter(|r| !before.iter().any(|b| b.0 == r.0)).collect();
    assert!(added.is_empty(), "NO ORDER OF THIS RUN MAY STILL REST: {added:?}");
    assert_eq!(
        deals_after, deals_before,
        "a far order FILLED and opened a deal in {EPIC}: close it by hand (vike-cli or the IG demo UI)"
    );
    if let Err(panic) = outcome {
        resume_unwind(panic);
    }
    println!(
        "IG ORDER KINDS SMOKE DONE: limit and stop, each side, rested, moved and cancelled; nothing left"
    );
}
