//! LIVE DEMO drill for the bybit position-mode / margin-mode heal (`crates/bridges/bybit/src/modes.rs`).
//!
//!     cargo test -p vike-bybit --test bybit_modes_smoke -- --ignored --nocapture --test-threads=1
//!
//! HAND-RUN ONLY: no workflow lists this file. DEMO host and DEMO tier only (`assert_demo_only`); the
//! account is the venue's default demo account, or the ONE active labelled `demo` row (the CI box's is
//! labelled `DEMO`: the same `demo_label` rule as `bybit_restore_smoke.rs`), self-skipping when absent.
//! `--test-threads=1`: the tests share one demo account.
//!
//! * `bybit_modes_read_smoke` — READ-ONLY. Prints the sanitized `account/info`, `position/list`
//!   (symbol-scoped and account-wide) and `order/realtime` answers: the shapes `modes.rs`'s parsers read.
//! * `bybit_wire_drill` — the venue's raw answers, no bridge code: a mode switch each way, a repeat of it,
//!   the order that names `positionIdx 0` on a hedge symbol, and `set-margin-mode`.
//! * `bybit_hedge_symbol_is_set_one_way_and_an_order_goes_through` — the BRIDGE: `SYMBOL` is put in HEDGE
//!   mode while flat; (A) an exec client mounted on it sets one-way at its start and its far limit order is
//!   accepted and cancelled; (B) a client mounted elsewhere does the same at the first order for `SYMBOL`;
//!   (C) with an order resting on the hedge symbol the order is REFUSED in words and nothing changes, and
//!   after the order is gone the next client puts the mode right.
//! * `bybit_isolated_account_is_set_cross_by_the_bridge` — flips the ACCOUNT to `ISOLATED_MARGIN` while the
//!   whole linear book is flat and empty, and asserts a client's start puts it back to `REGULAR_MARGIN`.
//!   The host answered `ISOLATED_MARGIN` with `10032 Demo trading are not supported` on 2026-07-19 and took
//!   it on 2026-10-26: the test self-skips, printing the answer, when it refuses.
//!
//! The mutating tests flip the POSITION MODE of `SYMBOL`, a minor symbol no engine of the running node
//! mounts, and put it back (and cancel what they rest on it) from a drop guard even when an assertion
//! fails. Every one self-skips while the account holds a position or a resting order on the symbols it uses.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bybit::exec::BybitExecutionClient;
use vike_bybit::perp::{BybitPerpRest, DEMO_REST, endpoints, parse_bybit_perp_instruments};
use vike_bybit::transport::{BybitTransport, UreqBybitTransport};
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, SymbolProperties, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

/// A minor symbol no engine of the running node mounts: the drill flips ITS position mode.
const SYMBOL: &str = "DOGEUSDT";
/// The symbol the exec client is MOUNTED on in the legs that test the first order of another symbol.
const MOUNT: &str = "XRPUSDT";
/// The only tier this file resolves an account at.
const TIER: AccountTier = AccountTier::Demo;

fn assert_demo_only() {
    assert_eq!(TIER, AccountTier::Demo, "a modes drill resolves the DEMO account only");
    assert_eq!(endpoints(false).0, DEMO_REST, "the demo host moved: re-check before signing");
}

/// The label of the bybit DEMO account: the unlabelled one when the store has it, else the ONE active
/// labelled `demo` row (`bybit_restore_smoke.rs`'s `demo_label`, same rule).
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

fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &demo_label(&directory), TIER);
    let creds = api_credentials(&account, "bybit");
    if creds.is_none() {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
    }
    creds
}

/// One signed call on the demo host whose answer is ALWAYS an envelope: a non-2xx answer or a transport
/// failure becomes `{"retCode": <code>, "retMsg": ..., "transport": true}` so a refusal is printed, not unwrapped.
struct Wire {
    signer: BybitV5Signer,
    transport: UreqBybitTransport,
}

impl Wire {
    fn new(creds: &Credentials) -> Self {
        Wire {
            signer: BybitV5Signer::new(creds, now_ms),
            transport: UreqBybitTransport::new()
                .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        }
    }

    fn raw(&self, method: &str, path: &str, params: &[(&str, Value)]) -> Value {
        match self.transport.signed(DEMO_REST, path, method, params, &self.signer) {
            Ok(envelope) => envelope,
            Err(e) => json!({"retCode": e.code, "retMsg": e.msg, "transport": true}),
        }
    }

    fn get(&self, path: &str, params: &[(&str, Value)]) -> Value {
        self.raw("GET", path, params)
    }

    fn post(&self, path: &str, params: &[(&str, Value)]) -> Value {
        self.raw("POST", path, params)
    }

    fn switch(&self, symbol: &str, mode: i64) -> Value {
        self.post(
            "/v5/position/switch-mode",
            &[("category", json!("linear")), ("symbol", json!(symbol)), ("mode", json!(mode))],
        )
    }

    /// `SYMBOL` into HEDGE mode (`110025` = it already was), read back.
    fn to_hedge(&self) {
        let answer = self.switch(SYMBOL, 3);
        assert!(
            matches!(code(&answer), 0 | 110025),
            "the demo host refused the switch to hedge: {answer}"
        );
        assert_eq!(self.position_mode(SYMBOL), Some(3));
    }

    /// `Some(0)` one-way, `Some(3)` hedge, `None` anything this drill will not mutate under.
    fn position_mode(&self, symbol: &str) -> Option<i64> {
        let idx: Vec<i64> = rows(&self.get("/v5/position/list", &linear(symbol)))
            .iter()
            .filter_map(|r| r.get("positionIdx").and_then(Value::as_i64))
            .collect();
        match idx.as_slice() {
            [0] => Some(0),
            [1, 2] | [2, 1] => Some(3),
            _ => None,
        }
    }

    /// True when `symbol` is flat and nothing rests on it.
    fn flat_and_empty(&self, symbol: &str) -> bool {
        let flat = rows(&self.get("/v5/position/list", &linear(symbol))).iter().all(|r| {
            r.get("size")
                .and_then(Value::as_str)
                .is_none_or(|s| s.parse::<f64>().unwrap_or(1.0) == 0.0)
        });
        flat && rows(&self.get("/v5/order/realtime", &linear(symbol))).is_empty()
    }

    /// The leverage the venue holds for `symbol` now.
    fn held_leverage(&self, symbol: &str) -> Option<f64> {
        rows(&self.get("/v5/position/list", &linear(symbol)))
            .first()
            .and_then(|r| r.get("leverage"))
            .and_then(Value::as_str)
            .and_then(|l| l.parse().ok())
    }

    /// The account's `marginMode` verbatim.
    fn margin_mode(&self) -> Option<String> {
        self.get("/v5/account/info", &[])["result"]["marginMode"].as_str().map(str::to_string)
    }

    /// True when no linear position exists and no linear order rests, for either settle coin.
    fn account_flat_and_empty(&self) -> bool {
        ["USDT", "USDC"].iter().all(|coin| {
            let by_coin = [("category", json!("linear")), ("settleCoin", json!(coin))];
            let positions = rows(&self.get("/v5/position/list", &by_coin));
            let orders = rows(&self.get("/v5/order/realtime", &by_coin));
            positions.is_empty() && orders.is_empty()
        })
    }
}

fn show(what: &str, raw: &Value) {
    let s = capture_frame("bybit", what, raw);
    println!(
        "\n=== {what} ===\nredacted: {:?}\n{}",
        s.redacted_paths,
        serde_json::to_string_pretty(&s.frame).unwrap_or_default()
    );
}

fn code(envelope: &Value) -> i64 {
    envelope.get("retCode").and_then(Value::as_i64).unwrap_or(-1)
}

fn rows(envelope: &Value) -> Vec<Value> {
    envelope
        .get("result")
        .and_then(|r| r.get("list"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn linear(symbol: &str) -> [(&'static str, Value); 2] {
    [("category", json!("linear")), ("symbol", json!(symbol))]
}

/// What the drill needs of a symbol to rest a far bid on it: its grid, and a size and price that cannot fill.
struct Bid {
    properties: SymbolProperties,
    qty: f64,
    price: f64,
}

fn far_bid(wire: &Wire, symbol: &str) -> Bid {
    let info = wire.get("/v5/market/instruments-info", &linear(symbol));
    let properties = parse_bybit_perp_instruments(&info)
        .get(symbol)
        .unwrap_or_else(|| panic!("no instrument row for {symbol}: {info}"))
        .properties;
    let last: f64 = rows(&wire.get("/v5/market/tickers", &linear(symbol)))
        .first()
        .and_then(|t| t.get("lastPrice"))
        .and_then(Value::as_str)
        .and_then(|p| p.parse().ok())
        .expect("a last price");
    let price = (last * 0.9 / properties.tick_size).round() * properties.tick_size;
    let qty = ((properties.min_notional * 1.5 / price).max(properties.min_qty)
        / properties.step_size)
        .ceil()
        * properties.step_size;
    Bid { properties, qty, price }
}

fn request(coid: &str, symbol: &str, bid: &Bid) -> OrderRequest {
    serde_json::from_value(json!({
        "client_order_id": coid, "venue": "bybit", "symbol": symbol, "side": 1,
        "qty": bid.qty, "order_type": "limit", "price": bid.price, "ts": now_ms()
    }))
    .expect("an order request")
}

fn coid(tag: &str) -> String {
    format!("mode{tag}{}", now_ms() % 100_000_000)
}

/// Puts the account back as found: cancels what the drill rests on `SYMBOL`, restores its position mode
/// and (when the margin test flipped it) the account's margin mode. Runs from `Drop`, so a panic in the
/// middle of a leg still ends with the account as it was; never panics itself.
struct Restore<'a> {
    wire: &'a Wire,
    original: i64,
    margin: Option<String>,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        let cancelled = self.wire.post(
            "/v5/order/cancel-all",
            &[("category", json!("linear")), ("symbol", json!(SYMBOL))],
        );
        eprintln!("restore: cancel-all {SYMBOL} -> {}", code(&cancelled));
        let answer = self.wire.switch(SYMBOL, self.original);
        // 0 = restored, 110025 = it was already there.
        if matches!(code(&answer), 0 | 110025) {
            eprintln!(
                "restore: {SYMBOL} position mode {} (answer {})",
                self.original,
                code(&answer)
            );
        } else {
            eprintln!(
                "RESTORE FAILED: set the DEMO account's {SYMBOL} position mode to {} by hand: {answer}",
                self.original
            );
        }
        if let Some(mode) = &self.margin {
            let answer =
                self.wire.post("/v5/account/set-margin-mode", &[("setMarginMode", json!(mode))]);
            if code(&answer) == 0 {
                eprintln!("restore: account margin mode {mode}");
            } else {
                eprintln!(
                    "RESTORE FAILED: set the DEMO account's margin mode to {mode} by hand: {answer}"
                );
            }
        }
    }
}

/// The next event for `coid` of the kind `want`, within `secs`. `poll` drains the client's event channel.
fn wait_for(
    poll: &mut dyn FnMut() -> Option<Ingest>,
    coid: &str,
    want: &str,
    secs: u64,
) -> Option<Event> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match poll() {
            Some(Ingest::Event(e)) => {
                let (c, kind) = match &e {
                    Event::OrderSubmitted(x) => (&x.client_order_id, "Submitted"),
                    Event::OrderAccepted(x) => (&x.client_order_id, "Accepted"),
                    Event::OrderRejected(x) => (&x.client_order_id, "Rejected"),
                    Event::OrderCanceled(x) => (&x.client_order_id, "Canceled"),
                    _ => continue,
                };
                tracing::info!(target: "vike_bybit", "adapter event: {c} -> {kind}");
                if c == coid && kind == want {
                    return Some(e);
                }
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    None
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_modes_read_smoke() {
    vike_log::test_init();
    assert_demo_only();
    let Some(creds) = load_demo_creds() else { return };
    let wire = Wire::new(&creds);
    show("account-info", &wire.get("/v5/account/info", &[]));
    show(&format!("position-list {SYMBOL}"), &wire.get("/v5/position/list", &linear(SYMBOL)));
    show(&format!("order-realtime {SYMBOL}"), &wire.get("/v5/order/realtime", &linear(SYMBOL)));
    for coin in ["USDT", "USDC"] {
        let params = [("category", json!("linear")), ("settleCoin", json!(coin))];
        show(&format!("position-list settleCoin={coin}"), &wire.get("/v5/position/list", &params));
        show(
            &format!("order-realtime settleCoin={coin}"),
            &wire.get("/v5/order/realtime", &params),
        );
    }
}

/// WIRE FACTS, with no bridge code: what the venue answers to a switch-mode in each direction, to a
/// repeat of it, to an order that names `positionIdx 0` on a hedge-mode symbol, and to
/// `set-margin-mode` asked for the mode the account already has.
#[test]
#[ignore = "network + demo creds + flips a demo symbol's position mode and puts it back — run manually (see module doc)"]
fn bybit_wire_drill() {
    vike_log::test_init();
    assert_demo_only();
    let Some(creds) = load_demo_creds() else { return };
    let wire = Wire::new(&creds);
    if !wire.flat_and_empty(SYMBOL) {
        tracing::warn!(target: "vike_bybit", "SKIP: the demo account holds a position or a resting order on {SYMBOL}");
        return;
    }
    let Some(original) = wire.position_mode(SYMBOL) else {
        tracing::warn!(target: "vike_bybit", "SKIP: {SYMBOL}'s position mode could not be read (see read smoke)");
        return;
    };
    println!("\n=== {SYMBOL} original position mode: {original} (0 one-way, 3 hedge) ===");
    let _restore = Restore { wire: &wire, original, margin: None };

    show("switch-mode -> hedge (3)", &wire.switch(SYMBOL, 3));
    show(
        &format!("position-list {SYMBOL} in hedge mode"),
        &wire.get("/v5/position/list", &linear(SYMBOL)),
    );
    show("switch-mode -> hedge (3) AGAIN", &wire.switch(SYMBOL, 3));

    // The base behaviour: an order that names positionIdx 0 on a hedge-mode symbol.
    let bid = far_bid(&wire, SYMBOL);
    let id = coid("wire");
    let refused = wire.post(
        "/v5/order/create",
        &[
            ("category", json!("linear")),
            ("symbol", json!(SYMBOL)),
            ("side", json!("Buy")),
            ("orderType", json!("Limit")),
            ("qty", json!(format_to_step_f(bid.qty, bid.properties.step_size))),
            ("price", json!(format_to_step_f(bid.price, bid.properties.tick_size))),
            ("timeInForce", json!("GTC")),
            ("orderLinkId", json!(id)),
            ("positionIdx", json!(0)),
            ("reduceOnly", json!(false)),
        ],
    );
    show("order/create positionIdx 0 on a HEDGE-mode symbol", &refused);
    assert_ne!(
        code(&refused),
        0,
        "the venue took a one-way order on a hedge symbol: re-read the module doc"
    );

    show("switch-mode -> one-way (0)", &wire.switch(SYMBOL, 0));
    show(
        &format!("position-list {SYMBOL} in one-way mode"),
        &wire.get("/v5/position/list", &linear(SYMBOL)),
    );
    show("switch-mode -> one-way (0) AGAIN", &wire.switch(SYMBOL, 0));

    // Asked for the mode the account already has, so nothing changes whatever the host answers.
    let mode = wire.margin_mode().unwrap_or_else(|| "REGULAR_MARGIN".to_string());
    show(
        &format!("set-margin-mode {mode} (the account's own, a no-op)"),
        &wire.post("/v5/account/set-margin-mode", &[("setMarginMode", json!(mode))]),
    );
}

#[test]
#[ignore = "network + demo creds + flips a demo symbol's position mode, rests and cancels far demo orders, puts the mode back — run manually (see module doc)"]
fn bybit_hedge_symbol_is_set_one_way_and_an_order_goes_through() {
    vike_log::test_init();
    assert_demo_only();
    let Some(creds) = load_demo_creds() else { return };
    let wire = Wire::new(&creds);
    for symbol in [SYMBOL, MOUNT] {
        if !wire.flat_and_empty(symbol) {
            tracing::warn!(target: "vike_bybit", "SKIP: the demo account holds a position or a resting order on {symbol}");
            return;
        }
    }
    let (Some(original), Some(mount_mode)) =
        (wire.position_mode(SYMBOL), wire.position_mode(MOUNT))
    else {
        tracing::warn!(target: "vike_bybit", "SKIP: a position mode could not be read (see read smoke)");
        return;
    };
    let (Some(symbol_leverage), Some(mount_leverage)) =
        (wire.held_leverage(SYMBOL), wire.held_leverage(MOUNT))
    else {
        tracing::warn!(target: "vike_bybit", "SKIP: a leverage could not be read (see read smoke)");
        return;
    };
    println!(
        "\n=== before: {SYMBOL} mode {original} {symbol_leverage}x, {MOUNT} mode {mount_mode} {mount_leverage}x ==="
    );
    let _restore = Restore { wire: &wire, original, margin: None };
    let doge = far_bid(&wire, SYMBOL);
    let xrp = far_bid(&wire, MOUNT);

    // BASE: without the rule, an order on a hedge symbol is the venue's own 10001.
    wire.to_hedge();
    let bare = BybitPerpRest {
        category: vike_bybit::data::Category::Linear,
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        base_url: DEMO_REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: doge.properties,
        leverage: symbol_leverage,
    };
    let before = bare.submit_order(&request(&coid("base"), SYMBOL, &doge));
    println!("=== BASE (no rule): {before:?} ===");
    assert!(
        matches!(before.last(), Some(Event::OrderRejected(r)) if r.reason.contains("position idx not match position mode")),
        "{before:?}"
    );

    // A. The MOUNTED symbol is hedge: the client's start sets one-way, before any order.
    let (events, mut rx) = event_channel(512);
    let mut poll = || rx.try_recv().ok();
    let mut client = BybitExecutionClient::spawn_with_recorder(
        creds.clone(),
        SYMBOL.to_string(),
        doge.properties,
        events,
        None,
        None,
        None,
        false,
        symbol_leverage,
        false,
        std::sync::Arc::default(),
    );
    std::thread::sleep(Duration::from_secs(5)); // start: grid, modes, leverage, pump
    let after_start = wire.position_mode(SYMBOL);
    println!(
        "=== A: {SYMBOL} position mode after the start of a client mounted on it: {after_start:?} ==="
    );
    assert_eq!(after_start, Some(0), "the start did not put the mounted symbol in one-way");
    let id = coid("a");
    client.submit(&request(&id, SYMBOL, &doge));
    assert!(wait_for(&mut poll, &id, "Accepted", 15).is_some(), "{id} never Accepted");
    assert!(
        rows(&wire.get("/v5/order/realtime", &linear(SYMBOL))).len() == 1,
        "the accepted order is not resting at the venue"
    );
    client.cancel(&id);
    assert!(wait_for(&mut poll, &id, "Canceled", 15).is_some(), "{id} never Canceled");
    client.detach();

    // B. An ADMITTED symbol is hedge: a client mounted elsewhere sets one-way at the first order for it.
    wire.to_hedge();
    let (events, mut rx) = event_channel(512);
    let mut poll = || rx.try_recv().ok();
    let mut client = BybitExecutionClient::spawn_with_recorder(
        creds.clone(),
        MOUNT.to_string(),
        xrp.properties,
        events,
        None,
        None,
        None,
        false,
        mount_leverage,
        false,
        std::sync::Arc::default(),
    );
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(
        wire.position_mode(SYMBOL),
        Some(3),
        "a client mounted elsewhere must not touch {SYMBOL} before an order"
    );
    let id = coid("b");
    client.submit(&request(&id, SYMBOL, &doge));
    assert!(wait_for(&mut poll, &id, "Accepted", 20).is_some(), "{id} never Accepted");
    let after_order = wire.position_mode(SYMBOL);
    println!(
        "=== B: {SYMBOL} position mode after the first order for it from a client mounted on {MOUNT}: {after_order:?} ==="
    );
    assert_eq!(after_order, Some(0));
    client.cancel(&id);
    assert!(wait_for(&mut poll, &id, "Canceled", 15).is_some(), "{id} never Canceled");
    client.detach();

    // C. A resting order on the hedge symbol: the fix is blocked, the order is refused in words and the
    // mode is left alone; after the order is gone the next client puts it right.
    wire.to_hedge();
    let resting = wire.post(
        "/v5/order/create",
        &[
            ("category", json!("linear")),
            ("symbol", json!(SYMBOL)),
            ("side", json!("Buy")),
            ("orderType", json!("Limit")),
            ("qty", json!(format_to_step_f(doge.qty, doge.properties.step_size))),
            ("price", json!(format_to_step_f(doge.price, doge.properties.tick_size))),
            ("timeInForce", json!("GTC")),
            ("orderLinkId", json!(coid("hedgeleg"))),
            ("positionIdx", json!(1)),
            ("reduceOnly", json!(false)),
        ],
    );
    show("a resting hedge-leg order (positionIdx 1)", &resting);
    assert_eq!(code(&resting), 0, "the hedge-leg order the drill rests was refused");
    let (events, mut rx) = event_channel(512);
    let mut poll = || rx.try_recv().ok();
    let mut client = BybitExecutionClient::spawn_with_recorder(
        creds.clone(),
        MOUNT.to_string(),
        xrp.properties,
        events,
        None,
        None,
        None,
        false,
        mount_leverage,
        false,
        std::sync::Arc::default(),
    );
    std::thread::sleep(Duration::from_secs(5));
    let id = coid("c");
    client.submit(&request(&id, SYMBOL, &doge));
    let rejected =
        wait_for(&mut poll, &id, "Rejected", 20).expect("the blocked order was not refused");
    let Event::OrderRejected(rejected) = rejected else { unreachable!() };
    println!("=== C: the refusal in words: {} ===", rejected.reason);
    for word in [SYMBOL, "HEDGE", "resting order"] {
        assert!(rejected.reason.contains(word), "{word}: {}", rejected.reason);
    }
    assert_eq!(wire.position_mode(SYMBOL), Some(3), "a refused symbol's mode must be left alone");
    client.detach();
    show(
        "cancel-all of the resting hedge-leg order",
        &wire.post(
            "/v5/order/cancel-all",
            &[("category", json!("linear")), ("symbol", json!(SYMBOL))],
        ),
    );
    let (events, mut rx) = event_channel(512);
    let mut poll = || rx.try_recv().ok();
    let mut client = BybitExecutionClient::spawn_with_recorder(
        creds.clone(),
        MOUNT.to_string(),
        xrp.properties,
        events,
        None,
        None,
        None,
        false,
        mount_leverage,
        false,
        std::sync::Arc::default(),
    );
    std::thread::sleep(Duration::from_secs(5));
    let id = coid("d");
    client.submit(&request(&id, SYMBOL, &doge));
    assert!(
        wait_for(&mut poll, &id, "Accepted", 20).is_some(),
        "{id} never Accepted after the order was gone"
    );
    assert_eq!(wire.position_mode(SYMBOL), Some(0));
    client.cancel(&id);
    assert!(wait_for(&mut poll, &id, "Canceled", 15).is_some(), "{id} never Canceled");
    client.detach();

    // Left as found: the guard puts {SYMBOL} back; {MOUNT} and the leverages were never changed.
    assert_eq!(wire.position_mode(MOUNT), Some(mount_mode));
    assert_eq!(wire.held_leverage(MOUNT), Some(mount_leverage));
    assert_eq!(wire.held_leverage(SYMBOL), Some(symbol_leverage));
}

#[test]
#[ignore = "network + demo creds + flips the DEMO ACCOUNT's margin mode for seconds and puts it back — run manually (see module doc)"]
fn bybit_isolated_account_is_set_cross_by_the_bridge() {
    vike_log::test_init();
    assert_demo_only();
    let Some(creds) = load_demo_creds() else { return };
    let wire = Wire::new(&creds);
    let Some(original) = wire.position_mode(SYMBOL) else {
        tracing::warn!(target: "vike_bybit", "SKIP: {SYMBOL}'s position mode could not be read");
        return;
    };
    let Some(leverage) = wire.held_leverage(SYMBOL) else {
        tracing::warn!(target: "vike_bybit", "SKIP: {SYMBOL}'s leverage could not be read");
        return;
    };
    if wire.margin_mode().as_deref() != Some("REGULAR_MARGIN") || !wire.account_flat_and_empty() {
        tracing::warn!(target: "vike_bybit", "SKIP: the account is not REGULAR_MARGIN, flat and empty");
        return;
    }
    let bid = far_bid(&wire, SYMBOL);
    let mut restore = Restore { wire: &wire, original, margin: None };
    let flip =
        wire.post("/v5/account/set-margin-mode", &[("setMarginMode", json!("ISOLATED_MARGIN"))]);
    show("set-margin-mode ISOLATED_MARGIN (the drill's own flip)", &flip);
    if code(&flip) != 0 {
        println!(
            "=== the host does not take ISOLATED_MARGIN ({}): the bridge's margin write is proven by fixtures only ===",
            code(&flip)
        );
        return;
    }
    restore.margin = Some("REGULAR_MARGIN".to_string());
    assert_eq!(wire.margin_mode().as_deref(), Some("ISOLATED_MARGIN"));

    let (events, _rx) = event_channel(512);
    let mut client = BybitExecutionClient::spawn_with_recorder(
        creds.clone(),
        SYMBOL.to_string(),
        bid.properties,
        events,
        None,
        None,
        None,
        false,
        leverage,
        false,
        std::sync::Arc::default(),
    );
    std::thread::sleep(Duration::from_secs(5));
    let after = wire.margin_mode();
    println!(
        "=== margin mode after the start of a client on an ISOLATED_MARGIN account: {after:?} ==="
    );
    client.detach();
    assert_eq!(
        after.as_deref(),
        Some("REGULAR_MARGIN"),
        "the start did not put the account in cross"
    );
}
