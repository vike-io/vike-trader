//! Decision 0175's order-client-id-map reproductions, the venue-neutral half (`test-support` only).
//!
//! The per-symbol bridges (aster, binance, bybit, okx) each reproduce the failure in their own
//! `tests/order_client_id_map_failure_repro.rs` over their own stateful venue fake and router: a cancel or a
//! confirm of an order client id the router's `crate::per_symbol::OrderClientIdSymbols` map never saw goes to the
//! MOUNTED symbol. The book of live orders, the invariant and the five scenarios are the same on every
//! venue, so they are written here once; each venue keeps its `#[test]`s (their `#[ignore]` reasons
//! name the venue's own not-found codes), its fake transport and its `router(book, order client ids)`, and hands
//! the router in.
//!
//! Every scenario rests `"c-eth"` on the foreign symbol (or `"c-btc"` on the mounted one), as the
//! venue copies did.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use vike_model::events::Event;

use crate::confirm::{ConfirmByOrderClientId, DispatchLedger, confirm_closure};
use crate::execution_drive::{drive_cancel, said_canceled};
use crate::per_symbol::OrderClientIdSymbols;
use crate::rest::VenueRest;

/// The venue's book: the `(symbol, order client id)` pairs that are LIVE.
#[derive(Clone, Default)]
pub struct Book(Arc<Mutex<HashSet<(String, String)>>>);

impl Book {
    /// `order_client_id` now rests on `symbol`.
    pub fn rest(&self, symbol: &str, order_client_id: &str) {
        self.0.lock().unwrap().insert((symbol.to_string(), order_client_id.to_string()));
    }

    /// Whether `order_client_id` rests on `symbol`.
    pub fn is_live(&self, symbol: &str, order_client_id: &str) -> bool {
        self.0.lock().unwrap().contains(&(symbol.to_string(), order_client_id.to_string()))
    }

    /// Cancel `order_client_id` on `symbol`: whether it was resting there.
    pub fn take(&self, symbol: &str, order_client_id: &str) -> bool {
        self.0.lock().unwrap().remove(&(symbol.to_string(), order_client_id.to_string()))
    }
}

/// The invariant decision 0175 is about, fix-agnostic: the lane may say `OrderCanceled` only when the
/// venue no longer holds the order.
pub fn assert_no_live_order_reported_canceled(
    events: &[Event],
    book: &Book,
    symbol: &str,
    order_client_id: &str,
) {
    assert!(
        !(said_canceled(events, order_client_id) && book.is_live(symbol, order_client_id)),
        "{order_client_id} still rests on {symbol} at the venue, yet the lane said OrderCanceled: {events:?}"
    );
}

/// RED on main: an order client id the router never saw, resting on `foreign`, is cancelled; the lane must not
/// say `OrderCanceled` while it still rests.
pub fn cancel_of_an_unmapped_foreign_order_client_id<R: VenueRest>(
    router: impl Fn(&Book, &Arc<OrderClientIdSymbols>) -> R,
    foreign: &str,
) {
    let book = Book::default();
    book.rest(foreign, "c-eth");
    let router = router(&book, &Arc::default());
    let events = drive_cancel(&router, "c-eth");
    assert_no_live_order_reported_canceled(&events, &book, foreign, "c-eth");
}

/// RED on main: the order client id IS in the dispatch ledger but the map has lost it, so the mounted symbol's
/// "absent" must not become a terminal `OrderRejected` for the order live on `foreign`.
pub fn confirm_of_a_dispatched_order_client_id_the_map_forgot<
    R: ConfirmByOrderClientId + Sync + 'static,
>(
    router: impl Fn(&Book, &Arc<OrderClientIdSymbols>) -> R,
    foreign: &str,
) {
    let book = Book::default();
    book.rest(foreign, "c-eth");
    let ledger = Arc::new(DispatchLedger::default());
    ledger.mark("c-eth");
    let confirm = confirm_closure(router(&book, &Arc::default()), ledger, |_| true);
    let events = confirm("c-eth");
    assert!(
        !events.iter().any(|e| matches!(e, Event::OrderRejected(_))),
        "a live order on {foreign} was rejected by a confirm sent to the mounted symbol: {events:?}"
    );
}

/// The lane's answer to a cancel the venue took: `OrderCanceled`, or nothing at all where the venue only takes the
/// request (`crate::rest::OrderCancelAck::Requested`: its own stream ends the order); never a refusal.
fn cancel_taken(events: &[Event], order_client_id: &str) -> bool {
    said_canceled(events, order_client_id) || events.is_empty()
}

/// Control: once the router is TAUGHT `foreign` (`OrderClientIdSymbols::record`, what `adopt_restored` does),
/// the cancel reaches the order's own symbol, the venue takes it, and the lane says so ([`cancel_taken`]).
pub fn a_taught_foreign_order_client_id_is_cancelled_on_its_own_symbol<R: VenueRest>(
    router: impl Fn(&Book, &Arc<OrderClientIdSymbols>) -> R,
    foreign: &str,
) {
    let book = Book::default();
    book.rest(foreign, "c-eth");
    let order_client_ids = Arc::new(OrderClientIdSymbols::default());
    order_client_ids.record("c-eth", foreign);
    let router = router(&book, &order_client_ids);
    let events = drive_cancel(&router, "c-eth");
    assert!(cancel_taken(&events, "c-eth"), "{events:?}");
    assert!(!book.is_live(foreign, "c-eth"), "the venue took the cancel on {foreign}");
}

/// Control: the fallback is right for what it was built for, an unmapped order client id resting on `mounted`.
pub fn an_unmapped_order_client_id_on_the_mounted_symbol_is_cancelled<R: VenueRest>(
    router: impl Fn(&Book, &Arc<OrderClientIdSymbols>) -> R,
    mounted: &str,
) {
    let book = Book::default();
    book.rest(mounted, "c-btc");
    let router = router(&book, &Arc::default());
    let events = drive_cancel(&router, "c-btc");
    assert!(cancel_taken(&events, "c-btc"), "{events:?}");
    assert!(!book.is_live(mounted, "c-btc"));
}

/// Control: the confirm of an unmapped order client id this process never dispatched is SILENT (no ledger entry,
/// so "absent" proves nothing): neither of the two wrong words for the order live on `foreign`.
pub fn a_confirm_of_an_unmapped_never_dispatched_order_client_id_rejects_nothing<
    R: ConfirmByOrderClientId + Sync + 'static,
>(
    router: impl Fn(&Book, &Arc<OrderClientIdSymbols>) -> R,
    foreign: &str,
) {
    let book = Book::default();
    book.rest(foreign, "c-eth");
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
