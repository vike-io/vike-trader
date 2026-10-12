//! How a suite's triggers are matched (direct, or the manifest closure), which suites leave the pull-request plan, and how the rest are packed into jobs.

/// The suites that fire on a direct edit OR on an edit to a MANIFEST (`Cargo.toml`) or BUILD SCRIPT
/// (`build.rs`) of a crate anywhere below their trigger crates — and on no other edit below them.
/// What each checks is a STRUCTURAL property, which a source edit cannot move and a manifest edit
/// can:
///   * `studio-standalone` — the Studio crates' DEFAULT build must stay DataFusion-free. The enable
///     that breaks it is a `[features]` or dependency line in a crate below (vike-data,
///     vike-backtest), not a function body.
///   * `bridges-feeds` — the feeds-only half of each venue bridge must not pull the signer stack
///     (`eip712`, in vike-bridge-core below all seven). Again a manifest property.
///   * `windows-cross` — the release's Windows build of the top crates. A target stops building when
///     a dependency, a feature or a `build.rs` changes; the ordinary source edit that adds
///     platform-only code is the one this misses on the PR, and `release.yml`'s `features` job
///     (which runs every suite) and its own `windows` job catch it before anything is published.
///   * `light-consumers` — vike-cli's NORMAL dependency tree must stay DataFusion-free,
///     transport-free and runtime-free (`cargo tree` greps in the arm): a manifest property by
///     construction.
///
/// Before 2026-10-06 the first three kept the WHOLE reverse-dependency closure (as the `multicall` and
/// `workspace-bins` suites did until decision 0132 folded them into the `tradehub-all` job), until the
/// owner asked what a `vike-data` source edit that fired them had to do with them; `light-consumers`
/// joined over the same question on #2543 and #2544.
pub const MANIFEST_CLOSURE_SUITES: &[&str] =
    &["windows-cross", "studio-standalone", "bridges-feeds", "light-consumers"];

/// The suites that LEAVE THE PULL-REQUEST PLAN: `plan::compute` drops these keys when the event is
/// `pull_request` and keeps them for every other event (`push` to main, `merge_group`,
/// `workflow_dispatch`, a run with no event) and for `CI_FULL=1`, the release's plan. ONE edit point:
/// a key joins or leaves by this row alone, and `xtask/tests/ci_plan_gate/post_merge_suites.rs` holds
/// every name here to a real [`super::feature_suites::FEATURE_SUITES`] key.
///
/// WHY these four (CI trim 2, owner-approved 2026-10-11, measured over 70 days of
/// `ci` runs): each guards a build almost nothing else depends on, and none caught a real regression
/// on a pull request in that window (0 unique catches), while each costs a runner slot on every PR
/// that touches its crate:
///   * `png-export` — the dev-box offscreen-render harnesses (`examples/export_png.rs`, `studio_shot`),
///     149 s, the longest suite. Nothing a user runs links them.
///   * `backfill` — the vike-backfill CLI tool features (`databento`, `tardis`, `vike-archive`).
///   * `study-cli` — the `vike-study` bin, off by default.
///   * `backtest-hist-replay-tests` — the test run SPLIT OFF `backtest-hist-replay` (owner-approved
///     2026-10-11): `cargo test -p vike-backtest --features datafusion-store`, about 220 s of a 330 s
///     job. The key it left, `backtest-hist-replay`, kept the two `clippy --all-targets` steps on the pull
///     request (since decision 0132 they are lines of `ci.yml`'s `datahub-all` job), so a compile error of
///     the crate is still caught on the PR; the suite's only unique catches in 88 days were two compile
///     errors, and the tests run after the merge and in the release. (0124 left the suite whole; the split
///     is the owner's later ruling.)
///
/// WHERE THEY RUN INSTEAD: on `release.yml`'s `features` job (`CI_FULL=1`, every suite, before anything
/// is published) and on every `push` run to main that exists. ⚠ **NOT on every merge:** a squash merge
/// whose message carries `[skip ci]` starts no run on main (60% of merges, 2026-10), so a break one of
/// these suites would have caught can sit on main until the next un-skipped push or the release. That
/// is the accepted risk, and the owner chose it over a per-merge post-merge workflow. Reopen when a
/// release's `features` job is red on one of these four from a change that merged green.
///
/// ⚠ **A global escalation on a pull request drops them too.** A workspace-global file (the root
/// manifest, `Cargo.lock`, `ci.yml`, a script) fires every OTHER suite on a PR; these four are
/// post-merge by rule, not by narrowing, so the escalation does not buy them back — the release runs
/// them. Likewise an unresolvable diff base on a `pull_request` event. Only the EVENT decides, with
/// ONE exception below: a plan for `CI_FULL=1` keeps all of them.
///
/// ⚠ **THE ONE EXCEPTION: THE DEPENDABOT GUARD.** On a `pull_request` a suite here STAYS in the plan
/// when the change MOVES a package that one of the suite's own trigger crates links
/// ([`super::feature_suites::FEATURE_SUITES`]' row for the key): a root-manifest
/// `[workspace.dependencies]` entry or a lockfile version that changed, read by
/// `xtask::ci::workspace_deps::moved_consumers` and matched by
/// `xtask::ci::selection::post_merge_dropped`. WHY: `png-export` was created after a Dependabot
/// `pollster` bump went green with nothing compiling the render harnesses (#1023 -> #1068), and 0124
/// took that protection away; this gives it back for the one class of change that can cause the
/// drift, at the cost of a lane on dependency pull requests only.
///   * **Dependency-SHAPED only.** A moved PACKAGE, not a `Cargo.toml` edit (a root edit outside
///     `[workspace.dependencies]`, a `[profile]` or members edit, moves no package and keeps nothing; a
///     member's own manifest is no root edit) and not a source edit (it keeps nothing, beside a bump
///     too: only the bump keeps a suite). A lockfile that only ADDED packages moved none.
///   * **Exactly the suites whose crates consume the package**, never all of them: a bump only
///     `vike-chart` links keeps `png-export` and neither of the others.
///   * **It reads the SAME consumer set the direct suites use.** For a dependency-only change that set
///     is `plan::compute`'s `dep_consumers`, so a suite is kept for exactly the reason it fires on
///     `push` (`xtask/tests/ci_plan_gate/post_merge_dependency_guard.rs` pins it).
///   * **PRECEDENCE with an escalation (decided):** a dependency move that also escalates — a lockfile
///     that moved with no manifest edit, or a dependency edit beside another global file — keeps the
///     suites its package reaches and ONLY those; the other post-merge suites are dropped as 0124
///     decided, and every other suite fires as on any escalation. A dependency-shaped change with no
///     other global file does not escalate at all (`workspace_deps`), and keeps the same set.
///
/// ⚠ **THE DEPENDABOT NARROWING LIVES IN THE PLANNER, for every guarded suite.** On a `pull_request`
/// whose `GITHUB_ACTOR` is [`DEPENDABOT_ACTOR`], `xtask::ci::plan::compute_with` keeps in the plan ONLY
/// the members of this table that the guard above kept, so `ci.yml`'s `features` job (its `if:` is
/// `suites != '[]'` and its matrix is the plan's list verbatim) runs exactly those: a bump a `png-export`,
/// `backfill`, `study-cli` or `backtest-hist-replay-tests` crate links runs that suite, and a bump no
/// guarded suite consumes plans none (the matrix is skipped). Every other suite is dropped for Dependabot,
/// whatever the diff would fire for a human (the full per-feature matrix on a `Cargo.lock` change is what
/// the narrowing exists to avoid). `CI_FULL=1` is no pull request and fires everything whatever the actor;
/// an unresolvable base keeps no guarded suite, so a Dependabot plan over one is empty. ⚠ `backfill`
/// triggers on `vike-bridge-core`, which links most third-party packages, so it runs on most Dependabot
/// bumps too.
pub const POST_MERGE_SUITES: &[&str] =
    &["png-export", "backfill", "study-cli", "backtest-hist-replay-tests"];

/// The `GITHUB_ACTOR` whose pull requests the planner narrows to the guarded suites
/// ([`POST_MERGE_SUITES`] after the guard): GitHub's name for Dependabot's own account, matched exactly.
pub const DEPENDABOT_ACTOR: &str = "dependabot[bot]";

/// What one suite costs, in seconds of JOB wall time — the weight [`super::super::selection::pack_suites`]
/// packs by. One row per [`super::feature_suites::FEATURE_SUITES`] key and no other
/// (`xtask/tests/ci_plan_gate/suite_cost_packing.rs` holds both directions: a suite without a row would
/// be packed as if it cost [`PACK_TARGET_SECONDS`], i.e. alone).
///
/// MEASURED: the mean wall time of the suite's job when it ran ALONE as a successful `features` leg,
/// over 88 days of `ci` runs (the jobs API, 260-608 samples per key), taken
/// from the `features (<key>)` job names. A job's wall time includes its setup (checkout, cache
/// restore, toolchain), about 20-25 s, so the sum of two rows OVERSTATES the packed job by about that
/// much (measured: a leg of the since-deleted fxcm + studio-standalone + bridges-feeds ran 90 s against
/// 117 s summed) —
/// the packing is conservative on purpose, it never builds a pack longer than the target.
///
/// ⚠ ONE ESTIMATE, marked: `backtest-hist-replay-tests` (223) is the test half of the split arm, taken
/// from ONE traced job (clippy 41 s + test build 138 s and run about 85 s + clippy 57 s, 2026-10-11), not
/// from 88 days of samples; re-measure it once the split has run. Every other row is a measured mean.
/// (The rows of the eight suites decision 0132 folded into the `datahub-all` and `tradehub-all` jobs went
/// with their keys: a job is not packed.)
/// `bridges-feeds` (29) was measured with its seven checks as seven invocations; decision 0132 made six of
/// them one, which can only shorten it.
/// Re-measure a row when its arm changes (`scripts/ci_feature_suite.sh`); the rows are claims, and an
/// outdated one only mis-packs, it never drops a suite.
pub const SUITE_COST_SECONDS: &[(&str, u32)] = &[
    // ESTIMATE, one traced job (see the doc above): the split-off test run of the old `backtest-hist-replay`.
    ("backtest-hist-replay-tests", 223),
    ("png-export", 149),
    ("light-consumers", 130),
    ("backfill", 74),
    ("study-cli", 72),
    ("socks-proxy", 65),
    ("windows-cross", 58),
    ("bridges-feeds", 29),
    ("studio-standalone", 24),
];

// Feature-suite MATRIX PACKING — why suites share a `features` job at all, and the knobs.
//
// A matrix leg's overhead is not its compile. MEASURED (last 10 green ci runs via the jobs API,
// 2026-08-19..21, corroborated on the the CI box runner journals): every leg pays a job-assignment gap
// that is bimodal at ~2s or ~52s (the runner's broker long-poll — the service never restarts
// between jobs, so no unit-level knob can shrink it), plus setup, plus a queue wait that averaged
// ~98s across 109 legs because the fan-out saturates the 6-runner `ci-build` pool, while the small
// lanes' useful work is seconds (a structural lane then measured averaged ~6s of work against ~184s
// of queue). Packing trades a little serialization inside one job for fewer assignment slots, fewer
// setups and a shorter pool queue.
//
// Until 2026-10-11 the packing was a hand list of two sets (`SUITE_GROUPS`) that were measured to fire
// together, and every suite outside them was a job of its own however small. It is now ONE rule over
// ONE table, [`SUITE_COST_SECONDS`]: [`super::super::selection::pack_suites`] fills jobs
// first-fit-decreasing up to [`PACK_TARGET_SECONDS`], never schedules fewer than [`PACK_MIN_JOBS`] jobs
// for that many suites, and keeps [`PACK_ALONE_SUITES`] out of any pack. A key added to
// `FEATURE_SUITES` needs a cost row (a test fails until it has one), not a decision about which set
// it joins.

/// The most a packed `features` job may sum to, in [`SUITE_COST_SECONDS`]: the long pole of a
/// typical run is the `test` job (~170-240 s) and the heaviest single suite is ~150 s, so a pack at or
/// under this never becomes the tail of the run. A suite whose own cost exceeds it stays alone.
pub const PACK_TARGET_SECONDS: u32 = 120;

/// The fewest `features` jobs a run with this many suites or more may schedule: the pool has SIX
/// `ci-build` runners ([`super::super::selection::pack_suites`] splits the largest packs until there are
/// this many), so packing 6+ suites into fewer jobs would leave a runner idle while another works
/// through a pack. Fewer suites than this pack freely.
pub const PACK_MIN_JOBS: usize = 6;

/// The suites that are NEVER packed with another, whatever their cost: their arm has a side effect
/// the next arm in the same job must not inherit, and `ci.yml` relies on it.
///   * `png-export` — its arm RUNS two render examples that write PNGs into the CWD, so a packed leg
///     would leave the next arm in the same job a directory of stray PNGs.
///   * `windows-cross` — its arm `rustup target add`s the Windows target and checks into a cross
///     target directory of its own; kept out of a shared job so a failed provisioning cannot take a
///     neighbour's result down with it.
///
/// Held to real keys, and to staying alone in a full matrix, by
/// `xtask/tests/ci_plan_gate/suite_cost_packing.rs`.
pub const PACK_ALONE_SUITES: &[&str] = &["png-export", "windows-cross"];
