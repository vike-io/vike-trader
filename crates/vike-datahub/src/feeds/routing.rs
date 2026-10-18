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
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use vike_data::{LiveDataSink, StreamLabel};
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

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

#[derive(Default)]
pub(crate) struct RoutingSink {
    md: OnceLock<Arc<dyn LiveDataSink>>,
    rec: OnceLock<Arc<dyn LiveDataSink>>,
    strat: OnceLock<Arc<dyn LiveDataSink>>,
    /// `venue -> symbol -> lane bits` the recorder holds. Two levels so a hot-path lookup takes
    /// borrowed `&str`s and allocates nothing (the reason `crate::md::MdHub::keys` is shaped so).
    rec_keys: RwLock<HashMap<String, HashMap<String, u8>>>,
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
        let bits =
            keys.entry(venue.to_string()).or_default().entry(symbol.to_string()).or_default();
        if on {
            *bits |= lane_bit(lane);
        } else {
            *bits &= !lane_bit(lane);
        }
    }

    pub(crate) fn set_strat(&self, venue: &str, symbol: &str, lane: &BrokerLane, on: bool) {
        let mut keys = self.strat_keys.write().unwrap_or_else(PoisonError::into_inner);
        keys.entry(venue.to_string())
            .or_default()
            .entry(symbol.to_string())
            .or_default()
            .set(lane, on);
    }

    /// The recorder's sink when it holds any lane in `mask` for `(venue, symbol)`.
    fn rec(&self, venue: &str, symbol: &str, mask: u8) -> Option<&Arc<dyn LiveDataSink>> {
        let rec = self.rec.get()?;
        let keys = self.rec_keys.read().unwrap_or_else(PoisonError::into_inner);
        let held = keys.get(venue).and_then(|m| m.get(symbol)).copied().unwrap_or(0);
        ((held & mask) != 0).then_some(rec)
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
        if let Some(strat) = self.strat(venue, symbol, StratKeys::admits_quote) {
            strat.quote(venue, symbol, quote.clone());
        }
        if let Some(rec) =
            self.rec(venue, symbol, lane_bit(StreamLabel::Quotes) | lane_bit(StreamLabel::Book))
        {
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
    // ---- md and the strategy lane: the recorder stores no delay label ----
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_stream_word(stream)) {
            strat.data_delay(venue, symbol, stream, delay);
        }
        if let Some(md) = self.md.get() {
            md.data_delay(venue, symbol, stream, delay);
        }
    }
}
