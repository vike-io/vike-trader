//! **The bybit INVERSE mount demo smoke** (decision 0258's bybit amendment): the coin-margined `BTCUSD` (user name
//! `BTCUSD.P`, decision 0290) mounted the
//! way `crates/bridges/bybit/src/execution/mod.rs`'s `run` mounts it signs in `inverse` on the venue's own grid, rests and
//! cancels, and the reconcile client of that mount reads the position, the BTC wallet and the fee rate back in the
//! inverse book.
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture --test-threads=1 bybit_inverse_smoke::
//!
//! ORDER-PLACING (inverse perpetual `BTCUSD`, the DEMO host only, the account `demo_label` names): ONE limit BUY
//! at 60% of the last price, sized just over the row's minimum value (whole USD contracts), which cannot fill in
//! the seconds it rests, then its cancel. Nothing touches a position. The account's BTC collateral switch is put
//! ON by the bridge's own rule (`Modes::ensure`, the venue answers `110101` otherwise) and put back OFF at the end
//! if it was off before.
//!
//! What it proves:
//!   1. the exec grid fetch (`fetch_bybit_instrument`) answers `BTCUSD`'s coin-margined row and `mount_money`
//!      reads it as `Inverse(1.0)` settled in BTC;
//!   2. a client in the mount's book (`inverse`) on that row's grid places the far limit through the real
//!      builder: ACCEPTED, and the venue's own row echoes the contracts and the price (`CAPTURED`);
//!   3. the mount's reconcile client (`vike_bybit::reconcile` for `BTCUSD`) lists it live, then the cancel ends it;
//!   4. the same client reads the position (`position/list?category=inverse`), the BTC wallet (`walletBalance` of
//!      `coin=BTC`, equal to the raw read) and the fee rate (`fee-rate?category=inverse`) back; each answer is a
//!      `CAPTURED` line (the fixtures of `tests/offline/inverse_mount.rs`) and a `FINDING`;
//!   5. the funding poll of an inverse mount (`category=inverse&currency=BTC`) is answered by the venue.
//!
//! Only `inverse smoke green` is a proof; a `SKIP:` line is not. Steps 4 and 5 alone are the READ-ONLY
//! `bybit_inverse_mount_reads_back_in_the_coin` (filter `bybit_inverse_smoke::bybit_inverse_mount_reads_back`),
//! which places nothing.
//!
//! CLEANUP: a `CancelOnDrop` guard cancels the order client id on every exit path, and a collateral guard restores the switch.
//! A run killed from outside leaves the order resting: cancel the order client id the log names (`vtinv…`) by hand.

use std::time::{Duration, Instant};

use crate::demo_label::demo_label;
use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::CancelOnDrop;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_bybit::account::modes::{Modes, PATH_COLLATERAL_INFO, PATH_SET_COLLATERAL_SWITCH};
use vike_bybit::execution::funding::BybitFundingPoller;
use vike_bybit::execution::rest::{
    PATH_ACCOUNT, PATH_ORDER_HISTORY, PATH_ORDER_REALTIME, VenueRestBybit, seed_endpoints,
};
use vike_bybit::history::Category;
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_model::events::Event;
use vike_model::{AccountTier, ContractValue, OrderRequest, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

/// The inverse perpetual's USER name (decision 0290): what the mount's money, mode rule, reconcile client, funding poll
/// and order name, read as its kept row's [`WIRE`] in `inverse` (`vike_bybit::history::kept_list_for_tests`).
const SYMBOL: &str = "BTCUSD.P";
/// The venue's own name of [`SYMBOL`]: what the exec grid fetch, the client and every raw read here name.
const WIRE: &str = "BTCUSD";
const COIN: &str = "BTC";

type PerpRest = VenueRestBybit<RestTransportJsonUreq>;

fn load_demo_credentials() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let label = demo_label(&directory);
    tracing::info!(target: "vike_bybit", ?label, "the bybit demo account this smoke trades");
    let account = directory.credentials_for("bybit", &label, AccountTier::Demo);
    let creds = api_credentials(&account, "bybit");
    if creds.is_none() {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
    }
    creds
}

fn gated() -> RestTransportJsonUreq {
    rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate())
}

/// The demo host, from the release seed's rows.
fn demo() -> String {
    seed_endpoints(AccountTier::Demo).rest
}

fn rest_for(creds: &Credentials, properties: vike_model::SymbolProperties) -> PerpRest {
    VenueRestBybit {
        category: Category::Inverse,
        signer: BybitV5Signer::new(creds, now_ms),
        transport: gated(),
        base_url: demo(),
        mainnet: false,
        symbol: WIRE.to_string(),
        properties,
        leverage: 2.0,
    }
}

/// The venue's row for `order_client_id` in the inverse book: open orders first, then the history.
fn venue_row(rest: &PerpRest, order_client_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for path in [PATH_ORDER_REALTIME, PATH_ORDER_HISTORY] {
            let result = rest
                .call_public(
                    path,
                    &[
                        ("category", json!("inverse")),
                        ("symbol", json!(WIRE)),
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

fn text(row: &Value, key: &str) -> String {
    row[key].as_str().map_or_else(|| row[key].to_string(), str::to_string)
}

/// Puts the BTC collateral switch back OFF when this run found it off (the demo account is left as it was).
struct CollateralGuard<'a> {
    rest: &'a PerpRest,
    was_on: bool,
}

impl Drop for CollateralGuard<'_> {
    fn drop(&mut self) {
        if self.was_on {
            return;
        }
        let params = [("coin", json!(COIN)), ("collateralSwitch", json!("OFF"))];
        match self.rest.transport.signed(
            &self.rest.base_url,
            PATH_SET_COLLATERAL_SWITCH,
            "POST",
            &params,
            &self.rest.signer,
        ) {
            Ok(_) => {
                tracing::info!(target: "vike_bybit", "cleanup: {COIN} collateral switch put back OFF")
            }
            Err(e) => tracing::error!(
                target: "vike_bybit", error = %e.msg,
                "CLEANUP FAILED: switch {COIN} collateral OFF by hand on the demo account"
            ),
        }
    }
}

#[test]
#[ignore = "network + demo creds — places and cancels one real (non-filling) inverse demo limit — run manually (see module doc)"]
fn bybit_inverse_mount_rests_cancels_and_reads_back_in_the_coin() {
    vike_log::test_init();
    vike_bybit::history::kept_list_for_tests();
    let Some(creds) = load_demo_credentials() else { return };

    // --- 1. the mount's row, read the way the exec thread reads it -----------------------------
    let clock = vike_bridge_core::signer::VenueClock::default();
    let row = vike_bybit::execution::fetch_bybit_instrument(&creds, WIRE, &demo(), &clock)
        .expect("the exec grid fetch answers BTCUSD");
    println!(
        "FINDING bybit inverse: exec fetch contractType={:?} contract={:?}",
        row.contract_type, row.contract
    );
    assert_eq!(
        vike_bybit::execution::mount_money(SYMBOL, Some(&row)),
        Ok(Some((ContractValue::inverse(1.0), COIN.to_string()))),
        "BTCUSD mounts Inverse(1.0) in BTC"
    );
    let properties = row.properties;
    let rest = rest_for(&creds, properties);
    let recon = vike_bybit::recon_client(&creds, SYMBOL, &demo()).expect("a reconcile client");

    // --- the collateral switch, by the bridge's own rule ----------------------------------------
    let info = rest
        .call_public(PATH_COLLATERAL_INFO, &[("currency", json!(COIN))])
        .expect("collateral-info");
    let was_on = info["list"]
        .as_array()
        .and_then(|l| l.first())
        .and_then(|r| r["collateralSwitch"].as_bool())
        .unwrap_or(false);
    println!("FINDING bybit inverse: {COIN} collateral switch before the run: {was_on}");
    let _collateral = CollateralGuard { rest: &rest, was_on };
    Modes::new(BybitV5Signer::new(&creds, now_ms), gated(), demo(), false)
        .ensure(SYMBOL)
        .expect("the bridge's mode rule settles BTCUSD");

    // --- 2. the far limit, through the real builder ----------------------------------------------
    let last = rest.last_price().expect("tickers");
    assert!(last > 0.0, "a live last price is required");
    let tick = properties.tick_size;
    let price = (last * 0.6 / tick).round() * tick;
    let qty = f64::max(properties.min_qty, (properties.min_notional * 1.6).ceil());
    let qty = (qty / properties.step_size).ceil() * properties.step_size;
    let order_client_id = format!("vtinv{}", now_ms() % 100_000_000);
    let request: OrderRequest = serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "bybit", "symbol": SYMBOL, "side": 1, "qty": qty,
        "order_type": "limit", "price": price, "ts": now_ms()
    }))
    .unwrap();
    tracing::info!(target: "vike_bybit", order_client_id, last, price, qty, "placing a far inverse limit");
    let events = VenueRest::submit_order(&rest, &request);
    let cleanup = CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo" };
    let Some(Event::OrderAccepted(accepted)) = events.last() else {
        panic!("{order_client_id}: the demo must ACCEPT the inverse limit: {events:?}")
    };
    let venue_id = accepted.venue_order_id.clone().expect("the accept names the venue id");
    let row = venue_row(&rest, &order_client_id);
    println!("CAPTURED bybit inverse order: {row}");
    assert_eq!(text(&row, "symbol"), WIRE, "{row}");
    assert_eq!(text(&row, "qty").parse::<f64>().ok(), Some(qty), "whole USD contracts: {row}");
    assert!(
        (text(&row, "price").parse::<f64>().unwrap_or(f64::NAN) - price).abs() < tick / 2.0,
        "{row}"
    );

    // --- 3. the mount's reconcile client lists it, then the cancel ends it -----------------------
    let reports = recon.fetch_order_status_reports(0).expect("order/realtime");
    let report = reports
        .iter()
        .find(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str()))
        .unwrap_or_else(|| {
            panic!("{order_client_id}: the reconcile read must list it: {reports:?}")
        });
    assert!(report.status.is_live() && report.qty == qty, "{order_client_id}: {report:?}");
    rest.cancel_order(&order_client_id).expect("the cancel");
    let deadline = Instant::now() + Duration::from_secs(10);
    let row = loop {
        let row = venue_row(&rest, &order_client_id);
        if text(&row, "orderStatus") == "Cancelled" || Instant::now() > deadline {
            break row;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(text(&row, "orderStatus"), "Cancelled", "{order_client_id}: {row}");
    drop(cleanup);
    tracing::info!(target: "vike_bybit", order_client_id, venue_order_id = %venue_id, "placed -> listed -> cancelled");
    read_back(&creds);
    tracing::info!(target: "vike_bybit", "inverse smoke green");
}

/// Steps 4 and 5 alone, READ-ONLY (no order, no account setting): what the mount's reconcile client and its
/// funding poll read back. Run it by its name to re-measure without placing anything.
#[test]
#[ignore = "network + demo creds — read-only — run manually (see module doc)"]
fn bybit_inverse_mount_reads_back_in_the_coin() {
    vike_log::test_init();
    vike_bybit::history::kept_list_for_tests();
    let Some(creds) = load_demo_credentials() else { return };
    read_back(&creds);
    tracing::info!(target: "vike_bybit", "inverse read-back green");
}

/// What the fee-rate endpoint answers for each spelling of an inverse read, printed (`FINDING`), never asserted.
fn probe_fee_rate(rest: &PerpRest) {
    let spellings: [&[(&str, Value)]; 4] = [
        &[("category", json!("inverse")), ("symbol", json!(WIRE))],
        &[("category", json!("inverse"))],
        &[("category", json!("inverse")), ("baseCoin", json!(COIN))],
        &[("category", json!("linear")), ("symbol", json!(WIRE))],
    ];
    for params in spellings {
        let answer = rest.call_public("/v5/account/fee-rate", params);
        println!("FINDING bybit inverse fee-rate {params:?}: {answer:?}");
    }
}

fn read_back(creds: &Credentials) {
    let rest = rest_for(creds, vike_model::SymbolProperties::default());
    let recon = vike_bybit::recon_client(creds, SYMBOL, &demo()).expect("a reconcile client");
    // --- 4. position, wallet and fee, read back in the coin --------------------------------------
    let positions = recon.fetch_position_status_reports().expect("position/list");
    println!("FINDING bybit inverse: positions {positions:?}");
    assert!(
        positions.iter().any(|p| p.symbol == SYMBOL),
        "the BTCUSD.P row is present: {positions:?}"
    );
    let raw_position = rest
        .call_public(
            vike_bybit::execution::rest::PATH_POSITION_LIST,
            &[("category", json!("inverse")), ("symbol", json!(WIRE))],
        )
        .expect("position/list");
    println!("CAPTURED bybit inverse position/list: {raw_position}");

    let wallet = rest
        .call_public(PATH_ACCOUNT, &[("accountType", json!("UNIFIED")), ("coin", json!(COIN))])
        .expect("wallet-balance");
    println!("CAPTURED bybit inverse wallet-balance coin={COIN}: {wallet}");
    let balance = recon.fetch_balance().expect("the coin balance");
    let raw =
        vike_bybit::reconcile::parse_wallet_balance_of(&wallet.to_string(), COIN).expect("parses");
    println!(
        "FINDING bybit inverse: reconcile balance {balance:?} {COIN}, raw walletBalance {raw:?}"
    );
    assert!(balance.is_some(), "the coin wallet is reported");
    assert_eq!(balance, raw, "the reconcile balance IS the coin's walletBalance");

    probe_fee_rate(&rest);
    let schedule = recon.fetch_fee_rates();
    println!("FINDING bybit inverse: reconcile fee schedule {schedule:?}");

    let since = now_ms() - 6 * 24 * 3_600_000;
    let fills = recon.fetch_fill_reports(since).expect("execution/list");
    println!(
        "FINDING bybit inverse: {} fill(s) in 6 days; fee assets {:?}",
        fills.len(),
        fills.iter().map(|f| f.commission_asset.as_str()).collect::<Vec<_>>()
    );

    // --- 5. the funding poll of an inverse mount --------------------------------------------------
    let spellings: [&[(&str, Value)]; 2] = [
        &[("category", json!("inverse")), ("type", json!("SETTLEMENT")), ("currency", json!(COIN))],
        &[("type", json!("SETTLEMENT")), ("currency", json!(COIN))],
    ];
    for params in spellings {
        let log = rest.call_public(vike_bybit::execution::funding::LOG_PATH, params);
        println!("CAPTURED bybit inverse transaction-log {params:?}: {log:?}");
    }
    let funding = BybitFundingPoller::new(&rest, SYMBOL, 0).poll();
    println!(
        "FINDING bybit inverse: the inverse funding poll answered {:?}",
        funding.as_ref().map(Vec::len)
    );
    assert!(funding.is_ok(), "the inverse funding poll is answered: {funding:?}");
}
