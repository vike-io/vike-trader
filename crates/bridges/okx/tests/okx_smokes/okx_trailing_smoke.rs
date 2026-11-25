//! A TRAILING order on the OKX DEMO account, through the bridge's own `VenueRestOkx`: the algo endpoint's
//! `ordType=move_order_stop` with `callbackSpread` (the trail, a price distance) or `callbackRatio` (a
//! percent trail's percent / 100) and `activePx` (the activation) (`crates/bridges/okx/src/execution/rest/stop_algo.rs`'s `build_trailing_algo_params`; the fields are the
//! official "Place algo order" table's,
//! <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-place-algo-order>).
//! `tests/offline/trailing.rs` pins the same logic offline. The price trail PASSED on the demo 2026-11-08,
//! 2 of 2 (main `d1624c326e` / `f1a5acec7c`, store account `#15 okx/demo`): the okx row lists `trailing`
//! behind it. The PERCENT trail (`callbackRatio`, the attributes wave's B3) PASSED on the demo 2026-11-09,
//! 3 of 3 (`callbackRatio` "0.015" read back): okx's `percent_trail_kinds` lists `trailing` behind it.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture --test-threads=1 okx_trailing_smoke::
//!
//! ORDER-PLACING (`BTC-USDT-SWAP`), DEMO ONLY, the instrument's minimum size, through the stop-limit smoke's
//! `Run` (`okx_stop_limit_smoke.rs`: the same client, guard and flatten):
//!   * `okx_a_far_trailing_order_rests_reads_back_as_trailing_and_cancels`: a SELL trailing order whose
//!     activation is TWICE the last price (it cannot activate, so it cannot fire, in a smoke's lifetime), trail
//!     150 USD, read back from `order-algo` (the venue's `ordType`, `callbackSpread` and `activePx` must be what
//!     the builder wrote), listed by the order report as a `Trailing` (the account-wide
//!     `orders-algo-pending?ordType=move_order_stop` read, whose request shape is NOT yet measured), found by the
//!     by-order-client-id confirm, cancelled through the bridge (`cancel-order` answers "unknown", then `cancel-algos`),
//!     gone, and a second cancel is idempotent; meanwhile the business socket, opened as the pump opens it
//!     with the trailing gate ON (`crates/bridges/okx/src/execution/user_data.rs`'s `open_okx_algo_websocket`), must push the
//!     cancel on `algo-advance` as `OrderCanceled` under the order client id.
//!   * `okx_a_far_percent_trailing_order_rests_reads_back_its_ratio_and_cancels`: the same order and
//!     walk trailing by 1.5 PERCENT (`trail_percent`): the venue's row must hold `callbackRatio` 0.015
//!     (compared as a number) and no `callbackSpread`, then the same report, confirm, cancels and push.
//!   * `okx_the_trailing_smokes_leave_the_account_flat_and_empty`: READ-ONLY, last by name.
//!
//! The row does not list `trailing` before this smoke passes, so the smoke turns the gate ON by hand: its recon
//! client is built `with_trailing(true)` and its socket asks for `algo-advance`.
//!
//! What a run must print before its asserts, and what a miss costs: `MEASURED submit` refused for
//! `callbackSpread` off the tick grid means the trail must be rounded the other way (the builder truncates
//! it); `MEASURED order report` without the trailing row means the account-wide trailing read was refused (it
//! is logged, not fatal: the row stays closed until it is fixed); `MEASURED order-algo` showing a
//! `callbackRatio` means the venue read the distance as a ratio; no `algo-advance` cancel within ten seconds
//! means the channel is refused or carries no trailing order (the row stays closed). For the percent:
//! `MEASURED submit` refused means the venue will not take the exact ratio (its spelling, a bound, or
//! `callbackRatio` beside `activePx`: the venue's words say which) and okx's percent row stays closed; a
//! `callbackRatio` read back other than 0.015, or a `callbackSpread` beside it, means the venue read the
//! percent another way (the row stays closed).
//!
//! Gated like every `*_smoke.rs` here (the DEMO account's creds through `vike_secrets::smoke_directory`, an early
//! return when absent; the simulated-trading header on every transport). Credentials are never printed.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserError, WebsocketSessionUserMessage, WebsocketSessionUserOpenOutcome,
    WebsocketSessionUserStream,
};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, OrderType, now_ms};
use vike_okx::ReconcileClientOkx;
use vike_okx::execution::event_mapper::map_okx_algo_frame;
use vike_okx::execution::rest::{PATH_ORDERS_ALGO_PENDING, PATH_ORDERS_PENDING};
use vike_okx::execution::user_data::open_okx_algo_websocket;
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;

use crate::demo_credentials::load_demo_credentials;
use crate::demo_swap::build_rest;
use crate::okx_stop_limit_smoke::{INST_ID, Run, SYMBOL, accepted_id, pick, position, px};

const ROW_KEYS: &[&str] = &[
    "ordType",
    "state",
    "side",
    "sz",
    "callbackSpread",
    "callbackRatio",
    "activePx",
    "moveTriggerPx",
    "reduceOnly",
    "algoId",
];

/// The trailing order's distance, as the request states it and as the venue's row must echo it.
#[derive(Clone, Copy, Debug)]
enum Distance {
    /// `trail`, a price: `callbackSpread`
    Price(f64),
    /// `trail_percent`: `callbackRatio` = the percent / 100
    Percent(f64),
}

#[test]
#[ignore = "network + demo creds — places + cancels one real (non-activating) demo trailing order — run manually (see module doc)"]
fn okx_a_far_trailing_order_rests_reads_back_as_trailing_and_cancels() {
    far_trailing_order_round_trip("vttrail", Distance::Price(150.0));
}

#[test]
#[ignore = "network + demo creds — places + cancels one real (non-activating) demo PERCENT trailing order — run manually (see module doc)"]
fn okx_a_far_percent_trailing_order_rests_reads_back_its_ratio_and_cancels() {
    far_trailing_order_round_trip("vttrailp", Distance::Percent(1.5));
}

/// ONE far SELL trailing order trailing by `distance`, order client id prefix `tag` (each starts `vttrail`, which the
/// read-only twin checks): placed, read back three ways, cancelled twice, its cancel seen on the socket.
fn far_trailing_order_round_trip(tag: &str, distance: Distance) {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let Some(mut run) = Run::start() else { return };
    // the gate ON by hand: the row lists `trailing` only after this smoke passes
    run.reconcile = ReconcileClientOkx::new(
        OkxV5Signer::new(&creds, now_ms),
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
        demo_net().rest,
        INST_ID,
        run.rest.ct_val,
    )
    .with_trailing(true);
    let passphrase = creds.passphrase.as_ref().expect("OKX needs the API passphrase");
    let stop = AtomicBool::new(false);
    let business_websocket = demo_net().business_websocket;
    let mut ws = match open_okx_algo_websocket(
        &business_websocket,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        passphrase.expose(),
        now_ms,
        true,
        &stop,
        &vike_okx::transport::ratelimit::websocket_rate_gate(),
    ) {
        WebsocketSessionUserOpenOutcome::Ready(ws) => ws,
        WebsocketSessionUserOpenOutcome::Transport(e)
        | WebsocketSessionUserOpenOutcome::Refused(e) => {
            panic!("okx business socket connect failed: {e}")
        }
        WebsocketSessionUserOpenOutcome::Auth(e) => panic!("okx business socket auth failed: {e}"),
        WebsocketSessionUserOpenOutcome::Stopped => unreachable!("stop flag is never raised here"),
    };
    println!(
        "MEASURED the business socket opened with the trailing gate on (see the log for a refusal)"
    );
    let last = run.rest.last_price().expect("ticker");
    assert!(last > 0.0, "a last price is required");
    let activation = last * 2.0;

    // --- 1. a far SELL trailing order, read back three ways --------------------------------------------
    let order_client_id = format!("{tag}{}", now_ms() % 100_000_000);
    run.stops.push(order_client_id.clone());
    let mut terms = json!({
        "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL, "side": -1, "qty": run.leg,
        "order_type": "trailing", "trigger_price": activation, "ts": now_ms()
    });
    match distance {
        Distance::Price(trail) => terms["trail"] = json!(trail),
        Distance::Percent(p) => terms["trail_percent"] = json!(p),
    }
    let request: OrderRequest = serde_json::from_value(terms).unwrap();
    let events = VenueRest::submit_order(&run.rest, &request);
    println!(
        "MEASURED submit of trailing {order_client_id} activation={activation} {distance:?}: {events:?}"
    );
    let algo_id = accepted_id(&events);
    let row = run.algo_row(&order_client_id);
    println!(
        "MEASURED order-algo of the resting trailing order: {:?}",
        row.as_ref().map(|r| pick(r, ROW_KEYS))
    );
    let mine = run.report_row(&order_client_id);
    println!("MEASURED order report row of the trailing order: {mine:?}");
    let confirm = run.rest.confirm_order(&order_client_id, now_ms());
    println!("MEASURED by-order-client-id confirm of the resting trailing order: {confirm:?}");

    // --- 2. the bridge's cancel, then a second one --------------------------------------------------------
    let first = run.rest.cancel_order(&order_client_id);
    println!("MEASURED bridge cancel of the resting trailing order: {first:?}");
    let pushed = cancel_on_the_socket(&mut ws, &order_client_id, Duration::from_secs(10));
    println!("MEASURED events mapped from the business socket after the cancel: {pushed:?}");
    let after = run.algo_row(&order_client_id);
    println!(
        "MEASURED order-algo after the cancel: {:?}",
        after.as_ref().map(|r| pick(r, &["state"]))
    );
    let second = run.rest.cancel_order(&order_client_id);
    println!("MEASURED second bridge cancel: {second:?}");

    // --- 3. ASSERT, last ----------------------------------------------------------------------------------
    assert!(algo_id.is_some_and(|id| !id.is_empty()), "ACCEPTED with an algoId");
    let row = row.expect("order-algo of the resting trailing order");
    assert_eq!(row["ordType"], "move_order_stop");
    match distance {
        Distance::Price(trail) => {
            assert_eq!(
                px(&row["callbackSpread"]),
                run.tick(trail).parse().ok(),
                "the trail as written"
            );
        }
        // compared as NUMBERS (`px`): the venue echoes a price word without the wire's own zeros
        Distance::Percent(p) => {
            assert_eq!(px(&row["callbackRatio"]), Some(p / 100.0), "the ratio as written");
            let spread = px(&row["callbackSpread"]);
            assert!(
                spread.is_none_or(|d| d == 0.0),
                "no price distance beside the ratio: {spread:?}"
            );
        }
    }
    assert_eq!(px(&row["activePx"]), run.tick(activation).parse().ok(), "the activation");
    let mine = mine.expect("the order report must list the resting trailing order");
    assert_eq!(mine.order_type, OrderType::Trailing, "the planner reads the kind it was placed as");
    assert_eq!(mine.status, OrderStatus::Accepted);
    assert!(matches!(confirm[..], [Event::OrderAccepted(_)]), "{confirm:?}");
    assert_eq!(
        first.expect("the bridge cancel of a resting trailing order"),
        OrderCancelAck::Canceled
    );
    assert_eq!(after.expect("order-algo after the cancel")["state"], "canceled");
    second.expect("a second cancel of a gone trailing order is idempotent");
    assert!(
        pushed
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id)),
        "algo-advance must push the trailing order's cancel as OrderCanceled({order_client_id}): {pushed:?}"
    );
    println!(
        "okx trailing smoke green ({distance:?}): placed -> distance/activePx held -> Trailing report -> \
         cancelled -> cancel pushed on algo-advance"
    );
}

/// Read the business socket for up to `within`, mapping every text frame (`map_okx_algo_frame`), until
/// `order_client_id`'s `OrderCanceled` shows. Returns every event seen.
fn cancel_on_the_socket(
    ws: &mut impl WebsocketSessionUserStream,
    order_client_id: &str,
    within: Duration,
) -> Vec<Event> {
    let deadline = Instant::now() + within;
    let mut seen: Vec<Event> = Vec::new();
    while Instant::now() < deadline
        && !seen
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id))
    {
        match ws.recv() {
            Ok(WebsocketSessionUserMessage::Text(text)) => {
                if let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) {
                    println!("FRAME {}", frame.get("arg").map_or(String::new(), |a| a.to_string()));
                    seen.extend(map_okx_algo_frame(&frame));
                }
            }
            Ok(_) | Err(WebsocketSessionUserError::Timeout) => {}
            Err(WebsocketSessionUserError::Closed(why)) => {
                panic!("the business socket closed: {why}")
            }
        }
    }
    seen
}

/// READ-ONLY, and last of the module by name: no order of the trailing smoke rests (its `vttrail` order client id
/// prefix, regular, stop or trailing) and the position is flat. Other clients' orders are counted, never
/// touched.
#[test]
#[ignore = "network + demo creds — read-only check that the trailing smoke left nothing behind — run manually (see module doc)"]
fn okx_the_trailing_smokes_leave_the_account_flat_and_empty() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds, INST_ID);
    let leg = rest.properties.min_qty * rest.ct_val;
    let regular = rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(INST_ID))])
        .expect("orders-pending");
    let trailing = rest
        .call_get(
            PATH_ORDERS_ALGO_PENDING,
            &[("ordType", json!("move_order_stop")), ("instId", json!(INST_ID))],
        )
        .expect("orders-algo-pending of the trailing orders");
    let rows = |v: &serde_json::Value| v.as_array().cloned().unwrap_or_default();
    let left: Vec<String> = rows(&regular)
        .iter()
        .chain(rows(&trailing).iter())
        .flat_map(|r| [r["clOrdId"].as_str(), r["algoClOrdId"].as_str()])
        .flatten()
        .filter(|c| c.starts_with("vttrail"))
        .map(String::from)
        .collect();
    // The position as the live reconcile pass reads it (the production factory, demo host).
    let recon = vike_okx::recon_client(&creds, INST_ID, rest.ct_val, &demo_net())
        .expect("okx recon_client is always Some");
    let position = position(recon.as_ref());
    println!("MEASURED account at the end: position {position:?}; ours {left:?}");
    assert!(left.is_empty(), "a smoke order still rests: {left:?}");
    assert!(position.is_ok_and(|q| q.abs() < leg * 0.5), "the account must end FLAT");
}
