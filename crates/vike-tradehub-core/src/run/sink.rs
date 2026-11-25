//! The feed-to-core adapter: the tick-to-bar synthesizer and the `DataClientSink` that drives it.

use std::sync::{Arc, Mutex};

use vike_core::CoreHandle;
use vike_data::DataClientSink;
use vike_execution::{BarSender, BarUpdate};
use vike_marketdata::{Bar, BarSides, L2Book, TickQuote, TickTrade};

#[cfg(doc)]
use super::config::MountPlacement;

/// Event-time OHLC bar synthesizer, the paper-fill driver: folds a quote stream into
/// fixed-`interval_ms` windows and emits a closed [`Bar`] of the MID, with its [`BarSides`] (the
/// bid's and the ask's path over the same window), when a quote crosses into a new window. The mid
/// bar is the series a strategy and the chart read; the sides are what the paper book fills on (a
/// buy on the ask, a sell on the bid: `vike_fills::fill_model::side_bar`, owner 2026-11-16).
/// Event-time (never wall-clock), so deterministic under a scripted feed and correct live. O(1).
///
/// The mid's fold is [`vike_marketdata::BarConsolidator`], whose monotonic close rule means an
/// out-of-order tick never emits a backwards-stamped bar; each quote is a one-price sample
/// ([`vike_model::one_price_bar`], `volume` 0.0) and the bar is stamped with the window's CLOSE. The
/// sides are the one quote-side fold every bar of the node gets (`crate::core_sink::QuoteSidePaths`,
/// epoch-aligned windows like the mid's): a window's side path opens at the quote standing when it
/// began.
pub struct TickBarSynthesizer {
    mid: vike_marketdata::BarConsolidator,
    sides: crate::core_sink::QuoteSidePaths,
}

impl TickBarSynthesizer {
    pub fn new(interval_ms: i64) -> Self {
        // Default a nonsensical interval to 1m here: the consolidator's floor is only a
        // div-by-zero guard.
        let interval_ms = if interval_ms > 0 { interval_ms } else { 60_000 };
        TickBarSynthesizer {
            mid: vike_marketdata::BarConsolidator::new(interval_ms),
            sides: crate::core_sink::QuoteSidePaths::new(interval_ms, 0),
        }
    }

    /// Fold one quote; `Some((bar, sides))` iff it closes the previous window (that window's mid
    /// bar and its sides, `None` when no usable quote came). The caller must emit it BEFORE
    /// forwarding the quote, so the paper fill uses the quotes that rested through the window (the
    /// maker re-quotes on the forwarded tick).
    pub fn on_quote(&mut self, quote: &TickQuote) -> Option<(Bar, Option<BarSides>)> {
        let closed = self.mid.fold(&vike_model::one_price_bar(quote.timestamp, quote.mid()));
        self.sides.on_quote(quote.timestamp, quote.bid, quote.ask);
        Some(self.stamped(closed?))
    }

    /// Force-close the current partial window (teardown / manual flush). `None` if nothing is open.
    pub fn flush(&mut self) -> Option<(Bar, Option<BarSides>)> {
        let closed = self.mid.flush()?;
        Some(self.stamped(closed))
    }

    /// The consolidator stamps a window's START; the mid bar is stamped at its CLOSE: shift one. The
    /// sides keep the START, as every venue candle's sides do (`crate::CoreLaneSink`): the window a
    /// side path covers, whatever the bar beside it carries, which is how a paper book stamps a
    /// `fill = "close"` fill at the bar's END on either kind of series (`ExecutionClient::fill_at_close`).
    fn stamped(&self, mut bar: Bar) -> (Bar, Option<BarSides>) {
        let start = bar.timestamp;
        bar.timestamp += self.mid.interval_ms();
        let sides = self.sides.sides(start, start);
        (bar, sides)
    }
}

/// The feed→core adapter: a [`DataClientSink`] forwarding quote/trade/book onto the core's lossless
/// tick lane AND driving the [`TickBarSynthesizer`], whose closed bars go onto `handle.bar_sender()`
/// to fill the paper book. Fed by a real feed (`vike-tradehub`'s `StrategyLanesLive`) or a scripted one.
///
/// Only the QUOTE lane drives the synth (a feed emits an L1 quote on every top-of-book change): its
/// mid makes the bar, its bid and ask the bar's sides, which ride the same `BarUpdate` to the paper
/// book; `book`/`trade` are forwarded but close no bar (an `L2Book` has no event ts).
pub struct TickSink {
    /// quote/trade/book onto the tick lane AND stream-health onto the strategy's `on_feed_status`.
    core: crate::CoreLaneSink,
    /// the SYNTHESIZED bars only: the bar verbs are no-ops (Polymarket has no candles); `mark_tick`
    /// is NOT one (it forwards through `core`).
    bars: BarSender,
    venue: String,
    symbol: String,
    interval: String,
    synth: Mutex<TickBarSynthesizer>,
}

impl TickSink {
    /// `venue`/`symbol`/`interval` must match the [`MountPlacement`]: the synth bars carry them
    /// so the paper book routes and fills.
    pub fn new(
        handle: &CoreHandle,
        venue: impl Into<String>,
        symbol: impl Into<String>,
        interval: impl Into<String>,
        interval_ms: i64,
    ) -> Self {
        TickSink {
            core: crate::CoreLaneSink::new(
                handle.bar_sender(),
                handle.market_sender(),
                handle.tick_sender(),
            ),
            bars: handle.bar_sender(),
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            synth: Mutex::new(TickBarSynthesizer::new(interval_ms)),
        }
    }

    /// Force-close the current partial synth window (call at teardown so the last bar's fills land).
    pub fn flush_bar(&self) {
        let closed = self.synth.lock().expect("synth mutex").flush();
        if let Some(closed) = closed {
            self.send_bar(closed);
        }
    }

    /// Emit a just-closed window's bar BEFORE the caller forwards the tick (the paper fill's order).
    fn feed_quote(&self, quote: &TickQuote) {
        let closed = self.synth.lock().expect("synth mutex").on_quote(quote);
        if let Some(closed) = closed {
            self.send_bar(closed);
        }
    }

    fn send_bar(&self, (bar, sides): (Bar, Option<BarSides>)) {
        let _ = self.bars.close(BarUpdate {
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            interval: self.interval.clone(),
            bar,
            sides: sides.map(Box::new),
        });
    }
}

impl DataClientSink for TickSink {
    // Bar lanes are unused: Polymarket has no candles, and the mount synthesizes its own bars.
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}

    /// "Option B" cross-symbol routing: forward the UNDERLYING spot mark (another symbol, e.g.
    /// `btcusdt`) onto the mark lane, where `drain_market` routes it to a maker declaring it as its
    /// `underlying_symbol`. Inert for a mount with no underlying (the core dispatch early-returns).
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.core.mark_tick(venue, symbol, px, ts);
    }

    /// The price tap's throttled top of book (decision 0208) forwards like `mark_tick`.
    fn top_of_book_tick(&self, venue: &str, symbol: &str, bid: f64, ask: f64, ts: i64) {
        self.core.top_of_book_tick(venue, symbol, bid, ask, ts);
    }

    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        // close the completed window FIRST, THEN forward the quote the maker re-quotes on.
        self.feed_quote(&quote);
        self.core.quote(venue, symbol, quote);
    }

    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        self.core.trade(venue, symbol, trade);
    }

    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.core.book(venue, symbol, book);
    }

    fn stream_status(
        &self,
        venue: &str,
        symbol: &str,
        stream: &str,
        status: vike_model::StreamStatus,
    ) {
        // Not the trait's no-op default: the maker must learn a feed went stale/down.
        self.core.stream_status(venue, symbol, stream, status);
    }

    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: vike_model::StreamDelay) {
        // Not the trait's no-op default either: the core sink is where the mount announces a
        // delayed stream (decision 0140; `vike_tradehub_core::CoreLaneSink`'s module doc).
        self.core.data_delay(venue, symbol, stream, delay);
    }
}
