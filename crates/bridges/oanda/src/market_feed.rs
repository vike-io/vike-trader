//! Live OANDA market data → the vike-data live seam. [`Feeds`] implements
//! [`vike_data::DataClient`] directly — the OANDA sibling of the crypto venues' `market_feed`
//! modules, with the venue's own SPLIT-PLANE shape:
//!
//! - **Quotes STREAM live** — `GET /v3/accounts/{id}/pricing/stream?instruments=…`, OANDA's
//!   chunked-HTTP line stream (the venue has no market-data WS at all), one JSON frame per line
//!   over the same Bearer-authed host family the exec side dials. Same transport shape as
//!   [`crate::stream::stream_transactions`] (the EXECUTION transactions lane — the two chunked
//!   GETs are DIFFERENT lanes; the crate CLAUDE.md's old "the chunked stream is not market data"
//!   trap is now split in half by this module). Decoded by the pure
//!   [`crate::market_data::decode_pricing_frame`].
//! - **Bars POLL REST** — the venue serves candles only as REST fetches
//!   ([`crate::data::fetch_candles`]'s endpoint), so the live bar lane is a poll-on-cadence
//!   thread over that same endpoint: seed, then re-fetch a small tail and emit newly-`complete`
//!   candles on the lossless `close_bar` lane + the still-forming candle on the conflating
//!   `forming_bar` lane. This is an honest venue-shape note, not a shortcut — the precedent is
//!   the tick-only venues synthesizing bars from ticks (polymarket): where a venue lacks a lane,
//!   the feed derives it from what the venue DOES serve and says so.
//!
//! Because the transport is chunked HTTP rather than a text WS, none of this can ride
//! [`vike_bridge_core::market_pump`]'s shared driver — this venue is `OwnPump` in
//! [`vike_bridge_core::pump_spec`] (the alpaca/ctrader/ibkr class), and the lifecycle conventions
//! are honored by construction instead: stop-aware fixed backoff ([`RECONNECT_BACKOFF`], the
//! driver's classic 3 s sliced at [`STOP_POLL_SLICE`]), a bounded dial (the market lane's
//! `dial_timeout`), per-subscription stop+join through the shared [`FeedRegistry`], and
//! stream-health disclosure through [`vike_bridge_core::stream_health::StreamHealth`] mapped 1:1
//! onto [`vike_model::StreamStatus`] at the sink boundary (the polymarket/crypto-depth idiom). Every
//! connection bound is a field of the market lane in `vike_venues::venue_timings::OANDA`.
//!
//! ## The heartbeat is the watchdog's clock
//! The pricing stream interleaves `{"type":"HEARTBEAT"}` frames (~every 5 s) with `PRICE` frames.
//! Liveness splits exactly as the §B net-hardening conventions split it:
//! - **Transport**: ANY line (price or heartbeat) proves the socket alive. The idle bound is the
//!   market lane's `idle_threshold` (4 missed ~5 s heartbeats) — ureq's `timeout_recv_body`, which
//!   for a streaming body bounds each read WAIT (the deadline is re-derived from `now` per wait,
//!   `ureq`'s `timings.rs` `next_timeout`), so a healthy heartbeating stream never trips it while a
//!   silently dead socket errors within ~one bound → gap + reconnect. Same mechanism and same value
//!   as the transactions stream's own idle bound in [`crate::stream`].
//! - **Data freshness**: heartbeats keep the transport watchdog quiet forever, which is exactly
//!   the failure it cannot see — a live socket serving no PRICE. [`StreamHealth`] ages the newest
//!   PRICE ts (armed at connect, so a session that never receives one still ages) past
//!   [`FRESHNESS_THRESHOLD_QUOTES`] behind the still-live socket → one `Stale` disclosure, `Live`
//!   on recovery. Disclose-only, mirroring the polymarket thresholds' contract — and note the FX
//!   market CLOSES (Friday 22:00 UTC through Sunday 21:00 UTC): a weekend `Stale` is an accurate
//!   disclosure of "no fresh price exists", not a false trip.
//!
//! ## Scope honesty
//! One chunked GET per subscribed symbol (the venue caps pricing-stream sessions per account;
//! fine at GUI scale — a many-symbol daemon wants the alpaca-style multiplexed single stream,
//! which this venue's `instruments=` list supports, deferred until a consumer needs it). No
//! trades lane (OANDA publishes no trade tape), no L2 book/depth lane (the pricing ladder is an
//! unsequenced top-of-book snapshot, not a delta-synced book — why the market-data conformance
//! row stays deferred; see `crates/vike-bridge-core/tests/market_data_conformance.rs`).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(doc)]
use vike_bridge_core::poller::STOP_POLL_SLICE;
#[cfg(doc)]
use vike_bridge_core::stream_health::StreamHealth;
use vike_catalog::to_oanda_instrument;
use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId, require_live_verb,
};
use vike_venues::LiveVerb;
use vike_venues::venue_timings;

use crate::config::OandaConfig;
use crate::data::granularity;

mod bars;
mod quotes;

#[cfg(doc)]
use bars::bar_poll_cadence;
use bars::bars_main;
use quotes::quotes_main;

const VENUE: &str = "oanda";

/// Stop-aware reconnect/retry backoff — the market lane's fixed `reconnect` wait in
/// `vike_venues::venue_timings::OANDA` (the shared market pumps' classic 3 s), walked in
/// [`STOP_POLL_SLICE`] naps so a raised stop is honoured within ~one slice. Two sites read it: the
/// quote pump's wait after a failed dial or a dropped stream, and the bar poll's wait after a
/// failed candle poll. It is the row's `initial`, which is the whole policy only while the row is
/// fixed (`reconnect_backoff_is_the_market_rows_fixed_three_seconds` holds that).
const RECONNECT_BACKOFF: Duration = venue_timings::OANDA.market().reconnect().initial;
/// §B data-freshness threshold for the quote lane, structured after the polymarket feed's
/// measured Book/Quotes floor (see `crates/bridges/polymarket/src/market_feed.rs`'s
/// `FRESHNESS_THRESHOLD_BOOK` for the tuning argument): majors tick sub-second in session, so
/// only a genuinely quiet stream trips — and off-session (nights near the daily rollover,
/// weekends) the disclosure is accurate rather than false. Disclose-only, never an auto-action.
const FRESHNESS_THRESHOLD_QUOTES: Duration = Duration::from_secs(300);

// --- Feeds (the DataClient) -------------------------------------------------------------------

/// Everything one feed thread needs, assembled around the registry-issued stop flag (the same
/// per-venue wrapper every sibling `market_feed` builds).
struct FeedCtx {
    config: OandaConfig,
    sink: Arc<dyn LiveDataSink>,
    status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
}

impl FeedCtx {
    fn set_status(&self, s: String) {
        *self.status.lock().unwrap() = s;
        (self.wake)();
    }
}

/// All live OANDA feed threads, keyed by [`SubscriptionId`] via the shared [`FeedRegistry`]
/// (one stop flag + one join handle per subscription; nothing detached — the
/// deterministic-teardown rule). Teardown latency note: a quote thread observes its stop flag
/// between lines, so joining waits for the next heartbeat (~5 s) or the idle bound (the market
/// lane's `idle_threshold`) worst-case — deterministic, not instant, exactly like the exec-side
/// transactions stream.
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    /// Operator-facing one-line status (shared with the spawned pumps).
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    config: Option<OandaConfig>,
    registry: FeedRegistry,
}

impl Feeds {
    /// `sink` receives every call from every subscription this `Feeds` spawns (construct once,
    /// subscribe many); `wake` is the GUI repaint nudge — pass `|| {}` headless. Constructed
    /// UNCREDENTIALED: every `subscribe_*` refuses until [`Feeds::with_config`] threads in the
    /// venue session (absent credentials ARE the live gate — nothing here reads the environment).
    pub fn new(sink: Arc<dyn LiveDataSink>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("OANDA feed: no credentials configured".into())),
            wake: Arc::new(wake),
            config: None,
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    VENUE,
                );
            }),
        }
    }

    /// The credentialed constructor variant: thread in the [`OandaConfig`] the composition root
    /// loaded through [`crate::config::load_oanda_config_from`] — the SAME loader (and therefore
    /// the same account's credentials) the exec side uses; never a second credential read.
    pub fn with_config(mut self, config: OandaConfig) -> Self {
        *self.status.lock().unwrap() = "connecting to OANDA…".into();
        self.config = Some(config);
        self
    }

    fn require_config(&self, label: &str) -> Result<&OandaConfig, LiveDataError> {
        self.config.as_ref().ok_or_else(|| {
            LiveDataError::Subscribe(format!(
                "oanda {label}: no credentials configured (absent credentials ARE the live \
                 gate) — build the feed with Feeds::with_config(load_oanda_config_from(…))"
            ))
        })
    }

    /// Per-subscription bookkeeping via [`FeedRegistry::spawn`]: the registry allocates the id +
    /// stop flag and owns the join handle (the MarketData affinity pin rides its spawn hook);
    /// this wrapper only assembles the [`FeedCtx`]. An OS thread-spawn failure is returned so
    /// the `subscribe_*` verb can map it to a [`LiveDataError`].
    fn spawn(
        &mut self,
        label: &str,
        config: OandaConfig,
        body: impl FnOnce(FeedCtx) + Send + 'static,
    ) -> std::io::Result<SubscriptionId> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        self.registry.spawn(format!("feed-oanda-{label}"), move |stop| {
            body(FeedCtx { config, sink, status, wake, stop })
        })
    }
}

impl DataClient for Feeds {
    /// Live bars by REST poll (see the module doc's split-plane note): seed +
    /// poll-on-[`bar_poll_cadence`] over the candles endpoint. The interval must have an OANDA
    /// granularity ([`granularity`]) — anything else is refused up front, before a thread exists.
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        require_live_verb(VENUE, LiveVerb::Bars)?;
        let Some(gran) = granularity(interval) else {
            return Err(LiveDataError::Subscribe(format!(
                "oanda {symbol}@{interval}: no OANDA candle granularity for this interval"
            )));
        };
        let config = self.require_config(&format!("{symbol}@{interval} bars"))?.clone();
        let instrument = to_oanda_instrument(symbol);
        let (series, iv) = (symbol.to_string(), interval.to_string());
        self.spawn(&format!("bars-{symbol}@{interval}"), config, move |ctx| {
            bars_main(instrument, series, iv, gran, ctx)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("oanda {symbol}@{interval}: {e}")))
    }

    /// Live L1 quotes from the chunked-HTTP pricing stream (see the module doc).
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        require_live_verb(VENUE, LiveVerb::Quotes)?;
        let config = self.require_config(&format!("{symbol} quotes"))?.clone();
        let instrument = to_oanda_instrument(symbol);
        let series = symbol.to_string();
        self.spawn(&format!("quotes-{symbol}"), config, move |ctx| {
            quotes_main(instrument, series, ctx)
        })
        .map_err(|e| LiveDataError::Subscribe(format!("oanda {symbol} quotes: {e}")))
    }

    fn subscribe_trades(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (oanda live_data.trades = false — the venue publishes no trade
        // tape): stays in lockstep with the declared table.
        require_live_verb(VENUE, LiveVerb::Trades)?;
        unreachable!("oanda declares no live trade tape")
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (oanda live_data.book = false — the pricing ladder is an
        // unsequenced top-of-book snapshot, no delta lane to record).
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("oanda declares no lossless book lane")
    }

    fn subscribe_depth(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (oanda live_data.depth = false), matching trades/book above.
        require_live_verb(VENUE, LiveVerb::Depth)?;
        unreachable!("oanda declares no live depth lane")
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

#[path = "market_feed_tests.rs"]
#[cfg(test)]
mod market_feed_tests;

#[path = "market_feed_props.rs"]
#[cfg(test)]
mod market_feed_props;

#[path = "recovery_disclosure_pin.rs"]
#[cfg(test)]
mod recovery_disclosure_pin;
