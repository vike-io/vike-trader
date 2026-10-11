//! The venue-neutral live **L2 depth-feed driver**. Every venue's order-book stream shares one
//! LIFECYCLE — connect, (optionally REST-)seed a book, read frames, fold each, publish the top-N,
//! and reconnect with backoff on error/gap — while the PROTOCOL (URL, subscribe frame, frame
//! decode, resync rule) differs per venue. This module owns the lifecycle; the venue crate supplies
//! its protocol as three closures (`seed`, `decode`, `on_book`).
//!
//! Deliberately no dependency on `vike-data`: the driver never sees a `LiveDataSink` or its
//! `StreamStatus`. The venue wires its own sink inside `on_book` and maps each [`HealthEvent`] onto
//! `vike_data::StreamStatus` 1:1 through [`crate::stream_health::health_to_stream_status`]. The
//! per-venue decoders (`route_frame`/`apply_depth_event`/…) stay in each bridge crate.
//!
//! **Dead-transport watchdog.** The read loop runs over a [`DepthStream`] seam that stamps a
//! liveness clock on EVERY inbound frame (data, server Ping, any control frame); a read timeout with
//! no frame of any kind for longer than `idle_threshold` returns `Err`, so the reconnect path
//! re-seeds. An idle trip and any transport error/gap emit [`HealthEvent::Gap`] through `on_health`
//! (once per outage — the [`StreamHealth`] owned by [`run_depth_feed`] dedups it across
//! reconnects), and the first book of each session emits [`HealthEvent::Live`] BEFORE that book
//! when a gap was open, so a consumer sees "recovered" ahead of the re-seeded data. It detects a
//! dead *connection*, not data starvation behind a live one.
//!
//! **Data-freshness watchdog (the data-side twin).** A reconnect whose re-subscribe silently failed
//! still answers keepalives, so the book reads live forever while frozen; a venue REPLAYING old data
//! arrives on time with stale event-times. Every [`BookOp::Updated`] carries the venue EVENT-TIME
//! (epoch-ms; `0` = omitted → receive-time), fed to [`StreamHealth::observe_data`] and judged
//! against the INJECTED `now_ms()`. On a read-timeout tick, ONLY after the transport idle check did
//! not trip (so the two never double-signal), [`StreamHealth::check_freshness`] emits
//! [`HealthEvent::Stale`] once when the newest update's event-time is older than the freshness
//! threshold, then [`HealthEvent::Live`] when updates resume (before that book). Recovery fires
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
//! checked). The optional `reseed_interval` (`None` = off) ends a session with
//! [`SessionOutcome::Reseed`] once the injected `now_ms()` advances that far past session start, and
//! [`run_depth_feed`] reconnects + re-seeds through the SAME path a gap takes — WITHOUT opening a
//! [`HealthEvent::Gap`], because a scheduled refresh is not an outage and must not flap a health
//! consumer's Gap/Live view. "Wrong forever" becomes "wrong ≤ reseed_interval".

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_model::{BookLevel, L2Book};

use crate::pump_spec::CONNECT_10S;
use crate::stream_health::{HealthEvent, StreamHealth};
use crate::user_data::{StreamError, StreamMsg, UserStream};
use crate::ws::{TungsteniteStream, configure_ws_stream};
use crate::ws_proxy::connect_ws;

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

/// The journal voice of [`run_depth_feed`]'s reconnect loop.
///
/// **Why it exists.** A depth lane's only other disclosure is the `HealthEvent` the venue maps onto
/// a `vike_data::StreamStatus`, and in a HEADLESS process (the recorder, the datahub daemon) that
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
/// the one identifier [`run_depth_feed`] holds — deliberately not a new `(venue, symbol)` parameter
/// on a driver hub already this wide.
#[derive(Debug, Default)]
pub struct DepthFaultLog {
    /// Faults in the current streak — reset by [`Self::recovered`], reported by the recovery line.
    streak: u64,
    /// Faults recorded since the last line was spoken (the count the next line carries).
    since_report: u64,
    /// When the last line was spoken, on the injected clock. `None` = nothing spoken this streak,
    /// so the next fault speaks immediately.
    last_report_ms: Option<i64>,
}

impl DepthFaultLog {
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
pub enum BookOp {
    /// the book was (re)seeded or a diff applied — publish it. Carries the venue EVENT-TIME of the
    /// update in epoch-ms (`0` = the venue supplied no usable timestamp → the driver falls back to
    /// receive-time). The data-freshness watchdog clocks staleness off this, so a frame that
    /// ARRIVES now but is STAMPED old still ages the book (catches a venue replaying stale data,
    /// which a receive-time clock would miss). `i64`, so `BookOp` stays `Copy`.
    Updated(i64),
    /// a sequence gap — the session must resync; the driver reconnects + re-seeds after a backoff
    Gap,
    /// nothing to publish (ack / pong / stale / non-book frame / not yet seeded)
    Ignored,
}

/// How a depth session ended CLEANLY — the `Ok` arm of [`run_depth_session`]. A transport FAULT
/// (socket error, sequence gap, or idle trip) stays in the `Err` arm, so `?` still short-circuits
/// faults and only the two deliberate, non-fault endings split here. [`run_depth_feed`] matches on
/// this to decide whether the reconnect it is about to do should open a transport gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    /// `stop` was raised — [`run_depth_feed`] exits its reconnect loop (deterministic teardown).
    Stopped,
    /// The periodic re-seed timer fired: [`run_depth_feed`] reconnects + re-seeds
    /// WITHOUT opening a transport gap. This is a PLANNED refresh, not an outage, so it must NOT
    /// emit [`HealthEvent::Gap`]/[`HealthEvent::Live`] the way a real fault does.
    Reseed,
}

/// A read/write seam over one depth-WS session. Production wraps tungstenite
/// ([`TungsteniteDepthStream`]); scripted tests drive a canned frame queue to exercise the idle
/// watchdog and recovery paths with zero real-time sleeps. Mirrors
/// [`crate::market_pump::MarketStream`]: `read_frame` returns exactly one TEXT frame per call;
/// Ping/Pong/other control frames are auto-handled INSIDE the impl (never surfaced), and each stamps
/// the liveness clock, so a connection kept alive purely by control-frame keepalive is not falsely
/// declared idle.
pub trait DepthStream {
    /// One decoded TEXT frame, or a read-timeout tick ([`StreamError::Timeout`] — poll `stop`, then
    /// the idle watchdog), or a close ([`StreamError::Closed`]).
    fn read_frame(&mut self) -> Result<String, StreamError>;
    /// Send an app-level text frame (subscribe, or a venue keepalive ping).
    fn send_text(&mut self, s: &str) -> Result<(), StreamError>;
    /// Time since the last inbound frame of ANY kind (data, Ping, or other control frame). The
    /// default `ZERO` (never idle) suits scripted seams that don't model wall-clock time; they
    /// override it to drive the idle path deterministically.
    fn since_last_frame(&self) -> Duration {
        Duration::ZERO
    }
}

/// tungstenite [`DepthStream`] over [`TungsteniteStream`] (reused from `ws.rs` — it already owns the
/// WouldBlock/TimedOut classifier and the Text/Ping/Close match). A Ping is auto-ponged and looped
/// past; a Close is surfaced as `Err(Closed)`. Every received frame — Text, Ping, OR Other (a server
/// Pong / binary control frame) — stamps `last_rx` so the idle watchdog sees ALL keepalive
/// traffic, not just data.
struct TungsteniteDepthStream {
    stream: TungsteniteStream,
    last_rx: Instant,
}

impl DepthStream for TungsteniteDepthStream {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        loop {
            match self.stream.recv() {
                Ok(StreamMsg::Text(t)) => {
                    self.last_rx = Instant::now();
                    return Ok(t);
                }
                Ok(StreamMsg::Ping(p)) => {
                    self.last_rx = Instant::now();
                    self.stream.pong(p)?;
                }
                Ok(StreamMsg::Other) => self.last_rx = Instant::now(),
                Err(StreamError::Timeout) => return Err(StreamError::Timeout),
                Err(StreamError::Closed(m)) => return Err(StreamError::Closed(m)),
            }
        }
    }

    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        self.stream.send_text(s)
    }

    fn since_last_frame(&self) -> Duration {
        self.last_rx.elapsed()
    }
}

/// Infer an instrument's price tick from a (dense) depth snapshot: the smallest positive gap
/// between adjacent price levels. Reliable for an actively-quoted symbol (some adjacent pair is
/// exactly one tick apart); for a genuinely thin/sparse book the min gap can be an integer MULTIPLE
/// of the true tick — seed from the venue `PRICE_FILTER` tickSize there instead. Falls back to 0.01
/// (fewer than two distinct levels). ONE shared copy so every venue quantizes the same way.
pub fn infer_tick_size(bids: &[BookLevel], asks: &[BookLevel]) -> f64 {
    // A non-finite price (`"NaN"` parses as f64) has no place in a gap; `partial_cmp` over NaN is
    // also not a total order, which a long-slice `sort_by` is allowed to panic on.
    let mut prices: Vec<f64> =
        bids.iter().chain(asks).map(|l| l.price).filter(|p| p.is_finite()).collect();
    prices.sort_by(f64::total_cmp);
    let mut min = f64::INFINITY;
    for w in prices.windows(2) {
        let d = w[1] - w[0];
        if d > 1e-9 && d < min {
            min = d;
        }
    }
    if min.is_finite() { min } else { 0.01 }
}

/// Parse one side of a venue book (`[["px","qty"], …]`, each number a decimal STRING) into
/// [`BookLevel`]s (qty 0 = remove, per [`L2Book`]). A level whose price or size is not a string that
/// parses as `f64` — including a JSON NUMBER, which none of these venues sends — is skipped, and an
/// absent or non-array `side` is an empty side.
///
/// ONE shared copy, so every venue that spells its book this way decodes it the same way.
/// Deliberately NOT [`crate::json::json_num`]: that accepts a JSON number too, and swapping it in
/// would change which frames decode.
#[must_use]
pub fn parse_levels(side: Option<&Value>) -> Vec<BookLevel> {
    fn num(v: Option<&Value>) -> Option<f64> {
        v.and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
    }
    side.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|lvl| {
                    let l = lvl.as_array()?;
                    Some(BookLevel { price: num(l.first())?, qty: num(l.get(1))? })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One WS depth session over an already-connected `stream`: optionally send `subscribe`, `seed` the
/// book (a REST snapshot for a snapshot-first venue like Binance; `None` for a WS-seeded venue like
/// Bybit/OKX), then read frames and fold each via `decode`, calling `on_book` on every
/// [`BookOp::Updated`]. Returns `Ok(())` on a requested stop; `Err` on a socket failure, a
/// [`BookOp::Gap`], OR the idle watchdog tripping (no inbound frame for `idle_threshold`) — either
/// way [`run_depth_feed`] opens a transport gap ([`HealthEvent::Gap`]), backs off, then reconnects (a
/// fresh session re-seeds). The FIRST book of the session emits [`HealthEvent::Live`] just BEFORE it
/// WHEN a gap was open (via [`StreamHealth::recover`]) — closing a prior outage's gap ahead of the
/// re-seeded data; a first connect with no open gap discloses nothing. `stop` is polled every read
/// tick AND once more just BEFORE `seed` — so a stop that landed during the (bounded) dial costs the
/// dial and nothing after it, instead of the dial plus a fresh REST round trip. Teardown is
/// therefore deterministic within one read tick, plus at most ONE in-flight `seed`, which the venue
/// bounds (`crates/bridges/binance/src/family/depth.rs`'s `DEPTH_SEED_TIMEOUT` for binance/aster; a
/// WS-seeded venue passes a `seed` that returns `None` and takes its snapshot off the wire).
/// `keepalive`, if set, is sent every ~15 s (Bybit/OKX drop clients that don't ping).
///
/// `stream_health` is owned by [`run_depth_feed`] ACROSS reconnects (so the transport `Gap`/`Live` is
/// deduped to one pair per outage); each session resets AND arms only its FRESHNESS half up front
/// ([`StreamHealth::reset_freshness`] with `now_ms()`), so even a dataless session ages toward
/// `Stale`. The DATA-side watchdog runs on the same read-timeout tick but ONLY after the transport
/// idle check passes (the module doc). `now_ms` is injected (production wall-clock; tests a
/// controllable counter) so this is testable with zero sleeps.
///
/// `reseed_interval`, if `Some`, is the timed book re-seed: once `now_ms()` advances past it from
/// session start the loop returns [`SessionOutcome::Reseed`] even on a perfectly healthy, in-sync
/// stream (no gap, no idle) — so [`run_depth_feed`] reconnects + re-seeds and a silently corrupted
/// book self-heals within `reseed_interval`. Clocked off the injected `now_ms` (never `Instant`) so
/// it is deterministic; `None` disables it.
#[expect(clippy::too_many_arguments)] // driver hub: subscribe/keepalive + 4 closures + health + stop + idle + reseed + clock
pub fn run_depth_session<S: DepthStream>(
    stream: &mut S,
    subscribe: Option<&str>,
    keepalive: Option<&str>,
    seed: &mut dyn FnMut() -> Option<L2Book>,
    decode: &mut dyn FnMut(&str, &mut Option<L2Book>) -> BookOp,
    on_book: &mut dyn FnMut(&L2Book),
    on_health: &mut dyn FnMut(HealthEvent),
    stream_health: &mut StreamHealth,
    stop: &AtomicBool,
    idle_threshold: Duration,
    reseed_interval: Option<Duration>,
    now_ms: &dyn Fn() -> i64,
) -> Result<SessionOutcome, Box<dyn std::error::Error>> {
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
    // Transport gap state is deliberately untouched — the `StreamHealth` owned by `run_depth_feed`
    // carries the open gap across the reconnect so the first book below can close it.
    stream_health.reset_freshness(session_start_ms);

    // Recovery is disclosed exactly once per session, on the FIRST published book (the consumer
    // sees the transport `Live` before the re-seeded data). `recover()` is a no-op (returns `None`)
    // when no gap is open — a first connect discloses nothing. `publish` centralizes that guard.
    let mut first_publish = true;
    let mut publish = |book: &L2Book,
                       on_health: &mut dyn FnMut(HealthEvent),
                       on_book: &mut dyn FnMut(&L2Book),
                       stream_health: &mut StreamHealth| {
        if first_publish {
            if let Some(ev) = stream_health.recover() {
                on_health(ev);
            }
            first_publish = false;
        }
        on_book(book);
    };

    // ⚠ **Poll `stop` BEFORE the seed, not only in the read loop below.** `seed` is a REST round
    // trip on the REST-seeded venues (binance/aster — `crates/bridges/binance/src/family/depth.rs`'s
    // `DEPTH_SEED_TIMEOUT` bounds it), and it is the LAST thing between a session's dial and the
    // loop's first flag read. A dial can spend its whole bounded window with the flag already raised
    // behind it, so without this check a stop landing mid-dial pays the dial AND THEN starts a fresh
    // network call the caller has already asked not to happen — the identical window
    // `crates/bridges/binance/src/family/trades.rs`'s connect closure refuses before its warmup.
    // Held by `a_stop_raised_before_the_session_skips_the_rest_seed`.
    if stop.load(Ordering::Relaxed) {
        return Ok(SessionOutcome::Stopped);
    }
    // Seed BEFORE the read loop (REST venues) so no diff is missed in the fetch window; WS-seeded
    // venues return None here and get their snapshot as the first decoded frame.
    let mut book: Option<L2Book> = seed();
    if let Some(b) = &book {
        publish(b, on_health, on_book, stream_health);
    }
    let mut last_ka = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(SessionOutcome::Stopped);
        }
        // Timed book re-seed (checksum-less venues): once the injected clock has
        // advanced `reseed_interval` past session start, END the session with `Reseed` so
        // `run_depth_feed` reconnects + re-snapshots. This bounds a SILENTLY corrupted book (a
        // mis-applied delta that never breaks the seq chain — so no `BookOp::Gap`, no idle, no
        // freshness trip) to ≤ `reseed_interval`, WITHOUT disclosing a transport gap (a scheduled
        // refresh is not an outage). Clocked off `now_ms` (never `Instant`), so it is deterministic;
        // `None` skips the `now_ms()` call entirely, so the default-off path adds zero overhead.
        if let Some(interval_ms) = reseed_interval_ms
            && now_ms() - session_start_ms >= interval_ms
        {
            return Ok(SessionOutcome::Reseed);
        }
        // App-level keepalive on a wall-clock cadence (checked before each read; the read timeout
        // guarantees this runs at least every read tick). Kept in the driver so it fires even while
        // data flows AND while idle — the case where the venue would otherwise drop us.
        if let Some(ka) = keepalive
            && last_ka.elapsed() >= KEEPALIVE_EVERY
        {
            stream.send_text(ka).map_err(|e| format!("depth keepalive failed: {e:?}"))?;
            last_ka = Instant::now();
        }
        match stream.read_frame() {
            Ok(txt) => match decode(&txt, &mut book) {
                BookOp::Updated(ts) => {
                    // Feed the venue EVENT-TIME (`ts`) to the freshness tracker, falling back to
                    // receive-time (`now_ms()`) when the venue omitted it (`ts <= 0`) — so a frame
                    // that ARRIVES now but is STAMPED old still ages the book. Then, if a stale
                    // episode is open, let `check_freshness` CLOSE it with `Live` BEFORE the book
                    // (recovery-before-data, mirroring how the transport `Live` precedes the re-seeded
                    // book). The `is_stale()` guard keeps this a RECOVERY-only step: a `Stale` opens
                    // only on a transport-alive timeout tick below, never here on an update frame
                    // carrying an OLD stamp.
                    let stamp = if ts > 0 { ts } else { now_ms() };
                    stream_health.observe_data(stamp);
                    if stream_health.is_stale()
                        && let Some(ev) = stream_health.check_freshness(now_ms())
                    {
                        on_health(ev);
                    }
                    if let Some(b) = &book {
                        publish(b, on_health, on_book, stream_health);
                    }
                }
                BookOp::Gap => return Err("depth sequence gap — resync".into()),
                BookOp::Ignored => {}
            },
            Err(StreamError::Timeout) => {
                // Idle over ANY inbound frame (data, Ping, or other control keepalive —
                // `since_last_frame`), so only true transport silence trips the gap.
                if stream.since_last_frame() > idle_threshold {
                    return Err("idle timeout — no depth frame within the watchdog window".into());
                }
                // Transport still alive → judge DATA freshness (the failure the idle check CANNOT see:
                // a re-subscribe that silently failed still answers keepalives, so the book freezes
                // behind a live socket). Runs ONLY after the idle check did NOT trip, so the two
                // watchdogs never double-signal (and `check_freshness` is internally gated to stay
                // silent during a transport gap anyway). Discloses `Stale` once per episode; the
                // recovery `Live` is emitted on the resuming update frame above.
                if let Some(ev) = stream_health.check_freshness(now_ms()) {
                    on_health(ev);
                }
            }
            Err(StreamError::Closed(m)) => return Err(m.into()),
        }
    }
}

/// Connect + set the stop-poll read timeout, wrapped as a [`DepthStream`].
///
/// **The dial is BOUNDED — [`CONNECT_10S`], the same window the market pump spends.** A bare
/// `tungstenite::connect` applies no connect bound of any kind, so [`run_depth_feed`]'s thread would
/// sit out the OS's own SYN ladder (~127 s on Linux defaults) on a black-holed route with `stop`
/// already raised behind it.
///
/// **It is on the recorder's teardown path.** `crates/vike-data/src/rec/live_rec.rs`'s
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
) -> Result<TungsteniteDepthStream, Box<dyn std::error::Error>> {
    let socket = connect_ws(url, None, Some(CONNECT_10S))?;
    configure_ws_stream(&socket, read_timeout);
    Ok(TungsteniteDepthStream { stream: TungsteniteStream(socket), last_rx: Instant::now() })
}

/// Reconnect loop around [`run_depth_session`]: on a socket error, gap, or idle trip, open a
/// transport gap ([`HealthEvent::Gap`], deduped to one per outage by the feed-owned [`StreamHealth`]),
/// back off (stop-aware), then reconnect and re-seed, until `stop`. A clean stop (`Ok`) ends the loop.
/// This IS the whole thread body a venue's `subscribe_depth` spawns. `backoff` throttles reconnects so
/// a persistent fault (e.g. a REST-seed that keeps failing) can't hot-loop. `idle_threshold` sizes the
/// dead-transport watchdog as a per-venue constant well above the keepalive cadence, so keepalive
/// traffic keeps a quiet book alive and only true silence trips it. `freshness_threshold` (baked into
/// the feed's `StreamHealth`) + the injected `now_ms` clock size and drive the DATA-freshness twin
/// inside [`run_depth_session`]: a book that stops updating behind a still-live socket for longer than
/// `freshness_threshold` emits [`HealthEvent::Stale`]/[`HealthEvent::Live`], which the venue maps onto
/// `vike_data::StreamStatus` next to the transport `Gap`. The one `StreamHealth` lives for the feed's
/// whole lifetime (ACROSS reconnects), which is what gives the transport `Gap`/`Live` its one-pair-
/// per-outage dedup; its freshness half is reset per session inside `run_depth_session`.
///
/// `reseed_interval`, if `Some`, is the timed book re-seed: every session that stays healthy that
/// long ends with [`SessionOutcome::Reseed`], on which this loop reconnects + re-seeds WITHOUT opening
/// a transport gap (a scheduled refresh, not an outage — so it does NOT flap the Gap/Live disclosure).
/// It reuses the exact reconnect + re-seed path a fault takes (through the same stop-aware `backoff`),
/// so the snapshot/rebuild logic is not duplicated. `None` disables it; a venue with no book
/// checksum opts in to bound silent book corruption to ≤ the interval.
///
/// **It also SPEAKS.** Every fault goes through a feed-owned [`DepthFaultLog`] as well as the health
/// disclosure: the first fault of a streak is a `tracing::warn!` naming the URL and the error, then
/// at most one line per [`FAULT_LOG_EVERY`] carrying how many faults it covers, and an `info!` when
/// a session survives to a scheduled re-seed. Read [`DepthFaultLog`]'s doc before deciding this is
/// decoration.
#[expect(clippy::too_many_arguments)] // a driver hub: URL + subscribe/keepalive + 4 closures + stop + timings + reseed + clock
pub fn run_depth_feed(
    url: &str,
    subscribe: Option<&str>,
    keepalive: Option<&str>,
    mut seed: impl FnMut() -> Option<L2Book>,
    mut decode: impl FnMut(&str, &mut Option<L2Book>) -> BookOp,
    mut on_book: impl FnMut(&L2Book),
    mut on_health: impl FnMut(HealthEvent),
    stop: &AtomicBool,
    read_timeout: Duration,
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
    // The JOURNAL voice, alongside the health disclosure (see [`DepthFaultLog`]). Owned here for
    // the same reason `stream_health` is: "is this feed faulting REPEATEDLY" cannot be answered
    // inside one session.
    let mut fault_log = DepthFaultLog::new();
    while !stop.load(Ordering::Relaxed) {
        let outcome = match connect_depth(url, read_timeout) {
            Ok(mut stream) => run_depth_session(
                &mut stream,
                subscribe,
                keepalive,
                &mut seed,
                &mut decode,
                &mut on_book,
                &mut on_health,
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
            Ok(SessionOutcome::Stopped) => break,
            // A PLANNED periodic re-seed: fall through to the same stop-aware backoff + reconnect +
            // re-seed a fault takes, but do NOT open a transport gap — a scheduled refresh is not an
            // outage, so it must not flap the `Gap`/`Live` disclosure a health consumer watches.
            // It IS, however, the only proof this driver ever gets that a lane ran a full
            // `reseed_interval` without faulting — so it closes an open fault streak in the journal.
            Ok(SessionOutcome::Reseed) => {
                fault_log.recovered(url);
            }
            // Session outage (idle trip, socket error, sequence gap, or a failed connect): open a
            // transport gap — deduped to ONE `Gap` per outage across reconnect attempts — AND say
            // so in the journal, throttled to `FAULT_LOG_EVERY` (the health dedup is the wrong
            // trigger for a log line: see `DepthFaultLog`).
            Err(e) => {
                fault_log.fault(url, &e.to_string(), now_ms(), FAULT_LOG_EVERY);
                if let Some(ev) = stream_health.enter_gap(now_ms()) {
                    on_health(ev);
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
