//! LIVE DEMO drills for the binance TAKE-PROFIT kinds and the spot ICEBERG (network + demo creds; run by
//! hand, never in a loop; NOT in `.github/workflows/live-smokes.yml`). Written in wave 2 of the order
//! kinds with no exchange contact; **both ran green on the demo account `#3 binance/demo` (2026-11-08)**.
//!
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture --test-threads=1 take_profit_smoke::perp_take_profit
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture --test-threads=1 take_profit_smoke::spot_take_profit
//!
//! The bridge builds them from the venue's pages alone (`crates/bridges/binance/src/execution/rest/algo.rs`'s
//! `build_perp_algo_params`, `crates/bridges/binance/src/execution/rest/spot.rs`'s `VenueRestBinanceSpot::build_order_params`),
//! and its cancel and confirm of a TRIGGERED take-profit (`crates/bridges/binance/src/execution/rest/perp.rs`'s
//! `VenueRestBinancePerp::cancel_member` and `lookup_by_order_client_id`) rest on the stop-limit's measurement that the
//! child keeps the `clientAlgoId`. Each `FINDING TP/...` line carries what the venue answered.
//!
//! * [`perp_take_profit_on_the_demo_algo_service`] (ETHUSDT, the perp algo drills' fence and `LeaveFlat`
//!   guard): a far `TAKE_PROFIT_MARKET` and a far `TAKE_PROFIT` through our `submit_order`, read back
//!   (the `openAlgoOrders` row, `openOrders` not listing them, our confirm, the report path's kind),
//!   refused a modify, cancelled through our code; then a near `TAKE_PROFIT` whose limit rests 3% away
//!   which, if the market crosses its trigger inside the window, must leave its child on `/fapi/v1/order`
//!   under the SAME client id, be confirmed live off that child, cancelled through our code with the
//!   child gone, and confirmed ended; then a minimal long closed by a reduce-only `TAKE_PROFIT_MARKET` one
//!   tick above the market, read back reduce-only and re-placed at the new price until it fires (bounded
//!   tries: a single placement never fired on a falling market), its child FILLED reduce-only under the
//!   same id, our confirm accepted.
//! * [`spot_take_profit_on_the_demo_host`]: far BUY `TAKE_PROFIT`, `TAKE_PROFIT_LIMIT` with an
//!   `icebergQty` and an iceberg `LIMIT` through our `VenueRestBinanceSpot`, each sized so its VISIBLE slice
//!   clears the minimum notional (the venue judges the slice: `Filter failure: NOTIONAL` otherwise),
//!   read back (the row's type, `stopPrice`, `price`, `timeInForce`, `icebergQty`, our confirm, the
//!   report path's kind), the
//!   take-profit refused a modify, the iceberg `LIMIT` modified by `cancelReplace` (does the replacement
//!   keep its `icebergQty`?), all cancelled. `SpotClean` cancels every spot order on the symbol.
//!
//! **DEMO ONLY**: the demo hosts alone (`Venue`, `demo_rest`, `demo_spot`: the seed's demo rows,
//! `crate::demo_credentials::demo_hosts`); credentials come from the smoke
//! store and are never printed.

use std::time::{Duration, Instant};

use serde_json::Value;
use vike_binance::execution::rest::spot::{self, VenueRestBinanceSpot, parse_symbol_properties};
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, now_ms};

use crate::demo_credentials::demo_hosts;
use crate::perp_algo_smoke::{
    BUDGET, SpotClean, accepted, demo_rest, demo_spot, drill_credentials, finding, number, sent,
};
use crate::perp_algo_venue::{LeaveFlat, SYMBOL, Venue, order};

/// The first of `order_client_ids` whose `GET /fapi/v1/algoOrder` reads TRIGGERED or FINISHED, polled every half
/// second for `within`, with that row.
fn first_triggered(
    venue: &Venue,
    order_client_ids: &[&str],
    within: Duration,
) -> Option<(String, Value)> {
    let until = Instant::now() + within;
    while Instant::now() < until {
        for order_client_id in order_client_ids {
            if let Ok(row) = venue.call(
                "GET",
                "/fapi/v1/algoOrder",
                &[("clientAlgoId", order_client_id.to_string())],
            ) && matches!(row["algoStatus"].as_str(), Some("TRIGGERED" | "FINISHED"))
            {
                return Some((order_client_id.to_string(), row));
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    None
}

/// An id field as text, whether the venue spelled it as a number or a string.
fn id(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

/// The regular endpoint's order under `order_client_id` (a triggered take-profit's child), or the venue's words.
fn child_of(venue: &Venue, order_client_id: &str) -> Result<Value, String> {
    venue
        .call(
            "GET",
            "/fapi/v1/order",
            &[venue.symbol(), ("origClientOrderId", order_client_id.into())],
        )
        .map_err(|e| format!("{} {}", e.code, e.msg))
}

/// The perp take-profits on the Algo Service, through our code (module doc).
#[test]
#[ignore = "network + DEMO creds: places and cancels real DEMO take-profits and closes a minimal long - run by hand (see module doc)"]
fn perp_take_profit_on_the_demo_algo_service() {
    vike_log::test_init();
    let started = Instant::now();
    let Some(creds) = drill_credentials() else { return };
    let venue = Venue::new(&creds);
    let regular = |v: &Venue| v.open_order_client_ids("/fapi/v1/openOrders", "clientOrderId");
    let algo = |v: &Venue| v.open_order_client_ids("/fapi/v1/openAlgoOrders", "clientAlgoId");
    if venue.position(SYMBOL) != 0.0 || !regular(&venue).is_empty() || !algo(&venue).is_empty() {
        println!("SKIP: {SYMBOL} is not flat/clean on the demo account");
        return;
    }
    let flat_guard = LeaveFlat { creds: creds.clone() };
    let (rest, properties) = demo_rest(&venue, &creds);
    let tick = properties.tick_size;
    let on_grid = |px: f64| (px / tick).round() * tick;
    let last = venue.last_price(SYMBOL);
    let want = (properties.min_notional.max(5.0) * 1.5 / last).max(properties.min_qty);
    let qty = (want / properties.step_size).ceil() * properties.step_size;
    let tag = now_ms() % 100_000_000;
    let take_profit =
        |order_client_id: &str, side: i32, trigger: f64, limit: Option<f64>| OrderRequest {
            price: limit,
            reduce_only: false,
            ..order(order_client_id, side, qty, OrderType::TakeProfit, Some(trigger))
        };

    // 1. a far SELL take-profit of each flavour (it fires ABOVE the market), through our submit_order
    let mut far = Vec::new();
    for (order_client_id, limited) in
        [(format!("vttpM{tag}"), false), (format!("vttpL{tag}"), true)]
    {
        let mut placed = None;
        for pct in [1.10, 1.04] {
            let trigger = on_grid(last * pct);
            let req = take_profit(
                &order_client_id,
                -1,
                trigger,
                limited.then_some(trigger + 10.0 * tick),
            );
            let event = rest.submit_order(&req);
            finding("TP/place", format!("{order_client_id} trigger {pct}x of {last}: {event:?}"));
            if accepted(&event).is_some() {
                placed = Some(req);
                break;
            }
        }
        far.push(
            placed
                .unwrap_or_else(|| panic!("a far take-profit {order_client_id} must be accepted")),
        );
    }

    // 2. read back: the venue's row, `openOrders`, our confirm, the report path
    let rows = venue.call("GET", "/fapi/v1/openAlgoOrders", &[venue.symbol()]).expect("open algo");
    let reports = vike_binance::recon_client(&creds, &format!("{SYMBOL}.P"), demo_hosts())
        .expect("recon client")
        .fetch_order_status_reports(0)
        .expect("report path");
    for req in &far {
        let order_client_id = req.order_client_id.as_str();
        let row = rows
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["clientAlgoId"].as_str() == Some(order_client_id))
            .cloned()
            .unwrap_or_else(|| panic!("openAlgoOrders lists {order_client_id}: {rows}"));
        finding("TP/row", format!("{order_client_id}: {row}"));
        let wire = rest.build_algo_params(req);
        let word = if req.price.is_some() { "TAKE_PROFIT" } else { "TAKE_PROFIT_MARKET" };
        assert_eq!(row["orderType"], word, "{row}");
        assert_eq!(number(&row["triggerPrice"]), sent(&wire, "triggerPrice"), "{row}");
        if req.price.is_some() {
            assert_eq!(
                number(&row["price"]),
                sent(&wire, "price"),
                "the limit the venue holds: {row}"
            );
            assert_eq!(row["timeInForce"], "GTC", "{row}");
        }
        assert!(
            !regular(&venue).iter().any(|c| c == order_client_id),
            "openOrders does not list {order_client_id}"
        );
        let confirmed = rest.confirm_order(order_client_id, now_ms());
        assert!(accepted(&confirmed).is_some(), "{order_client_id} confirms live: {confirmed:?}");
        let report = reports
            .iter()
            .find(|r| r.order_client_id.as_deref() == Some(order_client_id))
            .unwrap_or_else(|| panic!("the report path lists {order_client_id}"));
        assert_eq!(report.order_type, OrderType::TakeProfit, "{report:?}");
        // 3. an untriggered algo order is not modifiable: the advisory, nothing sent
        let modified = rest.modify_order(req, None, req.price.map(|p| p + tick));
        assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
        // 4. cancel through our code
        assert_eq!(
            rest.cancel_order(order_client_id).ok(),
            Some(OrderCancelAck::Canceled),
            "our cancel of {order_client_id}"
        );
    }
    assert!(algo(&venue).is_empty(), "the far take-profits are gone");

    // 5. a TRIGGERED take-profit whose limit rests 3% away: a SELL fires above the market and rests
    // above it, a BUY the mirror; one each side, whichever fires first
    let up = format!("vttpU{tag}");
    let down = format!("vttpD{tag}");
    let place = |order_client_id: &str, side: i32| -> bool {
        for ticks in [2.0, 6.0, 20.0] {
            let now = venue.last_price(SYMBOL);
            let base = (now / tick).floor() * tick;
            let (trigger, limit) = if side < 0 {
                (base + ticks * tick, on_grid(now * 1.03))
            } else {
                (base - ticks * tick, on_grid(now * 0.97))
            };
            let event =
                rest.submit_order(&take_profit(order_client_id, side, trigger, Some(limit)));
            finding(
                "TP/near",
                format!(
                    "{order_client_id} trigger {trigger} limit {limit} (last {now}): {event:?}"
                ),
            );
            if accepted(&event).is_some() {
                return true;
            }
        }
        false
    };
    let placed_any = place(&up, -1) | place(&down, 1);
    let window = Duration::from_secs(90).min(BUDGET.saturating_sub(started.elapsed()));
    let fired = placed_any
        .then(|| first_triggered(&venue, &[up.as_str(), down.as_str()], window))
        .flatten();
    match fired {
        Some((t, row)) => {
            let other = if t == up { &down } else { &up };
            std::thread::sleep(Duration::from_secs(2));
            finding(
                "TP/algo-get",
                format!(
                    "{t}: algoStatus {} actualOrderId {}",
                    row["algoStatus"], row["actualOrderId"]
                ),
            );
            // THE assumption the cancel and the confirm rest on: the child is under the same id
            let child = child_of(&venue, &t);
            finding("TP/child", format!("{t}: GET /fapi/v1/order by the clientAlgoId: {child:?}"));
            let child = child.expect("the triggered take-profit's child is under its clientAlgoId");
            assert_eq!(child["clientOrderId"].as_str(), Some(t.as_str()), "{child}");
            assert_eq!(id(&child["orderId"]), id(&row["actualOrderId"]), "{child} {row}");
            finding(
                "TP/lists",
                format!(
                    "openOrders lists the child {}; openAlgoOrders lists the take-profit {}",
                    regular(&venue).contains(&t),
                    algo(&venue).contains(&t)
                ),
            );
            let live = rest.confirm_order(&t, now_ms());
            finding("TP/confirm", format!("our confirm_order while the child rests: {live:?}"));
            assert!(matches!(live.as_slice(), [Event::OrderAccepted(_)]), "{live:?}");
            let ours = rest.cancel_order(&t);
            std::thread::sleep(Duration::from_secs(2));
            let still = regular(&venue).contains(&t);
            finding(
                "TP/cancel",
                format!("our cancel_order: {ours:?}; the child still rests: {still}"),
            );
            assert!(!still, "our cancel of a triggered take-profit must cancel its resting child");
            rest.algo_order_client_ids.insert(&t);
            let ended = rest.confirm_order(&t, now_ms());
            finding(
                "TP/confirm-ended",
                format!("our confirm_order once the child ended: {ended:?}"),
            );
            assert!(
                ended.iter().any(|e| matches!(e, Event::OrderCanceled(_))),
                "the ended child is the take-profit's state: {ended:?}"
            );
            assert!(rest.cancel_order(other).is_ok(), "the untriggered one cancels");
        }
        None => {
            for c in [&up, &down] {
                assert!(rest.cancel_order(c).is_ok(), "cancel {c}");
            }
            finding("TP/trigger", "NOT MEASURED: no near take-profit triggered inside the window");
        }
    }

    // 6. a minimal long closed by a reduce-only TAKE_PROFIT_MARKET one tick above the market, re-placed at
    // the new price until it fires: one placement a few ticks up never fired while the market fell
    let open = rest.submit_order(&order(&format!("vttpO{tag}"), 1, qty, OrderType::Market, None));
    assert!(accepted(&open).is_some(), "the entry must be accepted: {open:?}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while venue.position(SYMBOL) < qty * 0.99 {
        assert!(Instant::now() < deadline, "the entry never showed in positionRisk");
        std::thread::sleep(Duration::from_millis(500));
    }
    let algo_row = |order_client_id: &str| {
        venue
            .call("GET", "/fapi/v1/algoOrder", &[("clientAlgoId", order_client_id.to_string())])
            .map_err(|e| format!("{} {}", e.code, e.msg))
    };
    let mut fired = None;
    for attempt in 0..12 {
        if BUDGET.saturating_sub(started.elapsed()) < Duration::from_secs(25) {
            break;
        }
        let exit = format!("vttpX{tag}{attempt}");
        let now = venue.last_price(SYMBOL);
        let trigger = ((now / tick).ceil() + 1.0) * tick;
        let event = rest.submit_order(&order(&exit, -1, qty, OrderType::TakeProfit, Some(trigger)));
        finding(
            "TP/exit",
            format!(
                "{exit} reduce-only TAKE_PROFIT_MARKET trigger {trigger} (last {now}): {event:?}"
            ),
        );
        if accepted(&event).is_none() {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let row = algo_row(&exit);
        finding("TP/exit-row", format!("{exit}: {row:?}"));
        let row = row.expect("the exit's algoOrder row");
        assert_eq!(row["orderType"], "TAKE_PROFIT_MARKET", "{row}");
        assert_eq!(row["reduceOnly"], true, "the exit rests reduce-only: {row}");
        if let Some(hit) = first_triggered(&venue, &[exit.as_str()], Duration::from_secs(20)) {
            fired = Some(hit);
            break;
        }
        let ours = rest.cancel_order(&exit);
        // a trigger can land between the wait and the cancel: then it fired, and nothing is left to cancel
        if let Some(hit) = first_triggered(&venue, &[exit.as_str()], Duration::from_secs(1)) {
            fired = Some(hit);
            break;
        }
        assert!(ours.is_ok(), "cancel {exit}: {ours:?}");
        finding("TP/exit", format!("{exit} did not fire in 20 s; cancelled, re-placing"));
    }
    let (exit, row) = fired.expect("no reduce-only take-profit fired in the bounded tries");
    std::thread::sleep(Duration::from_secs(2));
    let child = child_of(&venue, &exit);
    finding(
        "TP/market-child",
        format!(
            "{exit}: algoStatus {} actualOrderId {}; the child: {child:?}",
            row["algoStatus"], row["actualOrderId"]
        ),
    );
    let child = child.expect("the market take-profit's child is under its clientAlgoId");
    assert_eq!(child["clientOrderId"].as_str(), Some(exit.as_str()), "{child}");
    assert_eq!(child["status"], "FILLED", "the market child fills: {child}");
    assert_eq!(child["reduceOnly"], true, "the child closes reduce-only: {child}");
    let confirmed = rest.confirm_order(&exit, now_ms());
    finding("TP/market-confirm", format!("our confirm_order after the fill: {confirmed:?}"));
    assert!(accepted(&confirmed).is_some(), "{confirmed:?}");
    let flat_by = Instant::now() + Duration::from_secs(15);
    while venue.position(SYMBOL) != 0.0 && Instant::now() < flat_by {
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(venue.position(SYMBOL), 0.0, "a triggered reduce-only take-profit flattens");

    // 7. flat and clean, whatever the path
    drop(flat_guard);
    let v = Venue::new(&creds);
    assert_eq!(v.position(SYMBOL), 0.0, "the drill leaves the symbol flat");
    assert!(algo(&v).is_empty() && regular(&v).is_empty(), "nothing rests");
    println!("TAKE-PROFIT DRILL done in {:?}: flat, nothing resting", started.elapsed());
}

/// The spot take-profits and icebergs through our `VenueRestBinanceSpot` (module doc). Every BUY rests or fires
/// 10% under the market, so nothing can fill.
#[test]
#[ignore = "network + DEMO creds: places and cancels real DEMO spot take-profits and icebergs, none can fill - run by hand (see module doc)"]
fn spot_take_profit_on_the_demo_host() {
    vike_log::test_init();
    let Some(creds) = drill_credentials() else { return };
    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate());
    let info = transport
        .public(demo_spot(), spot::PATH_EXCHANGE_INFO, &[("symbol", SYMBOL.into())])
        .expect("exchangeInfo");
    let properties_value = parse_symbol_properties(&info)[SYMBOL];
    let rest = VenueRestBinanceSpot {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: demo_spot().to_string(),
        symbol: SYMBOL.to_string(),
        properties: properties_value,
        base_asset: "ETH".to_string(),
    };
    rest.signer.set_offset_ms(rest.server_time_offset(now_ms()).expect("server time"));
    let open = |r: &VenueRestBinanceSpot<BinanceHmacSigner, UreqTransport>| -> Vec<Value> {
        let params = [("symbol", SYMBOL.to_string())];
        let rows =
            r.transport.signed(demo_spot(), spot::PATH_OPEN_ORDERS, "GET", &params, &r.signer);
        rows.expect("spot openOrders").as_array().cloned().unwrap_or_default()
    };
    if !open(&rest).is_empty() {
        println!("SKIP: spot {SYMBOL} has orders resting on the demo account");
        return;
    }
    let _clean = SpotClean(creds.clone());
    let ticker = rest
        .transport
        .public(demo_spot(), spot::PATH_TICKER, &[("symbol", SYMBOL.into())])
        .expect("ticker");
    let last: f64 = ticker["price"].as_str().and_then(|p| p.parse().ok()).expect("a price");
    let on_grid = |px: f64| (px / properties_value.tick_size).round() * properties_value.tick_size;
    let step = properties_value.step_size;
    // the venue judges an iceberg's VISIBLE slice against the minimum notional, so half the size must clear
    // it at the lowest price the drill rests (0.89x)
    let qty = ((properties_value.min_notional.max(5.0) * 2.5 / (last * 0.89)) / step).ceil() * step;
    let shown = ((qty / 2.0) / step).ceil() * step;
    let tag = now_ms() % 100_000_000;
    let take_profit = OrderRequest {
        order_client_id: format!("vtstpM{tag}"),
        venue: "binance".to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty,
        order_type: OrderType::TakeProfit,
        trigger_price: Some(on_grid(last * 0.90)),
        timestamp: now_ms(),
        ..Default::default()
    };
    let take_profit_limit = OrderRequest {
        order_client_id: format!("vtstpL{tag}"),
        price: Some(on_grid(last * 0.89)),
        display_quantity: Some(shown),
        ..take_profit.clone()
    };
    let iceberg = OrderRequest {
        order_client_id: format!("vtsicL{tag}"),
        order_type: OrderType::Limit,
        trigger_price: None,
        price: Some(on_grid(last * 0.90)),
        display_quantity: Some(shown),
        ..take_profit.clone()
    };
    let placed = [&take_profit, &take_profit_limit, &iceberg];
    for req in placed {
        let event = rest.submit_order(req);
        finding("SPOT-TP/place", format!("{} {}: {event:?}", req.order_type, req.order_client_id));
        assert!(
            accepted(&event).is_some(),
            "the spot {} must be accepted: {event:?}",
            req.order_type
        );
    }
    let rows = open(&rest);
    let reports = vike_binance::recon_client(&creds, SYMBOL, demo_hosts())
        .expect("recon client")
        .fetch_order_status_reports(0)
        .expect("report path");
    for req in placed {
        let order_client_id = req.order_client_id.as_str();
        let row = rows
            .iter()
            .find(|r| r["clientOrderId"].as_str() == Some(order_client_id))
            .unwrap_or_else(|| panic!("spot openOrders lists {order_client_id}: {rows:?}"));
        finding("SPOT-TP/row", format!("{order_client_id}: {row}"));
        let wire = rest.build_order_params(req);
        let word = wire.iter().find(|(k, _)| *k == "type").map(|(_, v)| v.as_str());
        assert_eq!(row["type"].as_str(), word, "{row}");
        if req.trigger_price.is_some() {
            assert_eq!(number(&row["stopPrice"]), sent(&wire, "stopPrice"), "{row}");
        }
        if req.price.is_some() {
            assert_eq!(number(&row["price"]), sent(&wire, "price"), "{row}");
            assert_eq!(row["timeInForce"], "GTC", "{row}");
        }
        if req.display_quantity.is_some() {
            assert_eq!(
                number(&row["icebergQty"]),
                sent(&wire, "icebergQty"),
                "the shown size: {row}"
            );
        }
        let confirmed = rest.confirm_order(order_client_id, now_ms());
        assert!(accepted(&confirmed).is_some(), "{order_client_id} confirms live: {confirmed:?}");
        let report = reports
            .iter()
            .find(|r| r.order_client_id.as_deref() == Some(order_client_id))
            .unwrap_or_else(|| panic!("the report path lists {order_client_id}: {reports:?}"));
        assert_eq!(report.order_type, req.order_type, "{report:?}");
    }
    let modified = rest.modify_order(&take_profit_limit, None, take_profit_limit.price);
    assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
    // the iceberg LIMIT's price modify is a cancelReplace: does the replacement keep its icebergQty?
    let lower = iceberg.price.map(|p| p - properties_value.tick_size);
    let modified = rest.modify_order(&iceberg, None, lower);
    let row = open(&rest)
        .into_iter()
        .find(|r| r["clientOrderId"].as_str() == Some(iceberg.order_client_id.as_str()));
    finding(
        "SPOT-TP/modify",
        format!("the iceberg's cancelReplace: {modified:?}; the row now: {row:?}"),
    );
    assert!(matches!(modified.as_slice(), [Event::OrderModified(_)]), "{modified:?}");
    let row = row.expect("the replaced iceberg rests under its client id");
    let shown_sent = sent(&rest.build_order_params(&iceberg), "icebergQty");
    assert_eq!(
        number(&row["icebergQty"]),
        shown_sent,
        "the replacement keeps its shown size: {row}"
    );
    for req in placed {
        let ack = rest.cancel_order(&req.order_client_id).ok();
        assert_eq!(ack, Some(OrderCancelAck::Canceled), "our cancel of {}", req.order_client_id);
    }
    assert!(open(&rest).is_empty(), "nothing rests on spot {SYMBOL}");
    println!("SPOT TAKE-PROFIT DRILL done: placed, read back, modified and cancelled");
}
