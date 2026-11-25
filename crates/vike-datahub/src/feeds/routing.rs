//! `DataClientSinkRouting` — the ONE sink every `Shared` venue client is built with. The strategy lane is called
//! FIRST (a lock-and-enqueue and nothing slower: the tradehub's prices are the latency-critical
//! consumer, and this sink calls its holders serially from one reader thread, so whatever sits ahead of
//! a holder delays it), then the recorder, which gets exactly the keys it holds, then market data, which gets
//! every verb (its `crate::market_data::sink::DataClientSinkMarketDataHub` looks each key up itself). Every verb is spelled out, never
//! inherited — the omission `DataClientSinkTee`'s comments record.
//!
//! # What each holder is admitted to
//!
//! * **market data** is called for every verb and decides by its own key table, called last, so a quote lane it
//!   holds delays no other holder's quote.
//! * **the recorder** is admitted by `rec_keys`, a `venue -> symbol -> lane bits` set the broker
//!   publishes BEFORE the real subscribe. A key is the instrument's USER name, and the recorder is handed its
//!   STORE name (`vike_paths::store_path::store_name_of_user_name`, decision 0290: `XYZ:TSLAUSDC.P` is `XYZ-TSLAUSDC.P`), the
//!   name its series is filed under.
//! * **the strategy lane** is admitted by `strategy_keys`, the same shape plus the bar intervals it holds
//!   ([`DataClientBrokerStrategyKeys`]), published by the broker the same way. It is the only holder that is also handed a
//!   status or a delay word that is a BAR INTERVAL, and the kline lane's two per-symbol ticks
//!   (`mark_tick`, `bar_close_tick`) when it holds any bar lane of the symbol.
//!
//! **A tape hole ([`TAPE_HOLE_WORD`]) goes to the recorder and nowhere else.** The word exists so that a venue can
//! say its socket is down without telling a strategy, which is what `quotes` would do: past the strategy lane
//! that is the venue's own socket to the link dead-man, and a venue whose row arms it would cancel and HALT.
//!
//! # What the recorder is NOT handed: a stream labelled delayed or frozen (decision 0193 verdict 7)
//!
//! The recorder stores no delay label, so a stream whose LAST label is `Delayed` or `Frozen` would be
//! written as if it were real time (IBKR's demo stock quotes are delayed). The sink's `data_delay`
//! therefore records, per `(venue, symbol, lane)`, that the recorder is to be withheld the lane's frames
//! until a `RealTime` label arrives. The strategy lane and market data are untouched: the label is information
//! for them, never a gate (`vike_model::StreamDelay`). The state lives in the recorder's own key entry
//! (`RecKey`), so the check is the lookup the recorder's admission already makes, and it is kept
//! whether or not the recorder holds the key yet: a label is sent when it is first known and when it
//! changes, never again for a holder that joins later.
#[cfg(feature = "record")]
use std::collections::BTreeSet;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

use vike_data::DataClientSink;
use vike_gatewayhub_client::credentialed_limits::first_price_comes_with_a_new_stream;
use vike_marketdata::{Bar, L2Book, L2BookLevel, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamStatus, now_ms};
use vike_model::{StreamLabel, TAPE_HOLE_WORD};

use vike_paths::store_path::store_name_of_user_name;

use super::{DataClientBrokerHolder, DataClientBrokerLane, lane_bit};

/// What ONE strategy-lane view holds of one `(venue, symbol)`: its tick lanes (the broker's filter
/// bits) and its bar intervals. The shape the broker publishes for the lane as a whole and each strat
/// SESSION keeps for itself (`crate::strat`), so one set of admission rules answers both.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct DataClientBrokerStrategyKeys {
    lanes: u8,
    bars: Vec<String>,
}

impl DataClientBrokerStrategyKeys {
    /// Nothing is held: the key can be forgotten.
    pub(crate) fn is_empty(&self) -> bool {
        self.lanes == 0 && self.bars.is_empty()
    }

    /// Mark `lane` held (`on`) or released.
    pub(crate) fn set(&mut self, lane: &DataClientBrokerLane, on: bool) {
        match lane {
            DataClientBrokerLane::Tick(label) => {
                if on {
                    self.lanes |= lane_bit(*label);
                } else {
                    self.lanes &= !lane_bit(*label);
                }
            }
            DataClientBrokerLane::Bars(interval) => {
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
    /// `FeedLane::Book`), so a book subscription's quotes are its own. The recorder's rule, and the
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
        match StreamLabel::from_word(stream) {
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
    /// (`DataClientSinkRouting::quote`'s recorder-only branch, and nowhere else: the strategy's path writes nothing),
    /// or the instant the strategy lane let the key go to the recorder alone; `0` for a key that has not
    /// had one. A strategy that joins a key only the recorder holds asks whether it has gone quiet
    /// ([`DataClientSinkRouting::rec_quote_quiet`], decision 0193).
    last_quote_ms: AtomicI64,
}

impl RecKey {
    /// The lanes of `mask` the recorder holds and is not withheld.
    fn admits(&self, mask: u8) -> bool {
        (self.held & !self.withheld & mask) != 0
    }
}

#[derive(Default)]
pub(crate) struct DataClientSinkRouting {
    market_data: OnceLock<Arc<dyn DataClientSink>>,
    rec: OnceLock<Arc<dyn DataClientSink>>,
    strat: OnceLock<Arc<dyn DataClientSink>>,
    /// `venue -> symbol -> what the recorder holds and is withheld`. Two levels so a hot-path lookup
    /// takes borrowed `&str`s and allocates nothing (the reason `crate::market_data::MarketDataHub::keys` is shaped so).
    rec_keys: RwLock<HashMap<String, HashMap<String, RecKey>>>,
    /// `venue -> symbol -> what the strategy lane holds`, shaped for the same borrowed lookup.
    strategy_keys: RwLock<HashMap<String, HashMap<String, DataClientBrokerStrategyKeys>>>,
    /// The keys a `config.datahub_live_resident` row names: market data's hold of one is the operator's own
    /// (`FeedBroker::pin_market_data_key`, rule nine). Written at the plane's start, read per attach or release.
    market_data_pins: RwLock<HashSet<(String, String, StreamLabel)>>,
    /// The last delay label of each key, for a window that attaches after it was said: a label is sent
    /// when it is first known and when it changes, never again for a holder that joins later. Touched
    /// by `data_delay` and by an attach, never by a quote.
    market_data_delays: Mutex<HashMap<(String, String, StreamLabel), StreamDelay>>,
}

impl DataClientSinkRouting {
    pub(crate) fn attach(&self, holder: DataClientBrokerHolder, sink: Arc<dyn DataClientSink>) {
        let slot = match holder {
            DataClientBrokerHolder::MarketData => &self.market_data,
            DataClientBrokerHolder::Rec => &self.rec,
            DataClientBrokerHolder::Strategy => &self.strat,
        };
        let _ = slot.set(sink);
    }

    pub(crate) fn sink_of(
        &self,
        holder: DataClientBrokerHolder,
    ) -> Option<Arc<dyn DataClientSink>> {
        match holder {
            DataClientBrokerHolder::MarketData => self.market_data.get().cloned(),
            DataClientBrokerHolder::Rec => self.rec.get().cloned(),
            DataClientBrokerHolder::Strategy => self.strat.get().cloned(),
        }
    }

    /// Whether an operator-owned holder holds this key right now (rule nine): the strategy lane, the
    /// recorder or a resident row. Reads the key sets alone and never the broker's venue lock, which a
    /// credentialed subscribe holds for seconds, so a `MarketDataSubscribe` reply waits on no venue.
    pub(crate) fn operator_holds(&self, venue: &str, symbol: &str, label: StreamLabel) -> bool {
        self.market_data_pinned(venue, symbol, label)
            || self
                .strategy_keys
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .get(venue)
                .and_then(|m| m.get(symbol))
                .is_some_and(|k| k.holds(label))
            || self
                .rec_keys
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .get(venue)
                .and_then(|m| m.get(symbol))
                .is_some_and(|k| k.held & lane_bit(label) != 0)
    }

    pub(crate) fn pin_market_data(&self, venue: &str, symbol: &str, label: StreamLabel) {
        self.market_data_pins.write().unwrap_or_else(PoisonError::into_inner).insert((
            venue.to_string(),
            symbol.to_string(),
            label,
        ));
    }

    pub(crate) fn market_data_pinned(&self, venue: &str, symbol: &str, label: StreamLabel) -> bool {
        self.market_data_pins.read().unwrap_or_else(PoisonError::into_inner).contains(&(
            venue.to_string(),
            symbol.to_string(),
            label,
        ))
    }

    /// Tell market data the status of a key whose stream the broker itself opened or closed under it.
    pub(crate) fn market_data_status(
        &self,
        venue: &str,
        symbol: &str,
        stream: &str,
        status: StreamStatus,
    ) {
        if let Some(market_data) = self.market_data.get() {
            market_data.stream_status(venue, symbol, stream, status);
        }
    }

    /// Tell market data the delay label this key's stream last said, if it said one: market data has just joined.
    pub(crate) fn restate_delay_to_market_data(
        &self,
        venue: &str,
        symbol: &str,
        label: StreamLabel,
    ) {
        let delay = self
            .market_data_delays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(venue.to_string(), symbol.to_string(), label))
            .copied();
        if let (Some(delay), Some(market_data)) = (delay, self.market_data.get()) {
            market_data.data_delay(venue, symbol, label.word(), delay);
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
    pub(crate) fn strategy_symbols(&self, venue: &str, label: StreamLabel) -> BTreeSet<String> {
        let keys = self.strategy_keys.read().unwrap_or_else(PoisonError::into_inner);
        keys.get(venue)
            .map(|m| {
                m.iter().filter(|(_, held)| held.holds(label)).map(|(s, _)| s.clone()).collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn set_strategy(
        &self,
        venue: &str,
        symbol: &str,
        lane: &DataClientBrokerLane,
        on: bool,
    ) {
        let mut keys = self.strategy_keys.write().unwrap_or_else(PoisonError::into_inner);
        keys.entry(venue.to_string())
            .or_default()
            .entry(symbol.to_string())
            .or_default()
            .set(lane, on);
    }

    /// The recorder's sink when it holds any lane in `mask` for `(venue, symbol)` and is not withheld it.
    fn rec(&self, venue: &str, symbol: &str, mask: u8) -> Option<&Arc<dyn DataClientSink>> {
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
    ) -> Option<&Arc<dyn DataClientSink>> {
        let rec = self.rec.get()?;
        let keys = self.rec_keys.read().unwrap_or_else(PoisonError::into_inner);
        let key = keys.get(venue).and_then(|m| m.get(symbol))?;
        if !key.admits(mask) {
            return None;
        }
        // Only a venue whose first price comes with a new stream is ever asked the question the clock answers.
        if !strategy_took_it && first_price_comes_with_a_new_stream(venue) {
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
        admits: impl FnOnce(&DataClientBrokerStrategyKeys) -> bool,
    ) -> Option<&Arc<dyn DataClientSink>> {
        let strat = self.strat.get()?;
        let keys = self.strategy_keys.read().unwrap_or_else(PoisonError::into_inner);
        keys.get(venue).and_then(|m| m.get(symbol)).filter(|held| admits(held)).map(|_| strat)
    }
}

impl DataClientSink for DataClientSinkRouting {
    // ---- market data, and the strategy lane when it holds a bar lane of the key: no plane RECORDS bars or
    // marks through the broker ----
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_interval(interval)) {
            strat.seed_bars(venue, symbol, interval, bars.clone());
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.seed_bars(venue, symbol, interval, bars);
        }
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_interval(interval)) {
            strat.close_bar(venue, symbol, interval, bar.clone());
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.close_bar(venue, symbol, interval, bar);
        }
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_interval(interval)) {
            strat.forming_bar(venue, symbol, interval, bar.clone());
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.forming_bar(venue, symbol, interval, bar);
        }
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        if let Some(strat) =
            self.strat(venue, symbol, DataClientBrokerStrategyKeys::admits_bar_tick)
        {
            strat.mark_tick(venue, symbol, px, ts);
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.mark_tick(venue, symbol, px, ts);
        }
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        if let Some(strat) =
            self.strat(venue, symbol, DataClientBrokerStrategyKeys::admits_bar_tick)
        {
            strat.bar_close_tick(venue, symbol, px, ts);
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.bar_close_tick(venue, symbol, px, ts);
        }
    }
    /// The venue's funding rate rides its mark lane, so it goes to the strategy lane where the mark does
    /// (a paper engine books funding from it). Neither market data nor the recorder takes it: the market-data
    /// plane shows no funding rate and the `interval=funding` series is filled from the venue's REST lane.
    fn funding(&self, venue: &str, symbol: &str, funding: vike_marketdata::FundingRate) {
        if let Some(strat) =
            self.strat(venue, symbol, DataClientBrokerStrategyKeys::admits_bar_tick)
        {
            strat.funding(venue, symbol, funding);
        }
    }
    // ---- the holders of a tick lane ----
    /// Admitted to the recorder and the strategy lane under `Quotes` OR `Book`: a book pump may derive
    /// L1 (polymarket's `FeedLane::Book`), so a quote can ride a book subscription. market data is called for
    /// every quote and drops the ones it holds no key for. What admits each holder is its own key set,
    /// never another holder's silence.
    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        let strat = self.strat(venue, symbol, DataClientBrokerStrategyKeys::admits_quote);
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
            rec.quote(venue, &store_name_of_user_name(symbol), quote.clone());
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.quote(venue, symbol, quote);
        }
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        if let Some(strat) = self.strat(venue, symbol, DataClientBrokerStrategyKeys::admits_trade) {
            strat.trade(venue, symbol, trade.clone());
        }
        if let Some(rec) = self.rec(venue, symbol, lane_bit(StreamLabel::Trades)) {
            rec.trade(venue, &store_name_of_user_name(symbol), trade.clone());
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.trade(venue, symbol, trade);
        }
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        if let Some(strat) = self.strat(venue, symbol, DataClientBrokerStrategyKeys::admits_book) {
            strat.book(venue, symbol, Arc::clone(&book));
        }
        if let Some(rec) = self.rec(venue, symbol, lane_bit(StreamLabel::Book)) {
            rec.book(venue, &store_name_of_user_name(symbol), Arc::clone(&book));
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.book(venue, symbol, book);
        }
    }
    fn book_update(&self, venue: &str, symbol: &str, update: L2BookUpdate) {
        if let Some(strat) = self.strat(venue, symbol, DataClientBrokerStrategyKeys::admits_book) {
            strat.book_update(venue, symbol, update.clone());
        }
        if let Some(rec) = self.rec(venue, symbol, lane_bit(StreamLabel::Book)) {
            rec.book_update(venue, &store_name_of_user_name(symbol), update.clone());
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.book_update(venue, symbol, update);
        }
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<L2BookLevel>,
        asks: Vec<L2BookLevel>,
        ts: i64,
    ) {
        if let Some(strat) = self.strat(venue, symbol, DataClientBrokerStrategyKeys::admits_depth) {
            strat.l2_snapshot(venue, symbol, tick_size, bids.clone(), asks.clone(), ts);
        }
        if let Some(rec) = self.rec(venue, symbol, lane_bit(StreamLabel::Depth)) {
            let store = store_name_of_user_name(symbol);
            rec.l2_snapshot(venue, &store, tick_size, bids.clone(), asks.clone(), ts);
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
        }
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        // The recorder's alone, whichever lanes the others hold: a strategy told this has a dead link to judge.
        if stream == TAPE_HOLE_WORD {
            let quote_lanes = lane_bit(StreamLabel::Quotes) | lane_bit(StreamLabel::Book);
            if let Some(rec) = self.rec(venue, symbol, quote_lanes) {
                rec.stream_status(venue, &store_name_of_user_name(symbol), stream, status);
            }
            return;
        }
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_stream_word(stream)) {
            strat.stream_status(venue, symbol, stream, status);
        }
        if let Some(lane) = StreamLabel::from_word(stream)
            && let Some(rec) = self.rec(venue, symbol, lane_bit(lane))
        {
            rec.stream_status(venue, &store_name_of_user_name(symbol), stream, status);
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.stream_status(venue, symbol, stream, status);
        }
    }
    // ---- market data and the strategy lane: the recorder stores no delay label, so it is WITHHELD a stream
    // that is not real time instead (the module doc) ----
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        if let Some(strat) = self.strat(venue, symbol, |k| k.admits_stream_word(stream)) {
            strat.data_delay(venue, symbol, stream, delay);
        }
        if let Some(lane) = StreamLabel::from_word(stream) {
            self.set_rec_withheld(venue, symbol, lane, !delay.is_real_time());
            self.market_data_delays
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert((venue.to_string(), symbol.to_string(), lane), delay);
        }
        if let Some(market_data) = self.market_data.get() {
            market_data.data_delay(venue, symbol, stream, delay);
        }
    }
}

#[cfg(test)]
mod tests {
    use vike_data::RecordingSink;

    use super::*;

    fn quote() -> TickQuote {
        vike_marketdata::test_support::quote(1, 1.0, 2.0)
    }

    fn rec_holds_eurusd() -> DataClientSinkRouting {
        let routing = DataClientSinkRouting::default();
        routing.attach(DataClientBrokerHolder::Rec, Arc::new(RecordingSink::default()));
        routing.attach(DataClientBrokerHolder::Strategy, Arc::new(RecordingSink::default()));
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

    /// **The recorder files a held key under its STORE name** (decision 0290): a builder-dex `XYZ:TSLAUSDC.P` is
    /// written `XYZ-TSLAUSDC.P` (a `:` is no store path's), its tape hole too, while the strategy lane keeps the
    /// user name it subscribed; a spot `HYPEUSDC` is its own store name.
    #[test]
    fn the_recorder_writes_a_held_key_under_its_store_name() {
        let (rec, strat) = (Arc::new(RecordingSink::default()), Arc::new(RecordingSink::default()));
        let routing = DataClientSinkRouting::default();
        routing.attach(DataClientBrokerHolder::Rec, rec.clone());
        routing.attach(DataClientBrokerHolder::Strategy, strat.clone());
        for user in ["XYZ:TSLAUSDC.P", "HYPEUSDC"] {
            let quotes = DataClientBrokerLane::Tick(StreamLabel::Quotes);
            routing.set_strategy("hyperliquid", user, &quotes, true);
            routing.set_rec("hyperliquid", user, StreamLabel::Quotes, true);
            routing.quote("hyperliquid", user, quote());
        }
        let gap = StreamStatus::GapStart { at_timestamp_ms: 5 };
        routing.stream_status("hyperliquid", "XYZ:TSLAUSDC.P", TAPE_HOLE_WORD, gap);

        let named = |calls: Vec<(String, String, TickQuote)>| -> Vec<String> {
            calls.into_iter().map(|(_, symbol, _)| symbol).collect()
        };
        assert_eq!(named(rec.quotes()), ["XYZ-TSLAUSDC.P", "HYPEUSDC"]);
        assert_eq!(named(strat.quotes()), ["XYZ:TSLAUSDC.P", "HYPEUSDC"]);
        assert_eq!(rec.stream_statuses()[0].1, "XYZ-TSLAUSDC.P");
    }

    /// Every holder attached, the strategy lane and market data holding every lane of the key and a bar lane under the
    /// word's own spelling, so nothing but the rule can keep it from them.
    fn all_holders()
    -> (DataClientSinkRouting, Arc<RecordingSink>, Arc<RecordingSink>, Arc<RecordingSink>) {
        let (rec, strat, market_data) = (
            Arc::new(RecordingSink::default()),
            Arc::new(RecordingSink::default()),
            Arc::new(RecordingSink::default()),
        );
        let routing = DataClientSinkRouting::default();
        routing.attach(DataClientBrokerHolder::Rec, rec.clone());
        routing.attach(DataClientBrokerHolder::Strategy, strat.clone());
        routing.attach(DataClientBrokerHolder::MarketData, market_data.clone());
        for label in StreamLabel::ALL {
            routing.set_strategy("ctrader", "EURUSD", &DataClientBrokerLane::Tick(label), true);
        }
        let as_a_bar = DataClientBrokerLane::Bars(TAPE_HOLE_WORD.to_string());
        routing.set_strategy("ctrader", "EURUSD", &as_a_bar, true);
        (routing, rec, strat, market_data)
    }

    /// **A tape hole reaches the recorder and no strategy, no window and no core.** What the strategy lane does
    /// not get cannot reach the tradehub's fold or its link dead-man, and every other word still does.
    #[test]
    fn a_tape_hole_reaches_the_recorder_and_never_a_strategy_or_a_window() {
        let (routing, rec, strat, market_data) = all_holders();
        routing.set_rec("ctrader", "EURUSD", StreamLabel::Quotes, true);
        let gap = StreamStatus::GapStart { at_timestamp_ms: 5 };
        let live = StreamStatus::Live { gap_started_timestamp_ms: Some(5) };
        routing.stream_status("ctrader", "EURUSD", TAPE_HOLE_WORD, gap);
        routing.stream_status("ctrader", "EURUSD", TAPE_HOLE_WORD, live);

        let told = rec.stream_statuses();
        assert_eq!(told.len(), 2, "{told:?}");
        assert!(told.iter().all(|(_, _, word, _)| word == TAPE_HOLE_WORD), "{told:?}");
        assert_eq!(told[0].3, gap);
        assert_eq!(told[1].3, live);
        assert!(strat.stream_statuses().is_empty(), "a strategy's link is not the tape's");
        assert!(market_data.stream_statuses().is_empty(), "a window holds no tape");
        assert!(strat.quotes().is_empty(), "and a mark is no price");

        // The rule is the word's: the strategy lane still gets the lane word it holds.
        routing.stream_status("ctrader", "EURUSD", StreamLabel::Quotes.word(), gap);
        assert_eq!(strat.stream_statuses().len(), 1);

        // A quote may ride a book hold (`Self::quote`), so a tape may too.
        routing.set_rec("polymarket", "TOKEN", StreamLabel::Book, true);
        routing.stream_status("polymarket", "TOKEN", TAPE_HOLE_WORD, gap);
        let last = rec.stream_statuses().pop().expect("the recorder was told");
        assert_eq!((last.1.as_str(), last.2.as_str()), ("TOKEN", TAPE_HOLE_WORD));
    }

    /// A key the recorder does not hold, or is withheld, has no tape to have a hole in.
    #[test]
    fn a_tape_hole_of_a_key_the_recorder_does_not_record_goes_nowhere() {
        let (routing, rec, strat, _market_data) = all_holders();
        let gap = StreamStatus::GapStart { at_timestamp_ms: 5 };
        routing.stream_status("ctrader", "EURUSD", TAPE_HOLE_WORD, gap);
        assert!(rec.stream_statuses().is_empty());

        routing.set_rec("ctrader", "EURUSD", StreamLabel::Quotes, true);
        routing.data_delay(
            "ctrader",
            "EURUSD",
            StreamLabel::Quotes.word(),
            StreamDelay::Delayed { lag_ms: None },
        );
        routing.stream_status("ctrader", "EURUSD", TAPE_HOLE_WORD, gap);
        assert!(rec.stream_statuses().is_empty());
        assert!(strat.stream_statuses().is_empty());
    }

    /// **The whole datahub half over a real store:** the word through this sink and the recorder into the
    /// lane; the tape's own read, and what the strategy holder got, exactly what they would have been with no
    /// outage; and the mark still there when the store is opened again by the next recorder.
    #[cfg(feature = "record")]
    #[test]
    fn a_tape_hole_through_the_routing_sink_lands_in_the_lane_and_nowhere_else() {
        use vike_data::store::store_kind::QUOTE_HOLES_LANE;
        use vike_data::{HistoryStore, TimestampRange};
        use vike_store::{HistoryStoreDataFusion, RecorderConfig, RecorderSink};

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).expect("store"));
        let (sink, handle) =
            RecorderSink::spawn(store.clone(), RecorderConfig::default()).expect("writer");
        let (routing, strat) =
            (DataClientSinkRouting::default(), Arc::new(RecordingSink::default()));
        routing.attach(DataClientBrokerHolder::Rec, sink);
        routing.attach(DataClientBrokerHolder::Strategy, strat.clone());
        routing.set_rec("ctrader", "EURUSD", StreamLabel::Quotes, true);
        routing.set_strategy(
            "ctrader",
            "EURUSD",
            &DataClientBrokerLane::Tick(StreamLabel::Quotes),
            true,
        );

        routing.quote("ctrader", "EURUSD", quote());
        let at_ts_ms = 86_400_000;
        routing.stream_status(
            "ctrader",
            "EURUSD",
            TAPE_HOLE_WORD,
            StreamStatus::GapStart { at_timestamp_ms: at_ts_ms },
        );
        handle.shutdown();

        let reopened = HistoryStoreDataFusion::open(dir.path()).expect("the next recorder's store");
        let tape = reopened.scan_quotes("ctrader", "EURUSD", TimestampRange::all()).expect("tape");
        assert_eq!(tape.len(), 1, "the tape's own read holds the quote and nothing else: {tape:?}");
        let marks = reopened
            .scan_quotes_sourced_capped(
                "ctrader",
                QUOTE_HOLES_LANE,
                "EURUSD",
                TimestampRange::all(),
                None,
            )
            .expect("lane");
        assert_eq!(
            marks.iter().map(|q| (q.timestamp, q.local_timestamp)).collect::<Vec<_>>(),
            [(at_ts_ms, 0)]
        );
        assert!(strat.stream_statuses().is_empty());
        assert_eq!(strat.quotes().len(), 1, "the strategy's price path carries the price, no mark");
    }

    /// A venue nobody asks the question of keeps no clock: the store reads the wall clock on every
    /// recorder-only quote (20 to 30 ns on the recorder's leg), so only a venue whose first price comes with
    /// a new stream pays it.
    #[test]
    fn a_venue_whose_first_price_does_not_wait_for_a_new_stream_keeps_no_quote_clock() {
        let routing = DataClientSinkRouting::default();
        routing.attach(DataClientBrokerHolder::Rec, Arc::new(RecordingSink::default()));
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
        routing.set_strategy(
            "oanda",
            "EURUSD",
            &DataClientBrokerLane::Tick(StreamLabel::Quotes),
            true,
        );
        routing.quote("oanda", "EURUSD", quote());
        assert!(
            routing.rec_quote_quiet("oanda", "EURUSD", now_ms(), 3_000),
            "a quote the strategy took must leave the clock alone"
        );

        routing.set_strategy(
            "oanda",
            "EURUSD",
            &DataClientBrokerLane::Tick(StreamLabel::Quotes),
            false,
        );
        routing.quote("oanda", "EURUSD", quote());
        assert!(
            !routing.rec_quote_quiet("oanda", "EURUSD", now_ms(), 3_000),
            "a quote only the recorder took writes it"
        );
    }
}
