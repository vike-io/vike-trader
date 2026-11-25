//! `CoreLaneSink` — the venue-agnostic [`vike_data::DataClientSink`] → core-ingest-lane bridge,
//! single-sited here (next to `spawn_core`) so every mount adapter forwards live market data onto
//! the core the SAME way instead of re-implementing the verb→lane mapping.
//!
//! It maps each sink verb onto the concrete producer lane the core drains: bar seed/close onto the
//! lossless `BarSender`, forming-bar/bar-close-tick/mark onto the conflating `MarketSender`
//! (`bar_close_tick` → `publish_bar_close`, `mark_tick` → `publish` — separate slot maps, so the
//! two never conflate each other away; the runtime files them into the `PriceBoard` bar-close vs
//! mark slots respectively), quote/trade/book/stream-status/funding onto the lossless `TickSender`.
//! `l2_snapshot`/`book_update` keep the trait's default no-op — the DOM book store and the
//! raw-recordable book are GUI/recorder concerns the core doesn't carry.
//!
//! Consumers (the duplication this removes): `vike-tradehub-core`'s strategy-mount adapters (vike-run's
//! until docs/decisions/0098) wrap it. ⚠ This named `vike_app_core::CoreSinkAdapter` as the first
//! consumer until 2026-10-09: nothing constructed that adapter after the desktop lost its local
//! core, it was deleted, and the GUI links no vike-core at all now (docs/decisions/0115). Because it forwards
//! `stream_status`, any mount built on it gives a mounted strategy's `on_feed_status` hook the
//! transition (the gap the hand-rolled run adapters had).
//!
//! Send-error policy (mirrors the pre-extraction `CoreSinkAdapter`): the lossless
//! `seed`/`close`/`quote`/`trade`/`book`/`stream_status` sends only fail with `CoreGone` (the
//! th-core thread has exited); the conflating `MarketSender` sends are wait-free and infallible. A
//! feed thread has no channel back to its owner to request a stop, so on `CoreGone` this sink can't
//! stop the feed — it logs ONE `tracing::warn!` for its whole lifetime (guarded by `Once`) rather
//! than one per dropped message. In practice near-unreachable: mounts stop every feed BEFORE
//! `core.shutdown_and_join()`, so a live feed should never observe `CoreGone`. NOT in the hot fold.
//!
//! # The delay label (decision 0140): forwarded, and announced once
//!
//! `data_delay` goes onto the lossless tick lane as `Ingest::DataDelay`, where the core files it per
//! mount (the strategy reads it through `OrderBroker::data_delay`) and in the table the submit path
//! stamps `OrderRequest::data_delay` from. Beside the forward, this sink writes the mount's ONE
//! informational line per stream that is not real-time, naming the venue, the symbol and the label
//! (`DelayAnnouncer`), so nobody mistakes a 15-minute-old stock quote for a live one in the log; the
//! fold logs nothing per message (its one line is per ORDER, at the submit stamp). The label is
//! information, never a gate: nothing here or in the core refuses, holds or delays anything.
//! `data_delay` arrives only when a stream's label CHANGES, so the lock the announcer takes is off
//! every per-message path.
//!
//! # A closed bar carries its quote sides (decision 0285)
//!
//! A venue's candle is a path of TRADES (a CEX kline, the hyperliquid, deribit and alpaca candles) or of
//! MIDS (OANDA's, IG's), so the paper book, which fills on closed bars, would fill a market buy at a
//! print or the mid where the venue charges the ask (the owner, 2026-11-16: "paper fills like the
//! real venue, never at the mid"). So this sink folds every two-sided quote of a series it has seen
//! (a seed, a forming or a closed bar names it) into the bid's and the ask's path over that series'
//! windows ([`QuoteSidePaths`]), and a closed bar goes onto the bar lane with the sides of ITS window
//! (`BarUpdate::sides`), which the core hands to the paper book only. A series whose quotes never
//! came, or whose interval has no fixed width (`1w`), closes with none: nothing is guessed. The fold
//! runs on the feed's thread, one uncontended lock per quote; the bar's own path is untouched.

use std::collections::HashSet;
use std::sync::{Mutex, Once, PoisonError};

use vike_data::DataClientSink;
use vike_execution::{
    BarSeed, BarSender, BarUpdate, BookUpdate, DataDelayUpdate, FundingRateUpdate, MarketSender,
    MarketTick, QuoteUpdate, StreamStatusUpdate, TickSender, TradeUpdate,
};
use vike_marketdata::{Bar, BarSides, FundingRate, L2Book, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamStatus};

/// **The bid's and the ask's path over each window of one bar series**, folded from quotes as they
/// come (decision 0285). Windows are `offset + k·step` in event time (`offset` a bar start of the
/// series, so a venue whose day starts at 17:00 New York is cut where its candles are). A window's
/// path OPENS at the quote standing when it began (the last quote of an earlier window), then takes
/// every quote stamped inside it; a quote stamped before the window being folded joins it (the
/// consolidator's out-of-order rule). A quote with a side that is not a finite price above zero is
/// skipped. O(1) per quote, no allocation.
pub(crate) struct QuoteSidePaths {
    step: i64,
    offset: i64,
    /// the window being folded: its start, the bid's and the ask's `[open, high, low, close]`
    current: Option<(i64, [f64; 4], [f64; 4])>,
    /// the last window a later quote closed
    closed: Option<(i64, [f64; 4], [f64; 4])>,
}

impl QuoteSidePaths {
    /// Windows `step_ms` wide (floored at 1), one of them starting at `bar_start`.
    pub(crate) fn new(step_ms: i64, bar_start: i64) -> Self {
        let step = step_ms.max(1);
        QuoteSidePaths { step, offset: bar_start.rem_euclid(step), current: None, closed: None }
    }

    /// Re-cut the windows on `bar_start` when the series moved its boundary (a daylight-saving day
    /// of a venue that cuts its days in New York time): what was folded on the old cut is dropped.
    pub(crate) fn realign(&mut self, bar_start: i64) {
        let offset = bar_start.rem_euclid(self.step);
        if offset != self.offset {
            *self = QuoteSidePaths::new(self.step, bar_start);
        }
    }

    /// Fold one quote.
    pub(crate) fn on_quote(&mut self, ts: i64, bid: f64, ask: f64) {
        if !(bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > 0.0) {
            return;
        }
        let start = ts - (ts - self.offset).rem_euclid(self.step);
        let extend = |path: &mut [f64; 4], px: f64| {
            path[1] = path[1].max(px);
            path[2] = path[2].min(px);
            path[3] = px;
        };
        // a later window opens at the quote that stood when it began (the first one at this quote)
        let (standing_bid, standing_ask) = match &mut self.current {
            Some((current, b, a)) if start <= *current => {
                extend(b, bid);
                extend(a, ask);
                return;
            }
            Some((_, b, a)) => (b[3], a[3]),
            None => (bid, ask),
        };
        let (mut b, mut a) = ([standing_bid; 4], [standing_ask; 4]);
        extend(&mut b, bid);
        extend(&mut a, ask);
        self.closed = self.current.replace((start, b, a));
    }

    /// The sides of the window starting at `start`, each stamped `stamp`: the window's own paths, or
    /// the quote that stood through a window no quote moved; `None` before the first quote, or for a
    /// window older than the last one closed. ⚠ Both producers stamp them with the window's START,
    /// whatever the bar beside them carries (a venue candle its start, the tick synthesizer its end): the
    /// paper book reads a `fill = "close"` fill's END off it (`vike_paper`'s `fill_at_close`).
    pub(crate) fn sides(&self, start: i64, stamp: i64) -> Option<BarSides> {
        let still = |b: &[f64; 4], a: &[f64; 4]| ([b[3]; 4], [a[3]; 4]);
        let (current, cb, ca) = self.current.as_ref()?;
        let (bid, ask) = if *current == start {
            (*cb, *ca)
        } else if *current < start {
            still(cb, ca)
        } else {
            match &self.closed {
                Some((closed, b, a)) if *closed == start => (*b, *a),
                Some((closed, b, a)) if *closed < start => still(b, a),
                _ => return None,
            }
        };
        let bar =
            |[open, high, low, close]: [f64; 4]| Bar::ohlcv(stamp, open, high, low, close, 0.0);
        Some(BarSides { bid: bar(bid), ask: bar(ask) })
    }
}

/// One bar series a [`CoreLaneSink`] has seen, with its quote sides.
struct SeriesSides {
    venue: String,
    symbol: String,
    interval: String,
    paths: QuoteSidePaths,
}

/// Forwards a venue feed's [`DataClientSink`] verbs onto the core's ingest lanes. Construct once per
/// mount from the core's senders and hand it (or an adapter wrapping it) to the feed.
pub struct CoreLaneSink {
    bars: BarSender,
    market: MarketSender,
    ticks: TickSender,
    /// the one informational line per stream that is not real-time (module doc)
    delays: DelayAnnouncer,
    /// fires at most once per sink lifetime — see the module doc's send-error policy
    core_gone_warned: Once,
    /// every bar series this sink has seen and its quote sides (the module doc's last section)
    series: Mutex<Vec<SeriesSides>>,
}

impl CoreLaneSink {
    /// Build over a spawned core's lanes (typically `handle.bar_sender()` /
    /// `handle.market_sender()` / `handle.tick_sender()`).
    pub fn new(bars: BarSender, market: MarketSender, ticks: TickSender) -> Self {
        CoreLaneSink {
            bars,
            market,
            ticks,
            delays: DelayAnnouncer::default(),
            core_gone_warned: Once::new(),
            series: Mutex::new(Vec::new()),
        }
    }

    /// Note a bar of `(venue, symbol, interval)` starting at `bar_start`: the series' quotes are
    /// folded from now on, its windows cut on that start; then `f` on its paths (`None` for an
    /// interval with no fixed width, which folds nothing).
    fn with_series<T>(
        &self,
        (venue, symbol, interval): (&str, &str, &str),
        bar_start: i64,
        f: impl FnOnce(&QuoteSidePaths) -> Option<T>,
    ) -> Option<T> {
        let mut series = self.series.lock().unwrap_or_else(PoisonError::into_inner);
        let i = match series
            .iter()
            .position(|s| s.venue == venue && s.symbol == symbol && s.interval == interval)
        {
            Some(i) => {
                series[i].paths.realign(bar_start);
                i
            }
            None => {
                let step = vike_marketdata::interval::bar_step_ms(interval)?;
                series.push(SeriesSides {
                    venue: venue.to_string(),
                    symbol: symbol.to_string(),
                    interval: interval.to_string(),
                    paths: QuoteSidePaths::new(step, bar_start),
                });
                series.len() - 1
            }
        };
        f(&series[i].paths)
    }

    /// Log the core-gone condition exactly once per sink lifetime (see module doc).
    fn warn_core_gone_once(&self) {
        self.core_gone_warned.call_once(|| {
            tracing::warn!(
                "CoreLaneSink: th-core ingest channel closed; further bar seed/close and \
                 quote/trade/book/stream-status sends from live feeds will be silently dropped \
                 until the feed is stopped"
            );
        });
    }
}

impl DataClientSink for CoreLaneSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        if let Some(last) = bars.last() {
            self.with_series((venue, symbol, interval), last.timestamp, |_| None::<()>);
        }
        let seed = BarSeed {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            bars,
        };
        if self.bars.seed(seed).is_err() {
            self.warn_core_gone_once();
        }
    }

    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        // a venue's bar is stamped with its window's START: the sides are that window's
        let start = bar.timestamp;
        let sides =
            self.with_series((venue, symbol, interval), start, |paths| paths.sides(start, start));
        let update = BarUpdate {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            bar,
            sides: sides.map(Box::new),
        };
        if self.bars.close(update).is_err() {
            self.warn_core_gone_once();
        }
    }

    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.with_series((venue, symbol, interval), bar.timestamp, |_| None::<()>);
        self.market.publish_forming(venue, symbol, interval, bar);
    }

    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.market.publish(MarketTick {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            price: px,
            timestamp: ts,
        });
    }

    fn top_of_book_tick(&self, venue: &str, symbol: &str, bid: f64, ask: f64, ts: i64) {
        self.market.publish_top(venue, symbol, bid, ask, ts);
    }

    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        // The candle-close twin of `mark_tick` — its own conflation slot, so a real-mark stream
        // and a kline feed on the same symbol never overwrite each other. Downstream the runtime
        // files it as `Account.set_mark` + `PriceBoard::set_bar_close` (mark-slot semantics).
        self.market.publish_bar_close(MarketTick {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            price: px,
            timestamp: ts,
        });
    }

    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        {
            let mut series = self.series.lock().unwrap_or_else(PoisonError::into_inner);
            for s in series.iter_mut().filter(|s| s.venue == venue && s.symbol == symbol) {
                s.paths.on_quote(quote.timestamp, quote.bid, quote.ask);
            }
        }
        let update = QuoteUpdate { venue: venue.to_string(), symbol: symbol.to_string(), quote };
        if self.ticks.quote(update).is_err() {
            self.warn_core_gone_once();
        }
    }

    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        let update = TradeUpdate { venue: venue.to_string(), symbol: symbol.to_string(), trade };
        if self.ticks.trade(update).is_err() {
            self.warn_core_gone_once();
        }
    }

    fn book(&self, venue: &str, symbol: &str, book: std::sync::Arc<L2Book>) {
        let update = BookUpdate { venue: venue.to_string(), symbol: symbol.to_string(), book };
        if self.ticks.book(update).is_err() {
            self.warn_core_gone_once();
        }
    }

    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        // Project the transition onto the strategy-facing state through the ONE map
        // (`StreamStatus::feed_status`) and forward it onto the tick lane, where the runtime
        // dispatches it to any strategy mounted on this (venue, symbol) — the `on_feed_status` wiring.
        let feed_status = status.feed_status();
        let update = StreamStatusUpdate {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            stream: stream.to_string(),
            status: feed_status,
        };
        if self.ticks.stream_status(update).is_err() {
            self.warn_core_gone_once();
        }
    }

    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        // Announced once per not-real-time stream (module doc), then forwarded to the core.
        self.delays.note(venue, symbol, stream, delay);
        let update = DataDelayUpdate {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            stream: stream.to_string(),
            delay,
        };
        if self.ticks.data_delay(update).is_err() {
            self.warn_core_gone_once();
        }
    }

    fn funding(&self, venue: &str, symbol: &str, funding: FundingRate) {
        // The venue's public funding rate onto the tick lane: the core books a PAPER engine's funding
        // from it (`Ingest::FundingRate`); a live engine ignores it.
        let update =
            FundingRateUpdate { venue: venue.to_string(), symbol: symbol.to_string(), funding };
        if self.ticks.funding_rate(update).is_err() {
            self.warn_core_gone_once();
        }
    }
}

/// The mount's ONE informational line per stream whose data is not real-time (decision 0140): the
/// first `Delayed`/`Frozen` label a `(venue, symbol, stream)` reports logs one `info!` naming the
/// symbol; every later label of that stream, and every `RealTime` one, logs nothing. It remembers a
/// stream for the sink's lifetime, so a stream that flips between delayed and real-time still logs
/// once. Called only on a label CHANGE (`DataClientSink::data_delay`'s contract), never per message.
#[derive(Default)]
pub(crate) struct DelayAnnouncer {
    announced: Mutex<HashSet<String>>,
}

impl DelayAnnouncer {
    /// Note one label; `true` iff this call logged the stream's one line.
    pub(crate) fn note(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) -> bool {
        if delay.is_real_time() {
            return false;
        }
        let key = format!("{venue}\u{1f}{symbol}\u{1f}{stream}");
        let first = self.announced.lock().unwrap_or_else(PoisonError::into_inner).insert(key);
        if first {
            let lag_ms = match delay {
                StreamDelay::Delayed { lag_ms } => lag_ms,
                StreamDelay::RealTime | StreamDelay::Frozen => None,
            };
            tracing::info!(
                venue,
                symbol,
                stream,
                delay = delay.word(),
                ?lag_ms,
                "{venue} {symbol} {stream}: market data is {} (the account's entitlement); \
                 orders may still be priced off it",
                delay.word()
            );
        }
        first
    }
}

#[path = "core_sink_tests.rs"]
#[cfg(test)]
mod core_sink_tests;
