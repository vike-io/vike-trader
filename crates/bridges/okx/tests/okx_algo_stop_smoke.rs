//! A resting STOP on the OKX DEMO account: place it, list it, cancel it through the bridge, prove the
//! algo list empties. The live proof of `OkxPerpRest::cancel_order`'s `cancel-algos` retry, the
//! `order-algo` lookup and the `orders-algo-pending` order report (`tests/offline/algo_stop.rs` pins the
//! same logic offline).
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --features exec --test okx_algo_stop_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING (`BTC-USDT-SWAP`), DEMO ONLY: one `conditional` SELL stop at HALF the last price (it
//! cannot trigger in a smoke's lifetime) for the instrument's MINIMUM size, `reduceOnly` (the demo account
//! is flat; if the venue refuses a reduce-only stop with no position the smoke says so and retries the
//! same far stop without the flag). Double-gated like every `*_smoke.rs` in this crate: network + the DEMO
//! account's creds via `vike_secrets::smoke_directory` (asked for `AccountTier::Demo`: no demo keys, no
//! run), a `tracing::warn!` and an early return when absent. Every transport is
//! `UreqOkxTransport::new(true)`, which always sends `x-simulated-trading: 1` (demo and mainnet share the
//! host, the HEADER is the switch). THAT pair is the demo-only fence; the two `assert!`s in `build_rest`
//! only re-read values the lines above them just set, so they catch a later edit of this file and prove
//! nothing about the keys or the account. Credentials are never printed: only venue words (codes,
//! messages, ids of this smoke's own orders) reach stdout.
//!
//! What it MEASURES (printed as `MEASURED ...`; the vendor docs are silent on all of them):
//!   * what the regular `cancel-order` answers to the stop's coid, and whether the stop survives it;
//!   * whether `orders-pending` lists the stop;
//!   * what `cancel-algos` and `GET order-algo` answer to an unknown algo id;
//!   * the `state` word `GET order-algo` gives the stop AFTER the cancel (the bridge's second cancel
//!     reads it: only `canceled` counts as gone).
//!
//! The answers are what the bridge's code sets are rebuilt from (`NOT_FOUND` and `algo_unknown` in
//! `crates/bridges/okx/src/perp.rs` and `crates/bridges/okx/src/perp/stop_algo.rs`): both were
//! guesses until this smoke's run of 2026-10-23 on the demo account showed the words and found them
//! correct (the regular cancel of the stop `51400` with the stop surviving, `cancel-algos` of an
//! unknown id `51400`, `order-algo` of an unknown id `51603`, the cancelled stop's state `canceled`,
//! `orders-pending` not listing the stop). A request-shape refusal of
//! `orders-algo-pending` (`ordType` + `instId`) panics here with the venue's words. **Measure first,
//! assert last:** the unknown-id lines print right after the stop is placed, and the cancel answers
//! are collected and printed before the final asserts, so a wrong guess cannot hide the words that
//! rebuild the sets. How to READ the output against the code is
//! `crates/bridges/okx/CLAUDE.md`'s "Test binaries" (the smoke-run checklist).
//!
//! CLEANUP: a `Drop` guard cancels the stop on every exit path, first through the bridge, then straight
//! on `cancel-algos` (idempotent). A run killed from outside leaves a half-price SELL stop resting on the
//! demo account: cancel it by hand.

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_exec::recon::ReconClient;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderStatus, OrderType, now_ms};
use vike_okx::OkxReconClient;
use vike_okx::perp::{
    OkxPerpRest, PATH_CANCEL_ALGOS, PATH_INSTRUMENTS, PATH_ORDER_ALGO, PATH_ORDER_CANCEL,
    PATH_ORDERS_ALGO_PENDING, PATH_ORDERS_PENDING, REST, parse_okx_perp_instruments,
};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};
use vike_secrets::credentials::{Credentials, api_credentials};

const SYMBOL: &str = "BTC-USDT-SWAP";

type SwapRest = OkxPerpRest<UreqOkxTransport>;

fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "okx");
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

/// The demo (simulated-trading) SWAP client. DEMO ONLY by construction: the credentials were asked
/// for as `AccountTier::Demo` (`load_demo_creds`) and every transport is `UreqOkxTransport::new(true)`
/// (the simulated-trading header, always on). The asserts below are tripwires for a later edit of
/// this function, not a second fence: they check what the lines above them just set.
fn build_rest(creds: &Credentials) -> SwapRest {
    let public = UreqOkxTransport::new(true);
    assert!(public.simulated, "this smoke is DEMO ONLY: the simulated-trading header must be on");
    let info = public
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
    )[SYMBOL]
        .clone();
    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(creds, now_ms),
        transport: UreqOkxTransport::new(true)
            .with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        base_url: REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: inst.properties,
        ct_val: inst.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    assert!(rest.transport.simulated, "this smoke is DEMO ONLY");
    assert_eq!(rest.base_url, REST, "this smoke talks to the shared OKX host only");
    rest
}

/// Cancels the stop when dropped: through the bridge, then straight on `cancel-algos`.
struct CancelOnDrop<'a> {
    rest: &'a SwapRest,
    coid: &'a str,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.rest.cancel_order(self.coid) {
            println!("CLEANUP: bridge cancel answered {} {}", e.code, e.msg);
        }
        let _ = raw_cancel_algos(self.rest, self.coid);
    }
}

fn raw_cancel_algos(rest: &SwapRest, coid: &str) -> Result<Value, String> {
    let body = json!([{ "instId": SYMBOL, "algoClOrdId": coid }]).to_string();
    rest.transport
        .signed_json(&rest.base_url, PATH_CANCEL_ALGOS, "POST", &body, &rest.signer)
        .map_err(|e| format!("{} {}", e.code, e.msg))
        .and_then(|raw| unwrap_okx(raw).map_err(|e| format!("{} {}", e.code, e.msg)))
}

/// The `algoClOrdId`s of the account's untriggered `conditional` orders on the instrument.
fn pending_algo_coids(rest: &SwapRest) -> Vec<String> {
    let rows = rest
        .call_get(
            PATH_ORDERS_ALGO_PENDING,
            &[("ordType", json!("conditional")), ("instId", json!(SYMBOL))],
        )
        .expect("orders-algo-pending");
    rows.as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("algoClOrdId").and_then(|c| c.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Did `coid` leave `orders-algo-pending` within 10 s? A bool, not an assert: the caller prints the
/// measurement first and asserts last.
fn left_orders_algo_pending(rest: &SwapRest, coid: &str) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while pending_algo_coids(rest).iter().any(|c| c == coid) {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    true
}

fn stop_request(coid: &str, qty: f64, trigger: f64, reduce_only: bool) -> OrderRequest {
    serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL,
        "side": -1, "qty": qty, "order_type": "stop", "trigger_price": trigger,
        "reduce_only": reduce_only, "ts": now_ms()
    }))
    .unwrap()
}

#[test]
#[ignore = "network + demo creds — places + cancels a real (non-triggering) demo stop — run manually (see module doc)"]
fn okx_stop_algo_is_listed_and_cancelled_on_the_algo_endpoint() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");
    let rest = build_rest(&creds);

    // --- 1. place a far SELL stop at the instrument's minimum size ------------------------------
    let mark = rest.last_price().expect("ticker");
    assert!(mark > 0.0, "live mark required");
    let qty = rest.properties.min_qty * rest.ct_val;
    let trigger = mark * 0.5;
    let coid = format!("vtalgo{}", now_ms() % 100_000_000);
    let mut events = VenueRest::submit_order(&rest, &stop_request(&coid, qty, trigger, true));
    if !matches!(events.last(), Some(Event::OrderAccepted(_))) {
        println!("MEASURED reduce-only stop on a flat demo account refused: {events:?}");
        events = VenueRest::submit_order(&rest, &stop_request(&coid, qty, trigger, false));
    }
    // From here the stop may be resting: the guard cancels it on every exit path.
    let _cleanup = CancelOnDrop { rest: &rest, coid: &coid };
    let algo_id = match events.last() {
        Some(Event::OrderAccepted(a)) => a.venue_order_id.as_deref().map(str::to_string),
        other => panic!("the demo stop must be ACCEPTED: {events:?} / {other:?}"),
    };
    println!("placed stop {coid} algoId={algo_id:?} trigger={trigger} qty={qty}");
    assert!(algo_id.is_some_and(|id| !id.is_empty()), "the ack must carry an algoId");

    // --- 1b. MEASURE the unknown-id words. They do not depend on the stop, so they are read and printed
    // NOW, before any step below can panic: the bridge's `algo_unknown` set is rebuilt from these lines.
    let bogus = format!("vtnone{}", now_ms() % 100_000_000);
    println!("MEASURED cancel-algos of an unknown id: {:?}", raw_cancel_algos(&rest, &bogus));
    let lookup = rest.transport.signed_requery(
        &rest.base_url,
        PATH_ORDER_ALGO,
        "GET",
        &[("algoClOrdId", json!(bogus))],
        &rest.signer,
    );
    match lookup {
        Ok(v) => println!("MEASURED GET order-algo of an unknown id: {v}"),
        Err(e) => println!("MEASURED GET order-algo of an unknown id: Err {} {}", e.code, e.msg),
    }
    let bogus_confirm = rest.confirm_order(&bogus, now_ms());
    println!("MEASURED by-coid lookup (both endpoints) of an unknown id: {bogus_confirm:?}");

    // --- 2. it is listed: orders-algo-pending, the order report, the by-coid lookup -------------
    assert!(pending_algo_coids(&rest).contains(&coid), "the stop must be on orders-algo-pending");
    let recon = OkxReconClient::new(
        OkxV5Signer::new(&creds, now_ms),
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        REST,
        SYMBOL,
        rest.ct_val,
    );
    let report = recon.fetch_order_status_reports(0).expect("order report");
    let mine = report
        .iter()
        .find(|r| r.client_order_id.as_deref() == Some(coid.as_str()))
        .unwrap_or_else(|| panic!("the order report must list the resting stop: {report:?}"));
    assert_eq!(mine.order_type, OrderType::Stop);
    assert_eq!(mine.status, OrderStatus::Accepted);
    assert!((mine.qty - qty).abs() <= qty * 1e-6 + 1e-12, "{} vs {qty}", mine.qty);
    match &rest.confirm_order(&coid, now_ms())[..] {
        [Event::OrderAccepted(a)] => assert!(a.venue_order_id.is_some(), "{a:?}"),
        other => panic!("the by-coid lookup must find the resting stop: {other:?}"),
    }
    let regular = rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("orders-pending");
    let listed = regular
        .as_array()
        .is_some_and(|rows| rows.iter().any(|r| r["clOrdId"] == coid || r["algoClOrdId"] == coid));
    println!("MEASURED orders-pending lists the algo stop: {listed}");

    // --- 3. MEASURE: what does the REGULAR cancel answer, and does the stop survive it? ---------
    let probe = rest.transport.signed(
        &rest.base_url,
        PATH_ORDER_CANCEL,
        "POST",
        &[("instId", json!(SYMBOL)), ("clOrdId", json!(coid))],
        &rest.signer,
    );
    match &probe {
        Ok(v) => println!("MEASURED regular cancel-order of the stop coid: {v}"),
        Err(e) => {
            println!("MEASURED regular cancel-order of the stop coid: Err {} {}", e.code, e.msg)
        }
    }
    let survived = pending_algo_coids(&rest).contains(&coid);
    println!("MEASURED the stop SURVIVES the regular cancel-order: {survived}");
    if !survived {
        println!(
            "VERDICT: the regular cancel-order cancels algo stops — the suspected defect is NOT real"
        );
        return;
    }

    // --- 4. the bridge's cancel: unknown on the regular book -> cancel-algos --------------------
    // Every answer is collected and PRINTED first; the asserts come last (step 5), so a wrong guess in
    // a code set cannot hide the words the sets are rebuilt from. The second cancel reads the
    // cancelled stop's `state` (`canceled` expected) to decide it is gone.
    let first = rest.cancel_order(&coid);
    println!("MEASURED bridge cancel of the resting stop: {first:?}");
    let left_list = left_orders_algo_pending(&rest, &coid);
    println!("MEASURED the cancelled stop left orders-algo-pending within 10 s: {left_list}");
    let after = recon.fetch_order_status_reports(0);
    let finished = rest.transport.signed_requery(
        &rest.base_url,
        PATH_ORDER_ALGO,
        "GET",
        &[("algoClOrdId", json!(coid))],
        &rest.signer,
    );
    match finished {
        Ok(v) => println!("MEASURED GET order-algo of the CANCELLED stop: {v}"),
        Err(e) => {
            println!("MEASURED GET order-algo of the CANCELLED stop: Err {} {}", e.code, e.msg)
        }
    }
    let second = rest.cancel_order(&coid);
    println!("MEASURED second bridge cancel of the cancelled stop: {second:?}");

    // --- 5. ASSERT, last -------------------------------------------------------------------------
    first.expect("the bridge cancel of a resting stop");
    assert!(
        left_list,
        "{coid} is STILL on orders-algo-pending 10 s after the cancel returned Ok: the stop rests"
    );
    let after = after.expect("order report after");
    assert!(
        after.iter().all(|r| r.client_order_id.as_deref() != Some(coid.as_str())),
        "the cancelled stop must leave the order report: {after:?}"
    );
    second.expect("a second cancel of a gone stop is idempotent");
    match &bogus_confirm[..] {
        [Event::OrderRejected(_)] => {}
        other => panic!("an id unknown on BOTH endpoints is the 'absent' reject: {other:?}"),
    }
    println!(
        "okx algo stop smoke green: placed -> listed (algo list, report, lookup) -> bridge cancel -> gone"
    );
}
