//! Restore-into-registry LIVE demo smoke for the binance PERP lane (the second half of the proof
//! `vike_model::venues::venue_restore`'s binance row waits for; `binance_restore_smoke.rs` is the
//! spot half): an order a PREVIOUS session left resting at the Binance demo FUTURES gateway is found
//! by the real perp `BinanceReconClient`, planned as ADOPT by `vike_exec::recon::plan_restore`, put
//! into a FRESH `ExecutionEngine` with `reregister_orders`, and then cancelled THROUGH that engine.
//!
//!     cargo test -p vike-binance --test binance_perp_restore_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING (USDS-M perp, exchange symbol `BTCUSDT`, series label `BTCUSDT.P`): rests one
//! far-from-market LIMIT BUY (90% of the fapi last price, rounded to the tick grid, sized just over
//! `min_notional` on the step grid, so it can never fill and sits inside the PERCENT_PRICE band) on
//! the Binance DEMO futures account, double-gated like every other `*_smoke.rs` in this crate:
//! network + `BINANCE_DEMO_*` creds via `load_workspace_secrets_from_env`, a `tracing::warn!` and an
//! early return when absent. Nothing reduces or touches a position and no leverage is changed.
//!
//! **Broker-stamped coid, on purpose.** Both REST clients carry `link_id: Some(LINK_ID)`, the way a
//! mount with an attribution code builds them, so the venue STORES `x-<LINK_ID>-<coid>`. The smoke
//! reads the raw `openOrders` row to prove the prefix is on the venue's record, then proves the
//! `ReconClient` report carries the BARE coid (`strip_broker_coid_prefix` in the family recon
//! parser), and that the engine's cancel re-prefixes it (`binance_broker_coid`) so the venue finds
//! the order. A missed strip would show here as an unmatched coid (`gone`, never `adopt`).
//!
//! **The symbol spelling, asserted.** A perp report row, the engine mounted on the perp, and the
//! order the ownership file records all spell the SERIES label (`perp_series_symbol`, `BTCUSDT.P`),
//! never the bare exchange symbol the REST requests use. `RestoreScope::symbols` and the ref's
//! `symbol` are spelled that way here, and a ghost ref spelled with the BARE symbol is asserted
//! UNTOUCHED (out of scope: it is neither gone nor adopted), so a scope spelled wrongly would fail
//! this smoke instead of silently declaring nothing gone.
//!
//! What it proves, in order:
//!   1. a "previous session" REST client places the order under a coid the smoke mints, and the
//!      venue's raw record carries the broker-prefixed id;
//!   2. the coid, handed to `plan_restore` as a `RestoredOrderRef` beside a made-up coid, lands in
//!      `adopt` with a live status, the report names `BTCUSDT.P` and the bare coid, while the made-up
//!      coid lands in `gone`;
//!   3. a fresh engine mounted on `BTCUSDT.P` over a SECOND REST client (the "after restart" client,
//!      which has never seen the order) takes `plan.adopt` through `reregister_orders`, holds the
//!      coid live with its venue order id and series symbol, and `ExecutionEngine::cancel_order`
//!      removes it at the venue (the next `ReconClient` fetch plans it `gone`, never `adopt`);
//!   4. the `OrderCanceled` the venue's user-data stream would deliver (the smoke opens no socket, so
//!      it builds that event itself) folds the restored order to `Canceled`.
//!
//! CLEANUP: the placing client cancels the coid from a `Drop` guard, so a failed assertion, a panic
//! or a hung poll still cancels it (idempotent: `-2011` "unknown order" is swallowed). A run killed
//! from outside (SIGKILL, power loss) skips destructors and leaves the order resting; it is a BUY 10%
//! under the last price that cannot fill, so cancel it by hand on the demo futures account.

use vike_binance::BinanceReconClient;
use vike_binance::family::order_map::binance_broker_coid;
use vike_binance::family::perp_series_symbol;
use vike_binance::perp::{
    BinancePerpRest, DEMO_FAPI_REST, PATH_EXCHANGE_INFO, PATH_OPEN_ORDERS,
    parse_binance_perp_instruments,
};
use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_secrets_from_env,
};
use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_exec::recon::{ReconClient, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, OrderStatus, Outbox,
    RiskGate,
};
use vike_model::events::{Event, OrderCanceled};
use vike_model::{OrderStatusReport, RiskLimits, SymbolProperties, now_ms};

/// The instrument as the venue names it on the wire: what the fapi REST clients are built on.
const API_SYMBOL: &str = "BTCUSDT";

/// A made-up broker/link code (5 chars: `x-VTRST-` + a 18-char coid stays under the 36-char ceiling,
/// so nothing truncates). It attributes nothing: the demo gateway accepts any well-formed id.
const LINK_ID: &str = "VTRST";

type PerpRest = BinancePerpRest<BinanceHmacSigner, UreqTransport>;

/// The double-gate every `*_smoke.rs` in this crate uses (see `binance_reconcile_smoke.rs`).
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_secrets_from_env(&std::env::vars().collect());
    let creds = load_credentials_from("binance", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
    }
    creds
}

fn perp_transport() -> UreqTransport {
    UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::perp_rest_gate())
}

/// The fapi grid of [`API_SYMBOL`], from the venue's own `exchangeInfo`.
fn fapi_properties() -> SymbolProperties {
    let info = perp_transport()
        .public(DEMO_FAPI_REST, PATH_EXCHANGE_INFO, &[("symbol", API_SYMBOL.into())])
        .expect("fapi exchangeInfo");
    parse_binance_perp_instruments(&info)[API_SYMBOL].properties
}

/// One signed perp REST client on the demo futures host, clock-synced against ITS OWN
/// `/fapi/v1/time`, broker-stamped with [`LINK_ID`]. A fresh call is a fresh client: no state about
/// any order.
fn build_rest(creds: &Credentials, properties: SymbolProperties) -> PerpRest {
    let rest = BinancePerpRest {
        link_id: Some(LINK_ID.to_string()),
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport: perp_transport(),
        base_url: DEMO_FAPI_REST.to_string(),
        symbol: API_SYMBOL.to_string(),
        properties,
        leverage: 1.0,
    };
    let local = now_ms();
    let server = rest
        .transport
        .public(DEMO_FAPI_REST, "/fapi/v1/time", &[])
        .expect("fapi time")
        .get("serverTime")
        .and_then(|t| t.as_i64())
        .expect("serverTime");
    rest.signer.set_offset_ms(server - local);
    rest
}

/// The fapi last price of [`API_SYMBOL`] (`/fapi/v1/ticker/price`), the mark the limit is priced from.
fn last_price() -> f64 {
    perp_transport()
        .public(DEMO_FAPI_REST, "/fapi/v1/ticker/price", &[("symbol", API_SYMBOL.into())])
        .expect("fapi ticker")
        .get("price")
        .and_then(|p| p.as_str())
        .and_then(|p| p.parse::<f64>().ok())
        .expect("ticker price")
}

/// Cancels `coid` through the PLACING client when dropped, so no failure path leaves it resting.
struct CancelOnDrop<'a, R: VenueRest> {
    rest: &'a R,
    coid: &'a str,
}

impl<R: VenueRest> Drop for CancelOnDrop<'_, R> {
    fn drop(&mut self) {
        match self.rest.cancel_order(self.coid) {
            Ok(()) => {
                tracing::info!(target: "vike_binance", coid = self.coid, "cleanup: cancel sent")
            }
            Err(e) => {
                tracing::error!(target: "vike_binance", coid = self.coid, error = %e.msg, "CLEANUP FAILED: cancel the demo order by hand")
            }
        }
    }
}

/// A ref spelled the way the ownership file records a perp order: venue + the engine's series label.
fn restored_ref(coid: &str, symbol: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        coid: coid.to_string(),
        venue: Some("binance".to_string()),
        symbol: Some(symbol.to_string()),
        account: None,
    }
}

/// One planner pass over what the venue reports now, plus the order rows it was planned from. The
/// scope names the SERIES label, exactly as `vike-core`'s `restore_orders_at_pass` spells it from
/// the engine's mounted symbol.
fn plan_pass(
    recon: &dyn ReconClient,
    series: &str,
    restored: &[RestoredOrderRef],
) -> (RestorePlan, Vec<OrderStatusReport>) {
    let scope = RestoreScope {
        venue: "binance".to_string(),
        account: None,
        symbols: vec![series.to_string()],
    };
    let orders = recon.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let fills = recon.fetch_fill_reports(now_ms() - 3_600_000).expect("fetch_fill_reports");
    (plan_restore(&scope, restored, &orders, &fills), orders)
}

fn adopts(plan: &RestorePlan, coid: &str) -> bool {
    plan.adopt.iter().any(|r| r.client_order_id.as_deref() == Some(coid))
}

fn is_gone(plan: &RestorePlan, coid: &str) -> bool {
    plan.gone.iter().any(|r| r.coid == coid)
}

/// Poll until `done(plan)` holds; fapi's order replica can lag the submit ack by a moment.
fn wait_for_plan(
    recon: &dyn ReconClient,
    series: &str,
    restored: &[RestoredOrderRef],
    what: &str,
    done: impl Fn(&RestorePlan) -> bool,
) -> (RestorePlan, Vec<OrderStatusReport>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let (plan, orders) = plan_pass(recon, series, restored);
        if done(&plan) {
            return (plan, orders);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: not reached within 15 s: {plan:?} over {orders:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn binance_perp_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    // The label the report rows, the engine and the ownership file's symbol all carry.
    let series = perp_series_symbol(API_SYMBOL);
    assert_ne!(series, API_SYMBOL, "a perp series label is never the bare exchange symbol");

    // --- 1. the "previous session": place a far-from-market resting LIMIT BUY -------------------
    let properties = fapi_properties();
    let previous_session = build_rest(&creds, properties);
    let mark = last_price();
    assert!(mark > 0.0, "ticker price must be live");
    // 90% of the last price on the tick grid (the spot restore smoke's price): a BUY 10% down never
    // fills inside the test window, and fapi's PERCENT_PRICE bounds only the aggressive side of a BUY.
    let tick = properties.tick_size;
    let limit_px = (mark * 0.9 / tick).round() * tick;
    // Notional >= 1.6 x min_notional AFTER rounding the quantity UP onto the step grid, so the venue's
    // MIN_NOTIONAL filter holds whatever the price and the step are.
    let step = properties.step_size;
    let raw_qty = (properties.min_notional.max(5.0) * 1.6) / limit_px;
    let qty = (raw_qty / step).ceil() * step;
    let coid = format!("vtprestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtprestoreghost{}", now_ms() % 100_000_000);
    let bare_ghost = format!("vtprestorebare{}", now_ms() % 100_000_000);
    // The order names the SERIES label, as the engine mounted on the perp does; the client puts the
    // bare symbol on the wire from its own `symbol` field.
    let request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": series,
        "side": 1, "qty": qty, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    let events = VenueRest::submit_order(&previous_session, &request);
    tracing::debug!(target: "vike_binance", "submit events: {events:?}");
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup = CancelOnDrop { rest: &previous_session, coid: &coid };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED: {events:?}"
    );

    // The venue's RAW record carries the broker-prefixed id (what `strip_broker_coid_prefix` undoes).
    let wire_coid = binance_broker_coid(Some(LINK_ID), &coid);
    assert!(wire_coid.starts_with(&format!("x-{LINK_ID}-")) && wire_coid.ends_with(&coid));
    assert!(wire_coid.len() <= 36, "the broker id must not truncate the coid: {wire_coid}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let raw = previous_session
            .transport
            .signed(
                DEMO_FAPI_REST,
                PATH_OPEN_ORDERS,
                "GET",
                &[("symbol", API_SYMBOL.into())],
                &previous_session.signer,
            )
            .expect("raw fapi openOrders");
        let stored: Vec<&str> = raw
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|o| o.get("clientOrderId").and_then(|c| c.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        if stored.contains(&wire_coid.as_str()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the venue never stored the broker-prefixed id {wire_coid}: {stored:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // --- 2. the restart: the real perp recon client, the planner -------------------------------
    // Exactly the factory's perp arm: the bare symbol in, the demo futures host, the perp rate gate.
    let recon = BinanceReconClient::perp(
        BinanceHmacSigner::new(&creds, now_ms),
        perp_transport(),
        DEMO_FAPI_REST,
        API_SYMBOL,
    );
    let restored = [
        restored_ref(&coid, &series),
        restored_ref(&ghost, &series),
        // Spelled with the BARE symbol: not covered by a scope that names the series label.
        restored_ref(&bare_ghost, API_SYMBOL),
    ];
    let (plan, orders) =
        wait_for_plan(&recon, &series, &restored, "the resting coid adopted", |p| adopts(p, &coid));
    assert_eq!(plan.adopt.len(), 1, "only the real coid is adopted: {plan:?}");
    let report = &plan.adopt[0];
    assert_eq!(report.venue, "binance");
    assert_eq!(report.symbol, series, "a perp order report carries the series label: {report:?}");
    assert_eq!(
        report.client_order_id.as_deref(),
        Some(coid.as_str()),
        "the report carries OUR coid, broker prefix stripped (the venue stores {wire_coid}): {report:?}"
    );
    assert!(
        orders.iter().all(|o| o.client_order_id.as_deref() != Some(wire_coid.as_str())),
        "no report may carry the broker-prefixed id: {orders:?}"
    );
    assert!(
        OrderStatus::parse(&report.status).is_none_or(|s| s.is_live()),
        "an adopted report is live: {report:?}"
    );
    assert!(!is_gone(&plan, &coid), "a resting coid is never gone: {plan:?}");
    assert!(is_gone(&plan, &ghost), "a coid the venue never had is GONE: {plan:?}");
    assert!(
        !is_gone(&plan, &bare_ghost) && !adopts(&plan, &bare_ghost),
        "a ref spelled with the BARE symbol is out of the series-label scope: {plan:?}"
    );
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over a client that has never seen the order -------------------------
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(build_rest(&creds, properties)),
        "binance",
        &series,
    );
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    let restored_order = engine.registry.get(&coid).expect("the coid is in the registry");
    assert!(restored_order.status.is_live(), "restored live: {:?}", restored_order.status);
    assert!(restored_order.venue_order_id.is_some(), "the venue order id came across");
    assert_eq!(
        restored_order.request.symbol, series,
        "the restored order is spelled on the engine's mounted series label"
    );

    engine.cancel_order(&coid);
    assert!(
        engine.client.last_cancel_error.is_none(),
        "the engine cancel was refused: {:?}",
        engine.client.last_cancel_error
    );
    while let Some(ev) = engine.client.poll_events() {
        assert!(!matches!(ev, Event::OrderCancelRejected(_)), "cancel rejected: {ev:?}");
    }
    wait_for_plan(&recon, &series, &restored, "the cancelled coid gone", |p| {
        is_gone(p, &coid) && !adopts(p, &coid)
    });

    // --- 4. the venue stream's cancel, folded --------------------------------------------------
    let canceled = Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.clone(),
        reason: "perp restore smoke: user-data stream cancel".into(),
        ts: now_ms(),
    });
    engine.on_event(&canceled, &mut Outbox::default());
    assert_eq!(
        engine.registry[&coid].status,
        OrderStatus::Canceled,
        "the fold must turn the restored order terminal"
    );

    drop(cleanup);
    tracing::info!(target: "vike_binance", "perp restore smoke green: placed (broker-prefixed) -> adopted on BTCUSDT.P with the bare coid -> re-registered -> engine cancel -> gone at venue -> folded Canceled");
}
