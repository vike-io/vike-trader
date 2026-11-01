//! The pump's fold, driven offline through [`Book`]: routing by conId, the per-lane delivery, bars
//! aggregated from prints (and from the mid for a `CASH` pair), the delay label, and the gap
//! bookkeeping. No socket, no REST.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use vike_data::SubscriptionId;
use vike_model::{DataDelay, StreamStatus};
use vike_venues::venue_timings;

use super::{Book, Lane, REDIAL, TICKLE_EVERY};
use crate::agg::BarAggregator;
use crate::data_door::test_support::{Call, RecSink};

fn bars(interval_ms: i64, from_mid: bool) -> Lane {
    Lane::Bars {
        interval: "1m".into(),
        interval_ms,
        agg: BarAggregator::new(interval_ms),
        from_mid,
    }
}

fn book(sink: &Arc<RecSink>) -> Book {
    Book::new(Arc::clone(sink) as Arc<dyn vike_data::LiveDataSink>)
}

/// A frame reaches every stream on ITS conId, each lane takes what it serves, and a frame for a
/// conId nobody holds (or on another topic) is ignored.
#[test]
fn frames_route_by_conid_and_lane() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    assert!(b.add(SubscriptionId(0), 265598, "AAPL.SMART.USD".into(), Lane::Quotes));
    assert!(!b.add(SubscriptionId(1), 265598, "AAPL.SMART.USD".into(), Lane::Trades));

    b.on_frame(&json!({ "topic": "smd+265598", "_updated": 1_000, "6509": "DPB",
                        "84": "190.10", "86": "190.12", "31": "190.11", "7059": "100" }));
    b.on_frame(&json!({ "topic": "smd+999", "84": "1.0", "86": "2.0", "31": "1.5" }));
    b.on_frame(&json!({ "topic": "sor", "args": [] }));

    let calls = sink.take();
    let data: Vec<_> = calls.iter().filter(|c| !matches!(c, Call::Delay(..))).collect();
    assert_eq!(data.len(), 2, "one quote + one print, nothing for the other conId: {calls:?}");
    assert!(matches!(data[0], Call::Quote(s, q) if s == "AAPL.SMART.USD" && q.bid == 190.10));
    assert!(matches!(data[1], Call::Trade(_, t) if t.price == 190.11 && t.size == 100.0));
}

/// A stock bar stream folds PRINTS through the shared aggregator; a bucket boundary closes the
/// prior bar, every print updates the forming one and the bar-close slot.
#[test]
fn bars_aggregate_prints_and_close_on_the_boundary() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(SubscriptionId(0), 1, "AAPL.SMART.USD".into(), bars(60_000, false));
    for (ts, px) in [(60_000, 10.0), (61_000, 12.0), (62_000, 9.0), (120_000, 11.0)] {
        b.on_frame(&json!({ "topic": "smd+1", "_updated": ts, "31": px, "7059": 1 }));
    }
    let calls = sink.take();
    let closed: Vec<_> = calls
        .iter()
        .filter_map(|c| if let Call::Close(_, bar) = c { Some(bar) } else { None })
        .collect();
    assert_eq!(closed.len(), 1, "one boundary crossed: {calls:?}");
    let c = closed[0];
    assert_eq!(
        (c.ts, c.open, c.high, c.low, c.close, c.volume),
        (60_000, 10.0, 12.0, 9.0, 9.0, 3.0)
    );
    let forming = calls.iter().filter(|c| matches!(c, Call::Forming(..))).count();
    assert_eq!(forming, 4, "every print refreshes the forming bar");
    assert!(calls.contains(&Call::BarCloseTick(11.0)));
}

/// A `CASH` pair prints no last price, so its bars fold the quote MID — and a print would not move
/// them.
#[test]
fn a_cash_bar_stream_aggregates_the_mid() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(SubscriptionId(0), 2, "EUR.USD.IDEALPRO".into(), bars(60_000, true));
    b.on_frame(&json!({ "topic": "smd+2", "_updated": 60_000, "84": "1.1000", "86": "1.1002" }));
    let calls = sink.take();
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, Call::Forming(_, bar) if (bar.close - 1.1001).abs() < 1e-12)),
        "the forming bar is the mid: {calls:?}"
    );
}

/// The `data_delay` calls alone, in order.
fn delays(calls: &[Call]) -> Vec<(String, DataDelay)> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Delay(_, stream, d) => Some((stream.clone(), *d)),
            _ => None,
        })
        .collect()
}

/// A delayed cpapi row labels the quotes stream `DataDelay::Delayed` ONCE — at first knowledge,
/// before the quote it rides on — and later rows (with or without the marker) send no label.
#[test]
fn a_delayed_row_is_labelled_delayed_once() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(SubscriptionId(0), 3, "AAPL.SMART.USD".into(), Lane::Quotes);
    b.on_frame(&json!({ "topic": "smd+3", "6509": "DPB", "84": 1.0, "86": 2.0 }));
    b.on_frame(&json!({ "topic": "smd+3", "84": 1.5 }));
    b.on_frame(&json!({ "topic": "smd+3", "6509": "DPB", "84": 1.6 }));
    let calls = sink.take();
    assert_eq!(delays(&calls), vec![("quotes".to_string(), DataDelay::DELAYED)], "{calls:?}");
    assert!(matches!(calls[0], Call::Delay(..)), "the label precedes the data: {calls:?}");
    assert_eq!(calls.iter().filter(|c| matches!(c, Call::Quote(..))).count(), 3);
}

/// A real-time row labels `RealTime`; a row before any marker sends no label at all.
#[test]
fn a_real_time_row_is_labelled_real_time() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(SubscriptionId(0), 4, "EUR.USD.IDEALPRO".into(), Lane::Quotes);
    b.on_frame(&json!({ "topic": "smd+4", "84": 1.1, "86": 1.2 }));
    assert!(delays(&sink.take()).is_empty(), "no marker yet, no label");
    b.on_frame(&json!({ "topic": "smd+4", "6509": "RpB", "84": 1.11 }));
    assert_eq!(delays(&sink.take()), vec![("quotes".to_string(), DataDelay::RealTime)]);
}

/// A change of marker is reported ONCE, per stream that carries it: delayed → frozen (`Y`) on the
/// quotes and trades lanes, then repeats of the new marker send nothing; `N` and unknown letters
/// send no label and keep the last one.
#[test]
fn a_change_is_reported_once() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(SubscriptionId(0), 5, "AAPL.SMART.USD".into(), Lane::Quotes);
    b.add(SubscriptionId(1), 5, "AAPL.SMART.USD".into(), Lane::Trades);
    b.on_frame(&json!({ "topic": "smd+5", "6509": "D", "84": 1.0, "86": 2.0, "31": 1.5 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "Y", "84": 1.1, "31": 1.6 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "Y", "84": 1.2, "31": 1.7 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "N", "84": 1.3, "31": 1.8 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "?", "84": 1.4, "31": 1.9 }));
    assert_eq!(
        delays(&sink.take()),
        vec![
            ("quotes".to_string(), DataDelay::DELAYED),
            ("trades".to_string(), DataDelay::DELAYED),
            ("quotes".to_string(), DataDelay::Frozen),
            ("trades".to_string(), DataDelay::Frozen),
        ]
    );
}

/// A lane that gets no data from a row gets no label from it: a quote-only row labels the quotes
/// stream, not the trades stream beside it.
#[test]
fn a_lane_without_data_in_the_row_is_not_labelled() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(SubscriptionId(0), 6, "AAPL.SMART.USD".into(), Lane::Quotes);
    b.add(SubscriptionId(1), 6, "AAPL.SMART.USD".into(), Lane::Trades);
    b.on_frame(&json!({ "topic": "smd+6", "6509": "D", "84": 1.0, "86": 2.0 }));
    assert_eq!(delays(&sink.take()), vec![("quotes".to_string(), DataDelay::DELAYED)]);
}

/// A quote stream joining a conId that already holds a quote is handed it at once, without
/// waiting for the next row.
#[test]
fn a_second_quote_stream_gets_the_held_quote() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(SubscriptionId(0), 4, "X.SMART.USD".into(), Lane::Trades);
    b.on_frame(&json!({ "topic": "smd+4", "84": 5.0, "86": 6.0 }));
    assert!(sink.take().is_empty(), "a trade lane takes no quote");
    b.add(SubscriptionId(1), 4, "X.SMART.USD".into(), Lane::Quotes);
    let calls = sink.take();
    assert!(matches!(calls.as_slice(), [Call::Quote(_, q)] if q.bid == 5.0 && q.ask == 6.0));
}

/// Removing streams: the conId is reported for unsubscription only with its LAST stream, and an
/// unknown id is a no-op.
#[test]
fn remove_reports_the_conid_with_its_last_stream() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(SubscriptionId(0), 5, "A.SMART.USD".into(), Lane::Quotes);
    b.add(SubscriptionId(1), 5, "A.SMART.USD".into(), Lane::Trades);
    assert_eq!(b.remove(SubscriptionId(9)), None);
    assert_eq!(b.remove(SubscriptionId(0)), None);
    assert_eq!(b.len(), 1);
    assert_eq!(b.remove(SubscriptionId(1)), Some(5));
    assert_eq!(b.len(), 0);
}

/// A dropped socket opens every stream's gap ONCE, however many redials fail.
#[test]
fn a_gap_opens_once_per_drop() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(SubscriptionId(0), 6, "A.SMART.USD".into(), Lane::Quotes);
    b.open_gaps(100);
    b.open_gaps(200);
    let calls = sink.take();
    assert_eq!(
        calls,
        vec![Call::Status(
            "A.SMART.USD".into(),
            "quotes".into(),
            StreamStatus::GapStart { at_ts_ms: 100 }
        )]
    );
}

/// The tickle and the redial this pump waits are the market row's: a 30 s REST renewal and a FIXED
/// 2 s wait between failed dials. `REDIAL` is the row's `initial`, which is the whole policy only
/// while the row is fixed.
#[test]
fn the_market_timings_are_the_rows_tickle_and_fixed_redial() {
    assert_eq!(TICKLE_EVERY, Duration::from_secs(30));
    assert_eq!(REDIAL, Duration::from_secs(2));
    assert!(venue_timings::IBKR.market().reconnect().is_fixed());
}
