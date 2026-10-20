//! Files no crate owns whose READERS must still run on their change: lane, datahub-all, LightGBM and api-docs job.

/// The crate a change to ANY exempted path force-adds to the test lane — the owner of the gates
/// that read these files (mechanism 1 on [`super::escalation::GLOBAL_EXEMPT_FILES`]).
///
/// ⚠ **A SET since decision 0108's follow-up (2026-10-08): `vike-ops` and `xtask`.** Ten CI-derivation gates
/// moved into `xtask/tests/` and read the same non-crate inputs (manifests, `.github/`, `justfile`, `scripts/`,
/// docs, every `.rs` tree for `ci_plan_gate`'s reader scan), which no reverse-dependency closure reaches either;
/// a single crate here would have stopped them running on the very changes they exist for. The set is deliberately
/// the same for every input class — selecting the whole of `xtask` runs the same gates the PR ran before they
/// moved, so the move adds no test-seconds; narrowing it per gate is the per-gate trigger work, not this table.
pub const EXEMPT_INPUT_GATE_CRATES: &[&str] = &["vike-ops", "xtask"];

/// The crates OUTSIDE [`EXEMPT_INPUT_GATE_CRATES`] whose tests read a file that owns no crate,
/// keyed `(prefix, suffix)` the way [`super::escalation::GLOBAL_PREFIX_EXEMPT`] is (an exact path is a row whose
/// suffix is empty, and a directory row ends in `/`): a changed path matching a row
/// force-adds its crates to the test LANE ([`super::super::selection::lane_crates_for`]) — never to `affected`, so no
/// feature suite fires to buy a test run.
///
/// ⚠ **This table is DERIVED, then written down, and the gate holds the two equal.**
/// `xtask/tests/ci_plan_gate.rs` scans every crate's sources for the repo paths they
/// name and fails BOTH ways: a crate that reads one of these files without a row (the next
/// `include_str!` would otherwise lose its trigger in silence), and a row whose crate no longer
/// reads it. A row is the SET of crates for one input, so a second reader is one more name.
///
/// The rows, each with the read that put it there:
///   * `.github/workflows/ci.yml` — `crates/vike-core/tests/common/latency_line.rs` reads the latency
///     step's `series_append` awk guard and its marker (`the_required_key_set_still_matches_the_workflow`).
///     The row came with decision 0142: until then every `ci.yml` edit escalated, which ran vike-core's
///     tests anyway; a narrowed edit (one job's block) would not have, so a change moving what the test
///     reads still selects it. A GLOBAL file can carry a row: an escalating edit plans the roster, and a
///     row only matters to the edits the planner narrows ([`super::escalation::NARROWED_GLOBAL_FILES`]).
///   * `.github/workflows/jforex-bridge.yml` and `.github/workflows/release.yml` —
///     `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` holds the JDK pin equal across the gate
///     workflow, the release that builds the jar people download, and the provisioners.
///   * `.github/workflows/release-image-assets.yml` — the same gate holds that the on-request workflow
///     that ships the JRE (decision 0178) obtains it through the pinned provisioner.
///   * `.github/workflows/release.yml` — `crates/vike-strategy-builder/tests/build_errors.rs` holds
///     the source-version stamp the release packaging step writes equal to the one the crate reads.
///     (vike-docs reads it too, through [`super::gate_crates::DOCS_DATA_GATE_INPUTS`].)
///   * `justfile` — `crates/bridges/fxcm/tests/fcsdk_packaging.rs` `include_str!`s it to hold the
///     `fxcm-package` recipe the runbook calls.
///   * `scripts/fetch_release_tools.sh` — `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` reads its
///     `jre` row, so the installed JDK and the pinned one cannot disagree.
///   * `scripts/qa_shots.sh` — `crates/vike-app-core/tests/qa_shot_account.rs` reads the fixture
///     store the contact sheet seeds.
///   * `docs/ops/config-unset.md` — `crates/vike-cli/tests/argv_accepted_ops.rs` reads the lines it tells an operator to type.
///   * `docs/ops/fxcm-forexconnect.md` — `crates/bridges/fxcm/tests/fxcm_login_triage_gate.rs` reads it.
///     (Moved there from vike-ops by decision 0108: a gate that moves out of `crates/vike-ops` is scanned by `ci_plan_gate`'s
///     forward rule, which skips vike-ops itself, so every file it names needs a row.)
///   * `docs/ops/graceful-stop.md` and `docs/ops/tradehub-windows.md` and `scripts/vike_tradehub_windows.ps1` —
///     `crates/vike-tradehub/tests/windows_host_gate.rs`.
///   * `docs/reference/history-channels.md` — `crates/vike-datahub/tests/history_channels_gate.rs` reads it.
///   * `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` —
///     `crates/vike-secrets/tests/settings_store_ddl_gate.rs` reads it.
///   * `scripts/publish_starter_data.sh` — `crates/vike-backtesthub/tests/starter_dataset_gate.rs`.
///
/// ⚠ **The rows under `docs/`, `deploy/` and `skills/` close a hole that has nothing to do with the
/// global escalation**, and it is the same hole: those trees never escalated, they select only
/// [`super::gate_crates::DOC_GATE_CRATES`] (`docs/`, `deploy/`, any `.md`), so a test OUTSIDE vike-ops that reads one of
/// their files did not run on that file's own change — it ran, and failed, on the next unrelated
/// PR that happened to select its crate. MEASURED when these rows landed, with the reader scan the
/// gate runs: fifteen test files in nine crates, against the six an earlier grep had listed —
///   * `deploy/` units: `crates/vike-config/tests/removed.rs` reads EVERY `deploy/*.service` (no
///     unit may set a retired variable); `deploy/vike-tradehub.service` is read by
///     `crates/vike-cli/tests/bootstrap_daemon_cli.rs`, `crates/vike-tradehub/src/tradehub_cli/tests/stop_and_deadlines.rs`
///     and `crates/vike-tradehub-core/tests/halt_default_path.rs` (the halt sentinel's grant);
///     `deploy/vike-datahub.service` by `crates/vike-datahub/src/recorder_tests.rs` — routed to the
///     `datahub-all` JOB by [`DATAHUB_ALL_JOB_INPUTS`] instead, because that test compiles only under
///     a non-default feature;
///     `deploy/jre/provision-jre.sh` by `crates/bridges/dukascopy/tests/jdk_pin_gate.rs`.
///   * `docs/ops/`: `crates/vike-config/tests/ceilings_are_distinct.rs` reads `tradehub-the CI box.md` and
///     `kill-switches.md`; `crates/bridges/fxcm/tests/fcsdk_packaging.rs` reads `tradehub-the CI box.md`.
///   * `docs/**/*.md`: `crates/bridges/aster/tests/testnet_claim_gate.rs` walks all of it for a
///     banned testnet claim.
///   * `skills/`: `crates/vike-cli/src/cmd/mcp/tests.rs` reads every `SKILL.md` and `data_tests.rs`
///     one; `crates/vike-agent-eval/tests/scripted_pipeline.rs` lists the skills for its cases.
///
/// A `(prefix, suffix)` row narrower than the read it covers is a CLAIM, made deliberately where
/// the reading code filters (`.service`, `.toml`, `.md`); the gate cannot see a filter, so it only
/// asks that a directory-listing reader has some row inside the directory it lists.
pub const LANE_INPUT_READERS: &[(&str, &str, &[&str])] = &[
    (".github/workflows/ci.yml", "", &["vike-core"]),
    (".github/workflows/jforex-bridge.yml", "", &["vike-dukascopy"]),
    (".github/workflows/release-image-assets.yml", "", &["vike-dukascopy"]),
    (".github/workflows/release.yml", "", &["vike-dukascopy", "vike-strategy-builder"]),
    ("deploy/", ".service", &["vike-config"]),
    ("deploy/jre/provision-jre.sh", "", &["vike-dukascopy"]),
    ("deploy/vike-tradehub.service", "", &["vike-cli", "vike-tradehub-core", "vike-tradehub"]),
    ("docs/", ".md", &["vike-aster"]),
    ("docs/ops/config-unset.md", "", &["vike-cli"]),
    ("docs/ops/fxcm-forexconnect.md", "", &["vike-fxcm"]),
    ("docs/ops/graceful-stop.md", "", &["vike-tradehub"]),
    ("docs/ops/kill-switches.md", "", &["vike-config"]),
    ("docs/ops/run-profile.md", "", &["vike-config"]),
    ("docs/ops/tradehub-the CI box.md", "", &["vike-fxcm"]),
    ("docs/ops/tradehub-windows.md", "", &["vike-tradehub"]),
    ("docs/reference/history-channels.md", "", &["vike-datahub"]),
    ("docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md", "", &["vike-secrets"]),
    ("justfile", "", &["vike-fxcm"]),
    ("scripts/fetch_release_tools.sh", "", &["vike-dukascopy"]),
    ("scripts/publish_starter_data.sh", "", &["vike-backtesthub"]),
    ("scripts/qa_shots.sh", "", &["vike-app-core"]),
    ("scripts/vike_tradehub_windows.ps1", "", &["vike-tradehub"]),
    ("skills/", "", &["vike-agent-eval", "vike-cli"]),
];

/// Files owned by no crate whose READER compiles only under a non-default feature of vike-datahub —
/// so the test lane, which builds every crate with its default features, never runs it, and putting
/// the crate in the lane would be a row that buys nothing. A changed file named here FIRES the
/// `datahub-all` job instead ([`super::super::selection::datahub_affected`]), the one build that
/// compiles the reader. Exact paths, like [`LIGHTGBM_JOB_INPUTS`].
///
/// `xtask/tests/ci_plan_gate.rs` holds this table with [`LANE_INPUT_READERS`]: a reader whose file
/// sits under a `cfg(feature = …)` the crate's default does not turn on — a gated `mod` on its way up
/// to its target, an inner `#![cfg]`, or a target's `required-features` — is REFUSED as a lane row,
/// and is accepted here only when the reader is vike-datahub's and `ci.yml`'s `datahub-all` job runs
/// `cargo test -p vike-datahub` with features that turn the gate on.
///
/// The one row, and why it ROUTES rather than being declared a residual:
///   * `deploy/vike-datahub.service` — `crates/vike-datahub/src/recorder_tests.rs`'s
///     `the_whole_stop_fits_inside_the_units_stop_timeout` reads the unit's `TimeoutStopSec=` and
///     holds the recorder's whole teardown inside it, so a unit edit that lowers it would let
///     SIGKILL cut the final flush. That file is a test module of `recorder`, which compiles only
///     under `record`, and `record` is not a default; the job's `record-polymarket` and
///     `record-binance` both imply it. Before the row (then a route to the deleted `recorder-venues`
///     suite), a change to that unit ran nothing that reads it (it selected only vike-ops). MEASURED
///     cost when the route landed: 28 of the 300 first-parent commits before it touched the unit, and
///     20 of them already fired the datahub's build through a crate they changed.
pub const DATAHUB_ALL_JOB_INPUTS: &[&str] = &["deploy/vike-datahub.service"];

/// The files OUTSIDE [`super::roster::LIGHTGBM_CRATES`] that the `lightgbm` job's suites compile and
/// run, or that the job itself runs, so an edit of one fires the job although its owner is not the
/// trigger crate ([`super::super::selection::lightgbm_affected`] reads this beside the changed set).
///
/// The three scripts are the job's own steps: `scripts/ci_lightgbm_suite.sh` (the driver),
/// `scripts/lightgbm_cache.sh` (the pinned binary's host cache) and `scripts/build_lightgbm.sh` (the
/// trainer build it falls back to). They still ESCALATE (`xtask/tests/ci_plan_gate/workflow_scripts.rs`'s
/// `FILES_THAT_STAY_GLOBAL`), but since decision 0138 an escalation no longer fires this job, so
/// each is named here; until then they fired it only as global files.
///
/// ⚠ **The `vike-paths` row is the whole of the evidence for narrowing the job to a direct edit.**
/// `vike-ml` has no `vike-*` normal dependency; the three suites `scripts/ci_lightgbm_suite.sh` runs
/// (`lightgbm_cli_smoke`, `train_infer_equality`, `gbdt_learner_smoke`) reach its dev-dependencies
/// only for `vike_paths::scratch::ScratchDir`, the std-only self-deleting temp directory they create
/// (it was `vike-model`'s until the paths left that crate). So an edit outside `vike-ml` can change
/// what those suites do through that one file and no other, and the job used to fire on every
/// `vike-model` edit anyway (14 of the last 30 pull requests; 3 once it reads the changed set).
/// `xtask/tests/ci_plan_gate.rs` holds the claim over the tree: the suites name
/// `vike_paths::scratch::`, nothing else of `vike-paths` and nothing of `vike-model`, so a second
/// reach is a red test that sends the author here.
///
/// The other inputs need no row: a vike-ml file is a direct edit, and `crates/vike-ml/src/pins.rs`
/// holds `PINNED_LIGHTGBM_TAG` (what `scripts/lightgbm_cache.sh` reads and the binary's `PROVENANCE`
/// is checked against) and is inside the crate. The workflow is NOT a row: a row would fire on every
/// edit of `ci.yml`, which is what decision 0138 removed; the job's own block is read by its TEXT
/// instead ([`LIGHTGBM_JOB_WORKFLOW`]).
pub const LIGHTGBM_JOB_INPUTS: &[&str] = &[
    "crates/vike-paths/src/scratch.rs",
    "scripts/ci_lightgbm_suite.sh",
    "scripts/lightgbm_cache.sh",
    "scripts/build_lightgbm.sh",
];

/// The workflow holding the `lightgbm` job, and that job's key. The job's OWN block is its other
/// input (decision 0138): an edit of this workflow fires the job when the text of the `  lightgbm:`
/// block differs between the base and HEAD (`super::super::selection::job_moved`, fail safe: an
/// unreadable workflow or a block missing on either side fires), as the `latency:` and `api-docs:`
/// blocks are read. An edit of another job, a workflow-level key or the comment above the key does not.
pub const LIGHTGBM_JOB_WORKFLOW: &str = ".github/workflows/ci.yml";

/// See [`LIGHTGBM_JOB_WORKFLOW`].
pub const LIGHTGBM_JOB_KEY: &str = "lightgbm";

/// Files that are inputs to the `api-docs` JOB itself rather than to any crate.
///
/// The job's `docs` trigger is otherwise computed from the crates that own a changed file
/// ([`super::super::roster::docs_affected`]), and a shell script owns no crate — so without this table a change to
/// the script that BUILDS the API reference would not run the job that builds it. `docs` also fires
/// whenever the global set is non-empty, so the script's coverage was an accident of the escalation
/// [`super::escalation::GLOBAL_EXEMPT_FILES`] removed for it.
///
/// ⚠ The full 62-crate matrix never ran this script at all. Only the `api-docs` job does
/// (`--selftest`, then a real build), so this table is not a smaller version of the escalation —
/// it is the only thing that was ever actually testing the change.
///
/// ⚠ The second and third entries are what that build EXECUTES, not what it is: it calls
/// `scripts/publish_mirror.sh --dry-run` for the redacted tree it documents, and that script's
/// forbidden-token scan reads `scripts/forbidden_tokens.ere` — the same file the job then scans the
/// built HTML against. Both rode the `scripts/` escalation until it was narrowed, and a change to
/// either can turn the job red with no crate in the diff.
///
/// ⚠ Since decision 0137 this table is ALSO the one door through which a PULL REQUEST still plans the job
/// (`super::super::plan::compute_with`): the job is post-merge, and an edit of one of these is the
/// change only that job executes.
pub const DOCS_JOB_INPUTS: &[&str] =
    &["scripts/build_api_docs.sh", "scripts/publish_mirror.sh", "scripts/forbidden_tokens.ere"];

/// The workflow holding the `api-docs` job, and that job's key. The job's OWN block is its other
/// input (decision 0137): a pull request whose edit of this workflow changes the text of the
/// `  api-docs:` block plans the job (`super::super::selection::job_moved`, fail safe), the way an
/// edit of the `latency:` block fires the latency gate. An edit of another job does not.
pub const DOCS_JOB_WORKFLOW: &str = ".github/workflows/ci.yml";

/// See [`DOCS_JOB_WORKFLOW`].
pub const DOCS_JOB_KEY: &str = "api-docs";

/// Files a DOCUMENTED crate's compiled source includes into its rendered documentation
/// (`#![doc = include_str!("…")]`, `#[doc = include_str!("…")]`, or any include-family macro of a `.md`),
/// as repository paths. A change to one fires the `api-docs` job by name: the job's content rule
/// (decision 0133, [`super::super::docs_content`]) reads a documented crate's own linkable files, and a
/// file outside every crate owns none.
///
/// ⚠ **EMPTY, and that is a measurement, not an omission** (2026-10-11: no compiled source of any crate
/// includes a file into its docs). A crate-owned `.md` cannot appear here at all while
/// `xtask/tests/ci_plan_gate/test_target_source.rs`'s `no_compiled_source_includes_a_crate_markdown_file`
/// refuses such an include; a file outside every crate (`docs/`, the root `README.md`) can, and
/// `xtask/tests/ci_plan_gate/docs_content_edits.rs`'s `the_docs_included_files_are_exactly_what_the_tree_includes`
/// holds this table equal to the tree both ways.
pub const DOCS_INCLUDED_FILES: &[&str] = &[];

/// `.rs` files whose TEXT some build or test reads as data, so an edit of their comments alone can still
/// change what a crate other than the file's owner sees: the planner never drops a comment-only edit of one
/// of these from its reverse-dependency walk and jobs (`super::super::plan::compute`, decision 0139). The
/// latency gate still drops it: text a reader sees is data, never a measured instruction. Three
/// shapes put a file here:
///   * an `include_str!`/`include_bytes!` of it from a file of ANOTHER crate;
///   * an `include_str!`/`include_bytes!` of it from a file of its own crate that is not test-only (the
///     text is then a value the crate's dependents can read: `vike_model::risk::surface::RISK_PROFILE_SRC`
///     is the shape, published by `vike-backtest`'s schema exporter);
///   * a repository path (`"crates/…/x.rs"`) or a crate-relative one (`"../other/src/x.rs"`) naming it, as
///     a string literal in a crate other than its owner (outside `vike-ops` and `xtask`, whose gates are
///     selected by `super::gate_triggers::GATE_TRIGGERS` rows on every `.rs` edit).
///
/// A test of the file's OWN crate that reads it needs no row: a comment-only edit still runs its owner's
/// tests (the owner joins the lane). `xtask/tests/ci_plan_gate/comment_only_sources.rs`'s
/// `the_sources_read_as_text_are_exactly_what_the_tree_reads` holds this table equal to the tree both ways
/// (a path built at run time with `join` is invisible to it: the declared residual).
pub const SOURCES_READ_AS_TEXT: &[&str] = &[
    "crates/bridges/ctrader/src/bin/ctrader_authorize.rs",
    "crates/bridges/hyperliquid/src/bin/hyperliquid_builder_fee_approve.rs",
    "crates/bridges/binance/src/family/market_feed.rs",
    "crates/bridges/binance/src/family/trades.rs",
    "crates/bridges/ctrader/src/adapter.rs",
    "crates/bridges/deribit/src/dvol.rs",
    "crates/bridges/hyperliquid/src/base/px.rs",
    "crates/bridges/hyperliquid/src/exec/outcome_settlement.rs",
    "crates/bridges/hyperliquid/src/feeds/instruments.rs",
    "crates/bridges/okx/src/perp.rs",
    "crates/bridges/polymarket/src/exec_plane/heartbeat.rs",
    "crates/bridges/polymarket/src/exec_plane/settlement/auto_redeem.rs",
    "crates/bridges/polymarket/src/exec_plane/settlement/resolve.rs",
    "crates/bridges/vike-ibkr/src/any_symbol_socket.rs",
    "crates/bridges/vike-ibkr/src/market_feed/mod.rs",
    "crates/bridges/vike-ibkr/src/properties.rs",
    "crates/vike-analytics/src/result.rs",
    "crates/vike-app-core/src/backend/backend_conn.rs",
    "crates/vike-app-core/src/backend/split_plane.rs",
    "crates/vike-app-core/src/ui/workspace/persist_tests/snapshot.rs",
    "crates/vike-backfill/src/bin/eod_backfill.rs",
    "crates/vike-backfill/src/bin/exec_trade_backfill.rs",
    "crates/vike-backfill/src/bin/vikedata_backfill.rs",
    "crates/vike-backfill/src/cli.rs",
    "crates/vike-backfill/src/databento/ingest.rs",
    "crates/vike-backfill/src/events_api.rs",
    "crates/vike-backfill/src/funding_rate.rs",
    "crates/vike-backfill/src/klines.rs",
    "crates/vike-backfill/src/pmxt/ingest.rs",
    "crates/vike-backfill/src/tardis/ingest.rs",
    "crates/vike-backfill/src/venues/dukascopy.rs",
    "crates/vike-backfill/src/venues/dukascopy/archive.rs",
    "crates/vike-backfill/src/vike_archive.rs",
    "crates/vike-backfill/src/vikedata/panel.rs",
    "crates/vike-backtesthub/src/backtest_cli/args.rs",
    "crates/vike-backtesthub/src/backtest_cli/run_record.rs",
    "crates/vike-backtesthub/src/backtest_cli/serve.rs",
    "crates/vike-backtesthub/src/bin/cheap_np_askgate/main.rs",
    "crates/vike-backtesthub/src/bin/cheap_np_depth.rs",
    "crates/vike-backtesthub/src/bin/cheap_np_run.rs",
    "crates/vike-backtesthub/src/bin/sport_taker_run.rs",
    "crates/vike-backtest/src/harness/mod.rs",
    "crates/vike-backtest/src/harness/optimize.rs",
    "crates/vike-backtest/src/harness/profile.rs",
    "crates/vike-backtest/src/harness/profile/data_cfg.rs",
    "crates/vike-backtest/src/harness/profile/engine_cfg.rs",
    "crates/vike-backtest/src/harness/profile/fee_cfg.rs",
    "crates/vike-backtest/src/harness/profile/impact_cfg.rs",
    "crates/vike-backtest/src/harness/profile/resolution_cfg.rs",
    "crates/vike-backtest/src/harness/profile/sizer_cfg.rs",
    "crates/vike-backtest/src/harness/profile/validate.rs",
    "crates/vike-backtest/src/harness/profile/walkforward_cfg.rs",
    "crates/vike-backtest/src/harness/registry.rs",
    "crates/vike-backtest/src/harness/sweep.rs",
    "crates/vike-backtest/src/hist_replay.rs",
    "crates/vike-backtest/src/search/euler.rs",
    "crates/vike-backtest/src/walkforward/runner.rs",
    "crates/vike-backtest/src/walkforward/windows.rs",
    "crates/vike-catalog/examples/history_reference.rs",
    "crates/vike-catalog/src/history/render.rs",
    "crates/vike-catalog/src/session.rs",
    "crates/vike-cli/src/boot.rs",
    "crates/vike-cli/src/cmd/args.rs",
    "crates/vike-cli/src/cmd/config/check.rs",
    "crates/vike-cli/src/cmd/config/retired_env.rs",
    "crates/vike-cli/src/cmd/mcp.rs",
    "crates/vike-cli/src/cmd/verbs.rs",
    "crates/vike-cli/src/lib.rs",
    "crates/vike-config/tests/policy_is_consumed.rs",
    "crates/vike-core/src/bin/replay_journal.rs",
    "crates/vike-core/src/bin/vike_stat.rs",
    "crates/vike-data-manager/src/view.rs",
    "crates/vike-data/src/demo.rs",
    "crates/vike-data/src/store/hist.rs",
    "crates/vike-datahub/src/datahub_cli.rs",
    "crates/vike-datahub/src/datahub_cli/boot.rs",
    "crates/vike-datahub/src/datahub_cli/lanes.rs",
    "crates/vike-datahub/src/datahub_cli/recording.rs",
    "crates/vike-datahub/src/feeds/venues.rs",
    "crates/vike-datahub/src/import/formats.rs",
    "crates/vike-datahub/src/materialize.rs",
    "crates/vike-datahub/src/md/venues.rs",
    "crates/vike-datahub/src/recording.rs",
    "crates/vike-desktop/src/app_methods.rs",
    "crates/vike-desktop/src/main.rs",
    "crates/vike-docs/src/bin/docs_data.rs",
    "crates/vike-exec/src/risk.rs",
    "crates/vike-gatewayhub/src/gatewayhub_cli.rs",
    "crates/vike-gatewayhub/src/gatewayhub_cli/vars.rs",
    "crates/vike-gatewayhub/src/trade/adapters/refreshed.rs",
    "crates/vike-gatewayhub/src/trade/lane.rs",
    "crates/vike-model/src/risk/profile.rs",
    "crates/vike-model/src/venues/mod.rs",
    "crates/vike-tradehub-core/src/bin/incident.rs",
    "crates/vike-tradehub-core/src/budget.rs",
    "crates/vike-tradehub-core/src/engine.rs",
    "crates/vike-tradehub-core/src/engine/assemble.rs",
    "crates/vike-tradehub-core/src/lib.rs",
    "crates/vike-tradehub-core/src/policy.rs",
    "crates/vike-config/src/settings/rows/bridges.rs",
    "crates/vike-paths/src/state_path.rs",
    "crates/vike-paths/src/state_path/project_dirs.rs",
    "crates/vike-paths/src/state_path/project_root.rs",
    "crates/vike-recorder/src/assembled.rs",
    "crates/vike-secrets/src/store_locator.rs",
    "crates/vike-store/src/live_rec/writer.rs",
    "crates/vike-store/src/properties_rec.rs",
    "crates/vike-studio-core/src/study_cli.rs",
    "crates/vike-tradehub-client/src/auth.rs",
    "crates/vike-tradehub-client/src/remote_handle/exchange.rs",
    "crates/vike-tradehub-client/src/wire.rs",
    "crates/vike-tradehub/src/hot_reload.rs",
    "crates/vike-tradehub/src/reconcile_config.rs",
    "crates/vike-tradehub/src/server/control.rs",
    "crates/vike-tradehub/src/tradehub_cli.rs",
    "crates/vike-tradehub/src/tradehub_cli/args.rs",
    "crates/vike-tradehub/src/tradehub_cli/banner.rs",
    "crates/vike-tradehub/src/tradehub_cli/flags.rs",
    "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
    "crates/vike-tradehub/src/tradehub_cli/settings.rs",
    "crates/vike-tradehub/src/venue_arming/deadman.rs",
    "crates/vike/src/main.rs",
];
