//! A STOP that FIRES, and a STOP the venue cancels itself, on the OKX DEMO account, through the whole
//! adapter: `vike_okx::execution::spawn` runs the real `run()` (the private `orders` pump, the business
//! `orders-algo` pump and their fence), the events are read off the client's own lane and folded through a
//! real `Engine`. The live proof of `crates/bridges/okx/src/execution/event_mapper.rs`'s `row_order_client_id` (a fired
//! stop's child order is filed under the stop's `algoClOrdId`, not under the venue's own `O...` id) and of
//! `crates/bridges/okx/src/execution/user_data.rs`'s `spawn_okx_algo_user_data_with_resync` (a stop's own state:
//! `effective` -> `OrderTriggered`, `canceled` -> `OrderCanceled`). `tests/offline/triggered_stop.rs` and
//! `tests/offline/algo_channel.rs` replay the captured frames offline; `okx_algo_socket_smoke.rs` reads the
//! socket alone (no pump thread). This file runs the pump.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture --test-threads=1 okx_fired_stop_smoke::
//!
//! ORDER-PLACING (`BTC-USDT-SWAP`), DEMO ONLY, 0.0001 BTC (the instrument's minimum): the first test
//! opens a long with a market order, rests a reduce-only SELL stop a few dollars under the last price and
//! lets it FIRE; the second rests a far stop and closes the position, and OKX cancels that reduce-only stop
//! BY ITSELF a second later (measured 2026-10-24); the third opens a long, lets a reduce-only SELL STOP-LIMIT
//! fire into a marketable limit that FILLS (the child is a regular limit under the venue's own `clOrdId`)
//! and judges the result as a restart would (the recon client's reports through `plan_restore`). All refuse
//! to start on a non-flat account, and the
//! cleanup (a `Drop` guard) cancels every stop it placed and flattens whatever is left, on every exit path.
//! A run killed from outside skips destructors: check the demo position and algo list by hand.
//!
//! Gated like every `*_smoke.rs` here: the DEMO account's creds through `vike_secrets::smoke_directory`
//! (asked for `AccountTier::Demo`), a `tracing::warn!` and an early return when absent; every transport is
//! `rest_transport(true)` and the client is spawned with `mainnet = false`. Credentials are never
//! printed: only events, venue words and ids of this smoke's own orders reach stdout.
//!
//! What it MEASURES (printed as `EVENT ...` per lane event and `VENUE ...` per REST read-back).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::demo_credentials::load_demo_credentials;
use serde_json::{Value, json};
use vike_bridge_core::exec_actor::ExecutionClientActor;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::lifecycle_word;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_execution::recon::{
    RESTORE_QUIET_PASSES, ReconcileClient, RestoreScope, RestoredOrderRef, plan_restore,
};
use vike_execution::testing::RecordingClient;
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, ExecutionClient, Ingest, RiskGate,
    event_channel,
};
use vike_model::events::Event;
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType, RiskLimits, now_ms};
use vike_okx::ReconcileClientOkx;
use vike_okx::execution::rest::{
    PATH_INSTRUMENTS, PATH_ORDER_ALGO, PATH_ORDER_CREATE, PATH_ORDERS_HISTORY, VenueRestOkx,
    parse_okx_perp_instruments,
};
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;
use vike_okx::transport::client::unwrap_okx;

const SYMBOL: &str = "BTC-USDT-SWAP";

type SwapRest = VenueRestOkx<RestTransportJsonUreq>;

/// Both tests trade the one demo account: never at the same time, whatever the harness' thread count.
static SERIAL: Mutex<()> = Mutex::new(());

fn has(events: &[Event], order_client_id: &str, word: &str) -> bool {
    events.iter().filter_map(lifecycle_word).any(|(c, k)| c == order_client_id && k == word)
}

fn market(order_client_id: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL, "side": side, "qty": qty,
        "order_type": "market", "reduce_only": reduce_only, "ts": now_ms()
    }))
    .unwrap()
}

/// A reduce-only SELL stop-limit: the stop's request with its `limit` as the price.
fn stop_limit_sell(order_client_id: &str, qty: f64, trigger: f64, limit: f64) -> OrderRequest {
    OrderRequest {
        order_type: OrderType::StopLimit,
        price: Some(limit),
        ..stop_sell(order_client_id, qty, trigger)
    }
}

fn stop_sell(order_client_id: &str, qty: f64, trigger: f64) -> OrderRequest {
    serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL, "side": -1, "qty": qty,
        "order_type": "stop", "trigger_price": trigger, "reduce_only": true, "ts": now_ms()
    }))
    .unwrap()
}

/// The venue's position in BASE units (signed), as the live reconcile pass reads it (the production
/// `ReconcileClientOkx`'s position report), or the venue's error in words.
fn position(recon: &dyn ReconcileClient) -> Result<f64, String> {
    let rows = recon.fetch_position_status_reports()?;
    Ok(rows.iter().filter(|p| p.symbol == SYMBOL).map(|p| p.qty).sum())
}

/// Close whatever is open with a reduce-only market order on the bridge's REST. Never panics (it runs
/// while a failed assertion unwinds).
fn flatten(rest: &SwapRest, recon: &dyn ReconcileClient, leg: f64) {
    match position(recon) {
        Ok(q) if q.abs() > leg * 0.5 => {
            let order_client_id = format!("vtflat{}", now_ms() % 100_000_000);
            let events = VenueRest::submit_order(
                rest,
                &market(&order_client_id, vike_model::closing_side(q), q.abs(), true),
            );
            println!("CLEANUP flatten {q}: {events:?}");
        }
        Ok(_) => {}
        Err(e) => println!("CLEANUP could not read the {SYMBOL} position ({e}): CHECK IT BY HAND"),
    }
}

/// The demo account, the adapter under test, its lane and the cleanup. DEMO ONLY by construction: the
/// credentials were asked for as `Demo`, every transport sends the simulated-trading header and the
/// client is spawned with `mainnet = false`.
struct Harness {
    rest: SwapRest,
    reconcile: ReconcileClientOkx<RestTransportJsonUreq>,
    client: ExecutionClientActor,
    pull: Box<dyn FnMut() -> Option<Ingest>>,
    events: Vec<Event>,
    started: Instant,
    since_ms: i64,
    /// One minimum lot, in BASE units.
    leg: f64,
    /// Every stop this run placed: cancelled (idempotently) on the way out.
    stops: Vec<String>,
}

impl Harness {
    fn start() -> Option<Self> {
        let creds = load_demo_credentials()?;
        assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");
        let public = rest_transport(true);
        assert_eq!(
            public.network(),
            vike_model::VenueNetwork::Testnet,
            "this smoke is DEMO ONLY: the simulated-trading header must be on"
        );
        let net = demo_net();
        let info = public
            .public(
                &net.rest,
                PATH_INSTRUMENTS,
                &[("instType", "SWAP".into()), ("instId", SYMBOL.into())],
            )
            .expect("public instruments");
        let instrument = parse_okx_perp_instruments(
            &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
        )[SYMBOL]
            .clone();
        let leg = instrument.properties.min_qty * instrument.ct_val;
        let rest = VenueRestOkx {
            signer: OkxV5Signer::new(&creds, now_ms),
            transport: rest_transport(true)
                .with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
            base_url: net.rest.clone(),
            symbol: SYMBOL.to_string(),
            properties: instrument.properties,
            ct_val: instrument.ct_val,
            leverage: 2.0,
            broker_code: None,
        };
        assert_eq!(
            rest.transport.network(),
            vike_model::VenueNetwork::Testnet,
            "this smoke is DEMO ONLY"
        );
        let recon = ReconcileClientOkx::new(
            OkxV5Signer::new(&creds, now_ms),
            rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
            net.rest.clone(),
            SYMBOL,
            rest.ct_val,
        );
        let base0 = position(&recon).expect("position");
        assert!(
            base0.abs() < leg * 0.5,
            "the demo account must be FLAT on {SYMBOL} before this smoke (position {base0}): it \
             flattens what it finds at the end and must not touch someone else's position"
        );
        let (events, mut rx) = event_channel(1024);
        let since_ms = now_ms();
        let client = vike_okx::execution::spawn(creds, SYMBOL.to_string(), events, net);
        std::thread::sleep(Duration::from_secs(5)); // both logins + subscribes settle
        Some(Self {
            rest,
            reconcile: recon,
            client,
            pull: Box::new(move || rx.try_recv().ok()),
            events: Vec::new(),
            started: Instant::now(),
            since_ms,
            leg,
            stops: Vec::new(),
        })
    }

    /// Read the lane until `done` accepts everything seen so far, or `secs` pass. Every lifecycle event is
    /// printed once, with the time it arrived.
    fn pump_until(&mut self, secs: u64, done: impl Fn(&[Event]) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            while let Some(ingest) = (self.pull)() {
                if let Ingest::Event(e) = ingest {
                    let at = self.started.elapsed().as_millis();
                    if lifecycle_word(&e).is_some() {
                        println!(
                            "EVENT +{at:>6}ms {}",
                            format!("{e:?}").chars().take(300).collect::<String>()
                        );
                    }
                    self.events.push(e);
                }
            }
            if done(&self.events) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Open (or close, `reduce_only`) with a market order through the adapter and wait for its fill.
    fn market(&mut self, tag: &str, side: i32, reduce_only: bool) -> String {
        let order_client_id = format!("{tag}{}", now_ms() % 100_000_000);
        self.client.submit(&market(&order_client_id, side, self.leg, reduce_only));
        assert!(
            self.pump_until(15, |e| has(e, &order_client_id, "Accepted")),
            "{order_client_id} never Accepted"
        );
        assert!(
            self.pump_until(40, |e| has(e, &order_client_id, "Filled")),
            "{order_client_id} never Filled"
        );
        order_client_id
    }

    /// Rest a reduce-only SELL stop through the adapter; `false` when the venue refused it.
    fn place_stop(&mut self, order_client_id: &str, trigger: f64) -> bool {
        self.stops.push(order_client_id.to_string());
        self.client.submit(&stop_sell(order_client_id, self.leg, trigger));
        self.pump_until(20, |e| {
            has(e, order_client_id, "Accepted") || has(e, order_client_id, "Rejected")
        });
        has(&self.events, order_client_id, "Accepted")
    }

    /// The stop's events, in lane order, as `Word` strings.
    fn words(&self, order_client_id: &str) -> Vec<&'static str> {
        self.events
            .iter()
            .filter_map(lifecycle_word)
            .filter(|(c, _)| *c == order_client_id)
            .map(|(_, k)| k)
            .collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        for order_client_id in &self.stops {
            if let Err(e) = self.rest.cancel_order(order_client_id) {
                println!(
                    "CLEANUP: bridge cancel of {order_client_id} answered {} {}",
                    e.code, e.msg
                );
            }
        }
        flatten(&self.rest, &self.reconcile, self.leg);
        self.client.detach();
    }
}

/// A real engine holding `req` as the core registers a submit, with this order's lane events folded in
/// arrival order.
fn fold(req: &OrderRequest, events: &[Event]) -> Engine<RecordingClient> {
    let mut engine = Engine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "okx",
        SYMBOL,
    );
    engine.registry.insert(req.order_client_id.clone(), OrderRecord::new(req.clone()));
    let mut outbox = EventBusOutbox::default();
    for e in events {
        if lifecycle_word(e).is_some_and(|(c, _)| c == req.order_client_id) {
            engine.on_event(e, &mut outbox);
        }
    }
    engine
}

/// The venue's own `state` word for a stop (`GET order-algo` by `algoClOrdId`), `None` when unreadable.
fn algo_state(rest: &SwapRest, order_client_id: &str) -> Option<String> {
    let rows = rest
        .transport
        .signed_requery(
            &rest.base_url,
            PATH_ORDER_ALGO,
            "GET",
            &[("algoClOrdId", json!(order_client_id))],
            &rest.signer,
        )
        .and_then(unwrap_okx)
        .ok()?;
    rows[0]["state"].as_str().map(str::to_string)
}

/// The named fields of a venue row, as `key=value` text.
fn pick(row: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| format!("{k}={}", row.get(*k).unwrap_or(&Value::Null)))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
#[ignore = "network + demo creds — opens 0.0001 BTC on the demo account, lets a stop fire, flattens — run manually (see module doc)"]
fn okx_a_stop_that_fires_ends_filled_under_its_own_order_client_id() {
    vike_log::test_init();
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut h) = Harness::start() else { return };

    // --- 1. a long, so a reduce-only SELL stop has something to close -----------------------------
    let entry = h.market("vtfent", 1, false);
    println!("ENTRY {entry} filled; position now {:?}", position(&h.reconcile));

    // --- 2. a stop a few dollars under the market; re-placed at the new price while it does not fire.
    // (A trigger the venue calls "not under the last price" is refused: the price moved between the
    // ticker read and the order; try again at once.)
    let mut fired = None;
    for attempt in 0..10 {
        let distance = [10.0_f64, 6.0, 4.0][attempt.min(2)];
        let last = h.rest.last_price().expect("ticker");
        let trigger = ((last - distance) * 10.0).round() / 10.0;
        let stop = format!("vtfire{}{attempt}", now_ms() % 10_000_000);
        if !h.place_stop(&stop, trigger) {
            println!(
                "MEASURED stop {stop} refused at trigger {trigger} (last {last}): {:?}",
                h.words(&stop)
            );
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        println!(
            "PLACED stop {stop} trigger={trigger} last={last} (attempt {attempt}, {distance} under)"
        );
        let reached = h.pump_until(60, |e| {
            has(e, &stop, "Triggered") || has(e, &stop, "Filled") || has(e, &stop, "Canceled")
        });
        if reached {
            fired = Some((stop, trigger));
            break;
        }
        // Silence is not "the market did not move": ask the venue, which is the truth about the stop.
        let state = algo_state(&h.rest, &stop);
        assert!(
            state.as_deref() != Some("effective"),
            "the stop {stop} FIRED at the venue (order-algo state effective) but the node never booked \
             it Triggered or Filled under its own id; lane order {:?}",
            h.words(&stop)
        );
        println!(
            "MEASURED stop {stop} did not fire in 60 s (market did not reach {trigger}; venue \
             state {state:?}); cancelling"
        );
        h.client.cancel(&stop);
        h.pump_until(15, |e| has(e, &stop, "Canceled"));
    }
    let (stop, trigger) = fired
        .expect("no stop fired in ten attempts: the market was too quiet (the cleanup flattens)");

    // --- 3. it ends Filled, under ITS OWN order client id -------------------------------------------------------
    let filled = h.pump_until(30, |e| has(e, &stop, "Filled"));
    let words = h.words(&stop);
    println!("MEASURED stop {stop} lane order: {words:?}");

    // The venue's own records, read back: the child order carries the venue's id and OUR stop's id.
    match h
        .rest
        .transport
        .signed_requery(
            &h.rest.base_url,
            PATH_ORDER_ALGO,
            "GET",
            &[("algoClOrdId", json!(stop))],
            &h.rest.signer,
        )
        .and_then(unwrap_okx)
    {
        Ok(rows) => println!(
            "VENUE order-algo: {}",
            pick(
                &rows[0],
                &[
                    "state",
                    "slTriggerPx",
                    "last",
                    "triggerTime",
                    "ordIdList",
                    "actualSz",
                    "reduceOnly",
                    "failCode"
                ]
            )
        ),
        Err(e) => println!("VENUE order-algo read failed: {} {}", e.code, e.msg),
    }
    let history = h.rest.call_get(
        PATH_ORDERS_HISTORY,
        &[("instType", json!("SWAP")), ("instId", json!(SYMBOL)), ("limit", json!("20"))],
    );
    let children: Vec<Value> = history
        .ok()
        .and_then(|rows| rows.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r["algoClOrdId"] == stop.as_str())
        .collect();
    for child in &children {
        println!(
            "VENUE child order: {}",
            pick(
                child,
                &["clOrdId", "algoClOrdId", "state", "avgPx", "sz", "source", "side", "reduceOnly"]
            )
        );
    }

    // Reconcile's join: the fills read names the child's fill by the stop's order client id (`trade/fills` itself
    // carries no algo field).
    let deadline = Instant::now() + Duration::from_secs(20);
    let named = loop {
        let fills = h.reconcile.fetch_fill_reports(h.since_ms - 1000).expect("fill reports");
        let named = fills.iter().any(|f| f.order_client_id.as_deref() == Some(stop.as_str()));
        if named || Instant::now() >= deadline {
            println!(
                "MEASURED reconcile fills: {:?}",
                fills
                    .iter()
                    .map(|f| (f.order_client_id.clone(), f.venue_order_id.clone()))
                    .collect::<Vec<_>>()
            );
            break named;
        }
        std::thread::sleep(Duration::from_secs(1));
    };

    // The flat check comes before the asserts so a failure still reports it.
    std::thread::sleep(Duration::from_secs(2));
    let after = position(&h.reconcile);
    println!("MEASURED position after the stop fired: {after:?}");

    // The venue's own child id must never have reached the lane as an order id.
    let stranger: Vec<&str> = h
        .events
        .iter()
        .filter_map(lifecycle_word)
        .map(|(c, _)| c)
        .filter(|c| c.starts_with('O') && c[1..].bytes().all(|b| b.is_ascii_digit()))
        .collect();

    assert!(filled, "the stop {stop} never ended Filled under its own id; lane order {words:?}");
    assert!(
        has(&h.events, &stop, "Triggered"),
        "the algo socket never said the stop fired: {words:?}"
    );
    assert!(
        has(&h.events, &stop, "Fill"),
        "the child's fill was not filed under {stop}: {words:?}"
    );
    assert!(
        stranger.is_empty(),
        "an event carried the venue's own child id {stranger:?} (the fill booked on an id the core never minted)"
    );
    assert!(named, "reconcile's fill read did not name the child's fill by {stop}");
    assert!(
        after.is_ok_and(|q| q.abs() < h.leg * 0.5),
        "the fired stop must have closed the position"
    );

    let engine = fold(&stop_sell(&stop, h.leg, trigger), &h.events);
    println!(
        "MEASURED engine after the fold: status={:?} filled_qty={} dropped_terminal_on_live={}",
        engine.registry[&stop].status,
        engine.registry[&stop].filled_qty,
        engine.dropped_terminal_on_live
    );
    assert_eq!(
        engine.registry[&stop].status,
        OrderStatus::Filled,
        "the stop must end FILLED, not ACCEPTED for good"
    );
    println!(
        "okx fired-stop smoke green: {stop} Submitted -> Accepted -> Triggered/Fill -> Filled under its own order client id"
    );
}

#[test]
#[ignore = "network + demo creds — opens 0.0001 BTC on the demo account, closes it under a resting stop, flattens — run manually (see module doc)"]
fn okx_a_stop_the_venue_cancels_is_booked_canceled() {
    vike_log::test_init();
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut h) = Harness::start() else { return };

    // --- 1. a long and a far reduce-only stop that cannot trigger in this test's lifetime ---------
    let entry = h.market("vtvent", 1, false);
    println!("ENTRY {entry} filled; position now {:?}", position(&h.reconcile));
    let last = h.rest.last_price().expect("ticker");
    let trigger = (last * 0.5 * 10.0).round() / 10.0;
    let stop = format!("vtvcan{}", now_ms() % 100_000_000);
    assert!(h.place_stop(&stop, trigger), "the far stop must be accepted: {:?}", h.words(&stop));
    println!("PLACED stop {stop} trigger={trigger} last={last}");

    // --- 2. close the position under it; WE never cancel the stop ------------------------------------
    let exit = h.market("vtvext", -1, true);
    println!("EXIT {exit} filled; position now {:?}", position(&h.reconcile));

    // OKX cancels a reduce-only stop whose position is gone (measured: about a second later) and says
    // so only on the business socket's `orders-algo` channel.
    let canceled = h.pump_until(30, |e| has(e, &stop, "Canceled"));
    let words = h.words(&stop);
    println!("MEASURED stop {stop} lane order: {words:?}");
    let reason = h.events.iter().find_map(|e| match e {
        Event::OrderCanceled(c) if c.order_client_id == stop => Some(c.reason.to_string()),
        _ => None,
    });
    println!("MEASURED the cancel's reason: {reason:?}");
    assert!(
        canceled,
        "the venue cancelled the reduce-only stop {stop} but the node never heard it (stays ACCEPTED for good): {words:?}"
    );
    let engine = fold(&stop_sell(&stop, h.leg, trigger), &h.events);
    println!("MEASURED engine after the fold: status={:?}", engine.registry[&stop].status);
    assert_eq!(engine.registry[&stop].status, OrderStatus::Canceled);
    println!(
        "okx venue-cancelled-stop smoke green: {stop} Submitted -> Accepted -> Canceled, no cancel of ours"
    );
}

/// A STOP-LIMIT that fires AND fills, through the whole adapter: its child is a regular LIMIT order under
/// the venue's own `clOrdId`, and every event of it must reach the lane under OUR order client id (`row_order_client_id`), as a
/// fired stop's market child does. A reduce-only SELL stop-limit rests a few dollars under the market with
/// its limit 60 USD under its trigger, so the order the trigger places takes the bid at once. Then the
/// same order is judged as a RESTART would judge it: the recon client's reports through the real restore
/// planner, for a record that has counted every quiet pass.
#[test]
#[ignore = "network + demo creds — opens 0.0001 BTC on the demo account, lets a stop-limit fire and fill, flattens — run manually (see module doc)"]
fn okx_a_stop_limit_that_fires_and_fills_ends_filled_under_its_own_order_client_id() {
    vike_log::test_init();
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut h) = Harness::start() else { return };

    // --- 1. a long, so a reduce-only SELL stop-limit has something to close ------------------------------
    let entry = h.market("vtlfent", 1, false);
    println!("ENTRY {entry} filled; position now {:?}", position(&h.reconcile));

    // --- 2. the stop-limit a few dollars under the market; re-placed at the new price while it does not fire
    let mut fired = None;
    for attempt in 0..10 {
        let distance = [10.0_f64, 6.0, 4.0][attempt.min(2)];
        let last = h.rest.last_price().expect("ticker");
        let trigger = ((last - distance) * 10.0).round() / 10.0;
        let limit = trigger - 60.0;
        let stop = format!("vtlffire{}{attempt}", now_ms() % 10_000_000);
        h.stops.push(stop.clone());
        h.client.submit(&stop_limit_sell(&stop, h.leg, trigger, limit));
        h.pump_until(20, |e| has(e, &stop, "Accepted") || has(e, &stop, "Rejected"));
        if !has(&h.events, &stop, "Accepted") {
            println!(
                "MEASURED stop-limit {stop} refused at trigger {trigger} (last {last}): {:?}",
                h.words(&stop)
            );
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        println!(
            "PLACED stop-limit {stop} trigger={trigger} limit={limit} last={last} (attempt {attempt})"
        );
        let reached = h.pump_until(60, |e| {
            has(e, &stop, "Triggered") || has(e, &stop, "Filled") || has(e, &stop, "Canceled")
        });
        if reached {
            fired = Some((stop, trigger, limit));
            break;
        }
        let state = algo_state(&h.rest, &stop);
        assert!(
            state.as_deref() != Some("effective"),
            "the stop-limit {stop} FIRED at the venue (order-algo state effective) but the node never \
             booked it Triggered or Filled under its own id; lane order {:?}",
            h.words(&stop)
        );
        println!(
            "MEASURED stop-limit {stop} did not fire in 60 s (venue state {state:?}); cancelling"
        );
        h.client.cancel(&stop);
        h.pump_until(15, |e| has(e, &stop, "Canceled"));
    }
    let (stop, trigger, limit) = fired.expect(
        "no stop-limit fired in ten attempts: the market was too quiet (the cleanup flattens)",
    );

    // --- 3. MEASURE: the lane's events, the venue's own records ---------------------------------------------
    let filled = h.pump_until(30, |e| has(e, &stop, "Filled"));
    let words = h.words(&stop);
    println!("MEASURED stop-limit {stop} lane order: {words:?}");
    for e in h.events.iter().filter(|e| lifecycle_word(e).is_some_and(|(c, _)| c == stop)) {
        println!("MEASURED lane event of {stop}: {e:?}");
    }
    let algo_row = h
        .rest
        .transport
        .signed_requery(
            &h.rest.base_url,
            PATH_ORDER_ALGO,
            "GET",
            &[("algoClOrdId", json!(stop))],
            &h.rest.signer,
        )
        .and_then(unwrap_okx);
    match &algo_row {
        Ok(rows) => println!(
            "VENUE order-algo: {}",
            pick(
                &rows[0],
                &[
                    "state",
                    "slTriggerPx",
                    "slOrdPx",
                    "ordIdList",
                    "actualPx",
                    "actualSz",
                    "triggerTime"
                ]
            )
        ),
        Err(e) => println!("VENUE order-algo read failed: {} {}", e.code, e.msg),
    }
    // The child by the `ordId` the stop names (`orders-history` may not list it yet).
    let child_id = algo_row.as_ref().ok().and_then(|rows| rows[0]["ordIdList"][0].as_str());
    let children: Vec<Value> = child_id
        .and_then(|id| {
            h.rest
                .call_get(PATH_ORDER_CREATE, &[("instId", json!(SYMBOL)), ("ordId", json!(id))])
                .ok()
        })
        .and_then(|rows| rows.as_array().cloned())
        .unwrap_or_default();
    for child in &children {
        println!(
            "VENUE child order: {}",
            pick(
                child,
                &[
                    "ordId",
                    "clOrdId",
                    "algoClOrdId",
                    "ordType",
                    "px",
                    "avgPx",
                    "accFillSz",
                    "state",
                    "source",
                    "fillTime"
                ]
            )
        );
    }

    // --- 4. a RESTART's view: the recon client's reports through the restore planner -----------------------
    let deadline = Instant::now() + Duration::from_secs(20);
    let (orders, fills) = loop {
        let fills = h.reconcile.fetch_fill_reports(h.since_ms - 1000).expect("fill reports");
        let named = fills.iter().any(|f| f.order_client_id.as_deref() == Some(stop.as_str()));
        if named || Instant::now() >= deadline {
            break (h.reconcile.fetch_order_status_reports(0).expect("order report"), fills);
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    for f in &fills {
        println!("MEASURED reconcile fill: {f:?}");
    }
    let listed = orders.iter().find(|r| r.order_client_id.as_deref() == Some(stop.as_str()));
    println!("MEASURED order report row of the filled stop-limit: {listed:?}");
    let scope = RestoreScope {
        venue: "okx".to_string(),
        account: None,
        symbols: vec![SYMBOL.to_string()],
        lists_trigger_orders: h.reconcile.lists_trigger_orders(),
    };
    let record = RestoredOrderRef {
        order_client_id: stop.clone(),
        venue: Some("okx".to_string()),
        symbol: Some(SYMBOL.to_string()),
        account: None,
        order_type: Some(OrderType::StopLimit),
        venue_order_id: None,
        quiet_passes: RESTORE_QUIET_PASSES,
    };
    let plan = plan_restore(&scope, &[record], &orders, &fills);
    println!("MEASURED restore plan of the filled stop-limit: {plan:?}");

    std::thread::sleep(Duration::from_secs(2));
    let after = position(&h.reconcile);
    println!("MEASURED position after the stop-limit filled: {after:?}");
    let stranger: Vec<&str> = h
        .events
        .iter()
        .filter_map(lifecycle_word)
        .map(|(c, _)| c)
        .filter(|c| c.starts_with('O') && c[1..].bytes().all(|b| b.is_ascii_digit()))
        .collect();

    // --- 5. ASSERT, last --------------------------------------------------------------------------------------
    assert!(
        filled,
        "the stop-limit {stop} never ended Filled under its own id; lane order {words:?}"
    );
    assert!(has(&h.events, &stop, "Triggered"), "the algo socket never said it fired: {words:?}");
    assert!(
        has(&h.events, &stop, "Fill"),
        "the child's fill was not filed under {stop}: {words:?}"
    );
    assert!(
        stranger.is_empty(),
        "an event carried the venue's own child id {stranger:?} (the fill booked on an id the core never minted)"
    );
    let child = children.first().expect("GET order of the stop-limit's child (ordIdList)");
    assert_eq!(child["ordType"], "limit", "a stop-limit fires a LIMIT order");
    assert!(
        fills.iter().any(|f| f.order_client_id.as_deref() == Some(stop.as_str())),
        "reconcile's fill read did not name the child's fill by {stop}"
    );
    assert!(
        listed.is_none_or(|r| r.status.is_terminal()),
        "a filled stop-limit rests nowhere: {listed:?}"
    );
    assert_eq!(
        plan.filled_while_down,
        std::slice::from_ref(&stop),
        "a restart reads it FILLED: {plan:?}"
    );
    assert!(after.is_ok_and(|q| q.abs() < h.leg * 0.5), "the fill must have closed the position");
    let engine = fold(&stop_limit_sell(&stop, h.leg, trigger, limit), &h.events);
    println!(
        "MEASURED engine after the fold: status={:?} filled_qty={}",
        engine.registry[&stop].status, engine.registry[&stop].filled_qty
    );
    assert_eq!(engine.registry[&stop].status, OrderStatus::Filled, "it must end FILLED");
    println!(
        "okx fired-and-filled stop-limit smoke green: {stop} Accepted -> Triggered/Fill -> Filled under its own order client id, restart reads it filled"
    );
}
