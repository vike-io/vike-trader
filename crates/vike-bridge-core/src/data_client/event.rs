//! **What one decoded market-data frame says** (`DataClientEvent`): the ONE vocabulary every data
//! client's decoders answer in, the four per-bridge `MdEvent` copies (binance family, bybit, okx,
//! deribit) folded into their superset, plus the bar, mark and book payloads (`BarKline`,
//! `DataClientMark`, `DataClientBook`) the kline, mark and book lanes decode to.
//!
//! A tick pump's `route_frame` answers one event per frame (a quote, the trades, "the book changed",
//! a gap, nothing); a `crate::data_client::ws::DataClientWebsocket` lane's decoder hands the engine as many
//! as the frame holds, and the engine is the one place they reach the sink.

use std::sync::Arc;

use vike_marketdata::{Bar, FundingRate, L2Book, L2BookLevel, TickQuote, TickTrade};

/// One decoded market-data event (the module doc says who answers in it).
#[derive(Debug, Clone, PartialEq)]
pub enum DataClientEvent {
    /// One kline: a closed bar goes to the lossless `close_bar` lane, a forming one to `forming_bar`.
    Bar(BarKline),
    /// A bar whose close the decoder INFERRED (the venue marks none: its successor's first push, a
    /// newer bucket, closes it): the lossless `close_bar` lane alone. The successor's forming
    /// [`DataClientEvent::Bar`], emitted next from the same frame, ticks the bar-close cache and wakes.
    InferredClose(Bar),
    /// The venue's REAL mark price.
    Mark(DataClientMark),
    /// The public funding rate a mark frame (or a ticker) states.
    Funding(FundingRate),
    /// A top-of-book quote.
    Quote(TickQuote),
    /// The ONE print of a frame that holds one (binance's `aggTrade`/`trade`): no `Vec` on the tick path.
    Trade(TickTrade),
    /// EVERY print of a frame that holds several, in frame order. Never empty: a frame with no readable
    /// print is [`DataClientEvent::Ignored`].
    Trades(Vec<TickTrade>),
    /// The L2 book changed (a snapshot applied or a diff folded): the pump publishes it. `timestamp_ms` is the
    /// frame's venue time in epoch-ms, `0` when the frame states none (binance, okx).
    BookUpdated { timestamp_ms: i64 },
    /// The lane's folded L2 book, whole: the lossless `book` lane, for a `Session` lane that folds its
    /// own standing book (deribit's `change_id` chain).
    Book(DataClientBook),
    /// A full-depth snapshot the frame states whole (no deltas, no sequence: hyperliquid's `l2Book`):
    /// the DOM's `l2_snapshot`, verbatim, at the venue's `ts` (epoch-ms, `0` when the frame states none).
    DepthSnapshot { tick_size: f64, bids: Vec<L2BookLevel>, asks: Vec<L2BookLevel>, ts: i64 },
    /// The book's sequence broke (frames were dropped); it was NOT folded and must be re-seeded.
    Resync,
    /// Anything else: an ack, a keepalive reply, another channel, a row the decoder cannot read.
    Ignored,
}

/// A kline lane's payload: one bar and whether the venue marked it FINAL (bybit `confirm=true`, okx
/// `confirm=="1"`, binance `x`).
#[derive(Debug, Clone, PartialEq)]
pub struct BarKline {
    /// The bar, built through [`crate::klines::kline_to_bar`].
    pub bar: Bar,
    /// `true` when the venue marked the candle closed.
    pub closed: bool,
}

/// A mark lane's payload: the venue's REAL mark price and the epoch-ms stamp it carried. Only a mark
/// that passes [`crate::mark_stream::is_valid_mark`] becomes one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DataClientMark {
    /// The mark price.
    pub px: f64,
    /// The venue's stamp for it (epoch-ms; `0` when the frame carried none).
    pub ts: i64,
}

/// A book lane's payload: the standing book the lane folds copy-on-write, handed to the sink as the
/// same `Arc`. Two are equal when they are the SAME published book: `L2Book` has no value equality.
#[derive(Debug, Clone)]
pub struct DataClientBook(pub Arc<L2Book>);

impl PartialEq for DataClientBook {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
