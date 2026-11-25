//! **The binance COIN-M mount demo smoke** (decision 0258's binance amendment): the coin-margined `BTCUSD_PERP`
//! mounted the way `crates/bridges/binance/src/execution/mod.rs`'s `run` mounts it signs on the DEMO `dapi` host, on the
//! row's own grid, in whole contracts, rests and cancels ONE order, and the mount's reconcile client reads the
//! position, the BTC wallet and the fee rate back in the coin.
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture --test-threads=1 binance_coin_m_smoke::
//!
//! The account is the store's default binance account at tier `demo`, and it must be account row 3; the host is
//! the DEMO `dapi` one only. Two tests:
//!
//! * `binance_coin_m_mount_reads_back_in_the_coin` (READ-ONLY, filter `binance_coin_m_smoke::binance_coin_m_mount_reads`):
//!   1. `vike_binance::execution::rest::coin_m::fetch_mount` answers `BTCUSD_PERP.P` on the demo network as `Inverse(100)` settled in
//!      BTC, on a whole-contract grid;
//!   2. the mount's reconcile client (`vike_binance::reconcile::recon_client` of `BTCUSD_PERP.P`) reads the BTC
//!      wallet (equal to the raw `/dapi/v1/balance` row), the position row and the commission rate in the COIN-M
//!      book; each raw answer is a `CAPTURED` line (fixtures for the offline tests);
//!   3. the COIN-M funding read (`/dapi/v1/income?incomeType=FUNDING_FEE`) is answered.
//! * `binance_coin_m_mount_rests_and_cancels_one_order` (ORDER-PLACING): ONE limit BUY of one contract (100 US
//!   dollars) at 92% of the mark (inside the row's PERCENT_PRICE band of 0.9), which cannot fill in the seconds it
//!   rests, through the production exec client (`vike_binance::execution::spawn_with_recorder`): ACCEPTED, the venue's own row echoes one contract, the
//!   reconcile client lists it live, the cancel ends it. The client's start makes the writes every demo perp start
//!   makes: the COIN-M position mode and margin type are put right when they are not one-way and cross, and the
//!   leverage 2 is posted. Nothing touches a position. ⚠ It places NOTHING (a `SKIP:` line) while the COIN-M BTC
//!   wallet holds less than the contract's margin: the venue refuses such an order `-2019` (MEASURED 2026-11-08 on
//!   account row 3: the order reached `dapi`, signed on the row's grid, and was judged on margin).
//!
//! Only `coin-m smoke green` is the order proof; a `SKIP:` line is not. GREEN on account row 3, 2026-11-08, once the
//! owner had funded the COIN-M wallets by hand (BTC 0.01; the demo hosts have no transfer API): venue order id
//! 4608218148, one contract at 69,588.7, accepted, listed, cancelled; the read-only test after it read the position
//! flat, the wallet 0.01 BTC, the commission 2 / 4 bps and an empty funding read.
//!
//! CLEANUP: a `CancelOnDrop` guard cancels the order client id on every exit path. A run killed from outside leaves the order
//! resting: cancel the order client id the log names (`vtcm…`) by hand on the demo COIN-M account.

use std::time::{Duration, Instant};

use crate::demo_credentials::demo_hosts;
use serde_json::Value;
use vike_binance::execution::rest::coin_m::{CoinMMount, RestTransportDapiPaths, fetch_mount};
use vike_binance::execution::rest::perp::VenueRestBinancePerp;
use vike_binance::execution::spawn_with_recorder;
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::smoke::CancelOnDrop;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_execution::recon::ReconcileClient;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, NotionalKind, OrderRequest, SymbolProperties, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

const WIRE: &str = "BTCUSD_PERP";
const SERIES: &str = "BTCUSD_PERP.P";
/// The one account this smoke may trade (the brief's): the default binance account at tier `demo`.
const ACCOUNT_ID: i64 = 3;
/// The leverage the client's start posts, so one contract's initial margin is `100 / LEVERAGE / mark` BTC.
const LEVERAGE: f64 = 2.0;

type Rest = VenueRestBinancePerp<BinanceHmacSigner, RestTransportDapiPaths<UreqTransport>>;

fn finding(text: impl std::fmt::Display) {
    println!("FINDING: {text}");
}

fn captured(what: &str, body: &Value) {
    println!("CAPTURED {what}: {body}");
}

/// The default binance demo account's key pair, only when it is account row [`ACCOUNT_ID`].
fn account() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let row =
        directory.account_row("binance", &AccountLabel::Default, AccountTier::Demo).map(|r| r.id);
    println!("ACCOUNT id {row:?} (the default binance account at the demo tier)");
    if row != Some(ACCOUNT_ID) {
        println!("SKIP: the default binance demo account is not row {ACCOUNT_ID} ({row:?})");
        return None;
    }
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "binance");
    if creds.is_none() {
        println!("SKIP: BINANCE_DEMO creds absent");
    }
    creds
}

/// A raw client of the mount on the demo `dapi` host (captures and the cleanup cancel), of the COIN-M book.
fn raw(creds: &Credentials, properties: SymbolProperties) -> Rest {
    let hosts = demo_hosts();
    VenueRestBinancePerp {
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport: RestTransportDapiPaths(
            UreqTransport::new("binance"),
            vike_model::Margining::Inverse,
        ),
        base_url: hosts.dapi_rest.to_string(),
        ty: hosts.ty,
        margining: vike_binance::execution::rest::coin_m::margining_of(WIRE),
        symbol: WIRE.to_string(),
        properties,
        leverage: LEVERAGE,
        link_id: None,
        algo_order_client_ids: Default::default(),
    }
}

fn get(rest: &Rest, path: &str, params: &[(&str, String)]) -> Value {
    rest.transport
        .signed(&rest.base_url, path, "GET", params, &rest.signer)
        .unwrap_or_else(|e| panic!("GET {path}: {e}"))
}

/// The mount's own row on the demo network, asserted to be BTCUSD_PERP's `Inverse(100)` in BTC.
fn mount() -> CoinMMount {
    let mount =
        fetch_mount(demo_hosts().dapi_rest, SERIES).expect("the demo dapi list values BTCUSD_PERP");
    finding(format!(
        "mount: {:?} settled in {}, grid {:?}",
        mount.value, mount.coin, mount.properties
    ));
    assert_eq!(
        (mount.value.kind, mount.value.per_unit, mount.coin.as_str()),
        (NotionalKind::Inverse, 100.0, "BTC")
    );
    mount
}

/// The BTC wallet as the mount's reconcile client reports it, held equal to the raw `/dapi/v1/balance` row.
fn btc_wallet(recon: &dyn ReconcileClient, rest: &Rest) -> Option<f64> {
    let wallet = recon.fetch_balance();
    let balance = get(rest, "/dapi/v1/balance", &[]);
    captured("GET /dapi/v1/balance", &balance);
    let raw_btc = balance.as_array().and_then(|rows| {
        rows.iter()
            .find(|r| r["asset"] == "BTC")
            .and_then(|r| r["balance"].as_str()?.parse::<f64>().ok())
    });
    finding(format!("BTC wallet: reconcile {wallet:?}, raw {raw_btc:?}"));
    assert_eq!(wallet.clone().ok().flatten(), raw_btc, "the reconcile wallet is the raw BTC row");
    raw_btc
}

/// The demo mark of the mounted contract (`/dapi/v1/premiumIndex` answers a one-row array).
fn mark(rest: &Rest) -> f64 {
    rest.transport
        .public(&rest.base_url, "/dapi/v1/premiumIndex", &[("symbol", WIRE.to_string())])
        .ok()
        .and_then(|v| v.as_array().and_then(|r| r.first().cloned()).or(Some(v)))
        .and_then(|v| v["markPrice"].as_str().and_then(|p| p.parse().ok()))
        .expect("the demo mark price")
}

/// The client's events until `order_client_id` reaches `want` (`Some(want)`), is rejected (`Some("Rejected: ..")`) or `secs`
/// pass (`None`).
fn wait_for(
    next: &mut dyn FnMut() -> Option<Ingest>,
    order_client_id: &str,
    want: &str,
    secs: u64,
) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match next() {
            Some(Ingest::Event(e)) => {
                let (c, kind, why) = match &e {
                    Event::OrderAccepted(a) => {
                        (a.order_client_id.clone(), "Accepted", String::new())
                    }
                    Event::OrderCanceled(a) => {
                        (a.order_client_id.clone(), "Canceled", String::new())
                    }
                    Event::OrderRejected(a) => {
                        (a.order_client_id.clone(), "Rejected", a.reason.to_string())
                    }
                    _ => continue,
                };
                println!("EVENT {c} -> {kind} {why}");
                if c == order_client_id && (kind == want || kind == "Rejected") {
                    return Some(if kind == want {
                        want.to_string()
                    } else {
                        format!("Rejected: {why}")
                    });
                }
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    None
}

#[test]
#[ignore = "network + DEMO creds (account row 3): READ-ONLY COIN-M reads - run by hand"]
fn binance_coin_m_mount_reads_back_in_the_coin() {
    vike_log::test_init();
    let Some(creds) = account() else { return };
    let mount = mount();
    let rest = raw(&creds, mount.properties);
    let recon =
        vike_binance::reconcile::recon_client(&creds, SERIES, demo_hosts()).expect("pure to build");
    btc_wallet(recon.as_ref(), &rest);
    let risk = get(&rest, "/dapi/v1/positionRisk", &[("pair", "BTCUSD".to_string())]);
    captured("GET /dapi/v1/positionRisk?pair=BTCUSD", &risk);
    let positions = recon.fetch_position_status_reports().expect("the position read");
    finding(format!("positions: {positions:?}"));
    assert!(positions.iter().all(|p| p.symbol == SERIES), "one symbol's rows, on the series label");
    let fee = get(&rest, "/dapi/v1/commissionRate", &[("symbol", WIRE.to_string())]);
    captured("GET /dapi/v1/commissionRate", &fee);
    finding(format!("fee rates: {:?}", recon.fetch_fee_rates()));
    let mut funding =
        vike_binance::execution::funding::income_poller(&rest, now_ms() - 7 * 24 * 3_600_000);
    let polled = funding.poll(now_ms());
    finding(format!("funding read (seven days of BTC FUNDING_FEE): {polled:?}"));
    assert!(polled.is_ok(), "the income history answers");
    println!("coin-m reads green");
}

#[test]
#[ignore = "network + DEMO creds (account row 3): rests and cancels ONE COIN-M order - run by hand"]
fn binance_coin_m_mount_rests_and_cancels_one_order() {
    vike_log::test_init();
    let Some(creds) = account() else { return };
    let mount = mount();
    let rest = raw(&creds, mount.properties);
    let recon =
        vike_binance::reconcile::recon_client(&creds, SERIES, demo_hosts()).expect("pure to build");
    let mark = mark(&rest);
    // One contract's initial margin at the posted leverage, doubled for the fee and the mark's move.
    let needed = 2.0 * mount.value.per_unit / LEVERAGE / mark;
    let held = btc_wallet(recon.as_ref(), &rest).unwrap_or(0.0);
    if held < needed {
        println!(
            "SKIP: the COIN-M demo wallet holds {held} BTC and one contract needs about {needed:.8}: the venue \
             would refuse the order -2019, so none is placed (fund the COIN-M BTC wallet of account row 3)"
        );
        return;
    }
    let tick = mount.properties.tick_size;
    let price = ((mark * 0.92) / tick).floor() * tick;
    let order_client_id = format!("vtcm{}", now_ms() % 100_000_000);
    finding(format!("mark {mark}, far limit BUY 1 contract at {price} ({order_client_id})"));
    let request: OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id.clone(), "venue": "binance", "symbol": SERIES, "side": 1, "qty": 1.0,
        "order_type": "limit", "price": price, "time_in_force": "GTC", "ts": now_ms(),
    }))
    .expect("an order");
    let (events, mut rx) = event_channel(256);
    let mut client = spawn_with_recorder(
        demo_hosts(),
        creds.clone(),
        SERIES.to_string(),
        events,
        None,
        None,
        None,
        LEVERAGE,
        false,
        std::sync::Arc::default(),
        vike_bridge_core::signer::VenueClock::default(),
    );
    std::thread::sleep(Duration::from_secs(5));
    let _guard =
        CancelOnDrop { rest: &rest, order_client_id: &order_client_id, host: "demo COIN-M" };
    let mut next = || rx.try_recv().ok();
    client.submit(&request);
    let placed = wait_for(&mut next, &order_client_id, "Accepted", 25);
    finding(format!("submit: {placed:?}"));
    if placed.as_deref() != Some("Accepted") {
        client.detach();
        panic!("the far limit was not accepted: {placed:?}");
    }
    let echo = get(
        &rest,
        "/dapi/v1/order",
        &[("symbol", WIRE.to_string()), ("origClientOrderId", order_client_id.clone())],
    );
    captured("GET /dapi/v1/order", &echo);
    assert_eq!(echo["origQty"].as_str(), Some("1"), "one whole contract");
    let open = recon.fetch_order_status_reports(0).expect("the open-order read");
    let listed =
        open.iter().any(|r| r.order_client_id.as_deref() == Some(order_client_id.as_str()));
    finding(format!("reconcile lists the order live: {listed}"));
    client.cancel(&order_client_id);
    let cancelled = wait_for(&mut next, &order_client_id, "Canceled", 20);
    finding(format!("cancel: {cancelled:?}"));
    client.detach();
    assert!(listed, "the reconcile client did not list the resting order");
    assert_eq!(cancelled.as_deref(), Some("Canceled"), "the resting order was not cancelled");
    println!("coin-m smoke green");
}
