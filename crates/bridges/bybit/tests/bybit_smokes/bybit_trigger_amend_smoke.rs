//! **The bybit TRIGGER-AMEND demo smoke**: the native amend's `triggerPrice`
//! (`crates/bridges/bybit/src/execution/rest/mod.rs`'s `VenueRestBybit::amend`) moves a resting conditional's trigger in place
//! on the Bybit DEMO venue, for each kind the seam hands it. PASSED on the demo 2026-11-08 (main `d1624c326e`,
//! store account `#4 bybit/demo`): `crates/vike-venues/src/venue_amend_mode.rs`'s rows for bybit are read
//! against it.
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture --test-threads=1 bybit_trigger_amend_smoke::
//!
//! ORDER-PLACING (linear perp `BTCUSDT`, the DEMO host only, the account `demo_label` names): rests four SELL
//! conditionals that cannot fire in the seconds they rest, none reduce-only, each sized just over
//! `min_notional`: a stop 3% UNDER the last price and a stop-limit there (limit 20 ticks under), both firing on
//! a fall, and a limit-if-touched and a market-if-touched 3% OVER (firing on a rise; the limit 20 ticks under
//! the trigger). Nothing touches a position.
//!
//! What it proves, per order:
//!   1. it is ACCEPTED through the real builder, and the venue's row is `Untriggered` at the trigger placed;
//!   2. the trigger moves 10 ticks FURTHER from the market: a stop-limit and both take-profits through the
//!      seam's door, `VenueRest::modify_trigger`; a STOP through its own door, `VenueRest::modify_order`, whose
//!      `new_price` is a stop's trigger and goes out as `triggerPrice` (run 2026-11-08 through
//!      `VenueRestBybit::amend`, the same bytes: `crates/bridges/bybit/tests/offline/trigger_amend.rs`);
//!      the answer is ONE `OrderModified` echoing the new trigger (a `FINDING` line, for the
//!      market-if-touched too);
//!   3. the venue's row then carries the new `triggerPrice`, the same `price` (the limit did not move), the
//!      same `orderType` and `triggerDirection`, still `Untriggered` (a `FINDING` line each);
//!   4. the cancel ends it, and the row the venue keeps for it is terminal (`FINDING`: its word).
//!
//! Only `trigger amend smoke green` is a proof; a `SKIP:` line is not.
//!
//! CLEANUP: a `CancelOnDrop` guard per order client id cancels through the placing client on every exit path. A run killed
//! from outside leaves the orders resting: cancel the order client ids the log names (`vtta…`) by hand on the demo account.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::CancelOnDrop;
use vike_model::events::Event;
use vike_model::{OrderRequest, now_ms};

use crate::bybit_stop_limit_smoke::{
    SYMBOL, build_rest, load_demo_credentials, on_grid, text, venue_row,
};

#[test]
#[ignore = "network + demo creds — places, re-triggers and cancels four real (non-firing) demo conditionals — run manually (see module doc)"]
fn bybit_trigger_amend_moves_the_trigger_and_keeps_the_limit() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds);
    let last = rest.last_price().expect("tickers");
    assert!(last > 0.0, "a live last price is required");
    let properties = rest.properties;
    let tick = properties.tick_size;
    let under = on_grid(last * 0.97, tick);
    let over = on_grid(last * 1.03, tick);

    // (kind, trigger, limit, the direction it fires in, the trigger 10 ticks further from the market)
    let cases = [
        ("stop", under, None, 2_i64, on_grid(under - 10.0 * tick, tick)),
        (
            "stop_limit",
            under,
            Some(on_grid(under - 20.0 * tick, tick)),
            2,
            on_grid(under - 10.0 * tick, tick),
        ),
        (
            "take_profit",
            over,
            Some(on_grid(over - 20.0 * tick, tick)),
            1,
            on_grid(over + 10.0 * tick, tick),
        ),
        ("take_profit", over, None, 1, on_grid(over + 10.0 * tick, tick)),
    ];
    for (kind, trigger, limit, direction, moved) in cases {
        let sizing = limit.unwrap_or(trigger);
        let qty = f64::max(properties.min_qty, (properties.min_notional * 1.4) / sizing);
        let qty = (qty / properties.step_size).ceil() * properties.step_size;
        let order_client_id = format!(
            "vtta{}{}",
            match (kind, limit.is_some()) {
                ("stop", _) => "s",
                ("stop_limit", _) => "sl",
                (_, true) => "tl",
                _ => "tm",
            },
            now_ms() % 100_000_000
        );
        let request: OrderRequest = serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "bybit", "symbol": SYMBOL, "side": -1, "qty": qty,
            "order_type": kind, "price": limit, "trigger_price": trigger, "ts": now_ms()
        }))
        .unwrap();
        tracing::info!(target: "vike_bybit", order_client_id, kind, last, trigger, ?limit, moved, qty, "placing a conditional");

        // --- 1. placed ---------------------------------------------------------------------------
        let events = VenueRest::submit_order(&rest, &request);
        let cleanup = CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };
        let Some(Event::OrderAccepted(_)) = events.last() else {
            panic!("{order_client_id}: the demo must ACCEPT the {kind}: {events:?}")
        };
        let row = venue_row(&rest, &order_client_id);
        let number =
            |row: &serde_json::Value, key: &str| text(row, key).parse::<f64>().unwrap_or(f64::NAN);
        assert_eq!(text(&row, "orderStatus"), "Untriggered", "{order_client_id}: {row}");
        assert!(
            (number(&row, "triggerPrice") - trigger).abs() < tick / 2.0,
            "{order_client_id}: {row}"
        );
        let (price_before, type_before) = (text(&row, "price"), text(&row, "orderType"));

        // --- 2. the trigger moves: the seam's door (a stop's one price is its trigger) -------------
        let amended = if kind == "stop" {
            VenueRest::modify_order(&rest, &request, None, Some(moved))
        } else {
            VenueRest::modify_trigger(&rest, &request, None, None, moved)
        };
        println!("FINDING bybit trigger amend {kind}: {trigger} -> {moved}: {amended:?}");
        let [Event::OrderModified(m)] = amended.as_slice() else {
            panic!("{order_client_id}: the trigger amend is refused: {amended:?}")
        };
        assert_eq!(
            m.new_trigger_price,
            Some(moved),
            "{order_client_id}: the echo names the new trigger"
        );

        // --- 3. the venue's row carries it, and nothing else moved --------------------------------
        let deadline = Instant::now() + Duration::from_secs(10);
        let row = loop {
            let row = venue_row(&rest, &order_client_id);
            if (number(&row, "triggerPrice") - moved).abs() < tick / 2.0
                || Instant::now() > deadline
            {
                break row;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        println!(
            "FINDING bybit trigger amend {kind}: venue triggerPrice={} price={} (was {price_before}) orderType={} \
             triggerDirection={} orderStatus={}",
            text(&row, "triggerPrice"),
            text(&row, "price"),
            text(&row, "orderType"),
            text(&row, "triggerDirection"),
            text(&row, "orderStatus")
        );
        assert!(
            (number(&row, "triggerPrice") - moved).abs() < tick / 2.0,
            "{order_client_id}: the trigger moved: {row}"
        );
        assert_eq!(
            text(&row, "price"),
            price_before,
            "{order_client_id}: the limit did not: {row}"
        );
        assert_eq!(text(&row, "orderType"), type_before, "{order_client_id}: {row}");
        assert_eq!(row["triggerDirection"].as_i64(), Some(direction), "{order_client_id}: {row}");
        assert_eq!(text(&row, "orderStatus"), "Untriggered", "{order_client_id}: {row}");

        // --- 4. the cancel ends it ----------------------------------------------------------------
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
            "FINDING bybit trigger amend {kind}: after the cancel orderStatus={:?} cancelType={:?}",
            text(&row, "orderStatus"),
            text(&row, "cancelType")
        );
        assert!(
            matches!(text(&row, "orderStatus").as_str(), "Deactivated" | "Cancelled"),
            "{order_client_id}: the cancelled {kind} must end: {row}"
        );
        drop(cleanup);
        tracing::info!(target: "vike_bybit", order_client_id, "placed -> re-triggered -> cancelled");
    }
    tracing::info!(target: "vike_bybit", "trigger amend smoke green");
}
