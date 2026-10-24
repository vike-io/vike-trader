//! The single-token session seam: the watchdog and timing knobs, and `run_session` over one seat.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use vike_bridge_core::market_pump::MarketStream;
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_bridge_core::stream_health::StreamHealth;
use vike_data::LiveDataSink;

use super::state::{PumpMode, TokenSlot, TokenState};
use super::{VENUE, run_shard_session};

/// Liveness-watchdog wiring threaded into [`run_session`] (net-hardening §B). It bundles the ONE
/// unified [`StreamHealth`] — owned by [`feed_main`] ACROSS reconnects — with this stream's
/// disclosure label and the timing knobs. `StreamHealth` fuses what used to be two separate
/// trackers into one state machine:
/// - TRANSPORT liveness: one `Gap`/`Live` pair per outage;
/// - DATA freshness (the follow-up the `run_session` scope note foreshadowed) — `Stale` when the
///   newest applied data ts lags wall-clock past the mode's freshness threshold
///   (`PumpMode::freshness_threshold`) behind a still-live socket, `Live` on recovery. Persists
///   across reconnects too, so a reconnect's re-seed `observe_data` naturally closes any open
///   staleness episode.
///
/// Production builds it via [`Watchdog::new`] (real 30s idle / 10s keepalive / per-mode freshness —
/// `PumpMode::freshness_threshold`); the scripted tests build it via [`Watchdog::with_timing`] to
/// drive the idle-trip and keepalive paths with zero-length durations, no real-time sleeps.
pub struct Watchdog<'a> {
    health: &'a mut StreamHealth,
    timing: PumpTiming<'a>,
}

impl<'a> Watchdog<'a> {
    /// Production wiring: the 30 s idle / 10 s keepalive knobs of this venue's `MarketPumpSpec`
    /// row (the one edit site for both values).
    pub fn new(health: &'a mut StreamHealth, stream_label: &'a str) -> Self {
        Watchdog { health, timing: PumpTiming::new(stream_label) }
    }

    /// Explicit-timing constructor so tests can force the idle/keepalive paths deterministically
    /// (e.g. `idle_threshold = Duration::ZERO` trips on the first read-timeout tick). The
    /// data-freshness threshold is carried by the injected `health` state itself
    /// ([`StreamHealth::new`]), so a test picks it there — mirroring how the idle threshold is
    /// passed here rather than read from the production const.
    pub fn with_timing(
        health: &'a mut StreamHealth,
        stream_label: &'a str,
        idle_threshold: Duration,
        ping_every: Duration,
    ) -> Self {
        Watchdog {
            health,
            timing: PumpTiming::with_timing(stream_label, idle_threshold, ping_every),
        }
    }
}

/// The health-free half of [`Watchdog`]: the disclosure label + the two timing knobs a session
/// needs, split out because a BATCHED session's health lives per token in its [`TokenSlot`]s rather
/// than in one socket-wide tracker. `Watchdog` is now this plus the single-token seam's `health`
/// borrow, so the two constructors above stay the only places the venue's row is read.
#[derive(Debug, Clone, Copy)]
pub struct PumpTiming<'a> {
    /// The `stream` key of this pump's `LiveDataSink::stream_status` disclosures (§B) — in
    /// production [`PumpMode::as_str`].
    pub stream_label: &'a str,
    /// Silent-stall watchdog window (production: the venue row's 30 s).
    pub idle_threshold: Duration,
    /// Client `"PING"` cadence (production: the venue row's 10 s).
    pub ping_every: Duration,
}

impl<'a> PumpTiming<'a> {
    /// Production wiring: the 30 s idle / 10 s keepalive knobs of this venue's `MarketPumpSpec` row.
    pub fn new(stream_label: &'a str) -> Self {
        let knobs = market_pump_spec(VENUE).knobs();
        PumpTiming {
            stream_label,
            idle_threshold: knobs.idle_threshold.expect("polymarket row declares idle watchdog"),
            ping_every: knobs.keepalive_every.expect("polymarket row declares keepalive"),
        }
    }

    /// Explicit-timing constructor (the test seam — see [`Watchdog::with_timing`]).
    pub fn with_timing(
        stream_label: &'a str,
        idle_threshold: Duration,
        ping_every: Duration,
    ) -> Self {
        PumpTiming { stream_label, idle_threshold, ping_every }
    }
}

/// Drive ONE WS session against `stream` on the SHARED driver session
/// ([`run_market_session`], dedup A6): subscribe (replayed verbatim by every caller session), then
/// read frames until `stop` fires (clean `Ok(())`), the stream errs (`Err(reason)` — the caller
/// reconnects), the idle watchdog trips (the driver's attributable `"no frames within {N}s
/// (silent stall)"`), or the keepalive/subscribe send fails. The client `"PING"` goes out every
/// `wd.ping_every`; the per-frame protocol is [`on_frame`]; §B data freshness rides the driver's
/// transport-alive tick hook ([`on_alive_tick`]).
///
/// This is the test/replay seam: production ([`feed_main`]) runs the SAME
/// closures under the driver's reconnect loop; `tests/offline/market_feed_scripted.rs` calls this
/// directly with a scripted/replayed [`MarketStream`] (vike-backfill's `poly_reparse` did too,
/// until docs/decisions/0094 deleted it on 2026-09-28).
pub fn run_session<S: MarketStream>(
    stream: &mut S,
    sink: &dyn LiveDataSink,
    token_id: &str,
    mode: PumpMode,
    state: &mut TokenState,
    stop: &AtomicBool,
    wd: &mut Watchdog<'_>,
    wake: impl Fn(),
) -> Result<(), String> {
    // The single-token seam is a ONE-SEAT shard, so there is exactly one session body (no
    // single-vs-batched drift): take the caller's `state`/`health` into a temporary slot, run, put
    // them back. Placeholders (`TokenState::new(0.0)`/`StreamHealth::new(0)`) are never observed —
    // the slot is handed straight to the session and moved back out before returning. Only a PANIC
    // inside the session would leave the placeholders behind, and that unwinds the whole feed
    // thread anyway.
    let timing = wd.timing;
    let mut slot = TokenSlot {
        token_id: token_id.to_string(),
        state: std::mem::replace(state, TokenState::new(0.0)),
        health: std::mem::replace(wd.health, StreamHealth::new(0)),
    };
    let res =
        run_shard_session(stream, sink, mode, std::slice::from_mut(&mut slot), stop, &timing, wake);
    *state = slot.state;
    *wd.health = slot.health;
    res
}
