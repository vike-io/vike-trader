//! An order Hyperliquid FILLS IN ITS OWN ANSWER, on the **testnet**, through the whole exec client: the
//! lane carries `OrderSubmitted`, `OrderAccepted`, then the fill. The live proof of
//! `crates/bridges/hyperliquid/src/exec/event_mapper.rs`'s `accept_implied_by_fill` as
//! `crates/bridges/hyperliquid/src/exec/state.rs`'s `submit` sends it (`src/exec_tests.rs` pins the same
//! lane order against a fake venue). Before it the lane was `OrderSubmitted`, `OrderFilled` and the
//! engine's order table, which has no `Submitted -> Filled` edge, refused the fill: the order stayed
//! SUBMITTED for good (`terminal event dropped on a live order ... status="SUBMITTED"`).
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-hyperliquid --test hyperliquid_instant_fill_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING, **TESTNET ONLY**: the demo tier is the only one this file loads credentials at
//! (`common`'s `demo_creds`), and the first thing the test does after loading them is assert the network
//! is `Testnet`. It buys 0.0002 BTC with a MARKET order (the exec client sends an emulated IOC at a
//! slippage from the mid, so it crosses the book and the venue answers `filled`), then closes what it
//! holds with a reduce-only market order from the same test. It refuses to start unless the account holds
//! no BTC position, and a `Drop` guard flattens whatever is left on every exit path (a run killed from
//! outside skips destructors: check the testnet BTC position by hand). Double-gated like every smoke in
//! this crate: the network and the demo account's `PRIVATE_KEY` in the credential store, a
//! `tracing::warn!` and an early return when absent. Nothing secret is printed: only events, venue
//! numbers and the ids of this smoke's own orders.

mod common;
use common::demo_creds;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Ingest, Outbox, RiskGate,
    event_channel,
};
use vike_hyperliquid::HyperliquidExecutionClient;
use vike_hyperliquid::base::config::Network;
use vike_hyperliquid::base::transport::HyperliquidTransport;
use vike_hyperliquid::feeds::instruments::HyperliquidInstruments;
use vike_hyperliquid::signing::Signer;
use vike_model::events::Event;
use vike_model::{ManagedOrder, OrderRequest, OrderStatus, OrderType, RiskLimits, now_ms};

const SYMBOL: &str = "BTC";
const VENUE: &str = "hyperliquid";
/// 0.0002 BTC: just over the venue's 10 USD minimum at a BTC price above 50,000.
const QTY: f64 = 0.0002;

type Pull = Box<dyn FnMut() -> Option<Ingest>>;

/// The `(client_order_id, word)` of an order-lifecycle event, `None` for anything else.
fn kind(e: &Event) -> Option<(&str, &'static str)> {
    Some(match e {
        Event::OrderSubmitted(x) => (&x.client_order_id, "Submitted"),
        Event::OrderAccepted(x) => (&x.client_order_id, "Accepted"),
        Event::OrderRejected(x) => (&x.client_order_id, "Rejected"),
        Event::OrderFilled(x) => (&x.client_order_id, "Filled"),
        Event::OrderPartiallyFilled(x) => (&x.client_order_id, "PartiallyFilled"),
        Event::OrderCanceled(x) => (&x.client_order_id, "Canceled"),
        Event::Fill(x) => (&x.client_order_id, "Fill"),
        _ => return None,
    })
}

fn market(coid: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type: OrderType::Market,
        reduce_only,
        ts: now_ms(),
        ..Default::default()
    }
}

/// The account's signed BTC position (0.0 when flat), straight off `clearinghouseState`.
fn position(transport: &HyperliquidTransport, master: &str) -> Result<f64, String> {
    let state = transport
        .info(&json!({ "type": "clearinghouseState", "user": master }))
        .map_err(|e| e.to_string())?;
    Ok(state["assetPositions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["position"]["coin"] == SYMBOL)
        .filter_map(|p| p["position"]["szi"].as_str()?.parse::<f64>().ok())
        .sum())
}

/// The exec client under test plus the cleanup: on every exit path it closes whatever BTC position the
/// account holds, with a reduce-only market order through the same client.
struct Flat {
    client: HyperliquidExecutionClient,
    transport: HyperliquidTransport,
    master: String,
}

impl Drop for Flat {
    fn drop(&mut self) {
        match position(&self.transport, &self.master) {
            Ok(q) if q.abs() > 1e-9 => {
                let coid = format!("vtihflat{}", now_ms() % 100_000_000);
                println!("CLEANUP flatten {q} with {coid}");
                self.client.submit(&market(&coid, vike_model::closing_side(q), q.abs(), true));
                // The client's own drop joins its thread; give the order the time to go out first.
                std::thread::sleep(Duration::from_secs(6));
            }
            Ok(_) => {}
            Err(e) => {
                println!("CLEANUP could not read the {SYMBOL} position ({e}): CHECK IT BY HAND")
            }
        }
    }
}

/// Read the lane until `done` accepts everything seen, or `secs` pass; every lifecycle event is printed
/// once with its arrival time.
fn pump_until(
    pull: &mut Pull,
    seen: &mut Vec<Event>,
    started: Instant,
    secs: u64,
    done: impl Fn(&[Event]) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        while let Some(ingest) = pull() {
            if let Ingest::Event(e) = ingest {
                if kind(&e).is_some() {
                    println!(
                        "EVENT +{:>6}ms {}",
                        started.elapsed().as_millis(),
                        format!("{e:?}").chars().take(300).collect::<String>()
                    );
                }
                seen.push(e);
            }
        }
        if done(seen) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn words(seen: &[Event], coid: &str) -> Vec<&'static str> {
    seen.iter().filter_map(kind).filter(|(c, _)| *c == coid).map(|(_, k)| k).collect()
}

/// A real engine holding `req` as the core registers a submit, with its lane events folded in arrival
/// order.
fn fold(req: &OrderRequest, seen: &[Event]) -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    engine.registry.insert(req.client_order_id.clone(), ManagedOrder::new(req.clone()));
    let mut outbox = Outbox::default();
    for e in seen {
        if kind(e).is_some_and(|(c, _)| c == req.client_order_id) {
            engine.on_event(e, &mut outbox);
        }
    }
    engine
}

/// What one instant-fill order must have put on the lane, and what the engine makes of it.
fn check_instant_fill(req: &OrderRequest, seen: &[Event]) {
    let coid = &req.client_order_id;
    let lane = words(seen, coid);
    let lifecycle: Vec<&str> = lane.iter().copied().filter(|w| *w != "Fill").collect();
    println!("MEASURED {coid} lane order: {lane:?}");
    assert!(
        matches!(lifecycle.as_slice(), ["Submitted", "Accepted", "Filled" | "PartiallyFilled", ..]),
        "{coid}: the answer's own events must be Submitted, Accepted, then the fill; got {lane:?}"
    );
    let accepted = seen.iter().find_map(|e| match e {
        Event::OrderAccepted(a) if a.client_order_id == *coid => Some(a),
        _ => None,
    });
    let oid = accepted.and_then(|a| a.venue_order_id.as_deref());
    println!("MEASURED {coid} Accepted names the venue oid {oid:?}");
    assert!(oid.is_some_and(|o| !o.is_empty()), "{coid}: the implied accept must name the oid");

    let engine = fold(req, seen);
    let order = &engine.registry[coid];
    println!(
        "MEASURED {coid} after the engine fold: status={:?} filled_qty={} avg_fill_px={} \
         dropped_terminal_on_live={}",
        order.status, order.filled_qty, order.avg_fill_px, engine.dropped_terminal_on_live
    );
    let want =
        if lifecycle[2] == "Filled" { OrderStatus::Filled } else { OrderStatus::PartiallyFilled };
    assert_eq!(order.status, want, "{coid}: the engine must walk the order to the fill");
}

#[test]
#[ignore = "network + TESTNET creds — buys 0.0002 BTC at the testnet and sells it back — run manually (see module doc)"]
fn hyperliquid_an_order_filled_in_the_answer_is_accepted_first() {
    vike_log::test_init();
    let Some(creds) = demo_creds() else { return };
    assert_eq!(creds.network, Network::Testnet, "this smoke places real orders: TESTNET ONLY");

    let signer = Signer::from_private_key(&creds.private_key, creds.network)
        .expect("the demo account's PRIVATE_KEY must be a valid secp256k1 key");
    let master = creds.account_address.clone().unwrap_or_else(|| signer.address().to_string());
    let transport = HyperliquidTransport::new(creds.network);
    let instruments = Arc::new(HyperliquidInstruments::load(&transport, false).expect("meta"));
    let inst = instruments.symbology().by_symbol(SYMBOL).expect("BTC is listed").clone();
    let mid: f64 = transport
        .info(&json!({ "type": "allMids" }))
        .expect("allMids")
        .get(&inst.coin)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .expect("a live BTC mid");
    assert!(QTY * mid > 10.5, "{QTY} BTC at {mid} is under the venue's 10 USD minimum");
    let base0 = position(&transport, &master).expect("clearinghouseState");
    assert!(
        base0.abs() < 1e-9,
        "the testnet account must hold no BTC position before this smoke (it holds {base0}): it \
         flattens what it finds at the end and must not touch another test's position"
    );
    println!("START mid={mid} position={base0} qty={QTY}");

    let (events, mut rx) = event_channel(1024);
    let client = HyperliquidExecutionClient::spawn(
        signer,
        master.clone(),
        Arc::clone(&instruments),
        events,
        None,
        None,
    );
    let mut flat = Flat { client, transport: HyperliquidTransport::new(creds.network), master };
    let mut pull: Pull = Box::new(move || rx.try_recv().ok());
    let mut seen: Vec<Event> = Vec::new();
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(4)); // the user-data socket settles

    // --- 1. BUY: a market order the venue fills in its answer ---------------------------------------
    let buy = market(&format!("vtihbuy{}", now_ms() % 100_000_000), 1, QTY, false);
    flat.client.submit(&buy);
    let done = pump_until(&mut pull, &mut seen, started, 25, |s| {
        let w = words(s, &buy.client_order_id);
        w.contains(&"Filled") || w.contains(&"PartiallyFilled") || w.contains(&"Rejected")
    });
    // let the socket's own rows (the bare fill, the order update) land behind the answer's
    pump_until(&mut pull, &mut seen, started, 4, |_| false);
    let held = position(&flat.transport, &flat.master);
    println!("MEASURED position after the buy: {held:?}");
    assert!(
        done,
        "{}: no fill and no refusal in 25 s: {:?}",
        buy.client_order_id,
        words(&seen, &buy.client_order_id)
    );
    check_instant_fill(&buy, &seen);

    // --- 2. SELL: the same, reduce-only, for what the account holds ----------------------------------
    let held = held.expect("clearinghouseState").abs();
    assert!(held > 0.0, "the buy filled, so the account must hold BTC");
    let sell = market(&format!("vtihsel{}", now_ms() % 100_000_000), -1, held, true);
    flat.client.submit(&sell);
    let done = pump_until(&mut pull, &mut seen, started, 25, |s| {
        let w = words(s, &sell.client_order_id);
        w.contains(&"Filled") || w.contains(&"PartiallyFilled") || w.contains(&"Rejected")
    });
    pump_until(&mut pull, &mut seen, started, 4, |_| false);
    let after = position(&flat.transport, &flat.master);
    println!("MEASURED position after the sell: {after:?}");
    assert!(
        done,
        "{}: no fill and no refusal in 25 s: {:?}",
        sell.client_order_id,
        words(&seen, &sell.client_order_id)
    );
    check_instant_fill(&sell, &seen);
    assert!(
        after.is_ok_and(|q| q.abs() < 1e-9),
        "the reduce-only sell must leave the account flat"
    );

    println!(
        "hyperliquid instant-fill smoke green: Submitted -> Accepted -> Filled for the buy and the reduce-only sell"
    );
}
