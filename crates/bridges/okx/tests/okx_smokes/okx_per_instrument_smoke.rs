//! Per-instrument exec DEMO smoke: one okx exec client (`vike_okx::execution::spawn`) mounted on `BTC-USDT-SWAP` signs each
//! order, cancel and status read on the ORDER's own instId, with that instrument's grid and `ctVal`;
//! and after a restart a fresh client cancels a resting order on another instrument as its own.
//!
//!     cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture okx_per_instrument_smoke::
//!
//! ORDER-PLACING, OKX DEMO only: the credentials are the default account's at tier demo
//! (`crate::demo_credentials`), every REST client is `crate::demo_swap`'s (header `x-simulated-trading: 1`)
//! and both exec clients are spawned with `mainnet = false`. At most 4 orders, order client ids
//! `n064ok<run>x<n>`, each a LIMIT BUY of one minimum lot 20% below the instrument's last price
//! (cannot cross):
//!   1. on `ETH-USDT-SWAP`: ACCEPTED, and the venue's own row of it (`orders-pending`) carries the
//!      instId and `sz` = base / ETH's `ctVal`, nothing filled;
//!   2. on `DOGE-USDT-SWAP` (another `ctVal`, read at run time), only if the demo lists it and its
//!      minimum lot is small in quote terms; same checks;
//!   3. on the mounted `BTC-USDT-SWAP`: the mounted path is unchanged;
//!      each is then cancelled THROUGH THE SAME client and the venue says `canceled` on its instId;
//!   4. the restart: the first client rests a second ETH order; the real reconcile client (mounted on
//!      BTC, ETH admitted) and `plan_restore` ADOPT it, as the node's first pass after a restart adopts
//!      an operator's resting limit since the restart-restores-resting-orders fix (back in the book as
//!      ours, not a held `UnknownOrder`); the adopted `(order client id, instId)` is announced to a SECOND client
//!      (mounted on BTC) through `adopt_restored`, as the core announces it, and that client's cancel
//!      must leave the order `canceled` on ETH at the VENUE (a cancel sent to the mounted instrument is
//!      answered "unknown", read as gone, and leaves the ETH order resting).
//!
//! Every cancel is confirmed on the venue (`GET trade/order`), and the account ends FLAT: no
//! `n064ok*` order resting, no change in any SWAP position.
//!
//! CLEANUP: each order has a `CancelOnDrop` guard on a client of its own instrument, armed before it
//! is sent, so a failed assertion still cancels it. Self-skips (a warn and an early return) without
//! the demo account's credentials.

use std::time::Duration;

use crate::demo_credentials::load_demo_credentials;
use crate::demo_swap::build_rest;
use serde_json::{Value, json};
use vike_bridge_core::smoke::{
    CancelOnDrop, adopts, buy, lifecycle_word, plan_pass, restored_limit,
};
use vike_bridge_core::transport::json::RestTransportJsonUreq;
use vike_execution::recon::ReconcileClient;
use vike_execution::testing::await_event;
use vike_execution::{ExecutionClient, event_channel};
use vike_log::poll::wait_until;
use vike_model::{OrderRequest, OrderType, TimeInForce, now_ms};
use vike_okx::ReconcileClientOkx;
use vike_okx::execution::fetch_okx_instrument_row;
use vike_okx::execution::rest::{
    PATH_ORDER_CREATE, PATH_ORDERS_PENDING, PATH_POSITIONS, VenueRestOkx,
};
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;

const MOUNTED: &str = "BTC-USDT-SWAP";
const ETH: &str = "ETH-USDT-SWAP";
const DOGE: &str = "DOGE-USDT-SWAP";
const PREFIX: &str = "n064ok";
/// Skip the DOGE leg when its minimum lot costs more than this many quote units at the limit price.
const MAX_MIN_LOT_NOTIONAL: f64 = 1000.0;
/// How long the client's answer, and the venue's row, may take.
const WAIT: Duration = Duration::from_secs(20);

type SwapRest = VenueRestOkx<RestTransportJsonUreq>;

fn field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The venue's own row for `order_client_id` on `rest`'s instrument (`GET trade/order`), `None` when it has none.
fn venue_order(rest: &SwapRest, order_client_id: &str) -> Option<Value> {
    let data = rest
        .call_get(
            PATH_ORDER_CREATE,
            &[("instId", json!(rest.symbol)), ("clOrdId", json!(order_client_id))],
        )
        .ok()?;
    data.as_array()?.first().cloned()
}

/// Every resting `n064ok*` order of the account's SWAPs, the venue's own rows (`orders-pending`).
fn resting_ours(rest: &SwapRest) -> Vec<Value> {
    let data =
        rest.call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP"))]).expect("orders-pending");
    data.as_array()
        .into_iter()
        .flatten()
        .filter(|o| field(o, "clOrdId").starts_with(PREFIX))
        .cloned()
        .collect()
}

/// Every nonzero SWAP position, `(instId, contracts)`, sorted: the "flat" witness.
fn positions(rest: &SwapRest) -> Vec<(String, String)> {
    let data = rest.call_get(PATH_POSITIONS, &[("instType", json!("SWAP"))]).expect("positions");
    let mut out: Vec<(String, String)> = data
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| field(p, "pos").parse::<f64>().unwrap_or(0.0) != 0.0)
        .map(|p| (field(p, "instId").to_string(), field(p, "pos").to_string()))
        .collect();
    out.sort();
    out
}

/// A LIMIT BUY of one minimum lot, in BASE units, 20% below `rest`'s last price: it cannot cross.
fn far_buy(rest: &SwapRest, order_client_id: &str) -> OrderRequest {
    let last = rest.last_price().expect("ticker");
    assert!(last > 0.0, "{}: a last price is required", rest.symbol);
    let base = rest.properties.min_qty * rest.ct_val;
    buy(
        order_client_id,
        "okx",
        &rest.symbol,
        base,
        OrderType::Limit,
        Some(last * 0.8),
        TimeInForce::Gtc,
    )
}

/// Polls until the venue rests `order` and holds it to the order's own instrument and contract size,
/// unfilled.
fn assert_resting(rest: &SwapRest, order: &OrderRequest) {
    let order_client_id = order.order_client_id.as_str();
    let mut row = None;
    wait_until(WAIT, || {
        row = resting_ours(rest).into_iter().find(|o| field(o, "clOrdId") == order_client_id);
        row.is_some()
    });
    let row = row.unwrap_or_else(|| panic!("{order_client_id} never rested at the venue"));
    let expect_sz = rest.to_contracts(order.qty);
    tracing::info!(
        target: "vike_okx",
        "WIRE {order_client_id}: instId={} sz={} px={} accFillSz={} (ctVal={} lotSz={} minSz={}, base {} -> expected sz {expect_sz})",
        field(&row, "instId"), field(&row, "sz"), field(&row, "px"), field(&row, "accFillSz"),
        rest.ct_val, rest.properties.step_size, rest.properties.min_qty, order.qty
    );
    assert_eq!(field(&row, "instId"), rest.symbol, "the order rests on its own instrument: {row}");
    assert_eq!(
        field(&row, "sz").parse::<f64>().unwrap(),
        expect_sz,
        "{order_client_id}: sz must be base/ctVal of {}: {row}",
        rest.symbol
    );
    assert_eq!(
        field(&row, "accFillSz").parse::<f64>().unwrap_or(0.0),
        0.0,
        "nothing filled: {row}"
    );
}

/// Polls until the venue says `order_client_id` is `canceled` on `rest`'s instrument.
fn wait_canceled(rest: &SwapRest, order_client_id: &str) {
    let mut row = None;
    let done = wait_until(WAIT, || {
        row = venue_order(rest, order_client_id);
        row.as_ref().is_some_and(|r| field(r, "state") == "canceled")
    });
    assert!(
        done,
        "{order_client_id} on {} never read canceled at the venue; last row: {row:?}",
        rest.symbol
    );
    tracing::info!(target: "vike_okx", "VENUE {order_client_id}: canceled on {}", rest.symbol);
}

#[test]
#[ignore = "network + demo creds — places + cancels real (non-filling) demo orders — run manually (see module doc)"]
fn okx_per_instrument_orders_cancel_and_restart() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let btc = build_rest(&creds, MOUNTED);
    let eth = build_rest(&creds, ETH);
    tracing::info!(target: "vike_okx", "ctVal read at run time: {MOUNTED}={} {ETH}={}", btc.ct_val, eth.ct_val);
    assert_ne!(btc.ct_val, eth.ct_val, "the test needs two different ctVals");
    let doge = fetch_okx_instrument_row(DOGE, &demo_net()).ok().map(|_| build_rest(&creds, DOGE)).filter(|d| {
        let notional = d.properties.min_qty * d.ct_val * d.last_price().expect("ticker") * 0.8;
        tracing::info!(target: "vike_okx", "{DOGE}: ctVal={} minSz={} min-lot notional={notional}", d.ct_val, d.properties.min_qty);
        notional <= MAX_MIN_LOT_NOTIONAL
    });
    if doge.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP DOGE leg: not listed on the demo, or its minimum lot is above {MAX_MIN_LOT_NOTIONAL}");
    }

    assert!(resting_ours(&btc).is_empty(), "stale {PREFIX}* orders rest from an earlier run");
    let positions_before = positions(&btc);
    let run = now_ms() % 100_000_000;
    let order_client_id = |n: u8| format!("{PREFIX}{run}x{n}");

    let (events, mut rx) = event_channel(512);
    let mut client =
        vike_okx::execution::spawn(creds.clone(), MOUNTED.to_string(), events, demo_net());
    std::thread::sleep(Duration::from_secs(4)); // login + subscribe settle

    // The client's answer about `order_client_id`: `want`, or a refusal, which fails; its other words are skipped.
    let said = |rx: &mut _, order_client_id: &str, want: &str| {
        let got = await_event(rx, WAIT, |e| {
            let (c, w) = lifecycle_word(e)?;
            (c == order_client_id && (w == want || w.ends_with("Rejected")))
                .then(|| format!("{w}: {e:?}"))
        });
        assert!(
            got.as_deref().is_some_and(|g| g.starts_with(want)),
            "{order_client_id}: wanted {want}, the client said {got:?}"
        );
    };

    // (1)-(3): ETH and DOGE (contract values other than the mounted one's), then the mounted BTC.
    let legs: Vec<(String, &SwapRest)> = [(1, Some(&eth)), (2, doge.as_ref()), (3, Some(&btc))]
        .into_iter()
        .filter_map(|(n, rest)| Some((order_client_id(n), rest?)))
        .collect();
    let _guards: Vec<_> = legs
        .iter()
        .map(|(c, rest)| CancelOnDrop { rest: *rest, order_client_id: c, host: "demo" })
        .collect();
    for (c, rest) in &legs {
        let order = far_buy(rest, c);
        client.submit(&order);
        said(&mut rx, c, "Accepted");
        assert_resting(rest, &order);
    }
    for (c, rest) in &legs {
        client.cancel(c);
        said(&mut rx, c, "Canceled");
        wait_canceled(rest, c);
    }

    // (4) the restart: the planner adopts the resting ETH order and a fresh client is told of it.
    let c4 = order_client_id(4);
    let _guard4 = CancelOnDrop { rest: &eth, order_client_id: &c4, host: "demo" };
    let order = far_buy(&eth, &c4);
    client.submit(&order);
    said(&mut rx, &c4, "Accepted");
    assert_resting(&eth, &order);

    let recon = ReconcileClientOkx::new(
        OkxV5Signer::new(&creds, now_ms),
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
        demo_net().rest,
        MOUNTED,
        btc.ct_val,
    );
    recon.admit_symbol(ETH);
    let (plan, _) = plan_pass(&recon, "okx", ETH, &[restored_limit(&c4, "okx", ETH)]);
    assert!(adopts(&plan, &c4), "the resting ETH order is ADOPTED, back as ours: {plan:?}");
    let announced: Vec<vike_execution::RestoredOrder> =
        plan.adopt.iter().filter_map(vike_execution::RestoredOrder::of_report).collect();
    let pairs: Vec<(&str, &str)> =
        announced.iter().map(|o| (o.order_client_id.as_str(), o.symbol.as_str())).collect();
    assert_eq!(pairs, [(c4.as_str(), ETH)], "adopted on its own instId: {plan:?}");

    let (events2, mut rx2) = event_channel(512);
    let mut second =
        vike_okx::execution::spawn(creds.clone(), MOUNTED.to_string(), events2, demo_net());
    std::thread::sleep(Duration::from_secs(4));
    second.adopt_restored(&announced);
    second.cancel(&c4);
    said(&mut rx2, &c4, "Canceled");
    wait_canceled(&eth, &c4);
    assert!(
        venue_order(&btc, &c4).is_none(),
        "the mounted instrument must not know the ETH order client id"
    );

    // (5) flat
    second.detach();
    client.detach();
    assert!(resting_ours(&btc).is_empty(), "a {PREFIX}* order still rests");
    assert_eq!(positions(&btc), positions_before, "a SWAP position changed");
    tracing::info!(target: "vike_okx", "per-instrument smoke GREEN (doge leg: {}): nothing resting, positions unchanged", doge.is_some());
}
