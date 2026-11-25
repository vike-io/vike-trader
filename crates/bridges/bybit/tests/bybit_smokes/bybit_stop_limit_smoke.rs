//! **The bybit STOP-LIMIT demo smoke**: what `crates/bridges/bybit/src/execution/rest/mod.rs`'s `build_order_params` sends
//! for a `stop_limit` (a conditional `Limit`: `price`, `triggerPrice`, `triggerDirection` from the side,
//! `triggerBy`) is what the Bybit DEMO venue rests, lists back, amends and cancels.
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture --test-threads=1 bybit_stop_limit_smoke::
//!
//! ORDER-PLACING (linear perp `BTCUSDT.P`, the venue's `BTCUSDT`, the DEMO host only, the account `demo_label`
//! names): rests two
//! conditional limits that cannot fire in the seconds they rest, a SELL whose trigger is 3% under the last
//! price (fires on a FALL, `triggerDirection 2`) with its limit 20 ticks under the trigger, and a BUY 3% over
//! (fires on a RISE, `1`) with its limit 20 ticks over. Neither is reduce-only, sized just over
//! `min_notional`. Nothing touches a position.
//!
//! What it proves, per side:
//!   1. `submit_order` through the real builder is ACCEPTED with the venue's order id;
//!   2. the venue's own row (`/v5/order/realtime` by `orderLinkId`) echoes `orderType Limit`, the limit as
//!      `price`, the trigger as `triggerPrice`, the direction, `triggerBy LastPrice` and `Untriggered`; the
//!      row is printed as a `CAPTURED` line (the fixture of `tests/offline/claim_prereqs.rs`), and its
//!      `stopOrderType` word is a `FINDING`;
//!   3. the real `ReconcileClientBybit` lists it back as a LIVE `StopLimit` carrying the order client id on the side placed
//!      (the claim `lists_trigger_orders` rests on this, decision 0192);
//!   4. the native amend (`VenueRestBybit::modify_order`, the seam's one price) moves the LIMIT and leaves the
//!      trigger where it was (`FINDING`): the fact `crates/vike-venues/src/venue_amend_mode.rs`'s
//!      `NO_NATIVE_KIND` is read against;
//!   5. the cancel ends it, and the row the venue keeps for it is terminal (`FINDING`: its word).
//!
//! Only `stop limit smoke green` is a proof; a `SKIP:` line is not.
//!
//! CLEANUP: a `CancelOnDrop` guard per order client id cancels through the placing client on every exit path
//! (idempotent: the venue's already-gone codes are swallowed). A run killed from outside leaves the orders
//! resting: cancel the order client ids the log names (`vtsl…`) by hand on the demo account.

use std::time::{Duration, Instant};

use crate::demo_label::demo_label;
use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::CancelOnDrop;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_bybit::ReconcileClientBybit;
use vike_bybit::execution::rest::{
    PATH_INSTRUMENTS, PATH_ORDER_HISTORY, PATH_ORDER_REALTIME, VenueRestBybit,
    parse_bybit_perp_instruments, seed_endpoints,
};
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_execution::recon::ReconcileClient;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

/// The linear perpetual's USER name (decision 0290): what an order and the reconcile client name, read as its kept
/// row's [`WIRE`] in `linear` (the list [`load_demo_credentials`] holds).
pub(crate) const SYMBOL: &str = "BTCUSDT.P";
/// The venue's own name of [`SYMBOL`]: what the grid read and the client sign.
const WIRE: &str = "BTCUSDT";

pub(crate) type PerpRest = VenueRestBybit<RestTransportJsonUreq>;

/// The demo account's credentials (`None`, said, when absent), with this process's bybit kept list held first
/// (`vike_bybit::history::kept_list_for_tests`): every user name the smokes of this module family name is read off it.
pub(crate) fn load_demo_credentials() -> Option<Credentials> {
    vike_bybit::history::kept_list_for_tests();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &demo_label(&directory), AccountTier::Demo);
    let creds = api_credentials(&account, "bybit");
    if creds.is_none() {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
    }
    creds
}

pub(crate) fn build_rest(creds: &Credentials) -> PerpRest {
    let demo = seed_endpoints(AccountTier::Demo).rest; // the demo host, from the release seed's rows
    let transport =
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            &demo,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!(WIRE))],
            &BybitV5Signer::new(creds, now_ms),
        )
        .expect("instruments-info");
    let instrument = parse_bybit_perp_instruments(&info)[WIRE].clone();
    VenueRestBybit {
        category: vike_bybit::history::Category::Linear,
        signer: BybitV5Signer::new(creds, now_ms),
        transport: rest_transport()
            .with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        base_url: demo,
        mainnet: false,
        symbol: WIRE.to_string(),
        properties: instrument.properties,
        leverage: 2.0,
    }
}

/// The venue's row for `order_client_id` in the client's own book: the open-orders view first, then the history (a closed
/// order leaves the first), polled for a few seconds while the venue catches up.
pub(crate) fn venue_row(rest: &PerpRest, order_client_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for path in [PATH_ORDER_REALTIME, PATH_ORDER_HISTORY] {
            let result = rest
                .call_public(
                    path,
                    &[
                        ("category", json!(rest.category.wire())),
                        ("symbol", json!(rest.symbol)),
                        ("orderLinkId", json!(order_client_id)),
                    ],
                )
                .expect("order read");
            if let Some(row) = result["list"].as_array().and_then(|l| l.first()) {
                return row.clone();
            }
        }
        assert!(Instant::now() < deadline, "{order_client_id}: the venue never listed it");
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub(crate) fn text(row: &Value, key: &str) -> String {
    row[key].as_str().map_or_else(|| row[key].to_string(), str::to_string)
}

pub(crate) fn on_grid(px: f64, tick: f64) -> f64 {
    (px / tick).round() * tick
}

#[test]
#[ignore = "network + demo creds — places, amends and cancels two real (non-firing) demo stop-limits — run manually (see module doc)"]
fn bybit_stop_limit_rests_lists_amends_and_cancels() {
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

    // (side, trigger, limit, the direction the side fires in)
    let sell_trigger = on_grid(last * 0.97, tick);
    let buy_trigger = on_grid(last * 1.03, tick);
    let cases = [
        (-1_i32, sell_trigger, on_grid(sell_trigger - 20.0 * tick, tick), 2_i64),
        (1_i32, buy_trigger, on_grid(buy_trigger + 20.0 * tick, tick), 1_i64),
    ];
    for (side, trigger, limit, direction) in cases {
        let qty = f64::max(properties.min_qty, (properties.min_notional * 1.4) / limit);
        let qty = (qty / properties.step_size).ceil() * properties.step_size;
        let order_client_id =
            format!("vtsl{}{}", if side > 0 { "b" } else { "s" }, now_ms() % 100_000_000);
        let request: OrderRequest = serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "bybit", "symbol": SYMBOL, "side": side, "qty": qty,
            "order_type": "stop_limit", "price": limit, "trigger_price": trigger, "ts": now_ms()
        }))
        .unwrap();
        assert_eq!(request.order_type, OrderType::StopLimit);
        tracing::info!(target: "vike_bybit", order_client_id, side, last, trigger, limit, qty, "placing a stop-limit");

        // --- 1. placed through the real builder ------------------------------------------------
        let events = VenueRest::submit_order(&rest, &request);
        // From here on the order may rest: the guard cancels it on every exit path.
        let cleanup = CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };
        let Some(Event::OrderAccepted(accepted)) = events.last() else {
            panic!("{order_client_id}: the demo must ACCEPT the stop-limit: {events:?}")
        };
        let venue_id = accepted.venue_order_id.clone().expect("the accept names the venue id");
        tracing::info!(target: "vike_bybit", order_client_id, venue_order_id = %venue_id, "ACCEPTED");

        // --- 2. the venue's own row -------------------------------------------------------------
        let row = venue_row(&rest, &order_client_id);
        println!("CAPTURED bybit stop-limit side {side}: {row}");
        println!(
            "FINDING bybit stop-limit side {side}: stopOrderType={:?} timeInForce={:?} orderStatus={:?}",
            text(&row, "stopOrderType"),
            text(&row, "timeInForce"),
            text(&row, "orderStatus")
        );
        let number = |key: &str| text(&row, key).parse::<f64>().unwrap_or(f64::NAN);
        assert_eq!(
            text(&row, "orderType"),
            "Limit",
            "{order_client_id}: it fires as a limit: {row}"
        );
        assert_eq!(text(&row, "orderStatus"), "Untriggered", "{order_client_id}: {row}");
        assert!(
            (number("price") - limit).abs() < tick / 2.0,
            "{order_client_id}: the limit is `price`: {row}"
        );
        assert!(
            (number("triggerPrice") - trigger).abs() < tick / 2.0,
            "{order_client_id}: the trigger is `triggerPrice`: {row}"
        );
        assert_eq!(row["triggerDirection"].as_i64(), Some(direction), "{order_client_id}: {row}");
        assert_eq!(text(&row, "triggerBy"), "LastPrice", "{order_client_id}: {row}");

        // --- 3. the reconcile read lists it back as a live stop-limit ---------------------------
        let reports = recon.fetch_order_status_reports(0).expect("order/realtime");
        let report = reports
            .iter()
            .find(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str()))
            .unwrap_or_else(|| {
                panic!("{order_client_id}: the reconcile read must list it: {reports:?}")
            });
        assert_eq!(report.order_type, OrderType::StopLimit, "{order_client_id}: {report:?}");
        assert!(report.status.is_live(), "{order_client_id}: {report:?}");
        assert_eq!(report.side, side, "{order_client_id}: {report:?}");
        tracing::info!(target: "vike_bybit", order_client_id, status = report.status.as_str(), "listed back as a stop_limit");

        // --- 4. the native amend moves the limit, not the trigger -------------------------------
        let moved = on_grid(limit - f64::from(side) * 10.0 * tick, tick);
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
            "FINDING bybit stop-limit side {side}: amend price {limit} -> {moved}: venue price={} triggerPrice={} orderStatus={}",
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
            "FINDING bybit stop-limit side {side}: after the cancel orderStatus={:?} cancelType={:?}",
            text(&row, "orderStatus"),
            text(&row, "cancelType")
        );
        assert!(
            matches!(text(&row, "orderStatus").as_str(), "Deactivated" | "Cancelled"),
            "{order_client_id}: the cancelled stop-limit must end: {row}"
        );
        drop(cleanup);
        tracing::info!(target: "vike_bybit", order_client_id, venue_order_id = %venue_id, "placed -> listed -> amended -> cancelled");
    }
    tracing::info!(target: "vike_bybit", "stop limit smoke green");
}
