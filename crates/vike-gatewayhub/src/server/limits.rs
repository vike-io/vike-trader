//! The server's POLICY numbers: what one connection and one session may cost the daemon.
//!
//! Three kinds of number live elsewhere and are only READ here. The protocol's facts (the frame
//! ceilings, the heartbeat range, the progress cadence, `GW_MAX_CONNECTIONS` and its siblings, which
//! the client crate states once because the status report and the tests name them) are
//! `vike_gatewayhub_client::proto`'s. The node substrate's (the pre-auth frame cap, the handshake
//! deadline, the idle read timeout) are `vike_hub_proto::limits`'s. What is declared HERE is what only
//! a server decides: how deep a session's queue is, how long a write may block, when a laggard is
//! cut off.
//!
//! Every value is a field of [`ServerConfig`] with a `PRODUCTION` constant, so a composition root
//! passes `ServerConfig::PRODUCTION` (or, once a row exists, a value folded from one) and a test
//! passes shorter ones to watch a timeout fire in milliseconds. ⚠ The values are floors in
//! production's mouth: a progress cadence or a heartbeat shorter than the peer's own bound is a
//! client's problem to tune, never a reason to change the shipped constant.

use std::time::Duration;

use vike_gatewayhub_client::proto::{
    FETCH_PAGE_CEILING_BYTES, FETCH_PROGRESS_EVERY, GW_DOWN_FRAME_CEILING_BYTES,
    GW_MAX_CONNECTIONS, GW_MAX_STREAM_SESSIONS, GW_MAX_SUBS_PER_SESSION, GW_STREAM_HEARTBEAT,
};
use vike_hub_proto::limits::{SERVER_HANDSHAKE_DEADLINE, SERVER_IDLE_READ_TIMEOUT};

/// The byte bound of ONE stream session's outgoing queue: 4 MiB of in-memory frame cost
/// (`vike_market_stream::queue::Payload::cost`), about four seconds of the measured worst polymarket burst
/// (`docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` section 12). Past it the
/// session evicts its own oldest lossless frames and owes a `Gap`, never blocking a venue pump.
/// ⚠ A single frame costing more than this is evicted at once (and disclosed): it is a bound on
/// what a laggard may make this process hold, not a frame size.
pub const WIRE_QUEUE_BYTES: usize = 4 * 1024 * 1024;

/// The frame bound of one session's data queue, the second of the two bounds (bytes first): a flood of
/// tiny frames costs a hash entry each long before it costs the byte bound.
pub const WIRE_QUEUE_FRAMES: usize = 32_768;

/// The reserved CONTROL lane of one session: twice the subscriptions it may hold, so every key can
/// have its `SubOk`, its status and its delay pending at once. It is never evicted; when even it cannot
/// be filled the session is closed with `ByeReason::ControlLaneOverflow` (the datahub mailbox's rule).
pub const WIRE_QUEUE_CTRL_FRAMES: usize = 2 * GW_MAX_SUBS_PER_SESSION;

/// How many `Gap` disclosures one session may be written inside [`LAPSE_WINDOW`] before it is ended
/// with `ByeReason::TooSlow` (`md::MD_LAPSE_BUDGET`'s value, for the same reason: a reader that keeps
/// losing frames is being lied to more slowly, and the datahub's reconnect re-seeds it).
pub const LAPSE_BUDGET: u64 = 8;

/// The window [`LAPSE_BUDGET`] is counted in.
pub const LAPSE_WINDOW: Duration = Duration::from_secs(60);

/// How long a write to a peer may block before the connection is closed: a peer that has stopped
/// reading holds a session (and a venue client) for nobody. Generous enough for an 8 MiB frame over a
/// slow tunnel; nothing below it is a liveness signal, the heartbeat is.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the REST of a frame may take once its first byte has arrived. A peer that sends a length
/// prefix and stalls holds a thread; a timeout mid-frame closes the connection (it is never resumed).
pub const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a stream session may hold no subscription before the daemon ends it with
/// `ByeReason::SessionIdle`: an idle session is a venue client kept for nobody.
pub const SESSION_IDLE: Duration = Duration::from_secs(600);

/// How long `crate::server::accept::serve` waits, after the stop request, for connection threads to
/// end before it gives up on them and returns (a thread blocked in a venue call outlives the wait).
pub const STOP_DRAIN: Duration = Duration::from_secs(5);

/// How many symbols a session binds WITHOUT a subscription (a feed emitting a spelling it was not
/// asked for, announced by `StreamDown::Key`) before the rest are dropped and counted. It is a fence
/// against a feed that invents symbols, not a limit a healthy venue meets.
pub const MAX_UNSOLICITED_KEYS: usize = 64;

/// How many options feed sockets are open at once. The datahub starts two, once each.
pub const MAX_OPTIONS_FEEDS: usize = 8;

/// The frame bound of an options feed's queue. A feed row is a latest-wins snapshot per instrument, so
/// a laggard loses the oldest ones (counted) and is ended with `TooSlow` if it keeps doing so.
pub const FEED_QUEUE_FRAMES: usize = 4_096;

/// The byte bound of an options feed's queue, the second of its two bounds (frames first): a
/// `Marks` frame is a whole venue batch (a chain is hundreds of rows), so a frame count alone lets a
/// parked-but-open consumer hold [`FEED_QUEUE_FRAMES`] batches. Counted like the stream queue counts,
/// by in-memory cost; past it the OLDEST rows go, counted, and a feed that keeps losing rows is ended.
/// Eight feeds at this bound are 32 MiB.
pub const FEED_QUEUE_BYTES: usize = 4 * 1024 * 1024;

/// How many fetch WORKERS (`gwh-fetch` threads) run at once, daemon-wide. The connection cap bounds
/// the threads that ASK; a worker can outlive its connection (a venue call has no probe, so a dropped
/// link's worker finishes the call it is inside), so this counts the workers themselves and a client
/// that starts a long fetch and drops, again and again, meets it. A start value: a fetch is one venue
/// request chain and the datahub runs a few at a time.
pub const MAX_FETCHES: usize = 32;

/// The most memory all stream sessions' queues may hold together: [`GW_MAX_STREAM_SESSIONS`] queues
/// at [`WIRE_QUEUE_BYTES`] must fit, so a plane of laggards is a bounded cost.
pub const STREAM_PLANE_MEMORY_CEILING_BYTES: usize = 128 * 1024 * 1024;

const _: () = assert!(
    GW_MAX_STREAM_SESSIONS * WIRE_QUEUE_BYTES <= STREAM_PLANE_MEMORY_CEILING_BYTES,
    "every stream session at its byte bound must fit the plane's memory ceiling"
);
const _: () = assert!(
    WIRE_QUEUE_BYTES <= GW_DOWN_FRAME_CEILING_BYTES as usize * 2,
    "a session's queue bound is a few frames deep, not unbounded: it must stay near the frame ceiling"
);
const _: () = assert!(
    WIRE_QUEUE_CTRL_FRAMES >= GW_MAX_SUBS_PER_SESSION,
    "the control lane must hold at least one pending frame per subscription"
);
const _: () = assert!(LAPSE_BUDGET > 0, "a zero lapse budget would end a session on its first gap");
const _: () = assert!(
    MAX_FETCHES > 0 && MAX_FETCHES <= GW_MAX_CONNECTIONS,
    "no fetch workers would refuse every fetch, and more workers than connections is no bound"
);

/// The timings of one server. See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerTimings {
    /// The whole pre-auth phase (`Hello` to `AuthOk`).
    pub handshake_deadline: Duration,
    /// The longest an authenticated `rpc` connection may go without sending a frame. ⚠ Stream and feed
    /// connections are NOT bounded by it: their client is silent by design, and their liveness is the
    /// heartbeat below in the other direction.
    pub idle_read_timeout: Duration,
    /// See [`WRITE_TIMEOUT`].
    pub write_timeout: Duration,
    /// See [`FRAME_READ_TIMEOUT`].
    pub frame_read_timeout: Duration,
    /// The heartbeat period stated in `DataOpened` / `OptionsFeedOpened`; the client's read deadline
    /// is three of them. Production stays inside `GW_STREAM_HEARTBEAT_MIN..=GW_STREAM_HEARTBEAT_MAX`.
    pub stream_heartbeat: Duration,
    /// The cadence stated in every `FetchProgress.next_in_ms`: the daemon owes a running fetch a frame
    /// at least this often.
    pub fetch_progress_every: Duration,
    /// See [`SESSION_IDLE`].
    pub session_idle: Duration,
    /// See [`STOP_DRAIN`].
    pub stop_drain: Duration,
}

impl ServerTimings {
    /// What a daemon runs.
    pub const PRODUCTION: ServerTimings = ServerTimings {
        handshake_deadline: SERVER_HANDSHAKE_DEADLINE,
        idle_read_timeout: SERVER_IDLE_READ_TIMEOUT,
        write_timeout: WRITE_TIMEOUT,
        frame_read_timeout: FRAME_READ_TIMEOUT,
        stream_heartbeat: GW_STREAM_HEARTBEAT,
        fetch_progress_every: FETCH_PROGRESS_EVERY,
        session_idle: SESSION_IDLE,
        stop_drain: STOP_DRAIN,
    };
}

/// The caps of one server. See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerLimits {
    /// Authenticated-or-not connections (threads) served at once; the excess is dropped without a
    /// reply.
    pub max_connections: usize,
    /// Stream sessions open at once.
    pub max_stream_sessions: usize,
    /// Subscriptions one stream session may hold.
    pub max_subs_per_session: usize,
    /// See [`WIRE_QUEUE_BYTES`].
    pub queue_bytes: usize,
    /// See [`WIRE_QUEUE_FRAMES`].
    pub queue_frames: usize,
    /// See [`WIRE_QUEUE_CTRL_FRAMES`].
    pub queue_ctrl_frames: usize,
    /// See [`LAPSE_BUDGET`].
    pub lapse_budget: u64,
    /// See [`LAPSE_WINDOW`].
    pub lapse_window: Duration,
    /// See [`MAX_UNSOLICITED_KEYS`].
    pub max_unsolicited_keys: usize,
    /// See [`MAX_OPTIONS_FEEDS`].
    pub max_options_feeds: usize,
    /// See [`FEED_QUEUE_FRAMES`].
    pub feed_queue_frames: usize,
    /// See [`FEED_QUEUE_BYTES`].
    pub feed_queue_bytes: usize,
    /// See [`MAX_FETCHES`].
    pub max_fetches: usize,
    /// The payload budget of one `FetchPage`, the target `pages_of` splits to.
    pub fetch_page_ceiling: usize,
}

impl ServerLimits {
    /// What a daemon runs.
    pub const PRODUCTION: ServerLimits = ServerLimits {
        max_connections: GW_MAX_CONNECTIONS,
        max_stream_sessions: GW_MAX_STREAM_SESSIONS,
        max_subs_per_session: GW_MAX_SUBS_PER_SESSION,
        queue_bytes: WIRE_QUEUE_BYTES,
        queue_frames: WIRE_QUEUE_FRAMES,
        queue_ctrl_frames: WIRE_QUEUE_CTRL_FRAMES,
        lapse_budget: LAPSE_BUDGET,
        lapse_window: LAPSE_WINDOW,
        max_unsolicited_keys: MAX_UNSOLICITED_KEYS,
        max_options_feeds: MAX_OPTIONS_FEEDS,
        feed_queue_frames: FEED_QUEUE_FRAMES,
        feed_queue_bytes: FEED_QUEUE_BYTES,
        max_fetches: MAX_FETCHES,
        fetch_page_ceiling: FETCH_PAGE_CEILING_BYTES,
    };
}

/// Everything a server is configured with besides its keys, lanes and listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerConfig {
    /// The timings.
    pub timings: ServerTimings,
    /// The caps.
    pub limits: ServerLimits,
}

impl ServerConfig {
    /// What a daemon runs.
    pub const PRODUCTION: ServerConfig =
        ServerConfig { timings: ServerTimings::PRODUCTION, limits: ServerLimits::PRODUCTION };
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self::PRODUCTION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped numbers are the protocol's own where the protocol states one.
    #[test]
    fn production_reads_the_protocol_constants() {
        let c = ServerConfig::PRODUCTION;
        assert_eq!(c.limits.max_connections, GW_MAX_CONNECTIONS);
        assert_eq!(c.limits.max_stream_sessions, GW_MAX_STREAM_SESSIONS);
        assert_eq!(c.limits.max_subs_per_session, GW_MAX_SUBS_PER_SESSION);
        assert_eq!(c.limits.fetch_page_ceiling, FETCH_PAGE_CEILING_BYTES);
        assert_eq!(c.timings.fetch_progress_every, FETCH_PROGRESS_EVERY);
        assert_eq!(c.timings.stream_heartbeat, GW_STREAM_HEARTBEAT);
        assert_eq!(c.timings.handshake_deadline, SERVER_HANDSHAKE_DEADLINE);
        assert_eq!(c.timings.idle_read_timeout, SERVER_IDLE_READ_TIMEOUT);
        assert_eq!(ServerConfig::default(), ServerConfig::PRODUCTION);
    }

    /// A write bound under the client's own deadline floors would cut a healthy link, and one over a
    /// minute would hold a session for a dead peer: the shipped value sits between.
    #[test]
    fn the_write_timeout_is_a_sane_bound() {
        assert!(
            WRITE_TIMEOUT >= Duration::from_secs(5) && WRITE_TIMEOUT <= Duration::from_secs(60)
        );
        assert!(FRAME_READ_TIMEOUT >= Duration::from_secs(1));
        assert!(SESSION_IDLE > LAPSE_WINDOW);
    }
}
