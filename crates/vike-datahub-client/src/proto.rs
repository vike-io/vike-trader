//! The vike-datahub wire protocol: length-prefixed `serde_json` frames over a blocking
//! `std::net` byte stream, and the request/response schema that rides them.
//!
//! # Framing
//!
//! One frame is a big-endian `u32` byte length followed by exactly that many bytes of UTF-8 JSON.
//! [`write_frame`] serializes a value, writes the length prefix, writes the bytes, and flushes;
//! [`read_frame`] reads the 4-byte length, bounds-checks it against [`MAX_FRAME_LEN`] (so a hostile
//! or corrupt peer cannot make us pre-allocate an unbounded buffer and OOM), then reads and decodes
//! the body. serde/JSON errors map to [`std::io::Error`] so a caller has ONE error channel. No
//! codec crate and no async runtime: the workspace `deny.toml` bans a second transport stack, so
//! the transport is bare `std::net` + `serde_json`.
//!
//! # Compute-to-data contract
//!
//! A client ships a small query next to where the data lives and gets back a compact ANSWER, never a
//! raw upstream data slice. The verbs that RUN something are the backtesthub's own protocol
//! (`vike_backtesthub_client::proto`), and a client that sends one here is told where to go: the frame
//! does not decode into this [`Request`], and the daemon answers `vike_hub_protocol::plane`'s named
//! refusal rather than a parse failure.
//!
//! - The READ verbs ([`Request::LoadBars`], [`Request::ScanQuotes`], [`Request::ScanTrades`],
//!   [`Request::PropertiesAsOf`] and their siblings) mirror the [`vike_data::HistoryStore`] read
//!   methods and return the exact typed `vike-model` result. They are the
//!   [`HistoryStoreDatahub`](crate::remote::HistoryStoreDatahub) seam: a chart's bars ARE the answer,
//!   bounded by the query rather than the whole store.
//!
//! # Protocol version and the decode-vs-drop contract
//!
//! A client opens with [`Request::Hello`]; the server answers [`Response::Welcome`] carrying ITS
//! [`PROTO_VERSION`] and the served-verb `features`, and the CLIENT fails loudly, naming BOTH
//! numbers, on a mismatch. An additive verb never moves the version: it is negotiated by a
//! `features` string such as [`FEATURE_BACKFILL`]
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).
//! Whether `Hello` is optional depends on the server's keys — see [`Request::Hello`].
//!
//! **Decode-vs-drop.** Reading a frame and DECODING it are split: [`read_frame_raw`] returns the
//! framed body bytes (behind the same OOM guard) WITHOUT decoding, and [`read_frame`] is that plus
//! `serde_json::from_slice`. A server MUST use the raw read so a well-framed body that fails to
//! decode into a known [`Request`] variant (an unknown verb from a newer client) is answered with
//! [`Response::Error`] and the connection SURVIVES. A decode error is a bad *request*, not a bad
//! *connection*; fusing decode into the read makes the two indistinguishable.

use serde::{Deserialize, Serialize};
use vike_data::{
    CohortRow, ExecFillRow, ExecOrderRow, InstrumentCoverage, PerpMetricRow, SeriesCoverage,
    SeriesId,
};

/// The removal vocabulary, RE-EXPORTED rather than restated, so a consumer can CONSTRUCT a
/// [`Request::DeleteSeries`] without a `vike-data` dependency edge of its own (`vike-cli` named them
/// here while it took `vike-data` as a DEV-dependency only; it now has the edge for the archive
/// verb's words, `vike_data::archive_day`, and still names these here). A re-export adds
/// no package and no edge (`vike-data` at default features is already in that graph through this
/// crate), where flattened DTOs here would be a second shape for the same facts.
pub use vike_data::store::removal::{RemovalOutcome, RemovalPlan, SeriesSelector, describe_id};
/// The `--produced-by` VALIDATOR, re-exported for the same reason: both ends of the wire share one
/// definition of a valid producer filter (`crates/vike-datahub/src/server/delete.rs`'s
/// `delete_series_verb` resolves through it, so a BLANK spelling cannot turn a provenance-asserted
/// delete into a wildcard one), and `vike-cli` can name the rule its refusals enforce before a
/// socket is dialled. Its module has no `use` and no DataFusion code, so it
/// adds no Arrow/DataFusion weight.
pub use vike_data::store::store_kind::resolve_produced_by;
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{EquitySample, SymbolProperties};

// The capability ceiling carried by `Request::Auth` / `Response::AuthOk`, from the shared
// `vike_hub_protocol::auth`; named at that path by every caller, never re-exported through either
// hub protocol.
use vike_hub_protocol::auth::Scope;

// The FRAME CODEC, re-exported as this protocol's own vocabulary — the shape
// `crates/vike-tradehub-client/src/proto.rs` uses for the same five items: a second codec would be
// a second answer to what a frame is. It lives in `vike-hub-protocol`, below both hub protocols
// (`docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`). Public-API
// surface rather than an alias shim: a protocol module's frame codec IS its vocabulary.
pub use vike_hub_protocol::frame::{
    MAX_FRAME_LEN, read_frame, read_frame_raw, read_frame_raw_capped, write_frame,
};

mod features;
mod verbs;

pub use features::{
    FEATURE_ARCHIVE_FETCH, FEATURE_ARCHIVE_IMPORT, FEATURE_BACKFILL, FEATURE_BACKFILL_CANCEL,
    FEATURE_BACKFILL_FUNDING, FEATURE_BAR_SOURCE, FEATURE_COVERAGE, FEATURE_CSV_COLUMNS,
    FEATURE_CSV_UPLOAD, FEATURE_DELETE_SERIES, FEATURE_EXECUTION_MATERIALIZE,
    FEATURE_FUNDING_HISTORY, FEATURE_GATEWAY_LINKS, FEATURE_GATEWAY_STATUS,
    FEATURE_HISTORY_CHANNELS, FEATURE_IMPORT_FORMAT_PREFIX, FEATURE_IMPORT_REPLACE_HOLED,
    FEATURE_INPUT_BARS, FEATURE_INSTRUMENT_LISTS, FEATURE_MARKET_DATA,
    FEATURE_MARKET_DATA_LANE_QUOTES, FEATURE_MARKET_DATA_VENUE_PREFIX,
    FEATURE_OPERATOR_INSTRUMENTS, FEATURE_OPTIONS_CHAIN, FEATURE_OPTIONS_REFRESH,
    FEATURE_REC_VENUE_PREFIX, FEATURE_SCAN_BOOK_UPDATES, FEATURE_SCAN_COHORT, FEATURE_SCAN_DEPTH,
    FEATURE_SCAN_EQUITY, FEATURE_SCAN_EXECUTION_FILLS, FEATURE_SCAN_EXECUTION_FILLS_RANGE,
    FEATURE_SCAN_EXECUTION_ORDERS, FEATURE_SCAN_LIMIT, FEATURE_SCAN_PERP_METRICS,
    FEATURE_SEED_CLASS, FEATURE_SEED_SERIES, FEATURE_SERIES_FACTS, FEATURE_SOURCE,
    FEATURE_STRATEGY_LANE, FEATURE_STRATEGY_LANE_BARS, FEATURE_STRATEGY_LANE_BOOK,
    FEATURE_STRATEGY_LANE_QUOTES, FEATURE_STRATEGY_LANE_TICKS, FEATURE_STRATEGY_VENUE_PREFIX,
    FEATURE_STREAM_CLASS, FEATURE_STREAM_FUNDING, FEATURE_SYMBOL_SETS, FEATURE_TOOL_FEEDS,
    FEATURE_VENUE_CATALOG, advertised_import_formats, advertised_market_data_venues,
    advertised_rec_venues, advertised_strategy_venues, import_format_feature,
    market_data_venue_feature, rec_venue_feature, strategy_venue_feature,
};
pub use verbs::{reply_wait, request_kind, required_scope};

/// The datahub wire protocol version, negotiated by the [`Request::Hello`] / [`Response::Welcome`]
/// handshake: the client fails the connection with a legible, both-numbers error on a mismatch (see
/// `DatahubClient::connect`). It changes ONLY for a change an old peer cannot DECODE; an additive
/// verb, field or advertisement is negotiated by a `Welcome.features` string instead
/// (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).
///
/// - `1` — Hello/Ping/`RunBacktest` + the four read verbs.
/// - `2` — `RunSlice` + `RunResult`.
/// - `3` — `RunSweep` / `RunWalkforward` + their results.
/// - `4` — [`Request::ListSeries`] / [`Request::Inventory`] / [`Request::SeriesGaps`] + answers.
/// - `5` — `ListStrategies` + `Strategies`.
/// - `6` — the OPTIONAL `params` field on `RunSweep` / `RunWalkforward`.
/// - `7` — `RunSweepProfile` / `RunWalkforwardProfile` + their reports.
/// - still `7`, each negotiated per decision 0112: `Backfill` (+ funding), `Coverage`, `Auth`,
///   `MarketDataSubscribe`/`MarketDataUpdate` (+ `market_data_venue=`), `rec_venue=`, `ImportArchive` (+ `import_format=`),
///   `ListBackfills`/`CancelBackfill`, `HistoryChannels`, and every later `FEATURE_*` verb.
///
/// The `Run*` verbs named above left this schema with no change of byte when the backtesthub got its
/// own client (decision 0252): they ride `vike_backtesthub_client::proto`, under the same number.
/// - `8` — the phase-4 renames the owner ruled (P4.5c): the strategy-lane verbs (`Strat*` is
///   `Strategy*`), `GwStatus` is `GatewayStatus`, and every renamed field key. A peer from before them could not decode those frames, so the number
///   steps and an old peer is refused at the handshake, naming both numbers (`client.rs`'s
///   `a_daemon_from_before_the_phase_four_renames_is_refused_at_the_handshake`).
pub const PROTO_VERSION: u32 = 8;

// ---- The STRATEGY LANE's shared vocabulary (wave 2, W5) ------------------------------------------------
//
// The lane is the datahub's `StrategyOpen` / `StrategyOpened` pair followed by the TUBE's frames
// (`vike_market_stream::wire`), in the shape of the gatewayhub's `DataOpen` / `DataOpened`: the open
// reply is the last positional frame its socket carries. These are the numbers BOTH ends need - the
// frame ceilings, the symbol guard, the heartbeat range - stated once here, in the lower crate, and
// read by the server (`vike-datahub`, which adds its own POLICY numbers beside them) and by the
// strategy lane's client. Nothing here is a negotiated value except the heartbeat, which the open
// reply STATES, so a period needs no compile-time agreement between independently deployed binaries.

/// The longest frame a post-auth CLIENT sends on a strategy-lane socket: a `Sub`, an `Unsubscribe`, a
/// `Delivery`, a `Ping`. A few hundred bytes each, so 16 KiB leaves a symbol at its guard plenty of
/// room and lets no authenticated peer make the daemon allocate more per frame.
pub const STRATEGY_LANE_UP_FRAME_CEILING_BYTES: u32 = 16 * 1024;

/// How many price levels a folded book on this lane may carry: the pump's REST seed is a thousand a
/// side, and a depth lane's levels are clamped to the same number.
pub const STRATEGY_LANE_BOOK_LEVELS_CEILING: usize = 1000;

/// The most JSON bytes one price level takes on the wire, `[price,size]` at serde's widest: a
/// seventeen-digit price and size with their exponents, with room. Re-measured by
/// `crates/vike-datahub/src/strat/limits.rs`'s tests against a real book.
pub const STRATEGY_LANE_WORST_LEVEL_JSON_BYTES: usize = 48;

/// The longest frame the daemon sends on a strategy-lane socket, and the one the client reads under:
/// a book of [`STRATEGY_LANE_BOOK_LEVELS_CEILING`] levels a side plus its envelope. A longer frame is refused by
/// the WRITER before its first byte and disclosed as a hole, never sent (the client would read it as a
/// framing fault that kills the session). The one exception is a `Seed` (a REST warm-up of up to a
/// thousand bars, about 150 KB), which the writer cuts to its newest bars that fit
/// (`vike_market_stream::writer`'s module doc).
pub const STRATEGY_LANE_FRAME_CEILING_BYTES: u32 = 128 * 1024;

/// The longest symbol or interval a strategy-lane client frames, in bytes: a FRAMING guard, as the
/// market-data lane's `MARKET_DATA_MAX_SYMBOL_BYTES` is not (that one is derived). A Polymarket token id is 78.
pub const STRATEGY_LANE_MAX_SYMBOL_BYTES: usize = 256;

/// The default strategy-lane heartbeat period: the daemon writes a `Heartbeat` after this much silence.
/// The client never assumes it - it reads the period from [`Response::StrategyOpened`] - and arms
/// `max(STRATEGY_LANE_READ_FLOOR, 3 x heartbeat_ms)`.
pub const STRATEGY_LANE_HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(5);

/// The shortest period a daemon may state (half a second): below it a quiet lane would write
/// continuously for no liveness gain. A stated period under this is clamped UP by the client.
pub const STRATEGY_LANE_HEARTBEAT_MIN: std::time::Duration = std::time::Duration::from_millis(500);

/// The longest period a daemon may state (fifteen seconds): above it the client's three-period deadline
/// stops being a liveness signal for a feed whose silence is a `Disconnected` for the dead-man. A stated
/// period over this is clamped DOWN by the client.
pub const STRATEGY_LANE_HEARTBEAT_MAX: std::time::Duration = std::time::Duration::from_secs(15);

/// The client's read-deadline FLOOR on a strategy-lane stream: three default heartbeats. A floor, not
/// the contract: the deadline is `max(STRATEGY_LANE_READ_FLOOR, 3 x StrategyOpened.heartbeat_ms)`.
pub const STRATEGY_LANE_READ_FLOOR: std::time::Duration =
    std::time::Duration::from_secs(3 * STRATEGY_LANE_HEARTBEAT.as_secs());

/// The wire word of the `network` field for the venue's main network.
pub const STRATEGY_NETWORK_MAINNET: &str = "mainnet";

/// The wire word of the `network` field for a venue's test network (Hyperliquid only: the one venue
/// whose feed follows the account tier).
pub const STRATEGY_NETWORK_TESTNET: &str = "testnet";

const _: () = assert!(
    STRATEGY_LANE_UP_FRAME_CEILING_BYTES > 0 && STRATEGY_LANE_UP_FRAME_CEILING_BYTES <= 1024 * 1024,
    "STRATEGY_LANE_UP_FRAME_CEILING_BYTES must stay far under MAX_FRAME_LEN: nothing a client sends here is \
     large, and an authenticated peer must not be able to name a large allocation"
);
const _: () = assert!(
    STRATEGY_LANE_FRAME_CEILING_BYTES <= MAX_FRAME_LEN
        && 2 * STRATEGY_LANE_BOOK_LEVELS_CEILING * STRATEGY_LANE_WORST_LEVEL_JSON_BYTES + 4096
            <= STRATEGY_LANE_FRAME_CEILING_BYTES as usize,
    "a full book (levels a side at the widest level) plus its envelope must fit the strategy lane's \
     frame ceiling, which must itself fit MAX_FRAME_LEN"
);
const _: () = assert!(
    STRATEGY_LANE_HEARTBEAT_MIN.as_millis() > 0
        && STRATEGY_LANE_HEARTBEAT_MIN.as_millis() <= STRATEGY_LANE_HEARTBEAT.as_millis()
        && STRATEGY_LANE_HEARTBEAT.as_millis() <= STRATEGY_LANE_HEARTBEAT_MAX.as_millis()
        && STRATEGY_LANE_HEARTBEAT_MAX.as_secs() <= 15,
    "the default heartbeat must sit inside the range a daemon may state, and the longest period must \
     stay short: the client's deadline is three periods and a silent lane is a Disconnected for the \
     dead-man"
);
const _: () = assert!(
    STRATEGY_LANE_READ_FLOOR.as_millis() >= 2 * STRATEGY_LANE_HEARTBEAT.as_millis(),
    "the read floor must allow at least TWO missed heartbeats"
);

/// A client-to-server request.
///
/// The READ verbs carry **typed params** mirroring [`vike_data::HistoryStore`]'s read methods one-for-one.
/// A range rides as decomposed `start` / `end` `Option<i64>` fields (inclusive, epoch-ms) because
/// `vike_data::TimestampRange` derives no serde; the server rebuilds the `TimestampRange`. Results are the
/// `vike-model` value types, which already derive serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// The version-handshake opener: the client's [`PROTO_VERSION`]. Answered by
    /// [`Response::Welcome`] (the server's version, its feature list and — on a KEYED server — the
    /// per-connection auth `nonce`).
    ///
    /// ⚠ **Whether it is OPTIONAL depends on the server's keys**
    /// (`docs/decisions/0025-datahub-remote-posture.md`; `vike_datahub::server`'s module doc is the
    /// authority for both arms):
    ///
    /// - **Key-LESS server** (no [`vike_hub_protocol::auth::HubKeys`] configured — the default): a
    ///   normal request sent without a prior `Hello` is served; `Hello` negotiates the version, it
    ///   does not gate the connection.
    /// - **KEYED server**: `Hello` is MANDATORY and must be the FIRST frame, followed by
    ///   [`Request::Auth`]. Any other verb before [`Response::AuthOk`] is refused with
    ///   [`Response::AuthDenied`] and the connection closes.
    Hello {
        /// The client's [`PROTO_VERSION`].
        proto_version: u32,
    },
    /// Answer a KEYED server's nonce challenge (the second and last pre-auth frame): `mac` is the
    /// HMAC over `(key_for(scope), Welcome.nonce, proto_version, scope)` under the DATAHUB domain
    /// separator, per [`vike_hub_protocol::auth::sign`]. Answered by [`Response::AuthOk`] or
    /// [`Response::AuthDenied`].
    ///
    /// `scope` is the capability ceiling for the whole connection, bound INTO the mac;
    /// [`required_scope`] is the ONE authority mapping each verb to the scope that may send it.
    /// [`Scope::Read`] reads history and catalog; [`Scope::Write`] additionally admits the store
    /// writes such as [`Request::Backfill`].
    ///
    /// Sent only against a server advertising [`vike_hub_protocol::auth::FEATURE_AUTH`]; a key-less server has nothing to
    /// verify it against and answers [`Response::AuthDenied`] saying so.
    Auth {
        /// The capability ceiling this client is authenticating under.
        scope: Scope,
        /// The HMAC-SHA256 tag over the connection's nonce challenge (raw bytes; serialized as a
        /// JSON array of `u8`).
        mac: Vec<u8>,
    },
    /// Liveness probe — answered by [`Response::Pong`].
    Ping,
    /// Read derived OHLCV bars — mirrors `HistoryStore::load_bars(venue, symbol, interval, range)`.
    /// Answered by [`Response::Bars`] (or [`Response::Error`]).
    LoadBars {
        /// Venue partition (e.g. `"binance"`).
        venue: String,
        /// Symbol partition (e.g. `"BTCUSDT"`).
        symbol: String,
        /// Bar interval (e.g. `"1d"`).
        interval: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        /// The `source=` lane to read (`HistoryStore::load_bars_sourced_capped`); `None` (or absent) is the
        /// sourceless series, as ever. Never sent to a server without [`FEATURE_BAR_SOURCE`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    /// Read L1 quotes — mirrors `HistoryStore::scan_quotes(venue, symbol, range)`. Answered by
    /// [`Response::Quotes`] (or [`Response::Error`]).
    ScanQuotes {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        /// The `source=` lane to read (`HistoryStore::scan_quotes_sourced_capped`); `None` (or absent) is the
        /// sourceless series, as ever. Never sent to a server without [`FEATURE_SOURCE`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    /// Read executed trades — mirrors `HistoryStore::scan_trades(venue, symbol, range)`. Answered by
    /// [`Response::Trades`] (or [`Response::Error`]).
    ScanTrades {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read raw L2 book updates — mirrors `HistoryStore::scan_book_updates(venue, symbol, range)`.
    /// Answered by [`Response::BookUpdates`] (or [`Response::Error`]). The first of 0084's family:
    /// see [`FEATURE_SCAN_BOOK_UPDATES`].
    ScanBookUpdates {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read the CONFLATING depth lane — mirrors `HistoryStore::scan_depth(venue, symbol, range)`.
    /// Answered by [`Response::Depth`] (or [`Response::Error`]).
    ///
    /// ⚠ **Same payload type as [`Self::ScanBookUpdates`], and deliberately its OWN request and
    /// reply variant.** The two read DIFFERENT `kind=` partitions
    /// (`crates/vike-data/src/store/store_kind.rs`), so one shared reply would make a desync
    /// between them undetectable: a conflated lane misread as a lossless one would decode as
    /// plausible data rather than an error.
    ScanDepth {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read cohort rows — mirrors `HistoryStore::scan_cohort(venue, asset, range)`. Answered by
    /// [`Response::Cohort`] (or [`Response::Error`]).
    ///
    /// ⚠ The second field is an **ASSET, not a symbol** — the one verb in this family whose middle
    /// argument is not a symbol partition; a field named `symbol` carrying an asset is how a caller
    /// passes the wrong one.
    ScanCohort {
        /// Venue partition.
        venue: String,
        /// Asset partition — NOT a symbol; see the variant doc.
        asset: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read perpetual-swap metric rows — mirrors
    /// `HistoryStore::scan_perp_metrics(venue, symbol, range)`. Answered by [`Response::PerpMetrics`]
    /// (or [`Response::Error`]).
    ScanPerpMetrics {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read the stored equity curve — mirrors `HistoryStore::scan_equity(venue, symbol, range)`.
    /// Answered by [`Response::Equity`] (or [`Response::Error`]).
    ScanEquity {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read the Tier-2 exec FILL log — `HistoryStore::scan_execution_fills_head(venue, symbol, range, n)`.
    /// Answered by [`Response::ExecFills`] (or [`Response::Error`]).
    ///
    /// ⚠ **`start`/`end`/`limit` are OPTIONAL and negotiated by [`FEATURE_SCAN_EXECUTION_FILLS_RANGE`]**,
    /// not by the variant: all three are skipped when `None`, so a request without them is
    /// BYTE-IDENTICAL to the frame this variant had before they existed, and an older server
    /// decodes a request WITH them by ignoring them — answering the whole series. That second fact
    /// is why a client sends a window only to a server that advertises the string. With no `limit`
    /// the server answers the whole range or refuses it by name above `SCAN_EXECUTION_FILLS_CEILING`;
    /// with one, a soft whole-`ts` page ([`FEATURE_SCAN_LIMIT`]).
    ScanExecFills {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` (or absent) = unbounded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` (or absent) = unbounded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read the Tier-2 ORDER-LIFECYCLE log — `HistoryStore::scan_execution_orders_head(venue, symbol,
    /// range, n)`. Answered by [`Response::ExecOrders`] (or [`Response::Error`]). Negotiated by
    /// [`FEATURE_SCAN_EXECUTION_ORDERS`]: a client refuses locally against a server that does not
    /// advertise it. Same range and soft-`limit` shape as [`Self::ScanEquity`].
    ScanExecOrders {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// Row cap, `None` (or absent) = every row in the range. SOFT: see [`FEATURE_SCAN_LIMIT`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Point-in-time properties lookup — mirrors `HistoryStore::properties_as_of(venue, symbol, ts)`.
    /// Answered by [`Response::Properties`] (or [`Response::Error`]).
    PropertiesAsOf {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// As-of instant (epoch-ms): the most-recent properties observed at or before this.
        timestamp: i64,
    },
    /// Enumerate every stored series — mirrors `HistoryStore::list_series()`. Answered by
    /// [`Response::SeriesList`] (or [`Response::Error`]).
    ListSeries,
    /// Every stored series with its cheap coverage — mirrors `HistoryStore::inventory()`. Answered by
    /// [`Response::Inventory`] (or [`Response::Error`]).
    Inventory,
    /// One series' coverage plus its commit keys — mirrors `HistoryStore::series_facts(id)`. Answered
    /// by [`Response::SeriesFacts`] (or [`Response::Error`]). Why it exists:
    /// [`FEATURE_SERIES_FACTS`].
    SeriesFacts {
        /// The series whose coverage and commit keys to report.
        id: SeriesId,
    },
    /// The gap ranges in one series' recorded span — mirrors `HistoryStore::series_gaps(id)`. Answered
    /// by [`Response::SeriesGaps`] (or [`Response::Error`]). `id` rides whole (`SeriesId` is
    /// serde).
    SeriesGaps {
        /// The series whose day-coverage gaps to report.
        id: SeriesId,
    },
    /// The CROSS-KIND coverage report — mirrors `HistoryStore::coverage_report()`. Answered by
    /// [`Response::Coverage`] (or [`Response::Error`]).
    ///
    /// No payload: like [`Request::Inventory`] it is a whole-store manifest fold, and the Data
    /// Manager renders all of it at once (a per-instrument verb would be N round trips per screen).
    /// Answers, not slices: day INDICES per kind per instrument, never rows. Negotiated by
    /// [`FEATURE_COVERAGE`].
    Coverage,
    /// Backfill-on-demand (split-plane REQ-9): fetch `(venue, symbol, interval)` history over
    /// `[start, end]` from the venue's public endpoints INTO the server's store, write-through
    /// BEFORE the reply, and answer [`Response::BackfillDone`] — so N clients asking for the same
    /// range cost the venue ONE fetch ("clients request, never fetch").
    ///
    /// **Three lanes, picked from `venue` and `interval`:** the venue's own kline BARS; Dukascopy's
    /// bars, RESAMPLED server-side from ticks it downloads and stores as quotes first (so a
    /// `dukascopy` bar request is a tick download); and the FUNDING-rate series under the reserved
    /// `interval` label below. `BackfillDone` counts bars in every lane (funding points are stored
    /// as bars too).
    ///
    /// SYNCHRONOUS per request over a BOUNDED range: `start`/`end` are REQUIRED (unlike
    /// [`Request::LoadBars`]'s), because an unbounded fetch is unbounded venue I/O. No job id, no
    /// progress stream: the collectors page and pace internally, and the reply IS the completion
    /// event; a range too big for one request is the CLIENT's chunking decision.
    ///
    /// Served ONLY by a `backfill-serve` server with a collector table mounted, negotiated by
    /// [`FEATURE_BACKFILL`]; any other build answers a clean [`Response::Error`] naming what is
    /// missing. ⚠ An `interval` of `vike_data::source::FUNDING_INTERVAL` (`"funding"`) is a
    /// reserved LABEL, not a step: it selects the funding-rate series, negotiated SEPARATELY by
    /// [`FEATURE_BACKFILL_FUNDING`].
    Backfill {
        /// Venue whose PUBLIC history the server fetches from, and the store partition the rows
        /// land in (e.g. `"binance"`). An unsupported venue is an error naming the supported set.
        venue: String,
        /// Symbol, in the STORE's spelling — the series key the rows land under. For most venues
        /// that is the venue's own (`"BTCUSDT"` is Binance SPOT), but a Binance PERPETUAL is
        /// spelled with the perpetual marker, `"BTCUSDT.P"`, and the funding lane REFUSES a bare
        /// `"BTCUSDT"`: spot has no funding.
        symbol: String,
        /// Bar interval (e.g. `"1h"`), or the reserved `"funding"` label (see above).
        interval: String,
        /// Inclusive range start (epoch-ms). REQUIRED — see above.
        start: i64,
        /// Inclusive range end (epoch-ms). REQUIRED — see above.
        end: i64,
    },
    /// **LIST the [`Request::Backfill`] requests this server is running right now** — on every
    /// connection, not this one. Answered by [`Response::RunningBackfills`] (or
    /// [`Response::Error`]).
    ///
    /// A client that went away stops its own backfill, but a half-open peer and a `fetch` left
    /// running in another session do not; this is how an operator SEES them
    /// (`docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §4). An empty
    /// list is the truthful "nothing is running". [`VerbScope::Read`](vike_hub_protocol::auth::VerbScope::Read): it answers from server state
    /// and changes nothing (0101, verdict 2). Negotiated by [`FEATURE_BACKFILL_CANCEL`].
    ListBackfills,
    /// **STOP every running [`Request::Backfill`] on one series** at its next chunk boundary.
    /// Answered by [`Response::BackfillsCancelled`] (or [`Response::Error`]) at ONCE: it raises
    /// each request's cancel flag and does NOT wait for any of them to stop.
    ///
    /// A cancelled request stops between two chunks — every earlier chunk stored with its key
    /// spent, nothing partial written — and its OWN connection is answered [`Response::Error`]
    /// naming the cancel, the chunks stored and the rows written, and saying that repeating the
    /// request resumes. Never `BackfillDone`: the window is not in the store. ⚠ A request on a
    /// ONE-BATCH lane is listed and NOT flagged ([`BackfillCancelDone::unstoppable`],
    /// [`BACKFILL_ONE_BATCH`]).
    ///
    /// The series is matched EXACTLY — the three strings the `Backfill` carried — and no request id
    /// is named: the door is "stop this series". [`VerbScope::Write`](vike_hub_protocol::auth::VerbScope::Write) (its arm in
    /// [`required_scope`] says why); negotiated by [`FEATURE_BACKFILL_CANCEL`].
    CancelBackfill {
        /// The venue, exactly as the running `Backfill` named it.
        venue: String,
        /// The symbol, exactly as the running `Backfill` named it.
        symbol: String,
        /// The interval, exactly as the running `Backfill` named it (`"funding"` included).
        interval: String,
    },
    /// **Through which doors each roster venue's history comes, how far back each goes — and what
    /// THIS server can say about its own lanes.** Answered by [`Response::HistoryChannels`] (or
    /// [`Response::Error`]). `docs/superpowers/specs/2026-10-02-history-channels-step2-design.md`
    /// §2.
    ///
    /// The rows are `vike_catalog::history_channels_for`'s as the SERVER's build declares them; the
    /// overlay is the server's alone — whether each lane is mounted, whether the credential it
    /// reads is stored (a presence word, never a value), and what the store holds per kind.
    /// [`crate::history`]'s module doc carries the shape.
    ///
    /// ⚠ **A unit variant: the request names NOTHING**, and adding a parameter is
    /// `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md`'s reopener (its arm in
    /// [`required_scope`] carries the scope argument). Negotiated by [`FEATURE_HISTORY_CHANNELS`].
    HistoryChannels,
    /// **Is the gatewayhub up, as this datahub sees it?** Answered by [`Response::GatewayStatus`], a
    /// down gatewayhub included. A unit variant: the request names NOTHING, and a read loop is served
    /// from a cache of a second or two ([`required_scope`]'s arm). Negotiated by
    /// [`FEATURE_GATEWAY_STATUS`].
    GatewayStatus,
    /// **The venue sessions the gatewayhub recorded**, opened since `since_ms`, newest first; `route` narrows to one
    /// account's route key. Answered by [`Response::GatewayLinks`], from the settings store this datahub already opens: it
    /// dials nothing, so it answers with the gatewayhub down. Negotiated by [`FEATURE_GATEWAY_LINKS`].
    GatewayLinks {
        /// The start of the window, epoch-ms: a session counts if it was open at any time since.
        since_ms: i64,
        /// One account's route key; `None` is every account and feed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route: Option<String>,
        /// One venue: every account of it and its feeds; `None` is every venue.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        venue: Option<String>,
    },
    /// **IMPORT one archive dataset from the SERVER's own imports directory into the store.**
    /// Answered by [`Response::ArchiveImported`] (or [`Response::Error`]). [`crate::archive`]'s
    /// module doc carries the contract,
    /// `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` the design.
    ///
    /// ⚠ **The request names NO PATH and NO VENUE**: a format from the server's own registry and
    /// ONE validated directory name ([`crate::archive::validate_import_dataset`]); the server
    /// composes the path and the format decides the venue. A request that could carry a path would
    /// make this wire a remote file reader.
    ///
    /// The PLAN is always computed and returned; `dry_run` decides only whether the import half
    /// runs. Served ONLY by a datahub whose import lane is MOUNTED, negotiated by
    /// [`FEATURE_ARCHIVE_IMPORT`] plus the per-format [`import_format_feature`] entries.
    /// [`VerbScope::Write`](vike_hub_protocol::auth::VerbScope::Write): see [`required_scope`]'s arm. A newtype over
    /// [`crate::archive::ImportSpec`], byte-identical on the wire to a struct variant.
    ImportArchive(crate::archive::ImportSpec),
    /// **DOWNLOAD one dataset's daily files of a vendor's bucket into the SERVER's own imports
    /// folder**, where [`Request::ImportArchive`] then reads them. Answered by
    /// [`Response::ArchiveFetched`] (or [`Response::Error`]); [`crate::archive_fetch`]'s module doc
    /// carries the contract. Like the import it names a format and ONE dataset name, never a path or
    /// a bucket; the bucket is the server's `venue_url` row and the AWS key its credential store's.
    /// Served by a datahub whose import lane MOUNTS a download, negotiated by
    /// [`FEATURE_ARCHIVE_FETCH`]. [`VerbScope::Write`](vike_hub_protocol::auth::VerbScope::Write):
    /// it spends the operator's AWS account and writes the server's disk.
    FetchArchive(crate::archive_fetch::FetchSpec),
    /// **A chart is open on a series the store cannot paint — SEED IT.** Answered by
    /// [`Response::SeriesSeeded`] (or [`Response::Error`]).
    ///
    /// ⚠ **A WRITE verb classified [`VerbScope::Read`](vike_hub_protocol::auth::VerbScope::Read)**:
    /// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` is the argument — read it
    /// before widening anything here; [`required_scope`]'s arm carries the short form. **The
    /// request names a SERIES AND NOTHING ELSE** (no range, bar count, lookback or cadence; the
    /// cost terms both ends need live in [`crate::seed`]), which is the whole difference from
    /// [`Request::Backfill`]'s client-named range. **Adding a parameter here is the first bullet of
    /// 0058's reopen list, not a small change.**
    ///
    /// It SEEDS; it does not MAINTAIN: one window per series per process (a repeat is free,
    /// `SeedDone::repeated`); deepening or refreshing a series is [`Request::Backfill`]'s job.
    /// Negotiated by [`FEATURE_SEED_SERIES`]; an unarmed server answers
    /// `SeriesSeeded { armed: false, .. }` rather than an error.
    SeedSeries {
        /// Venue whose PUBLIC kline REST the server fetches from, in the server's own table
        /// spelling (e.g. `"binance"`). A venue with no collector is an error naming the
        /// supported set — the identical refusal [`Request::Backfill`] gives.
        venue: String,
        /// Symbol, in the venue's own spelling (e.g. `"BTCUSDT"`, `"BTC-USDT-SWAP"`). Bounded in
        /// length AND charset by `vike_hub_protocol::wire_ident::validate_seed_symbol` before it reaches a bridge,
        /// because the binance kline URL interpolates it unencoded.
        symbol: String,
        /// Bar interval (e.g. `"5m"`). Checked against [`crate::seed::SEED_INTERVALS`] at the
        /// server's door, before dispatch — see that constant, and the ⚠ in [`crate::seed`]'s
        /// module doc for the hole it stands in front of.
        interval: String,
        /// **What KIND of instrument `symbol` names**, when the caller knows —
        /// `docs/decisions/0061` Phase 3, negotiated by [`FEATURE_SEED_CLASS`].
        ///
        /// ⚠ `default` + `skip_serializing_if` are load-bearing: a `None` frame is BYTE-IDENTICAL
        /// to the one shipped before this field existed, so a chart that names no class costs an
        /// older daemon nothing.
        ///
        /// ⚠ **It NARROWS the route; it moves no term of the cost** (the window, bar count, venue
        /// and interval sets, rate and per-process series cap stay server constants), so it is not
        /// the parameter 0058's first reopen bullet forbids: it can only make the server refuse a
        /// series it would otherwise have fetched from the wrong book.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<vike_model::AssetClass>,
    },
    /// **LIST ONE VENUE'S INSTRUMENTS** — the verb behind the Data Manager's catalog refresh.
    /// Answered by [`Response::VenueCatalog`] (or [`Response::Error`]).
    ///
    /// ⚠ **[`VerbScope::Read`](vike_hub_protocol::auth::VerbScope::Read) and NOT A WRITE**
    /// (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`; its arm
    /// in [`required_scope`] carries the argument). Nothing enters the served store; the durable
    /// copy of a catalog is the CLIENT's own `vike_catalog::persist` cache, and the server keeps
    /// only an in-process TTL memo. **Persisting anything here makes this a write and re-imposes
    /// 0058's four-part rule; adding a parameter is the first bullet of 0062's reopen list.** The
    /// request names a VENUE and nothing else; the cost terms both ends need live in
    /// [`crate::catalog`]. A CREDENTIALED venue (alpaca) is refused BY CONSTRUCTION
    /// with [`crate::catalog::CatalogRefusal::NeedsCredentials`].
    ///
    /// Negotiated by [`FEATURE_VENUE_CATALOG`]; an unarmed server answers
    /// [`crate::catalog::CatalogOutcome::NotArmed`] rather than an error.
    VenueCatalog {
        /// The venue, in the canonical roster spelling (e.g. `"binance"`), bounded in length AND
        /// charset by `vike_hub_protocol::wire_ident::validate_catalog_venue` at the server's door. A venue with
        /// no provider in this build is a [`crate::catalog::CatalogRefusal::NotServed`] carried in
        /// the SUCCESS variant — unlike every other unknown-venue path on this wire, because "which
        /// venues can I refresh" is a routine question and an error is the wrong shape for it.
        venue: String,
    },
    /// **ONE UNDERLYING'S OPTION-CHAIN SNAPSHOT** — the verb behind the desktop's Options tool.
    /// Answered by [`Response::OptionsChain`] (or [`Response::Error`]).
    ///
    /// ⚠ **[`VerbScope::Read`](vike_hub_protocol::auth::VerbScope::Read) and NOT A WRITE**, [`Request::VenueCatalog`]'s class: nothing enters
    /// the served store on this request's account, and the request names a venue, an underlying and
    /// at most [`crate::options::MAX_OPTIONS_EXPIRIES`] dates — every one SELECTING from what the
    /// server already holds. Which chains are polled, which instruments are streamed and how often
    /// are the server's; no client input reaches the venue. **Adding a parameter that reaches the
    /// venue (a strike window, a stream focus, a cadence) changes that classification.**
    ///
    /// Negotiated by [`FEATURE_OPTIONS_CHAIN`]; a server built without the lane answers
    /// [`crate::options::OptionsChainOutcome::NotArmed`] rather than an error.
    OptionsChain {
        /// The venue, in the canonical roster spelling (`"deribit"`). A venue the server's lane
        /// does not serve is a [`crate::options::OptionsRefusal::UnknownVenue`] in the SUCCESS
        /// variant, the [`Request::VenueCatalog`] rule.
        venue: String,
        /// The underlying, in the venue's own currency code (`"BTC"`, `"ETH"`, `"SOL"`).
        underlying: String,
        /// The expiries wanted, ISO `YYYY-MM-DD`. EMPTY means the front expiry only; one the server
        /// does not list is OMITTED from the answer; more than
        /// [`crate::options::MAX_OPTIONS_EXPIRIES`] is refused. All three bounded by
        /// [`crate::options::validate_options_request`] at the server's door.
        expiries: Vec<String>,
    },
    /// **READ THE VENUE'S OPTION LISTING AGAIN, NOW** — the "refresh the option list" a user asks for
    /// when an underlying was listed or delisted (the lane reads it on its own at most hourly).
    /// Answered by [`Response::OptionsRefreshed`], or [`Response::Error`] with the reason the listing
    /// could not be had (a gatewayhub that is down, a venue read that failed, a build without the lane).
    ///
    /// ⚠ **[`VerbScope::Read`](vike_hub_protocol::auth::VerbScope::Read)**: it writes nothing, names only a
    /// venue, and the venue cost is the gatewayhub's to bound: inside a minute of its last read of the
    /// listing it answers the listing it holds, so no client can make the box ask faster than that.
    ///
    /// Negotiated by [`FEATURE_OPTIONS_REFRESH`].
    OptionsRefresh {
        /// The venue, in the canonical roster spelling (`"deribit"`). A venue the lane does not serve
        /// is a [`Response::Error`] naming the ones it does.
        venue: String,
    },
    /// **THE NEWS FEED SNAPSHOT** — the verb behind the desktop's News window. Answered by
    /// [`Response::NewsFeed`] (or [`Response::Error`]).
    ///
    /// ⚠ **[`VerbScope::Read`](vike_hub_protocol::auth::VerbScope::Read) and NOT A WRITE**, [`Request::OptionsChain`]'s class: it answers from
    /// the server's in-memory cache. The request names NO feed, URL or cadence — which providers are
    /// polled, how often and with what are the server's — so no client input reaches a provider.
    /// **Adding a parameter that reaches a provider changes that classification.**
    ///
    /// Negotiated by [`FEATURE_TOOL_FEEDS`]; a server built without the lane answers
    /// [`crate::tool_feeds::ToolFeedOutcome::NotArmed`] rather than an error.
    NewsFeed {
        /// The generation the caller already holds. Equal to the server's current generation, the
        /// answer is [`crate::tool_feeds::ToolFeedOutcome::Unchanged`] and carries no rows.
        if_generation: Option<u64>,
    },
    /// **THE ECONOMIC-CALENDAR SNAPSHOT** (the ForexFactory week the server holds) — the verb
    /// behind the desktop's Calendar window. [`Request::NewsFeed`]'s class and argument, verbatim.
    /// Answered by [`Response::EconomicCalendar`] (or [`Response::Error`]); negotiated by
    /// [`FEATURE_TOOL_FEEDS`].
    EconomicCalendar {
        /// As on [`Request::NewsFeed`].
        if_generation: Option<u64>,
    },
    /// **ONE WEEK'S EARNINGS, DIVIDENDS AND IPOs** — the verb behind the desktop's three equity
    /// windows. [`Request::NewsFeed`]'s class and argument, and the one parameter is a SELECTION
    /// from what the server serves: a Monday within [`crate::tool_feeds::EQUITY_WEEKS_AHEAD`] weeks
    /// of the server's current week ([`crate::tool_feeds::validate_equity_week`]); anything else is
    /// a typed [`crate::tool_feeds::ToolFeedRefusal::WeekOutOfRange`] in the SUCCESS variant.
    /// Answered by [`Response::EquityCalendar`] (or [`Response::Error`]); negotiated by
    /// [`FEATURE_TOOL_FEEDS`].
    EquityCalendar {
        /// The Monday of the week wanted, ISO `YYYY-MM-DD`. A malformed value is refused by
        /// [`crate::tool_feeds::validate_equity_week_shape`] at the server's door.
        week_start: String,
        /// As on [`Request::NewsFeed`], for THIS week's cache.
        if_generation: Option<u64>,
    },
    /// **DELETE stored series, IRREVERSIBLY** — the one verb on this wire that destroys data.
    /// Answered by [`Response::Deleted`] (or [`Response::Error`]).
    ///
    /// It always computes and returns the PLAN — which series matched, what they hold, and who
    /// wrote them — and `dry_run` decides only whether the second half happens. No preview token on
    /// the wire: a caller runs `dry_run: true` first, and the surfaces that need a bound
    /// confirmation (the CLI's typed line, the MCP tool's `preview_token`) build it on their side.
    ///
    /// ⚠ Served ONLY by a KEYED server, refused OUTRIGHT by a key-less one whatever the request
    /// says ([`FEATURE_DELETE_SERIES`] carries the argument); a keyed server also requires
    /// `Scope::Write`.
    DeleteSeries {
        /// Which series to remove — the four identity dimensions, with an OMITTED one as the only
        /// wildcard. The server intersects it with its own enumeration; nothing here builds a path.
        selector: SeriesSelector,
        /// The commit-key prefix EVERY key of EVERY matched series must carry. `None` skips the
        /// assertion, which the server permits only for a fully-named single series — a SWEEP with
        /// no assertion is refused, because that is where "by name" stops being a name.
        produced_by: Option<String>,
        /// `true` computes and returns the plan and deletes nothing.
        dry_run: bool,
    },
    /// **Open a MARKET-DATA push stream on this connection** — the wire's one MODE SWITCH.
    ///
    /// Sent on a fresh connection and answered positionally with exactly one
    /// [`Response::MarketDataSubscribed`], **the last positional frame that socket will carry**: after it
    /// the server never reads this direction again and writes only [`Response::MarketData`].
    /// [`crate::market`]'s module doc states the invariant and its one exception — a
    /// [`Response::Error`] answer has NOT switched the connection, and a client that gets one must
    /// keep using the socket positionally rather than starting a reader thread.
    ///
    /// ⚠ **`refused` is PER-SPEC; a whole-request failure is [`Response::Error`].** A build with no
    /// hub does not answer `MarketDataSubscribed { accepted: [], refused: [all] }` — that would mode-switch
    /// the connection into a heartbeat-only writer that never sends a frame (hence no
    /// `HubNotMounted` in [`crate::market::MarketDataRefusal`]).
    ///
    /// ⚠ **There is deliberately NO long-lived control connection.** A session's mutation channel
    /// is [`Request::MarketDataUpdate`] on an ordinary short-lived connection, so a dead control connection
    /// is not a STATE: a client that cannot reach a persistent control socket cannot unsubscribe
    /// and leaks topics until its stream drops.
    ///
    /// [`VerbScope::Read`](vike_hub_protocol::auth::VerbScope::Read) — see [`required_scope`] and
    /// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md`.
    MarketDataSubscribe {
        /// The subscriptions wanted. Each is accepted (possibly with a CLAMPED `depth_levels`) or
        /// refused with its own reason; `accepted` echoes the server's AUTHORITATIVE spec.
        specs: Vec<crate::market::MarketDataSpec>,
    },
    /// **Change an existing market-data session's subscription set**, answered positionally with
    /// [`Response::MarketDataUpdated`].
    ///
    /// ⚠ **Sent on an ORDINARY, SHORT-LIVED connection — never on the stream socket**, whose server
    /// side has left its read loop: a frame written there is never read and the caller waits
    /// forever. The `crates/vike-datahub-client/src/remote.rs` dial-per-request shape.
    ///
    /// ⚠ `remove` matches on the KEY `(venue, symbol, lane)` and IGNORES `depth_levels` — depth is
    /// not part of a subscription's identity (`crate::market::MarketDataSpec::key`). An unknown or expired
    /// `session` is [`Response::Error`], not a refusal list.
    MarketDataUpdate {
        /// The session minted by the [`Response::MarketDataSubscribed`] that opened the stream.
        session: crate::market::MarketDataSessionId,
        /// Specs to ADD. Refusals are per-spec.
        add: Vec<crate::market::MarketDataSpec>,
        /// Specs to REMOVE, matched on their key.
        remove: Vec<crate::market::MarketDataSpec>,
    },
    /// **Open a STRATEGY-LANE stream on this connection** - the wire's second MODE SWITCH, for the
    /// tradehub's price path (wave 2 of `vike-gatewayhub`; `docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`
    /// verdict 2 and `docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`).
    ///
    /// Sent on a fresh connection authenticated under [`Scope::Trade`] and answered positionally with
    /// exactly one [`Response::StrategyOpened`], **the last positional frame that socket will carry**:
    /// after it the socket speaks the TUBE's frames and nothing else - the client sends
    /// `vike_market_stream::wire::StreamUp` (its subscriptions are in-band `Sub` / `Unsubscribe`, so there is
    /// no `StratUpdate` verb and no spec list here) and the daemon sends `StreamDown`. A
    /// [`Response::Error`] answer has NOT switched the connection: it stays positional, exactly as
    /// [`Request::MarketDataSubscribe`]'s refusals do.
    ///
    /// ONE venue per stream, because the tube's receiving engine is one venue's client: the strategy
    /// lane's client holds a stream per distinct venue (and network).
    ///
    /// [`VerbScope::Trade`](vike_hub_protocol::auth::VerbScope::Trade), and no other scope is admitted to it (and `Trade` is admitted to nothing
    /// else): see [`required_scope`] and [`scope_admits`](vike_hub_protocol::auth::scope_admits). Negotiated by [`FEATURE_STRATEGY_LANE`].
    StrategyOpen {
        /// The venue's roster slug.
        venue: String,
        /// The network the venue's feed runs on, for the one venue whose feed follows the account tier
        /// (Hyperliquid): [`STRATEGY_NETWORK_MAINNET`] or [`STRATEGY_NETWORK_TESTNET`]. `None` is the main
        /// network, and is the only value any other venue takes - the daemon refuses a network on a
        /// venue that has none, rather than ignore it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<String>,
    },
    /// **Read one perpetual's STORED FUNDING SETTLEMENTS on the strategy lane's key**: the bars of
    /// `(venue, symbol)` in `[start, end]` (epoch-ms, both inclusive) out of the store's `interval=funding`
    /// series (`vike_data::source::FUNDING_INTERVAL`, written by the funding backfill and by the datahub's
    /// own recording of the live funding lane), answered positionally with [`Response::Bars`]: each bar's
    /// `funding` is the rate and its `close` the price the venue valued it at (`0.0` for none). A restarted
    /// PAPER engine catches up the settlements it missed from it (decision 0262's amendment of 2026-11-08).
    ///
    /// Sent on a positional connection authenticated under [`Scope::Trade`], never after a
    /// [`Request::StrategyOpen`] on the same socket. [`VerbScope::Trade`](vike_hub_protocol::auth::VerbScope::Trade)
    /// like the open (the tradehub holds no Observe key); a READ of the store under the server's bar ceiling
    /// that calls no venue. Negotiated by [`FEATURE_FUNDING_HISTORY`].
    StrategyFundingHistory {
        /// The venue's roster slug.
        venue: String,
        /// The series symbol (the strategy lane's spelling, the store's key).
        symbol: String,
        /// The first settlement time to answer, epoch-ms (inclusive).
        start: i64,
        /// The last settlement time to answer, epoch-ms (inclusive).
        end: i64,
    },
    /// **Read one STORED BAR SERIES an input of a live strategy mount reads** (decision 0271's live half): the
    /// bars of `(venue, symbol, interval)` in `[start, end]` (epoch-ms, both inclusive), the venue's own candles
    /// or one `source=` lane of them (a user's `user-<label>` CSV import), answered positionally with
    /// [`Response::Bars`]: `LoadBars`' read, on the strategy lane's key.
    ///
    /// Sent on a positional connection authenticated under [`Scope::Trade`], never after a
    /// [`Request::StrategyOpen`] on the same socket. [`VerbScope::Trade`](vike_hub_protocol::auth::VerbScope::Trade)
    /// like the open; a READ of the store under the server's bar ceiling that calls no venue. Negotiated by
    /// [`FEATURE_INPUT_BARS`].
    StrategyInputBars {
        /// The venue's roster slug.
        venue: String,
        /// The series symbol (the store's key).
        symbol: String,
        /// Bar interval (e.g. `"1d"`).
        interval: String,
        /// The `source=` lane, `None` for the venue's own candles.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        /// The first bar time to answer, epoch-ms (inclusive).
        start: i64,
        /// The last bar time to answer, epoch-ms (inclusive).
        end: i64,
    },
    /// **Open an UPLOAD of a user's CSV into a `user-*` lane** (decision 0272): the series and the
    /// file's size. Answered [`Response::CsvUploadReady`] or refused, before a byte of the file is
    /// sent; then [`Request::CsvUploadChunk`]s and one [`Request::CsvUploadEnd`] on the SAME connection.
    /// [`crate::upload`]'s module doc carries the exchange. A
    /// [`VerbScope::Read`](vike_hub_protocol::auth::VerbScope::Read) write: see [`required_scope`]'s arm.
    /// Negotiated by [`FEATURE_CSV_UPLOAD`], which only an ARMED server advertises.
    CsvUpload(crate::upload::CsvUploadSpec),
    /// The next piece of an open upload's text, at most [`crate::upload::UPLOAD_CHUNK_BYTES`]; answered
    /// [`Response::CsvUploadReceived`]. Outside an upload it is refused.
    CsvUploadChunk {
        /// The text, in the file's order.
        text: String,
    },
    /// Every declared byte is sent: decode the file and, unless the upload is a dry run, store it.
    /// Answered [`Response::CsvUploaded`]. Its reply wait is the client's, as an import's is.
    CsvUploadEnd,
    /// **The user's named symbol lists** (the desktop's DataSets), as the server's settings database
    /// holds them (`vike_secrets::symbol_set`). Answered [`Response::SymbolSets`]; an EMPTY list is the
    /// ordinary first state, and the client shows its built-in defaults over it. Negotiated by
    /// [`FEATURE_SYMBOL_SETS`].
    SymbolSets,
    /// **Save one list**, replacing the list of the same name. A
    /// [`VerbScope::Write`](vike_hub_protocol::auth::VerbScope::Write) verb. Answered
    /// [`Response::SymbolSets`] with every list after the write, or [`Response::Error`] naming what was
    /// refused.
    SetSymbolSet {
        /// The list.
        set: vike_data::datasets::SymbolSet,
    },
    /// **Remove the list called `name`.** A [`VerbScope::Write`](vike_hub_protocol::auth::VerbScope::Write)
    /// verb, answered as [`Request::SetSymbolSet`]; a name no list has removes nothing.
    DeleteSymbolSet {
        /// The list's name.
        name: String,
    },
    /// **Every venue's FETCHED instrument list the datahub keeps** (`<project>/instruments/<venue>.json`) and the
    /// operator's own credentialed lists: the desktop's startup read of the symbol universe. Answered
    /// [`Response::InstrumentLists`]. Negotiated by [`FEATURE_INSTRUMENT_LISTS`].
    InstrumentLists,
    /// **Refresh one venue's list now, or every venue's** (`None`): fetched through the catalog lane and the
    /// gatewayhub as [`Request::VenueCatalog`] is, then KEPT by the datahub — which is what makes it a
    /// [`VerbScope::Write`](vike_hub_protocol::auth::VerbScope::Write) verb (`docs/decisions/0062`'s amendment of
    /// 2026-11-12). Answered [`Response::InstrumentListsRefreshed`].
    RefreshInstrumentLists {
        /// The venue, or `None` for every venue the lane serves.
        venue: Option<String>,
    },
    /// **The instruments the operator adds BY HAND** (`vike_secrets::operator_instrument`), one at a time: each symbol
    /// of `add` is looked up on `venue` through the gatewayhub (the venue's own lookup over a session it already
    /// holds) or, with `stated`, takes the operator's stated facts; each of `remove` is removed. The datahub folds the
    /// rows into the venue's kept list (`<project>/instruments/<venue>.json`), so every reader of the kept lists reads
    /// them too, and a refresh never erases them. Empty `add` and `remove` change nothing: that is how a client reads
    /// every row. A [`VerbScope::Write`](vike_hub_protocol::auth::VerbScope::Write) verb (the operator's own rows, and
    /// a lookup spends the operator's venue session). Answered [`Response::OperatorInstruments`]. Negotiated by
    /// [`FEATURE_OPERATOR_INSTRUMENTS`].
    OperatorInstruments {
        /// The roster venue `add` and `remove` name; unread when both are empty.
        venue: String,
        /// The symbols to add, as the operator typed them (`EURUSD`).
        add: Vec<String>,
        /// The symbols to remove: the venue's own name of a row, or the word it was added under.
        remove: Vec<String>,
        /// The facts of every symbol of `add`, stated by the operator for a venue that cannot answer them; `None` asks
        /// the venue.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stated: Option<crate::catalog::StatedInstrumentFacts>,
    },
}

/// The [`Response::Deleted`] payload: what one [`Request::DeleteSeries`] planned, and — unless it
/// was a dry run — what it then did.
///
/// The PLAN is always present, including on a dry run and when nothing matched. The OUTCOME is
/// `None` for a dry run, and a `Some` whose `failed` list is non-empty is a partial success: one
/// broken series is one skipped series, the rest went, and the caller exits non-zero off it. A
/// provenance REFUSAL is a [`Response::Error`] instead: nothing was deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteDone {
    /// What the selector matched, with each series' coverage and provenance.
    pub plan: RemovalPlan,
    /// What was removed. `None` iff the request was a dry run.
    pub outcome: Option<RemovalOutcome>,
}

/// The [`Response::BackfillDone`] payload: what one [`Request::Backfill`] actually did.
///
/// `first_timestamp`/`last_timestamp` are read BACK from the store over the requested range after the write —
/// the write-through proof, and the client's seam bookkeeping for stitching segment 2 to its live
/// tail — `None` when the range holds no bars. Failures are [`Response::Error`], never this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillDone {
    /// Rows the collector wrote. `0` is still a SUCCESS, and can mean more than one thing — the
    /// window was already ingested (the collectors are idempotent by commit key), or the venue had
    /// no rows in it; on dukascopy's tick lane, which counts the BARS it resampled, also that no
    /// whole bar fell inside the window.
    pub rows_written: u64,
    /// First bar ts now stored in the requested range (`None` = the range holds no bars).
    pub first_timestamp: Option<i64>,
    /// Last bar ts now stored in the requested range (`None` = the range holds no bars).
    pub last_timestamp: Option<i64>,
}

/// One RUNNING [`Request::Backfill`], as the server's registry holds it — a row of
/// [`Response::RunningBackfills`] and of [`BackfillCancelDone`].
///
/// ⚠ **Every field is what the request itself carried, or a counter the SERVER keeps.** A
/// read-scope key reads this
/// (`docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`,
/// verdict 2), so a field naming a credential, an account or anything else the request did not
/// carry is a different verb, and is that record's reopener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillRunWire {
    /// The server's own number for this request, unique for the life of the process — what tells
    /// two requests on one series apart. Never reused; it says nothing about any other server.
    pub id: u64,
    /// The venue the request named.
    pub venue: String,
    /// The symbol the request named.
    pub symbol: String,
    /// The interval the request named (`"funding"` on the funding lane).
    pub interval: String,
    /// The window's inclusive start, epoch-ms, as the request named it.
    pub start: i64,
    /// The window's inclusive end, epoch-ms, as the request named it.
    pub end: i64,
    /// The peer address of the connection that sent it, as the server sees it. ⚠ Through the
    /// deployed `ssh -L` tunnel every peer is sshd on loopback, so this says little there.
    pub peer: Option<String>,
    /// When the server began it, epoch-ms on the SERVER's clock.
    pub started_ms: i64,
    /// How long it has run, in milliseconds, by the server's monotonic clock at the moment of the
    /// answer — so a client on a different clock can still say "running for 3 h".
    pub elapsed_ms: u64,
    /// The server's name for the lane it runs on (`Klines`, `TickBars`, `Funding`,
    /// `CredentialedKlines`). Informational: [`Self::stoppable`] is what a client acts on, so a
    /// lane named in a future server needs no change here.
    pub lane: String,
    /// Whether a [`Request::CancelBackfill`] can stop it — whether its lane is CHUNKED and asks the
    /// stop probe between chunks. `false` is a ONE-BATCH lane ([`BACKFILL_ONE_BATCH`]).
    pub stoppable: bool,
    /// Whether an operator's cancel has been raised on it: "asked to stop", never "has stopped" —
    /// a request that has stopped is gone from the list.
    pub cancelled: bool,
    /// Chunk boundaries reached — the collector asks the stop probe once at the top of every chunk,
    /// so this counts chunks BEGUN. Always `0` on a one-batch lane. Not a percentage: the server
    /// does not know a lane's chunk count in advance.
    pub boundaries: u64,
}

/// What a [`Request::CancelBackfill`] cannot do to a request on a ONE-BATCH lane, in the words both
/// ends print. Such a lane — the keyless kline rows and the funding lane — fetches its whole window
/// in one request and commits it once, so it has no chunk boundary at which to stop; its requests
/// are LISTED by [`Request::ListBackfills`] and REFUSED by the cancel rather than pretended at
/// (the design's §8 and Q6). One spelling, in the crate below both the server and the CLI that
/// renders [`BackfillCancelDone::unstoppable`].
pub const BACKFILL_ONE_BATCH: &str = "cannot stop: one batch per request";

/// The [`Response::BackfillsCancelled`] payload: what one [`Request::CancelBackfill`] did to the
/// requests running on its series.
///
/// Both lists empty is a SUCCESS that says nothing was running on that series — the cancel is
/// idempotent, and asking twice is never an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillCancelDone {
    /// The requests whose cancel flag is now raised — by this call, or already by an earlier one.
    /// Each stops at its NEXT chunk boundary; the call did not wait for any of them, so a later
    /// [`Request::ListBackfills`] is how a caller sees them go.
    pub flagged: Vec<BackfillRunWire>,
    /// The requests on the series that NO cancel can stop: a one-batch lane, refused with
    /// [`BACKFILL_ONE_BATCH`]. Listed so the answer says what is still running, rather than
    /// implying by omission that nothing is. Their flag is NOT raised.
    pub unstoppable: Vec<BackfillRunWire>,
}

/// The [`Response::SeriesSeeded`] payload: what the server DID about one [`Request::SeedSeries`].
///
/// ⚠ **Every field here is an OUTCOME, and the request carried none of them** — the asymmetry
/// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` rests on: the client names a
/// series, the server names the window. A client that derives a NEXT request from these numbers
/// is building the range parameter 0058's reopen list forbids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedDone {
    /// **Whether the operator armed this lane at all.** `false` = `flags.datahub_chart_seed` is
    /// unset on the server: nothing was fetched or written, and this is a SUCCESS.
    ///
    /// ⚠ A client must not treat `false` as an error — it is the default configuration. It IS worth
    /// telling the operator, because from the chart's side an unarmed server and an empty store
    /// look identical (`vike_app_core::data::chart_seed::render_chart_seed_status` writes that
    /// sentence).
    pub armed: bool,
    /// **Whether this series had already been seeded by this process.** `true` = the ledger
    /// answered, no token was spent and no venue was called; `rows_written` is 0 and the timestamps
    /// describe what the store already holds. Distinct from `rows_written == 0` on a fresh seed,
    /// which means the venue was asked and the window was already ingested.
    pub repeated: bool,
    /// Rows the collector wrote (0 = the window was already ingested, or `armed`/`repeated` short
    /// -circuited the fetch — all three are success).
    pub rows_written: u64,
    /// The inclusive epoch-ms window the SERVER chose (`crate::seed::seed_range`), so a client can
    /// report what it was given. `None` only when nothing was fetched.
    pub range: Option<(i64, i64)>,
    /// First bar ts now stored in that window, read back through the served handle (`None` = the
    /// window holds no bars — an honest answer for a symbol the venue does not list).
    pub first_timestamp: Option<i64>,
    /// Last bar ts now stored in that window.
    pub last_timestamp: Option<i64>,
}

/// A server-to-client response. The `Report`/`Bars`/`Quotes`/`Trades`/`Properties` variants are the
/// compute-to-data answers; `Error` carries a server-side failure message so a client never has to
/// guess why a request did not produce its answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Reply to [`Request::Hello`]: the server's [`PROTO_VERSION`] and its advertised `features` —
    /// the capability strings naming what this server serves (`"load_bars"`, the
    /// `FEATURE_*` constants). The client compares the version and fails loudly on a mismatch.
    ///
    /// On a KEYED server it additionally carries `nonce` and advertises [`vike_hub_protocol::auth::FEATURE_AUTH`].
    Welcome {
        /// The server's [`PROTO_VERSION`].
        proto_version: u32,
        /// The verbs this server answers (advertised capability strings).
        features: Vec<String>,
        /// The per-connection auth challenge on a KEYED server: 32 fresh random bytes the client
        /// signs in [`Request::Auth`]. Freshly minted per connection, which is what makes a
        /// captured transcript unreplayable against a later one.
        ///
        /// ⚠ `None` on a key-less server, and the serde attributes are load-bearing:
        /// `skip_serializing_if` keeps a key-less server's `Welcome` BYTE-IDENTICAL to the pre-auth
        /// protocol's, and `default` lets an old server's `Welcome` decode for a new client.
        /// `crates/vike-datahub/tests/default_build/auth_roundtrip.rs`'s
        /// `a_keyless_servers_welcome_is_byte_identical_to_the_pre_auth_protocol` holds it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<[u8; 32]>,
    },
    /// Reply to an accepted [`Request::Auth`]: the connection is authenticated, and `scope` is its
    /// capability ceiling for the rest of its life (never re-negotiated — a second `Auth` is
    /// refused).
    AuthOk {
        /// The scope the connection authenticated under.
        scope: Scope,
    },
    /// Reply to a REFUSED [`Request::Auth`], or to any verb sent to a KEYED server before one
    /// succeeded. The connection closes after this frame.
    ///
    /// ⚠ `reason` is deliberately coarse (`"bad mac"`, `"not authenticated: expected Hello
    /// first"`): it never distinguishes "no such key" from "wrong key" and never echoes a key, a
    /// mac or a nonce. The detail goes to the OPERATOR's log.
    AuthDenied {
        /// A short, non-informative-to-an-attacker refusal reason.
        reason: String,
    },
    /// Reply to [`Request::Ping`].
    Pong,
    /// A server-side failure (invalid profile, missing strategy, data error, …), stringified.
    Error(String),
    /// Reply to [`Request::LoadBars`]: the derived OHLCV bars, ts-ascending.
    Bars(Vec<Bar>),
    /// Reply to [`Request::ScanQuotes`]: the L1 quotes, ts-ascending.
    Quotes(Vec<TickQuote>),
    /// Reply to [`Request::ScanTrades`]: the executed trades, ts-ascending.
    Trades(Vec<TickTrade>),
    /// Reply to [`Request::ScanBookUpdates`]: the raw L2 book updates, ts-ascending.
    BookUpdates(Vec<L2BookUpdate>),
    /// Reply to [`Request::ScanDepth`]: the conflating depth lane's updates, ts-ascending. ⚠ A
    /// SEPARATE variant from [`Self::BookUpdates`] on purpose — see [`Request::ScanDepth`].
    Depth(Vec<L2BookUpdate>),
    /// Reply to [`Request::ScanCohort`]: the cohort rows, ts-ascending.
    Cohort(Vec<CohortRow>),
    /// Reply to [`Request::ScanPerpMetrics`]: the perpetual-swap metric rows, ts-ascending.
    PerpMetrics(Vec<PerpMetricRow>),
    /// Reply to [`Request::ScanEquity`]: the equity samples, ts-ascending.
    Equity(Vec<EquitySample>),
    /// Reply to [`Request::ScanExecFills`]: the Tier-2 exec fill rows, ts-ascending.
    ExecFills(Vec<ExecFillRow>),
    /// Reply to [`Request::ScanExecOrders`]: the order-lifecycle snapshots, ts-ascending (one
    /// `order_client_id` appears once per materialized step).
    ExecOrders(Vec<ExecOrderRow>),
    /// Reply to [`Request::PropertiesAsOf`]: the point-in-time properties, or `None`.
    ///
    /// BOXED: `SymbolProperties` is ~140 bytes and DOCUMENTED to grow, so inline it would be by far
    /// the largest variant (`clippy::large_enum_variant`). serde treats `Box<T>` / `Option<Box<T>>`
    /// transparently, so the wire shape is identical — the box is in-memory only.
    Properties(Option<Box<SymbolProperties>>),
    /// Reply to [`Request::ListSeries`]: every stored series id, sorted.
    SeriesList(Vec<SeriesId>),
    /// Reply to [`Request::Inventory`]: every stored series paired with its cheap coverage.
    Inventory(Vec<(SeriesId, SeriesCoverage)>),
    /// Reply to [`Request::SeriesGaps`]: the inclusive epoch-ms gap ranges (empty = none).
    SeriesGaps(Vec<(i64, i64)>),
    /// Reply to [`Request::SeriesFacts`]: one series' coverage and the commit keys that produced
    /// it. Both halves ride whole — `SeriesCoverage` is already serde for [`Self::Inventory`].
    SeriesFacts(Box<(SeriesCoverage, Vec<String>)>),
    /// Reply to [`Request::Coverage`]: the cross-kind report, one entry per instrument, in the
    /// store's own (sorted) order. The value type is [`vike_data::InstrumentCoverage`] itself, not
    /// a wire DTO — see that module's "Why these types derive serde" — so a remote caller folds
    /// `partial_days()` over exactly the bytes a local caller folds.
    Coverage(Vec<InstrumentCoverage>),
    /// Reply to [`Request::Backfill`]: the fetch ran and the rows are IN THE STORE (write-through
    /// happens before this frame is written). See [`BackfillDone`] for the field contract; a
    /// fetch/validation failure is [`Response::Error`], never a partial `BackfillDone`.
    BackfillDone(BackfillDone),
    /// Reply to [`Request::ListBackfills`]: every `Backfill` the server is running, in the order it
    /// began them. Empty when none is.
    RunningBackfills(Vec<BackfillRunWire>),
    /// Reply to [`Request::CancelBackfill`]: which requests on the series were flagged and which no
    /// cancel can stop. See [`BackfillCancelDone`].
    BackfillsCancelled(BackfillCancelDone),
    /// Reply to [`Request::HistoryChannels`]: every roster venue's rows as this server's build
    /// declares them, with the server's overlay. See [`crate::history::HistoryChannelsReport`]. A
    /// store that cannot answer the `held` half's inventory read is [`Response::Error`], the answer
    /// [`Request::Inventory`] itself gives.
    HistoryChannels(crate::history::HistoryChannelsReport),
    /// Reply to [`Request::GatewayStatus`]: every outcome, a gatewayhub that is down or stalled
    /// included. See [`crate::gateway_status::GatewayStatusReply`].
    GatewayStatus(crate::gateway_status::GatewayStatusReply),
    /// Reply to [`Request::GatewayLinks`]. A store with no such table yet answers no rows; one that will not read is
    /// [`Response::Error`]. See [`crate::gateway_links::GatewayLinksReply`].
    GatewayLinks(crate::gateway_links::GatewayLinksReply),
    /// Reply to [`Request::SeedSeries`]: what the server did. See [`SeedDone`] for the field
    /// contract — and note that an UNARMED server answers this variant rather than an
    /// [`Response::Error`], which [`FEATURE_SEED_SERIES`] argues is the point rather than a
    /// leniency. A validation or fetch FAILURE is [`Response::Error`], never a partial `SeedDone`.
    SeriesSeeded(SeedDone),
    /// Reply to [`Request::VenueCatalog`]: one venue's instruments, or the typed reason there are
    /// none. See [`crate::catalog::CatalogListing`].
    ///
    /// ⚠ **Every ROUTINE outcome is this variant, including the refusals** — an unarmed lane, a
    /// venue with no bulk list, a credentialed venue and a venue this build does not serve are all
    /// [`crate::catalog::CatalogOutcome`] values: the Data Manager asks "which venues can I
    /// refresh" every time it opens, and a stream of errors is the wrong shape for that.
    /// [`Response::Error`] is for the exceptional — a malformed venue string, or a provider that
    /// failed mid fetch.
    VenueCatalog(crate::catalog::CatalogListing),
    /// Reply to [`Request::OptionsChain`]: one underlying's chain snapshot, or the typed reason
    /// there is none. See [`crate::options::OptionsChainReply`].
    ///
    /// ⚠ **Every ROUTINE outcome is this variant, including the refusals** — an unarmed build, a
    /// venue or underlying the lane does not serve, too many expiries and a snapshot not yet landed
    /// are all [`crate::options::OptionsChainOutcome`] values, [`Response::VenueCatalog`]'s rule: the
    /// Options tool asks every second. [`Response::Error`] is for a MALFORMED request alone.
    OptionsChain(crate::options::OptionsChainReply),
    /// Reply to [`Request::OptionsRefresh`]: the underlyings the venue lists now, in the listing's order,
    /// which the lane serves from this answer on.
    OptionsRefreshed {
        /// The venue, echoed.
        venue: String,
        /// The listed underlyings, in the listing's order (most listed options first).
        underlyings: Vec<String>,
    },
    /// Reply to [`Request::NewsFeed`]: the news rows, or the typed reason there are none. See
    /// [`crate::tool_feeds::NewsReply`]. Every ROUTINE outcome is this variant, refusals included —
    /// [`Response::OptionsChain`]'s rule; [`Response::Error`] is for a MALFORMED request alone.
    NewsFeed(crate::tool_feeds::NewsReply),
    /// Reply to [`Request::EconomicCalendar`]. See [`crate::tool_feeds::CalendarEconomicReply`];
    /// [`Response::NewsFeed`]'s rule.
    EconomicCalendar(crate::tool_feeds::CalendarEconomicReply),
    /// Reply to [`Request::EquityCalendar`]: three feeds, each with its own outcome. See
    /// [`crate::tool_feeds::CalendarEquityReply`]; [`Response::NewsFeed`]'s rule.
    EquityCalendar(crate::tool_feeds::CalendarEquityReply),
    /// Reply to [`Request::DeleteSeries`]: the plan, and what it did. See [`DeleteDone`]. A
    /// provenance REFUSAL, a key-less server and an unknown kind are all [`Response::Error`] —
    /// nothing was deleted in any of those cases, and this variant never reports a run that did not
    /// happen.
    Deleted(DeleteDone),
    /// Reply to [`Request::ImportArchive`]: the PLAN, and — unless the request was a plan-only dry
    /// run — what the import half did. See [`crate::archive::ImportDone`]. A WHOLE-REQUEST refusal
    /// (an unknown format, an invalid dataset, an instrument the format cannot scale, a window over
    /// the day cap, a second concurrent import) is [`Response::Error`]; a refused DAY is a value
    /// inside this answer, because the request carried on past it.
    ///
    /// ⚠ BOXED for `clippy::large_enum_variant`: a plan carries
    /// several strings and lists. The wire shape is unchanged.
    ArchiveImported(Box<crate::archive::ImportDone>),
    /// Reply to [`Request::FetchArchive`]: the plan, and what the download did when it ran. A
    /// whole-request refusal (no AWS key stored, a key the bucket refuses, a format with no download)
    /// is [`Response::Error`]; a file that failed is a value inside this answer. Boxed for
    /// [`Response::ArchiveImported`]'s reason.
    ArchiveFetched(Box<crate::archive_fetch::FetchDone>),
    /// Reply to [`Request::MarketDataSubscribe`]. **POSITIONAL, and the LAST positional frame this socket
    /// will carry** — see [`crate::market`]'s module doc.
    MarketDataSubscribed {
        /// The session token, for a later [`Request::MarketDataUpdate`] on a SHORT-LIVED connection.
        session: crate::market::MarketDataSessionId,
        /// The specs now served, echoing the server's AUTHORITATIVE form — including any CLAMPED
        /// `depth_levels`. A clamp is an acceptance with a smaller number, never a refusal, which
        /// is what lets a client that asked for 200 levels LEARN it got 50.
        accepted: Vec<crate::market::MarketDataSpec>,
        /// The specs refused, each with its own typed reason.
        refused: Vec<(crate::market::MarketDataSpec, crate::market::MarketDataRefusal)>,
        /// **The server's own heartbeat period in milliseconds.**
        ///
        /// ⚠ It keeps `crate::market::MARKET_DATA_READ_TIMEOUT` from being a COMPILE-TIME contract between
        /// two independently-deployed binaries: the client arms `max(MARKET_DATA_READ_TIMEOUT, 3 ×
        /// heartbeat_ms)`. (The ACCOUNT plane's `vike_tradehub_client::liveness`'s
        /// `OBSERVE_READ_TIMEOUT` is a constant on both sides, a hazard its own doc names.)
        heartbeat_ms: u64,
    },
    /// Reply to [`Request::MarketDataUpdate`]. **POSITIONAL**, on the short-lived control connection the
    /// request arrived on — the frames themselves go to the STREAM connection.
    MarketDataUpdated {
        /// Newly-served specs, in the server's authoritative form.
        accepted: Vec<crate::market::MarketDataSpec>,
        /// Refused additions, each with its own typed reason.
        refused: Vec<(crate::market::MarketDataSpec, crate::market::MarketDataRefusal)>,
        /// The specs this update actually RELEASED — matched on their key, so a `remove` naming a
        /// spec the session never held is silently absent here rather than an error.
        released: Vec<crate::market::MarketDataSpec>,
    },
    /// One PUSHED market-data frame. Appears ONLY on a stream connection, and after
    /// [`Response::MarketDataSubscribed`] it is the only variant that appears there at all — which makes a
    /// stream reader's `match` on this variant a complete protocol, with anything else a desync.
    ///
    /// BOXED on [`Response::Properties`]'s argument: `Response` is cloned and matched on every
    /// POSITIONAL path and should not grow for a variant one kind of connection sees
    /// (`crate::market::MarketDataFrame`'s largest variant is ~128 B). The wire shape is unchanged.
    MarketData(Box<crate::market::MarketDataFrame>),
    /// Reply to [`Request::StrategyOpen`]. **POSITIONAL, and the LAST positional frame this socket will
    /// carry**: every frame after it is the tube's (`vike_market_stream::wire::StreamDown` from the
    /// daemon, `StreamUp` from the client). There is NO `Response::Strategy` frame variant: the tube's own
    /// frames are the lane's grammar, so `Response` carries no pushed strategy frame and the market-data
    /// stream's `Response::MarketData` is never sent on a strategy-lane socket.
    StrategyOpened {
        /// The daemon's name for this lane session (a log and diagnostics key, nothing else).
        session: u64,
        /// **The daemon's own heartbeat period in milliseconds**: the client arms
        /// `max(STRATEGY_LANE_READ_FLOOR, 3 x heartbeat_ms)` against the next frame, clamped into
        /// `STRATEGY_LANE_HEARTBEAT_MIN..=STRATEGY_LANE_HEARTBEAT_MAX`. A silent lane is a lost lane, and a lost lane is a
        /// `Disconnected` for the consumer's dead-man.
        heartbeat_ms: u64,
    },
    /// Reply to [`Request::CsvUpload`]: the upload is open, and the file's text may follow.
    CsvUploadReady,
    /// Reply to [`Request::CsvUploadChunk`]: the bytes of the file received so far.
    CsvUploadReceived {
        /// The running total.
        bytes: u64,
    },
    /// Reply to [`Request::CsvUploadEnd`]: how the file was read and what was stored. A file the
    /// decoder refuses is answered here too, with the refusal and the file's header (see
    /// [`crate::upload::CsvUploadDone`]); a refusal of the upload itself is [`Response::Error`].
    /// BOXED for `clippy::large_enum_variant`.
    CsvUploaded(Box<crate::upload::CsvUploadDone>),
    /// Reply to [`Request::SymbolSets`], [`Request::SetSymbolSet`] and [`Request::DeleteSymbolSet`]: every
    /// saved list, in the order they were first saved.
    SymbolSets(Vec<vike_data::datasets::SymbolSet>),
    /// Reply to [`Request::InstrumentLists`]. See [`crate::catalog::InstrumentListsReply`].
    InstrumentLists(crate::catalog::InstrumentListsReply),
    /// Reply to [`Request::RefreshInstrumentLists`]: what each venue's refresh did, in the order asked.
    InstrumentListsRefreshed(Vec<crate::catalog::VenueRefresh>),
    /// Reply to [`Request::OperatorInstruments`]. See [`crate::catalog::OperatorInstrumentsReply`].
    OperatorInstruments(crate::catalog::OperatorInstrumentsReply),
}

#[path = "proto_tests.rs"]
#[cfg(test)]
mod proto_tests;
