//! `StratSink` - the strategy lane's ONE sink, the one the broker attaches for [`crate::feeds::Holder::Strat`].
//! It fans a venue client's frame to the lane SESSIONS that hold the frame's key: one `(venue, symbol)`
//! can be held by more than one session (a reconnect overlap), and each session has its own wire sink
//! (`vike_market_stream::sink::WireSink`, the tube's sending half) and its own view of what it holds.
//!
//! # Two filters, one set of rules
//!
//! `crate::feeds::routing::RoutingSink` already admits a frame to this sink only when the lane as a whole
//! holds the key (`strat_keys`). This sink then admits it to each SESSION by that session's own
//! [`StratKeys`] - the same rules, one level down - so a session that asked for the book of a symbol is
//! not handed the quotes another session asked for. A venue that is `PerHolder` (polymarket, deribit)
//! hands its client this sink directly, with no `RoutingSink` in front, and the session filter is then
//! the only one: every frame is still admitted by exactly the lanes a session holds.
//!
//! # A frame call must stay cheap
//!
//! Every verb here runs on a venue client's pump thread, serially with the md hub's and the recorder's
//! calls (the strat call is first in `RoutingSink`'s order). It takes a READ lock, finds the sessions
//! and calls their `WireSink`, which takes its own lock for a few hash operations and queues. Nothing
//! here serializes, writes a socket, allocates beyond a payload clone for a second session, or waits for
//! a consumer. A session registering or leaving takes the write lock for one map operation.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use vike_data::LiveDataSink;
use vike_datahub_client::proto::SL_BOOK_LEVELS_CEILING;
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

use crate::feeds::routing::StratKeys;

/// `book` as the strategy lane may carry it: at most [`SL_BOOK_LEVELS_CEILING`] levels a side, best first.
///
/// The lane's frame ceiling (`SL_FRAME_CEILING_BYTES`, 128 KiB) is sized for one folded book of that many
/// levels (`vike_datahub_client::proto`'s `const _` assert), and `WireSink::book` truncates nothing: a
/// whole book past it is refused by the writer before its first byte and owed as a hole, so a venue that
/// folds an uncapped standing book (deribit's `book.{inst}.100ms` is the whole ladder, polymarket's a whole
/// token book) would have EVERY book frame dropped as unsendable while the status stream flapped
/// `GapStart`/`Live`. The pump caps its own anchors (`PUMP_BOOK_LEVELS`); this is the same cap for the
/// whole-book verb. A book already within the ceiling is passed on as it is (the same `Arc`).
fn capped(book: Arc<L2Book>) -> Arc<L2Book> {
    if book.bid_levels() <= SL_BOOK_LEVELS_CEILING && book.ask_levels() <= SL_BOOK_LEVELS_CEILING {
        return book;
    }
    let (bids, asks) = book.top_n(SL_BOOK_LEVELS_CEILING);
    let mut out = L2Book::new(book.tick_size);
    out.apply_snapshot(book.last_seq, &bids, &asks);
    Arc::new(out)
}

/// One session's claim on one key: where its frames go and what it holds.
struct Route {
    session: u64,
    sink: Arc<dyn LiveDataSink>,
    keys: StratKeys,
}

/// See the module doc.
#[derive(Default)]
pub(crate) struct StratSink {
    /// `venue -> symbol -> the sessions holding it`, shaped for the borrowed `&str` lookup a hot path
    /// needs (`RoutingSink::strat_keys`' reason).
    routes: RwLock<HashMap<String, HashMap<String, Vec<Route>>>>,
    /// The last status each held lane word was told, keyed `(venue, symbol, word)`: what a session that
    /// JOINS the key later is told first ([`Self::restate`]). Only keys some session holds are kept; a key
    /// nobody holds any more is forgotten with its last holder.
    ///
    /// ⚠ **This lock is also the status ORDER.** A status is fanned out to the sessions AND recorded under it,
    /// and a joiner is restated under it, so a joiner's wire sees `GapStart` then `Live` exactly as the venue
    /// client said them: never the old `Live` after a new `GapStart` (an outage missed until the next
    /// transition), never a stale `GapStart` after a real `Live` (a window that trips the dead-man on a healthy
    /// lane). **Lock order: `last` BEFORE `routes`**, everywhere (`set`, `forget`, `stream_status`,
    /// `restate`); no frame verb other than a status takes `last`.
    last: Mutex<HashMap<(String, String, String), StreamStatus>>,
}

impl StratSink {
    /// Set `session`'s claim on `(venue, symbol)` to `keys`, delivering into `sink`; an EMPTY `keys`
    /// withdraws it (and forgets the key once no session holds it).
    pub(crate) fn set(
        &self,
        venue: &str,
        symbol: &str,
        session: u64,
        sink: &Arc<dyn LiveDataSink>,
        keys: StratKeys,
    ) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let mut routes = self.routes.write().unwrap_or_else(PoisonError::into_inner);
        if keys.is_empty() {
            if let Some(by_symbol) = routes.get_mut(venue) {
                if let Some(held) = by_symbol.get_mut(symbol) {
                    held.retain(|r| r.session != session);
                    if held.is_empty() {
                        by_symbol.remove(symbol);
                        last.retain(|(v, s, _), _| !(v == venue && s == symbol));
                    }
                }
                if by_symbol.is_empty() {
                    routes.remove(venue);
                }
            }
            return;
        }
        let held =
            routes.entry(venue.to_string()).or_default().entry(symbol.to_string()).or_default();
        match held.iter_mut().find(|r| r.session == session) {
            Some(route) => route.keys = keys,
            None => held.push(Route { session, sink: Arc::clone(sink), keys }),
        }
    }

    /// Withdraw every claim of `session`, on every key (the session ended).
    pub(crate) fn forget(&self, session: u64) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let mut routes = self.routes.write().unwrap_or_else(PoisonError::into_inner);
        routes.retain(|_, by_symbol| {
            by_symbol.retain(|_, held| {
                held.retain(|r| r.session != session);
                !held.is_empty()
            });
            !by_symbol.is_empty()
        });
        // A key nobody holds any more is forgotten with its last holder.
        last.retain(|(v, s, _), _| {
            routes.get(v).is_some_and(|by_symbol| by_symbol.contains_key(s))
        });
    }

    /// Tell `to` — a session that has just joined `(venue, symbol)`'s lane `word` — where the lane stands:
    /// the last status the venue's client disclosed for the word, or `Live` when it has disclosed none.
    ///
    /// ⚠ **Why a joiner is told at all.** A venue client discloses TRANSITIONS: a `GapStart` when it loses
    /// a socket and a `Live` when it is back, and nothing while it is simply up. A session that joins a
    /// lane later (a tradehub that reconnected after a datahub restart, a second session on a held key)
    /// would otherwise hear nothing about a lane whose last word before its old session died was a
    /// `GapStart`, and the tradehub's dead-man window for it, opened by that `GapStart` and never closed by
    /// the `Live` that fell into the dead socket, would stay open until it tripped on a healthy lane. The
    /// pump hub's joiners are told already (`vike-gatewayhub`'s `PumpHub`); this is the broker lanes' twin.
    pub(crate) fn restate(&self, venue: &str, symbol: &str, word: &str, to: &dyn LiveDataSink) {
        // Read AND sent under the lock `stream_status` fans out and records under: a status the venue client
        // says concurrently is ordered before or after this restatement as a whole (see `last`).
        let last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let status = last
            .get(&(venue.to_string(), symbol.to_string(), word.to_string()))
            .copied()
            .unwrap_or(StreamStatus::Live { gap_started_ts_ms: None });
        to.stream_status(venue, symbol, word, status);
        drop(last);
    }

    /// How many `(venue, symbol)` keys any session holds right now (the tests' witness).
    #[cfg(test)]
    pub(crate) fn key_count(&self) -> usize {
        let routes = self.routes.read().unwrap_or_else(PoisonError::into_inner);
        routes.values().map(HashMap::len).sum()
    }

    /// Call `deliver` on the sink of every session whose view of `(venue, symbol)` `admits` the frame.
    fn each(
        &self,
        venue: &str,
        symbol: &str,
        admits: impl Fn(&StratKeys) -> bool,
        mut deliver: impl FnMut(&dyn LiveDataSink),
    ) {
        let routes = self.routes.read().unwrap_or_else(PoisonError::into_inner);
        if let Some(held) = routes.get(venue).and_then(|m| m.get(symbol)) {
            for route in held.iter().filter(|r| admits(&r.keys)) {
                deliver(route.sink.as_ref());
            }
        }
    }
}

impl LiveDataSink for StratSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.each(
            venue,
            symbol,
            |k| k.admits_interval(interval),
            |s| s.seed_bars(venue, symbol, interval, bars.clone()),
        );
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.each(
            venue,
            symbol,
            |k| k.admits_interval(interval),
            |s| s.close_bar(venue, symbol, interval, bar.clone()),
        );
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.each(
            venue,
            symbol,
            |k| k.admits_interval(interval),
            |s| s.forming_bar(venue, symbol, interval, bar.clone()),
        );
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.each(venue, symbol, StratKeys::admits_bar_tick, |s| {
            s.mark_tick(venue, symbol, px, ts);
        });
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.each(venue, symbol, StratKeys::admits_bar_tick, |s| {
            s.bar_close_tick(venue, symbol, px, ts);
        });
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.each(venue, symbol, StratKeys::admits_quote, |s| {
            s.quote(venue, symbol, quote.clone());
        });
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.each(venue, symbol, StratKeys::admits_trade, |s| {
            s.trade(venue, symbol, trade.clone());
        });
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        let book = capped(book);
        self.each(venue, symbol, StratKeys::admits_book, |s| {
            s.book(venue, symbol, Arc::clone(&book));
        });
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        self.each(venue, symbol, StratKeys::admits_book, |s| {
            s.book_update(venue, symbol, update.clone());
        });
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
        self.each(venue, symbol, StratKeys::admits_depth, |s| {
            s.l2_snapshot(venue, symbol, tick_size, bids.clone(), asks.clone(), ts);
        });
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        // The fan-out and the record are ONE step under `last`: a joiner restated in between would be told the
        // old status after the new one, or the new one and then the old (see `last`).
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let mut told = 0_usize;
        self.each(
            venue,
            symbol,
            |k| k.admits_stream_word(stream),
            |s| {
                told += 1;
                s.stream_status(venue, symbol, stream, status);
            },
        );
        // Only a word some session holds is remembered, for the next one to join it.
        if told > 0 {
            last.insert((venue.to_string(), symbol.to_string(), stream.to_string()), status);
        }
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        self.each(
            venue,
            symbol,
            |k| k.admits_stream_word(stream),
            |s| s.data_delay(venue, symbol, stream, delay),
        );
    }
}
