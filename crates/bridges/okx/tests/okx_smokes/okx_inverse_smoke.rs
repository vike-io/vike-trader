//! **The okx INVERSE mount demo smoke** (decision 0258's okx amendment): the coin-margined `BTC-USD-SWAP`, mounted
//! the way `crates/bridges/okx/src/execution/mod.rs`'s `run` mounts it (`mount_money`: `Inverse(100)` settled in BTC, every
//! client's contracts→quantity factor 1.0), rests and cancels a far limit counted in CONTRACTS; the mount's
//! reconcile client, built as `crates/bridges/okx/src/adapter.rs`'s start builds it (factor 1.0), lists it and
//! reads the position, the BTC cash, the fee rate and the funding bills back in the coin.
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture --test-threads=1 okx_inverse_smoke::
//!
//! ORDER-PLACING (inverse swap `BTC-USD-SWAP`, the DEMO account `demo_credentials` resolves): ONE limit BUY of the row's
//! minimum size (0.1 contract, 10 USD) at 60% of the last price, which cannot fill in the seconds it rests, then
//! its cancel. Nothing touches a position or an account setting. It places NOTHING when the account's BTC cash
//! cannot carry the order's whole notional (a `SKIP:` line names both figures) and runs the read-back alone.
//!
//! What it proves:
//!   1. the exec grid fetch (`fetch_okx_instrument_row`) answers the coin-margined row and `mount_money` reads it
//!      as `Inverse(100)` settled in BTC;
//!   2. a client with the factor 1.0 on that row's grid places the far limit through the real builder: ACCEPTED,
//!      and the venue's own row echoes the contracts and the price (`CAPTURED`);
//!   3. the mount's reconcile client lists it live in contracts, then the cancel ends it;
//!   4. the same client reads the position, the BTC `cashBal` (equal to the raw read) and the crypto-margined fee
//!      rate back; each raw answer is a `CAPTURED` line (the fixtures of `tests/offline/inverse_mount.rs`);
//!   5. the funding poll of the mount (`bills`, `type=8`, `instId=BTC-USD-SWAP`) is answered.
//!
//! Only `inverse smoke green` is a proof; a `SKIP:` line is not. Steps 4 and 5 alone are the READ-ONLY
//! `okx_inverse_mount_reads_back_in_the_coin`, which places nothing. Credentials are never printed: only venue
//! words, this run's own ids and the account's balances of BTC and USDT.
//!
//! CLEANUP: a `CancelOnDrop` guard cancels the order client id on every exit path. A run killed from outside leaves the order
//! resting: cancel the order client id the log names (`vtoinv…`) by hand.

use std::time::{Duration, Instant};

use crate::demo_credentials::load_demo_credentials;
use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::{CancelOnDrop, buy};
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_model::events::Event;
use vike_model::{ContractValue, OrderType, TimeInForce, now_ms};
use vike_okx::execution::funding::OkxFundingPoller;
use vike_okx::execution::rest::{
    PATH_ACCOUNT, PATH_BILLS, PATH_ORDER_CREATE, PATH_ORDERS_PENDING, PATH_POSITIONS, VenueRestOkx,
};
use vike_okx::reconcile::PATH_TRADE_FEE;
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;
use vike_secrets::credentials::Credentials;

const SYMBOL: &str = "BTC-USD-SWAP";
const COIN: &str = "BTC";

type SwapRest = VenueRestOkx<RestTransportJsonUreq>;

/// The mount's client as the exec thread builds it: the row's CONTRACTS grid and the factor 1.0, DEMO only (the
/// simulated-trading header is on, the credentials were asked for at `demo`).
fn rest_for(creds: &Credentials, properties: vike_model::SymbolProperties) -> SwapRest {
    let rest = VenueRestOkx {
        signer: OkxV5Signer::new(creds, now_ms),
        transport: rest_transport(true)
            .with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
        base_url: demo_net().rest,
        symbol: SYMBOL.to_string(),
        properties,
        ct_val: 1.0,
        leverage: 2.0,
        broker_code: None,
    };
    assert_eq!(
        rest.transport.network(),
        vike_model::VenueNetwork::Testnet,
        "this smoke is DEMO ONLY"
    );
    rest
}

fn text(row: &Value, key: &str) -> String {
    row[key].as_str().map_or_else(|| row[key].to_string(), str::to_string)
}

/// The venue's own row for `order_client_id` (`GET trade/order`), polled until it answers.
fn venue_row(rest: &SwapRest, order_client_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(data) = rest.call_get(
            PATH_ORDER_CREATE,
            &[("instId", json!(SYMBOL)), ("clOrdId", json!(order_client_id))],
        ) && let Some(row) = data.as_array().and_then(|d| d.first())
        {
            return row.clone();
        }
        assert!(Instant::now() < deadline, "{order_client_id}: the venue never listed it");
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The account's detail row of `currency` in the raw `account/balance` answer, `Null` when it lists none.
fn detail(balance: &Value, currency: &str) -> Value {
    balance
        .as_array()
        .and_then(|a| a.first())
        .and_then(|account| account["details"].as_array())
        .and_then(|d| d.iter().find(|r| r["ccy"] == currency).cloned())
        .unwrap_or(Value::Null)
}

#[test]
#[ignore = "network + demo creds — places and cancels one real (non-filling) inverse demo limit — run manually (see module doc)"]
fn okx_inverse_mount_rests_cancels_and_reads_back_in_the_coin() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };

    // --- 1. the mount's row, read the way the exec thread reads it -------------------------------------
    let row = vike_okx::execution::fetch_okx_instrument_row(SYMBOL, &demo_net())
        .expect("the exec fetch answers");
    println!(
        "FINDING okx inverse: exec row ctType={:?} ctVal={} ctMult={} settleCcy={:?} grid={:?}",
        row.contract_type, row.ct_val, row.ct_mult, row.settle_ccy, row.properties
    );
    assert_eq!(
        vike_okx::execution::mount_money(SYMBOL, Some(&row)),
        Ok(Some((ContractValue::inverse(100.0), COIN.to_string()))),
        "BTC-USD-SWAP mounts Inverse(100) in BTC"
    );
    let properties = row.properties;
    let rest = rest_for(&creds, properties);
    let recon =
        vike_okx::recon_client(&creds, SYMBOL, 1.0, &demo_net()).expect("a reconcile client");

    // --- the margin the far order needs, against the account's BTC cash ---------------------------------
    let last = rest.last_price().expect("ticker");
    assert!(last > 0.0, "a live last price is required");
    let tick = properties.tick_size;
    let price = (last * 0.6 / tick).round() * tick;
    let qty = properties.min_qty;
    // the order's whole notional in the coin at its own price: the margin at 1x, the most it can ask
    let need = qty * row.ct_val / price;
    let cash = recon.fetch_balance().expect("the coin cash").unwrap_or(0.0);
    println!(
        "FINDING okx inverse: last {last}, far price {price}, {qty} contract(s) = {} USD; BTC cash {cash}, \
         notional in BTC {need}",
        qty * row.ct_val
    );
    if cash < need {
        println!(
            "SKIP: okx inverse: the demo account's BTC cash {cash} is below the far order's notional {need} BTC; \
             nothing is placed (no money is moved)"
        );
        read_back(&creds);
        return;
    }

    // --- 2. the far limit, through the real builder -----------------------------------------------------
    let order_client_id = format!("vtoinv{}", now_ms() % 100_000_000);
    let request =
        buy(&order_client_id, "okx", SYMBOL, qty, OrderType::Limit, Some(price), TimeInForce::Gtc);
    let body = rest.build_order_params(&request, None).expect("the builder takes it");
    println!("FINDING okx inverse: the wire body {body:?}");
    tracing::info!(target: "vike_okx", order_client_id, last, price, qty, "placing a far inverse limit");
    let events = VenueRest::submit_order(&rest, &request);
    let cleanup = CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };
    let Some(Event::OrderAccepted(accepted)) = events.last() else {
        panic!("{order_client_id}: the demo must ACCEPT the inverse limit: {events:?}")
    };
    let venue_id = accepted.venue_order_id.clone().expect("the accept names the venue id");
    let order = venue_row(&rest, &order_client_id);
    println!("CAPTURED okx inverse order: {order}");
    assert_eq!(text(&order, "instId"), SYMBOL, "{order}");
    assert_eq!(text(&order, "sz").parse::<f64>().ok(), Some(qty), "contracts, as sent: {order}");
    assert!((text(&order, "px").parse::<f64>().unwrap_or(f64::NAN) - price).abs() < tick / 2.0);
    let pending =
        rest.call_get(PATH_ORDERS_PENDING, &[("instId", json!(SYMBOL))]).expect("orders-pending");
    println!("CAPTURED okx inverse orders-pending: {pending}");

    // --- 3. the mount's reconcile client lists it, then the cancel ends it ------------------------------
    let reports = recon.fetch_order_status_reports(0).expect("orders-pending");
    let report = reports
        .iter()
        .find(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str()))
        .unwrap_or_else(|| {
            panic!("{order_client_id}: the reconcile read must list it: {reports:?}")
        });
    assert!(
        report.status.is_live() && report.qty == qty,
        "{order_client_id}: contracts: {report:?}"
    );
    rest.cancel_order(&order_client_id).expect("the cancel");
    let deadline = Instant::now() + Duration::from_secs(10);
    let order = loop {
        let row = venue_row(&rest, &order_client_id);
        if text(&row, "state") == "canceled" || Instant::now() > deadline {
            break row;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(text(&order, "state"), "canceled", "{order_client_id}: {order}");
    drop(cleanup);
    tracing::info!(target: "vike_okx", order_client_id, venue_order_id = %venue_id, "placed -> listed -> cancelled");
    read_back(&creds);
    tracing::info!(target: "vike_okx", "inverse smoke green");
}

/// Steps 4 and 5 alone, READ-ONLY (no order, no account setting): what the mount's reconcile client and its
/// funding poll read back. Run it by its name to re-measure without placing anything.
#[test]
#[ignore = "network + demo creds — read-only — run manually (see module doc)"]
fn okx_inverse_mount_reads_back_in_the_coin() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    read_back(&creds);
    tracing::info!(target: "vike_okx", "inverse read-back green");
}

fn read_back(creds: &Credentials) {
    let rest = rest_for(creds, vike_model::SymbolProperties::default());
    let recon =
        vike_okx::recon_client(creds, SYMBOL, 1.0, &demo_net()).expect("a reconcile client");
    // --- 4. position, cash and fee, read back in the coin -----------------------------------------------
    let positions = recon.fetch_position_status_reports().expect("positions");
    println!("FINDING okx inverse: positions {positions:?}");
    assert!(positions.iter().any(|p| p.symbol == SYMBOL), "the mount's row: {positions:?}");
    let raw_position = rest
        .call_get(PATH_POSITIONS, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("positions");
    println!("CAPTURED okx inverse positions: {raw_position}");

    let balance = rest.call_get(PATH_ACCOUNT, &[]).expect("account/balance");
    println!(
        "CAPTURED okx inverse balance details: BTC {} USDT {}",
        detail(&balance, COIN),
        detail(&balance, "USDT")
    );
    let cash = recon.fetch_balance().expect("the coin cash");
    let raw = vike_okx::reconcile::parse_balance_of(&balance.to_string(), COIN).expect("parses");
    println!("FINDING okx inverse: reconcile balance {cash:?} {COIN}, raw cashBal {raw:?}");
    assert!(cash.is_some(), "the coin cash is reported");
    assert_eq!(cash, raw, "the reconcile balance IS the coin's cashBal");

    let fee = rest.call_get(PATH_TRADE_FEE, &[("instType", json!("SWAP"))]);
    println!("CAPTURED okx inverse trade-fee instType=SWAP: {fee:?}");
    let schedule = recon.fetch_fee_rates();
    println!("FINDING okx inverse: reconcile fee schedule {schedule:?}");
    assert!(matches!(schedule, Ok(Some(_))), "the crypto-margined rates are read: {schedule:?}");

    let since = now_ms() - 3 * 24 * 3_600_000;
    let fills = recon.fetch_fill_reports(since).expect("fills");
    println!(
        "FINDING okx inverse: {} fill(s) in 3 days; fee assets {:?}",
        fills.len(),
        fills.iter().map(|f| f.commission_asset.as_str()).collect::<Vec<_>>()
    );

    // --- 5. the funding poll of the mount -----------------------------------------------------------------
    let bills = rest.call_get(
        PATH_BILLS,
        &[("instType", json!("SWAP")), ("type", json!("8")), ("instId", json!(SYMBOL))],
    );
    println!("CAPTURED okx inverse bills type=8: {bills:?}");
    let mut poller = OkxFundingPoller::new(&rest, SYMBOL, 0);
    let funding = poller.poll();
    println!("FINDING okx inverse: the funding poll answered {:?}", funding.as_ref().map(Vec::len));
    assert!(funding.is_ok(), "the mount's funding poll is answered: {funding:?}");
}
