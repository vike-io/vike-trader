//! Pure IBKR market-data → vike-model mappers. The ONLY place ibapi types touch vike types.
//! No I/O, no threads — fixture-tested (`tests/ibkr_mktdata_map.rs`).
//!
//! # The delay label is read off each TICK (decision 0140)
//!
//! `IbkrFeeds::connect` ASKS for delayed data (`crate::config::MktDataType`, delayed by default), and
//! IB then serves real-time for a class the account is entitled to and delayed for the rest. So the
//! request says nothing about what arrived; the tick does. A `DelayedBid`/`DelayedAsk` price tick
//! (types 66/67) is delayed data, a `Bid`/`Ask` one real-time, and the subscription's
//! `marketDataType` message (`TickTypes::MarketDataType`) says when IB is replaying the LAST value it
//! had (frozen, or delayed-frozen) rather than a moving market. [`QuoteAccumulator`] folds the prices
//! of both kinds onto one quote exactly as before and KEEPS the label beside it;
//! [`forward_quote_tick`] is the one place the quotes pump hands both to the sink, the label first
//! and only when it changes.

use ibapi::contracts::tick_types::TickType;
use ibapi::market_data::historical::Bar as HistBar;
use ibapi::market_data::realtime::{Bar as RtBar, TickTypes, Trade};
use vike_data::{LiveDataSink, StreamLabel};
use vike_marketdata::{Bar, QuoteTick, TradeTick};
use vike_model::DataDelay;

/// Folds reqMktData's separate bid/ask price+size ticks into a running `QuoteTick`, emitting a fresh
/// quote whenever a relevant field changes AND both a bid and ask price are known. Delayed tick
/// types (66-71) fold onto the same fields as their realtime counterparts, and the accumulator
/// REMEMBERS which kind each side's price came from: [`QuoteAccumulator::delay`] is the label of the
/// quote it would emit (module doc).
#[derive(Default)]
pub struct QuoteAccumulator {
    bid: f64,
    ask: f64,
    bid_size: f64,
    ask_size: f64,
    have_bid: bool,
    have_ask: bool,
    /// Whether the bid's / ask's last PRICE tick was a `Delayed*` type.
    bid_delayed: bool,
    ask_delayed: bool,
    /// The subscription's last `marketDataType` message said frozen or delayed-frozen.
    frozen: bool,
    /// The label [`QuoteAccumulator::take_delay_change`] last handed out.
    reported: Option<DataDelay>,
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
                    self.bid_delayed = is_delayed(&p.tick_type);
                    changed = true;
                }
                Some(Field::Ask) => {
                    self.ask = p.price;
                    self.have_ask = true;
                    self.ask_delayed = is_delayed(&p.tick_type);
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
                            self.bid_delayed = is_delayed(&ps.price_tick_type);
                        }
                        Field::Ask => {
                            self.ask = ps.price;
                            self.have_ask = true;
                            self.ask_delayed = is_delayed(&ps.price_tick_type);
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
            // Not a quote field: it relabels the stream and emits nothing by itself (the next quote
            // carries the new label). `Unknown` (0) says nothing and changes nothing.
            TickTypes::MarketDataType(t) => {
                use ibapi::market_data::MarketDataType as M;
                match t {
                    M::Frozen | M::DelayedFrozen => self.frozen = true,
                    M::Realtime | M::Delayed => self.frozen = false,
                    M::Unknown => {}
                }
            }
            _ => {}
        }
        if changed && self.have_bid && self.have_ask {
            Some(QuoteTick {
                ts: vike_model::now_ms(),
                local_ts: vike_model::now_ms(),
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

    /// The label of the quote this accumulator would emit: `None` until both sides have a price;
    /// [`DataDelay::Frozen`] while IB says it replays the last value; [`DataDelay::Delayed`] when
    /// EITHER side's price came off a `Delayed*` tick (a quote is only as fresh as its staler side);
    /// [`DataDelay::RealTime`] otherwise. IB states no lag figure on a socket tick, so `lag_ms` is
    /// `None` (`vike_model::DataDelay`'s doc).
    pub fn delay(&self) -> Option<DataDelay> {
        if !(self.have_bid && self.have_ask) {
            return None;
        }
        Some(if self.frozen {
            DataDelay::Frozen
        } else if self.bid_delayed || self.ask_delayed {
            DataDelay::DELAYED
        } else {
            DataDelay::RealTime
        })
    }

    /// The current [`Self::delay`] if it differs from the one this method last returned, else
    /// `None`: the first known label once, then every change once (the `LiveDataSink::data_delay`
    /// contract, "at subscribe and whenever it changes", never per tick).
    pub fn take_delay_change(&mut self) -> Option<DataDelay> {
        let now = self.delay()?;
        if self.reported == Some(now) {
            return None;
        }
        self.reported = Some(now);
        Some(now)
    }
}

/// The first price of a tick that can tell which BAND of a market rule a stock trades in: a bid, an ask, a last or a
/// closing price, real-time or delayed, finite and positive. `None` for every other tick (sizes, volumes, the
/// market-data-type message). Used only to read a band off (`crate::fx_socket::price_from`), never as a quote.
pub fn reference_price_of(tick: &TickTypes) -> Option<f64> {
    let (kind, price) = match tick {
        TickTypes::Price(p) => (&p.tick_type, p.price),
        TickTypes::PriceSize(ps) => (&ps.price_tick_type, ps.price),
        _ => return None,
    };
    let priced = matches!(
        kind,
        TickType::Bid
            | TickType::Ask
            | TickType::Last
            | TickType::Close
            | TickType::DelayedBid
            | TickType::DelayedAsk
            | TickType::DelayedLast
            | TickType::DelayedClose
    );
    (priced && price.is_finite() && price > 0.0).then_some(price)
}

/// The quotes pump's per-tick step: fold `tick`, send the stream's [`StreamLabel::Quotes`] label the
/// moment the fold changed it, then the quote the tick yielded, if any. The label goes FIRST, so a
/// consumer knows the label of the quote it is about to receive, and it goes from THIS tick even when
/// the tick yields no quote: IB's `marketDataType` message relabels a stream (frozen, or back)
/// without moving a price, and waiting for the next quote would leave the stream mislabelled until
/// then. Allocation-free on a tick that changes no label.
pub fn forward_quote_tick(
    acc: &mut QuoteAccumulator,
    tick: &TickTypes,
    venue: &str,
    symbol: &str,
    sink: &dyn LiveDataSink,
) {
    let quote = acc.apply(tick);
    if let Some(delay) = acc.take_delay_change() {
        sink.data_delay(venue, symbol, StreamLabel::Quotes.as_str(), delay);
    }
    if let Some(mut q) = quote {
        q.symbol.clear();
        // The delayed mark the exec judges an order's price against (E07, `crate::delayed_order`): the mid of a
        // quote of a stream that is not real-time; one label test on a real-time one.
        crate::delayed_order::note_quote(symbol, acc.delay(), q.bid, q.ask);
        sink.quote(venue, symbol, q);
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

/// Whether a quote-field tick type is IB's DELAYED twin. Only the four types [`normalize`] reads
/// reach it.
fn is_delayed(t: &TickType) -> bool {
    matches!(
        t,
        TickType::DelayedBid
            | TickType::DelayedAsk
            | TickType::DelayedBidSize
            | TickType::DelayedAskSize
    )
}

/// tick-by-tick AllLast/Last → TradeTick. `is_buyer_maker` is unknown from IB's tick-by-tick (no
/// aggressor side), so default false; ts = the tick's unix time in ms.
pub fn trade_from(t: &Trade) -> TradeTick {
    TradeTick {
        ts: t.time.unix_timestamp() * 1000,
        local_ts: vike_model::now_ms(),
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
