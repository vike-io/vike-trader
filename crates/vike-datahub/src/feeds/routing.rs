//! `RoutingSink` — the ONE sink every `Shared` venue client is built with. The strategy lane is called
//! FIRST (a lock-and-enqueue and nothing slower: the tradehub's prices are the latency-critical
//! consumer, and this sink calls its holders serially from one reader thread, so whatever sits ahead of
//! a holder delays it), then md, which gets every verb (its `crate::md::sink::MdHubSink` looks each key
//! up itself), then the recorder, which gets exactly the keys it holds. Every verb is spelled out, never
//! inherited — the omission `TeeSink`'s comments record.
//!
//! # What each holder is admitted to
//!
//! * **md** is called for every verb and decides by its own key table. It never subscribes a quote
//!   lane (`MdLane` has none), so a quote that reaches it is for a key it does not hold and is dropped
//!   there, as it always was.
//! * **the recorder** is admitted by `rec_keys`, a `venue -> symbol -> lane bits` set the broker
//!   publishes BEFORE the real subscribe.
//! * **the strategy lane** is admitted by `strat_keys`, the same shape plus the bar intervals it holds
//!   ([`StratKeys`]), published by the broker the same way. It is the only holder that is also handed a
//!   status or a delay word that is a BAR INTERVAL, and the kline lane's two per-symbol ticks
//!   (`mark_tick`, `bar_close_tick`) when it holds any bar lane of the symbol.
//!
//! # What the recorder is NOT handed: a stream labelled delayed or frozen (decision 0193 verdict 7)
//!
//! The recorder stores no delay label, so a stream whose LAST label is `Delayed` or `Frozen` would be
//! written as if it were real time (IBKR's demo stock quotes are delayed). The sink's `data_delay`
//! therefore records, per `(venue, symbol, lane)`, that the recorder is to be withheld the lane's frames
//! until a `RealTime` label arrives. The strategy lane and md are untouched: the label is information
//! for them, never a gate (`vike_model::DataDelay`). The state lives in the recorder's own key entry
//! (`RecKey`), so the check is the lookup the recorder's admission already makes, and it is kept
//! whether or not the recorder holds the key yet: a label is sent when it is first known and when it
//! changes, never again for a holder that joins later.
#[cfg(feature = "record")]
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use vike_data::{LiveDataSink, StreamLabel};
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus, now_ms};

use super::{BrokerLane, Holder, lane_bit};

/// What ONE strategy-lane view holds of one `(venue, symbol)`: its tick lanes (the broker's filter
/// bits) and its bar intervals. The shape the broker publishes for the lane as a whole and each strat
/// SESSION keeps for itself (`crate::strat`), so one set of admission rules answers both.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct StratKeys {
    lanes: u8,
    bars: Vec<String>,
}

impl StratKeys {
    /// Nothing is held: the key can be forgotten.
    pub(crate) fn is_empty(&self) -> bool {
        self.lanes == 0 && self.bars.is_empty()
    }

    /// Mark `lane` held (`on`) or released.
    pub(crate) fn set(&mut self, lane: &BrokerLane, on: bool) {
        match lane {
            BrokerLane::Tick(label) => {
                if on {
                    self.lanes |= lane_bit(*label);
                } else {
                    self.lanes &= !lane_bit(*label);
                }
            }
            BrokerLane::Bars(interval) => {
                let held = self.bars.iter().position(|b| b == interval);
                match (on, held) {
                    (true, None) => self.bars.push(interval.clone()),
                    (false, Some(i)) => {
                        self.bars.swap_remove(i);
                    }
                    (true, Some(_)) | (false, None) => {}
                }
            }
        }
    }

    fn holds(&self, label: StreamLabel) -> bool {
        self.lanes & lane_bit(label) != 0
    }

    /// A quote is admitted under `Quotes` OR `Book`: a book pump may derive L1 (polymarket's
    /// `PumpMode::Book`), so a book subscription's quotes are its own. The recorder's rule, and the
    /// lane's.
    pub(crate) fn admits_quote(&self) -> bool {
        self.lanes & (lane_bit(StreamLabel::Quotes) | lane_bit(StreamLabel::Book)) != 0
    }

    pub(crate) fn admits_trade(&self) -> bool {
        self.holds(StreamLabel::Trades)
    }

    /// Both the folded book and the raw delta ride the `Book` lane.
    pub(crate) fn admits_book(&self) -> bool {
        self.holds(StreamLabel::Book)
    }

    pub(crate) fn admits_depth(&self) -> bool {
        self.holds(StreamLabel::Depth)
    }

    /// A bar verb of `interval`.
    pub(crate) fn admits_interval(&self, interval: &str) -> bool {
        self.bars.iter().any(|b| b == interval)
    }

    /// The kline lane's per-symbol ticks (`mark_tick`, `bar_close_tick`) carry no interval: they
    /// belong to any holder of a bar lane of the symbol.
    pub(crate) fn admits_bar_tick(&self) -> bool {
        !self.bars.is_empty()
    }

    /// A status or delay for `stream`: a tick lane's label word is admitted when the lane is held, a
    /// bar interval when it is held, and any other word (the pump's link word) is not the broker's.
    pub(crate) fn admits_stream_word(&self, stream: &str) -> bool {
        match StreamLabel::from_label(stream) {
            Some(label) => self.holds(label),
            None => self.admits_interval(stream),
        }
    }
}

/// What the recorder is admitted to for one `(venue, symbol)`, as [`lane_bit`] sets.
#[derive(Debug, Default)]
struct RecKey {
    /// The tick lanes the recorder HOLDS.
    held: u8,
    /// The tick lanes whose last delay label is not real time: the recorder is withheld their frames.
    withheld: u8,
    /// The wall-clock millisecond of the last QUOTE that reached the recorder while no strategy took it
    /// (`RoutingSink::quote`'s recorder-only branch, and nowhere else: the strategy's path writes nothing),
    /// or the instant the strategy lane let the key go to the recorder alone; `0` for a key that has not
    /// had one. A strategy that joins a key only the recorder holds asks whether it has gone quiet
    /// ([`RoutingSink::rec_quote_quiet`], decision 0193).
    last_quote_ms: AtomicI64,
}

impl RecKey {
    /// The lanes of `mask` the recorder holds and is not withheld.
    fn admits(&self, mask: u8) -> bool {
        (self.held & !self.withheld & mask) != 0
    }
}

#[derive(Default)]
pub(crate) struct RoutingSink {
    md: OnceLock<Arc<dyn LiveDataSink>>,
    rec: OnceLock<Arc<dyn LiveDataSink>>,
    strat: OnceLock<Arc<dyn LiveDataSink>>,
    /// `venue -> symbol -> what the recorder holds and is withheld`. Two levels so a hot-path lookup
    /// takes borrowed `&str`s and allocates nothing (the reason `crate::md::MdHub::keys` is shaped so).
    rec_keys: RwLock<HashMap<String, HashMap<String, RecKey>>>,
    /// `venue -> symbol -> what the strategy lane holds`, shaped for the same borrowed lookup.
    strat_keys: RwLock<HashMap<String, HashMap<String, StratKeys>>>,
}

impl RoutingSink {
    pub(crate) fn attach(&self, holder: Holder, sink: Arc<dyn LiveDataSink>) {
        let slot = match holder {
            Holder::Md => &self.md,
            Holder::Rec => &self.rec,
            Holder::Strat => &self.strat,
        };
        let _ = slot.set(sink);
    }

    pub(crate) fn sink_of(&self, holder: Holder) -> Option<Arc<dyn LiveDataSink>> {
        match holder {
            Holder::Md => self.md.get().cloned(),
            Holder::Rec => self.rec.get().cloned(),
            Holder::Strat => self.strat.get().cloned(),
        }
    }

    pub(crate) fn set_rec(&self, venue: &str, symbol: &str, lane: StreamLabel, on: bool) {
        let mut keys = self.rec_keys.write().unwrap_or_else(PoisonError::into_inner);
        let key = keys.entry(venue.to_string()).or_default().entry(symbol.to_string()).or_default();
        if on {
            key.held |= lane_bit(lane);
        } else {
            key.held &= !lane_bit(lane);
        }
    }

    /// Record that `lane` of `(venue, symbol)` is (`withheld`) or is no longer labelled delayed or
    /// frozen, whether or not the recorder holds the key.
    fn set_rec_withheld(&self, venue: &str, symbol: &str, lane: StreamLabel, withheld: bool) {
        // Read first: a label that changes nothing (the usual one, a real-time label for a lane never
        // withheld) must not take the write lock the hot path's `rec` lookups wait on.
        let was = self
            .rec_keys
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(venue)
            .and_then(|m| m.get(symbol))
            .is_some_and(|k| k.withheld & lane_bit(lane) != 0);
        if withheld == was {
            return;
        }
        let mut keys = self.rec_keys.write().unwrap_or_else(PoisonError::into_inner);
        let key = keys.entry(venue.to_string()).or_default().entry(symbol.to_string()).or_default();
        if withheld {
            key.withheld |= lane_bit(lane);
        } else {
            key.withheld &= !lane_bit(lane);
        }
    }

    /// The symbols the strategy lane holds `venue` in tick lane `label` for: the key set its frames are
    /// admitted by, read for the recorder's "held by strategies" resolver.
    #[cfg(feature = "record")]
    pub(crate) fn strat_symbols(&self, venue: &str, label: StreamLabel) -> BTreeSet<String> {
        let keys = self.strat_keys.read().unwrap_or_else(PoisonError::into_inner);
        keys.get(venue)
            .map(|m| {
                m.iter().filter(|(_, held)| held.holds(label)).map(|(s, _)| s.clone()).collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn set_strat(&self, venue: &str, symbol: &str, lane: &BrokerLane, on: bool) {
        let mut keys = self.strat_keys.write().unwrap_or_else(PoisonError::into_inner);
        keys.entry(venue.to_string())
            .or_default()
            .entry(symbol.to_string())
            .or_default()
            .set(lane, on);
    }

    /// The recorder's sink when it holds any lane in `mask` for `(venue, symbol)` and is not withheld it.
    fn rec(&self, venue: &str, symbol: &str, mask: u8) -> Option<&Arc<dyn LiveDataSink>> {
        let rec = self.rec.get()?;
        let keys = self.rec_keys.read().unwrap_or_else(PoisonError::into_inner);
        let key = keys.get(venue).and_then(|m| m.get(symbol))?;
        key.admits(mask).then_some(rec)
    }

    /// [`Self::rec`] for a QUOTE, which also writes the key's quote clock when no strategy took the quote
    /// (`strategy_took_it` false) and the venue is one whose first price comes with a new stream: one relaxed
    /// store, on the recorder-only branch alone, so the strategy lane's path pays nothing for the question a
    /// joining strategy asks ([`Self::rec_quote_quiet`]) and no other venue pays the clock read.
    fn rec_quote(
        &self,
        venue: &str,
        symbol: &str,
        mask: u8,
        strategy_took_it: bool,
    ) -> Option<&Arc<dyn LiveDataSink>> {
        let rec = self.rec.get()?;
        let keys = self.rec_keys.read().unwrap_or_else(PoisonError::into_inner);
        let key = keys.get(venue).and_then(|m| m.get(symbol))?;
        if !key.admits(mask) {
            return None;
        }
        // Only a venue whose first price comes with a new stream is ever asked the question the clock answers.
        if !strategy_took_it && super::venues::first_price_comes_with_a_new_stream(venue) {
            key.last_quote_ms.store(now_ms(), Ordering::Relaxed);
        }
        Some(rec)
    }

    /// Write the quote clock of the recorder's hold of `(venue, symbol)` to `at_ms`: the strategy lane has just
    /// let the key go to the recorder alone, and the key was live up to this instant. A key the recorder does
    /// not hold has no clock to write.
    pub(crate) fn stamp_rec_quote(&self, venue: &str, symbol: &str, at_ms: i64) {
        let keys = self.rec_keys.read().unwrap_or_else(PoisonError::into_inner);
        if let Some(key) = keys.get(venue).and_then(|m| m.get(symbol)) {
            key.last_quote_ms.store(at_ms, Ordering::Relaxed);
        }
    }

    /// Whether the recorder's hold of `(venue, symbol)` has had NO quote for more than `quiet_ms` as of `now`:
    /// no strategy took the last one (or the key was handed to the recorder) longer ago than that. A key that has
    /// never had one, or that the recorder does not hold, is quiet.
    pub(crate) fn rec_quote_quiet(
        &self,
        venue: &str,
        symbol: &str,
        now: i64,
        quiet_ms: i64,
    ) -> bool {
        let keys = self.rec_keys.read().unwrap_or_else(PoisonError::into_inner);
        keys.get(venue).and_then(|m| m.get(symbol)).is_none_or(|key| {
            now.saturating_sub(key.last_quote_ms.load(Ordering::Relaxed)) > quiet_ms
        })
    }

    /// The strategy lane's sink when `admits` says its view of `(venue, symbol)` takes the frame.
    fn strat(
        &self,
        venue: &str,
        symbol: &str,
        admits: impl FnOnce(&StratKeys) -> bool,
    ) -> Option<&Arc<dyn LiveDataSink>> {
        let strat = self.strat.get()?;
        let keys = self.strat_keys.read().unwrap_or_else(PoisonError::into_inner);
        keys.get(venue).and_then(|m| m.get(symbol)).filter(|held| admits(held)).map(|_| strat)
    }
}

impl LiveDataSink for RoutingSink {
    // ---- md, and the strategy lane when it holds a bar lane of the key: no plane RECORDS bars or
    // marks through the broker ----
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_interval(interval)) {
            strat.seed_bars(venue, symbol, interval, bars.clone());
        }
        if let Some(md) = self.md.get() {
            md.seed_bars(venue, symbol, interval, bars);
        }
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_interval(interval)) {
            strat.close_bar(venue, symbol, interval, bar.clone());
        }
        if let Some(md) = self.md.get() {
            md.close_bar(venue, symbol, interval, bar);
        }
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_interval(interval)) {
            strat.forming_bar(venue, symbol, interval, bar.clone());
        }
        if let Some(md) = self.md.get() {
            md.forming_bar(venue, symbol, interval, bar);
        }
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        if let Some(strat) = self.strat(venue, symbol, StratKeys::admits_bar_tick) {
            strat.mark_tick(venue, symbol, px, ts);
        }
        if let Some(md) = self.md.get() {
            md.mark_tick(venue, symbol, px, ts);
        }
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        if let Some(strat) = self.strat(venue, symbol, StratKeys::admits_bar_tick) {
            strat.bar_close_tick(venue, symbol, px, ts);
        }
        if let Some(md) = self.md.get() {
            md.bar_close_tick(venue, symbol, px, ts);
        }
    }
    // ---- the holders of a tick lane ----
    /// Admitted to the recorder and the strategy lane under `Quotes` OR `Book`: a book pump may derive
    /// L1 (polymarket's `PumpMode::Book`), so a quote can ride a book subscription. md is called for
    /// every quote and drops the ones it holds no key for: it has no quotes lane (`MdLane` has none),
    /// so no subscription of its own can emit one — the wording that used to read "md never subscribes
    /// quotes" described md's table, and the broker no longer relies on it: the strategy lane DOES
    /// subscribe quotes, and its key set, not md's silence, is what admits it.
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        let strat = self.strat(venue, symbol, StratKeys::admits_quote);
        if let Some(strat) = strat {
            strat.quote(venue, symbol, quote.clone());
        }
        // The recorder-only branch also writes the key's quote clock (`RecKey::last_quote_ms`); a quote a
        // strategy took writes nothing.
        if let Some(rec) = self.rec_quote(
            venue,
            symbol,
            lane_bit(StreamLabel::Quotes) | lane_bit(StreamLabel::Book),
            strat.is_some(),
        ) {
            rec.quote(venue, symbol, quote.clone());
        }
        if let Some(md) = self.md.get() {
            md.quote(venue, symbol, quote);
        }
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        if let Some(strat) = self.strat(venue, symbol, StratKeys::admits_trade) {
            strat.trade(venue, symbol, trade.clone());
        }
        if let Some(rec) = self.rec(venue, symbol, lane_bit(StreamLabel::Trades)) {
            rec.trade(venue, symbol, trade.clone());
        }
        if let Some(md) = self.md.get() {
            md.trade(venue, symbol, trade);
        }
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        if let Some(strat) = self.strat(venue, symbol, StratKeys::admits_book) {
            strat.book(venue, symbol, Arc::clone(&book));
        }
        if let Some(rec) = self.rec(venue, symbol, lane_bit(StreamLabel::Book)) {
            rec.book(venue, symbol, Arc::clone(&book));
        }
        if let Some(md) = self.md.get() {
            md.book(venue, symbol, book);
        }
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        if let Some(strat) = self.strat(venue, symbol, StratKeys::admits_book) {
            strat.book_update(venue, symbol, update.clone());
        }
        if let Some(rec) = self.rec(venue, symbol, lane_bit(StreamLabel::Book)) {
            rec.book_update(venue, symbol, update.clone());
        }
        if let Some(md) = self.md.get() {
            md.book_update(venue, symbol, update);
        }
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
        if let Some(strat) = self.strat(venue, symbol, StratKeys::admits_depth) {
            strat.l2_snapshot(venue, symbol, tick_size, bids.clone(), asks.clone(), ts);
        }
        if let Some(rec) = self.rec(venue, symbol, lane_bit(StreamLabel::Depth)) {
            rec.l2_snapshot(venue, symbol, tick_size, bids.clone(), asks.clone(), ts);
        }
        if let Some(md) = self.md.get() {
            md.l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
        }
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_stream_word(stream)) {
            strat.stream_status(venue, symbol, stream, status);
        }
        if let Some(lane) = StreamLabel::from_label(stream)
            && let Some(rec) = self.rec(venue, symbol, lane_bit(lane))
        {
            rec.stream_status(venue, symbol, stream, status);
        }
        if let Some(md) = self.md.get() {
            md.stream_status(venue, symbol, stream, status);
        }
    }
    // ---- md and the strategy lane: the recorder stores no delay label, so it is WITHHELD a stream
    // that is not real time instead (the module doc) ----
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_stream_word(stream)) {
            strat.data_delay(venue, symbol, stream, delay);
        }
        if let Some(lane) = StreamLabel::from_label(stream) {
            self.set_rec_withheld(venue, symbol, lane, !delay.is_real_time());
        }
        if let Some(md) = self.md.get() {
            md.data_delay(venue, symbol, stream, delay);
        }
    }
}

#[cfg(test)]
mod tests {
    use vike_data::RecordingSink;

    use super::*;

    fn quote() -> QuoteTick {
        QuoteTick {
            ts: 1,
            local_ts: 0,
            bid: 1.0,
            ask: 2.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        }
    }

    fn rec_holds_eurusd() -> RoutingSink {
        let routing = RoutingSink::default();
        routing.attach(Holder::Rec, Arc::new(RecordingSink::default()));
        routing.attach(Holder::Strat, Arc::new(RecordingSink::default()));
        routing.set_rec("oanda", "EURUSD", StreamLabel::Quotes, true);
        routing
    }

    /// The quiet window is judged from the key's quote clock: a key that never had a quote is quiet, one that
    /// had one is quiet only once MORE than the window has passed, and a key the recorder does not hold has no
    /// clock to write and is quiet.
    #[test]
    fn a_recorder_held_key_is_quiet_before_its_first_quote_and_after_the_window() {
        let routing = rec_holds_eurusd();
        assert!(routing.rec_quote_quiet("oanda", "EURUSD", 10_000, 3_000), "no quote yet");
        routing.stamp_rec_quote("oanda", "EURUSD", 10_000);
        assert!(!routing.rec_quote_quiet("oanda", "EURUSD", 10_000, 3_000));
        assert!(!routing.rec_quote_quiet("oanda", "EURUSD", 13_000, 3_000), "exactly the window");
        assert!(routing.rec_quote_quiet("oanda", "EURUSD", 13_001, 3_000), "past it");

        routing.stamp_rec_quote("oanda", "GBPUSD", 10_000);
        assert!(routing.rec_quote_quiet("oanda", "GBPUSD", 10_000, 3_000), "not held: no clock");
    }

    /// A venue nobody asks the question of keeps no clock: the store reads the wall clock on every
    /// recorder-only quote (20 to 30 ns on the recorder's leg), so only a venue whose first price comes with
    /// a new stream pays it.
    #[test]
    fn a_venue_whose_first_price_does_not_wait_for_a_new_stream_keeps_no_quote_clock() {
        let routing = RoutingSink::default();
        routing.attach(Holder::Rec, Arc::new(RecordingSink::default()));
        routing.set_rec("binance", "BTCUSDT", StreamLabel::Quotes, true);
        routing.quote("binance", "BTCUSDT", quote());
        assert!(
            routing.rec_quote_quiet("binance", "BTCUSDT", now_ms(), 3_000),
            "a recorder-only quote of a venue nobody asks about wrote a clock"
        );
    }

    /// **Only the recorder-only branch of a quote writes the clock**: a quote the strategy lane takes writes
    /// nothing (the strategy's path gains no store), and the same quote with no strategy holding the key does.
    #[test]
    fn only_a_quote_no_strategy_took_writes_the_clock() {
        let routing = rec_holds_eurusd();
        routing.set_strat("oanda", "EURUSD", &BrokerLane::Tick(StreamLabel::Quotes), true);
        routing.quote("oanda", "EURUSD", quote());
        assert!(
            routing.rec_quote_quiet("oanda", "EURUSD", now_ms(), 3_000),
            "a quote the strategy took must leave the clock alone"
        );

        routing.set_strat("oanda", "EURUSD", &BrokerLane::Tick(StreamLabel::Quotes), false);
        routing.quote("oanda", "EURUSD", quote());
        assert!(
            !routing.rec_quote_quiet("oanda", "EURUSD", now_ms(), 3_000),
            "a quote only the recorder took writes it"
        );
    }
}
