//! The seam between the receiving engine and the protocol that hosts it: an [`Opener`] dials a
//! `stream` socket and hands back the session after its open reply, so the engine
//! (`crate::market::RemoteDataClient`) never knows what handshake, keys or address stand behind it.
//! The dial and the handshake stay with each client crate (the gatewayhub's is
//! `crates/vike-gatewayhub-client/src/opener.rs`'s `GatewayOpener`); what the engine needs to frame,
//! wait and back off is passed as plain numbers ([`StreamLimits`], [`StreamTimings`]).
//!
//! The two pure timing functions every reconnecting reader of a hub protocol shares live here too:
//! [`stream_deadline`] and [`reconnect_delay`].

use std::io;
use std::net::TcpStream;
use std::time::Duration;

/// The frame ceilings and the symbol guard a stream session frames under. The hosting protocol states
/// them (the gatewayhub's are `GW_UP_FRAME_CEILING_BYTES`, `GW_DOWN_FRAME_CEILING_BYTES` and
/// `GW_MAX_SYMBOL_BYTES`); the engine only enforces them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
    /// The longest frame the subscriber sends: a larger one is refused locally, before its first byte.
    pub up_frame_ceiling_bytes: u32,
    /// The longest frame the sender writes: the subscriber reads under it, so a longer one is a framing
    /// fault.
    pub down_frame_ceiling_bytes: u32,
    /// The longest symbol or interval the subscriber will frame, in bytes. A FRAMING guard, not a
    /// validity rule: whether a spelling is a real instrument is the venue's and the daemon's.
    pub max_symbol_bytes: usize,
}

/// Every wait the engine makes, in one value the opener can narrow in a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamTimings {
    /// How long a `subscribe_*` waits for its `SubOk`/`SubErr`.
    pub sub_reply: Duration,
    /// The floor of a stream's heartbeat deadline ([`stream_deadline`]).
    pub stream_read_floor: Duration,
    /// The first reconnect delay.
    pub reconnect_base: Duration,
    /// The longest reconnect delay.
    pub reconnect_max: Duration,
}

/// An opened `stream` session: the socket after the open reply, now speaking
/// [`crate::wire::StreamUp`] / [`crate::wire::StreamDown`] only.
#[derive(Debug)]
pub struct DataStream {
    /// The socket, read and write timeouts armed for stream use.
    pub stream: TcpStream,
    /// The daemon's name for this session.
    pub session: u64,
    /// The daemon's epoch.
    pub epoch: u64,
    /// The stated heartbeat period, clamped into the protocol's range.
    pub heartbeat: Duration,
    /// Whether the peer serves the delivery class of a subscription (a `class` on `StreamUp::Sub`, the
    /// `StreamUp::Delivery` frame and its `StreamDown::Delivery` marker), as the hosting protocol
    /// negotiates it (the gatewayhub's `stream_class` feature string, decision 0112). `false` means
    /// the peer ignores a `class` it is sent, so the engine sends none and says so.
    pub delivery_class: bool,
}

/// What the engine needs of the protocol that hosts it: where the peer is, what to frame and wait
/// under, how to spell a transport failure, and how to open one session.
///
/// `open` is called on the engine's reader thread, never on a caller's: it may block for the dial and
/// the handshake. It must hand `observe` the freshly dialled socket BEFORE the handshake reads from it,
/// so a shutdown can break a handshake parked on a peer that accepted and went quiet.
pub trait Opener: Send + Sync + 'static {
    /// The peer's address as given, for the engine's messages.
    fn addr(&self) -> &str;

    /// The frame ceilings and the symbol guard of this protocol.
    fn limits(&self) -> StreamLimits;

    /// The waits of this link. Read afresh by the engine, so an opener may change them.
    fn timings(&self) -> StreamTimings;

    /// How long the FIRST `subscribe_*` of a client for `venue` waits for the dial already in flight, when this
    /// opener's dial is allowed to take longer than the engine's default ([`crate::market::FIRST_CONNECT_WAIT`], 2 s,
    /// narrowed to the ack wait). `None` (the default) is that default. A dial whose OPEN includes a handshake
    /// with a venue (the gatewayhub's credentialed streams: ctrader's runs up to 12 s inside the open) states its
    /// own figure here, so a slow but healthy first dial is waited for instead of read as a transient `connecting`
    /// and the caller's start refused. The figure is a first-dial wait only: the ack that follows keeps
    /// [`StreamTimings::sub_reply`].
    fn first_dial_wait(&self, venue: &str) -> Option<Duration> {
        let _ = venue;
        None
    }

    /// An error as the one sentence this protocol spells a transport failure with (the gatewayhub's
    /// `gatewayhub <addr>: ...`), so an operator greps one word.
    fn failure_text(&self, e: &dyn std::fmt::Display) -> String;

    /// Dial, handshake and open one `stream` session for `venue`.
    fn open(&self, venue: &str, observe: &dyn Fn(&TcpStream)) -> io::Result<DataStream>;
}

/// The deadline a stream reader arms against the next frame: `max(floor, 3 × heartbeat)`.
pub fn stream_deadline(floor: Duration, heartbeat: Duration) -> Duration {
    floor.max(heartbeat.saturating_mul(3))
}

/// The reconnect delay after `attempt` consecutive failures (0 = the first): doubling from `base`,
/// capped at `max`.
pub fn reconnect_delay(base: Duration, max: Duration, attempt: u32) -> Duration {
    base.saturating_mul(1u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX)).min(max)
}

#[cfg(test)]
#[path = "opener_tests.rs"]
mod opener_tests;
