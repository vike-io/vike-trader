//! Decision 0140's "An order MAY be priced off delayed data", end to end through the real
//! dispatch: a stream's delay label reaches the mounted strategy (`OrderBroker::data_delay`), the order
//! the strategy prices off that stream clears pre-trade EXACTLY as the same order off a real-time
//! stream does, and the order's record (the registry's request, hence the published `OrderView`)
//! carries the label. The label is information, never a gate: the strategy arms and trades on a
//! delayed stream.

use std::sync::{Arc, Mutex};

use super::*;
use vike_execution::testing::RecordingClient;
use vike_marketdata::TickQuote;
use vike_model::{OrderBroker, OrderPlacement, StreamDelay};

use crate::runtime::test_support::core_of;
use crate::testkit::mount_of;
use vike_execution::testing::engine_on;

const VENUE: &str = "ibkr";
const SYMBOL: &str = "AAPL.SMART.USD";

/// Buys one share at the bid on every quote and remembers the label its broker reported.
struct PriceOffTheQuote {
    seen: Arc<Mutex<Vec<Option<StreamDelay>>>>,
}

impl Strategy<LiveBroker> for PriceOffTheQuote {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, q: &TickQuote) {
        self.seen.lock().unwrap().push(broker.data_delay(""));
        broker.place("", "", 1, 1.0, OrderPlacement::Limit { price: q.bid }, false);
    }
}

/// A core on the ROSTER venue `ibkr` (so the capability preflight really runs: `sim` is never
/// refused), one strategy mounted on `(ibkr, AAPL)`, the stream labelled `label`, then one quote.
fn trade_one_quote_under(
    label: StreamDelay,
) -> (CoreThread<RecordingClient>, Vec<Option<StreamDelay>>) {
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
        quote: TickQuote {
            timestamp: 1,
            local_timestamp: 1,
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
    let (mut delayed, delayed_seen) = trade_one_quote_under(StreamDelay::DELAYED);
    let (real, real_seen) = trade_one_quote_under(StreamDelay::RealTime);

    // The strategy ARMED on the delayed stream: its hook ran, saw the label, and submitted.
    assert_eq!(delayed_seen, vec![Some(StreamDelay::DELAYED)]);
    assert_eq!(real_seen, vec![Some(StreamDelay::RealTime)]);
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
    assert_eq!(d.request.data_delay, Some(StreamDelay::DELAYED));
    assert_eq!(r.request.data_delay, Some(StreamDelay::RealTime));
    let strip = |o: &vike_model::OrderRecord| vike_model::OrderRequest {
        order_client_id: String::new(),
        data_delay: None,
        ..o.request.clone()
    };
    assert_eq!(strip(d), strip(r));

    // The pre-trade LINE: one note for the delayed order, naming its order client id, symbol and label; none
    // for the real-time one.
    let notes = |c: &CoreThread<RecordingClient>| -> Vec<String> {
        c.recent.iter().filter(|l| l.starts_with("PRE-TRADE ")).map(|l| l.to_string()).collect()
    };
    assert_eq!(
        notes(&delayed),
        vec![format!(
            "PRE-TRADE {} {SYMBOL}: priced off delayed data (recorded with the order; not a gate)",
            d.request.order_client_id
        )]
    );
    assert!(notes(&real).is_empty(), "a real-time order writes no line: {:?}", real.recent);

    // …and the published read model shows it.
    delayed.publish_guarded();
    let snap = delayed.snapshot.load_full();
    assert_eq!(snap.orders.len(), 1);
    assert_eq!(snap.orders[0].data_delay, Some(StreamDelay::DELAYED));
}

/// A label change re-files every mount of the pair and the stamp table, and a mount that arrives
/// AFTER the label reads it at once (never "no label" for a stream that has said).
#[test]
fn a_label_change_reaches_the_table_and_a_late_mount_reads_it() {
    let (mut c, _) = trade_one_quote_under(StreamDelay::DELAYED);
    assert_eq!(c.delay_of(VENUE, SYMBOL), Some(StreamDelay::DELAYED));
    c.note_data_delay(VENUE, SYMBOL, StreamDelay::Frozen);
    assert_eq!(c.mount_delays, vec![Some(StreamDelay::Frozen)]);
    assert_eq!(c.delay_of(VENUE, SYMBOL), Some(StreamDelay::Frozen));
    assert_eq!(c.delay_of(VENUE, "MSFT.SMART.USD"), None, "another symbol stated nothing");
    assert_eq!(c.delay_of("binance", SYMBOL), None);
}

/// The labels read on one quote: the own symbol's, then the declared leg's.
type OwnAndLeg = (Option<StreamDelay>, Option<StreamDelay>);

/// Remembers the labels its broker reports for its own symbol and its declared leg on every quote.
struct ReadsTheLegsLabel {
    seen: Arc<Mutex<Vec<OwnAndLeg>>>,
}

const LEG: &str = "MSFT.SMART.USD";

impl Strategy<LiveBroker> for ReadsTheLegsLabel {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &TickQuote) {
        self.seen.lock().unwrap().push((broker.data_delay(""), broker.data_delay(LEG)));
    }
}

/// **A declared LEG reads its own stream's label** (one broker plan, step 12's leftover): it read `None`,
/// because the core carried the label of the mount's own pair alone. The leg's label is the one its
/// `(venue, symbol)` stream reported, filed after the mount was built, and the own symbol keeps its own.
#[test]
fn a_declared_leg_reads_its_own_streams_label() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let strategy = Box::new(ReadsTheLegsLabel { seen: Arc::clone(&seen) });
    let mut mount = mount_of(VENUE, SYMBOL, "1m", strategy);
    mount.symbols = vec![MountLeg::same_venue(LEG)];
    let mut engine = engine_on(VENUE, SYMBOL, RecordingClient::default());
    engine.extra_symbols = vec![LEG.to_string()];
    let config = CoreConfig { strategy: Some(mount), ..Default::default() };
    let mut c = core_of(engine, Vec::new(), config);
    let label = |c: &mut CoreThread<RecordingClient>, symbol: &str, delay| {
        c.dispatch(Ingest::DataDelay(Box::new(DataDelayUpdate {
            venue: VENUE.into(),
            symbol: symbol.into(),
            stream: "quotes".into(),
            delay,
        })));
    };
    label(&mut c, SYMBOL, StreamDelay::RealTime);
    label(&mut c, LEG, StreamDelay::DELAYED);
    c.dispatch(Ingest::Quote(Box::new(QuoteUpdate {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        quote: TickQuote {
            timestamp: 1,
            local_timestamp: 1,
            bid: 190.0,
            ask: 190.1,
            bid_size: 100.0,
            ask_size: 100.0,
            symbol: String::new(),
        },
    })));
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![(Some(StreamDelay::RealTime), Some(StreamDelay::DELAYED))],
        "the own symbol reads its label, the leg its own"
    );
}

fn only_order(c: &CoreThread<RecordingClient>) -> &vike_model::OrderRecord {
    let mut it = c.engine.registry.values();
    let o = it.next().expect("the strategy submitted one order");
    assert!(it.next().is_none(), "exactly one order");
    o
}
