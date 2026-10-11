//! How a suite's triggers are matched (closure, manifest closure, dependency-only) and packed.

/// The suites that keep the whole REVERSE-DEPENDENCY CLOSURE as their trigger: an edit to ANY file
/// of a crate below their trigger crates fires them. Only the two catch-alls, because only they
/// check something a plain source edit can break from below:
///   * `multicall` and `workspace-bins` — compile the whole stack from its top, the catch-all for a
///     break in code gated behind a feature that no narrower suite builds. A signature change in a
///     low crate is exactly the edit that breaks a feature-gated call site far above it, they cost
///     under a minute warm, and nothing else compiles every feature at once.
///
/// ⚠ **They walk the closure of what something can BUILD, not of what something OWNS.** A crate whose
/// only changed files are test-only (`tests/`, `benches/`, `examples/`, a `#[cfg(test)]` module) is no
/// seed: neither arm compiles such a file in any dependent, so a test-only edit of a trigger fires
/// neither suite — except for the crates [`CLOSURE_SUITE_TEST_BUILDS`] names, whose own test targets
/// the arm builds. A source file, `Cargo.toml`, `build.rs` and a `test-support` module (its owner and
/// its dev enablers) keep the whole walk. (Measured on the 30 PRs before 2026-10-11: one, a test-only
/// vike-report edit, fired both suites.)
///
/// Every other suite in [`super::feature_suites::FEATURE_SUITES`] fires only when a crate it names owns a changed file —
/// except those in [`MANIFEST_CLOSURE_SUITES`]. ⚠ Adding a key means deciding which side it is
/// on, and the default is DIRECT: a suite here costs a lane on every edit below its triggers, the
/// cost this rule stops paying for suites that merely COMPILE a feature of a crate that happens to
/// sit above a busy one. `xtask/tests/ci_plan_gate/lane_directness.rs`
/// holds every name here to a real [`super::feature_suites::FEATURE_SUITES`] key, and the two tables disjoint.
pub const CLOSURE_SUITES: &[&str] = &["multicall", "workspace-bins"];

/// For each [`CLOSURE_SUITES`] key, the crates whose OWN test targets its arm in
/// `scripts/ci_feature_suite.sh` builds — the only crates for which a TEST-ONLY edit (a `tests/`,
/// `benches/` or `examples/` source, a `#[cfg(test)]` module) still fires the suite.
///
/// A closure suite keeps the reverse-dependency walk for everything a dependent can LINK, and ignores
/// an edit nothing it builds can observe: a crate whose only changed files are test-only is a seed of
/// `changed` and not of `linkable` (`super::super::graph::is_test_target_source`,
/// `super::super::graph::is_cfg_test_module`), and the closure suites are matched against that
/// `linkable` closure — not against the crate itself — for every trigger NOT named here.
///   * `multicall` — `cargo test -p vike-backend --features full` and `cargo clippy -p vike-backend
///     --features full --all-targets` build vike-backend's test targets and no other crate's, so a
///     test-only edit of vike-backend fires it and a test-only edit of any other trigger (vike-report,
///     vike-datahub, vike-tradehub …) does not. Every other crate is compiled as a DEPENDENCY, lib only.
///   * `workspace-bins` — `cargo check --workspace --bins` builds the non-test profile of every `[[bin]]`
///     (no `cfg(test)`, no dev-dependency, no `tests/`/`benches/`/`examples/` target; no `[[bin]]` of
///     this workspace has its `path` under one of those directories), so no test-only edit can move it
///     and the row is empty.
///
/// A `test-support` module (`super::super::graph::feature_test_module`) is NOT test-only for this rule: its
/// owner and its dev enablers stay seeds, exactly as before.
///
/// ⚠ Held to the arm, both ways, by `xtask/tests/ci_plan_gate/closure_test_only.rs`: the crates the
/// arm names with `-p` must be exactly this row, and an empty row's arm may not build a test target.
/// A new closure suite needs a row here (a test fails until it has one); an arm that grows a `-p`
/// or a `--tests` fails that test, naming the row to decide.
pub const CLOSURE_SUITE_TEST_BUILDS: &[(&str, &[&str])] =
    &[("multicall", &["vike-backend"]), ("workspace-bins", &[])];

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
///   * `light-consumers` — vike-cli's NORMAL dependency tree must stay DataFusion-free and
///     transport-free (`cargo tree` greps in the arm), and vike-bridge-core must build with its
///     defaults off. The first is a manifest property by construction; the second is a direct edit
///     of vike-bridge-core, which is a trigger.
///
/// Before 2026-10-06 the first three sat in [`CLOSURE_SUITES`], until the owner asked what a
/// `vike-data` source edit that fired them had to do with them; `light-consumers` joined over the
/// same question on #2543 and #2544.
pub const MANIFEST_CLOSURE_SUITES: &[&str] =
    &["windows-cross", "studio-standalone", "bridges-feeds", "light-consumers"];

/// The trigger crates a suite names only because they are DEPENDENCIES of what its arm builds: the
/// arm's commands never run their tests, so an edit to their TEST files cannot change anything the
/// suite compiles or runs. A SOURCE edit to one still fires the suite — that is what compiles into
/// it — and a test-only edit fires nothing.
///
/// Before 2026-10-06 test files counted too, so a one-line edit to
/// `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` fired `venue-catalog` and `backfill-serve`
/// (both build `vike-datahub` with the Dukascopy provider in): lanes with nothing to do with it.
///
/// ⚠ A trigger the arm does NOT name and that is not in this table is a defect, and
/// `xtask/tests/ci_plan_gate/lane_triggers.rs` fails on it — which is the gate this
/// table exists for. Two stale rows were found the day it was written: `light-consumers` named
/// `vike-ops` after the check that built it was deleted, and `recorder-venues` names `vike-recorder`
/// although its own arm says "there is no `-p vike-recorder --features …` pair any more". The same
/// gate fails the other way: a row here whose crate the arm NOW names is stale, because then the
/// suite does run that crate's tests and its test files matter again. Suites in
/// [`CLOSURE_SUITES`] are not held to this: their triggers are the top of the stack, built through
/// one `-p vike-backend --features full`, and never named one by one.
pub const DEPENDENCY_ONLY_TRIGGERS: &[(&str, &[&str])] = &[
    ("backfill", &["vike-bridge-core"]),
    ("polymarket-stack", &["vike-polymarket"]),
    ("recorder-venues", &["vike-recorder", "vike-polymarket", "vike-binance"]),
    (
        "live-feeds",
        &[
            "vike-binance",
            "vike-bybit",
            "vike-okx",
            "vike-aster",
            "vike-hyperliquid",
            "vike-polymarket",
            "vike-deribit",
        ],
    ),
    (
        "venue-catalog",
        &[
            "vike-binance",
            "vike-bybit",
            "vike-okx",
            "vike-aster",
            "vike-hyperliquid",
            "vike-polymarket",
            "vike-dukascopy",
            "vike-fxcm",
        ],
    ),
    (
        "backfill-serve",
        &[
            "vike-backfill",
            "vike-data",
            "vike-binance",
            "vike-bybit",
            "vike-okx",
            "vike-aster",
            "vike-deribit",
            "vike-hyperliquid",
            "vike-dukascopy",
            "vike-oanda",
        ],
    ),
];

/// Feature-suite MATRIX PACKING — which suite keys may share one `features` job.
///
/// Each entry is a set of keys from [`super::feature_suites::FEATURE_SUITES`]; when several members of one set are
/// affected in the same run, [`super::super::selection::pack_suites`] emits them as ONE space-separated matrix leg
/// and `scripts/ci_feature_suite.sh` runs each key in argv order. What each lane RUNS is untouched:
/// the same `case` arms, serialized into one job instead of scheduled as siblings.
///
/// WHY: a matrix leg's overhead is not its compile. MEASURED (last 10 green ci runs via the jobs
/// API, 2026-08-19..21, corroborated on the the CI box runner journals): every leg pays a job-assignment
/// gap that is bimodal at ~2s or ~52s (the runner's broker long-poll — the service never restarts
/// between jobs, so no unit-level knob can shrink it), plus setup, plus a queue wait that averaged
/// ~98s across 109 legs because the fan-out saturates the 6-runner `ci-build` pool — while the
/// SMALL lanes' useful work is seconds (the smallest then measured, a structural lane deleted
/// since, averaged ~6s of work against ~184s of queue; fxcm ~21s against ~265s). Packing trades a
/// little serialization inside one job for fewer assignment slots, fewer setups, and a shorter
/// pool queue.
///
/// The packing rule, and what earns a set membership:
///   * combined average work must stay WELL UNDER the long pole (the `test` job, ~170-240s) so a
///     packed leg never becomes the run's tail;
///   * members should tend to FIRE TOGETHER (shared trigger crates, or the full-matrix
///     escalations that schedule everything) — a member affected alone is emitted alone, so
///     packing never widens what a narrow PR runs;
///   * a lane whose arm carries side effects needing isolation (png-export's CWD artifacts,
///     windows-cross's target provisioning) stays UNGROUPED.
///
/// The sets (10-run average work seconds in parens):
///   * the three structural/tiny lanes (~47s combined): studio-standalone (11) +
///     bridges-feeds (15) + fxcm (21). ⚠ It was FOUR (~53s) until 2026-09-28, when its smallest
///     member (6) was deleted — the vike-alerting GONE note on [`super::feature_suites::FEATURE_SUITES`] says why. ⚠ The
///     `fxcm` figure PREDATES that arm's widening to the vike-run/vike-tradehub feature chain, so it
///     is an underestimate kept as the last thing actually measured rather than a guess; the set
///     stays comfortably under the long pole even taking the tradehub pair's cost as its ceiling.
///     ⚠ The `bridges-feeds` figure is stale for the same reason and by MORE: ruling 8 doubled
///     that arm's crate count (three EIP-712 bridges → those plus binance/bybit/okx, each with a
///     `cargo check`; the three tree greps stayed at four, since group 2 shares aster's), so read
///     15 as a floor, not a measurement of today's arm.
///     Re-measure before packing anything else in here;
///   * the polymarket-adjacent trio (~116s): polymarket (33) + socks-proxy (44) +
///     recorder-venues (39). ⚠ The `recorder-venues` figure PREDATES ruling 10 adding a
///     `-p vike-datahub --features record-*` build to that arm, AND decision 0092 removing the arm's
///     `-p vike-recorder --features …` pair, so it measures neither of today's arm's two steps —
///     kept as the last thing actually MEASURED, like the `fxcm` and `ibkr` figures. Re-measure
///     before packing anything else into this trio;
///   * `ibkr` (46s measured) is UNGROUPED — a solo member is emitted alone anyway (see the packer
///     rule above), so it needs no set of its own. ⚠ It shared a job with `backfill-ibkr` (one
///     ibapi tree) until docs/decisions/0094 deleted that suite with the `ibkr` feature it compiled
///     (measured unused). The figure PREDATES `ibkr`'s own widening to the
///     vike-mount/vike-run/vike-tradehub feature chain, so it is an underestimate kept as the last
///     thing MEASURED, as the `fxcm` one is. RE-MEASURE before packing it with anything else;
///   * vike-tradehub's `telegram` was PACKED with `tradehub-sinks` (46s + 48s) until that lane went
///     with the two sink features it existed for (0084); unpaired, it is emitted alone.
///
/// ⚠ A member named here that is not a [`super::feature_suites::FEATURE_SUITES`] key would never be affected, so its set
/// would silently degrade to singletons — `xtask/tests/ci_plan_gate.rs`'s packing tests
/// pin membership validity and the packing behaviour itself.
pub const SUITE_GROUPS: &[&[&str]] = &[
    &["studio-standalone", "bridges-feeds", "fxcm"],
    &["polymarket", "socks-proxy", "recorder-venues"],
];
