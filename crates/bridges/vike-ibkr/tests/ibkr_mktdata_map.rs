//! Pure IBKR market-data mapper fixtures — the no-network CI gate.
#![cfg(feature = "ibkr-socket")]

use ibapi::contracts::tick_types::TickType;
use ibapi::market_data::realtime::{
    Bar as RtBar, TickAttribute, TickPrice, TickSize, TickTypes, Trade, TradeAttribute,
};
use vike_ibkr::market_feed::map::{QuoteAccumulator, bar_from_realtime, trade_from};

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
        tick_type: "AllLast".into(),
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
