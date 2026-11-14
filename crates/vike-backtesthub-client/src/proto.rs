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
//!   back the rendered [`BacktestResultWire`] / ranked / stitched answer, never the bars or ticks.
//! - [`Request::RunParamscanProfile`] / [`Request::RunWalkforwardProfile`] are their PROFILE-shaped
//!   twins: the profile's TOML text crosses verbatim and the SERVER parses it with
//!   `ProfileBacktest::from_toml_str` before running `vike_backtest::harness::run_paramscan` /
//!   `harness::run_walkforward`, so the WHOLE `[engine]` surface applies — the `fee` SCHEDULE,
//!   the impact model's coefficients, `[engine.resolution]`, `[risk]`, `snap_to_properties`, tick mode, cross-venue
//!   `[[data.series]]` — none of which a `DataSliceWire` + `StrategyEngineConfigWire` pair can carry, under ONE
//!   parser.
//!
//! # One handshake, two daemons, two enums
//!
//! `Hello`, `Auth`, `Ping` and their replies are spelled here and in `vike_datahub_client::proto` with
//! the same bytes, because both daemons answer them; `vike_hub_protocol::handshake` is the one body of the
//! keyed handshake. Every other tag is this enum's alone. A verb of the DATA plane does not decode here,
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
use vike_hub_protocol::auth::Scope;
use vike_model::run_request::search_methods::DEFAULT_SEARCH_METHOD;

use crate::wire_studio::{
    BacktestResultWire, DataSliceWire, ParamscanWire, StrategyEngineConfigWire, StrategySpecWire,
    StudioParamscanWire, WalkforwardReportWire, WalkforwardWire,
};

mod features;
mod verbs;

pub use features::{
    FEATURE_NAMED_RUN, FEATURE_SEARCH_METHOD, FEATURE_SLIPPAGE_MODEL, FEATURE_STRATEGY_FILES,
    FEATURE_STUDY, FEATURE_WALKFORWARD_SEARCH, STUDIO_RUNNER_SENTINEL,
};
pub use verbs::{reply_wait, request_kind, required_scope};

/// The backtesthub wire protocol version, negotiated by the [`Request::Hello`] / [`Response::Welcome`]
/// handshake: the client fails the connection with a legible, both-numbers error on a mismatch. It
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
///   ([`FEATURE_STRATEGY_FILES`]).
/// - `8` — the phase-4 renames the owner ruled (P4.5c): the four `paramscan` tags
///   (`RunParamscan`/`RunParamscanProfile`/`ParamscanResult`/`ParamscanReport`) and the `paramscan` key
///   of [`Request::RunParamscan`] are their Rust names on the wire, where they used to be spelled `sweep`
///   by a serde rename (ruling 20), and the report JSON's renamed keys. A peer from before them could not
///   decode those frames, so an old peer is refused at the handshake, naming both numbers
///   (`proto_tests.rs`'s `a_daemon_from_before_the_phase_four_renames_is_refused_at_the_handshake`).
///   `crates/vike-backtesthub-client/tests/wire_tag_fixtures.rs` round-trips the re-recorded frames.
pub const PROTO_VERSION: u32 = 8;

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
/// profile: `vike_backtest::harness::ProfileBacktest` is `#[serde(deny_unknown_fields)]`, and every
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

/// One STUDY run as it crosses the wire: which compiled study, its recipe's TEXT, and the window.
///
/// ⚠ **The recipe travels as TEXT and never as a PATH**: the backend is typically a different
/// machine, so a path would resolve against ITS filesystem (`crates/vike-cli/src/cmd/study.rs`'s
/// module doc argues it; the same division [`Request::RunBacktest`] draws over profile TOML).
///
/// ⚠ **`from`/`to` are the operator's own strings and are NOT parsed here.** The backend owns that
/// grammar — `YYYY-MM-DD`, `YYYY-MM-DDTHH`, or bare unix SECONDS — and a client parser would be a
/// second answer to what the string means ([`SearchSelectionWire`]'s rule).
///
/// ⚠ **No store root and no trainer path, deliberately**: both name paths on the BACKEND's box;
/// `cmd/study.rs`'s `parse` refuses `--store` and `--lightgbm` BY NAME for that reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyRunRequestWire {
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
/// The profile verbs ([`Request::RunBacktest`] and its sweep / walk-forward siblings) carry the profile
/// as **TOML text**, not the `vike_backtest` structs: the server parses it with
/// `ProfileBacktest::from_toml_str` — the path a `.toml` file takes — so the wire schema stays decoupled
/// from `vike-backtest`'s internal serde surface across engine refactors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// The version-handshake opener: the client's [`PROTO_VERSION`]. Answered by
    /// [`Response::Welcome`] (the server's version, its feature list and — on a KEYED server — the
    /// per-connection auth `nonce`). On a key-less server it negotiates the version and gates nothing;
    /// on a keyed one it is MANDATORY and must be the FIRST frame, followed by [`Request::Auth`].
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
    /// `vike_backtest::harness::run_paramscan` next to the data and replies with
    /// [`Response::ParamscanReport`], server-RANKED: the whole `[engine]` applies and the client
    /// needs no profile→DTO mapping and no metric math. Served on EVERY build (DataFusion-free).
    ///
    /// The tag on the wire is this name (it was `RunSweepProfile` until the owner's ruling 20).
    RunParamscanProfile {
        /// The profile's TOML text, shipped verbatim. Must carry a `[paramscan]` table (the
        /// `[sweep]` spelling still loads — see `vike_backtest::harness::ProfileBacktest`).
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
        search: Option<SearchSelectionWire>,
    },
    /// Run an anchored WALK-FORWARD from the profile's own `[walkforward]` table, given the profile
    /// as its **TOML text** — the walk-forward sibling of [`Request::RunBacktest`] and the
    /// profile-shaped twin of [`Request::RunWalkforward`]. The server runs
    /// `vike_backtest::harness::run_walkforward` (bar mode, ONE series, `WalkforwardMode::Anchored`) and
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
    /// Reply to [`Request::Hello`]: the server's [`PROTO_VERSION`] and its advertised `features`.
    /// The client compares the version and fails loudly on a mismatch. On a KEYED server it additionally
    /// carries `nonce` and advertises `vike_hub_protocol::auth::FEATURE_AUTH`.
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
    /// Reply to [`Request::RunSlice`]: the rendered [`BacktestResultWire`] — the equity curve, closed
    /// trades, and the counters the Studio shows. A run FAILURE (bad script/slice/params) comes
    /// back as [`Response::Error`] (the one line `vike_studio_core::RunError::wire_line` writes).
    RunResult(BacktestResultWire),
    /// Reply to [`Request::RunParamscan`]: the ranked [`StudioParamscanWire`] — the entries plus
    /// the deflated-Sharpe / PBO scores. A run FAILURE comes back as [`Response::Error`].
    ///
    /// The tag on the wire is this name (it was `SweepResult` until the owner's ruling 20).
    ParamscanResult(StudioParamscanWire),
    /// Reply to [`Request::RunWalkforward`]: the stitched [`WalkforwardReportWire`] — the OOS
    /// windows, stitched equity curve and summary stats. A run FAILURE comes back as
    /// [`Response::Error`].
    WalkforwardResult(WalkforwardReportWire),
    /// Reply to [`Request::RunParamscanProfile`]: the RANKED sweep as **JSON text**
    /// (`serde_json::to_string(&vike_backtest::harness::ParamscanReport)`, the `backtest --sweep
    /// --json` shape). Rows arrive ordered best-first, each with its own `BacktestReport`, so a
    /// client renders server-computed stats and never re-implements a metric.
    ///
    /// Text for [`Response::Report`]'s decoupling reason, and because `ParamscanReport` is
    /// `Serialize`-ONLY and cannot be read back into a typed value at all.
    ///
    /// The tag on the wire is this name (it was `SweepReport` until the owner's ruling 20).
    ParamscanReport(String),
    /// Reply to [`Request::RunWalkforwardProfile`]: the stitched
    /// `vike_analytics::result::WalkforwardReport` as **JSON text** — the OOS windows, the
    /// stitched equity curve, and the three summary scalars. Text for the same reason as
    /// [`Response::ParamscanReport`].
    WalkforwardReport(String),
    /// Reply to [`Request::RunStudy`]: the run now on the BACKEND's disk, as JSON text — the report
    /// types are Serialize-ONLY, so a typed variant would need `Deserialize` on the whole
    /// run-manifest and study-outcome tree.
    StudyReport(String),
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
    /// [`crate::wire_studio::BacktestResultWire`]. The wire shape is unchanged.
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
}

#[path = "proto_tests.rs"]
#[cfg(test)]
mod proto_tests;
