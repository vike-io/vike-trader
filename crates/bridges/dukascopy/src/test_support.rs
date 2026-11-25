//! The crate's own test helpers (`test-support`, never in a normal build; the `vike-ctrader` idiom): the
//! sidecar-side pieces the price-lane tests share — this crate's `quotes`, `quotes_rate`, `quotes_stream`,
//! `home_conversion` and `ledger` tests and the gatewayhub's (`crates/vike-gatewayhub/src/data/session_guard_bus_tests.rs`,
//! `crates/vike-gatewayhub/src/trade/adapters/assumed_currency_tests.rs`), which reach them through a dev edge
//! with `test-support` — and the market order the protocol unit tests encode and the `sidecar` suites
//! submit. Unit tests reach it as `crate::test_support`, the `tests/` binaries through the self
//! dev-dependency.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vike_model::OrderRequest;

use crate::feed::{LinePort, QuoteBus};
use crate::instrument::grid::JforexInstrument;

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
/// currency as the sidecar stated it (`None`: a jar from before the field), and the account's instrument list as its
/// login stated it ([`listed`]), which the datahub keeps: the kept list is held too ([`hold_kept_list_for_tests`]).
pub fn attached(features: &[&str], currency: Option<&str>) -> (Arc<QuoteBus>, Arc<Wire>) {
    hold_kept_list_for_tests();
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    let features: Vec<String> = features.iter().map(|f| (*f).to_string()).collect();
    let id = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &features);
    bus.set_account_currency(id, currency);
    bus.set_instrument_list(listed());
    (bus, wire)
}

/// The sidecar's `instruments` line as the fixture records it (`tests/fixtures/instruments_line.json`): the line's own
/// shape, carrying the 180 instruments the JForex SDK predefines with the figures the committed SDK table carried, the
/// commodity CFDs' amounts per contract and EUR/THB's pip as the server stated them at the demo login of 2026-11-13. A
/// TEST input: no list is compiled into the crate (owner 2026-11-13).
pub const INSTRUMENTS_LINE: &str = include_str!("../tests/fixtures/instruments_line.json");

/// The account's instrument list of [`INSTRUMENTS_LINE`], as the reader parses it.
///
/// # Panics
/// The fixture is not an `instruments` line.
pub fn listed() -> Vec<JforexInstrument> {
    match crate::transport::parse_envelope(INSTRUMENTS_LINE) {
        Some(crate::transport::Envelope::Instruments { instruments }) => instruments,
        other => panic!("the fixture is not an instruments line: {other:?}"),
    }
}

/// **The kept list the datahub keeps of [`listed`]**: each row as this bridge's catalog hands it over
/// (`crate::catalog`), so `EURUSD` is `EURUSD`, `XAUUSD` is `XAUUSD` and `AAPL.CFD` is `AAPL.US/USD` on the wire
/// (decision 0290), each row's names spelled as the datahub's write stores them (`vike_catalog::spelled`).
pub fn kept_list_for_tests() -> Vec<vike_model::Instrument> {
    vike_catalog::spelled(listed().iter().map(crate::catalog::instrument_of).collect())
}

/// **Hold [`kept_list_for_tests`] as dukascopy's kept list in this process** (`vike_catalog::hold_kept_list`): what a
/// running gatewayhub reads once the datahub has kept the list a login stated. Process-wide, so every test of one
/// binary that holds it holds THESE rows; a test of "no list" calls the pure `crate::instrument::names` functions with
/// `&[]`, or lives in a binary that holds nothing (`crates/bridges/dukascopy/tests/no_kept_list.rs`).
pub fn hold_kept_list_for_tests() {
    vike_catalog::hold_kept_list("dukascopy", kept_list_for_tests());
}

/// The `subscribe` command line the bus writes to the sidecar for `symbols`.
pub fn subscribe_line(symbols: &[&str]) -> String {
    command_line("subscribe", symbols)
}

/// The `unsubscribe` command line the bus writes to the sidecar for `symbols`.
pub fn unsubscribe_line(symbols: &[&str]) -> String {
    command_line("unsubscribe", symbols)
}

fn command_line(cmd: &str, symbols: &[&str]) -> String {
    format!(
        "{{\"command\":\"{cmd}\",\"symbols\":[{}]}}",
        symbols.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(",")
    )
}

/// A two-sided tick of `symbol` delivered to the bus, stamped now (both its own time and the receipt's).
pub fn tick_now(bus: &QuoteBus, symbol: &str, bid: f64, ask: f64) {
    let now = vike_model::now_ms();
    bus.deliver(symbol, bid, ask, now, now);
}

/// A market BUY of 1000 EURUSD. Every field is spelled, so a field `OrderRequest` gains is a decision
/// here rather than a silent default.
pub fn order(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        display_quantity: None,
        release: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        account: None,
        combo_legs: Vec::new(),
        order_client_id: order_client_id.into(),
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
        timestamp: 1,
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
