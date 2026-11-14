//! A TAKE-PROFIT on the OKX DEMO account, through the bridge's own `VenueRestOkx`: the stop's
//! `conditional` algo order on its take-profit side, `tpTriggerPx` and `tpOrdPx` (`-1` for a
//! market-if-touched, the limit for a limit-if-touched; `crates/bridges/okx/src/execution/rest/stop_algo.rs`'s
//! `build_stop_algo_params`, the fields the official "Place algo order" table's,
//! <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-place-algo-order>).
//! `tests/offline/take_profit.rs` pins the same logic offline. PASSED on the demo 2026-11-08, 3 of 3 (main
//! `d1624c326e` / `f1a5acec7c`, store account `#15 okx/demo`): the okx row lists `take_profit` behind it.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture --test-threads=1 okx_take_profit_smoke::
//!
//! ORDER-PLACING (`BTC-USDT-SWAP`), DEMO ONLY, the instrument's minimum size, through the stop-limit smoke's
//! `Run` (`okx_stop_limit_smoke.rs`: the same client, guard and flatten):
//!   * `okx_a_take_profit_rests_with_its_trigger_and_cancels`: a SELL market-if-touched and a SELL
//!     limit-if-touched at TWICE the last price (they cannot trigger in a smoke's lifetime), each read back from
//!     `order-algo` (the venue's `tpTriggerPx`/`tpOrdPx` must be what the builder wrote, the stop-loss side
//!     empty), listed by the order report as a `TakeProfit`, found by the by-order-client-id confirm, cancelled through the
//!     bridge, gone.
//!   * `okx_a_fired_limit_if_touched_resting_order_is_cancelled_by_the_bridge`: a SELL limit-if-touched a few
//!     dollars ABOVE the last price whose limit is 150 USD above it, so once the market touches the trigger its
//!     order RESTS; then the bridge's cancel of its order client id must reach that order by `ordId` (`stop_algo.rs`'s
//!     `cancel_fired_limit`). It refuses a non-flat account and flattens what the run opened. It also asks the
//!     restore planner how a restart reads it: the resting child, filed under our order client id, is ADOPTED.
//!   * `okx_the_take_profit_smokes_leave_the_account_flat_and_empty`: READ-ONLY, last by name.
//!
//! What a run must print before its asserts, and what a miss costs: `MEASURED submit` refused for the trigger's
//! side means the venue judges a take-profit's trigger against the last price the other way round from the
//! law (a SELL triggers at or above it): the kind is then refused, not flipped. `MEASURED order-algo` showing a
//! `tpOrdKind` other than `condition` means the default the builder leans on is not the venue's.
//!
//! Gated like every `*_smoke.rs` here (the DEMO account's creds through `vike_secrets::smoke_directory`, an early
//! return when absent; the simulated-trading header on every transport). Credentials are never printed.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_model::events::Event;
use vike_model::{OrderStatus, OrderType, now_ms};
use vike_okx::execution::rest::{PATH_ORDER_CREATE, PATH_ORDERS_ALGO_PENDING, PATH_ORDERS_PENDING};

use crate::demo_credentials::load_demo_credentials;
use crate::demo_swap::build_rest;
use crate::okx_stop_limit_smoke::{Run, SYMBOL, accepted_id, pick, position, px};

const ROW_KEYS: &[&str] = &[
    "ordType",
    "state",
    "side",
    "sz",
    "tpTriggerPx",
    "tpOrdPx",
    "tpOrdKind",
    "tpTriggerPxType",
    "slTriggerPx",
    "slOrdPx",
    "reduceOnly",
    "algoId",
];

#[test]
#[ignore = "network + demo creds — places + cancels two real (non-triggering) demo take-profits — run manually (see module doc)"]
fn okx_a_take_profit_rests_with_its_trigger_and_cancels() {
    vike_log::test_init();
    let Some(mut run) = Run::start() else { return };
    let last = run.rest.last_price().expect("ticker");
    assert!(last > 0.0, "a last price is required");
    let trigger = last * 2.0;

    for (tag, limit) in [("vttpmit", None), ("vttplit", Some(trigger * 0.99))] {
        // --- 1. a far SELL take-profit, read back three ways ----------------------------------------------
        let (order_client_id, events) = run.place(tag, -1, "take_profit", trigger, limit);
        println!(
            "MEASURED submit of take-profit {order_client_id} trigger={trigger} limit={limit:?}: {events:?}"
        );
        let algo_id = accepted_id(&events);
        let row = run.algo_row(&order_client_id);
        println!(
            "MEASURED order-algo of the resting take-profit: {:?}",
            row.as_ref().map(|r| pick(r, ROW_KEYS))
        );
        let mine = run.report_row(&order_client_id);
        println!("MEASURED order report row of the take-profit: {mine:?}");
        let confirm = run.rest.confirm_order(&order_client_id, now_ms());
        println!("MEASURED by-order-client-id confirm of the resting take-profit: {confirm:?}");

        // --- 2. the bridge's cancel, then a second one ----------------------------------------------------
        let first = run.rest.cancel_order(&order_client_id);
        println!("MEASURED bridge cancel of the resting take-profit: {first:?}");
        std::thread::sleep(Duration::from_secs(2));
        let after = run.algo_row(&order_client_id);
        println!(
            "MEASURED order-algo after the cancel: {:?}",
            after.as_ref().map(|r| pick(r, &["state"]))
        );
        let second = run.rest.cancel_order(&order_client_id);
        println!("MEASURED second bridge cancel: {second:?}");

        // --- 3. ASSERT, last ------------------------------------------------------------------------------
        assert!(algo_id.is_some_and(|id| !id.is_empty()), "{tag}: ACCEPTED with an algoId");
        let row = row.expect("order-algo of the resting take-profit");
        assert_eq!(row["ordType"], "conditional");
        assert_eq!(
            px(&row["tpTriggerPx"]),
            run.tick(trigger).parse().ok(),
            "the trigger as written"
        );
        let ord_px = limit.map_or_else(|| "-1".to_string(), |l| run.tick(l));
        assert_eq!(px(&row["tpOrdPx"]), ord_px.parse().ok(), "{tag}: the order price as written");
        assert!(row["slTriggerPx"].as_str().unwrap_or("").is_empty(), "no stop-loss side: {row}");
        let mine = mine.expect("the order report must list the resting take-profit");
        assert_eq!(
            mine.order_type,
            OrderType::TakeProfit,
            "the planner reads the kind it was placed as"
        );
        assert_eq!(mine.status, OrderStatus::Accepted);
        assert!(matches!(confirm[..], [Event::OrderAccepted(_)]), "{confirm:?}");
        assert_eq!(
            first.expect("the bridge cancel of a resting take-profit"),
            OrderCancelAck::Canceled
        );
        assert_eq!(after.expect("order-algo after the cancel")["state"], "canceled");
        second.expect("a second cancel of a gone take-profit is idempotent");
    }
    println!(
        "okx take-profit smoke green: MIT and LIT placed -> tp fields held -> TakeProfit report -> cancelled"
    );
}

#[test]
#[ignore = "network + demo creds — lets a demo limit-if-touched fire into a resting limit and cancels it; flattens — run manually (see module doc)"]
fn okx_a_fired_limit_if_touched_resting_order_is_cancelled_by_the_bridge() {
    vike_log::test_init();
    let Some(mut run) = Run::start() else { return };
    let base0 = run.position().expect("position");
    assert!(
        base0.abs() < run.leg * 0.5,
        "the demo account must be FLAT on {SYMBOL} before this smoke (position {base0}): it flattens \
         what it finds at the end and must not touch someone else's position"
    );

    // --- 1. a SELL limit-if-touched just above the market whose limit rests 150 USD above it; re-placed at
    // the new price while it does not fire
    let mut fired = None;
    for attempt in 0..10 {
        let distance = [10.0_f64, 6.0, 4.0][attempt.min(2)];
        let last = run.rest.last_price().expect("ticker");
        let (trigger, limit) = (last + distance, last + 150.0);
        let (order_client_id, events) =
            run.place("vttpfire", -1, "take_profit", trigger, Some(limit));
        if accepted_id(&events).is_none() {
            println!(
                "MEASURED take-profit {order_client_id} refused at trigger {trigger} limit {limit} (last {last}): {events:?}"
            );
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        println!(
            "PLACED take-profit {order_client_id} trigger={trigger} limit={limit} last={last} (attempt {attempt})"
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut state = None;
        while Instant::now() < deadline {
            state = run
                .algo_row(&order_client_id)
                .ok()
                .and_then(|r| r["state"].as_str().map(String::from));
            // the resting word is never spelled here: the lane's credential guard refuses that string
            if matches!(state.as_deref(), Some("effective" | "canceled" | "order_failed")) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        if state.as_deref() == Some("effective") {
            fired = Some(order_client_id);
            break;
        }
        println!(
            "MEASURED take-profit {order_client_id} did not fire in 60 s (venue state {state:?}); cancelling"
        );
        println!("  bridge cancel: {:?}", run.rest.cancel_order(&order_client_id));
    }
    let order_client_id =
        fired.expect("no take-profit fired in ten attempts: the market was too quiet");

    // --- 2. MEASURE: the fired row, the order it left, the report, the restart's reading -----------------
    let row = run.algo_row(&order_client_id).expect("order-algo of the fired take-profit");
    println!(
        "MEASURED order-algo of the FIRED take-profit: {}",
        pick(
            &row,
            &["state", "tpTriggerPx", "tpOrdPx", "actualSide", "ordId", "ordIdList", "actualPx"]
        )
    );
    let pending = run
        .rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("orders-pending");
    let child = pending.as_array().and_then(|rows| {
        rows.iter().find(|r| r["algoClOrdId"] == order_client_id.as_str()).cloned()
    });
    println!(
        "MEASURED the fired take-profit's order on orders-pending: {:?}",
        child.as_ref().map(|c| pick(
            c,
            &["ordId", "clOrdId", "algoClOrdId", "ordType", "px", "sz", "source"]
        ))
    );
    if let Some(id) = child.as_ref().and_then(|c| c["ordId"].as_str()) {
        run.children.push(id.to_string());
    }
    let mine = run.report_row(&order_client_id);
    println!("MEASURED order report row of the fired take-profit: {mine:?}");
    let plan = run.restore_plan(&order_client_id, OrderType::TakeProfit);
    println!("MEASURED restore plan of the fired, resting take-profit: {plan:?}");

    // --- 3. the bridge's cancel of the take-profit's order client id must reach the resting order -------------------
    let cancel = run.rest.cancel_order(&order_client_id);
    println!("MEASURED bridge cancel of the fired take-profit: {cancel:?}");
    std::thread::sleep(Duration::from_secs(2));
    let child_after = run.children.first().map(|id| {
        run.rest
            .call_get(PATH_ORDER_CREATE, &[("instId", json!(SYMBOL)), ("ordId", json!(id))])
            .map(|rows| pick(&rows[0], &["state", "accFillSz", "cancelSource"]))
    });
    println!("MEASURED the order after the bridge cancel: {child_after:?}");

    // --- 4. ASSERT, last ------------------------------------------------------------------------------
    let child =
        child.expect("the fired take-profit's limit must REST (it is 150 USD above the market)");
    assert_eq!(child["ordType"], "limit", "a limit-if-touched fires a LIMIT order");
    assert_eq!(child["px"].as_str(), Some(row["tpOrdPx"].as_str().unwrap_or("?")), "at its limit");
    assert!(
        mine.expect("the report files the resting order under our order client id")
            .status
            .is_live()
    );
    let plan = plan.expect("the reports a restart reads");
    assert!(
        plan.adopt.iter().any(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str())),
        "a restart adopts the resting order as the take-profit's: {plan:?}"
    );
    assert_eq!(
        cancel.expect("the bridge cancel reaches the fired take-profit's order"),
        OrderCancelAck::Canceled
    );
    let after = child_after.expect("a child id").expect("GET order of the child");
    assert!(after.contains("state=\"canceled\""), "{after}");
    run.children.clear(); // cancelled: nothing for the guard to do
    println!(
        "okx fired take-profit smoke green: fired -> limit rests under our order client id -> bridge cancel reached it"
    );
}

/// READ-ONLY, and last of the module by name: no order of the take-profit smokes rests (their `vttp` order client id
/// prefix, regular or algo) and the position is flat. Other clients' orders are counted, never touched.
#[test]
#[ignore = "network + demo creds — read-only check that the take-profit smokes left nothing behind — run manually (see module doc)"]
fn okx_the_take_profit_smokes_leave_the_account_flat_and_empty() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds, SYMBOL);
    let leg = rest.properties.min_qty * rest.ct_val;
    let regular = rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("orders-pending");
    let algo = rest
        .call_get(
            PATH_ORDERS_ALGO_PENDING,
            &[("ordType", json!("conditional")), ("instId", json!(SYMBOL))],
        )
        .expect("orders-algo-pending");
    let rows = |v: &serde_json::Value| v.as_array().cloned().unwrap_or_default();
    let left: Vec<String> = rows(&regular)
        .iter()
        .chain(rows(&algo).iter())
        .flat_map(|r| [r["clOrdId"].as_str(), r["algoClOrdId"].as_str()])
        .flatten()
        .filter(|c| c.starts_with("vttp"))
        .map(String::from)
        .collect();
    // The position as the live reconcile pass reads it (the production factory, demo host).
    let recon =
        vike_okx::recon_client(&creds, SYMBOL, rest.ct_val, &vike_okx::test_support::demo_net())
            .expect("okx recon_client is always Some");
    let position = position(recon.as_ref());
    println!("MEASURED account at the end: position {position:?}; ours {left:?}");
    assert!(left.is_empty(), "a smoke order still rests: {left:?}");
    assert!(position.is_ok_and(|q| q.abs() < leg * 0.5), "the account must end FLAT");
}
