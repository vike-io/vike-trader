//! `RoutingSink` — the ONE sink every `Shared` venue client is built with. Md gets every verb (its
//! `crate::md::sink::MdHubSink` looks each key up itself); the recorder gets exactly the keys it
//! holds. Every verb is spelled out, never inherited — the omission `TeeSink`'s comments record.
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use vike_data::live::{LiveDataSink, StreamStatus};
use vike_model::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};

use super::{Holder, Lane};

#[derive(Default)]
pub(crate) struct RoutingSink {
    md: OnceLock<Arc<dyn LiveDataSink>>,
    rec: OnceLock<Arc<dyn LiveDataSink>>,
    /// `venue -> symbol -> lane bits` the recorder holds. Two levels so a hot-path lookup takes
    /// borrowed `&str`s and allocates nothing (the reason `crate::md::MdHub::keys` is shaped so).
    rec_keys: RwLock<HashMap<String, HashMap<String, u8>>>,
}

impl RoutingSink {
    pub(crate) fn attach(&self, holder: Holder, sink: Arc<dyn LiveDataSink>) {
        let slot = match holder {
            Holder::Md => &self.md,
            Holder::Rec => &self.rec,
        };
        let _ = slot.set(sink);
    }

    pub(crate) fn sink_of(&self, holder: Holder) -> Option<Arc<dyn LiveDataSink>> {
        match holder {
            Holder::Md => self.md.get().cloned(),
            Holder::Rec => self.rec.get().cloned(),
        }
    }

    pub(crate) fn set_rec(&self, venue: &str, symbol: &str, lane: Lane, on: bool) {
        let mut keys = self.rec_keys.write().unwrap_or_else(PoisonError::into_inner);
        let bits =
            keys.entry(venue.to_string()).or_default().entry(symbol.to_string()).or_default();
        if on {
            *bits |= lane.bit();
        } else {
            *bits &= !lane.bit();
        }
    }

    /// The recorder's sink when it holds any lane in `mask` for `(venue, symbol)`.
    fn rec(&self, venue: &str, symbol: &str, mask: u8) -> Option<&Arc<dyn LiveDataSink>> {
        let rec = self.rec.get()?;
        let keys = self.rec_keys.read().unwrap_or_else(PoisonError::into_inner);
        let held = keys.get(venue).and_then(|m| m.get(symbol)).copied().unwrap_or(0);
        ((held & mask) != 0).then_some(rec)
    }
}

impl LiveDataSink for RoutingSink {
    // ---- md only: no plane records bars or marks through the broker ----
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        if let Some(md) = self.md.get() {
            md.seed_bars(venue, symbol, interval, bars);
        }
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        if let Some(md) = self.md.get() {
            md.close_bar(venue, symbol, interval, bar);
        }
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        if let Some(md) = self.md.get() {
            md.forming_bar(venue, symbol, interval, bar);
        }
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        if let Some(md) = self.md.get() {
            md.mark_tick(venue, symbol, px, ts);
        }
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        if let Some(md) = self.md.get() {
            md.bar_close_tick(venue, symbol, px, ts);
        }
    }
    // ---- both planes ----
    /// Admitted to the recorder under `Quotes` OR `Book`: a book pump may derive L1 (polymarket's
    /// `PumpMode::Book`), and md never subscribes quotes — `MdLane` has no quotes lane — so no other
    /// subscription can emit a quote for a key the recorder holds.
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        if let Some(rec) = self.rec(venue, symbol, Lane::Quotes.bit() | Lane::Book.bit()) {
            rec.quote(venue, symbol, quote.clone());
        }
        if let Some(md) = self.md.get() {
            md.quote(venue, symbol, quote);
        }
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        if let Some(rec) = self.rec(venue, symbol, Lane::Trades.bit()) {
            rec.trade(venue, symbol, trade.clone());
        }
        if let Some(md) = self.md.get() {
            md.trade(venue, symbol, trade);
        }
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        if let Some(rec) = self.rec(venue, symbol, Lane::Book.bit()) {
            rec.book(venue, symbol, Arc::clone(&book));
        }
        if let Some(md) = self.md.get() {
            md.book(venue, symbol, book);
        }
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        if let Some(rec) = self.rec(venue, symbol, Lane::Book.bit()) {
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
        if let Some(rec) = self.rec(venue, symbol, Lane::Depth.bit()) {
            rec.l2_snapshot(venue, symbol, tick_size, bids.clone(), asks.clone(), ts);
        }
        if let Some(md) = self.md.get() {
            md.l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
        }
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        if let Some(lane) = Lane::from_stream_label(stream)
            && let Some(rec) = self.rec(venue, symbol, lane.bit())
        {
            rec.stream_status(venue, symbol, stream, status);
        }
        if let Some(md) = self.md.get() {
            md.stream_status(venue, symbol, stream, status);
        }
    }
}
