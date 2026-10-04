use super::*;
use vike_model::{Bar, Broker};

/// A minimal recording `HftBroker` double: tracks every tagged submit/modify/cancel call plus
/// a settable signed position, so the cutoff logic can be asserted call-by-call without a full
/// fill-simulation engine. Not a fill model — `pos` is set directly by the test to simulate
/// "already holding" for the Holding-phase assertions.
#[derive(Default)]
struct MockBroker {
    pos: f64,
    now_ts: i64,
    submitted: Vec<(String, i32, f64, f64)>,
    modified: Vec<(String, Option<f64>, Option<f64>)>,
    cancelled: Vec<String>,
}

impl Broker for MockBroker {
    fn submit_market(&mut self, _symbol: &str, _side: i32, _qty: f64) {}
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
    fn position(&self, _symbol: &str) -> f64 {
        self.pos
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
        self.now_ts
    }
}

impl HftBroker for MockBroker {
    fn position(&self) -> f64 {
        self.pos
    }
    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        self.submitted.push((tag.to_string(), side, qty, price));
    }
    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        self.modified.push((tag.to_string(), new_qty, new_price));
    }
    fn cancel_tagged(&mut self, tag: &str) {
        self.cancelled.push(tag.to_string());
    }
}

fn quote(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 0.0, ask_size: 0.0, symbol: String::new() }
}

fn fill(ts: i64, side: i32, price: f64) -> Fill {
    Fill { side, size: 1.0, price, fee: 0.0, ts, is_maker: true, symbol: String::new() }
}

// ---- defaults-off byte-identical behavior --------------------------------------------------

#[test]
fn default_params_never_suppress_entries() {
    // `from_params` with an empty table must resolve every cutoff knob to 0 — inert.
    let s = TrailingScalper::from_params(&toml::Value::Table(Default::default()));
    assert_eq!(s.entry_open_delay_ms, 0);
    assert_eq!(s.entry_cutoff_before_close_ms, 0);
    assert_eq!(s.market_open_ms, 0);
    assert_eq!(s.market_close_ms, 0);
    assert!(s.entries_allowed(0));
    assert!(s.entries_allowed(i64::MAX / 2));
}

#[test]
fn with_default_gates_the_first_tick_still_places_both_entries() {
    // Byte-identical reproduction of today's behavior: a fresh strategy with the cutoffs off
    // places both entries on the very first tick, exactly like before this feature existed.
    let mut s = TrailingScalper::new(1.0, 0.01, 2000, 0.0);
    let mut b = MockBroker::default();
    Strategy::on_quote_tick(&mut s, &mut b, &quote(0, 0.49, 0.51));
    assert_eq!(b.submitted.len(), 2, "both entries placed with cutoffs off");
    assert!(b.cancelled.is_empty());
}

// ---- entry_open_delay_ms --------------------------------------------------------------------

#[test]
fn entry_open_delay_suppresses_entries_before_the_delay_elapses() {
    let mut s = TrailingScalper::new(1.0, 0.01, 2000, 0.0)
        .with_entry_gates(5_000, 0, /* market_open_ms */ 1_000, 0);
    let mut b = MockBroker::default();
    // ts = 5_999 < open(1_000) + delay(5_000) = 6_000 -> suppressed.
    Strategy::on_quote_tick(&mut s, &mut b, &quote(5_999, 0.49, 0.51));
    assert!(b.submitted.is_empty(), "no entries before market_open + entry_open_delay_ms");
    assert!(b.cancelled.is_empty(), "nothing was resting, so nothing to cancel");
}

#[test]
fn entry_open_delay_allows_entries_once_the_delay_elapses() {
    let mut s = TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(5_000, 0, 1_000, 0);
    let mut b = MockBroker::default();
    // ts = 6_000 == open(1_000) + delay(5_000) -> allowed (boundary is inclusive).
    Strategy::on_quote_tick(&mut s, &mut b, &quote(6_000, 0.49, 0.51));
    assert_eq!(b.submitted.len(), 2, "entries placed once the open delay has elapsed");
}

// ---- entry_cutoff_before_close_ms --------------------------------------------------------

#[test]
fn entry_cutoff_allows_entries_before_the_close_boundary() {
    let mut s = TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
    let mut b = MockBroker::default();
    // ts = 69_999 < close(100_000) - cutoff(30_000) = 70_000 -> allowed.
    Strategy::on_quote_tick(&mut s, &mut b, &quote(69_999, 0.49, 0.51));
    assert_eq!(b.submitted.len(), 2, "entries still allowed just before the cutoff boundary");
}

#[test]
fn entry_cutoff_cancels_resting_entries_at_the_boundary() {
    let mut s = TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
    let mut b = MockBroker::default();
    // First rest both entries well before the cutoff.
    Strategy::on_quote_tick(&mut s, &mut b, &quote(0, 0.49, 0.51));
    assert_eq!(b.submitted.len(), 2);
    // ts = 70_000 == close(100_000) - cutoff(30_000) -> the boundary itself is suppressed,
    // and the resting pair must be CANCELLED (never left dangling on the book).
    Strategy::on_quote_tick(&mut s, &mut b, &quote(70_000, 0.49, 0.51));
    assert_eq!(b.cancelled, vec!["buy".to_string(), "sell".to_string()]);
    // A later tick past the cutoff must NOT re-cancel (idempotent) or re-submit.
    Strategy::on_quote_tick(&mut s, &mut b, &quote(80_000, 0.49, 0.51));
    assert_eq!(b.cancelled.len(), 2, "no repeated cancels on subsequent suppressed ticks");
    assert_eq!(b.submitted.len(), 2, "no re-entry after the cutoff");
}

#[test]
fn entry_cutoff_never_re_prices_entries_past_the_boundary() {
    let mut s = TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
    let mut b = MockBroker::default();
    Strategy::on_quote_tick(&mut s, &mut b, &quote(0, 0.49, 0.51));
    Strategy::on_quote_tick(&mut s, &mut b, &quote(75_000, 0.40, 0.60));
    assert!(b.modified.is_empty(), "past the cutoff, entries are cancelled not re-priced");
}

// ---- EXIT/flatten is unaffected by either cutoff -----------------------------------------

#[test]
fn exit_still_places_and_reprices_past_the_entry_cutoff() {
    // Arm BOTH cutoffs tight, then drive the strategy into Holding via a fill that lands
    // exactly AT the close cutoff boundary — proving the exit path ignores both gates
    // entirely (the module doc's "exit-only tail" contract).
    let mut s = TrailingScalper::new(1.0, 0.01, /* exit_delay_ms */ 2_000, 0.0)
        .with_entry_gates(5_000, 30_000, 1_000, 100_000);
    let mut b = MockBroker::default();
    // A fill at ts = 70_000 (== close - cutoff) opens a long.
    Strategy::on_fill(&mut s, &mut b, &fill(70_000, 1, 0.50));
    b.pos = 1.0; // simulate the resulting long position
    assert!(matches!(s.phase, Phase::Holding { .. }));
    // Before the reaction gap elapses (ts < entry_ts + exit_delay_ms): no exit order yet.
    Strategy::on_quote_tick(&mut s, &mut b, &quote(71_000, 0.49, 0.51));
    assert!(b.submitted.is_empty(), "no exit order until the reaction gap elapses");
    // After the gap (ts = 72_000, well past the entry-cutoff boundary at 70_000): the flatten
    // is placed — proving the entry cutoff never blocks an exit.
    Strategy::on_quote_tick(&mut s, &mut b, &quote(72_000, 0.49, 0.51));
    assert_eq!(b.submitted, vec![("exit".to_string(), -1, 1.0, 0.51)]);
    // A later tick re-prices the SAME exit order (modify, not a fresh submit).
    Strategy::on_quote_tick(&mut s, &mut b, &quote(80_000, 0.55, 0.60));
    assert_eq!(b.modified.len(), 1, "the flatten re-prices via modify, not a second submit");
    assert_eq!(b.submitted.len(), 1, "still exactly one exit submit");
}

#[test]
fn exit_completes_and_resumes_quoting_even_though_entries_stay_cut_off() {
    // Once flat again, the strategy tries to re-quote — and correctly stays suppressed if the
    // tick is still past the close cutoff (no entries resurrected in the exit-only tail).
    let mut s = TrailingScalper::new(1.0, 0.01, 0, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
    let mut b = MockBroker::default();
    Strategy::on_fill(&mut s, &mut b, &fill(80_000, 1, 0.50));
    b.pos = 1.0;
    Strategy::on_quote_tick(&mut s, &mut b, &quote(80_001, 0.49, 0.51)); // places the exit
    assert_eq!(b.submitted, vec![("exit".to_string(), -1, 1.0, 0.51)]);
    // The flatten fills; position goes flat.
    b.pos = 0.0;
    Strategy::on_fill(&mut s, &mut b, &fill(80_002, -1, 0.51));
    assert!(matches!(s.phase, Phase::Quoting));
    // Back in Quoting, but ts = 80_002 is still past the close cutoff (70_000) -> no re-entry.
    Strategy::on_quote_tick(&mut s, &mut b, &quote(80_003, 0.49, 0.51));
    assert_eq!(b.submitted.len(), 1, "still just the one exit submit — no fresh entries");
}
