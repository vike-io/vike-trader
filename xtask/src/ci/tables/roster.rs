//! Which workspace members the roster and each narrow job (datahub-all, tradehub-all, LightGBM, latency, app) build, and which
//! files of a member are not a compile input at all ([`UNLINKED_DOC_SUFFIXES`]).

/// Workspace members that are NOT in the default `test`+`clippy` lane. The roster is
/// `sorted(names - EXCLUDE_FROM_CI)`, so a NEW crate joins CI the moment it joins
/// `[workspace].members` and nothing has to be edited to let it in.
///
/// The exclusions, and why each is not in the default lane:
///
///   * `vike-desktop` — the egui/wgpu GUI binary: out of the shared nextest+clippy roster, because
///     it drags the whole eframe/wgpu/winit tree into the the CI box `test` job's SHARED cache, which no
///     other roster crate builds against. ⚠ No longer the deleted `fat` feature's resolution: the
///     crate has no features, so the cost is the tree, not a second resolution of it. NOT
///     uncovered — `ci.yml`'s `app-check` job checks it, lints it and runs its unit tests, gated
///     off the `app` output ([`APP_CHECK_CRATE`]).
///   * `vike-backfill` — the `ci.yml` lanes that NAME it are the `datahub-all` job (its
///     `-p vike-backfill` test line and `-p vike-store -p vike-backfill` lint) and the `backfill` suite, which build and test it, and
///     `windows-cross`, which checks its `#[cfg(windows)]` archive reader (it is one of
///     [`DATAHUB_ALL_CRATES`]). The roster's build compiles it again without naming it: vike-datahub's
///     `backfill-serve` CARGO FEATURE depends on it, and that crate's self dev-dependency turns the feature on.
///     docs/decisions/0094 took the last bridge crate off its tree (its rank dropped to 30,
///     `engine`), which changed what it may depend on, not which lane compiles it.
///
/// ⚠ `ibapi` (once an in-tree row here) is now a registry dependency, not a workspace member, so it
/// has no row to exclude; the roster's build compiles it (vike-ibkr's `ibkr` feature, on through the
/// gatewayhub's self dev-dependency).
///
/// A new crate that genuinely cannot build in the plain lane belongs here, with its reason.
///
/// ⚠ `xtask` is deliberately ABSENT: it is a workspace member and a CI crate like any other.
/// Excluding it is refused by
/// `xtask/tests/feature_lane_coverage.rs`'s `every_workspace_member_is_built_by_some_lane`
/// unless some lane names it, and would leave the tool that DERIVES the merge gate as the one crate
/// the merge gate never compiles.
pub const EXCLUDE_FROM_CI: &[&str] = &["vike-desktop", "vike-backfill"];

/// The suffixes of a file INSIDE a crate directory that is a DOCUMENT: read by people and by the prose
/// gates, compiled into nothing. An edit of one plans its owning crate (that crate's own tests may read
/// it at run time: `crates/bridges/aster/tests/testnet_claim_gate.rs` reads the crate's own
/// `CLAUDE.md`) and the gate crates every `.md` selects ([`super::gate_crates::DOC_GATE_INPUT_SUFFIXES`]),
/// but it is not LINKABLE: the planner seeds neither the reverse-dependency closure, the API reference,
/// the dependency-only suite triggers nor the latency gate from it
/// ([`super::super::graph::is_crate_document`], used by [`super::super::plan::compute`]).
///
/// MEASURED (the last 30 merged PRs, 2026-10-11): a one-line edit of `crates/vike-model/CLAUDE.md`
/// planned 60 crates and fired hist, latency, app-check, lightgbm, the API reference and both closure
/// suites, because `owner_of` reads any file under a crate directory as that crate's production
/// source. Six of the 30 PRs carried such an edit beside others; the rule changed the plan of those six
/// and of no other.
///
/// ⚠ The claim is "nothing compiles one of these files into another crate's build", and it is held by
/// `xtask/tests/ci_plan_gate/test_target_source.rs`'s `no_compiled_source_includes_a_crate_markdown_file`,
/// which fails on any `include_str!`/`include_bytes!`/`include!` (or `#[doc = include_str!(..)]`) of an
/// in-crate `.md` in compiled, non-test code. A crate that must compile a `.md` in either removes the suffix
/// here or reads the file at run time; it does not widen the sweep's allowance.
///
/// Only `.md`: a `.json`, `.tsv` or `.txt` beside the sources really is `include_str!`ed
/// (`crates/vike-ml/src/model/infer.rs`, `crates/vike-gatewayhub/src/trade/adapters/server_time.rs`), so those stay production.
/// A `.md` that sits in NO crate (`docs/`, the repository root, `content/`) owns no crate and is untouched
/// by this table: it selects the prose-gate crates and nothing else, as before.
pub const UNLINKED_DOC_SUFFIXES: &[&str] = &[".md"];

/// The crates whose test targets the `datahub-all` job builds — the `-p` crates of its lines that run or lint them in
/// `ci.yml` — and so the crates whose OWN edit fires it, whatever the file (a test-only edit too), through the DIRECT
/// set (the crates that own a changed file, or every crate when a global file changed);
/// [`super::super::selection::datahub_affected`] is the rule. `xtask/tests/ci_plan_gate/lane_directness.rs`'s
/// `the_data_job_names_exactly_its_crates` holds this table equal to those crates.
///
/// The job is what is left of the DATA side's feature build
/// (`docs/decisions/0132-the-feature-suites-fold-into-one-data-job-and-one-trading-job.md`). ⚠ Since 2026-11-06 CI tests
/// each hub once, in the configuration that ships: vike-datahub and the gatewayhub left this table, because the roster
/// `test` job runs and lints their tests with the shipped features on (their self dev-dependencies), and the job only
/// reads their trees ([`DATAHUB_ALL_DEPENDENCY_CRATES`] carries them). What stays, each its own invocation: the hist
/// store's lints (`-p vike-data`, and its backend `-p vike-store -p vike-backfill`), vike-backfill's test run (the roster
/// excludes it), and the lints of vike-backtest and vike-backtesthub (the deleted `backtest-hist-replay` suite).
///
/// ⚠ **A crate BELOW these does not fire the job, and that is the accepted risk** the `hist-datafusion`
/// job carried (owner-approved 2026-10-11 as an extension of decision 0104): a semantic change in
/// vike-model or the other normal dependencies that breaks a test only this job runs (vike-backfill's)
/// shows up on the `push` run after the merge. The hubs' feature-gated tests left that gap on 2026-11-06:
/// the `test` job runs them over the reverse-dependency closure of such a change. Reopen when such a change
/// merges green and turns the `push` run's `datahub-all` red. Do
/// NOT add a crate here to cover one of them — that is the closure again, one name at a time.
pub const DATAHUB_ALL_CRATES: &[&str] =
    &["vike-data", "vike-store", "vike-backfill", "vike-backtest", "vike-backtesthub"];

/// The crates whose LINKABLE edit fires the `datahub-all` job, a manifest's above all, though the job builds no test
/// target of theirs. They are matched against the LINKABLE set (`plan::compute`'s `compiled`), so a test-only edit of one
/// fires nothing. Two kinds:
///   * the three crates the job READS: vike-datahub (guards (A) to (D)), vike-gatewayhub (guard (E)) and
///     vike-gatewayhub-client (guard (F) and the check of its data half alone);
///   * every workspace crate the shipped datahub features and the gatewayhub's `data` feature turn on, because those trees
///     are what the guards read: an OPTIONAL dependency enabled by `dep:`: vike-datahub's own (vike-recorder, vike-options,
///     vike-bi5; vike-backfill and vike-store are such too, and named crates) and, since decision 0164,
///     vike-gatewayhub's `data` feature's (the seven feed bridges, dukascopy, fxcm and oanda).
///
/// `xtask/tests/ci_plan_gate/lane_directness.rs`'s `the_datahub_job_triggers_are_what_its_features_turn_on` derives
/// this table from BOTH manifests (the datahub's shipped features, and the gatewayhub's `--features data` as guard (E)
/// spells it), so it holds whichever side owns a row. `vike-recorder`, `vike-options` and `vike-bi5` are rows of the
/// datahub alone.
///
/// ⚠ **One crate the features turn on is deliberately NOT here**, and that test names it: `vike-alerting` (its alert
/// sinks). It never triggered the `recorder-venues` suite this job replaced. ⚠ The NORMAL dependencies of vike-datahub
/// (vike-model, vike-catalog, vike-datahub-client, vike-marketdata, …) are not here either, on decision 0123's rule:
/// their edits reach the hubs' tests through the `test` job's closure.
pub const DATAHUB_ALL_DEPENDENCY_CRATES: &[&str] = &[
    "vike-datahub",
    "vike-gatewayhub",
    "vike-gatewayhub-client",
    "vike-recorder",
    "vike-options",
    "vike-binance",
    "vike-bybit",
    "vike-okx",
    "vike-aster",
    "vike-hyperliquid",
    "vike-polymarket",
    "vike-deribit",
    "vike-dukascopy",
    "vike-bi5",
    "vike-fxcm",
    "vike-oanda",
    // The strategy lane's additions to the gatewayhub's `data` feature (wave 2, W10): the three
    // credentialed-data bridges. (`vike-execution`, the ingest channel a tick pump writes onto, is the one
    // crate the feature also turns on that is NOT a trigger: see `DATAHUB_ENABLED_BUT_NOT_TRIGGERS`.)
    "vike-alpaca",
    "vike-ctrader",
    "vike-ig",
    // The gatewayhub's `data` feature names it for `data_client::bus` (the dukascopy and fxcm streams), so it is in
    // the tree guard (E) reads; its pins are the gatewayhub's tests, which the `test` job runs.
    "vike-bridge-core",
];

/// The crates whose LINKABLE edit, or a linkable edit of anything BELOW them (the reverse-dependency
/// closure of what something can build: `plan::compute`'s `closure`), fires the `tradehub-all` job —
/// the triggers of the deleted `multicall` and `workspace-bins` suites, the two that kept the whole
/// walk (decision 0104), because the job's `cargo check --workspace --bins` compiles the whole stack, and its
/// guards read the trees from the top. ⚠ It is the job's ONE door since 2026-11-06: CI tests each hub once, in the
/// roster `test` job, so no line of this job builds a test target and a test-only edit fires nothing.
/// `xtask/tests/ci_plan_gate/lane_directness.rs`'s `the_trading_job_reads_only_what_its_door_can_see` holds its crates.
///
/// The set is vike-backend and every crate it dispatches into through an OPTIONAL dependency (no reverse
/// edge in the default graph, so each is named; `xtask/tests/ci_plan_gate/multicall_trigger.rs` derives
/// it from vike-backend's manifest), which includes the six crates whose bins `workspace-bins` was
/// written for. ⚠ `vike-recorder` is here although the dispatcher names it nowhere: `full` reaches it
/// one edge further out, through vike-datahub's `record` feature, and the rule is "every crate this
/// build compiles", not "every crate the dispatcher names".
///
/// ⚠ **The walk is of what something can BUILD**: a crate whose only changed files are test-only
/// (`tests/`, `benches/`, `examples/`, a `#[cfg(test)]` module) seeds no closure, so a test-only edit
/// of, say, vike-cli fires nothing. A source file, `Cargo.toml`,
/// `build.rs` and a `test-support` module (its owner and its dev enablers) keep the whole walk.
/// `xtask/tests/ci_plan_gate/closure_test_only.rs` holds both halves.
pub const TRADEHUB_ALL_CLOSURE_CRATES: &[&str] = &[
    "vike-backend",
    // ⚠ `vike-studio-core` stood here while the dispatcher's `study` tool lived in it (an OPTIONAL dependency
    // of vike-backend under `study-cli`). That CLI is `vike-backtesthub`'s since 2026-11-10, and a change to
    // vike-studio-core reaches the trigger below through vike-backtesthub's plain `[dependencies]` edge.
    "vike-backtesthub",
    "vike-datahub",
    "vike-recorder",
    "vike-tradehub",
    "vike-cli",
    // Joined with decision 0256: the `telegram` tool (`crates/vike/src/main.rs`'s `telegram_main`) is an
    // OPTIONAL dependency of the dispatcher under `full`.
    "vike-telegram",
    // Joined with plan 2b (F4): the `strategy-sandbox` tool (`crates/vike/src/main.rs`'s
    // `strategy_sandbox_main`, the ONE crate that links the strategy file loader) is an OPTIONAL dependency
    // of the dispatcher under `full`.
    "vike-strategy-sandbox",
    // Joined with decision 0146: the tearsheet tool body (`crates/vike/src/tearsheet_cli.rs`) opens
    // its store through `vike_secrets::open_credential_store`, so `full` names vike-secrets as an
    // OPTIONAL dependency of the dispatcher.
    "vike-secrets",
    // Joined with decision 0155: the tearsheet tool body reads its flags through
    // `vike_utility::cli` (clap), so `full` names vike-utility as an OPTIONAL dependency of the dispatcher.
    "vike-utility",
    // Joined with decision 0183 (item 13): the tearsheet tool body refuses a journal of fills on a symbol
    // quoted in another fiat currency (`vike_catalog::foreign_fiat_quoted_list`), so `full` names
    // vike-catalog as an OPTIONAL dependency of the dispatcher.
    "vike-catalog",
    // Joined with decision 0164 (wave 1): the `gatewayhub` tool (`crates/vike/src/main.rs`'s
    // `gatewayhub_main`) is an OPTIONAL dependency of the dispatcher under `full`, so a change confined
    // to vike-gatewayhub reaches this job only by being named. `datahub-all` reads its `data` tree (guard (E)), this
    // job its trade tree (guard (H)).
    "vike-gatewayhub",
    // Joined with wave 2 (W16): the bridges' EXECUTION halves are the gatewayhub's now (`trade`), and
    // they reach it by OPTIONAL edges, which carry no reverse edge in the planner's default graph. They
    // reached this job through vike-tradehub's normal dependency edges until the venue half moved, so
    // without these rows an edit confined to, say, `crates/bridges/okx/src/execution/mod.rs` would schedule no
    // build that compiles it into the signing daemon. `vike-bridge-core` needs no row: it is below
    // every bridge, so the walk reaches one of them. `ci_plan_gate/multicall_trigger.rs`'s
    // `every_bridge_the_gatewayhub_links_triggers_the_trading_job` holds the list to the manifest.
    "vike-binance",
    "vike-bybit",
    "vike-okx",
    "vike-aster",
    "vike-hyperliquid",
    "vike-deribit",
    "vike-dukascopy",
    "vike-oanda",
    "vike-ig",
    "vike-ctrader",
    "vike-alpaca",
    "vike-ibkr",
    "vike-fxcm",
    "vike-polymarket",
];

/// The the CI box LightGBM job's trigger set: the ONE crate whose tests DRIVE the real trainer binary —
/// vike-ml owns all three suites the job runs (`lightgbm_cli_smoke`, `train_infer_equality` and
/// `gbdt_learner_smoke`; `scripts/ci_lightgbm_suite.sh` is the authority for that list). Same shape
/// as [`DATAHUB_ALL_CRATES`]: a narrow explicit set, but intersected with the crates that OWN a changed
/// file (never the global expansion, decision 0138), not the affected set:
/// [`super::super::selection::lightgbm_affected`] says why, and [`super::readers::LIGHTGBM_JOB_INPUTS`]
/// holds the job's own scripts and the one `vike-model` file the suites also compile.
///
/// ⚠ It was a PAIR until `vike-research` dissolved and its `ml_adapter_smoke` moved with
/// `GbdtLearner` into `crates/vike-ml/tests/gbdt_learner_smoke.rs`; the second name had to go (the
/// HARD-failure note below).
///
/// ⚠ Those suites are `#[ignore]`d AND self-skipping, so before that job existed CI compiled them
/// and executed NONE of them, silently. [`super::super::selection::lightgbm_affected`] is what makes them run; the job
/// itself (`scripts/ci_lightgbm_suite.sh`) is what makes a zero-test run RED.
///
/// A name here that is not a workspace member is a HARD failure in [`super::super::plan::compute`], because the
/// job would otherwise stop firing in silence.
pub const LIGHTGBM_CRATES: &[&str] = &["vike-ml"];

/// The root of what COMPILES INTO the binary the the latency box latency gate measures.
///
/// The gate builds exactly `cargo test -p vike-core --release --test runtime_latency`, whose library
/// half is vike-core's lib plus its NORMAL dependencies (`crates/vike-core/Cargo.toml`).
///
/// One row, and the rest DERIVED: [`super::super::selection::measured_crates`] adds every workspace
/// crate of its NORMAL closure, so a crate inserted anywhere below vike-core fires the gate with no
/// edit here (decision 0130, verdict 2). `vike-execution` and `vike-model` were rows until 2026-11-06 and
/// join through the closure. `vike-data` was a row too and is out: vike-core dropped the dependency
/// (decision 0157), the binary linked it in none of 49 measured CI builds and holds no `vike_data`
/// symbol, and on 2026-11-06 its whole normal closure lies inside vike-core's.
///
/// ⚠ vike-strategies-check / vike-strategy / vike-rhai (and vike-market-making and vike-indicators, under
/// them) are vike-core's DEV-dependencies and are deliberately ABSENT. `cargo test --no-run` compiles them, but they are not
/// in vike-core's LIBRARY dep graph, so they cannot change one instruction of the fold whose p99 is
/// being measured — and `runtime_latency.rs` imports only vike_core / vike_execution / vike_model. A compile
/// break in any of them is caught by the roster lane, which tests and clippy-gates them on every run.
pub const LATENCY_CRATES: &[&str] = &["vike-core"];

/// Every file that compiles into the `runtime_latency` TEST binary itself — a test-target source
/// (or a module under one) that is nonetheless part of the measurement, so an edit of it fires the
/// latency gate where any other test-only edit does not
/// ([`super::super::selection::latency_compiled`]).
///
/// The harness plus everything it pulls in: its `#[path]` modules (`common/latency_line.rs`,
/// `common/rt_gap.rs`), the `#[cfg(test)]` module those declare (`common/rt_gap/source_gate.rs`,
/// which `include_str!`s the harness back), and every `include*!` of any of them. Cargo
/// auto-discovers `tests/*.rs` as one binary each and `tests/<dir>/` as no binary, so a file under
/// `tests/` that no `mod` here names belongs to ANOTHER test binary and is not on this list.
///
/// ⚠ Not derived at plan time on purpose (a planner that parsed Rust to find its own table would be
/// one more thing to trust), and held instead:
/// `xtask/tests/ci_plan_gate/latency_test_edits.rs`'s
/// `the_harness_table_is_exactly_the_files_runtime_latency_compiles` scans the harness's module and
/// include closure on every run and fails in BOTH directions, so a harness that gains an included
/// file (or loses one) fails CI until this row is edited.
pub const LATENCY_HARNESS_FILES: &[&str] = &[
    "crates/vike-core/tests/runtime_latency.rs",
    "crates/vike-core/tests/common/latency_line.rs",
    "crates/vike-core/tests/common/rt_gap.rs",
    "crates/vike-core/tests/common/rt_gap/source_gate.rs",
];

/// Crates whose TESTS SPAWN another crate's shipped BINARY, and the crates those binaries are
/// built from — a DECLARATION, no longer a selection rule.
///
/// ⚠ **This row used to force-add the companions into the test LANE whenever the driver was
/// selected, and that force-add is GONE (2026-10-03).** It existed to get the binaries BUILT, and
/// the `test` job builds the whole roster on every run now. What the row still declares is the edge
/// cargo cannot see, and `xtask/tests/ci_plan_gate.rs` holds the property the build
/// depends on: every companion is a roster member, so the roster build contains its bin.
///
/// # The measurement
///
/// CI run 34020320299 (PR #1653, a change confined to `crates/vike-agent-evaluation/`) planned
/// `-p vike-agent-evaluation -p vike-cli -p vike-data -p vike-ops`, and the `test` job went red:
/// `crates/vike-agent-evaluation/tests/relative_work_dir.rs`'s `a_relative_work_dir_still_stands_a_node_up`
/// found `target/debug/vike-cli` but no `vike-tradehub` binary, and
/// `crates/vike-agent-evaluation/tests/scripted_pipeline.rs`'s
/// `every_case_passes_when_driven_by_its_own_scripted_plan` failed identically. Those two are the
/// whole CI-gateable half of that harness — the model half is nondeterministic and can never be a
/// merge gate.
///
/// ⚠ The suite had been green on every earlier run, and that was LUCK rather than coverage:
/// `.github/workflows/ci.yml`'s jobs check out with `clean: false` to keep `target/` warm on the
/// runner, so a `vike-tradehub` left behind by an unrelated run had been answering
/// `crates/vike-agent-evaluation/src/lib.rs`'s `locate_binary`. The PR that sees the defect is whichever
/// one lands on a box after that stale copy is evicted — the same "reddens the next unrelated
/// change" shape [`super::gate_crates::SETTINGS_GATE_CRATES`] exists against, wearing a build artifact instead of a
/// gate.
///
/// # Why no closure can find this edge
///
/// `crates/vike-agent-evaluation/Cargo.toml` declares NO `vike-*` dependency at all, normal or dev, and
/// says why: the harness reaches the system the way an operator does, by SPAWNING the shipped
/// binaries. A spawn is not a dependency — cargo models no edge for it and `cargo metadata` reports
/// none — so [`super::super::graph::affected_from`] has nothing to walk. It would still have
/// nothing to walk if the harness took the dev-dependency anyway: a lib edge builds a LIB, and what
/// a spawn needs is an uplifted `target/debug/<name>`.
///
/// # Why building a crate builds its binary (which is how the force-add once worked)
///
/// cargo builds a package's BIN targets whenever it builds that package's INTEGRATION tests — that
/// is what `CARGO_BIN_EXE_<name>` names, and the binary it names is the one uplifted to
/// `target/debug/<name>`, the second path `locate_binary` tries. Both companions qualify, and not
/// incidentally: their own integration tests spawn their own binary through exactly that variable
/// (`crates/vike-tradehub/tests/daemon/help_and_log_dir.rs`, `crates/vike-cli/tests/shipped_cli/exit_codes.rs`),
/// so a lane that runs them has already produced the artifact this table is about. The failing run
/// is the positive control as well as the negative one: `vike-cli` WAS in that `-p` list and the
/// panic reports it found at `target/debug/vike-cli`, while `vike-tradehub` was not, and was not
/// there.
///
/// # Why the force-add went: the roster build makes the binaries, on every plan
///
/// Since the `test` job builds the WHOLE roster (`super::plan::Plan::roster`, the `roster=` output) and
/// selects what RUNS with a nextest filter (`tests=`), every roster crate's integration tests — and
/// therefore both companions' bins — are built on every run, whatever the plan selected. MEASURED
/// on a the latency box lane (main `ec5e9d12b`), both bin sources touched, then `.github/workflows/ci.yml`'s
/// own shape — `cargo nextest run <the 68-crate roster> -E 'package(=vike-agent-evaluation)'`: nextest
/// reported "1 test and 725 binaries skipped", i.e. it BUILT all 735 test binaries and filtered at
/// run time; `vike-tradehub` and `vike-cli` were recompiled and re-uplifted (`target/debug/<name>`
/// mtimes past the touch); and the agent-eval suite's 74 tests passed against them.
///
/// The row's last reason to stay, "a consumer that builds only `crates=` would need the build half
/// back", fails, checked rather than assumed: every reader of the plan is
/// `.github/workflows/ci.yml`'s `test` job (all three cargo steps build `${ROSTER:?}`, pinned by
/// `ci_plan_gate.rs`'s `the_test_job_builds_the_roster_and_runs_the_filter`),
/// `scripts/verify_branch.sh` (nextest, doctests and clippy all build `$roster`), and
/// `.github/workflows/release.yml`, which builds `crates=` under `CI_FULL=1` — the WHOLE roster by
/// construction. So all the force-add still bought was the companions' own TESTS (~318
/// test-seconds) on a change confined to the harness, which cannot break them.
///
/// # Rejected
///
/// **Make the tests SKIP when the binary is absent.** A green that ran nothing is the failure this
/// repository gates against everywhere else, and the one skip precedent does not transfer:
/// `crates/vike-core/tests/runtime_latency.rs` skips loudly because its input is withheld from the
/// published mirror BY DESIGN — a condition no build can repair. Here the input is absent only
/// because CI failed to build it, which is a defect with an address. And these two tests are the
/// only merge gate over the MCP transport, the node spawn, the two-call gate and the graders, so a
/// skip would retire them on precisely the PRs that change them.
///
/// **Let the test shell out to `cargo build`.** `crates/vike-agent-evaluation/src/test_support.rs`
/// carries that argument on its own `binaries` fn, where it was paid for (the first shape: nextest
/// KILLS a test that outruns the per-test budget `.config/nextest.toml` sets). It is also
/// non-hermetic — a test whose verdict depends on a compile — and it fights the shared warm
/// `target/` these runners exist to reuse: two test binaries here, each deciding to build, against
/// cargo's package lock and every other job on the box.
///
/// ⚠ A RENAME rots this table, and the failure is LOUD rather than silent — the driver's tests
/// panic naming the binary they could not find — so it carries no `compute`-time refusal the way
/// [`LIGHTGBM_CRATES`] does. `xtask/tests/ci_plan_gate.rs` pins the names against the
/// real crate directories instead, and pins that no companion sits in [`EXCLUDE_FROM_CI`]: the
/// roster is `names - EXCLUDE_FROM_CI`, so an excluded companion is a binary no CI build makes.
pub const BINARY_DRIVER_COMPANIONS: &[(&str, &[&str])] =
    &[("vike-agent-evaluation", &["vike-cli", "vike-mcp", "vike-tradehub"])];

/// The GUI shell. Excluded from the test lane ([`EXCLUDE_FROM_CI`]), so its compile gate reads the
/// affected set directly.
pub const APP_CHECK_CRATE: &str = "vike-desktop";
