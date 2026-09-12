//! `CoreLaneSink` — the venue-agnostic [`vike_data::LiveDataSink`] → core-ingest-lane bridge,
//! single-sited here (next to `spawn_core`) so every mount adapter forwards live market data onto
//! the core the SAME way instead of re-implementing the verb→lane mapping.
//!
//! It maps each sink verb onto the concrete producer lane the core drains: bar seed/close onto the
//! lossless `BarSender`, forming-bar/bar-close-tick/mark onto the conflating `MarketSender`
//! (`bar_close_tick` → `publish_bar_close`, `mark_tick` → `publish` — separate slot maps, so the
//! two never conflate each other away; the runtime files them into the `PriceBoard` bar-close vs
//! mark slots respectively), quote/trade/book/stream-status onto the lossless `TickSender`.
//! `l2_snapshot`/`book_update` keep the trait's default no-op — the DOM book store and the
//! raw-recordable book are GUI/recorder concerns the core doesn't carry.
//!
//! Consumers (the duplication this removes): `vike_app_core::CoreSinkAdapter` delegates to it and
//! adds its GUI-only book/trade stores; the `vike-run` mount adapters wrap it. Because it forwards
//! `stream_status`, any mount built on it gives a mounted strategy's `on_feed_status` hook the
//! transition (the gap the hand-rolled run adapters had).
//!
//! Send-error policy (mirrors the pre-extraction `CoreSinkAdapter`): the lossless
//! `seed`/`close`/`quote`/`trade`/`book`/`stream_status` sends only fail with `CoreGone` (the
//! vt-core thread has exited); the conflating `MarketSender` sends are wait-free and infallible. A
//! feed thread has no channel back to its owner to request a stop, so on `CoreGone` this sink can't
//! stop the feed — it logs ONE `tracing::warn!` for its whole lifetime (guarded by `Once`) rather
//! than one per dropped message. In practice near-unreachable: mounts stop every feed BEFORE
//! `core.shutdown_and_join()`, so a live feed should never observe `CoreGone`. NOT in the hot fold.

use std::sync::Once;

use vike_data::{LiveDataSink, StreamStatus};
use vike_exec::{
    BarSeed, BarSender, BarUpdate, BookUpdate, MarketSender, MarketTick, QuoteUpdate,
    StreamStatusUpdate, TickSender, TradeUpdate,
};
use vike_model::{Bar, FeedStatus, L2Book, QuoteTick, TradeTick};

/// Forwards a venue feed's [`LiveDataSink`] verbs onto the core's ingest lanes. Construct once per
/// mount from the core's senders and hand it (or an adapter wrapping it) to the feed.
pub struct CoreLaneSink {
    bars: BarSender,
    market: MarketSender,
    ticks: TickSender,
    /// fires at most once per sink lifetime — see the module doc's send-error policy
    core_gone_warned: Once,
}

impl CoreLaneSink {
    /// Build over a spawned core's lanes (typically `handle.bar_sender()` /
    /// `handle.market_sender()` / `handle.tick_sender()`).
    pub fn new(bars: BarSender, market: MarketSender, ticks: TickSender) -> Self {
        CoreLaneSink { bars, market, ticks, core_gone_warned: Once::new() }
    }

    /// Log the core-gone condition exactly once per sink lifetime (see module doc).
    fn warn_core_gone_once(&self) {
        self.core_gone_warned.call_once(|| {
            tracing::warn!(
                "CoreLaneSink: vt-core ingest channel closed; further bar seed/close and \
                 quote/trade/book/stream-status sends from live feeds will be silently dropped \
                 until the feed is stopped"
            );
        });
    }
}

impl LiveDataSink for CoreLaneSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
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
        let update = BarUpdate {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            bar,
        };
        if self.bars.close(update).is_err() {
            self.warn_core_gone_once();
        }
    }

    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.market.publish_forming(venue, symbol, interval, bar);
    }

    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.market.publish(MarketTick {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            px,
            ts,
        });
    }

    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        // The candle-close twin of `mark_tick` — its own conflation slot, so a real-mark stream
        // and a kline feed on the same symbol never overwrite each other. Downstream the runtime
        // files it as `Account.set_mark` + `PriceBoard::set_bar_close` (mark-slot semantics).
        self.market.publish_bar_close(MarketTick {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            px,
            ts,
        });
    }

    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        let update = QuoteUpdate { venue: venue.to_string(), symbol: symbol.to_string(), quote };
        if self.ticks.quote(update).is_err() {
            self.warn_core_gone_once();
        }
    }

    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
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
        // Map the data-layer StreamStatus onto the vike-model-native FeedStatus 1:1 and forward it
        // onto the tick lane, where the runtime dispatches it to any strategy mounted on this
        // (venue, symbol) — the strategy-facing `on_feed_status` wiring.
        let feed_status = match status {
            StreamStatus::GapStart { .. } => FeedStatus::Disconnected,
            StreamStatus::Stale { .. } => FeedStatus::Stale,
            StreamStatus::Live { .. } => FeedStatus::Live,
        };
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
}
