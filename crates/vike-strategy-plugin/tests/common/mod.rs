//! The one `Broker` + `HftBroker` test double both integration binaries of this crate share.

use vike_model::{Bar, Broker, HftBroker};

/// A broker a plugin's `guest::HostBroker` can really call back into, recording every submit.
///
/// ⚠ **Required, not cosmetic: a REAL broker behind the host's REAL vtable.**
/// `guest::HostBroker::new` dereferences `BrokerRef::vtable` on entry, so the null-vtable
/// `BrokerRef` the hand-written `panicking` fixture is driven with in `load_refusals.rs` would be
/// undefined behaviour against a template-built plugin. `submits` exists so a dispatch can be
/// PROVEN to have reached user code: the first bar of `load_refusals.rs`'s template panic-boundary
/// test, and every row of `equivalence.rs`'s live-only witness.
///
/// ⚠ **Not a `SimBroker`, and it cannot be one.** `SimBroker::idx` resolves a symbol by position
/// in the mounted universe and PANICS on anything else, and the six live-only hooks describe
/// themselves through fabricated symbols (`"feed:stale"`, `"ord:<coid>:…"`). More importantly a
/// `SimBroker` would add nothing: no engine calls those hooks, so there is no engine behaviour to
/// compare — the question is only whether the payload arrived intact.
///
/// Every read answers a neutral constant: no strategy these binaries drive through it reads one
/// (the live-only hooks and both template probes only submit). The methods mirror
/// `src/host_tests.rs`'s own `FakeBroker`, which a `#[cfg(test)]` module of the library keeps for
/// itself because it cannot see `tests/`.
#[derive(Default)]
pub(super) struct RecordingBroker {
    pub(super) submits: Vec<(String, i32, f64)>,
}

impl Broker for RecordingBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.submits.push((symbol.to_string(), side, qty));
    }
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
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
}

impl HftBroker for RecordingBroker {
    fn position(&self) -> f64 {
        0.0
    }
    fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {}
    fn modify_tagged(&mut self, _tag: &str, _new_qty: Option<f64>, _new_price: Option<f64>) {}
    fn cancel_tagged(&mut self, _tag: &str) {}
}
