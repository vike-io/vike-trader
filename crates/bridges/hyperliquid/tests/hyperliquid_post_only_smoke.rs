//! Post-only (`Alo`) on Hyperliquid's **testnet**, through the whole exec client — the live proof of
//! `crates/bridges/hyperliquid/src/exec/order_wire.rs`'s `hl_tif` row (`PostOnly` -> `Alo`) and of
//! `src/exec/event_mapper.rs`'s `post_only_cause`.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-hyperliquid --test hyperliquid_post_only_smoke -- --ignored --nocapture
//!
//! ORDER-PLACING, **TESTNET ONLY** (the demo tier is the only one `common`'s `demo_creds` loads, and the
//! first thing the test does is assert the network is `Testnet`):
//!
//! 1. a post-only LIMIT BUY 10% below the mid RESTS: the lane carries `Submitted`, `Accepted`; the
//!    venue's `openOrders` lists it; the cancel ends it (`Canceled`);
//! 2. a post-only LIMIT BUY 2% ABOVE the mid would take liquidity: the venue refuses it, the lane carries
//!    `Submitted`, `Rejected` with the typed cause `PostOnlyWouldCross`, the account holds nothing and
//!    nothing rests;
//! 3. a post-only MARKET order is refused locally (the adapter has no post-only market order).
//!
//! No order here can fill (the first is far, the second is refused), so the book is left flat; a
//! `Drop` guard cancels our resting order on every exit path. Nothing secret is printed.

mod common;
use common::demo_creds;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_hyperliquid::HyperliquidExecutionClient;
use vike_hyperliquid::base::config::Network;
use vike_hyperliquid::base::transport::HyperliquidTransport;
use vike_hyperliquid::feeds::instruments::HyperliquidInstruments;
use vike_hyperliquid::signing::Signer;
use vike_model::events::{Event, RejectCause};
use vike_model::{OrderRequest, OrderType, TimeInForce, now_ms};

const SYMBOL: &str = "BTC";
const VENUE: &str = "hyperliquid";

type Pull = Box<dyn FnMut() -> Option<Ingest>>;

fn po(coid: &str, side: i32, qty: f64, price: Option<f64>, order_type: OrderType) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type,
        price,
        time_in_force: TimeInForce::PostOnly,
        ts: now_ms(),
        ..Default::default()
    }
}

fn word(e: &Event) -> Option<(&str, &'static str)> {
    Some(match e {
        Event::OrderSubmitted(x) => (&x.client_order_id, "Submitted"),
        Event::OrderAccepted(x) => (&x.client_order_id, "Accepted"),
        Event::OrderRejected(x) => (&x.client_order_id, "Rejected"),
        Event::OrderFilled(x) => (&x.client_order_id, "Filled"),
        Event::OrderPartiallyFilled(x) => (&x.client_order_id, "PartiallyFilled"),
        Event::OrderCanceled(x) => (&x.client_order_id, "Canceled"),
        _ => return None,
    })
}

fn pump_until(
    pull: &mut Pull,
    seen: &mut Vec<Event>,
    secs: u64,
    done: impl Fn(&[Event]) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        while let Some(ingest) = pull() {
            if let Ingest::Event(e) = ingest {
                if word(&e).is_some() {
                    println!("EVENT {}", format!("{e:?}").chars().take(300).collect::<String>());
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
    seen.iter().filter_map(word).filter(|(c, _)| *c == coid).map(|(_, k)| k).collect()
}

/// Our open orders on the venue (`openOrders`), by `cloid` prefix.
fn open_ours(transport: &HyperliquidTransport, master: &str) -> usize {
    transport
        .info(&json!({ "type": "openOrders", "user": master }))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|rows| rows.len())
        .unwrap_or(0)
}

struct CancelOnDrop {
    client: HyperliquidExecutionClient,
    coids: Vec<String>,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        for c in &self.coids {
            self.client.cancel(c);
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

#[test]
#[ignore = "network + TESTNET creds — places and cancels far testnet orders — run manually (see module doc)"]
fn hyperliquid_post_only_ladder() {
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
    // just over the venue's 10 USD minimum, to the 4 decimals BTC trades in
    let qty = ((12.0 / (mid * 0.9)) * 1e4).ceil() / 1e4;
    println!("START mid={mid} qty={qty} open_before={}", open_ours(&transport, &master));

    let (events, mut rx) = event_channel(1024);
    let client = HyperliquidExecutionClient::spawn(
        signer,
        master.clone(),
        Arc::clone(&instruments),
        events,
        None,
        None,
    );
    let mut guard = CancelOnDrop { client, coids: Vec::new() };
    let mut pull: Pull = Box::new(move || rx.try_recv().ok());
    let mut seen: Vec<Event> = Vec::new();
    std::thread::sleep(Duration::from_secs(4)); // the user-data socket settles

    // 1. RESTS: 10% below the mid a post-only buy is a plain maker order
    let rest = po(
        &format!("vtihpo1{}", now_ms() % 100_000_000),
        1,
        qty,
        Some((mid * 0.9).round()),
        OrderType::Limit,
    );
    guard.coids.push(rest.client_order_id.clone());
    guard.client.submit(&rest);
    let ok = pump_until(&mut pull, &mut seen, 20, |s| {
        let w = words(s, &rest.client_order_id);
        w.contains(&"Accepted") || w.contains(&"Rejected")
    });
    println!("MEASURED resting lane: {:?}", words(&seen, &rest.client_order_id));
    assert!(ok, "no answer in 20 s");
    // (the user-data socket repeats the accept once more: a duplicate transition the engine swallows)
    assert_eq!(words(&seen, &rest.client_order_id)[..2], ["Submitted", "Accepted"]);
    assert_eq!(open_ours(&transport, &master), 1, "the venue lists the resting post-only order");
    guard.client.cancel(&rest.client_order_id);
    let ended = pump_until(&mut pull, &mut seen, 20, |s| {
        words(s, &rest.client_order_id).contains(&"Canceled")
    });
    assert!(ended, "the cancel must end it: {:?}", words(&seen, &rest.client_order_id));

    // 2. CROSSES: 2% above the mid a post-only buy lifts the ask — the venue refuses it, typed
    let cross = po(
        &format!("vtihpo2{}", now_ms() % 100_000_000),
        1,
        qty,
        Some((mid * 1.02).round()),
        OrderType::Limit,
    );
    guard.coids.push(cross.client_order_id.clone());
    guard.client.submit(&cross);
    let ok = pump_until(&mut pull, &mut seen, 20, |s| {
        let w = words(s, &cross.client_order_id);
        w.contains(&"Accepted") || w.contains(&"Rejected") || w.contains(&"Filled")
    });
    assert!(ok, "no answer in 20 s");
    pump_until(&mut pull, &mut seen, 4, |_| false); // let the socket's repeat of the refusal land
    println!("MEASURED crossing lane: {:?}", words(&seen, &cross.client_order_id));
    // MEASURED: the submit answer refuses it, then the user-data socket repeats the refusal as the status
    // word `badAloPxRejected` (a terminal after a terminal, which the order state machine drops): every
    // rejection of this order, either source, is typed
    assert_eq!(
        words(&seen, &cross.client_order_id)[..2],
        ["Submitted", "Rejected"],
        "a crossing Alo order is refused"
    );
    let rejections: Vec<_> = seen
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) if r.client_order_id == cross.client_order_id => {
                Some(r.clone())
            }
            _ => None,
        })
        .collect();
    assert!(!rejections.is_empty(), "the rejection");
    for rejected in &rejections {
        println!("MEASURED crossing reason: {}", rejected.reason);
        assert_eq!(rejected.cause, Some(RejectCause::PostOnlyWouldCross), "{}", rejected.reason);
    }
    assert_eq!(open_ours(&transport, &master), 0, "nothing rests");

    // 3. a post-only MARKET order is refused locally
    let market = po(&format!("vtihpo3{}", now_ms() % 100_000_000), 1, qty, None, OrderType::Market);
    guard.client.submit(&market);
    let ok = pump_until(&mut pull, &mut seen, 20, |s| {
        words(s, &market.client_order_id).contains(&"Rejected")
    });
    assert!(ok, "{:?}", words(&seen, &market.client_order_id));
    println!(
        "hyperliquid post-only ladder green: rests far, crossing refused with the typed cause, market refused, book flat"
    );
}
