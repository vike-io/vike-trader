//! Deribit CLASS MEASUREMENT on the TESTNET (decision 0233, A64): what the venue itself says about the classes the door
//! refuses by name (a dated future, an option, a spot pair) and about the money one account holds in several currencies.
//! It drives no production client: it asks the venue, so the answers can be captured as fixtures.
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-deribit --test deribit_classes_testnet_smoke -- --ignored --nocapture
//!
//! It rests far POST-ONLY limits (cancelled) and then trades the minimum-size round trips (a marketable IOC buy and
//! the sale back on a dated linear future, a dated inverse future, the perpetuals, a USDC option, a coin option and
//! the BTC_USDC spot pair: they FILL on the testnet and end flat, and a `Drop` guard flattens what the run added).
//! TESTNET ONLY: [`only_testnet`] refuses any other endpoint. Labels all start with [`PREFIX`]; the cleanup cancels
//! by label (OUR labels only, never `cancel_all`).
//!
//! Every answer worth a fixture is printed as `CAPTURED <what> <json>` (one line each). Double-gated like every
//! `*_smoke.rs`: absent demo credentials print `SKIP` and return.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_deribit::transport::{
    DeribitOrderTransport, TESTNET_REST, TESTNET_WS, is_dead_socket_error,
};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::api_credentials;

const PREFIX: &str = "n099dc";

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

fn captured(what: &str, v: &Value) {
    println!("CAPTURED {what} {v}");
}

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
                msg: format!("{} {}", e["message"].as_str().unwrap_or(""), e["data"]),
            }),
            None => Ok(env["result"].clone()),
        }
    }

    /// A read is re-dialled once on a dead socket; a write never is.
    fn rpc(&mut self, method: &str, params: &Value) -> Result<Value, VenueApiError> {
        match self.send(method, params) {
            Err(e) if method.starts_with("private/get_") && is_dead_socket_error(&e) => {
                *self = Self::open(&self.key.clone(), &self.secret.clone()).expect("re-dial");
                self.send(method, params)
            }
            r => r,
        }
    }

    /// A read that is allowed to fail: prints the answer or the venue's error as a finding.
    fn probe(&mut self, what: &str, method: &str, params: Value) -> Option<Value> {
        match self.rpc(method, &params) {
            Ok(v) => {
                captured(what, &v);
                Some(v)
            }
            Err(e) => {
                println!("FINDING {what}: {method} {params} answered error {} {}", e.code, e.msg);
                None
            }
        }
    }

    /// Non-zero positions as a SIGNED ORDER AMOUNT: MEASURED 2026-10-30, a LINEAR future's `size` is its
    /// QUOTE notional (0.0001 BTC of `BTC_USDC-3NOV26` read `size` 7.678599) and `size_currency` the base
    /// amount an order counts, while an inverse future's `size` is the order's US dollars and an option's
    /// the base coin.
    fn positions(&mut self) -> BTreeMap<String, f64> {
        let v =
            self.rpc("private/get_positions", &json!({ "currency": "any" })).unwrap_or_default();
        v.as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["size"].as_f64().is_some_and(|s| s != 0.0))
            .map(|p| {
                let name = p["instrument_name"].as_str().unwrap_or("?");
                let size = p["size"].as_f64().unwrap();
                let amount = if p["kind"] == "future" && name.contains('_') {
                    p["size_currency"].as_f64().unwrap_or(0.0).abs() * size.signum()
                } else {
                    size
                };
                (name.to_string(), amount)
            })
            .collect()
    }
}

/// Cancels every resting order under [`PREFIX`] (by label, ours only) and closes every position the run left
/// (a reduce-only market order per instrument not in `baseline`).
fn cleanup(key: &str, secret: &str, baseline: &BTreeMap<String, f64>) {
    let Ok(mut c) = Conn::open(key, secret) else {
        println!(
            "CLEANUP FAILED: no testnet socket; cancel `{PREFIX}*` on testnet and flatten by hand"
        );
        return;
    };
    let open = c.rpc("private/get_open_orders", &json!({})).unwrap_or_default();
    let mut labels: Vec<String> = open
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["label"].as_str())
        .filter(|l| l.starts_with(PREFIX))
        .map(String::from)
        .collect();
    labels.sort();
    labels.dedup();
    for l in labels {
        if let Err(e) = c.send("private/cancel_by_label", &json!({ "label": l })) {
            println!("CLEANUP cancel_by_label {l} FAILED: {} {}", e.code, e.msg);
        }
    }
    for (name, size) in c.positions() {
        let was = baseline.get(&name).copied().unwrap_or(0.0);
        if (size - was).abs() < 1e-12 {
            continue;
        }
        let (side, amount) =
            if size > was { ("private/sell", size - was) } else { ("private/buy", was - size) };
        let amount = (amount * 1e8).round() / 1e8;
        let r = c.send(side, &json!({ "instrument_name": name, "amount": amount, "type": "market", "reduce_only": true, "label": format!("{PREFIX}fl") }));
        println!("CLEANUP flatten {name} {side} {amount}: {r:?}");
    }
}

struct Guard {
    key: String,
    secret: String,
    baseline: BTreeMap<String, f64>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        cleanup(&self.key, &self.secret, &self.baseline);
    }
}

/// The tick in force at `price` on a row with tiered `tick_size_steps` (the venue's own rule).
fn tick_at(row: &Value, price: f64) -> f64 {
    let mut tick = row["tick_size"].as_f64().unwrap_or(0.0);
    for s in row["tick_size_steps"].as_array().into_iter().flatten() {
        if price >= s["above_price"].as_f64().unwrap_or(f64::INFINITY) {
            tick = s["tick_size"].as_f64().unwrap_or(tick);
        }
    }
    tick
}

fn snap(row: &Value, price: f64, up: bool) -> f64 {
    let tick = tick_at(row, price);
    let n = price / tick;
    let n = if up { n.ceil() } else { n.floor() };
    ((n.max(1.0) * tick) * 1e10).round() / 1e10
}

fn book(name: &str) -> (Option<f64>, Option<f64>, Value) {
    let b = public(
        "/api/v2/public/get_order_book",
        &[("instrument_name", name.into()), ("depth", "1".into())],
    );
    (
        b["best_bid_price"].as_f64().filter(|p| *p > 0.0),
        b["best_ask_price"].as_f64().filter(|p| *p > 0.0),
        b,
    )
}

/// A SHORT round trip (a marketable IOC sale, the position read raw, a reduce-only IOC buy): the sign of
/// `size` and `size_currency` on a short is what a reconcile of a LINEAR contract must read.
fn short_trip(c: &mut Conn, row: &Value, name: &str, i: usize, amount: f64, bid: f64, ask: f64) {
    let sell_px = snap(row, bid * 0.99, false);
    let sold = c.send("private/sell", &json!({ "instrument_name": name, "amount": amount, "type": "limit", "price": sell_px, "time_in_force": "immediate_or_cancel", "label": format!("{PREFIX}{i}t") }));
    match sold {
        Ok(s) => captured(&format!("short trip SELL {name}"), &s),
        Err(e) => {
            println!(
                "FINDING short trip {name}: the opening sale was refused: {} {}",
                e.code, e.msg
            );
            return;
        }
    }
    c.probe(
        &format!("short position {name}"),
        "private/get_position",
        json!({ "instrument_name": name }),
    );
    c.probe(
        &format!("short positions any {name}"),
        "private/get_positions",
        json!({ "currency": "any", "kind": "future" }),
    );
    let held = c.positions().get(name).copied().unwrap_or(0.0);
    let buy_px = snap(row, ask * 1.01, true);
    let closed = c.send("private/buy", &json!({ "instrument_name": name, "amount": held.abs().max(amount), "type": "limit", "price": buy_px, "time_in_force": "immediate_or_cancel", "reduce_only": true, "label": format!("{PREFIX}{i}u") }));
    match closed {
        Ok(s) => captured(&format!("short trip BUY {name}"), &s),
        Err(e) => {
            println!("FINDING short trip {name}: the closing buy was refused: {} {}", e.code, e.msg)
        }
    }
    c.probe(
        &format!("short trip end {name}"),
        "private/get_position",
        json!({ "instrument_name": name }),
    );
}

/// One instrument of each class to measure: (what, instrument name).
fn pick() -> Vec<(&'static str, String)> {
    let now = vike_model::now_ms();
    let list = |cur: &str, kind: &str| -> Vec<Value> {
        public(
            "/api/v2/public/get_instruments",
            &[("currency", cur.into()), ("kind", kind.into()), ("expired", "false".into())],
        )
        .as_array()
        .cloned()
        .unwrap_or_default()
    };
    // a WEEKLY (or later) future, so no settlement lands inside the run
    let later =
        |r: &&Value| r["expiration_timestamp"].as_i64().unwrap_or(0) > now + 3 * 24 * 3_600_000;
    let mut out = Vec::new();
    let pick_future = |rows: &[Value], base: &str| -> Option<String> {
        rows.iter()
            .filter(|r| r["settlement_period"] != "perpetual" && r["base_currency"] == base)
            .find(later)
            .and_then(|r| r["instrument_name"].as_str().map(String::from))
    };
    if let Some(n) = pick_future(&list("BTC", "future"), "BTC") {
        out.push(("inverse dated future", n));
    }
    if let Some(n) = pick_future(&list("USDC", "future"), "BTC") {
        out.push(("linear dated future", n));
    }
    // an option 4 to 14 days out whose strike sits nearest the index
    let index = public("/api/v2/public/get_index_price", &[("index_name", "btc_usd".into())])["index_price"]
        .as_f64()
        .unwrap_or(0.0);
    let near = |rows: &[Value]| -> Option<String> {
        rows.iter()
            .filter(|r| {
                let e = r["expiration_timestamp"].as_i64().unwrap_or(0);
                r["base_currency"] == "BTC"
                    && r["option_type"] == "call"
                    && e > now + 4 * 24 * 3_600_000
                    && e < now + 14 * 24 * 3_600_000
            })
            .min_by(|a, b| {
                let d = |r: &Value| (r["strike"].as_f64().unwrap_or(0.0) - index).abs();
                d(a).total_cmp(&d(b))
            })
            .and_then(|r| r["instrument_name"].as_str().map(String::from))
    };
    if let Some(n) = near(&list("BTC", "option")) {
        out.push(("coin option", n));
    }
    if let Some(n) = near(&list("USDC", "option")) {
        out.push(("usdc option", n));
    }
    out.push(("spot", "BTC_USDC".to_string()));
    out.push(("inverse perpetual", "BTC-PERPETUAL".to_string()));
    out.push(("linear perpetual", "BTC_USDC-PERPETUAL".to_string()));
    // the linear USDC classes that are not a crypto coin (decision 0233): an equity and a commodity perpetual,
    // and a volatility-index future (a different lot and tick); each only if the testnet lists it
    let usdc = list("USDC", "future");
    for (what, want) in [
        ("equity perpetual", "AAPL_USDC-PERPETUAL"),
        ("commodity perpetual", "GOLD_USDC-PERPETUAL"),
    ] {
        if usdc.iter().any(|r| r["instrument_name"] == want) {
            out.push((what, want.to_string()));
        }
    }
    if let Some(n) = pick_future(&usdc, "BTCDVOL") {
        out.push(("volatility future", n));
    }
    out
}

#[test]
#[ignore = "network + testnet creds - places + cancels real testnet orders, FILL=1 trades - run manually (see module doc)"]
fn deribit_classes_on_the_testnet() {
    vike_log::test_init();
    only_testnet();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("deribit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "deribit") else {
        println!("SKIP: DERIBIT_DEMO creds absent");
        return;
    };
    let (key, secret) = (creds.api_key.expose().to_string(), creds.api_secret.expose().to_string());
    let mut c = Conn::open(&key, &secret).expect("testnet order-WS auth");
    let baseline = c.positions();
    println!("positions at the start (non-zero): {baseline:?}");
    let _guard = Guard { key: key.clone(), secret: secret.clone(), baseline: baseline.clone() };
    cleanup(&key, &secret, &baseline);

    println!("\n===== 1. the account, per currency =====");
    for cur in ["BTC", "ETH", "USDC", "USDT", "EURR", "SOL"] {
        c.probe(
            &format!("account_summary {cur}"),
            "private/get_account_summary",
            json!({ "currency": cur, "extended": true }),
        );
    }
    c.probe("subaccounts", "private/get_subaccounts", json!({ "with_portfolio": false }));
    c.probe("account_summaries", "private/get_account_summaries", json!({ "extended": true }));
    for (cur, kind) in [("BTC", "spot"), ("USDC", "spot"), ("any", "option"), ("any", "future")] {
        c.probe(
            &format!("positions {cur} {kind}"),
            "private/get_positions",
            json!({ "currency": cur, "kind": kind }),
        );
    }
    for cur in ["BTC", "USDC"] {
        c.probe(
            &format!("settlement_history {cur}"),
            "private/get_settlement_history_by_currency",
            json!({ "currency": cur, "type": "settlement", "count": 3 }),
        );
    }

    let classes = pick();
    println!("\n===== 2. the instruments: {classes:?} =====");
    let mut placed: Vec<(String, String, String)> = Vec::new(); // (what, instrument, order id)
    for (i, (what, name)) in classes.iter().enumerate() {
        let row = public("/api/v2/public/get_instrument", &[("instrument_name", name.clone())]);
        captured(&format!("get_instrument {name}"), &row);
        c.probe(
            &format!("get_contract_size {name}"),
            "public/get_contract_size",
            json!({ "instrument_name": name }),
        );
        let (bid, ask, b) = book(name);
        captured(
            &format!("order_book {name}"),
            &json!({ "best_bid": b["best_bid_price"], "best_ask": b["best_ask_price"], "mark": b["mark_price"], "index": b["index_price"], "underlying": b["underlying_price"], "bid_iv": b["bid_iv"], "ask_iv": b["ask_iv"], "mark_iv": b["mark_iv"], "greeks": b["greeks"], "stats": b["stats"] }),
        );
        let reference = [b["mark_price"].as_f64(), ask, bid]
            .into_iter()
            .flatten()
            .filter(|p| *p > 0.0)
            .fold(f64::INFINITY, f64::min);
        if !reference.is_finite() {
            println!(
                "FINDING {what} {name}: no usable reference price (book empty), nothing placed"
            );
            continue;
        }
        let amount = row["min_trade_amount"].as_f64().expect("a minimum amount");
        // a far POST-ONLY buy: half the lowest of mark, ask and bid, on the tick in force at THAT price
        let far = snap(&row, reference * 0.5, false);
        let label = format!("{PREFIX}{i}f");
        c.probe(
            &format!("get_margins {name}"),
            "private/get_margins",
            json!({ "instrument_name": name, "amount": amount, "price": far }),
        );
        match c.send("private/buy", &json!({ "instrument_name": name, "amount": amount, "type": "limit", "price": far, "post_only": true, "label": label })) {
            Ok(r) => {
                captured(&format!("buy far limit {name}"), &r);
                let id = r["order"]["order_id"].as_str().unwrap_or_default().to_string();
                c.probe(&format!("order_margin {name}"), "private/get_order_margin_by_ids", json!({ "ids": [id] }));
                c.probe(&format!("order_state {name}"), "private/get_order_state", json!({ "order_id": id }));
                placed.push((what.to_string(), name.clone(), id));
            }
            Err(e) => println!("FINDING {what} {name}: the far post-only buy of {amount} at {far} was refused: {} {}", e.code, e.msg),
        }
        // a price OFF the grid (one tenth of a tick above the snapped price) must be refused or snapped: what does the venue say
        let off = far + tick_at(&row, far) / 10.0;
        match c.send("private/buy", &json!({ "instrument_name": name, "amount": amount, "type": "limit", "price": off, "post_only": true, "label": format!("{PREFIX}{i}g") })) {
            Ok(r) => captured(&format!("buy off-grid price {name} (asked {off})"), &r["order"]),
            Err(e) => println!("FINDING {what} {name}: an off-grid price {off} was refused: {} {}", e.code, e.msg),
        }
        // an amount BELOW the minimum
        match c.send("private/buy", &json!({ "instrument_name": name, "amount": amount / 10.0, "type": "limit", "price": far, "post_only": true, "label": format!("{PREFIX}{i}h") })) {
            Ok(r) => captured(&format!("buy sub-minimum amount {name}"), &r["order"]),
            Err(e) => println!("FINDING {what} {name}: a sub-minimum amount {} was refused: {} {}", amount / 10.0, e.code, e.msg),
        }
        // a SELL to open a short option or future: the far post-only sell, twice the highest of mark and ask
        let sell_px = snap(
            &row,
            [b["mark_price"].as_f64(), ask].into_iter().flatten().fold(0.0, f64::max) * 2.0,
            true,
        );
        match c.send("private/sell", &json!({ "instrument_name": name, "amount": amount, "type": "limit", "price": sell_px, "post_only": true, "label": format!("{PREFIX}{i}s") })) {
            Ok(r) => {
                captured(&format!("sell far limit {name}"), &r["order"]);
                let id = r["order"]["order_id"].as_str().unwrap_or_default().to_string();
                c.probe(&format!("order_margin sell {name}"), "private/get_order_margin_by_ids", json!({ "ids": [id] }));
            }
            Err(e) => println!("FINDING {what} {name}: the far post-only SELL {amount} at {sell_px} was refused: {} {}", e.code, e.msg),
        }
        // reduce_only on an order that opens: what the venue does with it on each class
        match c.send("private/buy", &json!({ "instrument_name": name, "amount": amount, "type": "limit", "price": far, "post_only": true, "reduce_only": true, "label": format!("{PREFIX}{i}r") })) {
            Ok(r) => captured(&format!("buy reduce_only with no position {name}"), &r["order"]),
            Err(e) => println!("FINDING {what} {name}: reduce_only with no position was refused: {} {}", e.code, e.msg),
        }
        // the SELL twin: a reduce-only sale with no position is what an option long-only rule would have to rely on
        match c.send("private/sell", &json!({ "instrument_name": name, "amount": amount, "type": "limit", "price": sell_px, "post_only": true, "reduce_only": true, "label": format!("{PREFIX}{i}q") })) {
            Ok(r) => captured(&format!("sell reduce_only with no position {name}"), &r["order"]),
            Err(e) => println!("FINDING {what} {name}: a reduce-only SELL with no position was refused: {} {}", e.code, e.msg),
        }
    }
    cleanup(&key, &secret, &baseline);
    for (what, name, id) in &placed {
        if let Some(o) = c.probe(
            &format!("order_state after cancel {name}"),
            "private/get_order_state",
            json!({ "order_id": id }),
        ) {
            println!(
                "FINDING {what} {name}: after the cleanup the far order is {}",
                o["order_state"]
            );
        }
    }

    {
        println!("\n===== 3. round trips =====");
        for (i, (what, name)) in classes.iter().enumerate() {
            let row = public("/api/v2/public/get_instrument", &[("instrument_name", name.clone())]);
            let amount = row["min_trade_amount"].as_f64().expect("a minimum amount");
            let (bid, ask, _) = book(name);
            let (Some(bid), Some(ask)) = (bid, ask) else {
                println!("FINDING {what} {name}: a one-sided or empty book, no round trip");
                continue;
            };
            let buy_px = snap(&row, ask * 1.01, true);
            let before = c.positions();
            match c.send("private/buy", &json!({ "instrument_name": name, "amount": amount, "type": "limit", "price": buy_px, "time_in_force": "immediate_or_cancel", "label": format!("{PREFIX}{i}b") })) {
                Ok(r) => {
                    captured(&format!("round trip BUY {name}"), &r);
                    c.probe(&format!("position {name}"), "private/get_position", json!({ "instrument_name": name }));
                    c.probe(&format!("account_summary after buy {name}"), "private/get_account_summary", json!({ "currency": row["settlement_currency"].as_str().unwrap_or(row["quote_currency"].as_str().unwrap_or("BTC")), "extended": true }));
                    let held = c.positions().get(name).copied().unwrap_or(0.0) - before.get(name).copied().unwrap_or(0.0);
                    if held.abs() > 0.0 {
                        // close what the buy took: a reduce-only IOC sale well below the bid (spot has no position: sell what was bought)
                        let sell_px = snap(&row, bid * 0.99, false);
                        let qty = if *what == "spot" { r["trades"].as_array().map(|t| t.iter().map(|x| x["amount"].as_f64().unwrap_or(0.0)).sum::<f64>()).unwrap_or(amount) } else { held.abs() };
                        if what.contains("option") {
                            // a reduce-only sale of TWICE the position: does the venue cut it to the position or refuse it
                            let over = c.send("private/sell", &json!({ "instrument_name": name, "amount": qty * 2.0, "type": "limit", "price": sell_px, "time_in_force": "immediate_or_cancel", "reduce_only": true, "label": format!("{PREFIX}{i}o") }));
                            captured(&format!("option over-size reduce-only SELL {name}"), &over.unwrap_or_else(|e| json!({ "error": format!("{} {}", e.code, e.msg) })));
                        }
                        let sold = c.send("private/sell", &json!({ "instrument_name": name, "amount": qty, "type": "limit", "price": sell_px, "time_in_force": "immediate_or_cancel", "reduce_only": *what != "spot", "label": format!("{PREFIX}{i}c") }));
                        match sold {
                            Ok(s) => captured(&format!("round trip SELL {name}"), &s),
                            Err(e) => println!("FINDING {what} {name}: the closing sale was refused: {} {}", e.code, e.msg),
                        }
                    } else if *what == "spot" {
                        // a spot IOC answers before its fill is booked: read what it took from the account's trades
                        let bought = c.probe(&format!("spot trades {name}"), "private/get_user_trades_by_instrument", json!({ "instrument_name": name, "count": 5 }));
                        let qty = bought.map(|b| b["trades"].as_array().map(|t| t.iter().filter(|x| x["label"] == format!("{PREFIX}{i}b").as_str() && x["direction"] == "buy").map(|x| x["amount"].as_f64().unwrap_or(0.0)).sum::<f64>()).unwrap_or(0.0)).unwrap_or(0.0);
                        if qty > 0.0 {
                            let sell_px = snap(&row, bid * 0.99, false);
                            let sold = c.send("private/sell", &json!({ "instrument_name": name, "amount": qty, "type": "limit", "price": sell_px, "time_in_force": "immediate_or_cancel", "label": format!("{PREFIX}{i}c") }));
                            captured(&format!("round trip SELL {name}"), &sold.unwrap_or_else(|e| json!({ "error": format!("{} {}", e.code, e.msg) })));
                        }
                    }
                    if what.starts_with("linear") || what.starts_with("inverse perp") {
                        short_trip(&mut c, &row, name, i, amount, bid, ask);
                    }
                    c.probe(&format!("user trades {name}"), "private/get_user_trades_by_instrument", json!({ "instrument_name": name, "count": 6 }));
                    c.probe(&format!("position after {name}"), "private/get_position", json!({ "instrument_name": name }));
                }
                Err(e) => println!("FINDING {what} {name}: the marketable IOC buy at {buy_px} was refused: {} {}", e.code, e.msg),
            }
        }
    }

    cleanup(&key, &secret, &baseline);
    let end = c.positions();
    println!("positions at the end (non-zero): {end:?}");
    for cur in ["BTC", "ETH", "USDC"] {
        c.probe(
            &format!("account_summary end {cur}"),
            "private/get_account_summary",
            json!({ "currency": cur, "extended": true }),
        );
    }
}
