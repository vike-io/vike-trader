//! `DatahubClient`'s compute-daemon verbs: the `Run*` family (`run_backtest`, `run_slice`,
//! `run_paramscan`, `run_walkforward`, `run_paramscan_profile`, `run_study`,
//! `run_walkforward_profile`) and the strategy-roster / named-run verbs (`list_strategies`,
//! `named_strategies`, `run_named`, `serves_named_run`). `use super::*` reaches the parent module's
//! imports and shared helpers.
//!
//! Every capability check here refuses BEFORE the write, sending nothing
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).

use super::*;

impl DatahubClient {
    /// Ship a backtest profile (its TOML text) to the server, run it there, and return the report
    /// as JSON text — the same JSON the `backtest --json` bin emits, which
    /// `serde_json::from_str::<BacktestReport>` parses.
    ///
    /// Both a transport failure and a server-side [`Response::Error`] surface through the ONE
    /// `Err(String)` channel, so a caller has a single place to handle "no report". Any other reply
    /// (a protocol desync) is likewise reported as `Err`.
    pub fn run_backtest(&mut self, profile_toml: &str) -> Result<String, String> {
        let request = Request::RunBacktest(profile_toml.to_string());
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Report(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Report, got {}", resp_kind(&other))),
        }
    }

    /// Ship a Studio `RunSlice`: resolve `spec`, load `slice`, backtest it server-side, and
    /// return the rendered [`WireRunResult`] — the answer, NOT the bars/ticks. `params` is the
    /// optional cost/cash override (`None` = every engine field takes `EngineParams::default()`).
    ///
    /// Both a transport failure and a server-side [`Response::Error`] (a bad script/slice/params, or
    /// a lean server that lacks `serve-datafusion`) surface through the ONE `Err(String)` channel —
    /// the error string is kind-first (`"compile: …"` / `"data: …"` / `"strategy: …"`), so a caller
    /// can classify the failure. Any other reply (a protocol desync) is likewise `Err`.
    pub fn run_slice(
        &mut self,
        spec: WireSpec,
        slice: WireSlice,
        params: Option<WireEngineParams>,
    ) -> Result<WireRunResult, String> {
        // `slice` is boxed in the variant to keep the enum small (serde-transparent — see the proto).
        let request = Request::RunSlice { spec, slice: Box::new(slice), params };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::RunResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected RunResult, got {}", resp_kind(&other))),
        }
    }

    /// Ship a Studio `RunSweep`: resolve `spec`, load `slice`, run the parameter `sweep` grid
    /// server-side (`vike_studio_core::run_paramscan_slice`, next to the data), and return the
    /// ranked [`WireParamscanResult`] — the answer, NOT the bars/ticks. `params` is the optional
    /// cost/cash override applied to EVERY grid point (`None` = `EngineParams::default()`).
    ///
    /// Errors as [`run_slice`](Self::run_slice): ONE kind-first `Err(String)` channel.
    pub fn run_paramscan(
        &mut self,
        spec: WireSpec,
        slice: WireSlice,
        sweep: WireParamscan,
        params: Option<WireEngineParams>,
    ) -> Result<WireParamscanResult, String> {
        let request =
            Request::RunParamscan { spec, slice: Box::new(slice), paramscan: sweep, params };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::ParamscanResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SweepResult, got {}", resp_kind(&other)))
            }
        }
    }

    /// Ship a Studio `RunWalkforward`: resolve `spec`, load `slice`, walk it forward over
    /// `walkforward.n_splits` anchored OOS windows server-side
    /// (`vike_studio_core::run_walkforward_slice`), and return the stitched
    /// [`WireWalkforwardResult`]. `params` is the optional cost/cash override applied to every OOS
    /// window (`None` = `EngineParams::default()`). Errors as [`run_slice`](Self::run_slice) (a
    /// tick/multi-symbol slice is one).
    ///
    /// ⚠ **A per-window SEARCH an older daemon would silently DOWNGRADE is refused here, WITHOUT
    /// SENDING.** [`Request`] has no `deny_unknown_fields`, so a daemon predating
    /// `WireWalkforward::search` drops the field, runs the FIXED walk and answers a well-formed
    /// [`Response::WalkforwardResult`] — an answer to a different question, with no reply to
    /// inspect. The refusal names [`crate::FEATURE_WALKFORWARD_SEARCH`]. A fixed walk — no
    /// `search`, or an explicit `none` carrying no grid — is SENT unchanged to every peer.
    pub fn run_walkforward(
        &mut self,
        spec: WireSpec,
        slice: WireSlice,
        walkforward: WireWalkforward,
        params: Option<WireEngineParams>,
    ) -> Result<WireWalkforwardResult, String> {
        // ⚠ BEFORE THE WRITE, AND IT HAS TO BE — see this method's doc.
        let needs_capability =
            walkforward.search.as_ref().is_some_and(WireWindowSearch::needs_capability);
        if needs_capability && !self.features.iter().any(|f| f == FEATURE_WALKFORWARD_SEARCH) {
            return Err(format!(
                "backtest daemon does not advertise `{FEATURE_WALKFORWARD_SEARCH}` (advertised: \
                 {:?}) — nothing was sent. A per-window walk-forward SEARCH needs a newer \
                 `vike-backend backtest --addr` with the Studio runners mounted; this verb is \
                 capability-negotiated, not version-gated. An older daemon would DROP the search, \
                 run the FIXED walk and report success — which is not a coarser answer to the same \
                 question but an answer to a different one, and that silent downgrade is what this \
                 refusal exists to prevent. Upgrade the daemon, or ask for the fixed walk.",
                self.features
            ));
        }
        let request = Request::RunWalkforward { spec, slice: Box::new(slice), walkforward, params };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::WalkforwardResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected WalkforwardResult, got {}",
                resp_kind(&other)
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
    /// operator typed — see [`crate::WireSearch`] for why they are strings rather than typed
    /// scalars (one parser, `vike_backtest::search::select`, on whichever side runs).
    ///
    /// ⚠ **A selector an older daemon would silently DOWNGRADE is refused here, WITHOUT SENDING.**
    /// [`Request`] has no `deny_unknown_fields`, so a daemon predating the field drops it, runs the
    /// exhaustive grid and answers a normal report — there is no reply to inspect. The refusal
    /// names [`crate::FEATURE_SEARCH_METHOD`]. An ordinary grid search — no selector, or an
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
        search: Option<&WireSearch>,
    ) -> Result<String, String> {
        // ⚠ THE REFUSAL IS BEFORE THE WRITE (this method's doc). `rank_by == "multi"` joins it for
        // a different reason: an old daemon resolves that string through `RankMetric::from_str_ci`,
        // whose four arms have no `multi`, and answers an error naming a set this client offers
        // five of.
        let needs_capability = search.is_some_and(WireSearch::needs_capability)
            || rank_by.is_some_and(|r| r.eq_ignore_ascii_case("multi"));
        if needs_capability && !self.features.iter().any(|f| f == FEATURE_SEARCH_METHOD) {
            return Err(format!(
                "backtest daemon does not advertise `{FEATURE_SEARCH_METHOD}` (advertised: {:?}) \
                 — nothing was sent. A search METHOD (--optimizer/--euler-depth/--trials/--seed) \
                 and --rank-by multi need a newer `vike-backend backtest --addr`; this verb is \
                 capability-negotiated, not version-gated. An older daemon would DROP the field, \
                 run the exhaustive grid and report success, which is the silent downgrade this \
                 refusal exists to prevent — upgrade the daemon, or drop the flag to search the \
                 grid on the server.",
                self.features
            ));
        }
        let request = Request::RunParamscanProfile {
            profile_toml: profile_toml.to_string(),
            rank_by: rank_by.map(str::to_string),
            // An EMPTY selector serializes as `None`: an ordinary grid search's frame is
            // byte-identical to one from before the field existed.
            search: search.filter(|s| !s.is_empty()).cloned(),
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::ParamscanReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SweepReport, got {}", resp_kind(&other)))
            }
        }
    }

    /// Ask the COMPUTE daemon to run one compiled study over the store IT holds, and return the
    /// minted run as JSON text ([`Response::StudyReport`]).
    ///
    /// ⚠ `crates/vike-cli/src/cmd/study.rs` checks the same advertisement first and prints the
    /// richer refusal (the `vike-backend study` escape hatch with this run's arguments filled in);
    /// this one exists for every OTHER caller, so the rule lives with the protocol.
    ///
    /// ⚠ [`crate::FEATURE_STUDY`] is MOUNT-conditional on the serving side: the compute daemon
    /// advertises it only when a study runner was injected into it. See that constant.
    pub fn run_study(&mut self, study: &WireStudy) -> Result<String, String> {
        if !self.features.iter().any(|f| f == FEATURE_STUDY) {
            return Err(format!(
                "backtest daemon does not advertise `{FEATURE_STUDY}` (advertised: {:?}) — nothing \
                 was sent. A study is served by `vike-backend backtest --addr` with the study \
                 runner MOUNTED; this verb is capability-negotiated, not version-gated. Run it on \
                 the box that holds the store instead: `vike-backend study`.",
                self.features
            ));
        }
        write_frame(&mut self.stream, &Request::RunStudy(Box::new(study.clone())))
            .map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::StudyReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected StudyReport, got {}", resp_kind(&other)))
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
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::WalkforwardReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected WalkforwardReport, got {}",
                resp_kind(&other)
            )),
        }
    }

    /// Enumerate the compiled native backtest-strategy roster server-side
    /// (`vike_backtest::harness::STRATEGIES` over RPC) — the names a profile's `strategy.name` can
    /// resolve. A transport failure or a server-side [`Response::Error`] surfaces through the ONE
    /// `Err(String)` channel; any other reply (a protocol desync) is likewise `Err`.
    pub fn list_strategies(&mut self) -> Result<Vec<String>, String> {
        write_frame(&mut self.stream, &Request::ListStrategies).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Strategies(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Strategies, got {}", resp_kind(&other)))
            }
        }
    }

    /// **Enumerate the roster a NAMED RUN would resolve, and whether the lane is armed** —
    /// [`Request::NamedStrategies`].
    ///
    /// ⚠ **Not [`Self::list_strategies`], and the two answer different rosters.** That one returns
    /// `vike_backtest::harness::STRATEGIES`, the simulator roster; this one returns what a named run
    /// can actually resolve — the portable registry plus the operator's own compiled-in user
    /// strategies, which `ListStrategies` never enumerates. A picker built on the wrong one offers
    /// names that will be refused and hides names that would run.
    ///
    /// ⚠ **`armed: false` is a SUCCESS, and it arrives with an EMPTY roster.** The arming gates the
    /// NAMES as well as the run (`docs/decisions/0064`'s decision 8 leg 3: publishing the
    /// operator's compiled-in strategy names is itself the disclosure arming makes an operator's
    /// act). Render [`NamedRoster::unarmed_note`], never the bare empty list: the note separates
    /// "withheld" from "this daemon holds none", which is also why
    /// [`crate::proto::FEATURE_NAMED_RUN`] is a BUILD fact rather than a per-lane one.
    ///
    /// ONE local refusal, before a frame: the capability check.
    pub fn named_strategies(&mut self) -> Result<NamedRoster, String> {
        self.refuse_unadvertised_named_run()?;
        write_frame(&mut self.stream, &Request::NamedStrategies).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::NamedStrategies(roster) => Ok(roster),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected NamedStrategies, got {}", resp_kind(&other)))
            }
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
    ///    which this verb can do and [`Self::seed_series`] cannot, because
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
        write_frame(&mut self.stream, &Request::RunNamed(Box::new(spec.clone())))
            .map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::NamedRun(outcome) => Ok(*outcome),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected NamedRun, got {}", resp_kind(&other))),
        }
    }

    /// Whether this server advertised the named-run capability — the question a picker asks BEFORE
    /// it offers a Run control, so an old server produces a sentence rather than a button that
    /// always fails (the [`Self::serves_venue_catalog`] leg: an un-advertised capability must not
    /// render as an EMPTY roster).
    pub fn serves_named_run(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_NAMED_RUN)
    }

    /// The ONE capability refusal both named-run verbs share.
    fn refuse_unadvertised_named_run(&self) -> Result<(), String> {
        if self.serves_named_run() {
            return Ok(());
        }
        Err(format!(
            "this compute daemon does not advertise `{FEATURE_NAMED_RUN}` (advertised: {:?}) — \
             nothing was sent. Unlike the data daemon's armed lanes, this string is a BUILD fact, \
             so its absence means the server PREDATES the named-run verb rather than that its \
             operator declined to arm the lane: an unarmed server advertises it and answers \
             `NotArmed`. Check the address too — the `Run*` verbs are served by \
             `vike-backend backtest --addr` (default 127.0.0.1:7880), not by the datahub.",
            self.features
        ))
    }
}
