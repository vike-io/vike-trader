//! The blocking backtesthub client: a [`HubConnection`] that speaks the [`crate::proto`] request/response
//! protocol. The `Run*` family (`run_backtest`, `run_slice`, `run_paramscan`, `run_walkforward`,
//! `run_paramscan_profile`, `run_study`, `run_walkforward_profile`) and the strategy-roster / named-run
//! verbs (`list_strategies`, `named_strategies`, `run_named`, `serves_named_run`).
//!
//! Connecting, authenticating and bounding a reply are [`HubConnection`]'s, shared with the datahub's client
//! (`crates/vike-hub-protocol/src/conn.rs`'s module doc carries each rule). [`BacktesthubClient`] derefs to
//! it, so `features`, `set_reply_timeout`, `cancel_handle`, `is_usable`, `requires_auth` and
//! `authenticated_scope` are called on the client itself.
//!
//! Every capability check here refuses BEFORE the write, sending nothing
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).

use std::io;
use std::net::ToSocketAddrs;
use std::ops::{Deref, DerefMut};
use std::time::Duration;

use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::conn::HubConnection;
use vike_hub_protocol::plane::Plane;

use crate::named_run::{NamedRunOutcome, NamedRunRoster, NamedRunSpec, validate_named_run};
use crate::proto::{
    FEATURE_NAMED_RUN, FEATURE_SEARCH_METHOD, FEATURE_SLIPPAGE_MODEL, FEATURE_STRATEGY_FILES,
    FEATURE_STUDY, FEATURE_WALKFORWARD_SEARCH, PROTO_VERSION, Request, Response,
    SearchSelectionWire, StudyRunRequestWire, WireStrategyFile, reply_wait, request_kind,
};
use crate::wire_studio::{
    BacktestResultWire, DataSliceWire, ParamscanWire, StrategyEngineConfigWire, StrategySpecWire,
    StudioParamscanWire, WalkforwardReportWire, WalkforwardWindowSearchWire, WalkforwardWire,
};

/// A connected backtesthub client: a [`HubConnection`] speaking this protocol. Drop it to close the
/// connection.
#[derive(Debug)]
pub struct BacktesthubClient {
    conn: HubConnection,
}

impl Deref for BacktesthubClient {
    type Target = HubConnection;

    fn deref(&self) -> &HubConnection {
        &self.conn
    }
}

impl DerefMut for BacktesthubClient {
    fn deref_mut(&mut self) -> &mut HubConnection {
        &mut self.conn
    }
}

/// The variant NAME of a response (no payload), its wire tag, for protocol-desync error messages, so we
/// never stringify a whole (possibly large) `RunResult` payload just to report "unexpected variant".
fn response_kind(r: &Response) -> &'static str {
    match r {
        Response::Welcome { .. } => "Welcome",
        Response::AuthOk { .. } => "AuthOk",
        Response::AuthDenied { .. } => "AuthDenied",
        Response::Pong => "Pong",
        Response::Report(_) => "Report",
        Response::RunResult(_) => "RunResult",
        Response::ParamscanResult(_) => "ParamscanResult",
        Response::WalkforwardResult(_) => "WalkforwardResult",
        Response::ParamscanReport(_) => "ParamscanReport",
        Response::WalkforwardReport(_) => "WalkforwardReport",
        Response::StudyReport(_) => "StudyReport",
        Response::Error(_) => "Error",
        Response::Strategies(_) => "Strategies",
        Response::NamedStrategies(_) => "NamedStrategies",
        Response::NamedRun(_) => "NamedRun",
        Response::StrategyFiles(_) => "StrategyFiles",
        Response::StrategyFileText { .. } => "StrategyFileText",
        Response::StrategyFileSaved { .. } => "StrategyFileSaved",
    }
}

impl BacktesthubClient {
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

    /// [`HubConnection::connect_authed_on`], speaking [`PROTO_VERSION`]: refused before the first byte of
    /// `Auth` unless the `Welcome` names `plane`. One node pair authenticates both daemons, so a Write
    /// key signed toward the data daemon would open a session that carries `Backfill` and `DeleteSeries`.
    pub fn connect_authed_on<A: ToSocketAddrs>(
        addr: A,
        keys: &HubKeys,
        scope: Scope,
        plane: Plane,
    ) -> io::Result<Self> {
        Ok(Self {
            conn: HubConnection::connect_authed_on(addr, keys, scope, plane, PROTO_VERSION)?,
        })
    }

    /// [`HubConnection::connect_with_keys`], speaking [`PROTO_VERSION`]: `connect_authed` under `scope` when
    /// `keys` is `Some`, plain `connect` when it is `None`. ⚠ UNGUARDED on the plane: a caller holding a
    /// key for ONE plane dials [`connect_authed_on`](Self::connect_authed_on) instead.
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

    /// Ship a backtest profile (its TOML text) to the server, run it there, and return the report
    /// as JSON text — the same JSON the `backtest --json` bin emits, which
    /// `serde_json::from_str::<BacktestReport>` parses.
    ///
    /// Both a transport failure and a server-side [`Response::Error`] surface through the ONE
    /// `Err(String)` channel, so a caller has a single place to handle "no report". Any other reply
    /// (a protocol desync) is likewise reported as `Err`.
    pub fn run_backtest(&mut self, profile_toml: &str) -> Result<String, String> {
        let request = Request::RunBacktest(profile_toml.to_string());
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::Report(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Report, got {}", response_kind(&other)))
            }
        }
    }

    /// Ship a Studio `RunSlice`: resolve `spec`, load `slice`, backtest it server-side, and
    /// return the rendered [`BacktestResultWire`] — the answer, NOT the bars/ticks. `params` is the
    /// optional cost/cash override (`None` = every engine field takes `StrategyEngineConfig::default()`).
    ///
    /// Both a transport failure and a server-side [`Response::Error`] (a bad script/slice/params, or
    /// a lean server that lacks `datafusion-serve`) surface through the ONE `Err(String)` channel —
    /// the error string is kind-first (`"compile: …"` / `"data: …"` / `"strategy: …"`), so a caller
    /// can classify the failure. Any other reply (a protocol desync) is likewise `Err`.
    pub fn run_slice(
        &mut self,
        spec: StrategySpecWire,
        slice: DataSliceWire,
        params: Option<StrategyEngineConfigWire>,
    ) -> Result<BacktestResultWire, String> {
        self.refuse_unserved_slippage(params.as_ref())?;
        // `slice` is boxed in the variant to keep the enum small (serde-transparent — see the proto).
        let request = Request::RunSlice { spec, slice: Box::new(slice), params };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::RunResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected RunResult, got {}", response_kind(&other)))
            }
        }
    }

    /// Ship a Studio `RunSweep`: resolve `spec`, load `slice`, run the parameter `sweep` grid
    /// server-side (`vike_backtesthub::studio_run::run_paramscan_slice`, next to the data), and return the
    /// ranked [`StudioParamscanWire`] — the answer, NOT the bars/ticks. `params` is the optional
    /// cost/cash override applied to EVERY grid point (`None` = `StrategyEngineConfig::default()`).
    ///
    /// Errors as [`run_slice`](Self::run_slice): ONE kind-first `Err(String)` channel.
    pub fn run_paramscan(
        &mut self,
        spec: StrategySpecWire,
        slice: DataSliceWire,
        sweep: ParamscanWire,
        params: Option<StrategyEngineConfigWire>,
    ) -> Result<StudioParamscanWire, String> {
        self.refuse_unserved_slippage(params.as_ref())?;
        let request =
            Request::RunParamscan { spec, slice: Box::new(slice), paramscan: sweep, params };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::ParamscanResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SweepResult, got {}", response_kind(&other)))
            }
        }
    }

    /// Ship a Studio `RunWalkforward`: resolve `spec`, load `slice`, walk it forward over
    /// `walkforward.n_splits` anchored OOS windows server-side
    /// (`vike_backtesthub::studio_run::run_walkforward_slice`), and return the stitched
    /// [`WalkforwardReportWire`]. `params` is the optional cost/cash override applied to every OOS
    /// window (`None` = `StrategyEngineConfig::default()`). Errors as [`run_slice`](Self::run_slice) (a
    /// tick/multi-symbol slice is one).
    ///
    /// ⚠ **A per-window SEARCH an older daemon would silently DOWNGRADE is refused here, WITHOUT
    /// SENDING.** [`Request`] has no `deny_unknown_fields`, so a daemon predating
    /// `WalkforwardWire::search` drops the field, runs the FIXED walk and answers a well-formed
    /// [`Response::WalkforwardResult`] — an answer to a different question, with no reply to
    /// inspect. The refusal names [`crate::proto::FEATURE_WALKFORWARD_SEARCH`]. A fixed walk — no
    /// `search`, or an explicit `none` carrying no grid — is SENT unchanged to every peer.
    pub fn run_walkforward(
        &mut self,
        spec: StrategySpecWire,
        slice: DataSliceWire,
        walkforward: WalkforwardWire,
        params: Option<StrategyEngineConfigWire>,
    ) -> Result<WalkforwardReportWire, String> {
        // ⚠ BEFORE THE WRITE, AND IT HAS TO BE — see this method's doc.
        let needs_capability =
            walkforward.search.as_ref().is_some_and(WalkforwardWindowSearchWire::needs_capability);
        if needs_capability && !self.features().iter().any(|f| f == FEATURE_WALKFORWARD_SEARCH) {
            return Err(format!(
                "backtest daemon does not advertise `{FEATURE_WALKFORWARD_SEARCH}` (advertised: \
                 {:?}) — nothing was sent. A per-window walk-forward SEARCH needs a newer \
                 `vike-backend backtesthub --addr` with the Studio runners mounted; this verb is \
                 capability-negotiated, not version-gated. An older daemon would DROP the search, \
                 run the FIXED walk and report success — which is not a coarser answer to the same \
                 question but an answer to a different one, and that silent downgrade is what this \
                 refusal exists to prevent. Upgrade the daemon, or ask for the fixed walk.",
                self.features()
            ));
        }
        self.refuse_unserved_slippage(params.as_ref())?;
        let request = Request::RunWalkforward { spec, slice: Box::new(slice), walkforward, params };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::WalkforwardResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected WalkforwardResult, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Ship a profile (its TOML text) to be run as a PARAMETER SWEEP over its own `[sweep]` table
    /// and return the RANKED report as JSON text — the sweep sibling of
    /// [`run_backtest`](Self::run_backtest).
    ///
    /// `rank_by` names the server-side ranking (`"sharpe"` / `"return"` / `"max_dd"` / `"equity"`,
    /// case-insensitive, or `"multi"` — the composite objective); `None` = the `sharpe` default.
    /// Rows come back ALREADY ordered best-first, each carrying its own `BacktestReport`, so a
    /// caller renders server-computed stats rather than re-implementing a metric.
    ///
    /// `search` names WHICH METHOD walks the grid, and that method's own knobs, as the TOKENS the
    /// operator typed — see [`crate::proto::SearchSelectionWire`] for why they are strings rather than typed
    /// scalars (one parser, `vike_backtest::search::select`, on whichever side runs).
    ///
    /// ⚠ **A selector an older daemon would silently DOWNGRADE is refused here, WITHOUT SENDING.**
    /// [`Request`] has no `deny_unknown_fields`, so a daemon predating the field drops it, runs the
    /// exhaustive grid and answers a normal report — there is no reply to inspect. The refusal
    /// names [`crate::proto::FEATURE_SEARCH_METHOD`]. An ordinary grid search — no selector, or an
    /// explicit `grid` — is SENT unchanged to every peer.
    ///
    /// Unlike [`run_paramscan`](Self::run_paramscan) (the Studio DTO verb) this carries the WHOLE
    /// profile, so the whole `[engine]` applies — the `fee` schedule included — and a LEAN server
    /// serves it too. Both a transport failure and a server-side [`Response::Error`] surface
    /// through the ONE `Err(String)` channel.
    pub fn run_paramscan_profile(
        &mut self,
        profile_toml: &str,
        rank_by: Option<&str>,
        search: Option<&SearchSelectionWire>,
    ) -> Result<String, String> {
        // ⚠ THE REFUSAL IS BEFORE THE WRITE (this method's doc). `rank_by == "multi"` joins it for
        // a different reason: an old daemon resolves that string through `RankMetric::from_str_ci`,
        // whose four arms have no `multi`, and answers an error naming a set this client offers
        // five of.
        let needs_capability = search.is_some_and(SearchSelectionWire::needs_capability)
            || rank_by.is_some_and(|r| r.eq_ignore_ascii_case("multi"));
        if needs_capability && !self.features().iter().any(|f| f == FEATURE_SEARCH_METHOD) {
            return Err(format!(
                "backtest daemon does not advertise `{FEATURE_SEARCH_METHOD}` (advertised: {:?}) \
                 — nothing was sent. A search METHOD (--optimizer/--euler-depth/--trials/--seed) \
                 and --rank-by multi need a newer `vike-backend backtesthub --addr`; this verb is \
                 capability-negotiated, not version-gated. An older daemon would DROP the field, \
                 run the exhaustive grid and report success, which is the silent downgrade this \
                 refusal exists to prevent — upgrade the daemon, or drop the flag to search the \
                 grid on the server.",
                self.features()
            ));
        }
        let request = Request::RunParamscanProfile {
            profile_toml: profile_toml.to_string(),
            rank_by: rank_by.map(str::to_string),
            // An EMPTY selector serializes as `None`: an ordinary grid search's frame is
            // byte-identical to one from before the field existed.
            search: search.filter(|s| !s.is_empty()).cloned(),
        };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::ParamscanReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SweepReport, got {}", response_kind(&other)))
            }
        }
    }

    /// Ask the backtesthub to run one compiled study over the store IT holds, and return the
    /// minted run as JSON text ([`Response::StudyReport`]).
    ///
    /// ⚠ `crates/vike-cli/src/cmd/study.rs` checks the same advertisement first and prints the
    /// richer refusal (the `vike-backend study` escape hatch with this run's arguments filled in);
    /// this one exists for every OTHER caller, so the rule lives with the protocol.
    ///
    /// ⚠ [`crate::proto::FEATURE_STUDY`] is MOUNT-conditional on the serving side: the backtesthub
    /// advertises it only when a study runner was injected into it. See that constant.
    pub fn run_study(&mut self, study: &StudyRunRequestWire) -> Result<String, String> {
        if !self.features().iter().any(|f| f == FEATURE_STUDY) {
            return Err(format!(
                "backtest daemon does not advertise `{FEATURE_STUDY}` (advertised: {:?}) — nothing \
                 was sent. A study is served by `vike-backend backtesthub --addr` with the study \
                 runner MOUNTED; this verb is capability-negotiated, not version-gated. Run it on \
                 the box that holds the store instead: `vike-backend study`.",
                self.features()
            ));
        }
        match self
            .exchange(&Request::RunStudy(Box::new(study.clone())))
            .map_err(|e| e.to_string())?
        {
            Response::StudyReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected StudyReport, got {}", response_kind(&other)))
            }
        }
    }

    /// Ship a profile (its TOML text) to be WALKED FORWARD over its own `[walkforward].n_splits`
    /// anchored out-of-sample windows and return the stitched report as JSON text — the
    /// walk-forward sibling of [`run_backtest`](Self::run_backtest).
    ///
    /// The split count lives IN the profile (there is no wire override), so one file describes the
    /// whole run. Server-side this is bar-mode + single-series only; a tick or multi-symbol profile
    /// comes back as a clean [`Response::Error`] on the ONE `Err(String)` channel, like every other
    /// failure.
    pub fn run_walkforward_profile(&mut self, profile_toml: &str) -> Result<String, String> {
        let request = Request::RunWalkforwardProfile { profile_toml: profile_toml.to_string() };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::WalkforwardReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected WalkforwardReport, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Enumerate the strategy files the daemon's `strategies/` folder holds — the paths a profile's
    /// `[strategy] file` can name. A transport failure or a server-side [`Response::Error`] surfaces
    /// through the ONE `Err(String)` channel; any other reply (a protocol desync) is likewise `Err`.
    pub fn list_strategies(&mut self) -> Result<Vec<String>, String> {
        match self.exchange(&Request::ListStrategies).map_err(|e| e.to_string())? {
            Response::Strategies(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Strategies, got {}", response_kind(&other)))
            }
        }
    }

    /// The strategy files of the daemon's `strategies/` folder with their states
    /// ([`Request::ListStrategyFiles`]). Refused LOCALLY, nothing sent, by a daemon that does not
    /// advertise [`FEATURE_STRATEGY_FILES`].
    pub fn list_strategy_files(&mut self) -> Result<Vec<WireStrategyFile>, String> {
        self.refuse_unadvertised_strategy_files()?;
        match self.exchange(&Request::ListStrategyFiles).map_err(|e| e.to_string())? {
            Response::StrategyFiles(files) => Ok(files),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected StrategyFiles, got {}",
                response_kind(&other)
            )),
        }
    }

    /// One strategy file's text, by its path under the daemon's `strategies/` folder
    /// ([`Request::ReadStrategyFile`]). Refused LOCALLY, nothing sent, by a daemon that does not
    /// advertise [`FEATURE_STRATEGY_FILES`].
    pub fn read_strategy_file(&mut self, file: &str) -> Result<String, String> {
        self.refuse_unadvertised_strategy_files()?;
        let request = Request::ReadStrategyFile { file: file.to_string() };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::StrategyFileText { text, .. } => Ok(text),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected StrategyFileText, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Write one strategy file of the daemon's `strategies/` folder ([`Request::SaveStrategyFile`]):
    /// the sha256 of the text written and the file's states as the list now shows them. Needs a
    /// connection authenticated at the Control scope; refused LOCALLY, nothing sent, by a daemon
    /// that does not advertise [`FEATURE_STRATEGY_FILES`].
    pub fn save_strategy_file(
        &mut self,
        file: &str,
        text: &str,
    ) -> Result<(String, WireStrategyFile), String> {
        self.refuse_unadvertised_strategy_files()?;
        let request = Request::SaveStrategyFile { file: file.to_string(), text: text.to_string() };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::StrategyFileSaved { sha256, file } => Ok((sha256, file)),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected StrategyFileSaved, got {}",
                response_kind(&other)
            )),
        }
    }

    /// The strategy-file verbs' one local refusal: a daemon that does not advertise
    /// [`FEATURE_STRATEGY_FILES`] is older than them.
    fn refuse_unadvertised_strategy_files(&self) -> Result<(), String> {
        if self.features().iter().any(|f| f == FEATURE_STRATEGY_FILES) {
            return Ok(());
        }
        Err(format!(
            "this backtest daemon does not advertise `{FEATURE_STRATEGY_FILES}` (advertised: {:?}) — \
             nothing was sent. It is older than the strategy-file list: run a newer \
             `vike-backend backtesthub --addr`.",
            self.features()
        ))
    }

    /// **Enumerate the roster a NAMED RUN would resolve, and whether the lane is armed** —
    /// [`Request::NamedStrategies`].
    ///
    /// ⚠ **Not [`Self::list_strategies`], and the two answer different rosters.** That one returns
    /// the strategy files of the daemon's folder; this one returns what a named run
    /// can actually resolve, which since plan 2b is nothing (no strategy is compiled into the
    /// server). A picker built on the wrong one offers names that will be refused.
    ///
    /// ⚠ **`armed: false` is a SUCCESS, and it arrives with an EMPTY roster.** The arming gates the
    /// NAMES as well as the run (`docs/decisions/0064`'s decision 8 leg 3: publishing the
    /// operator's compiled-in strategy names is itself the disclosure arming makes an operator's
    /// act). Render [`NamedRunRoster::unarmed_note`], never the bare empty list: the note separates
    /// "withheld" from "this daemon holds none", which is also why
    /// [`crate::proto::FEATURE_NAMED_RUN`] is a BUILD fact rather than a per-lane one.
    ///
    /// ONE local refusal, before a frame: the capability check.
    pub fn named_strategies(&mut self) -> Result<NamedRunRoster, String> {
        self.refuse_unadvertised_named_run()?;
        match self.exchange(&Request::NamedStrategies).map_err(|e| e.to_string())? {
            Response::NamedStrategies(roster) => Ok(roster),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected NamedStrategies, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **Run ONE strategy the server already holds** — [`Request::RunNamed`],
    /// `docs/decisions/0064-a-named-run-carries-no-source.md`.
    ///
    /// The one `Run*` verb reachable from a connection holding only an OBSERVE key, which is the
    /// point of the record: `crates/vike-app-core/src/backend/backend_registry.rs`'s
    /// `datahub_observe_key` states why the desktop must never hold the datahub's CONTROL key.
    ///
    /// **Two local refusals, both before a frame is sent**, so an operator sees the rule rather than
    /// a round trip:
    ///
    /// 1. the capability check — its message distinguishes an OLD server from an unarmed one,
    ///    which this verb can do and `vike_datahub_client::DatahubClient::seed_series` cannot, because
    ///    [`crate::proto::FEATURE_NAMED_RUN`] is a build fact and the arming rides in the answer;
    /// 2. [`validate_named_run`] — the SAME function the server's door calls, duplicated here for
    ///    the MESSAGE and never for the enforcement: the server re-checks every bound before it
    ///    touches the store.
    ///
    /// ⚠ The capability check is LOAD-BEARING, not polite: never send a bounded request to a peer
    /// that has not said it understands the bound (a future server that grew a field would drop
    /// it silently, the [`Self::run_paramscan_profile`] failure).
    ///
    /// ⚠ **`Ok(NamedRunOutcome::NotArmed)` is a SUCCESS and must not be rendered as a failure** —
    /// it means the operator has not armed the lane; nothing ran.
    pub fn run_named(&mut self, spec: &NamedRunSpec) -> Result<NamedRunOutcome, String> {
        self.refuse_unadvertised_named_run()?;
        validate_named_run(spec)?;
        match self
            .exchange(&Request::RunNamed(Box::new(spec.clone())))
            .map_err(|e| e.to_string())?
        {
            Response::NamedRun(outcome) => Ok(*outcome),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected NamedRun, got {}", response_kind(&other)))
            }
        }
    }

    /// Whether this server advertised the named-run capability — the question a picker asks BEFORE
    /// it offers a Run control, so an old server produces a sentence rather than a button that
    /// always fails (`vike_datahub_client::DatahubClient::serves_venue_catalog`'s leg: an un-advertised capability must not
    /// render as an EMPTY roster).
    pub fn serves_named_run(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_NAMED_RUN)
    }

    /// The ONE capability refusal both named-run verbs share.
    fn refuse_unadvertised_named_run(&self) -> Result<(), String> {
        if self.serves_named_run() {
            return Ok(());
        }
        Err(format!(
            "this backtesthub does not advertise `{FEATURE_NAMED_RUN}` (advertised: {:?}) — \
             nothing was sent. Unlike the data daemon's armed lanes, this string is a BUILD fact, \
             so its absence means the server PREDATES the named-run verb rather than that its \
             operator declined to arm the lane: an unarmed server advertises it and answers \
             `NotArmed`. Check the address too — the `Run*` verbs are served by \
             `vike-backend backtesthub --addr` (default 127.0.0.1:7880), not by the datahub.",
            self.features()
        ))
    }

    /// `Err`, with nothing sent, when `params` name a slippage MODEL and this daemon does not
    /// advertise [`FEATURE_SLIPPAGE_MODEL`]: an older daemon drops the model and runs the value as
    /// a flat fraction of the price, so two ticks would be charged as 200%.
    fn refuse_unserved_slippage(
        &self,
        params: Option<&StrategyEngineConfigWire>,
    ) -> Result<(), String> {
        if !params.is_some_and(StrategyEngineConfigWire::needs_capability)
            || self.features().iter().any(|f| f == FEATURE_SLIPPAGE_MODEL)
        {
            return Ok(());
        }
        Err(format!(
            "backtest daemon does not advertise `{FEATURE_SLIPPAGE_MODEL}` (advertised: {:?}) — \
             nothing was sent. A slippage model (ticks, market impact, or an explicit none) needs \
             a newer `vike-backend backtesthub --addr`: an older daemon would drop the model and \
             charge the value as a fraction of the price. Upgrade the daemon, or run with the \
             percent model's value alone.",
            self.features()
        ))
    }
}
