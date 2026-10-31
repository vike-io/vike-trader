//! A STOP that FIRES, and a STOP the venue cancels itself, on the OKX DEMO account, through the whole
//! adapter: `OkxExecutionClient::spawn` runs the real `run()` (the private `orders` pump, the business
//! `orders-algo` pump and their fence), the events are read off the client's own lane and folded through a
//! real `ExecutionEngine`. The live proof of `crates/bridges/okx/src/event_mapper.rs`'s `row_coid` (a fired
//! stop's child order is filed under the stop's `algoClOrdId`, not under the venue's own `O...` id) and of
//! `crates/bridges/okx/src/user_data.rs`'s `spawn_okx_algo_user_data_with_resync` (a stop's own state:
//! `effective` -> `OrderTriggered`, `canceled` -> `OrderCanceled`). `tests/offline/triggered_stop.rs` and
//! `tests/offline/algo_channel.rs` replay the captured frames offline; `okx_algo_socket_smoke.rs` reads the
//! socket alone (no pump thread). This file runs the pump.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_fired_stop_smoke -- --ignored --nocapture --test-threads=1
//!
//! ORDER-PLACING (`BTC-USDT-SWAP`), DEMO ONLY, 0.0001 BTC (the instrument's minimum): the first test
//! opens a long with a market order, rests a reduce-only SELL stop a few dollars under the last price and
//! lets it FIRE; the second rests a far stop and closes the position, and OKX cancels that reduce-only stop
//! BY ITSELF a second later (measured 2026-10-24). Both refuse to start on a non-flat account, and the
//! cleanup (a `Drop` guard) cancels every stop it placed and flattens whatever is left, on every exit path.
//! A run killed from outside skips destructors: check the demo position and algo list by hand.
//!
//! Gated like every `*_smoke.rs` here: the DEMO account's creds through `vike_secrets::smoke_directory`
//! (asked for `AccountTier::Demo`), a `tracing::warn!` and an early return when absent; every transport is
//! `UreqOkxTransport::new(true)` and the client is spawned with `mainnet = false`. Credentials are never
//! printed: only events, venue words and ids of this smoke's own orders reach stdout.
//!
//! What it MEASURES (printed as `EVENT ...` per lane event and `VENUE ...` per REST read-back).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_exec::recon::ReconClient;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Ingest, Outbox, RiskGate,
    event_channel,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, ManagedOrder, OrderRequest, OrderStatus, RiskLimits, now_ms};
use vike_okx::perp::{
    OkxPerpRest, PATH_INSTRUMENTS, PATH_ORDER_ALGO, PATH_ORDERS_HISTORY, REST,
    parse_okx_perp_instruments,
};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};
use vike_okx::{OkxExecutionClient, OkxReconClient};
use vike_secrets::credentials::{Credentials, api_credentials};

const SYMBOL: &str = "BTC-USDT-SWAP";

type SwapRest = OkxPerpRest<UreqOkxTransport>;

/// Both tests trade the one demo account: never at the same time, whatever the harness' thread count.
static SERIAL: Mutex<()> = Mutex::new(());

fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "okx");
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

/// The `(client_order_id, word)` of an order-lifecycle event, `None` for anything else.
fn kind(e: &Event) -> Option<(&str, &'static str)> {
    Some(match e {
        Event::OrderSubmitted(x) => (&x.client_order_id, "Submitted"),
        Event::OrderAccepted(x) => (&x.client_order_id, "Accepted"),
        Event::OrderRejected(x) => (&x.client_order_id, "Rejected"),
        Event::OrderTriggered(x) => (&x.client_order_id, "Triggered"),
        Event::OrderFilled(x) => (&x.client_order_id, "Filled"),
        Event::OrderPartiallyFilled(x) => (&x.client_order_id, "PartiallyFilled"),
        Event::OrderCanceled(x) => (&x.client_order_id, "Canceled"),
        Event::OrderCancelRejected(x) => (&x.client_order_id, "CancelRejected"),
        Event::Fill(x) => (&x.client_order_id, "Fill"),
        _ => return None,
    })
}

fn has(events: &[Event], coid: &str, word: &str) -> bool {
    events.iter().filter_map(kind).any(|(c, k)| c == coid && k == word)
}

fn market(coid: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL, "side": side, "qty": qty,
        "order_type": "market", "reduce_only": reduce_only, "ts": now_ms()
    }))
    .unwrap()
}

fn stop_sell(coid: &str, qty: f64, trigger: f64) -> OrderRequest {
    serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL, "side": -1, "qty": qty,
        "order_type": "stop", "trigger_price": trigger, "reduce_only": true, "ts": now_ms()
    }))
    .unwrap()
}

/// The venue's position in BASE units (signed), or the venue's error in words.
fn position(rest: &SwapRest) -> Result<f64, String> {
    let snapshot = rest.reconcile_positions().map_err(|e| e.to_string())?;
    Ok(snapshot.positions.iter().filter(|(s, _)| s == SYMBOL).map(|(_, q)| *q).sum())
}

/// Close whatever is open with a reduce-only market order on the bridge's REST. Never panics (it runs
/// while a failed assertion unwinds).
fn flatten(rest: &SwapRest, leg: f64) {
    match position(rest) {
        Ok(q) if q.abs() > leg * 0.5 => {
            let coid = format!("vtflat{}", now_ms() % 100_000_000);
            let events = VenueRest::submit_order(
                rest,
                &market(&coid, vike_model::closing_side(q), q.abs(), true),
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
    recon: OkxReconClient<UreqOkxTransport>,
    client: OkxExecutionClient,
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
        let creds = load_demo_creds()?;
        assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");
        let public = UreqOkxTransport::new(true);
        assert!(
            public.simulated,
            "this smoke is DEMO ONLY: the simulated-trading header must be on"
        );
        let info = public
            .public(
                REST,
                PATH_INSTRUMENTS,
                &[("instType", "SWAP".into()), ("instId", SYMBOL.into())],
            )
            .expect("public instruments");
        let inst = parse_okx_perp_instruments(
            &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
        )[SYMBOL]
            .clone();
        let leg = inst.properties.min_qty * inst.ct_val;
        let rest = OkxPerpRest {
            signer: OkxV5Signer::new(&creds, now_ms),
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
        let base0 = position(&rest).expect("position");
        assert!(
            base0.abs() < leg * 0.5,
            "the demo account must be FLAT on {SYMBOL} before this smoke (position {base0}): it \
             flattens what it finds at the end and must not touch someone else's position"
        );
        let recon = OkxReconClient::new(
            OkxV5Signer::new(&creds, now_ms),
            UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
            REST,
            SYMBOL,
            rest.ct_val,
        );
        let (events, mut rx) = event_channel(1024);
        let since_ms = now_ms();
        let client = OkxExecutionClient::spawn(
            creds,
            SYMBOL.to_string(),
            inst.properties,
            inst.ct_val,
            events,
            false,
        );
        std::thread::sleep(Duration::from_secs(5)); // both logins + subscribes settle
        Some(Self {
            rest,
            recon,
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
                    if kind(&e).is_some() {
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
        let coid = format!("{tag}{}", now_ms() % 100_000_000);
        self.client.submit(&market(&coid, side, self.leg, reduce_only));
        assert!(self.pump_until(15, |e| has(e, &coid, "Accepted")), "{coid} never Accepted");
        assert!(self.pump_until(40, |e| has(e, &coid, "Filled")), "{coid} never Filled");
        coid
    }

    /// Rest a reduce-only SELL stop through the adapter; `false` when the venue refused it.
    fn place_stop(&mut self, coid: &str, trigger: f64) -> bool {
        self.stops.push(coid.to_string());
        self.client.submit(&stop_sell(coid, self.leg, trigger));
        self.pump_until(20, |e| has(e, coid, "Accepted") || has(e, coid, "Rejected"));
        has(&self.events, coid, "Accepted")
    }

    /// The stop's events, in lane order, as `Word` strings.
    fn words(&self, coid: &str) -> Vec<&'static str> {
        self.events.iter().filter_map(kind).filter(|(c, _)| *c == coid).map(|(_, k)| k).collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        for coid in &self.stops {
            if let Err(e) = self.rest.cancel_order(coid) {
                println!("CLEANUP: bridge cancel of {coid} answered {} {}", e.code, e.msg);
            }
        }
        flatten(&self.rest, self.leg);
        self.client.detach();
    }
}

/// A real engine holding `req` as the core registers a submit, with this order's lane events folded in
/// arrival order.
fn fold(req: &OrderRequest, events: &[Event]) -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "okx",
        SYMBOL,
    );
    engine.registry.insert(req.client_order_id.clone(), ManagedOrder::new(req.clone()));
    let mut outbox = Outbox::default();
    for e in events {
        if kind(e).is_some_and(|(c, _)| c == req.client_order_id) {
            engine.on_event(e, &mut outbox);
        }
    }
    engine
}

/// The venue's own `state` word for a stop (`GET order-algo` by `algoClOrdId`), `None` when unreadable.
fn algo_state(rest: &SwapRest, coid: &str) -> Option<String> {
    let rows = rest
        .transport
        .signed_requery(
            &rest.base_url,
            PATH_ORDER_ALGO,
            "GET",
            &[("algoClOrdId", json!(coid))],
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
fn okx_a_stop_that_fires_ends_filled_under_its_own_coid() {
    vike_log::test_init();
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut h) = Harness::start() else { return };

    // --- 1. a long, so a reduce-only SELL stop has something to close -----------------------------
    let entry = h.market("vtfent", 1, false);
    println!("ENTRY {entry} filled; position now {:?}", position(&h.rest));

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

    // --- 3. it ends Filled, under ITS OWN coid -------------------------------------------------------
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

    // Reconcile's join: the fills read names the child's fill by the stop's coid (`trade/fills` itself
    // carries no algo field).
    let deadline = Instant::now() + Duration::from_secs(20);
    let named = loop {
        let fills = h.recon.fetch_fill_reports(h.since_ms - 1000).expect("fill reports");
        let named = fills.iter().any(|f| f.client_order_id.as_deref() == Some(stop.as_str()));
        if named || Instant::now() >= deadline {
            println!(
                "MEASURED reconcile fills: {:?}",
                fills
                    .iter()
                    .map(|f| (f.client_order_id.clone(), f.venue_order_id.clone()))
                    .collect::<Vec<_>>()
            );
            break named;
        }
        std::thread::sleep(Duration::from_secs(1));
    };

    // The flat check comes before the asserts so a failure still reports it.
    std::thread::sleep(Duration::from_secs(2));
    let after = position(&h.rest);
    println!("MEASURED position after the stop fired: {after:?}");

    // The venue's own child id must never have reached the lane as an order id.
    let stranger: Vec<&str> = h
        .events
        .iter()
        .filter_map(kind)
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
        "okx fired-stop smoke green: {stop} Submitted -> Accepted -> Triggered/Fill -> Filled under its own coid"
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
    println!("ENTRY {entry} filled; position now {:?}", position(&h.rest));
    let last = h.rest.last_price().expect("ticker");
    let trigger = (last * 0.5 * 10.0).round() / 10.0;
    let stop = format!("vtvcan{}", now_ms() % 100_000_000);
    assert!(h.place_stop(&stop, trigger), "the far stop must be accepted: {:?}", h.words(&stop));
    println!("PLACED stop {stop} trigger={trigger} last={last}");

    // --- 2. close the position under it; WE never cancel the stop ------------------------------------
    let exit = h.market("vtvext", -1, true);
    println!("EXIT {exit} filled; position now {:?}", position(&h.rest));

    // OKX cancels a reduce-only stop whose position is gone (measured: about a second later) and says
    // so only on the business socket's `orders-algo` channel.
    let canceled = h.pump_until(30, |e| has(e, &stop, "Canceled"));
    let words = h.words(&stop);
    println!("MEASURED stop {stop} lane order: {words:?}");
    let reason = h.events.iter().find_map(|e| match e {
        Event::OrderCanceled(c) if c.client_order_id == stop => Some(c.reason.to_string()),
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
