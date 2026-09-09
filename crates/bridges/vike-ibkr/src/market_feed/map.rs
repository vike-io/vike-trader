//! Pure IBKR market-data → vike-model mappers. The ONLY place ibapi types touch vike types.
//! No I/O, no threads — fixture-tested (`tests/ibkr_mktdata_map.rs`).

use ibapi::contracts::tick_types::TickType;
use ibapi::market_data::historical::Bar as HistBar;
use ibapi::market_data::realtime::{Bar as RtBar, TickTypes, Trade};
use vike_model::{Bar, QuoteTick, TradeTick};

/// Machine receive time in epoch-ms. Delegates to [`vike_model::clock::now_ms`] (the consolidation
/// point for the `SystemTime::now()…` idiom); kept as a thin `pub` wrapper for `super::pump`'s
/// `use super::map::now_ms` and this module's own call sites.
pub fn now_ms() -> i64 {
    vike_model::clock::now_ms()
}

/// Folds reqMktData's separate bid/ask price+size ticks into a running `QuoteTick`, emitting a fresh
/// quote whenever a relevant field changes AND both a bid and ask price are known. Delayed tick
/// types (66-71) fold identically to their realtime counterparts.
#[derive(Default)]
pub struct QuoteAccumulator {
    bid: f64,
    ask: f64,
    bid_size: f64,
    ask_size: f64,
    have_bid: bool,
    have_ask: bool,
}

impl QuoteAccumulator {
    /// Apply one `TickTypes` item; returns a `QuoteTick` to emit if the quote changed and both sides
    /// have a price, else `None`.
    pub fn apply(&mut self, tick: &TickTypes) -> Option<QuoteTick> {
        let mut changed = false;
        match tick {
            TickTypes::Price(p) => match normalize(&p.tick_type) {
                Some(Field::Bid) => {
                    self.bid = p.price;
                    self.have_bid = true;
                    changed = true;
                }
                Some(Field::Ask) => {
                    self.ask = p.price;
                    self.have_ask = true;
                    changed = true;
                }
                _ => {}
            },
            TickTypes::Size(s) => match normalize(&s.tick_type) {
                Some(Field::BidSize) => {
                    self.bid_size = s.size;
                    changed = true;
                }
                Some(Field::AskSize) => {
                    self.ask_size = s.size;
                    changed = true;
                }
                _ => {}
            },
            TickTypes::PriceSize(ps) => {
                if let Some(f) = normalize(&ps.price_tick_type) {
                    match f {
                        Field::Bid => {
                            self.bid = ps.price;
                            self.have_bid = true;
                        }
                        Field::Ask => {
                            self.ask = ps.price;
                            self.have_ask = true;
                        }
                        _ => {}
                    }
                }
                if let Some(f) = normalize(&ps.size_tick_type) {
                    match f {
                        Field::BidSize => self.bid_size = ps.size,
                        Field::AskSize => self.ask_size = ps.size,
                        _ => {}
                    }
                }
                changed = true;
            }
            _ => {}
        }
        if changed && self.have_bid && self.have_ask {
            Some(QuoteTick {
                ts: now_ms(),
                local_ts: now_ms(),
                bid: self.bid,
                ask: self.ask,
                bid_size: self.bid_size,
                ask_size: self.ask_size,
                symbol: String::new(),
            })
        } else {
            None
        }
    }
}

enum Field {
    Bid,
    Ask,
    BidSize,
    AskSize,
}

/// Collapse realtime + delayed tick types onto the four L1 fields we track.
fn normalize(t: &TickType) -> Option<Field> {
    match t {
        TickType::Bid | TickType::DelayedBid => Some(Field::Bid),
        TickType::Ask | TickType::DelayedAsk => Some(Field::Ask),
        TickType::BidSize | TickType::DelayedBidSize => Some(Field::BidSize),
        TickType::AskSize | TickType::DelayedAskSize => Some(Field::AskSize),
        _ => None,
    }
}

/// tick-by-tick AllLast/Last → TradeTick. `is_buyer_maker` is unknown from IB's tick-by-tick (no
/// aggressor side), so default false; ts = the tick's unix time in ms.
pub fn trade_from(t: &Trade) -> TradeTick {
    TradeTick {
        ts: t.time.unix_timestamp() * 1000,
        local_ts: now_ms(),
        price: t.price,
        size: t.size,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

/// reqRealTimeBars (5s) → vike `Bar`. `date` is the bar's start time.
pub fn bar_from_realtime(b: &RtBar) -> Bar {
    Bar {
        ts: b.date.unix_timestamp() * 1000,
        open: b.open,
        high: b.high,
        low: b.low,
        close: b.close,
        volume: b.volume,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Historical bar → vike `Bar` (used by seed + PR-3b). `BarTimestamp::Date` (daily) → midnight-UTC ms.
pub fn bar_from_historical(b: &HistBar) -> Bar {
    use ibapi::market_data::historical::BarTimestamp;
    let ts = match b.date {
        BarTimestamp::DateTime(dt) => dt.unix_timestamp() * 1000,
        BarTimestamp::Date(d) => d.midnight().assume_utc().unix_timestamp() * 1000,
    };
    Bar {
        ts,
        open: b.open,
        high: b.high,
        low: b.low,
        close: b.close,
        volume: b.volume,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}
