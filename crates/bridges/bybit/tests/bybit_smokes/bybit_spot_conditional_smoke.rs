//! **The bybit SPOT conditional demo smoke**: what `crates/bridges/bybit/src/execution/rest/mod.rs`'s `build_order_params`
//! sends for a stop-limit and a take-profit on a SPOT pair (`orderFilter StopOrder` plus `triggerPrice`, no
//! direction or source word: https://bybit-exchange.github.io/docs/v5/order/create-order) is what the Bybit
//! DEMO venue rests, lists back, amends and cancels. PASSED on the demo 2026-11-08, 5 of 5 (main `d1624c326e`,
//! store account `#4 bybit/demo`). The page states no spot direction rule, so the direction the venue reads is
//! this smoke's main `FINDING`.
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture --test-threads=1 bybit_spot_conditional_smoke::
//!
//! ORDER-PLACING (the spot pair `DOGEUSDT`, its user name too, the DEMO host only, the account
//! `demo_label` names): rests five conditionals that cannot fire in the seconds they rest, each sized just over
//! the pair's `minOrderAmt`, all on the side of the market their kind means: a stop-limit BUY 3% over the last
//! price (limit 20 ticks over), a stop-limit SELL 3% under (limit 20 ticks under), a limit-if-touched BUY 3%
//! under (limit 20 ticks over the trigger), and two market-if-touched, a SELL 3% over and a BUY 3% under. A
//! conditional holds no asset until it fires (the page's `StopOrder` row), so a SELL of coin the account may not
//! hold is placed too; a refusal of one is printed as a `FINDING` and that order is skipped, never a proof.
//!
//! What it proves, per order:
//!   1. `submit_order` through the real builder is ACCEPTED with the venue's order id;
//!   2. the venue's own row (`/v5/order/realtime`, `category=spot`, by `orderLinkId`) echoes `orderType`, the
//!      limit as `price`, the trigger as `triggerPrice`; printed as a `CAPTURED` line, its `orderStatus`,
//!      `stopOrderType`, `orderFilter`, `marketUnit` and `triggerDirection` are `FINDING`s, and a direction the
//!      venue states must be the side of the market the trigger was placed on;
//!   3. the real `ReconcileClientBybit` (mounted on `DOGEUSDT`) lists it back LIVE carrying the order client id on the side
//!      placed: the claim `lists_trigger_orders` holds for the spot book only if it does (the read sends no
//!      `orderFilter`, which the open-order page says lists "all kinds of orders"); the kind it reads is a
//!      `FINDING`;
//!   4. a limited order's native amend (`VenueRestBybit::modify_order`, `category=spot`) moves the LIMIT and
//!      leaves the trigger where it was (`FINDING`); then the trigger amend (`VenueRest::modify_trigger`, the
//!      amend's `triggerPrice`) moves the TRIGGER 10 ticks further from the market and leaves the limit;
//!   5. the cancel ends it, and the row the venue keeps for it is terminal (`FINDING`: its word).
//!
//! Only `spot conditional smoke green` is a proof; a `SKIP:` line is not.
//!
//! CLEANUP: a `CancelOnDrop` guard per order client id cancels through the placing client on every exit path
//! (idempotent: the venue's already-gone codes are swallowed). A run killed from outside leaves the orders
//! resting: cancel the order client ids the log names (`vtsc…`) by hand on the demo account.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::CancelOnDrop;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::ReconcileClientBybit;
use vike_bybit::execution::rest::{PATH_INSTRUMENTS, VenueRestBybit, seed_endpoints};
use vike_bybit::history::Category;
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_execution::recon::ReconcileClient;
use vike_model::events::Event;
use vike_model::{OrderRequest, SymbolProperties, now_ms};
use vike_secrets::credentials::Credentials;

use crate::bybit_stop_limit_smoke::{PerpRest, load_demo_credentials, on_grid, text, venue_row};

/// The spot pair: its USER name (decision 0290) is the venue's own name, which its kept row stores in `spot` (the list
/// `load_demo_credentials` holds).
const PAIR: &str = "DOGEUSDT";

/// A number of the spot row's `section` (`priceFilter`, `lotSizeFilter`), the venue's decimal string.
fn grid_number(row: &Value, section: &str, key: &str) -> f64 {
    row[section][key].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0)
}

/// A spot client on the pair's own grid, read from the DEMO host's `instruments-info?category=spot`: the spot
/// row's `basePrecision` is the quantity step and `minOrderAmt` the minimum value, as the exec plane reads them.
fn build_spot_rest(creds: &Credentials) -> PerpRest {
    let gate = || vike_bybit::transport::ratelimit::rest_rate_gate();
    let demo = seed_endpoints(vike_model::AccountTier::Demo).rest; // the release seed's demo host
    let info = rest_transport()
        .with_rate_gate(gate())
        .signed(
            &demo,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("spot")), ("symbol", json!(PAIR))],
            &BybitV5Signer::new(creds, now_ms),
        )
        .expect("instruments-info");
    let row = info["result"]["list"][0].clone();
    assert_eq!(text(&row, "status"), "Trading", "{PAIR} must be a trading spot pair: {row}");
    let properties = SymbolProperties {
        tick_size: grid_number(&row, "priceFilter", "tickSize"),
        step_size: grid_number(&row, "lotSizeFilter", "basePrecision"),
        min_qty: grid_number(&row, "lotSizeFilter", "minOrderQty"),
        min_notional: grid_number(&row, "lotSizeFilter", "minOrderAmt"),
        ..Default::default()
    };
    assert!(properties.tick_size > 0.0 && properties.step_size > 0.0, "{PAIR} grid: {row}");
    VenueRestBybit {
        category: Category::Spot,
        signer: BybitV5Signer::new(creds, now_ms),
        transport: rest_transport().with_rate_gate(gate()),
        base_url: demo,
        mainnet: false,
        symbol: PAIR.to_string(),
        properties,
        leverage: 1.0,
    }
}

#[test]
#[ignore = "network + demo creds — places, amends and cancels five real (non-firing) demo spot conditionals — run manually (see module doc)"]
fn bybit_spot_conditionals_rest_list_amend_and_cancel() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_spot_rest(&creds);
    let recon = ReconcileClientBybit::new(
        BybitV5Signer::new(&creds, now_ms),
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        rest.base_url.as_str(), // the demo host `build_spot_rest` signs against
        PAIR,
    );
    let last = rest.last_price().expect("tickers");
    assert!(last > 0.0, "a live last price is required");
    let properties = rest.properties;
    let tick = properties.tick_size;
    let over = on_grid(last * 1.03, tick);
    let under = on_grid(last * 0.97, tick);

    // (kind, side, trigger, limit, the direction the trigger's side of the market means: 1 rise, 2 fall)
    let cases = [
        ("stop_limit", 1_i32, over, Some(on_grid(over + 20.0 * tick, tick)), 1_i64),
        ("stop_limit", -1_i32, under, Some(on_grid(under - 20.0 * tick, tick)), 2_i64),
        ("take_profit", 1_i32, under, Some(on_grid(under + 20.0 * tick, tick)), 2_i64),
        ("take_profit", -1_i32, over, None, 1_i64),
        ("take_profit", 1_i32, under, None, 2_i64),
    ];
    let mut proved = 0;
    for (kind, side, trigger, limit, direction) in cases {
        let sizing = limit.unwrap_or(trigger);
        let qty = f64::max(properties.min_qty, (properties.min_notional * 1.4) / sizing);
        let qty = (qty / properties.step_size).ceil() * properties.step_size;
        let order_client_id = format!(
            "vtsc{}{}{}",
            if kind == "stop_limit" {
                "s"
            } else if limit.is_some() {
                "l"
            } else {
                "m"
            },
            if side > 0 { "b" } else { "s" },
            now_ms() % 100_000_000
        );
        let request: OrderRequest = serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "bybit", "symbol": PAIR, "side": side, "qty": qty,
            "order_type": kind, "price": limit, "trigger_price": trigger, "ts": now_ms()
        }))
        .unwrap();
        tracing::info!(target: "vike_bybit", order_client_id, kind, side, last, trigger, ?limit, qty, "placing a spot conditional");

        // --- 1. placed through the real builder ------------------------------------------------
        let events = VenueRest::submit_order(&rest, &request);
        // From here on the order may rest: the guard cancels it on every exit path.
        let cleanup = CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };
        let venue_id = match events.last() {
            Some(Event::OrderAccepted(accepted)) => {
                accepted.venue_order_id.clone().expect("the accept names the venue id")
            }
            Some(Event::OrderRejected(rejected)) if side < 0 => {
                println!(
                    "FINDING bybit spot {kind} side {side}: the demo REFUSED a SELL conditional: {}",
                    rejected.reason
                );
                continue;
            }
            _ => panic!("{order_client_id}: the demo must ACCEPT the spot {kind}: {events:?}"),
        };
        tracing::info!(target: "vike_bybit", order_client_id, venue_order_id = %venue_id, "ACCEPTED");

        // --- 2. the venue's own row -------------------------------------------------------------
        let row = venue_row(&rest, &order_client_id);
        println!("CAPTURED bybit spot {kind} side {side}: {row}");
        println!(
            "FINDING bybit spot {kind} side {side}: orderStatus={:?} stopOrderType={:?} orderFilter={:?} \
             marketUnit={:?} triggerDirection={:?} timeInForce={:?}",
            text(&row, "orderStatus"),
            text(&row, "stopOrderType"),
            text(&row, "orderFilter"),
            text(&row, "marketUnit"),
            text(&row, "triggerDirection"),
            text(&row, "timeInForce")
        );
        let number = |key: &str| text(&row, key).parse::<f64>().unwrap_or(f64::NAN);
        let fires_as = if limit.is_some() { "Limit" } else { "Market" };
        assert_eq!(text(&row, "orderType"), fires_as, "{order_client_id}: {row}");
        if let Some(limit) = limit {
            assert!(
                (number("price") - limit).abs() < tick / 2.0,
                "{order_client_id}: the limit is `price`: {row}"
            );
        }
        assert!(
            (number("triggerPrice") - trigger).abs() < tick / 2.0,
            "{order_client_id}: the trigger is `triggerPrice`: {row}"
        );
        if let Some(stated) = row["triggerDirection"].as_i64().filter(|d| *d != 0) {
            assert_eq!(
                stated, direction,
                "{order_client_id}: the venue fires toward the trigger's side: {row}"
            );
        }

        // --- 3. the reconcile read lists it back live -------------------------------------------
        let reports = recon.fetch_order_status_reports(0).expect("order/realtime");
        let report = reports
            .iter()
            .find(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str()))
            .unwrap_or_else(|| {
                panic!("{order_client_id}: the reconcile read must list it: {reports:?}")
            });
        println!(
            "FINDING bybit spot {kind} side {side}: the reconcile read types it {} ({})",
            report.order_type,
            report.status.as_str()
        );
        assert!(report.status.is_live(), "{order_client_id}: {report:?}");
        assert_eq!(report.side, side, "{order_client_id}: {report:?}");
        assert_eq!(report.symbol, PAIR, "{order_client_id}: {report:?}");

        // --- 4. a limited order's native amend moves the limit, not the trigger -----------------
        if let Some(limit) = limit {
            let moved = on_grid(limit + f64::from(side) * 10.0 * tick, tick);
            let amended = rest.modify_order(&order_client_id, None, Some(moved));
            println!(
                "FINDING bybit spot {kind} side {side}: amend price {limit} -> {moved}: {amended:?}"
            );
            assert!(
                matches!(amended.as_slice(), [Event::OrderModified(_)]),
                "{order_client_id}: the amend of the limit is refused: {amended:?}"
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            let row = loop {
                let row = venue_row(&rest, &order_client_id);
                let price = text(&row, "price").parse::<f64>().unwrap_or(f64::NAN);
                if (price - moved).abs() < tick / 2.0 || Instant::now() > deadline {
                    break row;
                }
                std::thread::sleep(Duration::from_millis(500));
            };
            let number = |key: &str| text(&row, key).parse::<f64>().unwrap_or(f64::NAN);
            assert!(
                (number("price") - moved).abs() < tick / 2.0,
                "{order_client_id}: the limit moved: {row}"
            );
            assert!(
                (number("triggerPrice") - trigger).abs() < tick / 2.0,
                "{order_client_id}: the trigger did not: {row}"
            );
        }

        // --- 4b. the trigger amend moves the trigger 10 ticks further from the market -----------
        let away = if direction == 1 { 10.0 } else { -10.0 };
        let moved = on_grid(trigger + away * tick, tick);
        let price_before = text(&venue_row(&rest, &order_client_id), "price");
        let amended = VenueRest::modify_trigger(&rest, &request, None, None, moved);
        println!(
            "FINDING bybit spot {kind} side {side}: amend trigger {trigger} -> {moved}: {amended:?}"
        );
        assert!(
            matches!(amended.as_slice(), [Event::OrderModified(m)] if m.new_trigger_price == Some(moved)),
            "{order_client_id}: the trigger amend is refused: {amended:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let row = loop {
            let row = venue_row(&rest, &order_client_id);
            let at = text(&row, "triggerPrice").parse::<f64>().unwrap_or(f64::NAN);
            if (at - moved).abs() < tick / 2.0 || Instant::now() > deadline {
                break row;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        println!(
            "FINDING bybit spot {kind} side {side}: after the trigger amend triggerPrice={} price={} \
             triggerDirection={} orderStatus={}",
            text(&row, "triggerPrice"),
            text(&row, "price"),
            text(&row, "triggerDirection"),
            text(&row, "orderStatus")
        );
        let at = text(&row, "triggerPrice").parse::<f64>().unwrap_or(f64::NAN);
        assert!((at - moved).abs() < tick / 2.0, "{order_client_id}: the trigger moved: {row}");
        assert_eq!(
            text(&row, "price"),
            price_before,
            "{order_client_id}: the limit did not: {row}"
        );

        // --- 5. the cancel ends it --------------------------------------------------------------
        rest.cancel_order(&order_client_id).expect("the cancel");
        let deadline = Instant::now() + Duration::from_secs(10);
        let row = loop {
            let row = venue_row(&rest, &order_client_id);
            let status = text(&row, "orderStatus");
            if matches!(status.as_str(), "Deactivated" | "Cancelled") || Instant::now() > deadline {
                break row;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        println!(
            "FINDING bybit spot {kind} side {side}: after the cancel orderStatus={:?} cancelType={:?}",
            text(&row, "orderStatus"),
            text(&row, "cancelType")
        );
        assert!(
            matches!(text(&row, "orderStatus").as_str(), "Deactivated" | "Cancelled"),
            "{order_client_id}: the cancelled spot {kind} must end: {row}"
        );
        drop(cleanup);
        proved += 1;
        tracing::info!(target: "vike_bybit", order_client_id, venue_order_id = %venue_id, "placed -> listed -> cancelled");
    }
    assert!(proved >= 3, "every BUY must be proved: {proved} of 5");
    tracing::info!(target: "vike_bybit", proved, "spot conditional smoke green");
}
