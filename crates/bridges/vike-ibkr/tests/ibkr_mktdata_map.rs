//! Pure IBKR market-data mapper fixtures — the no-network CI gate.
#![cfg(feature = "ibkr-socket")]

use ibapi::contracts::tick_types::TickType;
use ibapi::market_data::realtime::{
    Bar as RtBar, TickAttribute, TickPrice, TickSize, TickTypes, Trade, TradeAttribute,
};
use vike_data::RecordingSink;
use vike_ibkr::market_feed::map::{
    QuoteAccumulator, bar_from_realtime, forward_quote_tick, trade_from,
};
use vike_model::DataDelay;

#[test]
fn quote_accumulator_folds_bid_ask_price_and_size() {
    let mut q = QuoteAccumulator::default();
    // First a bid price, then ask price, then sizes — QuoteTick emits once both sides have a price.
    assert!(
        q.apply(&TickTypes::Price(TickPrice {
            tick_type: TickType::Bid,
            price: 190.0,
            attributes: attr()
        }))
        .is_none()
    );
    let out = q.apply(&TickTypes::Price(TickPrice {
        tick_type: TickType::Ask,
        price: 190.5,
        attributes: attr(),
    }));
    let t = out.expect("emits once both bid and ask present");
    assert_eq!(t.bid, 190.0);
    assert_eq!(t.ask, 190.5);
    // A size tick updates the running quote and re-emits.
    let out2 = q.apply(&TickTypes::Size(TickSize { tick_type: TickType::BidSize, size: 300.0 }));
    assert_eq!(out2.expect("size re-emits").bid_size, 300.0);
    // Delayed tick types map the same as their realtime counterparts.
    let out3 = q.apply(&TickTypes::Price(TickPrice {
        tick_type: TickType::DelayedBid,
        price: 189.0,
        attributes: attr(),
    }));
    assert_eq!(out3.expect("delayed bid").bid, 189.0);
}

#[test]
fn trade_maps_price_size_and_maker_flag() {
    let tr = Trade {
        time: time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        price: 191.25,
        size: 10.0,
        trade_attribute: TradeAttribute { past_limit: false, unreported: false },
        exchange: "SMART".into(),
        special_conditions: String::new(),
    };
    let t = trade_from(&tr);
    assert_eq!(t.price, 191.25);
    assert_eq!(t.size, 10.0);
    assert_eq!(t.ts, 1_700_000_000_000); // seconds → ms
}

#[test]
fn realtime_bar_maps_ohlcv_and_ts_ms() {
    let b = RtBar {
        date: time::OffsetDateTime::from_unix_timestamp(1_700_000_005).unwrap(),
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 100.0,
        wap: 1.2,
        count: 7,
    };
    let vb = bar_from_realtime(&b);
    assert_eq!(
        (vb.ts, vb.open, vb.high, vb.low, vb.close, vb.volume),
        (1_700_000_005_000, 1.0, 2.0, 0.5, 1.5, 100.0)
    );
}

fn attr() -> TickAttribute {
    TickAttribute { can_auto_execute: false, past_limit: false, pre_open: false }
}

// ---- the delay label (decision 0140): read off each tick, kept beside the quote ----

fn price(tick_type: TickType, price: f64) -> TickTypes {
    TickTypes::Price(TickPrice { tick_type, price, attributes: attr() })
}

/// Drive `ticks` through the quotes pump's one per-tick step into a recording sink.
fn pump(ticks: &[TickTypes]) -> RecordingSink {
    let sink = RecordingSink::default();
    let mut acc = QuoteAccumulator::default();
    for t in ticks {
        forward_quote_tick(&mut acc, t, "ibkr", "AAPL.SMART.USD", &sink);
    }
    sink
}

fn labels(sink: &RecordingSink) -> Vec<(String, DataDelay)> {
    sink.data_delays().into_iter().map(|(_, _, stream, d)| (stream, d)).collect()
}

/// A `DelayedBid`/`DelayedAsk` sequence yields a quote whose stream reports `Delayed`, sent ONCE,
/// BEFORE the first quote, and not again while the label holds (a size tick re-emits the quote, not
/// the label).
#[test]
fn a_delayed_tick_sequence_reports_a_delayed_stream() {
    let sink = pump(&[
        price(TickType::DelayedBid, 189.0),
        price(TickType::DelayedAsk, 189.5),
        TickTypes::Size(TickSize { tick_type: TickType::DelayedBidSize, size: 300.0 }),
    ]);
    assert_eq!(labels(&sink), vec![("quotes".to_string(), DataDelay::DELAYED)]);
    let calls = sink.calls();
    assert!(
        calls[0].starts_with("data_delay:ibkr:AAPL.SMART.USD:quotes:"),
        "label first: {calls:?}"
    );
    assert_eq!(sink.quotes().len(), 2, "both quote emissions still delivered: {calls:?}");
    assert_eq!((sink.quotes()[0].2.bid, sink.quotes()[0].2.ask), (189.0, 189.5));
}

/// The real-time twin of the sequence above reports `RealTime`, also once.
#[test]
fn a_real_time_tick_sequence_reports_a_real_time_stream() {
    let sink = pump(&[
        price(TickType::Bid, 1.0850),
        price(TickType::Ask, 1.0851),
        price(TickType::Bid, 1.0849),
    ]);
    assert_eq!(labels(&sink), vec![("quotes".to_string(), DataDelay::RealTime)]);
    assert_eq!(sink.quotes().len(), 2);
}

/// The label follows the TICKS, not the request: a stream that turns real-time mid-way (or back)
/// reports each change once; a quote is only as fresh as its staler side; IB's frozen
/// `marketDataType` message relabels the next quote `Frozen`, and a later real-time one clears it.
#[test]
fn the_label_changes_with_the_ticks_and_each_change_is_reported_once() {
    use ibapi::market_data::MarketDataType;
    let sink = pump(&[
        price(TickType::DelayedBid, 10.0),
        price(TickType::Ask, 10.1), // one side delayed -> the quote is delayed
        price(TickType::Bid, 10.0), // both real-time now
        TickTypes::MarketDataType(MarketDataType::DelayedFrozen),
        price(TickType::Bid, 10.0),
        TickTypes::MarketDataType(MarketDataType::Realtime),
        price(TickType::Ask, 10.2),
    ]);
    assert_eq!(
        labels(&sink).into_iter().map(|(_, d)| d).collect::<Vec<_>>(),
        vec![DataDelay::DELAYED, DataDelay::RealTime, DataDelay::Frozen, DataDelay::RealTime]
    );
}

/// Nothing is labelled before there is a quote to label.
#[test]
fn no_label_before_both_sides_are_priced() {
    let mut acc = QuoteAccumulator::default();
    assert_eq!(acc.delay(), None);
    assert!(acc.apply(&price(TickType::DelayedBid, 5.0)).is_none());
    assert_eq!(acc.take_delay_change(), None);
    assert!(acc.apply(&price(TickType::DelayedAsk, 5.1)).is_some());
    assert_eq!(acc.take_delay_change(), Some(DataDelay::DELAYED));
    assert_eq!(acc.take_delay_change(), None, "reported once");
}

/// IB's `marketDataType` message relabels the stream AT ONCE: the label is sent from that tick,
/// which yields no quote, not held back until the next price moves.
#[test]
fn a_market_data_type_relabel_is_sent_from_its_own_tick() {
    use ibapi::market_data::MarketDataType;
    let sink = RecordingSink::default();
    let mut acc = QuoteAccumulator::default();
    let mut step = |t: TickTypes| forward_quote_tick(&mut acc, &t, "ibkr", "AAPL.SMART.USD", &sink);
    step(price(TickType::DelayedBid, 10.0));
    step(price(TickType::DelayedAsk, 10.1));
    let quotes_before = sink.quotes().len();
    step(TickTypes::MarketDataType(MarketDataType::DelayedFrozen));
    assert_eq!(sink.quotes().len(), quotes_before, "a relabel moves no price");
    assert_eq!(
        labels(&sink).into_iter().map(|(_, d)| d).collect::<Vec<_>>(),
        vec![DataDelay::DELAYED, DataDelay::Frozen],
        "the frozen label left with its own tick"
    );
    assert!(sink.calls().last().unwrap().starts_with("data_delay:"), "{:?}", sink.calls());
}
