//! The venue-neutral live **L2 depth-feed driver**. Every venue's order-book stream shares one
//! LIFECYCLE — connect, (optionally REST-)seed a book, read frames, fold each, publish the top-N,
//! and reconnect with backoff on error/gap — while the PROTOCOL (URL, subscribe frame, frame
//! decode, resync rule) differs per venue. This module owns the lifecycle; the venue crate supplies
//! its protocol as three closures (`seed`, `decode`, `on_book`).
//!
//! Deliberately no dependency on `vike-data`: the driver never sees a `LiveDataSink` or its
//! `StreamStatus`. The venue wires its own sink inside the `on_book` publish callback, so this stays
//! pure lifecycle over `vike_model::L2Book`. (The per-venue decoders —
//! `route_frame`/`apply_depth_event`/… — stay in each bridge crate, where the protocol belongs.)
//!
//! **Net-hardening §B — dead-transport watchdog.** A half-dead socket (open, no frames, no error)
//! used to loop forever on the read-timeout tick, leaving the DOM showing a silently stale book.
//! The read loop now runs over a [`DepthStream`] seam that stamps a liveness clock on EVERY inbound
//! frame (data, server Ping, or any other control frame); a read timeout with no frame of any kind
//! for longer than `idle_threshold` returns `Err`, so the existing reconnect path re-seeds. Both an
//! idle trip and any transport error/gap emit a [`HealthEvent::Gap`] through the venue's `on_health`
//! callback (once per outage — the [`StreamHealth`] owned by [`run_depth_feed`] dedups it across
//! reconnects, subsuming the per-venue transport-gap tracker this driver used to lean on), and the
//! first book of each session emits [`HealthEvent::Live`] BEFORE that book when a gap was open — so a
//! consumer sees "recovered" ahead
//! of the re-seeded data. [`HealthEvent`] is `vike-data`-free so this module keeps no `vike-data`
//! dependency; the venue maps it onto `vike_data::StreamStatus` + its `LiveDataSink` 1:1. This is a
//! TRANSPORT-liveness watchdog (mirrors the Polymarket tick feed's §B seam and LEAN's `KeepAlive`
//! shape): it detects a dead *connection*, not data-starvation behind a live one.
//!
//! **Net-hardening §B — data-freshness watchdog (the data-side twin).** The transport watchdog above
//! cannot see a reconnect whose re-subscribe silently failed: the socket still answers keepalives, so
//! `since_last_frame` stays fresh and the book reads as live forever while its ladder is
//! frozen — nor can it see a venue REPLAYING old data (frames arrive on time but carry stale
//! event-times). A symmetric data-freshness watchdog closes both holes: every [`BookOp::Updated`]
//! carries the venue EVENT-TIME of the update (epoch-ms; `0` = the venue omitted it → fall back to
//! receive-time), fed to [`StreamHealth::observe_data`] and judged against the INJECTED `now_ms()`
//! clock (production wall-clock, tests a controllable counter). On a read-timeout tick, ONLY after the
//! transport idle check above did NOT trip (so the two never double-signal),
//! [`StreamHealth::check_freshness`] emits [`HealthEvent::Stale`] once when the newest applied
//! update's event-time is older than the freshness threshold, then [`HealthEvent::Live`] when updates
//! resume (before that book, mirroring recovery-before-data). Recovery fires ONLY when data is
//! genuinely fresh again — an update whose OWN stamp is still past the threshold keeps the episode
//! open rather than flapping a false `Live`/`Stale` (an intentional improvement over the old inline
//! freshness; pinned by `a_stale_stamped_update_while_stale_does_not_falsely_recover`). Clocking off the venue event-time — not
//! receive-time — is what lets a frame that ARRIVES now but is STAMPED old still age the book; this
//! assumes venue/local clocks are ~NTP-synced, which holds for these venues (the 120s threshold
//! dwarfs any real clock skew). The freshness state is per-session — [`run_depth_session`] calls
//! [`StreamHealth::reset_freshness`] (passing `now_ms()`) at each session start, which BOTH drops any
//! open `Stale` from the prior outage (the next session's transport `Live` discloses recovery) AND
//! ARMS the freshness clock at session start — so even a session that receives ZERO book updates (a
//! subscribe that silently succeeds but sends nothing behind a live keepalive'd socket) still ages
//! toward `Stale` from session start, rather than a dataless session tripping nothing (the §B
//! zero-data hole; pinned by `a_seeded_but_dataless_session_goes_stale`). Once real data lands its
//! venue event-time supersedes that arm floor. The venue maps every `HealthEvent` onto
//! `vike_data::StreamStatus` 1:1, so the driver stays `vike-data`-free.
//!
//! **Net-hardening — timed book re-seed (checksum-less venues).** The three watchdogs above all
//! react to an OBSERVABLE fault: a broken seq chain ([`BookOp::Gap`]), transport silence (the idle
//! watchdog), or a frozen/stale-stamped book (freshness). NONE catches a book that silently corrupts
//! while updates keep flowing — a dropped or mis-applied delta that does NOT break the seq chain — on
//! a venue with no book checksum to validate (Binance/Bybit have none; OKX's CRC is intentionally not
//! checked). Such a book stays wrong until the next natural reconnect (which can be hours). The
//! optional `reseed_interval` (`None` = off, the default — zero behavior change) forces a fresh
//! snapshot on a timer: once the injected `now_ms()` advances `reseed_interval` past session start,
//! [`run_depth_session`] ends with [`SessionOutcome::Reseed`] and [`run_depth_feed`] reconnects +
//! re-seeds through the SAME path a gap takes — but WITHOUT opening a transport gap
//! ([`HealthEvent::Gap`]), because a scheduled refresh is not an outage and must not flap a health
//! consumer's Gap/Live view. This converts "wrong forever" into "wrong ≤ reseed_interval". Clocked
//! off the injected `now_ms` (like the freshness watchdog above, never `Instant`), so it trips
//! deterministically and is unit-tested with zero real sleeps.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_model::{L2Book, Level};

use crate::pump_spec::CONNECT_10S;
use crate::stream_health::{HealthEvent, StreamHealth};
use crate::user_data::{StreamError, StreamMsg, UserStream};
use crate::ws::{configure_ws_stream, TungsteniteStream};
use crate::ws_proxy::connect_ws;

/// Client keepalive cadence for venues that require one (Bybit/OKX drop idle clients).
const KEEPALIVE_EVERY: Duration = Duration::from_secs(15);

/// What decoding one WS frame did to the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookOp {
    /// the book was (re)seeded or a diff applied — publish it. Carries the venue EVENT-TIME of the
    /// update in epoch-ms (`0` = the venue supplied no usable timestamp → the driver falls back to
    /// receive-time). The §B data-freshness watchdog clocks staleness off this, so a frame that
    /// ARRIVES now but is STAMPED old still ages the book (catches a venue replaying stale data,
    /// which a receive-time clock would miss). `i64`, so `BookOp` stays `Copy`.
    Updated(i64),
    /// a sequence gap — the session must resync; the driver reconnects + re-seeds after a backoff
    Gap,
    /// nothing to publish (ack / pong / stale / non-book frame / not yet seeded)
    Ignored,
}

/// How a depth session ended CLEANLY — the `Ok` arm of [`run_depth_session`]. A transport FAULT
/// (socket error, sequence gap, or §B idle trip) stays in the `Err` arm, so `?` still short-circuits
/// faults and only the two deliberate, non-fault endings split here. [`run_depth_feed`] matches on
/// this to decide whether the reconnect it is about to do should open a transport gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOutcome {
    /// `stop` was raised — [`run_depth_feed`] exits its reconnect loop (deterministic teardown).
    Stopped,
    /// The periodic re-seed timer fired (net-hardening): [`run_depth_feed`] reconnects + re-seeds
    /// WITHOUT opening a transport gap. This is a PLANNED refresh, not an outage, so it must NOT
    /// emit [`HealthEvent::Gap`]/[`HealthEvent::Live`] the way a real fault does.
    Reseed,
}

/// A read/write seam over one depth-WS session (net-hardening §B). Production wraps tungstenite
/// ([`TungsteniteDepthStream`]); scripted tests drive a canned frame queue to exercise the idle
/// watchdog and recovery paths with zero real-time sleeps. Mirrors polymarket's `MarketStream` and
/// the [`UserStream`] pattern one layer up: `read_frame` returns exactly one TEXT frame per call;
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
/// Pong / binary control frame) — stamps `last_rx` so the §B idle watchdog sees ALL keepalive
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
pub fn infer_tick_size(bids: &[Level], asks: &[Level]) -> f64 {
    let mut prices: Vec<f64> = bids.iter().chain(asks).map(|(p, _)| *p).collect();
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut min = f64::INFINITY;
    for w in prices.windows(2) {
        let d = w[1] - w[0];
        if d > 1e-9 && d < min {
            min = d;
        }
    }
    if min.is_finite() {
        min
    } else {
        0.01
    }
}

/// One WS depth session over an already-connected `stream`: optionally send `subscribe`, `seed` the
/// book (a REST snapshot for a snapshot-first venue like Binance; `None` for a WS-seeded venue like
/// Bybit/OKX), then read frames and fold each via `decode`, calling `on_book` on every
/// [`BookOp::Updated`]. Returns `Ok(())` on a requested stop; `Err` on a socket failure, a
/// [`BookOp::Gap`], OR the §B idle watchdog tripping (no inbound frame for `idle_threshold`) — either
/// way [`run_depth_feed`] opens a transport gap ([`HealthEvent::Gap`]), backs off, then reconnects (a
/// fresh session re-seeds). The FIRST book of the session emits [`HealthEvent::Live`] just BEFORE it
/// WHEN a gap was open (via [`StreamHealth::recover`]) — closing a prior outage's gap ahead of the
/// re-seeded data; a first connect with no open gap discloses nothing. `stop` is polled every read
/// tick AND once more just BEFORE `seed` — so a stop that landed during the (bounded) dial costs the
/// dial and nothing after it, instead of the dial plus a fresh REST round trip. Teardown is
/// therefore deterministic within one read tick, plus at most ONE in-flight `seed`, which the venue
/// bounds (`crates/bridges/binance/src/family/depth.rs`'s `DEPTH_SEED_TIMEOUT` — the only
/// REST-seeded venues are binance/aster; bybit/okx pass a `seed` that returns `None` and take their
/// snapshot off the wire). `keepalive`, if set, is sent every ~15 s (Bybit/OKX drop
/// clients that don't ping; Binance doesn't need one).
///
/// `stream_health` is owned by [`run_depth_feed`] ACROSS reconnects (so the transport `Gap`/`Live` is
/// deduped to one pair per outage); each session resets AND arms only its FRESHNESS half up front
/// ([`StreamHealth::reset_freshness`] with `now_ms()`), preserving the old per-session
/// arm-at-session-start so even a dataless session ages toward `Stale`. A second,
/// DATA-side watchdog runs on the same read-timeout tick but ONLY after the transport idle check
/// passes (transport alive): [`StreamHealth::check_freshness`] emits [`HealthEvent::Stale`] once when
/// the newest applied [`BookOp::Updated`]'s VENUE EVENT-TIME is older than the freshness threshold
/// (baked into `stream_health`; falling back to receive-time when the venue omits its ts —
/// `Updated(0)`), then [`HealthEvent::Live`] when updates resume. `now_ms` is injected (production
/// wall-clock; tests a controllable counter) so this is testable with zero sleeps.
///
/// `reseed_interval`, if `Some`, is the timed book re-seed (net-hardening): once `now_ms()` advances
/// past it from session start the loop returns [`SessionOutcome::Reseed`] even on a perfectly healthy,
/// in-sync stream (no gap, no idle) — so [`run_depth_feed`] reconnects + re-seeds and a silently
/// corrupted book self-heals within `reseed_interval`. Clocked off the injected `now_ms` (never
/// `Instant`) so it is deterministic; `None` (the default) disables it (zero behavior change).
#[allow(clippy::too_many_arguments)] // driver hub: subscribe/keepalive + 4 closures + health + stop + idle + reseed + clock
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
    // Per-session FRESHNESS reset + ARM (preserves the old per-session `let mut last_update_ms =
    // now_ms();` arm-at-session-start): a new session re-seeds fresh data, so any stale episode left
    // open by the prior outage is dropped here WITHOUT emitting a spurious recovery, AND the freshness
    // clock is armed at `now_ms()` so a session that then receives ZERO book updates (a subscribe that
    // silently succeeds but sends nothing behind a live keepalive'd socket — worst on REST-seeded
    // Binance, where the seed closes the transport gap so nothing else catches the frozen book) still
    // ages toward `Stale` from session start, rather than a dataless session tripping nothing.
    // Transport gap state is deliberately untouched — the `StreamHealth` owned by `run_depth_feed`
    // carries the open gap across the reconnect so the first book below can close it.
    stream_health.reset_freshness(session_start_ms);

    // Recovery is disclosed exactly once per session, on the FIRST published book (§B ordering: the
    // consumer sees the transport `Live` before the re-seeded data). `recover()` is a no-op (returns
    // `None`) when no gap is open — a first connect discloses nothing, matching the venue's former
    // transport-gap tracker's no-op recover. `publish` centralizes that guard.
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
        // Timed book re-seed (net-hardening, checksum-less venues): once the injected clock has
        // advanced `reseed_interval` past session start, END the session with `Reseed` so
        // `run_depth_feed` reconnects + re-snapshots. This bounds a SILENTLY corrupted book (a
        // mis-applied delta that never breaks the seq chain — so no `BookOp::Gap`, no idle, no
        // freshness trip) to ≤ `reseed_interval`, WITHOUT disclosing a transport gap (a scheduled
        // refresh is not an outage). Clocked off `now_ms` (never `Instant`), so it is deterministic;
        // `None` skips the `now_ms()` call entirely, so the default-off path adds zero overhead.
        if let Some(interval_ms) = reseed_interval_ms {
            if now_ms() - session_start_ms >= interval_ms {
                return Ok(SessionOutcome::Reseed);
            }
        }
        // App-level keepalive on a wall-clock cadence (checked before each read; the read timeout
        // guarantees this runs at least every read tick). Kept in the driver so it fires even while
        // data flows AND while idle — the case where the venue would otherwise drop us.
        if let Some(ka) = keepalive {
            if last_ka.elapsed() >= KEEPALIVE_EVERY {
                stream.send_text(ka).map_err(|e| format!("depth keepalive failed: {e:?}"))?;
                last_ka = Instant::now();
            }
        }
        match stream.read_frame() {
            Ok(txt) => match decode(&txt, &mut book) {
                BookOp::Updated(ts) => {
                    // Feed the venue EVENT-TIME (`ts`) to the freshness tracker, falling back to
                    // receive-time (`now_ms()`) when the venue omitted it (`ts <= 0`) — so a frame
                    // that ARRIVES now but is STAMPED old still ages the book. Then, if a stale
                    // episode is open, let `check_freshness` CLOSE it with `Live` BEFORE the book
                    // (recovery-before-data, mirroring how the transport `Live` precedes the re-seeded
                    // book). The `is_stale()` guard keeps this a RECOVERY-only step: it preserves the
                    // old "trip only on the transport-alive timeout tick" behavior — an update
                    // carrying an OLD stamp must not open a fresh `Stale` here on the update frame;
                    // that trip still happens on the next timeout tick below.
                    let stamp = if ts > 0 { ts } else { now_ms() };
                    stream_health.observe_data(stamp);
                    if stream_health.is_stale() {
                        if let Some(ev) = stream_health.check_freshness(now_ms()) {
                            on_health(ev);
                        }
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
/// **The dial is BOUNDED — [`CONNECT_10S`], the same window the market pump spends.** It dialed with
/// a bare `tungstenite::connect` until 2026-08-08, which applies no connect bound of any kind, so
/// [`run_depth_feed`]'s thread sat out the OS's own SYN ladder (~127 s on Linux defaults) on a
/// black-holed route with `stop` already raised behind it — the identical defect the market pump's
/// rows carried, in the driver beside it.
///
/// **It is in `vike-recorder`'s teardown path, and a doc saying otherwise shipped.** The claim was
/// that `RecorderSink` implements no `l2_snapshot` so a depth socket is never opened by the daemon.
/// It is false and was false when written: `crates/vike-data/src/live_rec.rs`'s `RecorderSink`
/// implements `l2_snapshot` (added by #995, which changed
/// `crates/vike-recorder/src/venues/binance.rs`'s module doc but not the sentence that said the verb
/// was swallowed), the depth rows land under `kind=depth`, and
/// `crates/vike-recorder/src/recorder_cli.rs` subscribes `Stream::ALL` — whose `Stream::Depth`
/// arm calls `subscribe_depth`, which binance/bybit/okx all serve (`vike_model::venue_caps` gives
/// them `book: false, depth: true`). So this dial spends
/// `crates/vike-recorder/src/recorder_cli.rs`'s `FEED_STOP_BUDGET_SECS` like any other.
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

/// Reconnect loop around [`run_depth_session`]: on a socket error, gap, or §B idle trip, open a
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
/// per-outage dedup (subsuming the per-venue transport-gap tracker the driver used to lean on); its
/// freshness half is reset per session inside `run_depth_session`.
///
/// `reseed_interval`, if `Some`, is the timed book re-seed: every session that stays healthy that
/// long ends with [`SessionOutcome::Reseed`], on which this loop reconnects + re-seeds WITHOUT opening
/// a transport gap (a scheduled refresh, not an outage — so it does NOT flap the Gap/Live disclosure).
/// It reuses the exact reconnect + re-seed path a fault takes (through the same stop-aware `backoff`),
/// so the snapshot/rebuild logic is not duplicated. `None` (the default) disables it — zero behavior
/// change; a venue with no book checksum opts in to bound silent book corruption to ≤ the interval.
#[allow(clippy::too_many_arguments)] // a driver hub: URL + subscribe/keepalive + 4 closures + stop + timings + reseed + clock
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
    // the `Gap`/`Live` to one pair per outage (subsuming the per-venue transport-gap tracker the
    // driver used to lean on); its freshness half is reset per session inside `run_depth_session`.
    let mut stream_health = StreamHealth::new(freshness_threshold.as_millis() as i64);
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
            Ok(SessionOutcome::Stopped) => break, // requested stop
            // A PLANNED periodic re-seed: fall through to the same stop-aware backoff + reconnect +
            // re-seed a fault takes, but do NOT open a transport gap — a scheduled refresh is not an
            // outage, so it must not flap the `Gap`/`Live` disclosure a health consumer watches.
            Ok(SessionOutcome::Reseed) => {}
            // Session outage (idle trip, socket error, sequence gap, or a failed connect): open a
            // transport gap — deduped to ONE `Gap` per outage across reconnect attempts.
            Err(_) => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// **The depth dial goes through the BOUNDED shared arm** — the test this defect needed and did
    /// not have. [`connect_depth`] hand-rolled `tungstenite::connect`, which applies no connect
    /// bound at all, and the market-pump work that bounded every other on-driver dial left this
    /// driver alone on the claim that it was out of the recorder's path. It is not: see
    /// [`connect_depth`]'s own doc for the chain (`RecorderSink::l2_snapshot` → `kind=depth`, and
    /// `Stream::ALL` → `subscribe_depth`, which binance/bybit/okx all serve).
    ///
    /// **How it observes the path with no network.** `crate::ws_proxy::connect_ws` branches on
    /// `connect_timeout`: `Some` parses the TCP target ITSELF first (`ws_target`, whose rejection is
    /// the distinctive `"bad ws url"`), `None` hands the whole string to `tungstenite::connect`,
    /// which fails in its own parser with its own wording. An unparseable host therefore makes the
    /// two arms say different things, offline and deterministically. The second half asserts they
    /// really do differ, so the discriminator cannot decay into something both arms satisfy.
    #[test]
    fn the_depth_dial_goes_through_the_bounded_shared_path() {
        let bounded = match connect_depth("not a url", Duration::from_secs(2)) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("an unparseable url cannot dial"),
        };
        assert!(
            bounded.starts_with("bad ws url"),
            "the depth dial did not take `connect_ws`'s BOUNDED arm — it is dialing unbounded again, \
             so a black-holed route pins this thread past the stop flag. error was: {bounded}"
        );

        // …and the discriminator discriminates: the unbounded arm fails somewhere else entirely.
        let unbounded = connect_ws("not a url", None, None)
            .expect_err("an unparseable url cannot dial")
            .to_string();
        assert!(
            !unbounded.starts_with("bad ws url"),
            "the unbounded arm must be distinguishable from the bounded one, or this test proves \
             nothing. error was: {unbounded}"
        );
    }

    /// The depth driver spends the SAME window the market pump does — one constant, so the
    /// recorder's `FEED_STOP_BUDGET_SECS` has one number to be derived from. A local copy here
    /// would be a second thing to forget.
    #[test]
    fn the_depth_dial_window_is_the_shared_one() {
        assert_eq!(
            CONNECT_10S,
            crate::pump_spec::market_pump_spec("binance").knobs().connect_timeout.unwrap()
        );
        assert_eq!(
            CONNECT_10S,
            crate::pump_spec::market_pump_spec("bybit").knobs().connect_timeout.unwrap()
        );
        assert_eq!(
            CONNECT_10S,
            crate::pump_spec::market_pump_spec("okx").knobs().connect_timeout.unwrap()
        );
    }

    #[test]
    fn infer_tick_takes_the_min_adjacent_gap() {
        // dense levels one tick (0.01) apart on each side → inferred tick 0.01
        let bids = vec![(100.00, 1.0), (99.99, 2.0), (99.98, 3.0)];
        let asks = vec![(100.02, 1.0), (100.03, 1.0)];
        assert!((infer_tick_size(&bids, &asks) - 0.01).abs() < 1e-9);
    }

    #[test]
    fn infer_tick_uses_both_sides_and_orders_them() {
        // unsorted, cross-side: the smallest gap (0.5, between the ask 101.0 and bid 100.5) wins
        let bids = vec![(100.5, 1.0), (100.0, 1.0)];
        let asks = vec![(102.0, 1.0), (101.0, 1.0)];
        assert!((infer_tick_size(&bids, &asks) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn infer_tick_falls_back_without_two_levels() {
        assert_eq!(infer_tick_size(&[], &[]), 0.01);
        assert_eq!(infer_tick_size(&[(100.0, 1.0)], &[]), 0.01);
    }

    // ---- net-hardening §B: idle watchdog + gap/recovery disclosure over a scripted DepthStream ----

    // The shared scripted DepthStream double (testing-arch Phase 4c) — the canonical copy of the
    // `Step`/`ScriptedDepthStream` pair that used to live inline here. `send_text` records what
    // the driver sent (subscribe + keepalive, read back via `.sent()`); `stalled`/`stalled_clocked`
    // drive the §B idle/freshness watchdogs, the shared clock aging per read so consuming the
    // script ages the book with no real sleep.
    use crate::scripted::ScriptedStream as ScriptedDepthStream;

    /// A tiny test book so `on_book` has something to report; content is irrelevant to the watchdog.
    fn a_book() -> L2Book {
        let mut b = L2Book::new(1.0);
        b.apply_snapshot(1, &[(100.0, 1.0)], &[(101.0, 1.0)]);
        b
    }

    /// Drive one session with a caller-provided [`StreamHealth`] (so a test can pre-open a transport
    /// gap to exercise recovery) + injected `now_ms`, recording `on_health`/`on_book` calls in order.
    /// `decode`: `"gap"` → Gap, `"ignore"` → Ignored, a bare integer text frame → `Updated(<that
    /// i64>)` (its venue event-ts, so a test can feed a stale-STAMPED update), any other text →
    /// `Updated(0)` = receive-time fallback (the pre-change behavior the existing freshness tests rely
    /// on). Returns `(log, result)`. The `on_health` recorder stringifies each [`HealthEvent`] under
    /// the same `status:` prefix the old driver-local status recorder used, so the migrated assertions
    /// read almost unchanged.
    fn drive_clocked_h(
        stream: &mut ScriptedDepthStream,
        stream_health: &mut StreamHealth,
        idle_threshold: Duration,
        reseed_interval: Option<Duration>,
        now_ms: &dyn Fn() -> i64,
        stop: &AtomicBool,
    ) -> (Vec<String>, Result<SessionOutcome, String>) {
        let log = RefCell::new(Vec::<String>::new());
        let mut seed = || None::<L2Book>;
        let mut decode = |txt: &str, book: &mut Option<L2Book>| -> BookOp {
            match txt {
                "gap" => BookOp::Gap,
                "ignore" => BookOp::Ignored,
                // A bare integer text frame IS the venue event-ts (epoch-ms) this update carries — lets
                // a test feed a stale-STAMPED update; any other text (`"snap"`) parses to `0` =
                // receive-time fallback, the pre-change behavior the other freshness tests rely on.
                _ => {
                    *book = Some(a_book());
                    BookOp::Updated(txt.parse::<i64>().unwrap_or(0))
                }
            }
        };
        let mut on_book = |b: &L2Book| log.borrow_mut().push(format!("book:{:?}", b.best_bid()));
        let mut on_health = |ev: HealthEvent| log.borrow_mut().push(format!("status:{ev:?}"));
        let res = run_depth_session(
            stream,
            None,
            None,
            &mut seed,
            &mut decode,
            &mut on_book,
            &mut on_health,
            stream_health,
            stop,
            idle_threshold,
            reseed_interval,
            now_ms,
        )
        .map_err(|e| e.to_string());
        (log.into_inner(), res)
    }

    /// Like [`drive_clocked_h`] but builds a FRESH [`StreamHealth`] from `freshness_threshold` — the
    /// default for the freshness tests (no prior transport gap, so `recover()` is a no-op on the first
    /// book, matching a first connect exactly as the venue's old transport-gap tracker did). Reseed is
    /// OFF (`None`); [`drive_reseed`] threads a `reseed_interval` for the periodic-reseed tests.
    fn drive_clocked(
        stream: &mut ScriptedDepthStream,
        idle_threshold: Duration,
        freshness_threshold: Duration,
        now_ms: &dyn Fn() -> i64,
        stop: &AtomicBool,
    ) -> (Vec<String>, Result<SessionOutcome, String>) {
        drive_reseed(stream, idle_threshold, freshness_threshold, None, now_ms, stop)
    }

    /// Like [`drive_clocked`] but threads a `reseed_interval` (the timed book re-seed) — for the
    /// periodic-reseed tests. Fresh [`StreamHealth`] (no prior gap), so the only way the session ends
    /// with [`SessionOutcome::Reseed`] is the reseed timer, not a recovered gap.
    fn drive_reseed(
        stream: &mut ScriptedDepthStream,
        idle_threshold: Duration,
        freshness_threshold: Duration,
        reseed_interval: Option<Duration>,
        now_ms: &dyn Fn() -> i64,
        stop: &AtomicBool,
    ) -> (Vec<String>, Result<SessionOutcome, String>) {
        let mut health = StreamHealth::new(freshness_threshold.as_millis() as i64);
        drive_clocked_h(stream, &mut health, idle_threshold, reseed_interval, now_ms, stop)
    }

    /// The idle/recovery tests don't exercise DATA-freshness: a threshold larger than any scripted
    /// clock advance + a constant `now_ms` keep that watchdog dormant, so these read exactly as
    /// before (the freshness state machine is proven separately, below).
    fn drive(
        stream: &mut ScriptedDepthStream,
        idle_threshold: Duration,
        stop: &AtomicBool,
    ) -> (Vec<String>, Result<SessionOutcome, String>) {
        drive_clocked(stream, idle_threshold, Duration::from_secs(86_400), &|| 0, stop)
    }

    /// Like [`drive`] (dormant freshness watchdog, constant clock) but with a caller-provided
    /// [`StreamHealth`] — for the recovery test, which pre-opens a transport gap so the first book's
    /// `recover()` fires with a `Live`.
    fn drive_h(
        stream: &mut ScriptedDepthStream,
        stream_health: &mut StreamHealth,
        idle_threshold: Duration,
        stop: &AtomicBool,
    ) -> (Vec<String>, Result<SessionOutcome, String>) {
        drive_clocked_h(stream, stream_health, idle_threshold, None, &|| 0, stop)
    }

    /// A stalled-but-open stream (read-timeouts, no frames) past the idle threshold must return `Err`
    /// so `run_depth_feed` reconnects — the dead-transport detection itself. `stall = 60s` vs a
    /// `30s` threshold trips on the first timeout tick, with no real sleep.
    #[test]
    fn idle_watchdog_trips_when_stalled_past_threshold() {
        let mut stream = ScriptedDepthStream::stalled(Duration::from_secs(60));
        stream.push_timeout();
        let stop = AtomicBool::new(false);
        let (log, res) = drive(&mut stream, Duration::from_secs(30), &stop);
        let err =
            res.expect_err("a stalled stream must Err so the feed reconnects and opens a gap");
        assert!(err.contains("idle"), "the error names the idle-watchdog trip: {err}");
        assert!(log.is_empty(), "no data flowed, nothing published: {log:?}");
    }

    /// A connection kept alive purely by keepalive traffic keeps `since_last_frame` fresh, so MANY
    /// read-timeout ticks are absorbed WITHOUT a false idle trip. Only true silence (nothing inbound
    /// for the whole threshold) may trip. Here the 5s liveness clock stays under the 30s threshold,
    /// so the session ends on the scripted close, not an idle trip.
    #[test]
    fn a_keepalive_fresh_stream_is_not_falsely_declared_idle() {
        let mut stream = ScriptedDepthStream::stalled(Duration::from_secs(5));
        stream.push_timeout();
        stream.push_timeout();
        stream.push_timeout();
        let stop = AtomicBool::new(false);
        let (log, res) = drive(&mut stream, Duration::from_secs(30), &stop);
        let err = res.expect_err("the script ends by exhaustion (Closed), not an idle trip");
        assert!(
            !err.contains("idle"),
            "a keepalive-fresh stream must NOT trip the watchdog: {err}"
        );
        assert!(log.is_empty(), "no data flowed, nothing disclosed: {log:?}");
    }

    /// The first book of a session that RECOVERS from an open transport gap discloses `Live` (the
    /// recovery) BEFORE the book itself, so a consumer sees "recovered" ahead of the re-seeded data
    /// (§B ordering, via the pre-publish `recover()` check). Recovery is gap-aware now — the driver
    /// owns the `StreamHealth` — so we pre-open a gap (a prior outage) to exercise it. This is the
    /// translation of the old recovered-before-book test: the old DRIVER always emitted a recovered
    /// status and the venue's transport-gap tracker turned it into `Live` (gap open) or a no-op (no
    /// gap); the dedup now lives in the driver, so `recover()` emits `Live` ONLY when a gap is open.
    /// End-to-end the venue sink sees the SAME thing (a `Live` closing the gap, before the book).
    #[test]
    fn recovery_is_disclosed_before_the_first_book() {
        let mut health = StreamHealth::new(86_400_000);
        health.enter_gap(1000); // a prior outage opened a transport gap
        let mut stream = ScriptedDepthStream::from_texts(["snap"]);
        let stop = AtomicBool::new(false);
        let (log, _res) = drive_h(&mut stream, &mut health, Duration::from_secs(30), &stop);
        assert_eq!(
            log[0], "status:Live { gap_started_ts_ms: Some(1000) }",
            "recovery (Live) disclosed first, closing the open gap: {log:?}"
        );
        assert!(log[1].starts_with("book:"), "then the re-seeded book: {log:?}");
        // exactly one Live per session — a second book does not re-disclose it
        let mut health2 = StreamHealth::new(86_400_000);
        health2.enter_gap(1000);
        let mut stream2 = ScriptedDepthStream::from_texts(["snap", "snap"]);
        let (log2, _) = drive_h(&mut stream2, &mut health2, Duration::from_secs(30), &stop);
        assert_eq!(
            log2.iter().filter(|c| c.contains("Live")).count(),
            1,
            "the recovery Live fires once per session, not per book: {log2:?}"
        );
    }

    /// A first connect with NO open transport gap discloses nothing before the first book —
    /// `recover()` is a no-op (returns `None`), matching the venue's old transport-gap tracker's no-op
    /// recover on first connect. (The recovered status the old DRIVER always emitted was a venue-side
    /// no-op in exactly this case; the dedup moved into the driver's `StreamHealth`, so the venue sink
    /// still sees the same thing: only the book.)
    #[test]
    fn a_first_connect_with_no_gap_discloses_no_recovery() {
        let mut stream = ScriptedDepthStream::from_texts(["snap"]);
        let stop = AtomicBool::new(false);
        let (log, _res) = drive(&mut stream, Duration::from_secs(30), &stop);
        assert!(
            log[0].starts_with("book:"),
            "no recovery disclosed on a gapless first connect — the book is first: {log:?}"
        );
        assert!(
            log.iter().all(|c| !c.starts_with("status:")),
            "a gapless session discloses no stream-health status: {log:?}"
        );
    }

    /// A sequence-gap frame ends the session with `Err` (so the reconnect loop opens a gap) and — a
    /// gap on the very first frame, before any book — publishes nothing, so no `Recovered` leaks out.
    #[test]
    fn a_sequence_gap_frame_ends_the_session_with_err_and_no_recovery() {
        let mut stream = ScriptedDepthStream::from_texts(["gap"]);
        let stop = AtomicBool::new(false);
        let (log, res) = drive(&mut stream, Duration::from_secs(30), &stop);
        assert!(res.is_err(), "a gap frame ends the session so the driver resyncs");
        assert!(log.is_empty(), "no book published before the gap, so no disclosure: {log:?}");
    }

    /// A stop flag already raised before the first read returns `Ok(())` immediately, touching
    /// nothing — the deterministic-teardown property.
    #[test]
    fn stop_flag_set_before_the_first_read_exits_cleanly() {
        let mut stream = ScriptedDepthStream::from_texts(["snap"]);
        let stop = AtomicBool::new(true);
        let (log, res) = drive(&mut stream, Duration::from_secs(30), &stop);
        assert!(res.is_ok(), "a pre-raised stop exits Ok");
        assert!(log.is_empty(), "a pre-raised stop must not process any frame: {log:?}");
    }

    /// **A stop already raised must skip the REST book SEED, not merely the read loop** — the test
    /// this defect needed and did not have.
    ///
    /// [`stop_flag_set_before_the_first_read_exits_cleanly`] above looks like it covers this and does
    /// not: its `seed` returns `None` and records nothing, so it passed unchanged while the seed ran
    /// on every stopped session. That matters because the seed is a REST round trip on binance/aster
    /// — the LONGEST thing between a session's dial and the loop's first flag read, and the recorder's
    /// `FEED_STOP_BUDGET_SECS` is derived from the largest single position a feed thread can be caught
    /// in. A `stop` raised during the (bounded) dial would otherwise buy a whole extra network call.
    ///
    /// MUTATION PROOF: delete the `stop` check in front of `let mut book = seed();` and this goes
    /// red on `the seed ran anyway` — no network, no timing, no clock, so it fails the same way on
    /// any box.
    #[test]
    fn a_stop_raised_before_the_session_skips_the_rest_seed() {
        let seeded = Cell::new(false);
        let stop = AtomicBool::new(true);
        let mut stream = ScriptedDepthStream::from_texts(["snap"]);
        let mut seed = || {
            seeded.set(true);
            Some(a_book())
        };
        let mut decode = |_t: &str, _b: &mut Option<L2Book>| BookOp::Ignored;
        let mut on_book = |_b: &L2Book| {};
        let mut on_health = |_ev: HealthEvent| {};
        let mut health = StreamHealth::new(86_400_000);
        let res = run_depth_session(
            &mut stream,
            None,
            None,
            &mut seed,
            &mut decode,
            &mut on_book,
            &mut on_health,
            &mut health,
            &stop,
            Duration::from_secs(30),
            None,
            &|| 0,
        );
        assert!(
            matches!(res, Ok(SessionOutcome::Stopped)),
            "a pre-raised stop ends the session cleanly"
        );
        assert!(
            !seeded.get(),
            "the seed ran anyway — a stop landing during the dial now pays for a whole extra REST \
             round trip on top of it, which is outside the recorder's FEED_STOP_BUDGET_SECS \
             derivation. Poll `stop` before calling `seed`."
        );
    }

    /// The subscribe frame is written once, before the first read.
    #[test]
    fn subscribe_is_sent_before_the_first_read() {
        let mut stream = ScriptedDepthStream::from_texts(["snap"]);
        let stop = AtomicBool::new(false);
        let log = RefCell::new(Vec::<String>::new());
        let mut seed = || None::<L2Book>;
        let mut decode = |_t: &str, book: &mut Option<L2Book>| {
            *book = Some(a_book());
            BookOp::Updated(0)
        };
        let mut on_book = |_b: &L2Book| log.borrow_mut().push("book".into());
        let mut on_health = |_ev: HealthEvent| {};
        let mut health = StreamHealth::new(86_400_000);
        let _ = run_depth_session(
            &mut stream,
            Some(r#"{"op":"subscribe"}"#),
            None,
            &mut seed,
            &mut decode,
            &mut on_book,
            &mut on_health,
            &mut health,
            &stop,
            Duration::from_secs(30),
            None, // reseed off
            &|| 0,
        );
        assert_eq!(stream.sent().first().map(String::as_str), Some(r#"{"op":"subscribe"}"#));
    }

    // ---- net-hardening §B: DATA-freshness watchdog (the data-side twin of the idle watchdog) ----
    //
    // Transport stays ALIVE (`stall` well under `idle_threshold`, so the idle watchdog never trips);
    // the injected `now_ms` reads a clock the `ScriptedDepthStream` ages a fixed amount per read, so
    // the book's data-age is driven deterministically by consuming the script — zero real sleeps.

    /// A shared data clock + a `now_ms` closure reading it, for the freshness tests.
    fn clock_and_now() -> (Rc<Cell<i64>>, impl Fn() -> i64) {
        let clock = Rc::new(Cell::new(0_i64));
        let now = {
            let c = Rc::clone(&clock);
            move || c.get()
        };
        (clock, now)
    }

    /// Pulls the `i64` value of a named field out of one recorded `status:Variant { field: N, .. }`
    /// log entry (Debug-format parsing — `drive_clocked`'s log is `Vec<String>`, not raw
    /// `HealthEvent`). Used below to prove a recovery `Live { gap_started_ts_ms }` echoes the exact
    /// `now_ms` of the `Stale` disclosure it closes, without restructuring the string-based recorder
    /// every other test in this module already relies on. Skips any non-numeric prefix after the
    /// field key (e.g. an `Option`'s `Some(`), so both a bare `newest_data_ts_ms: 20000` and a wrapped
    /// `gap_started_ts_ms: Some(60000)` parse to their inner `i64`.
    fn field_i64(entry: &str, field: &str) -> i64 {
        let key = format!("{field}: ");
        let at = entry.find(&key).unwrap_or_else(|| panic!("{field} not found in {entry:?}"));
        let rest = &entry[at + key.len()..];
        // Skip any non-numeric prefix (e.g. an `Option`'s `Some(`) to the value's first digit / `-`.
        let start = rest
            .find(|c: char| c.is_ascii_digit() || c == '-')
            .unwrap_or_else(|| panic!("no numeric value for {field} in {entry:?}"));
        let rest = &rest[start..];
        let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
        rest[..end]
            .parse::<i64>()
            .unwrap_or_else(|_| panic!("failed to parse {field} from {entry:?}"))
    }

    /// (1) Book updates keep arriving, each within the freshness window (the clock advances 10s per
    /// read, threshold 30s) → the data watchdog stays quiet: NO `Stale` ever disclosed.
    #[test]
    fn steady_book_updates_never_go_stale() {
        let (clock, now) = clock_and_now();
        let mut stream =
            ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 10_000);
        for _ in 0..3 {
            stream.push_text("snap");
            stream.push_text("snap");
            stream.push_timeout();
        }
        let stop = AtomicBool::new(false);
        let (log, _res) = drive_clocked(
            &mut stream,
            Duration::from_secs(300), // idle threshold — far above `stall`, never trips
            Duration::from_secs(30),  // freshness threshold
            &now,
            &stop,
        );
        assert!(
            !log.iter().any(|c| c.contains("Stale")),
            "a book updated within the freshness window must never be declared stale: {log:?}"
        );
    }

    /// (2) Transport stays alive but NO book update lands past the freshness window (only `Timeout`
    /// ticks after one seed update; the clock advances 20s per read, threshold 30s) → EXACTLY ONE
    /// `Stale`, and never a `Gap` (the idle watchdog never trips — the transport is alive).
    #[test]
    fn a_frozen_book_behind_a_live_transport_goes_stale_once() {
        let (clock, now) = clock_and_now();
        let mut stream =
            ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
        stream.push_text("snap"); // one update at t=20s → the newest data ts
        stream.push_timeout(); // t=40s, age 20s < 30s
        stream.push_timeout(); // t=60s, age 40s > 30s → Stale
        stream.push_timeout(); // t=80s, age 60s — already stale, no re-fire
        stream.push_timeout(); // t=100s, still stale
        let stop = AtomicBool::new(false);
        let (log, res) = drive_clocked(
            &mut stream,
            Duration::from_secs(300),
            Duration::from_secs(30),
            &now,
            &stop,
        );
        let stale = log.iter().filter(|c| c.contains("Stale")).count();
        assert_eq!(stale, 1, "a frozen book discloses Stale exactly once per episode: {log:?}");
        assert!(
            !log.iter().any(|c| c.contains("Gap")),
            "the transport stayed alive — no transport Gap may be disclosed: {log:?}"
        );
        let err = res.expect_err("the script ends by exhaustion (Closed), not an idle trip");
        assert!(!err.contains("idle"), "the freshness trip is NOT an idle-transport trip: {err}");
    }

    /// (3) After a `Stale` episode a book update resumes → EXACTLY ONE recovery `Live`, disclosed
    /// BEFORE the resuming book (recovery-before-data). The old driver-local "fresh" status maps to
    /// `HealthEvent::Live` (freshness recovery reuses the shared `Live`, exactly as the venue does
    /// onto `vike_data::StreamStatus::Live`); this test uses a gapless session, so the only `Live` in
    /// the log is this freshness recovery (no transport `Live`).
    #[test]
    fn a_resuming_book_update_discloses_fresh_once_before_its_book() {
        let (clock, now) = clock_and_now();
        let mut stream =
            ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
        stream.push_text("snap"); // t=20s update
        stream.push_timeout(); // t=40s, age 20s
        stream.push_timeout(); // t=60s, age 40s > 30s → Stale
        stream.push_text("snap"); // t=80s update → Fresh, then the book
        stream.push_timeout(); // t=100s, age 20s — fresh again, quiet
        let stop = AtomicBool::new(false);
        let (log, _res) = drive_clocked(
            &mut stream,
            Duration::from_secs(300),
            Duration::from_secs(30),
            &now,
            &stop,
        );
        assert_eq!(
            log.iter().filter(|c| c.contains("Live")).count(),
            1,
            "a resumed book update closes the episode with exactly one recovery Live: {log:?}"
        );
        let live_i = log.iter().position(|c| c.contains("Live")).expect("a Live was disclosed");
        let stale_i = log.iter().position(|c| c.contains("Stale")).expect("a Stale preceded it");
        assert!(stale_i < live_i, "Stale precedes the recovery Live: {log:?}");
        assert!(
            log[live_i + 1].starts_with("book:"),
            "the recovery Live is disclosed BEFORE the resuming book: {log:?}"
        );
        // Trip-time echo (mirrors `vike_data::StreamStatus::Live`'s `gap_started_ts_ms` contract,
        // and Polymarket's freshness recovery): the closing `Live`'s `gap_started_ts_ms` must equal
        // the `now_ms` the preceding `Stale` was judged at, not e.g. the `newest_data_ts_ms` or a
        // fresh clock read.
        assert_eq!(
            field_i64(&log[live_i], "gap_started_ts_ms"),
            field_i64(&log[stale_i], "now_ms"),
            "the recovery Live must echo the Stale episode's trip time (now_ms): {log:?}"
        );
    }

    /// (4) A book updated within the freshness window, then a quiet stretch that never ages past it
    /// (clock advances 5s per read, threshold 30s) → sparse-but-current: NO `Stale`.
    #[test]
    fn a_sparse_but_current_book_is_not_stale() {
        let (clock, now) = clock_and_now();
        let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 5_000);
        stream.push_text("snap"); // t=5s update
        for _ in 0..4 {
            stream.push_timeout(); // t=10s..25s, age never exceeds 20s < 30s
        }
        let stop = AtomicBool::new(false);
        let (log, _res) = drive_clocked(
            &mut stream,
            Duration::from_secs(300),
            Duration::from_secs(30),
            &now,
            &stop,
        );
        assert!(
            !log.iter().any(|c| c.contains("Stale")),
            "a recently-updated book stays fresh through a quiet stretch under the window: {log:?}"
        );
    }

    /// (5) Re-arm: TWO freshness episodes in one session prove the stale episode resets per-episode,
    /// not just once. Script: update → age past threshold → `Stale#1` → update resumes → recovery
    /// `Live#1` → age past threshold again → `Stale#2` (the script ends there, so the SECOND episode
    /// never recovers). Exactly two `Stale` and exactly one recovery `Live` are disclosed, and — since
    /// the transport never drops in this (gapless) script — no `Gap` either.
    #[test]
    fn a_second_freshness_episode_re_arms_after_the_first_recovers() {
        let (clock, now) = clock_and_now();
        let mut stream =
            ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
        stream.push_text("snap"); // t=20s update
        stream.push_timeout(); // t=40s, age 20s
        stream.push_timeout(); // t=60s, age 40s > 30s → Stale #1
        stream.push_text("snap"); // t=80s update → Fresh #1, then the book
        stream.push_timeout(); // t=100s, age 20s — fresh again, quiet
        stream.push_timeout(); // t=120s, age 40s > 30s → Stale #2 (re-armed; unresolved — script ends)
        let stop = AtomicBool::new(false);
        let (log, _res) = drive_clocked(
            &mut stream,
            Duration::from_secs(300),
            Duration::from_secs(30),
            &now,
            &stop,
        );
        let stale = log.iter().filter(|c| c.contains("Stale")).count();
        let live = log.iter().filter(|c| c.contains("Live")).count();
        assert_eq!(stale, 2, "two separate episodes must each disclose their own Stale: {log:?}");
        assert_eq!(
            live, 1,
            "only the first episode recovers within this script (one Live): {log:?}"
        );
        assert!(
            !log.iter().any(|c| c.contains("Gap")),
            "the transport never dropped in this script — no Gap may be disclosed: {log:?}"
        );
    }

    /// (6) THE NEW CAPABILITY: a frame that ARRIVES now but is STAMPED old — a venue replaying stale
    /// data. The transport stays alive (`stall` ≪ idle threshold) and the injected `now_ms` barely
    /// advances across the frame's arrival and the judging tick (1s/read), so a RECEIVE-time clock
    /// would read the update as fresh (it just landed) and NEVER trip. Because the driver now clocks
    /// staleness off the VENUE EVENT-TIME the `Updated` carries (here `5_000` epoch-ms, ~245s behind a
    /// "now" pre-advanced to ~250s), the very next timeout tick judges it stale → EXACTLY ONE `Stale`,
    /// and — the transport never dropped — no `Gap`.
    #[test]
    fn a_stale_stamped_update_behind_a_live_transport_goes_stale_once() {
        let (clock, now) = clock_and_now();
        clock.set(250_000); // "now" is already well past the stale event-stamp fed below
        let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 1_000);
        stream.push_text("5000"); // an Updated STAMPED at t=5s (epoch-ms) — ~245s behind "now"…
        stream.push_timeout(); // …now ≈ 252s, age = 252s − 5s = 247s > 30s → Stale (receive-time: age ≈ 1s, no trip)
        stream.push_timeout(); // still stale — no re-fire
        stream.push_timeout();
        let stop = AtomicBool::new(false);
        let (log, res) = drive_clocked(
            &mut stream,
            Duration::from_secs(300), // idle threshold — transport stays alive throughout
            Duration::from_secs(30),  // freshness threshold
            &now,
            &stop,
        );
        let stale = log.iter().filter(|c| c.contains("Stale")).count();
        assert_eq!(
            stale, 1,
            "a stale-STAMPED update (old venue ts) behind a live transport trips Stale exactly once, \
             even though the frame just arrived — receive-time freshness would MISS it: {log:?}"
        );
        // The Stale must carry the VENUE event-ts (5_000), not the receive-time (~251_000) — proof the
        // freshness clock is the venue stamp, which is the whole point of this change.
        let stale_entry = log.iter().find(|c| c.contains("Stale")).expect("a Stale was disclosed");
        assert_eq!(
            field_i64(stale_entry, "newest_data_ts_ms"),
            5_000,
            "Stale.newest_data_ts_ms is the venue event-time of the update, not receive-time: {log:?}"
        );
        assert!(
            !log.iter().any(|c| c.contains("Gap")),
            "the transport stayed alive — no transport Gap may be disclosed: {log:?}"
        );
        let err = res.expect_err("the script ends by exhaustion (Closed), not an idle trip");
        assert!(!err.contains("idle"), "the freshness trip is NOT an idle-transport trip: {err}");
    }

    /// (7) THE §B ZERO-DATA REGRESSION this fix restores: a session that SEEDS/publishes a first book
    /// but then receives ZERO `BookOp::Updated` frames behind a LIVE transport (only timeout ticks,
    /// `stall` ≪ idle threshold) must STILL go `Stale` — via the arm-at-session-start floor. This is
    /// the capability the old inline freshness had (it armed `last_update_ms = now_ms()` at session
    /// start) that a naive `reset_freshness` clearing `newest_ts` to `None` would lose: a subscribe
    /// that silently sends nothing (worst on REST-seeded Binance — the seed closes the transport gap,
    /// so nothing else catches the frozen book). Exactly one `Stale` — carrying the session-start arm
    /// time as `newest_data_ts_ms` (no real data ever landed) — and no `Gap` (transport stayed alive).
    #[test]
    fn a_seeded_but_dataless_session_goes_stale() {
        let (clock, now) = clock_and_now();
        clock.set(1_000); // session-start "now" — the arm floor
        let mut stream =
            ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
        // NO data frames — only timeout ticks, so zero `BookOp::Updated`; the clock ages 20s/read.
        stream.push_timeout(); // now → 21_000, age 20s < 30s → still fresh
        stream.push_timeout(); // now → 41_000, age 40s > 30s → Stale (from the 1_000 arm floor)
        stream.push_timeout(); // now → 61_000, still stale — no re-fire
        let stop = AtomicBool::new(false);

        // A seed that publishes a first book (the REST snapshot), mirroring Binance; then only
        // timeouts. `seed` is called once by `run_depth_session`, so this returns the one book.
        let log = RefCell::new(Vec::<String>::new());
        let mut seed = || Some(a_book());
        let mut decode = |_txt: &str, _book: &mut Option<L2Book>| -> BookOp { BookOp::Ignored };
        let mut on_book = |b: &L2Book| log.borrow_mut().push(format!("book:{:?}", b.best_bid()));
        let mut on_health = |ev: HealthEvent| log.borrow_mut().push(format!("status:{ev:?}"));
        let mut health = StreamHealth::new(Duration::from_secs(30).as_millis() as i64);
        let _ = run_depth_session(
            &mut stream,
            None,
            None,
            &mut seed,
            &mut decode,
            &mut on_book,
            &mut on_health,
            &mut health,
            &stop,
            Duration::from_secs(300), // idle threshold — far above `stall`, never trips
            None,                     // reseed off
            &now,
        );
        let log = log.into_inner();
        assert!(log[0].starts_with("book:"), "the seed book is published first: {log:?}");
        let stale = log.iter().filter(|c| c.contains("Stale")).count();
        assert_eq!(
            stale, 1,
            "a seeded-but-dataless session still trips Stale exactly once from the arm floor: {log:?}"
        );
        let stale_entry = log.iter().find(|c| c.contains("Stale")).expect("a Stale was disclosed");
        assert_eq!(
            field_i64(stale_entry, "newest_data_ts_ms"),
            1_000,
            "Stale carries the session-start arm time (no real data ever landed): {log:?}"
        );
        assert!(
            !log.iter().any(|c| c.contains("Gap")),
            "the transport stayed alive — no transport Gap may be disclosed: {log:?}"
        );
    }

    /// (8) DIVERGENCE-2 PIN (accepted improvement): a stale-STAMPED update that ARRIVES while the book
    /// is already `Stale` must NOT falsely recover. The old inline freshness flapped here — an update
    /// received during a stale episode set `last_update_ms` and cleared stale, emitting a spurious
    /// `Live`, even though the update's OWN (old) event-time was still past the threshold; the very
    /// next tick then re-tripped `Stale` (a Live/Stale flap). The unified `StreamHealth` clocks
    /// recovery off the update's VENUE event-time via `newest_ts`, so a stale-stamped update keeps the
    /// episode open — recovery fires only when data is genuinely fresh again. Exactly one `Stale`,
    /// zero `Live`, and the following tick discloses nothing new.
    #[test]
    fn a_stale_stamped_update_while_stale_does_not_falsely_recover() {
        let (clock, now) = clock_and_now();
        let mut stream =
            ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
        stream.push_text("snap"); // t=20s: a fresh update (receive-time stamp) → newest data ts 20s
        stream.push_timeout(); // t=40s, age 20s < 30s
        stream.push_timeout(); // t=60s, age 40s > 30s → Stale
        stream.push_text("25000"); // t=80s: an update STAMPED at 25s — still 55s (> 30s) behind now
        stream.push_timeout(); // t=100s: still stale — no re-fire, no recovery
        let stop = AtomicBool::new(false);
        let (log, _res) = drive_clocked(
            &mut stream,
            Duration::from_secs(300),
            Duration::from_secs(30),
            &now,
            &stop,
        );
        assert_eq!(
            log.iter().filter(|c| c.contains("Stale")).count(),
            1,
            "exactly one Stale — the episode opens once and stays open: {log:?}"
        );
        assert!(
            !log.iter().any(|c| c.contains("Live")),
            "a stale-STAMPED update received while stale must NOT falsely recover (no Live): {log:?}"
        );
        assert_eq!(
            log.iter().filter(|c| c.starts_with("status:")).count(),
            1,
            "only the single Stale is disclosed — the following tick emits nothing new: {log:?}"
        );
    }

    // ---- net-hardening: the timed book re-seed (checksum-less venues) ----------------------------
    //
    // Same scripted-clock seam as the freshness tests: transport ALIVE (`stall` ≪ idle threshold) and
    // the injected `now_ms` reads a clock the stream ages a fixed amount per read, so elapsed session
    // time is driven purely by consuming the script — zero real sleeps.

    /// A HEALTHY, in-sync stream (updates flowing, no gap, transport alive) whose injected `now_ms`
    /// advances past `reseed_interval` must END the session with [`SessionOutcome::Reseed`] — so
    /// [`run_depth_feed`] reconnects + re-snapshots — even though NOTHING is observably wrong (no
    /// `BookOp::Gap`, no idle, no `Stale`). This is the whole point: it converts a book that silently
    /// corrupts while its seq chain stays intact from "wrong forever" into "wrong ≤ reseed_interval".
    /// And because the reseed is a PLANNED refresh, the session discloses NO stream-health status.
    #[test]
    fn a_periodic_reseed_fires_on_a_healthy_in_sync_stream_past_the_interval() {
        let (clock, now) = clock_and_now();
        // Healthy updates (each "snap" → BookOp::Updated, receive-time fresh); clock ages 20s/read.
        let mut stream =
            ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
        for _ in 0..6 {
            stream.push_text("snap"); // an in-sync update — no gap ever
        }
        let stop = AtomicBool::new(false);
        let (log, res) = drive_reseed(
            &mut stream,
            Duration::from_secs(300), // idle threshold — transport stays alive throughout
            Duration::from_secs(600), // freshness threshold — never trips before the reseed
            Some(Duration::from_secs(60)), // reseed after ~60s of injected clock (trips on read #4)
            &now,
            &stop,
        );
        assert_eq!(
            res,
            Ok(SessionOutcome::Reseed),
            "a healthy in-sync stream past reseed_interval ends the session with Reseed: {log:?}"
        );
        // It really was HEALTHY: books were published and NOTHING unhealthy was disclosed — a planned
        // reseed must not flap Gap/Stale/Live (the suppression `run_depth_feed` relies on).
        assert!(
            log.iter().any(|c| c.starts_with("book:")),
            "the healthy stream published books before the reseed: {log:?}"
        );
        assert!(
            log.iter().all(|c| !c.starts_with("status:")),
            "a healthy periodic reseed discloses no gap/stale/live status: {log:?}"
        );
    }

    /// The timed re-seed is OFF by default: with `reseed_interval = None`, NO amount of elapsed
    /// `now_ms` ends the session with [`SessionOutcome::Reseed`]. Drive the SAME healthy stream, its
    /// clock racing far past any interval a caller might pick, and let the script exhaust — the session
    /// ends by the stream closing (`Err`), never by a reseed. This is the zero-behavior-change guarantee.
    #[test]
    fn reseed_interval_none_never_triggers_a_reseed() {
        let (clock, now) = clock_and_now();
        // Clock ages 60s/read → races to 360s across the script, dwarfing any plausible interval.
        let mut stream =
            ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 60_000);
        for _ in 0..6 {
            stream.push_text("snap"); // healthy updates the whole way
        }
        let stop = AtomicBool::new(false);
        let (log, res) = drive_reseed(
            &mut stream,
            Duration::from_secs(3_600), // idle threshold — never trips
            Duration::from_secs(7_200), // freshness threshold — never trips
            None,                       // reseed OFF (the default)
            &now,
            &stop,
        );
        assert!(
            !matches!(res, Ok(SessionOutcome::Reseed)),
            "reseed_interval=None must NEVER trigger a forced re-seed: {res:?} / {log:?}"
        );
        assert!(
            res.is_err(),
            "with reseed off, a healthy stream runs until the socket closes (script exhaustion): {log:?}"
        );
    }
}
