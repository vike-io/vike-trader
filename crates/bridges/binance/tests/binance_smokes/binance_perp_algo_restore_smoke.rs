//! Restore-into-registry LIVE DEMO smoke for a Binance USDⓈ-M perp ALGO STOP (network + demo creds -
//! run by hand, never in a loop; NOT in `.github/workflows/live-smokes.yml`):
//!
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture binance_perp_algo_restore_smoke::
//!
//! The algo twin of `binance_perp_restore_smoke.rs` (a regular LIMIT), and the first stop-kind round
//! trip of the restart-safe-orders work (a restart cancels no order, so a resting stop must be seen,
//! adopted and cancelable afterwards): a reduce-only STOP a PREVIOUS session left resting on the Algo
//! Service is found by the real perp
//! `BinanceReconClient` (its report appends `openAlgoOrders`), planned as ADOPT by
//! `vike_exec::recon::plan_restore`, put into a FRESH `ExecutionEngine` with `reregister_orders`,
//! and then cancelled THROUGH that engine by its bare coid (the non-member path: the regular
//! endpoint's `-2011` retried once on the algo endpoint).
//!
//! ## What it does (ETHUSDT, a minimal position, a far-from-market trigger)
//!
//! 0. refuses to run unless the symbol is flat with nothing resting, and never on an off-limits
//!    account (the same fence `perp_algo_smoke.rs` carries);
//! 1. opens a minimal long with a market BUY and rests a reduce-only STOP_MARKET 15% under the market;
//! 2. the "after restart" side: a fresh recon client lists the stop (`order_type` Stop, the series
//!    label `ETHUSDT.P`, OUR bare coid) and the planner adopts it while a made-up coid is gone;
//! 3. a fresh engine over a client that never saw the order re-registers it and cancels it by coid;
//!    the venue's `openAlgoOrders` no longer lists it, the planner calls it gone, and the venue
//!    stream's `OrderCanceled` (built here, the smoke opens no socket) folds it `Canceled`;
//! 4. ALWAYS leaves the account flat: a `Drop` guard cancels every algo and regular order on the symbol
//!    and closes any position with a reduce-only market order, even when an assertion fails.
//!
//! What it does NOT prove, because only the core can: that the adopted stop keeps its `reduce_only`
//! flag and trigger price (a venue report carries neither; the core puts them back from the
//! ownership file or the journal, `crates/vike-core/src/runtime/tests/boot_restore.rs`'s
//! `an_operators_stop_is_adopted_with_its_terms_and_leaves_the_pending_table`).
//!
//! **DEMO ONLY.** Refuses any base URL but `DEMO_FAPI_REST`. Credentials come from the smoke store
//! (`vike_secrets::smoke_directory`) and are never printed, logged or put on argv.

use std::time::{Duration, Instant};

use crate::perp_algo_venue::{LeaveFlat, SYMBOL, Venue, order};
use vike_binance::perp::{
    BinancePerpRest, DEMO_FAPI_REST, PATH_EXCHANGE_INFO, parse_binance_perp_instruments,
};
use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_exec::recon::{ReconClient, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Outbox, RiskGate,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, OrderCanceled};
use vike_model::{AccountTier, OrderStatus, OrderType, RiskLimits, SymbolProperties, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

type PerpRest = BinancePerpRest<BinanceHmacSigner, UreqTransport>;

// ---- the venue, raw ------------------------------------------------------------------------------

// A free fn, not an `impl Venue` method: `perp_algo_smoke.rs` shares this binary and `Venue`, and
// keeps its own `open_coids`, which also prints the row-26 finding.
fn open_coids(venue: &Venue, path: &str, key: &str) -> Vec<String> {
    let rows = venue.call("GET", path, &[venue.symbol()]).unwrap_or_else(|e| panic!("{path}: {e}"));
    rows.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.get(key)?.as_str().map(str::to_string))
        .collect()
}

// ---- the smoke ------------------------------------------------------------------------------------

fn accepted(events: &[Event]) -> bool {
    events.iter().any(|e| matches!(e, Event::OrderAccepted(_)))
}

/// One signed perp REST client on the demo futures host: a fresh call is a fresh client with no state
/// about any order (an empty `algo_coids` set, so a cancel of a restored stop takes the non-member path).
fn perp_rest(creds: &Credentials, properties: SymbolProperties) -> PerpRest {
    BinancePerpRest {
        link_id: None,
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport: UreqTransport::new("binance")
            .with_rate_gate(vike_binance::ratelimit::perp_rest_gate()),
        base_url: DEMO_FAPI_REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties,
        leverage: 2.0,
        algo_coids: Default::default(),
    }
}

fn restored_ref(coid: &str, series: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        coid: coid.to_string(),
        venue: Some("binance".to_string()),
        symbol: Some(series.to_string()),
        account: None,
        order_type: Some(OrderType::Stop),
        ..Default::default()
    }
}

/// One planner pass over what the venue reports now, with the scope `restore_orders_at_pass` builds.
fn plan_pass(
    recon: &dyn ReconClient,
    series: &str,
    restored: &[RestoredOrderRef],
) -> (RestorePlan, usize) {
    let scope = RestoreScope {
        venue: "binance".to_string(),
        account: None,
        symbols: vec![series.to_string()],
        // the REAL client's claim, as the pass carries it (`ReconcileReports::lists_trigger_orders`)
        lists_trigger_orders: recon.lists_trigger_orders(),
    };
    let orders = recon.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let fills = recon.fetch_fill_reports(now_ms() - 3_600_000).expect("fetch_fill_reports");
    (plan_restore(&scope, restored, &orders, &fills), orders.len())
}

/// Poll until `done(plan)` holds; the Algo Service's list can lag its ack by a moment.
fn wait_for_plan(
    recon: &dyn ReconClient,
    series: &str,
    restored: &[RestoredOrderRef],
    what: &str,
    done: impl Fn(&RestorePlan) -> bool,
) -> RestorePlan {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (plan, rows) = plan_pass(recon, series, restored);
        if done(&plan) {
            return plan;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: not reached within 15 s: {plan:?} over {rows} rows"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "network + DEMO creds: places and cancels real DEMO orders, flattens - run by hand (see module doc)"]
fn binance_perp_algo_stop_is_adopted_after_a_restart_and_the_engine_cancels_it() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };
    // The owner's fence: these account ids are never driven by a smoke, whatever their tier says.
    const OFF_LIMITS: [i64; 6] = [2, 8, 11, 12, 16, 18];
    let id = directory
        .account_row("binance", &AccountLabel::Default, AccountTier::Demo)
        .map(|row| row.id);
    println!("SMOKE account id {id:?} (default binance account at the demo tier)");
    if id.is_none_or(|id| OFF_LIMITS.contains(&id)) {
        println!("SKIP: the default binance demo account is off limits or unresolved ({id:?})");
        return;
    }
    let venue = Venue::new(&creds);

    // 0. the symbol must be flat with nothing resting, or this smoke touches nothing
    let (pos, regular, algo) = (
        venue.position(SYMBOL),
        open_coids(&venue, "/fapi/v1/openOrders", "clientOrderId"),
        open_coids(&venue, "/fapi/v1/openAlgoOrders", "clientAlgoId"),
    );
    if pos != 0.0 || !regular.is_empty() || !algo.is_empty() {
        println!(
            "SKIP: {SYMBOL} is not flat/clean on the demo account (position {pos}, {} regular, {} algo resting)",
            regular.len(),
            algo.len()
        );
        return;
    }
    let flat_guard = LeaveFlat { creds: creds.clone() };

    // 1. the "previous session": a minimal long and a far reduce-only STOP_MARKET under it
    let info = venue
        .transport
        .public(DEMO_FAPI_REST, PATH_EXCHANGE_INFO, &[venue.symbol()])
        .expect("exchangeInfo");
    let props = parse_binance_perp_instruments(&info)[SYMBOL].properties;
    let previous_session = perp_rest(&creds, props);
    let series = format!("{SYMBOL}.P");
    let last = venue.last_price(SYMBOL);
    let want = (props.min_notional.max(5.0) * 1.5 / last).max(props.min_qty);
    let qty = (want / props.step_size).ceil() * props.step_size;
    let tag = now_ms() % 100_000_000;
    let open = previous_session.submit_order(&order(
        &format!("vtalgoR{tag}"),
        1,
        qty,
        OrderType::Market,
        None,
    ));
    assert!(accepted(&open), "the entry must be accepted: {open:?}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while venue.position(SYMBOL) < qty * 0.99 {
        assert!(Instant::now() < deadline, "the entry never showed in positionRisk");
        std::thread::sleep(Duration::from_millis(500));
    }
    let coid = format!("vtalgoS{tag}");
    let ghost = format!("vtalgoG{tag}");
    let trigger = ((last * 0.85) / props.tick_size).floor() * props.tick_size;
    let placed =
        previous_session.submit_order(&order(&coid, -1, qty, OrderType::Stop, Some(trigger)));
    assert!(accepted(&placed), "the reduce-only stop must be accepted: {placed:?}");

    // 2. the restart: the real perp recon client (it appends `openAlgoOrders`), the planner
    let recon = vike_binance::recon_client(&creds, &series, false).expect("recon client");
    let restored = [restored_ref(&coid, &series), restored_ref(&ghost, &series)];
    let plan = wait_for_plan(&*recon, &series, &restored, "the resting stop adopted", |p| {
        p.adopt.iter().any(|r| r.client_order_id.as_deref() == Some(coid.as_str()))
    });
    assert_eq!(plan.adopt.len(), 1, "only the real coid is adopted: {plan:?}");
    let report = &plan.adopt[0];
    assert_eq!(report.symbol, series, "an algo report carries the series label: {report:?}");
    assert_eq!(report.client_order_id.as_deref(), Some(coid.as_str()), "OUR bare coid: {report:?}");
    assert_eq!(report.order_type, OrderType::Stop, "STOP_MARKET reads back as a Stop: {report:?}");
    assert_eq!(report.side, -1, "the stop SELLs the long: {report:?}");
    assert!(report.status.is_live(), "an adopted report is live: {report:?}");
    assert!(
        plan.unproven.iter().any(|r| r.coid == ghost),
        "a coid the venue never had is UNPROVEN (silence is no proof of absence): {plan:?}"
    );
    assert!(
        plan.gone.iter().chain(plan.unproven.iter()).all(|r| r.coid != coid),
        "a resting stop is neither gone nor unproven: {plan:?}"
    );
    assert!(
        recon.lists_trigger_orders(),
        "the perp client claims to list trigger orders, which is what lets silence about a stop \
         ever make it gone"
    );

    // 3. a FRESH engine over a client that has never seen the order
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(perp_rest(&creds, props)),
        "binance",
        &series,
    );
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    assert!(engine.registry[&coid].status.is_live(), "restored live");
    engine.cancel_order(&coid);
    assert!(
        engine.client.last_cancel_error.is_none(),
        "the engine cancel of a restored stop was refused: {:?}",
        engine.client.last_cancel_error
    );
    while let Some(ev) = engine.client.poll_events() {
        assert!(!matches!(ev, Event::OrderCancelRejected(_)), "cancel rejected: {ev:?}");
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while open_coids(&venue, "/fapi/v1/openAlgoOrders", "clientAlgoId").contains(&coid) {
        assert!(Instant::now() < deadline, "the stop still rests after the engine cancel");
        std::thread::sleep(Duration::from_millis(500));
    }
    wait_for_plan(&*recon, &series, &restored, "the cancelled stop absent", |p| {
        p.gone.iter().chain(p.unproven.iter()).any(|r| r.coid == coid) && p.adopt.is_empty()
    });

    // the venue stream's cancel, folded
    let canceled = Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.clone(),
        reason: "algo restore smoke: user-data stream cancel".into(),
        ts: now_ms(),
    });
    engine.on_event(&canceled, &mut Outbox::default());
    assert_eq!(engine.registry[&coid].status, OrderStatus::Canceled, "the fold turns it terminal");

    // 4. flat and clean, whatever the path
    drop(flat_guard);
    let v = Venue::new(&creds);
    assert_eq!(v.position(SYMBOL), 0.0, "the smoke leaves the symbol flat");
    assert!(open_coids(&v, "/fapi/v1/openAlgoOrders", "clientAlgoId").is_empty());
    assert!(open_coids(&v, "/fapi/v1/openOrders", "clientOrderId").is_empty());
    tracing::info!(target: "vike_binance", "algo restore smoke green: reduce-only stop placed -> listed with the bare coid -> adopted -> re-registered -> engine cancel by coid -> gone at venue -> folded Canceled");
}
