//! LIVE DEMO drills that MEASURE the Binance classes the any-symbol door did not admit (TradFi perpetuals, dated
//! USDⓈ-M futures, COIN-M): network + demo creds, run by hand and never in a loop, NOT in
//! `.github/workflows/live-smokes.yml`:
//!
//!     cargo test -p vike-binance --test binance_classes_smoke -- --ignored --nocapture --test-threads=1
//!
//! Every test prints `FINDING:` lines (the measured answers the docs cite), and a `Drop` guard cancels what a drill
//! rested and flattens what it opened, on the drill's own symbols ONLY: the demo account is SHARED (the lane twin
//! trades `BTCUSDT` and rests a stop on it) and no drill touches a symbol it does not list below.
//!
//! * `binance_classes_census` — READ-ONLY. What the demo hosts (`demo-fapi`, `demo-dapi`) answer for the account:
//!   position mode, the wallet assets, the margin type and leverage of each drill symbol, and whether the COIN-M
//!   user-data stream hands out a listen key.
//! * `binance_classes_far_limit_echo` — rests ONE far limit BUY per class symbol (inside the venue's PERCENT_PRICE
//!   band, far from the touch, so it can never fill), reads the venue's OWN echo of price and quantity, cancels it
//!   and requires the order gone. The wire answer of "does the venue take this class on the order endpoint".
//! * `binance_classes_minimum_fill_and_flatten` — ONE minimum-size market BUY per symbol, the venue's own trade rows
//!   (price, quantity, quote and base quantity, the commission and its ASSET), then a reduce-only close: the fee row
//!   and the size meaning of each class.
//!
//! **DEMO ONLY**: every request goes to `demo-fapi.binance.com` or `demo-dapi.binance.com` (asserted), the account
//! is the store's default demo account (`vike_secrets::smoke_directory`, credentials never printed) and the owner's
//! fence of account ids no drill drives (`OFF_LIMITS`) applies.

use serde_json::Value;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

const OFF_LIMITS: [i64; 6] = [2, 8, 11, 12, 16, 18];

/// The USDⓈ-M demo host and the COIN-M demo host: the only two origins this file signs against.
const FAPI: &str = "https://demo-fapi.binance.com";
const DAPI: &str = "https://demo-dapi.binance.com";

/// The drill's symbols: (host, symbol, what the class is). USDⓈ-M: two TradFi perpetuals, the current quarterly, the
/// weekly the demo lists with a delivery date that has PASSED (the expiry guard's measured case). COIN-M: the
/// perpetual and the current quarterly.
const SYMBOLS: [(&str, &str, &str); 6] = [
    (FAPI, "TSLAUSDT", "TRADIFI_PERPETUAL equity"),
    (FAPI, "XAUUSDT", "TRADIFI_PERPETUAL commodity"),
    (FAPI, "BTCUSDT_261225", "CURRENT_QUARTER"),
    (FAPI, "BSBUSDT_260618", "CURRENT_WEEK with a past deliveryDate"),
    (DAPI, "BTCUSD_PERP", "COIN-M PERPETUAL"),
    (DAPI, "BTCUSD_261225", "COIN-M CURRENT_QUARTER"),
];

fn finding(text: impl std::fmt::Display) {
    println!("FINDING: {text}");
}

fn prefix(host: &str) -> &'static str {
    if host == DAPI { "/dapi/v1" } else { "/fapi/v1" }
}

struct Venue {
    transport: UreqTransport,
    signer: BinanceHmacSigner,
}

impl Venue {
    fn new(creds: &Credentials) -> Self {
        assert_eq!(FAPI, "https://demo-fapi.binance.com");
        assert_eq!(DAPI, "https://demo-dapi.binance.com");
        Self {
            transport: UreqTransport::new("binance"),
            signer: BinanceHmacSigner::new(creds, now_ms),
        }
    }

    fn call(
        &self,
        host: &str,
        method: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        assert!(host == FAPI || host == DAPI, "demo hosts only");
        self.transport.signed(host, path, method, params, &self.signer)
    }

    fn public(&self, host: &str, path: &str, params: &[(&str, String)]) -> Value {
        self.transport.public(host, path, params).unwrap_or_else(|e| panic!("GET {path}: {e}"))
    }

    /// One exchangeInfo row (the whole list is read and filtered: both hosts ignore `symbol=` here).
    fn row(&self, host: &str, symbol: &str) -> Option<Value> {
        let info = self.public(host, &format!("{}/exchangeInfo", prefix(host)), &[]);
        info["symbols"].as_array()?.iter().find(|r| r["symbol"] == symbol).cloned()
    }

    /// `(best bid, best ask)` off the public book ticker (the COIN-M host answers an array).
    fn touch(&self, host: &str, symbol: &str) -> Option<(f64, f64)> {
        let t = self.public(
            host,
            &format!("{}/ticker/bookTicker", prefix(host)),
            &[("symbol", symbol.to_string())],
        );
        let row = t.as_array().and_then(|a| a.first()).unwrap_or(&t);
        let num = |k: &str| row.get(k)?.as_str()?.parse::<f64>().ok();
        Some((num("bidPrice")?, num("askPrice")?))
    }

    /// The signed position amount of `symbol` (sum of its rows), `None` when the read failed.
    fn position(&self, host: &str, symbol: &str) -> Option<f64> {
        let (path, params) = if host == DAPI {
            ("/dapi/v1/positionRisk", vec![])
        } else {
            ("/fapi/v2/positionRisk", vec![("symbol", symbol.to_string())])
        };
        let rows = self.call(host, "GET", path, &params).ok()?;
        Some(
            rows.as_array()?
                .iter()
                .filter(|r| r["symbol"] == symbol)
                .filter_map(|r| r["positionAmt"].as_str()?.parse::<f64>().ok())
                .sum(),
        )
    }
}

/// The resolved demo account, or `None` (with a `SKIP:` line) when it is absent or fenced off.
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

/// Cancels what the drill rested and flattens what it opened, on [`SYMBOLS`] only. Runs on a failing test too and
/// NEVER panics (a second panic while unwinding aborts before the cleanup is done).
struct LeaveFlat {
    creds: Credentials,
}

impl Drop for LeaveFlat {
    fn drop(&mut self) {
        let v = Venue::new(&self.creds);
        for (host, symbol, _) in SYMBOLS {
            let p = prefix(host);
            let _ = v.call(
                host,
                "DELETE",
                &format!("{p}/allOpenOrders"),
                &[("symbol", symbol.to_string())],
            );
            if let Some(amt) = v.position(host, symbol).filter(|a| *a != 0.0) {
                let side = if amt > 0.0 { "SELL" } else { "BUY" };
                let r = v.call(
                    host,
                    "POST",
                    &format!("{p}/order"),
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

fn s<'a>(row: &'a Value, k: &str) -> &'a str {
    row.get(k).and_then(Value::as_str).unwrap_or("")
}

fn filter<'a>(row: &'a Value, kind: &str) -> Option<&'a Value> {
    row["filters"].as_array()?.iter().find(|f| f["filterType"] == kind)
}

fn num(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_str).and_then(|x| x.parse().ok()).unwrap_or(0.0)
}

/// Round `x` UP to a multiple of `step`, spelled with the digits the step has (no float tail on the wire).
fn ceil_to(x: f64, step: f64) -> String {
    let n = (x / step - 1e-9).ceil();
    let digits = format!("{step}").split('.').nth(1).map_or(0, str::len);
    format!("{:.*}", digits, n * step)
}

#[test]
#[ignore = "live demo drill: network + demo credentials; run by hand"]
fn binance_classes_census() {
    let Some(creds) = account() else { return };
    let v = Venue::new(&creds);
    for (host, symbol, class) in SYMBOLS {
        match v.row(host, symbol) {
            None => finding(format!("{symbol} ({class}): NOT in the demo exchangeInfo of {host}")),
            Some(row) => finding(format!(
                "{symbol} ({class}): contractType={} status={} deliveryDate={} marginAsset={} \
                 quoteAsset={} contractSize={} pricePrecision={} quantityPrecision={}",
                s(&row, "contractType"),
                row.get("status")
                    .or(row.get("contractStatus"))
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                row["deliveryDate"],
                s(&row, "marginAsset"),
                s(&row, "quoteAsset"),
                row["contractSize"],
                row["pricePrecision"],
                row["quantityPrecision"],
            )),
        }
        finding(format!("{symbol}: touch {:?}", v.touch(host, symbol)));
    }
    for host in [FAPI, DAPI] {
        let p = prefix(host);
        finding(format!(
            "{host} positionSide/dual: {:?}",
            v.call(host, "GET", &format!("{p}/positionSide/dual"), &[])
        ));
    }
    // The COIN-M wallet and account, as the demo host states them (balances are demo figures).
    match v.call(DAPI, "GET", "/dapi/v1/balance", &[]) {
        Ok(rows) => {
            for r in rows.as_array().into_iter().flatten() {
                finding(format!(
                    "dapi balance {} balance={} withdrawAvailable={} crossWalletBalance={}",
                    s(r, "asset"),
                    s(r, "balance"),
                    s(r, "availableBalance"),
                    s(r, "crossWalletBalance")
                ));
            }
        }
        Err(e) => finding(format!("dapi balance refused: {e}")),
    }
    match v.call(DAPI, "GET", "/dapi/v1/positionRisk", &[]) {
        Ok(rows) => {
            for r in rows
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| SYMBOLS.iter().any(|(h, sym, _)| *h == DAPI && r["symbol"] == *sym))
            {
                finding(format!(
                    "dapi positionRisk {} amt={} marginType={} leverage={} marginAsset={}",
                    s(r, "symbol"),
                    s(r, "positionAmt"),
                    s(r, "marginType"),
                    s(r, "leverage"),
                    s(r, "marginAsset")
                ));
            }
        }
        Err(e) => finding(format!("dapi positionRisk refused: {e}")),
    }
    for (host, symbol, _) in SYMBOLS.iter().filter(|(h, ..)| *h == FAPI) {
        finding(format!(
            "{symbol} positionRisk: {:?}",
            v.call(host, "GET", "/fapi/v2/positionRisk", &[("symbol", (*symbol).to_string())])
                .map(|r| r.to_string().chars().take(400).collect::<String>())
        ));
    }
    // The user-data stream of the COIN-M host: a listen key is the proof the stream exists for the account.
    match v.call(DAPI, "POST", "/dapi/v1/listenKey", &[]) {
        Ok(r) => {
            finding(format!("dapi listenKey: granted ({} chars)", s(&r, "listenKey").len()));
            let _ = v.call(DAPI, "DELETE", "/dapi/v1/listenKey", &[]);
        }
        Err(e) => finding(format!("dapi listenKey refused: {e}")),
    }
}

/// **Signs the TradFi-Perps agreement on the DEMO account** (`POST /fapi/v1/stock/contract`, the one verb the
/// venue documents for it), after proving the account has not signed it: the venue answers every order on a
/// `TRADIFI_PERPETUAL` symbol `-4411` until then. An agreement cannot be unsigned, so this is its own test, run
/// by hand on purpose and only ever against the demo host (the file asserts it).
#[test]
#[ignore = "live demo drill: signs an agreement on the demo account; run by hand, once"]
fn binance_classes_sign_the_tradfi_agreement_on_demo() {
    let Some(creds) = account() else { return };
    let v = Venue::new(&creds);
    let probe = |label: &str| {
        let (host, symbol) = (FAPI, "XAUUSDT");
        let Some((bid, _)) = v.touch(host, symbol) else { return };
        let price = ceil_to(bid * 0.985, 0.01);
        let r = v.call(
            host,
            "POST",
            "/fapi/v1/order",
            &[
                ("symbol", symbol.to_string()),
                ("side", "BUY".to_string()),
                ("type", "LIMIT".to_string()),
                ("timeInForce", "GTC".to_string()),
                ("quantity", "0.002".to_string()),
                ("price", price),
                ("newClientOrderId", format!("vikeagr{}", now_ms() % 1_000_000_000)),
            ],
        );
        finding(format!("{label}: far limit on {symbol}: {r:?}"));
        if let Ok(o) = r {
            let _ = v.call(
                host,
                "DELETE",
                "/fapi/v1/order",
                &[("symbol", symbol.to_string()), ("orderId", o["orderId"].to_string())],
            );
        }
    };
    probe("before signing");
    let signed = v.call(FAPI, "POST", "/fapi/v1/stock/contract", &[]);
    finding(format!("POST /fapi/v1/stock/contract (demo): {signed:?}"));
    probe("after signing");
}

/// The venue answered `-2027` ("Exceeded the maximum allowable position at current leverage") to a minimum order
/// on `TSLAUSDT` (an equity perpetual, out of its underlying's session) and on the expired weekly, while the
/// commodity and the quarterly took theirs. This reads the bracket the venue states for the symbol, then asks
/// the same far limit before and after the account's leverage on the symbol is written (it reads `0` on a symbol
/// the account never traded), to say WHICH of the two it is. Demo only; the symbols are the drill's own.
#[test]
#[ignore = "live demo drill: writes a leverage on the demo account; run by hand"]
fn binance_classes_an_order_refused_for_position_at_current_leverage() {
    let Some(creds) = account() else { return };
    let v = Venue::new(&creds);
    let _guard = LeaveFlat { creds: creds.clone() };
    for symbol in ["TSLAUSDT", "BSBUSDT_260618"] {
        let Some(row) = v.row(FAPI, symbol) else { continue };
        let Some((bid, _)) = v.touch(FAPI, symbol) else { continue };
        let tick = filter(&row, "PRICE_FILTER").map_or(0.0, |f| num(f, "tickSize"));
        let step = filter(&row, "LOT_SIZE").map_or(0.0, |f| num(f, "stepSize"));
        let min_qty = filter(&row, "LOT_SIZE").map_or(0.0, |f| num(f, "minQty"));
        let band = filter(&row, "PERCENT_PRICE").map_or(0.95, |f| num(f, "multiplierDown"));
        let price = ceil_to(bid * (1.0 + band) / 2.0, tick);
        let qty = ceil_to((6.0 / price.parse::<f64>().unwrap_or(1.0)).max(min_qty), step);
        finding(format!(
            "{symbol} leverageBracket: {:?}",
            v.call(FAPI, "GET", "/fapi/v1/leverageBracket", &[("symbol", symbol.to_string())])
                .map(|r| r.to_string().chars().take(600).collect::<String>())
        ));
        let try_order = |label: &str| {
            let r = v.call(
                FAPI,
                "POST",
                "/fapi/v1/order",
                &[
                    ("symbol", symbol.to_string()),
                    ("side", "BUY".to_string()),
                    ("type", "LIMIT".to_string()),
                    ("timeInForce", "GTC".to_string()),
                    ("quantity", qty.clone()),
                    ("price", price.clone()),
                    ("newClientOrderId", format!("vikelev{}", now_ms() % 1_000_000_000)),
                ],
            );
            finding(format!(
                "{symbol} {label}: qty={qty} price={price}: {}",
                match &r {
                    Ok(o) => format!("accepted status={}", s(o, "status")),
                    Err(e) => format!("REFUSED {e}"),
                }
            ));
            if let Ok(o) = r {
                let _ = v.call(
                    FAPI,
                    "DELETE",
                    "/fapi/v1/order",
                    &[("symbol", symbol.to_string()), ("orderId", o["orderId"].to_string())],
                );
            }
        };
        try_order("with the leverage the account holds (0)");
        let set = v.call(
            FAPI,
            "POST",
            "/fapi/v1/leverage",
            &[("symbol", symbol.to_string()), ("leverage", "2".to_string())],
        );
        finding(format!("{symbol} POST leverage 2: {set:?}"));
        try_order("after the leverage write");
    }
}

#[test]
#[ignore = "live demo drill: network + demo credentials; run by hand"]
fn binance_classes_far_limit_echo() {
    let Some(creds) = account() else { return };
    let v = Venue::new(&creds);
    let _guard = LeaveFlat { creds: creds.clone() };
    for (host, symbol, class) in SYMBOLS {
        let p = prefix(host);
        let Some(row) = v.row(host, symbol) else {
            finding(format!("{symbol} ({class}): not listed on {host}; nothing to place"));
            continue;
        };
        let Some((bid, _ask)) = v.touch(host, symbol) else {
            finding(format!("{symbol}: no book"));
            continue;
        };
        let tick = filter(&row, "PRICE_FILTER").map_or(0.0, |f| num(f, "tickSize"));
        let step = filter(&row, "LOT_SIZE").map_or(0.0, |f| num(f, "stepSize"));
        let min_qty = filter(&row, "LOT_SIZE").map_or(0.0, |f| num(f, "minQty"));
        let band = filter(&row, "PERCENT_PRICE").map_or(0.95, |f| num(f, "multiplierDown"));
        let min_notional = filter(&row, "MIN_NOTIONAL").map_or(0.0, |f| num(f, "notional"));
        // Inside the PERCENT_PRICE band (above its floor), far under the touch: rests, cannot fill.
        let price_f = bid * (1.0 + band) / 2.0;
        let price = ceil_to(price_f, tick);
        let price_n: f64 = price.parse().unwrap_or(price_f);
        let mut qty_f = min_qty;
        if min_notional > 0.0 && qty_f * price_n < min_notional * 1.05 {
            qty_f = min_notional * 1.05 / price_n;
        }
        let qty = ceil_to(qty_f.max(min_qty), step);
        let coid = format!("vikecls{}{}", now_ms() % 1_000_000_000, symbol.len());
        let placed = v.call(
            host,
            "POST",
            &format!("{p}/order"),
            &[
                ("symbol", symbol.to_string()),
                ("side", "BUY".to_string()),
                ("type", "LIMIT".to_string()),
                ("timeInForce", "GTC".to_string()),
                ("quantity", qty.clone()),
                ("price", price.clone()),
                ("newClientOrderId", coid.clone()),
                ("newOrderRespType", "RESULT".to_string()),
            ],
        );
        finding(format!(
            "{symbol} ({class}) POST order price={price} qty={qty} (bid {bid}, tick {tick}, step {step}, \
             band {band}): {}",
            match &placed {
                Ok(r) => r.to_string(),
                Err(e) => format!("REFUSED {e}"),
            }
        ));
        if placed.is_err() {
            continue;
        }
        let echo = v.call(
            host,
            "GET",
            &format!("{p}/order"),
            &[("symbol", symbol.to_string()), ("origClientOrderId", coid.clone())],
        );
        finding(format!("{symbol} GET order: {echo:?}"));
        if let Ok(e) = &echo {
            let same_price = s(e, "price").parse::<f64>().ok() == price.parse::<f64>().ok();
            let same_qty = s(e, "origQty").parse::<f64>().ok() == qty.parse::<f64>().ok();
            finding(format!("{symbol} echo equals the wire: price={same_price} qty={same_qty}"));
        }
        let cancelled = v.call(
            host,
            "DELETE",
            &format!("{p}/order"),
            &[("symbol", symbol.to_string()), ("origClientOrderId", coid.clone())],
        );
        finding(format!(
            "{symbol} DELETE order: {}",
            match &cancelled {
                Ok(r) => s(r, "status").to_string(),
                Err(e) => format!("REFUSED {e}"),
            }
        ));
        let after = v.call(
            host,
            "GET",
            &format!("{p}/order"),
            &[("symbol", symbol.to_string()), ("origClientOrderId", coid)],
        );
        finding(format!(
            "{symbol} after cancel: {}",
            match &after {
                Ok(r) => s(r, "status").to_string(),
                Err(e) => format!("{e}"),
            }
        ));
    }
}

#[test]
#[ignore = "live demo drill: network + demo credentials; run by hand"]
fn binance_classes_minimum_fill_and_flatten() {
    let Some(creds) = account() else { return };
    let v = Venue::new(&creds);
    let _guard = LeaveFlat { creds: creds.clone() };
    for (host, symbol, class) in SYMBOLS {
        let p = prefix(host);
        let Some(row) = v.row(host, symbol) else { continue };
        let Some((bid, ask)) = v.touch(host, symbol) else { continue };
        let step = filter(&row, "LOT_SIZE").map_or(0.0, |f| num(f, "stepSize"));
        let min_qty = filter(&row, "LOT_SIZE").map_or(0.0, |f| num(f, "minQty"));
        let min_notional = filter(&row, "MIN_NOTIONAL").map_or(0.0, |f| num(f, "notional"));
        let mut qty_f = min_qty;
        if min_notional > 0.0 && qty_f * ask < min_notional * 1.2 {
            qty_f = min_notional * 1.2 / ask;
        }
        let qty = ceil_to(qty_f.max(min_qty), step);
        let before = v.position(host, symbol);
        if before.is_none_or(|b| b != 0.0) {
            finding(format!("{symbol}: not flat or unreadable ({before:?}); skipped"));
            continue;
        }
        let open = v.call(
            host,
            "POST",
            &format!("{p}/order"),
            &[
                ("symbol", symbol.to_string()),
                ("side", "BUY".to_string()),
                ("type", "MARKET".to_string()),
                ("quantity", qty.clone()),
                ("newOrderRespType", "RESULT".to_string()),
            ],
        );
        finding(format!("{symbol} ({class}) MARKET BUY {qty} (touch {bid}/{ask}): {open:?}"));
        if open.is_err() {
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(700));
        let trades = v.call(
            host,
            "GET",
            &format!("{p}/userTrades"),
            &[("symbol", symbol.to_string()), ("limit", "5".to_string())],
        );
        finding(format!("{symbol} userTrades after the open: {trades:?}"));
        let held = v.position(host, symbol).unwrap_or(0.0);
        finding(format!("{symbol} position after the open: {held}"));
        if held != 0.0 {
            let close = v.call(
                host,
                "POST",
                &format!("{p}/order"),
                &[
                    ("symbol", symbol.to_string()),
                    ("side", "SELL".to_string()),
                    ("type", "MARKET".to_string()),
                    ("quantity", format!("{}", held.abs())),
                    ("reduceOnly", "true".to_string()),
                    ("newOrderRespType", "RESULT".to_string()),
                ],
            );
            finding(format!("{symbol} reduce-only close: {close:?}"));
            std::thread::sleep(std::time::Duration::from_millis(700));
            let trades = v.call(
                host,
                "GET",
                &format!("{p}/userTrades"),
                &[("symbol", symbol.to_string()), ("limit", "5".to_string())],
            );
            finding(format!("{symbol} userTrades after the close: {trades:?}"));
        }
        finding(format!("{symbol} position after the close: {:?}", v.position(host, symbol)));
    }
}
