//! The blocking gatewayhub client: a thin wrapper over one [`TcpStream`] that speaks the
//! [`crate::proto`] protocol — the dial, the handshake, `exchange` for the positional verbs, `fetch`
//! for the progress-framed family, and the two socket-class openers the remote seams build on.
//!
//! One client owns one connection and issues requests sequentially; it is not internally
//! synchronized. **The positional seams DIAL PER CALL** (a describe, a presence word, an options
//! REST call): a loopback dial plus one HMAC is microseconds, a fresh connection can never be stale
//! or half-open, and a dropped connection is the cancellation the daemon already understands (a link
//! drop cancels every fetch that connection started). Three seams deliberately HOLD a connection:
//! a history SOURCE (`RemoteKlines`, `RemoteFunding`, `RemoteTicks`) holds ONE, lazily dialled, for
//! the life of the request it was built for (the daemon keeps per-connection state such as the
//! read-once OANDA token memo, so a connection per day chunk would be a store read per chunk), and
//! `RemoteRollingFamily` holds one for the daemon's per-connection planner. The two long-lived
//! sockets — a `stream` session and an options `feed` — are owned by their own reconnecting readers
//! (`vike_market_stream::market`, `options`).
//!
//! A reply is waited for by CLASS ([`crate::proto::ReplyWait`]): a pure lookup is bounded in seconds
//! (`LOOKUP_REPLY_TIMEOUT`), a REST-backed call in minutes (`REPLY_IDLE_TIMEOUT`), a fetch per frame
//! by the cadence the daemon states. A client frame over `GW_UP_FRAME_CEILING_BYTES` is refused
//! locally before its first byte, so no request can wedge the daemon's read bound.
//!
//! # The connect handshake
//!
//! [`GatewayClient::connect`] bounds the dial (`vike_hub_proto::frame::connect_bounded`), sends
//! `Hello`, checks the daemon's version (naming BOTH numbers) and that its `Welcome` advertises the
//! plane sentinel, and only then signs: a mac signed toward the wrong service is a mac for another
//! service. There is no key-less path — a daemon that offers no authentication is refused.
//!
//! # Bounding a reply
//!
//! Past the handshake every positional verb goes through ONE exchange, [`GatewayClient::exchange`],
//! which arms the socket's IDLE read bound for that verb before writing it, from the exhaustive
//! [`reply_wait`] class. A socket read timeout is per syscall, so a large reply streaming steadily
//! is never clipped. The fetch family is the exception ([`GatewayClient::fetch`]): its bound is per
//! FRAME and per REQUEST — `max(fetch_stall_floor, 3 × the daemon's stated next_in_ms)` — so a long
//! paged fetch that keeps emitting progress never times out sooner than the in-process fetch would
//! (drift register R8), while a wedged daemon is still called dead.
//!
//! ⚠ **A failed exchange makes the client UNUSABLE.** A timeout or any transport error mid-frame
//! leaves the stream desynced, so every later verb on that client is refused LOCALLY with
//! [`io::ErrorKind::NotConnected`], nothing sent, naming the first failure.

use std::io;
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::source::SourceError;
use vike_hub_proto::auth::{self, HubKeys, Scope};
use vike_hub_proto::frame::connect_bounded;
use vike_hub_proto::limits::{CLIENT_HANDSHAKE_DEADLINE, SERVER_HANDSHAKE_MAX_FRAME_LEN};
use vike_market_stream::frame::{is_timeout, read_capped, write_up};
use vike_market_stream::opener::DataStream;

use crate::proto::{
    FEATURE_GW_DATA, FEATURE_GW_FETCH_CREDENTIALED, FEATURE_GW_FETCH_PROGRESS, FEATURE_GW_LEASE,
    FEATURE_GW_OPTIONS, FEATURE_GW_PUMP, FEATURE_GW_STATUS, FEATURE_GW_STREAM_CLASS,
    FEATURE_GW_STREAM_NETWORK, FEATURE_GW_TRADE, FEATURE_GW_TRADE_CURSOR, FETCH_NEXT_IN_CEILING,
    FETCH_PROGRESS_EVERY, FETCH_STALL_FLOOR, FetchAssembler, FetchFailure, FetchWhat,
    GATEWAYHUB_DOMAIN, GATEWAYHUB_PROTO_VERSION, GW_DOWN_FRAME_CEILING_BYTES,
    GW_STREAM_HEARTBEAT_MAX, GW_STREAM_HEARTBEAT_MIN, GW_STREAM_READ_FLOOR, GW_SUB_REPLY_TIMEOUT,
    GW_TRADE_HEARTBEAT_MAX, GW_TRADE_HEARTBEAT_MIN, GW_TRADE_WRITE_TIMEOUT,
    GW_UP_FRAME_CEILING_BYTES, GwStatusReport, LOOKUP_REPLY_TIMEOUT, OptionsFeedSpec,
    REPLY_IDLE_TIMEOUT, ReplyWait, Request, Response, TradeOpened, WireCursor,
    advertised_cd_venues, advertised_cs_venues, advertised_md_venues, advertised_trade_venues,
    cd_venue_feature, cs_venue_feature, md_venue_feature, plane_mismatch_message, reply_wait,
    request_kind, required_scope, scope_admits, trade_venue_feature, welcome_is_gatewayhub,
    write_frame,
};

/// Write timeout for a positional connection's life once the handshake is done. A request frame is
/// tens of bytes, so a write blocked this long is a peer that has stopped READING.
const REQUEST_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a probed fetch re-checks its stop probe while it waits for the next frame.
const FETCH_PROBE_SLICE: Duration = Duration::from_millis(250);

/// The first reconnect delay of a long-lived socket (a stream, a feed).
const RECONNECT_BASE: Duration = Duration::from_millis(250);

/// The longest reconnect delay: a flapping daemon is retried at this cadence for ever.
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// The text of a fetch abandoned at the caller's stop probe (`SourceError::Fetch`, per the wire
/// design's B.6).
pub const FETCH_CANCELLED: &str = "gatewayhub fetch cancelled by the caller's stop probe";

/// Every wait this crate makes, in one value the caller can narrow in a test.
///
/// The defaults ARE the protocol constants ([`REPLY_IDLE_TIMEOUT`], [`FETCH_STALL_FLOOR`],
/// [`GW_STREAM_READ_FLOOR`], [`GW_SUB_REPLY_TIMEOUT`]); production passes [`GatewayTimings::default`]
/// and a test passes shorter ones so a stall is watched in milliseconds rather than half a minute.
/// ⚠ The values are floors in production's mouth: a window shorter than the daemon's own cadence
/// (progress every [`FETCH_PROGRESS_EVERY`], heartbeats every [`crate::proto::GW_STREAM_HEARTBEAT`])
/// tears a healthy link down, which is why nothing but a test narrows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayTimings {
    /// The idle reply bound of a positional verb that calls a venue (an options chain, a family resolve).
    pub reply_idle: Duration,
    /// The idle reply bound of a pure lookup (a describe, a presence word, a status snapshot, an open
    /// acknowledgement).
    pub lookup_reply: Duration,
    /// The floor of a fetch's per-frame idle bound.
    pub fetch_stall_floor: Duration,
    /// How often a probed fetch re-checks its stop probe.
    pub fetch_probe_slice: Duration,
    /// The floor of a stream's heartbeat deadline.
    pub stream_read_floor: Duration,
    /// How long a `subscribe_*` waits for its `SubOk`/`SubErr`.
    pub sub_reply: Duration,
    /// The first reconnect delay of a long-lived socket.
    pub reconnect_base: Duration,
    /// The longest reconnect delay.
    pub reconnect_max: Duration,
}

impl Default for GatewayTimings {
    fn default() -> Self {
        Self {
            reply_idle: REPLY_IDLE_TIMEOUT,
            lookup_reply: LOOKUP_REPLY_TIMEOUT,
            fetch_stall_floor: FETCH_STALL_FLOOR,
            fetch_probe_slice: FETCH_PROBE_SLICE,
            stream_read_floor: GW_STREAM_READ_FLOOR,
            sub_reply: GW_SUB_REPLY_TIMEOUT,
            reconnect_base: RECONNECT_BASE,
            reconnect_max: RECONNECT_MAX,
        }
    }
}

/// What a strategy-lane stream session asks beyond the keyless default ([`GatewayClient::open_stream`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamAsk {
    /// The venue's network (`DATA_NETWORK_MAINNET` / `_TESTNET`), for the one venue that takes one.
    pub network: Option<String>,
    /// A credentialed venue (`cs_venue=`): the daemon logs in as the account the strategy trades.
    pub credentialed: bool,
    /// The session hosts a tick pump (`SubOp::Ticks`): `gw_pump` must be advertised.
    pub pump: bool,
}

/// Where the gatewayhub is and how to prove who we are: an address, the hub keys (as parameters —
/// this crate reads no store) and the timings. Cheap to clone (the keys are shared) and carried by
/// every remote seam, which dials from it (per call, or once per request for a source).
///
/// `Debug` shows the address and which scopes have a key, never a key (`HubKeys` redacts).
#[derive(Debug, Clone)]
pub struct GatewayLink {
    addr: String,
    keys: Arc<HubKeys>,
    timings: GatewayTimings,
}

impl GatewayLink {
    /// A link to the gatewayhub at `addr` (`host:port`), signing with `keys`.
    pub fn new(addr: impl Into<String>, keys: HubKeys) -> Self {
        Self { addr: addr.into(), keys: Arc::new(keys), timings: GatewayTimings::default() }
    }

    /// The same link with other timings — a test's tool; see [`GatewayTimings`].
    #[must_use]
    pub fn with_timings(mut self, timings: GatewayTimings) -> Self {
        self.timings = timings;
        self
    }

    /// The daemon's address as given.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// The timings this link dials with.
    pub fn timings(&self) -> GatewayTimings {
        self.timings
    }

    /// Whether this link holds a non-empty key for `scope` — whether a dial under it can be signed.
    pub fn can_sign(&self, scope: Scope) -> bool {
        self.keys.has(scope)
    }
}

/// A connected, authenticated gatewayhub client over one positional connection. See the module doc.
#[derive(Debug)]
pub struct GatewayClient {
    stream: TcpStream,
    addr: String,
    features: Vec<String>,
    scope: Scope,
    timings: GatewayTimings,
    next_rid: u64,
    /// Whether the fetch in progress (or the last one) read at least one reply frame. A link failure
    /// BEFORE any frame on a held connection is a stale connection, not a failed fetch
    /// ([`GatewayClient::heard_a_reply`]).
    heard: bool,
    /// Whether the fetch in progress (or the last one) ended on a TIMEOUT: the stall bound fired, or a
    /// frame that had begun never finished ([`GatewayClient::timed_out`]).
    timed_out: bool,
    unusable: Option<String>,
}

/// What a finished [`GatewayClient::fetch`] hands back: the reassembly and the terminal frame's
/// totals, closed by one of [`FetchAssembler`]'s typed `finish_*` methods.
#[derive(Debug)]
pub struct FetchReply {
    /// The pages, in order.
    pub assembler: FetchAssembler,
    /// `FetchDone.pages`.
    pub pages: u32,
    /// `FetchDone.rows`.
    pub rows: u64,
}

/// Why a fetch did not produce rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The daemon answered with a failure; the in-process `SourceError`/`CatalogError` variant and
    /// inner text, unchanged.
    Failed(FetchFailure),
    /// The link failed or the daemon misbehaved: a complete `gatewayhub <addr>: ...` sentence. NEVER
    /// an empty answer and never a `Refused` — nothing says the request cannot be expressed.
    Link(String),
    /// The caller's stop probe fired and the fetch was abandoned ([`FETCH_CANCELLED`]).
    Cancelled,
}

impl FetchError {
    /// The `SourceError` this is: a daemon failure keeps its variant and text; a link failure and a
    /// cancel are `Fetch` (retryable), never `Refused`.
    pub fn into_source_error(self) -> SourceError {
        match self {
            FetchError::Failed(failure) => failure.into_source_error(),
            FetchError::Link(text) => SourceError::Fetch(text),
            FetchError::Cancelled => SourceError::Fetch(FETCH_CANCELLED.to_string()),
        }
    }

    /// The text of this error, whatever the variant — what a `String`-error seam (the tick fetch, a
    /// `CatalogError`) carries.
    pub fn into_text(self) -> String {
        match self {
            FetchError::Failed(failure) => failure.text().to_string(),
            FetchError::Link(text) => text,
            FetchError::Cancelled => FETCH_CANCELLED.to_string(),
        }
    }
}

/// An opened options `feed`: the socket after [`Response::OptionsFeedOpened`], speaking
/// [`crate::proto::OptionsFeedDown`] only.
#[derive(Debug)]
pub struct FeedStream {
    /// The socket, write timeout armed.
    pub stream: TcpStream,
    /// The daemon's epoch.
    pub epoch: u64,
    /// The stated heartbeat period, clamped into the protocol's range.
    pub heartbeat: Duration,
}

/// What a caller asks of [`GatewayClient::open_trade`]: one lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeOpenArgs {
    /// The caller's id for the lease, stable across its reconnects.
    pub client_id: String,
    /// A `vike_model::VENUES` slug.
    pub venue: String,
    /// The account label's text; `None` is the default account.
    pub account: Option<String>,
    /// The tier the caller believes the account runs at.
    pub tier: vike_model::AccountTier,
    /// Where to resume the event ring from, if the caller holds a cursor.
    pub resume: Option<WireCursor>,
    /// Take the lease over from its holder.
    pub takeover: bool,
}

/// An opened `trade` lease: the socket after [`Response::TradeOpened`], speaking
/// [`crate::proto::TradeUp`] / [`crate::proto::TradeDown`] only.
#[derive(Debug)]
pub struct TradeStream {
    /// The socket, write timeout armed, read timeout cleared (the owner arms its own deadline).
    pub stream: TcpStream,
    /// The open reply, verbatim.
    pub opened: TradeOpened,
    /// The stated heartbeat period, clamped into the protocol's range.
    pub heartbeat: Duration,
    /// The daemon's advertised features (the lease owner checks per-verb strings against them).
    pub features: Vec<String>,
}

/// Why a lease did not open. `LeaseHeld` and `Refused` are the DAEMON's answers about the account and
/// are never retried blindly; `Link` is a transport or negotiation failure.
#[derive(Debug)]
pub enum TradeOpenError {
    /// Dial, handshake, a missing feature string or a transport error (`io::ErrorKind` as ever).
    Link(io::Error),
    /// Another client holds the lease.
    LeaseHeld {
        /// The holder's key id.
        holder_key_id: String,
        /// When it took the lease, epoch ms.
        since_ms: i64,
    },
    /// The daemon refused the account's state; `block_word` is `ArmingBlock::word()`.
    Refused {
        /// The daemon's word.
        block_word: String,
        /// The daemon's text.
        text: String,
    },
    /// A [`Response::Error`] of the daemon, verbatim.
    Daemon(String),
}

impl std::fmt::Display for TradeOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TradeOpenError::Link(e) => write!(f, "{e}"),
            TradeOpenError::LeaseHeld { holder_key_id, since_ms } => write!(
                f,
                "the trade lease is held by {holder_key_id} since {since_ms}; take it over \
                 explicitly or stop the other trade client"
            ),
            TradeOpenError::Refused { block_word, text } => {
                write!(f, "the gatewayhub refused the lease ({block_word}): {text}")
            }
            TradeOpenError::Daemon(text) => write!(f, "{text}"),
        }
    }
}

impl std::error::Error for TradeOpenError {}

impl From<io::Error> for TradeOpenError {
    fn from(e: io::Error) -> Self {
        TradeOpenError::Link(e)
    }
}

/// A stated trade heartbeat period as a `Duration`, clamped into
/// [`GW_TRADE_HEARTBEAT_MIN`]`..=`[`GW_TRADE_HEARTBEAT_MAX`]: a period outside the range is a daemon bug
/// and the client runs the nearest legal deadline rather than believe it.
pub fn stated_trade_heartbeat(heartbeat_ms: u64) -> Duration {
    Duration::from_millis(heartbeat_ms).clamp(GW_TRADE_HEARTBEAT_MIN, GW_TRADE_HEARTBEAT_MAX)
}

/// The idle bound against the next frame of a fetch: `max(floor, 3 × next_in_ms)`, with the stated cadence
/// believed up to [`FETCH_NEXT_IN_CEILING`] and no further, so a daemon cannot switch the detector off.
pub fn stall_bound(floor: Duration, next_in_ms: u64) -> Duration {
    let ceiling = u64::try_from(FETCH_NEXT_IN_CEILING.as_millis()).unwrap_or(u64::MAX);
    floor.max(Duration::from_millis(next_in_ms.min(ceiling).saturating_mul(3)))
}

/// A stated heartbeat period as a `Duration`, clamped into
/// [`GW_STREAM_HEARTBEAT_MIN`]`..=`[`GW_STREAM_HEARTBEAT_MAX`] — the range a server may run. A
/// period outside it is a daemon bug and the client would rather run the nearest legal deadline than
/// believe it.
pub fn stated_heartbeat(heartbeat_ms: u64) -> Duration {
    Duration::from_millis(heartbeat_ms).clamp(GW_STREAM_HEARTBEAT_MIN, GW_STREAM_HEARTBEAT_MAX)
}

/// An error as a `gatewayhub <addr>: ...` sentence, without doubling a prefix the error already has —
/// the one spelling of a transport failure every remote seam reports, so an operator greps one word.
pub fn gateway_text(addr: &str, e: &dyn std::fmt::Display) -> String {
    let text = e.to_string();
    if text.starts_with("gatewayhub") { text } else { format!("gatewayhub {addr}: {text}") }
}

fn legible_stall(e: io::Error, msg: impl FnOnce() -> String) -> io::Error {
    if is_timeout(&e) { io::Error::new(io::ErrorKind::TimedOut, msg()) } else { e }
}

impl GatewayClient {
    /// Dial `link`, handshake and authenticate under `scope`.
    ///
    /// Fails with [`io::ErrorKind::PermissionDenied`] when the link holds no key for `scope` (nothing
    /// is dialled) or the daemon refuses the mac, and [`io::ErrorKind::InvalidData`] on a version skew
    /// (naming both numbers), a peer that is not a gatewayhub (no plane sentinel — refused BEFORE
    /// `Auth` is written) or a `Welcome` carrying no usable challenge.
    pub fn connect(link: &GatewayLink, scope: Scope) -> io::Result<Self> {
        Self::connect_observed(link, scope, |_| {})
    }

    /// [`GatewayClient::connect`], handing `observe` the freshly dialled socket BEFORE the handshake
    /// reads from it. A long-lived owner (the stream reader) stores a clone so that a shutdown can break
    /// a dial parked in `Welcome` on a daemon that accepted and went quiet, instead of waiting out the
    /// handshake deadline.
    pub fn connect_observed(
        link: &GatewayLink,
        scope: Scope,
        observe: impl FnOnce(&TcpStream),
    ) -> io::Result<Self> {
        if !link.keys.has(scope) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "gatewayhub {}: no {} hub key is configured for this link, so a {scope:?} \
                     connection cannot be signed (nothing was dialled)",
                    link.addr,
                    scope.name()
                ),
            ));
        }
        let stream = connect_bounded(link.addr.as_str()).map_err(|e| {
            io::Error::new(e.kind(), format!("gatewayhub {}: cannot connect: {e}", link.addr))
        })?;
        observe(&stream);
        let mut client = Self {
            stream,
            addr: link.addr.clone(),
            features: Vec::new(),
            scope,
            timings: link.timings,
            next_rid: 0,
            heard: false,
            timed_out: false,
            unusable: None,
        };
        let nonce = client.handshake()?;
        // `Auth` under `Scope::Trade` is never written to a daemon that did not advertise the class:
        // the same refusal-before-signing the plane sentinel makes, because a mac signed with the ORDER
        // key toward a daemon that does not serve orders is a mac for nothing the operator meant.
        if scope == Scope::Trade {
            client.require_feature(FEATURE_GW_TRADE)?;
        }
        let mac = auth::sign(
            GATEWAYHUB_DOMAIN,
            link.keys.key_for(scope),
            &nonce,
            GATEWAYHUB_PROTO_VERSION,
            scope,
        );
        write_frame(&mut client.stream, &Request::Auth { scope, mac })?;
        match read_capped::<_, Response>(&mut client.stream, SERVER_HANDSHAKE_MAX_FRAME_LEN)? {
            Response::AuthOk { scope: granted } if granted == scope => {}
            Response::AuthOk { scope: granted } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "gatewayhub {}: asked for {scope:?} and the daemon granted {granted:?}",
                        client.addr
                    ),
                ));
            }
            Response::AuthDenied { reason } => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("gatewayhub {}: auth denied ({}): {reason}", client.addr, scope.name()),
                ));
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "gatewayhub {}: auth: expected AuthOk/AuthDenied, got {}",
                        client.addr,
                        resp_kind(&other)
                    ),
                ));
            }
        }
        client.stream.set_read_timeout(None)?;
        client.stream.set_write_timeout(Some(REQUEST_WRITE_TIMEOUT))?;
        Ok(client)
    }

    /// `Hello` → `Welcome`: check the version and the plane, keep the features, return the nonce.
    fn handshake(&mut self) -> io::Result<[u8; 32]> {
        write_frame(&mut self.stream, &Request::Hello { proto_version: GATEWAYHUB_PROTO_VERSION })?;
        match read_capped::<_, Response>(&mut self.stream, SERVER_HANDSHAKE_MAX_FRAME_LEN)? {
            Response::Welcome { proto_version, features, nonce } => {
                if proto_version != GATEWAYHUB_PROTO_VERSION {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "gatewayhub {}: protocol version mismatch — this client speaks \
                             {GATEWAYHUB_PROTO_VERSION}, the daemon speaks {proto_version}. \
                             Upgrade whichever side is older.",
                            self.addr
                        ),
                    ));
                }
                self.features = features;
                if !welcome_is_gatewayhub(&self.features) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        plane_mismatch_message(&self.addr, &self.features),
                    ));
                }
                Ok(nonce)
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "gatewayhub {}: handshake: expected Welcome, got {}",
                    self.addr,
                    resp_kind(&other)
                ),
            )),
        }
    }

    /// The daemon's advertised feature strings.
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// The scope this connection authenticated under.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// The daemon's address as given to the link.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Whether this client may still send: `false` once an exchange failed mid-frame.
    pub fn is_usable(&self) -> bool {
        self.unusable.is_none()
    }

    /// Whether the latest [`GatewayClient::fetch`] read at least one reply frame. A link failure with
    /// `false` here, on a connection that had been idle, is a STALE connection (the daemon reaped it) and
    /// not a failed fetch: nothing reached the venue, so repeating the fetch on a fresh connection is safe.
    pub fn heard_a_reply(&self) -> bool {
        self.heard
    }

    /// Whether the latest [`GatewayClient::fetch`] ended on a TIMEOUT: the per-frame stall bound fired
    /// (nothing arrived), or a frame that had begun never finished. **A timeout is never a stale
    /// connection**: a reaped connection fails at once (EOF or a reset), whereas a daemon that read the
    /// request and went quiet may well be spending the venue on it, so a caller that repeats a failed
    /// fetch on a fresh connection must not repeat one that timed out, however little it heard
    /// ([`GatewayClient::heard_a_reply`] is `false` for a stall that fired before the first frame).
    pub fn timed_out(&self) -> bool {
        self.timed_out
    }

    /// **Refuse locally, naming the string**, unless the daemon advertised `feature` — the client leg
    /// of decision 0112. Nothing has been sent when this fails, and the message says what to do.
    pub fn require_feature(&self, feature: &str) -> io::Result<()> {
        if self.features.iter().any(|f| f == feature) {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "gatewayhub at {} does not advertise {feature:?} — the daemon predates this verb or \
                 was built without the lane; upgrade vike-gatewayhub. Nothing was sent.",
                self.addr
            ),
        ))
    }

    /// **The ONE post-handshake exchange for positional verbs**: arm the verb's idle reply bound,
    /// write it, read one response.
    ///
    /// ⚠ Any transport error from the write or the read MARKS THE CLIENT UNUSABLE (the module doc).
    /// The fetch family is refused here ([`io::ErrorKind::InvalidInput`], nothing sent): its reply is
    /// many frames and goes through [`GatewayClient::fetch`].
    pub fn exchange(&mut self, request: &Request) -> io::Result<Response> {
        self.exchange_bounded(request, None)
    }

    /// [`GatewayClient::exchange`] with the reply wait ALSO capped at `cap`: the verb's class bound
    /// ([`reply_wait`]) or `cap`, whichever is SHORTER. For a caller whose own latency budget is
    /// tighter than the class's (the recorder's tick resolves a family with a stop signal waiting
    /// behind it, `crate::resolve::RESOLVE_REPLY_BOUND`). The handshake bound is not narrowed, and a
    /// timeout is the same failure it always is: the client is unusable afterwards.
    pub fn exchange_within(&mut self, request: &Request, cap: Duration) -> io::Result<Response> {
        self.exchange_bounded(request, Some(cap))
    }

    fn exchange_bounded(
        &mut self,
        request: &Request,
        cap: Option<Duration>,
    ) -> io::Result<Response> {
        let verb = request_kind(request);
        if let Some(first) = &self.unusable {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                format!(
                    "gatewayhub {} {verb}: not sent — this connection is unusable since {first}; \
                     drop the client and dial a new one",
                    self.addr
                ),
            ));
        }
        if !scope_admits(self.scope, required_scope(request)) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "gatewayhub {} {verb}: not sent — this connection authenticated as {} and the \
                     verb needs more",
                    self.addr,
                    self.scope.name()
                ),
            ));
        }
        let narrowed = |class_bound: Duration| cap.map_or(class_bound, |c| class_bound.min(c));
        let bound = match reply_wait(request) {
            ReplyWait::Handshake => Some(CLIENT_HANDSHAKE_DEADLINE),
            ReplyWait::Lookup => Some(narrowed(self.timings.lookup_reply)),
            ReplyWait::Bounded => Some(narrowed(self.timings.reply_idle)),
            ReplyWait::Progress => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("gatewayhub {verb}: a progress-framed verb goes through fetch()"),
                ));
            }
        };
        self.stream.set_read_timeout(bound)?;
        if let Err(e) = write_up(&mut self.stream, request, GW_UP_FRAME_CEILING_BYTES) {
            // `InvalidData` is `write_frame`'s refusal BEFORE the first byte: the stream is in step.
            if e.kind() == io::ErrorKind::InvalidData {
                return Err(e);
            }
            let addr = self.addr.clone();
            let e = legible_stall(e, || {
                format!(
                    "gatewayhub {addr} {verb}: the daemon accepted no request byte for \
                     {REQUEST_WRITE_TIMEOUT:?} (it stopped reading)"
                )
            });
            return Err(self.mark_unusable(verb, e));
        }
        match read_capped::<_, Response>(&mut self.stream, GW_DOWN_FRAME_CEILING_BYTES) {
            Ok(response) => Ok(response),
            Err(e) => {
                let addr = self.addr.clone();
                let e = legible_stall(e, || {
                    let bound = bound.map_or_else(|| "its bound".to_string(), |b| format!("{b:?}"));
                    format!(
                        "gatewayhub {addr} {verb}: no reply byte for {bound} — the link is dead or \
                         the daemon is wedged; this connection is not reused"
                    )
                });
                Err(self.mark_unusable(verb, e))
            }
        }
    }

    fn mark_unusable(&mut self, verb: &str, e: io::Error) -> io::Error {
        self.unusable = Some(format!("{verb} failed mid-exchange ({e})"));
        e
    }

    /// `GwStatus` → the daemon's status report (names and counters, never values). Refuses locally,
    /// naming `gw_status`, when the daemon does not advertise it.
    pub fn status(&mut self) -> io::Result<GwStatusReport> {
        self.require_feature(FEATURE_GW_STATUS)?;
        match self.exchange(&Request::GwStatus)? {
            Response::StatusIs(report) => Ok(*report),
            Response::Error(text) => Err(io::Error::other(text)),
            other => Err(unexpected(&self.addr, "StatusIs", &other)),
        }
    }

    /// `Ping` → `Pong`.
    pub fn ping(&mut self) -> io::Result<()> {
        match self.exchange(&Request::Ping)? {
            Response::Pong => Ok(()),
            other => Err(unexpected(&self.addr, "Pong", &other)),
        }
    }

    /// **One fetch**: write `Fetch` (or `FetchCredentialed`), then read the progress / page /
    /// terminal sequence, reassembling the pages.
    ///
    /// * Local refusals send nothing: a daemon that does not advertise the lane's feature, the
    ///   progress contract or (credentialed) the venue, and a connection whose scope does not admit
    ///   the verb ([`required_scope`]).
    /// * The idle bound is per FRAME: `max(fetch_stall_floor, 3 × next_in_ms)` of the latest
    ///   [`Response::FetchProgress`]; any frame resets it, so a fetch that keeps emitting frames runs
    ///   as long as the in-process one would. Before the first frame the bound is
    ///   `max(floor, 3 × FETCH_PROGRESS_EVERY)`.
    /// * `should_stop`, when given, is polled every `fetch_probe_slice` while waiting. On `true` the
    ///   client writes `FetchCancel`, drops the connection and returns [`FetchError::Cancelled`] —
    ///   faster than a chunk boundary, and a dropped connection cancels the daemon's work anyway.
    ///   The wait is a `peek`, never a timed-out frame read, so a probe tick can never desync the
    ///   stream.
    ///
    /// The client stays usable after a terminal frame, and is unusable after any link error or cancel.
    pub fn fetch(
        &mut self,
        what: &FetchWhat,
        credentialed: bool,
        should_stop: Option<&dyn Fn() -> bool>,
    ) -> Result<FetchReply, FetchError> {
        self.next_rid += 1;
        self.heard = false;
        self.timed_out = false;
        let rid = self.next_rid;
        let request = if credentialed {
            Request::FetchCredentialed { rid, what: what.clone() }
        } else {
            Request::Fetch { rid, what: what.clone() }
        };
        let verb = request_kind(&request);
        if let Some(first) = &self.unusable {
            return Err(self.link_error(format!(
                "{verb}: not sent — this connection is unusable since {first}"
            )));
        }
        let mut needed = vec![what.feature(), FEATURE_GW_FETCH_PROGRESS];
        if credentialed {
            needed.push(FEATURE_GW_FETCH_CREDENTIALED);
        }
        for feature in needed {
            if let Err(e) = self.require_feature(feature) {
                return Err(FetchError::Link(e.to_string()));
            }
        }
        if credentialed && let FetchWhat::Klines { venue, .. } = what {
            let served = advertised_cd_venues(&self.features);
            if !served.iter().any(|v| v == venue) {
                return Err(FetchError::Link(format!(
                    "gatewayhub at {} does not advertise {:?} — it holds no credential for \
                     {venue} history. Nothing was sent.",
                    self.addr,
                    cd_venue_feature(venue)
                )));
            }
        }
        if !scope_admits(self.scope, required_scope(&request)) {
            return Err(self.link_error(format!(
                "{verb}: not sent — this connection authenticated as {} and the verb needs more",
                self.scope.name()
            )));
        }
        if let Err(e) = write_up(&mut self.stream, &request, GW_UP_FRAME_CEILING_BYTES) {
            if e.kind() == io::ErrorKind::InvalidData {
                return Err(self.link_error(format!("{verb}: {e}")));
            }
            let e = self.mark_unusable(verb, e);
            return Err(self.link_error(format!("{verb}: the request could not be written: {e}")));
        }

        let floor = self.timings.fetch_stall_floor;
        let slice = self.timings.fetch_probe_slice;
        let mut gap = stall_bound(floor, FETCH_PROGRESS_EVERY.as_millis() as u64);
        let mut last = Instant::now();
        let mut assembler = FetchAssembler::new(rid);
        loop {
            // 1. Wait until a frame has BEGUN, polling the stop probe between slices.
            loop {
                if let Some(stop) = should_stop
                    && stop()
                {
                    let _ = write_frame(&mut self.stream, &Request::FetchCancel { rid });
                    self.unusable = Some(format!("{verb} {rid} was cancelled by the caller"));
                    let _ = self.stream.shutdown(Shutdown::Both);
                    return Err(FetchError::Cancelled);
                }
                let waited = last.elapsed();
                if waited >= gap {
                    self.timed_out = true;
                    self.unusable = Some(format!("{verb} {rid} went silent"));
                    let _ = self.stream.shutdown(Shutdown::Both);
                    return Err(self.link_error(format!(
                        "{verb} {rid}: no frame for {waited:?} (the bound is {gap:?}) — the link is \
                         dead or the daemon is wedged"
                    )));
                }
                let remaining = gap - waited;
                let wait = if should_stop.is_some() { remaining.min(slice) } else { remaining };
                if let Err(e) =
                    self.stream.set_read_timeout(Some(wait.max(Duration::from_millis(1))))
                {
                    return Err(self.fail(verb, rid, e));
                }
                let mut probe = [0u8; 1];
                match self.stream.peek(&mut probe) {
                    Ok(0) => {
                        self.unusable = Some(format!("{verb} {rid} lost its connection"));
                        return Err(self.link_error(format!(
                            "{verb} {rid}: the daemon closed the connection mid-fetch"
                        )));
                    }
                    Ok(_) => break,
                    Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(self.fail(verb, rid, e)),
                }
            }
            // 2. Read the whole frame. Its bytes are on the way: the per-syscall bound is generous.
            if let Err(e) = self.stream.set_read_timeout(Some(floor)) {
                return Err(self.fail(verb, rid, e));
            }
            let response =
                match read_capped::<_, Response>(&mut self.stream, GW_DOWN_FRAME_CEILING_BYTES) {
                    Ok(r) => r,
                    Err(e) => return Err(self.fail(verb, rid, e)),
                };
            last = Instant::now();
            self.heard = true;
            match response {
                Response::FetchProgress(p) if p.rid == rid => {
                    gap = stall_bound(floor, p.next_in_ms);
                }
                Response::FetchPage { rid: r, page, rows } => {
                    if let Err(why) = assembler.push(r, page, rows) {
                        self.unusable = Some(format!("{verb} {rid} sent a bad page ({why})"));
                        return Err(self.link_error(format!("{verb} {rid}: {why}")));
                    }
                }
                Response::FetchDone { rid: r, pages, rows } if r == rid => {
                    return Ok(FetchReply { assembler, pages, rows });
                }
                Response::FetchFailed { rid: r, failure } if r == rid => {
                    return Err(FetchError::Failed(failure));
                }
                Response::FetchCancelled { rid: r } if r == rid => {
                    self.unusable = Some(format!("{verb} {rid} was cancelled by the daemon"));
                    return Err(self.link_error(format!(
                        "{verb} {rid}: the daemon cancelled the fetch (it is stopping)"
                    )));
                }
                Response::Error(text) => return Err(FetchError::Failed(FetchFailure::Fetch(text))),
                other => {
                    self.unusable = Some(format!("{verb} {rid} desynced"));
                    return Err(self.link_error(format!(
                        "{verb} {rid}: unexpected {} in a fetch reply",
                        resp_kind(&other)
                    )));
                }
            }
        }
    }

    /// A link failure of fetch `rid`: mark the client unusable and render it.
    fn fail(&mut self, verb: &str, rid: u64, e: io::Error) -> FetchError {
        self.timed_out |= is_timeout(&e);
        self.unusable = Some(format!("{verb} {rid} failed ({e})"));
        self.link_error(format!("{verb} {rid}: {e}"))
    }

    fn link_error(&self, msg: String) -> FetchError {
        FetchError::Link(format!("gatewayhub {}: {msg}", self.addr))
    }

    /// Open a `stream` session for `venue`, consuming this client: the connection becomes a
    /// [`DataStream`] and is the caller's to read.
    ///
    /// Refuses locally, naming the string, when the daemon does not advertise `gw_data` or the venue's
    /// `md_venue=` entry; a daemon-side refusal ([`Response::Error`]) is returned as an
    /// [`io::ErrorKind::Other`] error carrying the daemon's text unchanged.
    pub fn open_data(self, venue: &str) -> io::Result<DataStream> {
        self.open_stream(venue, &StreamAsk::default())
    }

    /// [`GatewayClient::open_data`] with what a strategy-lane session may add (`ask`): a network, a
    /// credentialed venue (the venue is then checked against `cs_venue=`, and this connection must be
    /// `Scope::Write`) and a tick pump (`gw_pump` required).
    ///
    /// Every check is LOCAL and names the string: a daemon that does not advertise `stream_network`
    /// would ignore the unknown field and open the main network, so a network is never sent to one.
    pub fn open_stream(mut self, venue: &str, ask: &StreamAsk) -> io::Result<DataStream> {
        self.require_feature(FEATURE_GW_DATA)?;
        let (served, entry) = if ask.credentialed {
            (advertised_cs_venues(&self.features), cs_venue_feature(venue))
        } else {
            (advertised_md_venues(&self.features), md_venue_feature(venue))
        };
        if !served.iter().any(|v| v == venue) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "gatewayhub at {} does not advertise {entry:?} — it serves no live stream for                      {venue}. Nothing was sent.",
                    self.addr,
                ),
            ));
        }
        if ask.credentialed && self.scope != Scope::Write {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "gatewayhub at {}: the credentialed stream of {venue} needs a control-scope \
                     connection (this one is {}). Nothing was sent.",
                    self.addr,
                    self.scope.name()
                ),
            ));
        }
        if ask.network.is_some() {
            self.require_feature(FEATURE_GW_STREAM_NETWORK)?;
        }
        if ask.pump {
            self.require_feature(FEATURE_GW_PUMP)?;
        }
        let open = Request::DataOpen { venue: venue.to_string(), network: ask.network.clone() };
        match self.exchange(&open)? {
            Response::DataOpened { session, epoch, heartbeat_ms } => {
                self.stream.set_write_timeout(Some(crate::proto::GW_STREAM_WRITE_TIMEOUT))?;
                Ok(DataStream {
                    stream: self.stream,
                    session,
                    epoch,
                    heartbeat: stated_heartbeat(heartbeat_ms),
                    delivery_class: self.features.iter().any(|f| f == FEATURE_GW_STREAM_CLASS),
                })
            }
            Response::Error(text) => Err(io::Error::other(text)),
            other => Err(unexpected(&self.addr, "DataOpened", &other)),
        }
    }

    /// Open an options `feed`, consuming this client; see [`GatewayClient::open_data`].
    pub fn open_options_feed(mut self, spec: &OptionsFeedSpec) -> io::Result<FeedStream> {
        self.require_feature(FEATURE_GW_OPTIONS)?;
        match self.exchange(&Request::OptionsFeedOpen { feed: spec.clone() })? {
            Response::OptionsFeedOpened { epoch, heartbeat_ms } => {
                self.stream.set_write_timeout(Some(crate::proto::GW_STREAM_WRITE_TIMEOUT))?;
                Ok(FeedStream {
                    stream: self.stream,
                    epoch,
                    heartbeat: stated_heartbeat(heartbeat_ms),
                })
            }
            Response::Error(text) => Err(io::Error::other(text)),
            other => Err(unexpected(&self.addr, "OptionsFeedOpened", &other)),
        }
    }

    /// Open a `trade` lease, consuming this client: on success the connection becomes a
    /// [`TradeStream`] and is the caller's to read and write.
    ///
    /// Refuses locally, naming the string, when the daemon does not advertise `gw_trade`, `gw_lease`,
    /// the venue's `trade_venue=` entry, or (a resume) `gw_trade_cursor`; nothing is sent. The daemon's
    /// own answers keep their shape: [`TradeOpenError::LeaseHeld`], [`TradeOpenError::Refused`].
    pub fn open_trade(mut self, args: &TradeOpenArgs) -> Result<TradeStream, TradeOpenError> {
        self.require_feature(FEATURE_GW_TRADE)?;
        self.require_feature(FEATURE_GW_LEASE)?;
        if args.resume.is_some() {
            self.require_feature(FEATURE_GW_TRADE_CURSOR)?;
        }
        if !advertised_trade_venues(&self.features).contains(&args.venue) {
            return Err(TradeOpenError::Link(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "gatewayhub at {} does not advertise {:?} - it carries no trade adapter for {}. \
                     Nothing was sent.",
                    self.addr,
                    trade_venue_feature(&args.venue),
                    args.venue
                ),
            )));
        }
        let open = Request::TradeOpen {
            client_id: args.client_id.clone(),
            venue: args.venue.clone(),
            account: args.account.clone(),
            tier: args.tier,
            resume: args.resume,
            takeover: args.takeover,
        };
        match self.exchange(&open)? {
            Response::TradeOpened(opened) => {
                self.stream.set_write_timeout(Some(GW_TRADE_WRITE_TIMEOUT))?;
                self.stream.set_read_timeout(None)?;
                let heartbeat = stated_trade_heartbeat(opened.heartbeat_ms);
                Ok(TradeStream { stream: self.stream, opened, heartbeat, features: self.features })
            }
            Response::LeaseHeld { holder_key_id, since_ms } => {
                Err(TradeOpenError::LeaseHeld { holder_key_id, since_ms })
            }
            Response::TradeRefused { block_word, text } => {
                Err(TradeOpenError::Refused { block_word, text })
            }
            Response::Error(text) => Err(TradeOpenError::Daemon(text)),
            other => Err(unexpected(&self.addr, "TradeOpened", &other).into()),
        }
    }
}

/// A response variant's name, for a desync message — never its payload.
pub fn resp_kind(r: &Response) -> &'static str {
    match r {
        Response::Welcome { .. } => "Welcome",
        Response::AuthOk { .. } => "AuthOk",
        Response::AuthDenied { .. } => "AuthDenied",
        Response::Pong => "Pong",
        Response::Error(_) => "Error",
        Response::DataOpened { .. } => "DataOpened",
        Response::OptionsFeedOpened { .. } => "OptionsFeedOpened",
        Response::FetchProgress(_) => "FetchProgress",
        Response::FetchPage { .. } => "FetchPage",
        Response::FetchDone { .. } => "FetchDone",
        Response::FetchFailed { .. } => "FetchFailed",
        Response::FetchCancelled { .. } => "FetchCancelled",
        Response::CatalogDescribed { .. } => "CatalogDescribed",
        Response::OptionsReplied(_) => "OptionsReplied",
        Response::FamilyResolved { .. } => "FamilyResolved",
        Response::PresenceIs { .. } => "PresenceIs",
        Response::StatusIs(_) => "StatusIs",
        Response::ArmingTable(_) => "ArmingTable",
        Response::PreflightLegs { .. } => "PreflightLegs",
        Response::TradeOpened(_) => "TradeOpened",
        Response::LeaseHeld { .. } => "LeaseHeld",
        Response::TradeRefused { .. } => "TradeRefused",
    }
}

/// The error for a reply of the wrong kind.
pub fn unexpected(addr: &str, wanted: &str, got: &Response) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("gatewayhub {addr}: expected {wanted}, got {}", resp_kind(got)),
    )
}
