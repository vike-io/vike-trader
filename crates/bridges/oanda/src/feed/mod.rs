//! Live OANDA market data → the vike-data live seam. [`Feeds`] implements
//! [`vike_data::DataClient`] directly — the OANDA sibling of the crypto venues' `market_feed`
//! modules, with the venue's own SPLIT-PLANE shape:
//!
//! - **Quotes STREAM live** — `GET /v3/accounts/{id}/pricing/stream?instruments=…`, OANDA's
//!   chunked-HTTP line stream (the venue has no market-data WS at all), one JSON frame per line
//!   over the same Bearer-authed host family the exec side dials. Same transport shape as
//!   [`crate::execution::user_data::stream_transactions`] (the EXECUTION transactions lane — the two chunked
//!   GETs are DIFFERENT lanes; the crate CLAUDE.md's old "the chunked stream is not market data"
//!   trap is now split in half by this module). Decoded by the pure
//!   [`crate::feed::decode::decode_pricing_frame`].
//! - **Bars POLL REST** — the venue serves candles only as REST fetches
//!   ([`crate::history::candles::fetch_candles`]'s endpoint), so the live bar lane is a poll-on-cadence
//!   thread over that same endpoint: seed, then re-fetch a small tail and emit newly-`complete`
//!   candles on the lossless `close_bar` lane + the still-forming candle on the conflating
//!   `forming_bar` lane. This is an honest venue-shape note, not a shortcut — the precedent is
//!   the tick-only venues synthesizing bars from ticks (polymarket): where a venue lacks a lane,
//!   the feed derives it from what the venue DOES serve and says so.
//! - **A pair's FIRST quote can come from REST** — the pricing stream host answers an open at
//!   random (a hung open waits out the market lane's whole `response_timeout`), so each quote
//!   subscription also asks the `seed` lane for one tick from the pricing ENDPOINT, beside the
//!   stream open. The stream keeps running and takes over; the seed's contract (tradeable, fresh,
//!   bounded, rate-paced, no seed rather than a made-up one) is that module's `//!`.
//!
//! A subscription names the USER name (`EURUSD`, decision 0290): its series is keyed by that name and the wire
//! instrument it dials (`EUR_USD`) is the name's kept row's (`crate::names`). A name with no kept row is refused in
//! words before any thread exists.
//!
//! The transport is chunked HTTP rather than a text WS, and the quote lane still rides
//! [`vike_bridge_core::websocket_session::market`]'s shared driver: the body is read as lines
//! (`vike_bridge_core::websocket_session::user::line_stream`'s `LineStream`) on `run_market_on`, and this venue is
//! a pump row of [`vike_bridge_core::websocket_session::spec`] (`row`), which reads the market lane of
//! `vike_venues::venue_timings::OANDA` (every connection bound is a field there). The driver owns the fixed 3 s redial,
//! the idle watchdog and the session's `Live`/`Error` edges; the lane owns its open (a bounded dial, the market lane's
//! `dial_timeout`), per-subscription stop+join through the shared [`DataClientRegistry`], and
//! stream-health disclosure through [`vike_bridge_core::stream_health::StreamHealth`] mapped 1:1
//! onto [`vike_model::StreamStatus`] at the sink boundary (the polymarket/crypto-depth idiom). The bar poll is a REST
//! retry beside it, not a session, and waits the same row's redial after a failed fetch.
//!
//! ## The heartbeat is the watchdog's clock
//! The pricing stream interleaves `{"type":"HEARTBEAT"}` frames (~every 5 s) with `PRICE` frames.
//! Liveness splits exactly as the §B net-hardening conventions split it:
//! - **Transport**: ANY line (price or heartbeat) proves the socket alive. The idle bound is the
//!   market lane's `idle_threshold` (4 missed ~5 s heartbeats) — ureq's `timeout_per_read`, which
//!   bounds each transport read WAIT (it restarts per read), so a healthy heartbeating stream never
//!   trips it while a silently dead socket's first timed-out read trips the driver's idle watchdog
//!   (the pump row's `idle_threshold`, the same bound) → gap + reconnect. Same value as the
//!   transactions stream's own idle bound in [`crate::execution::user_data`].
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

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bridge_core::data_client::context::DataClientContext;
#[cfg(doc)]
use vike_bridge_core::stream_health::StreamHealth;
use vike_data::{
    DataClient, DataClientError, DataClientRegistry, DataClientSink, DataClientSubscriptionId,
    require_live_verb,
};
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_venues::DataClientVerb;

use crate::config::OandaConfig;
use crate::history::candles::granularity;
use crate::refusals::OandaFeedRefusal;

mod bar;
pub mod decode;
mod tick;

#[cfg(doc)]
use bar::bar_poll_cadence;
use bar::bars_main;
use bar::seed::{Seed, Want, seed_main};
use tick::quotes_main;

const VENUE: &str = "oanda";

/// §B data-freshness threshold for the quote lane, structured after the polymarket feed's
/// measured Book/Quotes floor (see `crates/bridges/polymarket/src/feed/mod.rs`'s
/// `FRESHNESS_THRESHOLD_BOOK` for the tuning argument): majors tick sub-second in session, so
/// only a genuinely quiet stream trips — and off-session (nights near the daily rollover,
/// weekends) the disclosure is accurate rather than false. Disclose-only, never an auto-action.
const FRESHNESS_THRESHOLD_QUOTES: Duration = Duration::from_secs(300);

/// A subscription refused before any thread exists: its refusal, as the shared seam's text.
fn refused(refusal: OandaFeedRefusal) -> DataClientError {
    DataClientError::Subscribe(refusal.to_string())
}

// --- Feeds (the DataClient) -------------------------------------------------------------------

/// All live OANDA feed threads, keyed by [`DataClientSubscriptionId`] via the shared [`DataClientRegistry`]
/// (one stop flag + one join handle per subscription; nothing detached — the
/// deterministic-teardown rule). Teardown latency note: a quote thread observes its stop flag
/// between lines, so joining waits for the next heartbeat (~5 s) or the idle bound (the market
/// lane's `idle_threshold`) worst-case — deterministic, not instant, exactly like the exec-side
/// transactions stream.
pub struct Feeds {
    sink: Arc<dyn DataClientSink>,
    /// Operator-facing status, state and words (shared with the spawned pumps).
    pub status: Arc<Mutex<DataClientStatus>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    config: Option<OandaConfig>,
    registry: DataClientRegistry,
    /// The ask line of the REST first-price lane (`seed`), its thread started by the first quote
    /// subscription. `None` until then, and after the OS refused the thread: seeding is best effort.
    seeds: Option<Sender<Want>>,
}

impl Feeds {
    /// `sink` receives every call from every subscription this `Feeds` spawns (construct once,
    /// subscribe many); `wake` is the GUI repaint nudge — pass `|| {}` headless. Constructed
    /// UNCREDENTIALED: every `subscribe_*` refuses until [`Feeds::with_config`] threads in the
    /// venue session (absent credentials ARE the live gate — nothing here reads the environment).
    pub fn new(sink: Arc<dyn DataClientSink>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feeds {
            sink,
            // Unarmed on purpose (absent credentials ARE the live gate), not a fault.
            status: Arc::new(Mutex::new(DataClientStatus::new(
                DataClientConnectionState::Disconnected,
                "OANDA feed: no credentials configured",
            ))),
            wake: Arc::new(wake),
            config: None,
            registry: vike_bridge_core::feed_ctx::pinned_feed_registry(VENUE),
            seeds: None,
        }
    }

    /// The credentialed constructor variant: thread in the [`OandaConfig`] the composition root
    /// loaded through [`crate::config::load_oanda_config_from`] — the SAME loader (and therefore
    /// the same account's credentials) the exec side uses; never a second credential read.
    pub fn with_config(mut self, config: OandaConfig) -> Self {
        *self.status.lock().unwrap() =
            DataClientStatus::new(DataClientConnectionState::Connecting, "connecting to OANDA…");
        self.config = Some(config);
        self
    }

    fn require_config(&self, label: &str) -> Result<&OandaConfig, DataClientError> {
        self.config
            .as_ref()
            .ok_or_else(|| refused(OandaFeedRefusal::NoConfig { label: label.to_string() }))
    }

    /// Per-subscription bookkeeping via [`DataClientRegistry::spawn`]: the registry allocates the id +
    /// stop flag and owns the join handle (the MarketData affinity pin rides its spawn hook);
    /// this wrapper only assembles the thread's [`DataClientContext`], its session config as `own`. An OS thread-spawn failure is returned so
    /// the `subscribe_*` verb can map it to a [`DataClientError`].
    fn spawn(
        &mut self,
        label: &str,
        config: OandaConfig,
        body: impl FnOnce(DataClientContext<OandaConfig>) + Send + 'static,
    ) -> std::io::Result<DataClientSubscriptionId> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        self.registry.spawn(format!("feed-oanda-{label}"), move |stop| {
            body(DataClientContext { venue: VENUE, sink, status, wake, stop, own: config })
        })
    }

    /// The REST first-price lane's ask line, starting its thread on first use (one lane per `Feeds`,
    /// so a burst of subscriptions shares one request). `None` when the OS refused the thread: the
    /// quote threads then run without a seed, which is the behaviour from before the lane.
    fn seed_line(&mut self, config: &OandaConfig) -> Option<Sender<Want>> {
        if self.seeds.is_none() {
            let (line, asks) = std::sync::mpsc::channel();
            match self.spawn("seed", config.clone(), move |ctx| seed_main(asks, ctx)) {
                Ok(_) => self.seeds = Some(line),
                Err(e) => tracing::warn!(
                    venue = VENUE,
                    error = %e,
                    "could not start the REST first-price lane; quotes come from the stream only"
                ),
            }
        }
        self.seeds.clone()
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
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        require_live_verb(VENUE, DataClientVerb::Bars)?;
        let Some(gran) = granularity(interval) else {
            let (symbol, interval) = (symbol.to_string(), interval.to_string());
            return Err(refused(OandaFeedRefusal::NoGranularity { symbol, interval }));
        };
        let config = self.require_config(&format!("{symbol}@{interval} bars"))?.clone();
        let subject = format!("{symbol}@{interval}");
        let instrument = crate::names::wire_name(symbol)
            .map_err(|why| refused(OandaFeedRefusal::NotKept { subject: subject.clone(), why }))?;
        let (series, iv) = (symbol.to_string(), interval.to_string());
        self.spawn(&format!("bars-{symbol}@{interval}"), config, move |ctx| {
            bars_main(instrument, series, iv, gran, ctx)
        })
        .map_err(|e| refused(OandaFeedRefusal::ThreadRefused { subject, why: e.to_string() }))
    }

    /// Live L1 quotes from the chunked-HTTP pricing stream (see the module doc).
    fn subscribe_quotes(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        require_live_verb(VENUE, DataClientVerb::Quotes)?;
        let config = self.require_config(&format!("{symbol} quotes"))?.clone();
        let subject = format!("{symbol} quotes");
        let instrument = crate::names::wire_name(symbol)
            .map_err(|why| refused(OandaFeedRefusal::NotKept { subject: subject.clone(), why }))?;
        let series = symbol.to_string();
        let seed = self.seed_line(&config).map(Seed::new);
        self.spawn(&format!("quotes-{symbol}"), config, move |ctx| {
            quotes_main(instrument, series, ctx, seed)
        })
        .map_err(|e| refused(OandaFeedRefusal::ThreadRefused { subject, why: e.to_string() }))
    }

    fn subscribe_trades(
        &mut self,
        _symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        // Caps-driven refusal (oanda live_data.trades = false — the venue publishes no trade
        // tape): stays in lockstep with the declared table.
        require_live_verb(VENUE, DataClientVerb::Trades)?;
        unreachable!("oanda declares no live trade tape")
    }

    fn subscribe_book(
        &mut self,
        _symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        // Caps-driven refusal (oanda live_data.book = false — the pricing ladder is an
        // unsequenced top-of-book snapshot, no delta lane to record).
        require_live_verb(VENUE, DataClientVerb::Book)?;
        unreachable!("oanda declares no lossless book lane")
    }

    fn subscribe_depth(
        &mut self,
        _symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        // Caps-driven refusal (oanda live_data.depth = false), matching trades/book above.
        require_live_verb(VENUE, DataClientVerb::Depth)?;
        unreachable!("oanda declares no live depth lane")
    }

    fn unsubscribe(&mut self, id: DataClientSubscriptionId) {
        self.registry.stop_join(id);
    }

    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;

#[path = "feed_property_tests.rs"]
#[cfg(test)]
mod feed_property_tests;

#[path = "recovery_disclosure_pin.rs"]
#[cfg(test)]
mod recovery_disclosure_pin;
