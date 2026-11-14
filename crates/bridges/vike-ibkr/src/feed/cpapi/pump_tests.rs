//! The pump's fold, driven offline through [`Book`]: routing by conId, the per-lane delivery, bars
//! aggregated from prints (and from the mid for a `CASH` pair), the delay label, and the gap
//! bookkeeping. No socket, no REST.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use vike_data::DataClientSubscriptionId;
use vike_model::{StreamDelay, StreamStatus};
use vike_venues::venue_timings;

use super::{Book, FeedLane, REDIAL, TICKLE_EVERY};
use crate::feed::bar::BarAggregator;
use crate::feed::test_support::{Call, RecSink};

fn bars(interval_ms: i64, from_mid: bool) -> FeedLane {
    FeedLane::Bars {
        interval: "1m".into(),
        interval_ms,
        agg: BarAggregator::new(interval_ms),
        from_mid,
    }
}

fn book(sink: &Arc<RecSink>) -> Book {
    Book::new(Arc::clone(sink) as Arc<dyn vike_data::DataClientSink>)
}

/// A frame reaches every stream on ITS conId, each lane takes what it serves, and a frame for a
/// conId nobody holds (or on another topic) is ignored.
#[test]
fn frames_route_by_conid_and_lane() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    assert!(b.add(DataClientSubscriptionId(0), 265598, "AAPL.SMART.USD".into(), FeedLane::Quotes));
    assert!(!b.add(DataClientSubscriptionId(1), 265598, "AAPL.SMART.USD".into(), FeedLane::Trades));

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
    b.add(DataClientSubscriptionId(0), 1, "AAPL.SMART.USD".into(), bars(60_000, false));
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
        (c.timestamp, c.open, c.high, c.low, c.close, c.volume),
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
    b.add(DataClientSubscriptionId(0), 2, "EUR.USD.IDEALPRO".into(), bars(60_000, true));
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
fn delays(calls: &[Call]) -> Vec<(String, StreamDelay)> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Delay(_, stream, d) => Some((stream.clone(), *d)),
            _ => None,
        })
        .collect()
}

/// A delayed cpapi row labels the quotes stream `StreamDelay::Delayed` ONCE — at first knowledge,
/// before the quote it rides on — and later rows (with or without the marker) send no label.
#[test]
fn a_delayed_row_is_labelled_delayed_once() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 3, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+3", "6509": "DPB", "84": 1.0, "86": 2.0 }));
    b.on_frame(&json!({ "topic": "smd+3", "84": 1.5 }));
    b.on_frame(&json!({ "topic": "smd+3", "6509": "DPB", "84": 1.6 }));
    let calls = sink.take();
    assert_eq!(delays(&calls), vec![("quotes".to_string(), StreamDelay::DELAYED)], "{calls:?}");
    assert!(matches!(calls[0], Call::Delay(..)), "the label precedes the data: {calls:?}");
    assert_eq!(calls.iter().filter(|c| matches!(c, Call::Quote(..))).count(), 3);
}

/// A real-time row labels `RealTime`; a row before any marker sends no label at all.
#[test]
fn a_real_time_row_is_labelled_real_time() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 4, "EUR.USD.IDEALPRO".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+4", "84": 1.1, "86": 1.2 }));
    assert!(delays(&sink.take()).is_empty(), "no marker yet, no label");
    b.on_frame(&json!({ "topic": "smd+4", "6509": "RpB", "84": 1.11 }));
    assert_eq!(delays(&sink.take()), vec![("quotes".to_string(), StreamDelay::RealTime)]);
}

/// **The delay rule's terms** (`DataClientSinkRouting::data_delay`: only a `RealTime` label under the word `quotes` lets the
/// recorder back in, and a re-subscribe does not): on one client, each symbol says its own label under that word, and a
/// stream new on a conId that already streams says the label in force before its first quote. A delayed stream that
/// turns real time says so, before the first real-time quote.
#[test]
fn each_stream_says_its_own_label_under_the_word_quotes_and_says_when_it_turns_real_time() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 10, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.add(DataClientSubscriptionId(1), 11, "EUR.USD.IDEALPRO".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+10", "6509": "D", "84": 1.0, "86": 2.0 }));
    b.on_frame(&json!({ "topic": "smd+11", "6509": "R", "84": 1.1, "86": 1.2 }));
    let calls = sink.take();
    assert!(calls.contains(&Call::Delay(
        "AAPL.SMART.USD".into(),
        "quotes".into(),
        StreamDelay::DELAYED
    )));
    assert!(calls.contains(&Call::Delay(
        "EUR.USD.IDEALPRO".into(),
        "quotes".into(),
        StreamDelay::RealTime
    )));

    assert_eq!(b.remove(DataClientSubscriptionId(0)), Some(10));
    b.add(DataClientSubscriptionId(2), 11, "EUR.USD.IDEALPRO.CASH".into(), FeedLane::Quotes);
    let calls = sink.take();
    assert!(
        matches!(
            calls.as_slice(),
            [Call::Delay(s, lane, StreamDelay::RealTime), Call::Quote(q, _)]
                if s == "EUR.USD.IDEALPRO.CASH" && lane == "quotes" && q == s
        ),
        "{calls:?}"
    );

    b.add(DataClientSubscriptionId(3), 12, "MSFT.SMART.USD".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+12", "6509": "D", "84": 1.0, "86": 2.0 }));
    sink.take();
    b.on_frame(&json!({ "topic": "smd+12", "6509": "R", "84": 1.5 }));
    let calls = sink.take();
    assert!(
        matches!(calls.as_slice(), [Call::Delay(_, _, StreamDelay::RealTime), Call::Quote(_, q)] if q.bid == 1.5),
        "the label first, or the recorder would stay withheld for this quote: {calls:?}"
    );
}

/// **A quote with no label in force is not delivered** (decision 0193 verdict 7): the recorder stores what is not
/// labelled delayed or frozen, so a row with no marker must not reach it as real time. The fold keeps the row's fields,
/// so the first row WITH a marker delivers its label and then the whole quote, and a row without a marker keeps the
/// label in force. A marker the decoder does not read (`N`, not subscribed) ends it, also after a real-time one.
#[test]
fn a_quote_with_no_label_in_force_is_not_delivered() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 8, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+8", "84": 1.0, "86": 2.0 }));
    b.on_frame(&json!({ "topic": "smd+8", "6509": "N", "84": 1.1 }));
    assert!(sink.take().is_empty(), "no label yet: neither a label nor a quote");

    b.on_frame(&json!({ "topic": "smd+8", "6509": "DPB", "84": 1.2 }));
    let calls = sink.take();
    assert!(
        matches!(
            calls.as_slice(),
            [Call::Delay(_, _, StreamDelay::Delayed { .. }), Call::Quote(_, q)]
                if q.bid == 1.2 && q.ask == 2.0
        ),
        "the label, then the quote the row completes: {calls:?}"
    );

    b.on_frame(&json!({ "topic": "smd+8", "84": 1.3 }));
    assert!(matches!(sink.take().as_slice(), [Call::Quote(_, q)] if q.bid == 1.3));

    b.on_frame(&json!({ "topic": "smd+8", "6509": "R", "84": 1.4 }));
    sink.take();
    b.on_frame(&json!({ "topic": "smd+8", "6509": "N", "84": 1.5 }));
    assert!(sink.take().is_empty(), "not subscribed: not real time on the last row's word");
    b.on_frame(&json!({ "topic": "smd+8", "6509": "R", "84": 1.6 }));
    assert!(
        matches!(sink.take().as_slice(), [Call::Quote(_, q)] if q.bid == 1.6),
        "the label did not change, so none is said again"
    );
}

/// A change of marker is reported ONCE, per stream that carries it: delayed → frozen (`Y`) on the
/// quotes and trades lanes, then repeats of the new marker send nothing; `N` and unknown letters
/// send no label and keep the last one.
#[test]
fn a_change_is_reported_once() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 5, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.add(DataClientSubscriptionId(1), 5, "AAPL.SMART.USD".into(), FeedLane::Trades);
    b.on_frame(&json!({ "topic": "smd+5", "6509": "D", "84": 1.0, "86": 2.0, "31": 1.5 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "Y", "84": 1.1, "31": 1.6 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "Y", "84": 1.2, "31": 1.7 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "N", "84": 1.3, "31": 1.8 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "?", "84": 1.4, "31": 1.9 }));
    assert_eq!(
        delays(&sink.take()),
        vec![
            ("quotes".to_string(), StreamDelay::DELAYED),
            ("trades".to_string(), StreamDelay::DELAYED),
            ("quotes".to_string(), StreamDelay::Frozen),
            ("trades".to_string(), StreamDelay::Frozen),
        ]
    );
}

/// A lane that gets no data from a row gets no label from it: a quote-only row labels the quotes
/// stream, not the trades stream beside it.
#[test]
fn a_lane_without_data_in_the_row_is_not_labelled() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 6, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.add(DataClientSubscriptionId(1), 6, "AAPL.SMART.USD".into(), FeedLane::Trades);
    b.on_frame(&json!({ "topic": "smd+6", "6509": "D", "84": 1.0, "86": 2.0 }));
    assert_eq!(delays(&sink.take()), vec![("quotes".to_string(), StreamDelay::DELAYED)]);
}

/// A quote stream joining a conId that already holds a quote is handed it at once, without
/// waiting for the next row, its label first. A held quote with no label stays held.
#[test]
fn a_second_quote_stream_gets_the_held_quote() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 4, "X.SMART.USD".into(), FeedLane::Trades);
    b.on_frame(&json!({ "topic": "smd+4", "6509": "D", "84": 5.0, "86": 6.0 }));
    assert!(sink.take().is_empty(), "a trade lane takes no quote");
    b.add(DataClientSubscriptionId(1), 4, "X.SMART.USD".into(), FeedLane::Quotes);
    let calls = sink.take();
    assert!(matches!(
        calls.as_slice(),
        [Call::Delay(..), Call::Quote(_, q)] if q.bid == 5.0 && q.ask == 6.0
    ));

    b.add(DataClientSubscriptionId(2), 7, "Y.SMART.USD".into(), FeedLane::Trades);
    b.on_frame(&json!({ "topic": "smd+7", "84": 5.0, "86": 6.0 }));
    b.add(DataClientSubscriptionId(3), 7, "Y.SMART.USD".into(), FeedLane::Quotes);
    assert!(sink.take().is_empty(), "no label yet: not handed over");
}

/// The held quote a late subscriber is handed keeps the VENUE time it was first delivered with
/// (`_updated` of the row that made it), so its age reads true; the hand-over time is its
/// `local_ts`. It used to be stamped with the hand-over time, so a quote quiet for minutes read as
/// fresh.
#[test]
fn a_held_quote_keeps_its_venue_time_for_a_late_subscriber() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 8, "Z.SMART.USD".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+8", "_updated": 1_000, "6509": "R", "84": 5.0, "86": 6.0 }));
    // A later row that changes no quote field leaves the quote's time where it was.
    b.on_frame(&json!({ "topic": "smd+8", "_updated": 2_000, "7059": 3 }));
    let first = sink.take();
    assert!(
        matches!(first.as_slice(), [Call::Delay(..), Call::Quote(_, q)] if q.timestamp == 1_000)
    );

    b.add(DataClientSubscriptionId(1), 8, "Z.SMART.USD".into(), FeedLane::Quotes);
    let calls = sink.take();
    let [Call::Delay(..), Call::Quote(_, q)] = calls.as_slice() else {
        panic!("the held quote, its label first: {calls:?}")
    };
    assert_eq!(q.timestamp, 1_000, "the venue time the quote first had");
    assert!(
        q.local_timestamp > 1_000,
        "the receipt time is the hand-over's: {}",
        q.local_timestamp
    );
}

/// Removing streams: the conId is reported for unsubscription only with its LAST stream, and an
/// unknown id is a no-op.
#[test]
fn remove_reports_the_conid_with_its_last_stream() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 5, "A.SMART.USD".into(), FeedLane::Quotes);
    b.add(DataClientSubscriptionId(1), 5, "A.SMART.USD".into(), FeedLane::Trades);
    assert_eq!(b.remove(DataClientSubscriptionId(9)), None);
    assert_eq!(b.remove(DataClientSubscriptionId(0)), None);
    assert_eq!(b.len(), 1);
    assert_eq!(b.remove(DataClientSubscriptionId(1)), Some(5));
    assert_eq!(b.len(), 0);
}

/// A dropped socket opens every stream's gap ONCE, however many redials fail.
#[test]
fn a_gap_opens_once_per_drop() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 6, "A.SMART.USD".into(), FeedLane::Quotes);
    b.open_gaps(100);
    b.open_gaps(200);
    let calls = sink.take();
    assert_eq!(
        calls,
        vec![Call::Status(
            "A.SMART.USD".into(),
            "quotes".into(),
            StreamStatus::GapStart { at_timestamp_ms: 100 }
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
