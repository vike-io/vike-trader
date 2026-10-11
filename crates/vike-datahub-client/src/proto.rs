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
//! A client ships a small CONFIG next to where the data lives and gets back a compact ANSWER, never
//! a raw upstream data slice:
//!
//! - [`Request::RunBacktest`] ships a profile as its TOML text and gets a [`Response::Report`]
//!   (JSON text).
//! - [`Request::RunSlice`] / [`Request::RunParamscan`] / [`Request::RunWalkforward`] are the STUDIO
//!   verbs: they ship the [`WireSpec`]/[`WireSlice`]/[`WireEngineParams`] DTOs (see
//!   [`crate::wire_studio`]) plus a [`WireParamscan`] grid or a [`WireWalkforward`] split count,
//!   and get back the rendered [`WireRunResult`] / ranked / stitched answer — never the bars or
//!   ticks. Served ONLY by a `serve-datafusion` build; a lean build still DECODES them and answers
//!   [`Response::Error`], so a mismatched client is never dropped.
//! - [`Request::RunParamscanProfile`] / [`Request::RunWalkforwardProfile`] are their PROFILE-shaped
//!   twins: the profile's TOML text crosses verbatim and the SERVER parses it with
//!   `BacktestProfile::from_toml_str` before running `vike_backtest::harness::run_paramscan` /
//!   `harness::run_walkforward`, so the WHOLE `[engine]` surface applies — the `fee` SCHEDULE,
//!   `[engine.impact]`, `[engine.resolution]`, `[risk]`, `snap_to_properties`, tick mode,
//!   cross-venue `[[data.series]]` — none of which a `WireSlice` + `WireEngineParams` pair can
//!   carry, under ONE parser. Served on EVERY build, like `RunBacktest`: the harness is
//!   DataFusion-free.
//! - The READ verbs ([`Request::LoadBars`], [`Request::ScanQuotes`], [`Request::ScanTrades`],
//!   [`Request::PropertiesAsOf`] and their siblings) mirror the [`vike_data::HistStore`] read
//!   methods and return the exact typed `vike-model` result. They are the
//!   [`RemoteHistStore`](crate::remote::RemoteHistStore) seam: a chart's bars ARE the answer,
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
    CohortRow, ExecFillRow, InstrumentCoverage, PerpMetricRow, SeriesCoverage, SeriesId,
};

/// The removal vocabulary, RE-EXPORTED rather than restated, so a consumer can CONSTRUCT a
/// [`Request::DeleteSeries`] without a `vike-data` dependency edge of its own: `vike-cli`, whose
/// identity is being DataFusion-free, takes `vike-data` as a DEV-dependency only. A re-export adds
/// no package and no edge (`vike-data` at default features is already in that graph through this
/// crate), where flattened DTOs here would be a second shape for the same facts.
pub use vike_data::store::removal::{RemovalOutcome, RemovalPlan, SeriesSelector, describe_id};
/// The `--produced-by` VALIDATOR, re-exported for the same reason: both ends of the wire share one
/// definition of a valid producer filter (`crates/vike-datahub/src/server/delete.rs`'s
/// `delete_series_verb` resolves through it, so a BLANK spelling cannot turn a provenance-asserted
/// delete into a wildcard one), and `vike-cli` can name the rule its refusals enforce before a
/// socket is dialled. Its module has no `use` and sits outside every `hist-datafusion` gate, so it
/// adds no Arrow/DataFusion weight.
pub use vike_data::store::store_kind::resolve_produced_by;
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

// The capability ceiling carried by `Request::Auth` / `Response::AuthOk`, from the shared
// `vike_node_proto::auth`, so `proto::Scope` resolves here exactly as on the tradehub node's
// protocol.
pub use vike_node_proto::auth::Scope;

// The FRAME CODEC, re-exported as this protocol's own vocabulary — the shape
// `crates/vike-tradehub-client/src/proto.rs` uses for the same five items: a second codec would be
// a second answer to what a frame is. It lives in `vike-node-proto`, below both node protocols
// (`docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`). Public-API
// surface rather than an alias shim: a protocol module's frame codec IS its vocabulary.
pub use vike_node_proto::frame::{
    MAX_FRAME_LEN, read_frame, read_frame_raw, read_frame_raw_capped, write_frame,
};

use crate::wire_studio::{
    WireEngineParams, WireParamscan, WireParamscanResult, WireRunResult, WireSlice, WireSpec,
    WireWalkforward, WireWalkforwardResult,
};

mod features;
mod verbs;

pub use features::{
    DEFAULT_SEARCH_METHOD, FEATURE_ARCHIVE_IMPORT, FEATURE_AUTH, FEATURE_BACKFILL,
    FEATURE_BACKFILL_CANCEL, FEATURE_BACKFILL_FUNDING, FEATURE_COVERAGE, FEATURE_DELETE_SERIES,
    FEATURE_HISTORY_CHANNELS, FEATURE_IMPORT_FORMAT_PREFIX, FEATURE_MARKET_DATA,
    FEATURE_MD_VENUE_PREFIX, FEATURE_NAMED_RUN, FEATURE_REC_VENUE_PREFIX,
    FEATURE_SCAN_BOOK_UPDATES, FEATURE_SCAN_COHORT, FEATURE_SCAN_DEPTH, FEATURE_SCAN_EQUITY,
    FEATURE_SCAN_EXEC_FILLS, FEATURE_SCAN_LIMIT, FEATURE_SCAN_PERP_METRICS, FEATURE_SEARCH_METHOD,
    FEATURE_SEED_CLASS, FEATURE_SEED_SERIES, FEATURE_SERIES_FACTS, FEATURE_STUDY,
    FEATURE_VENUE_CATALOG, FEATURE_WALKFORWARD_SEARCH, SEARCH_METHODS, advertised_import_formats,
    advertised_md_venues, advertised_rec_venues, import_format_feature, md_venue_feature,
    rec_venue_feature,
};
pub use verbs::{
    COMPUTE_PLANE_SENTINEL, DATA_PLANE_SENTINEL, Plane, STUDIO_RUNNER_SENTINEL, VerbScope,
    plane_of, request_kind, required_scope, scope_admits, welcome_plane, wrong_plane_message,
};

/// The datahub wire protocol version, negotiated by the [`Request::Hello`] / [`Response::Welcome`]
/// handshake: the client fails the connection with a legible, both-numbers error on a mismatch (see
/// `DatahubClient::connect`). It changes ONLY for a change an old peer cannot DECODE; an additive
/// verb, field or advertisement is negotiated by a `Welcome.features` string instead
/// (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).
///
/// - `1` — Hello/Ping/RunBacktest + the four read verbs.
/// - `2` — [`Request::RunSlice`] + [`Response::RunResult`].
/// - `3` — [`Request::RunParamscan`] / [`Request::RunWalkforward`] + their results.
/// - `4` — [`Request::ListSeries`] / [`Request::Inventory`] / [`Request::SeriesGaps`] + answers.
/// - `5` — [`Request::ListStrategies`] + [`Response::Strategies`].
/// - `6` — the OPTIONAL `params` field on [`Request::RunParamscan`] / [`Request::RunWalkforward`]
///   (`#[serde(default)]`, so an old frame decodes as `None`).
/// - `7` — [`Request::RunParamscanProfile`] / [`Request::RunWalkforwardProfile`] + their reports.
/// - still `7`, each negotiated per decision 0112: `Backfill` (+ funding), `Coverage`, `Auth`,
///   `MdSubscribe`/`MdUpdate` (+ `md_venue=`), the `search` selector and `"multi"`, `RunStudy`, the
///   Studio walk-forward `search`, `rec_venue=`, `ImportArchive` (+ `import_format=`),
///   `ListBackfills`/`CancelBackfill`, `HistoryChannels`, and every later `FEATURE_*` verb.
/// - still `7`, and NOT A SCHEMA CHANGE: the `sweep` -> `paramscan` rename moved four Rust
///   identifiers (`RunSweep`/`RunSweepProfile`/`SweepResult`/`SweepReport`) and ZERO bytes — each
///   keeps its old tag by a serde rename, as does the `"sweep"` field key of
///   [`Request::RunParamscan`]. `crates/vike-datahub-client/tests/wire_tag_fixtures.rs` guards them
///   over committed frames in `fixtures/datahub_wire/`, out of reach of any edit under `crates/`;
///   it is the incident's record.
pub const PROTO_VERSION: u32 = 7;

/// A parameter search's SEARCH SELECTION as it crosses the wire: which method, and that method's
/// own knobs, carried as the operator's own TOKENS.
///
/// ⚠ **Strings, not typed scalars, and that is the whole point.** The one authority for what
/// `"128"` means as a `--trials` value — including the refusal text for `"abc"` — is
/// `vike_backtest::search::select`, on the SERVER. `vike-cli` has no `vike-backtest` dependency, so
/// typed fields would force a second parser with its own messages; these `String`s buy one parser
/// and one message on both routes.
///
/// ⚠ Not [`crate::wire_studio::WireParamscan`], the Studio's GRID: this is the METHOD that walks a
/// grid, and the grid stays in the profile's `[paramscan]` table. The method cannot live in the
/// profile: `vike_backtest::harness::BacktestProfile` is `#[serde(deny_unknown_fields)]`, and every
/// key of its parameter-grid table is an AXIS. The sibling `Request::RunWalkforwardProfile` carries
/// no selector: a second place to say "optimize" is a second place for the two to disagree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSearch {
    /// One of [`SEARCH_METHODS`]. `None` = [`DEFAULT_SEARCH_METHOD`].
    #[serde(default)]
    pub optimizer: Option<String>,
    /// euler's halving depth. Refused under any other method, by the server.
    #[serde(default)]
    pub euler_depth: Option<String>,
    /// tpe's trial budget. Refused under any other method, by the server.
    #[serde(default)]
    pub trials: Option<String>,
    /// The reproducibility seed tpe and genetic both take. Refused under any other method.
    #[serde(default)]
    pub seed: Option<String>,
}

impl WireSearch {
    /// Nothing was selected at all — the shape a caller sends as `None` rather than as an empty
    /// struct, so an ordinary grid search's frame is byte-identical to the one shipped before this
    /// field existed.
    pub fn is_empty(&self) -> bool {
        self.optimizer.is_none()
            && self.euler_depth.is_none()
            && self.trials.is_none()
            && self.seed.is_none()
    }

    /// Whether a daemon that does not advertise [`FEATURE_SEARCH_METHOD`] would answer this
    /// selection DIFFERENTLY from what it asks for — the predicate
    /// [`crate::DatahubClient::run_paramscan_profile`] refuses on.
    ///
    /// An explicit `grid` and nothing else is `false`: an old daemon drops the field and runs the
    /// grid, which is what was asked, so refusing it would be a false refusal. Anything else is
    /// `true` — including a knob written UNDER the grid, because the refusal that argv deserves
    /// (`--trials is a tpe or genetic flag`) is one an old daemon cannot produce.
    pub fn needs_capability(&self) -> bool {
        self.euler_depth.is_some()
            || self.trials.is_some()
            || self.seed.is_some()
            || self
                .optimizer
                .as_deref()
                .is_some_and(|m| !m.eq_ignore_ascii_case(DEFAULT_SEARCH_METHOD))
    }
}

/// One STUDY run as it crosses the wire: which compiled study, its recipe's TEXT, and the window.
///
/// ⚠ **The recipe travels as TEXT and never as a PATH**: the backend is typically a different
/// machine, so a path would resolve against ITS filesystem (`crates/vike-cli/src/cmd/study.rs`'s
/// module doc argues it; the same division [`Request::RunBacktest`] draws over profile TOML).
///
/// ⚠ **`from`/`to` are the operator's own strings and are NOT parsed here.** The backend owns that
/// grammar — `YYYY-MM-DD`, `YYYY-MM-DDTHH`, or bare unix SECONDS — and a client parser would be a
/// second answer to what the string means ([`WireSearch`]'s rule).
///
/// ⚠ **No store root and no trainer path, deliberately**: both name paths on the BACKEND's box;
/// `cmd/study.rs`'s `parse` refuses `--store` and `--lightgbm` BY NAME for that reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireStudy {
    /// The study's registry name, which is also its folder name under
    /// `<project>/user_data/research/studies/rust/`. Validated only by the backend: the registry is
    /// a compile-time const in the study host, which `vike-cli` deliberately does not link.
    pub study: String,
    /// The recipe's TEXT — one of that study folder's `.toml` files, read on the CLIENT.
    pub recipe_toml: String,
    /// The window's start, as typed.
    pub from: String,
    /// The window's end, as typed.
    pub to: String,
}

/// A client-to-server request.
///
/// Two payload styles ride here:
///
/// - The profile verbs ([`Request::RunBacktest`] and its sweep / walk-forward siblings) carry the
///   profile as **TOML text**, not the `vike_backtest` structs: the server parses it with
///   `BacktestProfile::from_toml_str` — the path a `.toml` file takes — so the wire schema stays
///   decoupled from `vike-backtest`'s internal serde surface across engine refactors.
/// - The READ verbs carry **typed params** mirroring [`vike_data::HistStore`]'s read methods
///   one-for-one. A range rides as decomposed `start` / `end` `Option<i64>` fields (inclusive,
///   epoch-ms) because `vike_data::TsRange` derives no serde; the server rebuilds the `TsRange`.
///   Results are the `vike-model` value types, which already derive serde.
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
    /// - **Key-LESS server** (no [`vike_node_proto::auth::NodeKeys`] configured — the default): a
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
    /// separator, per [`vike_node_proto::auth::sign`]. Answered by [`Response::AuthOk`] or
    /// [`Response::AuthDenied`].
    ///
    /// `scope` is the capability ceiling for the whole connection, bound INTO the mac;
    /// [`required_scope`] is the ONE authority mapping each verb to the scope that may send it.
    /// [`Scope::Read`] reads history and catalog; [`Scope::Write`] additionally admits the store
    /// writes such as [`Request::Backfill`] and every `Run*` verb but [`Request::RunNamed`] (they
    /// can carry client-supplied Rhai, which the server COMPILES, or persist a run).
    ///
    /// Sent only against a server advertising [`FEATURE_AUTH`]; a key-less server has nothing to
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
    /// Run one backtest, given the profile as its **TOML text**. The server parses+validates it via
    /// `BacktestProfile::from_toml_str`, runs it next to the data, and replies with
    /// [`Response::Report`] (or [`Response::Error`]).
    RunBacktest(String),
    /// The Studio compute-to-data run: resolve `spec`, load `slice`, backtest it, and reply with
    /// [`Response::RunResult`] — the rendered answer, NOT the bars/ticks. The DTOs (see
    /// [`crate::wire_studio`]) are converted at the server boundary, which runs the EXISTING
    /// `vike_studio_core::run::run_slice`. Served ONLY by a `serve-datafusion` build; a lean build
    /// decodes the request but answers [`Response::Error`].
    RunSlice {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: WireSpec,
        /// The data window (venue + symbols + interval + range + bars-vs-ticks). BOXED because
        /// [`WireSlice`] (~112 bytes) would push the variant past `clippy::large_enum_variant`'s
        /// bar; serde treats `Box<T>` transparently, so the wire shape is unchanged.
        slice: Box<WireSlice>,
        /// Cost/cash overrides; `None` = every engine field takes `EngineParams::default()`.
        params: Option<WireEngineParams>,
    },
    /// The Studio SWEEP run: resolve `spec`, load `slice`, run the parameter grid next to the data,
    /// and reply with [`Response::ParamscanResult`] — the ranked answer, NOT the bars/ticks. Runs
    /// the EXISTING `vike_studio_core::run_paramscan_slice`; served ONLY by a `serve-datafusion`
    /// build (a lean build decodes the request but answers [`Response::Error`]).
    ///
    /// ⚠ **Renamed from `RunSweep` in SOURCE ONLY.** The attribute pins the wire tag every deployed
    /// peer sends and expects; a Rust identifier was never on the wire, but this attribute's STRING
    /// ARGUMENT is, and it must never change. No `FEATURE_*` fix exists if it did: a capability
    /// lets a client OMIT something, it cannot make one enum encode under two tags.
    #[serde(rename = "RunSweep")]
    RunParamscan {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: WireSpec,
        /// The data window, BOXED like [`Request::RunSlice`]'s.
        slice: Box<WireSlice>,
        /// The parameter grid — each `(name, values)` axis overrides `strategy.params.<name>`.
        ///
        /// ⚠ Its wire KEY is pinned too: externally-tagged serde serializes a struct variant's
        /// fields under their own names, so this is not just the outer tag.
        #[serde(rename = "sweep")]
        paramscan: WireParamscan,
        /// Cost/cash overrides applied to EVERY grid point; `None` = `EngineParams::default()`.
        /// `#[serde(default)]` so a pre-v6 frame that omits it still decodes.
        #[serde(default)]
        params: Option<WireEngineParams>,
    },
    /// The Studio WALK-FORWARD run: resolve `spec`, load `slice`, walk it forward over
    /// `walkforward.n_splits` anchored out-of-sample windows next to the data, and reply with
    /// [`Response::WalkforwardResult`]. Runs the EXISTING
    /// `vike_studio_core::run_walkforward_slice`; served ONLY by a `serve-datafusion` build (a lean
    /// build answers [`Response::Error`]).
    RunWalkforward {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: WireSpec,
        /// The data window, BOXED like [`Request::RunSlice`]'s.
        slice: Box<WireSlice>,
        /// The walk-forward config (number of anchored OOS splits).
        walkforward: WireWalkforward,
        /// Cost/cash overrides applied to every OOS window; `None` = `EngineParams::default()`.
        /// `#[serde(default)]` so a pre-v6 frame that omits it still decodes.
        #[serde(default)]
        params: Option<WireEngineParams>,
    },
    /// Run a parameter SWEEP from the profile's own `[paramscan]` table, given the profile as its
    /// **TOML text** — the sweep sibling of [`Request::RunBacktest`] and the profile-shaped twin of
    /// [`Request::RunParamscan`]. The server parses it with `BacktestProfile::from_toml_str`, runs
    /// `vike_backtest::harness::run_paramscan` next to the data and replies with
    /// [`Response::ParamscanReport`], server-RANKED: the whole `[engine]` applies and the client
    /// needs no profile→DTO mapping and no metric math. Served on EVERY build (DataFusion-free).
    ///
    /// ⚠ **Renamed from `RunSweepProfile` in SOURCE ONLY**, pinned to its wire tag for the reason
    /// [`Request::RunParamscan`] gives.
    #[serde(rename = "RunSweepProfile")]
    RunParamscanProfile {
        /// The profile's TOML text, shipped verbatim. Must carry a `[paramscan]` table (the
        /// `[sweep]` spelling still loads — see `vike_backtest::harness::BacktestProfile`).
        profile_toml: String,
        /// Which ranking orders the rows — `"sharpe"` / `"return"` / `"max_dd"` / `"equity"`
        /// (`harness::RankMetric`, case-insensitive) or `"multi"`, the COMPOSITE objective.
        /// `None` = `"sharpe"`, the `backtest --rank-by` default. An unrecognized name is a
        /// [`Response::Error`], never a silent fallback.
        ///
        /// ⚠ **`"multi"` needs [`FEATURE_SEARCH_METHOD`] too**: a daemon predating it resolves this
        /// through `RankMetric::from_str_ci`, whose four arms have no `multi`, and answers an error
        /// naming a four-name set this client advertises five of. One refusal for one capability
        /// beats two.
        #[serde(default)]
        rank_by: Option<String>,
        /// Which SEARCH METHOD walks the grid, and its knobs ([`FEATURE_SEARCH_METHOD`]). `None` =
        /// the exhaustive grid, which is what an omitted field decodes to.
        ///
        /// ⚠ **`#[serde(default)]` makes an OLD client's frame decode; it does NOT make a NEW
        /// client's frame safe against an OLD server**, which drops the field silently — the client
        /// must refuse before sending ([`FEATURE_SEARCH_METHOD`] carries the argument).
        #[serde(default)]
        search: Option<WireSearch>,
    },
    /// Run an anchored WALK-FORWARD from the profile's own `[walkforward]` table, given the profile
    /// as its **TOML text** — the walk-forward sibling of [`Request::RunBacktest`] and the
    /// profile-shaped twin of [`Request::RunWalkforward`]. The server runs
    /// `vike_backtest::harness::run_walkforward` (bar mode, ONE series, `WalkMode::Anchored`) and
    /// replies with [`Response::WalkforwardReport`]. Served on EVERY build.
    RunWalkforwardProfile {
        /// The profile's TOML text, shipped verbatim. Must carry a `[walkforward]` table (the
        /// split count lives IN the profile — there is no wire override).
        profile_toml: String,
    },
    /// Run one COMPILED study over the daemon's own hist store and leave a run behind THERE
    /// ([`FEATURE_STUDY`]).
    ///
    /// ⚠ **BOXED**, like [`Request::RunSlice`]'s slice: four `String`s inline widen every
    /// [`Request`] value on every connection for a variant almost none of them carry.
    ///
    /// ⚠ **A COMPUTE verb, not a fourth plane on the wire.** Ruling R1 of
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` makes `research` a CLI
    /// plane; that spec's own fence says giving it its own DAEMON reopens ruling 7. A study opens
    /// the hist store and runs an engine, so it is served where every such verb is: `vike-backend
    /// backtest --addr`.
    RunStudy(Box<WireStudy>),
    /// Read derived OHLCV bars — mirrors `HistStore::load_bars(venue, symbol, interval, range)`.
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
    },
    /// Read L1 quotes — mirrors `HistStore::scan_quotes(venue, symbol, range)`. Answered by
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
    },
    /// Read executed trades — mirrors `HistStore::scan_trades(venue, symbol, range)`. Answered by
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
    /// Read raw L2 book updates — mirrors `HistStore::scan_book_updates(venue, symbol, range)`.
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
    /// Read the CONFLATING depth lane — mirrors `HistStore::scan_depth(venue, symbol, range)`.
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
    /// Read cohort rows — mirrors `HistStore::scan_cohort(venue, asset, range)`. Answered by
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
    /// `HistStore::scan_perp_metrics(venue, symbol, range)`. Answered by [`Response::PerpMetrics`]
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
    /// Read the stored equity curve — mirrors `HistStore::scan_equity(venue, symbol, range)`.
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
    /// Read the Tier-2 exec FILL log — mirrors `HistStore::scan_exec_fills(venue, symbol)`.
    /// Answered by [`Response::ExecFills`] (or [`Response::Error`]).
    ///
    /// ⚠ **No `start`/`end`: the TRAIT takes `(venue, symbol)` alone.** A wire range the store
    /// method has no parameter for would let a caller set a bound and watch it be silently ignored.
    ScanExecFills {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
    },
    /// Point-in-time properties lookup — mirrors `HistStore::properties_as_of(venue, symbol, ts)`.
    /// Answered by [`Response::Properties`] (or [`Response::Error`]).
    PropertiesAsOf {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// As-of instant (epoch-ms): the most-recent properties observed at or before this.
        ts: i64,
    },
    /// Enumerate every stored series — mirrors `HistStore::list_series()`. Answered by
    /// [`Response::SeriesList`] (or [`Response::Error`]).
    ListSeries,
    /// Every stored series with its cheap coverage — mirrors `HistStore::inventory()`. Answered by
    /// [`Response::Inventory`] (or [`Response::Error`]).
    Inventory,
    /// One series' coverage plus its commit keys — mirrors `HistStore::series_facts(id)`. Answered
    /// by [`Response::SeriesFacts`] (or [`Response::Error`]). Why it exists:
    /// [`FEATURE_SERIES_FACTS`].
    SeriesFacts {
        /// The series whose coverage and commit keys to report.
        id: SeriesId,
    },
    /// The gap ranges in one series' recorded span — mirrors `HistStore::series_gaps(id)`. Answered
    /// by [`Response::SeriesGaps`] (or [`Response::Error`]). `id` rides whole (`SeriesId` is
    /// serde).
    SeriesGaps {
        /// The series whose day-coverage gaps to report.
        id: SeriesId,
    },
    /// The CROSS-KIND coverage report — mirrors `HistStore::coverage_report()`. Answered by
    /// [`Response::Coverage`] (or [`Response::Error`]).
    ///
    /// No payload: like [`Request::Inventory`] it is a whole-store manifest fold, and the Data
    /// Manager renders all of it at once (a per-instrument verb would be N round trips per screen).
    /// Answers, not slices: day INDICES per kind per instrument, never rows. Negotiated by
    /// [`FEATURE_COVERAGE`].
    Coverage,
    /// Enumerate the compiled NATIVE backtest-strategy roster
    /// (`vike_backtest::harness::STRATEGIES`) — the names a profile's `strategy.name` can resolve
    /// WITH DEFAULT PARAMS. Answered by [`Response::Strategies`] (or [`Response::Error`]). The
    /// `"rhai"` SCRIPT arm is NOT in the roster (it needs a `src` param). Store-independent (a
    /// compile-time const), so every build serves it without DataFusion.
    ListStrategies,
    /// **Enumerate the roster a NAMED RUN would resolve** — and say whether the lane is ARMED.
    /// Answered by [`Response::NamedStrategies`] (or [`Response::Error`]).
    ///
    /// ⚠ **It is NOT [`Request::ListStrategies`]: the two answer different rosters.** That one is
    /// the SIMULATOR roster (`vike_backtest::harness::STRATEGIES`, including arms beside the Rhai
    /// compiler); a named run resolves only through crates that cannot name `vike-script`, plus the
    /// operator's compiled-in user strategies, which `ListStrategies` never enumerated.
    /// `docs/decisions/0064`'s decision 7: *the roster the verb SERVES is the roster it ENUMERATES*
    /// (naming into the dark is the lie `docs/decisions/0062`'s decision 5 fences against).
    ///
    /// Store-independent, so every build of the compute daemon serves it. Negotiated by
    /// [`FEATURE_NAMED_RUN`]; an UNARMED server still answers, with
    /// [`crate::named_run::NamedRoster::armed`] `false`.
    NamedStrategies,
    /// **Run ONE strategy the server already holds** — one strategy, one param set, one window,
    /// one pass. Answered by [`Response::NamedRun`] (or [`Response::Error`]).
    ///
    /// ⚠ **The only `Run*` verb that is `VerbScope::Read`**
    /// (`docs/decisions/0064-a-named-run-carries-no-source.md`): it carries
    /// [`crate::named_run::NamedParam`], which no script can occupy, and
    /// [`crate::named_run::NamedRunSpec`] has no field a search can occupy. [`required_scope`]'s
    /// doc and arm carry the argument and the bounds that are its CONDITION. **Adding a search
    /// dimension here is 0064's first reopener, not a small change.**
    ///
    /// Negotiated by [`FEATURE_NAMED_RUN`]; a server whose operator has not armed the lane answers
    /// [`crate::named_run::NamedRunOutcome::NotArmed`] rather than an error.
    RunNamed(Box<crate::named_run::NamedRunSpec>),
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
    /// list is the truthful "nothing is running". [`VerbScope::Read`]: it answers from server state
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
    /// is named: the door is "stop this series". [`VerbScope::Write`] (its arm in
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
    /// [`VerbScope::Write`]: see [`required_scope`]'s arm. A newtype over
    /// [`crate::archive::ImportSpec`], byte-identical on the wire to a struct variant.
    ImportArchive(crate::archive::ImportSpec),
    /// **A chart is open on a series the store cannot paint — SEED IT.** Answered by
    /// [`Response::SeriesSeeded`] (or [`Response::Error`]).
    ///
    /// ⚠ **A WRITE verb classified [`VerbScope::Read`]**:
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
        /// length AND charset by [`crate::seed::validate_seed_symbol`] before it reaches a bridge,
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
    /// ⚠ **[`VerbScope::Read`] and NOT A WRITE**
    /// (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`; its arm
    /// in [`required_scope`] carries the argument). Nothing enters the served store; the durable
    /// copy of a catalog is the CLIENT's own `vike_catalog::persist` cache, and the server keeps
    /// only an in-process TTL memo. **Persisting anything here makes this a write and re-imposes
    /// 0058's four-part rule; adding a parameter is the first bullet of 0062's reopen list.** The
    /// request names a VENUE and nothing else; the cost terms both ends need live in
    /// [`crate::catalog`]. A CREDENTIALED venue (alpaca/oanda/ctrader) is refused BY CONSTRUCTION
    /// with [`crate::catalog::CatalogRefusal::NeedsCredentials`].
    ///
    /// Negotiated by [`FEATURE_VENUE_CATALOG`]; an unarmed server answers
    /// [`crate::catalog::CatalogOutcome::NotArmed`] rather than an error.
    VenueCatalog {
        /// The venue, in the canonical roster spelling (e.g. `"binance"`), bounded in length AND
        /// charset by [`crate::catalog::validate_catalog_venue`] at the server's door. A venue with
        /// no provider in this build is a [`crate::catalog::CatalogRefusal::NotServed`] carried in
        /// the SUCCESS variant — unlike every other unknown-venue path on this wire, because "which
        /// venues can I refresh" is a routine question and an error is the wrong shape for it.
        venue: String,
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
    /// [`Response::MdSubscribed`], **the last positional frame that socket will carry**: after it
    /// the server never reads this direction again and writes only [`Response::Md`].
    /// [`crate::market`]'s module doc states the invariant and its one exception — a
    /// [`Response::Error`] answer has NOT switched the connection, and a client that gets one must
    /// keep using the socket positionally rather than starting a reader thread.
    ///
    /// ⚠ **`refused` is PER-SPEC; a whole-request failure is [`Response::Error`].** A build with no
    /// hub does not answer `MdSubscribed { accepted: [], refused: [all] }` — that would mode-switch
    /// the connection into a heartbeat-only writer that never sends a frame (hence no
    /// `HubNotMounted` in [`crate::market::MdRefusal`]).
    ///
    /// ⚠ **There is deliberately NO long-lived control connection.** A session's mutation channel
    /// is [`Request::MdUpdate`] on an ordinary short-lived connection, so a dead control connection
    /// is not a STATE: a client that cannot reach a persistent control socket cannot unsubscribe
    /// and leaks topics until its stream drops.
    ///
    /// [`VerbScope::Read`] — see [`required_scope`] and
    /// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md`.
    MdSubscribe {
        /// The subscriptions wanted. Each is accepted (possibly with a CLAMPED `depth_levels`) or
        /// refused with its own reason; `accepted` echoes the server's AUTHORITATIVE spec.
        specs: Vec<crate::market::MdSpec>,
    },
    /// **Change an existing market-data session's subscription set**, answered positionally with
    /// [`Response::MdUpdated`].
    ///
    /// ⚠ **Sent on an ORDINARY, SHORT-LIVED connection — never on the stream socket**, whose server
    /// side has left its read loop: a frame written there is never read and the caller waits
    /// forever. The `crates/vike-datahub-client/src/remote.rs` dial-per-request shape.
    ///
    /// ⚠ `remove` matches on the KEY `(venue, symbol, lane)` and IGNORES `depth_levels` — depth is
    /// not part of a subscription's identity (`crate::market::MdSpec::key`). An unknown or expired
    /// `session` is [`Response::Error`], not a refusal list.
    MdUpdate {
        /// The session minted by the [`Response::MdSubscribed`] that opened the stream.
        session: crate::market::MdSessionId,
        /// Specs to ADD. Refusals are per-spec.
        add: Vec<crate::market::MdSpec>,
        /// Specs to REMOVE, matched on their key.
        remove: Vec<crate::market::MdSpec>,
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
/// `first_ts`/`last_ts` are read BACK from the store over the requested range after the write —
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
    pub first_ts: Option<i64>,
    /// Last bar ts now stored in the requested range (`None` = the range holds no bars).
    pub last_ts: Option<i64>,
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
pub struct RunningBackfill {
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
    pub flagged: Vec<RunningBackfill>,
    /// The requests on the series that NO cancel can stop: a one-batch lane, refused with
    /// [`BACKFILL_ONE_BATCH`]. Listed so the answer says what is still running, rather than
    /// implying by omission that nothing is. Their flag is NOT raised.
    pub unstoppable: Vec<RunningBackfill>,
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
    pub first_ts: Option<i64>,
    /// Last bar ts now stored in that window.
    pub last_ts: Option<i64>,
}

/// A server-to-client response. The `Report`/`Bars`/`Quotes`/`Trades`/`Properties` variants are the
/// compute-to-data answers; `Error` carries a server-side failure message so a client never has to
/// guess why a request did not produce its answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Reply to [`Request::Hello`]: the server's [`PROTO_VERSION`] and its advertised `features` —
    /// the capability strings naming what this server serves (`"backtest"`, `"load_bars"`, the
    /// `FEATURE_*` constants). The client compares the version and fails loudly on a mismatch.
    ///
    /// On a KEYED server it additionally carries `nonce` and advertises [`FEATURE_AUTH`].
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
        /// `crates/vike-datahub/tests/auth_roundtrip.rs`'s
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
    /// The backtest metrics summary as **JSON text** (`serde_json::to_string(&BacktestReport)` —
    /// the exact shape the `backtest --json` bin emits). `BacktestReport` derives `Deserialize`, so
    /// a client may parse it; the wire stays TEXT so the protocol is independent of
    /// `vike-backtest`'s internal serde surface and a field added there is no frame-compatibility
    /// question.
    Report(String),
    /// Reply to [`Request::RunSlice`]: the rendered [`WireRunResult`] — the equity curve, closed
    /// trades, and the counters the Studio shows. A run FAILURE (bad script/slice/params) comes
    /// back as [`Response::Error`] (the stringified [`crate::wire_studio::WireRunError`]).
    RunResult(WireRunResult),
    /// Reply to [`Request::RunParamscan`]: the ranked [`WireParamscanResult`] — the entries plus
    /// the deflated-Sharpe / PBO scores. A run FAILURE comes back as [`Response::Error`].
    ///
    /// ⚠ `#[serde(rename = "SweepResult")]` pins the wire tag; see [`Request::RunParamscan`] for
    /// why.
    #[serde(rename = "SweepResult")]
    ParamscanResult(WireParamscanResult),
    /// Reply to [`Request::RunWalkforward`]: the stitched [`WireWalkforwardResult`] — the OOS
    /// windows, stitched equity curve and summary stats. A run FAILURE comes back as
    /// [`Response::Error`].
    WalkforwardResult(WireWalkforwardResult),
    /// Reply to [`Request::RunParamscanProfile`]: the RANKED sweep as **JSON text**
    /// (`serde_json::to_string(&vike_backtest::harness::ParamscanReport)`, the `backtest --sweep
    /// --json` shape). Rows arrive ordered best-first, each with its own `BacktestReport`, so a
    /// client renders server-computed stats and never re-implements a metric.
    ///
    /// Text for [`Response::Report`]'s decoupling reason, and because `ParamscanReport` is
    /// `Serialize`-ONLY and cannot be read back into a typed value at all.
    ///
    /// ⚠ `#[serde(rename = "SweepReport")]` pins the wire tag; see [`Request::RunParamscan`] for
    /// why.
    #[serde(rename = "SweepReport")]
    ParamscanReport(String),
    /// Reply to [`Request::RunWalkforwardProfile`]: the stitched
    /// `vike_backtest::walkforward::WalkForwardReport` as **JSON text** — the OOS windows, the
    /// stitched equity curve, and the three summary scalars. Text for the same reason as
    /// [`Response::ParamscanReport`].
    WalkforwardReport(String),
    /// Reply to [`Request::RunStudy`]: the run now on the BACKEND's disk, as JSON text — the report
    /// types are Serialize-ONLY, so a typed variant would need `Deserialize` on the whole
    /// run-manifest and study-outcome tree.
    StudyReport(String),
    /// A server-side failure (invalid profile, missing strategy, data error, …), stringified.
    Error(String),
    /// Reply to [`Request::LoadBars`]: the derived OHLCV bars, ts-ascending.
    Bars(Vec<Bar>),
    /// Reply to [`Request::ScanQuotes`]: the L1 quotes, ts-ascending.
    Quotes(Vec<QuoteTick>),
    /// Reply to [`Request::ScanTrades`]: the executed trades, ts-ascending.
    Trades(Vec<TradeTick>),
    /// Reply to [`Request::ScanBookUpdates`]: the raw L2 book updates, ts-ascending.
    BookUpdates(Vec<BookUpdate>),
    /// Reply to [`Request::ScanDepth`]: the conflating depth lane's updates, ts-ascending. ⚠ A
    /// SEPARATE variant from [`Self::BookUpdates`] on purpose — see [`Request::ScanDepth`].
    Depth(Vec<BookUpdate>),
    /// Reply to [`Request::ScanCohort`]: the cohort rows, ts-ascending.
    Cohort(Vec<CohortRow>),
    /// Reply to [`Request::ScanPerpMetrics`]: the perpetual-swap metric rows, ts-ascending.
    PerpMetrics(Vec<PerpMetricRow>),
    /// Reply to [`Request::ScanEquity`]: the equity samples, ts-ascending.
    Equity(Vec<EquitySample>),
    /// Reply to [`Request::ScanExecFills`]: the Tier-2 exec fill rows.
    ExecFills(Vec<ExecFillRow>),
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
    /// Reply to [`Request::ListStrategies`]: the compiled native backtest-strategy names (the
    /// `vike_backtest::harness::STRATEGIES` roster, in its declared order).
    Strategies(Vec<String>),
    /// Reply to [`Request::NamedStrategies`]: the roster a NAMED RUN would resolve, plus whether
    /// the lane is armed. See [`crate::named_run::NamedRoster`] — and note `armed: false` is a
    /// SUCCESS.
    NamedStrategies(crate::named_run::NamedRoster),
    /// Reply to [`Request::RunNamed`]: what the run did, which is one of THREE outcomes and only
    /// one of them an error. See [`crate::named_run::NamedRunOutcome`].
    ///
    /// ⚠ BOXED for `clippy::large_enum_variant`, as [`Response::Properties`] is: the `Ran` arm
    /// carries a whole [`crate::wire_studio::WireRunResult`]. The wire shape is unchanged.
    NamedRun(Box<crate::named_run::NamedRunOutcome>),
    /// Reply to [`Request::Backfill`]: the fetch ran and the rows are IN THE STORE (write-through
    /// happens before this frame is written). See [`BackfillDone`] for the field contract; a
    /// fetch/validation failure is [`Response::Error`], never a partial `BackfillDone`.
    BackfillDone(BackfillDone),
    /// Reply to [`Request::ListBackfills`]: every `Backfill` the server is running, in the order it
    /// began them. Empty when none is.
    RunningBackfills(Vec<RunningBackfill>),
    /// Reply to [`Request::CancelBackfill`]: which requests on the series were flagged and which no
    /// cancel can stop. See [`BackfillCancelDone`].
    BackfillsCancelled(BackfillCancelDone),
    /// Reply to [`Request::HistoryChannels`]: every roster venue's rows as this server's build
    /// declares them, with the server's overlay. See [`crate::history::HistoryChannelsReport`]. A
    /// store that cannot answer the `held` half's inventory read is [`Response::Error`], the answer
    /// [`Request::Inventory`] itself gives.
    HistoryChannels(crate::history::HistoryChannelsReport),
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
    /// ⚠ BOXED for `clippy::large_enum_variant`, as [`Response::NamedRun`] is: a plan carries
    /// several strings and lists. The wire shape is unchanged.
    ArchiveImported(Box<crate::archive::ImportDone>),
    /// Reply to [`Request::MdSubscribe`]. **POSITIONAL, and the LAST positional frame this socket
    /// will carry** — see [`crate::market`]'s module doc.
    MdSubscribed {
        /// The session token, for a later [`Request::MdUpdate`] on a SHORT-LIVED connection.
        session: crate::market::MdSessionId,
        /// The specs now served, echoing the server's AUTHORITATIVE form — including any CLAMPED
        /// `depth_levels`. A clamp is an acceptance with a smaller number, never a refusal, which
        /// is what lets a client that asked for 200 levels LEARN it got 50.
        accepted: Vec<crate::market::MdSpec>,
        /// The specs refused, each with its own typed reason.
        refused: Vec<(crate::market::MdSpec, crate::market::MdRefusal)>,
        /// **The server's own heartbeat period in milliseconds.**
        ///
        /// ⚠ It keeps `crate::market::MD_READ_TIMEOUT` from being a COMPILE-TIME contract between
        /// two independently-deployed binaries: the client arms `max(MD_READ_TIMEOUT, 3 ×
        /// heartbeat_ms)`. (The ACCOUNT plane's `vike_tradehub_client::liveness`'s
        /// `OBSERVE_READ_TIMEOUT` is a constant on both sides, a hazard its own doc names.)
        heartbeat_ms: u64,
    },
    /// Reply to [`Request::MdUpdate`]. **POSITIONAL**, on the short-lived control connection the
    /// request arrived on — the frames themselves go to the STREAM connection.
    MdUpdated {
        /// Newly-served specs, in the server's authoritative form.
        accepted: Vec<crate::market::MdSpec>,
        /// Refused additions, each with its own typed reason.
        refused: Vec<(crate::market::MdSpec, crate::market::MdRefusal)>,
        /// The specs this update actually RELEASED — matched on their key, so a `remove` naming a
        /// spec the session never held is silently absent here rather than an error.
        released: Vec<crate::market::MdSpec>,
    },
    /// One PUSHED market-data frame. Appears ONLY on a stream connection, and after
    /// [`Response::MdSubscribed`] it is the only variant that appears there at all — which makes a
    /// stream reader's `match` on this variant a complete protocol, with anything else a desync.
    ///
    /// BOXED on [`Response::Properties`]'s argument: `Response` is cloned and matched on every
    /// POSITIONAL path and should not grow for a variant one kind of connection sees
    /// (`crate::market::MdFrame`'s largest variant is ~128 B). The wire shape is unchanged.
    Md(Box<crate::market::MdFrame>),
}

#[path = "proto_tests.rs"]
#[cfg(test)]
mod proto_tests;
