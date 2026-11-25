//! The venue-neutral live **L2 depth-feed driver**. Every venue's order-book stream shares one
//! LIFECYCLE — connect, (optionally REST-)seed a book, read frames, fold each, publish the top-N,
//! and reconnect with backoff on error/gap — while the PROTOCOL (URL, subscribe frame, frame
//! decode, resync rule) differs per venue. This module owns the lifecycle; the venue crate supplies
//! its protocol as three closures (`seed`, `decode`, `on_book`).
//!
//! Deliberately no dependency on `vike-data`: the driver never sees a `DataClientSink`. It hands each
//! [`vike_model::StreamStatus`] transition to the venue's `on_health`, which passes it to its own
//! sink's `stream_status` unchanged. The
//! per-venue decoders (`route_frame`/`apply_depth_event`/…) stay in each bridge crate.
//!
//! **The book rides behind an `Arc`.** `decode` folds through `Arc::make_mut` and `on_book` gets the
//! `Arc`, so a tick pump hands the core a refcount bump, not a deep copy of both sides
//! (`vike_execution::BookUpdate`'s perf note). A DOM lane that only reads the top N keeps no second
//! reference, so its folds never copy.
//!
//! **Dead-transport watchdog.** The read loop runs over a [`WebsocketSessionDepthStream`] seam that stamps a
//! liveness clock on EVERY inbound frame (data, server Ping, any control frame); a read timeout with
//! no frame of any kind for longer than `idle_threshold` returns `Err`, so the reconnect path
//! re-seeds. An idle trip and any transport error/gap emit [`StreamStatus::GapStart`] through `on_health`
//! (once per outage — the [`StreamHealth`] owned by [`run_depth`] dedups it across
//! reconnects), and the first book of each session emits [`StreamStatus::Live`] BEFORE that book
//! when a gap was open, so a consumer sees "recovered" ahead of the re-seeded data. It detects a
//! dead *connection*, not data starvation behind a live one.
//!
//! **Data-freshness watchdog (the data-side twin).** A reconnect whose re-subscribe silently failed
//! still answers keepalives, so the book reads live forever while frozen; a venue REPLAYING old data
//! arrives on time with stale event-times. Every [`WebsocketSessionDepthOperation::Updated`] carries the venue EVENT-TIME
//! (epoch-ms; `0` = omitted → receive-time), fed to [`StreamHealth::observe_data`] and judged
//! against the INJECTED `now_ms()`. On a read-timeout tick, ONLY after the transport idle check did
//! not trip (so the two never double-signal), [`StreamHealth::check_freshness`] emits
//! [`StreamStatus::Stale`] once when the newest update's event-time is older than the freshness
//! threshold, then [`StreamStatus::Live`] when updates resume (before that book). Recovery fires
//! ONLY when data is genuinely fresh again: an update whose OWN stamp is still past the threshold
//! keeps the episode open (`a_stale_stamped_update_while_stale_does_not_falsely_recover`). Clocking
//! off event-time assumes venue/local clocks are ~NTP-synced (the venues' 120 s thresholds dwarf
//! real skew). [`run_depth_session`] calls [`StreamHealth::reset_freshness`] with `now_ms()` at each
//! session start, which drops a prior outage's open `Stale` AND arms the freshness clock, so even a
//! session that receives ZERO updates ages toward `Stale`
//! (`a_seeded_but_dataless_session_goes_stale`); once real data lands its event-time supersedes
//! that arm floor.
//!
//! **Timed book re-seed (checksum-less venues).** None of the above catches a book that silently
//! corrupts while updates keep flowing — a dropped or mis-applied delta that does NOT break the seq
//! chain — on a venue with no book checksum (Binance/Bybit have none; OKX's CRC is intentionally not
//! checked). The optional `reseed_interval` (`None` = off) rebuilds the book from a fresh snapshot
//! each time the injected `now_ms()` advances that far, IN PLACE on the open socket: a REST seed
//! spliced onto the queued diffs by the venue's own id rule, or a WS venue's re-subscribe and its
//! new snapshot frame ([`run_depth_session`]'s doc). No reconnect, no backoff and no
//! [`StreamStatus::GapStart`], because a scheduled refresh is not an outage and must neither darken
//! the lane nor flap a health consumer's Gap/Live view; a re-seed that cannot line up ends the
//! session as a fault. "Wrong forever" becomes "wrong ≤ reseed_interval".

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_execution::{BookUpdate, QuoteUpdate, StreamStatusUpdate, TickSender, TradeUpdate};
use vike_marketdata::{L2Book, L2BookLevel, TickQuote, TickTrade};
use vike_model::StreamStatus;

use crate::json::str_f64;
use crate::stream_health::StreamHealth;
use crate::websocket::WebsocketSessionUserStreamTungstenite;
use crate::websocket_session::market::{
    WebsocketSessionMarketStream, WebsocketSessionMarketStreamTungstenite,
};
use crate::websocket_session::spec::CONNECT_10S;
use crate::websocket_session::user::WebsocketSessionUserError;
use vike_transport::websocket::configure_websocket_stream;
use vike_transport::websocket_proxy::connect_websocket;

/// Client keepalive cadence for venues that require one (Bybit/OKX drop idle clients).
const KEEPALIVE_EVERY: Duration = Duration::from_secs(15);

/// How often a depth feed that keeps faulting may SPEAK in the journal. The first fault of a streak
/// is spoken immediately; after that at most one line per window, carrying how many faults it
/// covers — so a feed reconnecting every few seconds speaks ~1,440 lines a day, not one per fault,
/// and a healthy feed that blips once speaks exactly one.
///
/// **Why a window rather than an edge.** [`StreamHealth::enter_gap`] already dedups a transport
/// outage to ONE `Gap`, the right disclosure for a HEALTH consumer, but the wrong trigger for a
/// journal line: a lane whose every cycle seeds, publishes a book (closing the gap through
/// [`StreamHealth::recover`]) and then faults is, to the health tracker, a series of separate
/// fully-recovered outages, and an edge-triggered log would write one line per cycle forever. The
/// signal an operator needs there is the RATE, which is what this window reports.
pub const FAULT_LOG_EVERY: Duration = Duration::from_secs(60);

/// The most depth diffs a REST re-seed HOLDS for its splice while its snapshot is fetched on its own
/// thread ([`run_depth_session`]'s `reseed_interval`). The fetch is bounded by the venue's seed
/// agent (`crates/bridges/binance/src/family/feed/tick/mod.rs`'s `DEPTH_SEED_TIMEOUT`, 10 s), and binance's
/// `@depth@100ms` stream sends at most ten diffs a second, so even a fetch that runs into its
/// timeout holds about 100; this is ten times that. Holding more means the stream outran any fetch
/// the venue bounds: a fault (the full reconnect), never a silent drop.
pub const RESEED_HELD_DIFFS_MAX: usize = 1024;

/// The journal voice of [`run_depth`]'s reconnect loop.
///
/// **Why it exists.** A depth lane's only other disclosure is the `StreamStatus` the venue hands
/// its sink, and in a HEADLESS process (the recorder, the datahub daemon) that
/// reaches nothing a human or `journalctl` sees — so without this a lane can reconnect-loop
/// indefinitely with an empty journal (one did for forty days at 4 % of its expected rate, found
/// only by measuring the stored series).
///
/// Owned by the feed (one per lane, across reconnects) and clocked off the SAME injected `now_ms`
/// the reseed timer and the freshness watchdog use, so it is deterministic and testable with no
/// sleeps. It is off the hot path by construction: it is touched once per SESSION (a reconnect),
/// never per frame, so nothing here goes near `vike-core`'s fold or the p99 gate.
///
/// The lane is named by its URL, which already carries the venue host and the stream name, and is
/// the one identifier [`run_depth`] holds — deliberately not a new `(venue, symbol)` parameter
/// on a driver hub already this wide.
#[derive(Debug, Default)]
pub struct WebsocketSessionDepthFaultLog {
    /// Faults in the current streak — reset by [`Self::recovered`], reported by the recovery line.
    streak: u64,
    /// Faults recorded since the last line was spoken (the count the next line carries).
    since_report: u64,
    /// When the last line was spoken, on the injected clock. `None` = nothing spoken this streak,
    /// so the next fault speaks immediately.
    last_report_ms: Option<i64>,
}

impl WebsocketSessionDepthFaultLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one session fault and speak it if this one is due. Returns whether it spoke (the
    /// return exists so a test can assert the THROTTLE without reading the journal; the journal
    /// itself is asserted separately, over this same function).
    pub fn fault(&mut self, url: &str, err: &str, now_ms: i64, every: Duration) -> bool {
        self.streak += 1;
        self.since_report += 1;
        let due = match self.last_report_ms {
            None => true,
            Some(spoken_at) => now_ms.saturating_sub(spoken_at) >= every.as_millis() as i64,
        };
        if !due {
            return false;
        }
        let faults = self.since_report;
        self.since_report = 0;
        self.last_report_ms = Some(now_ms);
        tracing::warn!(
            target: "vike_bridge_core::depth",
            url = %url,
            faults,
            streak = self.streak,
            error = %err,
            "depth feed fault — reconnecting and re-seeding; a lane that keeps faulting is \
             publishing a teleporting book, not a live one"
        );
        true
    }

    /// A session ran a full `reseed_interval` without faulting, which is the only
    /// evidence this driver has that a lane is healthy again. Speaks once if a streak was open, and
    /// returns whether it spoke.
    pub fn recovered(&mut self, url: &str) -> bool {
        if self.streak == 0 {
            return false;
        }
        let faults = self.streak;
        self.streak = 0;
        self.since_report = 0;
        self.last_report_ms = None;
        tracing::info!(
            target: "vike_bridge_core::depth",
            url = %url,
            faults,
            "depth feed healthy again after a fault streak"
        );
        true
    }
}

/// What decoding one WS frame did to the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebsocketSessionDepthOperation {
    /// the book was (re)seeded or a diff applied — publish it. Carries the venue EVENT-TIME of the
    /// update in epoch-ms (`0` = the venue supplied no usable timestamp → the driver falls back to
    /// receive-time). The data-freshness watchdog clocks staleness off this, so a frame that
    /// ARRIVES now but is STAMPED old still ages the book (catches a venue replaying stale data,
    /// which a receive-time clock would miss). `i64`, so `WebsocketSessionDepthOperation` stays `Copy`.
    Updated(i64),
    /// a sequence gap — the session must resync; the driver reconnects + re-seeds after a backoff
    Gap,
    /// nothing to publish (ack / pong / stale / non-book frame / not yet seeded)
    Ignored,
}

/// How a depth session ended CLEANLY — the `Ok` arm of [`run_depth_session`]. A transport FAULT
/// (socket error, sequence gap, idle trip, or a timed re-seed that could not line up) stays in the
/// `Err` arm, so `?` still short-circuits faults. The planned timed re-seed does NOT end a session:
/// it re-seeds in place, on the open socket ([`run_depth_session`]'s `reseed_interval`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebsocketSessionDepthOutcome {
    /// `stop` was raised — [`run_depth`] exits its reconnect loop (deterministic teardown).
    Stopped,
}

/// A read/write seam over one depth-WS session. Production wraps tungstenite
/// ([`WebsocketSessionMarketStreamTungstenite`], the market pump's stamped stream); scripted tests drive a canned
/// frame queue to exercise the idle watchdog and recovery paths with zero real-time sleeps. Mirrors
/// [`crate::websocket_session::market::WebsocketSessionMarketStream`]: `read_frame` returns exactly one TEXT frame per call;
/// Ping/Pong/other control frames are auto-handled INSIDE the impl (never surfaced), and each stamps
/// the liveness clock, so a connection kept alive purely by control-frame keepalive is not falsely
/// declared idle.
pub trait WebsocketSessionDepthStream {
    /// One decoded TEXT frame, or a read-timeout tick ([`WebsocketSessionUserError::Timeout`] — poll `stop`, then
    /// the idle watchdog), or a close ([`WebsocketSessionUserError::Closed`]).
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError>;
    /// Send an app-level text frame (subscribe, or a venue keepalive ping).
    fn send_text(&mut self, s: &str) -> Result<(), WebsocketSessionUserError>;
    /// Time since the last inbound frame of ANY kind (data, Ping, or other control frame). The
    /// default `ZERO` (never idle) suits scripted seams that don't model wall-clock time; they
    /// override it to drive the idle path deterministically.
    fn since_last_frame(&self) -> Duration {
        Duration::ZERO
    }
}

/// The depth driver reads the market pump's stamped stream: a Ping is auto-ponged and looped past,
/// a Close is surfaced as `Err(Closed)`, and every received frame stamps the idle watchdog's clock.
impl WebsocketSessionDepthStream for WebsocketSessionMarketStreamTungstenite {
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        WebsocketSessionMarketStream::read_frame(self)
    }

    fn send_text(&mut self, s: &str) -> Result<(), WebsocketSessionUserError> {
        WebsocketSessionMarketStream::send_text(self, s)
    }

    fn since_last_frame(&self) -> Duration {
        WebsocketSessionMarketStream::since_last_frame(self)
    }
}

/// Infer an instrument's price tick from a (dense) depth snapshot: the smallest positive gap
/// between adjacent price levels. Reliable for an actively-quoted symbol (some adjacent pair is
/// exactly one tick apart); for a genuinely thin/sparse book the min gap can be an integer MULTIPLE
/// of the true tick — seed from the venue `PRICE_FILTER` tickSize there instead. Falls back to 0.01
/// (fewer than two distinct levels). ONE shared copy so every venue quantizes the same way.
///
/// The gap is the f64 SUBTRACTION of two decimal prices, so it carries their rounding
/// (`3000.05 - 3000.0` is `0.0500000000001819`), and a book on that grid reads 3000.05 as
/// 3000.050000010914. So the gap is read as the shortest decimal (`n / 10^k`, `k` up to 15) within
/// that rounding, `4 × f64::EPSILON` of the larger price: `0.05`. On it the book's
/// `vike_marketdata::TickFraction` reads every level as its decimal price. A gap that is no such
/// decimal is returned as it is.
pub fn infer_tick_size(bids: &[L2BookLevel], asks: &[L2BookLevel]) -> f64 {
    // A non-finite price (`"NaN"` parses as f64) has no place in a gap; `partial_cmp` over NaN is
    // also not a total order, which a long-slice `sort_by` is allowed to panic on.
    let mut prices: Vec<f64> =
        bids.iter().chain(asks).map(|l| l.price).filter(|p| p.is_finite()).collect();
    prices.sort_by(f64::total_cmp);
    let (mut min, mut noise) = (f64::INFINITY, 0.0);
    for w in prices.windows(2) {
        let d = w[1] - w[0];
        if d > 1e-9 && d < min {
            min = d;
            noise = 4.0 * f64::EPSILON * w[0].abs().max(w[1].abs());
        }
    }
    if !min.is_finite() {
        return 0.01;
    }
    for denominator in (0..16).filter_map(vike_math::powers::power_of_ten) {
        let numerator = (min * denominator).round();
        if numerator >= 1.0 && (numerator / denominator - min).abs() <= noise {
            return numerator / denominator;
        }
    }
    min
}

/// Parse one side of a venue book (`[["px","qty"], …]`, each number a decimal STRING) into
/// [`L2BookLevel`]s (qty 0 = remove, per [`L2Book`]). A level whose price or size is not a string that
/// parses as `f64` — including a JSON NUMBER, which none of these venues sends — is skipped, and an
/// absent or non-array `side` is an empty side.
///
/// ONE shared copy, so every venue that spells its book this way decodes it the same way.
/// Each number is read through [`crate::json::str_f64`] (string-only), deliberately NOT
/// [`crate::json::json_number`]: that accepts a JSON number too, and swapping it in would change which
/// frames decode.
#[must_use]
pub fn parse_levels(side: Option<&Value>) -> Vec<L2BookLevel> {
    side.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|level| {
                    let l = level.as_array()?;
                    Some(L2BookLevel { price: str_f64(l.first())?, qty: str_f64(l.get(1))? })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One WS depth session over an already-connected `stream`: optionally send `subscribe`, `seed` the
/// book (a REST snapshot for a snapshot-first venue like Binance; `None` for a WS-seeded venue like
/// Bybit/OKX), then read frames and fold each via `decode`, calling `on_book` on every
/// [`WebsocketSessionDepthOperation::Updated`]. Returns `Ok(())` on a requested stop; `Err` on a socket failure, a
/// [`WebsocketSessionDepthOperation::Gap`], OR the idle watchdog tripping (no inbound frame for `idle_threshold`) — either
/// way [`run_depth`] opens a transport gap ([`StreamStatus::GapStart`]), backs off, then reconnects (a
/// fresh session re-seeds). The FIRST book of the session emits [`StreamStatus::Live`] just BEFORE it
/// WHEN a gap was open (via [`StreamHealth::recover`]) — closing a prior outage's gap ahead of the
/// re-seeded data; a first connect with no open gap discloses nothing. `stop` is polled every read
/// tick AND once more just BEFORE `seed` — so a stop that landed during the (bounded) dial costs the
/// dial and nothing after it, instead of the dial plus a fresh REST round trip. Teardown is
/// therefore deterministic within one read tick, plus at most ONE in-flight `seed`, which the venue
/// bounds (`crates/bridges/binance/src/family/feed/tick/mod.rs`'s `DEPTH_SEED_TIMEOUT` for binance/aster; a
/// WS-seeded venue passes a `seed` that returns `None` and takes its snapshot off the wire).
/// `keepalive`, if set, is sent every ~15 s (Bybit/OKX drop clients that don't ping).
///
/// `stream_health` is owned by [`run_depth`] ACROSS reconnects (so the transport `Gap`/`Live` is
/// deduped to one pair per outage); each session resets AND arms only its FRESHNESS half up front
/// ([`StreamHealth::reset_freshness`] with `now_ms()`), so even a dataless session ages toward
/// `Stale`. The DATA-side watchdog runs on the same read-timeout tick but ONLY after the transport
/// idle check passes (the module doc). `now_ms` is injected (production wall-clock; tests a
/// controllable counter) so this is testable with zero sleeps.
///
/// `reseed_interval`, if `Some`, is the timed book re-seed, done IN PLACE on the open socket (owner
/// ruling 2026-11-11): every `reseed_interval` of the injected clock, even on a perfectly healthy,
/// in-sync stream, the book is rebuilt from a fresh snapshot, so a silently corrupted book self-heals
/// within the interval while the consumers keep their quotes, trades and books and the link word
/// stays as it was (no gap opens, none is closed). The snapshot comes the way the session's first
/// one came, by the venue's own rule:
///
/// - **`resubscribe` is empty** (a REST-seeded venue: binance/aster): `seed` runs on its OWN scoped
///   thread (owner ruling 2026-11-11, "variant B") while this thread keeps reading the socket: quotes
///   and trades go out at once, and the current book keeps folding and publishing. Every frame
///   `decode` answers [`WebsocketSessionDepthOperation::Updated`] meanwhile is also HELD (at most [`RESEED_HELD_DIFFS_MAX`]).
///   When the snapshot answers, the held frames are replayed onto it through the same `decode`, the
///   venue's own sync rule (stale ones dropped, the first one straddling the snapshot's id), and the
///   spliced book replaces the current one; it is published at once if a held diff lined up, else
///   with the first live diff that does. ⚠ So `decode` must not EMIT anything for a frame it answers
///   `Updated` (binance's book frames carry no quote or trade): a replay would emit it twice.
/// - **`resubscribe` is not empty** (a WS-seeded venue: bybit, okx's `books`, deribit): the driver
///   sends it (the venue's unsubscribe and subscribe of the book channel) on the same socket and
///   clears the book, which `decode` rebuilds from the fresh snapshot frame as at the session's start.
///
/// Every way a re-seed cannot line up ends the session for the full reconnect (gap, backoff, dial):
/// the fetch fails (`seed` answers `None`, or panics), a held or a live diff gaps, more than the cap
/// is held, a send fails, or no fresh snapshot has come within `idle_threshold` of asking. A stop or
/// a fault while a fetch is in flight waits for it as the session's first seed would (the venue
/// bounds it: `crates/bridges/binance/src/family/feed/tick/mod.rs`'s `DEPTH_SEED_TIMEOUT`).
///
/// The session's FIRST seed stays on this thread, before the loop: its book is published before any
/// frame is read, and the transport `Live` rides it (`a_seeded_but_dataless_session_goes_stale`
/// pins that order).
///
/// `on_reseeded` is told each time the timer fires (the proof the session lasted an interval).
/// Clocked off the injected `now_ms` (never `Instant`) so it is deterministic; `None` disables it.
#[expect(clippy::too_many_arguments)] // driver hub: subscribe/keepalive/resubscribe + 5 closures + health + stop + idle + reseed + clock
pub fn run_depth_session<S: WebsocketSessionDepthStream>(
    stream: &mut S,
    subscribe: Option<&str>,
    keepalive: Option<&str>,
    resubscribe: &[&str],
    seed: &(dyn Fn() -> Option<L2Book> + Sync),
    decode: &mut dyn FnMut(&str, &mut Option<Arc<L2Book>>) -> WebsocketSessionDepthOperation,
    on_book: &mut dyn FnMut(&Arc<L2Book>),
    on_health: &mut dyn FnMut(StreamStatus),
    on_reseeded: &mut dyn FnMut(),
    stream_health: &mut StreamHealth,
    stop: &AtomicBool,
    idle_threshold: Duration,
    reseed_interval: Option<Duration>,
    now_ms: &dyn Fn() -> i64,
) -> Result<WebsocketSessionDepthOutcome, Box<dyn std::error::Error>> {
    if let Some(sub) = subscribe {
        stream.send_text(sub).map_err(|e| format!("depth subscribe failed: {e:?}"))?;
    }
    // Session-start clock read ONCE (injected `now_ms`), reused for BOTH the freshness arm below and
    // the periodic reseed timer in the loop — so both age off the same injected clock deterministically.
    let session_start_ms = now_ms();
    let reseed_interval_ms = reseed_interval.map(|d| d.as_millis() as i64);
    // Per-session FRESHNESS reset + ARM: a new session re-seeds fresh data, so any stale episode left
    // open by the prior outage is dropped here WITHOUT emitting a spurious recovery, AND the freshness
    // clock is armed at `now_ms()` so a session that then receives ZERO book updates (a subscribe that
    // silently succeeds but sends nothing behind a live keepalive'd socket — worst on REST-seeded
    // Binance, where the seed closes the transport gap so nothing else catches the frozen book) still
    // ages toward `Stale` from session start, rather than a dataless session tripping nothing.
    // Transport gap state is deliberately untouched — the `StreamHealth` owned by `run_depth`
    // carries the open gap across the reconnect so the first book below can close it.
    stream_health.reset_freshness(session_start_ms);

    // Recovery is disclosed exactly once per session, on the FIRST published book (the consumer
    // sees the transport `Live` before the re-seeded data). `recover()` is a no-op (returns `None`)
    // when no gap is open — a first connect discloses nothing. `publish` centralizes that guard.
    let mut first_publish = true;
    let mut publish = |book: &Arc<L2Book>,
                       on_health: &mut dyn FnMut(StreamStatus),
                       on_book: &mut dyn FnMut(&Arc<L2Book>),
                       stream_health: &mut StreamHealth| {
        if first_publish {
            if let Some(event) = stream_health.recover() {
                on_health(event);
            }
            first_publish = false;
        }
        on_book(book);
    };

    // ⚠ **Poll `stop` BEFORE the seed, not only in the read loop below.** `seed` is a REST round
    // trip on the REST-seeded venues (binance/aster — `crates/bridges/binance/src/family/feed/tick/mod.rs`'s
    // `DEPTH_SEED_TIMEOUT` bounds it), and it is the LAST thing between a session's dial and the
    // loop's first flag read. A dial can spend its whole bounded window with the flag already raised
    // behind it, so without this check a stop landing mid-dial pays the dial AND THEN starts a fresh
    // network call the caller has already asked not to happen — the identical window
    // `crates/bridges/binance/src/family/feed/tick/trades.rs`'s connect closure refuses before its warmup.
    // Held by `a_stop_raised_before_the_session_skips_the_rest_seed`.
    if stop.load(Ordering::Relaxed) {
        return Ok(WebsocketSessionDepthOutcome::Stopped);
    }
    // Seed BEFORE the read loop (REST venues) so no diff is missed in the fetch window; WS-seeded
    // venues return None here and get their snapshot as the first decoded frame.
    let mut book: Option<Arc<L2Book>> = seed().map(Arc::new);
    if let Some(b) = &book {
        publish(b, on_health, on_book, stream_health);
    }
    // The timed re-seed's clock (armed at session start, re-armed at each re-seed), and when the
    // re-seed in flight asked for its fresh snapshot (`None` = none in flight).
    let mut reseed_from_ms = session_start_ms;
    let mut reseed_asked_ms: Option<i64> = None;
    let mut last_ka = Instant::now();
    // A REST re-seed's fetch runs on a SCOPED thread (it borrows `seed`), so the scope is the loop.
    std::thread::scope(
        |scope| -> Result<WebsocketSessionDepthOutcome, Box<dyn std::error::Error>> {
            // The REST re-seed in flight, and the diffs held for its splice.
            let mut fetch: Option<std::thread::ScopedJoinHandle<'_, Option<L2Book>>> = None;
            let mut held: Vec<String> = Vec::new();
            loop {
                if stop.load(Ordering::Relaxed) {
                    return Ok(WebsocketSessionDepthOutcome::Stopped);
                }
                // Timed book re-seed (checksum-less venues), IN PLACE: once the injected clock has
                // advanced `reseed_interval`, rebuild the book from a fresh snapshot on THIS socket (the
                // doc above has the two venue shapes). This bounds a SILENTLY corrupted book (a
                // mis-applied delta that never breaks the seq chain — so no `WebsocketSessionDepthOperation::Gap`, no idle, no
                // freshness trip) to ≤ `reseed_interval` with no reconnect and no transport gap. Clocked
                // off `now_ms` (never `Instant`); `None` skips the `now_ms()` call entirely. Never while a
                // re-seed is still in flight: the deadline below judges that one.
                if reseed_asked_ms.is_none()
                    && let Some(interval_ms) = reseed_interval_ms
                    && now_ms() - reseed_from_ms >= interval_ms
                {
                    if resubscribe.is_empty() {
                        // REST-seeded: fetch on its own thread; this one keeps reading.
                        fetch = Some(scope.spawn(seed));
                    } else {
                        // WS-seeded: ask the venue for a fresh snapshot frame on this socket.
                        for frame in resubscribe {
                            stream
                                .send_text(frame)
                                .map_err(|e| format!("depth re-seed request failed: {e:?}"))?;
                        }
                        book = None;
                    }
                    reseed_asked_ms = Some(now_ms());
                    reseed_from_ms = now_ms();
                    on_reseeded();
                }
                // The REST re-seed's snapshot has answered: splice the held diffs onto it.
                if fetch.as_ref().is_some_and(|f| f.is_finished()) {
                    // A fetch that panicked answered nothing, as one that failed.
                    let fresh = fetch.take().and_then(|f| f.join().ok()).flatten();
                    let Some(fresh) = fresh else {
                        return Err(
                            "timed re-seed: the snapshot fetch failed, so reconnect to seed".into(),
                        );
                    };
                    let mut spliced = Some(Arc::new(fresh));
                    let mut lined_up = false;
                    for txt in held.drain(..) {
                        match decode(&txt, &mut spliced) {
                            WebsocketSessionDepthOperation::Updated(_) => lined_up = true,
                            WebsocketSessionDepthOperation::Gap => {
                                return Err(
                                    "timed re-seed: the held diffs do not line up with the \
                                        fresh snapshot"
                                        .into(),
                                );
                            }
                            WebsocketSessionDepthOperation::Ignored => {} // already inside the snapshot
                        }
                    }
                    book = spliced;
                    reseed_asked_ms = None;
                    if lined_up && let Some(b) = &book {
                        publish(b, on_health, on_book, stream_health);
                    }
                }
                if let Some(asked) = reseed_asked_ms
                    && now_ms() - asked > idle_threshold.as_millis() as i64
                {
                    return Err("timed re-seed: no fresh snapshot within the idle threshold".into());
                }
                // App-level keepalive on a wall-clock cadence (checked before each read; the read timeout
                // guarantees this runs at least every read tick). Kept in the driver so it fires even
                // while data flows AND while idle — the case where the venue would otherwise drop us.
                if let Some(ka) = keepalive
                    && last_ka.elapsed() >= KEEPALIVE_EVERY
                {
                    stream.send_text(ka).map_err(|e| format!("depth keepalive failed: {e:?}"))?;
                    last_ka = Instant::now();
                }
                match stream.read_frame() {
                    Ok(txt) => match decode(&txt, &mut book) {
                        WebsocketSessionDepthOperation::Updated(ts) => {
                            // Feed the venue EVENT-TIME (`ts`) to the freshness tracker, falling back to
                            // receive-time (`now_ms()`) when the venue omitted it (`ts <= 0`) — so a
                            // frame that ARRIVES now but is STAMPED old still ages the book. Then, if a
                            // stale episode is open, let `check_freshness` CLOSE it with `Live` BEFORE
                            // the book (recovery-before-data, mirroring how the transport `Live` precedes
                            // the re-seeded book). The `is_stale()` guard keeps this a RECOVERY-only step:
                            // a `Stale` opens only on a transport-alive timeout tick below, never here on
                            // an update frame carrying an OLD stamp.
                            let stamp = if ts > 0 { ts } else { now_ms() };
                            stream_health.observe_data(stamp);
                            if stream_health.is_stale()
                                && let Some(event) = stream_health.check_freshness(now_ms())
                            {
                                on_health(event);
                            }
                            if let Some(b) = &book {
                                if fetch.is_none() {
                                    reseed_asked_ms = None; // a WS re-seed's fresh snapshot built this
                                }
                                publish(b, on_health, on_book, stream_health);
                            }
                            if fetch.is_some() {
                                // A REST re-seed is in flight: hold the diff for its splice.
                                if held.len() >= RESEED_HELD_DIFFS_MAX {
                                    return Err(format!(
                                        "timed re-seed: more than {RESEED_HELD_DIFFS_MAX} diffs \
                                     queued behind the snapshot fetch"
                                    )
                                    .into());
                                }
                                held.push(txt);
                            }
                        }
                        WebsocketSessionDepthOperation::Gap => {
                            return Err("depth sequence gap — resync".into());
                        }
                        WebsocketSessionDepthOperation::Ignored => {}
                    },
                    Err(WebsocketSessionUserError::Timeout) => {
                        // Idle over ANY inbound frame (data, Ping, or other control keepalive —
                        // `since_last_frame`), so only true transport silence trips the gap.
                        if stream.since_last_frame() > idle_threshold {
                            return Err(
                                "idle timeout — no depth frame within the watchdog window".into()
                            );
                        }
                        // Transport still alive → judge DATA freshness (the failure the idle check
                        // CANNOT see: a re-subscribe that silently failed still answers keepalives, so
                        // the book freezes behind a live socket). Runs ONLY after the idle check did NOT
                        // trip, so the two watchdogs never double-signal (and `check_freshness` is
                        // internally gated to stay silent during a transport gap anyway). Discloses
                        // `Stale` once per episode; the recovery `Live` is emitted on the resuming update
                        // frame above.
                        if let Some(event) = stream_health.check_freshness(now_ms()) {
                            on_health(event);
                        }
                    }
                    Err(WebsocketSessionUserError::Closed(m)) => return Err(m.into()),
                }
            }
        },
    )
}

/// Connect + set the stop-poll read timeout, wrapped as a [`WebsocketSessionDepthStream`].
///
/// **The dial is BOUNDED — [`CONNECT_10S`], the same window the market pump spends.** A bare
/// `tungstenite::connect` applies no connect bound of any kind, so [`run_depth`]'s thread would
/// sit out the OS's own SYN ladder (~127 s on Linux defaults) on a black-holed route with `stop`
/// already raised behind it.
///
/// **It is on the recorder's teardown path.** `crates/vike-store/src/live_rec.rs`'s
/// `RecorderSink` implements `l2_snapshot` (the depth rows land under `kind=depth`), and
/// `crates/vike-datahub/src/recorder.rs` subscribes `Stream::ALL`, whose `Stream::Depth` arm calls
/// `subscribe_depth`. So this dial spends `crates/vike-datahub/src/recorder.rs`'s
/// `FEED_STOP_BUDGET_SECS` like any other.
///
/// The window is not a parameter because it is not a per-venue decision: see [`CONNECT_10S`]'s own
/// doc for why one constant serves both drivers.
fn connect_depth(
    url: &str,
    read_timeout: Duration,
) -> Result<WebsocketSessionMarketStreamTungstenite, Box<dyn std::error::Error>> {
    let socket = connect_websocket(url, None, Some(CONNECT_10S))?;
    configure_websocket_stream(&socket, read_timeout);
    Ok(WebsocketSessionMarketStreamTungstenite::new(WebsocketSessionUserStreamTungstenite(socket)))
}

/// Reconnect loop around [`run_depth_session`]: on a socket error, gap, or idle trip, open a
/// transport gap ([`StreamStatus::GapStart`], deduped to one per outage by the feed-owned [`StreamHealth`]),
/// back off (stop-aware), then reconnect and re-seed, until `stop`. A clean stop (`Ok`) ends the loop.
/// This IS the whole thread body a venue's `subscribe_depth` spawns. `backoff` throttles reconnects so
/// a persistent fault (e.g. a REST-seed that keeps failing) can't hot-loop. `idle_threshold` sizes the
/// dead-transport watchdog as a per-venue constant well above the keepalive cadence, so keepalive
/// traffic keeps a quiet book alive and only true silence trips it. `freshness_threshold` (baked into
/// the feed's `StreamHealth`) + the injected `now_ms` clock size and drive the DATA-freshness twin
/// inside [`run_depth_session`]: a book that stops updating behind a still-live socket for longer than
/// `freshness_threshold` emits [`StreamStatus::Stale`]/[`StreamStatus::Live`], which the venue discloses next to the
/// transport `GapStart`. The one `StreamHealth` lives for the feed's
/// whole lifetime (ACROSS reconnects), which is what gives the transport `GapStart`/`Live` its one-pair-
/// per-outage dedup; its freshness half is reset per session inside `run_depth_session`.
///
/// `reseed_interval`, if `Some`, is the timed book re-seed, done in place on the open socket inside
/// [`run_depth_session`] (with `resubscribe`, a WS-seeded venue's unsubscribe and subscribe frames
/// of the book channel, empty for a REST-seeded one): no reconnect, no backoff, no transport gap,
/// so the lane's consumers see neither a dark spell nor a `GapStart`/`Live` flap. Only a re-seed
/// that cannot line up its ids (or gets no fresh snapshot) ends the session, and then it is a fault
/// like any other: gap, backoff, reconnect. `None` disables it; a venue with no book checksum opts
/// in to bound silent book corruption to ≤ the interval.
///
/// **It also SPEAKS.** Every fault goes through a feed-owned [`WebsocketSessionDepthFaultLog`] as well as the health
/// disclosure: the first fault of a streak is a `tracing::warn!` naming the URL and the error, then
/// at most one line per [`FAULT_LOG_EVERY`] carrying how many faults it covers, and an `info!` when
/// a session survives to a scheduled re-seed. Read [`WebsocketSessionDepthFaultLog`]'s doc before deciding this is
/// decoration.
#[expect(clippy::too_many_arguments)] // a driver hub: URL + subscribe/keepalive/resubscribe + 4 closures + stop + timings + reseed + clock
pub fn run_depth(
    url: &str,
    subscribe: Option<&str>,
    keepalive: Option<&str>,
    resubscribe: &[&str],
    seed: impl Fn() -> Option<L2Book> + Sync,
    decode: impl FnMut(&str, &mut Option<Arc<L2Book>>) -> WebsocketSessionDepthOperation,
    on_book: impl FnMut(&Arc<L2Book>),
    on_health: impl FnMut(StreamStatus),
    stop: &AtomicBool,
    read_timeout: Duration,
    idle_threshold: Duration,
    freshness_threshold: Duration,
    reseed_interval: Option<Duration>,
    now_ms: &dyn Fn() -> i64,
    backoff: Duration,
) {
    run_depth_feed_on(
        || connect_depth(url, read_timeout),
        url,
        subscribe,
        keepalive,
        resubscribe,
        seed,
        decode,
        on_book,
        on_health,
        stop,
        idle_threshold,
        freshness_threshold,
        reseed_interval,
        now_ms,
        backoff,
    );
}

/// [`run_depth`]'s loop over any dial: production passes [`connect_depth`], a test a scripted
/// [`WebsocketSessionDepthStream`] per session. `url` names the lane in the journal.
#[expect(clippy::too_many_arguments)] // run_depth's arguments, the dial in place of read_timeout
fn run_depth_feed_on<S: WebsocketSessionDepthStream>(
    mut connect: impl FnMut() -> Result<S, Box<dyn std::error::Error>>,
    url: &str,
    subscribe: Option<&str>,
    keepalive: Option<&str>,
    resubscribe: &[&str],
    seed: impl Fn() -> Option<L2Book> + Sync,
    mut decode: impl FnMut(&str, &mut Option<Arc<L2Book>>) -> WebsocketSessionDepthOperation,
    mut on_book: impl FnMut(&Arc<L2Book>),
    mut on_health: impl FnMut(StreamStatus),
    stop: &AtomicBool,
    idle_threshold: Duration,
    freshness_threshold: Duration,
    reseed_interval: Option<Duration>,
    now_ms: &dyn Fn() -> i64,
    backoff: Duration,
) {
    // One `StreamHealth` for the whole feed lifetime (ACROSS reconnects): its transport half dedups
    // the `Gap`/`Live` to one pair per outage; its freshness half is reset per session inside
    // `run_depth_session`.
    let mut stream_health = StreamHealth::new(freshness_threshold.as_millis() as i64);
    // The JOURNAL voice, alongside the health disclosure (see [`WebsocketSessionDepthFaultLog`]). Owned here for
    // the same reason `stream_health` is: "is this feed faulting REPEATEDLY" cannot be answered
    // inside one session.
    let mut fault_log = WebsocketSessionDepthFaultLog::new();
    while !stop.load(Ordering::Relaxed) {
        let outcome = match connect() {
            Ok(mut stream) => run_depth_session(
                &mut stream,
                subscribe,
                keepalive,
                resubscribe,
                &seed,
                &mut decode,
                &mut on_book,
                &mut on_health,
                // A scheduled re-seed is the only proof this driver ever gets that a lane ran a full
                // `reseed_interval` without faulting — so it closes an open fault streak in the journal.
                &mut || {
                    fault_log.recovered(url);
                },
                &mut stream_health,
                stop,
                idle_threshold,
                reseed_interval,
                now_ms,
            ),
            Err(e) => Err(e), // connect failed — treat as an outage, open a gap, back off
        };
        match outcome {
            // A requested stop closes NOTHING in the journal, deliberately: a lane that was
            // faulting when the daemon was stopped did not recover, and saying otherwise on the
            // teardown path would be the one line an operator reads after an incident.
            Ok(WebsocketSessionDepthOutcome::Stopped) => break,
            // Session outage (idle trip, socket error, sequence gap, a re-seed that could not line
            // up, or a failed connect): open a transport gap — deduped to ONE `Gap` per outage
            // across reconnect attempts — AND say so in the journal, throttled to `FAULT_LOG_EVERY`
            // (the health dedup is the wrong trigger for a log line: see `WebsocketSessionDepthFaultLog`).
            Err(e) => {
                fault_log.fault(url, &e.to_string(), now_ms(), FAULT_LOG_EVERY);
                if let Some(event) = stream_health.enter_gap(now_ms()) {
                    on_health(event);
                }
            }
        }
        let steps = (backoff.as_millis() / 100).max(1);
        for _ in 0..steps {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[path = "depth_tests.rs"]
#[cfg(test)]
mod depth_tests;

/// Disclose a tick pump's link state onto the core's tick lane (the binance family's, bybit's and
/// okx's public market-data pumps, through [`FeedTickSender::link`]): the depth driver's transport
/// `GapStart`/`Live` and its freshness `Stale`, under the pump's link word
/// ([`vike_venues::link_deadman::TICK_LINK_STREAM`]). The word is informational for the dead-man's
/// windows, which key on `(venue, symbol)` and the word's SOURCE (`vike_venues::link_deadman::DeadManLinkSource`),
/// but it is what tells an operator reading an `on_feed_status` trace WHICH of a venue's sockets died:
/// this pump is not the DOM lane (label `"depth"`: `crates/bridges/binance/src/family/feed/mod.rs`'s
/// `depth_main`, bybit's and okx's `market_feed/depth.rs`).
///
/// ⚠ **This is the lane the connection-state dead-man actually hears** on a `vike-tradehub` CEX
/// start, and the reason it exists at all. That switch (`vike_core::DeadManLinkConfig`, decision
/// `docs/decisions/0038-the-dead-man-observes-the-connection-not-silence.md`) is ON by default and
/// arms per venue, but a venue arms only where the START subscribed a lane that carries a
/// disconnect. The binance family's only other emitter is the DOM depth feed
/// (`crates/bridges/binance/src/family/feed/mod.rs`'s `depth_main`, through a `DataClientSink`),
/// which that daemon does not subscribe — so before this call site existed, a default CEX daemon
/// built no switch at all, whatever the `policy` settings said.
///
/// No `DataClientSink` is involved and none is wanted: the pump writes onto a bare [`TickSender`], and
/// `vike_model::FeedStatus` is exactly what `vike_tradehub_core::CoreLaneSink` converts a sink-side
/// `StreamStatus` into anyway ([`StreamStatus::feed_status`] is the one map). A send failure is
/// `CoreGone` — the core thread has exited — and is dropped for the same reason every other push on
/// the pump's loop drops it: a feed thread has no channel back to its owner.
pub fn disclose_link(ticks: &TickSender, venue: &str, symbol: &str, event: StreamStatus) {
    let _ = ticks.stream_status(StreamStatusUpdate {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        stream: vike_venues::link_deadman::TICK_LINK_STREAM.to_string(),
        status: event.feed_status(),
    });
}

/// One decoded top-of-book leaves a tick pump: stamped with the machine RECEIVE time, then sent.
///
/// The venues' quote decoders (binance-family `decode_book_ticker` and its typed scan, bybit's derived
/// L1, okx's `decode_bbo`) leave `local_timestamp` at the "not stamped" `0` because a decoder is pure; the
/// pump is the first place that knows when the frame arrived, so the stamp is here, as it is for the
/// trade lanes and for hyperliquid, deribit and oanda's quotes. Without it a quote carried no receive
/// time, so its feed latency (`local_timestamp - ts`) could not be measured. One `now_ms` read on the venue
/// thread per quote; nothing is added to `vike-core`'s fold. `Err` is `CoreGone`.
///
/// **The ONE rule for a quote with no venue time: it gets the receive time.** A decoder states the
/// venue's own time in `ts` when the frame carries one, else `0` (the raw binance spot `bookTicker`
/// states none at all). The core writes `quote.ts` onto its price board (the `Ingest::Quote` arm),
/// where a `0` reads as a price from 1970: an age-bounded read never trusts it, and a mark nothing
/// refreshed priced the symbol for ever (`crates/vike-execution/src/price_board.rs`'s
/// `PriceBoard::classify`). So a `ts <= 0` leaves as `local_timestamp`, on every venue, here once
/// ([`stamp_quote`], which a data client's lane applies too).
pub fn send_quote(
    ticks: &TickSender,
    venue: &str,
    symbol: &str,
    mut quote: TickQuote,
) -> Result<(), vike_execution::CoreGone> {
    stamp_quote(&mut quote);
    ticks.quote(QuoteUpdate { venue: venue.into(), symbol: symbol.into(), quote })
}

/// The ONE receive stamp of a decoded quote: `local_timestamp` is now (the dual-timestamp capture), and a
/// quote with no venue time (`ts <= 0`) takes that same time ([`send_quote`]'s doc says why).
pub fn stamp_quote(quote: &mut TickQuote) {
    quote.local_timestamp = vike_model::now_ms();
    if quote.timestamp <= 0 {
        quote.timestamp = quote.local_timestamp;
    }
}

/// The bridge end of the feed-tick family: one tick feed's voice onto the core's tick lane (the
/// gatewayhub's feed-tick hub drains that lane). It is what the three emitting closures of the feed's
/// book socket on [`run_depth`] (`decode`, `on_book`, `on_health`) share. Every push carries the
/// feed's venue and the symbol it was spawned for (the caller's spelling, whatever the wire calls it).
///
/// A push that finds the core gone (`CoreGone`: the lane's consumer has exited) raises the pump's own
/// stop flag, so the driver ends at its next poll and the thread exits, as the hand-written sessions
/// did on the same error. A link disclosure that finds it gone is dropped ([`disclose_link`]).
pub struct FeedTickSender {
    venue: &'static str,
    symbol: String,
    ticks: TickSender,
    stop: Arc<AtomicBool>,
}

impl FeedTickSender {
    /// A pump over `ticks` for `(venue, symbol)` that ends when `stop` is raised. [`spawn_tick`]
    /// builds the one its thread runs; a test builds one over its own channel.
    pub fn new(
        venue: &'static str,
        symbol: &str,
        ticks: TickSender,
        stop: Arc<AtomicBool>,
    ) -> Self {
        Self { venue, symbol: symbol.to_string(), ticks, stop }
    }

    /// The flag [`run_depth`] polls: the owner's stop, or the core gone.
    pub fn stop(&self) -> &AtomicBool {
        &self.stop
    }

    /// A quote, through [`send_quote`] (the receive stamp and the one no-venue-time rule).
    pub fn quote(&self, quote: TickQuote) {
        self.sent(send_quote(&self.ticks, self.venue, &self.symbol, quote));
    }

    /// A trade print.
    pub fn trade(&self, trade: TickTrade) {
        let update = TradeUpdate { venue: self.venue.into(), symbol: self.symbol.clone(), trade };
        self.sent(self.ticks.trade(update));
    }

    /// The standing book: a refcount bump, never a deep copy (`vike_execution::BookUpdate`).
    pub fn book(&self, book: &Arc<L2Book>) {
        let update = BookUpdate {
            venue: self.venue.into(),
            symbol: self.symbol.clone(),
            book: Arc::clone(book),
        };
        self.sent(self.ticks.book(update));
    }

    /// The driver's health word, as this pump's link state ([`disclose_link`]).
    pub fn link(&self, event: StreamStatus) {
        disclose_link(&self.ticks, self.venue, &self.symbol, event);
    }

    fn sent(&self, pushed: Result<(), vike_execution::CoreGone>) {
        if pushed.is_err() {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// The tick pump thread the binance family's, bybit's and okx's public market-data lanes run
/// (`crates/bridges/binance/src/family/feed/tick/mod.rs`'s `spawn_feed_tick`, `spawn_bybit_feed_tick`,
/// `spawn_okx_feed_tick`): named `gw-market-data` and pinned to the market-data affinity role,
/// it runs `body` once and ends when `body` returns. The returned handle stops and joins the thread.
///
/// `body` is the venue's book socket on [`run_depth`], with [`FeedTickSender::link`] as its
/// `on_health` and [`FeedTickSender::stop`] as its flag: the URL, the subscribe and keepalive text, the
/// fold and the venue's `depth` timings row stay the venue's. So a tick pump has what a DOM lane has
/// on the same streams: the dead-socket watchdog (a half-open socket says `Disconnected` after the
/// row's idle threshold instead of reading live for ever), the freshness check (`Stale`), the timed
/// re-seed, the journal line ([`WebsocketSessionDepthFaultLog`]) and the backed-off reconnect (a gap re-dials and
/// re-seeds after the row's backoff, never a REST re-seed per frame). The link's recovery is told on
/// the first BOOK of a session, not on any frame, so a refused subscribe whose socket keeps answering
/// never reads healthy.
pub fn spawn_tick(
    venue: &'static str,
    symbol: &str,
    ticks: TickSender,
    body: impl FnOnce(&FeedTickSender) + Send + 'static,
) -> vike_utility::poller::StopHandle {
    let symbol = symbol.to_string();
    vike_utility::poller::spawn_poller("gw-market-data", move |stop| {
        vike_execution::affinity::pin_current_thread(
            vike_execution::affinity::Role::MarketData,
            venue,
        );
        body(&FeedTickSender::new(venue, &symbol, ticks, stop));
    })
}
