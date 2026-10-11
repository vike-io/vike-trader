//! Restore-into-registry LIVE **testnet** smoke (the proof `vike_model::venues::venue_restore`'s
//! hyperliquid row waits for): an order a PREVIOUS session left resting at the Hyperliquid testnet
//! is found by the real `HyperliquidReconClient` under its `0x` cloid, read back into its coid
//! through the client's own `wire_id_for` (exactly what vike-core's `restore_orders_at_pass` does),
//! planned as ADOPT, put into a FRESH `ExecutionEngine`, announced to a FRESH client with
//! `adopt_restored`, and then cancelled THROUGH that engine, which before decision 0121 died as
//! "no local order mapping to cancel".
//!
//!     cargo test -p vike-hyperliquid --test hyperliquid_restore_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING, **TESTNET ONLY**: `Env::Demo` is the only environment this file names, and the
//! first thing the test does after loading credentials is assert the network is `Testnet`; there is
//! no `Env::Live` anywhere in it. It rests one far-from-market GTC LIMIT BUY on `BTC` (15% below
//! the mid, sized just over the venue's 10 USD minimum, so it cannot fill) on the testnet account
//! behind `HYPERLIQUID_DEMO_PRIVATE_KEY`, double-gated like every other smoke in this crate: the
//! network and that key in the workspace `.env`, a `tracing::warn!` and an early return when absent. It is
//! the FIRST test in this crate that puts an order on a wire (`crates/bridges/hyperliquid/CLAUDE.md`
//! says so): treat the first run as an experiment and watch it.
//!
//! What it proves, in order:
//!   1. a "previous session" client places the order under a coid the smoke mints (the venue accepts
//!      it and reports a resting order);
//!   2. the real recon client reports that order with a `0x` cloid and NOT our coid; rewriting the
//!      reports through a SECOND, fresh client's `wire_id_for` (the pass's translation step), the
//!      coid handed to `plan_restore` as a `RestoredOrderRef` beside a made-up coid lands in `adopt`
//!      with a live status and the made-up coid in `gone`;
//!   3. a fresh engine over that second client (which has never seen the order) takes `plan.adopt`
//!      through `reregister_orders` and `adopt_restored`, holds the coid live, and
//!      `ExecutionEngine::cancel_order` cancels it AT THE VENUE: the client reports a canceled order
//!      for the coid (never an `OrderCancelRejected`), and no event it emits carries the raw cloid
//!      (the restored registry resolved its own order);
//!   4. the next recon fetch plans the coid `gone`, never `adopt`, and the event the second client
//!      emitted folds the restored order to `Canceled`.
//!
//! CLEANUP: the placing client is held by a `Drop` guard that cancels the coid on EVERY exit path (a
//! failed assertion, a panic, a hung poll), idempotently (a second cancel of a gone order is a
//! harmless reject). A run killed from outside (SIGKILL, power loss) skips destructors and leaves the
//! order resting; it is a BUY 15% under the mid that cannot fill, so cancel it by hand on the
//! testnet account.
//!
//! NOT covered: the spot lane (`HYPE/USDC`-style pairs; the translation remaps the coin the same
//! way, but the testnet spot book is thin) and a restored order's `modify` (the exec thread does not
//! know a restored order's oid; it is rejected as before).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::load_workspace_secrets_from_env;
use vike_exec::recon::{ReconClient, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore};
use vike_exec::{
    Account, BalanceMode, EventHandler, EventSender, ExecutionClient, ExecutionEngine, Ingest,
    OrderStatus, Outbox, RiskGate, event_channel,
};
use vike_hyperliquid::config::{self, Env, Network, Product};
use vike_hyperliquid::instruments::HyperliquidInstruments;
use vike_hyperliquid::signing::Signer;
use vike_hyperliquid::transport::HyperliquidTransport;
use vike_hyperliquid::{HyperliquidExecutionClient, HyperliquidReconClient};
use vike_model::events::Event;
use vike_model::{FillReport, OrderRequest, OrderStatusReport, RiskLimits, now_ms};

/// A non-blocking pull on one client's event lane (the lane's receiver type is the core's
/// runtime type, which this crate does not name).
type Pull = Box<dyn FnMut() -> Option<Ingest>>;

const SYMBOL: &str = "BTC";
const VENUE: &str = "hyperliquid";

/// The double-gate every live smoke here uses (see `hyperliquid_reconcile_smoke.rs`): `None` after a
/// `tracing::warn!` when the testnet key is absent, so the caller self-skips.
fn load_demo_creds() -> Option<config::HlCredentials> {
    let vars = load_workspace_secrets_from_env(&std::env::vars().collect());
    let creds = config::load(Env::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_hyperliquid", "SKIP: HYPERLIQUID_DEMO creds absent");
    }
    creds
}

/// One exec client on the testnet over its own event lane. A fresh call is a fresh client: an empty
/// cloid registry and an empty asset map, which is what a restarted process has.
fn spawn_client(
    creds: &config::HlCredentials,
    master: &str,
    instruments: &Arc<HyperliquidInstruments>,
) -> (HyperliquidExecutionClient, Pull) {
    let (events, mut rx): (EventSender, _) = event_channel(1024);
    let signer = Signer::from_private_key(&creds.private_key, creds.network)
        .expect("HYPERLIQUID_DEMO_PRIVATE_KEY must be a valid secp256k1 key");
    let client = HyperliquidExecutionClient::spawn(
        signer,
        master.to_string(),
        Arc::clone(instruments),
        events,
        None,
        None,
    );
    (client, Box::new(move || rx.try_recv().ok()))
}

/// Every event currently waiting on `rx`.
fn drain(rx: &mut Pull) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(ing) = rx() {
        if let Ingest::Event(e) = ing {
            out.push(e);
        }
    }
    out
}

/// Cancels `coid` through the PLACING client when dropped, so no failure path leaves it resting.
/// The cancel is queued ahead of the client's own `Shutdown` (one FIFO lane), and the client's drop
/// joins its thread, so the request is on the wire before the test binary exits.
struct CancelOnDrop {
    client: HyperliquidExecutionClient,
    coid: String,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        tracing::info!(target: "vike_hyperliquid", coid = %self.coid, "cleanup: cancel queued");
        self.client.cancel(&self.coid);
    }
}

fn restored_ref(coid: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        coid: coid.to_string(),
        venue: Some(VENUE.to_string()),
        symbol: Some(SYMBOL.to_string()),
        account: None,
    }
}

/// The pass's translation step, as `vike-core`'s `restore_orders_at_pass` does it: ask `client` for
/// the wire id of each restored coid and rewrite the reports from wire id back to coid (and the
/// venue coin back to the ref's unified symbol). The core's helper is private to a higher layer; this
/// is the same rule over the same trait method, which is the thing the smoke must prove.
fn translate(
    client: &dyn ExecutionClient,
    restored: &[RestoredOrderRef],
    orders: &mut [OrderStatusReport],
    fills: &mut [FillReport],
) {
    let wire: HashMap<String, (String, Option<String>)> = restored
        .iter()
        .filter_map(|r| Some((client.wire_id_for(&r.coid)?, (r.coid.clone(), r.symbol.clone()))))
        .collect();
    for o in orders {
        if let Some((coid, symbol)) = o.client_order_id.as_deref().and_then(|w| wire.get(w)) {
            o.client_order_id = Some(coid.clone());
            if let Some(s) = symbol {
                o.symbol = s.clone();
            }
        }
    }
    for f in fills {
        if let Some((coid, symbol)) = f.client_order_id.as_deref().and_then(|w| wire.get(w)) {
            f.client_order_id = Some(coid.clone());
            if let Some(s) = symbol {
                f.symbol = s.clone();
            }
        }
    }
}

/// One planner pass over what the venue reports now, plus the (UNtranslated) order rows it read.
fn plan_pass(
    recon: &dyn ReconClient,
    translator: &dyn ExecutionClient,
    restored: &[RestoredOrderRef],
) -> (RestorePlan, Vec<OrderStatusReport>) {
    let scope =
        RestoreScope { venue: VENUE.to_string(), account: None, symbols: vec![SYMBOL.to_string()] };
    let raw_orders = recon.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let mut orders = raw_orders.clone();
    let mut fills = recon.fetch_fill_reports(now_ms() - 3_600_000).expect("fetch_fill_reports");
    translate(translator, restored, &mut orders, &mut fills);
    (plan_restore(&scope, restored, &orders, &fills), raw_orders)
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
    translator: &dyn ExecutionClient,
    restored: &[RestoredOrderRef],
    coid: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (plan, _) = plan_pass(recon, translator, restored);
        if is_gone(&plan, coid) && !adopts(&plan, coid) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{coid} still not gone at the venue 20 s after the engine cancel: {plan:?}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Collect `rx`'s events until `done` accepts the accumulated list or 20 s pass.
fn wait_for_events(rx: &mut Pull, what: &str, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut all = Vec::new();
    loop {
        all.extend(drain(rx));
        if done(&all) {
            return all;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; saw {all:?}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[test]
#[ignore = "network + TESTNET creds — places + cancels a real (non-filling) testnet order — run manually (see module doc)"]
fn hyperliquid_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    assert_eq!(creds.network, Network::Testnet, "this smoke places a real order: TESTNET ONLY");

    let signer = Signer::from_private_key(&creds.private_key, creds.network)
        .expect("HYPERLIQUID_DEMO_PRIVATE_KEY must be a valid secp256k1 key");
    let master = creds.account_address.clone().unwrap_or_else(|| signer.address().to_string());
    let transport = HyperliquidTransport::new(creds.network);
    let instruments = Arc::new(HyperliquidInstruments::load(&transport, false).expect("meta"));
    let inst = instruments.symbology().by_symbol(SYMBOL).expect("BTC is listed").clone();

    // --- 1. the "previous session": rest a far-from-market GTC LIMIT BUY -----------------------
    let mids = transport.info(&serde_json::json!({ "type": "allMids" })).expect("allMids");
    let mid: f64 = mids
        .get(&inst.coin)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .expect("a live BTC mid");
    assert!(mid > 0.0, "the BTC mid must be live");
    // 15% below the mid: far enough to never fill inside the test window, close enough that the
    // venue's price band accepts it. Size just over the 10 USD minimum notional, rounded UP to the
    // size grid.
    let limit_px = (mid * 0.85).round();
    let step = 10f64.powi(-(inst.sz_decimals as i32));
    let qty = ((11.0 / limit_px) / step).ceil() * step;
    let coid = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        client_order_id: coid.clone(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(limit_px),
        ts: now_ms(),
        ..Default::default()
    };
    let (mut previous, mut previous_rx) = spawn_client(&creds, &master, &instruments);
    previous.submit(&request);
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup = CancelOnDrop { client: previous, coid: coid.clone() };
    let placed = wait_for_events(&mut previous_rx, "the placing client's accept", |evs| {
        evs.iter().any(|e| {
            matches!(e, Event::OrderAccepted(a) if a.client_order_id == coid)
                || matches!(e, Event::OrderRejected(r) if r.client_order_id == coid)
        })
    });
    assert!(
        placed.iter().any(|e| matches!(e, Event::OrderAccepted(a) if a.client_order_id == coid)),
        "the testnet order must be ACCEPTED: {placed:?}"
    );

    // --- 2. the restart: a fresh client, the real recon client, the planner --------------------
    let (restarted, mut restarted_rx) = spawn_client(&creds, &master, &instruments);
    let wire = restarted.wire_id_for(&coid).expect("hyperliquid names a wire id");
    assert!(wire.starts_with("0x"), "the wire id is the 0x cloid: {wire}");
    let recon = HyperliquidReconClient::new(
        HyperliquidTransport::new(creds.network),
        master.clone(),
        Product::Perp,
    );
    let restored = [restored_ref(&coid), restored_ref(&ghost)];
    // The venue may take a beat to list the new order in `frontendOpenOrders`.
    let deadline = Instant::now() + Duration::from_secs(20);
    let (plan, raw) = loop {
        let (plan, raw) = plan_pass(&recon, &restarted, &restored);
        if adopts(&plan, &coid) || Instant::now() >= deadline {
            break (plan, raw);
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert!(
        raw.iter().any(|o| o.client_order_id.as_deref() == Some(wire.as_str())),
        "the venue reports the order under its cloid, not our coid: {raw:?}"
    );
    assert!(
        !raw.iter().any(|o| o.client_order_id.as_deref() == Some(coid.as_str())),
        "the raw report never names the coid itself: {raw:?}"
    );
    assert!(adopts(&plan, &coid), "the resting coid must be ADOPTED once translated: {plan:?}");
    assert_eq!(plan.adopt.len(), 1, "only the real coid is adopted: {plan:?}");
    let report = &plan.adopt[0];
    assert_eq!(report.symbol, SYMBOL, "translated onto the unified symbol: {report:?}");
    assert!(
        OrderStatus::parse(&report.status).is_none_or(|s| s.is_live()),
        "an adopted report is live: {report:?}"
    );
    assert!(!is_gone(&plan, &coid), "a resting coid is never gone: {plan:?}");
    assert!(is_gone(&plan, &ghost), "a coid the venue never had is GONE: {plan:?}");
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over the client that has never seen the order -----------------------
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        restarted,
        VENUE,
        SYMBOL,
    );
    // Exactly the core's order: announce to the client, then write the registry.
    let announced: Vec<(String, String)> = plan
        .adopt
        .iter()
        .filter_map(|r| Some((r.client_order_id.clone()?, r.symbol.clone())))
        .collect();
    engine.client.adopt_restored(&announced);
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    let restored_order = engine.registry.get(&coid).expect("the coid is in the registry");
    assert!(restored_order.status.is_live(), "restored live: {:?}", restored_order.status);
    assert!(restored_order.venue_order_id.is_some(), "the venue order id came across");

    engine.cancel_order(&coid);
    let cancel_events = wait_for_events(&mut restarted_rx, "the restored order's cancel", |evs| {
        evs.iter().any(|e| {
            matches!(e, Event::OrderCanceled(c) if c.client_order_id == coid)
                || matches!(e, Event::OrderCancelRejected(c) if c.client_order_id == coid)
        })
    });
    assert!(
        !cancel_events.iter().any(|e| matches!(e, Event::OrderCancelRejected(_))),
        "the engine cancel was refused (before 0121: no local order mapping): {cancel_events:?}"
    );
    assert!(
        !cancel_events.iter().any(|e| format!("{e:?}").contains(&wire)),
        "an event of the restored order carried the raw cloid, so the registry was not seeded: \
         {cancel_events:?}"
    );
    wait_until_gone(&recon, &engine.client, &restored, &coid);

    // --- 4. the event the client emitted, folded -----------------------------------------------
    let canceled = cancel_events
        .iter()
        .find(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == coid))
        .expect("the client reported the cancel");
    engine.on_event(canceled, &mut Outbox::default());
    assert_eq!(
        engine.registry[&coid].status,
        OrderStatus::Canceled,
        "the fold must turn the restored order terminal"
    );

    drop(cleanup);
    tracing::info!(target: "vike_hyperliquid", "restore smoke green: placed -> 0x cloid reported -> translated -> adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled");
}
