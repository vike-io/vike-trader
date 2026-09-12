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
//! driver's classic 3 s sliced at [`STOP_POLL_SLICE`]), a bounded dial ([`CONNECT_TIMEOUT`]),
//! per-subscription stop+join through the shared [`FeedRegistry`], and stream-health disclosure
//! through [`vike_bridge_core::stream_health::StreamHealth`] mapped 1:1 onto
//! [`vike_data::StreamStatus`] at the sink boundary (the polymarket/crypto-depth idiom).
//!
//! ## The heartbeat is the watchdog's clock
//! The pricing stream interleaves `{"type":"HEARTBEAT"}` frames (~every 5 s) with `PRICE` frames.
//! Liveness splits exactly as the §B net-hardening conventions split it:
//! - **Transport**: ANY line (price or heartbeat) proves the socket alive. The idle bound is
//!   [`RECV_IDLE_TIMEOUT`] — ureq's `timeout_recv_body`, which for a streaming body bounds each
//!   read WAIT (the deadline is re-derived from `now` per wait, `ureq`'s `timings.rs`
//!   `next_timeout`), so a healthy heartbeating stream never trips it while a silently dead
//!   socket errors within ~one bound → gap + reconnect. Same mechanism and same value as the
//!   transactions stream's own idle bound in [`crate::stream`].
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

use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bridge_core::poller::{STOP_POLL_SLICE, sleep_stop_aware};
use vike_bridge_core::stream_health::{HealthEvent, StreamHealth};
use vike_data::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamStatus, SubscriptionId,
    require_live_verb,
};
use vike_model::{Bar, LiveVerb, QuoteTick, now_ms};

use crate::config::OandaConfig;
use crate::data::{granularity, to_oanda_instrument};
use crate::market_data::{PricingFrame, decode_pricing_frame, parse_forming_candle};
use crate::rest::OandaRest;

const VENUE: &str = "oanda";

/// Idle bound on the chunked pricing stream: 4 missed ~5 s heartbeats. The same value (and the
/// same `timeout_recv_body` mechanism) the transactions stream dials with in [`crate::stream`].
const RECV_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Bounded dial, so a black-holed route can never hold a feed thread for the OS's own SYN ladder —
/// the same ceiling `vike_bridge_core::pump_spec` pins for every on-driver venue's connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Stop-aware reconnect/retry backoff — the shared market pumps' classic fixed 3 s, walked in
/// [`STOP_POLL_SLICE`] naps so a raised stop is honoured within ~one slice.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(3);
/// §B data-freshness threshold for the quote lane, structured after the polymarket feed's
/// measured Book/Quotes floor (see `crates/bridges/polymarket/src/market_feed.rs`'s
/// `FRESHNESS_THRESHOLD_BOOK` for the tuning argument): majors tick sub-second in session, so
/// only a genuinely quiet stream trips — and off-session (nights near the daily rollover,
/// weekends) the disclosure is accurate rather than false. Disclose-only, never an auto-action.
const FRESHNESS_THRESHOLD_QUOTES: Duration = Duration::from_secs(300);
/// REST warm-up seed depth for `subscribe_bars` (the binance family seeds 1000; OANDA candle
/// fetches are per-request priced, so a lighter seed keeps the poll lane cheap).
const SEED_BARS: u32 = 300;
/// Ceiling on one incremental candle re-fetch. `count=N` returns the NEWEST N candles, so an
/// outage longer than this many bars leaves a hole the poll lane cannot repair — the
/// `GapStart`/`Live` disclosure brackets it, and repairing further back is a backfill job, not a
/// live-feed job (the same honesty as any REST-seeded feed).
const MAX_TAIL_COUNT: u32 = 500;
/// Poll-cadence clamp floor: bounds one bar subscription to ≤0.2 req/s against the venue's
/// documented 120 req/s per-token REST budget (shared with the exec side — negligible).
const MIN_POLL: Duration = Duration::from_secs(5);
/// Poll-cadence clamp ceiling: even a daily/weekly series re-polls once a minute, so a close is
/// never disclosed more than a minute late.
const MAX_POLL: Duration = Duration::from_secs(60);
/// Pacing fallback for the intervals `vike_model::interval_ms` cannot express (`1w`/`1mo`):
/// pace them as a day. Over-fetch-safe by construction — assuming a SHORTER interval only ever
/// raises [`tail_count`], never lowers it.
const FALLBACK_PACE_MS: i64 = 86_400_000;

/// Map the neutral [`HealthEvent`] onto the `vike_data::StreamStatus` disclosure vocabulary
/// (1:1) — mirrors the same-named fn in the polymarket/crypto-depth feeds; kept per-venue because
/// `vike-bridge-core` is deliberately `vike-data`-free.
fn health_to_stream_status(ev: HealthEvent) -> StreamStatus {
    match ev {
        HealthEvent::Gap { at_ts_ms } => StreamStatus::GapStart { at_ts_ms },
        HealthEvent::Live { gap_started_ts_ms } => StreamStatus::Live { gap_started_ts_ms },
        HealthEvent::Stale { newest_data_ts_ms, now_ms } => {
            StreamStatus::Stale { newest_data_ts_ms, now_ms }
        }
    }
}

// --- The generic stop-aware chunked-line pump (pure lifecycle; I/O injected) ------------------

/// One lifecycle event out of [`pump_lines`] — the seam that keeps the reconnect loop testable
/// without a socket (the scripted-dial tests below drive it with in-memory readers).
enum PumpEvent<'a> {
    /// A dial succeeded; lines follow.
    Connected,
    /// One non-empty, trimmed line.
    Line(&'a str),
    /// A dial failed (transport error or a non-2xx stream open, pre-described by the dialer).
    ConnectFailed(&'a str),
    /// The stream ended or errored mid-read (idle bound included) — a reconnect follows.
    Disconnected,
}

/// Stop-aware dial → read-lines → backoff → reconnect, until `stop`. The chunked-HTTP twin of
/// the shared WS market pump's lifecycle: `backoff` is walked in [`STOP_POLL_SLICE`] naps after
/// BOTH a failed dial and a disconnect (the second is a deliberate delta from
/// [`crate::stream::stream_transactions`], which redials a dropped stream immediately — a server
/// close-on-accept loop would spin hot there).
fn pump_lines<R: BufRead>(
    stop: &AtomicBool,
    backoff: Duration,
    mut dial: impl FnMut() -> Result<R, String>,
    mut on_event: impl FnMut(PumpEvent<'_>),
) {
    while !stop.load(Ordering::Relaxed) {
        let reader = match dial() {
            Ok(r) => r,
            Err(e) => {
                on_event(PumpEvent::ConnectFailed(&e));
                if sleep_stop_aware(stop, backoff, STOP_POLL_SLICE) {
                    return;
                }
                continue;
            }
        };
        on_event(PumpEvent::Connected);
        for line in reader.lines() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(line) = line else { break }; // idle bound / disconnect → reconnect
            let line = line.trim();
            if !line.is_empty() {
                on_event(PumpEvent::Line(line));
            }
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        on_event(PumpEvent::Disconnected);
        if sleep_stop_aware(stop, backoff, STOP_POLL_SLICE) {
            return;
        }
    }
}

// --- The quote lane (pricing stream) ----------------------------------------------------------

/// PURE fold of one pricing-stream line into (quote to emit, health disclosures to emit), in
/// sink order. Any decoded venue frame — PRICE or HEARTBEAT — closes an open transport gap
/// (`Live`); only a PRICE advances the freshness clock; every decoded frame re-judges freshness
/// (`Stale` once when the socket heartbeats on while no fresh PRICE arrives, `Live` on recovery —
/// [`StreamHealth::check_freshness`] owns the episode dedup and the gap-gating).
fn fold_pricing_line(
    line: &str,
    series: &str,
    health: &mut StreamHealth,
    now_ms: i64,
) -> (Option<QuoteTick>, Vec<HealthEvent>) {
    let mut events = Vec::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return (None, events);
    };
    match decode_pricing_frame(&v) {
        PricingFrame::Quote(mut q) => {
            events.extend(health.recover());
            q.local_ts = now_ms; // machine receive time (dual-timestamp capture)
            q.symbol = series.to_string(); // relabel venue instrument → subscription series
            health.observe_data(q.ts);
            events.extend(health.check_freshness(now_ms));
            (Some(q), events)
        }
        PricingFrame::Heartbeat { .. } => {
            events.extend(health.recover());
            events.extend(health.check_freshness(now_ms));
            (None, events)
        }
        PricingFrame::Other => (None, events),
    }
}

/// Describe a failed stream open WITHOUT echoing the URL (it carries the account id). A non-2xx
/// is surfaced BY STATUS — the transactions stream's documented silent-retry trap (a bad/expired
/// token looping forever with no log, `crates/bridges/oanda/CLAUDE.md`) is answered here by
/// logging every failed dial, with the status code when there is one.
fn describe_dial_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(code) => {
            format!("HTTP {code} on stream open (401/403: bad or expired token / wrong account)")
        }
        _ => "transport error on stream open".to_string(),
    }
}

/// The streaming agent: idle-bounded reads ([`RECV_IDLE_TIMEOUT`]), bounded dial
/// ([`CONNECT_TIMEOUT`]), and — unlike the transactions stream's hand-built agent — non-2xx AS A
/// RESPONSE, so a rejected open is classified by status instead of blurring into the transport
/// arm. Hand-built for the same reason [`crate::stream`]'s is: a chunked GET cannot live under
/// `vike_bridge_core::http::blocking_agent`'s global request timeout.
fn streaming_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(CONNECT_TIMEOUT))
        .timeout_recv_body(Some(RECV_IDLE_TIMEOUT))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent()
}

/// `subscribe_quotes` thread body: hold the pricing stream, fold each line, emit quotes +
/// health disclosures (stream label `"quotes"`).
fn quotes_main(instrument: String, series: String, ctx: FeedCtx) {
    let agent = streaming_agent();
    let url = format!(
        "{}/v3/accounts/{}/pricing/stream?instruments={}",
        ctx.config.stream_base, ctx.config.account_id, instrument
    );
    let bearer = format!("Bearer {}", ctx.config.api_token);
    let mut health = StreamHealth::new(FRESHNESS_THRESHOLD_QUOTES.as_millis() as i64);
    ctx.set_status(format!("connecting · OANDA quotes {series}"));

    let dial = || {
        let resp = agent
            .get(&url)
            .header("Authorization", &bearer)
            .header("Accept-Datetime-Format", "UNIX")
            .call()
            .map_err(describe_dial_error)?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(describe_dial_error(ureq::Error::StatusCode(status)));
        }
        Ok(BufReader::new(resp.into_body().into_reader()))
    };

    pump_lines(&ctx.stop, RECONNECT_BACKOFF, dial, |ev| match ev {
        PumpEvent::Connected => {
            // Arm freshness per session (the depth feeds' arm-at-start): a session that then
            // receives no PRICE at all still ages toward `Stale` from here. In practice OANDA
            // replays a current-price snapshot on open, which re-seeds the clock immediately.
            health.reset_freshness(now_ms());
            ctx.set_status(format!("LIVE · OANDA quotes {series}"));
        }
        PumpEvent::Line(line) => {
            let (quote, events) = fold_pricing_line(line, &series, &mut health, now_ms());
            let emitted = quote.is_some() || !events.is_empty();
            for e in events {
                ctx.sink.stream_status(VENUE, &series, "quotes", health_to_stream_status(e));
            }
            if let Some(q) = quote {
                ctx.sink.quote(VENUE, &series, q);
            }
            if emitted {
                (ctx.wake)();
            }
        }
        PumpEvent::ConnectFailed(e) => {
            if let Some(g) = health.enter_gap(now_ms()) {
                ctx.sink.stream_status(VENUE, &series, "quotes", health_to_stream_status(g));
            }
            tracing::warn!(
                venue = VENUE,
                symbol = %series,
                error = %e,
                "pricing-stream open failed; backing off"
            );
            ctx.set_status(format!("OANDA quotes {series}: {e}; reconnecting"));
        }
        PumpEvent::Disconnected => {
            if let Some(g) = health.enter_gap(now_ms()) {
                ctx.sink.stream_status(VENUE, &series, "quotes", health_to_stream_status(g));
            }
            ctx.set_status(format!("OANDA quotes {series}: stream ended; reconnecting"));
        }
    });
}

// --- The bar lane (REST poll) -----------------------------------------------------------------

/// Poll cadence for one bar subscription: `interval / 12`, clamped to
/// [[`MIN_POLL`], [`MAX_POLL`]] — a 1m series re-polls every 5 s (forming updates + a close
/// disclosed within seconds), a 1h+ series never faster than once a minute.
fn bar_poll_cadence(interval_ms: i64) -> Duration {
    let ms = (interval_ms / 12).clamp(MIN_POLL.as_millis() as i64, MAX_POLL.as_millis() as i64);
    Duration::from_millis(ms as u64)
}

/// How many newest candles one incremental poll requests: enough to cover everything since the
/// last seen close (plus slack for the boundary candle and the forming tail), capped at
/// [`MAX_TAIL_COUNT`].
fn tail_count(elapsed_ms: i64, interval_ms: i64) -> u32 {
    let needed = elapsed_ms.max(0) / interval_ms.max(1) + 2;
    needed.clamp(2, i64::from(MAX_TAIL_COUNT)) as u32
}

/// PURE fold of one candles response against the newest close already emitted: `(fresh closed
/// bars ts-ascending, the forming candle if any, the advanced watermark)`.
fn fold_candles_response(
    v: &serde_json::Value,
    last_closed_ts: i64,
) -> (Vec<Bar>, Option<Bar>, i64) {
    let fresh: Vec<Bar> =
        crate::data::parse_candles(v).into_iter().filter(|b| b.ts > last_closed_ts).collect();
    let new_last = fresh.last().map_or(last_closed_ts, |b| b.ts);
    (fresh, parse_forming_candle(v), new_last)
}

/// `subscribe_bars` thread body: REST seed, then poll-on-cadence. Newly-complete candles ride
/// the lossless `close_bar` lane; the forming candle rides the conflating `forming_bar` lane;
/// the newest price snapshot rides `bar_close_tick` (candle-close semantics — OANDA has no mark
/// stream). A failed poll discloses `GapStart` (stream label = the interval); the next
/// successful poll emits the caught-up closes FIRST and `Live` after them, so the lossless lane
/// is already repaired when consumers see the recovery (the bar-lane `StreamStatus` contract).
fn bars_main(instrument: String, series: String, interval: String, gran: &str, ctx: FeedCtx) {
    let rest = OandaRest::new(ctx.config.api_token.clone());
    let pace_ms = vike_model::time::interval_ms(&interval).unwrap_or(FALLBACK_PACE_MS);
    let cadence = bar_poll_cadence(pace_ms);
    // Freshness is not armed on this lane: the poll IS the clock, so "no fresh data" is either a
    // failed poll (→ the gap half below) or the venue honestly serving no new candle.
    let mut health = StreamHealth::new(i64::MAX);
    let mut last_closed_ts = 0_i64;
    let mut seeded = false;
    // ONE spelling for this lane's healthy text, written from TWO places (the seed and the
    // recovery edge below) — so the two cannot drift into a pair of strings that alternate on the
    // shared mutex.
    let live_status = format!("LIVE · OANDA bars {series}@{interval}");
    ctx.set_status(format!("connecting · OANDA bars {series}@{interval}"));

    while !ctx.stop.load(Ordering::Relaxed) {
        let count = if seeded { tail_count(now_ms() - last_closed_ts, pace_ms) } else { SEED_BARS };
        let query = format!("granularity={gran}&count={count}&price=M");
        let path = format!("/v3/instruments/{instrument}/candles");
        match rest.get(&ctx.config.rest_base, &path, &query) {
            Ok(v) => {
                let (fresh, forming, new_last) = fold_candles_response(&v, last_closed_ts);
                last_closed_ts = new_last;
                if !seeded {
                    seeded = true;
                    ctx.sink.seed_bars(VENUE, &series, &interval, fresh);
                    ctx.set_status(live_status.clone());
                } else {
                    for b in fresh {
                        ctx.sink.close_bar(VENUE, &series, &interval, b);
                    }
                }
                if let Some(f) = forming {
                    ctx.sink.bar_close_tick(VENUE, &series, f.close, f.ts);
                    ctx.sink.forming_bar(VENUE, &series, &interval, f);
                }
                if let Some(l) = health.recover() {
                    ctx.sink.stream_status(VENUE, &series, &interval, health_to_stream_status(l));
                    // ⚠ **THE STATUS STRING'S RECOVERY EDGE — the second half of the same fact,
                    // and it was missing.** The healthy write above sits inside `if !seeded`, so
                    // once a poll had failed and written `"… poll failed (HTTP …); retrying"`,
                    // every LATER successful poll took the `else` branch and rewrote nothing: the
                    // string latched a fault permanently, exactly the shape that suppressed
                    // bybit's reconcile leg for 42 hours on the CI box — and strictly worse, because
                    // this lane has no session boundary to hang a disclosure on and shares one
                    // last-writer-wins mutex with `quotes_main`, so a latched bars string can mask
                    // a live quotes lane. `vike_bridge_core::market_pump`'s `SessionStatus::Live`
                    // cured that class for every venue on the shared driver; oanda is `OwnPump` in
                    // `vike_bridge_core::pump_spec` and the driver cannot reach it, so it is cured
                    // HERE, on the gap-close edge this lane already computes for its typed
                    // channel. Edge-triggered rather than per-poll for the same reason the typed
                    // disclosure is: nothing recovered if nothing was broken.
                    //
                    // `LiveFeeds::Oanda` returns no `recon_feed_statuses` row today, so nothing
                    // health-gated on this string — but that method's doc states what would EARN
                    // oanda one, and `crates/vike-ops/tests/feed_success_disclosure_gate.rs`'s
                    // `OWN_PUMP_RESIDUALS` is where this venue's verdict is recorded, because that
                    // gate is structurally blind to every `OwnPump` venue.
                    ctx.set_status(live_status.clone());
                }
                (ctx.wake)();
            }
            Err(e) => {
                if let Some(g) = health.enter_gap(now_ms()) {
                    ctx.sink.stream_status(VENUE, &series, &interval, health_to_stream_status(g));
                }
                tracing::warn!(
                    venue = VENUE,
                    symbol = %series,
                    interval = %interval,
                    status = e.status,
                    "candle poll failed; retrying"
                );
                ctx.set_status(format!(
                    "OANDA bars {series}@{interval}: poll failed (HTTP {}); retrying",
                    e.status
                ));
                if sleep_stop_aware(&ctx.stop, RECONNECT_BACKOFF, STOP_POLL_SLICE) {
                    return;
                }
                continue;
            }
        }
        if sleep_stop_aware(&ctx.stop, cadence, STOP_POLL_SLICE) {
            return;
        }
    }
}

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
/// between lines, so joining waits for the next heartbeat (~5 s) or the idle bound
/// ([`RECV_IDLE_TIMEOUT`]) worst-case — deterministic, not instant, exactly like the exec-side
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
    /// the same store-backed var map) the exec side uses; never a second credential read.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use vike_data::NoopSink;

    fn health(threshold_ms: i64) -> StreamHealth {
        StreamHealth::new(threshold_ms)
    }

    fn price_line(ts_secs: &str) -> String {
        format!(
            r#"{{"type":"PRICE","time":"{ts_secs}","instrument":"EUR_USD",
                "bids":[{{"price":"1.09000","liquidity":1000000}}],
                "asks":[{{"price":"1.09010","liquidity":1000000}}]}}"#
        )
    }

    // --- fold_pricing_line: the heartbeat→freshness contract ---------------------------------

    #[test]
    fn a_price_line_yields_a_relabelled_stamped_quote() {
        let mut h = health(300_000);
        let (q, events) = fold_pricing_line(&price_line("100.000"), "eurusd", &mut h, 100_500);
        let q = q.expect("price → quote");
        assert_eq!(q.symbol, "eurusd", "venue instrument relabelled to the subscription series");
        assert_eq!(q.local_ts, 100_500, "receive time stamped by the fold");
        assert_eq!(q.ts, 100_000);
        assert!(events.is_empty(), "fresh data on a healthy stream discloses nothing");
    }

    #[test]
    fn heartbeats_keep_transport_alive_but_age_to_stale_once() {
        let mut h = health(300_000);
        // t=0: a real price seeds the freshness clock
        let (q, ev) = fold_pricing_line(&price_line("0.000"), "eurusd", &mut h, 0);
        assert!(q.is_some() && ev.is_empty());
        // heartbeats keep flowing: fresh at +200s, stale at +301s — disclosed ONCE
        let hb = r#"{"type":"HEARTBEAT","time":"200.000"}"#;
        let (q, ev) = fold_pricing_line(hb, "eurusd", &mut h, 200_000);
        assert!(q.is_none() && ev.is_empty());
        let (_, ev) = fold_pricing_line(hb, "eurusd", &mut h, 301_000);
        assert_eq!(ev, vec![HealthEvent::Stale { newest_data_ts_ms: 0, now_ms: 301_000 }]);
        let (_, ev) = fold_pricing_line(hb, "eurusd", &mut h, 400_000);
        assert!(ev.is_empty(), "an open stale episode is not re-disclosed");
        // a fresh price closes the episode
        let (q, ev) = fold_pricing_line(&price_line("401.000"), "eurusd", &mut h, 401_000);
        assert!(q.is_some());
        assert_eq!(ev, vec![HealthEvent::Live { gap_started_ts_ms: Some(301_000) }]);
    }

    #[test]
    fn the_first_line_after_a_gap_recovers_even_a_heartbeat() {
        let mut h = health(300_000);
        assert!(h.enter_gap(50).is_some());
        let (q, ev) = fold_pricing_line(r#"{"type":"HEARTBEAT","time":"1.0"}"#, "s", &mut h, 60);
        assert!(q.is_none());
        assert_eq!(ev, vec![HealthEvent::Live { gap_started_ts_ms: Some(50) }]);
    }

    #[test]
    fn garbage_and_other_frames_do_nothing() {
        let mut h = health(300_000);
        for line in ["not json", r#"{"type":"UNKNOWN"}"#, ""] {
            let (q, ev) = fold_pricing_line(line, "s", &mut h, 0);
            assert!(q.is_none() && ev.is_empty(), "line {line:?}");
        }
        // …and an un-decodable line after a gap does NOT recover it (no venue frame was seen)
        let _ = h.enter_gap(10);
        let (_, ev) = fold_pricing_line("not json", "s", &mut h, 20);
        assert!(ev.is_empty() && h.in_gap());
    }

    // --- pump_lines: the scripted-dial reconnect/backoff lifecycle ---------------------------

    /// Drive the pump with a script of dial outcomes; record the event tags in order. The pump
    /// is stopped by the script running dry (the dial raises the stop flag).
    fn run_scripted(script: Vec<Result<&'static str, String>>) -> Vec<String> {
        let stop = AtomicBool::new(false);
        let mut dials = script.into_iter();
        let log = std::cell::RefCell::new(Vec::new());
        let dial = || match dials.next() {
            Some(Ok(body)) => Ok(Cursor::new(body.as_bytes().to_vec())),
            Some(Err(e)) => Err(e),
            None => {
                stop.store(true, Ordering::Relaxed);
                Err("script over".into())
            }
        };
        pump_lines(&stop, Duration::ZERO, dial, |ev| {
            log.borrow_mut().push(match ev {
                PumpEvent::Connected => "connect".to_string(),
                PumpEvent::Line(l) => format!("line:{l}"),
                PumpEvent::ConnectFailed(e) => format!("fail:{e}"),
                PumpEvent::Disconnected => "disconnect".to_string(),
            });
        });
        log.into_inner()
    }

    #[test]
    fn pump_delivers_trimmed_lines_and_reconnects_after_disconnect_and_dial_failure() {
        let got = run_scripted(vec![
            Ok("a\n\n  b  \n"), // blank skipped, whitespace trimmed
            Err("HTTP 401 on stream open".into()),
            Ok("c\n"),
        ]);
        assert_eq!(
            got,
            vec![
                "connect",
                "line:a",
                "line:b",
                "disconnect",
                "fail:HTTP 401 on stream open",
                "connect",
                "line:c",
                "disconnect",
                "fail:script over",
            ]
        );
    }

    #[test]
    fn a_stop_raised_during_backoff_ends_the_pump() {
        // Stop raised by the dial itself (the script-over arm) — the pump must return without
        // another dial; with a REAL backoff the sleep is stop-aware (sleep_stop_aware returns
        // true within ~one slice), covered by vike-bridge-core's own poller tests.
        let got = run_scripted(vec![]);
        assert_eq!(got, vec!["fail:script over"]);
    }

    #[test]
    fn a_stop_raised_mid_stream_stops_before_the_next_line() {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_in = Arc::clone(&stop);
        let mut seen = Vec::new();
        let mut dialed = false;
        pump_lines(
            &stop,
            Duration::ZERO,
            move || {
                assert!(!dialed, "no redial after a mid-stream stop");
                dialed = true;
                Ok(Cursor::new(b"one\ntwo\nthree\n".to_vec()))
            },
            |ev| {
                if let PumpEvent::Line(l) = ev {
                    seen.push(l.to_string());
                    stop_in.store(true, Ordering::Relaxed); // raise on the FIRST line
                }
            },
        );
        assert_eq!(seen, vec!["one"], "reads stop at the flag, mid-body");
    }

    // --- the bar lane's pure pieces ----------------------------------------------------------

    #[test]
    fn bar_poll_cadence_clamps_to_the_declared_window() {
        assert_eq!(bar_poll_cadence(60_000), MIN_POLL); // 1m → floor
        assert_eq!(bar_poll_cadence(300_000), Duration::from_secs(25)); // 5m → interval/12
        assert_eq!(bar_poll_cadence(3_600_000), MAX_POLL); // 1h+ → ceiling
        assert_eq!(bar_poll_cadence(FALLBACK_PACE_MS), MAX_POLL);
    }

    #[test]
    fn tail_count_covers_the_outage_and_caps() {
        assert_eq!(tail_count(0, 60_000), 2); // steady state: boundary + forming
        assert_eq!(tail_count(60_000, 60_000), 3);
        assert_eq!(tail_count(10 * 60_000, 60_000), 12);
        assert_eq!(tail_count(i64::MAX / 2, 60_000), MAX_TAIL_COUNT); // capped
        assert_eq!(tail_count(-5, 60_000), 2, "clock skew never underflows");
    }

    #[test]
    fn fold_candles_emits_only_closes_past_the_watermark() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"candles": [
                {"complete": true,  "volume": 1, "time": "60.0",
                 "mid": {"o": "1", "h": "1", "l": "1", "c": "1.1"}},
                {"complete": true,  "volume": 1, "time": "120.0",
                 "mid": {"o": "1", "h": "1", "l": "1", "c": "1.2"}},
                {"complete": false, "volume": 1, "time": "180.0",
                 "mid": {"o": "1", "h": "1", "l": "1", "c": "1.3"}}
            ]}"#,
        )
        .unwrap();
        // seed shape: everything complete is fresh, watermark advances to the newest close
        let (fresh, forming, last) = fold_candles_response(&v, 0);
        assert_eq!(fresh.len(), 2);
        assert_eq!(last, 120_000);
        assert_eq!(forming.as_ref().map(|b| b.ts), Some(180_000));
        // steady state: nothing new past the watermark → no closes, watermark holds
        let (fresh, _, last) = fold_candles_response(&v, 120_000);
        assert!(fresh.is_empty());
        assert_eq!(last, 120_000);
        // one candle closed since
        let (fresh, _, last) = fold_candles_response(&v, 60_000);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].close, 1.2);
        assert_eq!(last, 120_000);
    }

    // --- Feeds: refusals + lifecycle (no sockets) --------------------------------------------

    fn feeds() -> Feeds {
        Feeds::new(Arc::new(NoopSink), || {})
    }

    fn dummy_config() -> OandaConfig {
        let vars: std::collections::HashMap<String, String> = [
            ("OANDA_DEMO_API_KEY".to_string(), "tok".to_string()),
            ("OANDA_DEMO_ACCOUNT_ID".to_string(), "acct-1".to_string()),
        ]
        .into();
        crate::config::load_oanda_config_from(
            vike_bridge_core::credentials::Environment::Demo,
            &vars,
        )
        .unwrap()
    }

    #[test]
    fn uncredentialed_subscribes_refuse_with_the_live_gate() {
        let mut f = feeds();
        for verdict in [
            f.subscribe_quotes("eurusd").unwrap_err(),
            f.subscribe_bars("eurusd", "1m").unwrap_err(),
        ] {
            let LiveDataError::Subscribe(msg) = verdict else {
                panic!(
                    "uncredentialed refusal is Subscribe, not Unsupported (the venue DOES \
                        serve the verb)"
                );
            };
            assert!(msg.contains("no credentials"), "{msg}");
        }
        f.shutdown(); // nothing spawned — a no-op
    }

    #[test]
    fn caps_refusals_stay_in_lockstep_with_the_declared_table() {
        // Credentialed, so the caps refusal (not the credential gate) is what answers.
        let mut f = feeds().with_config(dummy_config());
        for err in [
            f.subscribe_trades("eurusd").unwrap_err(),
            f.subscribe_book("eurusd").unwrap_err(),
            f.subscribe_depth("eurusd").unwrap_err(),
        ] {
            assert!(matches!(err, LiveDataError::Unsupported(_)), "{err}");
        }
    }

    #[test]
    fn an_interval_without_a_granularity_is_refused_before_any_thread_exists() {
        let mut f = feeds().with_config(dummy_config());
        let err = f.subscribe_bars("eurusd", "7s").unwrap_err();
        assert!(matches!(err, LiveDataError::Subscribe(ref m) if m.contains("granularity")));
        f.shutdown();
    }

    #[test]
    fn unsubscribe_of_an_unknown_id_is_a_noop() {
        let mut f = feeds();
        f.unsubscribe(SubscriptionId(41));
        f.begin_shutdown();
        f.shutdown();
    }
}

#[cfg(test)]
mod recovery_disclosure_pin {
    //! **The bars lane must rewrite its healthy string on RECOVERY, not only at seed time** — the
    //! oanda half of the 2026-09-10 feed-health latch.
    //!
    //! `bars_main` polls REST on a cadence rather than riding a session, so it has no
    //! `SessionStatus::Live` arm for `vike_bridge_core::market_pump`'s fix to reach: this venue is
    //! `OwnPump` in `vike_bridge_core::pump_spec` and calls none of the shared driver's entry
    //! points, which is exactly why `crates/vike-ops/tests/feed_success_disclosure_gate.rs`'s
    //! roster scan is structurally blind to it (its `OWN_PUMP_RESIDUALS` table is where that
    //! blindness is declared, with this venue's verdict).
    //!
    //! The defect this pins was STRICTLY WORSE than bybit's: the healthy write sat inside
    //! `if !seeded`, so after one failed poll wrote `"… poll failed (HTTP …); retrying"` — which
    //! `vike_model::feed_status::parse_feed_status` reads as `Error` — every LATER SUCCESSFUL poll
    //! took the `else` branch and rewrote nothing. Permanent, with no session boundary to hang a
    //! cure on, on a status mutex shared last-writer-wins with `quotes_main`.
    //!
    //! A behavioural test would need a live REST endpoint (`bars_main` dials one on its first
    //! line), so this is a SOURCE pin over the recovery arm — the same shape as
    //! `crates/bridges/bybit/src/market_feed.rs`'s `healthy_string_pin`, including its lesson:
    //! COMMENT lines are skipped, because the comments in this file quote the very strings being
    //! reasoned about and a scan that read them would be green on a broken tree.

    const SRC: &str = include_str!("market_feed.rs");

    /// The lines of `bars_main`'s `health.recover()` arm, comments dropped.
    fn recovery_arm() -> Vec<String> {
        let at = SRC
            .find("fn bars_main(")
            .expect("`bars_main` has been renamed — re-anchor this pin, do not delete it");
        let body = &SRC[at..];
        let start = body
            .find("health.recover()")
            .expect("the bars lane no longer computes a recovery edge — the pin cannot see it");
        let arm = &body[start..];
        let end = arm.find("(ctx.wake)();").unwrap_or(arm.len());
        arm[..end].lines().map(|l| l.trim().to_string()).filter(|l| !l.starts_with("//")).collect()
    }

    /// The recovery edge must publish the healthy STRING, not only the typed `StreamStatus`. Those
    /// are two different channels with two different consumers, and shipping only the typed half is
    /// precisely the half-fix ig had made for itself.
    #[test]
    fn a_recovered_poll_rewrites_the_healthy_status_string() {
        let arm = recovery_arm();
        assert!(
            arm.iter().any(|l| l.contains("stream_status(")),
            "the typed disclosure has left the recovery arm — this pin is now anchored on the \
             wrong block: {arm:?}"
        );
        assert!(
            arm.iter().any(|l| l.contains("set_status(live_status")),
            "the bars lane recovers its typed StreamStatus but NOT its status string, so a failed \
             poll latches an Error-reading text that no later successful poll rewrites — the \
             2026-09-10 the CI box latch, in the one venue the shared driver's fix cannot reach. Write \
             `live_status` here. Arm was: {arm:?}"
        );
    }

    /// …and the text it writes must classify as healthy, or the rewrite is cosmetic. ig's old
    /// `"IG stream up (…)"` passed a scan exactly like the one above and still parsed `Unknown`.
    #[test]
    fn the_bars_healthy_string_reads_as_connected() {
        use vike_model::feed_status::{ConnectionState, parse_feed_status};
        let s = "LIVE · OANDA bars EURUSD@1m";
        assert_eq!(parse_feed_status(s), ConnectionState::Connected, "{s}");
        // …and the failure text it replaces must NOT, or there was never anything to cure.
        assert_eq!(
            parse_feed_status("OANDA bars EURUSD@1m: poll failed (HTTP 502); retrying"),
            ConnectionState::Error,
            "the poll-failure text is what latched; if it stopped reading as Error this pin's \
             premise is gone and the whole block needs re-arguing"
        );
    }
}
