//! Files no crate owns whose READERS must still run on their change: the lane and the LightGBM job.

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
///   * `scripts/package_strategy_source.sh` — `crates/vike-strategy-builder/tests/build_errors.rs` holds
///     the SDK marker the release packaging step writes to the name the crate reads, and
///     `crates/vike-rust-strategy/tests/sdk_deploy.rs` runs it (`#[ignore]`d, lane-only) and verifies its archive.
///   * `deploy/docker/vikectl` — `crates/vike-rust-strategy/tests/sdk_deploy.rs` runs its SDK marker readers on the
///     core's own marker text, and `crates/vike-strategy/tests/sdk_pin_reader.rs` its pin reader against the header
///     parser (decision 0296).
///     (vike-docs reads `.github/workflows/release.yml`, through [`super::gate_crates::DOCS_DATA_GATE_INPUTS`].)
///   * `justfile` — `crates/bridges/fxcm/tests/fcsdk_packaging.rs` `include_str!`s it to hold the
///     `fxcm-package` recipe the runbook calls.
///   * `scripts/fetch_release_tools.sh` — `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` reads its
///     `jre` row, so the installed JDK and the pinned one cannot disagree.
///   * `scripts/qa_shots.sh` — `crates/vike-app-core/tests/qa_shot_account.rs` reads the fixture
///     store the contact sheet seeds.
///   * `docs/ops/*.md` — `crates/vike-cli/src/surface/examples_tests.rs` parses every `vike-cli` line on every page
///     (its other pages are [`is_command_page`]'s).
///   * `docs/ops/config-unset.md` — `crates/vike-cli/tests/shipped_cli/argv_accepted_ops.rs` reads the lines it tells an operator to type.
///   * `docs/ops/fxcm-forexconnect.md` — `crates/bridges/fxcm/tests/fxcm_login_triage_gate.rs` reads it.
///     (Moved there from vike-ops by decision 0108: a gate that moves out of `crates/vike-ops` is scanned by `ci_plan_gate`'s
///     forward rule, which skips vike-ops itself, so every file it names needs a row.)
///   * `docs/ops/graceful-stop.md` and `docs/ops/tradehub-windows.md` and `scripts/vike_tradehub_windows.ps1` —
///     `crates/vike-tradehub/tests/source_gates/windows_host_gate.rs`.
///   * `docs/reference/history-channels.md` — `crates/vike-datahub/tests/history_channels_gate.rs` reads it.
///   * `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` —
///     `crates/vike-secrets/tests/settings_store_ddl_gate.rs` reads it.
///   * `scripts/publish_starter_data.sh` — `crates/vike-datahub/tests/starter_dataset_gate.rs`.
///   * `content/book/` — `crates/vike-strategy-sandbox/tests/book_listings.rs` builds and runs every listing of the
///     book and compares its result with the page committed for it (a drift gate), so a listing, a recipe or a
///     generated page changed alone still runs it: `content/` owns no crate, so no closure reaches the reader.
///
/// ⚠ **The rows under `docs/`, `deploy/` and `skills/` close a hole that has nothing to do with the
/// global escalation**, and it is the same hole: those trees never escalated, they select only
/// [`super::gate_crates::DOC_GATE_CRATES`] (`docs/`, `deploy/`, any `.md`), so a test OUTSIDE vike-ops that reads one of
/// their files did not run on that file's own change — it ran, and failed, on the next unrelated
/// PR that happened to select its crate. MEASURED when these rows landed, with the reader scan the
/// gate runs: fifteen test files in nine crates, against the six an earlier grep had listed —
///   * `deploy/` units: `crates/vike-config/tests/removed.rs` reads EVERY `deploy/*.service` (no
///     unit may set a retired variable); `deploy/vike-tradehub.service` is read by
///     `crates/vike-cli/tests/cli_verbs/bootstrap_daemon_cli.rs`, `crates/vike-tradehub/src/tradehub_cli/tests/stop_and_deadlines.rs`
///     and `crates/vike-tradehub-core/tests/halt_default_path.rs` (the halt sentinel's grant);
///     `deploy/vike-datahub.service` by `crates/vike-datahub/src/recorder_tests.rs`'s
///     `the_whole_stop_fits_inside_the_units_stop_timeout`, which holds the recorder's whole teardown inside the unit's
///     `TimeoutStopSec=`, so a unit edit that lowers it cannot let SIGKILL cut the final flush. That file sits behind the
///     `record` feature, which the crate's self dev-dependency turns on in every test build. (`DATAHUB_ALL_JOB_INPUTS`
///     routed it to the `datahub-all` job until CI began testing each hub once, 2026-11-06: the table is gone.)
///     `deploy/jre/provision-jre.sh` by `crates/bridges/dukascopy/tests/jdk_pin_gate.rs`; `deploy/row-steps` by
///     `crates/vike-cli/src/surface/row_steps_tests.rs`, which parses every step through the `clap` tree.
///   * `docs/ops/`: `crates/vike-config/tests/ceilings_are_distinct.rs` reads `tradehub-the CI box.md` and
///     `kill-switches.md`; `crates/bridges/fxcm/tests/fcsdk_packaging.rs` reads `tradehub-the CI box.md`.
///   * `docs/**/*.md`: `crates/bridges/aster/tests/testnet_claim_gate.rs` walks all of it for a
///     banned testnet claim.
///   * `skills/`: `crates/vike-mcp/src/tests/manifest_and_skills.rs` reads every `SKILL.md` and `data_tests.rs`
///     one; `crates/vike-agent-evaluation/tests/scripted_pipeline.rs` lists the skills for its cases.
///
/// A `(prefix, suffix)` row narrower than the read it covers is a CLAIM, made deliberately where
/// the reading code filters (`.service`, `.toml`, `.md`); the gate cannot see a filter, so it only
/// asks that a directory-listing reader has some row inside the directory it lists.
pub const LANE_INPUT_READERS: &[(&str, &str, &[&str])] = &[
    (".github/workflows/ci.yml", "", &["vike-core"]),
    (".github/workflows/jforex-bridge.yml", "", &["vike-dukascopy"]),
    (".github/workflows/release-image-assets.yml", "", &["vike-dukascopy"]),
    (".github/workflows/release.yml", "", &["vike-dukascopy"]),
    ("content/book/", "", &["vike-strategy-sandbox"]),
    ("deploy/", ".service", &["vike-config"]),
    ("deploy/docker/vikectl", "", &["vike-rust-strategy", "vike-strategy"]),
    ("deploy/jre/provision-jre.sh", "", &["vike-dukascopy"]),
    ("deploy/row-steps", "", &["vike-cli"]),
    ("deploy/vike-datahub.service", "", &["vike-datahub"]),
    ("deploy/vike-tradehub.service", "", &["vike-cli", "vike-tradehub-core", "vike-tradehub"]),
    ("docs/", ".md", &["vike-aster"]),
    ("docs/ops/", ".md", &["vike-cli"]),
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
    ("scripts/package_strategy_source.sh", "", &["vike-rust-strategy", "vike-strategy-builder"]),
    ("scripts/publish_starter_data.sh", "", &["vike-datahub"]),
    ("scripts/qa_shots.sh", "", &["vike-app-core"]),
    ("scripts/vike_tradehub_windows.ps1", "", &["vike-tradehub"]),
    ("skills/", "", &["vike-agent-evaluation", "vike-cli", "vike-mcp"]),
];

/// The crate that parses the command lines of the pages [`is_command_page`] answers for:
/// `crates/vike-cli/src/surface/examples_tests.rs`. Lane only, like every [`LANE_INPUT_READERS`] row.
pub const COMMAND_PAGES_READER: &str = "vike-cli";

/// A crate's `CLAUDE.md`, or a page directly under `docs/`. A [`LANE_INPUT_READERS`] row cannot say
/// either: a `docs/` prefix matches every depth, so each decision record would buy this crate's tests,
/// and `crates/` is no root that table's gate scans.
pub fn is_command_page(file: &str) -> bool {
    (file.starts_with("crates/") && file.ends_with("/CLAUDE.md"))
        || file
            .strip_prefix("docs/")
            .is_some_and(|page| page.ends_with(".md") && !page.contains('/'))
}

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
/// The model crates name no `vike-*` crate but each other (both in the trigger set); the three suites `scripts/ci_lightgbm_suite.sh` runs
/// (`lightgbm_cli_smoke`, `train_infer_equality`, `gbdt_learner_smoke`) reach their dev-dependencies
/// only for `vike_paths::scratch::ScratchDir`, the std-only self-deleting temp directory they create
/// (it was `vike-model`'s until the paths left that crate). So an edit outside the model crates can change
/// what those suites do through that one file and no other, and the job used to fire on every
/// `vike-model` edit anyway (14 of the last 30 pull requests; 3 once it reads the changed set).
/// `xtask/tests/ci_plan_gate.rs` holds the claim over the tree: the suites name
/// `vike_paths::scratch::` and, of `vike-model`, only `vike_model::test_support::corpus::` (the seeded
/// `lcg` a double-ML suite draws with, book W16: its own row), so a second reach is a red test that
/// sends the author here.
///
/// The other inputs need no row: a model-crate file is a direct edit, and `crates/vike-ml-training/src/pins.rs`
/// holds `PINNED_LIGHTGBM_TAG` (what `scripts/lightgbm_cache.sh` reads and the binary's `PROVENANCE`
/// is checked against) and is inside the crate. The workflow is NOT a row: a row would fire on every
/// edit of `ci.yml`, which is what decision 0138 removed; the job's own block is read by its TEXT
/// instead ([`LIGHTGBM_JOB_WORKFLOW`]).
pub const LIGHTGBM_JOB_INPUTS: &[&str] = &[
    "crates/vike-paths/src/scratch.rs",
    // `gbdt_learner_smoke`'s double-ML nuisance test (book W16) draws its data with `vike_model`'s `lcg`.
    "crates/vike-model/src/test_support/corpus.rs",
    "scripts/ci_lightgbm_suite.sh",
    "scripts/lightgbm_cache.sh",
    "scripts/build_lightgbm.sh",
];

/// The TREES whose files fire the `lightgbm` job too, by prefix: the book (`content/book/`), whose
/// listings `crates/vike-strategy-sandbox/tests/book_listings.rs` runs in that job with the pinned
/// trainer (decision 0138's amendment of 2026-11-16). A listing, its recipe, the market sample or a
/// generated page changes no crate, so no row of [`LIGHTGBM_JOB_INPUTS`] (exact files) could say it.
pub const LIGHTGBM_JOB_INPUT_TREES: &[&str] = &["content/book/"];

/// The workflow holding the `lightgbm` part, and the key of its job (half A's side job, since 2026-11-22;
/// the part was a `lightgbm:` job before). The part's OWN steps are its other input (decision 0138): an
/// edit of this workflow fires the part when its view of the job's block (its own steps and the rest of
/// the block, every other part's steps cut: `super::super::global_parts::output_moved`) differs between
/// the base and HEAD, fail safe (an unreadable workflow or a block missing on either side fires), as the
/// `latency:` block is read. An edit of another job or part, a workflow-level key or the comment above the
/// key does not.
pub const LIGHTGBM_JOB_WORKFLOW: &str = ".github/workflows/ci.yml";

/// See [`LIGHTGBM_JOB_WORKFLOW`].
pub const LIGHTGBM_JOB_KEY: &str = "side-a";

/// `.rs` files whose TEXT some build or test reads as data, so an edit of their comments alone can still
/// change what a crate other than the file's owner sees: the planner never drops a comment-only edit of one
/// of these from its reverse-dependency walk and jobs (`super::super::plan::compute`, decision 0139). The
/// latency gate still drops it: text a reader sees is data, never a measured instruction. Three
/// shapes put a file here:
///   * an `include_str!`/`include_bytes!` of it from a file of ANOTHER crate;
///   * an `include_str!`/`include_bytes!` of it from a file of its own crate that is not test-only (the
///     text is then a value the crate's dependents can read);
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
    "crates/bridges/binance/src/family/feed/mod.rs",
    "crates/bridges/binance/src/family/feed/tick/trades.rs",
    "crates/bridges/ctrader/src/adapter.rs",
    "crates/bridges/deribit/src/feed/dvol.rs",
    "crates/bridges/hyperliquid/src/instrument/price.rs",
    "crates/bridges/hyperliquid/src/instrument/properties.rs",
    "crates/bridges/okx/src/execution/rest/mod.rs",
    "crates/bridges/polymarket/src/execution/heartbeat.rs",
    "crates/bridges/polymarket/src/settlement/auto_redeem.rs",
    "crates/bridges/polymarket/src/settlement/resolve.rs",
    "crates/bridges/vike-ibkr/src/instrument/any_symbol/socket.rs",
    "crates/bridges/vike-ibkr/src/feed/socket/mod.rs",
    "crates/bridges/vike-ibkr/src/instrument/properties.rs",
    // The `site` of each own loop and native session in `crates/vike-venues/src/venue_connection.rs`, held to
    // the tree by `crates/vike-venues/src/venue_connection_tests.rs`'s `every_driver_site_is_in_the_tree`.
    "crates/bridges/dukascopy/src/execution/mod.rs",
    "crates/bridges/dukascopy/src/feed.rs",
    "crates/bridges/fxcm/src/execution/mod.rs",
    "crates/bridges/fxcm/src/feed.rs",
    "crates/bridges/ig/src/execution/user_data.rs",
    "crates/bridges/vike-ibkr/src/transport/socket.rs",
    // The tests that prove the link harness's cells on their own seam, each found by name at run time
    // (`crates/vike-bridge-core/tests/link_conformance/sessions.rs`'s `ROWS`, and a covered row's `Cell::Proven`
    // in `crates/vike-bridge-core/tests/link_conformance/venues.rs` and
    // `crates/vike-bridge-core/tests/link_conformance/market.rs`).
    "crates/bridges/alpaca/src/feed/connection_tests.rs",
    "crates/bridges/alpaca/src/feed/feed_tests.rs",
    "crates/bridges/aster/src/execution/user_data/perp.rs",
    "crates/bridges/binance/src/execution/user_data/perp.rs",
    "crates/bridges/binance/src/family/execution/user_data.rs",
    "crates/bridges/binance/tests/listenkey_stream_pin.rs",
    "crates/bridges/ig/tests/offline/quote_feed.rs",
    "crates/bridges/oanda/src/feed/feed_tests.rs",
    "crates/bridges/ctrader/src/transport/connection/connection_tests.rs",
    "crates/bridges/ctrader/tests/offline/connection_reconnect.rs",
    "crates/bridges/ctrader/tests/offline/connection_retry.rs",
    "crates/bridges/ctrader/tests/offline/execution_mapper.rs",
    "crates/bridges/ctrader/tests/offline/session_lapse.rs",
    "crates/bridges/dukascopy/src/reconcile/query.rs",
    "crates/bridges/dukascopy/tests/sidecar/dukascopy_exec.rs",
    "crates/bridges/dukascopy/tests/sidecar/dukascopy_restart.rs",
    "crates/bridges/fxcm/src/account/venue_link_tests.rs",
    "crates/bridges/ig/src/execution/user_data/retry_tests.rs",
    "crates/bridges/ig/src/execution/user_data_tests.rs",
    "crates/bridges/vike-ibkr/src/execution/event_mapper_tests.rs",
    "crates/bridges/vike-ibkr/src/transport/link_watch_tests.rs",
    // `crates/vike-bridge-core/tests/bridge_conformance/confirm.rs`'s `CONFIRM_ROWS`: each venue's confirm form is
    // proven by named tests the row finds in these files (decision 0171's table, unit S12a).
    "crates/bridges/aster/tests/offline/confirm_by_order_client_id.rs",
    "crates/bridges/binance/tests/offline/confirm_by_order_client_id.rs",
    "crates/bridges/bybit/tests/offline/confirm_by_order_client_id.rs",
    "crates/bridges/fxcm/src/execution/execution_tests.rs",
    "crates/bridges/hyperliquid/src/execution/confirm_tests.rs",
    "crates/bridges/ig/src/execution/execution_tests.rs",
    "crates/bridges/oanda/src/execution/execution_tests.rs",
    "crates/bridges/okx/tests/offline/confirm_by_order_client_id.rs",
    "crates/bridges/polymarket/src/execution/execution_tests.rs",
    "crates/bridges/vike-ibkr/src/transport/socket_tests.rs",
    "crates/bridges/vike-ibkr/tests/ibkr_lifecycle.rs",
    "crates/vike-gatewayhub/tests/trade/fxcm_reconnect.rs",
    "crates/vike-gatewayhub/tests/trade/ibkr_reconnect.rs",
    "crates/vike-analytics/src/record.rs",
    "crates/vike-app-core/src/backend/connection.rs",
    "crates/vike-app-core/src/backend/split_plane.rs",
    "crates/vike-app-core/src/ui/window/workspace/persist_tests/snapshot.rs",
    "crates/vike-backfill/src/bin/eod_backfill.rs",
    "crates/vike-backfill/src/bin/exec_trade_backfill.rs",
    "crates/vike-backfill/src/bin/tardis_backfill.rs",
    "crates/vike-backfill/src/bin/vikedata_backfill.rs",
    "crates/vike-backfill/src/cli.rs",
    "crates/vike-backfill/src/databento/ingest.rs",
    "crates/vike-backfill/src/events_api.rs",
    "crates/vike-backfill/src/funding_rate.rs",
    "crates/vike-backfill/src/klines.rs",
    "crates/vike-backfill/src/market_rules.rs",
    // The option-trades lane's two declared keys (`option_trade:…`, `option_trade_chain:…`), read the same way.
    "crates/vike-backfill/src/option_trades.rs",
    "crates/vike-backfill/src/pmxt/ingest.rs",
    // `crates/vike-ops/tests/venues/store_kind_gate/shape.rs` reads the three declared `sec-…` commit keys out
    // of their producer.
    "crates/vike-backfill/src/sec_edgar/store.rs",
    "crates/vike-backfill/src/tardis/ingest.rs",
    "crates/vike-backfill/src/text_scores.rs",
    "crates/vike-backfill/src/user_bars.rs",
    "crates/vike-backfill/src/venues/binance_archive.rs",
    "crates/vike-backfill/src/venues/binance_archive/klines.rs",
    // Binance's option-market archive lanes (`binance-eohsummary:…`, `binance-bvolindex:…`), read the same way.
    "crates/vike-backfill/src/venues/binance_archive/options/summary.rs",
    "crates/vike-backfill/src/venues/binance_archive/options/volatility_index.rs",
    "crates/vike-backfill/src/venues/dukascopy.rs",
    "crates/vike-backfill/src/venues/dukascopy/archive.rs",
    "crates/vike-backfill/src/vike_archive.rs",
    "crates/vike-backfill/src/vikedata/panel.rs",
    "crates/vike-backtesthub/src/backtest_cli/args.rs",
    "crates/vike-backtesthub/src/backtest_cli/run_record.rs",
    "crates/vike-backtesthub/src/backtest_cli/serve.rs",
    "crates/vike-backtesthub/src/bin/cheap_np_askgate/main.rs",
    "crates/vike-backtesthub/src/bin/cheap_np_depth.rs",
    "crates/vike-backtesthub/src/study_cli.rs",
    // `vike-backtest`'s profile schema names it as the evidence file of the Studio strategy refusals
    // (`crates/vike-backtest/src/run/profile_surface/refusal_sites.rs`).
    "crates/vike-backtesthub/src/studio_run/strategy.rs",
    "crates/vike-backtest/src/search/optimize.rs",
    "crates/vike-backtest/src/search/paramscan.rs",
    // `config.optional_writes_min_free_mib`'s consumption row (`crates/vike-config/src/consumed/table.rs`) finds
    // its needle in the one free-space floor (REVIEW-LIVE-MONEY H1).
    "crates/vike-boot/src/disk_floor.rs",
    "crates/vike-catalog/examples/history_reference.rs",
    "crates/vike-catalog/src/history/render.rs",
    "crates/vike-catalog/src/provider.rs",
    "crates/vike-catalog/src/session.rs",
    "crates/vike-catalog/src/tab.rs",
    "crates/vike-cli/src/boot.rs",
    "crates/vike-cli/src/cmd/args.rs",
    "crates/vike-cli/src/cmd/config/check.rs",
    "crates/vike-cli/src/cmd/secrets.rs",
    "crates/vike-cli/src/cmd/secrets/account.rs",
    "crates/vike-cli/src/cmd/secrets/delete.rs",
    "crates/vike-cli/src/cmd/secrets/set.rs",
    "crates/vike-client/src/hubkeys.rs",
    "crates/vike-client/src/order_gate.rs",
    "crates/vike-client/src/resolved.rs",
    "crates/vike-config/tests/policy_is_consumed.rs",
    "crates/vike-core/src/bin/replay_journal.rs",
    // `vike_config::PRE_TRADE_CEILINGS`'s `max_mount_leverage` row finds its enforcement needle here.
    "crates/vike-core/src/runtime/strategy_drive/broker_drain.rs",
    "crates/vike-data-manager/src/inventory/filter.rs",
    "crates/vike-data-manager/src/inventory/grid.rs",
    "crates/vike-data-manager/src/inventory/list.rs",
    "crates/vike-data-manager/src/inventory/mod.rs",
    "crates/vike-data-manager/src/inventory/partial_days.rs",
    "crates/vike-data-manager/src/inventory/table.rs",
    "crates/vike-data-manager/src/polymarket_proxy.rs",
    // `crates/vike-backtest/src/run/profile_surface.rs`'s `profile_tables` names it as the file of
    // `[data] file_format` (`CsvBarsFormatWords`), whose field docs `profile.json` publishes.
    "crates/vike-data/src/csv_bars/format_words.rs",
    "crates/vike-data/src/delivery.rs",
    "crates/vike-data/src/demo.rs",
    "crates/vike-data/src/source.rs",
    "crates/vike-data/src/store/hist.rs",
    "crates/vike-datahub/src/datahub_cli.rs",
    "crates/vike-datahub/src/datahub_cli/boot.rs",
    "crates/vike-datahub/src/datahub_cli/lanes.rs",
    "crates/vike-datahub/src/datahub_cli/recording.rs",
    "crates/vike-datahub/src/execution_stream/sink.rs",
    "crates/vike-datahub/src/recording.rs",
    // `crates/vike-ops/tests/venues/store_kind_gate/shape.rs` reads PM-2's two declared `token-facts:` commit keys
    // out of their producer.
    "crates/vike-datahub/src/recording/token_facts.rs",
    // The same gate reads the market-rules writer's declared `market-rules-recorded:` key out of its producer.
    "crates/vike-datahub/src/recording/token_rules.rs",
    // The same gate reads the text recorder's declared `text:` commit key out of its producer.
    "crates/vike-datahub/src/tool_feeds/record.rs",
    "crates/vike-desktop/src/app_methods.rs",
    "crates/vike-desktop/src/main.rs",
    "crates/vike-docs/src/bin/docs_data.rs",
    "crates/vike-execution/src/risk.rs",
    // `vike-backtest`'s profile schema names it as the evidence file of the cost params' refusals
    // (`crates/vike-backtest/src/run/profile_surface/refusal_sites.rs`).
    "crates/vike-fills/src/costs.rs",
    "crates/vike-gatewayhub-client/src/trade/recon.rs",
    "crates/vike-gatewayhub/src/gatewayhub_cli.rs",
    "crates/vike-gatewayhub/src/gatewayhub_cli/vars.rs",
    // `vike_config::CONSUMPTION`'s four gatewayhub flag rows find their needles in `adapter_flags`, the one
    // line per field that copies `vike_config::Flags` into `AdapterFlags` (decision 0095's B5 amendment).
    "crates/vike-gatewayhub/src/trade/adapters/contract.rs",
    "crates/vike-gatewayhub/src/trade/adapters/refreshed.rs",
    "crates/vike-gatewayhub/src/trade/lane.rs",
    // `crates/vike-mcp/src/tests/credential_fence.rs`'s `the_cli_keyring_cannot_present_the_admin_scope`
    // reads the one home of the admin-key constructor (`HubKeys::with_admin`).
    "crates/vike-hub-protocol/src/auth.rs",
    // `crates/vike-rust-strategy/tests/sdk_adapter.rs`'s `reorder_bar` (decision 0296's step 6): a synthetic older
    // SDK copies this file with `Bar`'s fields reordered, so its text is the test's input.
    "crates/vike-marketdata/src/bar.rs",
    // `crates/vike-indicators/tests/parity/variance_fold_gate.rs` classifies the squared-deviation folds of the
    // `vike-math` formulas the indicator cores call (decision 0294), reading their text.
    "crates/vike-math/src/powers.rs",
    "crates/vike-math/src/regression.rs",
    "crates/vike-math/src/rolling.rs",
    "crates/vike-math/src/stationarity_tests.rs",
    "crates/vike-math/src/time_series.rs",
    "crates/vike-mcp/src/config.rs",
    "crates/vike-mcp/src/lib.rs",
    // `crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`'s `CREDENTIAL_SOURCE`: the wire-tag gate reads
    // `CredentialPresence`'s variant names out of their one home in vike-model, below every hub client (B1.26).
    "crates/vike-model/src/accounts/credential_presence.rs",
    "crates/vike-model/src/money/fees.rs",
    "crates/vike-model/src/orders/cancel.rs",
    // `crates/vike-datahub-client/tests/execution_stream_wire.rs`'s `OWNERSHIP_SOURCE`: the wire-enum gate reads
    // `ExecutionOwnership`'s variants where the model declares them, since the wire carries the model's enum (B2.08).
    "crates/vike-model/src/orders/execution_report.rs",
    // `crates/vike-config/tests/ceilings_are_distinct.rs`'s `DENIAL_CATALOG_FILE`: the ceilings roster reads each
    // `OrderDenial` variant's word out of the refusal catalog (refusal step 2).
    "crates/vike-model/src/orders/refusals.rs",
    "crates/vike-model/src/risk/grid.rs",
    // `crates/vike-backtest/src/run/profile_surface.rs`'s `profile_tables` and `profile_rosters` name
    // them as the files of `[risk]` and of `data.kind`'s roster, whose docs `profile.json` publishes.
    "crates/vike-model/src/risk/profile.rs",
    "crates/vike-model/src/run_request/replay.rs",
    "crates/vike-model/src/stream/label.rs",
    // Its own `WALL_CLOCK_READERS` test reads the file it sits in (B1.23).
    "crates/vike-model/src/time/clock.rs",
    "crates/vike-model/src/venues/declaration.rs",
    "crates/vike-model/src/venues/mod.rs",
    // The build cache keys a strategy file's build by the source the builder reads (BUILD-CACHE's `SOURCE_HASH`).
    "crates/vike-strategy-builder/src/render.rs",
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
    // `crates/vike-backtest/src/run/profile_surface.rs`'s `profile_tables` names them as the files of
    // `[engine.in_bar]` and `[engine.margin_call]`, whose field docs `profile.json` publishes.
    "crates/vike-simulator/src/engine/sim_broker/in_bar.rs",
    "crates/vike-simulator/src/engine/sim_broker/knobs.rs",
    "crates/vike-store/src/live_rec/writer.rs",
    // `vike-strategy-builder`'s `tests/build_jail.rs` `include_str!`s it (the normal-strategy build
    // proof), as `vike-rust-strategy`'s own `tests/equivalence.rs` already does within its crate.
    "crates/vike-rust-strategy/tests/fixtures/equivalence/ma_cross.rs",
    "crates/vike-telegram/src/telegram_cli.rs",
    "crates/vike-tradehub-client/src/tradehub_handle/exchange.rs",
    "crates/vike-tradehub-client/src/wire.rs",
    "crates/vike-tradehub/src/hot_reload.rs",
    "crates/vike-tradehub/src/reconcile_config.rs",
    "crates/vike-tradehub/src/server/control.rs",
    "crates/vike-tradehub/src/tradehub_cli.rs",
    "crates/vike-tradehub/src/tradehub_cli/args.rs",
    "crates/vike-tradehub/src/tradehub_cli/banner.rs",
    "crates/vike-tradehub/src/tradehub_cli/live_runtime.rs",
    "crates/vike-tradehub/src/tradehub_cli/settings.rs",
    "crates/vike-tradehub/src/venue_arming/deadman.rs",
    "crates/vike-tradehub/src/venue_arming/mount_budget.rs",
    "crates/vike-backend/src/main.rs",
];
