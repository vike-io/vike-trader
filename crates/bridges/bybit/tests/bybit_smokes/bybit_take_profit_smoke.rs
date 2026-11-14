//! **The bybit TAKE-PROFIT demo smoke**: what `crates/bridges/bybit/src/execution/rest/mod.rs`'s `build_order_params` sends
//! for a `take_profit` (the stop's conditional firing the OTHER way: a BUY on a fall, `triggerDirection 2`, a
//! SELL on a rise, `1`; a `Limit` at its `price` or a `Market` without one) is what the Bybit DEMO venue rests,
//! lists back, amends and cancels. PASSED on the demo 2026-11-08 (main `d1624c326e`, store account
//! `#4 bybit/demo`): the capability row lists `take_profit` behind it, and its four `CAPTURED` rows are
//! `tests/offline/claim_prereqs.rs`'s `RECORDED_TAKE_PROFIT_ROWS`.
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture --test-threads=1 bybit_take_profit_smoke::
//!
//! ORDER-PLACING (linear perp `BTCUSDT`, the DEMO host only, the account `demo_label` names): rests four
//! conditionals that cannot fire in the seconds they rest, none reduce-only, each sized just over
//! `min_notional`: a limit-if-touched BUY whose trigger is 3% UNDER the last price (fires on a FALL) with its
//! limit 20 ticks over the trigger, its SELL twin 3% over (fires on a RISE) with its limit 20 ticks under, and
//! the two market-if-touched (no `price`) at the same triggers. Nothing touches a position.
//!
//! What it proves, per order:
//!   1. `submit_order` through the real builder is ACCEPTED with the venue's order id;
//!   2. the venue's own row (`/v5/order/realtime` by `orderLinkId`) echoes `orderType` (`Limit`/`Market`), the
//!      limit as `price` (the limit-if-touched), the trigger as `triggerPrice`, the take-profit's direction,
//!      `triggerBy LastPrice` and `Untriggered`; the row is printed as a `CAPTURED` line (the recorded row the
//!      take-profit claim needs in `tests/offline/claim_prereqs.rs`) and its `stopOrderType` word is a `FINDING`;
//!   3. the real `ReconcileClientBybit` lists it back LIVE, carrying the order client id on the side placed; the KIND it reads
//!      is a `FINDING`, not an assertion: `reconcile/mod.rs`'s `conditional_order_type` reads a `Stop` in the
//!      profit direction as its `Limit`/`Market` base today, and the row cannot list `take_profit` until that
//!      reading and this echo agree;
//!   4. a limit-if-touched's native amend (`VenueRestBybit::modify_order`, the seam's one price) moves the LIMIT
//!      and leaves the trigger where it was (`FINDING`); a market-if-touched is sent the same amend with a
//!      `price` (its trigger's), and the venue's answer and row are a `FINDING`, asserted nothing;
//!   5. the cancel ends it, and the row the venue keeps for it is terminal (`FINDING`: its word).
//!
//! Only `take profit smoke green` is a proof; a `SKIP:` line is not.
//!
//! CLEANUP: a `CancelOnDrop` guard per order client id cancels through the placing client on every exit path
//! (idempotent: the venue's already-gone codes are swallowed). A run killed from outside leaves the orders
//! resting: cancel the order client ids the log names (`vttp…`) by hand on the demo account.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::CancelOnDrop;
use vike_bybit::ReconcileClientBybit;
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_execution::recon::ReconcileClient;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, now_ms};

use crate::bybit_stop_limit_smoke::{
    SYMBOL, build_rest, load_demo_credentials, on_grid, text, venue_row,
};

#[test]
#[ignore = "network + demo creds — places, amends and cancels four real (non-firing) demo take-profits — run manually (see module doc)"]
fn bybit_take_profit_rests_lists_amends_and_cancels() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds);
    let recon = ReconcileClientBybit::new(
        BybitV5Signer::new(&creds, now_ms),
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        rest.base_url.as_str(), // the demo host `build_rest` signs against
        SYMBOL,
    );
    let last = rest.last_price().expect("tickers");
    assert!(last > 0.0, "a live last price is required");
    let properties = rest.properties;
    let tick = properties.tick_size;

    // (side, trigger, limit, the direction a take-profit of the side fires in)
    let buy_trigger = on_grid(last * 0.97, tick);
    let sell_trigger = on_grid(last * 1.03, tick);
    let cases = [
        (1_i32, buy_trigger, Some(on_grid(buy_trigger + 20.0 * tick, tick)), 2_i64),
        (-1_i32, sell_trigger, Some(on_grid(sell_trigger - 20.0 * tick, tick)), 1_i64),
        (1_i32, buy_trigger, None, 2_i64),
        (-1_i32, sell_trigger, None, 1_i64),
    ];
    for (side, trigger, limit, direction) in cases {
        let sizing = limit.unwrap_or(trigger);
        let qty = f64::max(properties.min_qty, (properties.min_notional * 1.4) / sizing);
        let qty = (qty / properties.step_size).ceil() * properties.step_size;
        let shape = if limit.is_some() { "l" } else { "m" };
        let side_tag = if side > 0 { "b" } else { "s" };
        let order_client_id = format!("vttp{shape}{side_tag}{}", now_ms() % 100_000_000);
        let request: OrderRequest = serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "bybit", "symbol": SYMBOL, "side": side, "qty": qty,
            "order_type": "take_profit", "price": limit, "trigger_price": trigger, "ts": now_ms()
        }))
        .unwrap();
        assert_eq!(request.order_type, OrderType::TakeProfit);
        tracing::info!(target: "vike_bybit", order_client_id, side, last, trigger, ?limit, qty, "placing a take-profit");

        // --- 1. placed through the real builder ------------------------------------------------
        let events = VenueRest::submit_order(&rest, &request);
        // From here on the order may rest: the guard cancels it on every exit path.
        let cleanup = CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };
        let Some(Event::OrderAccepted(accepted)) = events.last() else {
            panic!("{order_client_id}: the demo must ACCEPT the take-profit: {events:?}")
        };
        let venue_id = accepted.venue_order_id.clone().expect("the accept names the venue id");
        tracing::info!(target: "vike_bybit", order_client_id, venue_order_id = %venue_id, "ACCEPTED");

        // --- 2. the venue's own row -------------------------------------------------------------
        let row = venue_row(&rest, &order_client_id);
        println!("CAPTURED bybit take-profit {shape} side {side}: {row}");
        println!(
            "FINDING bybit take-profit {shape} side {side}: stopOrderType={:?} timeInForce={:?} orderStatus={:?}",
            text(&row, "stopOrderType"),
            text(&row, "timeInForce"),
            text(&row, "orderStatus")
        );
        let number = |key: &str| text(&row, key).parse::<f64>().unwrap_or(f64::NAN);
        let fires_as = if limit.is_some() { "Limit" } else { "Market" };
        assert_eq!(text(&row, "orderType"), fires_as, "{order_client_id}: {row}");
        assert_eq!(text(&row, "orderStatus"), "Untriggered", "{order_client_id}: {row}");
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
        assert_eq!(row["triggerDirection"].as_i64(), Some(direction), "{order_client_id}: {row}");
        assert_eq!(text(&row, "triggerBy"), "LastPrice", "{order_client_id}: {row}");

        // --- 3. the reconcile read lists it back live -------------------------------------------
        let reports = recon.fetch_order_status_reports(0).expect("order/realtime");
        let report = reports
            .iter()
            .find(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str()))
            .unwrap_or_else(|| {
                panic!("{order_client_id}: the reconcile read must list it: {reports:?}")
            });
        println!(
            "FINDING bybit take-profit {shape} side {side}: the reconcile read types it {}",
            report.order_type
        );
        assert!(report.status.is_live(), "{order_client_id}: {report:?}");
        assert_eq!(report.side, side, "{order_client_id}: {report:?}");

        // --- 4. a limit-if-touched's native amend moves the limit, not the trigger --------------
        if let Some(limit) = limit {
            let moved = on_grid(limit + f64::from(side) * 10.0 * tick, tick);
            let amended = rest.modify_order(&order_client_id, None, Some(moved));
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
            println!(
                "FINDING bybit take-profit {shape} side {side}: amend price {limit} -> {moved}: venue price={} triggerPrice={} orderStatus={}",
                text(&row, "price"),
                text(&row, "triggerPrice"),
                text(&row, "orderStatus")
            );
            assert!(
                (number("price") - moved).abs() < tick / 2.0,
                "{order_client_id}: the limit moved: {row}"
            );
            assert!(
                (number("triggerPrice") - trigger).abs() < tick / 2.0,
                "{order_client_id}: the trigger did not: {row}"
            );
        } else {
            // A market-if-touched has no limit, yet the seam's modify can name a price (a take-profit's
            // `new_price` is its limit): what the venue answers to a `price` on a conditional `Market` is a
            // FINDING, asserted nothing. The price is the trigger's own, which no side would fill at once.
            let amended = rest.modify_order(&order_client_id, None, Some(trigger));
            let row = venue_row(&rest, &order_client_id);
            println!(
                "FINDING bybit market-if-touched side {side}: amend price -> {trigger}: answer {amended:?}; venue \
                 orderType={} price={} triggerPrice={} orderStatus={}",
                text(&row, "orderType"),
                text(&row, "price"),
                text(&row, "triggerPrice"),
                text(&row, "orderStatus")
            );
        }

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
            "FINDING bybit take-profit {shape} side {side}: after the cancel orderStatus={:?} cancelType={:?}",
            text(&row, "orderStatus"),
            text(&row, "cancelType")
        );
        assert!(
            matches!(text(&row, "orderStatus").as_str(), "Deactivated" | "Cancelled"),
            "{order_client_id}: the cancelled take-profit must end: {row}"
        );
        drop(cleanup);
        tracing::info!(target: "vike_bybit", order_client_id, venue_order_id = %venue_id, "placed -> listed -> cancelled");
    }
    tracing::info!(target: "vike_bybit", "take profit smoke green");
}
