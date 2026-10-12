//! Decision 0140's "An order MAY be priced off delayed data", end to end through the real
//! dispatch: a stream's delay label reaches the mounted strategy (`Broker::data_delay`), the order
//! the strategy prices off that stream clears pre-trade EXACTLY as the same order off a real-time
//! stream does, and the order's record (the registry's request, hence the published `OrderView`)
//! carries the label. The label is information, never a gate: the strategy arms and trades on a
//! delayed stream.

use std::sync::{Arc, Mutex};

use super::*;
use vike_exec::testing::RecordingClient;
use vike_model::{DataDelay, QuoteTick};

use crate::runtime::test_support::{core_of, engine_on, mount_of};

const VENUE: &str = "ibkr";
const SYMBOL: &str = "AAPL.SMART.USD";

/// Buys one share at the bid on every quote and remembers the label its broker reported.
struct PriceOffTheQuote {
    seen: Arc<Mutex<Vec<Option<DataDelay>>>>,
}

impl Strategy<LiveBroker> for PriceOffTheQuote {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, q: &QuoteTick) {
        self.seen.lock().unwrap().push(broker.data_delay(""));
        broker.submit_limit("", 1, 1.0, q.bid);
    }
}

/// A core on the ROSTER venue `ibkr` (so the capability preflight really runs: `sim` is never
/// refused), one strategy mounted on `(ibkr, AAPL)`, the stream labelled `label`, then one quote.
fn trade_one_quote_under(
    label: DataDelay,
) -> (CoreThread<RecordingClient>, Vec<Option<DataDelay>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let strategy = Box::new(PriceOffTheQuote { seen: Arc::clone(&seen) });
    let config = CoreConfig {
        strategy: Some(mount_of(VENUE, SYMBOL, "1m", strategy)),
        ..Default::default()
    };
    let mut c = core_of(engine_on(VENUE, SYMBOL, RecordingClient::default()), Vec::new(), config);
    assert_eq!(c.mount_delays, vec![None], "no label is stated before the feed sends one");
    c.dispatch(Ingest::DataDelay(Box::new(DataDelayUpdate {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        stream: "quotes".into(),
        delay: label,
    })));
    c.dispatch(Ingest::Quote(Box::new(QuoteUpdate {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        quote: QuoteTick {
            ts: 1,
            local_ts: 1,
            bid: 190.0,
            ask: 190.1,
            bid_size: 100.0,
            ask_size: 100.0,
            symbol: String::new(),
        },
    })));
    let seen = seen.lock().unwrap().clone();
    (c, seen)
}

/// The one test the record names: the delayed order clears pre-trade exactly as the real-time one,
/// its record carries `Delayed`, and the strategy armed (its hook ran and traded) on the delayed
/// stream.
#[test]
fn a_delayed_priced_order_clears_pre_trade_exactly_as_a_real_time_one_and_records_delayed() {
    let (mut delayed, delayed_seen) = trade_one_quote_under(DataDelay::DELAYED);
    let (real, real_seen) = trade_one_quote_under(DataDelay::RealTime);

    // The strategy ARMED on the delayed stream: its hook ran, saw the label, and submitted.
    assert_eq!(delayed_seen, vec![Some(DataDelay::DELAYED)]);
    assert_eq!(real_seen, vec![Some(DataDelay::RealTime)]);
    assert_eq!(delayed.mount_states, real.mount_states, "arming is the same on both streams");

    // Pre-trade cleared it identically: one live order each, the same status, no refusal and no
    // risk-gate denial on either (a refusal would be a terminal order and a recent-events note).
    let (d, r) = (only_order(&delayed), only_order(&real));
    assert_eq!(d.status, r.status, "same lifecycle off a delayed quote as off a real-time one");
    assert!(d.status.is_live(), "the delayed order is working, not refused: {:?}", d.status);
    assert_eq!(delayed.engine.client.submissions.len(), 1, "it reached the client");
    assert_eq!(real.engine.client.submissions.len(), 1);

    // The record carries the label; with the label (and the minted id) set aside the two requests
    // are the same order.
    assert_eq!(d.request.data_delay, Some(DataDelay::DELAYED));
    assert_eq!(r.request.data_delay, Some(DataDelay::RealTime));
    let strip = |o: &vike_model::ManagedOrder| vike_model::OrderRequest {
        client_order_id: String::new(),
        data_delay: None,
        ..o.request.clone()
    };
    assert_eq!(strip(d), strip(r));

    // The pre-trade LINE: one note for the delayed order, naming its coid, symbol and label; none
    // for the real-time one.
    let notes = |c: &CoreThread<RecordingClient>| -> Vec<String> {
        c.recent.iter().filter(|l| l.starts_with("PRE-TRADE ")).map(|l| l.to_string()).collect()
    };
    assert_eq!(
        notes(&delayed),
        vec![format!(
            "PRE-TRADE {} {SYMBOL}: priced off delayed data (recorded with the order; not a gate)",
            d.request.client_order_id
        )]
    );
    assert!(notes(&real).is_empty(), "a real-time order writes no line: {:?}", real.recent);

    // …and the published read model shows it.
    delayed.publish_guarded();
    let snap = delayed.snapshot.load_full();
    assert_eq!(snap.orders.len(), 1);
    assert_eq!(snap.orders[0].data_delay, Some(DataDelay::DELAYED));
}

/// A label change re-files every mount of the pair and the stamp table, and a mount that arrives
/// AFTER the label reads it at once (never "no label" for a stream that has said).
#[test]
fn a_label_change_reaches_the_table_and_a_late_mount_reads_it() {
    let (mut c, _) = trade_one_quote_under(DataDelay::DELAYED);
    assert_eq!(c.delay_of(VENUE, SYMBOL), Some(DataDelay::DELAYED));
    c.note_data_delay(VENUE, SYMBOL, DataDelay::Frozen);
    assert_eq!(c.mount_delays, vec![Some(DataDelay::Frozen)]);
    assert_eq!(c.delay_of(VENUE, SYMBOL), Some(DataDelay::Frozen));
    assert_eq!(c.delay_of(VENUE, "MSFT.SMART.USD"), None, "another symbol stated nothing");
    assert_eq!(c.delay_of("binance", SYMBOL), None);
}

fn only_order(c: &CoreThread<RecordingClient>) -> &vike_model::ManagedOrder {
    let mut it = c.engine.registry.values();
    let o = it.next().expect("the strategy submitted one order");
    assert!(it.next().is_none(), "exactly one order");
    o
}
