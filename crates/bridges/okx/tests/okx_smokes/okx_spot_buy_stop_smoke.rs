//! A SPOT market BUY STOP on the OKX DEMO account, through the bridge's own `VenueRestOkx`, FIRED and
//! FILLED: the proof of wave 2's `tgtCcy=base_ccy` fix (`crates/bridges/okx/src/execution/rest/stop_algo.rs`'s
//! `submit_stop_algo`). OKX's "Place algo order" table counts a spot "Market buy conditional order"'s
//! `sz` in the QUOTE by default ("Default is quote_ccy for buy",
//! <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-place-algo-order>), so before the
//! fix a buy stop for a few satoshi bought a few cents of BTC. `tests/offline/take_profit.rs`'s
//! `a_spot_market_buy_stop_counts_its_size_in_the_base` pins the bytes offline. PASSED on the demo
//! 2026-11-08, 2 of 2 (main `d1624c326e` / `f1a5acec7c`, store account `#15 okx/demo`): the fill counted in the
//! base coin.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture --test-threads=1 okx_spot_buy_stop_smoke::
//!
//! ORDER-PLACING (`BTC-USDT` spot), DEMO ONLY:
//!   * `okx_a_spot_market_buy_stop_fills_its_size_in_the_base_coin`: a BUY stop for three times the pair's
//!     minimum size (one minimum, net of the fee the venue takes in BTC, could not be sold back alone) with its
//!     trigger one tick above the demo's own last price, re-placed at the new price until it fires (ten tries
//!     of 60 s); once it is `effective`, its child order is read by `ordId` until `filled`, and its
//!     `accFillSz` must equal the requested size within one lot (the quote reading would be thousands of
//!     times smaller). Then the bought BTC, net of the fee, is sold back through the bridge and the BTC
//!     balance must end where it began within the fee and one lot.
//!   * `okx_the_spot_buy_stop_smoke_leaves_nothing_resting`: READ-ONLY, last by name: no order with this
//!     smoke's `vtsb` order client id prefix rests on `BTC-USDT` (regular or algo).
//!
//! What a run must print before its asserts, and what a miss costs: `MEASURED the fired stop's order`
//! with an `accFillSz` far below the request means the venue still read the size as quote, and every spot buy
//! stop the bridge places is mis-sized; the `tgtCcy` of the `order-algo` row says what the venue holds.
//!
//! Gated like every `*_smoke.rs` here: the DEMO account's creds through `vike_secrets::smoke_directory` (asked
//! for `AccountTier::Demo`), an early return when absent; every transport carries the simulated-trading header
//! (`demo_swap.rs`'s `build_rest`). Credentials are never printed: only venue words and this smoke's own ids.
//! A `Drop` guard cancels the stop and sells back what was bought on every exit path; a run killed from
//! outside skips it: check the demo account's BTC by hand.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_model::events::Event;
use vike_model::{OrderRequest, now_ms};
use vike_okx::execution::rest::{
    PATH_ACCOUNT, PATH_ORDER_ALGO, PATH_ORDER_CREATE, PATH_ORDERS_ALGO_PENDING,
    PATH_ORDERS_PENDING, VenueRestOkx,
};
use vike_okx::transport::client::unwrap_okx;

use crate::demo_credentials::load_demo_credentials;
use crate::demo_swap::build_rest;
use crate::okx_stop_limit_smoke::{accepted_id, pick};

const SPOT: &str = "BTC-USDT";

/// What the run placed and bought, undone on every exit path.
struct Run {
    rest: VenueRestOkx<RestTransportJsonUreq>,
    stops: Vec<String>,
    /// BTC bought and not yet sold back (net of the fee), in base.
    to_sell: f64,
}

impl Run {
    fn order(&self, request: OrderRequest) -> Vec<Event> {
        VenueRest::submit_order(&self.rest, &request)
    }

    fn request(
        &self,
        order_client_id: &str,
        side: i32,
        order_type: &str,
        qty: f64,
        trigger: Option<f64>,
    ) -> OrderRequest {
        serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "okx", "symbol": SPOT, "side": side, "qty": qty,
            "order_type": order_type, "trigger_price": trigger, "ts": now_ms()
        }))
        .unwrap()
    }

    /// `GET order-algo` by `algoClOrdId`: the venue's own row of the stop.
    fn algo_row(&self, order_client_id: &str) -> Result<Value, String> {
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

    /// `GET trade/order` by the venue's `ordId`.
    fn order_row(&self, ord_id: &str) -> Result<Value, String> {
        self.rest
            .call_get(PATH_ORDER_CREATE, &[("instId", json!(SPOT)), ("ordId", json!(ord_id))])
            .map(|rows| rows[0].clone())
            .map_err(|e| format!("{} {}", e.code, e.msg))
    }

    /// The account's BTC `cashBal`.
    fn btc(&self) -> Result<f64, String> {
        let rows = self.rest.call_get(PATH_ACCOUNT, &[("ccy", json!("BTC"))]).map_err(|e| e.msg)?;
        let details = rows[0]["details"].as_array().cloned().unwrap_or_default();
        Ok(details
            .iter()
            .find(|d| d["ccy"] == "BTC")
            .and_then(|d| d["cashBal"].as_str())
            .and_then(|t| t.parse::<f64>().ok())
            .unwrap_or(0.0))
    }

    /// Sell `qty` BTC back through the bridge (a spot market sell: `sz` is base).
    fn sell_back(&mut self, qty: f64) -> Vec<Event> {
        let order_client_id = format!("vtsbsell{}", now_ms() % 100_000_000);
        let events = self.order(self.request(&order_client_id, -1, "market", qty, None));
        if accepted_id(&events).is_some() {
            self.to_sell = 0.0;
        }
        events
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        for order_client_id in std::mem::take(&mut self.stops) {
            if let Err(e) = self.rest.cancel_order(&order_client_id) {
                println!(
                    "CLEANUP: bridge cancel of {order_client_id} answered {} {}",
                    e.code, e.msg
                );
            }
        }
        if self.to_sell > 0.0 {
            let qty = self.to_sell;
            println!("CLEANUP sell back {qty} BTC: {:?}", self.sell_back(qty));
        }
    }
}

#[test]
#[ignore = "network + demo creds — lets a demo SPOT buy stop FIRE and FILL, then sells the BTC back — run manually (see module doc)"]
fn okx_a_spot_market_buy_stop_fills_its_size_in_the_base_coin() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");
    let mut run = Run { rest: build_rest(&creds, SPOT), stops: Vec::new(), to_sell: 0.0 };
    let (lot, tick) = (run.rest.properties.step_size, run.rest.properties.tick_size);
    assert!((run.rest.ct_val - 1.0).abs() < 1e-12, "a spot pair counts in its base coin");
    let qty = run.rest.properties.min_qty * 3.0;
    let btc0 = run.btc().expect("the BTC balance");
    println!("MEASURED start: BTC cashBal {btc0}, request {qty} BTC (lot {lot}, tick {tick})");

    // --- 1. a BUY stop one tick above the demo's own last price, re-placed until it fires ---------------
    let mut fired = None;
    for attempt in 0..10 {
        let last = run.rest.last_price().expect("ticker");
        let trigger = last + tick * [1.0, 5.0, 10.0][attempt.min(2)];
        let order_client_id = format!("vtsbstop{}", now_ms() % 100_000_000);
        run.stops.push(order_client_id.clone());
        let events = run.order(run.request(&order_client_id, 1, "stop", qty, Some(trigger)));
        if accepted_id(&events).is_none() {
            println!(
                "MEASURED buy stop {order_client_id} refused at trigger {trigger} (last {last}): {events:?}"
            );
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        println!(
            "PLACED spot buy stop {order_client_id} qty={qty} trigger={trigger} last={last} (attempt {attempt})"
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
            run.stops.retain(|c| c != &order_client_id);
            fired = Some(order_client_id);
            break;
        }
        println!(
            "MEASURED buy stop {order_client_id} did not fire in 60 s (venue state {state:?}); cancelling"
        );
        println!("  bridge cancel: {:?}", run.rest.cancel_order(&order_client_id));
    }
    let order_client_id =
        fired.expect("no spot buy stop fired in ten attempts: the market was too quiet");

    // --- 2. MEASURE: the fired stop's row and the order it placed, until filled ------------------------
    let row = run.algo_row(&order_client_id).expect("order-algo of the fired stop");
    println!(
        "MEASURED order-algo of the FIRED spot buy stop: {}",
        pick(
            &row,
            &[
                "state",
                "sz",
                "tgtCcy",
                "slTriggerPx",
                "slOrdPx",
                "ordIdList",
                "actualSz",
                "actualPx"
            ]
        )
    );
    let child = row["ordIdList"][0].as_str().or(row["ordId"].as_str()).unwrap_or("").to_string();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut filled = run.order_row(&child);
    while Instant::now() < deadline && !filled.as_ref().is_ok_and(|r| r["state"] == "filled") {
        std::thread::sleep(Duration::from_secs(1));
        filled = run.order_row(&child);
    }
    let filled = filled.expect("GET order of the fired stop's child");
    println!(
        "MEASURED the fired stop's order: {}",
        pick(
            &filled,
            &["state", "ordType", "side", "sz", "tgtCcy", "accFillSz", "avgPx", "fee", "feeCcy"]
        )
    );
    let number =
        |r: &Value, k: &str| r[k].as_str().and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0);
    let accumulated = number(&filled, "accFillSz");
    // the venue takes a spot buy's fee in BTC (negative): what the account holds is the fill net of it
    let fee_btc = if filled["feeCcy"] == "BTC" { number(&filled, "fee") } else { 0.0 };
    run.to_sell = accumulated + fee_btc;

    // --- 3. sell the bought BTC back; the balance ends where it began --------------------------------
    let sold = run.sell_back(accumulated + fee_btc);
    println!("MEASURED sell back of {} BTC: {sold:?}", accumulated + fee_btc);
    std::thread::sleep(Duration::from_secs(3));
    let btc1 = run.btc();
    println!("MEASURED end: BTC cashBal {btc1:?} (start {btc0})");

    // --- 4. ASSERT, last -------------------------------------------------------------------------------
    assert_eq!(filled["state"], "filled", "the fired stop's market order must fill");
    assert!(
        (accumulated - qty).abs() <= lot + 1e-12,
        "the FILLED size must be the requested BASE quantity within one lot: filled {accumulated} BTC for {qty} \
         (a quote reading would be about {qty} USDT of BTC)"
    );
    assert!(accepted_id(&sold).is_some(), "the sell back must be accepted: {sold:?}");
    let drift = btc1.expect("the BTC balance at the end") - btc0;
    assert!(
        drift.abs() <= qty * 0.002 + lot * 2.0,
        "the BTC balance must end where it began within the fee and the lot: moved {drift}"
    );
    println!(
        "okx spot buy stop smoke green: fired -> filled {accumulated} BTC for {qty} -> sold back -> balance flat"
    );
}

/// READ-ONLY, and last of the module by name: nothing with this smoke's `vtsb` order client id prefix rests on
/// `BTC-USDT`, on the regular book or the algo book. Other clients' orders are counted, never touched.
#[test]
#[ignore = "network + demo creds — read-only check that the spot buy stop smoke left nothing resting — run manually (see module doc)"]
fn okx_the_spot_buy_stop_smoke_leaves_nothing_resting() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds, SPOT);
    let regular = rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SPOT")), ("instId", json!(SPOT))])
        .expect("orders-pending");
    let algo = rest
        .call_get(
            PATH_ORDERS_ALGO_PENDING,
            &[("ordType", json!("conditional")), ("instId", json!(SPOT))],
        )
        .expect("orders-algo-pending");
    let rows = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let left: Vec<String> = rows(&regular)
        .iter()
        .chain(rows(&algo).iter())
        .flat_map(|r| [r["clOrdId"].as_str(), r["algoClOrdId"].as_str()])
        .flatten()
        .filter(|c| c.starts_with("vtsb"))
        .map(String::from)
        .collect();
    println!(
        "MEASURED BTC-USDT at the end: resting regular {} algo {}; ours {left:?}",
        rows(&regular).len(),
        rows(&algo).len()
    );
    assert!(left.is_empty(), "a smoke order still rests: {left:?}");
}
