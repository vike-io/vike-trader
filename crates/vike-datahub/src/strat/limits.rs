//! The strategy lane's POLICY numbers: what one lane session and the lane as a whole may cost this
//! process. Feature-free, like `crate::md`'s constants, and shaped the same way: a named constant per
//! number, a `const _` assertion for every relation a ruling leans on, and a [`StratLimits`] value a
//! composition root hands the hub so a test can run the same code with shorter numbers.
//!
//! **START VALUES, not measurements.** They are `wire_design.md` D.4's re-derivations (the tradehub is
//! one client, a reconnect overlap is one more, and the plane is separate from the market-data wire's),
//! to be replaced by the wave's hop measurements. A lapse here is a `Disconnected` for the consumer's
//! dead-man, so [`SL_LAPSE_BUDGET`] and [`SL_WRITE_TIMEOUT`] are trading-safety numbers and not link
//! judgements; their owner is the operator.
//!
//! Three kinds of number live elsewhere and are only READ here. The protocol's facts (the frame
//! ceilings, the symbol guard, the heartbeat range) are `vike_datahub_client::proto`'s `SL_*`, stated
//! once in the lower crate because the lane's client reads the same numbers. The connection cap is
//! [`crate::server::MAX_CONNECTIONS`]. The dead-man's grace, which two numbers below are held under, is
//! `vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS`.

use std::time::Duration;

use vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS;
use vike_datahub_client::proto::{
    SL_BOOK_LEVELS_CEILING, SL_FRAME_CEILING_BYTES, SL_HEARTBEAT, SL_HEARTBEAT_MAX,
    SL_WORST_LEVEL_JSON_BYTES,
};

/// Strategy-lane connections open at once. **A tradehub dials ONE connection per feed**: one per venue
/// (and Hyperliquid's testnet beside its mainnet), and one per Polymarket TOKEN (each token has its own
/// sink), so the cap must cover the whole roster and not "one tradehub plus a spare":
///
/// * the 14-venue roster, one dial each: 14 (a venue with no feed arm costs nothing, but the cap is the
///   roster's, so a feed arm added later needs no edit here);
/// * the Hyperliquid testnet beside the mainnet: +1;
/// * Polymarket tokens beyond the first, up to ten more: +10. That is 25 in steady state;
/// * a reconnect overlap (the old tradehub's sockets may still be closing while the new ones dial; a
///   closed socket is noticed at once, so the overlap is a fraction of a full set): half again, 12.
///
/// 25 + 12 = 37, rounded to 40. It stays a SUBSET of the server's connection cap (an assertion below), and
/// its memory is `SL_PLANE_MEMORY_CEILING_BYTES`'s: 40 queues at [`SL_QUEUE_BYTES`] is 320 MiB. Four, the
/// first value, refused the fifth venue a tradehub opened.
pub const SL_MAX_STREAM_CONNS: usize = 40;

/// Subscriptions (and so distinct symbols) ONE session may hold: at least twice the largest mount set
/// the tradehub arms (`FEED_ARMS` times mounts). A refusal past it is a `Budget`.
pub const SL_MAX_SPECS_PER_SESSION: usize = 128;

/// Distinct `(venue, symbol)` keys the whole lane may hold, across every session. **These do NOT draw
/// on `crate::md::MD_MAX_KEYS_PER_VENUE`**: a maker must not be refused because GUI windows used the
/// venue's market-data cap. The venue is protected by the gatewayhub's budget, not by this number.
pub const SL_MAX_KEYS_TOTAL: usize = 128;

/// Distinct keys on ONE venue.
pub const SL_MAX_KEYS_PER_VENUE: usize = 64;

/// The byte bound of ONE session's outgoing queue: at least eight seconds of the measured worst burst
/// (about a megabyte a second). Past it the session evicts its own oldest lossless frame of the pushing
/// key and owes a `Gap`; it never blocks a venue pump.
pub const SL_QUEUE_BYTES: usize = 8 * 1024 * 1024;

/// The frame bound of one session's data queue, the second of the two bounds.
pub const SL_QUEUE_FRAMES: usize = 65_536;

/// The reserved CONTROL lane of one session: twice the subscriptions it may hold, so every key can have
/// its `SubOk`, its status and its delay pending at once. Never evicted; when even it cannot be filled
/// the session is closed with `ByeReason::ControlLaneOverflow`.
pub const SL_CTRL_FRAMES: usize = 2 * SL_MAX_SPECS_PER_SESSION;

/// The widest a CONTROL frame can be, in bytes: a `Status` or `Delay` naming a bar interval at the
/// symbol guard, under the widest `StreamStatus`. Pinned by `tests::the_widest_control_frame_fits_its_ceiling`
/// against real frames, so the memory claim below prices the control lane at a number that is measured.
pub const SL_CTRL_FRAME_CEILING_BYTES: usize = 1024;

/// How many symbols a session binds WITHOUT a subscription (a feed emitting a spelling it was not asked
/// for, announced by `StreamDown::Key`) before the rest are dropped and counted.
pub const SL_MAX_UNSOLICITED: usize = 16;

/// How long a write to the tradehub may block before the connection is closed: a peer that stopped
/// reading holds a session for nobody, and a vanished tradehub must be noticed inside the dead-man's
/// grace. Held below HALF the grace by an assertion below.
pub const SL_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the REST of a client frame may take once its first byte arrived.
pub const SL_FRAME_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Overflow episodes ([`SL_LAPSE_EPISODE`]) one session may be written inside [`SL_LAPSE_WINDOW`] before it is ended with
/// `ByeReason::TooSlow` (md's `MD_LAPSE_BUDGET` value; the tradehub's client reconnects and replays).
pub const SL_LAPSE_BUDGET: u64 = 8;

/// The window [`SL_LAPSE_BUDGET`] is counted in.
pub const SL_LAPSE_WINDOW: Duration = Duration::from_secs(60);

/// The span, from the FIRST `Gap` of an overflow, in which the other `Gap`s of the same overflow belong to
/// one lapse (the tube writer's episode, `vike_market_stream::writer`'s module doc): one overflow holes
/// every busy `(key, verb)` at once, and counting each `Gap` would end a session on a single event. The
/// gatewayhub's own value, for the same reason.
pub const SL_LAPSE_EPISODE: Duration = Duration::from_secs(1);

/// How long a session may hold no subscription before it is ended with `ByeReason::SessionIdle`.
pub const SL_SESSION_IDLE: Duration = Duration::from_secs(600);

/// What the hub's own bookkeeping costs per key it holds: the route entries, the key-book entry, a
/// session's per-symbol state. A generous round number, asserted against below.
pub const SL_HUB_BYTES_PER_KEY: usize = 4096;

/// The most memory the lane may hold in total - every session's queue at its bound plus the hub's state
/// for every key. **A claim separate from the market-data plane's 32 MiB** (`crate::md`), so a plane of
/// slow strategy readers cannot be argued into the md plane's budget, nor the reverse.
///
/// The worst case is a tradehub whose reader stalls on every dial at once:
/// `SL_MAX_STREAM_CONNS` (40) x [`SL_QUEUE_BYTES`] (8 MiB) = 320 MiB of queues, plus the hub's state
/// ([`SL_HUB_STATE_BYTES`], about 11 MiB), 331 MiB, under this 512 MiB. Against the datahub unit's
/// `MemoryMax=8G` (`deploy/vike-datahub.service`) it is 6 % of the cap, and the md plane's 32 MiB is
/// separate. It was 64 MiB when the lane had four connections.
pub const SL_PLANE_MEMORY_CEILING_BYTES: usize = 512 * 1024 * 1024;

/// The lane's hub state, computed and not quoted: every key the lane may hold at [`SL_HUB_BYTES_PER_KEY`],
/// plus every session's control lane at its ceiling.
pub const SL_HUB_STATE_BYTES: usize = SL_MAX_KEYS_TOTAL * SL_HUB_BYTES_PER_KEY
    + SL_MAX_STREAM_CONNS * SL_CTRL_FRAMES * SL_CTRL_FRAME_CEILING_BYTES;

const _: () = assert!(
    SL_MAX_STREAM_CONNS * SL_QUEUE_BYTES + SL_HUB_STATE_BYTES <= SL_PLANE_MEMORY_CEILING_BYTES,
    "every strategy-lane session at its byte bound plus the hub's state must fit the lane's own \
     memory ceiling - separate from the market-data plane's claim, so raising a number here cannot \
     quietly spend that one"
);
const _: () = assert!(
    SL_QUEUE_BYTES >= 8 * SL_FRAME_CEILING_BYTES as usize,
    "a session's queue must be several frames deep: a bound near one frame evicts on every book"
);
const _: () = assert!(
    SL_MAX_KEYS_PER_VENUE > 0
        && SL_MAX_KEYS_PER_VENUE <= SL_MAX_KEYS_TOTAL
        && SL_MAX_KEYS_TOTAL <= SL_MAX_STREAM_CONNS * SL_MAX_SPECS_PER_SESSION,
    "the per-venue cap is positive and inside the total, and the total is no more than the sessions \
     could hold between them"
);
const _: () = assert!(
    SL_CTRL_FRAMES >= SL_MAX_SPECS_PER_SESSION,
    "the control lane must hold at least one pending frame per subscription"
);
const _: () = assert!(
    SL_MAX_STREAM_CONNS > 0 && SL_MAX_STREAM_CONNS < crate::server::MAX_CONNECTIONS,
    "the lane's connection cap is a SUBSET of the server's, so a box that filled it still has \
     connections for the ordinary verbs"
);
const _: () = assert!(
    (SL_WRITE_TIMEOUT.as_millis() as u64) * 2 < DEFAULT_LINK_DEADMAN_GRACE_MS,
    "the write timeout must stay under HALF the dead-man's grace: a vanished tradehub is noticed \
     long before the grace it would otherwise be judged against"
);
const _: () = assert!(
    (3 * SL_HEARTBEAT_MAX.as_millis() as u64) < DEFAULT_LINK_DEADMAN_GRACE_MS
        && SL_HEARTBEAT.as_millis() as u64 * 3 < DEFAULT_LINK_DEADMAN_GRACE_MS,
    "the client's deadline is three heartbeats and must fire well inside the dead-man's grace, so a \
     dead lane is known before the dead-man acts"
);
const _: () = assert!(
    SL_LAPSE_EPISODE.as_millis() > 0 && SL_LAPSE_EPISODE.as_millis() < SL_LAPSE_WINDOW.as_millis(),
    "an episode is a burst inside the budget window, not the window"
);
const _: () = assert!(
    SL_LAPSE_BUDGET > 0 && SL_SESSION_IDLE.as_millis() > SL_LAPSE_WINDOW.as_millis(),
    "a zero lapse budget would end a session on its first gap, and an idle bound inside the lapse \
     window would end a session that is merely quiet"
);
const _: () = assert!(
    2 * SL_BOOK_LEVELS_CEILING * SL_WORST_LEVEL_JSON_BYTES < SL_FRAME_CEILING_BYTES as usize,
    "a full book at the widest level must fit the frame ceiling"
);

/// The numbers one hub runs with. [`StratLimits::PRODUCTION`] is every constant above; a test hands
/// shorter ones to watch a timeout or a cap fire in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StratLimits {
    /// See [`SL_MAX_STREAM_CONNS`].
    pub max_stream_conns: usize,
    /// See [`SL_MAX_SPECS_PER_SESSION`].
    pub max_specs_per_session: usize,
    /// See [`SL_MAX_KEYS_TOTAL`].
    pub max_keys_total: usize,
    /// See [`SL_MAX_KEYS_PER_VENUE`].
    pub max_keys_per_venue: usize,
    /// See [`SL_QUEUE_BYTES`].
    pub queue_bytes: usize,
    /// See [`SL_QUEUE_FRAMES`].
    pub queue_frames: usize,
    /// See [`SL_CTRL_FRAMES`].
    pub ctrl_frames: usize,
    /// See [`SL_MAX_UNSOLICITED`].
    pub max_unsolicited: usize,
    /// See [`SL_WRITE_TIMEOUT`].
    pub write_timeout: Duration,
    /// See [`SL_FRAME_READ_TIMEOUT`].
    pub frame_read_timeout: Duration,
    /// See [`SL_LAPSE_BUDGET`].
    pub lapse_budget: u64,
    /// See [`SL_LAPSE_WINDOW`].
    pub lapse_window: Duration,
    /// See [`SL_LAPSE_EPISODE`].
    pub lapse_episode: Duration,
    /// See [`SL_SESSION_IDLE`].
    pub session_idle: Duration,
    /// The heartbeat period stated in `StratOpened`; the client's read deadline is three of them.
    /// Production is `vike_datahub_client::proto::SL_HEARTBEAT`.
    pub heartbeat: Duration,
}

impl StratLimits {
    /// What a daemon runs.
    pub const PRODUCTION: StratLimits = StratLimits {
        max_stream_conns: SL_MAX_STREAM_CONNS,
        max_specs_per_session: SL_MAX_SPECS_PER_SESSION,
        max_keys_total: SL_MAX_KEYS_TOTAL,
        max_keys_per_venue: SL_MAX_KEYS_PER_VENUE,
        queue_bytes: SL_QUEUE_BYTES,
        queue_frames: SL_QUEUE_FRAMES,
        ctrl_frames: SL_CTRL_FRAMES,
        max_unsolicited: SL_MAX_UNSOLICITED,
        write_timeout: SL_WRITE_TIMEOUT,
        frame_read_timeout: SL_FRAME_READ_TIMEOUT,
        lapse_budget: SL_LAPSE_BUDGET,
        lapse_window: SL_LAPSE_WINDOW,
        lapse_episode: SL_LAPSE_EPISODE,
        session_idle: SL_SESSION_IDLE,
        heartbeat: SL_HEARTBEAT,
    };
}

// ---- the refusal-timing chain, tradehub > datahub > gatewayhub -----------------------------------------------

use vike_datahub_client::strat::STRAT_SUB_REPLY;
use vike_gatewayhub_client::proto::{
    GW_CREDENTIALED_FIRST_DIAL_WAIT, GW_CREDENTIALED_OPEN_BUDGET, GW_SUB_REPLY_TIMEOUT,
    LOOKUP_REPLY_TIMEOUT,
};
use vike_market_stream::market::FIRST_CONNECT_WAIT;

// A subscribe the tradehub sends waits for the datahub's ack; the datahub answers it once ITS subscribe at
// the gatewayhub returned, which waits for a dial in flight (once) and then the gatewayhub's ack. If the
// tradehub's wait is not the longest, it gives up at about the moment the datahub does and the operator
// reads a bare timeout in place of the refusal's words; the subscription may complete a moment later for a
// process that is exiting.
const _: () = assert!(
    STRAT_SUB_REPLY.as_millis() > FIRST_CONNECT_WAIT.as_millis() + GW_SUB_REPLY_TIMEOUT.as_millis(),
    "the tradehub's wait for a subscribe's ack must outlast the datahub's own wait for the gatewayhub \
     (the first-dial wait plus the gatewayhub's ack)"
);
// …and for a CREDENTIALED stream, whose first dial includes the venue handshake (ctrader's runs up to 12 s inside
// the open): the datahub's per-opener first-dial wait (13 s) plus the gatewayhub's ack (10 s) is 23 s, still under
// the tradehub's 25 s.
const _: () = assert!(
    STRAT_SUB_REPLY.as_millis()
        > GW_CREDENTIALED_FIRST_DIAL_WAIT.as_millis() + GW_SUB_REPLY_TIMEOUT.as_millis(),
    "the tradehub's wait for a subscribe's ack must outlast the datahub's own wait for a CREDENTIALED \
     stream (its handshake-long first-dial wait plus the gatewayhub's ack)"
);
// …and the start history of a bar key (flag-gated, `history.rs`) is read BEFORE the venue is asked, on the same
// thread, so its per-key budget is spent inside the same tradehub wait: the longest first dial, the gatewayhub's
// ack and the budget together stay under it.
const _: () = assert!(
    STRAT_SUB_REPLY.as_millis()
        > GW_CREDENTIALED_FIRST_DIAL_WAIT.as_millis()
            + GW_SUB_REPLY_TIMEOUT.as_millis()
            + super::history::PRELOAD_KEY_BUDGET.as_millis(),
    "the tradehub's wait for a subscribe's ack must outlast a credentialed first dial, the gatewayhub's ack \
     and the start history's per-key budget together"
);
// …and one session's whole start-history budget is its own number, never under a key's.
const _: () = assert!(
    super::history::PRELOAD_SESSION_BUDGET.as_millis()
        >= super::history::PRELOAD_KEY_BUDGET.as_millis(),
    "a session's start-history budget must hold at least one key's"
);
// …and the datahub's client waits for a credentialed stream's open (a synchronous handshake inside it) longer
// than the gatewayhub lets that handshake run, so the gatewayhub's words are what arrive.
const _: () = assert!(
    GW_CREDENTIALED_OPEN_BUDGET.as_millis() < LOOKUP_REPLY_TIMEOUT.as_millis(),
    "the gatewayhub's handshake budget must sit under the open-reply wait of the client that asked"
);

#[cfg(test)]
mod tests {

    use vike_market_stream::wire::{StreamDown, WireStream};
    use vike_marketdata::{BookLevel, L2Book};
    use vike_model::StreamStatus;

    use super::*;

    /// **The frame ceiling holds for a REAL full book at the widest levels the type can produce**, and
    /// the per-level figure `SL_WORST_LEVEL_JSON_BYTES` is no smaller than what serde writes for one
    /// (a stringified tick key and a float at its longest spelling). The compile-time assertion
    /// above trusts the figure; this measures it.
    #[test]
    fn a_full_thousand_level_book_at_the_widest_levels_fits_the_frame_ceiling() {
        // Keys of 16 digits (prices near 1e15 on a unit grid) and quantities at their longest ryu
        // spelling: the widest stringified-key form a book of this ceiling can take.
        let wide_qty = 1.234_567_890_123_456_7e-7;
        let levels = |base: f64| -> Vec<BookLevel> {
            (0..SL_BOOK_LEVELS_CEILING).map(|i| BookLevel::new(base + i as f64, wide_qty)).collect()
        };
        let mut book = L2Book::new(1.0);
        book.apply_snapshot(u64::MAX, &levels(1.0e15), &levels(2.0e15));
        let frame = StreamDown::Book { k: u32::MAX, seq: u64::MAX, book: Box::new(book) };
        let bytes = serde_json::to_vec(&frame).expect("a book encodes");
        assert!(
            bytes.len() < SL_FRAME_CEILING_BYTES as usize,
            "a full widest book is {} bytes, over the {} ceiling",
            bytes.len(),
            SL_FRAME_CEILING_BYTES
        );
        let per_level = bytes.len() / (2 * SL_BOOK_LEVELS_CEILING);
        assert!(
            per_level <= SL_WORST_LEVEL_JSON_BYTES,
            "a level costs {per_level} bytes, over the {SL_WORST_LEVEL_JSON_BYTES} the ceiling assumes"
        );
        // ...and the raw-level form (`[price,qty]`) is no wider.
        let level = serde_json::to_vec(&BookLevel::new(-1.234_567_890_123_456_7e-300, 1.0e300))
            .expect("a level encodes");
        assert!(
            level.len() <= SL_WORST_LEVEL_JSON_BYTES + 6,
            "a raw level at the extremes is {} bytes",
            level.len()
        );
    }

    /// **The widest control frame fits the ceiling the memory claim prices it at**: a `Status` naming a
    /// bar interval at the symbol guard, under the widest `StreamStatus`.
    #[test]
    fn the_widest_control_frame_fits_its_ceiling() {
        let interval = "x".repeat(vike_datahub_client::proto::SL_MAX_SYMBOL_BYTES);
        for status in [
            StreamStatus::Live { gap_started_ts_ms: Some(i64::MIN) },
            StreamStatus::GapStart { at_ts_ms: i64::MIN },
            StreamStatus::Stale { newest_data_ts_ms: i64::MIN, now_ms: i64::MIN },
        ] {
            let frame = StreamDown::Status {
                k: u32::MAX,
                stream: WireStream::Bar(interval.clone()),
                status,
            };
            let len = serde_json::to_vec(&frame).expect("encodes").len();
            assert!(len <= SL_CTRL_FRAME_CEILING_BYTES, "{status:?}: {len} bytes");
        }
    }

    #[test]
    fn production_reads_the_constants() {
        let p = StratLimits::PRODUCTION;
        assert_eq!(p.max_stream_conns, SL_MAX_STREAM_CONNS);
        assert_eq!(p.queue_bytes, SL_QUEUE_BYTES);
        assert_eq!(p.heartbeat, SL_HEARTBEAT);
        assert_eq!(p.lapse_budget, SL_LAPSE_BUDGET);
    }
}
