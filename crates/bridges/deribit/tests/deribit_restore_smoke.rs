//! Restore-into-registry LIVE testnet smoke (the proof `vike_model::venues::venue_restore`'s deribit
//! row waits for): an order a PREVIOUS session left resting at Deribit TESTNET is found by the real
//! `DeribitReconClient`, planned as ADOPT by `vike_exec::recon::plan_restore`, put into a FRESH
//! `ExecutionEngine` with `reregister_orders`, and then cancelled THROUGH that engine.
//!
//!     cargo test -p vike-deribit --test deribit_restore_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING (testnet, the most liquid BTC option with a live ask, discovered exactly like
//! `deribit_tif_smoke.rs`): rests one far-below LIMIT BUY of the minimum amount, priced at a quarter
//! of the ask and under the tiered-tick boundary, so it cannot fill. Double-gated like every other
//! `*_smoke.rs` in this crate: network + `DERIBIT_DEMO_*` creds via
//! `load_workspace_secrets_from_env`, a `tracing::warn!` and an early return when absent.
//!
//! What it proves, in order:
//!   1. a "previous session" `DeribitRest` (its own authed order-WS) places the order under a coid
//!      the smoke mints (the `label`, within the 64-character budget);
//!   2. the coid, handed to `plan_restore` as a `RestoredOrderRef` beside a made-up coid, lands in
//!      `adopt` with a live status, while the made-up coid lands in `gone`;
//!   3. a fresh engine over a SECOND `DeribitRest` on a second authed socket (the "after restart"
//!      client, which has never seen the order) takes `plan.adopt` through `reregister_orders`,
//!      holds the coid live with its venue order id, and `ExecutionEngine::cancel_order` removes it
//!      at the venue (the next `ReconClient` fetch plans it `gone`, never `adopt`);
//!   4. the `OrderCanceled` the user-data stream would deliver (the smoke opens no socket, so it
//!      builds that event itself) folds the restored order to `Canceled`.
//!
//! STEP 3 IS THE ONE THAT ONCE FAILED, AND ITS FAILURE WAS A FINDING, NOT A FLAKE. `DeribitRest`'s
//! `cancel_order` cancels by venue `order_id` from a coid -> order_id map that lives in ONE
//! `DeribitRest` and is filled only by that client's own submit; the "after restart" client has an
//! empty map. It used to return `Ok(())` and send nothing; it now finds the order by `label` (the
//! first bullet under "Local traps" in the crate's own CLAUDE.md) and cancels it by the id the venue
//! reports. If this smoke stops at "still not gone", that lookup path is broken and the deribit
//! restore row must go back to `awaiting_smoke`.
//!
//! CLEANUP: the PLACING client (the one whose map knows the order id) cancels the coid from a `Drop`
//! guard, so a failed assertion, a panic or a hung poll still cancels it. A run killed from outside
//! (SIGKILL, power loss) skips destructors and leaves the order resting on testnet; cancel it by hand
//! there.

use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_secrets_from_env,
};
use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_deribit::DeribitReconClient;
use vike_deribit::client::{DeribitRest, parse_deribit_option_instruments};
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS};
use vike_exec::recon::{ReconClient, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, OrderStatus, Outbox,
    RiskGate,
};
use vike_model::events::{Event, OrderCanceled};
use vike_model::{OrderRequest, RiskLimits, SymbolProperties, now_ms};

/// The double-gate every `*_smoke.rs` in this crate uses (see `deribit_reconcile_smoke.rs`).
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_secrets_from_env(&std::env::vars().collect());
    let creds = load_credentials_from("deribit", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
    }
    creds
}

/// The most liquid BTC option with a live ask, as `deribit_tif_smoke.rs` discovers it:
/// `(symbol, ask, properties)`.
fn pick_instrument() -> (String, f64, SymbolProperties) {
    let public = UreqTransport::new("deribit");
    let book = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_book_summary_by_currency",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("book summary");
    let mut best: Option<(String, f64, f64)> = None;
    for row in book["result"].as_array().unwrap_or(&vec![]) {
        let Some(ask) = row.get("ask_price").and_then(|a| a.as_f64()) else { continue };
        let oi = row.get("open_interest").and_then(|o| o.as_f64()).unwrap_or(0.0);
        let name = row.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("");
        if ask > 0.0 && !name.is_empty() && best.as_ref().is_none_or(|(_, b_oi, _)| oi > *b_oi) {
            best = Some((name.to_string(), oi, ask));
        }
    }
    let (symbol, oi, ask) = best.expect("a live-ask BTC option on testnet");
    tracing::info!(target: "vike_deribit", "instrument: {symbol} (oi={oi}, ask={ask})");
    let info = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_instruments",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("instruments");
    let properties = parse_deribit_option_instruments(&info)[&symbol].properties;
    (symbol, ask, properties)
}

/// One `DeribitRest` on its OWN authed testnet order-WS. A fresh call is a fresh client with an
/// EMPTY coid -> order_id map: it can cancel only what it submits itself.
fn build_rest(creds: &Credentials, symbol: &str, properties: SymbolProperties) -> DeribitRest {
    let mut transport =
        DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
    transport.connect().expect("order-WS auth");
    DeribitRest::new(transport, symbol, properties, "BTC")
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
                tracing::info!(target: "vike_deribit", coid = self.coid, "cleanup: cancel sent")
            }
            Err(e) => {
                tracing::error!(target: "vike_deribit", coid = self.coid, error = %e.msg, "CLEANUP FAILED: cancel the testnet order by hand")
            }
        }
    }
}

fn restored_ref(coid: &str, symbol: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        coid: coid.to_string(),
        venue: Some("deribit".to_string()),
        symbol: Some(symbol.to_string()),
        account: None,
    }
}

/// One planner pass over what the venue reports now.
fn plan_pass(recon: &dyn ReconClient, symbol: &str, restored: &[RestoredOrderRef]) -> RestorePlan {
    let scope = RestoreScope {
        venue: "deribit".to_string(),
        account: None,
        symbols: vec![symbol.to_string()],
    };
    let orders = recon.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let fills = recon.fetch_fill_reports(now_ms() - 3_600_000).expect("fetch_fill_reports");
    plan_restore(&scope, restored, &orders, &fills)
}

fn adopts(plan: &RestorePlan, coid: &str) -> bool {
    plan.adopt.iter().any(|r| r.client_order_id.as_deref() == Some(coid))
}

fn is_gone(plan: &RestorePlan, coid: &str) -> bool {
    plan.gone.iter().any(|r| r.coid == coid)
}

/// Poll until the venue's report no longer keeps `coid` alive (planned `gone`, never `adopt`).
fn wait_until_gone(
    recon: &dyn ReconClient,
    symbol: &str,
    restored: &[RestoredOrderRef],
    coid: &str,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let plan = plan_pass(recon, symbol, restored);
        if is_gone(&plan, coid) && !adopts(&plan, coid) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{coid} still not gone at the venue 15 s after the engine cancel: {plan:?}. The engine \
             cancel on the restart client (empty coid -> order_id map) must find the order by \
             label and cancel it; the deribit restore row must not be enabled while this fails."
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[test]
#[ignore = "network + testnet creds — places + cancels a real (non-filling) testnet order — run manually (see module doc)"]
fn deribit_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let (symbol, ask, properties) = pick_instrument();

    // --- 1. the "previous session": place a far-below resting LIMIT BUY ------------------------
    let previous_session = build_rest(&creds, &symbol, properties);
    let low_px = (ask * 0.25).min(0.004).max(properties.tick_size);
    let coid = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        client_order_id: coid.clone(),
        venue: "deribit".to_string(),
        symbol: symbol.clone(),
        side: 1,
        qty: properties.min_qty,
        order_type: "limit".to_string(),
        price: Some(low_px),
        ts: now_ms(),
        ..Default::default()
    };
    let events = VenueRest::submit_order(&previous_session, &request);
    tracing::debug!(target: "vike_deribit", "submit events: {events:?}");
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup = CancelOnDrop { rest: &previous_session, coid: &coid };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "testnet order must be ACCEPTED: {events:?}"
    );

    // --- 2. the restart: the real recon client (its own socket), the planner -------------------
    let Some(recon) = DeribitReconClient::connect(&creds, &symbol) else {
        panic!("DeribitReconClient::connect failed — testnet order-WS auth did not succeed");
    };
    let restored = [restored_ref(&coid, &symbol), restored_ref(&ghost, &symbol)];
    let plan = plan_pass(&recon, &symbol, &restored);
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
        Account::new(1.0, "deribit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(build_rest(&creds, &symbol, properties)),
        "deribit",
        &symbol,
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
    wait_until_gone(&recon, &symbol, &restored, &coid);

    // --- 4. the user-data stream's cancel, folded ----------------------------------------------
    let canceled = Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.clone(),
        reason: "restore smoke: user-data stream cancel".into(),
        ts: now_ms(),
    });
    engine.on_event(&canceled, &mut Outbox::default());
    assert_eq!(
        engine.registry[&coid].status,
        OrderStatus::Canceled,
        "the fold must turn the restored order terminal"
    );

    drop(cleanup);
    previous_session.detach();
    tracing::info!(target: "vike_deribit", "restore smoke green: placed -> adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled");
}
