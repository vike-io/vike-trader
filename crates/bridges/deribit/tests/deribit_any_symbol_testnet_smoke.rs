//! Deribit ANY-SYMBOL LIVE TESTNET drill (`docs/ops/deribit-any-symbol-plan.md`): the new single-client
//! adapter (`DeribitRest::with_foreign_instruments`, `MoneyModel`, the settlement-currency scope of the
//! label lookup) against the real testnet, where the offline fake could only say what we believe.
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box feat/deribit-any-symbol env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-deribit --test deribit_any_symbol_testnet_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING, TESTNET ONLY. Every socket and every read is spelled `TESTNET_WS` / `TESTNET_REST`
//! (the adapter has no mainnet path) and [`only_testnet`] refuses to run on anything else. At most
//! [`MAX_ORDERS`] orders, three on the happy path (a fourth only if the T1 step's duplicate frame is
//! delivered after all): LIMIT BUYs of the minimum amount at 75% (else 70%) of the LOWEST of index, mark
//! and best ask, so none can cross. ⚠ The bridge writes `post_only: false` on every order by design (a
//! marketable limit must be able to take), so these are not venue-side post-only; their distance is
//! what keeps them from filling. No market order, no position, no fill: the end of the run asserts it.
//! Labels all start with [`PREFIX`]; a `Drop` guard cancels by label (OUR labels only, never
//! `cancel_all`), and a run starts by sweeping what a killed earlier run left.
//!
//! Double-gated like every `*_smoke.rs`: network + the default DEMO account's keys through
//! `vike_secrets::smoke_directory`; absent credentials print `SKIP` and return.
//!
//! What it proves, in order:
//!   1. an INVERSE mount (`BTC-PERPETUAL`) admits `ETH-PERPETUAL`, signs it on ITS grid (a price on the
//!      0.05 tick but not on the mounted 0.5 one, amount 1 where the mounted lot is 10), and the order
//!      reads back by label in the RIGHT currency (ETH), by id, and as `[]` in the mounted one (the
//!      measured trap behind the T1 rule);
//!   2. a LINEAR mount (`BTC_USDC-PERPETUAL`) does the same for `ETH_USDC-PERPETUAL` (USDC lookup);
//!   3. refusals by name, nothing written: linear on inverse, inverse on linear, a dated future, an option;
//!   4. `get_open_orders` with no currency lists both, `get_positions currency=any` is unchanged;
//!   5. the audit-T1 path: the session is logged out, so the next frame meets a dead socket (read-half
//!      failure = ambiguous); re-submitting the SAME label must come back ACCEPTED under the first
//!      order's venue id, i.e. the re-query asked the order's OWN currency (ETH) and a wrong-currency
//!      empty answer did not become a reject. Tolerant of the venue's timing: if the frame was delivered
//!      (a real duplicate) or the send failed definitely, it prints that and does not claim the proof;
//!   6. a cancel of an order this client never submitted: found in a currency the client knows, and (a
//!      fresh client that has resolved nothing) through the unfiltered open orders; an absent label is
//!      a no-op `Ok`;
//!   7. the end: zero resting orders under [`PREFIX`], positions as at the start.
//!
//! KILLED FROM OUTSIDE (SIGKILL, power loss) skips destructors: at most the resting orders above stay on
//! testnet, far from the market; the next run of this test sweeps them first, or cancel `n067dr*`.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_deribit::client::{DeribitRest, InstrumentRows, MoneyModel};
use vike_deribit::exec::{fetch_instrument_row, settlement_currency_of};
use vike_deribit::transport::{
    DeribitOrderTransport, TESTNET_REST, TESTNET_WS, is_dead_socket_error,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType, SymbolProperties};
use vike_secrets::credentials::api_credentials;

/// Every label of this drill starts with it (`n067dr01`...), and nothing else on the account does.
const PREFIX: &str = "n067dr";
/// The hard ceiling on orders that reached the venue in one run.
const MAX_ORDERS: usize = 6;
/// How far below the reference price a buy rests, nearest first (both >= 25% away).
const FAR: [f64; 2] = [0.75, 0.70];

const ETH_INVERSE: &str = "ETH-PERPETUAL";
const ETH_LINEAR: &str = "ETH_USDC-PERPETUAL";
const COID_ETH: &str = "n067dr01";
const COID_USDC: &str = "n067dr02";
const COID_RESTART: &str = "n067dr03";
const COID_ABSENT: &str = "n067dr98";
/// Refused orders: nothing may ever rest under these.
const REFUSED: [&str; 4] = ["n067dr91", "n067dr92", "n067dr93", "n067dr94"];

/// Refuses to go anywhere but the testnet. Every socket and read below goes through a spelled constant;
/// this is the second lock on the same door.
fn only_testnet() {
    assert!(
        TESTNET_WS == "wss://test.deribit.com/ws/api/v2"
            && TESTNET_REST == "https://test.deribit.com",
        "REFUSING TO RUN: the resolved endpoint is not the Deribit testnet ({TESTNET_WS}, {TESTNET_REST})"
    );
}

fn public(path: &str, params: &[(&str, String)]) -> Value {
    only_testnet();
    let r = UreqTransport::new("deribit").public(TESTNET_REST, path, params);
    r.unwrap_or_else(|e| panic!("testnet {path}: {} ({})", e.msg, e.code))["result"].clone()
}

fn err_s(e: &VenueApiError) -> String {
    format!("ERR code={} msg={}", e.code, e.msg)
}

/// One authed testnet socket for the READS and the cleanup (the clients under test own their own).
struct Conn {
    key: String,
    secret: String,
    t: DeribitOrderTransport,
}

impl Conn {
    fn open(key: &str, secret: &str) -> Result<Self, String> {
        only_testnet();
        let mut t = DeribitOrderTransport::new(TESTNET_WS, key, secret, None);
        t.connect().map_err(|e| e.0)?;
        Ok(Self { key: key.into(), secret: secret.into(), t })
    }

    fn send(&mut self, method: &str, params: &Value) -> Result<Value, VenueApiError> {
        let env = self.t.call(method, params)?;
        match env.get("error").filter(|e| !e.is_null()) {
            Some(e) => Err(VenueApiError {
                code: e["code"].as_i64().unwrap_or(-1),
                msg: e["message"].as_str().unwrap_or("").to_string(),
            }),
            None => Ok(env["result"].clone()),
        }
    }

    /// A read is re-dialled once on a dead socket; a write never is.
    fn rpc(&mut self, method: &str, params: &Value) -> Result<Value, VenueApiError> {
        match self.send(method, params) {
            Err(e) if method.starts_with("private/get_") && is_dead_socket_error(&e) => {
                println!("  (socket dead on {method}: {}; re-dialling once)", e.msg);
                *self = Self::open(&self.key.clone(), &self.secret.clone()).expect("re-dial");
                self.send(method, params)
            }
            r => r,
        }
    }

    fn array(&mut self, method: &str, params: &Value) -> Vec<Value> {
        let v = self.rpc(method, params).unwrap_or_else(|e| panic!("{method}: {}", err_s(&e)));
        v.as_array().cloned().unwrap_or_default()
    }

    /// The orders under `label` as the venue states them in `currency` (`[]` for a wrong currency).
    fn by_label(&mut self, currency: &str, label: &str) -> Vec<Value> {
        self.array(
            "private/get_order_state_by_label",
            &json!({ "currency": currency, "label": label }),
        )
    }

    /// Every open order of the account, ALL currencies, whose label starts with [`PREFIX`].
    fn open_ours(&mut self) -> Vec<Value> {
        let all = self.array("private/get_open_orders", &json!({}));
        all.into_iter()
            .filter(|o| o["label"].as_str().is_some_and(|l| l.starts_with(PREFIX)))
            .collect()
    }

    /// Non-zero positions, `currency=any`: instrument -> size.
    fn positions(&mut self) -> BTreeMap<String, f64> {
        self.array("private/get_positions", &json!({ "currency": "any" }))
            .iter()
            .filter(|p| p["size"].as_f64().is_some_and(|s| s != 0.0))
            .map(|p| {
                (
                    p["instrument_name"].as_str().unwrap_or("?").to_string(),
                    p["size"].as_f64().unwrap(),
                )
            })
            .collect()
    }
}

/// Cancels, by label, every label this drill may have used, then whatever else rests under [`PREFIX`]
/// (OUR labels only, never `cancel_all`). Used at the start (a killed run's residue) and as the `Drop`
/// guard, so a failed assertion or panic leaves nothing resting.
fn sweep(key: &str, secret: &str) {
    let mut c = match Conn::open(key, secret) {
        Ok(c) => c,
        Err(e) => {
            println!(
                "CLEANUP FAILED: no testnet socket ({e}); cancel `{PREFIX}*` on testnet by hand"
            );
            return;
        }
    };
    let mut labels: Vec<String> = [COID_ETH, COID_USDC, COID_RESTART, COID_ABSENT]
        .iter()
        .chain(REFUSED.iter())
        .map(|s| s.to_string())
        .collect();
    match c.rpc("private/get_open_orders", &json!({})) {
        Ok(v) => labels.extend(
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| o["label"].as_str())
                .filter(|l| l.starts_with(PREFIX))
                .map(String::from),
        ),
        Err(e) => println!(
            "CLEANUP: open-orders read failed ({}); cancelling the known labels",
            err_s(&e)
        ),
    }
    labels.sort();
    labels.dedup();
    for l in labels {
        match c.send("private/cancel_by_label", &json!({ "label": l })) {
            Ok(v) => {
                if v.as_i64() != Some(0) {
                    println!("cleanup cancel_by_label {l}: cancelled {v}");
                }
            }
            Err(e) => println!("CLEANUP cancel_by_label {l} FAILED: {}", err_s(&e)),
        }
    }
}

struct Guard {
    key: String,
    secret: String,
}

impl Drop for Guard {
    fn drop(&mut self) {
        sweep(&self.key, &self.secret);
    }
}

/// What an account is mounted on: the row, and what the exec thread's `run` derives from it.
struct Mount {
    symbol: String,
    props: SymbolProperties,
    currency: String,
    model: MoneyModel,
}

fn mount(symbol: &str) -> Mount {
    let row =
        fetch_instrument_row(symbol).unwrap_or_else(|| panic!("{symbol} is not on the testnet"));
    Mount {
        symbol: symbol.to_string(),
        props: vike_deribit::catalog::properties_of(&row).expect("the mounted grid"),
        currency: settlement_currency_of(&row).expect("the mounted settlement currency"),
        model: MoneyModel::of_perpetual_row(&row).expect("a perpetual mount"),
    }
}

/// A fresh client on its own authed testnet order socket, wired as `exec.rs`'s `run` wires it.
fn client(key: &str, secret: &str, m: &Mount) -> DeribitRest {
    only_testnet();
    let mut t = DeribitOrderTransport::new(TESTNET_WS, key, secret, None);
    t.connect().expect("testnet order-WS auth");
    let rows: InstrumentRows = Box::new(|name| {
        fetch_instrument_row(name)
            .ok_or_else(|| format!("`{name}` could not be read from the testnet"))
    });
    DeribitRest::new(t, &m.symbol, m.props, &m.currency)
        .with_foreign_instruments(m.model.clone(), rows)
}

fn limit(coid: &str, symbol: &str, qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "deribit".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(price),
        ts: vike_model::now_ms(),
        ..Default::default()
    }
}

/// `[OrderSubmitted, X]`; returns X.
fn outcome(events: Vec<Event>) -> Event {
    assert_eq!(events.len(), 2, "the emitter split is [OrderSubmitted, terminal-or-accepted]");
    events.into_iter().nth(1).expect("second event")
}

fn refusal(rest: &DeribitRest, coid: &str, symbol: &str, why: &str) {
    match outcome(rest.submit_order(&limit(coid, symbol, 1.0, 1.0))) {
        Event::OrderRejected(r) => {
            let reason = r.reason.to_string();
            assert!(reason.contains(why) && reason.contains(symbol), "{symbol}: {reason}");
            println!("  refused {symbol}: {reason}");
        }
        other => panic!("{symbol} must be refused by name, got {other:?}"),
    }
}

/// The lowest positive of index, mark and best ask: what a buy must sit far below.
fn reference_price(symbol: &str) -> f64 {
    let t = public("/api/v2/public/ticker", &[("instrument_name", symbol.to_string())]);
    ["index_price", "mark_price", "best_ask_price"]
        .iter()
        .filter_map(|k| t[*k].as_f64())
        .filter(|p| *p > 0.0)
        .fold(f64::INFINITY, f64::min)
}

fn on_grid(x: f64, step: f64) -> bool {
    ((x / step).round() * step - x).abs() < step / 100.0
}

struct Placed {
    coid: String,
    order_id: String,
    price: f64,
    tick: f64,
    qty: f64,
}

/// One far LIMIT BUY of `symbol`'s minimum amount through `rest`, priced on the order's OWN tick and
/// deliberately NOT on `mounted_tick` (so a wrong grid would show), 25-30% below the reference.
fn place(
    rest: &DeribitRest,
    placed: &mut usize,
    coid: &str,
    symbol: &str,
    mounted_tick: f64,
) -> Placed {
    let row = fetch_instrument_row(symbol).expect("the order's own row");
    let own = vike_deribit::catalog::properties_of(&row).expect("the order's own grid");
    let reference = reference_price(symbol);
    assert!(reference.is_finite() && reference > 0.0, "{symbol}: no usable reference price");
    for far in FAR {
        let mut price = (reference * far / own.tick_size).floor() * own.tick_size;
        if on_grid(price, mounted_tick) {
            price -= own.tick_size; // farther from the market, and off the mounted grid
        }
        price = (price * 1e8).round() / 1e8;
        assert!(price <= reference * 0.75 + 1e-9, "{symbol}: {price} is not 25% below {reference}");
        match outcome(rest.submit_order(&limit(coid, symbol, own.min_qty, price))) {
            Event::OrderAccepted(a) => {
                let id =
                    a.venue_order_id.as_ref().map(|v| v.as_str().to_string()).unwrap_or_default();
                assert!(!id.is_empty(), "{symbol}: accepted without a venue order id");
                *placed += 1;
                assert!(*placed <= MAX_ORDERS, "order budget exceeded");
                println!(
                    "  placed {coid} on {symbol}: order_id={id} price={price} (reference {reference}, tick {} vs mounted {mounted_tick}) amount={}",
                    own.tick_size, own.min_qty
                );
                return Placed {
                    coid: coid.into(),
                    order_id: id,
                    price,
                    tick: own.tick_size,
                    qty: own.min_qty,
                };
            }
            Event::OrderRejected(r) => {
                println!("  {symbol} at {far}*reference rejected: {}", r.reason)
            }
            other => panic!("{symbol}: unexpected {other:?}"),
        }
    }
    panic!("{symbol}: the venue took neither far price");
}

/// The venue's own account of one order, as the venue states it.
fn assert_resting(o: &Value, p: &Placed, symbol: &str, how: &str) {
    assert_eq!(o["order_id"].as_str(), Some(p.order_id.as_str()), "{how}: order id");
    assert_eq!(o["instrument_name"], symbol, "{how}: instrument");
    assert_eq!(o["label"], p.coid.as_str(), "{how}: label");
    assert_eq!(o["order_state"], "open", "{how}: state");
    assert!(
        (o["price"].as_f64().unwrap_or(0.0) - p.price).abs() < p.tick / 100.0,
        "{how}: signed on ITS grid: {o}"
    );
    assert!((o["amount"].as_f64().unwrap_or(0.0) - p.qty).abs() < 1e-12, "{how}: amount: {o}");
}

/// Reads one placed order back: by label in its own currency, `[]` in the wrong one, and by id.
fn read_back(c: &mut Conn, p: &Placed, symbol: &str, own_cur: &str, wrong_cur: &str) {
    let own = c.by_label(own_cur, &p.coid);
    assert_eq!(own.len(), 1, "{symbol}: by label in {own_cur}: {own:?}");
    assert_resting(&own[0], p, symbol, &format!("label/{own_cur}"));
    let wrong = c.by_label(wrong_cur, &p.coid);
    assert!(wrong.is_empty(), "{symbol}: the wrong currency {wrong_cur} must answer []: {wrong:?}");
    let by_id =
        c.rpc("private/get_order_state", &json!({ "order_id": p.order_id })).expect("by id");
    assert_resting(&by_id, p, symbol, "id");
    println!(
        "  read back {}: label/{own_cur} 1 order, label/{wrong_cur} [], by id; price {} amount {}",
        p.coid, p.price, p.qty
    );
}

fn assert_cancelled(c: &mut Conn, p: &Placed) {
    let o = c.rpc("private/get_order_state", &json!({ "order_id": p.order_id })).expect("by id");
    assert_eq!(o["order_state"], "cancelled", "{} must be cancelled: {o}", p.coid);
    println!("  {} cancelled at the venue ({})", p.coid, p.order_id);
}

#[test]
#[ignore = "network + testnet creds - places + cancels real (non-filling) testnet orders - run manually (see module doc)"]
fn deribit_any_symbol_testnet_drill() {
    vike_log::test_init();
    only_testnet();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("deribit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "deribit") else {
        println!("SKIP: DERIBIT_DEMO creds absent");
        return;
    };
    println!("TESTNET endpoint: {TESTNET_WS}");
    let (key, secret) = (creds.api_key.expose().to_string(), creds.api_secret.expose().to_string());
    sweep(&key, &secret);
    let _guard = Guard { key: key.clone(), secret: secret.clone() };
    let mut c = Conn::open(&key, &secret).expect("testnet order-WS auth");
    let mut placed_count = 0usize;

    let baseline = c.positions();
    println!("positions at the start (non-zero): {baseline:?}");
    assert!(c.open_ours().is_empty(), "the sweep left something under {PREFIX}");

    let inverse = mount("BTC-PERPETUAL");
    let linear = mount("BTC_USDC-PERPETUAL");
    assert_eq!((inverse.currency.as_str(), linear.currency.as_str()), ("BTC", "USDC"));
    let a = client(&key, &secret, &inverse);
    let b = client(&key, &secret, &linear);

    // ---- 1. inverse mount, another inverse perpetual ---------------------------------------------
    println!("\n===== 1. INVERSE mount BTC-PERPETUAL places {ETH_INVERSE} =====");
    let eth = place(&a, &mut placed_count, COID_ETH, ETH_INVERSE, inverse.props.tick_size);
    read_back(&mut c, &eth, ETH_INVERSE, "ETH", "BTC");

    // ---- 2. linear mount, another linear perpetual -----------------------------------------------
    println!("\n===== 2. LINEAR mount BTC_USDC-PERPETUAL places {ETH_LINEAR} =====");
    let usdc = place(&b, &mut placed_count, COID_USDC, ETH_LINEAR, linear.props.tick_size);
    read_back(&mut c, &usdc, ETH_LINEAR, "USDC", "BTC");

    // ---- 3. refusals by name, nothing written ----------------------------------------------------
    println!("\n===== 3. REFUSALS =====");
    refusal(&a, REFUSED[0], ETH_LINEAR, "LINEAR");
    refusal(&b, REFUSED[1], ETH_INVERSE, "INVERSE");
    let option = public(
        "/api/v2/public/get_instruments",
        &[("currency", "BTC".into()), ("kind", "option".into()), ("expired", "false".into())],
    );
    let option = option[0]["instrument_name"].as_str().expect("a listed BTC option").to_string();
    refusal(&a, REFUSED[2], &option, "not a perpetual");
    let futures = public(
        "/api/v2/public/get_instruments",
        &[("currency", "BTC".into()), ("kind", "future".into()), ("expired", "false".into())],
    );
    match futures.as_array().into_iter().flatten().find(|r| r["settlement_period"] != "perpetual") {
        Some(r) => {
            refusal(&a, REFUSED[3], r["instrument_name"].as_str().expect("name"), "not a perpetual")
        }
        None => println!(
            "  (no dated BTC future is listed on the testnet: that refusal is offline-only)"
        ),
    }

    // ---- 4. the account's view: open orders without a currency, positions currency=any -----------
    println!("\n===== 4. OPEN ORDERS (no currency) and POSITIONS (currency=any) =====");
    let open = c.open_ours();
    let mut seen: Vec<(String, String)> = open
        .iter()
        .map(|o| {
            (
                o["label"].as_str().unwrap_or("").into(),
                o["instrument_name"].as_str().unwrap_or("").into(),
            )
        })
        .collect();
    seen.sort();
    println!("  resting under {PREFIX}: {seen:?}");
    assert_eq!(
        seen,
        vec![(COID_ETH.into(), ETH_INVERSE.into()), (COID_USDC.into(), ETH_LINEAR.into())],
        "exactly the two placed orders rest, none of the refused ones"
    );
    assert_eq!(c.positions(), baseline, "a resting far order opens no position");

    // ---- 5. the audit-T1 path ----------------------------------------------------------------------
    println!(
        "\n===== 5. T1: the same label, a dead session, the re-query in the order's own currency ====="
    );
    let logout =
        a.transport.lock().unwrap().call("private/logout", &json!({ "invalidate_token": true }));
    println!(
        "  logout of the placing session: {}",
        logout.map_or_else(|e| err_s(&e), |_| "ok".into())
    );
    let again = outcome(a.submit_order(&limit(COID_ETH, ETH_INVERSE, eth.qty, eth.price)));
    let copies = c.by_label("ETH", COID_ETH).len();
    println!("  re-submit of {COID_ETH} -> {again:?}; orders under the label now: {copies}");
    if copies > 1 {
        placed_count += 1;
        assert!(placed_count <= MAX_ORDERS, "order budget exceeded");
        println!(
            "  the logout did not sever the session: the frame was delivered as a real second order; the T1 path was NOT exercised"
        );
        for o in c.by_label("ETH", COID_ETH) {
            if o["order_id"].as_str() != Some(eth.order_id.as_str()) {
                let id = o["order_id"].clone();
                let r = c.rpc("private/cancel", &json!({ "order_id": id }));
                println!("  duplicate {id} cancelled: {}", r.is_ok());
            }
        }
    } else {
        match &again {
            Event::OrderAccepted(acc) => {
                let id = acc.venue_order_id.as_ref().map(|v| v.as_str().to_string());
                assert_eq!(
                    id.as_deref(),
                    Some(eth.order_id.as_str()),
                    "the re-query must find the ETH order under its own venue id"
                );
                println!(
                    "  T1 PROVED: ambiguous submit -> label re-query in ETH -> adopted {}",
                    eth.order_id
                );
            }
            Event::OrderRejected(r) => {
                let why = r.reason.to_string();
                assert!(
                    !why.contains("venue confirms order absent"),
                    "a wrong-currency empty answer became a reject while the order rests: {why}"
                );
                println!("  the send failed definitely ({why}); the T1 path was NOT exercised");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // ---- 6. a cancel of an order this client never submitted ---------------------------------------
    println!("\n===== 6. CANCEL MISS =====");
    let a2 = client(&key, &secret, &inverse);
    let restart = place(&a2, &mut placed_count, COID_RESTART, ETH_INVERSE, inverse.props.tick_size);
    a2.cancel_order(COID_ETH).expect("cancel of an order the client never submitted");
    assert_cancelled(&mut c, &eth);
    println!("  A2 (knows BTC and ETH) cancelled {COID_ETH} it never submitted");
    let fresh = client(&key, &secret, &inverse);
    fresh.cancel_order(COID_RESTART).expect("cancel through the unfiltered open orders");
    assert_cancelled(&mut c, &restart);
    println!(
        "  a fresh client (knows BTC only) cancelled {COID_RESTART} through the unfiltered open orders"
    );
    fresh.cancel_order(COID_ABSENT).expect("an absent label is a no-op");
    println!("  an absent label is Ok(()), no order touched");
    b.cancel_order(COID_USDC).expect("cancel by the client's own map");
    assert_cancelled(&mut c, &usdc);

    // ---- 7. the end ---------------------------------------------------------------------------------
    println!("\n===== 7. END =====");
    let left = c.open_ours();
    assert!(left.is_empty(), "orders still rest under {PREFIX}: {left:?}");
    assert_eq!(c.positions(), baseline, "the drill opened a position");
    println!(
        "orders that reached the venue: {placed_count} (cap {MAX_ORDERS}); resting: 0; positions unchanged"
    );
    for rest in [&a, &a2, &b, &fresh] {
        rest.detach();
    }
}
