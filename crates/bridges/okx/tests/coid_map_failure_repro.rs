//! Decision 0175's failure, reproduced on a fake okx venue (offline, no network). READ-ONLY
//! verification: the `#[ignore = "0175 ..."]` tests are RED on `main` by design and are NOT to be made
//! to pass here; they are the finding. Run them with `--ignored` to see the failure.
//!
//! The question: a cancel (or a confirm) of a client order id that is NOT in the router's
//! coid-to-instrument map (`vike_okx::per_symbol::CoidSymbols`) goes to the MOUNTED instrument, the
//! venue answers not-found (`51400`/`51401`/`51402` on `cancel-order`), `OkxPerpRest::cancel_order`
//! retries once on `cancel-algos` (which answers "unknown" too) and, `order-algo` having no such stop
//! either, reads that as the idempotent "already gone"; `vike_bridge_core::exec_actor::run_loop` turns
//! the `Ok` into a terminal `OrderCanceled` while the order still rests on its own instrument.
//!
//! The pipeline here is the production one minus the sockets: the real `OkxPerpRest` over a stateful
//! fake venue (a BOOK of live `(instId, clOrdId)` pairs), wrapped in the real `PerSymbolRest` router,
//! driven by the real `run_loop` with a real `ExecCommand::Cancel`, as
//! `crates/bridges/okx/src/exec.rs`'s `run` wires it. The assertion is the invariant, so ANY fix
//! satisfies it (the cancel reaches the order's own instrument, or the cancel is refused): the lane may
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
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_exec::{CancelIntent, Ingest, event_channel};
use vike_model::SymbolProperties;
use vike_model::events::Event;
use vike_okx::per_symbol::{ClientFactory, CoidSymbols, PerSymbolRest};
use vike_okx::perp::OkxPerpRest;
use vike_okx::transport::OkxTransport;
use vike_secrets::credentials::Credentials;

const BTC: &str = "BTC-USDT-SWAP"; // the mounted instrument
const ETH: &str = "ETH-USDT-SWAP"; // a foreign instrument of the same account

/// The venue's book: the `(instId, clOrdId)` pairs that are LIVE (regular book; the algo book is empty).
#[derive(Clone, Default)]
struct Book(Arc<Mutex<HashSet<(String, String)>>>);

impl Book {
    fn rest(&self, inst: &str, coid: &str) {
        self.0.lock().unwrap().insert((inst.to_string(), coid.to_string()));
    }
    fn is_live(&self, inst: &str, coid: &str) -> bool {
        self.0.lock().unwrap().contains(&(inst.to_string(), coid.to_string()))
    }
    fn take(&self, inst: &str, coid: &str) -> bool {
        self.0.lock().unwrap().remove(&(inst.to_string(), coid.to_string()))
    }
}

/// A stateful fake of the order endpoints: a `cancel-order` of a live `(instId, clOrdId)` removes it;
/// any other answers the per-row `51400` (the rows `unwrap_okx` reads), as the real venue does for the
/// wrong instrument; `cancel-algos` and `order-algo` know no stop; the `order` status read lists a live
/// pair and answers `51603` for any other.
struct Venue(Book);

fn text(params: &[(&str, Value)], key: &str) -> String {
    params
        .iter()
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn failed_row(s_code: &str, s_msg: &str) -> Value {
    json!({ "code": "1", "msg": "All operations failed", "data": [{ "sCode": s_code, "sMsg": s_msg }] })
}

impl OkxTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        match (method, path) {
            ("POST", "/api/v5/trade/cancel-order") => {
                if self.0.take(&text(params, "instId"), &text(params, "clOrdId")) {
                    Ok(json!({ "code": "0", "msg": "", "data": [{ "ordId": "1", "sCode": "0" }] }))
                } else {
                    Ok(failed_row("51400", "Cancellation failed as the order does not exist"))
                }
            }
            ("GET", "/api/v5/trade/order") => {
                if self.0.is_live(&text(params, "instId"), &text(params, "clOrdId")) {
                    Ok(
                        json!({ "code": "0", "msg": "", "data": [{ "ordId": "1", "state": "live" }] }),
                    )
                } else {
                    Ok(json!({ "code": "51603", "msg": "Order does not exist", "data": [] }))
                }
            }
            ("GET", "/api/v5/trade/order-algo") => {
                Ok(json!({ "code": "51603", "msg": "Order does not exist", "data": [] }))
            }
            other => panic!("unexpected venue call {other:?}"),
        }
    }
    fn signed_json(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _body: &str,
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        match path {
            "/api/v5/trade/cancel-algos" => {
                Ok(failed_row("51400", "Cancellation failed as the order does not exist"))
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

fn client(inst: &str, book: &Book) -> OkxPerpRest<Venue> {
    OkxPerpRest {
        signer: OkxV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            || 0,
        ),
        transport: Venue(book.clone()),
        base_url: "https://unused.invalid".to_string(),
        symbol: inst.to_string(),
        properties: SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() },
        ct_val: 0.1,
        leverage: 1.0,
        broker_code: None,
    }
}

/// The router `run` builds: mounted on BTC-USDT-SWAP, any other instrument through the factory, ONE
/// coid map shared with whoever teaches it.
fn router(book: &Book, coids: &Arc<CoidSymbols>) -> PerSymbolRest<OkxPerpRest<Venue>> {
    let factory_book = book.clone();
    let make: ClientFactory<OkxPerpRest<Venue>> =
        Box::new(move |inst: &str| Ok(client(&inst.to_uppercase(), &factory_book)));
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
fn assert_no_live_order_reported_canceled(events: &[Event], book: &Book, inst: &str, coid: &str) {
    assert!(
        !(said_canceled(events, coid) && book.is_live(inst, coid)),
        "{coid} still rests on {inst} at the venue, yet the lane said OrderCanceled: {events:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// RED on main (ignored): a coid the router never saw, resting on a foreign instrument.

/// The source of the unseen coid is irrelevant to the router (an order restored across a restart that
/// no `adopt_restored` reached, an order a reconcile row names, a cleared map): the map has no entry,
/// so the cancel is signed on BTC-USDT-SWAP, `51400`, then the `cancel-algos` retry's own "unknown"
/// and an empty `order-algo`: `cancel_algo` reads "already gone".
#[test]
#[ignore = "0175: a cancel of a coid absent from the coid map goes to the MOUNTED instrument, the venue says 51400, the cancel-algos retry and order-algo both say unknown, the adapter swallows it as 'already gone' and the lane reports OrderCanceled over a resting order"]
fn cancel_of_an_unmapped_foreign_coid_is_not_reported_canceled_while_it_rests() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let router = router(&book, &Arc::default());
    let events = drive_cancel(&router, "c-eth");
    assert_no_live_order_reported_canceled(&events, &book, ETH, "c-eth");
}

/// The confirm leg, in the one shape that can hurt: the coid IS in the dispatch ledger (the exec thread
/// finished its submit) but the map has lost it. The mounted instrument says "absent" (regular book AND
/// algo book) and, because the ledger vouches for the dispatch, `resolve_confirm_by_id` rejects: a
/// terminal `OrderRejected` for a live order. NOT reachable on main (nothing ever removes a map entry,
/// and a submit records the coid in the map and stamps the ledger together); it is what a cleared or
/// evicted map would cost.
#[test]
#[ignore = "0175: reachable only if the coid map ever forgets a dispatched coid (no path does on main): the mounted instrument reports it absent and the ledger turns that into a terminal OrderRejected"]
fn confirm_of_a_dispatched_coid_the_map_forgot_does_not_reject_a_live_order() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let ledger = Arc::new(DispatchLedger::default());
    ledger.mark("c-eth");
    let confirm = confirm_closure(router(&book, &Arc::default()), ledger, |_| true);
    let events = confirm("c-eth");
    assert!(
        !events.iter().any(|e| matches!(e, Event::OrderRejected(_))),
        "a live order on {ETH} was rejected by a confirm sent to the mounted instrument: {events:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// GREEN on main (not ignored): the controls that make the red ones mean something.

/// The harness can pass: once the router has been TAUGHT the instrument (what
/// `OkxExecutionClient::adopt_restored` does, `CoidSymbols::record`; okx HAS that override on main),
/// the same cancel reaches the order's own instrument, the venue takes it, and `OrderCanceled` is true.
#[test]
fn control_a_taught_foreign_coid_is_cancelled_on_its_own_instrument() {
    let book = Book::default();
    book.rest(ETH, "c-eth");
    let coids = Arc::new(CoidSymbols::default());
    coids.record("c-eth", ETH);
    let router = router(&book, &coids);
    let events = drive_cancel(&router, "c-eth");
    assert!(said_canceled(&events, "c-eth"), "{events:?}");
    assert!(!book.is_live(ETH, "c-eth"), "the venue took the cancel on ETH-USDT-SWAP");
}

/// The fallback is right for what it was built for: an unmapped coid that rests on the MOUNTED
/// instrument.
#[test]
fn control_an_unmapped_coid_on_the_mounted_instrument_is_cancelled() {
    let book = Book::default();
    book.rest(BTC, "c-btc");
    let router = router(&book, &Arc::default());
    let events = drive_cancel(&router, "c-btc");
    assert!(said_canceled(&events, "c-btc"), "{events:?}");
    assert!(!book.is_live(BTC, "c-btc"));
}

/// The confirm of an unmapped coid whose submit this process never dispatched (a restored or adopted
/// order) is SILENT: no ledger entry, so "absent" proves nothing and `resolve_confirm` says nothing.
/// A live order is not rejected. (A fix that routes it to ETH-USDT-SWAP would answer `OrderAccepted`:
/// also fine, so the assertion names only the two wrong words.)
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

/// "A map that was cleared or evicted" is not reproducible on main: the cap decision 0175 reads off the
/// okx branch is gone here, `CoidSymbols` has no removal, so a recorded foreign order is never
/// forgotten to make room.
#[test]
fn control_a_recorded_foreign_coid_is_never_forgotten() {
    let coids = CoidSymbols::default();
    for n in 0..10_000 {
        coids.record(&format!("c{n}"), ETH);
    }
    assert_eq!(coids.symbol_of("c0").as_deref(), Some(ETH));
    assert_eq!(coids.symbol_of("c9999").as_deref(), Some(ETH));
}
