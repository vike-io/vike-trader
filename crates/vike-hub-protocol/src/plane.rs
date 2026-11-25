//! Which hub answers which verb: the two daemons that speak the keyed data-plane handshake (the
//! datahub and the backtesthub), how a client tells them apart before authenticating, and the refusal
//! each answers a verb of the other with.
//!
//! Stated BELOW both clients because they cannot see each other (decision 0252's verdict 4), so a table
//! kept in one would be a second copy in the other, and the first divergence is a verb BOTH refuse.
//! Nothing here knows what a verb DOES; these are names, and a name is what crosses the wire first.
//!
//! # ⚠ A verb of the other plane does not DECODE, and the refusal must not read as if it failed to
//!
//! Each daemon's `Request` enum carries only its own verbs, so serde answers a verb of the other plane
//! with `unknown variant ..., expected one of ...`: true, and useless to an operator who dialled the
//! wrong port. So a daemon asks [`foreign_verb`] on the decode's `Err` arm, BEFORE it words the
//! undecodable refusal: the frame's outer tag is read off the body ([`frame_tag`]), and a tag on the
//! other plane's roster gets [`wrong_plane_message`], with the connection kept. A tag on neither roster
//! is what it always was: an unknown verb from a newer peer, answered `undecodable`. Each client's
//! `proto/verbs_tests.rs` holds its own enum's tags equal to its roster here, off serde's own
//! `expected one of` list, so a verb cannot join one and miss the other.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, IgnoredAny, MapAccess, Visitor};

/// Which daemon serves a verb, or which daemon a `Welcome` says answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// The DATA plane: the history and catalog reads, the store writes and the store removal. Served by
    /// `vike-backend datahub`.
    Data,
    /// The BACKTEST plane: the verbs that run an engine, or answer out of the compiled-in strategy
    /// roster. Served by `vike-backend backtesthub --addr`.
    Backtest,
}

impl Plane {
    /// The command that SERVES this plane, for the refusal a client gets when it dialled the other
    /// daemon: a command rather than a port, so an operator can start the missing daemon without
    /// looking anything up.
    pub fn served_by(self) -> &'static str {
        match self {
            Plane::Data => "vike-backend datahub",
            Plane::Backtest => "vike-backend backtesthub --addr",
        }
    }

    /// The settings key naming the address this plane's daemon is dialled at: the OTHER half of an
    /// actionable refusal, what to start and where the client is pointing.
    pub fn address_key(self) -> &'static str {
        match self {
            Plane::Data => "config.datahub_addr",
            Plane::Backtest => "config.backtest_addr",
        }
    }

    /// The wire tags of this plane's own verbs, frozen tokens all (a tag is what a deployed peer sends).
    /// `Hello`, `Auth` and `Ping` are on neither: both daemons serve them.
    pub fn verbs(self) -> &'static [&'static str] {
        match self {
            Plane::Data => DATA_PLANE_VERBS,
            Plane::Backtest => BACKTEST_PLANE_VERBS,
        }
    }

    fn other(self) -> Plane {
        match self {
            Plane::Data => Plane::Backtest,
            Plane::Backtest => Plane::Data,
        }
    }
}

/// [`Plane::Data`]'s verb tags. The roster `vike_datahub_client::proto::Request` is held to.
const DATA_PLANE_VERBS: &[&str] = &[
    "LoadBars",
    "ScanQuotes",
    "ScanTrades",
    "ScanBookUpdates",
    "ScanDepth",
    "ScanCohort",
    "ScanPerpMetrics",
    "ScanTexts",
    "ScanChain",
    "ScanFinancialFacts",
    "ScanInstitutionalHoldings",
    "ScanTextScores",
    "ScanForecasts",
    "ScanEquity",
    "ScanExecFills",
    "ScanExecOrders",
    "PropertiesAsOf",
    "ListSeries",
    "Inventory",
    "ExecutionSeries",
    "SeriesFacts",
    "SeriesDays",
    "SeriesGaps",
    "Coverage",
    "Backfill",
    "ListBackfills",
    "CancelBackfill",
    "HistoryChannels",
    "GatewayStatus",
    "GatewayLinks",
    "CredentialsChanged",
    "ImportArchive",
    "FetchArchive",
    "SeedSeries",
    "VenueCatalog",
    "OptionsChain",
    "OptionsRefresh",
    "NewsFeed",
    "EconomicCalendar",
    "EquityCalendar",
    "DeleteSeries",
    "MarketDataSubscribe",
    "MarketDataUpdate",
    "StrategyOpen",
    "StrategyFundingHistory",
    "StrategyInputBars",
    "StrategySymbolFacts",
    "StrategyForecasts",
    "CsvUpload",
    "CsvUploadChunk",
    "CsvUploadEnd",
    "SymbolSets",
    "SetSymbolSet",
    "DeleteSymbolSet",
    "InstrumentLists",
    "InstrumentListsOfVenues",
    "KeptRowsOfVenues",
    "RefreshInstrumentLists",
    "OperatorInstruments",
    "SplitWarnings",
    "LiveFamilyTokens",
    "ExecutionStreamOpen",
    "ExecutionStreams",
    "ExecutionAccountPositions",
];

/// [`Plane::Backtest`]'s verb tags, the wire's spelling (`RunSweep` is `Request::RunParamscan` in Rust).
/// The roster `vike_backtesthub_client::proto::Request` is held to.
const BACKTEST_PLANE_VERBS: &[&str] = &[
    "RunBacktest",
    "RunSlice",
    "RunParamscan",
    "RunWalkforward",
    "RunParamscanProfile",
    "RunWalkforwardProfile",
    "RunStudy",
    "ListStrategies",
    "NamedStrategies",
    "RunNamed",
    "ListStrategyFiles",
    "ReadStrategyFile",
    "SaveStrategyFile",
    "BuildStrategyFile",
    "Summarized",
    "ReadTrades",
    "ReadPoint",
    "ReadCurve",
    "ListStudies",
    "ListRuns",
    "ReadRunArtifact",
    "ListSdks",
    "ReadSdkFile",
    "ReadParamscanRows",
    "Fresh",
];

/// The capability the backtesthub pushes UNCONDITIONALLY in its `Welcome`, so a client learns BEFORE
/// authenticating which daemon answered.
///
/// ⚠ A constant rather than a literal because something REFUSES on it: `HubConnection::connect_authed_on`
/// will not sign a key toward a server whose plane [`welcome_plane`] cannot confirm, so a sentinel
/// renamed on one side only would refuse EVERY keyed Studio Run. The VALUE is the frozen token it always
/// was; an older peer compares it literally.
pub const BACKTEST_PLANE_SENTINEL: &str = "backtest";

/// The DATA daemon's unconditional twin of [`BACKTEST_PLANE_SENTINEL`]. Its presence in a `Welcome` is
/// what [`welcome_plane`] reads as "this socket reaches the store": the plane that carries `Backfill` (a
/// store write spending venue budget) and `DeleteSeries` (the destructive verb) at `VerbScope::Write`.
pub const DATA_PLANE_SENTINEL: &str = "load_bars";

/// **Which plane a server's PRE-AUTH `Welcome` says it serves**, or `None` when the feature list does
/// not name exactly one.
///
/// `Some(Plane::Backtest)` needs [`BACKTEST_PLANE_SENTINEL`] AND the absence of [`DATA_PLANE_SENTINEL`];
/// `Some(Plane::Data)` the mirror image. BOTH present is a pre-split daemon (one process serving both
/// planes, from before ruling 7) and answers `None` deliberately: it serves `Backfill` and
/// `DeleteSeries` too, so a caller that must not open a Write session on the data plane cannot treat it
/// as a backtesthub. NEITHER present is an unknown peer: `None`.
///
/// It reads the `Welcome` because that is the one frame both daemons send before authentication.
pub fn welcome_plane(features: &[String]) -> Option<Plane> {
    let has = |f: &str| features.iter().any(|x| x == f);
    match (has(BACKTEST_PLANE_SENTINEL), has(DATA_PLANE_SENTINEL)) {
        (true, false) => Some(Plane::Backtest),
        (false, true) => Some(Plane::Data),
        (true, true) | (false, false) => None,
    }
}

/// The `Response::Error` text a daemon answers a verb from the OTHER plane with: one spelling, so the
/// two refusals mirror each other and neither drifts into being less useful.
///
/// A wrong-plane dial is a CONFIGURATION mistake, so it names each step of the fix: the verb, the
/// daemon that serves it, and the settings key the client dialled from. It deliberately prints NO
/// address: this side knows only where IT listens, and a tunnelled client's port may differ. It rides an
/// `Error` frame, never a dropped connection: the decode-vs-drop contract.
pub fn wrong_plane_message(verb: &str, serves: Plane, wanted: Plane) -> String {
    format!(
        "{verb} is a {wanted:?}-plane verb and this is the {serves:?} daemon — it is served by \
         `{}`. Point the client at that daemon ({}); this connection stays open and every \
         {serves:?}-plane verb still works on it.",
        wanted.served_by(),
        wanted.address_key(),
    )
}

/// The outer tag of an externally-tagged frame body (`"Ping"` or `{"RunBacktest":...}`), or `None` when
/// the body has no such shape. The payload is skipped unread: this runs on a frame that already failed
/// to decode, and a refusal must not cost a tree the size of the frame.
pub fn frame_tag(body: &[u8]) -> Option<String> {
    struct Tag(String);
    struct TagVisitor;
    impl<'de> Visitor<'de> for TagVisitor {
        type Value = Tag;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a variant name, or an object with one key")
        }

        fn visit_str<E: de::Error>(self, tag: &str) -> Result<Tag, E> {
            Ok(Tag(tag.to_string()))
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Tag, A::Error> {
            let tag: String =
                map.next_key()?.ok_or_else(|| de::Error::custom("an empty object has no tag"))?;
            map.next_value::<IgnoredAny>()?;
            Ok(Tag(tag))
        }
    }
    impl<'de> Deserialize<'de> for Tag {
        fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            d.deserialize_any(TagVisitor)
        }
    }
    // In whichever codec the body is written (`crate::frame::BodyCodec`): a msgpack verb names its tag the same way.
    crate::frame::decode_body::<Tag>(body).ok().map(|t| t.0)
}

/// The verb of the OTHER plane this body names, or `None`. Asked by a daemon on `here` when the body did
/// not decode into its own `Request` (the module doc).
pub fn foreign_verb(body: &[u8], here: Plane) -> Option<&'static str> {
    let tag = frame_tag(body)?;
    here.other().verbs().iter().copied().find(|verb| *verb == tag)
}

#[cfg(test)]
#[path = "plane_tests.rs"]
mod plane_tests;
