//! Decision 0175's failure, reproduced on a fake bybit venue (offline, no network). READ-ONLY
//! verification: the `#[ignore = "0175 ..."]` tests are RED on `main` by design and are NOT to be made
//! to pass here; they are the finding. Run them with `--ignored` to see the failure.
//!
//! The question: a cancel (or a confirm) of an order client id that is NOT in the router's
//! order-client-id-to-symbol map (`vike_bridge_core::per_symbol::OrderClientIdSymbols`) goes to the MOUNTED symbol, the venue
//! answers not-found (`110001`; `170213` on the other product), `VenueRestBybit::cancel_order` swallows
//! that as the idempotent "already gone", and `vike_bridge_core::exec_actor::run_loop` turns the `Ok`
//! into a terminal `OrderCanceled` while the order still rests on its own symbol.
//!
//! The pipeline here is the production one minus the sockets: the real `VenueRestBybit` over a
//! stateful fake venue (a BOOK of live `(symbol, orderLinkId)` pairs), wrapped in the real
//! `VenueRestPerSymbolBybit` router, driven by the real `run_loop` with a real `ExecutionClientActorCommand::Cancel`, as
//! `crates/bridges/bybit/src/execution/mod.rs`'s `run` wires it. The assertion is the invariant, so ANY fix
//! satisfies it (the cancel reaches the order's own symbol, or the cancel is refused): the lane may
//! say `OrderCanceled` only when the venue no longer holds the order.
//!
//! Own test binary (not the grouped `offline` one) because that group takes no `#[ignore]`d test.
#![cfg(feature = "execution")]

use std::sync::Arc;

use serde_json::{Value, json};
use vike_bridge_core::order_client_id_repro::{self, Book};
use vike_bridge_core::per_symbol::{ClientFactory, OrderClientIdSymbols};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::rest::VenueRestBybit;
use vike_bybit::execution::rest::per_symbol::VenueRestPerSymbolBybit;
use vike_bybit::signer::BybitV5Signer;
use vike_model::SymbolProperties;
use vike_secrets::credentials::Credentials;

const BTC: &str = "BTCUSDT"; // the mounted symbol
const ETH: &str = "ETHUSDT"; // a foreign symbol of the same account

/// A stateful fake of the order endpoints: a cancel of a live `(symbol, orderLinkId)` removes it; any
/// other cancel answers `110001 order not exists`, as the real venue does for the wrong symbol; the
/// status reads (`realtime`, `history`) list a live pair and are empty for any other.
struct Venue(Book);

impl RestTransportJson for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let get = |key: &str| {
            params
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v.as_str().map(str::to_string))
                .unwrap_or_default()
        };
        let (symbol, order_client_id) = (get("symbol"), get("orderLinkId"));
        match (method, path) {
            ("POST", "/v5/order/cancel") => {
                if self.0.take(&symbol, &order_client_id) {
                    Ok(json!({
                        "retCode": 0, "retMsg": "OK",
                        "result": { "orderId": "oid-1", "orderLinkId": order_client_id }
                    }))
                } else {
                    Ok(json!({
                        "retCode": 110001, "retMsg": "Order does not exist", "result": {}
                    }))
                }
            }
            ("GET", "/v5/order/realtime" | "/v5/order/history") => {
                let list = if self.0.is_live(&symbol, &order_client_id) {
                    json!([{ "orderId": "oid-1", "orderLinkId": order_client_id }])
                } else {
                    json!([])
                };
                Ok(json!({ "retCode": 0, "retMsg": "OK", "result": { "list": list } }))
            }
            other => panic!("unexpected venue call {other:?}"),
        }
    }
}

fn client(symbol: &str, book: &Book) -> VenueRestBybit<Venue> {
    VenueRestBybit {
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
        mainnet: false,
        symbol: symbol.to_string(),
        properties: SymbolProperties { tick_size: 0.01, step_size: 0.01, ..Default::default() },
        leverage: 1.0,
        category: vike_bybit::history::Category::Linear,
    }
}

/// The router `run` builds: mounted on BTCUSDT, any other symbol through the factory, ONE order client id map
/// shared with whoever teaches it.
fn router(
    book: &Book,
    order_client_ids: &Arc<OrderClientIdSymbols>,
) -> VenueRestPerSymbolBybit<VenueRestBybit<Venue>> {
    let factory_book = book.clone();
    let make: ClientFactory<VenueRestBybit<Venue>> =
        Box::new(move |symbol: &str| Ok(client(&symbol.to_uppercase(), &factory_book)));
    VenueRestPerSymbolBybit::new(BTC, client(BTC, book), make, Arc::clone(order_client_ids))
}

// ---------------------------------------------------------------------------------------------
// RED on main (ignored): an order client id the router never saw, resting on a foreign symbol.

/// The source of the unseen order client id is irrelevant to the router (an order restored across a restart that
/// no `adopt_restored` reached, an order a reconcile row names, a cleared map): the map has no entry,
/// so the cancel is signed on BTCUSDT, `110001`, which `cancel_order` reads as "already gone".
#[test]
#[ignore = "0175: a cancel of an order client id absent from the order client id map goes to the MOUNTED symbol, the venue says 110001, the adapter swallows it as 'already gone' and the lane reports OrderCanceled over a resting order"]
fn cancel_of_an_unmapped_foreign_order_client_id_is_not_reported_canceled_while_it_rests() {
    order_client_id_repro::cancel_of_an_unmapped_foreign_order_client_id(router, ETH);
}

/// The confirm leg, in the one shape that can hurt: the order client id IS in the dispatch ledger (the exec thread
/// finished its submit) but the map has lost it. The mounted symbol says "absent" (realtime AND history)
/// and, because the ledger vouches for the dispatch, `resolve_confirm` rejects: a terminal
/// `OrderRejected` for a live order. NOT reachable on main (nothing ever removes a map entry, and a
/// submit records the order client id in the map and stamps the ledger together); it is what a cleared or evicted
/// map would cost.
#[test]
#[ignore = "0175: reachable only if the order client id map ever forgets a dispatched order client id (no path does on main): the mounted symbol reports it absent and the ledger turns that into a terminal OrderRejected"]
fn confirm_of_a_dispatched_order_client_id_the_map_forgot_does_not_reject_a_live_order() {
    order_client_id_repro::confirm_of_a_dispatched_order_client_id_the_map_forgot(router, ETH);
}

// ---------------------------------------------------------------------------------------------
// GREEN on main (not ignored): the controls that make the red ones mean something.

/// The harness can pass: once the router has been TAUGHT the symbol (what
/// the exec client's `adopt_restored` hook does, `OrderClientIdSymbols::record`), the same cancel reaches the
/// order's own symbol, the venue takes it, and `OrderCanceled` is true.
#[test]
fn control_a_taught_foreign_order_client_id_is_cancelled_on_its_own_symbol() {
    order_client_id_repro::a_taught_foreign_order_client_id_is_cancelled_on_its_own_symbol(
        router, ETH,
    );
}

/// The fallback is right for what it was built for: an unmapped order client id that rests on the MOUNTED symbol.
#[test]
fn control_an_unmapped_order_client_id_on_the_mounted_symbol_is_cancelled() {
    order_client_id_repro::an_unmapped_order_client_id_on_the_mounted_symbol_is_cancelled(
        router, BTC,
    );
}

/// The confirm of an unmapped order client id whose submit this process never dispatched (a restored or adopted
/// order) is SILENT: no ledger entry, so "absent" proves nothing and `resolve_confirm` says nothing.
/// A live order is not rejected. (A fix that routes it to ETHUSDT would answer `OrderAccepted`: also
/// fine, so the assertion names only the two wrong words.)
#[test]
fn control_a_confirm_of_an_unmapped_never_dispatched_order_client_id_rejects_nothing() {
    order_client_id_repro::a_confirm_of_an_unmapped_never_dispatched_order_client_id_rejects_nothing(router, ETH);
}
