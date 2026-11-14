//! A STOP-LIMIT on the OKX DEMO account, through the bridge's own `VenueRestOkx`: it is the stop's
//! `conditional` algo order with the limit in `slOrdPx` (`crates/bridges/okx/src/execution/rest/stop_algo.rs`'s
//! `build_stop_algo_params`; the field is the official "Place algo order" table's,
//! <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-place-algo-order>).
//! `tests/offline/stop_limit.rs` pins the same logic offline.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture --test-threads=1 okx_stop_limit_smoke::
//!
//! ORDER-PLACING (`BTC-USDT-SWAP`), DEMO ONLY, the instrument's minimum size:
//!   * `okx_a_stop_limit_rests_with_its_limit_and_cancels`: a SELL stop-limit at HALF the last price
//!     (it cannot trigger in a smoke's lifetime), read back from `order-algo` (the venue's `slOrdPx` must be
//!     the limit the builder wrote), listed by the order report as a `StopLimit`, found by the by-order-client-id
//!     confirm, cancelled through the bridge, gone.
//!   * `okx_a_fired_stop_limits_resting_order_is_cancelled_by_the_bridge`: a BUY stop-limit a few dollars
//!     above the last price whose limit is 150 USD BELOW it, so once it fires its order RESTS; then the
//!     bridge's cancel of the stop's order client id must reach that order (its `clOrdId` is the venue's, not ours:
//!     `stop_algo.rs`'s `cancel_fired_limit`). It refuses a non-flat account and flattens what the run
//!     opened (the resting order can fill if the market falls 150 USD meanwhile). It also asks the restore
//!     planner how a restart would read the fired stop: the resting child, filed under our order client id, is ADOPTED.
//!   * `okx_the_stop_limit_smokes_leave_the_account_flat_and_empty`: READ-ONLY, last by name: no order with a
//!     stop-limit smoke's order client id prefix rests and the position is flat.
//!
//! The stop-limit that fires AND FILLS through the whole adapter (both pumps) is
//! `okx_fired_stop_smoke.rs`'s `okx_a_stop_limit_that_fires_and_fills_ends_filled_under_its_own_order_client_id`; the
//! filter `stop_limit` runs all four.
//!
//! Gated like every `*_smoke.rs` here: the DEMO account's creds through `vike_secrets::smoke_directory`
//! (asked for `AccountTier::Demo`), a `tracing::warn!` and an early return when absent; every transport is
//! `rest_transport(true)` (the simulated-trading header: demo and mainnet share the host). The
//! account is the node's own demo account, so a node reconcile pass that lands while an order rests holds a
//! transient `UnknownOrder` (cleared by its next pass). Credentials are never printed: only venue words and
//! ids of this smoke's own orders reach stdout. **Measure first, assert last:** every venue answer is
//! printed as `MEASURED ...` before the asserts. A `Drop` guard cancels every stop and child it placed and
//! flattens on every exit path; a run killed from outside skips it: check the demo account by hand.

use std::time::{Duration, Instant};

use crate::demo_credentials::load_demo_credentials;
use crate::demo_swap::build_rest;
use serde_json::{Value, json};
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_execution::recon::{
    RESTORE_QUIET_PASSES, ReconcileClient, RestorePlan, RestoreScope, RestoredOrderRef,
    plan_restore,
};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, OrderType, now_ms};
use vike_okx::ReconcileClientOkx;
use vike_okx::execution::rest::{
    PATH_CANCEL_ALGOS, PATH_ORDER_ALGO, PATH_ORDER_CANCEL, PATH_ORDER_CREATE,
    PATH_ORDERS_ALGO_PENDING, PATH_ORDERS_PENDING, VenueRestOkx,
};
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;
use vike_okx::transport::client::unwrap_okx;

pub(crate) const SYMBOL: &str = "BTC-USDT-SWAP";

type SwapRest = VenueRestOkx<RestTransportJsonUreq>;

/// What a run placed, cancelled and flattened on every exit path. Shared with
/// `okx_take_profit_smoke.rs`, which places its take-profits through [`Run::place`].
pub(crate) struct Run {
    pub(crate) rest: SwapRest,
    pub(crate) reconcile: ReconcileClientOkx<RestTransportJsonUreq>,
    /// One minimum lot, in BASE units.
    pub(crate) leg: f64,
    pub(crate) stops: Vec<String>,
    /// The `ordId`s of the regular orders a fired stop-limit left on the book.
    pub(crate) children: Vec<String>,
}

impl Run {
    pub(crate) fn start() -> Option<Self> {
        let creds = load_demo_credentials()?;
        assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");
        let rest = build_rest(&creds, SYMBOL);
        let recon = ReconcileClientOkx::new(
            OkxV5Signer::new(&creds, now_ms),
            rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
            demo_net().rest,
            SYMBOL,
            rest.ct_val,
        );
        let leg = rest.properties.min_qty * rest.ct_val;
        Some(Self { rest, reconcile: recon, leg, stops: Vec::new(), children: Vec::new() })
    }

    pub(crate) fn tick(&self, px: f64) -> String {
        format_to_step_f(px, self.rest.properties.tick_size)
    }

    fn stop_limit(
        &mut self,
        tag: &str,
        side: i32,
        trigger: f64,
        limit: f64,
    ) -> (String, Vec<Event>) {
        self.place(tag, side, "stop_limit", trigger, Some(limit))
    }

    /// One minimum lot of `order_type` (a trigger kind) through the bridge's own submit, remembered for
    /// the guard's cancel.
    pub(crate) fn place(
        &mut self,
        tag: &str,
        side: i32,
        order_type: &str,
        trigger: f64,
        limit: Option<f64>,
    ) -> (String, Vec<Event>) {
        let order_client_id = format!("{tag}{}", now_ms() % 100_000_000);
        self.stops.push(order_client_id.clone());
        let request: OrderRequest = serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL, "side": side, "qty": self.leg,
            "order_type": order_type, "trigger_price": trigger, "price": limit, "ts": now_ms()
        }))
        .unwrap();
        let events = VenueRest::submit_order(&self.rest, &request);
        (order_client_id, events)
    }

    /// `GET order-algo` by `algoClOrdId`: the venue's own row of the stop.
    pub(crate) fn algo_row(&self, order_client_id: &str) -> Result<Value, String> {
        self.rest
            .transport
            .signed_requery(
                &self.rest.base_url,
                PATH_ORDER_ALGO,
                "GET",
                &[("algoClOrdId", json!(order_client_id))],
                &self.rest.signer,
            )
            .and_then(unwrap_okx)
            .map(|rows| rows[0].clone())
            .map_err(|e| format!("{} {}", e.code, e.msg))
    }

    /// The order report's row for `order_client_id`, `None` when it lists none (or the read failed: printed).
    pub(crate) fn report_row(
        &self,
        order_client_id: &str,
    ) -> Option<vike_model::OrderStatusReport> {
        match self.reconcile.fetch_order_status_reports(0) {
            Ok(rows) => {
                rows.into_iter().find(|r| r.order_client_id.as_deref() == Some(order_client_id))
            }
            Err(e) => {
                println!("MEASURED order report read failed: {e}");
                None
            }
        }
    }

    /// How a RESTART would judge `order_client_id`: the recon client's order and fill reports through the real
    /// restore planner, for a `kind` record that has already counted every quiet pass.
    pub(crate) fn restore_plan(
        &self,
        order_client_id: &str,
        kind: OrderType,
    ) -> Result<RestorePlan, String> {
        let orders = self.reconcile.fetch_order_status_reports(0)?;
        let fills = self.reconcile.fetch_fill_reports(now_ms() - 600_000)?;
        let scope = RestoreScope {
            venue: "okx".to_string(),
            account: None,
            symbols: vec![SYMBOL.to_string()],
            lists_trigger_orders: self.reconcile.lists_trigger_orders(),
        };
        let record = RestoredOrderRef {
            order_client_id: order_client_id.to_string(),
            venue: Some("okx".to_string()),
            symbol: Some(SYMBOL.to_string()),
            account: None,
            order_type: Some(kind),
            venue_order_id: None,
            quiet_passes: RESTORE_QUIET_PASSES,
        };
        Ok(plan_restore(&scope, &[record], &orders, &fills))
    }

    /// The venue's position in BASE units (signed), as the live reconcile pass reads it (the
    /// production `ReconcileClientOkx`'s position report).
    pub(crate) fn position(&self) -> Result<f64, String> {
        position(&self.reconcile)
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        for order_client_id in &self.stops {
            if let Err(e) = self.rest.cancel_order(order_client_id) {
                println!(
                    "CLEANUP: bridge cancel of {order_client_id} answered {} {}",
                    e.code, e.msg
                );
            }
            let body = json!([{ "instId": SYMBOL, "algoClOrdId": order_client_id }]).to_string();
            let _ = self.rest.transport.signed_json(
                &self.rest.base_url,
                PATH_CANCEL_ALGOS,
                "POST",
                &body,
                &self.rest.signer,
            );
        }
        for id in &self.children {
            let answer = self.rest.transport.signed(
                &self.rest.base_url,
                PATH_ORDER_CANCEL,
                "POST",
                &[("instId", json!(SYMBOL)), ("ordId", json!(id))],
                &self.rest.signer,
            );
            println!("CLEANUP: cancel of child {id}: {}", answer.is_ok());
        }
        match self.position() {
            Ok(q) if q.abs() > self.leg * 0.5 => {
                let order_client_id = format!("vtslflat{}", now_ms() % 100_000_000);
                let request: OrderRequest = serde_json::from_value(json!({
                    "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL,
                    "side": vike_model::closing_side(q), "qty": q.abs(), "order_type": "market",
                    "reduce_only": true, "ts": now_ms()
                }))
                .unwrap();
                println!(
                    "CLEANUP flatten {q}: {:?}",
                    VenueRest::submit_order(&self.rest, &request)
                );
            }
            Ok(_) => {}
            Err(e) => {
                println!("CLEANUP could not read the {SYMBOL} position ({e}): CHECK IT BY HAND")
            }
        }
    }
}

/// The signed `SYMBOL` position in BASE units off the recon client's position report.
pub(crate) fn position(recon: &dyn ReconcileClient) -> Result<f64, String> {
    let rows = recon.fetch_position_status_reports()?;
    Ok(rows.iter().filter(|p| p.symbol == SYMBOL).map(|p| p.qty).sum())
}

/// The named fields of a venue row, as `key=value` text.
pub(crate) fn pick(row: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| format!("{k}={}", row.get(*k).unwrap_or(&Value::Null)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A venue price word as a NUMBER, the only way to compare it: OKX echoes a price without the wire's
/// trailing zeros (MEASURED on the demo, 2026-11-08: `"151646.00"` sent reads back `"151646"`, a
/// `callbackSpread` of `"150.00"` reads back `"150.0000000000000000"`).
pub(crate) fn px(v: &Value) -> Option<f64> {
    v.as_str()?.parse().ok()
}

pub(crate) fn accepted_id(events: &[Event]) -> Option<String> {
    match events.last() {
        Some(Event::OrderAccepted(a)) => a.venue_order_id.as_deref().map(str::to_string),
        _ => None,
    }
}

#[test]
#[ignore = "network + demo creds — places + cancels a real (non-triggering) demo stop-limit — run manually (see module doc)"]
fn okx_a_stop_limit_rests_with_its_limit_and_cancels() {
    vike_log::test_init();
    let Some(mut run) = Run::start() else { return };

    // --- 1. a far SELL stop-limit: trigger at half the last price, limit 1% under it -----------------
    let last = run.rest.last_price().expect("ticker");
    assert!(last > 0.0, "live last price required");
    let trigger = last * 0.5;
    let limit = trigger * 0.99;
    let (order_client_id, events) = run.stop_limit("vtslrest", -1, trigger, limit);
    println!(
        "MEASURED submit of stop-limit {order_client_id} trigger={trigger} limit={limit}: {events:?}"
    );
    let algo_id = accepted_id(&events);

    // --- 2. read it back: the venue's own row, the order report, the by-order-client-id confirm -----------------
    let row = run.algo_row(&order_client_id);
    match &row {
        Ok(r) => println!(
            "MEASURED order-algo of the resting stop-limit: {}",
            pick(
                r,
                &[
                    "ordType",
                    "state",
                    "side",
                    "sz",
                    "slTriggerPx",
                    "slOrdPx",
                    "slTriggerPxType",
                    "reduceOnly",
                    "algoId"
                ]
            )
        ),
        Err(e) => println!("MEASURED order-algo of the resting stop-limit: Err {e}"),
    }
    let mine = run.report_row(&order_client_id);
    println!("MEASURED order report row of the stop-limit: {mine:?}");
    let confirm = run.rest.confirm_order(&order_client_id, now_ms());
    println!("MEASURED by-order-client-id confirm of the resting stop-limit: {confirm:?}");

    // --- 3. the bridge's cancel, then a second one -----------------------------------------------------
    let first = run.rest.cancel_order(&order_client_id);
    println!("MEASURED bridge cancel of the resting stop-limit: {first:?}");
    std::thread::sleep(Duration::from_secs(2));
    let after = run.algo_row(&order_client_id);
    println!(
        "MEASURED order-algo after the cancel: {:?}",
        after.as_ref().map(|r| pick(r, &["state"]))
    );
    let second = run.rest.cancel_order(&order_client_id);
    println!("MEASURED second bridge cancel: {second:?}");

    // --- 4. ASSERT, last --------------------------------------------------------------------------------
    assert!(
        algo_id.is_some_and(|id| !id.is_empty()),
        "the stop-limit must be ACCEPTED with an algoId"
    );
    let row = row.expect("order-algo of the resting stop-limit");
    assert_eq!(row["ordType"], "conditional");
    assert_eq!(px(&row["slTriggerPx"]), run.tick(trigger).parse().ok(), "the trigger as written");
    assert_eq!(px(&row["slOrdPx"]), run.tick(limit).parse().ok(), "the venue holds the LIMIT");
    let mine = mine.expect("the order report must list the resting stop-limit");
    assert_eq!(
        mine.order_type,
        OrderType::StopLimit,
        "the planner reads the kind it was placed as"
    );
    assert_eq!(mine.status, OrderStatus::Accepted);
    assert!((mine.qty - run.leg).abs() <= run.leg * 1e-6, "{} vs {}", mine.qty, run.leg);
    assert!(matches!(confirm[..], [Event::OrderAccepted(_)]), "{confirm:?}");
    assert_eq!(first.expect("the bridge cancel of a resting stop-limit"), OrderCancelAck::Canceled);
    assert_eq!(after.expect("order-algo after the cancel")["state"], "canceled");
    second.expect("a second cancel of a gone stop-limit is idempotent");
    println!("okx stop-limit smoke green: placed -> slOrdPx held -> StopLimit report -> cancelled");
}

#[test]
#[ignore = "network + demo creds — lets a demo stop-limit fire into a resting limit and cancels it; flattens — run manually (see module doc)"]
fn okx_a_fired_stop_limits_resting_order_is_cancelled_by_the_bridge() {
    vike_log::test_init();
    let Some(mut run) = Run::start() else { return };
    let base0 = run.position().expect("position");
    assert!(
        base0.abs() < run.leg * 0.5,
        "the demo account must be FLAT on {SYMBOL} before this smoke (position {base0}): it flattens \
         what it finds at the end and must not touch someone else's position"
    );

    // --- 1. a BUY stop-limit just above the market whose limit rests 150 USD under it; re-placed at the
    // new price while it does not fire (a trigger the venue calls "not above the last price" is refused:
    // the price moved between the ticker read and the order; try again)
    let mut fired = None;
    for attempt in 0..10 {
        let distance = [10.0_f64, 6.0, 4.0][attempt.min(2)];
        let last = run.rest.last_price().expect("ticker");
        let trigger = last + distance;
        let limit = last - 150.0;
        let (order_client_id, events) = run.stop_limit("vtslfire", 1, trigger, limit);
        if accepted_id(&events).is_none() {
            println!(
                "MEASURED stop-limit {order_client_id} refused at trigger {trigger} limit {limit} (last {last}): {events:?}"
            );
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        println!(
            "PLACED stop-limit {order_client_id} trigger={trigger} limit={limit} last={last} (attempt {attempt})"
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut state = None;
        while Instant::now() < deadline {
            state = run
                .algo_row(&order_client_id)
                .ok()
                .and_then(|r| r["state"].as_str().map(String::from));
            // the stop's resting word is never spelled here: the lane's credential guard refuses that string
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
            "MEASURED stop-limit {order_client_id} did not fire in 60 s (venue state {state:?}); cancelling"
        );
        println!("  bridge cancel: {:?}", run.rest.cancel_order(&order_client_id));
    }
    let order_client_id =
        fired.expect("no stop-limit fired in ten attempts: the market was too quiet");

    // --- 2. MEASURE: the fired stop's row, the order it left, the report, the confirm ------------------
    let row = run.algo_row(&order_client_id).expect("order-algo of the fired stop-limit");
    println!(
        "MEASURED order-algo of the FIRED stop-limit: {}",
        pick(
            &row,
            &[
                "state",
                "slTriggerPx",
                "slOrdPx",
                "ordId",
                "ordIdList",
                "actualPx",
                "actualSz",
                "triggerTime"
            ]
        )
    );
    let pending = run
        .rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("orders-pending");
    let child = pending.as_array().and_then(|rows| {
        rows.iter().find(|r| r["algoClOrdId"] == order_client_id.as_str()).cloned()
    });
    match &child {
        Some(c) => println!(
            "MEASURED the fired stop-limit's order on orders-pending: {}",
            pick(c, &["ordId", "clOrdId", "algoClOrdId", "ordType", "px", "sz", "state", "source"])
        ),
        None => {
            println!("MEASURED the fired stop-limit's order is NOT on orders-pending (it filled?)")
        }
    }
    if let Some(id) = child.as_ref().and_then(|c| c["ordId"].as_str()) {
        run.children.push(id.to_string());
    }
    let mine = run.report_row(&order_client_id);
    println!("MEASURED order report row of the fired stop-limit: {mine:?}");
    let plan = run.restore_plan(&order_client_id, OrderType::StopLimit);
    println!("MEASURED restore plan of the fired, resting stop-limit: {plan:?}");
    println!(
        "MEASURED by-order-client-id confirm of the fired stop-limit: {:?}",
        run.rest.confirm_order(&order_client_id, now_ms())
    );
    let probe = run.rest.transport.signed_json(
        &run.rest.base_url,
        PATH_CANCEL_ALGOS,
        "POST",
        &json!([{ "instId": SYMBOL, "algoClOrdId": order_client_id }]).to_string(),
        &run.rest.signer,
    );
    println!("MEASURED raw cancel-algos of the FIRED stop-limit: {probe:?}");

    // --- 3. the bridge's cancel of the stop's order client id must reach the resting order -----------------------
    let cancel = run.rest.cancel_order(&order_client_id);
    println!("MEASURED bridge cancel of the fired stop-limit: {cancel:?}");
    std::thread::sleep(Duration::from_secs(2));
    let child_after = run.children.first().map(|id| {
        run.rest
            .call_get(PATH_ORDER_CREATE, &[("instId", json!(SYMBOL)), ("ordId", json!(id))])
            .map(|rows| pick(&rows[0], &["state", "accFillSz", "cancelSource"]))
    });
    println!("MEASURED the order after the bridge cancel: {child_after:?}");

    // --- 4. ASSERT, last --------------------------------------------------------------------------------
    let child =
        child.expect("the fired stop-limit's limit must REST (it is 150 USD under the market)");
    assert_eq!(child["ordType"], "limit", "a stop-limit fires a LIMIT order");
    assert_eq!(
        child["px"].as_str(),
        Some(row["slOrdPx"].as_str().unwrap_or("?")),
        "at the stop's limit"
    );
    let mine = mine.expect("the report files the resting order under the stop's order client id");
    assert!(mine.status.is_live(), "{mine:?}");
    let plan = plan.expect("the reports a restart reads");
    assert!(
        plan.adopt.iter().any(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str())),
        "a restart adopts the resting order as the stop-limit's: {plan:?}"
    );
    assert_eq!(
        cancel.expect("the bridge cancel reaches the fired stop-limit's order"),
        OrderCancelAck::Canceled
    );
    let after = child_after.expect("a child id").expect("GET order of the child");
    assert!(after.contains("state=\"canceled\""), "{after}");
    run.children.clear(); // cancelled: nothing for the guard to do
    println!(
        "okx fired stop-limit smoke green: fired -> limit rests under our order client id -> bridge cancel reached it"
    );
}

/// READ-ONLY, and last of the module by name: the stop-limit smokes left the demo account as they found
/// it: no order of theirs rests (regular or algo, by their `vtsl`/`vtlf` order client id prefixes) and the position
/// is flat. Other clients' orders are counted, never touched.
#[test]
#[ignore = "network + demo creds — read-only check that the stop-limit smokes left nothing behind — run manually (see module doc)"]
fn okx_the_stop_limit_smokes_leave_the_account_flat_and_empty() {
    vike_log::test_init();
    // No `Run`: its guard flattens what it finds, and this test changes nothing.
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds, SYMBOL);
    let leg = rest.properties.min_qty * rest.ct_val;
    let ours = |order_client_id: &str| {
        order_client_id.starts_with("vtsl") || order_client_id.starts_with("vtlf")
    };
    let regular = rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("orders-pending");
    let algo = rest
        .call_get(
            PATH_ORDERS_ALGO_PENDING,
            &[("ordType", json!("conditional")), ("instId", json!(SYMBOL))],
        )
        .expect("orders-algo-pending");
    let rows = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let left: Vec<String> = rows(&regular)
        .iter()
        .chain(rows(&algo).iter())
        .flat_map(|r| [r["clOrdId"].as_str(), r["algoClOrdId"].as_str()])
        .flatten()
        .filter(|c| ours(c))
        .map(String::from)
        .collect();
    // The position as the live reconcile pass reads it (the production factory, demo host).
    let recon = vike_okx::recon_client(&creds, SYMBOL, rest.ct_val, &demo_net())
        .expect("okx recon_client is always Some");
    let position = position(recon.as_ref());
    println!(
        "MEASURED account at the end: position {position:?}; resting regular {} algo {}; ours {left:?}",
        rows(&regular).len(),
        rows(&algo).len()
    );
    assert!(left.is_empty(), "a smoke order still rests: {left:?}");
    assert!(position.is_ok_and(|q| q.abs() < leg * 0.5), "the account must end FLAT");
}
