//! `PriceTap` — the last price the CEX tick pump delivered for each `(venue, symbol)`, kept BESIDE
//! the core's ingest so the control door can tell whether a symbol it just admitted has a price yet
//! (decision 0174).
//!
//! The core holds prices in its engines' price boards, which only the fold thread reads; the door
//! must not enter the fold. [`TapSink`] sits between [`super::pump_book::PumpBookFold`] and the
//! core's lane sink, forwards every verb unchanged and records the price of the three that carry
//! one (a quote's mid, a trade, a folded book's mid). One mutex, taken on the FEED's thread per
//! message and on a connection thread per door lookup: never on the fold.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::LiveDataSink;
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};
use vike_venues::link_deadman::LinkSource;

/// How old the last price may be and still be a price. A pump that died (or a symbol that stopped
/// quoting) must not leave an order to be priced and marked at a number from hours ago: past this the
/// answer is "none yet", which the door turns into a prompt retry. The core's price-only risk sweeps
/// read the same bound (`vike_core::PriceOnlyConfig::max_price_age`, set from here by
/// `venue_arming/deadman.rs`'s `price_only_config`), so the door and the sweeps never disagree about
/// what a stale price is.
pub(crate) const MAX_PRICE_AGE: Duration = Duration::from_secs(60);

/// The last price seen per `(venue, symbol)`, and the symbols whose VENUE socket has a gap open.
#[derive(Default)]
pub struct PriceTap {
    last: Mutex<HashMap<(String, String), (f64, Instant)>>,
    /// `(venue, symbol)` with a venue-source `GapStart` not yet closed by a `Live` (the core's link
    /// dead-man keeps the same window): a pump is not closed under one ([`Self::gap_open`]).
    gaps: Mutex<HashSet<(String, String)>>,
}

impl PriceTap {
    /// A price for `symbol` on `venue` no older than [`MAX_PRICE_AGE`], or `None` if the pump has
    /// delivered none (in that time).
    pub fn price(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.price_at(venue, symbol, Instant::now())
    }

    fn price_at(&self, venue: &str, symbol: &str, now: Instant) -> Option<f64> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string()))
            .filter(|(_, at)| now.saturating_duration_since(*at) <= MAX_PRICE_AGE)
            .map(|(px, _)| *px)
    }

    fn note(&self, venue: &str, symbol: &str, px: f64) {
        self.note_at(venue, symbol, px, Instant::now());
    }

    fn note_at(&self, venue: &str, symbol: &str, px: f64, at: Instant) {
        if px.is_finite() && px > 0.0 {
            self.last
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert((venue.to_string(), symbol.to_string()), (px, at));
        }
    }

    /// Whether the venue's own socket has a gap open for `symbol` right now: closing the symbol's pump
    /// under one would leave the core's link dead-man window for it without the `Live` that closes it.
    pub fn gap_open(&self, venue: &str, symbol: &str) -> bool {
        self.gaps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&(venue.to_string(), symbol.to_string()))
    }

    /// The pump of `symbol` was closed: drop its price (a re-subscribe starts from none).
    pub fn forget(&self, venue: &str, symbol: &str) {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(venue.to_string(), symbol.to_string()));
    }

    /// Follow the window the core's link dead-man keeps for the venue source: a `GapStart` opens it, a
    /// `Live` closes it, `Stale` moves nothing, and the strat-link and trade-link words are other sources.
    fn note_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        if LinkSource::of(stream) != LinkSource::Venue {
            return;
        }
        let key = (venue.to_string(), symbol.to_string());
        let mut gaps = self.gaps.lock().unwrap_or_else(PoisonError::into_inner);
        match status {
            StreamStatus::GapStart { .. } => {
                gaps.insert(key);
            }
            StreamStatus::Live { .. } => {
                gaps.remove(&key);
            }
            StreamStatus::Stale { .. } => {}
        }
    }
}

/// A [`LiveDataSink`] that records prices into a [`PriceTap`] and forwards everything to `inner`.
pub(super) struct TapSink {
    inner: Arc<dyn LiveDataSink>,
    tap: Arc<PriceTap>,
}

impl TapSink {
    /// Wrap `inner`, recording into `tap`.
    pub(super) fn new(inner: Arc<dyn LiveDataSink>, tap: Arc<PriceTap>) -> Self {
        Self { inner, tap }
    }
}

impl LiveDataSink for TapSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.inner.seed_bars(venue, symbol, interval, bars);
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.close_bar(venue, symbol, interval, bar);
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.forming_bar(venue, symbol, interval, bar);
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.mark_tick(venue, symbol, px, ts);
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.bar_close_tick(venue, symbol, px, ts);
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.inner.l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.tap.note(venue, symbol, quote.mid());
        self.inner.quote(venue, symbol, quote);
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.tap.note(venue, symbol, trade.price);
        self.inner.trade(venue, symbol, trade);
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        if let Some(mid) = book.mid() {
            self.tap.note(venue, symbol, mid);
        }
        self.inner.book(venue, symbol, book);
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        self.inner.book_update(venue, symbol, update);
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.tap.note_status(venue, symbol, stream, status);
        self.inner.stream_status(venue, symbol, stream, status);
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        self.inner.data_delay(venue, symbol, stream, delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Null;
    impl LiveDataSink for Null {
        fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<Bar>) {}
        fn close_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn forming_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
        fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
        fn quote(&self, _: &str, _: &str, _: QuoteTick) {}
        fn trade(&self, _: &str, _: &str, _: TradeTick) {}
        fn book(&self, _: &str, _: &str, _: Arc<L2Book>) {}
    }

    #[test]
    fn a_quote_prices_its_symbol_and_nothing_else() {
        let tap = Arc::new(PriceTap::default());
        let sink = TapSink::new(Arc::new(Null), Arc::clone(&tap));
        assert_eq!(tap.price("bybit", "ETHUSDT"), None);
        let quote = QuoteTick {
            ts: 1,
            local_ts: 0,
            bid: 3000.0,
            ask: 3001.0,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: String::new(),
        };
        sink.quote("bybit", "ETHUSDT", quote);
        assert_eq!(tap.price("bybit", "ETHUSDT"), Some(3000.5));
        assert_eq!(tap.price("bybit", "BTCUSDT"), None);
        assert_eq!(tap.price("binance", "ETHUSDT"), None);
    }

    /// A price older than [`MAX_PRICE_AGE`] is no price (a dead pump must not mark an order), a fresh one
    /// replaces it, and a forgotten symbol starts from none.
    #[test]
    fn a_stale_price_is_no_price_and_a_forgotten_one_is_gone() {
        let tap = PriceTap::default();
        let now = Instant::now();
        let old = now.checked_sub(MAX_PRICE_AGE + Duration::from_secs(1)).expect("an old instant");
        tap.note_at("bybit", "ETHUSDT", 3000.0, old);
        assert_eq!(tap.price_at("bybit", "ETHUSDT", now), None, "older than the limit");
        let edge = now.checked_sub(MAX_PRICE_AGE).expect("an instant");
        tap.note_at("bybit", "ETHUSDT", 3001.0, edge);
        assert_eq!(
            tap.price_at("bybit", "ETHUSDT", now),
            Some(3001.0),
            "exactly at the limit stands"
        );
        tap.note("bybit", "ETHUSDT", 3002.0);
        assert_eq!(tap.price("bybit", "ETHUSDT"), Some(3002.0));
        tap.forget("bybit", "ETHUSDT");
        assert_eq!(tap.price("bybit", "ETHUSDT"), None);
    }

    /// The tap follows the VENUE source's window only: a venue `GapStart` opens it, a venue `Live` closes
    /// it, and the strat link's own word, a `Stale` and another symbol touch nothing.
    #[test]
    fn the_tap_follows_the_venue_sources_gap_window_only() {
        let tap = Arc::new(PriceTap::default());
        let sink = TapSink::new(Arc::new(Null), Arc::clone(&tap));
        let gap = StreamStatus::GapStart { at_ts_ms: 1 };
        let live = StreamStatus::Live { gap_started_ts_ms: Some(1) };
        sink.stream_status("bybit", "ETHUSDT", vike_venues::link_deadman::STRAT_LINK_STREAM, gap);
        assert!(!tap.gap_open("bybit", "ETHUSDT"), "the strat link is another source");
        sink.stream_status("bybit", "ETHUSDT", "book", gap);
        assert!(tap.gap_open("bybit", "ETHUSDT"));
        assert!(!tap.gap_open("bybit", "BTCUSDT"));
        sink.stream_status(
            "bybit",
            "ETHUSDT",
            "book",
            StreamStatus::Stale { newest_data_ts_ms: 1, now_ms: 2 },
        );
        assert!(tap.gap_open("bybit", "ETHUSDT"), "Stale moves nothing");
        sink.stream_status("bybit", "ETHUSDT", vike_venues::link_deadman::STRAT_LINK_STREAM, live);
        assert!(tap.gap_open("bybit", "ETHUSDT"), "the strat link's Live closes no venue gap");
        sink.stream_status("bybit", "ETHUSDT", "book", live);
        assert!(!tap.gap_open("bybit", "ETHUSDT"));
    }
}
