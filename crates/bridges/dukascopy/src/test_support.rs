//! The crate's own test helpers (`test-support`, never in a normal build; the `vike-ctrader` idiom): the
//! sidecar-side pieces the price-lane tests share — this crate's `quotes`, `quotes_rate` and
//! `home_conversion` tests and the gatewayhub's (`crates/vike-gatewayhub/src/data/dukascopy_quotes_tests.rs`,
//! `crates/vike-gatewayhub/src/trade/adapters/dukascopy_rate_tests.rs`), which reach them through a dev edge
//! with `test-support` — and the market order the protocol unit tests encode and the `sidecar` suites
//! submit. Unit tests reach it as `crate::test_support`, the `tests/` binaries through the self
//! dev-dependency.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vike_model::OrderRequest;

use crate::quotes::{LinePort, QuoteBus};

/// A sidecar's command pipe that records what was written and can be closed.
#[derive(Default)]
pub struct Wire {
    /// Every line written while open, in order.
    pub sent: Mutex<Vec<String>>,
    /// Once set, a write is refused (`send` answers `false`) and nothing is recorded.
    pub closed: AtomicBool,
}

impl Wire {
    /// The lines written so far.
    pub fn lines(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

impl LinePort for Wire {
    fn send(&self, line: &str) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        self.sent.lock().unwrap().push(line.to_string());
        true
    }
}

/// A bus with one sidecar attached over a fresh [`Wire`], its jar announcing `features`, the account's
/// currency as the sidecar stated it (`None`: a jar from before the field).
pub fn attached(features: &[&str], currency: Option<&str>) -> (Arc<QuoteBus>, Arc<Wire>) {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    let features: Vec<String> = features.iter().map(|f| (*f).to_string()).collect();
    let id = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &features);
    bus.set_account_currency(id, currency);
    (bus, wire)
}

/// A market BUY of 1000 EURUSD. Every field is spelled, so a field `OrderRequest` gains is a decision
/// here rather than a silent default.
pub fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Market,
        price: None,
        trigger_price: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts: 1,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}
