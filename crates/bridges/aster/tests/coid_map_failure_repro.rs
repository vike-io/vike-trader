//! Decision 0175's failure, reproduced on a fake aster venue (offline, no network). READ-ONLY
//! verification: the `#[ignore = "0175 ..."]` tests are RED on `main` by design and are NOT to be made
//! to pass here; they are the finding. Run them with `--ignored` to see the failure.
//!
//! The question: a cancel (or a confirm) of a client order id that is NOT in the router's
//! coid-to-symbol map goes to the MOUNTED symbol, the venue answers not-found (`-2011` on cancel,
//! `-2013` on a status read), the venue client swallows that as the idempotent "already gone", and
//! `vike_bridge_core::exec_actor::run_loop` turns the `Ok` into a terminal `OrderCanceled` while the
//! order still rests on its own symbol. Aster reuses binance's router, map included
//! (`vike_binance::per_symbol::{PerSymbolRest, CoidSymbols}`), over its own clients.
//!
//! The pipeline here is the production one minus the sockets: the real REST client
//! (`AsterPerpRest` / `AsterSpotRest`) over a stateful fake venue (a BOOK of live
//! `(symbol, client order id)` pairs), wrapped in the real router, driven by the real `run_loop` with a
//! real `ExecCommand::Cancel`, as `crates/bridges/aster/src/exec.rs`'s `run_perp` / `run_spot` wire it.
//! The assertion is the invariant, so ANY fix satisfies it (the cancel reaches the order's own symbol,
//! or the cancel is refused): the lane may say `OrderCanceled` only when the venue no longer holds the
//! order. Aster stays out of demo RUNS (mainnet reads): this file touches no network.
//!
//! Own test binary (not the grouped `offline` one) because that group takes no `#[ignore]`d test.
#![cfg(feature = "exec")]

use std::collections::HashSet;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use vike_aster::perp::AsterPerpRest;
use vike_aster::spot::AsterSpotRest;
use vike_binance::per_symbol::{ClientFactory, CoidSymbols, PerSymbolRest};
use vike_bridge_core::confirm::{DispatchLedger, confirm_closure};
use vike_bridge_core::exec_actor::{ExecCommand, run_loop};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_exec::{CancelIntent, Ingest, event_channel};
use vike_model::SymbolProperties;
use vike_model::events::Event;

const BTC: &str = "BTCUSDT"; // the mounted symbol
const ETH: &str = "ETHUSDT"; // a foreign symbol of the same account

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// The venue's book: the `(symbol, client order id)` pairs that are LIVE.
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

/// A stateful fake of the order endpoints: a cancel of a live `(symbol, coid)` removes it; any other
/// cancel answers `-2011 Unknown order sent.`, as the venue does for the wrong symbol; a status read
/// of a pair that is not live answers `-2013`.
struct Venue(Book);

fn api_err(code: i64, msg: &str) -> VenueApiError {
    VenueApiError { code, msg: msg.to_string() }
}

impl RestTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        let get = |key: &str| {
            params.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone()).unwrap_or_default()
        };
        match (method, path) {
            ("DELETE", "/fapi/v3/order" | "/api/v3/order") => {
                if self.0.take(&get("symbol"), &get("origClientOrderId")) {
                    Ok(json!({ "orderId": 1, "status": "CANCELED" }))
                } else {
                    Err(api_err(-2011, "Unknown order sent."))
                }
            }
            ("GET", "/fapi/v3/order" | "/api/v3/order") => {
                if self.0.is_live(&get("symbol"), &get("origClientOrderId")) {
                    Ok(json!({ "orderId": 1, "status": "NEW" }))
                } else {
                    Err(api_err(-2013, "Order does not exist."))
                }
            }
            other => panic!("unexpected venue call {other:?}"),
        }
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("no public call expected ({path})")
    }
}

fn grid() -> SymbolProperties {
    SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() }
}

fn perp(symbol: &str, book: &Book) -> AsterPerpRest<NullSigner, Venue> {
    AsterPerpRest {
        signer: NullSigner,
        transport: Venue(book.clone()),
        base_url: "https://unused.invalid".to_string(),
        symbol: symbol.to_string(),
        properties: grid(),
        leverage: 1.0,
        builder: None,
    }
}

fn spot(symbol: &str, book: &Book) -> AsterSpotRest<NullSigner, Venue> {
    AsterSpotRest {
        signer: NullSigner,
        transport: Venue(book.clone()),
        base_url: "https://unused.invalid".to_string(),
        symbol: symbol.to_string(),
        properties: grid(),
        base_asset: String::new(),
        builder: None,
    }
}

/// The router `run_perp` / `run_spot` build: mounted on BTCUSDT, any other symbol through the
/// factory, ONE coid map shared with whoever teaches it.
fn router<R: VenueRest + 'static>(
    book: &Book,
    coids: &Arc<CoidSymbols>,
    build: fn(&str, &Book) -> R,
) -> PerSymbolRest<R> {
    let factory_book = book.clone();
    let make: ClientFactory<R> =
        Box::new(move |symbol: &str| Ok(build(&symbol.to_uppercase(), &factory_book)));
    PerSymbolRest::new(BTC, build(BTC, book), make, Arc::clone(coids))
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

/// Perp lane. The source of the unseen coid is irrelevant to the router (an order restored across a
/// restart that no `adopt_restored` reached, an order a reconcile row names, a cleared map): the map
/// has no entry, so the cancel is signed on BTCUSDT and `-2011` is swallowed by `cancel_order`.
#[test]
#[ignore = "0175: a cancel of a coid absent from the coid map goes to the MOUNTED symbol, the venue says -2011, the adapter swallows it as 'already gone' and the lane reports OrderCanceled over a resting order"]
fn perp_cancel_of_an_unmapped_foreign_coid_is_not_reported_canceled_while_it_rests() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let router = router(&book, &Arc::default(), perp);
    let events = drive_cancel(&router, "c-eth");
    assert_no_live_order_reported_canceled(&events, &book, ETH, "c-eth");
}

/// Spot lane (its own client; `cancel_order` swallows `-2011` the same way).
#[test]
#[ignore = "0175: same failure on the spot lane: the unmapped coid is cancelled on the mounted symbol, -2011 is swallowed, OrderCanceled over a resting order"]
fn spot_cancel_of_an_unmapped_foreign_coid_is_not_reported_canceled_while_it_rests() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let router = router(&book, &Arc::default(), spot);
    let events = drive_cancel(&router, "c-eth");
    assert_no_live_order_reported_canceled(&events, &book, ETH, "c-eth");
}

/// The confirm leg, in the one shape that can hurt: the coid IS in the dispatch ledger (the exec thread
/// finished its submit) but the map has lost it. The mounted symbol says "absent" and, because the
/// ledger vouches for the dispatch, `resolve_confirm` rejects: a terminal `OrderRejected` for a live
/// order. NOT reachable on main (nothing ever removes a map entry, and a submit records the coid in the
/// map and stamps the ledger together); it is what a cleared or evicted map would cost.
#[test]
#[ignore = "0175: reachable only if the coid map ever forgets a dispatched coid (no path does on main): the mounted symbol reports it absent and the ledger turns that into a terminal OrderRejected"]
fn confirm_of_a_dispatched_coid_the_map_forgot_does_not_reject_a_live_order() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let ledger = Arc::new(DispatchLedger::default());
    ledger.mark("c-eth");
    let confirm = confirm_closure(router(&book, &Arc::default(), perp), ledger, |_| true);
    let events = confirm("c-eth");
    assert!(
        !events.iter().any(|e| matches!(e, Event::OrderRejected(_))),
        "a live order on {ETH} was rejected by a confirm sent to the mounted symbol: {events:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// GREEN on main (not ignored): the controls that make the red ones mean something.

/// The harness can pass: once the router has been TAUGHT the symbol (what
/// `AsterExecutionClient::adopt_restored` does, `CoidSymbols::record`), the same cancel reaches the
/// order's own symbol, the venue takes it, and `OrderCanceled` is true.
#[test]
fn control_a_taught_foreign_coid_is_cancelled_on_its_own_symbol() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let coids = Arc::new(CoidSymbols::default());
    coids.record("c-eth", ETH);
    let router = router(&book, &coids, perp);
    let events = drive_cancel(&router, "c-eth");
    assert!(said_canceled(&events, "c-eth"), "{events:?}");
    assert!(!book.is_live(ETH, "c-eth"), "the venue took the cancel on ETHUSDT");
}

/// The fallback is right for what it was built for: an unmapped coid that rests on the MOUNTED symbol.
#[test]
fn control_an_unmapped_coid_on_the_mounted_symbol_is_cancelled() {
    let book = Book::default();
    book.rest(BTC, "c-btc");
    let router = router(&book, &Arc::default(), perp);
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
        router(&book, &Arc::default(), perp),
        Arc::new(DispatchLedger::default()),
        |_| true,
    );
    let events = confirm("c-eth");
    assert!(
        !events.iter().any(|e| matches!(e, Event::OrderRejected(_) | Event::OrderCanceled(_))),
        "{events:?}"
    );
}
