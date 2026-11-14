//! Decision 0175's failure, reproduced on a fake aster venue (offline, no network). READ-ONLY
//! verification: the `#[ignore = "0175 ..."]` tests are RED on `main` by design and are NOT to be made
//! to pass here; they are the finding. Run them with `--ignored` to see the failure.
//!
//! The question: a cancel (or a confirm) of an order client id that is NOT in the router's
//! order-client-id-to-symbol map goes to the MOUNTED symbol, the venue answers not-found (`-2011` on cancel,
//! `-2013` on a status read), the venue client swallows that as the idempotent "already gone", and
//! `vike_bridge_core::exec_actor::run_loop` turns the `Ok` into a terminal `OrderCanceled` while the
//! order still rests on its own symbol. Aster reuses binance's router, map included
//! (`vike_bridge_core::per_symbol::{VenueRestPerSymbol, OrderClientIdSymbols}`), over its own clients.
//!
//! The pipeline here is the production one minus the sockets: the real REST client
//! (`VenueRestAsterPerp` / `VenueRestAsterSpot`) over a stateful fake venue (a BOOK of live
//! `(symbol, order client id)` pairs), wrapped in the real router, driven by the real `run_loop` with a
//! real `ExecutionClientActorCommand::Cancel`, as `crates/bridges/aster/src/execution/mod.rs`'s `run_perp` / `run_spot` wire it.
//! The assertion is the invariant, so ANY fix satisfies it (the cancel reaches the order's own symbol,
//! or the cancel is refused): the lane may say `OrderCanceled` only when the venue no longer holds the
//! order. Aster stays out of demo RUNS (mainnet reads): this file touches no network.
//!
//! Own test binary (not the grouped `offline` one) because that group takes no `#[ignore]`d test.
#![cfg(feature = "execution")]

use std::sync::Arc;

use serde_json::{Value, json};
use vike_aster::execution::rest::perp::VenueRestAsterPerp;
use vike_aster::execution::rest::spot::VenueRestAsterSpot;
use vike_binance::execution::rest::per_symbol::refused_no_client;
use vike_bridge_core::order_client_id_repro::{self, Book};
use vike_bridge_core::per_symbol::{ClientFactory, OrderClientIdSymbols, VenueRestPerSymbol};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::SymbolProperties;

const BTC: &str = "BTCUSDT"; // the mounted symbol
const ETH: &str = "ETHUSDT"; // a foreign symbol of the same account

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

/// A stateful fake of the order endpoints: a cancel of a live `(symbol, order client id)` removes it; any other
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
    vike_bridge_core::no_public_call!("no public call expected");
}

fn grid() -> SymbolProperties {
    SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() }
}

fn perp(symbol: &str, book: &Book) -> VenueRestAsterPerp<NullSigner, Venue> {
    VenueRestAsterPerp {
        signer: NullSigner,
        transport: Venue(book.clone()),
        base_url: "https://unused.invalid".to_string(),
        symbol: symbol.to_string(),
        properties: grid(),
        leverage: 1.0,
        builder: None,
    }
}

fn spot(symbol: &str, book: &Book) -> VenueRestAsterSpot<NullSigner, Venue> {
    VenueRestAsterSpot {
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
/// factory, ONE order client id map shared with whoever teaches it.
fn router<R: VenueRest + 'static>(
    book: &Book,
    order_client_ids: &Arc<OrderClientIdSymbols>,
    build: fn(&str, &Book) -> R,
) -> VenueRestPerSymbol<R> {
    let factory_book = book.clone();
    let make: ClientFactory<R> =
        Box::new(move |symbol: &str| Ok(build(&symbol.to_uppercase(), &factory_book)));
    VenueRestPerSymbol::new(
        BTC,
        build(BTC, book),
        make,
        Arc::clone(order_client_ids),
        refused_no_client,
    )
}

// ---------------------------------------------------------------------------------------------
// RED on main (ignored): an order client id the router never saw, resting on a foreign symbol.

/// Perp lane. The source of the unseen order client id is irrelevant to the router (an order restored across a
/// restart that no `adopt_restored` reached, an order a reconcile row names, a cleared map): the map
/// has no entry, so the cancel is signed on BTCUSDT and `-2011` is swallowed by `cancel_order`.
#[test]
#[ignore = "0175: a cancel of an order client id absent from the order client id map goes to the MOUNTED symbol, the venue says -2011, the adapter swallows it as 'already gone' and the lane reports OrderCanceled over a resting order"]
fn perp_cancel_of_an_unmapped_foreign_order_client_id_is_not_reported_canceled_while_it_rests() {
    order_client_id_repro::cancel_of_an_unmapped_foreign_order_client_id(
        |b, c| router(b, c, perp),
        ETH,
    );
}

/// Spot lane (its own client; `cancel_order` swallows `-2011` the same way).
#[test]
#[ignore = "0175: same failure on the spot lane: the unmapped order client id is cancelled on the mounted symbol, -2011 is swallowed, OrderCanceled over a resting order"]
fn spot_cancel_of_an_unmapped_foreign_order_client_id_is_not_reported_canceled_while_it_rests() {
    order_client_id_repro::cancel_of_an_unmapped_foreign_order_client_id(
        |b, c| router(b, c, spot),
        ETH,
    );
}

/// The confirm leg, in the one shape that can hurt: the order client id IS in the dispatch ledger (the exec thread
/// finished its submit) but the map has lost it. The mounted symbol says "absent" and, because the
/// ledger vouches for the dispatch, `resolve_confirm` rejects: a terminal `OrderRejected` for a live
/// order. NOT reachable on main (nothing ever removes a map entry, and a submit records the order client id in the
/// map and stamps the ledger together); it is what a cleared or evicted map would cost.
#[test]
#[ignore = "0175: reachable only if the order client id map ever forgets a dispatched order client id (no path does on main): the mounted symbol reports it absent and the ledger turns that into a terminal OrderRejected"]
fn confirm_of_a_dispatched_order_client_id_the_map_forgot_does_not_reject_a_live_order() {
    order_client_id_repro::confirm_of_a_dispatched_order_client_id_the_map_forgot(
        |b, c| router(b, c, perp),
        ETH,
    );
}

// ---------------------------------------------------------------------------------------------
// GREEN on main (not ignored): the controls that make the red ones mean something.

/// The harness can pass: once the router has been TAUGHT the symbol (what
/// the exec client's `adopt_restored` hook does, `OrderClientIdSymbols::record`), the same cancel reaches the
/// order's own symbol, the venue takes it, and `OrderCanceled` is true.
#[test]
fn control_a_taught_foreign_order_client_id_is_cancelled_on_its_own_symbol() {
    order_client_id_repro::a_taught_foreign_order_client_id_is_cancelled_on_its_own_symbol(
        |b, c| router(b, c, perp),
        ETH,
    );
}

/// The fallback is right for what it was built for: an unmapped order client id that rests on the MOUNTED symbol.
#[test]
fn control_an_unmapped_order_client_id_on_the_mounted_symbol_is_cancelled() {
    order_client_id_repro::an_unmapped_order_client_id_on_the_mounted_symbol_is_cancelled(
        |b, c| router(b, c, perp),
        BTC,
    );
}

/// The confirm of an unmapped order client id whose submit this process never dispatched (a restored or adopted
/// order) is SILENT: no ledger entry, so "absent" proves nothing and `resolve_confirm` says nothing.
/// A live order is not rejected. (A fix that routes it to ETHUSDT would answer `OrderAccepted`: also
/// fine, so the assertion names only the two wrong words.)
#[test]
fn control_a_confirm_of_an_unmapped_never_dispatched_order_client_id_rejects_nothing() {
    order_client_id_repro::a_confirm_of_an_unmapped_never_dispatched_order_client_id_rejects_nothing(
        |b, c| router(b, c, perp),
        ETH,
    );
}
