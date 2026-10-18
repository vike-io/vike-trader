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

use vike_data::{LiveDataSink, StreamLabel};
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
    /// `Some` for a [`Self::real_time_only`] tap: the stream label the venue last sent per `(venue, symbol)`.
    labels: Option<Mutex<HashMap<(String, String), DataDelay>>>,
}

impl PriceTap {
    /// **A tap that prices ONLY from a two-sided quote of a stream the venue labelled REAL-TIME** (ibkr, decision
    /// 0174; the owner's rule for a symbol the engine was not mounted on: a delayed stock quote is not a mark).
    /// A trade print or a folded book never prices here, a one-sided or crossed quote is ignored, and
    /// [`Self::door_price`] REFUSES a delayed or frozen stream by name, answering "none yet" until the stream has
    /// a label at all (so a stream that never labels itself is never a price: fail-closed). Every other tap
    /// prices as it always did.
    pub fn real_time_only() -> Self {
        Self { labels: Some(Mutex::default()), ..Self::default() }
    }

    /// **What the control door asks** ([`crate::feeds::CexWatcher::door_price`]): the latest price, or the
    /// reason there can be none. `Ok(None)` is "none yet" (retry); an `Err` is a refusal in words. An ordinary
    /// tap never refuses.
    ///
    /// # Errors
    /// A [`Self::real_time_only`] tap whose stream for `symbol` is labelled delayed or frozen.
    pub fn door_price(&self, venue: &str, symbol: &str) -> Result<Option<f64>, String> {
        let Some(labels) = &self.labels else { return Ok(self.price(venue, symbol)) };
        let label = labels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string()))
            .copied();
        match label {
            None => Ok(None),
            Some(DataDelay::RealTime) => Ok(self.price(venue, symbol)),
            Some(other) => Err(format!(
                "{venue}: the {symbol} quote stream is {word} and a {word} price is not a mark; the account \
                 needs a real-time market-data entitlement for it",
                word = other.word()
            )),
        }
    }

    /// Whether a trade print or a folded book may price (not on a [`Self::real_time_only`] tap).
    fn marks_from_prints(&self) -> bool {
        self.labels.is_none()
    }

    /// Record one quote: its mid, but on a real-time-only tap only when it is two-sided (finite, a positive bid,
    /// an ask not below it).
    fn note_quote(&self, venue: &str, symbol: &str, quote: &QuoteTick) {
        let two_sided = quote.bid.is_finite()
            && quote.ask.is_finite()
            && quote.bid > 0.0
            && quote.ask >= quote.bid;
        if self.labels.is_none() || two_sided {
            self.note(venue, symbol, quote.mid());
        }
    }

    /// Record the label of a quote stream (real-time-only taps; the first label and every change). A price noted
    /// under another label is forgotten: it is not this label's.
    fn note_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        let Some(labels) = &self.labels else { return };
        if stream != StreamLabel::Quotes.as_str() {
            return;
        }
        let changed = labels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((venue.to_string(), symbol.to_string()), delay)
            != Some(delay);
        if changed {
            self.forget(venue, symbol);
        }
    }

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

    /// The pump of `symbol` was closed: drop its price and any gap it left open (a re-subscribe starts from
    /// none of either; a pump is closed under a gap only where no link dead-man window exists to strand,
    /// `remote_backend::facts`'s `gap_holds_pump`).
    pub fn forget(&self, venue: &str, symbol: &str) {
        let key = (venue.to_string(), symbol.to_string());
        self.last.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        self.gaps.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
    }

    /// Follow the window the core's link dead-man keeps for the venue source: a `GapStart` opens it, a
    /// `Live` closes it, `Stale` moves nothing, and the strat-link and trade-link words are other sources.
    pub(crate) fn note_status(
        &self,
        venue: &str,
        symbol: &str,
        stream: &str,
        status: StreamStatus,
    ) {
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
        self.tap.note_quote(venue, symbol, &quote);
        self.inner.quote(venue, symbol, quote);
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        if self.tap.marks_from_prints() {
            self.tap.note(venue, symbol, trade.price);
        }
        self.inner.trade(venue, symbol, trade);
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        if self.tap.marks_from_prints()
            && let Some(mid) = book.mid()
        {
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
        self.tap.note_delay(venue, symbol, stream, delay);
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

    fn quote_of(bid: f64, ask: f64) -> QuoteTick {
        QuoteTick {
            ts: 1,
            local_ts: 0,
            bid,
            ask,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: String::new(),
        }
    }

    fn print_at(price: f64) -> TradeTick {
        TradeTick {
            ts: 1,
            local_ts: 0,
            price,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        }
    }

    const AAPL: &str = "AAPL.SMART.USD";

    fn label(sink: &TapSink, delay: DataDelay) {
        sink.data_delay("ibkr", AAPL, StreamLabel::Quotes.as_str(), delay);
    }

    /// **Only a two-sided quote of a stream labelled REAL-TIME prices** on a real-time-only tap (ibkr): the mid,
    /// read back through the door's `door_price`.
    #[test]
    fn a_real_time_two_sided_quote_prices_a_real_time_only_tap() {
        let tap = Arc::new(PriceTap::real_time_only());
        let sink = TapSink::new(Arc::new(Null), Arc::clone(&tap));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "nothing yet");
        label(&sink, DataDelay::RealTime);
        sink.quote("ibkr", AAPL, quote_of(149.98, 150.02));
        let px = tap.door_price("ibkr", AAPL).expect("a real-time stream").expect("one arrived");
        assert!((px - 150.0).abs() < 1e-9, "{px}");
        assert_eq!(tap.door_price("ibkr", "TSLA.SMART.USD"), Ok(None), "another symbol has none");
    }

    /// **A delayed or frozen stream is refused BY NAME** and a stream with no label at all is "none yet": the
    /// quote is not a mark, however two-sided.
    #[test]
    fn a_delayed_frozen_or_unlabelled_stream_is_refused_or_unpriced() {
        let tap = Arc::new(PriceTap::real_time_only());
        let sink = TapSink::new(Arc::new(Null), Arc::clone(&tap));
        sink.quote("ibkr", AAPL, quote_of(149.0, 151.0));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "no label yet: not a price");

        label(&sink, DataDelay::DELAYED);
        sink.quote("ibkr", AAPL, quote_of(149.0, 151.0));
        let why = tap.door_price("ibkr", AAPL).expect_err("delayed");
        assert!(
            why.contains("delayed") && why.contains("entitlement") && why.contains(AAPL),
            "{why}"
        );

        label(&sink, DataDelay::Frozen);
        sink.quote("ibkr", AAPL, quote_of(149.0, 151.0));
        assert!(tap.door_price("ibkr", AAPL).expect_err("frozen").contains("frozen"));
    }

    /// A one-sided, crossed or non-finite quote is ignored, and a trade print or a folded book never prices.
    #[test]
    fn a_real_time_only_tap_ignores_what_is_not_a_two_sided_quote() {
        let tap = Arc::new(PriceTap::real_time_only());
        let sink = TapSink::new(Arc::new(Null), Arc::clone(&tap));
        label(&sink, DataDelay::RealTime);
        for (bid, ask) in
            [(0.0, 151.0), (149.0, 0.0), (151.0, 149.0), (f64::NAN, 151.0), (-1.0, -1.0)]
        {
            sink.quote("ibkr", AAPL, quote_of(bid, ask));
            assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "{bid}/{ask}");
        }
        sink.trade("ibkr", AAPL, print_at(150.0));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "a last trade is not a mark here");
    }

    /// A price noted under one label is forgotten when the label changes: a delayed price must not be served
    /// as real-time the moment the stream is relabelled (nor a real-time one as a mark while delayed).
    #[test]
    fn a_relabelled_stream_forgets_the_price_of_the_other_label() {
        let tap = Arc::new(PriceTap::real_time_only());
        let sink = TapSink::new(Arc::new(Null), Arc::clone(&tap));
        label(&sink, DataDelay::DELAYED);
        sink.quote("ibkr", AAPL, quote_of(149.0, 151.0));
        label(&sink, DataDelay::RealTime);
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(None), "the delayed price is gone");
        sink.quote("ibkr", AAPL, quote_of(150.0, 152.0));
        assert_eq!(tap.door_price("ibkr", AAPL), Ok(Some(151.0)));
        label(&sink, DataDelay::DELAYED);
        assert!(tap.door_price("ibkr", AAPL).is_err(), "relabelled delayed: refused again");
        // A label of another lane (trades) says nothing about the quote stream.
        sink.data_delay("ibkr", AAPL, "trades", DataDelay::RealTime);
        assert!(tap.door_price("ibkr", AAPL).is_err());
    }

    /// An ordinary tap is untouched by labels and never refuses: it prices as it always did (a trade prices).
    #[test]
    fn an_ordinary_tap_ignores_labels_and_never_refuses() {
        let tap = Arc::new(PriceTap::default());
        let sink = TapSink::new(Arc::new(Null), Arc::clone(&tap));
        sink.data_delay("bybit", "ETHUSDT", StreamLabel::Quotes.as_str(), DataDelay::DELAYED);
        sink.quote("bybit", "ETHUSDT", quote_of(0.0, 3001.0));
        assert_eq!(
            tap.door_price("bybit", "ETHUSDT"),
            Ok(Some(1500.5)),
            "its quote rule is unchanged"
        );
        sink.trade("bybit", "ETHUSDT", print_at(3002.0));
        assert_eq!(tap.door_price("bybit", "ETHUSDT"), Ok(Some(3002.0)));
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
