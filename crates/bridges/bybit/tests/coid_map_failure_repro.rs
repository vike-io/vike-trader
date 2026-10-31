//! Decision 0175's failure, reproduced on a fake bybit venue (offline, no network). READ-ONLY
//! verification: the `#[ignore = "0175 ..."]` tests are RED on `main` by design and are NOT to be made
//! to pass here; they are the finding. Run them with `--ignored` to see the failure.
//!
//! The question: a cancel (or a confirm) of a client order id that is NOT in the router's
//! coid-to-symbol map (`vike_bybit::per_symbol::CoidSymbols`) goes to the MOUNTED symbol, the venue
//! answers not-found (`110001`; `170213` on the other product), `BybitPerpRest::cancel_order` swallows
//! that as the idempotent "already gone", and `vike_bridge_core::exec_actor::run_loop` turns the `Ok`
//! into a terminal `OrderCanceled` while the order still rests on its own symbol.
//!
//! The pipeline here is the production one minus the sockets: the real `BybitPerpRest` over a
//! stateful fake venue (a BOOK of live `(symbol, orderLinkId)` pairs), wrapped in the real
//! `PerSymbolRest` router, driven by the real `run_loop` with a real `ExecCommand::Cancel`, as
//! `crates/bridges/bybit/src/exec.rs`'s `run` wires it. The assertion is the invariant, so ANY fix
//! satisfies it (the cancel reaches the order's own symbol, or the cancel is refused): the lane may
//! say `OrderCanceled` only when the venue no longer holds the order.
//!
//! Own test binary (not the grouped `offline` one) because that group takes no `#[ignore]`d test.
#![cfg(feature = "exec")]

use std::collections::HashSet;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use vike_bridge_core::confirm::{DispatchLedger, confirm_closure};
use vike_bridge_core::exec_actor::{ExecCommand, run_loop};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::per_symbol::{ClientFactory, CoidSymbols, PerSymbolRest};
use vike_bybit::perp::BybitPerpRest;
use vike_bybit::transport::BybitTransport;
use vike_exec::{CancelIntent, Ingest, event_channel};
use vike_model::SymbolProperties;
use vike_model::events::Event;
use vike_secrets::credentials::Credentials;

const BTC: &str = "BTCUSDT"; // the mounted symbol
const ETH: &str = "ETHUSDT"; // a foreign symbol of the same account

/// The venue's book: the `(symbol, orderLinkId)` pairs that are LIVE.
#[derive(Clone, Default)]
struct Book(Arc<Mutex<HashSet<(String, String)>>>);

impl Book {
    fn rest(&self, symbol: &str, coid: &str) {
        self.0.lock().unwrap().insert((symbol.to_string(), coid.to_string()));
    }
    fn is_live(&self, symbol: &str, coid: &str) -> bool {
        self.0.lock().unwrap().contains(&(symbol.to_string(), coid.to_string()))
    }
    fn take(&self, symbol: &str, coid: &str) -> bool {
        self.0.lock().unwrap().remove(&(symbol.to_string(), coid.to_string()))
    }
}

/// A stateful fake of the order endpoints: a cancel of a live `(symbol, orderLinkId)` removes it; any
/// other cancel answers `110001 order not exists`, as the real venue does for the wrong symbol; the
/// status reads (`realtime`, `history`) list a live pair and are empty for any other.
struct Venue(Book);

impl BybitTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        let get = |key: &str| {
            params
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v.as_str().map(str::to_string))
                .unwrap_or_default()
        };
        let (symbol, coid) = (get("symbol"), get("orderLinkId"));
        match (method, path) {
            ("POST", "/v5/order/cancel") => {
                if self.0.take(&symbol, &coid) {
                    Ok(json!({
                        "retCode": 0, "retMsg": "OK",
                        "result": { "orderId": "oid-1", "orderLinkId": coid }
                    }))
                } else {
                    Ok(json!({
                        "retCode": 110001, "retMsg": "Order does not exist", "result": {}
                    }))
                }
            }
            ("GET", "/v5/order/realtime" | "/v5/order/history") => {
                let list = if self.0.is_live(&symbol, &coid) {
                    json!([{ "orderId": "oid-1", "orderLinkId": coid }])
                } else {
                    json!([])
                };
                Ok(json!({ "retCode": 0, "retMsg": "OK", "result": { "list": list } }))
            }
            other => panic!("unexpected venue call {other:?}"),
        }
    }
}

fn client(symbol: &str, book: &Book) -> BybitPerpRest<Venue> {
    BybitPerpRest {
        signer: BybitV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: None,
            },
            || 0,
        ),
        transport: Venue(book.clone()),
        base_url: "https://unused.invalid".to_string(),
        symbol: symbol.to_string(),
        properties: SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() },
        leverage: 1.0,
        category: vike_bybit::data::Category::Linear,
    }
}

/// The router `run` builds: mounted on BTCUSDT, any other symbol through the factory, ONE coid map
/// shared with whoever teaches it.
fn router(book: &Book, coids: &Arc<CoidSymbols>) -> PerSymbolRest<BybitPerpRest<Venue>> {
    let factory_book = book.clone();
    let make: ClientFactory<BybitPerpRest<Venue>> =
        Box::new(move |symbol: &str| Ok(client(&symbol.to_uppercase(), &factory_book)));
    PerSymbolRest::new(BTC, client(BTC, book), make, Arc::clone(coids))
}

/// One `ExecCommand::Cancel` through the real `run_loop`; every event the lane handed the core.
fn drive_cancel<R: VenueRest>(rest: &R, coid: &str) -> Vec<Event> {
    let (events, mut ingest) = event_channel(64);
    let (commands, rx) = mpsc::channel();
    let cancel = ExecCommand::Cancel {
        client_order_id: coid.to_string(),
        intent: CancelIntent::Unspecified,
    };
    commands.send(cancel).expect("the loop's channel is open");
    commands.send(ExecCommand::Shutdown).expect("the loop's channel is open");
    run_loop(rest, &events, rx, Vec::new, Duration::from_secs(3600));
    let mut seen = Vec::new();
    while let Ok(Ingest::Event(event)) = ingest.try_recv() {
        seen.push(event);
    }
    seen
}

fn said_canceled(events: &[Event], coid: &str) -> bool {
    events.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == coid))
}

/// The invariant decision 0175 is about, fix-agnostic.
fn assert_no_live_order_reported_canceled(events: &[Event], book: &Book, symbol: &str, coid: &str) {
    assert!(
        !(said_canceled(events, coid) && book.is_live(symbol, coid)),
        "{coid} still rests on {symbol} at the venue, yet the lane said OrderCanceled: {events:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// RED on main (ignored): a coid the router never saw, resting on a foreign symbol.

/// The source of the unseen coid is irrelevant to the router (an order restored across a restart that
/// no `adopt_restored` reached, an order a reconcile row names, a cleared map): the map has no entry,
/// so the cancel is signed on BTCUSDT, `110001`, which `cancel_order` reads as "already gone".
#[test]
#[ignore = "0175: a cancel of a coid absent from the coid map goes to the MOUNTED symbol, the venue says 110001, the adapter swallows it as 'already gone' and the lane reports OrderCanceled over a resting order"]
fn cancel_of_an_unmapped_foreign_coid_is_not_reported_canceled_while_it_rests() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let router = router(&book, &Arc::default());
    let events = drive_cancel(&router, "c-eth");
    assert_no_live_order_reported_canceled(&events, &book, ETH, "c-eth");
}

/// The confirm leg, in the one shape that can hurt: the coid IS in the dispatch ledger (the exec thread
/// finished its submit) but the map has lost it. The mounted symbol says "absent" (realtime AND history)
/// and, because the ledger vouches for the dispatch, `resolve_confirm_by_id` rejects: a terminal
/// `OrderRejected` for a live order. NOT reachable on main (nothing ever removes a map entry, and a
/// submit records the coid in the map and stamps the ledger together); it is what a cleared or evicted
/// map would cost.
#[test]
#[ignore = "0175: reachable only if the coid map ever forgets a dispatched coid (no path does on main): the mounted symbol reports it absent and the ledger turns that into a terminal OrderRejected"]
fn confirm_of_a_dispatched_coid_the_map_forgot_does_not_reject_a_live_order() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let ledger = Arc::new(DispatchLedger::default());
    ledger.mark("c-eth");
    let confirm = confirm_closure(router(&book, &Arc::default()), ledger, |_| true);
    let events = confirm("c-eth");
    assert!(
        !events.iter().any(|e| matches!(e, Event::OrderRejected(_))),
        "a live order on {ETH} was rejected by a confirm sent to the mounted symbol: {events:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// GREEN on main (not ignored): the controls that make the red ones mean something.

/// The harness can pass: once the router has been TAUGHT the symbol (what
/// `BybitExecutionClient::adopt_restored` does, `CoidSymbols::record`), the same cancel reaches the
/// order's own symbol, the venue takes it, and `OrderCanceled` is true.
#[test]
fn control_a_taught_foreign_coid_is_cancelled_on_its_own_symbol() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let coids = Arc::new(CoidSymbols::default());
    coids.record("c-eth", ETH);
    let router = router(&book, &coids);
    let events = drive_cancel(&router, "c-eth");
    assert!(said_canceled(&events, "c-eth"), "{events:?}");
    assert!(!book.is_live(ETH, "c-eth"), "the venue took the cancel on ETHUSDT");
}

/// The fallback is right for what it was built for: an unmapped coid that rests on the MOUNTED symbol.
#[test]
fn control_an_unmapped_coid_on_the_mounted_symbol_is_cancelled() {
    let book = Book::default();
    book.rest(BTC, "c-btc");
    let router = router(&book, &Arc::default());
    let events = drive_cancel(&router, "c-btc");
    assert!(said_canceled(&events, "c-btc"), "{events:?}");
    assert!(!book.is_live(BTC, "c-btc"));
}

/// The confirm of an unmapped coid whose submit this process never dispatched (a restored or adopted
/// order) is SILENT: no ledger entry, so "absent" proves nothing and `resolve_confirm` says nothing.
/// A live order is not rejected. (A fix that routes it to ETHUSDT would answer `OrderAccepted`: also
/// fine, so the assertion names only the two wrong words.)
#[test]
fn control_a_confirm_of_an_unmapped_never_dispatched_coid_rejects_nothing() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let confirm = confirm_closure(
        router(&book, &Arc::default()),
        Arc::new(DispatchLedger::default()),
        |_| true,
    );
    let events = confirm("c-eth");
    assert!(
        !events.iter().any(|e| matches!(e, Event::OrderRejected(_) | Event::OrderCanceled(_))),
        "{events:?}"
    );
}

/// "A map that was cleared or evicted" is not reproducible on main: `CoidSymbols` has no removal and no
/// cap (its `record` says so), so a recorded foreign order is never forgotten to make room.
#[test]
fn control_a_recorded_foreign_coid_is_never_forgotten() {
    let coids = CoidSymbols::default();
    for n in 0..10_000 {
        coids.record(&format!("c{n}"), ETH);
    }
    assert_eq!(coids.symbol_of("c0").as_deref(), Some(ETH));
    assert_eq!(coids.symbol_of("c9999").as_deref(), Some(ETH));
}
