//! Files no crate owns whose READERS must still run on their change: lane, suite and api-docs job.

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
///   * `.github/workflows/jforex-bridge.yml` and `.github/workflows/release.yml` —
///     `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` holds the JDK pin equal across the gate
///     workflow, the release that builds the jar people download, and the provisioners.
///   * `.github/workflows/release.yml` — `crates/vike-strategy-builder/tests/build_errors.rs` holds
///     the source-version stamp the release packaging step writes equal to the one the crate reads.
///     (vike-docs reads it too, through [`super::gate_crates::DOCS_DATA_GATE_INPUTS`].)
///   * `justfile` — `crates/bridges/fxcm/tests/fcsdk_packaging.rs` `include_str!`s it to hold the
///     `fxcm-package` recipe the runbook calls.
///   * `scripts/fetch_release_tools.sh` — `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` reads its
///     `jre` row, so the installed JDK and the pinned one cannot disagree.
///   * `scripts/qa_shots.sh` — `crates/vike-app-core/tests/qa_shot_account.rs` reads the fixture
///     store the contact sheet seeds.
///   * `docs/ops/fxcm-forexconnect.md` — `crates/bridges/fxcm/tests/fxcm_login_triage_gate.rs` reads it.
///     (Moved there from vike-ops by decision 0108: a gate that moves out of `crates/vike-ops` is scanned by `ci_plan_gate`'s
///     forward rule, which skips vike-ops itself, so every file it names needs a row.)
///   * `docs/ops/graceful-stop.md` and `docs/ops/tradehub-windows.md` and `scripts/vike_tradehub_windows.ps1` —
///     `crates/vike-tradehub/tests/windows_host_gate.rs`.
///   * `docs/reference/history-channels.md` — `crates/vike-datahub/tests/history_channels_gate.rs` reads it.
///   * `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` —
///     `crates/vike-secrets/tests/settings_store_ddl_gate.rs` reads it.
///   * `scripts/publish_starter_data.sh` — `crates/vike-backtest/tests/starter_dataset_gate.rs`.
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
///     and `crates/vike-bridge-core/tests/halt_default_path.rs` (the halt sentinel's grant);
///     `deploy/vike-datahub.service` by `crates/vike-datahub/src/recorder_tests.rs` — routed to a
///     FEATURE SUITE by [`SUITE_INPUT_READERS`] instead, because that test compiles only under a
///     non-default feature;
///     `deploy/jre/provision-jre.sh` by `crates/bridges/dukascopy/tests/jdk_pin_gate.rs`.
///   * `docs/ops/`: `crates/vike-tradehub/tests/daemon/docs_profiles_parse.rs` LISTS the directory
///     for its `tradehub-*-live.toml` profiles and `crates/vike-tradehub/tests/profile_risk_rows.rs`
///     round-trips `run-profile-live.toml` — so every `.toml` there; `crates/vike-config/tests/profile_risk.rs`
///     and `ceilings_are_distinct.rs` read `run-profile-live.toml` and `kill-switches.md`;
///     `crates/bridges/fxcm/tests/fcsdk_packaging.rs` reads `tradehub-the CI box.md`.
///   * `docs/**/*.md`: `crates/bridges/aster/tests/testnet_claim_gate.rs` walks all of it for a
///     banned testnet claim.
///   * `skills/`: `crates/vike-cli/src/cmd/mcp/tests.rs` reads every `SKILL.md` and `data_tests.rs`
///     one; `crates/vike-agent-eval/tests/scripted_pipeline.rs` lists the skills for its cases.
///
/// A `(prefix, suffix)` row narrower than the read it covers is a CLAIM, made deliberately where
/// the reading code filters (`.service`, `.toml`, `.md`); the gate cannot see a filter, so it only
/// asks that a directory-listing reader has some row inside the directory it lists.
pub const LANE_INPUT_READERS: &[(&str, &str, &[&str])] = &[
    (".github/workflows/jforex-bridge.yml", "", &["vike-dukascopy"]),
    (".github/workflows/release.yml", "", &["vike-dukascopy", "vike-strategy-builder"]),
    ("deploy/", ".service", &["vike-config"]),
    ("deploy/jre/provision-jre.sh", "", &["vike-dukascopy"]),
    ("deploy/vike-tradehub.service", "", &["vike-bridge-core", "vike-cli", "vike-tradehub"]),
    ("docs/", ".md", &["vike-aster"]),
    ("docs/ops/", ".toml", &["vike-tradehub"]),
    ("docs/ops/fxcm-forexconnect.md", "", &["vike-fxcm"]),
    ("docs/ops/graceful-stop.md", "", &["vike-tradehub"]),
    ("docs/ops/kill-switches.md", "", &["vike-config"]),
    ("docs/ops/run-profile-live.toml", "", &["vike-config"]),
    ("docs/ops/tradehub-the CI box.md", "", &["vike-fxcm"]),
    ("docs/ops/tradehub-windows.md", "", &["vike-tradehub"]),
    ("docs/reference/history-channels.md", "", &["vike-datahub"]),
    ("docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md", "", &["vike-secrets"]),
    ("justfile", "", &["vike-fxcm"]),
    ("scripts/fetch_release_tools.sh", "", &["vike-dukascopy"]),
    ("scripts/publish_starter_data.sh", "", &["vike-backtest"]),
    ("scripts/qa_shots.sh", "", &["vike-app-core"]),
    ("scripts/vike_tradehub_windows.ps1", "", &["vike-tradehub"]),
    ("skills/", "", &["vike-agent-eval", "vike-cli"]),
];

/// Files owned by no crate whose READER compiles only under a non-default feature — so the test
/// lane, which builds every crate with its default features, never runs it, and putting its crate
/// in the lane would be a row that buys nothing. A changed path matching `(prefix, suffix)` FIRES
/// the named [`super::feature_suites::FEATURE_SUITES`] leg instead, the one build that compiles the reader.
///
/// `xtask/tests/ci_plan_gate.rs` holds this table with [`LANE_INPUT_READERS`]: a reader
/// whose file sits under a `cfg(feature = …)` the crate's default does not turn on — a gated `mod`
/// on its way up to its target, an inner `#![cfg]`, or a target's `required-features` — is REFUSED
/// as a lane row, and is accepted here only when the named suite's arm in
/// `scripts/ci_feature_suite.sh` runs `cargo test -p <that crate>` with features that turn the
/// gate on.
///
/// The one row, and why it ROUTES rather than being declared a residual:
///   * `deploy/vike-datahub.service` — `crates/vike-datahub/src/recorder_tests.rs`'s
///     `the_whole_stop_fits_inside_the_units_stop_timeout` reads the unit's `TimeoutStopSec=` and
///     holds the recorder's whole teardown inside it, so a unit edit that lowers it would let
///     SIGKILL cut the final flush. That file is a test module of `recorder`, which compiles only
///     under `record`, and `record` is not a default; `recorder-venues` runs
///     `cargo test -p vike-datahub --features record-polymarket,record-binance`, and both imply
///     `record`. Before this row, a change to that unit ran nothing that reads it (it selected only
///     vike-ops), so routing makes it strictly better, never worse. MEASURED cost: 28 of the 300
///     first-parent commits before this landed touched the unit, and 20 of them already fired
///     `recorder-venues` through a crate they changed; the other 8 now add that leg (63 s median
///     work, plus its job's setup).
pub const SUITE_INPUT_READERS: &[(&str, &str, &str)] =
    &[("deploy/vike-datahub.service", "", "recorder-venues")];

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
pub const DOCS_JOB_INPUTS: &[&str] =
    &["scripts/build_api_docs.sh", "scripts/publish_mirror.sh", "scripts/forbidden_tokens.ere"];
