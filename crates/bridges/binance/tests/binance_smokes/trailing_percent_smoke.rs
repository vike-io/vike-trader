//! LIVE DEMO drills for the binance PERCENT trailing stops (network + demo creds; run by hand, never in a
//! loop; NOT in `.github/workflows/live-smokes.yml`). Written in the attributes wave with no exchange
//! contact; **both GREEN on the demo 2026-11-09** (perp first, then spot), so the capability row's
//! `percent_trail_kinds` lists `trailing`.
//!
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture --test-threads=1 trailing_percent_smoke::perp_percent_trailing
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture --test-threads=1 trailing_percent_smoke::spot_percent_trailing
//!
//! The bridge builds them from the venue's pages alone (`crates/bridges/binance/src/execution/rest/algo.rs`'s
//! `build_perp_algo_params`, `crates/bridges/binance/src/execution/rest/spot.rs`'s `VenueRestBinanceSpot::build_order_params`).
//! Each `FINDING TR/...` line carries what the venue answered.
//!
//! * [`perp_percent_trailing_on_the_demo_algo_service`] (ETHUSDT, the perp algo drills' fence and
//!   `LeaveFlat` guard): a far SELL `TRAILING_STOP_MARKET` with no activation (`callbackRate` 10) and one
//!   armed 10% above the market (`activatePrice`, `callbackRate` 5) through our `submit_order`, read back
//!   (the `openAlgoOrders` row's `orderType`, `callbackRate`, `activatePrice`; `openOrders` not listing
//!   them; our confirm; the report path's kind `trailing`), refused a modify, cancelled through our code.
//!   A `callbackRate` of 25 is refused by the bridge with nothing sent (the page says 0.1 to 20); the
//!   venue's answer to 1.25, off the 0.1 grid the page does not state; and a SELL armed BELOW the market
//!   (vike's activation is already met there) must come back refused in the venue's words (the page says
//!   `-2021`). Then a minimal long closed by a reduce-only 0.1% trailing stop with no activation: does it
//!   fire inside the window, is its child under the `clientAlgoId` and FILLED reduce-only, does our
//!   confirm read it.
//! * [`spot_percent_trailing_on_the_demo_host`]: a BUY `STOP_LOSS` (`trailingDelta` 1000, tracking from the
//!   next trade), a BUY `TAKE_PROFIT` armed 10% under the market and a SELL `TAKE_PROFIT` armed 10% above
//!   it (`stopPrice`, `trailingDelta` 500; the SELL needs ETH in the demo wallet) through our
//!   `VenueRestBinanceSpot`, read back (the row's `type` and `side`, `trailingDelta`, `stopPrice`; our confirm;
//!   the report path's kind `trailing`), refused a modify, cancelled. The venue's words for a
//!   `trailingDelta` of 5 (the demo fixture's filter says 10 to 2000; the bridge does not cache the
//!   filter) and for a BUY `TAKE_PROFIT` armed ABOVE the market (vike's activation is already met there).
//!   Each needs a 10% move to arm or fire, so nothing fills. `SpotClean` cancels every spot order on the
//!   symbol.
//!
//! **DEMO ONLY**: the demo hosts alone (`Venue`, `demo_rest`, `demo_spot`: the seed's demo rows); credentials come from the smoke
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

/// A venue field as text, whether it was spelled as a number or a string.
fn text(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

/// The perp percent trailing stops on the Algo Service, through our code (module doc).
#[test]
#[ignore = "network + DEMO creds: places and cancels real DEMO trailing stops and closes a minimal long - run by hand (see module doc)"]
fn perp_percent_trailing_on_the_demo_algo_service() {
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
    let trailing = |order_client_id: &str, percent: f64, activation: Option<f64>| OrderRequest {
        trail_percent: Some(percent),
        reduce_only: false,
        ..order(order_client_id, -1, qty, OrderType::Trailing, activation)
    };

    // 1. a far SELL trailing stop of each flavour, through our submit_order
    let far = [
        trailing(&format!("vttrN{tag}"), 10.0, None),
        trailing(&format!("vttrA{tag}"), 5.0, Some(on_grid(last * 1.10))),
    ];
    for req in &far {
        let event = rest.submit_order(req);
        finding("TR/place", format!("{} (last {last}): {event:?}", req.order_client_id));
        assert!(accepted(&event).is_some(), "a far trailing stop must be accepted: {event:?}");
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
        finding("TR/row", format!("{order_client_id}: {row}"));
        let wire = rest.build_algo_params(req);
        assert_eq!(row["orderType"], "TRAILING_STOP_MARKET", "{row}");
        assert_eq!(number(&row["callbackRate"]), sent(&wire, "callbackRate"), "the percent: {row}");
        if req.trigger_price.is_some() {
            let held = number(&row["activatePrice"]);
            assert_eq!(held, sent(&wire, "activatePrice"), "the activation: {row}");
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
        assert_eq!(report.order_type, OrderType::Trailing, "{report:?}");
        // 3. an untriggered algo order is not modifiable: the advisory, nothing sent
        let modified = rest.modify_order(req, Some(qty * 2.0), None);
        assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
        // 4. cancel through our code
        assert_eq!(
            rest.cancel_order(order_client_id).ok(),
            Some(OrderCancelAck::Canceled),
            "our cancel of {order_client_id}"
        );
    }
    assert!(algo(&venue).is_empty(), "the far trailing stops are gone");

    // 5. a percent outside the page's 0.1-20 range is refused by the bridge, before the wire
    let event = rest.submit_order(&trailing(&format!("vttrR{tag}"), 25.0, None));
    assert!(
        matches!(event.as_slice(), [Event::OrderSubmitted(_), Event::OrderRejected(_)]),
        "refused locally: {event:?}"
    );
    // the step the page does not state: a rate off the 0.1 grid, the venue's answer recorded
    let off_step = trailing(&format!("vttrS{tag}"), 1.25, Some(on_grid(last * 1.10)));
    let event = rest.submit_order(&off_step);
    finding("TR/step", format!("callbackRate 1.25: {event:?}"));
    if accepted(&event).is_some() {
        let row = venue.call(
            "GET",
            "/fapi/v1/algoOrder",
            &[("clientAlgoId", off_step.order_client_id.clone())],
        );
        finding("TR/step", format!("callbackRate 1.25 ACCEPTED, the venue holds: {row:?}"));
        assert!(rest.cancel_order(&off_step.order_client_id).is_ok(), "cancel the 1.25 one");
    }
    // an activation already met at placement (a SELL armed UNDER the market): the venue refuses it
    // (`-2021` on the page) and the operator gets its words
    let met = trailing(&format!("vttrU{tag}"), 5.0, Some(on_grid(last * 0.9)));
    let event = rest.submit_order(&met);
    finding("TR/met", format!("SELL armed under the market: {event:?}"));
    if accepted(&event).is_some() {
        assert!(rest.cancel_order(&met.order_client_id).is_ok(), "cancel the armed-under one");
        panic!("the venue ACCEPTED a SELL armed under the market: {event:?}");
    }
    let words = event.iter().find_map(|e| match e {
        Event::OrderRejected(r) => Some(r.reason.to_string()),
        _ => None,
    });
    assert!(words.is_some_and(|w| !w.is_empty()), "the venue's words reach the reject: {event:?}");

    // 6. a minimal long closed by a reduce-only 0.1% trailing stop with no activation
    let open = rest.submit_order(&order(&format!("vttrO{tag}"), 1, qty, OrderType::Market, None));
    assert!(accepted(&open).is_some(), "the entry must be accepted: {open:?}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while venue.position(SYMBOL) < qty * 0.99 {
        assert!(Instant::now() < deadline, "the entry never showed in positionRisk");
        std::thread::sleep(Duration::from_millis(500));
    }
    let exit = format!("vttrX{tag}");
    let exit_request = OrderRequest { reduce_only: true, ..trailing(&exit, 0.1, None) };
    let event = rest.submit_order(&exit_request);
    finding(
        "TR/exit",
        format!("{exit} reduce-only 0.1% (last {}): {event:?}", venue.last_price(SYMBOL)),
    );
    assert!(
        accepted(&event).is_some(),
        "the reduce-only trailing exit must be accepted: {event:?}"
    );
    let window = Duration::from_secs(120).min(BUDGET.saturating_sub(started.elapsed()));
    let until = Instant::now() + window;
    let mut fired = None;
    while fired.is_none() && Instant::now() < until {
        let row = venue.call("GET", "/fapi/v1/algoOrder", &[("clientAlgoId", exit.clone())]);
        fired =
            row.ok().filter(|r| matches!(r["algoStatus"].as_str(), Some("TRIGGERED" | "FINISHED")));
        std::thread::sleep(Duration::from_millis(500));
    }
    match fired {
        Some(row) => {
            std::thread::sleep(Duration::from_secs(2));
            let child = venue
                .call(
                    "GET",
                    "/fapi/v1/order",
                    &[venue.symbol(), ("origClientOrderId", exit.clone())],
                )
                .map_err(|e| format!("{} {}", e.code, e.msg));
            finding(
                "TR/child",
                format!(
                    "{exit}: algoStatus {} actualOrderId {}; the child: {child:?}",
                    row["algoStatus"], row["actualOrderId"]
                ),
            );
            let child = child.expect("the trailing stop's child is under its clientAlgoId");
            assert_eq!(child["clientOrderId"].as_str(), Some(exit.as_str()), "{child}");
            assert_eq!(text(&child["orderId"]), text(&row["actualOrderId"]), "{child} {row}");
            assert_eq!(child["status"], "FILLED", "the market child fills: {child}");
            assert_eq!(child["reduceOnly"], true, "the child closes reduce-only: {child}");
            let confirmed = rest.confirm_order(&exit, now_ms());
            finding("TR/confirm", format!("our confirm_order after the fill: {confirmed:?}"));
            assert!(accepted(&confirmed).is_some(), "{confirmed:?}");
            let flat_by = Instant::now() + Duration::from_secs(15);
            while venue.position(SYMBOL) != 0.0 && Instant::now() < flat_by {
                std::thread::sleep(Duration::from_millis(500));
            }
            assert_eq!(venue.position(SYMBOL), 0.0, "a fired reduce-only trailing stop flattens");
        }
        None => {
            let ours = rest.cancel_order(&exit);
            finding(
                "TR/trigger",
                format!("NOT MEASURED: {exit} did not fire in {window:?}; cancel: {ours:?}"),
            );
        }
    }

    // 7. flat and clean, whatever the path
    drop(flat_guard);
    let v = Venue::new(&creds);
    assert_eq!(v.position(SYMBOL), 0.0, "the drill leaves the symbol flat");
    assert!(algo(&v).is_empty() && regular(&v).is_empty(), "nothing rests");
    println!("PERCENT TRAILING DRILL done in {:?}: flat, nothing resting", started.elapsed());
}

/// The spot percent trailing stops through our `VenueRestBinanceSpot` (module doc). Every BUY needs a 10% move
/// to fire, so nothing can fill.
#[test]
#[ignore = "network + DEMO creds: places and cancels real DEMO spot trailing stops, none can fill - run by hand (see module doc)"]
fn spot_percent_trailing_on_the_demo_host() {
    vike_log::test_init();
    let Some(creds) = drill_credentials() else { return };
    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate());
    let info = transport
        .public(demo_spot(), spot::PATH_EXCHANGE_INFO, &[("symbol", SYMBOL.into())])
        .expect("exchangeInfo");
    finding("TR-SPOT/filter", format!("{SYMBOL} filters: {}", info["symbols"][0]["filters"]));
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
    let qty = ((properties_value.min_notional.max(5.0) * 2.5 / last) / step).ceil() * step;
    let tag = now_ms() % 100_000_000;
    let trailing = |order_client_id: String, percent: f64, activation: Option<f64>| OrderRequest {
        order_client_id,
        venue: "binance".to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty,
        order_type: OrderType::Trailing,
        trail_percent: Some(percent),
        trigger_price: activation,
        timestamp: now_ms(),
        ..Default::default()
    };
    // a BUY armed 10% UNDER the market and a SELL armed 10% ABOVE it: both `TAKE_PROFIT` (the FAQ's side
    // table), neither armed inside the window; the SELL needs the wallet's ETH
    let placed = [
        trailing(format!("vtstrN{tag}"), 10.0, None),
        trailing(format!("vtstrA{tag}"), 5.0, Some(on_grid(last * 0.90))),
        OrderRequest {
            side: -1,
            ..trailing(format!("vtstrS{tag}"), 5.0, Some(on_grid(last * 1.10)))
        },
    ];
    for req in &placed {
        let event = rest.submit_order(req);
        finding("TR-SPOT/place", format!("{} (last {last}): {event:?}", req.order_client_id));
        assert!(accepted(&event).is_some(), "the spot trailing stop must be accepted: {event:?}");
    }
    let rows = open(&rest);
    let reports = vike_binance::recon_client(&creds, SYMBOL, demo_hosts())
        .expect("recon client")
        .fetch_order_status_reports(0)
        .expect("report path");
    for req in &placed {
        let order_client_id = req.order_client_id.as_str();
        let row = rows
            .iter()
            .find(|r| r["clientOrderId"].as_str() == Some(order_client_id))
            .unwrap_or_else(|| panic!("spot openOrders lists {order_client_id}: {rows:?}"));
        finding("TR-SPOT/row", format!("{order_client_id}: {row}"));
        let wire = rest.build_order_params(req);
        let get = |key: &str| wire.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
        assert_eq!(row["type"].as_str().map(str::to_string), get("type"), "{row}");
        let word = if req.trigger_price.is_some() { "TAKE_PROFIT" } else { "STOP_LOSS" };
        assert_eq!(row["type"], word, "the venue holds the {} as {word}: {row}", req.side);
        assert_eq!(row["side"], if req.side > 0 { "BUY" } else { "SELL" }, "{row}");
        assert_eq!(Some(text(&row["trailingDelta"])), get("trailingDelta"), "the delta: {row}");
        if req.trigger_price.is_some() {
            assert_eq!(
                number(&row["stopPrice"]),
                sent(&wire, "stopPrice"),
                "the activation: {row}"
            );
        }
        let confirmed = rest.confirm_order(order_client_id, now_ms());
        assert!(accepted(&confirmed).is_some(), "{order_client_id} confirms live: {confirmed:?}");
        let report = reports
            .iter()
            .find(|r| r.order_client_id.as_deref() == Some(order_client_id))
            .unwrap_or_else(|| panic!("the report path lists {order_client_id}: {reports:?}"));
        assert_eq!(
            report.order_type,
            OrderType::Trailing,
            "the report reads the delta: {report:?}"
        );
        let modified = rest.modify_order(req, Some(qty * 2.0), None);
        assert!(matches!(modified.as_slice(), [Event::OrderModifyRejected(_)]), "{modified:?}");
    }
    for req in &placed {
        let ack = rest.cancel_order(&req.order_client_id).ok();
        assert_eq!(ack, Some(OrderCancelAck::Canceled), "our cancel of {}", req.order_client_id);
    }
    // the venue's words for a delta under the filter (the bridge does not cache `TRAILING_DELTA`, so it
    // sends a whole delta) and for an activation already met
    for (what, req) in [
        ("trailingDelta 5", trailing(format!("vtstrR{tag}"), 0.05, None)),
        (
            "BUY armed above the market",
            trailing(format!("vtstrU{tag}"), 10.0, Some(on_grid(last * 1.10))),
        ),
    ] {
        let event = rest.submit_order(&req);
        finding("TR-SPOT/refuse", format!("{what}: {event:?}"));
        if accepted(&event).is_some() {
            finding("TR-SPOT/refuse", format!("{what} was ACCEPTED; cancelling it"));
            assert!(rest.cancel_order(&req.order_client_id).is_ok(), "cancel {what}");
        }
    }
    assert!(open(&rest).is_empty(), "nothing rests on spot {SYMBOL}");
    println!("SPOT PERCENT TRAILING DRILL done: placed, read back and cancelled");
}
