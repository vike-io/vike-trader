//! The feed-to-core adapter: the tick-to-bar synthesizer and the `LiveDataSink` that drives it.

use std::sync::{Arc, Mutex};

use vike_core::CoreHandle;
use vike_data::LiveDataSink;
use vike_exec::{BarSender, BarUpdate};
use vike_model::{Bar, L2Book, QuoteTick, TradeTick};

#[cfg(doc)]
use super::config::MakerMountConfig;

/// Event-time OHLC bar synthesizer, the paper-fill driver: folds a `(ts, mid)` stream into
/// fixed-`interval_ms` windows and emits a closed [`Bar`] when a tick crosses into a new window.
/// Event-time (never wall-clock), so deterministic under a scripted feed and correct live. O(1).
///
/// The fold is [`vike_model::BarConsolidator`], whose monotonic close rule means an out-of-order
/// tick never emits a backwards-stamped bar. Added here: each tick is a one-price sample
/// ([`vike_model::one_price_bar`], `volume` 0.0) and the bar is stamped with the window's CLOSE.
pub struct TickBarSynthesizer {
    inner: vike_model::BarConsolidator,
}

impl TickBarSynthesizer {
    pub fn new(interval_ms: i64) -> Self {
        // Default a nonsensical interval to 1m here: the consolidator's floor is only a
        // div-by-zero guard.
        let interval_ms = if interval_ms > 0 { interval_ms } else { 60_000 };
        TickBarSynthesizer { inner: vike_model::BarConsolidator::new(interval_ms) }
    }

    /// Fold one `(ts, mid)`; `Some(bar)` iff this tick closes the previous window (that window's
    /// bar). The caller must emit it BEFORE forwarding the tick, so the paper fill uses the quotes
    /// that rested through the window (the maker re-quotes on the forwarded tick).
    pub fn on_price(&mut self, ts: i64, mid: f64) -> Option<Bar> {
        let closed = self.inner.fold(&vike_model::one_price_bar(ts, mid))?;
        Some(self.close_stamped(closed))
    }

    /// Force-close the current partial window (teardown / manual flush). `None` if nothing is open.
    pub fn flush(&mut self) -> Option<Bar> {
        let closed = self.inner.flush()?;
        Some(self.close_stamped(closed))
    }

    /// The consolidator stamps a window's START; the paper book fills at its CLOSE: shift one.
    fn close_stamped(&self, mut b: Bar) -> Bar {
        b.ts += self.inner.interval_ms();
        b
    }
}

/// The feed→core adapter: a [`LiveDataSink`] forwarding quote/trade/book onto the core's lossless
/// tick lane AND driving the [`TickBarSynthesizer`], whose closed bars go onto `handle.bar_sender()`
/// to fill the paper book. Fed by a real feed (`vike-tradehub`'s `Feeds`) or a scripted one.
///
/// Only the QUOTE lane's `(ts, mid)` drives the synth (`Feeds` emits an L1 quote on every
/// top-of-book change); `book`/`trade` are forwarded but close no bar (an `L2Book` has no event ts).
pub struct MakerSink {
    /// quote/trade/book onto the tick lane AND stream-health onto the strategy's `on_feed_status`.
    core: vike_core::CoreLaneSink,
    /// the SYNTHESIZED bars only: the bar verbs are no-ops (Polymarket has no candles); `mark_tick`
    /// is NOT one (it forwards through `core`).
    bars: BarSender,
    venue: String,
    symbol: String,
    interval: String,
    synth: Mutex<TickBarSynthesizer>,
}

impl MakerSink {
    /// `venue`/`symbol`/`interval` must match the [`MakerMountConfig`]: the synth bars carry them
    /// so the paper book routes and fills.
    pub fn new(
        handle: &CoreHandle,
        venue: impl Into<String>,
        symbol: impl Into<String>,
        interval: impl Into<String>,
        interval_ms: i64,
    ) -> Self {
        MakerSink {
            core: vike_core::CoreLaneSink::new(
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
        if let Some(bar) = closed {
            self.send_bar(bar);
        }
    }

    /// Emit a just-closed window's bar BEFORE the caller forwards the tick (the paper fill's order).
    fn feed_price(&self, ts: i64, mid: f64) {
        let closed = self.synth.lock().expect("synth mutex").on_price(ts, mid);
        if let Some(bar) = closed {
            self.send_bar(bar);
        }
    }

    fn send_bar(&self, bar: Bar) {
        let _ = self.bars.close(BarUpdate {
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            interval: self.interval.clone(),
            bar,
        });
    }
}

impl LiveDataSink for MakerSink {
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

    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        // close the completed window FIRST, THEN forward the quote the maker re-quotes on.
        self.feed_price(quote.ts, quote.mid());
        self.core.quote(venue, symbol, quote);
    }

    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
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
        status: vike_data::StreamStatus,
    ) {
        // Not the trait's no-op default: the maker must learn a feed went stale/down.
        self.core.stream_status(venue, symbol, stream, status);
    }
}
