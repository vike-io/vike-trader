//! Restore-into-registry LIVE **testnet** smoke (the proof `vike_venues::venue_restore`'s
//! hyperliquid row waits for): an order a PREVIOUS session left resting at the Hyperliquid testnet
//! is found by the real `ReconcileClientHyperliquid` under its `0x` cloid, read back into its order client id
//! through the client's own `wire_id_for` (exactly what vike-core's `restore_orders_at_pass` does),
//! planned as ADOPT, put into a FRESH `Engine`, announced to a FRESH client with
//! `adopt_restored`, and then cancelled THROUGH that engine, which before decision 0121 died as
//! "no local order mapping to cancel".
//!
//!     cargo test -p vike-hyperliquid --test hyperliquid_smokes -- --ignored --nocapture hyperliquid_restore_smoke::
//!
//! ORDER-PLACING, **TESTNET ONLY**: the demo tier is the only one this file loads credentials at
//! (`common`'s `demo_credentials`, `AccountTier::Demo`), and the first thing the test does after loading
//! credentials is assert the network is `Testnet`; there is no `AccountTier::Live` anywhere in it.
//! It rests one far-from-market GTC LIMIT BUY on `BTC` (15% below
//! the mid, sized just over the venue's 10 USD minimum, so it cannot fill) on the testnet account
//! behind the default `demo` hyperliquid account's `PRIVATE_KEY` field, double-gated like every
//! other smoke in this crate: the network and that key in the credential store, a `tracing::warn!`
//! and an early return when absent. It is
//! the FIRST test in this crate that puts an order on a wire (`crates/bridges/hyperliquid/CLAUDE.md`
//! says so): treat the first run as an experiment and watch it.
//!
//! What it proves, in order:
//!   1. a "previous session" client places the order under an order client id the smoke mints (the venue accepts
//!      it and reports a resting order);
//!   2. the real recon client reports that order with a `0x` cloid and NOT our order client id; rewriting the
//!      reports through a SECOND, fresh client's `wire_id_for` (the pass's translation step), the
//!      order client id handed to `plan_restore` as a `RestoredOrderRef` beside a made-up order client id lands in `adopt`
//!      with a live status and the made-up order client id in `gone`;
//!   3. a fresh engine over that second client (which has never seen the order) takes `plan.adopt`
//!      through `reregister_orders` and `adopt_restored`, holds the order client id live, and
//!      `Engine::cancel_order` cancels it AT THE VENUE: the client reports a canceled order
//!      for the order client id (never an `OrderCancelRejected`), and no event it emits carries the raw cloid
//!      (the restored registry resolved its own order);
//!   4. the next recon fetch plans the order client id `gone`, never `adopt`, and the event the second client
//!      emitted folds the restored order to `Canceled`.
//!
//! CLEANUP: the placing client is held by a `Drop` guard that cancels the order client id on EVERY exit path (a
//! failed assertion, a panic, a hung poll), idempotently (a second cancel of a gone order is a
//! harmless reject). A run killed from outside (SIGKILL, power loss) skips destructors and leaves the
//! order resting; it is a BUY 15% under the mid that cannot fill, so cancel it by hand on the
//! testnet account.
//!
//! NOT covered: the spot lane (`HYPE/USDC`-style pairs; the translation remaps the coin the same
//! way, but the testnet spot book is thin) and a restored order's `modify` (the exec thread does not
//! know a restored order's oid; it is rejected as before).

use crate::common;
use common::demo_credentials;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ingest::{Pull, wait_for_events};
use vike_bridge_core::exec_actor::ExecutionClientActor;
use vike_execution::recon::{
    ReconcileClient, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore,
};
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, EventSender, ExecutionClient,
    RiskGate, event_channel,
};
use vike_hyperliquid::ReconcileClientHyperliquid;
use vike_hyperliquid::config;
use vike_hyperliquid::instrument::properties::{HyperliquidInstruments, SharedInstruments};
use vike_hyperliquid::signing::Signer;
use vike_model::OrderStatus;
use vike_model::VenueNetwork;
use vike_model::events::Event;
use vike_model::{FillReport, OrderRequest, OrderStatusReport, RiskLimits, now_ms};

/// The perpetual's USER name (decision 0290): its row's coin is `BTC`.
const SYMBOL: &str = "BTCUSDC.P";
const VENUE: &str = "hyperliquid";

/// One exec client on the testnet over its own event lane. A fresh call is a fresh client: an empty
/// cloid registry and an empty asset map, which is what a restarted process has.
fn spawn_client(
    creds: &config::HyperliquidConfig,
    master: &str,
    instruments: &Arc<HyperliquidInstruments>,
) -> (ExecutionClientActor, Pull) {
    let (events, mut rx): (EventSender, _) = event_channel(1024);
    let signer = Signer::from_private_key(creds.private_key.expose(), creds.network)
        .expect("the demo account's PRIVATE_KEY must be a valid secp256k1 key");
    let client = vike_hyperliquid::execution::client::spawn(
        signer,
        master.to_string(),
        Arc::clone(instruments),
        events,
        None,
        None,
        vike_hyperliquid::test_support::seed_execution_urls(creds.network),
    );
    (client, Box::new(move || rx.try_recv().ok()))
}

/// Cancels `order_client_id` through the PLACING client when dropped, so no failure path leaves it resting.
/// The cancel is queued ahead of the client's own `Shutdown` (one FIFO lane), and the client's drop
/// joins its thread, so the request is on the wire before the test binary exits.
struct CancelOnDrop {
    client: ExecutionClientActor,
    order_client_id: String,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        tracing::info!(target: "vike_hyperliquid", order_client_id = %self.order_client_id, "cleanup: cancel queued");
        self.client.cancel(&self.order_client_id);
    }
}

fn restored_ref(order_client_id: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        order_client_id: order_client_id.to_string(),
        venue: Some(VENUE.to_string()),
        symbol: Some(SYMBOL.to_string()),
        account: None,
        order_type: Some(vike_model::OrderType::Limit),
        ..Default::default()
    }
}

/// The pass's translation step, as `vike-core`'s `restore_orders_at_pass` does it: ask `client` for
/// the wire id of each restored order client id and rewrite the reports from wire id back to order client id (and the
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
        .filter_map(|r| {
            Some((
                client.wire_id_for(&r.order_client_id)?,
                (r.order_client_id.clone(), r.symbol.clone()),
            ))
        })
        .collect();
    for o in orders {
        if let Some((order_client_id, symbol)) =
            o.order_client_id.as_deref().and_then(|w| wire.get(w))
        {
            o.order_client_id = Some(order_client_id.clone());
            if let Some(s) = symbol {
                o.symbol = s.clone();
            }
        }
    }
    for f in fills {
        if let Some((order_client_id, symbol)) =
            f.order_client_id.as_deref().and_then(|w| wire.get(w))
        {
            f.order_client_id = Some(order_client_id.clone());
            if let Some(s) = symbol {
                f.symbol = s.clone();
            }
        }
    }
}

/// One planner pass over what the venue reports now, plus the (UNtranslated) order rows it read.
fn plan_pass(
    recon: &dyn ReconcileClient,
    translator: &dyn ExecutionClient,
    restored: &[RestoredOrderRef],
) -> (RestorePlan, Vec<OrderStatusReport>) {
    let scope = RestoreScope {
        venue: VENUE.to_string(),
        account: None,
        symbols: vec![SYMBOL.to_string()],
        lists_trigger_orders: recon.lists_trigger_orders(),
    };
    let raw_orders = recon.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let mut orders = raw_orders.clone();
    let mut fills = recon.fetch_fill_reports(now_ms() - 3_600_000).expect("fetch_fill_reports");
    translate(translator, restored, &mut orders, &mut fills);
    (plan_restore(&scope, restored, &orders, &fills), raw_orders)
}

fn adopts(plan: &RestorePlan, order_client_id: &str) -> bool {
    plan.adopt.iter().any(|r| r.order_client_id.as_deref() == Some(order_client_id))
}

/// Absent from the venue: `gone` (a terminal report) or `unproven` (silence, which is no proof by
/// itself since the restart-safe-orders work: `plan_restore` makes it gone only after the quiet
/// passes the core counts, and this smoke makes a single pass).
fn is_gone(plan: &RestorePlan, order_client_id: &str) -> bool {
    plan.gone.iter().chain(plan.unproven.iter()).any(|r| r.order_client_id == order_client_id)
}

/// Poll until the venue's report no longer keeps `order_client_id` alive (planned `gone`, never `adopt`).
fn wait_until_gone(
    recon: &dyn ReconcileClient,
    translator: &dyn ExecutionClient,
    restored: &[RestoredOrderRef],
    order_client_id: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (plan, _) = plan_pass(recon, translator, restored);
        if is_gone(&plan, order_client_id) && !adopts(&plan, order_client_id) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{order_client_id} still not gone at the venue 20 s after the engine cancel: {plan:?}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "network + TESTNET creds — places + cancels a real (non-filling) testnet order — run manually (see module doc)"]
fn hyperliquid_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some(creds) = demo_credentials() else { return };
    assert_eq!(
        creds.network,
        VenueNetwork::Testnet,
        "this smoke places a real order: TESTNET ONLY"
    );

    let signer = Signer::from_private_key(creds.private_key.expose(), creds.network)
        .expect("the demo account's PRIVATE_KEY must be a valid secp256k1 key");
    let master = creds.account_address.clone().unwrap_or_else(|| signer.address().to_string());
    let transport = vike_hyperliquid::test_support::seed_transport(creds.network);
    let instruments = Arc::new(HyperliquidInstruments::load(&transport, false).expect("meta"));
    let instrument =
        instruments.symbology().by_symbol(SYMBOL).expect("BTCUSDC.P is listed").clone();

    // --- 1. the "previous session": rest a far-from-market GTC LIMIT BUY -----------------------
    let mids = transport.info(&serde_json::json!({ "type": "allMids" })).expect("allMids");
    let mid: f64 = mids
        .get(&instrument.coin)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .expect("a live BTC mid");
    assert!(mid > 0.0, "the BTC mid must be live");
    // 15% below the mid: far enough to never fill inside the test window, close enough that the
    // venue's price band accepts it. Size just over the 10 USD minimum notional, rounded UP to the
    // size grid.
    let limit_px = (mid * 0.85).round();
    let step = 10f64.powi(-(instrument.sz_decimals as i32));
    let qty = ((11.0 / limit_px) / step).ceil() * step;
    let order_client_id = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        order_client_id: order_client_id.clone(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(limit_px),
        timestamp: now_ms(),
        ..Default::default()
    };
    let (mut previous, mut previous_rx) = spawn_client(&creds, &master, &instruments);
    previous.submit(&request);
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup = CancelOnDrop { client: previous, order_client_id: order_client_id.clone() };
    let placed = wait_for_events(
        &mut previous_rx,
        Duration::from_secs(20),
        "the placing client's accept",
        |evs| {
            evs.iter().any(|e| {
                matches!(e, Event::OrderAccepted(a) if a.order_client_id == order_client_id)
                    || matches!(e, Event::OrderRejected(r) if r.order_client_id == order_client_id)
            })
        },
    );
    assert!(
        placed
            .iter()
            .any(|e| matches!(e, Event::OrderAccepted(a) if a.order_client_id == order_client_id)),
        "the testnet order must be ACCEPTED: {placed:?}"
    );

    // --- 2. the restart: a fresh client, the real recon client, the planner --------------------
    let (restarted, mut restarted_rx) = spawn_client(&creds, &master, &instruments);
    let wire = restarted.wire_id_for(&order_client_id).expect("hyperliquid names a wire id");
    assert!(wire.starts_with("0x"), "the wire id is the 0x cloid: {wire}");
    // The live start's recon names a row's coin by its user name through the account's universe.
    let recon = ReconcileClientHyperliquid::new(
        vike_hyperliquid::test_support::seed_transport(creds.network),
        master.clone(),
        vike_model::AssetClass::CryptoPerp,
    )
    .with_instruments(SharedInstruments::new(Arc::clone(&instruments)));
    let restored = [restored_ref(&order_client_id), restored_ref(&ghost)];
    // The venue may take a beat to list the new order in `frontendOpenOrders`.
    let deadline = Instant::now() + Duration::from_secs(20);
    let (plan, raw) = loop {
        let (plan, raw) = plan_pass(&recon, &restarted, &restored);
        if adopts(&plan, &order_client_id) || Instant::now() >= deadline {
            break (plan, raw);
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert!(
        raw.iter().any(|o| o.order_client_id.as_deref() == Some(wire.as_str())),
        "the venue reports the order under its cloid, not our order client id: {raw:?}"
    );
    assert!(
        !raw.iter().any(|o| o.order_client_id.as_deref() == Some(order_client_id.as_str())),
        "the raw report never names the order client id itself: {raw:?}"
    );
    assert!(
        adopts(&plan, &order_client_id),
        "the resting order client id must be ADOPTED once translated: {plan:?}"
    );
    assert_eq!(plan.adopt.len(), 1, "only the real order client id is adopted: {plan:?}");
    let report = &plan.adopt[0];
    assert_eq!(report.symbol, SYMBOL, "translated onto the unified symbol: {report:?}");
    assert!(report.status.is_live(), "an adopted report is live: {report:?}");
    assert!(!is_gone(&plan, &order_client_id), "a resting order client id is never gone: {plan:?}");
    assert!(is_gone(&plan, &ghost), "an order client id the venue never had is GONE: {plan:?}");
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over the client that has never seen the order -----------------------
    let mut engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        restarted,
        VENUE,
        SYMBOL,
    );
    // Exactly the core's order: announce to the client, then write the registry.
    let announced: Vec<vike_execution::RestoredOrder> =
        plan.adopt.iter().filter_map(vike_execution::RestoredOrder::of_report).collect();
    engine.client.adopt_restored(&announced);
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    let restored_order =
        engine.registry.get(&order_client_id).expect("the order client id is in the registry");
    assert!(restored_order.status.is_live(), "restored live: {:?}", restored_order.status);
    assert!(restored_order.venue_order_id.is_some(), "the venue order id came across");

    engine.cancel_order(&order_client_id);
    let cancel_events = wait_for_events(
        &mut restarted_rx,
        Duration::from_secs(20),
        "the restored order's cancel",
        |evs| {
            evs.iter().any(|e| {
                matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id)
                    || matches!(e, Event::OrderCancelRejected(c) if c.order_client_id == order_client_id)
            })
        },
    );
    assert!(
        !cancel_events.iter().any(|e| matches!(e, Event::OrderCancelRejected(_))),
        "the engine cancel was refused (before 0121: no local order mapping): {cancel_events:?}"
    );
    assert!(
        !cancel_events.iter().any(|e| format!("{e:?}").contains(&wire)),
        "an event of the restored order carried the raw cloid, so the registry was not seeded: \
         {cancel_events:?}"
    );
    wait_until_gone(&recon, &engine.client, &restored, &order_client_id);

    // --- 4. the event the client emitted, folded -----------------------------------------------
    let canceled = cancel_events
        .iter()
        .find(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id))
        .expect("the client reported the cancel");
    engine.on_event(canceled, &mut EventBusOutbox::default());
    assert_eq!(
        engine.registry[&order_client_id].status,
        OrderStatus::Canceled,
        "the fold must turn the restored order terminal"
    );

    drop(cleanup);
    tracing::info!(target: "vike_hyperliquid", "restore smoke green: placed -> 0x cloid reported -> translated -> adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled");
}
