//! LIVE demo probe for the Bybit instrument CLASSES the exec plane did not trade (decision 0231): the
//! wire facts that the offline fixtures under `tests/fixtures/classes/` were captured from.
//!
//!     VIKE_SETTINGS_DIR=<settings> VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_classes_probe -- --ignored --nocapture --test-threads=1
//!
//! `#[ignore]`d and DEMO ACCOUNT ONLY (the default demo row's keys, `DEMO_REST`; it self-skips without them).
//! It MUTATES the demo account for seconds and leaves it flat: per class it (1) reads the instrument row,
//! (2) rests ONE far limit order under its own `bbcls` order-link id, reads the venue's own echo of it
//! (`order/realtime`), cancels it by id and reads the closed row (`order/history`), (3) for a class whose
//! ordinary order can be placed at minimum size, round-trips the minimum (a market buy and a reduce-only
//! sell, or for spot a sell of what the buy really delivered) and prints the `execution/list` rows, the
//! `position/list` row and the wallet delta. It NEVER calls `cancel-all` (the node's own resting orders are
//! on this account), never touches a symbol the node mounts, and prints every request and its whole answer
//! as `CAPTURE <tag> <path> <params> => <envelope>` lines so the transcript IS the fixture source.
//!
//! The classes: SPOT (`category=spot`), a dated LINEAR future (`LinearFutures`, `category=linear`), the
//! INVERSE perpetual and a dated INVERSE future (`category=inverse`), an OPTION (`category=option`). An
//! answer that says the demo account cannot trade a class is a measurement too (printed, not an error).

use std::time::Duration;

use serde_json::{Value, json};

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bybit::perp::DEMO_REST;
use vike_bybit::transport::{BybitTransport, UreqBybitTransport};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::now_ms;
use vike_secrets::credentials::api_credentials;

/// Symbols the probe trades. None is the symbol a demo node mounts (`BTCUSDT`) or admitted in a drill
/// (`ETHUSDT`, `XRPUSDT`): a fill here reaches the node's account-wide user-data stream, which carries no
/// category on the symbol it folds.
const SPOT: &str = "DOGEUSDT";
const INVERSE_PERP: &str = "BTCUSD";
/// The inverse contracts' settlement coin (the one the demo account's collateral switch must have on).
const INVERSE_COIN: &str = "BTC";
const DATED_LINEAR_BASE: &str = "ETHUSDT-";
const INVERSE_DATED_BASE: &str = "BTCUSD";

struct Probe {
    signer: BybitV5Signer,
    transport: UreqBybitTransport,
    /// Orders this probe rested and has not cancelled: (category, symbol, orderLinkId). A `Drop` cancels them.
    open: std::cell::RefCell<Vec<(String, String, String)>>,
}

impl Probe {
    /// One signed call; prints the request and the WHOLE envelope. A transport failure prints and is `None`.
    fn raw(&self, tag: &str, path: &str, method: &str, params: &[(&str, Value)]) -> Option<Value> {
        let shown: serde_json::Map<String, Value> =
            params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        match self.transport.signed(DEMO_REST, path, method, params, &self.signer) {
            Ok(env) => {
                println!("CAPTURE {tag} {method} {path} {} => {env}", Value::Object(shown));
                Some(env)
            }
            Err(e) => {
                println!(
                    "CAPTURE {tag} {method} {path} {} => TRANSPORT-ERROR code={} msg={}",
                    Value::Object(shown),
                    e.code,
                    e.msg
                );
                None
            }
        }
    }

    fn ok(env: &Option<Value>) -> bool {
        env.as_ref().and_then(|e| e.get("retCode")).and_then(Value::as_i64) == Some(0)
    }

    fn result(env: &Option<Value>) -> Value {
        env.as_ref().and_then(|e| e.get("result")).cloned().unwrap_or(Value::Null)
    }

    fn first_row(env: &Option<Value>) -> Option<Value> {
        Self::result(env).get("list").and_then(Value::as_array).and_then(|l| l.first()).cloned()
    }

    /// The instrument row of `symbol` in `category` (public data through the signed client).
    fn instrument(&self, category: &str, symbol: &str) -> Option<Value> {
        let env = self.raw(
            &format!("instrument-{category}-{symbol}"),
            "/v5/market/instruments-info",
            "GET",
            &[("category", json!(category)), ("symbol", json!(symbol))],
        );
        Self::first_row(&env)
    }

    fn num(row: &Value, a: &str, b: &str) -> f64 {
        row.get(a).and_then(|x| x.get(b)).and_then(json_num).unwrap_or(0.0)
    }

    /// The mark (derivatives) or last (spot) price.
    fn price(&self, category: &str, symbol: &str) -> f64 {
        let env = self.raw(
            &format!("ticker-{category}-{symbol}"),
            "/v5/market/tickers",
            "GET",
            &[("category", json!(category)), ("symbol", json!(symbol))],
        );
        let row = Self::first_row(&env).unwrap_or(Value::Null);
        let mark = row.get("markPrice").and_then(json_num).unwrap_or(0.0);
        if mark > 0.0 { mark } else { row.get("lastPrice").and_then(json_num).unwrap_or(0.0) }
    }

    /// A fresh order-link id (36 characters at most on this venue): `bbcls-<n>-<ms>`; the tag lives in the log.
    fn link(_tag: &str) -> String {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("bbcls-{n}-{}", now_ms() % 1_000_000_000)
    }

    /// Rest `params` (a create body without the category/symbol/link) and remember it for the cleanup.
    fn rest_order(
        &self,
        tag: &str,
        category: &str,
        symbol: &str,
        extra: Vec<(&str, Value)>,
    ) -> Option<String> {
        let link = Self::link(tag);
        let mut params: Vec<(&str, Value)> = vec![
            ("category", json!(category)),
            ("symbol", json!(symbol)),
            ("orderLinkId", json!(link)),
        ];
        params.extend(extra);
        let env = self.raw(&format!("create-{tag}"), "/v5/order/create", "POST", &params);
        if !Self::ok(&env) {
            return None;
        }
        self.open.borrow_mut().push((category.to_string(), symbol.to_string(), link.clone()));
        Some(link)
    }

    fn echo_and_cancel(&self, tag: &str, category: &str, symbol: &str, link: &str) {
        let q = [
            ("category", json!(category)),
            ("symbol", json!(symbol)),
            ("orderLinkId", json!(link)),
        ];
        self.raw(&format!("realtime-{tag}"), "/v5/order/realtime", "GET", &q);
        self.raw(&format!("cancel-{tag}"), "/v5/order/cancel", "POST", &q);
        std::thread::sleep(Duration::from_millis(800));
        self.raw(&format!("history-{tag}"), "/v5/order/history", "GET", &q);
        self.open.borrow_mut().retain(|(_, _, l)| l != link);
    }

    fn account(&self, tag: &str) {
        self.raw(&format!("account-info-{tag}"), "/v5/account/info", "GET", &[]);
        self.raw(
            &format!("wallet-{tag}"),
            "/v5/account/wallet-balance",
            "GET",
            &[("accountType", json!("UNIFIED"))],
        );
    }

    fn wallet_coin(&self, coin: &str) -> f64 {
        let env = self.raw(
            &format!("wallet-coin-{coin}"),
            "/v5/account/wallet-balance",
            "GET",
            &[("accountType", json!("UNIFIED")), ("coin", json!(coin))],
        );
        Self::result(&env)
            .get("list")
            .and_then(Value::as_array)
            .and_then(|l| l.first())
            .and_then(|a| a.get("coin"))
            .and_then(Value::as_array)
            .and_then(|c| c.iter().find(|x| x.get("coin").and_then(Value::as_str) == Some(coin)))
            .and_then(|c| c.get("walletBalance"))
            .and_then(json_num)
            .unwrap_or(0.0)
    }

    fn executions(&self, tag: &str, category: &str, symbol: &str) {
        std::thread::sleep(Duration::from_millis(1500));
        self.raw(
            &format!("executions-{tag}"),
            "/v5/execution/list",
            "GET",
            &[("category", json!(category)), ("symbol", json!(symbol)), ("limit", json!(10))],
        );
    }

    fn position(&self, tag: &str, category: &str, symbol: &str) -> Option<Value> {
        let env = self.raw(
            &format!("position-{tag}"),
            "/v5/position/list",
            "GET",
            &[("category", json!(category)), ("symbol", json!(symbol))],
        );
        Self::first_row(&env)
    }

    /// Best-effort cancel of whatever this probe still rests.
    fn sweep(&self) {
        let open: Vec<_> = self.open.borrow().clone();
        for (category, symbol, link) in open {
            self.raw(
                "sweep-cancel",
                "/v5/order/cancel",
                "POST",
                &[
                    ("category", json!(category)),
                    ("symbol", json!(symbol)),
                    ("orderLinkId", json!(link)),
                ],
            );
        }
        self.open.borrow_mut().clear();
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.sweep();
    }
}

/// The qty that is worth at least `usd` at `price` on the instrument's lot grid (rounded UP one step).
fn qty_worth(usd: f64, price: f64, min_qty: f64, step: f64) -> f64 {
    let raw = (usd / price).max(min_qty);
    (raw / step).ceil() * step
}

/// The label of the bybit DEMO account: the unlabelled one when the store has it, else the ONE active labelled
/// `demo` row (the CI box's is labelled `DEMO`); only the `demo` tier is ever asked for (the restore smoke's rule).
fn demo_label(directory: &vike_secrets::account_directory::AccountDirectory) -> AccountLabel {
    if directory.account_row("bybit", &AccountLabel::Default, AccountTier::Demo).is_some() {
        return AccountLabel::Default;
    }
    let rows = directory.rows().and_then(Result::ok).and_then(|a| a.known()).unwrap_or_default();
    let mut labels = rows
        .iter()
        .filter(|r| r.active && r.venue == "bybit" && r.tier == AccountTier::Demo)
        .filter_map(|r| r.label.clone());
    match (labels.next(), labels.next()) {
        (Some(label), None) => AccountLabel::Named(label),
        _ => AccountLabel::Default,
    }
}

fn probe() -> Option<Probe> {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &demo_label(&directory), AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: the default bybit DEMO credentials are absent");
        return None;
    };
    Some(Probe {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new(),
        open: std::cell::RefCell::new(Vec::new()),
    })
}

#[test]
#[ignore = "network + demo creds, mutates the demo account for seconds (see module doc)"]
fn classes_account_and_spot() {
    let Some(p) = probe() else { return };
    p.account("start");
    let Some(row) = p.instrument("spot", SPOT) else { panic!("{SPOT} is not a spot row") };
    let tick = Probe::num(&row, "priceFilter", "tickSize");
    let base_precision = Probe::num(&row, "lotSizeFilter", "basePrecision");
    let min_amt = Probe::num(&row, "lotSizeFilter", "minOrderAmt");
    let last = p.price("spot", SPOT);
    assert!(last > 0.0 && tick > 0.0 && base_precision > 0.0, "spot row: {row}");

    // (1) a far limit BUY: the echo, the cancel, the closed row.
    let far = format_to_step_f(last * 0.7, tick);
    let far_px: f64 = far.parse().unwrap();
    let qty = qty_worth(min_amt * 2.5, far_px, 0.0, base_precision);
    let link = p.rest_order(
        "spot-far",
        "spot",
        SPOT,
        vec![
            ("side", json!("Buy")),
            ("orderType", json!("Limit")),
            ("qty", json!(format_to_step_f(qty, base_precision))),
            ("price", json!(far)),
            ("timeInForce", json!("GTC")),
        ],
    );
    if let Some(link) = link {
        p.echo_and_cancel("spot-far", "spot", SPOT, &link);
    }

    // (2) a far limit SELL of coin the account may not hold: the venue's answer is the measurement.
    let sell_far = format_to_step_f(last * 1.5, tick);
    let link = p.rest_order(
        "spot-far-sell",
        "spot",
        SPOT,
        vec![
            ("side", json!("Sell")),
            ("orderType", json!("Limit")),
            ("qty", json!(format_to_step_f(qty, base_precision))),
            ("price", json!(sell_far)),
            ("timeInForce", json!("GTC")),
        ],
    );
    if let Some(link) = link {
        p.echo_and_cancel("spot-far-sell", "spot", SPOT, &link);
    }

    // (3) a MARKET BUY with `qty` and NO `marketUnit`: which unit does the venue read? Then the base-unit one.
    let base0 = p.wallet_coin("DOGE");
    let quote_qty = format!("{}", (min_amt * 2.5).ceil());
    let link = p.rest_order(
        "spot-mkt-default",
        "spot",
        SPOT,
        vec![("side", json!("Buy")), ("orderType", json!("Market")), ("qty", json!(quote_qty))],
    );
    if let Some(link) = link {
        p.executions("spot-mkt-default", "spot", SPOT);
        p.echo_and_cancel("spot-mkt-default", "spot", SPOT, &link);
    }
    let base1 = p.wallet_coin("DOGE");
    let base_qty = qty_worth(min_amt * 2.5, last, 0.0, base_precision);
    let link = p.rest_order(
        "spot-mkt-base",
        "spot",
        SPOT,
        vec![
            ("side", json!("Buy")),
            ("orderType", json!("Market")),
            ("qty", json!(format_to_step_f(base_qty, base_precision))),
            ("marketUnit", json!("baseCoin")),
        ],
    );
    if let Some(link) = link {
        p.executions("spot-mkt-base", "spot", SPOT);
        p.echo_and_cancel("spot-mkt-base", "spot", SPOT, &link);
    }
    let base2 = p.wallet_coin("DOGE");
    println!(
        "MEASURE spot DOGE wallet: before={base0} after-default-unit-buy={base1} after-base-unit-buy={base2}"
    );

    // (4) sell back what the account holds above its start, in BASE units, reduce-only is not a spot word.
    let held = (base2 - base0).max(0.0);
    let sell_qty = (held / base_precision).floor() * base_precision;
    if sell_qty > 0.0 {
        let link = p.rest_order(
            "spot-mkt-sell",
            "spot",
            SPOT,
            vec![
                ("side", json!("Sell")),
                ("orderType", json!("Market")),
                ("qty", json!(format_to_step_f(sell_qty, base_precision))),
                ("marketUnit", json!("baseCoin")),
            ],
        );
        if let Some(link) = link {
            p.executions("spot-mkt-sell", "spot", SPOT);
            p.echo_and_cancel("spot-mkt-sell", "spot", SPOT, &link);
        }
    }
    let base3 = p.wallet_coin("DOGE");
    println!("MEASURE spot DOGE wallet after the sell: {base3} (dust left: {})", base3 - base0);
    p.raw(
        "spot-open-orders-after",
        "/v5/order/realtime",
        "GET",
        &[("category", json!("spot")), ("symbol", json!(SPOT)), ("openOnly", json!(0))],
    );
}

/// The listed row of the first symbol in `category` whose name starts with `prefix` and satisfies `pick`.
fn listed(
    p: &Probe,
    category: &str,
    prefix: &str,
    pick: impl Fn(&Value) -> bool,
) -> Option<(String, Value)> {
    let env = p.raw(
        &format!("list-{category}-{prefix}"),
        "/v5/market/instruments-info",
        "GET",
        &[("category", json!(category)), ("limit", json!(1000))],
    );
    let list =
        Probe::result(&env).get("list").and_then(Value::as_array).cloned().unwrap_or_default();
    list.into_iter()
        .filter(|r| {
            r.get("symbol").and_then(Value::as_str).is_some_and(|s| s.starts_with(prefix))
                && r.get("status").and_then(Value::as_str) == Some("Trading")
                && pick(r)
        })
        .map(|r| (r["symbol"].as_str().unwrap_or_default().to_string(), r))
        .min_by_key(|(_, r)| r.get("deliveryTime").and_then(json_num).unwrap_or(0.0) as i64)
}

/// A derivative class: far limit, round trip at the minimum, position row, wallet.
fn derivative(p: &Probe, tag: &str, category: &str, symbol: &str, row: &Value, coin: &str) {
    let tick = Probe::num(row, "priceFilter", "tickSize");
    let step = Probe::num(row, "lotSizeFilter", "qtyStep");
    let min_qty = Probe::num(row, "lotSizeFilter", "minOrderQty");
    let min_notional = Probe::num(row, "lotSizeFilter", "minNotionalValue");
    let mark = p.price(category, symbol);
    assert!(mark > 0.0 && tick > 0.0 && step > 0.0, "{symbol}: row {row}");
    // The inverse quantity is USD contracts (a notional), the linear one base units.
    let inverse = category == "inverse";
    let usd = (min_notional * 1.6).max(8.0);
    let qty = if inverse {
        qty_worth(usd, 1.0, min_qty, step)
    } else {
        qty_worth(usd, mark, min_qty, step)
    };
    let qty_s = format_to_step_f(qty, step);

    p.raw(
        &format!("leverage-read-{tag}"),
        "/v5/position/list",
        "GET",
        &[("category", json!(category)), ("symbol", json!(symbol))],
    );
    let wallet0 = p.wallet_coin(coin);

    let far = format_to_step_f(mark * 0.6, tick);
    let link = p.rest_order(
        &format!("{tag}-far"),
        category,
        symbol,
        vec![
            ("side", json!("Buy")),
            ("orderType", json!("Limit")),
            ("qty", json!(qty_s)),
            ("price", json!(far)),
            ("timeInForce", json!("GTC")),
            ("positionIdx", json!(0)),
            ("reduceOnly", json!(false)),
        ],
    );
    if let Some(link) = link {
        p.echo_and_cancel(&format!("{tag}-far"), category, symbol, &link);
    }

    // A far STOP (a native conditional) as the bracket's stop leg would place it.
    let trigger = format_to_step_f(mark * 0.6, tick);
    let link = p.rest_order(
        &format!("{tag}-far-stop"),
        category,
        symbol,
        vec![
            ("side", json!("Sell")),
            ("orderType", json!("Market")),
            ("qty", json!(qty_s)),
            ("triggerPrice", json!(trigger)),
            ("triggerDirection", json!(2)),
            ("triggerBy", json!("LastPrice")),
            ("positionIdx", json!(0)),
            ("reduceOnly", json!(false)),
        ],
    );
    if let Some(link) = link {
        p.echo_and_cancel(&format!("{tag}-far-stop"), category, symbol, &link);
    }

    // The round trip at the minimum.
    let buy = p.rest_order(
        &format!("{tag}-mkt-buy"),
        category,
        symbol,
        vec![
            ("side", json!("Buy")),
            ("orderType", json!("Market")),
            ("qty", json!(qty_s)),
            ("positionIdx", json!(0)),
            ("reduceOnly", json!(false)),
        ],
    );
    if let Some(link) = buy {
        p.executions(&format!("{tag}-mkt-buy"), category, symbol);
        p.echo_and_cancel(&format!("{tag}-mkt-buy"), category, symbol, &link);
        let row = p.position(&format!("{tag}-open"), category, symbol);
        println!("MEASURE {tag} position row after the buy: {row:?}");
        p.raw(
            &format!("wallet-{tag}-open"),
            "/v5/account/wallet-balance",
            "GET",
            &[("accountType", json!("UNIFIED"))],
        );
        let sell = p.rest_order(
            &format!("{tag}-mkt-sell"),
            category,
            symbol,
            vec![
                ("side", json!("Sell")),
                ("orderType", json!("Market")),
                ("qty", json!(qty_s)),
                ("positionIdx", json!(0)),
                ("reduceOnly", json!(true)),
            ],
        );
        if let Some(link) = sell {
            p.executions(&format!("{tag}-mkt-sell"), category, symbol);
            p.echo_and_cancel(&format!("{tag}-mkt-sell"), category, symbol, &link);
        }
        let row = p.position(&format!("{tag}-flat"), category, symbol);
        println!("MEASURE {tag} position row after the sell: {row:?}");
    }
    let wallet1 = p.wallet_coin(coin);
    println!("MEASURE {tag} {coin} wallet: before={wallet0} after={wallet1}");
}

#[test]
#[ignore = "network + demo creds, mutates the demo account for seconds (see module doc)"]
fn classes_dated_linear_future() {
    let Some(p) = probe() else { return };
    // The nearest ETH dated future that is Trading and more than a day from its delivery: the deepest book.
    let env = p.raw(
        "list-linear-dated",
        "/v5/market/instruments-info",
        "GET",
        &[("category", json!("linear")), ("limit", json!(1000))],
    );
    let list =
        Probe::result(&env).get("list").and_then(Value::as_array).cloned().unwrap_or_default();
    let pick = list
        .into_iter()
        .filter(|r| {
            r.get("contractType").and_then(Value::as_str) == Some("LinearFutures")
                && r.get("status").and_then(Value::as_str) == Some("Trading")
                && r["symbol"].as_str().is_some_and(|s| s.starts_with(DATED_LINEAR_BASE))
                && r.get("deliveryTime").and_then(json_num).unwrap_or(0.0)
                    > (now_ms() + 86_400_000) as f64
        })
        .min_by_key(|r| r.get("deliveryTime").and_then(json_num).unwrap_or(0.0) as i64);
    let Some(row) = pick else { panic!("no Trading {DATED_LINEAR_BASE}* LinearFutures row") };
    let symbol = row["symbol"].as_str().unwrap().to_string();
    println!("MEASURE dated linear symbol {symbol} deliveryTime {}", row["deliveryTime"]);
    // The grid read the way the exec thread reads one symbol (the linear category answers for it).
    p.instrument("linear", &symbol);
    derivative(&p, "dated", "linear", &symbol, &row, "USDT");
}

#[test]
#[ignore = "network + demo creds, mutates the demo account for seconds (see module doc)"]
fn classes_inverse() {
    let Some(p) = probe() else { return };
    // The measured refusal of the first run (110101): an inverse contract needs its SETTLEMENT coin switched on
    // as collateral. This probe switches BTC on for its own run and puts the switch back as it found it.
    let coin = INVERSE_COIN;
    let info = p.raw(
        "collateral-info-before",
        "/v5/account/collateral-info",
        "GET",
        &[("currency", json!(coin))],
    );
    let was_on = Probe::first_row(&info)
        .and_then(|r| r.get("collateralSwitch").and_then(Value::as_bool))
        .unwrap_or(false);
    let _restore = CollateralGuard { probe: &p, coin, was_on };
    if !was_on {
        p.raw(
            "collateral-switch-on",
            "/v5/account/set-collateral-switch",
            "POST",
            &[("coin", json!(coin)), ("collateralSwitch", json!("ON"))],
        );
    }
    p.raw("collateral-info-on", "/v5/account/collateral-info", "GET", &[("currency", json!(coin))]);
    // The inverse perpetual, read through BOTH categories (the linear one answers for it: measured 2026-09-16).
    let via_linear = p.instrument("linear", INVERSE_PERP);
    println!("MEASURE inverse perp via the linear category: {via_linear:?}");
    let Some(row) = p.instrument("inverse", INVERSE_PERP) else { panic!("no {INVERSE_PERP} row") };
    derivative(&p, "inverse-perp", "inverse", INVERSE_PERP, &row, coin);

    // A dated inverse future of the same base coin, the nearest.
    if let Some((symbol, row)) = listed(&p, "inverse", INVERSE_DATED_BASE, |r| {
        r.get("contractType").and_then(Value::as_str) == Some("InverseFutures")
    }) {
        println!("MEASURE dated inverse symbol {symbol} deliveryTime {}", row["deliveryTime"]);
        derivative(&p, "inverse-dated", "inverse", &symbol, &row, coin);
    }
}

/// Puts a coin's collateral switch back to what the probe found (the demo account is left as it was).
struct CollateralGuard<'a> {
    probe: &'a Probe,
    coin: &'a str,
    was_on: bool,
}

impl Drop for CollateralGuard<'_> {
    fn drop(&mut self) {
        if !self.was_on {
            self.probe.raw(
                "collateral-switch-restore",
                "/v5/account/set-collateral-switch",
                "POST",
                &[("coin", json!(self.coin)), ("collateralSwitch", json!("OFF"))],
            );
        }
    }
}

#[test]
#[ignore = "network + demo creds, mutates the demo account for seconds (see module doc)"]
fn classes_option() {
    let Some(p) = probe() else { return };
    let Some((symbol, row)) = listed(&p, "option", "BTC-", |r| {
        r.get("optionsType").and_then(Value::as_str) == Some("Call")
    }) else {
        println!("MEASURE option: the demo lists no BTC call");
        return;
    };
    let tick = Probe::num(&row, "priceFilter", "tickSize");
    let step = Probe::num(&row, "lotSizeFilter", "qtyStep");
    let min_qty = Probe::num(&row, "lotSizeFilter", "minOrderQty");
    let min_px = Probe::num(&row, "priceFilter", "minPrice");
    println!(
        "MEASURE option symbol {symbol}: tick {tick} step {step} min qty {min_qty} min price {min_px}"
    );
    p.raw(
        "option-ticker",
        "/v5/market/tickers",
        "GET",
        &[("category", json!("option")), ("symbol", json!(symbol))],
    );
    let link = p.rest_order(
        "option-far",
        "option",
        &symbol,
        vec![
            ("side", json!("Buy")),
            ("orderType", json!("Limit")),
            ("qty", json!(format_to_step_f(min_qty, step))),
            ("price", json!(format_to_step_f(min_px, tick))),
            ("timeInForce", json!("GTC")),
        ],
    );
    if let Some(link) = link {
        p.echo_and_cancel("option-far", "option", &symbol, &link);
    }
    p.raw(
        "option-positions",
        "/v5/position/list",
        "GET",
        &[("category", json!("option")), ("symbol", json!(symbol))],
    );
}
