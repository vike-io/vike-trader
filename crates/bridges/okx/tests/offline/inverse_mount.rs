//! **A coin-margined MOUNT** (decision 0258's okx amendment), offline: an engine mounted on `BTC-USD-SWAP`
//! (`ctType` `inverse`, `ctVal` 100 USD, settled in BTC) is a coin ledger whose quantity is the CONTRACT count.
//! The start builds its reconcile client with the factor 1.0 (`adapter.rs`'s `mount_parts`); that client
//! reports the settle coin's `cashBal`, the crypto-margined fee rate, and reads an admitted instrument only
//! when it is a coin-margined contract of the same coin, in contracts. The fill stream and the funding poll
//! already state the coin as the venue does. A linear mount's answers are unchanged.
//!
//! The instrument rows are the committed captures (`tests/fixtures/instruments/`, `tests/fixtures/classes/`'s
//! `inverse_swap`, the demo's of 2026-10-30), and the mount's resting order, the account's BTC and USDT balance
//! rows and the SWAP fee row are the demo's of 2026-11-08 (`demo_inverse_2026-11-08.json`, captured by
//! `tests/okx_smokes/okx_inverse_smoke.rs`); every other answer is SYNTHETIC in the documented shape of the
//! endpoint, said so beside it (the demo held no inverse position, fill or funding bill).
//!
//! Only public seams, so every test here also runs on the code before the amendment; where a test says "base",
//! it FAILED there (the assertion differs), it did not fail to compile.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_execution::recon::ReconcileClient;
use vike_model::FeeSchedule;
use vike_model::events::Event;
use vike_okx::ReconcileClientOkx;
use vike_okx::execution::funding::OkxFundingPoller;
use vike_okx::execution::rest::{PATH_BILLS, VenueRestOkx, parse_okx_perp_instruments};
use vike_okx::signer::OkxV5Signer;
use vike_secrets::credentials::Credentials;

const MOUNT: &str = "BTC-USD-SWAP";
const LINEAR: &str = "BTC-USDT-SWAP";

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

fn envelope(rows: Value) -> Value {
    json!({"code": "0", "msg": "", "data": rows})
}

/// The committed `public/instruments` row of `instrument`, from the swap and futures captures; an instrument they
/// do not hold is a synthetic row in their shape (`ETH-USD-SWAP`, another coin's inverse swap).
fn instrument_row(instrument: &str) -> Value {
    for page in [
        include_str!("../fixtures/instruments/swap.json"),
        include_str!("../fixtures/instruments/futures.json"),
    ] {
        let page: Value = serde_json::from_str(page).expect("a committed fixture");
        if let Some(row) = page["data"].as_array().and_then(|rows| {
            rows.iter().find(|r| r["instId"].as_str() == Some(instrument)).cloned()
        }) {
            return row;
        }
    }
    match instrument {
        "ETH-USD-SWAP" => json!({
            "instType": "SWAP", "instId": "ETH-USD-SWAP", "uly": "ETH-USD", "settleCcy": "ETH",
            "ctVal": "10", "ctMult": "1", "ctValCcy": "USD", "ctType": "inverse", "lever": "100",
            "tickSz": "0.01", "lotSz": "1", "minSz": "1", "maxMktSz": "10000", "state": "live"
        }),
        "ETH-USDT-SWAP" => json!({
            "instType": "SWAP", "instId": "ETH-USDT-SWAP", "uly": "ETH-USDT", "settleCcy": "USDT",
            "ctVal": "0.1", "ctMult": "1", "ctValCcy": "ETH", "ctType": "linear", "lever": "100",
            "tickSz": "0.01", "lotSz": "0.01", "minSz": "0.01", "maxMktSz": "15000", "state": "live"
        }),
        other => panic!("an instrument nobody scripted was asked for: {other}"),
    }
}

/// The demo's captured answers of 2026-11-08, by key.
fn captured(key: &str) -> Value {
    let all: Value =
        serde_json::from_str(include_str!("../fixtures/classes/demo_inverse_2026-11-08.json"))
            .expect("a committed fixture");
    all[key].clone()
}

/// Plays the venue: records every signed request and answers each path in its documented shape.
/// `demo_fee` answers the fee read with the demo's captured row instead of the synthetic one.
struct Venue {
    calls: Calls,
    demo_fee: bool,
}

impl RestTransportJson for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let tail = path.rsplit('/').next().unwrap_or(path).to_string();
        let map: serde_json::Map<String, Value> =
            params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        let instrument = map.get("instId").and_then(Value::as_str).unwrap_or("").to_string();
        self.calls.lock().unwrap().push((tail.clone(), Value::Object(map)));
        Ok(envelope(match tail.as_str() {
            // CAPTURED detail rows of the demo account (the answer's other currencies left out).
            "balance" => json!([{"details": [captured("balance_usdt"), captured("balance_btc")]}]),
            "trade-fee" if self.demo_fee => json!([captured("trade_fee_swap")]),
            // SYNTHETIC, the documented `account/trade-fee` row of `instType=SWAP`: `maker`/`taker` are the
            // crypto-margined contracts' rates, `makerU`/`takerU` the USDT-margined ones (told apart here; the
            // demo states both alike).
            "trade-fee" => json!([{"instType": "SWAP", "level": "Lv1",
                "maker": "-0.0001", "taker": "-0.0004", "makerU": "-0.0002", "takerU": "-0.0005"}]),
            // CAPTURED: the far limit of 0.1 contract that rested on the mount.
            "orders-pending" if instrument == MOUNT => json!([captured("order")]),
            // SYNTHETIC: one resting limit of 3 contracts on each other instrument read.
            "orders-pending" => json!([{
                "instId": instrument, "ordId": format!("o-{instrument}"), "clOrdId": format!("c-{instrument}"),
                "side": "buy", "ordType": "limit", "sz": "3", "accFillSz": "0", "px": "45000",
                "avgPx": "", "state": "live", "uTime": "5"
            }]),
            "orders-algo-pending" => json!([]),
            // SYNTHETIC: a long of 7 contracts on the mount, at its harmonic entry (an instrument-filtered read
            // of another instrument holds nothing).
            "positions" if instrument.is_empty() || instrument == MOUNT => json!([{
                "instId": MOUNT, "instType": "SWAP", "pos": "7", "posSide": "net", "avgPx": "76000.5",
                "mgnMode": "cross", "ccy": "BTC", "uTime": "8"
            }]),
            "positions" => json!([]),
            // SYNTHETIC, the documented funding bill (`type` 8) of the inverse swap, in its coin.
            "bills" => json!([{
                "billId": "b1", "type": "8", "instId": MOUNT, "ccy": "BTC",
                "pnl": "-0.00000012", "balChg": "-0.00000012", "ts": "1000"
            }]),
            other => panic!("not a read this suite plays: {other}"),
        }))
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        let instrument =
            params.iter().find(|(k, _)| *k == "instId").map(|(_, v)| v.clone()).unwrap();
        self.calls.lock().unwrap().push((
            path.rsplit('/').next().unwrap().to_string(),
            json!({"instId": instrument.clone()}),
        ));
        Ok(envelope(json!([instrument_row(&instrument)])))
    }
}

fn signer() -> OkxV5Signer {
    OkxV5Signer::new(&Credentials::for_test("k", "s", Some("p")), || 0)
}

/// The reconcile client a start builds: an inverse mount's factor is 1.0, a linear one's its `ctVal`.
fn recon(symbol: &str, factor: f64) -> (ReconcileClientOkx<Venue>, Calls) {
    reconcile_on(symbol, factor, false)
}

fn reconcile_on(symbol: &str, factor: f64, demo_fee: bool) -> (ReconcileClientOkx<Venue>, Calls) {
    let calls: Calls = Arc::default();
    let client = ReconcileClientOkx::new(
        signer(),
        Venue { calls: Arc::clone(&calls), demo_fee },
        "https://unused.invalid",
        symbol,
        factor,
    )
    .with_trailing(false);
    (client, calls)
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

/// The committed demo row of the mount is what the exec plane parses: a coin-margined SWAP of 100 USD a
/// contract, settled in BTC, on a grid of 0.1 contract. (Already true before the amendment: pinned because the
/// mount reads its value, coin and grid off exactly this row.)
#[test]
fn the_mounts_demo_row_is_a_coin_margined_swap_of_one_hundred_dollars() {
    let all: Value =
        serde_json::from_str(include_str!("../fixtures/classes/demo_instruments_2026-10-30.json"))
            .expect("a committed fixture");
    let rows = parse_okx_perp_instruments(&json!({ "data": [all["inverse_swap"].clone()] }));
    let row = &rows[MOUNT];
    assert_eq!(row.contract_type.as_deref(), Some("inverse"));
    assert_eq!(row.settle_ccy.as_deref(), Some("BTC"));
    assert_eq!((row.ct_val, row.ct_mult), (100.0, 1.0));
    assert_eq!((row.properties.step_size, row.properties.min_qty), (0.1, 0.1), "contracts");
}

/// **The mount's reconcile balance is its SETTLE COIN's cash** (decision 0258: the engine keeps its books in the
/// coin and adopts this figure): the client of `BTC-USD-SWAP` reports the demo account's BTC `cashBal`
/// (1.0000173807 BTC); a linear mount still reports USDT, from the same request.
///
/// Base: the inverse mount reported the USDT row's 4,997.34 as its "BTC".
#[test]
fn an_inverse_mount_reports_its_settle_coins_cash() {
    let (client, calls) = recon(MOUNT, 1.0);
    assert_eq!(client.fetch_balance(), Ok(Some(1.000_017_380_708_651_4)), "the BTC cashBal");
    let (linear, linear_calls) = recon(LINEAR, 0.01);
    assert_eq!(linear.fetch_balance(), Ok(Some(4_997.339_867_531_687)), "a linear mount: USDT");
    let asked = |c: &Calls| c.lock().unwrap().clone();
    assert_eq!(asked(&calls), asked(&linear_calls), "one request, the same for both");
}

/// The fee rate of an inverse mount is the crypto-margined one (`maker`/`taker` of the SWAP row); a linear
/// mount keeps the USDT-margined `makerU`/`takerU`, as before. The demo's own row states both alike, 2 and 5
/// bps, which the inverse mount reads.
///
/// Base: the inverse mount read the USDT-margined rates.
#[test]
fn an_inverse_mounts_fee_rate_is_the_crypto_margined_one() {
    let (client, _) = recon(MOUNT, 1.0);
    assert_eq!(
        client.fetch_fee_rates(),
        Ok(Some(FeeSchedule::from_fractions(0.0001, 0.0004))),
        "maker/taker"
    );
    let (linear, _) = recon(LINEAR, 0.01);
    assert_eq!(linear.fetch_fee_rates(), Ok(Some(FeeSchedule::from_fractions(0.0002, 0.0005))));
    let (demo, _) = reconcile_on(MOUNT, 1.0, true);
    assert_eq!(demo.fetch_fee_rates(), Ok(Some(FeeSchedule::from_fractions(0.0002, 0.0005))));
}

/// The mount's orders and position are CONTRACTS (the factor 1.0): the demo's far limit rests as 0.1 contract,
/// a long of 7 is 7. (Already true before the amendment for a client built with 1.0; pinned because the coin
/// ledger books exactly these.)
#[test]
fn an_inverse_mounts_orders_and_position_are_contracts() {
    let (client, _) = recon(MOUNT, 1.0);
    let orders = client.fetch_order_status_reports(0).expect("orders");
    let order = orders.iter().find(|o| o.symbol == MOUNT).expect("the resting limit");
    assert!(near(order.qty, 0.1) && order.status.is_live(), "{order:?}");
    assert_eq!(order.order_client_id.as_deref(), Some("vtoinv53559191"));
    let rows = client.fetch_position_status_reports().expect("positions");
    let row = rows.iter().find(|r| r.symbol == MOUNT).expect("the mount's row");
    assert!(near(row.qty, 7.0) && near(row.average_price, 76_000.5), "{row:?}");
}

/// **An admitted coin-margined contract of the SAME coin is read in contracts** (a dated BTC future on the
/// BTC swap engine): its order is 3 contracts, its contract size never scales it.
///
/// Base: the pass failed (`inverse_swap_refusal` refused the admitted inverse row), blinding the whole leg.
#[test]
fn an_inverse_mount_reads_an_admitted_contract_of_its_own_coin_in_contracts() {
    let (client, _) = recon(MOUNT, 1.0);
    client.admit_symbol("BTC-USD-261030");
    let orders = client.fetch_order_status_reports(0).expect("the same coin is read");
    let order = orders.iter().find(|o| o.symbol == "BTC-USD-261030").expect("its resting limit");
    assert!(near(order.qty, 3.0), "contracts, not x100: {order:?}");
}

/// **One settle coin of one kind per engine** (decision 0258, item 7): an admitted contract of ANOTHER coin
/// (`ETH-USD-SWAP`) or a LINEAR one (`ETH-USDT-SWAP`) fails the pass in words naming the engine's coin; the
/// door never admits either, so one is a venue change, not a size to guess.
///
/// Base: the linear one was read (scaled by its `ctVal`) into the coin engine's reports.
#[test]
fn an_inverse_mount_refuses_another_coin_or_a_linear_contract() {
    for other in ["ETH-USD-SWAP", "ETH-USDT-SWAP"] {
        let (client, _) = recon(MOUNT, 1.0);
        client.admit_symbol(other);
        let why = client.fetch_order_status_reports(0).expect_err(other);
        assert!(why.contains("keeps its books in BTC"), "{other}: {why}");
    }
}

/// A linear mount still refuses an admitted inverse contract (unchanged): its ledger is USDT.
#[test]
fn a_linear_mount_still_refuses_an_admitted_inverse_contract() {
    let (client, _) = recon(LINEAR, 0.01);
    client.admit_symbol(MOUNT);
    let why = client.fetch_order_status_reports(0).expect_err("refused");
    assert!(why.contains("inverse"), "{why}");
}

/// The private `orders` stream sizes a fill of the mount by the factor 1.0 and states its fee as the venue
/// does, in BTC (decision 0258 books a coin fee as stated). Pinned, not changed: the mapper always read
/// `fillFeeCcy`. SYNTHETIC frame, the documented `orders` push of a filled limit.
#[test]
fn a_fill_of_the_mount_is_contracts_with_its_fee_in_the_coin() {
    let frame = json!({"arg": {"channel": "orders", "instType": "SWAP"}, "data": [{
        "instType": "SWAP", "instId": MOUNT, "ordId": "o1", "clOrdId": "c1", "tradeId": "t1",
        "side": "buy", "posSide": "net", "ordType": "limit", "sz": "2", "px": "60000",
        "fillSz": "2", "fillPx": "60000", "accFillSz": "2", "avgPx": "60000",
        "fillFee": "-0.0000016667", "fillFeeCcy": "BTC", "execType": "M", "state": "filled",
        "fillTime": "1000", "uTime": "1000"
    }]});
    let events =
        vike_okx::execution::event_mapper::map_okx_perp_by_instrument(&frame, "okx", MOUNT, |_| {
            Some(1.0)
        });
    let fill = events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f.clone()),
            Event::OrderFilled(w) => Some(w.fill.clone()),
            _ => None,
        })
        .expect("a fill");
    assert!(near(fill.last_qty, 2.0), "two contracts: {fill:?}");
    assert!(near(fill.commission, 0.0000016667), "{fill:?}");
    assert_eq!(fill.commission_asset.as_str(), "BTC");
}

/// The funding poll of the mount asks the bills of `BTC-USD-SWAP` and states the coin amount as the venue
/// does (decision 0258 books it as stated on a coin ledger). Pinned, not changed.
#[test]
fn the_mounts_funding_is_its_bills_in_the_coin() {
    let calls: Calls = Arc::default();
    let rest = VenueRestOkx {
        signer: signer(),
        transport: Venue { calls: Arc::clone(&calls), demo_fee: false },
        base_url: "https://unused.invalid".to_string(),
        symbol: MOUNT.to_string(),
        properties: vike_model::SymbolProperties::default(),
        ct_val: 0.0,
        leverage: 2.0,
        broker_code: None,
    };
    let events = OkxFundingPoller::new(&rest, MOUNT, 0).poll().expect("the bills answered");
    let asked = calls.lock().unwrap().clone();
    assert_eq!(asked[0].0, PATH_BILLS.rsplit('/').next().unwrap());
    assert_eq!(asked[0].1["instId"], json!(MOUNT));
    match events.as_slice() {
        [Event::Funding(f)] => assert_eq!(f.amount, -0.00000012, "the coin amount as stated"),
        other => panic!("expected one funding event, got {other:?}"),
    }
}
