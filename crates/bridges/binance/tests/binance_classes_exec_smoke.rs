//! LIVE DEMO proof of the REAL client over the classes decision 0230 admits: network + demo creds, run by hand and
//! never in a loop, NOT in `.github/workflows/live-smokes.yml`:
//!
//!     cargo test -p vike-binance --test binance_classes_exec_smoke -- --ignored --nocapture --test-threads=1
//!
//! The client is the production `BinanceExecutionClient`, mounted on `ETHUSDT.P`; every order goes to ANOTHER
//! symbol through its per-symbol router (the any-symbol path), so the proof is of the code the node runs:
//!
//! * `binance_classes_a_tradfi_perpetual_the_account_never_traded_takes_its_first_order` -- `NVDAUSDT.P` (an equity
//!   perpetual, bracket maximum far below the venue's default leverage) reads leverage 0 on the account; the first
//!   order used to be refused `-2027`. The factory now writes the configured leverage first (DEMO host only): the
//!   order is ACCEPTED and the account then reads the written figure. Cancelled at the end.
//! * `binance_classes_a_dated_future_trades_through_the_real_client` -- `BTCUSDT_261225.P`: a far limit rests and
//!   cancels, a minimum market BUY fills and its `Fill` carries the label `BTCUSDT_261225.P` (the user-data stream
//!   mapper spells it, `perp_series_symbol`), the commission asset, then a reduce-only SELL flattens.
//!
//! **DEMO ONLY**: the client is built with `AccountTier::Demo` (asserted), the raw reads go to `DEMO_FAPI_REST`, the
//! account is the store's default demo account and the owner's fence `OFF_LIMITS` applies. A `Drop` guard cancels
//! what the drill rested and flattens what it opened, on the drill's own symbols only.

use std::time::{Duration, Instant};

use serde_json::Value;
use vike_binance::exec::BinanceExecutionClient;
use vike_binance::perp::DEMO_FAPI_REST;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, SymbolProperties, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

const OFF_LIMITS: [i64; 6] = [2, 8, 11, 12, 16, 18];
/// The symbol the client is mounted on; the drill never orders on it.
const MOUNTED: &str = "ETHUSDT";
const TRADFI: &str = "NVDAUSDT";
const DATED: &str = "BTCUSDT_261225";

fn finding(text: impl std::fmt::Display) {
    println!("FINDING: {text}");
}

struct Venue {
    transport: UreqTransport,
    signer: BinanceHmacSigner,
}

impl Venue {
    fn new(creds: &Credentials) -> Self {
        assert_eq!(DEMO_FAPI_REST, "https://demo-fapi.binance.com", "demo host constant moved");
        Self {
            transport: UreqTransport::new("binance"),
            signer: BinanceHmacSigner::new(creds, now_ms),
        }
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        self.transport.signed(DEMO_FAPI_REST, path, method, params, &self.signer)
    }

    fn row(&self, symbol: &str) -> Value {
        let info = self
            .transport
            .public(DEMO_FAPI_REST, "/fapi/v1/exchangeInfo", &[])
            .expect("exchangeInfo");
        info["symbols"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["symbol"] == symbol))
            .cloned()
            .unwrap_or_else(|| panic!("{symbol} is not in the demo list"))
    }

    fn bid(&self, symbol: &str) -> f64 {
        let t = self
            .transport
            .public(DEMO_FAPI_REST, "/fapi/v1/ticker/bookTicker", &[("symbol", symbol.to_string())])
            .expect("bookTicker");
        t["bidPrice"].as_str().and_then(|p| p.parse().ok()).expect("a bid")
    }

    /// `(leverage, positionAmt)` of `symbol` as `positionRisk` states them.
    fn risk(&self, symbol: &str) -> (f64, f64) {
        let rows = self
            .call("GET", "/fapi/v2/positionRisk", &[("symbol", symbol.to_string())])
            .expect("positionRisk");
        let row = rows.as_array().and_then(|r| r.first()).cloned().expect("a row");
        let num = |k: &str| row[k].as_str().and_then(|x| x.parse().ok()).unwrap_or(f64::NAN);
        (num("leverage"), num("positionAmt"))
    }
}

fn account() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        println!("SKIP: BINANCE_DEMO creds absent");
        return None;
    };
    let id = directory
        .account_row("binance", &AccountLabel::Default, AccountTier::Demo)
        .map(|row| row.id);
    println!("DRILL account id {id:?} (the default binance account at the demo tier)");
    if id.is_none_or(|id| OFF_LIMITS.contains(&id)) {
        println!("SKIP: the default binance demo account is off limits or unresolved ({id:?})");
        return None;
    }
    Some(creds)
}

/// Cancels what the drill rested and flattens what it opened, on its two symbols. Never panics.
struct LeaveFlat {
    creds: Credentials,
}

impl Drop for LeaveFlat {
    fn drop(&mut self) {
        let v = Venue::new(&self.creds);
        for symbol in [TRADFI, DATED] {
            let _ = v.call("DELETE", "/fapi/v1/allOpenOrders", &[("symbol", symbol.to_string())]);
            let amt = v
                .call("GET", "/fapi/v2/positionRisk", &[("symbol", symbol.to_string())])
                .ok()
                .and_then(|rows| {
                    rows.as_array()?
                        .iter()
                        .filter_map(|r| r["positionAmt"].as_str()?.parse::<f64>().ok())
                        .reduce(|a, b| a + b)
                });
            if let Some(amt) = amt.filter(|a| *a != 0.0) {
                let side = if amt > 0.0 { "SELL" } else { "BUY" };
                let r = v.call(
                    "POST",
                    "/fapi/v1/order",
                    &[
                        ("symbol", symbol.to_string()),
                        ("side", side.to_string()),
                        ("type", "MARKET".to_string()),
                        ("quantity", format!("{}", amt.abs())),
                        ("reduceOnly", "true".to_string()),
                    ],
                );
                println!("CLEANUP flatten {symbol} {amt}: {r:?}");
            }
        }
    }
}

fn filter_num(row: &Value, kind: &str, key: &str) -> f64 {
    row["filters"]
        .as_array()
        .and_then(|f| f.iter().find(|f| f["filterType"] == kind))
        .and_then(|f| f[key].as_str()?.parse().ok())
        .unwrap_or(0.0)
}

/// `x` rounded UP to a multiple of `step`, spelled with the step's digits.
fn ceil_to(x: f64, step: f64) -> f64 {
    let digits = format!("{step}").split('.').nth(1).map_or(0, str::len);
    format!("{:.*}", digits, (x / step - 1e-9).ceil() * step).parse().unwrap_or(x)
}

/// A far limit BUY on `symbol` (series label `<symbol>.P`): inside the PERCENT_PRICE band, far under the touch.
fn far_buy(v: &Venue, symbol: &str, coid: &str) -> OrderRequest {
    let row = v.row(symbol);
    let band = filter_num(&row, "PERCENT_PRICE", "multiplierDown").max(0.5);
    let tick = filter_num(&row, "PRICE_FILTER", "tickSize");
    let step = filter_num(&row, "LOT_SIZE", "stepSize");
    let price = ceil_to(v.bid(symbol) * (1.0 + band) / 2.0, tick);
    let qty = ceil_to(
        (filter_num(&row, "MIN_NOTIONAL", "notional").max(5.0) * 1.6 / price)
            .max(filter_num(&row, "LOT_SIZE", "minQty")),
        step,
    );
    serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": format!("{symbol}.P"),
        "side": 1, "qty": qty, "order_type": "limit", "price": price, "ts": now_ms()
    }))
    .expect("an order")
}

fn market(symbol: &str, coid: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": format!("{symbol}.P"),
        "side": side, "qty": qty, "order_type": "market", "reduce_only": reduce_only, "ts": now_ms()
    }))
    .expect("an order")
}

/// What `coid` became: drains the client's events until it reaches `want` (true), is rejected (false, with the
/// reason printed) or `secs` pass. A `Fill` is reported with its series label and fee.
fn wait_for(next: &mut Next, coid: &str, want: &str, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match next() {
            Some(Ingest::Event(e)) => {
                let (c, k, why) = match &e {
                    Event::OrderAccepted(x) => {
                        (x.client_order_id.clone(), "Accepted", String::new())
                    }
                    Event::OrderRejected(x) => {
                        (x.client_order_id.clone(), "Rejected", x.reason.to_string())
                    }
                    Event::OrderCanceled(x) => {
                        (x.client_order_id.clone(), "Canceled", String::new())
                    }
                    Event::Fill(f) => (
                        f.client_order_id.clone(),
                        "Filled",
                        format!(
                            "symbol={} qty={} px={} commission={} {} {:?}",
                            f.symbol,
                            f.last_qty,
                            f.last_px,
                            f.commission,
                            f.commission_asset,
                            f.liquidity_side
                        ),
                    ),
                    _ => continue,
                };
                println!("EVENT {c} -> {k} {why}");
                if c == coid && k == want {
                    return true;
                }
                if c == coid && k == "Rejected" {
                    return false;
                }
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    false
}

/// The client's next event, if one is waiting (the receiver hidden behind it).
type Next = dyn FnMut() -> Option<Ingest>;

fn client(creds: &Credentials) -> (BinanceExecutionClient, Box<Next>) {
    let (events, mut rx) = event_channel(256);
    let client = BinanceExecutionClient::spawn_with_recorder(
        AccountTier::Demo,
        creds.clone(),
        format!("{MOUNTED}.P"),
        SymbolProperties::default(),
        events,
        None,
        None,
        None,
        2.0,
        false,
        std::sync::Arc::default(),
    );
    std::thread::sleep(Duration::from_secs(4));
    (client, Box::new(move || rx.try_recv().ok()))
}

#[test]
#[ignore = "network + DEMO creds: rests and cancels one order, may write a leverage - run by hand"]
fn binance_classes_a_tradfi_perpetual_the_account_never_traded_takes_its_first_order() {
    vike_log::test_init();
    let Some(creds) = account() else { return };
    let v = Venue::new(&creds);
    let _guard = LeaveFlat { creds: creds.clone() };
    let (leverage, held) = v.risk(TRADFI);
    finding(format!("{TRADFI} before: leverage={leverage} position={held}"));
    if leverage >= 1.0 || held != 0.0 {
        println!(
            "SKIP: {TRADFI} is no longer a never-traded symbol (leverage {leverage}, position {held})"
        );
        return;
    }
    let (mut client, mut rx) = client(&creds);
    let coid = format!("vtcls{}", now_ms() % 100_000_000);
    client.submit(&far_buy(&v, TRADFI, &coid));
    let accepted = wait_for(&mut rx, &coid, "Accepted", 25);
    let (after, _) = v.risk(TRADFI);
    finding(format!("{TRADFI} first order accepted={accepted}; leverage after={after}"));
    client.cancel(&coid);
    let cancelled = wait_for(&mut rx, &coid, "Canceled", 15);
    finding(format!("{TRADFI} cancelled={cancelled}"));
    client.detach();
    assert!(accepted, "the first order on a never-traded TradFi perpetual was not accepted");
    assert!(after >= 1.0, "the bridge wrote no leverage ({after})");
    assert!(cancelled, "the resting order was not cancelled");
}

#[test]
#[ignore = "network + DEMO creds: a minimum fill on the quarterly, flattened - run by hand"]
fn binance_classes_a_dated_future_trades_through_the_real_client() {
    vike_log::test_init();
    let Some(creds) = account() else { return };
    let v = Venue::new(&creds);
    let _guard = LeaveFlat { creds: creds.clone() };
    let (mut client, mut rx) = client(&creds);
    // 1. a far limit rests and cancels
    let rest = format!("vtdat{}", now_ms() % 100_000_000);
    client.submit(&far_buy(&v, DATED, &rest));
    assert!(wait_for(&mut rx, &rest, "Accepted", 25), "{rest} was not accepted");
    client.cancel(&rest);
    assert!(wait_for(&mut rx, &rest, "Canceled", 15), "{rest} was not cancelled");
    // 2. a minimum market BUY fills, labelled `<exchange symbol>.P`
    let row = v.row(DATED);
    let step = filter_num(&row, "LOT_SIZE", "stepSize");
    let ask_cover = v.bid(DATED);
    let qty = ceil_to(
        (filter_num(&row, "MIN_NOTIONAL", "notional").max(5.0) * 1.6 / ask_cover)
            .max(filter_num(&row, "LOT_SIZE", "minQty")),
        step,
    );
    let open = format!("vtdatO{}", now_ms() % 100_000_000);
    client.submit(&market(DATED, &open, 1, qty, false));
    let filled = wait_for(&mut rx, &open, "Filled", 25);
    finding(format!("{DATED}.P market BUY {qty}: a Fill event arrived={filled}"));
    let held = v.risk(DATED).1;
    finding(format!("{DATED} position after the open: {held}"));
    // 3. the reduce-only close
    let close = format!("vtdatC{}", now_ms() % 100_000_000);
    client.submit(&market(DATED, &close, -1, held.abs(), true));
    let closed = wait_for(&mut rx, &close, "Filled", 25);
    finding(format!(
        "{DATED}.P reduce-only SELL: a Fill event arrived={closed}; position now {}",
        v.risk(DATED).1
    ));
    client.detach();
    assert!(
        filled && closed,
        "the dated future's fills did not reach the client under its `.P` label"
    );
    assert_eq!(v.risk(DATED).1, 0.0, "the drill left a position");
}
