//! The blocking datahub client: a [`HubConnection`] that speaks the [`crate::proto`] request/response
//! protocol.
//!
//! The "thin client" half of the compute-to-data design: it ships a typed read query or a store verb
//! and receives a compact answer, never a raw UPSTREAM data slice (a read verb returns exactly the
//! query's bounded result). One client owns one connection and issues requests sequentially; it is not
//! internally synchronized, so share it per thread.
//!
//! Connecting, authenticating and bounding a reply are [`HubConnection`]'s, shared with the backtesthub's
//! client (`crates/vike-hub-protocol/src/conn.rs`'s module doc carries each rule). [`DatahubClient`] derefs
//! to it, so `features`, `set_reply_timeout`, `cancel_handle`, `is_usable`, `requires_auth` and
//! `authenticated_scope` are called on the client itself; this file adds the constructors that name this
//! protocol's version and [`exchange`](DatahubClient::exchange), which names each verb and its bound.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::ops::{Deref, DerefMut};
use std::time::Duration;

use vike_data::store::removal::SeriesSelector;
use vike_data::{
    CohortRow, ExecFillRow, ExecOrderRow, InstrumentCoverage, PerpMetricRow, SeriesCoverage,
    SeriesId, TimestampRange,
};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{EquitySample, StreamLabel, SymbolProperties};

use crate::archive::{ImportDone, ImportSpec, ImportTarget, validate_import_spec};
use crate::catalog::CatalogListing;
use crate::gateway_links::GatewayLinksReply;
use crate::gateway_status::GatewayStatusReply;
use crate::history::HistoryChannelsReport;
use crate::market::{
    MARKET_DATA_READ_TIMEOUT, MarketDataRefusal, MarketDataSessionId, MarketDataSpec,
    validate_market_data_symbol,
};
use crate::options::{OptionsChainReply, validate_options_request};
use crate::proto::{
    BackfillCancelDone, BackfillDone, BackfillRunWire, DeleteDone, FEATURE_ARCHIVE_IMPORT,
    FEATURE_BACKFILL, FEATURE_BACKFILL_CANCEL, FEATURE_BACKFILL_FUNDING, FEATURE_BAR_SOURCE,
    FEATURE_COVERAGE, FEATURE_CSV_COLUMNS, FEATURE_CSV_UPLOAD, FEATURE_DELETE_SERIES,
    FEATURE_EXECUTION_MATERIALIZE, FEATURE_GATEWAY_LINKS, FEATURE_GATEWAY_STATUS,
    FEATURE_HISTORY_CHANNELS, FEATURE_IMPORT_REPLACE_HOLED, FEATURE_INSTRUMENT_LISTS,
    FEATURE_MARKET_DATA, FEATURE_OPERATOR_INSTRUMENTS, FEATURE_OPTIONS_CHAIN,
    FEATURE_OPTIONS_REFRESH, FEATURE_SCAN_EXECUTION_FILLS_RANGE, FEATURE_SCAN_EXECUTION_ORDERS,
    FEATURE_SEED_CLASS, FEATURE_SEED_SERIES, FEATURE_SOURCE, FEATURE_SYMBOL_SETS,
    FEATURE_TOOL_FEEDS, FEATURE_VENUE_CATALOG, PROTO_VERSION, Request, Response, SeedDone,
    advertised_import_formats, reply_wait, request_kind,
};
use crate::tool_feeds::{
    CalendarEconomicReply, CalendarEquityReply, NewsReply, validate_equity_week_shape,
};
use crate::upload::{CsvUploadDone, CsvUploadSpec, validate_upload_spec};
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::conn::HubConnection;
use vike_hub_protocol::plane::Plane;

/// A connected datahub client: a [`HubConnection`] speaking this protocol. Drop it to close the connection.
#[derive(Debug)]
pub struct DatahubClient {
    conn: HubConnection,
}

impl Deref for DatahubClient {
    type Target = HubConnection;

    fn deref(&self) -> &HubConnection {
        &self.conn
    }
}

impl DerefMut for DatahubClient {
    fn deref_mut(&mut self) -> &mut HubConnection {
        &mut self.conn
    }
}

impl DatahubClient {
    /// [`HubConnection::connect`], speaking [`PROTO_VERSION`]. Against a KEYED server this fails HERE, with an
    /// actionable message; use [`connect_authed`](Self::connect_authed) for those.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        Ok(Self { conn: HubConnection::connect(addr, PROTO_VERSION)? })
    }

    /// [`HubConnection::connect_authed`], speaking [`PROTO_VERSION`]. ⚠ A KEY-LESS server is a successful
    /// unauthenticated connect, not an error; a caller that must be sure it authenticated checks
    /// `authenticated_scope`.
    pub fn connect_authed<A: ToSocketAddrs>(
        addr: A,
        keys: &HubKeys,
        scope: Scope,
    ) -> io::Result<Self> {
        Ok(Self { conn: HubConnection::connect_authed(addr, keys, scope, PROTO_VERSION)? })
    }

    /// [`HubConnection::connect_with_keys`], speaking [`PROTO_VERSION`]: `connect_authed` under `scope` when
    /// `keys` is `Some`, plain `connect` when it is `None`. ⚠ UNGUARDED on the plane, as every dial of this
    /// client but the strategy lane's is.
    pub fn connect_with_keys<A: ToSocketAddrs>(
        addr: A,
        keys: Option<&HubKeys>,
        scope: Scope,
    ) -> io::Result<Self> {
        Ok(Self { conn: HubConnection::connect_with_keys(addr, keys, scope, PROTO_VERSION)? })
    }

    /// The idle reply bound a verb arms for `request` on this client before writing it: the caller's
    /// `set_reply_timeout` value, else the per-verb default ([`crate::proto::reply_wait`]'s class).
    pub fn reply_timeout_for(&self, request: &Request) -> Option<Duration> {
        self.conn.reply_bound(reply_wait(request).default_bound())
    }

    /// **The ONE post-handshake exchange of this protocol**: [`HubConnection::exchange_frame`] under the verb's
    /// wire tag and its [`crate::proto::reply_wait`] class. Every positional verb goes through here.
    fn exchange(&mut self, request: &Request) -> io::Result<Response> {
        self.conn.exchange_frame(
            request_kind(request),
            reply_wait(request).default_bound(),
            request,
        )
    }

    /// Liveness probe: send [`Request::Ping`] and assert the server answers [`Response::Pong`].
    /// A non-`Pong` reply is an [`io::ErrorKind::InvalidData`] protocol error.
    pub fn ping(&mut self) -> io::Result<()> {
        match self.exchange(&Request::Ping)? {
            Response::Pong => Ok(()),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected Pong, got {}", response_kind(&other)),
            )),
        }
    }
}

// `DatahubClient`'s verbs, split by concern; this file keeps the struct, the constructors and the
// helpers the verbs share.
mod backfill;
mod data_ops;
mod market_data;
mod reads;
mod strategy_lane;

/// What a successful [`DatahubClient::market_data_subscribe`] learned — the positional answer, unpacked, so a
/// caller never has to match `Response` again on a socket that has stopped carrying one.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketDataSubscribedInfo {
    /// The session token, for a later [`DatahubClient::market_data_update`] on a SHORT-LIVED connection.
    pub session: MarketDataSessionId,
    /// The specs now served, in the server's AUTHORITATIVE form — depths already clamped.
    pub accepted: Vec<MarketDataSpec>,
    /// The specs refused, each with its own typed reason. ⚠ A non-empty list is NOT a failure: a
    /// subscribe with one bad spec still opens the stream for the good ones, and
    /// [`MarketDataRefusal::is_permanent`] is what decides whether the refused ones stay in the caller's
    /// desired set for a retry.
    pub refused: Vec<(MarketDataSpec, MarketDataRefusal)>,
    /// The server's own heartbeat period. The read deadline is already armed from it; this is here
    /// so a caller can LOG what it agreed to.
    pub heartbeat_ms: u64,
}

/// What a [`DatahubClient::market_data_update`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketDataUpdatedInfo {
    /// Newly-served specs, authoritative form.
    pub accepted: Vec<MarketDataSpec>,
    /// Refused additions.
    pub refused: Vec<(MarketDataSpec, MarketDataRefusal)>,
    /// The specs actually RELEASED — a `remove` naming a spec the session never held is absent here
    /// rather than an error.
    pub released: Vec<MarketDataSpec>,
}

/// Refuse a market-data request LOCALLY when any spec's symbol is not one this wire carries —
/// [`crate::market::validate_market_data_symbol`], the same function the server's door calls.
///
/// ⚠ The message names the spec's POSITION, venue and lane, never the symbol: the symbol is the
/// unbounded field, and echoing it into an error a caller logs moves the cost rather than refusing
/// it (`validate_market_data_symbol` states the rule in full).
fn refuse_an_unadvertised_quote_lane(
    verb: &str,
    specs: &[MarketDataSpec],
    features: &[String],
) -> Result<(), String> {
    if specs.iter().any(|s| s.lane == StreamLabel::Quotes)
        && !features.iter().any(|f| f == crate::proto::FEATURE_MARKET_DATA_LANE_QUOTES)
    {
        return Err(format!(
            "{verb}: this datahub does not advertise `{}`, so it could not decode a spec on the quote \
             lane. Nothing was sent. Upgrade the datahub to serve it.",
            crate::proto::FEATURE_MARKET_DATA_LANE_QUOTES
        ));
    }
    Ok(())
}

fn refuse_bad_symbols(verb: &str, specs: &[MarketDataSpec]) -> Result<(), String> {
    for (i, s) in specs.iter().enumerate() {
        if let Err(why) = validate_market_data_symbol(&s.symbol) {
            return Err(format!(
                "{verb}: spec {i} ({}, {:?}) — {why} The connection is unchanged and nothing was \
                 sent.",
                s.venue, s.lane
            ));
        }
    }
    Ok(())
}

/// The variant NAME of a response (no payload) — for protocol-desync error messages, so we never
/// stringify a whole (possibly large) `Bars`/`Quotes`/`Trades` payload just to report "unexpected
/// variant".
fn response_kind(r: &Response) -> &'static str {
    match r {
        Response::Welcome { .. } => "Welcome",
        Response::AuthOk { .. } => "AuthOk",
        Response::AuthDenied { .. } => "AuthDenied",
        Response::Pong => "Pong",
        Response::Error(_) => "Error",
        Response::Bars(_) => "Bars",
        Response::Quotes(_) => "Quotes",
        Response::Trades(_) => "Trades",
        Response::BookUpdates(_) => "BookUpdates",
        Response::Depth(_) => "Depth",
        Response::Cohort(_) => "Cohort",
        Response::PerpMetrics(_) => "PerpMetrics",
        Response::Equity(_) => "Equity",
        Response::ExecFills(_) => "ExecFills",
        Response::ExecOrders(_) => "ExecOrders",
        Response::Properties(_) => "Properties",
        Response::SeriesList(_) => "SeriesList",
        Response::Inventory(_) => "Inventory",
        Response::SeriesGaps(_) => "SeriesGaps",
        Response::SeriesFacts(_) => "SeriesFacts",
        Response::Coverage(_) => "Coverage",
        Response::BackfillDone(_) => "BackfillDone",
        Response::RunningBackfills(_) => "RunningBackfills",
        Response::BackfillsCancelled(_) => "BackfillsCancelled",
        Response::HistoryChannels(_) => "HistoryChannels",
        Response::GatewayStatus(_) => "GatewayStatus",
        Response::GatewayLinks(_) => "GatewayLinks",
        Response::SeriesSeeded(_) => "SeriesSeeded",
        Response::VenueCatalog(_) => "VenueCatalog",
        Response::OptionsChain(_) => "OptionsChain",
        Response::OptionsRefreshed { .. } => "OptionsRefreshed",
        Response::NewsFeed(_) => "NewsFeed",
        Response::EconomicCalendar(_) => "EconomicCalendar",
        Response::EquityCalendar(_) => "EquityCalendar",
        Response::Deleted(_) => "Deleted",
        Response::ArchiveImported(_) => "ArchiveImported",
        Response::ArchiveFetched(_) => "ArchiveFetched",
        Response::MarketDataSubscribed { .. } => "MarketDataSubscribed",
        Response::MarketDataUpdated { .. } => "MarketDataUpdated",
        Response::StrategyOpened { .. } => "StrategyOpened",
        Response::MarketData(_) => "MarketData",
        Response::CsvUploadReady => "CsvUploadReady",
        Response::CsvUploadReceived { .. } => "CsvUploadReceived",
        Response::CsvUploaded(_) => "CsvUploaded",
        Response::SymbolSets(_) => "SymbolSets",
        Response::InstrumentLists(_) => "InstrumentLists",
        Response::InstrumentListsRefreshed(_) => "InstrumentListsRefreshed",
        Response::OperatorInstruments(_) => "OperatorInstruments",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_hub_protocol::conn::REPLY_IDLE_TIMEOUT;
    use vike_hub_protocol::limits::CLIENT_HANDSHAKE_DEADLINE;

    /// The default table follows `reply_wait`'s class: a server-bounded verb gets
    /// [`REPLY_IDLE_TIMEOUT`], a client-sized one none, a handshake frame the handshake's bound.
    #[test]
    fn the_reply_bound_is_the_verb_class() {
        let default = |r: &Request| reply_wait(r).default_bound();
        assert_eq!(default(&Request::Ping), Some(REPLY_IDLE_TIMEOUT));
        assert_eq!(default(&Request::Inventory), Some(REPLY_IDLE_TIMEOUT));
        let backfill = Request::Backfill {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            start: 0,
            end: 1,
        };
        assert_eq!(default(&backfill), None, "a client-sized verb stays unbounded");
        assert_eq!(
            default(&Request::Hello { proto_version: PROTO_VERSION }),
            Some(CLIENT_HANDSHAKE_DEADLINE)
        );
    }

    /// The P4.5c renames changed this wire's bytes, so the version stepped 7 -> 8: a daemon from
    /// before them is refused at the handshake, naming both numbers, never at a decode later.
    #[test]
    fn a_daemon_from_before_the_phase_four_renames_is_refused_at_the_handshake() {
        assert_eq!(PROTO_VERSION, 8);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            let _hello: Request = vike_hub_protocol::frame::read_frame(&mut s).expect("Hello");
            let welcome = Response::Welcome { proto_version: 7, features: vec![], nonce: None };
            vike_hub_protocol::frame::write_frame(&mut s, &welcome).expect("Welcome");
        });
        let Err(err) = DatahubClient::connect(addr) else { panic!("a v7 daemon must be refused") };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("client speaks 8, server speaks 7"), "{err}");
    }
}
