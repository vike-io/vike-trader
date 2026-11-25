//! The one `OrderBroker` test double both integration binaries of this crate share,
//! compiled ONCE here behind the `test-support` feature (only this crate's self-referential dev edge
//! turns it on) instead of once per binary that declared `tests/common/mod.rs`.

use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderListLeg, OrderPlacement};

/// A broker a strategy file's `guest::HostBroker` can really call back into, recording every
/// market order.
///
/// ⚠ **Required, not cosmetic: a REAL broker behind the host's REAL vtable.**
/// `guest::HostBroker::new` dereferences `BrokerRef::vtable` on entry, so the null-vtable
/// `BrokerRef` the hand-written `panicking` fixture is driven with in `load_refusals.rs` would be
/// undefined behaviour against a template-built strategy file. `submits` exists so a dispatch can be
/// PROVEN to have reached user code: the first bar of `load_refusals.rs`'s template panic-boundary
/// test, and every row of `equivalence.rs`'s live-only witness.
///
/// ⚠ **Not an `OrderBrokerSimulator`, and it cannot be one.** `OrderBrokerSimulator` refuses an order on a symbol its run
/// does not hold, and the six live-only hooks describe themselves through fabricated symbols
/// (`"feed:stale"`, `"ord:<order client id>:…"`). More importantly an `OrderBrokerSimulator` would add nothing: no engine
/// calls those hooks, so there is no engine behaviour to compare — the question is only whether the
/// payload arrived intact.
///
/// Every read but `contract_value` answers a neutral constant: no strategy these binaries drive
/// through it reads one (the live-only hooks and the params and panic probes only place orders; the
/// contract-value probe reads `value` and places it back). The methods mirror
/// `src/host_tests.rs`'s own `FakeBroker`, which a `#[cfg(test)]` module of the library keeps for
/// itself because it is a `#[cfg(test)]` module and this one is the feature's.
#[derive(Default)]
pub struct OrderBrokerRecording {
    /// Every market order, as `(symbol, side, qty)`: an `OrderBroker::place` of an
    /// `OrderPlacement::Market`.
    pub submits: Vec<(String, i32, f64)>,
    /// What [`OrderBroker::contract_value`] answers for every symbol, so a probe can prove the
    /// `ABI_VERSION` 4 slot carries it.
    pub value: Option<vike_model::ContractValue>,
}

/// A market order is recorded in [`OrderBrokerRecording::submits`]; every other verb is inert (the order
/// verbs' round trip is `src/host_tests.rs`'s, the template's is the tactics equivalence run over
/// `OrderBrokerSimulator`).
impl OrderBroker for OrderBrokerRecording {
    fn place(
        &mut self,
        symbol: &str,
        _tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        _: bool,
    ) {
        if order == OrderPlacement::Market {
            self.submits.push((symbol.to_string(), side, qty));
        }
    }
    fn place_group(&mut self, _symbol: &str, _legs: Vec<(String, OrderListLeg)>) {}
    fn amend(
        &mut self,
        _tag: &str,
        _quantity: Option<f64>,
        _price: Option<f64>,
        _trig: Option<f64>,
    ) {
    }
    fn cancel(&mut self, _tag: &str) {}
    fn deny(&mut self, _symbol: &str, _tag: &str, _quantity: f64, _reason: &str) {}
    fn average_price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn position(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        0
    }
    fn contract_value(&self, _symbol: &str) -> Option<vike_model::ContractValue> {
        self.value
    }
}
