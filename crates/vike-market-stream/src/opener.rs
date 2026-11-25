//! The seam between the receiving engine and the protocol that hosts it: a [`StreamOpener`] dials a
//! `stream` socket and hands back the session after its open reply, so the engine
//! (`crate::market::DataClientStream`) never knows what handshake, keys or address stand behind it.
//! The dial and the handshake stay with each client crate (the gatewayhub's is
//! `crates/vike-gatewayhub-client/src/opener.rs`'s `StreamOpenerGateway`); what the engine needs to frame,
//! wait and back off is passed as plain numbers ([`StreamLimits`], [`StreamTimings`]), and the two
//! reconnect delays every hosting client defaults to are declared once ([`RECONNECT_BASE`],
//! [`RECONNECT_MAX`]).
//!
//! The pure timing function every reconnecting reader of a hub protocol shares lives here too, [`stream_deadline`];
//! the wait before a redial is the one ladder of every reconnect loop, `vike_venues::reconnect`'s `ReconnectBackoff`,
//! from [`StreamTimings::reconnect_base`] doubling to [`StreamTimings::reconnect_max`].

use std::io;
use std::net::TcpStream;
use std::time::Duration;

/// The frame ceilings and the symbol guard a stream session frames under. The hosting protocol states
/// them (the gatewayhub's are `GATEWAY_UP_FRAME_CEILING_BYTES`, `GATEWAY_DOWN_FRAME_CEILING_BYTES` and
/// `vike_marketdata::Symbol::MAX_READ_LEN`); the engine only enforces them.
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

/// The first reconnect delay of a long-lived hub socket (a stream, a feed, a lease): the
/// [`StreamTimings::reconnect_base`] every hosting client's default carries.
pub const RECONNECT_BASE: Duration = Duration::from_millis(250);

/// The longest reconnect delay: a flapping daemon is retried at this cadence for ever. The
/// [`StreamTimings::reconnect_max`] every hosting client's default carries.
pub const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// Every wait the engine makes, in one value the opener can narrow in a test. Each hosting client
/// embeds one in its own timings (beside the waits only that protocol makes), so the engine's four
/// are spelled once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamTimings {
    /// How long a `subscribe_*` waits for its `SubscribeOk`/`SubscribeError`.
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
pub struct StreamConnection {
    /// The socket, read and write timeouts armed for stream use.
    pub stream: TcpStream,
    /// The daemon's name for this session.
    pub session: u64,
    /// The daemon's epoch.
    pub epoch: u64,
    /// The stated heartbeat period, clamped into the protocol's range.
    pub heartbeat: Duration,
    /// Whether the peer serves the delivery class of a subscription (a `class` on `StreamUp::Subscribe`, the
    /// `StreamUp::Delivery` frame and its `StreamDown::Delivery` marker), as the hosting protocol
    /// negotiates it (the gatewayhub's `stream_class` feature string, decision 0112). `false` means
    /// the peer ignores a `class` it is sent, so the engine sends none and says so.
    pub delivery_class: bool,
    /// Whether the peer serves the venue's funding frames, as the hosting protocol negotiates it (the
    /// gatewayhub's `stream_funding` and the datahub's `strategy_funding` feature strings, decision 0112).
    /// `true` makes the engine ask for them (`StreamUp::Funding`) at the start of every session; `false`
    /// asks nothing, so a peer that predates the frame never sees the request.
    pub funding: bool,
    /// The form the engine writes its frames up in (`vike_hub_protocol::frame::BodyCodec`), as the hosting protocol
    /// negotiated it (the datahub's `wire_msgpack` feature string); a frame down is read in any form.
    pub codec: vike_hub_protocol::frame::BodyCodec,
    /// Whether the peer writes the per-tick frames as BINARY bodies on request, as the hosting protocol
    /// negotiates it (the datahub's `stream_binary_ticks`; the gatewayhub advertises none). `true` makes the
    /// engine ask (`StreamUp::BinaryTicks`) at the start of every session; `false` asks nothing and the
    /// session stays JSON.
    pub binary_ticks: bool,
}

/// What the engine needs of the protocol that hosts it: where the peer is, what to frame and wait
/// under, how to spell a transport failure, and how to open one session.
///
/// `open` is called on the engine's reader thread, never on a caller's: it may block for the dial and
/// the handshake. It must hand `observe` the freshly dialled socket BEFORE the handshake reads from it,
/// so a shutdown can break a handshake parked on a peer that accepted and went quiet.
pub trait StreamOpener: Send + Sync + 'static {
    /// The peer's address as given, for the engine's messages.
    fn addr(&self) -> &str;

    /// The daemon this opener dials, as every message of the engine names it: `gatewayhub` (the
    /// gatewayhub's `StreamOpenerGateway`) or `datahub` (the strategy lane's `StreamOpenerStrategyLane`). Required, with
    /// no default: a default word is how the strategy lane's refusals came to say "gatewayhub" to a
    /// tradehub whose peer is the datahub, and a wrapper that forwards the other methods would lose it.
    fn peer(&self) -> &'static str;

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
    /// `gatewayhub <addr>: ...`), so an operator greps one word. The `io::Error` itself, never its text: a
    /// failed [`open`](Self::open) carries the client's typed failure inside it, which an opener decides on.
    fn failure_text(&self, e: &io::Error) -> String;

    /// Dial, handshake and open one `stream` session for `venue`.
    fn open(&self, venue: &str, observe: &dyn Fn(&TcpStream)) -> io::Result<StreamConnection>;
}

/// **The daemon refused the stream SESSION because a cap is full** (it already serves the most sessions it is built
/// to): what an opener returns, inside the `io::Error` of [`StreamOpener::open`], when the open reply is its hub's
/// cap refusal (the refusal frame's cap id). The engine then answers every `subscribe_*` with
/// `vike_data::DataClientError::Cap` while the link stays down for it, never with a `NotNow` a caller would restart
/// on. The text is the daemon's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOpenCapped(pub String);

impl std::fmt::Display for StreamOpenCapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StreamOpenCapped {}

/// Whether a failed [`StreamOpener::open`] was the daemon's CAP: a [`StreamOpenCapped`] (decided on its type), or the
/// words a daemon that predates the refusal frame wrote (`crate::legacy`).
pub(crate) fn open_was_capped(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<StreamOpenCapped>())
        || crate::legacy::older_open_was_capped(&e.to_string())
}

/// The deadline a stream reader arms against the next frame: `max(floor, 3 × heartbeat)`.
pub fn stream_deadline(floor: Duration, heartbeat: Duration) -> Duration {
    floor.max(heartbeat.saturating_mul(3))
}

#[cfg(test)]
#[path = "opener_tests.rs"]
mod opener_tests;
