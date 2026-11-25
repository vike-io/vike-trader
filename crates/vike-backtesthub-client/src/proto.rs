//! The backtesthub wire protocol: the request/response schema of the verbs that RUN something, over
//! the length-prefixed `serde_json` frames of `vike_hub_protocol::frame`.
//!
//! # Compute-to-data contract
//!
//! A client ships a small CONFIG next to where the data lives and gets back a compact ANSWER, never a
//! raw upstream data slice:
//!
//! - [`Request::RunBacktest`] ships a profile as its TOML text and gets a [`Response::Report`] (JSON
//!   text).
//! - [`Request::RunSlice`] / [`Request::RunParamscan`] / [`Request::RunWalkforward`] are the STUDIO
//!   verbs: they ship the [`StrategySpecWire`]/[`DataSliceWire`]/[`StrategyEngineConfigWire`] DTOs (see
//!   [`crate::wire_studio`]) plus a [`ParamscanWire`] grid or a [`WalkforwardWire`] split count, and get
//!   back the rendered [`TradingRecordWire`] / ranked / stitched answer, never the bars or ticks.
//! - [`Request::RunParamscanProfile`] / [`Request::RunWalkforwardProfile`] are their PROFILE-shaped
//!   twins: the profile's TOML text crosses verbatim and the SERVER parses it with
//!   `ProfileBacktest::from_toml_str` before running `vike_backtest::search::paramscan::run_paramscan` /
//!   `walkforward::runner::run_walkforward`, so the WHOLE `[engine]` surface applies — the `fee` SCHEDULE,
//!   the impact model's coefficients, `[engine.resolution]`, `[risk]`, `snap_to_properties`, tick mode, cross-venue
//!   `[[data.series]]` — none of which a `DataSliceWire` + `StrategyEngineConfigWire` pair can carry, under ONE
//!   parser.
//!
//! # One handshake, two daemons, two enums
//!
//! The handshake's frames (`Hello`, `Auth` and their replies) are `vike_hub_protocol::handshake`'s, the one
//! spelling every hub shares, and this schema carries no copy of them (decision 0288's amendment of
//! 2026-11-21, the same bytes). `Ping` and its `Pong` are spelled here and in `vike_datahub_client::proto`
//! with the same bytes, because both daemons answer them. Every other tag is this enum's alone. A verb of the DATA plane does not decode here,
//! and the daemon answers it with `vike_hub_protocol::plane`'s named refusal rather than a parse failure.
//!
//! **Decode-vs-drop.** A server reads with `vike_hub_protocol::frame::read_frame_raw` so a well-framed body
//! that fails to decode into a known [`Request`] variant (an unknown verb from a newer client) is
//! answered with [`Response::Error`] and the connection SURVIVES. A decode error is a bad *request*, not
//! a bad *connection*.
//!
//! The wire tags are the Rust names: the `paramscan` verbs carry no `#[serde(rename)]` since the
//! owner's ruling 20 (phase 4) put the word on the wire too.

use serde::{Deserialize, Serialize};
use vike_model::run_request::search_methods::DEFAULT_SEARCH_METHOD;

use crate::sdk_source::{WireSdkFile, WireSdkListing};
use crate::study::{WireRunArtifact, WireRunPage, WireStudyListing};
use crate::summary::{
    CurvePageWire, ParamscanProfileSummaryWire, ParamscanRowsPageWire, ParamscanSummaryWire,
    RunSummaryWire, TradesPageWire, WalkforwardSummaryWire,
};
use crate::wire_studio::{
    DataSliceWire, ParamscanWire, StrategyEngineConfigWire, StrategySpecWire, StudioParamscanWire,
    TradingRecordWire, WalkforwardResultsWire, WalkforwardWire,
};

mod features;
mod verbs;

pub use features::{
    FEATURE_ATTACH_FUNDING, FEATURE_BUILD_STRATEGY_FILE, FEATURE_LIST_STRATEGIES,
    FEATURE_NAMED_RUN, FEATURE_PARAMSCAN_PROFILE_SUMMARY, FEATURE_RHAI_PARAMETERS,
    FEATURE_RUN_CURVE, FEATURE_RUN_FILES, FEATURE_RUN_FROM_SAVED, FEATURE_RUN_PARAMSCAN,
    FEATURE_RUN_PARAMSCAN_PROFILE, FEATURE_RUN_SUMMARY, FEATURE_RUN_WALKFORWARD,
    FEATURE_RUN_WALKFORWARD_PROFILE, FEATURE_SDK_SOURCE, FEATURE_SEARCH_METHOD,
    FEATURE_SLIPPAGE_MODEL, FEATURE_STRATEGY_FILES, FEATURE_STRATEGY_SDK, FEATURE_STUDY,
    FEATURE_STUDY_FILES, FEATURE_WALKFORWARD_SEARCH, STUDIO_RUNNER_SENTINEL,
};
pub use verbs::{may_answer_from_saved, reply_wait, request_kind, required_scope};

/// The backtesthub wire protocol version, negotiated by the `Hello` / `Welcome`
/// handshake (`vike_hub_protocol::handshake`): the client fails the connection with a legible, both-numbers error on a mismatch. It
/// changes ONLY for a change an old peer cannot DECODE; an additive verb, field or advertisement is
/// negotiated by a `Welcome.features` string instead
/// (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).
///
/// The number is the one this plane shipped under while it shared a schema with the data daemon
/// (`vike_datahub_client::proto::PROTO_VERSION` carries the history), and the mac signs it, so a change
/// here is a change for every deployed peer.
///
/// - `1` — Hello/Ping/RunBacktest.
/// - `2` — [`Request::RunSlice`] + [`Response::RunResult`].
/// - `3` — [`Request::RunParamscan`] / [`Request::RunWalkforward`] + their results.
/// - `5` — [`Request::ListStrategies`] + [`Response::Strategies`].
/// - `6` — the OPTIONAL `params` field on [`Request::RunParamscan`] / [`Request::RunWalkforward`]
///   (`#[serde(default)]`, so an old frame decodes as `None`).
/// - `7` — [`Request::RunParamscanProfile`] / [`Request::RunWalkforwardProfile`] + their reports.
/// - still `7`, each negotiated per decision 0112: the `search` selector and `"multi"`, `RunStudy`, the
///   Studio walk-forward `search`, the named run, the strategy-file list and read
///   ([`FEATURE_STRATEGY_FILES`]), and (on `8`) the Studio's Build ([`FEATURE_BUILD_STRATEGY_FILE`])
///   and the summarized Studio runs ([`FEATURE_RUN_SUMMARY`]: [`Request::Summarized`],
///   [`Request::ReadTrades`], [`Request::ReadPoint`] and their four replies, all additive), and the whole
///   curve on request ([`FEATURE_RUN_CURVE`]: [`Request::ReadCurve`], [`Response::CurvePage`] and a
///   summarized [`Request::RunBacktest`], additive too), and (on `9`) the server's studies and runs
///   ([`FEATURE_STUDY_FILES`]: [`Request::ListStudies`] and `RunStudy`'s tier and recipe name;
///   [`FEATURE_RUN_FILES`]: [`Request::ListRuns`], [`Request::ReadRunArtifact`]; all additive), and the
///   versioned strategy SDK's source ([`FEATURE_SDK_SOURCE`]: [`Request::ListSdks`],
///   [`Request::ReadSdkFile`], additive too), and a profile's summarized parameter search
///   ([`FEATURE_PARAMSCAN_PROFILE_SUMMARY`]: [`Request::Summarized`] around [`Request::RunParamscanProfile`],
///   [`Request::ReadParamscanRows`] and their two replies, additive).
/// - `8` — the phase-4 renames the owner ruled (P4.5c): the four `paramscan` tags
///   (`RunParamscan`/`RunParamscanProfile`/`ParamscanResult`/`ParamscanReport`) and the `paramscan` key
///   of [`Request::RunParamscan`] are their Rust names on the wire, where they used to be spelled `sweep`
///   by a serde rename (ruling 20), and the report JSON's renamed keys. A peer from before them could not
///   decode those frames, so an old peer is refused at the handshake, naming both numbers
///   (`proto_tests.rs`'s `a_daemon_from_before_the_phase_four_renames_is_refused_at_the_handshake`).
///   `crates/vike-backtesthub-client/tests/wire_tag_fixtures.rs` round-trips the re-recorded frames.
/// - `9` — the walk-forward results through the shared `vike_analytics::results::TradingResults`
///   (package 2 step 4): [`Response::WalkforwardResult`]'s [`WalkforwardResultsWire`] and
///   [`Response::WalkforwardSummary`] carry `steps` (each its own results), `overall`,
///   `equity_curve` and `profitable_step_share`, where they carried `windows` and the four numbers
///   `out_of_sample_return`, `out_of_sample_sharpe`, `out_of_sample_equity_curve` and
///   `walkforward_consistency`. A peer from before cannot decode them, so the desktop and the
///   backtesthub deploy together and an old one is refused at the handshake, naming both numbers
///   (`proto_tests.rs`'s `a_daemon_from_before_the_walkforward_results_is_refused_at_the_handshake`).
///   The walk-forward lines of `fixtures/datahub_wire/v7_backtest_plane_responses.jsonl` were
///   re-captured with it.
pub const PROTO_VERSION: u32 = 9;

// ⚠ `STRATEGIES`, the compiled native roster, stood here until plan 2b: no strategy is compiled
// into the backtesthub, and `Request::ListStrategies` answers the scan of its `strategies/` folder
// (`vike_backtesthub::backtest_cli`'s `strategy_roster`).

/// A parameter search's SEARCH SELECTION as it crosses the wire: which method, and that method's
/// own knobs, carried as the operator's own TOKENS.
///
/// ⚠ **Strings, not typed scalars, and that is the whole point.** The one authority for what
/// `"128"` means as a `--trials` value — including the refusal text for `"abc"` — is
/// `vike_backtest::search::select`, on the SERVER. `vike-cli` has no `vike-backtest` dependency, so
/// typed fields would force a second parser with its own messages; these `String`s buy one parser
/// and one message on both routes.
///
/// ⚠ Not [`crate::wire_studio::ParamscanWire`], the Studio's GRID: this is the METHOD that walks a
/// grid, and the grid stays in the profile's `[paramscan]` table. The method cannot live in the
/// profile: `vike_backtest::run::profile::ProfileBacktest` is `#[serde(deny_unknown_fields)]`, and every
/// key of its parameter-grid table is an AXIS. The sibling `Request::RunWalkforwardProfile` carries
/// no selector: a second place to say "optimize" is a second place for the two to disagree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSelectionWire {
    /// One of [`SEARCH_METHODS`](vike_model::run_request::search_methods::SEARCH_METHODS). `None` =
    /// [`DEFAULT_SEARCH_METHOD`].
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

impl SearchSelectionWire {
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
    /// [`crate::BacktesthubClient::run_paramscan_profile`] refuses on.
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

/// One STUDY run as it crosses the wire: which study (its folder's name on the backend's box) in which
/// tier, its recipe (TEXT, or the name of a recipe file in that folder), and the window.
///
/// ⚠ **A client's recipe travels as TEXT and never as a PATH**: the backend is typically a different
/// machine, so a path would resolve against ITS filesystem (`crates/vike-cli/src/cmd/research/study.rs`'s
/// module doc argues it; the same division [`Request::RunBacktest`] draws over profile TOML). A recipe
/// FILE of the server's own study folder is named by its file name ([`Self::recipe`]), which the
/// server checks against that folder's listing.
///
/// ⚠ **`from`/`to` are the operator's own strings and are NOT parsed here.** The backend owns that
/// grammar — `YYYY-MM-DD`, `YYYY-MM-DDTHH`, or bare unix SECONDS — and a client parser would be a
/// second answer to what the string means ([`SearchSelectionWire`]'s rule).
///
/// ⚠ **No store root and no trainer path, deliberately**: both name paths on the BACKEND's box;
/// `cmd/research/study.rs`'s `parse` refuses `--store` and `--lightgbm` BY NAME for that reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyRunRequestWire {
    /// The study's folder name under the backend's `<project>/user_data/research/studies/<tier>/`.
    /// Validated only by the backend, the one side that can see the folder.
    pub study: String,
    /// The recipe's TEXT — one of that study folder's `.toml` files, read on the CLIENT.
    pub recipe_toml: String,
    /// The window's start, as typed. EMPTY is unbounded on that side (the Studio's "whole store"
    /// window); an older server refuses an empty bound in words.
    pub from: String,
    /// The window's end, as typed; empty is unbounded, as [`Self::from`].
    pub to: String,
    /// Answer a run whose result could not be SAVED with its numbers
    /// ([`crate::study::WireStudyReport::not_saved`]) rather than with `Response::Error`. Opt-in, so
    /// a client that predates the field (and would read any `StudyReport` as a saved run) keeps getting
    /// the error. No feature string: a server that predates it answers the error, which is that client's
    /// correct and readable answer (decision 0112's verdict 3 asks for one only for a field an old server
    /// would drop and answer WRONG). `false` is not written, so a default request's bytes are the ones
    /// shipped before it (`tests/backtest_plane_wire_pin.rs`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub report_unsaved_result: bool,
    /// Which tier the study is in: a Rhai study runs inside the server, a Rust study FILE in its
    /// sandboxed child. `None` is the Rust tier, what every request before this field meant.
    /// Negotiated by [`FEATURE_STUDY_FILES`]: an older server would drop it and run the Rust study of
    /// that name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<crate::study::StudyTierWire>,
    /// A recipe by its FILE NAME in the study's folder on the server (one of
    /// [`crate::study::WireListedStudy::recipes`]), read there. Refused beside a non-empty
    /// [`Self::recipe_toml`]: one run, one recipe. Negotiated by [`FEATURE_STUDY_FILES`], like `tier`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
}

impl StudyRunRequestWire {
    /// Whether an older server would answer this request DIFFERENTLY from what it asks (it would
    /// drop [`Self::tier`] or [`Self::recipe`]): the predicate
    /// [`crate::BacktesthubClient::run_study`] refuses on without [`FEATURE_STUDY_FILES`].
    pub fn needs_study_files(&self) -> bool {
        self.tier.is_some() || self.recipe.is_some()
    }
}

/// A client-to-server request.
///
/// The profile verbs ([`Request::RunBacktest`] and its sweep / walk-forward siblings) carry the profile
/// as **TOML text**, not the `vike_backtest` structs: the server parses it with
/// `ProfileBacktest::from_toml_str` — the path a `.toml` file takes — so the wire schema stays decoupled
/// from `vike-backtest`'s internal serde surface across engine refactors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Liveness probe — answered by [`Response::Pong`].
    Ping,
    /// Run one backtest, given the profile as its **TOML text**. The server parses+validates it via
    /// `ProfileBacktest::from_toml_str`, runs it next to the data, and replies with
    /// [`Response::Report`] (or [`Response::Error`]).
    RunBacktest(String),
    /// The Studio compute-to-data run: resolve `spec`, load `slice`, backtest it, and reply with
    /// [`Response::RunResult`] — the rendered answer, NOT the bars/ticks. The DTOs (see
    /// [`crate::wire_studio`]) are converted at the server boundary, which runs the EXISTING
    /// `vike_backtesthub::studio_run::run_slice`.
    RunSlice {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: StrategySpecWire,
        /// The data window (venue + symbols + interval + range + bars-vs-ticks). BOXED because
        /// [`DataSliceWire`] (~112 bytes) would push the variant past `clippy::large_enum_variant`'s
        /// bar; serde treats `Box<T>` transparently, so the wire shape is unchanged.
        slice: Box<DataSliceWire>,
        /// Cost/cash overrides; `None` = every engine field takes `StrategyEngineConfig::default()`.
        params: Option<StrategyEngineConfigWire>,
    },
    /// The Studio SWEEP run: resolve `spec`, load `slice`, run the parameter grid next to the data,
    /// and reply with [`Response::ParamscanResult`] — the ranked answer, NOT the bars/ticks. Runs
    /// the EXISTING `vike_backtesthub::studio_run::run_paramscan_slice`.
    ///
    /// The tag on the wire is this name (it was `RunSweep` until the owner's ruling 20).
    RunParamscan {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: StrategySpecWire,
        /// The data window, BOXED like [`Request::RunSlice`]'s.
        slice: Box<DataSliceWire>,
        /// The parameter grid — each `(name, values)` axis overrides `strategy.params.<name>`.
        ///
        /// Its wire KEY is this field's name (it was `sweep` until the owner's ruling 20).
        paramscan: ParamscanWire,
        /// Cost/cash overrides applied to EVERY grid point; `None` = `StrategyEngineConfig::default()`.
        /// `#[serde(default)]` so a pre-v6 frame that omits it still decodes.
        #[serde(default)]
        params: Option<StrategyEngineConfigWire>,
    },
    /// The Studio WALK-FORWARD run: resolve `spec`, load `slice`, walk it forward over
    /// `walkforward.n_splits` anchored out-of-sample windows next to the data, and reply with
    /// [`Response::WalkforwardResult`]. Runs the EXISTING `vike_backtesthub::studio_run::run_walkforward_slice`.
    RunWalkforward {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: StrategySpecWire,
        /// The data window, BOXED like [`Request::RunSlice`]'s.
        slice: Box<DataSliceWire>,
        /// The walk-forward config (number of anchored OOS splits).
        walkforward: WalkforwardWire,
        /// Cost/cash overrides applied to every OOS window; `None` = `StrategyEngineConfig::default()`.
        /// `#[serde(default)]` so a pre-v6 frame that omits it still decodes.
        #[serde(default)]
        params: Option<StrategyEngineConfigWire>,
    },
    /// Run a parameter SWEEP from the profile's own `[paramscan]` table, given the profile as its
    /// **TOML text** — the sweep sibling of [`Request::RunBacktest`] and the profile-shaped twin of
    /// [`Request::RunParamscan`]. The server parses it with `ProfileBacktest::from_toml_str`, runs
    /// `vike_backtest::search::paramscan::run_paramscan` next to the data and replies with
    /// [`Response::ParamscanReport`], server-RANKED: the whole `[engine]` applies and the client
    /// needs no profile→DTO mapping and no metric math. Served on EVERY build (DataFusion-free).
    ///
    /// The tag on the wire is this name (it was `RunSweepProfile` until the owner's ruling 20).
    RunParamscanProfile {
        /// The profile's TOML text, shipped verbatim. Must carry a `[paramscan]` table (the
        /// `[sweep]` spelling still loads — see `vike_backtest::run::profile::ProfileBacktest`).
        profile_toml: String,
        /// Which ranking orders the rows — `"sharpe"` / `"return"` / `"max_dd"` / `"equity"`
        /// (`search::paramscan::RankMetric`, case-insensitive) or `"multi"`, the COMPOSITE objective.
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
        search: Option<SearchSelectionWire>,
    },
    /// Run an anchored WALK-FORWARD from the profile's own `[walkforward]` table, given the profile
    /// as its **TOML text** — the walk-forward sibling of [`Request::RunBacktest`] and the
    /// profile-shaped twin of [`Request::RunWalkforward`]. The server runs
    /// `vike_backtest::walkforward::runner::run_walkforward` (bar mode, ONE series, `WalkforwardMode::Anchored`) and
    /// replies with [`Response::WalkforwardReport`]. Served on EVERY build.
    RunWalkforwardProfile {
        /// The profile's TOML text, shipped verbatim. Must carry a `[walkforward]` table (the
        /// split count lives IN the profile — there is no wire override).
        profile_toml: String,
    },
    /// Run one study of the daemon's study folder over its own hist store and leave a run behind
    /// THERE ([`FEATURE_STUDY`]; its tier and recipe name, [`FEATURE_STUDY_FILES`]).
    ///
    /// ⚠ **BOXED**, like [`Request::RunSlice`]'s slice: four `String`s inline widen every
    /// [`Request`] value on every connection for a variant almost none of them carry.
    ///
    /// ⚠ **A COMPUTE verb, not a fourth plane on the wire.** Ruling R1 of
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` makes `research` a CLI
    /// plane; that spec's own fence says giving it its own DAEMON reopens ruling 7. A study opens
    /// the hist store and runs an engine, so it is served where every such verb is: `vike-backend
    /// backtesthub --addr`.
    RunStudy(Box<StudyRunRequestWire>),
    /// Enumerate the strategy FILES the daemon's project holds: the scan of its `strategies/`
    /// folder, each a path a profile's `[strategy] file` can name. Answered by
    /// [`Response::Strategies`] (or [`Response::Error`]). No strategy is compiled into the daemon, so
    /// the folder is the whole roster. Store-independent.
    ListStrategies,
    /// **Enumerate the roster a NAMED RUN would resolve** — and say whether the lane is ARMED.
    /// Answered by [`Response::NamedStrategies`] (or [`Response::Error`]).
    ///
    /// ⚠ **It is NOT [`Request::ListStrategies`]: the two answer different rosters.** That one is
    /// the strategy files of the daemon's `strategies/` folder; a named run resolves only a name
    /// compiled into the server, and since plan 2b none is.
    /// `docs/decisions/0064`'s decision 7: *the roster the verb SERVES is the roster it ENUMERATES*
    /// (naming into the dark is the lie `docs/decisions/0062`'s decision 5 fences against).
    ///
    /// Store-independent, so every build of the backtesthub serves it. Negotiated by
    /// [`FEATURE_NAMED_RUN`]; an UNARMED server still answers, with
    /// [`crate::named_run::NamedRunRoster::armed`] `false`.
    NamedStrategies,
    /// **Run ONE strategy the server already holds** — one strategy, one param set, one window,
    /// one pass. Answered by [`Response::NamedRun`] (or [`Response::Error`]).
    ///
    /// ⚠ **The only `Run*` verb that is `VerbScope::Read`**
    /// (`docs/decisions/0064-a-named-run-carries-no-source.md`): it carries
    /// [`crate::named_run::NamedRunParam`], which no script can occupy, and
    /// [`crate::named_run::NamedRunSpec`] has no field a search can occupy. [`required_scope`]'s
    /// doc and arm carry the argument and the bounds that are its CONDITION. **Adding a search
    /// dimension here is 0064's first reopener, not a small change.**
    ///
    /// Negotiated by [`FEATURE_NAMED_RUN`]; a server whose operator has not armed the lane answers
    /// [`crate::named_run::NamedRunOutcome::NotArmed`] rather than an error.
    RunNamed(Box<crate::named_run::NamedRunSpec>),
    /// The strategy files of the daemon's `strategies/` folder WITH their states — what a Studio
    /// lists. Answered by [`Response::StrategyFiles`] (or [`Response::Error`]). Negotiated by
    /// [`FEATURE_STRATEGY_FILES`]; [`Request::ListStrategies`] stays the bare path list an older
    /// client asks for. Store-independent.
    ListStrategyFiles,
    /// One strategy file's TEXT, by its path under the daemon's `strategies/` folder — what a Studio
    /// opens into its editor. Answered by [`Response::StrategyFileText`] (or [`Response::Error`]: a
    /// path outside the folder, a symbolic link, a missing or non-UTF-8 file). Negotiated by
    /// [`FEATURE_STRATEGY_FILES`].
    ReadStrategyFile {
        /// The path under the folder, `/`-separated (`trend/momentum.rs`).
        file: String,
    },
    /// Write one strategy file of the daemon's `strategies/` folder (created or replaced
    /// atomically) — what a Studio's Save sends (the owner's ruling of 2026-11-13: the backtesthub
    /// manages the strategy files; the builder only compiles). Answered by
    /// [`Response::StrategyFileSaved`] (or [`Response::Error`]). `VerbScope::Write`, and a daemon
    /// with no hub keys refuses it outright. A running live mount is not changed: it keeps the file
    /// it opened until it restarts. Negotiated by [`FEATURE_STRATEGY_FILES`].
    SaveStrategyFile {
        /// The path under the folder, `/`-separated (`trend/momentum.rs`).
        file: String,
        /// The file's whole new text.
        text: String,
    },
    /// Build a Rust strategy file's TEXT — what a Studio's Build sends. The daemon forwards it to the
    /// strategy builder, which only the server's daemons reach (decision 0288's amendment of
    /// 2026-11-16), and answers [`Response::StrategyFileBuilt`] with the text's sha256 (or
    /// [`Response::Error`]: the compiler's diagnostics verbatim, or why the builder did not answer).
    /// Blocking for as long as the compile takes. `VerbScope::Write`: it hands source to a compiler,
    /// and a daemon with no hub keys refuses it outright. Negotiated by
    /// [`FEATURE_BUILD_STRATEGY_FILE`].
    BuildStrategyFile {
        /// The path the text is built as, under the `strategies/` folder (`trend/momentum.rs`).
        file: String,
        /// The text to build.
        text: String,
        /// The SDK to build against, over the text's own `[strategy] sdk` pin: the Studio's SDK
        /// picker (decision 0296). `None` builds as the text says and writes no key; one is
        /// negotiated by [`FEATURE_STRATEGY_SDK`], as [`crate::wire_studio::StrategySpecWire::File`]'s.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdk: Option<u32>,
    },
    /// Run a Studio [`Request::RunSlice`], [`Request::RunParamscan`] or [`Request::RunWalkforward`],
    /// or a profile's [`Request::RunBacktest`] or [`Request::RunParamscanProfile`], and answer its SUMMARY
    /// ([`crate::summary`]): [`Response::RunSummary`], [`Response::ParamscanSummary`],
    /// [`Response::WalkforwardSummary`] or [`Response::ParamscanProfileSummary`], or the inner verb's
    /// [`Response::Error`] (a data plan answers as it does alone). The daemon runs the inner request exactly
    /// as it would alone, then holds the run's trades (and a scan's points) for [`Request::ReadTrades`] and
    /// [`Request::ReadPoint`], a single run's whole equity curve for [`Request::ReadCurve`], and a profile
    /// search's rows for [`Request::ReadParamscanRows`]. Any other inner request is refused in words.
    /// Negotiated by [`FEATURE_RUN_SUMMARY`] (a `RunBacktest` inside it by [`FEATURE_RUN_CURVE`] too, a
    /// `RunParamscanProfile` by [`FEATURE_PARAMSCAN_PROFILE_SUMMARY`]); its scope and reply wait are the inner
    /// request's.
    Summarized(Box<Request>),
    /// One page of a summarized run's trades ([`Response::TradesPage`]), in close order. `point`
    /// names a param scan's point (`None` for a single run). A run the daemon no longer holds is
    /// refused in words. Negotiated by [`FEATURE_RUN_SUMMARY`].
    ReadTrades {
        /// [`crate::summary::RunSummaryWire::run`] of the run.
        run: String,
        /// [`crate::summary::RunSummaryWire::point`] of the run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        point: Option<usize>,
        /// Which page, from 0.
        page: usize,
        /// Trades a page ([`crate::summary::TRADES_PAGE_SIZE`]; the daemon holds it at
        /// [`crate::summary::TRADES_PAGE_SIZE_MAX`]).
        page_size: usize,
    },
    /// The summary of one point of a summarized param scan ([`Response::RunSummary`]): what the
    /// Studio shows when the user opens a point other than the best. Negotiated by
    /// [`FEATURE_RUN_SUMMARY`].
    ReadPoint {
        /// [`crate::summary::ParamscanSummaryWire::run`] of the scan.
        run: String,
        /// The point's index in [`crate::summary::ParamscanSummaryWire::points`].
        point: usize,
    },
    /// One page of a summarized profile search's rows ([`Response::ParamscanRows`]), best first. `run` is
    /// [`crate::summary::ParamscanProfileSummaryWire::run`], held in the daemon's memory among its newest
    /// summarized runs; a search it no longer holds is refused in words. Negotiated by
    /// [`FEATURE_PARAMSCAN_PROFILE_SUMMARY`].
    ReadParamscanRows {
        /// [`crate::summary::ParamscanProfileSummaryWire::run`] of the search.
        run: String,
        /// Which page, from 0.
        page: usize,
        /// Rows a page ([`crate::summary::PARAMSCAN_ROWS_PAGE_SIZE`] when 0; the daemon holds it at
        /// [`crate::summary::PARAMSCAN_ROWS_PAGE_SIZE_MAX`]).
        page_size: usize,
    },
    /// One page of a summarized single run's WHOLE equity curve ([`Response::CurvePage`]), in step
    /// order, so no frame carries the whole curve and a run of any length reads back (decision 0078's
    /// amendment of 2026-11-18). `run` is [`crate::summary::RunSummaryWire::run`], held in the daemon's
    /// memory, or [`crate::summary::RunSummaryWire::saved_run`], read from the run the daemon saved; a
    /// run that is neither is refused in words. Negotiated by [`FEATURE_RUN_CURVE`].
    ReadCurve {
        /// The held run's name, or the saved run's id.
        run: String,
        /// Which page, from 0.
        page: usize,
        /// Steps a page ([`crate::summary::CURVE_PAGE_SIZE`]; the daemon holds it at
        /// [`crate::summary::CURVE_PAGE_SIZE_MAX`]).
        page_size: usize,
    },
    /// The studies of the server's study folder, both tiers, each with its recipe files
    /// ([`Response::Studies`]): what the Studio's Research pane lists, since studies run only on the
    /// server (the owner's ruling of 2026-11-19). Names only, never a file's text. Negotiated by
    /// [`FEATURE_STUDY_FILES`].
    ListStudies,
    /// One page of the server's saved runs, every kind, newest page first ([`Response::Runs`]): the
    /// Research pane's runs list. Negotiated by [`FEATURE_RUN_FILES`].
    ListRuns {
        /// Which page, from 0 (the newest runs).
        page: usize,
        /// Runs a page ([`crate::study::RUNS_PAGE_SIZE`] when 0; the daemon holds it at
        /// [`crate::study::RUNS_PAGE_SIZE_MAX`]).
        page_size: usize,
    },
    /// Bytes of one file of a saved run ([`Response::RunArtifact`]): a file the run's manifest declares
    /// as an artifact, never another path. Negotiated by [`FEATURE_RUN_FILES`].
    ReadRunArtifact {
        /// The run's id (its directory's name).
        run_id: String,
        /// The artifact's file name, as the run's manifest lists it.
        file: String,
        /// The first byte to read.
        offset: u64,
        /// How many bytes ([`crate::study::RUN_ARTIFACT_PAGE`] when 0, and never more).
        length: u64,
    },
    /// The versioned strategy SDKs the server holds, newest first, each with its number, commit,
    /// interface version and whether a strategy file builds against it there ([`Response::Sdks`]):
    /// what the Studio's SDK source view lists (decision 0296, verdict 3). Negotiated by
    /// [`FEATURE_SDK_SOURCE`].
    ListSdks,
    /// One file's text, or one folder's entries, of one SDK of the server ([`Response::SdkFile`]):
    /// the Studio's read-only SDK source view. Only a path under the SDK's
    /// [`crate::sdk_source::SDK_CRATES_FOLDER`] is served, a file only up to
    /// [`crate::sdk_source::SDK_FILE_MAX_BYTES`]; a path that leaves the SDK (`..`, an absolute path,
    /// a link) is refused. Negotiated by [`FEATURE_SDK_SOURCE`].
    ReadSdkFile {
        /// The SDK's name as [`crate::sdk_source::WireSdk::name`] lists it (`sdk-3`, `sdk-dev`).
        sdk: String,
        /// The path under the SDK's root, `/`-separated (`crates/vike-tactics/src/lib.rs`).
        path: String,
    },
    /// Run the wrapped request (a run verb, summarized or not) even when an IDENTICAL run's saved answer could answer
    /// it (decision 0078's amendment of 2026-11-20): a measurement, or a strategy the client knows answers otherwise
    /// each time. Answered as the wrapped request is, and saved as it is. Negotiated by [`FEATURE_RUN_FROM_SAVED`]
    /// (the client sends it only to a backtesthub that advertises it); its scope and reply wait are the inner
    /// request's.
    Fresh(Box<Request>),
}

/// One file of the daemon's `strategies/` folder as [`Response::StrategyFiles`] lists it: its path
/// and the states `vike_strategy::file::scan` read off it, in that scan's words.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireStrategyFile {
    /// The path under the folder, `/`-separated (`trend/momentum.rs`): the file's identity.
    pub file: String,
    /// A readable `.rs` file's build state: `not built`, `built <sha8>`, `build failed: <message>`
    /// or `cannot run here: <why>`. Empty for a `.rhai` file and for a file that could not be read.
    #[serde(default)]
    pub build: String,
    /// Why the file is listed broken (its header, or why it could not be read). Empty when it is not.
    #[serde(default)]
    pub broken: String,
    /// The file differs from the release's shipped copy of the same path (information only).
    #[serde(default)]
    pub edited: bool,
}

/// A server-to-client response. `Error` carries a server-side failure message so a client never has to
/// guess why a request did not produce its answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Reply to [`Request::Ping`].
    Pong,
    /// The backtest metrics summary as **JSON text** (`serde_json::to_string(&TradingResults)` —
    /// the exact shape the `backtest --json` bin emits). `TradingResults` derives `Deserialize`, so
    /// a client may parse it; the wire stays TEXT so the protocol is independent of
    /// `vike-backtest`'s internal serde surface and a field added there is no frame-compatibility
    /// question.
    ///
    /// ⚠ **On a msgpack connection ([`FEATURE_WIRE_MSGPACK`](vike_hub_protocol::features::FEATURE_WIRE_MSGPACK)) the six text reports cross without JSON inside JSON**:
    /// as the text in one msgpack string, and to a thin-link (LZ4) client as the report's TREE with its long lists
    /// as columns (`crate::msgpack_body::write_reply`). This field reads either form back into the very text
    /// (`crate::msgpack_body`'s module doc), so every caller still holds the JSON the server made.
    Report(#[serde(deserialize_with = "crate::msgpack_body::report_text")] String),
    /// Reply to [`Request::RunSlice`]: the rendered [`TradingRecordWire`] — the equity curve, closed
    /// trades, and the counters the Studio shows. A run FAILURE (bad script/slice/params) comes
    /// back as [`Response::Error`] (the one line `vike_studio_core::RunError::wire_line` writes).
    RunResult(TradingRecordWire),
    /// Reply to [`Request::RunParamscan`]: the ranked [`StudioParamscanWire`] — the entries plus
    /// the deflated-Sharpe / PBO scores. A run FAILURE comes back as [`Response::Error`].
    ///
    /// The tag on the wire is this name (it was `SweepResult` until the owner's ruling 20).
    ParamscanResult(StudioParamscanWire),
    /// Reply to [`Request::RunWalkforward`]: the stitched [`WalkforwardResultsWire`] — every step's
    /// results, the walk's overall results and its stitched equity curve. A run FAILURE comes back
    /// as [`Response::Error`]. Boxed since protocol 9 (its `TradingResults` are kilobytes, the
    /// enum's other variants bytes); serde writes a `Box` as its content, so the bytes are the same.
    WalkforwardResult(Box<WalkforwardResultsWire>),
    /// Reply to [`Request::RunParamscanProfile`]: the RANKED sweep as **JSON text**
    /// (`serde_json::to_string(&vike_backtest::search::paramscan::ParamscanReport)`, the `backtest --sweep
    /// --json` shape). Rows arrive ordered best-first, each with its own `TradingResults`, so a
    /// client renders server-computed stats and never re-implements a metric.
    ///
    /// Text for [`Response::Report`]'s decoupling reason, and because `ParamscanReport` is
    /// `Serialize`-ONLY and cannot be read back into a typed value at all.
    ///
    /// The tag on the wire is this name (it was `SweepReport` until the owner's ruling 20).
    ParamscanReport(#[serde(deserialize_with = "crate::msgpack_body::report_text")] String),
    /// Reply to [`Request::RunWalkforwardProfile`]: the stitched
    /// `vike_analytics::walkforward::WalkforwardResults` as **JSON text** — every step's results,
    /// the walk's overall results and the stitched equity curve. Text for the same reason as
    /// [`Response::ParamscanReport`]. The tag keeps its stored word.
    WalkforwardReport(#[serde(deserialize_with = "crate::msgpack_body::report_text")] String),
    /// Reply to [`Request::RunWalkforwardProfile`] for a profile whose `[walkforward]` declares the
    /// COMBINATORIAL form (`groups`/`test_groups`): the
    /// `vike_backtest::walkforward::combinatorial::CombinatorialWalkforwardReport` as **JSON text**,
    /// its paths each the whole walk-forward results. The profile picks it, as it picks the searched
    /// walk; an older daemon refuses the two keys at parse, so no feature string is needed.
    CombinatorialWalkforwardReport(
        #[serde(deserialize_with = "crate::msgpack_body::report_text")] String,
    ),
    /// Reply to [`Request::RunBacktest`] for a profile that declares a `[permutation]` table: the
    /// `vike_backtest::permutation::PermutationTestReport` as **JSON text** (the real score,
    /// every permuted one and the p-value). The profile picks it; an older daemon refuses the table
    /// at parse.
    PermutationReport(#[serde(deserialize_with = "crate::msgpack_body::report_text")] String),
    /// Reply to [`Request::RunStudy`]: the run now on the BACKEND's disk, as the JSON text of a
    /// [`crate::study::WireStudyReport`] (or, to a client that asked, a result that could not be saved,
    /// with its numbers). Text, as it was before that type existed, so every older reader still reads it.
    StudyReport(#[serde(deserialize_with = "crate::msgpack_body::report_text")] String),
    /// A server-side failure (invalid profile, missing strategy, data error, …), stringified.
    Error(String),
    /// Reply to [`Request::ListStrategies`]: the strategy files of the daemon's `strategies/` folder,
    /// each its path under that folder, in the scan's order.
    Strategies(Vec<String>),
    /// Reply to [`Request::NamedStrategies`]: the roster a NAMED RUN would resolve, plus whether
    /// the lane is armed. See [`crate::named_run::NamedRunRoster`] — and note `armed: false` is a
    /// SUCCESS.
    NamedStrategies(crate::named_run::NamedRunRoster),
    /// Reply to [`Request::RunNamed`]: what the run did, which is one of THREE outcomes and only
    /// one of them an error. See [`crate::named_run::NamedRunOutcome`].
    ///
    /// ⚠ BOXED for `clippy::large_enum_variant`: the `Ran` arm carries a whole
    /// [`crate::wire_studio::TradingRecordWire`]. The wire shape is unchanged.
    NamedRun(Box<crate::named_run::NamedRunOutcome>),
    /// Reply to [`Request::ListStrategyFiles`]: every file of the daemon's `strategies/` folder, in
    /// the scan's order, broken ones included.
    StrategyFiles(Vec<WireStrategyFile>),
    /// Reply to [`Request::ReadStrategyFile`]: the file named and its text.
    StrategyFileText {
        /// The path asked for, as asked.
        file: String,
        /// The file's text.
        text: String,
    },
    /// Reply to [`Request::SaveStrategyFile`]: the saved text's sha256 (lowercase hex) and the
    /// file's states as the list now shows them.
    StrategyFileSaved {
        /// The sha256 of the text written.
        sha256: String,
        /// The file as [`Response::StrategyFiles`] would list it now.
        file: WireStrategyFile,
    },
    /// Reply to [`Request::BuildStrategyFile`]: the built text's sha256 (lowercase hex), which names
    /// its artifact.
    StrategyFileBuilt {
        /// The sha256 of the text built.
        sha256: String,
    },
    /// Reply to a [`Request::Summarized`] [`Request::RunSlice`], and to [`Request::ReadPoint`]: the
    /// run's summary. BOXED for `clippy::large_enum_variant`; the wire shape is unchanged.
    RunSummary(Box<RunSummaryWire>),
    /// Reply to a [`Request::Summarized`] [`Request::RunParamscan`]: every point's numbers and the
    /// best point's summary.
    ParamscanSummary(Box<ParamscanSummaryWire>),
    /// Reply to a [`Request::Summarized`] [`Request::RunWalkforward`]: the walk with its stitched
    /// curve thinned.
    WalkforwardSummary(Box<WalkforwardSummaryWire>),
    /// Reply to [`Request::ReadTrades`]: one page of a held run's trades.
    TradesPage(TradesPageWire),
    /// Reply to a [`Request::Summarized`] [`Request::RunParamscanProfile`]: the search's header, its first page of
    /// rows (each point's overrides and metric table) and the run it was saved as. BOXED for
    /// `clippy::large_enum_variant`; the wire shape is unchanged.
    ParamscanProfileSummary(Box<ParamscanProfileSummaryWire>),
    /// Reply to [`Request::ReadParamscanRows`]: one page of a held search's rows.
    ParamscanRows(ParamscanRowsPageWire),
    /// Reply to [`Request::ReadCurve`]: one page of a run's whole equity curve.
    CurvePage(CurvePageWire),
    /// Reply to [`Request::ListStudies`]: the server's studies and its study folder.
    Studies(WireStudyListing),
    /// Reply to [`Request::ListRuns`]: one page of the server's saved runs.
    Runs(WireRunPage),
    /// Reply to [`Request::ReadRunArtifact`]: the bytes asked for, base64.
    RunArtifact(WireRunArtifact),
    /// Reply to [`Request::ListSdks`]: the server's SDKs.
    Sdks(WireSdkListing),
    /// Reply to [`Request::ReadSdkFile`]: the file's text, or the folder's entries.
    SdkFile(WireSdkFile),
}

#[path = "proto_tests.rs"]
#[cfg(test)]
mod proto_tests;
