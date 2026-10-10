//! Which workspace members the roster and each narrow job (hist, LightGBM, latency, app) build.

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
///   * `vike-backfill` — the `ci.yml` lanes that NAME it are `hist` and the `backfill` suite, which
///     build and test it; `windows-cross`, which checks its `#[cfg(windows)]` archive reader; and
///     the `backfill-serve` FEATURE SUITE, which checks+lints the vike-datahub build for the reason
///     that suite's own note argues (this crate is one of ITS triggers beside the seven kline
///     bridges). `multicall` compiles it without naming it: its `full` turns on vike-datahub's
///     `backfill-serve` CARGO FEATURE, which depends on it. docs/decisions/0094 took the last
///     bridge crate off its tree (its rank dropped to 30, `engine`), which changed what it may
///     depend on, not which lane compiles it.
///
/// ⚠ `ibapi` (once an in-tree row here) is now a registry dependency, not a workspace member, so it
/// has no row to exclude; the `ibkr` feature suite compiles it.
///
/// A new crate that genuinely cannot build in the plain lane belongs here, with its reason.
///
/// ⚠ `xtask` is deliberately ABSENT: it is a workspace member and a CI crate like any other.
/// Excluding it is refused by
/// `xtask/tests/feature_lane_coverage.rs`'s `every_workspace_member_is_built_by_some_lane`
/// unless some lane names it, and would leave the tool that DERIVES the merge gate as the one crate
/// the merge gate never compiles.
pub const EXCLUDE_FROM_CI: &[&str] = &["vike-desktop", "vike-backfill"];

/// The DataFusion `hist` job's trigger set — intersected with the affected set, not closed over it.
///
/// That job compiles vike-data + vike-backfill with `hist-datafusion`, plus vike-report with its own
/// `hist` feature (which enables `vike-data/hist-datafusion`) — the tearsheet bin's `--store` path.
/// Listing vike-report here is what makes a vike-report-only change trigger the hist job too.
pub const HIST_CRATES: &[&str] = &["vike-data", "vike-backfill", "vike-report", "vike-datahub"];

/// The the CI box LightGBM job's trigger set: the ONE crate whose tests DRIVE the real trainer binary —
/// vike-ml owns all three suites the job runs (`lightgbm_cli_smoke`, `train_infer_equality` and
/// `gbdt_learner_smoke`; `scripts/ci_lightgbm_suite.sh` is the authority for that list). Same shape
/// as [`HIST_CRATES`]: a narrow explicit set intersected with the affected set, not a graph closure
/// of its own.
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

/// The crates whose code actually COMPILES INTO the binary the the latency box latency gate measures.
///
/// The gate builds exactly `cargo test -p vike-core --release --test runtime_latency`, which links
/// vike-core's lib plus its NORMAL dependencies — vike-model, vike-exec and vike-data
/// (`crates/vike-core/Cargo.toml`) — and nothing else. Same shape as [`HIST_CRATES`]: a narrow
/// explicit set, not a graph closure.
///
/// Not closed downward by hand: [`super::super::selection::latency_affected`] walks the NORMAL
/// reverse-dep graph from the changed crates, so a crate inserted BELOW any of these four (a new
/// leaf under vike-model, say) fires the gate with no edit here.
///
/// ⚠ vike-mm / vike-strategy / vike-script are vike-core's DEV-dependencies and are deliberately
/// ABSENT. `cargo test --no-run` compiles them, but they are not in vike-core's LIBRARY dep graph,
/// so they cannot change one instruction of the fold whose p99 is being measured — and
/// `runtime_latency.rs` imports only vike_core / vike_exec / vike_model. A compile break in any of
/// the three is caught by the roster lane, which tests and clippy-gates all three on every run.
pub const LATENCY_CRATES: &[&str] = &["vike-core", "vike-exec", "vike-model", "vike-data"];

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
/// CI run 34020320299 (PR #1653, a change confined to `crates/vike-agent-eval/`) planned
/// `-p vike-agent-eval -p vike-cli -p vike-data -p vike-ops`, and the `test` job went red:
/// `crates/vike-agent-eval/tests/relative_work_dir.rs`'s `a_relative_work_dir_still_stands_a_node_up`
/// found `target/debug/vike-cli` but no `vike-tradehub` binary, and
/// `crates/vike-agent-eval/tests/scripted_pipeline.rs`'s
/// `every_case_passes_when_driven_by_its_own_scripted_plan` failed identically. Those two are the
/// whole CI-gateable half of that harness — the model half is nondeterministic and can never be a
/// merge gate.
///
/// ⚠ The suite had been green on every earlier run, and that was LUCK rather than coverage:
/// `.github/workflows/ci.yml`'s jobs check out with `clean: false` to keep `target/` warm on the
/// runner, so a `vike-tradehub` left behind by an unrelated run had been answering
/// `crates/vike-agent-eval/src/lib.rs`'s `locate_binary`. The PR that sees the defect is whichever
/// one lands on a box after that stale copy is evicted — the same "reddens the next unrelated
/// change" shape [`super::gate_crates::SETTINGS_GATE_CRATES`] exists against, wearing a build artifact instead of a
/// gate.
///
/// # Why no closure can find this edge
///
/// `crates/vike-agent-eval/Cargo.toml` declares NO `vike-*` dependency at all, normal or dev, and
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
/// (`crates/vike-tradehub/tests/daemon/help_and_log_dir.rs`, `crates/vike-cli/tests/exit_codes.rs`),
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
/// own shape — `cargo nextest run <the 68-crate roster> -E 'package(=vike-agent-eval)'`: nextest
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
/// **Let the test shell out to `cargo build`.** `crates/vike-agent-eval/tests/scripted_pipeline.rs`
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
    &[("vike-agent-eval", &["vike-cli", "vike-tradehub"])];

/// The GUI shell. Excluded from the test lane ([`EXCLUDE_FROM_CI`]), so its compile gate reads the
/// affected set directly.
pub const APP_CHECK_CRATE: &str = "vike-desktop";
