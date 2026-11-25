//! The vike-gatewayhub wire protocol, wave 1 (the DATA surface): length-prefixed `serde_json` frames
//! over a blocking `std::net` byte stream, and the request/response schema that rides them.
//! `vike-gatewayhub` imports this module as its contract, so everything a server needs that is not
//! server policy — the codec, the caps, the constants, the scope table, the paging and reassembly
//! helpers — lives HERE and is stated once.
//!
//! # Framing and the decode-vs-drop contract
//!
//! One frame is a big-endian `u32` byte length followed by that many bytes of UTF-8 JSON
//! ([`write_frame`], [`read_frame_raw_capped`]). A server MUST read raw and decode separately, so a
//! well-framed body it cannot decode (a verb from a newer client) is a bad REQUEST answered with
//! [`Response::Error`] and the connection survives; only a framing fault closes it. Post-auth reads
//! are capped per direction ([`GATEWAY_UP_FRAME_CEILING_BYTES`] for what a client sends,
//! [`GATEWAY_DOWN_FRAME_CEILING_BYTES`] for what a server sends) rather than left at
//! [`MAX_FRAME_LEN`]'s 64 MiB, and the pre-auth frames at
//! `vike_hub_protocol::limits::SERVER_HANDSHAKE_MAX_FRAME_LEN`.
//!
//! # The handshake (always keyed)
//!
//! `Hello` → `Welcome` (version, `features`, a fresh 32-byte `nonce`) → `Auth`
//! (`scope` plus the HMAC under [`GATEWAYHUB_DOMAIN`]) → `AuthOk` or the coarse
//! `AuthDenied`, after which the daemon closes. The frames are `vike_hub_protocol::handshake`'s, the one
//! spelling every hub shares, and this schema carries no copy of them (decision 0288's amendment of
//! 2026-11-21, the same bytes). There is NO key-less mode: this daemon
//! holds credentials and, in a later wave, places orders, so "no keys, no start" is the daemon's
//! rule and `Welcome.nonce` is not optional. The client checks the version (naming BOTH numbers) and
//! the [`GATEWAYHUB_PLANE_SENTINEL`] BEFORE it signs: a mac signed toward the wrong service is a mac
//! for another service. [`GATEWAYHUB_PROTO_VERSION`] moves only for a change an old peer cannot
//! DECODE; every additive verb is negotiated by a `features` string (decision 0112).
//!
//! # Three socket classes, one first frame decides
//!
//! After `AuthOk` the first request classifies the connection ([`surface_of`]):
//!
//! | class | opened by | duplex | carries |
//! |---|---|---|---|
//! | `rpc` | any positional verb | no | one request, one reply — except [`Request::Fetch`], whose reply is a PROGRESS/PAGE sequence ending in exactly one terminal frame |
//! | `stream` | [`Request::DataOpen`] → [`Response::DataOpened`] | yes | [`StreamUp`](vike_market_stream::wire::StreamUp) in, [`StreamDown`](vike_market_stream::wire::StreamDown) out: the `DataClientSink` verbs, one frame per verb, keyed by a per-session `k` |
//! | `feed` | [`Request::OptionsFeedOpen`] → [`Response::OptionsFeedOpened`] | no | [`OptionsFeedDown`] out; the client is silent |
//! | `trade` | [`Request::TradeOpen`] → [`Response::TradeOpened`] | yes | [`TradeUp`] in, [`TradeDown`] out: orders, requests, events (`Scope::Trade`; `proto/trade.rs`) |
//!
//! `TradeOpened`, like `DataOpened`, is the LAST positional frame its socket carries, and a refusal
//! ([`Response::LeaseHeld`], [`Response::TradeRefused`], [`Response::Error`]) has NOT switched it.
//! [`Request::ArmingQuery`] and [`Request::PreflightQuery`] are `rpc` verbs of the same scope.
//!
//! ⚠ **`DataOpened` is the LAST positional frame its socket carries**, `datahub-client`'s
//! `MarketDataSubscribed` rule: after it every client frame is a `StreamUp` and every server frame a
//! `StreamDown` (both `vike_market_stream::wire`'s), so a stream reader's `match` on `StreamDown` is a complete
//! protocol and anything else is a desync. A server that answers [`Response::Error`] has NOT switched.
//!
//! # The `stream` class
//!
//! The frames after the open reply, the twelve sink verbs, `k`, `seq` and the back-pressure rule are the
//! tube's (`crates/vike-market-stream/src/wire.rs`, decision 0167); this protocol keeps the open and the
//! liveness numbers it states:
//!
//! * **Liveness**: the open reply states the heartbeat period ([`Response::DataOpened`]'s
//!   `heartbeat_ms`); the server writes a `Heartbeat` frame after that much silence and the
//!   client's read deadline is `max(GATEWAY_STREAM_READ_FLOOR, 3 × heartbeat_ms)`, so a period needs no
//!   compile-time agreement between independently deployed binaries.
//!
//! ⚠ **Wave 2 adds, never changes**: `Scope::Trade` and the trade surface arrive with their own feature
//! strings (`KNOWN_TRADE_FEATURES`), the credentialed stream open with its own. No wave-1 frame's bytes moved
//! (`fixtures/gatewayhub_wire/` holds them).
//!
//! # Floats and the codec
//!
//! After the handshake a body is JSON or msgpack with field names (`vike_hub_protocol::frame::BodyCodec`, one
//! switch per connection): a client writes msgpack to a daemon that advertises `wire_msgpack`, the daemon answers
//! every frame of that connection (a reply, a fetch page, a feed frame, a lease frame) in the codec the
//! connection's request came in, and every reader here reads either body ([`read_frame`] and
//! `vike_market_stream::frame::read_capped` tell them apart by the body's magic). An older peer on either end
//! keeps JSON. The `stream` class's tube frames (`vike_market_stream::wire`) follow its `DataOpen`'s codec, the ticks as binary bodies once asked.
//!
//! Every `f64` crosses bit for bit: JSON as `serde_json` writes it (shortest round-trip, read back with the
//! workspace's `float_roundtrip` parser; `event_roundtrip` holds it with `to_bits`), msgpack as its eight
//! bytes. A NON-FINITE value serializes to `null` in JSON and fails to decode: it cannot cross a JSON
//! connection, and a daemon must drop such a row rather than send it, whatever the codec.
//!
//! [`read_frame`]: vike_hub_protocol::frame::read_frame
//! [`read_frame_raw_capped`]: vike_hub_protocol::frame::read_frame_raw_capped
//! [`write_frame`]: vike_hub_protocol::frame::write_frame

use std::time::Duration;

use serde::{Deserialize, Serialize};
use vike_catalog::{CatalogMode, PickerTab};
use vike_data::TextRow;
use vike_data::option_trade_log::OptionTradeRow;
use vike_data::source::SourceError;
use vike_hub_protocol::auth::Domain;
#[cfg(doc)]
use vike_hub_protocol::conn::ReplyWait;
use vike_market_stream::wire::StreamByeReason;
use vike_marketdata::FundingRatePoint;
use vike_marketdata::{Bar, OptionChain, OptionChainExpiry, OptionTicker, TickQuote};
use vike_model::{AccountTier, AssetClass, EventRingCursor, Instrument};

// The FRAME CODEC is `vike_hub_protocol::frame`'s, below both hub protocols
// (`docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`), and every caller names it
// at that path: this module re-exports none of it (one name, the owner's "do all 10" of 2026-11-22).
use vike_hub_protocol::frame::MAX_FRAME_LEN;

mod backend_dto;
mod features;
mod paging;
mod trade;
mod verbs;

pub use backend_dto::{
    AdapterReply, AdapterRisk, AdapterStart, ArmingRowWire, ArmingTableWire, ClockRiskInfoWire,
    CredentialGapWire, NetReadingWire, PreflightAnswer, PreflightLegWire, PreflightPlanWire,
    PreflightStep, ReconcileKind, ReconcilePayload, ServerTimeGapWire, SymbolPropertiesWire,
    TradeStatusReport, VenueLinkWire,
};
pub use features::{
    FEATURE_CREDENTIALED_DATA_VENUE_PREFIX, FEATURE_CS_VENUE_PREFIX, FEATURE_GATEWAY_ADAPTER,
    FEATURE_GATEWAY_ARMING, FEATURE_GATEWAY_CATALOG, FEATURE_GATEWAY_CREDENTIALS_CHANGED,
    FEATURE_GATEWAY_DATA, FEATURE_GATEWAY_FAMILY, FEATURE_GATEWAY_FEED_TICK, FEATURE_GATEWAY_FETCH,
    FEATURE_GATEWAY_FETCH_CANCEL, FEATURE_GATEWAY_FETCH_CREDENTIALED,
    FEATURE_GATEWAY_FETCH_PROGRESS, FEATURE_GATEWAY_LEASE, FEATURE_GATEWAY_MARKET_RULES,
    FEATURE_GATEWAY_MARKET_RULES_SERIES, FEATURE_GATEWAY_OPTION_TRADES, FEATURE_GATEWAY_OPTIONS,
    FEATURE_GATEWAY_OPTIONS_LISTING, FEATURE_GATEWAY_PREFLIGHT, FEATURE_GATEWAY_PRESENCE,
    FEATURE_GATEWAY_STREAM_BINARY_TICKS, FEATURE_GATEWAY_STREAM_CLASS,
    FEATURE_GATEWAY_STREAM_FUNDING, FEATURE_GATEWAY_STREAM_NETWORK, FEATURE_GATEWAY_TICKS,
    FEATURE_GATEWAY_TRADE, FEATURE_GATEWAY_TRADE_CONTRACT_VALUE, FEATURE_GATEWAY_TRADE_CURSOR,
    FEATURE_GATEWAY_TRADE_MODIFY_TRIGGER, FEATURE_GATEWAY_TRADE_ORDER_ATTRIBUTES,
    FEATURE_GATEWAY_TRADE_RECONCILE_READ, FEATURE_GATEWAY_TRADE_RESOLVE,
    FEATURE_GATEWAY_TRADE_SYMBOL_PROPERTIES, FEATURE_GATEWAY_TRADE_VENUE_POSITIONS,
    FEATURE_GATEWAY_TRADE_VENUE_READ, FEATURE_GATEWAY_TRADE_WIRE_IDS, FEATURE_TRADE_VENUE_PREFIX,
    GATEWAYHUB_PLANE_SENTINEL, KNOWN_FEATURES, KNOWN_TRADE_FEATURES,
    advertised_credentialed_data_venues, advertised_cs_venues, advertised_trade_venues,
    credentialed_data_venue_feature, cs_venue_feature, gateway_codec, trade_venue_feature,
};
pub use paging::{FetchAssembler, Pages, pages_of};
pub use trade::{
    DetachPhase, OrderResolution, PROPERTIES_BUSY, PROPERTIES_EXPIRED, PROPERTIES_LIST_EMPTY,
    PROPERTIES_LIST_UNAVAILABLE, PROPERTIES_NO_INSTRUMENT, PROPERTIES_NOT_STARTED,
    PROPERTIES_RATE_UNAVAILABLE, ResolveState, SYMBOL_FORGOTTEN, SymbolPropertiesRefusal,
    SymbolPropertiesRefusalCode, TradeBye, TradeDown, TradeOpened, TradeUp,
};
pub use verbs::{
    Surface, plane_mismatch_message, reply_wait, request_kind, required_scope, surface_of,
    welcome_is_gatewayhub,
};

// ---- Identity and version ------------------------------------------------------------------------

/// The gatewayhub wire protocol version, negotiated by the `Hello` /
/// `Welcome` handshake and folded into the auth mac: the client fails the connection
/// with a legible, both-numbers error on a mismatch. It changes ONLY for a change an old peer cannot
/// DECODE; an additive verb, field or advertisement is negotiated by a `Welcome.features` string
/// instead (decision 0112).
///
/// - `1` — wave 1: the data surface (`stream`, `rpc` and `feed` classes; scopes `Read` and `Write`).
/// - `2` — the phase-4 renames the owner ruled (P4.5c): `GwStatus` is `GatewayStatus`, `ReconProgress`
///   is `ReconcileProgress`, every renamed field key, and the `stream` class's frames (`vike_market_stream::wire`, which open through this
///   handshake and carry no version of their own). A peer from before them could not decode those
///   frames, so an old peer is refused at the handshake, naming both numbers (`opener.rs`'s two
///   `..._from_before_the_phase_four_renames_is_refused_at_the_handshake` tests).
pub const GATEWAYHUB_PROTO_VERSION: u32 = 2;

/// The gatewayhub's domain separator (20 bytes, NUL-terminated) — its OWN, disjoint from the
/// datahub's and the tradehub's, so a mac captured off one service cannot be replayed at another
/// even if an operator reuses key bytes (`vike_hub_protocol::auth`'s `domain_separators_are_disjoint`
/// is the test that says so for the pair it knows; this one is held by
/// `crates/vike-gatewayhub-client/src/proto_tests.rs`).
pub const GATEWAYHUB_DOMAIN: Domain = Domain::new(b"vike-gatewayhub-auth\0");

// ---- Constants both ends read --------------------------------------------------------------------

/// The longest frame a post-auth CLIENT sends: every [`Request`], [`StreamUp`](vike_market_stream::wire::StreamUp) — a symbol, a venue, a
/// handful of strings. 64 KiB, the pre-auth cap's size, because nothing a client sends here is
/// larger than a few hundred bytes and a daemon that read 64 MiB of it would let any authenticated
/// peer make it allocate that much per connection.
pub const GATEWAY_UP_FRAME_CEILING_BYTES: u32 = 64 * 1024;

/// The longest frame a post-auth SERVER sends: a [`StreamDown`](vike_market_stream::wire::StreamDown) frame, any single reply, one
/// [`Response::FetchPage`]. 8 MiB — twice [`FETCH_PAGE_CEILING_BYTES`], the page payload's budget,
/// so the envelope, one over-sized row and a full-depth `Book` all fit with room.
pub const GATEWAY_DOWN_FRAME_CEILING_BYTES: u32 = 8 * 1024 * 1024;

/// The target payload size of one [`Response::FetchPage`]: 4 MiB, `MAX_FRAME_LEN / 16`. A bar series
/// can reach ~64 MiB (`LOAD_BARS_CEILING` is 520,223 rows at ~129 B) and a dukascopy day of ticks is
/// the same order, so no fetch reply is one frame; [`pages_of`] splits and [`FetchAssembler`]
/// reassembles.
pub const FETCH_PAGE_CEILING_BYTES: usize = (MAX_FRAME_LEN as usize) / 16;

/// How often a server owes a running fetch a frame: the immediate [`FetchProgress`] on acceptance,
/// then at least one frame (progress, a page, or the terminal frame) every this long. EMITTED BY A
/// TIMER, not by the page loop: a single venue call can block 30 s, and the stall bound is read
/// against this cadence, not against the page rate.
pub const FETCH_PROGRESS_EVERY: Duration = Duration::from_secs(5);

/// The FLOOR of the client's idle bound on a fetch: `max(FETCH_STALL_FLOOR, 3 × next_in_ms)`, where
/// `next_in_ms` is the cadence the server states in every [`FetchProgress`]. A fetch that keeps
/// emitting frames runs as long as the in-process fetch would (R8: a remote fetch never times out
/// SOONER than the in-process one); only a wedged or vanished server is called dead.
pub const FETCH_STALL_FLOOR: Duration = Duration::from_secs(30);

/// How long ONE positional reply of a server-bounded verb ([`ReplyWait::Bounded`]) may stay silent:
/// five minutes. The server bounds the work (a catalog describe is a table lookup, a chain a few REST
/// calls at the bridges' own 30 s HTTP timeout), so a finite bound is safe; it equals the datahub
/// client's `REPLY_IDLE_TIMEOUT` by choice, not coupling.
pub const REPLY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a pure LOOKUP reply ([`ReplyWait::Lookup`]) may stay silent: fifteen seconds. A table
/// read, a counter snapshot, an open acknowledgement — nothing the daemon does there touches a venue,
/// so a wedged daemon is called dead in seconds instead of [`REPLY_IDLE_TIMEOUT`]'s five minutes
/// (the history-channels read asks one of these per request).
pub const LOOKUP_REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// The most a fetch's stated `next_in_ms` is believed: one minute. The client's idle bound is
/// `max(floor, 3 × next_in_ms)`, so an unbounded promise would switch the stall detector off; a
/// cadence over this is clamped DOWN, the way a stated heartbeat is clamped into its range.
pub const FETCH_NEXT_IN_CEILING: Duration = Duration::from_secs(60);

/// The default stream heartbeat period: the server writes a [`StreamDown::Heartbeat`](vike_market_stream::wire::StreamDown::Heartbeat) after this
/// much silence. `vike-config` restates the range as `config.gateway_stream_heartbeat_ms` (wave
/// 2/3); the client never assumes the default — it reads the period from [`Response::DataOpened`].
pub const GATEWAY_STREAM_HEARTBEAT: Duration = Duration::from_secs(15);

/// The shortest heartbeat period a server may state (one second): below it a quiet stream would
/// write continuously for no liveness gain. A stated period under this is clamped UP by the client.
pub const GATEWAY_STREAM_HEARTBEAT_MIN: Duration = Duration::from_secs(1);

/// The longest heartbeat period a server may state (a minute): above it the client's three-period
/// deadline stops being a liveness signal. A stated period over this is clamped DOWN by the client.
pub const GATEWAY_STREAM_HEARTBEAT_MAX: Duration = Duration::from_secs(60);

/// The client's read-deadline FLOOR on a stream: three default heartbeats. ⚠ A floor, not the
/// contract: the deadline is `max(GATEWAY_STREAM_READ_FLOOR, 3 × DataOpened.heartbeat_ms)`.
pub const GATEWAY_STREAM_READ_FLOOR: Duration =
    Duration::from_secs(3 * GATEWAY_STREAM_HEARTBEAT.as_secs());

/// How long a `subscribe_*` waits for its [`StreamDown::SubscribeOk`](vike_market_stream::wire::StreamDown::SubscribeOk) / [`StreamDown::SubscribeError`](vike_market_stream::wire::StreamDown::SubscribeError). The server
/// acks after the in-process `subscribe_*` it forwards returned, which is milliseconds; this bounds
/// a half-open socket, and the broker holds its per-venue mutex for at most this long.
pub const GATEWAY_SUB_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// **How long a credentialed stream's synchronous handshake may take inside its `DataOpen`** (ctrader's
/// `connect_and_auth` runs in the open, `crates/vike-gatewayhub/src/data/credentialed.rs`). The first link of
/// the refusal-timing chain `tradehub > datahub > gatewayhub`:
///
/// * the gatewayhub gives its handshake [`GATEWAY_CREDENTIALED_OPEN_BUDGET`] (12 s) and then answers the open with
///   its own words;
/// * the datahub's client waits [`LOOKUP_REPLY_TIMEOUT`] (15 s) for that `DataOpened`, so the words arrive and
///   are not a bare "reply timed out" (which would also make the datahub redial, and restart the handshake);
/// * the datahub's first subscribe on a credentialed stream waits [`GATEWAY_CREDENTIALED_FIRST_DIAL_WAIT`] (13 s) for the
///   dial in flight, which includes this open, plus [`GATEWAY_SUB_REPLY_TIMEOUT`] (10 s) for the gatewayhub's ack: 23 s;
/// * the tradehub waits `vike_datahub_client::strat::STRATEGY_SUB_REPLY` (25 s) for the datahub's ack of a subscribe,
///   so the words of every refusal below it arrive (a keyless stream: 2 s + 10 s = 12 s).
///
/// A venue whose handshake takes longer than the budget is a refusal in words, not a silent livelock. A
/// handshake step is a well venue's sub-second answer; the bridge's own per-step wait is 15 s.
pub const GATEWAY_CREDENTIALED_OPEN_BUDGET: Duration = Duration::from_secs(12);

/// **How long the datahub's FIRST subscribe on a credentialed stream waits for the dial already in flight**
/// ([`vike_market_stream::opener::StreamOpener::first_dial_wait`]): the open's handshake budget
/// ([`GATEWAY_CREDENTIALED_OPEN_BUDGET`], 12 s) plus one second for the connect, the authentication and the open
/// reply that frame it. At the engine's default (2 s) a slow but healthy ctrader handshake read as a transient
/// `connecting` and the tradehub's start was refused; the next start found the connected client. A keyless
/// stream keeps the 2 s: its dial is a connect and a lookup.
pub const GATEWAY_CREDENTIALED_FIRST_DIAL_WAIT: Duration = Duration::from_secs(13);

/// The write timeout of a stream socket for its life: a client frame is tens of bytes, so a write
/// blocked this long is a peer that has stopped reading.
pub const GATEWAY_STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// SERVER POLICY, a start value shared so the status report and the tests name one number: how many
/// authenticated connections (threads) the daemon serves at once. Excess is accepted by the kernel
/// and dropped.
pub const GATEWAY_MAX_CONNECTIONS: usize = 128;

/// SERVER POLICY, a start value: how many `stream` sessions are open at once. The datahub opens one
/// per broker client key: the market-data and recorder holders (at most `Shared` venues once and
/// `PerHolder` venues twice, so up to the 14-venue roster), plus one per strategy-lane dial (a
/// tradehub dials once per venue and network, and once per Polymarket token: 14 roster venues, the
/// Hyperliquid testnet and ten more tokens make 25), plus one pump session for each of the four CEX
/// pump venues. 14 + 25 + 4 = 43, and 48 leaves a reconnect overlap's room. (The datahub's own cap on
/// strategy dials, `STRATEGY_LANE_MAX_STREAM_CONNECTIONS`, is 40; 32 refused the dials that fit it.)
pub const GATEWAY_MAX_STREAM_SESSIONS: usize = 48;

/// SERVER POLICY, a start value: how many subscriptions one stream session may hold. A recorder glob
/// over a venue's perps is hundreds of keys (binance lists ~500).
pub const GATEWAY_MAX_SUBS_PER_SESSION: usize = 2048;

/// SERVER POLICY (the 0052 bound, now client-named and therefore capped here): the most
/// `markprice.options` indexes one [`OptionsFeedSpec::MarkPrice`] may name. The datahub names three.
pub const GATEWAY_OPTIONS_MAX_INDEXES: usize = 8;

/// SERVER POLICY (the 0052 bound): the most instruments one [`OptionsFeedSpec::Ticker`] may name.
/// The datahub's focus set is two underlyings × (10 strikes each side) × (call and put).
pub const GATEWAY_OPTIONS_MAX_INSTRUMENTS: usize = 256;

/// SERVER POLICY, a start value: the longest index or instrument name an [`OptionsFeedSpec`] may carry,
/// in bytes. A real Deribit name is under 32 (`BTC_USDC-27JUL26-58000-C`); the bound stops a name that
/// is a paragraph from becoming a channel string the venue is asked to subscribe.
pub const GATEWAY_OPTIONS_MAX_NAME_BYTES: usize = 64;

/// SERVER POLICY, a start value: the widest window one `Fetch { Ticks }` may name, in milliseconds
/// (31 days). The lane makes ONE real CDN request per hour file of the window, so a window the client
/// names without a bound is a request count the client chooses; with this bound the most one fetch
/// costs is 745 hour files (31 days, unaligned). The datahub's tick backfill cuts its window into
/// whole-bar chunks of a day or one bar, so a legitimate request is far under it. Klines and funding
/// windows are NOT bounded by a daemon constant: see [`required_scope`].
pub const GATEWAY_TICKS_MAX_SPAN_MS: i64 = 31 * vike_model::MS_PER_DAY;

// ---- The `trade` class constants -------------------------------------------------------------------

/// The longest frame a post-auth client sends on an open `trade` lease: 256 KiB, a `SubmitBatch` (a few
/// hundred orders at ~700 B). Larger than [`GATEWAY_UP_FRAME_CEILING_BYTES`] because that ceiling guards the
/// pre-lease positional verbs, whose largest is a few hundred bytes; a lease writes orders. A batch over
/// it is split by the client before its first byte.
pub const GATEWAY_TRADE_UP_FRAME_CEILING_BYTES: u32 = 256 * 1024;

/// SERVER POLICY, a start value shared so both ends name one number: the largest [`TradeDown::Event`] frame
/// a daemon sends, 64 KiB. (The client reads under [`GATEWAY_DOWN_FRAME_CEILING_BYTES`], which a reconcile page
/// needs; an event is one `Event` and is far under this.)
pub const GATEWAY_TRADE_EVENT_FRAME_CEILING_BYTES: u32 = 64 * 1024;

/// The default `trade` lease heartbeat period: BOTH ends beat. The client pings after this much silence
/// of its own, the server writes [`TradeDown::Heartbeat`] after this much of its own, and the server's
/// peer deadline is three periods - a server writer cannot detect a silent peer (writes succeed into the
/// kernel buffer), so a ghost would otherwise hold the lease. The client never assumes the default: it
/// reads the period from [`TradeOpened::heartbeat_ms`].
pub const GATEWAY_TRADE_HEARTBEAT: Duration = Duration::from_secs(2);

/// The shortest heartbeat period a server may state: below it a quiet lease writes continuously for no
/// liveness gain. A stated period under this is clamped UP by the client.
pub const GATEWAY_TRADE_HEARTBEAT_MIN: Duration = Duration::from_millis(500);

/// The longest heartbeat period a server may state: above it the three-period deadline stops being a
/// signal a dead link is known before the stuck-order watchdog (60 s) and the dead-man (120 s) act. A
/// stated period over this is clamped DOWN by the client.
pub const GATEWAY_TRADE_HEARTBEAT_MAX: Duration = Duration::from_secs(15);

/// The client's read-deadline FLOOR on a lease: three default heartbeats. ⚠ A floor, not the contract:
/// the deadline is `max(GATEWAY_TRADE_READ_FLOOR, 3 x TradeOpened.heartbeat_ms)`.
pub const GATEWAY_TRADE_READ_FLOOR: Duration =
    Duration::from_secs(3 * GATEWAY_TRADE_HEARTBEAT.as_secs());

/// The write timeout of a lease socket for its life: a client frame is at most
/// [`GATEWAY_TRADE_UP_FRAME_CEILING_BYTES`], so a write blocked this long is a peer that stopped reading.
pub const GATEWAY_TRADE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

// Compile-time RANGES: a deliberate tweak stays free while "the bound was effectively removed" does
// not compile (the `vike_hub_protocol::limits` idiom).
const _: () = assert!(
    GATEWAY_UP_FRAME_CEILING_BYTES > 0 && GATEWAY_UP_FRAME_CEILING_BYTES <= 1024 * 1024,
    "GW_UP_FRAME_CEILING_BYTES must stay far under MAX_FRAME_LEN: nothing a client sends here is \
     large, and an authenticated peer must not be able to name a large allocation"
);
const _: () = assert!(
    GATEWAY_DOWN_FRAME_CEILING_BYTES <= MAX_FRAME_LEN
        && (GATEWAY_DOWN_FRAME_CEILING_BYTES as usize) >= 2 * FETCH_PAGE_CEILING_BYTES,
    "a server frame must fit MAX_FRAME_LEN, and one page's payload budget must leave the frame \
     room for its envelope and one row that alone exceeds the budget"
);
const _: () = assert!(
    FETCH_STALL_FLOOR.as_millis() >= 3 * FETCH_PROGRESS_EVERY.as_millis(),
    "the stall floor must allow at least THREE missed progress beats: one missed beat is a loaded \
     box, not a dead server"
);
const _: () = assert!(
    GATEWAY_STREAM_HEARTBEAT_MIN.as_millis() > 0
        && GATEWAY_STREAM_HEARTBEAT_MIN.as_millis() <= GATEWAY_STREAM_HEARTBEAT.as_millis()
        && GATEWAY_STREAM_HEARTBEAT.as_millis() <= GATEWAY_STREAM_HEARTBEAT_MAX.as_millis()
        && GATEWAY_STREAM_HEARTBEAT_MAX.as_secs() <= 60,
    "the default heartbeat must sit inside the range a server may state, and the longest period \
     must stay short: the client's deadline is three periods"
);
const _: () = assert!(
    GATEWAY_STREAM_READ_FLOOR.as_millis() >= 2 * GATEWAY_STREAM_HEARTBEAT.as_millis(),
    "the read floor must allow at least TWO missed heartbeats"
);
const _: () = assert!(
    GATEWAY_SUB_REPLY_TIMEOUT.as_secs() > 0 && GATEWAY_SUB_REPLY_TIMEOUT.as_secs() <= 60,
    "the subscribe reply bound holds the broker's per-venue mutex: short, and never zero"
);
const _: () = assert!(
    GATEWAY_CREDENTIALED_FIRST_DIAL_WAIT.as_millis() > GATEWAY_CREDENTIALED_OPEN_BUDGET.as_millis()
        && GATEWAY_CREDENTIALED_FIRST_DIAL_WAIT.as_millis() <= LOOKUP_REPLY_TIMEOUT.as_millis(),
    "the first-dial wait of a credentialed stream must cover the open's handshake budget and never exceed \
     the wait for the open reply (past that the dial has failed in words anyway)"
);
const _: () = assert!(
    GATEWAY_CREDENTIALED_OPEN_BUDGET.as_secs() < LOOKUP_REPLY_TIMEOUT.as_secs(),
    "a credentialed open's handshake must be bounded BELOW the wait the datahub's client makes for the \
     open reply, or the gatewayhub's words are lost to a bare timeout"
);
const _: () = assert!(
    LOOKUP_REPLY_TIMEOUT.as_secs() > 0
        && LOOKUP_REPLY_TIMEOUT.as_secs() < REPLY_IDLE_TIMEOUT.as_secs(),
    "a lookup reply is bounded tighter than a REST-backed one, and never by zero"
);
const _: () = assert!(
    FETCH_NEXT_IN_CEILING.as_millis() >= FETCH_PROGRESS_EVERY.as_millis(),
    "the stated-cadence ceiling must admit the cadence the protocol itself specifies"
);
const _: () = assert!(
    REPLY_IDLE_TIMEOUT.as_secs() >= 60 && REPLY_IDLE_TIMEOUT.as_secs() <= 3600,
    "REPLY_IDLE_TIMEOUT must stay above the longest legitimate silence of a server-bounded verb \
     and finite enough that a dead link is reported within the hour"
);

const _: () = assert!(
    GATEWAY_TRADE_HEARTBEAT_MIN.as_millis() > 0
        && GATEWAY_TRADE_HEARTBEAT_MIN.as_millis() <= GATEWAY_TRADE_HEARTBEAT.as_millis()
        && GATEWAY_TRADE_HEARTBEAT.as_millis() <= GATEWAY_TRADE_HEARTBEAT_MAX.as_millis(),
    "the default trade heartbeat must sit inside the range a server may state"
);
const _: () = assert!(
    3 * GATEWAY_TRADE_HEARTBEAT_MAX.as_secs() < 60,
    "the longest trade deadline (three of the longest periods) must stay below the stuck-order \
     watchdog's 60 s, so a dead link is known before the watchdog or the 120 s dead-man acts"
);
const _: () = assert!(
    GATEWAY_TRADE_READ_FLOOR.as_millis() >= 2 * GATEWAY_TRADE_HEARTBEAT.as_millis(),
    "the trade read floor must allow at least TWO missed heartbeats"
);
const _: () = assert!(
    GATEWAY_TRADE_UP_FRAME_CEILING_BYTES <= 1024 * 1024
        && GATEWAY_TRADE_UP_FRAME_CEILING_BYTES >= GATEWAY_UP_FRAME_CEILING_BYTES
        && GATEWAY_TRADE_EVENT_FRAME_CEILING_BYTES <= GATEWAY_DOWN_FRAME_CEILING_BYTES,
    "a lease writes orders: its up ceiling is not below the positional one, stays far under \
     MAX_FRAME_LEN, and an event frame fits the down ceiling"
);

// ---- Requests ------------------------------------------------------------------------------------

/// A client-to-server request on the handshake, `rpc` and class-opening frames.
///
/// Every verb is classified in [`required_scope`] (EXHAUSTIVE, no `_` arm), [`surface_of`],
/// [`reply_wait`] and [`request_kind`], so a new verb fails to COMPILE until somebody says which
/// scope may send it, which socket class it opens and who bounds its reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Liveness probe — answered by [`Response::Pong`].
    Ping,
    /// Open a `stream` session for one KEYLESS venue (a `Welcome` entry
    /// [`market_data_venue_feature`](vike_hub_protocol::features::market_data_venue_feature) names), or for a
    /// CREDENTIALED one ([`cs_venue_feature`]: `Scope::Write` only, the daemon logs in as the account the
    /// strategy trades).
    /// Answered by [`Response::DataOpened`], after which the socket speaks [`StreamUp`](vike_market_stream::wire::StreamUp) /
    /// [`StreamDown`](vike_market_stream::wire::StreamDown) only; a refusal is [`Response::Error`] and the socket stays positional.
    ///
    /// One session is one venue client at the daemon: the datahub opens one per broker client key,
    /// so nothing one session does (an unsubscribe, a reconnect storm, a slow reader) can gap another.
    DataOpen {
        /// A `vike_model::VENUES` slug.
        venue: String,
        /// The network the venue's client is built for, for the one venue whose feed follows the
        /// account tier (Hyperliquid): a `vike_model::VenueNetwork::word`. `None` is
        /// the main network (the field is then absent from the frame, so every old peer's bytes are
        /// unchanged), and is the only value any other venue takes: the daemon refuses a network on a
        /// venue that has none rather than ignore it. Gated by [`FEATURE_GATEWAY_STREAM_NETWORK`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<String>,
    },
    /// Open one `feed` socket: the Deribit options streams (decision 0052's bound, client-named and
    /// therefore capped by the server). Answered by [`Response::OptionsFeedOpened`], after which the
    /// server writes [`OptionsFeedDown`] and the client says nothing.
    OptionsFeedOpen {
        /// Which feed, and its focus set.
        feed: OptionsFeedSpec,
    },
    /// A PAGED fetch on a keyless lane. **The scope follows the lane** ([`required_scope`]):
    /// [`FetchWhat::Klines`], [`FetchWhat::Funding`] and [`FetchWhat::Ticks`] walk a window the client
    /// names and are `Scope::Write` (the datahub's `Backfill` was); the catalog and option-index
    /// lanes are `Scope::Read`. The reply is a [`Response::FetchProgress`]
    /// immediately, then progress and [`Response::FetchPage`] frames, then exactly ONE terminal frame:
    /// [`Response::FetchDone`], [`Response::FetchFailed`] or [`Response::FetchCancelled`].
    ///
    /// A request the daemon REFUSES without asking the venue (a venue it does not serve, a symbol the
    /// source cannot express) is answered `FetchFailed { failure: Refused(..) }`; [`Response::Error`] is
    /// for a malformed request or a verb it cannot serve at all, and the client treats it as retryable.
    Fetch {
        /// The client's correlation id for this fetch, echoed on every reply frame; unique per
        /// connection.
        rid: u64,
        /// What to fetch.
        what: FetchWhat,
    },
    /// The same family on a CREDENTIALED lane — the daemon spends a credential IT holds (the OANDA
    /// practice token), `Scope::Write`. Only [`FetchWhat::Klines`] for a `credentialed_data_venue=` venue is served;
    /// anything else is [`Response::Error`].
    ///
    /// ⚠ A separate VERB rather than a flag on [`Request::Fetch`]: the scope is visible per variant in
    /// an exhaustive `match`, and a `Scope::Read` client (a desktop) must never be able to make the
    /// daemon authenticate as its operator (decision 0062's rule 3, carried over the hop).
    FetchCredentialed {
        /// See [`Request::Fetch`].
        rid: u64,
        /// See [`Request::Fetch`].
        what: FetchWhat,
    },
    /// Abandon the fetch `rid` that THIS connection started. No reply is owed (the daemon MAY write
    /// [`Response::FetchCancelled`]); the client drops the connection right after, and a dropped
    /// connection cancels every fetch it started, so a lost cancel costs only time. Cancellation is
    /// cooperative: the daemon stops at its next venue page or hour-file boundary.
    FetchCancel {
        /// The `rid` of the running fetch.
        rid: u64,
    },
    /// One venue's catalog traits — its asset classes and population mode, cached by the client.
    /// Answered by [`Response::CatalogDescribed`]. Only a venue `vike_catalog::catalog_availability`
    /// calls public is served.
    CatalogDescribe {
        /// A `vike_model::VENUES` slug.
        venue: String,
    },
    /// One call to the persistent Deribit options provider (decision D7: the datahub keeps the poll
    /// thread and the folds, the daemon serves the fetches). Answered by [`Response::OptionsReplied`]
    /// or [`Response::Error`].
    OptionsRest {
        /// Which provider call.
        call: OptionsCall,
    },
    /// Resolve a rotating recorder family (polymarket) to the symbols that should be streaming at
    /// `now_ms`. Answered by [`Response::FamilyResolved`] or [`Response::Error`]; an error means
    /// UNKNOWN, never empty — an empty set would unsubscribe live streams.
    ResolveFamily {
        /// A `vike_model::VENUES` slug (`polymarket`).
        venue: String,
        /// The family name, e.g. `btc-updown-5m`.
        family: String,
        /// The instant the symbols are wanted at, epoch-ms (the caller's clock decides the window).
        now_ms: i64,
    },
    /// Whether the credential a venue's credentialed lane reads is STORED — a presence word, never a
    /// value. Answered by [`Response::PresenceIs`]. Called once per history-channels request,
    /// uncached, so a key stored or removed after start is what the next answer says.
    CredentialPresence {
        /// A `vike_model::VENUES` slug.
        venue: String,
    },
    /// The daemon's status: names and counters, never values. Answered by [`Response::StatusIs`].
    GatewayStatus,
    /// Every venue's static arming facts and every account's arming row, computed at the daemon from
    /// its own account table and credential store: words, never a credential. `Scope::Trade`. Answered
    /// by [`Response::ArmingTable`]; gated by [`FEATURE_GATEWAY_ARMING`].
    ArmingQuery,
    /// The preflight legs of `venues` (clock, credential, identity, network), measured at the daemon.
    /// `Scope::Trade`. Answered by [`Response::PreflightLegs`]; gated by [`FEATURE_GATEWAY_PREFLIGHT`].
    ///
    /// `step` says WHICH question: the tradehub's `run_preflight` owns the schedule (a clock read is
    /// repeated while its band straddles a threshold, two budgets interleave between reads), so it asks
    /// ONE read at a time and the daemon only measures ([`PreflightStep`]). A frame without `step` is the
    /// whole batch, one measurement per leg, as before.
    PreflightQuery {
        /// A `vike_model::VENUES` slug each.
        venues: Vec<String>,
        /// Which question; absent = [`PreflightStep::Batch`].
        #[serde(default)]
        step: PreflightStep,
    },
    /// **The operator changed the credentials of `account` in the store**: every session of that account the daemon
    /// runs is reconnected on what the store holds NOW (its adapter torn down without a cancel, a new epoch, the
    /// lease ended so its client reconnects, re-teaches and starts the adapter again), an account that inherits its
    /// keys (a derived lane twin) included, and every credentialed price stream that logged in with them is ended so
    /// its client reopens it. No other account is touched. It carries the row id and nothing else: the daemon re-reads the store itself, so no value ever
    /// crosses (decision 0287). `Scope::Write` (the datahub relays it for an operator's credential verb). Answered
    /// by [`Response::CredentialsReloaded`]; gated by [`FEATURE_GATEWAY_CREDENTIALS_CHANGED`].
    CredentialsChanged {
        /// The `account` row id whose credentials changed.
        account: i64,
    },
    /// **Open a `trade` lease** for one `(venue, account)`; the first frame of the class, `Scope::Trade`.
    /// Answered by [`Response::TradeOpened`], after which the socket speaks [`TradeUp`] / [`TradeDown`]
    /// only; a refusal is [`Response::LeaseHeld`], [`Response::TradeRefused`] or [`Response::Error`] and
    /// the socket stays positional. Gated by [`FEATURE_GATEWAY_TRADE`] and [`FEATURE_GATEWAY_LEASE`], the venue by
    /// its [`trade_venue_feature`].
    TradeOpen {
        /// The caller's id for this lease, stable across its reconnects (a process): the daemon keys
        /// `received_through_command_sequence` (with the account) and the explicit takeover on it.
        client_id: String,
        /// A `vike_model::VENUES` slug.
        venue: String,
        /// The account label's text; `None` is the unlabelled default account.
        #[serde(default)]
        account: Option<String>,
        /// The tier the caller believes the account runs at. The daemon re-derives it from its own
        /// account table and refuses a mismatch with its own `ArmingBlock` word.
        tier: AccountTier,
        /// The last event the caller folded, to replay from. `None` = a fresh open (gated by
        /// [`FEATURE_GATEWAY_TRADE_CURSOR`]).
        #[serde(default)]
        resume: Option<EventRingCursor>,
        /// Take the lease over from its holder (explicit and fenced) instead of being refused.
        #[serde(default)]
        takeover: bool,
        /// The `account` row id the caller's engine started on (the store links an account by NUMBER). An open
        /// whose `account` label names no row any more (the operator renamed the account while it ran) is judged
        /// by THIS row and its adapter starts on the row's keys. Absent (an older client, a derived lane twin) =
        /// judged by the label alone, exactly as before the field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row: Option<i64>,
    },
}

/// What a [`Request::Fetch`] / [`Request::FetchCredentialed`] asks for. Every lane replies through
/// the same progress / page / terminal sequence, so paging, cancellation and the stall bound are
/// written once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FetchWhat {
    /// `KlineSource::fetch(symbol, interval, start_ms, end_ms)` of `venue`'s source → [`FetchRows::Bars`].
    Klines {
        /// The source's venue.
        venue: String,
        /// The unified store symbol, as `KlineSource::fetch` takes it.
        symbol: String,
        /// The bar interval.
        interval: String,
        /// Window start, epoch-ms inclusive.
        start_ms: i64,
        /// Window end, epoch-ms inclusive.
        end_ms: i64,
    },
    /// `FundingRateSource::fetch(symbol, start_ms, end_ms)` → [`FetchRows::Funding`].
    Funding {
        /// The source's venue.
        venue: String,
        /// The symbol.
        symbol: String,
        /// Window start, epoch-ms inclusive.
        start_ms: i64,
        /// Window end, epoch-ms inclusive.
        end_ms: i64,
    },
    /// The dukascopy tick range (`vike_dukascopy::DukascopyTicks::fetch_quotes_range`) → [`FetchRows::Quotes`]. The
    /// datahub's `backfill_quotes_then_bars` stays where the store is and takes this as its fetch
    /// closure. Keyless; gated by [`FEATURE_GATEWAY_TICKS`].
    Ticks {
        /// The source's venue (`dukascopy`).
        venue: String,
        /// The symbol.
        symbol: String,
        /// Window start, epoch-ms inclusive.
        start_ms: i64,
        /// Window end, epoch-ms inclusive.
        end_ms: i64,
    },
    /// One venue's whole instrument universe (`CatalogProvider::list_instruments`) →
    /// [`FetchRows::Instruments`]. The CAP is the caller's (`CATALOG_MAX_INSTRUMENTS` is the
    /// datahub's); the daemon sends the provider's full answer. Gated by [`FEATURE_GATEWAY_CATALOG`].
    CatalogList {
        /// A `vike_model::VENUES` slug.
        venue: String,
    },
    /// `CatalogProvider::search_remote(query, filter)` for a QUERY-BACKED venue →
    /// [`FetchRows::Instruments`]. Gated by [`FEATURE_GATEWAY_CATALOG`].
    CatalogSearch {
        /// A `vike_model::VENUES` slug.
        venue: String,
        /// The picker's query text.
        query: String,
        /// The picker's tab filter (`None` = the All tab).
        #[serde(default)]
        tab: Option<PickerTab>,
        /// The picker's venue filter (`None` = every venue).
        #[serde(default)]
        filter_venue: Option<String>,
    },
    /// Deribit's `public/get_instruments` for `currency=any` and one kind → [`FetchRows::Instruments`].
    /// The datahub asks twice (options, futures) and folds the union into its option index; this
    /// fails as a whole when the venue call fails, so the caller keeps its last-good index.
    OptionIndex {
        /// Which instrument kind.
        kind: OptionIndexKind,
    },
    /// The options venue's public TRADE log of `currency` over a client-named window
    /// (`vike_data::source::OptionTradeSource::fetch`) → [`FetchRows::OptionTrades`]. It names no venue: it is
    /// the options surface's, as [`FetchWhat::OptionIndex`] is. Keyless; gated by
    /// [`FEATURE_GATEWAY_OPTION_TRADES`]. The datahub asks one settled UTC day per fetch.
    OptionTrades {
        /// The venue's currency word (`BTC`, `ETH`, `USDC`).
        currency: String,
        /// Window start, epoch-ms inclusive.
        start_ms: i64,
        /// Window end, epoch-ms inclusive.
        end_ms: i64,
    },
    /// A prediction-market venue's archived RULES texts: every CLOSED market whose scheduled end falls
    /// in a client-named window (`vike_data::source::MarketRulesSource::closed_ending`) →
    /// [`FetchRows::Texts`]. Keyless; gated by [`FEATURE_GATEWAY_MARKET_RULES`]. The datahub asks one
    /// settled UTC day per fetch.
    ClosedMarketRules {
        /// The venue (`polymarket`).
        venue: String,
        /// Window start, epoch-ms inclusive.
        start_ms: i64,
        /// Window end, epoch-ms inclusive.
        end_ms: i64,
    },
    /// The RULES text of the open market that lists one outcome `token`
    /// (`vike_data::source::MarketRulesSource::of_token`) → [`FetchRows::Texts`] (no row: no open market
    /// lists it). Keyless; gated by [`FEATURE_GATEWAY_MARKET_RULES`].
    MarketRulesOfToken {
        /// The venue (`polymarket`).
        venue: String,
        /// The outcome token id.
        token: String,
    },
    /// A prediction-market venue's archived RULES texts of ONE recurring series: every CLOSED market of
    /// `series` whose window starts in a client-named window
    /// (`vike_data::source::MarketRulesSource::closed_of_series`) → [`FetchRows::Texts`]. Keyless; gated by
    /// [`FEATURE_GATEWAY_MARKET_RULES_SERIES`]. The datahub asks one settled UTC day per fetch.
    SeriesMarketRules {
        /// The venue (`polymarket`).
        venue: String,
        /// The series, its markets' slug stem (`btc-updown-5m`).
        series: String,
        /// Window start, epoch-ms inclusive.
        start_ms: i64,
        /// Window end, epoch-ms inclusive.
        end_ms: i64,
    },
}

impl FetchWhat {
    /// The lane's name, for a log line or a refusal — never its payload.
    pub fn lane(&self) -> &'static str {
        match self {
            FetchWhat::Klines { .. } => "klines",
            FetchWhat::Funding { .. } => "funding",
            FetchWhat::Ticks { .. } => "ticks",
            FetchWhat::CatalogList { .. } => "catalog_list",
            FetchWhat::CatalogSearch { .. } => "catalog_search",
            FetchWhat::OptionIndex { .. } => "option_index",
            FetchWhat::OptionTrades { .. } => "option_trades",
            FetchWhat::ClosedMarketRules { .. } => "closed_market_rules",
            FetchWhat::MarketRulesOfToken { .. } => "market_rules_of_token",
            FetchWhat::SeriesMarketRules { .. } => "series_market_rules",
        }
    }

    /// The feature string a daemon must advertise for this lane, so a client refuses locally and
    /// names it. EXHAUSTIVE: a new lane fails to compile until it is negotiated.
    pub fn feature(&self) -> &'static str {
        match self {
            FetchWhat::Klines { .. } | FetchWhat::Funding { .. } => FEATURE_GATEWAY_FETCH,
            FetchWhat::Ticks { .. } => FEATURE_GATEWAY_TICKS,
            FetchWhat::CatalogList { .. } | FetchWhat::CatalogSearch { .. } => {
                FEATURE_GATEWAY_CATALOG
            }
            FetchWhat::OptionIndex { .. } => FEATURE_GATEWAY_OPTIONS,
            FetchWhat::OptionTrades { .. } => FEATURE_GATEWAY_OPTION_TRADES,
            FetchWhat::ClosedMarketRules { .. } | FetchWhat::MarketRulesOfToken { .. } => {
                FEATURE_GATEWAY_MARKET_RULES
            }
            FetchWhat::SeriesMarketRules { .. } => FEATURE_GATEWAY_MARKET_RULES_SERIES,
        }
    }
}

/// Which Deribit instrument kind an `OptionIndex` fetch lists. A closed pair, mapped to the venue's
/// own `kind=` words by the daemon, so a client never spells a venue word on this wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionIndexKind {
    /// Option contracts.
    Option,
    /// Dated futures (the index's forward legs).
    Future,
}

/// One provider call of [`Request::OptionsRest`], mirroring `DeribitOptionsProvider`'s methods one
/// for one. The daemon keeps ONE provider (and its 5 s book cache and its listing) behind a lock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OptionsCall {
    /// `list_underlyings()` — the served set in the provider's own order.
    Underlyings,
    /// `list_expiries(underlying)`.
    Expiries {
        /// An underlying from [`OptionsCall::Underlyings`].
        underlying: String,
    },
    /// `fetch_chain(underlying, expiry, strike_window, r)`: one expiry's chain, greeks enriched.
    Chain {
        /// An underlying from [`OptionsCall::Underlyings`].
        underlying: String,
        /// The expiry's ISO date from [`OptionsCall::Expiries`].
        expiry: String,
        /// `Some(n)` windows the chain to ±n strikes around spot; `None` is the FULL chain (what the
        /// datahub's chain recorder needs before it applies its own display window).
        strike_window: Option<u32>,
        /// The risk-free rate the greeks are computed with.
        r: f64,
    },
    /// The venue's option LISTING: which venue's options the daemon serves, and every underlying it
    /// lists with its `markprice.options` index, read from the venue's own instrument rows. It is
    /// [`OptionsCall::Underlyings`] with the indexes and the venue added, as a call of its own because
    /// that reply is a bare list an older client decodes and cannot grow a field. `refresh` asks the
    /// daemon to read the listing from the venue NOW; it answers the listing it holds instead when it
    /// read one within the last minute, so a client cannot make the box ask faster than that.
    /// Negotiated by [`FEATURE_GATEWAY_OPTIONS_LISTING`].
    Listing {
        /// Read it from the venue now rather than answer the listing held.
        refresh: bool,
    },
}

/// The answer to one [`OptionsCall`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OptionsReply {
    /// Answer to [`OptionsCall::Underlyings`].
    Underlyings(Vec<String>),
    /// Answer to [`OptionsCall::Expiries`], ascending.
    Expiries(Vec<OptionChainExpiry>),
    /// Answer to [`OptionsCall::Chain`]. BOXED: a chain is hundreds of rows, and `Response` is matched
    /// on every positional path.
    Chain(Box<OptionChain>),
    /// Answer to [`OptionsCall::Listing`].
    Listing(OptionsListing),
}

/// The venue's option listing ([`OptionsCall::Listing`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionsListing {
    /// The venue slug whose options the daemon serves (`deribit`): the venue the datahub serves them
    /// under.
    pub venue: String,
    /// Every underlying the venue lists options on, in the listing's order (most listed options first).
    pub underlyings: Vec<OptionsUnderlying>,
}

/// One underlying of an [`OptionsListing`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionsUnderlying {
    /// The underlying as the venue spells it: `BTC`.
    pub name: String,
    /// The venue index its options' marks stream under ([`OptionsFeedSpec::MarkPrice`]): `btc_usd`.
    pub index: String,
}

/// Which options feed a [`Request::OptionsFeedOpen`] opens, and its focus set. The set is fixed for
/// the life of the socket (the datahub starts each feed once); a different set is a new socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionsFeedSpec {
    /// `markprice.options.{index}` for each index (mark + implied vol, chain-wide) →
    /// [`OptionsFeedDown::Marks`]. At most [`GATEWAY_OPTIONS_MAX_INDEXES`].
    MarkPrice {
        /// Venue index names, e.g. `btc_usd`.
        indexes: Vec<String>,
    },
    /// `ticker.{instrument}.100ms` for each instrument (bid/ask for the focus set) →
    /// [`OptionsFeedDown::Ticker`]. At most [`GATEWAY_OPTIONS_MAX_INSTRUMENTS`].
    Ticker {
        /// Venue instrument names, case-sensitive, e.g. `BTC-27JUL26-58000-C`.
        instruments: Vec<String>,
    },
}

impl OptionsFeedSpec {
    /// Why a daemon should refuse this spec, or `None` when it may serve it: an EMPTY focus set (the
    /// feed would stream nothing and look healthy), a set over its cap ([`GATEWAY_OPTIONS_MAX_INDEXES`],
    /// [`GATEWAY_OPTIONS_MAX_INSTRUMENTS`] — the 0052 bound, now client-named and therefore capped by the
    /// server), a blank name, or a name that is not a channel-safe word: over
    /// [`GATEWAY_OPTIONS_MAX_NAME_BYTES`] or holding a byte outside `[A-Za-z0-9_-]`. A name becomes part of
    /// a venue channel string (`markprice.options.<index>`, `ticker.<instrument>.100ms`), so a dot, a
    /// space or a control byte would let the client write the channel grammar itself. Whether a
    /// well-formed word is a REAL index or instrument is the daemon's to check against its venue
    /// (`OptionsHub::open_feed`). The text is for the [`Response::Error`] the daemon answers with.
    pub fn refusal(&self) -> Option<String> {
        let (what, names, cap) = match self {
            OptionsFeedSpec::MarkPrice { indexes } => {
                ("indexes", indexes, GATEWAY_OPTIONS_MAX_INDEXES)
            }
            OptionsFeedSpec::Ticker { instruments } => {
                ("instruments", instruments, GATEWAY_OPTIONS_MAX_INSTRUMENTS)
            }
        };
        if names.is_empty() {
            return Some(format!("an options feed must name at least one of its {what}"));
        }
        if names.len() > cap {
            return Some(format!(
                "an options feed may name at most {cap} {what} and this one names {}",
                names.len()
            ));
        }
        if names.iter().any(|n| n.trim().is_empty()) {
            return Some(format!("an options feed names a BLANK entry among its {what}"));
        }
        if let Some(long) = names.iter().find(|n| n.len() > GATEWAY_OPTIONS_MAX_NAME_BYTES) {
            return Some(format!(
                "an options feed names a {} byte entry among its {what}; a name is at most \
                 {GATEWAY_OPTIONS_MAX_NAME_BYTES} bytes",
                long.len()
            ));
        }
        if names.iter().any(|n| !n.bytes().all(channel_safe)) {
            return Some(format!(
                "an options feed names an entry among its {what} with a byte outside \
                 [A-Za-z0-9_-]; a name is spliced into the venue's channel string"
            ));
        }
        None
    }
}

/// Whether `b` may appear in an options index or instrument name: ASCII letters and digits, `_` and
/// `-` (`btc_usd`, `BTC_USDC-27JUL26-58000-C`). Deribit writes a decimal strike with a `d`, so no name
/// needs a dot.
fn channel_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

// ---- Responses -----------------------------------------------------------------------------------

/// A server-to-client response on the handshake and `rpc` classes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Reply to [`Request::Ping`].
    Pong,
    /// A server-side refusal or failure of a positional verb, stringified: an unknown or unserved
    /// venue, a scope that may not send the verb, a malformed request, an operation that failed. The
    /// connection survives (decode-vs-drop). A server-originated text is forwarded by the client
    /// UNCHANGED so the in-process error text survives the hop.
    Error(String),
    /// Reply to [`Request::DataOpen`]. **POSITIONAL, and the LAST positional frame this socket will
    /// carry** (the module doc's mode switch).
    DataOpened {
        /// The daemon's name for this session, for its logs and the status report.
        session: u64,
        /// A random id minted per daemon process start. A client that sees it change across a
        /// reconnect knows the daemon restarted (every session state it held is gone, not merely the
        /// link).
        epoch: u64,
        /// **The server's own heartbeat period in milliseconds** — the client arms
        /// `max(GATEWAY_STREAM_READ_FLOOR, 3 × heartbeat_ms)` after clamping this into
        /// `GATEWAY_STREAM_HEARTBEAT_MIN..=GATEWAY_STREAM_HEARTBEAT_MAX`.
        heartbeat_ms: u64,
    },
    /// Reply to [`Request::OptionsFeedOpen`]. Positional and the last positional frame of its socket.
    OptionsFeedOpened {
        /// See [`Response::DataOpened`].
        epoch: u64,
        /// See [`Response::DataOpened`].
        heartbeat_ms: u64,
    },
    /// Progress of a running fetch. The FIRST reply frame of every accepted fetch (so a fetch queued
    /// behind a venue budget is distinguishable from no daemon at all) and then at least one frame
    /// every [`FETCH_PROGRESS_EVERY`].
    FetchProgress(FetchProgress),
    /// One page of a fetch's rows, in order: `page` counts from 0 and rises by 1 per page of a `rid`.
    FetchPage {
        /// The fetch's id.
        rid: u64,
        /// The page number, from 0, strictly +1.
        page: u32,
        /// The rows; every page of a `rid` carries the same [`FetchRows`] kind.
        rows: FetchRows,
    },
    /// Terminal: the fetch finished, and `pages` / `rows` are the TOTALS the client verifies the
    /// reassembly against, so a lost or duplicated page is an error and never a short answer.
    FetchDone {
        /// The fetch's id.
        rid: u64,
        /// How many [`Response::FetchPage`] frames were written (0 is a legitimate empty result).
        pages: u32,
        /// The total row count across them.
        rows: u64,
    },
    /// Terminal: the fetch failed. `failure` IS the in-process `SourceError` (a `CatalogError` crosses
    /// as `Fetch`), variant and inner text byte-identical across the hop; its refusal's id rides beside it
    /// in `refusal_id`. Written by [`Response::fetch_failed`], read back by [`Response::source_failure`].
    FetchFailed {
        /// The fetch's id.
        rid: u64,
        /// What failed and how: `Fetch` the venue was asked and failed (retryable), `Refused` the
        /// request cannot be expressed and the venue was NOT asked (retrying cannot help). Its bytes are
        /// its variant and its text alone (`SourceError`'s doc).
        failure: SourceError,
        /// The id of the bridge's refusal `failure` carries (`ig.history.…`), `None` for free text. Additive
        /// (decision 0112): skipped while empty, and an older peer's frame reads `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal_id: Option<String>,
    },
    /// Terminal: the daemon stopped a fetch at the client's [`Request::FetchCancel`].
    FetchCancelled {
        /// The fetch's id.
        rid: u64,
    },
    /// Reply to [`Request::CatalogDescribe`].
    CatalogDescribed {
        /// The provider's `asset_classes()`.
        asset_classes: Vec<AssetClass>,
        /// The provider's `mode()`.
        mode: CatalogMode,
    },
    /// Reply to [`Request::OptionsRest`].
    OptionsReplied(OptionsReply),
    /// Reply to [`Request::ResolveFamily`]: the symbols that should be streaming at `now_ms`. A
    /// legitimately empty set is `Ok(empty)` and DOES unsubscribe; a failure is [`Response::Error`].
    FamilyResolved {
        /// The desired symbols, ascending.
        symbols: Vec<String>,
    },
    /// Reply to [`Request::CredentialPresence`].
    PresenceIs {
        /// The word.
        presence: vike_model::CredentialPresence,
    },
    /// Reply to [`Request::GatewayStatus`]. BOXED: the budget table makes it the widest reply.
    StatusIs(Box<GatewayStatusReport>),
    /// Reply to [`Request::ArmingQuery`]. BOXED: a row per account.
    ArmingTable(Box<ArmingTableWire>),
    /// Reply to [`Request::PreflightQuery`].
    PreflightLegs {
        /// Every leg measured, in the daemon's order.
        legs: Vec<PreflightLegWire>,
        /// The venue lists of a [`PreflightStep::Plan`] question; absent for every other step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<PreflightPlanWire>,
    },
    /// Reply to [`Request::CredentialsChanged`]: what was reconnected, empty when the daemon runs nothing on the
    /// account's credentials (its next start reads the store anyway).
    CredentialsReloaded {
        /// The route keys of the trade sessions reconnected, sorted (`binance#ALT`; an account that inherits the row's
        /// keys, `binance#PERP`, among them), then one `<venue> price stream` per credentialed data session ended so
        /// its client reopens it.
        reconnected: Vec<String>,
    },
    /// Reply to an accepted [`Request::TradeOpen`]. **POSITIONAL, and the LAST positional frame this
    /// socket will carry** (the module doc's mode switch).
    TradeOpened(TradeOpened),
    /// Reply to a [`Request::TradeOpen`] the lease table refused: another client holds the lease. The
    /// holder is named by its KEY ID (`nk-...`), never a key.
    LeaseHeld {
        /// The holder's key id.
        holder_key_id: String,
        /// When the holder took the lease, epoch ms.
        since_ms: i64,
    },
    /// Reply to a [`Request::TradeOpen`] the daemon refused for the account's own state: its tier does
    /// not match, it is not armed, the adapter is absent.
    TradeRefused {
        /// The daemon's `ArmingBlock::word()`; an UNKNOWN word is "adapter failed" at the consumer.
        block_word: String,
        /// The refusal text, the daemon's words: a refusal it declares arrives rendered, `error [<id>]: <text>`
        /// ([`crate::refusals::GatewayhubTradeRefusal`]; this frame has no id slot).
        text: String,
    },
    /// A refusal the daemon DECLARED with an id ([`crate::refusals`]): the refusal FRAME
    /// (`vike_hub_protocol::refusal::HubRefusal`), only to a client that said it reads it
    /// (`vike_hub_protocol::handshake::HandshakeFrame::Reads`); any other client is answered the
    /// [`Response::Error`] it always read ([`Response::older_form`]).
    Refused(vike_hub_protocol::refusal::HubRefusal),
}

impl Response {
    /// The terminal `FetchFailed` of fetch `rid`: `failure`, its refusal's id beside it.
    #[must_use]
    pub fn fetch_failed(rid: u64, failure: SourceError) -> Self {
        use vike_utility::errors::Declared as _;
        let refusal_id = failure.declared_id().map(str::to_owned);
        Response::FetchFailed { rid, failure, refusal_id }
    }

    /// A `FetchFailed`'s `failure` with the frame's `refusal_id` put back into it: the in-process
    /// [`SourceError`] the daemon's bridge answered, its id kept.
    #[must_use]
    pub fn source_failure(failure: SourceError, refusal_id: Option<String>) -> SourceError {
        use vike_utility::errors::CarriedRefusal;
        let carried = |why: CarriedRefusal| CarriedRefusal::new(refusal_id, why.text().to_owned());
        match failure {
            SourceError::Fetch(why) => SourceError::Fetch(carried(why)),
            SourceError::Refused(why) => SourceError::Refused(carried(why)),
        }
    }

    /// The reply a client sees in this one's place when it did not say it reads the refusal frame
    /// (`reads_refusal_frame`: what its connection said, `vike_hub_protocol::handshake::reads_refusal_frame`): a
    /// refusal FRAME becomes the [`Response::Error`] it always read, worded as an older client classes it
    /// ([`crate::refusals::older_text`]). `None`: send this reply as it is. The daemon's one write of a positional
    /// reply asks it, so an older client never meets a variant it cannot decode.
    #[must_use]
    pub fn older_form(&self, reads_refusal_frame: bool) -> Option<Self> {
        match self {
            Response::Refused(refusal) if !reads_refusal_frame => {
                Some(Response::Error(crate::refusals::older_text(refusal.clone())))
            }
            _ => None,
        }
    }
}

/// A running fetch's heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchProgress {
    /// The fetch's id.
    pub rid: u64,
    /// Whether the fetch is waiting or working.
    pub state: FetchState,
    /// Pages written so far.
    pub pages: u32,
    /// Rows written so far.
    pub rows: u64,
    /// Milliseconds since the daemon accepted the fetch.
    pub elapsed_ms: u64,
    /// **The daemon's promise: another frame of this fetch within this many milliseconds.** The client
    /// arms `max(FETCH_STALL_FLOOR, 3 × next_in_ms)` against the next frame, so the cadence needs no
    /// compile-time agreement between independently deployed binaries.
    pub next_in_ms: u64,
}

/// Whether a fetch is waiting on the venue budget or making progress. A budget-deferred fetch shows
/// `Queued`, so a stall detector does not misread waiting for the budget as a dead server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FetchState {
    /// Accepted and waiting (a venue budget, a lock).
    Queued,
    /// Calling the venue or encoding pages.
    Running,
}

/// The rows of one [`Response::FetchPage`]. One kind per lane, constant across the pages of a `rid`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FetchRows {
    /// [`FetchWhat::Klines`].
    Bars(Vec<Bar>),
    /// [`FetchWhat::Funding`].
    Funding(Vec<FundingRatePoint>),
    /// [`FetchWhat::Ticks`].
    Quotes(Vec<TickQuote>),
    /// [`FetchWhat::CatalogList`], [`FetchWhat::CatalogSearch`], [`FetchWhat::OptionIndex`].
    Instruments(Vec<Instrument>),
    /// [`FetchWhat::OptionTrades`].
    OptionTrades(Vec<OptionTradeRow>),
    /// [`FetchWhat::ClosedMarketRules`], [`FetchWhat::MarketRulesOfToken`], [`FetchWhat::SeriesMarketRules`].
    Texts(Vec<TextRow>),
}

impl FetchRows {
    /// The kind's name, for a mismatch message.
    pub fn kind(&self) -> &'static str {
        match self {
            FetchRows::Bars(_) => "Bars",
            FetchRows::Funding(_) => "Funding",
            FetchRows::Quotes(_) => "Quotes",
            FetchRows::Instruments(_) => "Instruments",
            FetchRows::OptionTrades(_) => "OptionTrades",
            FetchRows::Texts(_) => "Texts",
        }
    }

    /// How many rows this page holds.
    pub fn len(&self) -> usize {
        match self {
            FetchRows::Bars(v) => v.len(),
            FetchRows::Funding(v) => v.len(),
            FetchRows::Quotes(v) => v.len(),
            FetchRows::Instruments(v) => v.len(),
            FetchRows::OptionTrades(v) => v.len(),
            FetchRows::Texts(v) => v.len(),
        }
    }

    /// Whether this page holds no rows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append `other`'s rows, which must be the SAME kind.
    pub fn extend(&mut self, other: FetchRows) -> Result<(), String> {
        match (self, other) {
            (FetchRows::Bars(a), FetchRows::Bars(b)) => a.extend(b),
            (FetchRows::Funding(a), FetchRows::Funding(b)) => a.extend(b),
            (FetchRows::Quotes(a), FetchRows::Quotes(b)) => a.extend(b),
            (FetchRows::Instruments(a), FetchRows::Instruments(b)) => a.extend(b),
            (FetchRows::OptionTrades(a), FetchRows::OptionTrades(b)) => a.extend(b),
            (FetchRows::Texts(a), FetchRows::Texts(b)) => a.extend(b),
            (mine, theirs) => {
                return Err(format!(
                    "a {} page followed a {} page of the same fetch",
                    theirs.kind(),
                    mine.kind()
                ));
            }
        }
        Ok(())
    }

    /// Split these rows into pages of at most [`FETCH_PAGE_CEILING_BYTES`] of payload (at least one
    /// row per page) — the server's half of the reassembly [`FetchAssembler`] does. The pages are
    /// made as the iterator is pulled ([`pages_of`]), so splitting an answer never holds a second
    /// copy of it.
    pub fn into_pages(self, ceiling_bytes: usize) -> FetchPages {
        FetchPages(match self {
            FetchRows::Bars(v) => PagesOf::Bars(pages_of(v, ceiling_bytes)),
            FetchRows::Funding(v) => PagesOf::Funding(pages_of(v, ceiling_bytes)),
            FetchRows::Quotes(v) => PagesOf::Quotes(pages_of(v, ceiling_bytes)),
            FetchRows::Instruments(v) => PagesOf::Instruments(Box::new(pages_of(v, ceiling_bytes))),
            FetchRows::OptionTrades(v) => PagesOf::OptionTrades(pages_of(v, ceiling_bytes)),
            FetchRows::Texts(v) => PagesOf::Texts(Box::new(pages_of(v, ceiling_bytes))),
        })
    }
}

/// The pages of one answer, one [`FetchRows`] per [`Response::FetchPage`], made as they are pulled.
/// See [`FetchRows::into_pages`].
#[derive(Debug)]
pub struct FetchPages(PagesOf);

#[derive(Debug)]
enum PagesOf {
    Bars(Pages<Bar>),
    Funding(Pages<FundingRatePoint>),
    Quotes(Pages<TickQuote>),
    /// Boxed: an `Instrument` is hundreds of bytes and the carried row sits in the pager by value.
    Instruments(Box<Pages<Instrument>>),
    OptionTrades(Pages<OptionTradeRow>),
    /// Boxed for the same reason: a rules text is kilobytes.
    Texts(Box<Pages<TextRow>>),
}

impl Iterator for FetchPages {
    type Item = FetchRows;

    fn next(&mut self) -> Option<FetchRows> {
        match &mut self.0 {
            PagesOf::Bars(p) => p.next().map(FetchRows::Bars),
            PagesOf::OptionTrades(p) => p.next().map(FetchRows::OptionTrades),
            PagesOf::Funding(p) => p.next().map(FetchRows::Funding),
            PagesOf::Quotes(p) => p.next().map(FetchRows::Quotes),
            PagesOf::Instruments(p) => p.next().map(FetchRows::Instruments),
            PagesOf::Texts(p) => p.next().map(FetchRows::Texts),
        }
    }
}

/// The daemon's status report (`Request::GatewayStatus`): names and counters, never values.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GatewayStatusReport {
    /// The daemon's epoch (see [`Response::DataOpened`]).
    pub epoch: u64,
    /// When the daemon started, epoch-ms.
    pub started_ms: i64,
    /// Milliseconds since it started.
    pub uptime_ms: u64,
    /// Authenticated connections right now.
    pub connections: u32,
    /// Open `stream` sessions.
    pub stream_sessions: u32,
    /// Fetches running right now.
    pub fetches_running: u32,
    /// The `market_data_venue=` set, in advertisement order.
    #[serde(default)]
    pub market_data_venues: Vec<String>,
    /// The `credentialed_data_venue=` set, in advertisement order.
    #[serde(default)]
    pub credentialed_data_venues: Vec<String>,
    /// The budget counters (wave 1: SHADOW — counted and disclosed, nothing refused).
    #[serde(default)]
    pub budget: Vec<BudgetCounter>,
    /// Every venue whose last kept-list read was refused (oanda's start or refresh read, ig's list walk), in words
    /// with the time it was recorded: `vike_catalog::held_list::refusals`. Empty for an older daemon.
    #[serde(default)]
    pub list_refusals: Vec<vike_catalog::held_list::HeldListRefusal>,
}

/// One budget counter row of the status report. Strings, not the daemon's budget types, so this
/// crate names none of the daemon's internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetCounter {
    /// The venue.
    pub venue: String,
    /// The host or market label the ceiling is keyed by.
    pub host: String,
    /// The budget scope (ip, account, key, connection).
    pub scope: String,
    /// The traffic class (order, live, interactive, bulk).
    pub class: String,
    /// Units used in the current window.
    pub used: u64,
    /// The published ceiling.
    pub cap: u64,
    /// The window length in milliseconds.
    pub window_ms: u64,
    /// How many requests WOULD have been throttled had enforcement been on.
    pub would_throttle: u64,
}

// ---- The `feed` class ----------------------------------------------------------------------------

/// A server frame on an options `feed` socket, after [`Response::OptionsFeedOpened`]. The client says
/// nothing; a lost socket is re-opened with the same [`OptionsFeedSpec`] (reconnect == resubscribe),
/// silently — the datahub's options book has no gap concept, and the in-process feeds reconnect
/// silently too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OptionsFeedDown {
    /// One decoded `markprice.options` batch (mark + implied vol only) on a [`OptionsFeedSpec::MarkPrice`]
    /// feed.
    Marks(Vec<OptionTicker>),
    /// One decoded `ticker` row on a [`OptionsFeedSpec::Ticker`] feed. BOXED: an option row is ~320 bytes
    /// of optional fields and the other variants are a header.
    Ticker(Box<OptionTicker>),
    /// Written after the stated heartbeat period of silence.
    Heartbeat,
    /// The server is ending this feed, and why.
    Bye(StreamByeReason),
}

#[cfg(test)]
#[path = "proto_tests.rs"]
mod proto_tests;
