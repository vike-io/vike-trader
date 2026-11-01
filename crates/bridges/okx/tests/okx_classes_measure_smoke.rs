//! **THE OKX INSTRUMENT-CLASS MEASUREMENT** (decision 0232): what the DEMO (simulated trading) account answers to
//! an order on each class the bridge refuses by name. Network + the default DEMO account's key trio, every test
//! `#[ignore]`d and self-skipping, run by hand on a the CI box lane (the credentials are that box's):
//!
//!     cargo test -p vike-okx --test okx_classes_measure_smoke -- --ignored --nocapture <test name>
//!
//! ⚠ DEMO ONLY: the transport is `UreqOkxTransport::new(true)` (the `x-simulated-trading: 1` header is the whole
//! tier on this venue); run it under `VIKE_NO_LIVE_KEYS=true` like every smoke of this crate. Orders are minimum
//! size or far limits, and each leg flattens what it opened (a spot leg leaves the base coin's dust below one
//! lot). Every answer is printed as one `MEASURED` line: the raw venue words are the point, nothing is asserted
//! beyond "the account was reachable". An order that is refused is a MEASUREMENT, not a failure.
//!
//! What each test answers (the questions the per-class admission cannot guess):
//!
//! * `okx_classes_account_read` (read-only): `acctLv`, `posMode`, the coin balances the account holds, the
//!   fee rates per `instType`, every open position.
//! * `okx_classes_reads_per_class`: the reads the reconcile client and the history replay make, on a spot id,
//!   a swap, a dated future and an inverse swap: which answer, with which `instType`.
//! * `okx_classes_spot_wire`: a far limit on `BTC-USDT` under `tdMode` `cross` with and without `posSide` and
//!   `reduceOnly` (round 1 measured: `cash` is refused with "Parameter tdMode error" on a level-3 account,
//!   `cross` is accepted); a far spot stop on the algo endpoint; a market BUY with no `tgtCcy` (is `sz` the
//!   quote amount?) and with `tgtCcy=base_ccy`; the fill's `fee`, `feeCcy`, `fillSz`, `accFillSz`; the SELL
//!   that returns the coin.
//! * `okx_classes_futures_wire`: a dated USDT-linear future (the first of QTUM, EDGE1, GRASS this account may
//!   trade): leverage write, a far limit, a market round trip, the position row (`instType=FUTURES`), the fee.
//! * `okx_classes_inverse_and_option_wire`: an inverse swap (`BTC-USD-SWAP`): a far limit and a market round
//!   trip with the USDT and BTC `cashBal` before and after (where does the PnL and the fee settle?); a BTC-USD
//!   option (`BTC-USD-261106-76000-C`): a far limit and its echo (the premium's unit).
//! * `okx_classes_orders_channel_any`: the private `orders` channel subscribed with `instType` ANY (one
//!   subscription for spot, swap and futures) while a spot market order round-trips: is the subscription
//!   accepted, and what do the frames carry?

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::user_data::{OpenOutcome, StreamError, StreamMsg, UserStream};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, now_ms};
use vike_okx::perp::{
    DEMO_WS, PATH_ACCOUNT, PATH_BILLS, PATH_INSTRUMENTS, PATH_ORDER_CANCEL, PATH_ORDER_CREATE,
    PATH_POSITIONS, PATH_SET_LEVERAGE, PATH_TICKER, REST,
};
use vike_okx::transport::{OkxTransport, UreqOkxTransport};
use vike_secrets::credentials::{Credentials, api_credentials};

const PATH_ACCOUNT_CONFIG: &str = "/api/v5/account/config";
const PATH_TRADE_FEE: &str = "/api/v5/account/trade-fee";

const SPOT: &str = "BTC-USDT";
/// The demo's three USDT-linear dated futures (measured 2026-10-30: EDGE1, GRASS, QTUM); GRASS answered
/// `51155` ("local compliance restrictions") to the first round, so the leg walks the list.
const FUTURE_CANDIDATES: [&str; 3] = ["QTUM-USDT-261225", "EDGE1-USDT-261225", "GRASS-USDT-261225"];
const INVERSE: &str = "BTC-USD-SWAP";
const OPTION: &str = "BTC-USD-261106-76000-C";

fn creds() -> Option<Credentials> {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "okx");
    if creds.is_none() {
        println!("SKIP: OKX demo credentials absent");
    }
    creds
}

/// One demo REST session: the DEMO transport (never `new(false)`), the signer, a coid counter.
struct Probe {
    transport: UreqOkxTransport,
    signer: OkxV5Signer,
    seq: u32,
}

impl Probe {
    fn new(creds: &Credentials) -> Self {
        Self {
            transport: UreqOkxTransport::new(true)
                .with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
            signer: OkxV5Signer::new(creds, now_ms),
            seq: 0,
        }
    }

    /// A fresh client order id (alphanumeric, at most 32 characters).
    fn coid(&mut self, tag: &str) -> String {
        self.seq += 1;
        format!("okxcls{tag}{}{}", self.seq, now_ms() % 1_000_000_000)
    }

    /// The raw envelope of a signed call, or `{"transport_error": ...}`: printed, never unwrapped.
    fn call(&self, label: &str, method: &str, path: &str, params: &[(&str, Value)]) -> Value {
        let out = match self.transport.signed(REST, path, method, params, &self.signer) {
            Ok(body) => body,
            Err(e) => json!({"transport_error": {"code": e.code, "msg": e.msg}}),
        };
        println!("MEASURED {label}: {out}");
        out
    }

    /// The first `data` row of a public read (demo header on).
    fn public_row(&self, path: &str, params: &[(&str, String)]) -> Value {
        match self.transport.public(REST, path, params) {
            Ok(body) => body["data"].get(0).cloned().unwrap_or(Value::Null),
            Err(e) => json!({"transport_error": {"code": e.code, "msg": e.msg}}),
        }
    }

    fn instrument(&self, inst_type: &str, inst_id: &str) -> Value {
        let mut params: Vec<(&str, String)> =
            vec![("instType", inst_type.into()), ("instId", inst_id.into())];
        if inst_type == "OPTION" {
            // OPTION refuses a bare listing: the family (`BTC-USD`) is the first two segments
            params.push(("instFamily", inst_id.split('-').take(2).collect::<Vec<_>>().join("-")));
        }
        let row = self.public_row(PATH_INSTRUMENTS, &params);
        println!(
            "MEASURED instrument {inst_id}: {}",
            json!({
                "instType": row["instType"], "state": row["state"], "ctType": row["ctType"],
                "ctVal": row["ctVal"], "ctMult": row["ctMult"], "ctValCcy": row["ctValCcy"],
                "settleCcy": row["settleCcy"], "baseCcy": row["baseCcy"], "quoteCcy": row["quoteCcy"],
                "tickSz": row["tickSz"], "lotSz": row["lotSz"], "minSz": row["minSz"],
                "maxMktSz": row["maxMktSz"], "lever": row["lever"], "expTime": row["expTime"],
            })
        );
        row
    }

    /// The demo's last price of `inst_id` (the demo book is not the public one: read it, never assume).
    fn last(&self, inst_id: &str) -> f64 {
        self.public_row(PATH_TICKER, &[("instId", inst_id.into())])["last"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0)
    }

    fn num(row: &Value, key: &str) -> f64 {
        row[key].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0)
    }

    /// `POST /trade/order`; the coid is the order's handle for `order_row` and `cancel`.
    fn order(&self, label: &str, params: &[(&str, Value)]) -> Value {
        self.call(label, "POST", PATH_ORDER_CREATE, params)
    }

    /// The order's own row (`GET /trade/order` by `clOrdId`), polled until it is `filled`/`canceled` (at most
    /// `wait`): the fill's `fee`, `feeCcy`, `fillSz` and `accFillSz` are read from it.
    fn order_row(&self, inst_id: &str, coid: &str, wait: Duration) -> Value {
        let deadline = Instant::now() + wait;
        loop {
            let body = match self.transport.signed(
                REST,
                PATH_ORDER_CREATE,
                "GET",
                &[("instId", json!(inst_id)), ("clOrdId", json!(coid))],
                &self.signer,
            ) {
                Ok(b) => b,
                Err(e) => json!({"transport_error": {"code": e.code, "msg": e.msg}}),
            };
            let row = body["data"].get(0).cloned().unwrap_or(body);
            let state = row["state"].as_str().unwrap_or("");
            if matches!(state, "filled" | "canceled" | "mmp_canceled") || Instant::now() >= deadline
            {
                println!(
                    "MEASURED order row {coid}: {}",
                    json!({
                        "instId": row["instId"], "instType": row["instType"], "state": state,
                        "ordType": row["ordType"], "side": row["side"], "tdMode": row["tdMode"],
                        "posSide": row["posSide"], "tgtCcy": row["tgtCcy"], "sz": row["sz"],
                        "px": row["px"], "accFillSz": row["accFillSz"], "fillSz": row["fillSz"],
                        "avgPx": row["avgPx"], "fee": row["fee"], "feeCcy": row["feeCcy"],
                        "rebate": row["rebate"], "rebateCcy": row["rebateCcy"],
                        "pnl": row["pnl"], "reduceOnly": row["reduceOnly"], "lever": row["lever"],
                        "category": row["category"], "cancelSource": row["cancelSource"],
                        "transport_error": row["transport_error"],
                    })
                );
                return row;
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }

    fn cancel(&self, inst_id: &str, coid: &str) {
        self.call(
            &format!("cancel {coid}"),
            "POST",
            PATH_ORDER_CANCEL,
            &[("instId", json!(inst_id)), ("clOrdId", json!(coid))],
        );
    }

    /// The coin rows of the account balance for `ccys` (cash, equity, available): printed and returned.
    fn balances(&self, label: &str, ccys: &[&str]) -> Vec<(String, f64)> {
        let body = match self.transport.signed(REST, PATH_ACCOUNT, "GET", &[], &self.signer) {
            Ok(b) => b,
            Err(e) => json!({"transport_error": {"code": e.code, "msg": e.msg}}),
        };
        let account = body["data"].get(0).cloned().unwrap_or(Value::Null);
        let mut out = Vec::new();
        let mut rows = Vec::new();
        for d in account["details"].as_array().into_iter().flatten() {
            let ccy = d["ccy"].as_str().unwrap_or("");
            if ccys.contains(&ccy) {
                rows.push(json!({
                    "ccy": ccy, "cashBal": d["cashBal"], "eq": d["eq"], "availBal": d["availBal"],
                    "upl": d["upl"], "liab": d["liab"], "eqUsd": d["eqUsd"],
                }));
                out.push((ccy.to_string(), Self::num(d, "cashBal")));
            }
        }
        println!(
            "MEASURED balance {label}: {}",
            json!({"totalEq": account["totalEq"], "details": rows, "transport_error": body["transport_error"]})
        );
        out
    }

    /// Flatten a derivative the leg opened: read the position of `inst_id`, send the opposite side
    /// reduce-only at market. `inst_type` is the position read's own filter.
    fn flatten(&mut self, inst_type: &str, inst_id: &str) {
        let body = match self.transport.signed(
            REST,
            PATH_POSITIONS,
            "GET",
            &[("instType", json!(inst_type)), ("instId", json!(inst_id))],
            &self.signer,
        ) {
            Ok(b) => b,
            Err(e) => json!({"transport_error": {"code": e.code, "msg": e.msg}}),
        };
        println!("MEASURED positions {inst_id}: {}", pretty_positions(&body));
        let rows: Vec<Value> = body["data"].as_array().cloned().unwrap_or_default();
        for row in rows {
            let pos = Self::num(&row, "pos");
            if pos == 0.0 {
                continue;
            }
            let coid = self.coid("flat");
            let mode = row["mgnMode"].as_str().unwrap_or("cross").to_string();
            let label = format!("FLATTEN {inst_id} ({pos} contracts)");
            let params: Vec<(&str, Value)> = vec![
                ("instId", json!(inst_id)),
                ("tdMode", json!(mode)),
                ("side", json!(if pos > 0.0 { "sell" } else { "buy" })),
                ("ordType", json!("market")),
                ("sz", json!(trim_num(pos.abs()))),
                ("clOrdId", json!(coid)),
                ("posSide", json!("net")),
                ("reduceOnly", json!(true)),
            ];
            self.order(&label, &params);
            self.order_row(inst_id, &coid, Duration::from_secs(6));
        }
    }
}

/// A float as the shortest decimal string (`1.0` -> `1`, `0.1` -> `0.1`).
fn trim_num(x: f64) -> String {
    let s = format!("{x}");
    s.strip_suffix(".0").unwrap_or(&s).to_string()
}

fn pretty_positions(body: &Value) -> Value {
    let rows: Vec<Value> = body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            json!({
                "instId": p["instId"], "instType": p["instType"], "posSide": p["posSide"],
                "pos": p["pos"], "mgnMode": p["mgnMode"], "lever": p["lever"], "avgPx": p["avgPx"],
                "ccy": p["ccy"], "upl": p["upl"], "margin": p["margin"], "imr": p["imr"],
            })
        })
        .collect();
    json!({"code": body["code"], "msg": body["msg"], "rows": rows, "transport_error": body["transport_error"]})
}

/// `0.5 * last`, snapped DOWN to the tick: a limit BUY far below the market (rests, never fills).
fn far_buy_px(last: f64, tick: f64) -> String {
    format_to_step_f(last * 0.5, tick)
}

#[test]
#[ignore = "network + demo creds: a MEASUREMENT, run manually (see the module doc)"]
fn okx_classes_account_read() {
    let Some(creds) = creds() else { return };
    let p = Probe::new(&creds);
    // The account's own words (level, position mode); the uid and the key's permission are not printed.
    let body = p.transport.signed(REST, PATH_ACCOUNT_CONFIG, "GET", &[], &p.signer);
    match &body {
        Ok(b) => {
            let c = b["data"].get(0).cloned().unwrap_or(Value::Null);
            println!(
                "MEASURED account config: {}",
                json!({"acctLv": c["acctLv"], "posMode": c["posMode"], "autoLoan": c["autoLoan"],
                       "greeksType": c["greeksType"], "level": c["level"], "spotOffsetType": c["spotOffsetType"]})
            );
        }
        Err(e) => println!("MEASURED account config: refused {} {}", e.code, e.msg),
    }
    p.balances("account", &["USDT", "USDC", "USD", "BTC", "ETH", "GRASS", "EUR"]);
    for inst_type in ["SPOT", "SWAP", "FUTURES", "OPTION"] {
        let b = match p.transport.signed(
            REST,
            PATH_TRADE_FEE,
            "GET",
            &[("instType", json!(inst_type))],
            &p.signer,
        ) {
            Ok(b) => b,
            Err(e) => json!({"transport_error": {"code": e.code, "msg": e.msg}}),
        };
        let r = b["data"].get(0).cloned().unwrap_or(Value::Null);
        println!(
            "MEASURED trade-fee {inst_type}: {}",
            json!({"code": b["code"], "msg": b["msg"], "level": r["level"], "maker": r["maker"], "taker": r["taker"],
                   "makerU": r["makerU"], "takerU": r["takerU"], "makerUSDC": r["makerUSDC"], "takerUSDC": r["takerUSDC"],
                   "transport_error": b["transport_error"]})
        );
    }
    let all = p.transport.signed(REST, PATH_POSITIONS, "GET", &[], &p.signer);
    match all {
        Ok(b) => println!("MEASURED open positions (every instType): {}", pretty_positions(&b)),
        Err(e) => println!("MEASURED open positions: refused {} {}", e.code, e.msg),
    }
}

#[test]
#[ignore = "network + demo creds: a MEASUREMENT, run manually (see the module doc)"]
fn okx_classes_reads_per_class() {
    let Some(creds) = creds() else { return };
    let p = Probe::new(&creds);
    // the reads the reconcile client and the history replay make, once per class, on an instrument of each:
    // which of them answer for a spot id and a dated future with the SAME request shape as a swap's?
    for (class, inst) in [
        ("SPOT", SPOT),
        ("SWAP", "BTC-USDT-SWAP"),
        ("FUTURES", FUTURE_CANDIDATES[0]),
        ("SWAP", INVERSE),
    ] {
        type Read<'a> = (&'a str, &'a str, Vec<(&'a str, Value)>);
        let reads: [Read<'_>; 7] = [
            (
                "orders-pending (instId only)",
                "/api/v5/trade/orders-pending",
                vec![("instId", json!(inst))],
            ),
            ("fills (instId only)", "/api/v5/trade/fills", vec![("instId", json!(inst))]),
            (
                "orders-algo-pending (conditional, instId)",
                "/api/v5/trade/orders-algo-pending",
                vec![("ordType", json!("conditional")), ("instId", json!(inst))],
            ),
            (
                "positions (instType + instId)",
                PATH_POSITIONS,
                vec![("instType", json!(class)), ("instId", json!(inst))],
            ),
            (
                "orders-history (instType + instId)",
                "/api/v5/trade/orders-history",
                vec![("instType", json!(class)), ("instId", json!(inst)), ("limit", json!("3"))],
            ),
            (
                "fills-history (instType + instId)",
                "/api/v5/trade/fills-history",
                vec![("instType", json!(class)), ("instId", json!(inst)), ("limit", json!("3"))],
            ),
            (
                "orders-history WITH the SWAP instType (what the resync sends today)",
                "/api/v5/trade/orders-history",
                vec![("instType", json!("SWAP")), ("instId", json!(inst)), ("limit", json!("3"))],
            ),
        ];
        for (what, path, params) in reads {
            let body = match p.transport.signed(REST, path, "GET", &params, &p.signer) {
                Ok(b) => b,
                Err(e) => json!({"transport_error": {"code": e.code, "msg": e.msg}}),
            };
            println!(
                "MEASURED read {class} {inst} {what}: {}",
                json!({"code": body["code"], "msg": body["msg"],
                       "rows": body["data"].as_array().map(Vec::len),
                       "transport_error": body["transport_error"]})
            );
        }
    }
    // the account-wide position read of the instrument class a spot id belongs to (it has none)
    for class in ["SPOT", "MARGIN", "FUTURES"] {
        let body = p.transport.signed(
            REST,
            PATH_POSITIONS,
            "GET",
            &[("instType", json!(class))],
            &p.signer,
        );
        match body {
            Ok(b) => println!("MEASURED positions instType={class}: {}", pretty_positions(&b)),
            Err(e) => println!("MEASURED positions instType={class}: refused {} {}", e.code, e.msg),
        }
    }
}

#[test]
#[ignore = "network + demo creds: a MEASUREMENT with REAL DEMO ORDERS, run manually (see the module doc)"]
fn okx_classes_spot_wire() {
    let Some(creds) = creds() else { return };
    let mut p = Probe::new(&creds);
    let row = p.instrument("SPOT", SPOT);
    let (tick, lot, min) =
        (Probe::num(&row, "tickSz"), Probe::num(&row, "lotSz"), Probe::num(&row, "minSz"));
    let last = p.last(SPOT);
    println!("MEASURED spot last {SPOT}: {last}");
    if last <= 0.0 || min <= 0.0 {
        println!("MEASURED spot: no price or no minSz, nothing is sent");
        return;
    }
    let before = p.balances("before spot", &["BTC", "USDT"]);
    let px = far_buy_px(last, tick);
    let sz = format_to_step_f(min, lot);

    // -- a far limit BUY under tdMode `cross` (round 1 measured: `cash` is refused on this level-3 account),
    // with and without the swap builder's two extra keys: which does a SPOT order refuse? --
    let variants: [(&str, Vec<(&str, Value)>); 4] = [
        ("cross", vec![("tdMode", json!("cross"))]),
        ("cross+posSide=net", vec![("tdMode", json!("cross")), ("posSide", json!("net"))]),
        ("cross+reduceOnly=false", vec![("tdMode", json!("cross")), ("reduceOnly", json!(false))]),
        (
            "cross+posSide=net+reduceOnly=false (the SWAP builder's body)",
            vec![
                ("tdMode", json!("cross")),
                ("posSide", json!("net")),
                ("reduceOnly", json!(false)),
            ],
        ),
    ];
    for (name, extra) in variants {
        let coid = p.coid("spl");
        let mut params: Vec<(&str, Value)> = vec![
            ("instId", json!(SPOT)),
            ("side", json!("buy")),
            ("ordType", json!("limit")),
            ("sz", json!(sz)),
            ("px", json!(px)),
            ("clOrdId", json!(coid)),
        ];
        params.extend(extra);
        let r = p.order(&format!("spot far limit BUY, keys = {name}"), &params);
        if r["code"].as_str() == Some("0") {
            p.order_row(SPOT, &coid, Duration::from_secs(1));
            p.cancel(SPOT, &coid);
        }
    }
    // a far limit SELL (the account holds BTC): the same body, the other side
    let coid = p.coid("sps");
    let sell_px = format_to_step_f(last * 2.0, tick);
    let r = p.order(
        "spot far limit SELL, keys = cross+posSide=net+reduceOnly=false",
        &[
            ("instId", json!(SPOT)),
            ("tdMode", json!("cross")),
            ("side", json!("sell")),
            ("ordType", json!("limit")),
            ("sz", json!(sz)),
            ("px", json!(sell_px)),
            ("clOrdId", json!(coid)),
            ("posSide", json!("net")),
            ("reduceOnly", json!(false)),
        ],
    );
    if r["code"].as_str() == Some("0") {
        p.order_row(SPOT, &coid, Duration::from_secs(1));
        p.cancel(SPOT, &coid);
    }

    // -- a far STOP on the spot instrument: the algo endpoint's body, cross and cash --
    for td in ["cross", "cash"] {
        let algo = p.coid("ast");
        let r = p.call(
            &format!("spot far conditional SELL stop, tdMode={td}"),
            "POST",
            "/api/v5/trade/order-algo",
            &[
                ("instId", json!(SPOT)),
                ("tdMode", json!(td)),
                ("side", json!("sell")),
                ("ordType", json!("conditional")),
                ("sz", json!(sz)),
                ("slTriggerPx", json!(format_to_step_f(last * 0.5, tick))),
                ("slOrdPx", json!("-1")),
                ("algoClOrdId", json!(algo)),
            ],
        );
        if let Some(id) =
            r["data"][0]["algoId"].as_str().filter(|_| r["code"].as_str() == Some("0"))
        {
            let body = json!([{"algoId": id, "instId": SPOT}]).to_string();
            let out = p.transport.signed_json(
                REST,
                "/api/v5/trade/cancel-algos",
                "POST",
                &body,
                &p.signer,
            );
            println!("MEASURED spot stop cancel (cancel-algos): {out:?}");
        }
    }

    // -- a market BUY with no `tgtCcy`: is `sz` the QUOTE amount? ("5" = five USDT) --
    let market_buy = |p: &mut Probe, label: &str, sz: &str, tgt: Option<&str>| {
        let coid = p.coid("smk");
        let mut params: Vec<(&str, Value)> = vec![
            ("instId", json!(SPOT)),
            ("tdMode", json!("cross")),
            ("side", json!("buy")),
            ("ordType", json!("market")),
            ("sz", json!(sz)),
            ("clOrdId", json!(coid)),
        ];
        if let Some(t) = tgt {
            params.push(("tgtCcy", json!(t)));
        }
        p.order(label, &params);
        p.order_row(SPOT, &coid, Duration::from_secs(6))
    };
    let quote_buy = market_buy(&mut p, "spot market BUY sz=5, no tgtCcy (cross)", "5", None);
    let base_buy = market_buy(
        &mut p,
        "spot market BUY sz=minSz, tgtCcy=base_ccy (cross)",
        &sz,
        Some("base_ccy"),
    );

    // -- the SELL that returns what the two buys received, net of the base-coin fee --
    let received = |row: &Value| {
        let filled = Probe::num(row, "accFillSz");
        // a BUY's fee is charged in the base coin (a negative number); a quote fee leaves the coin whole
        if row["feeCcy"].as_str() == Some("BTC") { filled + Probe::num(row, "fee") } else { filled }
    };
    let coin = received(&quote_buy) + received(&base_buy);
    let sell_sz = format_to_step_f(coin, lot);
    println!(
        "MEASURED spot coin received by the two buys (net of a base fee): {coin} -> sell {sell_sz}"
    );
    if Probe::num(&json!({"x": sell_sz}), "x") > 0.0 {
        let coid = p.coid("sms");
        p.order(
            "spot market SELL sz=received, tgtCcy=base_ccy (cross)",
            &[
                ("instId", json!(SPOT)),
                ("tdMode", json!("cross")),
                ("side", json!("sell")),
                ("ordType", json!("market")),
                ("sz", json!(sell_sz)),
                ("tgtCcy", json!("base_ccy")),
                ("clOrdId", json!(coid)),
            ],
        );
        p.order_row(SPOT, &coid, Duration::from_secs(6));
    }
    // the recon reads over what the round trip left: fills of the instrument, the account's fee rate
    p.call(
        "fills BTC-USDT (instId only)",
        "GET",
        "/api/v5/trade/fills",
        &[("instId", json!(SPOT)), ("limit", json!("4"))],
    );
    let after = p.balances("after spot", &["BTC", "USDT"]);
    println!("MEASURED spot cashBal before {before:?} after {after:?}");
}

#[test]
#[ignore = "network + demo creds: a MEASUREMENT with REAL DEMO ORDERS, run manually (see the module doc)"]
fn okx_classes_futures_wire() {
    let Some(creds) = creds() else { return };
    let mut p = Probe::new(&creds);
    // the demo lists three USDT-linear dated futures (EDGE1, GRASS, QTUM); the first one this account may trade is used
    let mut chosen = None;
    for cand in FUTURE_CANDIDATES {
        let row = p.instrument("FUTURES", cand);
        let (tick, lot, min) =
            (Probe::num(&row, "tickSz"), Probe::num(&row, "lotSz"), Probe::num(&row, "minSz"));
        let last = p.last(cand);
        println!("MEASURED future last {cand}: {last}");
        if last <= 0.0 || min <= 0.0 {
            continue;
        }
        let sz = format_to_step_f(min, lot);
        let coid = p.coid("fl");
        let r = p.order(
            &format!("future {cand} far limit BUY (cross, posSide net)"),
            &[
                ("instId", json!(cand)),
                ("tdMode", json!("cross")),
                ("side", json!("buy")),
                ("ordType", json!("limit")),
                ("sz", json!(sz)),
                ("px", json!(far_buy_px(last, tick))),
                ("clOrdId", json!(coid)),
                ("posSide", json!("net")),
                ("reduceOnly", json!(false)),
            ],
        );
        if r["code"].as_str() == Some("0") {
            p.order_row(cand, &coid, Duration::from_secs(1));
            p.cancel(cand, &coid);
            chosen = Some((cand, sz));
            break;
        }
    }
    let Some((future, sz)) = chosen else {
        println!(
            "MEASURED future: no USDT-linear dated future of the demo accepted an order, nothing opened"
        );
        return;
    };
    p.balances("before future", &["USDT"]);
    p.call(
        "future set-leverage cross 2",
        "POST",
        PATH_SET_LEVERAGE,
        &[("instId", json!(future)), ("lever", json!("2")), ("mgnMode", json!("cross"))],
    );
    let coid = p.coid("fm");
    p.order(
        "future market BUY (cross, posSide net)",
        &[
            ("instId", json!(future)),
            ("tdMode", json!("cross")),
            ("side", json!("buy")),
            ("ordType", json!("market")),
            ("sz", json!(sz)),
            ("clOrdId", json!(coid)),
            ("posSide", json!("net")),
            ("reduceOnly", json!(false)),
        ],
    );
    p.order_row(future, &coid, Duration::from_secs(6));
    // the position row, under the class the venue files it in (FUTURES), and under SWAP (the read the
    // reconcile client sends today: a FUTURES position must not be in it)
    for class in ["FUTURES", "SWAP"] {
        let body = p.transport.signed(
            REST,
            PATH_POSITIONS,
            "GET",
            &[("instType", json!(class))],
            &p.signer,
        );
        match body {
            Ok(b) => println!(
                "MEASURED positions instType={class} with the future open: {}",
                pretty_positions(&b)
            ),
            Err(e) => println!("MEASURED positions instType={class}: refused {} {}", e.code, e.msg),
        }
    }
    p.flatten("FUTURES", future);
    p.call(
        "bills FUTURES (the leg's fee and pnl rows)",
        "GET",
        PATH_BILLS,
        &[("instType", json!("FUTURES")), ("limit", json!("6"))],
    );
    p.balances("after future", &["USDT"]);
}

#[test]
#[ignore = "network + demo creds: a MEASUREMENT with REAL DEMO ORDERS, run manually (see the module doc)"]
fn okx_classes_inverse_and_option_wire() {
    let Some(creds) = creds() else { return };
    let mut p = Probe::new(&creds);

    // -- the inverse swap: a far limit, then a market round trip with the coin balances either side --
    let row = p.instrument("SWAP", INVERSE);
    let (tick, lot, min) =
        (Probe::num(&row, "tickSz"), Probe::num(&row, "lotSz"), Probe::num(&row, "minSz"));
    let last = p.last(INVERSE);
    println!("MEASURED inverse last {INVERSE}: {last}");
    if last > 0.0 && min > 0.0 {
        let sz = format_to_step_f(min, lot);
        let before = p.balances("before inverse", &["USDT", "BTC", "USD"]);
        let coid = p.coid("il");
        p.order(
            "inverse far limit BUY (cross, posSide net)",
            &[
                ("instId", json!(INVERSE)),
                ("tdMode", json!("cross")),
                ("side", json!("buy")),
                ("ordType", json!("limit")),
                ("sz", json!(sz)),
                ("px", json!(far_buy_px(last, tick))),
                ("clOrdId", json!(coid)),
                ("posSide", json!("net")),
                ("reduceOnly", json!(false)),
            ],
        );
        p.order_row(INVERSE, &coid, Duration::from_secs(1));
        p.cancel(INVERSE, &coid);
        let coid = p.coid("im");
        p.order(
            "inverse market BUY (cross, posSide net)",
            &[
                ("instId", json!(INVERSE)),
                ("tdMode", json!("cross")),
                ("side", json!("buy")),
                ("ordType", json!("market")),
                ("sz", json!(sz)),
                ("clOrdId", json!(coid)),
                ("posSide", json!("net")),
                ("reduceOnly", json!(false)),
            ],
        );
        p.order_row(INVERSE, &coid, Duration::from_secs(6));
        p.flatten("SWAP", INVERSE);
        p.call(
            "bills SWAP (the inverse round trip's fee and pnl rows)",
            "GET",
            PATH_BILLS,
            &[("instType", json!("SWAP")), ("limit", json!("10"))],
        );
        let after = p.balances("after inverse", &["USDT", "BTC", "USD"]);
        println!("MEASURED inverse cashBal before {before:?} after {after:?}");
    } else {
        println!("MEASURED inverse: no price or no minSz, nothing is sent");
    }

    // -- the option: a far limit only (its premium's unit is the question; nothing is opened) --
    let row = p.instrument("OPTION", OPTION);
    let (tick, lot, min) =
        (Probe::num(&row, "tickSz"), Probe::num(&row, "lotSz"), Probe::num(&row, "minSz"));
    let last = p.last(OPTION);
    println!("MEASURED option last {OPTION}: {last}");
    let px =
        if last > 0.0 { far_buy_px(last, tick) } else { format_to_step_f(tick.max(0.0005), tick) };
    for td in ["cross", "isolated"] {
        let coid = p.coid("op");
        let r = p.order(
            &format!("option far limit BUY tdMode={td}"),
            &[
                ("instId", json!(OPTION)),
                ("tdMode", json!(td)),
                ("side", json!("buy")),
                ("ordType", json!("limit")),
                ("sz", json!(format_to_step_f(min, lot))),
                ("px", json!(px)),
                ("clOrdId", json!(coid)),
            ],
        );
        if r["code"].as_str() == Some("0") {
            p.order_row(OPTION, &coid, Duration::from_secs(1));
            p.cancel(OPTION, &coid);
        }
    }
}

#[test]
#[ignore = "network + demo creds: a MEASUREMENT with REAL DEMO ORDERS, run manually (see the module doc)"]
fn okx_classes_orders_channel_any() {
    let Some(creds) = creds() else { return };
    let Some(passphrase) = creds.passphrase.as_ref() else {
        println!("SKIP: OKX demo passphrase absent");
        return;
    };
    let mut p = Probe::new(&creds);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let gate = vike_okx::ratelimit::ws_rate_gate();
    let outcome = vike_okx::user_data::open_okx_user_data_ws(
        DEMO_WS,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        passphrase.expose(),
        now_ms,
        "orders",
        "ANY",
        &stop,
        &gate,
    );
    let mut socket = match outcome {
        OpenOutcome::Ready(s) => {
            println!("MEASURED orders channel instType=ANY: subscribe ACCEPTED");
            s
        }
        OpenOutcome::Transport(why) => {
            println!("MEASURED orders channel instType=ANY: transport failure: {why}");
            return;
        }
        OpenOutcome::Auth(why) => {
            println!("MEASURED orders channel instType=ANY: REFUSED: {why:?}");
            return;
        }
        OpenOutcome::Stopped => return,
    };
    // a spot round trip while the socket is open: market BUY in base, then the SELL of what it received
    let row = p.instrument("SPOT", SPOT);
    let (lot, min) = (Probe::num(&row, "lotSz"), Probe::num(&row, "minSz"));
    let sz = format_to_step_f(min, lot);
    let coid = p.coid("wb");
    p.order(
        "ws leg: spot market BUY base",
        &[
            ("instId", json!(SPOT)),
            ("tdMode", json!("cross")),
            ("side", json!("buy")),
            ("ordType", json!("market")),
            ("sz", json!(sz)),
            ("tgtCcy", json!("base_ccy")),
            ("clOrdId", json!(coid)),
        ],
    );
    let bought = p.order_row(SPOT, &coid, Duration::from_secs(6));
    let filled = Probe::num(&bought, "accFillSz");
    let fee =
        if bought["feeCcy"].as_str() == Some("BTC") { Probe::num(&bought, "fee") } else { 0.0 };
    let sell_sz = format_to_step_f(filled + fee, lot);
    let coid = p.coid("ws");
    p.order(
        "ws leg: spot market SELL base",
        &[
            ("instId", json!(SPOT)),
            ("tdMode", json!("cross")),
            ("side", json!("sell")),
            ("ordType", json!("market")),
            ("sz", json!(sell_sz)),
            ("tgtCcy", json!("base_ccy")),
            ("clOrdId", json!(coid)),
        ],
    );
    p.order_row(SPOT, &coid, Duration::from_secs(6));
    // the frames the socket buffered: print the fields the mapper reads
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut frames = 0;
    while Instant::now() < deadline {
        match socket.recv() {
            Ok(StreamMsg::Text(t)) => {
                let Ok(frame) = serde_json::from_str::<Value>(&t) else { continue };
                for d in frame["data"].as_array().into_iter().flatten() {
                    frames += 1;
                    println!(
                        "MEASURED ws frame: {}",
                        json!({
                            "arg": frame["arg"], "instType": d["instType"], "instId": d["instId"],
                            "state": d["state"], "side": d["side"], "ordType": d["ordType"],
                            "tgtCcy": d["tgtCcy"], "sz": d["sz"], "px": d["px"], "fillSz": d["fillSz"],
                            "fillPx": d["fillPx"], "accFillSz": d["accFillSz"], "tradeId": d["tradeId"],
                            "fillFee": d["fillFee"], "fillFeeCcy": d["fillFeeCcy"], "fee": d["fee"],
                            "feeCcy": d["feeCcy"], "tdMode": d["tdMode"], "posSide": d["posSide"],
                            "execType": d["execType"],
                        })
                    );
                }
            }
            Ok(_) | Err(StreamError::Timeout) => {}
            Err(StreamError::Closed(why)) => {
                println!("MEASURED ws: closed: {why}");
                break;
            }
        }
    }
    println!("MEASURED ws frames read: {frames}");
}

#[test]
#[ignore = "network + demo creds: a MEASUREMENT with REAL DEMO ORDERS (far, cancelled), run manually (see the module doc)"]
fn okx_classes_round3() {
    let Some(creds) = creds() else { return };
    let Some(passphrase) = creds.passphrase.as_ref() else {
        println!("SKIP: OKX demo passphrase absent");
        return;
    };
    let mut p = Probe::new(&creds);

    // -- the algo channel on the BUSINESS socket with instType ANY: one subscription for the stops of
    // every class? (the private `orders` channel accepted ANY in round 1) --
    let stop = std::sync::atomic::AtomicBool::new(false);
    let gate = vike_okx::ratelimit::ws_rate_gate();
    match vike_okx::user_data::open_okx_user_data_ws(
        vike_okx::perp::DEMO_BUSINESS_WS,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        passphrase.expose(),
        now_ms,
        "orders-algo",
        "ANY",
        &stop,
        &gate,
    ) {
        OpenOutcome::Ready(_) => {
            println!("MEASURED orders-algo channel instType=ANY: subscribe ACCEPTED")
        }
        OpenOutcome::Transport(why) => {
            println!("MEASURED orders-algo channel instType=ANY: transport: {why}")
        }
        OpenOutcome::Auth(why) => {
            println!("MEASURED orders-algo channel instType=ANY: REFUSED: {why:?}")
        }
        OpenOutcome::Stopped => {}
    }

    // -- a spot limit order AMENDED in place (newSz is base, newPx tick-rounded) --
    let row = p.instrument("SPOT", SPOT);
    let (tick, lot, min) =
        (Probe::num(&row, "tickSz"), Probe::num(&row, "lotSz"), Probe::num(&row, "minSz"));
    let last = p.last(SPOT);
    if last > 0.0 && min > 0.0 {
        let coid = p.coid("am");
        let r = p.order(
            "spot far limit BUY for the amend",
            &[
                ("instId", json!(SPOT)),
                ("tdMode", json!("cross")),
                ("side", json!("buy")),
                ("ordType", json!("limit")),
                ("sz", json!(format_to_step_f(min, lot))),
                ("px", json!(far_buy_px(last, tick))),
                ("clOrdId", json!(coid)),
            ],
        );
        if r["code"].as_str() == Some("0") {
            p.call(
                "spot amend-order (newSz, newPx)",
                "POST",
                "/api/v5/trade/amend-order",
                &[
                    ("instId", json!(SPOT)),
                    ("clOrdId", json!(coid)),
                    ("newSz", json!(format_to_step_f(min * 2.0, lot))),
                    ("newPx", json!(format_to_step_f(last * 0.51, tick))),
                ],
            );
            p.order_row(SPOT, &coid, Duration::from_secs(1));
            p.cancel(SPOT, &coid);
        }
        // -- a spot SELL of more coin than the account holds: the venue's word (the engine can open a
        // "short" on a Cash symbol; does the venue borrow or refuse?) --
        let coid = p.coid("os");
        let r = p.order(
            "spot far limit SELL of 5 BTC (the account holds about 1)",
            &[
                ("instId", json!(SPOT)),
                ("tdMode", json!("cross")),
                ("side", json!("sell")),
                ("ordType", json!("limit")),
                ("sz", json!("5")),
                ("px", json!(format_to_step_f(last * 2.0, tick))),
                ("clOrdId", json!(coid)),
            ],
        );
        if r["code"].as_str() == Some("0") {
            p.order_row(SPOT, &coid, Duration::from_secs(1));
            p.cancel(SPOT, &coid);
        }
    }

    // -- a dated future: the leverage the venue holds, and a far (non-reduce-only) conditional stop --
    for cand in FUTURE_CANDIDATES {
        let last = p.last(cand);
        let row = p.instrument("FUTURES", cand);
        let (tick, lot, min) =
            (Probe::num(&row, "tickSz"), Probe::num(&row, "lotSz"), Probe::num(&row, "minSz"));
        if last <= 0.0 || min <= 0.0 {
            continue;
        }
        p.call(
            &format!("leverage-info cross {cand}"),
            "GET",
            vike_okx::leverage_read::PATH_LEVERAGE_INFO,
            &[("mgnMode", json!("cross")), ("instId", json!(cand))],
        );
        let algo = p.coid("fst");
        let r = p.call(
            &format!(
                "future {cand} far conditional SELL stop (cross, posSide net, not reduce-only)"
            ),
            "POST",
            "/api/v5/trade/order-algo",
            &[
                ("instId", json!(cand)),
                ("tdMode", json!("cross")),
                ("side", json!("sell")),
                ("ordType", json!("conditional")),
                ("sz", json!(format_to_step_f(min, lot))),
                ("slTriggerPx", json!(format_to_step_f(last * 0.5, tick))),
                ("slOrdPx", json!("-1")),
                ("algoClOrdId", json!(algo)),
                ("posSide", json!("net")),
                ("reduceOnly", json!(false)),
            ],
        );
        if let Some(id) =
            r["data"][0]["algoId"].as_str().filter(|_| r["code"].as_str() == Some("0"))
        {
            let body = json!([{"algoId": id, "instId": cand}]).to_string();
            let out = p.transport.signed_json(
                REST,
                "/api/v5/trade/cancel-algos",
                "POST",
                &body,
                &p.signer,
            );
            println!("MEASURED future stop cancel (cancel-algos): {out:?}");
            break;
        }
    }
}
