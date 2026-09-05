//! The declared sets the CI plan is computed from — and, since the Python planner was deleted, the
//! ONE authority for them.
//!
//! Every table below carries its own argument. That is the point of this file: the sets are small
//! and the reasons are not, and each reason is a measurement somebody paid for. Until the port
//! landed these comments lived on the Python planner's constants, and this file said "the rationale
//! lives there, and MOVES here in the PR that deletes the script". This is that PR, so they moved —
//! once, not copied, because a copy of a justification rots exactly the way the crate rosters this
//! repository keeps re-deriving do.
//!
//! ⚠ A row here is not a preference, it is a claim about what CI would otherwise fail to test.
//! Deleting one is allowed; deleting one without answering its comment is how a gate stops firing
//! in silence, which every "EXCLUDED, with the reason" list below exists to prevent.

/// Workspace members that are NOT in the default `test`+`clippy` lane. The roster is
/// `sorted(names - EXCLUDE_FROM_CI)`, so a NEW crate joins CI the moment it joins
/// `[workspace].members` and nothing has to be edited to let it in.
///
/// The exclusions, and why each is not in the default lane:
///
///   * `ibapi` — the vendored IBKR API copy (exercised via vike-ibkr's `ibkr` feature).
///   * `vike-app` — the egui/wgpu GUI binary: out of the shared nextest+clippy roster, because its
///     `fat` feature resolution matches no other lane's cache. NOT uncovered — `ci.yml`'s
///     `app-check` job checks it (fat + thin), lints it and runs its unit tests, gated off the `app`
///     output ([`APP_CHECK_CRATE`]).
///   * `vike-backfill` — only builds/tests under features (the hist + `backfill`/`backfill-ibkr`
///     suites).
///
/// A new crate that genuinely cannot build in the plain lane belongs here, with its reason.
///
/// ⚠ `xtask` is deliberately ABSENT: it is a workspace member and a CI crate like any other. The
/// alternative — a member excluded from the roster — is refused outright by
/// `crates/vike-ops/tests/feature_lane_coverage.rs`'s `every_workspace_member_is_built_by_some_lane`
/// unless some lane names it, and it would leave the tool that DERIVES the merge gate as the one
/// crate the merge gate never compiles.
pub const EXCLUDE_FROM_CI: &[&str] = &["ibapi", "vike-app", "vike-backfill"];

/// Feature-gated suites: run a suite iff ANY crate it compiles is in the affected set.
///
/// ⚠ **ORDER IS PART OF THE OUTPUT.** `suites` is emitted as a JSON array in THIS order and
/// `.github/workflows/ci.yml` turns it into a matrix, so reordering these rows reorders the matrix
/// legs.
///
/// ⚠ **A suite that turns on an OPTIONAL cross-crate dep must list BOTH crates** (e.g.
/// `backfill-ibkr` = `vike-backfill --features ibkr`, which pulls `vike-ibkr`): an optional dep is
/// not a default-build reverse-dep edge, so a change to the PULLED crate would otherwise be missed
/// by the narrow selection. Every multi-crate row below is that rule being applied.
///
/// Per-row notes, each of which is why the lane exists rather than what it runs
/// (`scripts/ci_feature_suite.sh` is the authority for the invocations):
///
///   * `socks-proxy` — the optional SOCKS5 WS egress (`ws_proxy.rs`) lives behind an opt-in feature
///     so a default build compiles no proxy code: its dial arm plus the `socks5h` wire test only
///     compile with `--features socks-proxy`. Only vike-polymarket's own off-by-default feature
///     enables it, so — unlike `eip712`/`capture` — the default lane never does.
///     ⚠ There are deliberately NO `eip712`/`capture` suites: both features gate UNIT tests inside
///     vike-bridge-core's own lib (`src/eip712.rs` / `src/capture.rs`; nothing under `tests/` is
///     feature-gated), and the default multi-package lane already compiles and runs them —
///     vike-aster's NORMAL dep enables `vike-bridge-core/eip712`, and vike-binance/vike-bybit's
///     dev-deps enable `capture`, so the single invocation unifies both into bridge-core's one lib
///     build (resolver 2 unifies features across packages built together).
///   * `backfill` — ONE union-features lane over vike-backfill's five independent tool features
///     (`poly-reparse` + `databento` + `tardis` + `poly-ch-backtest` + `vike-archive`; the merge
///     rationale is in `scripts/ci_feature_suite.sh`). The trigger is the union of the five lanes'
///     sets: `poly-reparse` pulls vike-polymarket, `databento`/`tardis`/`vike-archive` pull
///     vike-bridge-core (the canonical credentials loader — vike-archive's own bin uses it for
///     `VIKE_ARCHIVE_API_KEY`), `poly-ch-backtest` pulls the vike-backtest harness.
///   * `backfill-ibkr` — kept SEPARATE from `backfill`: the `ibkr` feature compiles the vendored
///     ibapi tree, a large build no other backfill lane needs.
///   * `polymarket-stack` — ONE lane for the nested polymarket mount stack (`vike-mount/polymarket`
///     ⊂ `vike-run/polymarket` ⊂ `vike-tradehub/polymarket`, each feature turning on the one below
///     it): test+clippy over the three crates with each crate's own feature, so every feature-gated
///     arm and bin (mount's RECON-ONLY `make_engine` arm, run's paper-maker bin + live Feeds
///     wiring, tradehub's LIVE daemon arm) still compiles while the shared tree builds once instead
///     of three times.
///   * `telegram` — vike-tradehub's opt-in TELEGRAM control channel. A remote order-origination
///     path reachable from a third-party chat service is compiled OUT of a default build
///     (`#[cfg(feature = "telegram")]` on the module, on the mount and on both call sites), so a
///     default `cargo test -p vike-tradehub` compiles NONE of it and `tests/telegram_control.rs` is
///     `#![cfg]`-ed away to an empty binary. Without this suite the whole channel would be
///     un-type-checked, un-clippy-gated and never run — the way the `benches/engines.rs` `--bench`
///     bug survived. Kept SEPARATE from `tradehub-sinks`: those two features pull DataFusion in and
///     this one must stay provably DataFusion-free. The trigger is the crate itself, because the
///     feature turns on no cross-crate optional dep — only the crate's own `dep:ureq`, already in
///     this graph transitively via vike-bridge-core.
///   * `tradehub-sinks` — ONE lane for vike-tradehub's two opt-in DataFusion sink arms
///     (`record-feeds` + `materialize`; both enable `vike-data/hist-datafusion` plus one cfg'd
///     module, and a default `cargo test -p vike-tradehub` compiles NEITHER). The materializer
///     itself lives in vike-ops, so a change there must trigger this suite too.
///   * `backtest-hist-replay` — vike-backtest's OPTIONAL `hist-replay` feature gates the whole
///     `harness/` tree (profile/run/sweep/euler), `hist_replay.rs` and the rayon sweep pool; a
///     default `cargo test -p vike-backtest` compiles NONE of it, and no other crate enables the
///     feature transitively. This suite runs BOTH lanes: `hist-replay` (the harness compiled
///     TRAIT-ONLY, DataFusion-free — the decouple gate) and `datafusion-store` (the concrete
///     `DataFusionHist` bins/tests/streaming). Without it neither lane is type-checked,
///     clippy-gated or run.
///   * `alerting-standalone` — vike-alerting's DEFAULT (vike-free) build. The plain lane NEVER
///     compiles it: that lane selects `-p vike-alerting -p vike-ops` in one invocation and vike-ops
///     depends on the crate with `core` + `workspace-env`, so resolver-2 unifies both features in.
///     This suite runs the crate ALONE and asserts the property the split exists for — no `vike-*`
///     crate in its default dep tree. The trigger is the crate itself; vike-ops is deliberately NOT
///     listed, since a vike-ops-only change cannot alter this crate's default build.
///   * `recorder-venues` — the recorder's venue feeds. The default lane compiles vike-recorder
///     WITHOUT any venue feature, and that build has no `venues::*` module at all, so this is the
///     only lane that type-checks the rolling-family resolver, the `narrow` de-duplication and the
///     daemon bin's venue arm. vike-polymarket/vike-binance are in the trigger set because the feed
///     is an ASSEMBLY over those crates' discovery/universe/Feeds.
///   * `light-consumers` — the default-features-OFF half of vike-bridge-core, plus the light crates
///     that take it that way. A WORKSPACE build cannot see this configuration: resolver-2 unifies
///     features, every other consumer takes `full`, so a module that forgets its
///     `#[cfg(feature = "full")]` compiles everywhere else CI looks. That let #1046 land an ungated
///     `leverage` module naming `vike_exec` one PR after #1042 made vike-exec optional — both
///     green, and `main` could not build `-p vike-cli` at all. The trigger set is the crates whose
///     feature gating or light-half dependency edges define the property.
///   * `png-export` — the two offscreen-render harnesses behind `png-export` (vike-chart's
///     `examples/export_png.rs` and vike-studio's `studio_shot`). Both are `required-features`-gated
///     and off by default, so NOTHING compiled them before this suite — not the roster lane, not
///     `app-check`, not any other feature lane. The drift that proves it: the two crates' pollster
///     pins diverged (1.0 vs 0.4) and a dependabot bump on exactly that dependency went green,
///     because CI never built either target (#1023 -> #1068).
///   * `windows-cross` — the WINDOWS compile witness (`cargo check --target
///     x86_64-pc-windows-gnu`). Every workflow here runs on the self-hosted LINUX boxes, so this is
///     the only job in CI that compiles anything for Windows at all.
///
///     It began as the COMPLEMENT of `just windows-check`: that recipe builds `ci_crates`, which
///     excludes vike-app and vike-backfill (they are in [`EXCLUDE_FROM_CI`], so the derived roster
///     cannot name them either), and the result was that
///     `crates/vike-backfill/src/vike_archive.rs`'s `#[cfg(windows)]` `read_at` — the `seek_read`
///     arm that fixed a real `--jobs 4` read-corruption — was compiled by no CI job and no local
///     recipe, on any box. Mutation-proved: a bogus argument to `seek_read` is invisible to every
///     Linux check and fails E0425 under a Windows target. vike-app sits at the top of the dep
///     graph so this fires on nearly every code PR — the same shape as `app-check`, and affordable
///     for the same reason: measured 3.1s warm, 10.8s with the cross `target/` wiped but sccache
///     warm, ~67s only on a runner whose sccache has never seen it.
///
///     ⚠ vike-tradehub joins it as a DELIBERATE overlap with `just windows-check`, not as a third
///     complement — it IS in the derived roster, so that recipe already checks it natively. The
///     overlap is the point: `windows-check` runs on ONE box, by hand, when somebody remembers,
///     and vike-tradehub is the daemon whose `#[cfg(windows)]` background-hosting surface
///     (split-plane B10's console-ctrl stop, I13's stop-file arm) is Windows-only code in the
///     binary that signs real orders. Windows-only code gated on a human running a local recipe is
///     the same shape as the `vike_archive` hole, one manual step removed. It rides the SAME
///     invocation as the other two rather than a second `cargo check`, so it shares their already
///     compiled dependency tree.
///
///     ⚠ vike-cli is a FOURTH admission with a fourth argument: a RELEASE ASSET is cross-built
///     from it. `.github/workflows/release.yml`'s `windows` job builds `vike-cli.exe` beside the
///     two GUI assets, and a tag is the worst place to discover that a target does not build —
///     the tag is already pushed by then. So this lane rehearses the release's own invocation.
///     ⚠ **It must be a TRIGGER, not only a line in the arm**, and that is the half a first
///     reading misses: `affected_keys` fires a suite when its trigger crates intersect the affected
///     set, `vike-cli` is a LEAF binary (nothing depends on it, so reverse-dep closure never
///     reaches it) and it triggers only `light-consumers` — so without this row a PR touching
///     `crates/vike-cli/` alone would schedule every OTHER lane and not the one added for it,
///     leaving the release build to be first compiled at tag time. It rides its own `cargo check`
///     line inside the arm rather than the shared selection, because that line is deliberately not
///     `--all-targets` (the crate's dev-deps are a second large tree the release compiles none of).
///   * `studio-standalone` — the Studio crates' DEFAULT builds, ALONE (split-plane I7:
///     vike-studio/vike-studio-core moved their `vike-data/hist-datafusion` enables to
///     [dev-dependencies], so a default Studio build is DataFusion-free). The roster lane can never
///     see that configuration — it builds the Studio crates in the same invocation as
///     vike-recorder, whose NORMAL dep enables the feature, and resolver-2 unifies it straight back
///     on (`alerting-standalone`'s argument wearing a different feature). The suite also asserts the
///     structural property itself via `cargo tree -e normal` — a compile-only check would go green
///     again the day the enable moved back onto the normal dep. The trigger is the two crates
///     themselves; anything BELOW them (vike-data, vike-backtest, vike-ai …) lands in the affected
///     set by reverse-dep closure and fires the suite without being named here.
///   * `bridges-feeds` — the FEEDS-ONLY half of the three EIP-712 venue bridges (split-plane
///     Phase-5 hardening): vike-hyperliquid / vike-aster with default features off, and
///     vike-polymarket under its `feeds` feature alone (its default is EMPTY) — the market-data
///     surface without the EIP-712 order signer. No other lane can see it — every consumer takes
///     hl/aster with their default-on `exec` feature or polymarket with its full `polymarket`,
///     and resolver-2 unifies it straight back in
///     (`studio-standalone`'s argument wearing the signer stack) — so without this suite a feed
///     module that grew an exec-plane import would compile everywhere else CI looks. The suite
///     also asserts the structural property itself via `cargo tree -e normal` (no signer crate in
///     any of the three feeds trees), not just that the configuration compiles. The trigger is
///     the three crates themselves; vike-bridge-core (whose `eip712` feature is the stack being
///     kept out) sits below all three and lands in the affected set by reverse-dep closure.
pub const FEATURE_SUITES: &[(&str, &[&str])] = &[
    ("polymarket", &["vike-polymarket"]),
    // vike-mount joined the trigger set when the `("fxcm", _)` live arm landed behind
    // vike-mount's own `fxcm` feature: that arm is compiled by this suite and by nothing else, so a
    // vike-mount edit that breaks it has to fire the lane. (The reverse-dep closure does not supply
    // this — vike-mount sits ABOVE vike-fxcm, so a vike-fxcm change reaches it, never the other
    // way.) Same shape as `polymarket-stack`, which names its whole nested stack for the same
    // reason.
    // ...and vike-run/vike-tradehub joined it when the release grew an fxcm-LINKED tradehub asset
    // (`scripts/release_fxcm_artifact.sh`): the lane now compiles the whole forwarding chain
    // `--features fxcm` resolves on that binary, and the reverse-dep closure supplies none of it
    // for the same direction reason as vike-mount. An edit up there that breaks the fxcm feature
    // resolution would otherwise be discovered by a tag, on the artifact people download.
    ("fxcm", &["vike-fxcm", "vike-mount", "vike-run", "vike-tradehub"]),
    // vike-mount/vike-run joined the trigger set when the arm widened from `-p vike-ibkr` to the
    // whole ibkr forwarding chain (`vike-run/ibkr` → `vike-mount/ibkr` → `vike-ibkr/ibkr`). The
    // reverse-dep closure supplies neither, for the same DIRECTION reason `fxcm` records above: both
    // sit ABOVE vike-ibkr, so a vike-ibkr change reaches them and never the other way. What the lane
    // now compiles and nothing else does: vike-mount's `("ibkr", _)` make_engine arm and arming row,
    // and vike-run's node.rs ibkr sites + its `required-features` `ibkr_mount` bin. The chain now
    // reaches vike-tradehub, exactly as `fxcm`'s does: this note used to read "the chain stops at
    // vike-run — vike-tradehub declares no `ibkr` feature, so unlike `fxcm` there is no fourth
    // package to name", and that was true. It was also the defect — `vike-run` carried the feature
    // and `vike-app` forwarded it, so the GUI could mount IBKR live while the headless daemon fell
    // through `make_engine`'s `("ibkr", _)` to paper in silence. The forward exists now, so the
    // fourth package is named here too, or a change to the daemon would not fire the lane that
    // compiles its own ibkr arm.
    ("ibkr", &["vike-ibkr", "vike-mount", "vike-run", "vike-tradehub"]),
    ("socks-proxy", &["vike-bridge-core"]),
    // The `vike-study` bin (`--features study-cli`, off by default so `studio-standalone`'s
    // DataFusion-free structural check keeps holding) — the only lane that type-checks it.
    // vike-studio-core alone: `affected` propagates to dependents, so a change in any crate the
    // bin builds against (vike-data, vike-ml, vike-user-research) reaches this trigger through it.
    ("study-cli", &["vike-studio-core"]),
    // The `vike` multicall dispatcher (`--features full`): every tool row at once, the
    // configuration the release builds. Triggered by the dispatcher itself AND by every crate it
    // dispatches into — a change to any tool's `run` signature breaks this build and nothing else.
    (
        "multicall",
        &[
            "vike",
            "vike-studio-core",
            "vike-report",
            "vike-backtest",
            "vike-datahub",
            "vike-recorder",
        ],
    ),
    ("backfill", &["vike-backfill", "vike-polymarket", "vike-bridge-core", "vike-backtest"]),
    ("backfill-ibkr", &["vike-backfill", "vike-ibkr"]),
    ("polymarket-stack", &["vike-mount", "vike-run", "vike-tradehub", "vike-polymarket"]),
    ("telegram", &["vike-tradehub"]),
    ("tradehub-sinks", &["vike-tradehub", "vike-data", "vike-ops"]),
    ("backtest-hist-replay", &["vike-backtest"]),
    ("alerting-standalone", &["vike-alerting"]),
    ("recorder-venues", &["vike-recorder", "vike-polymarket", "vike-binance"]),
    ("light-consumers", &["vike-bridge-core", "vike-ops", "vike-cli", "vike-secrets"]),
    ("png-export", &["vike-chart", "vike-studio"]),
    ("windows-cross", &["vike-app", "vike-backfill", "vike-tradehub", "vike-cli"]),
    ("studio-standalone", &["vike-studio", "vike-studio-core"]),
    ("bridges-feeds", &["vike-hyperliquid", "vike-aster", "vike-polymarket"]),
];

/// Feature-suite MATRIX PACKING — which suite keys may share one `features` job.
///
/// Each entry is a set of keys from [`FEATURE_SUITES`]; when several members of one set are
/// affected in the same run, [`super::pack_suites`] emits them as ONE space-separated matrix leg
/// and `scripts/ci_feature_suite.sh` runs each key in argv order. What each lane RUNS is untouched
/// — the script's `case` arms are the same invocations, serialized into one job instead of
/// scheduled as siblings.
///
/// WHY: a matrix leg's overhead is not its compile. MEASURED (last 10 green ci runs via the jobs
/// API, 2026-08-19..21, corroborated on the the CI box runner journals): every leg pays a job-assignment
/// gap that is bimodal at ~2s or ~52s (the runner's broker long-poll — the service never restarts
/// between jobs, so no unit-level knob can shrink it), plus setup, plus a queue wait that averaged
/// ~98s across 109 legs because the fan-out saturates the 6-runner `ci-build` pool — while the
/// SMALL lanes' useful work is seconds (alerting-standalone averaged ~6s of work against ~184s of
/// queue; fxcm ~21s against ~265s). Packing trades a little serialization inside one job for
/// fewer assignment slots, fewer setups, and a shorter pool queue.
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
///   * the four structural/tiny lanes (~53s combined): alerting-standalone (6) +
///     studio-standalone (11) + bridges-feeds (15) + fxcm (21). ⚠ The `fxcm` figure PREDATES that
///     arm's widening to the vike-run/vike-tradehub feature chain and is now an underestimate; it
///     is left as the last thing actually measured rather than replaced by a guess, and the set
///     stays comfortably under the long pole even taking the tradehub pair's cost as its ceiling.
///     Re-measure before packing anything else in here;
///   * the polymarket-adjacent trio (~116s): polymarket (33) + socks-proxy (44) +
///     recorder-venues (39);
///   * the vendored-ibapi pair (~106s): ibkr (46) + backfill-ibkr (61) — sharing one job also
///     shares the ibapi tree the second member would otherwise re-warm. ⚠ The `ibkr` figure
///     PREDATES that arm's widening to the vike-mount/vike-run feature chain and is now an
///     underestimate, exactly as the `fxcm` figure above is; it is left as the last thing actually
///     MEASURED rather than replaced by a guess. The unmeasured ESTIMATE that was reasoned from
///     this table when the arm widened: the two vike-tradehub lanes each build a superset of the
///     vike-mount+vike-run tree for 46-48s, so the added work is of that order and the pair lands
///     near ~150s — under the ~170-240s pole, but with far less headroom than any other set here.
///     RE-MEASURE this pair before packing anything else into it, and if it is observed above the
///     pole the fix is to un-group `ibkr` (a member affected alone is emitted alone anyway, and
///     backfill-ibkr's shared-ibapi saving is a fraction of its 61s), not to shave the lane;
///   * the vike-tradehub pair (~94s): telegram (46) + tradehub-sinks (48). ⚠ Their arms stay
///     SEPARATE cargo invocations, so telegram's provably-DataFusion-free build survives packing
///     — the script runs the keys sequentially, never as a feature union.
///
/// ⚠ A member named here that is not a [`FEATURE_SUITES`] key would never be affected, so its set
/// would silently degrade to singletons — `crates/vike-ops/tests/ci_plan_gate.rs`'s packing tests
/// pin membership validity and the packing behaviour itself.
pub const SUITE_GROUPS: &[&[&str]] = &[
    &["alerting-standalone", "studio-standalone", "bridges-feeds", "fxcm"],
    &["polymarket", "socks-proxy", "recorder-venues"],
    &["ibkr", "backfill-ibkr"],
    &["telegram", "tradehub-sinks"],
];

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
/// ⚠ It was a PAIR until the research crate dissolved: the learner adapter's smoke was
/// `vike-research`'s `ml_adapter_smoke`, and it moved with `GbdtLearner` into
/// `crates/vike-ml/tests/gbdt_learner_smoke.rs`. The second element had to go with the crate —
/// see the HARD-failure note below.
///
/// ⚠ Those suites are `#[ignore]`d AND self-skipping, so before that job existed CI compiled them
/// and executed NONE of them, silently. [`super::lightgbm_affected`] is what makes them run; the job
/// itself (`scripts/ci_lightgbm_suite.sh`) is what makes a zero-test run RED.
///
/// A name here that is not a workspace member is a HARD failure in [`super::compute`], because the
/// job would otherwise stop firing in silence.
pub const LIGHTGBM_CRATES: &[&str] = &["vike-ml"];

/// The crates whose code actually COMPILES INTO the binary the the latency box latency gate measures.
///
/// The gate builds exactly `cargo test -p vike-core --release --test runtime_latency`, which links
/// vike-core's lib plus its NORMAL dependencies — vike-model, vike-exec and vike-data
/// (`crates/vike-core/Cargo.toml`) — and nothing else. Same shape as [`HIST_CRATES`]: a narrow
/// explicit set, not a graph closure.
///
/// This set does NOT need to be closed downward by hand. [`super::latency_affected`] walks the
/// NORMAL reverse-dep graph from the changed crates, so a crate inserted BELOW any of these four (a
/// new leaf under vike-model, say) fires the gate automatically, with no edit here.
///
/// ⚠ vike-mm / vike-strategy / vike-script are vike-core's DEV-dependencies and are deliberately
/// ABSENT. `cargo test --no-run` compiles them, but they are not in vike-core's LIBRARY dep graph,
/// so they cannot change one instruction of the fold whose p99 is being measured — and
/// `runtime_latency.rs` imports only vike_core / vike_exec / vike_model. A compile break in any of
/// the three is caught by the roster lane, which tests and clippy-gates all three on every run.
pub const LATENCY_CRATES: &[&str] = &["vike-core", "vike-exec", "vike-model", "vike-data"];

/// The NON-crate inputs that can move the measured binary or the measurement itself. Deliberately
/// much narrower than [`GLOBAL_PREFIXES`], and each entry earns its place:
///
///   * `Cargo.toml` (root, matched by [`super::latency_affected`] rather than by a prefix) —
///     `[profile.release]` (opt-level 3, `lto = "thin"`) and the `[workspace.dependencies]` pins
///     vike-core's own deps inherit.
///   * `Cargo.lock` — the resolved versions of tokio/arc-swap/memmap2/serde that compile INTO the
///     fold. ⚠ The [`super::lock_additive_only`] exemption is deliberately NOT honoured here:
///     "additive" proves no ALREADY-RESOLVED version moved, which is not the same claim as "the fold
///     is unchanged" — adding a brand-new dependency to vike-core is an additive lock change that
///     alters the measured binary.
///   * `rust-toolchain` — the compiler, i.e. the codegen. The gate builds `--release`.
///   * `.cargo/` — the committed Windows-scoped cargo config: RUSTFLAGS / target-cpu / the linker
///     choice all live there. It carries no SETTING today (every knob measured so far is in its
///     rejected list), but a PATH is all this trigger can see, so a comment-only edit to it fires
///     the gate too — which is stated in that file so nobody pays the bill unknowingly.
///   * `.github/workflows/ci.yml` — the gate's OWN harness: the taskset pin, SCHED_FIFO, the
///     quiesce preflight, the settles and the 3x retry. Change how it measures and it must
///     re-measure.
///   * `xtask/` — the planner itself: the trigger. A change here must prove the gate still fires.
///     ⚠ This entry used to name the Python planner's own path, and it MOVED with the code rather
///     than being dropped: a table entry naming a deleted file matches nothing, forever, and looks
///     exactly like a table entry that is working.
///
/// EXCLUDED, with the reason (each was checked, not assumed):
///
///   * `rustfmt.toml` — formatting; cannot reach codegen.
///   * `deny.toml` — read by `cargo deny`, never by rustc.
///   * `justfile` — local recipes. CI derives its own plan and does not read it.
///   * `scripts/*` — `ci_feature_suite.sh` / `ci_lockfile_gate.sh` drive OTHER jobs.
///   * `.github/*` (other) — the other workflows (release/deny/jforex-bridge/ctrader-proto), plus
///     the `rust-ci-setup` composite action, which the `plan` job deliberately does NOT use.
///   * `docs/`, `CLAUDE.md`, `CODEOWNERS` — prose.
pub const LATENCY_GLOBAL_PREFIXES: &[&str] =
    &["Cargo.lock", "rust-toolchain", ".cargo/", ".github/workflows/ci.yml", "xtask/"];

/// Paths that invalidate the narrow selection entirely and escalate to the FULL crate matrix.
///
/// ⚠ `settings/` is here because it is a RUNTIME input to every test, not because it is
/// configuration. The repo root is the workspace root, so `vike_model::state_path`'s
/// `project_settings_dir` walk — started from any working directory inside the checkout — answers
/// with `<repo>/settings/`. A committed `policy.toml` therefore changes what a test that does NOT
/// build its own `TempDir` project SEES, in any crate, with no crate of its own to select by.
/// MEASURED: the commit that first tracked those four TOMLs changed zero crates, so `plan` selected
/// nothing and `test`/`features`/`hist-datafusion`/`app-check`/`latency` all SKIPPED — a green tick
/// in 21 seconds over a change that alters the resolved policy for the whole workspace. It is
/// deliberately NOT in [`LATENCY_GLOBAL_PREFIXES`]: a policy ceiling is read at mount, never by the
/// vike-core fold, and escalating there would buy an ~11-minute pinned-core run for editing
/// `max_notional_per_order`.
///
/// ⚠ `xtask/` is here for a SELF-REFERENCE reason the other entries do not have, and it is the
/// entry most easily argued away. The planner computes the plan for its OWN pull request out of the
/// changed code, so a bug that makes the selection too narrow makes its own PR too narrow as well —
/// it would plan `xtask` plus one dev hop, go green, and conceal itself. While the planner lived
/// under `scripts/` it inherited this escalation from the `scripts/` row and nobody had to notice;
/// moving it to a workspace member is what made the row need saying out loud. The cost is the full
/// matrix on planner PRs, which is what those PRs already paid.
pub const GLOBAL_PREFIXES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    ".cargo/",
    ".github/",
    "rustfmt.toml",
    "deny.toml",
    "justfile",
    "scripts/",
    "settings/",
    "xtask/",
];

/// The crate owning the settings-registry gate (`crates/vike-ops/tests/settings_registry.rs`). That
/// gate walks EVERY `.rs` file in the workspace, so an undeclared `env::var` added ANYWHERE is its
/// business — which makes selecting it by reverse-dep closure unsound. See
/// [`super::gate_crates_for`].
pub const SETTINGS_GATE_CRATE: &str = "vike-ops";

/// The crate owning the STORE-KIND gate (`crates/vike-data/tests/store_kind_gate.rs`). Same
/// unsoundness as [`SETTINGS_GATE_CRATE`], one crate over: that gate checks each declared
/// commit-key template verbatim against the PRODUCER file that builds it, and those producers live
/// in other crates. Most of them reach vike-data's job anyway — vike-backfill is in [`HIST_CRATES`]
/// itself, vike-backtest reaches vike-datahub, `crates/bridges/deribit` reaches vike-backfill — but
/// `crates/vike-ops/src/journal_mat.rs` reaches NONE of them: vike-ops' dependents are
/// vike-app-core / vike-cli / vike-recorder / vike-tradehub. So renaming a `journal_mat` commit key
/// merges green and reddens the next unrelated vike-data PR, which is exactly the "blames the wrong
/// change" failure this whole mechanism exists to prevent.
///
/// ⚠ Selected into the TEST LANE (`ordered`) only, NEVER into `affected`: vike-data is in
/// [`HIST_CRATES`], so adding it there would fire the DataFusion `hist` job on every `.rs` PR in the
/// repo. The gate is a text scan that runs in the default lane; it needs the lane, not that job.
pub const STORE_KIND_GATE_CRATE: &str = "vike-data";

/// The crate owning the PROSE gates — `crates/vike-ops/tests/`'s `citation_gate.rs`,
/// `one_authority_gate.rs`, `docs_constants_gate.rs`, `unrun_command_gate.rs` and
/// `decision_index_gate.rs`. Same crate as [`SETTINGS_GATE_CRATE`] and the same unsoundness for the
/// same reason, on a different input: those five walk MARKDOWN, which belongs to no workspace
/// member, so the reverse-dep closure cannot reach them either.
pub const DOC_GATE_CRATE: &str = "vike-ops";

/// File suffixes that are an input to the prose gates.
///
/// MEASURED on the real graph (2026-08-09), before [`super::gate_crates_for`] existed — every one of
/// these produced `any=false`, i.e. the `test` job SKIPPED and not one prose gate ran:
/// `["docs/decisions/0014-….md", "docs/decisions/README.md"]` -> 0 crates (that PR's own class);
/// `["CLAUDE.md"]` -> 0; `["README.md"]` -> 0; `["content/learn/rsi.md"]` -> 0; and
/// `["crates/vike-cli/CLAUDE.md"]` -> 1 crate, which is vike-CLI, not vike-ops — so still no gate.
///
/// That is the `settings/` blind spot wearing a different costume. It is NOT the same cost, though,
/// so it deliberately does NOT escalate to the full matrix the way `settings/` does: `settings/` is
/// a RUNTIME input every test can read, while a markdown file is read only by these five gates.
/// Force-adding the ONE crate that owns them buys the coverage for one lane instead of the whole
/// roster plus every feature suite on every typo fix. MEASURED after: a docs-only PR now plans
/// `-p vike-ops` plus the two feature suites whose trigger sets happen to name vike-ops
/// (`tradehub-sinks`, `light-consumers`), with `hist`/`core`/`app` all still false. Those two are
/// the accepted overspend — the alternative is a second force-add mechanism that feeds `ordered`
/// without feeding `affected`, and one selection rule beats two.
///
/// `.md` is the suffix because `citation_gate.rs`'s `scanned_docs` reads EVERY tracked `.md` outside
/// its own `DOC_SCAN_EXCLUDED`, which is why `content/`'s pages count too: `content/README.md`
/// cites `crates/vike-indicators/src/indicators/` and that citation is checked.
///
/// ⚠ The DECLARED residual: rule 1 of `citation_gate.rs` resolves a cited path against the whole
/// file INDEX, so DELETING or renaming any file under a cited root (`deploy/`, `fixtures/`,
/// `assets/`, `bench/`, `content/tools/`) can rot a citation without touching a `.rs` or a `.md`.
/// Covering that means treating every path in the repo as an input, i.e. the full matrix — the cost
/// this narrow rule exists to avoid. Stated here rather than implied away.
pub const DOC_GATE_INPUT_SUFFIXES: &[&str] = &[".md"];

/// Path prefixes that are an input to the prose gates. `docs/` is a PREFIX rather than a suffix
/// match because the tree is not all markdown (`docs/ops/*.toml` are live run profiles), and a
/// non-`.md` file there is still cited by the pages around it.
///
/// ⚠ `deploy/` is here for the SAME reason and closes a MEASURED hole. [`DOC_GATE_CRATE`] is
/// `vike-ops`, which owns every gate that reads that directory — `deploy_layout_gate.rs`,
/// `deploy_tool_root_gate.rs`, `graceful_stop_pin.rs` and `container_image_gate.rs` — and `deploy/`
/// belongs to no workspace member, so before this row a PR touching only `deploy/` closed nothing,
/// `any` was "false", the `test` job SKIPPED and not one of those four gates ran. The failure that
/// exposed it: a `deploy/docker/entrypoint.sh` added on its own would merge green with the script
/// unclassified by `deploy_tool_root_gate.rs`'s `every_deploy_script_is_classified`, and then redden
/// the next unrelated PR that happened to select `vike-ops` — which is exactly the "blames the wrong
/// change" failure this whole mechanism exists to prevent.
///
/// It also partly closes the residual [`DOC_GATE_INPUT_SUFFIXES`] declares above: `citation_gate.rs`
/// resolves cited paths against the whole file index, and `deploy/` is one of the roots it names, so
/// deleting or renaming a file here can rot a citation without touching a `.rs` or a `.md`. This
/// makes that class SELECT the gate that would catch it.
///
/// Same cost shape as `docs/`, and the same argument for paying it: this force-adds ONE crate for
/// one lane rather than escalating to the full matrix the way `settings/` does, because nothing
/// under `deploy/` is a runtime input to an unrelated test.
pub const DOC_GATE_INPUT_PREFIXES: &[&str] = &["docs/", "deploy/"];

/// The crate owning the MCP registry manifest's drift gate
/// (`crates/vike-cli/src/cmd/mcp.rs`'s `the_registry_manifest_lists_every_tool_this_server_serves`,
/// which `include_str!`s the repo-root `server.json` and holds it equal to `tools_spec`).
///
/// ⚠ Third instance of the same unsoundness as [`SETTINGS_GATE_CRATE`], on a THIRD input class: the
/// manifest is a file at the REPOSITORY ROOT, and this is a virtual workspace — the root owns no
/// package, so [`super::graph::owner_of`] matches it to no crate, and a repo-root `.json` is in no
/// [`GLOBAL_PREFIXES`] entry and is neither `.md` nor under [`DOC_GATE_INPUT_PREFIXES`]. So a PR
/// editing ONLY `server.json` selected ZERO crates: `any` was "false" and the `test` job SKIPPED.
///
/// That left the gate firing in exactly one of its two directions. It caught `tools_spec` drifting
/// away from the manifest (a `.rs` change selects vike-cli), and missed the manifest drifting away
/// from `tools_spec` — a tool name reordered, a `version` hand-bumped, a `_vike.tools` row edited —
/// which is precisely the edit somebody makes when preparing a registry listing, and the direction
/// where the published listing is what ends up lying.
///
/// Same cost shape as [`DOC_GATE_CRATE`] and the same argument for paying it: force-add the ONE
/// crate that owns the gate rather than escalating to the full matrix the way [`GLOBAL_PREFIXES`]
/// does, because nothing under this path is a runtime input to an unrelated test.
pub const MANIFEST_GATE_CRATE: &str = "vike-cli";

/// The files that are an input to [`MANIFEST_GATE_CRATE`]'s gate. Exact paths, not a prefix: this
/// is one tracked file, and a prefix rule over the repository root would sweep every root-level
/// file into vike-cli's lane.
pub const MANIFEST_GATE_INPUTS: &[&str] = &["server.json"];

/// The GUI shell. Excluded from the test lane ([`EXCLUDE_FROM_CI`]), so its compile gate reads the
/// affected set directly.
pub const APP_CHECK_CRATE: &str = "vike-app";
