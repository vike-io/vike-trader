//! Restore-into-registry LIVE demo smoke (the proof `vike_model::venues::venue_restore`'s okx row
//! waits for): an order a PREVIOUS session left resting at the OKX demo (simulated-trading) venue is
//! found by the real `OkxReconClient`, planned as ADOPT by `vike_exec::recon::plan_restore`, put into
//! a FRESH `ExecutionEngine` with `reregister_orders`, and then cancelled THROUGH that engine.
//!
//!     cargo test -p vike-okx --test okx_restore_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING (SWAP, `BTC-USDT-SWAP`): rests one far-from-market LIMIT BUY (half the last price,
//! ONE contract's worth of base) on the OKX DEMO account, double-gated like every other `*_smoke.rs`
//! in this crate: network + `OKX_DEMO_*` creds + passphrase via `load_workspace_secrets_from_env`, a
//! `tracing::warn!` and an early return when absent. Every transport is `UreqOkxTransport::new(true)`
//! (`x-simulated-trading: 1`; demo and mainnet share the host, the HEADER is the switch, see
//! `crates/bridges/okx/CLAUDE.md`). Nothing reduces or touches a position.
//!
//! What it proves, in order:
//!   1. a "previous session" REST client places the order under a coid the smoke mints (`clOrdId`,
//!      alphanumeric, within the 32-character budget);
//!   2. the coid, handed to `plan_restore` as a `RestoredOrderRef` beside a made-up coid, lands in
//!      `adopt` with a live status, while the made-up coid lands in `gone`;
//!   3. a fresh engine over a SECOND REST client (the "after restart" client) takes `plan.adopt`
//!      through `reregister_orders`, holds the coid live with its venue order id, and
//!      `ExecutionEngine::cancel_order` removes it at the venue (the next `ReconClient` fetch plans
//!      it `gone`, never `adopt`);
//!   4. the `OrderCanceled` the private `orders` stream would deliver (the smoke opens no socket, so
//!      it builds that event itself) folds the restored order to `Canceled`.
//!
//! CLEANUP: the placing client cancels the coid from a `Drop` guard, so a failed assertion, a panic
//! or a hung poll still cancels it (idempotent: the venue's already-gone codes are swallowed). A run
//! killed from outside (SIGKILL, power loss) skips destructors and leaves the order resting; it is a
//! half-price BUY that cannot fill, so cancel it by hand on the demo account.

use serde_json::json;
use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_secrets_from_env,
};
use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_bridge_core::signer::OkxV5Signer;
use vike_exec::recon::{ReconClient, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, OrderStatus, Outbox,
    RiskGate,
};
use vike_model::events::{Event, OrderCanceled};
use vike_model::{OrderStatusReport, RiskLimits, now_ms};
use vike_okx::OkxReconClient;
use vike_okx::perp::{OkxPerpRest, PATH_INSTRUMENTS, REST, parse_okx_perp_instruments};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};

const SYMBOL: &str = "BTC-USDT-SWAP";

type SwapRest = OkxPerpRest<UreqOkxTransport>;

/// The double-gate every `*_smoke.rs` in this crate uses (see `okx_reconcile_smoke.rs`).
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_secrets_from_env(&std::env::vars().collect());
    let creds = load_credentials_from("okx", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

/// One signed SWAP REST client on the demo (simulated-trading) transport, built the way
/// `okx_tif_smoke.rs` builds its own. A fresh call is a fresh client: no state about any order.
fn build_rest(creds: &Credentials) -> SwapRest {
    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()); // demo: x-simulated-trading: 1
    let info = transport
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
    )[SYMBOL]
        .clone();
    OkxPerpRest {
        signer: OkxV5Signer::new(creds, now_ms),
        transport: UreqOkxTransport::new(true)
            .with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        base_url: REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: inst.properties,
        ct_val: inst.ct_val,
        leverage: 2.0,
        broker_code: None,
    }
}

/// Cancels `coid` through the PLACING client when dropped, so no failure path leaves it resting.
struct CancelOnDrop<'a, R: VenueRest> {
    rest: &'a R,
    coid: &'a str,
}

impl<R: VenueRest> Drop for CancelOnDrop<'_, R> {
    fn drop(&mut self) {
        match self.rest.cancel_order(self.coid) {
            Ok(()) => tracing::info!(target: "vike_okx", coid = self.coid, "cleanup: cancel sent"),
            Err(e) => {
                tracing::error!(target: "vike_okx", coid = self.coid, error = %e.msg, "CLEANUP FAILED: cancel the demo order by hand")
            }
        }
    }
}

fn restored_ref(coid: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        coid: coid.to_string(),
        venue: Some("okx".to_string()),
        symbol: Some(SYMBOL.to_string()),
        account: None,
    }
}

/// One planner pass over what the venue reports now, plus the order rows it was planned from.
fn plan_pass(
    recon: &dyn ReconClient,
    restored: &[RestoredOrderRef],
) -> (RestorePlan, Vec<OrderStatusReport>) {
    let scope =
        RestoreScope { venue: "okx".to_string(), account: None, symbols: vec![SYMBOL.to_string()] };
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

/// Poll until the venue's report no longer keeps `coid` alive (planned `gone`, never `adopt`).
fn wait_until_gone(recon: &dyn ReconClient, restored: &[RestoredOrderRef], coid: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let (plan, _) = plan_pass(recon, restored);
        if is_gone(&plan, coid) && !adopts(&plan, coid) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{coid} still not gone at the venue 15 s after the engine cancel: {plan:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn okx_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    // --- 1. the "previous session": place a far-from-market resting LIMIT BUY -------------------
    let previous_session = build_rest(&creds);
    let mark = previous_session.last_price().expect("ticker");
    assert!(mark > 0.0, "live mark required");
    let ct_val = previous_session.ct_val;
    // one contract's worth of BASE, half the market: the BUY rests and cannot cross
    let leg_base = previous_session.properties.min_qty * ct_val;
    let limit_px = mark * 0.5;
    let coid = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL,
        "side": 1, "qty": leg_base, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    let events = VenueRest::submit_order(&previous_session, &request);
    tracing::debug!(target: "vike_okx", "submit events: {events:?}");
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup = CancelOnDrop { rest: &previous_session, coid: &coid };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED: {events:?}"
    );

    // --- 2. the restart: the real recon client, the planner ------------------------------------
    let recon = OkxReconClient::new(
        OkxV5Signer::new(&creds, now_ms),
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        REST,
        SYMBOL,
        ct_val,
    );
    let restored = [restored_ref(&coid), restored_ref(&ghost)];
    let (plan, _) = plan_pass(&recon, &restored);
    assert!(adopts(&plan, &coid), "the resting coid must be ADOPTED: {plan:?}");
    assert_eq!(plan.adopt.len(), 1, "only the real coid is adopted: {plan:?}");
    let report = &plan.adopt[0];
    assert!(
        OrderStatus::parse(&report.status).is_none_or(|s| s.is_live()),
        "an adopted report is live: {report:?}"
    );
    assert!(!is_gone(&plan, &coid), "a resting coid is never gone: {plan:?}");
    assert!(is_gone(&plan, &ghost), "a coid the venue never had is GONE: {plan:?}");
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over a client that has never seen the order -------------------------
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(build_rest(&creds)),
        "okx",
        SYMBOL,
    );
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    let restored_order = engine.registry.get(&coid).expect("the coid is in the registry");
    assert!(restored_order.status.is_live(), "restored live: {:?}", restored_order.status);
    assert!(restored_order.venue_order_id.is_some(), "the venue order id came across");

    engine.cancel_order(&coid);
    assert!(
        engine.client.last_cancel_error.is_none(),
        "the engine cancel was refused: {:?}",
        engine.client.last_cancel_error
    );
    while let Some(ev) = engine.client.poll_events() {
        assert!(!matches!(ev, Event::OrderCancelRejected(_)), "cancel rejected: {ev:?}");
    }
    wait_until_gone(&recon, &restored, &coid);

    // --- 4. the private stream's cancel, folded ------------------------------------------------
    let canceled = Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.clone(),
        reason: "restore smoke: private stream cancel".into(),
        ts: now_ms(),
    });
    engine.on_event(&canceled, &mut Outbox::default());
    assert_eq!(
        engine.registry[&coid].status,
        OrderStatus::Canceled,
        "the fold must turn the restored order terminal"
    );

    drop(cleanup);
    tracing::info!(target: "vike_okx", "restore smoke green: placed -> adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled");
}
