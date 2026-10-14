//! The feature-gated suites: each lane KEY and the trigger crates that fire it.

/// Feature-gated suites: run a suite iff ANY crate it compiles is in the affected set.
///
/// ⚠ **ORDER IS PART OF THE OUTPUT.** `suites` is emitted as a JSON array in THIS order and
/// `.github/workflows/ci.yml` turns it into a matrix, so reordering these rows reorders the matrix
/// legs.
///
/// ⚠ **A suite that turns on an OPTIONAL cross-crate dep must list BOTH crates**: an optional dep
/// is not a default-build reverse-dep edge, so a change to the PULLED crate would otherwise be
/// missed by the narrow selection. Every multi-crate row below is that rule being applied.
///
/// Per-row notes, each of which is why the lane exists rather than what it runs
/// (`scripts/ci_feature_suite.sh` is the authority for the invocations):
///
///   * `socks-proxy` — the optional SOCKS5 WS egress (`ws_proxy.rs`): its dial arm and the `socks5h`
///     wire test compile only with `--features socks-proxy`, and only vike-polymarket's own
///     off-by-default feature enables it, so — unlike `eip712`/`capture` — the default lane never does.
///     ⚠ There are deliberately NO `eip712`/`capture` suites: both gate UNIT tests in vike-bridge-core's
///     own lib (`src/eip712.rs` / `src/capture.rs`; nothing under `tests/` is feature-gated), and the
///     default multi-package lane compiles and runs them — vike-aster's NORMAL dep enables
///     `vike-bridge-core/eip712`, vike-binance/vike-bybit's dev-deps enable `capture`, and resolver 2
///     unifies features across packages built together into bridge-core's one lib build.
///   * `backfill` — ONE union-features lane over vike-backfill's four independent tool features
///     (`databento` + `tardis` + `vike-archive`; the merge rationale is in `scripts/ci_feature_suite.sh`).
///     The trigger is vike-backfill alone: the features pull no `vike-*` crate (the bins load their
///     key through `vike_secrets`, a plain dependency; they pulled vike-bridge-core for it until
///     2026-10-11). ⚠ Deleted: `poly-reparse` (docs/decisions/0094: no deployed process ever
///     produced its input) and `poly-ch-backtest` (2026-09-20: it pulled the vike-backtest harness onto a COLLECTOR).
///   * ⚠ `backfill-ibkr` is DELETED with vike-backfill's `ibkr` feature (docs/decisions/0094), measured
///     unused: no `venue=ibkr` series existed in any store.
///   * ⚠ `polymarket`, `fxcm`, `ibkr`, `polymarket-stack`, `telegram`, `multicall` and `workspace-bins`
///     are GONE (2026-10-11,
///     `docs/decisions/0132-the-feature-suites-fold-into-one-data-job-and-one-trading-job.md`): each
///     built one tradehub feature (or the venue crate under it, or the multicall under `full`, or every
///     bin), and `ci.yml`'s `tradehub-all` job builds them ONCE, in the shipped binary's resolution,
///     with `cargo check --workspace --bins` and vike-ibkr's `ibkr test-support` configuration as lines
///     of their own. Their trigger crates are its trigger crates (`super::roster::TRADEHUB_ALL_CRATES`,
///     and `super::roster::TRADEHUB_ALL_CLOSURE_CRATES` for the two that kept the closure).
///   * ⚠ `tradehub-sinks` is GONE with vike-tradehub's `record-feeds`/`materialize` sink features:
///     `docs/decisions/0084-only-the-datahub-touches-the-store.md` — the daemon no longer writes the store.
///   * `backtest-hist-replay-tests` — the store-backed and process-spawning tests of vike-backtesthub (the backtesthub daemon and its CLI) that OPEN a
///     `DataFusionHist` or spawn the `backtesthub` bin (`cargo test -p vike-backtesthub`; it ran `-p vike-backtest --features datafusion-store` until that feature died with the move), the bulk of what the
///     `backtest-hist-replay` suite ran until it was SPLIT (2026-10-11): this key is in
///     `suite_rules.rs`'s `POST_MERGE_SUITES` (`push`, `CI_FULL=1`, `workflow_dispatch` and the release,
///     never a pull request). ⚠ The clippy half, the key `backtest-hist-replay`, is GONE as a suite since
///     decision 0132: its two lines (the single-package `cargo clippy -p vike-backtest` of the feature-free harness library,
///     and the
///     `cargo clippy -p vike-backtesthub` lint that replaced the `--features datafusion-store` one) are lines of `ci.yml`'s `datahub-all` job, each
///     its own invocation, fired by a direct edit of `vike-backtest` or `vike-backtesthub`
///     (`super::roster::DATAHUB_ALL_CRATES`). `xtask/tests/ci_plan_gate/hist_replay_split.rs` holds both.
///   * ⚠ `alerting-standalone` is GONE (2026-09-28). It built vike-alerting ALONE because the roster
///     lane never compiled its vike-free build: vike-ops depended on it with a `core` feature (and
///     `workspace-env`), and resolver-2 unified both in. Both features and vike-ops' edge went
///     (2026-09-23 and 2026-09-25), so the crate has ONE configuration and the roster lane builds, lints and tests it — MEASURED on a
///     2026-09-28 verify-branch run, the roster's nextest ran all 52 of its tests and the lane re-ran
///     the same 52, compiling nothing new. Its `cargo tree` property (no `vike-*` crate among its
///     normal dependencies) is `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s `VIKE_FREE_CRATES`
///     now, which is stricter (it counts optional and target-specific edges too) and which
///     `just test` runs, where the tree check was a pipeline only CI could.
///   * ⚠ `recorder-venues`, `live-feeds`, `venue-catalog` and `backfill-serve` are GONE (2026-10-11,
///     `docs/decisions/0131-the-datahub-is-verified-by-its-default-build-and-one-all-features-build.md`):
///     each built vike-datahub with one feature group, and `ci.yml`'s `datahub-all` job builds it ONCE
///     with every feature the shipped binary turns on. Their trigger crates are its trigger crates
///     (`super::roster::DATAHUB_ALL_CRATES`, `super::roster::DATAHUB_ALL_DEPENDENCY_CRATES`) and their
///     `cargo tree` guards are its last step. Owner-accepted loss: no per-PR build proves a feature
///     group compiles ALONE; `.github/workflows/feature-matrix.yml`'s `cargo hack` does, on demand.
///   * `light-consumers` — vike-cli built ALONE, in its shipped configuration. A WORKSPACE build
///     cannot see it: resolver-2 unifies every other consumer's features into vike-cli's
///     dependencies. That let #1046 land an ungated `leverage` module naming `vike_exec` one PR after
///     #1042 made vike-exec optional under vike-bridge-core's `full` — both green, and `main` could
///     not build `-p vike-cli` at all. The trigger set is the one crate the arm BUILDS, vike-cli, and
///     the lane is in [`super::suite_rules::MANIFEST_CLOSURE_SUITES`]: the property is the shape of
///     vike-cli's NORMAL dependency tree (no DataFusion, no transport, no runtime), so what moves it
///     is a `Cargo.toml` edit anywhere below.
///     ⚠ `vike-bridge-core` was a trigger until docs/decisions/0135, with two arm lines that
///     checked and documented it with its defaults off (the half vike-cli linked for
///     `credentials`). That half moved into `vike-secrets`, vike-cli dropped the edge and `full` was
///     deleted, so the arm builds no bridge-core and a bridge-core edit cannot reach it.
///     ⚠ `vike-ops` and `vike-secrets` were triggers until 2026-10-06 and the arm builds neither:
///     the `cargo check -p vike-ops --no-default-features` that justified the first went on
///     2026-09-26, so a source or test edit to either fired a lane that could not see it (#2543,
///     #2544). Their MANIFESTS still do, through vike-cli, which depends on both.
///   * `png-export` — the two offscreen-render harnesses behind `png-export` (vike-chart's
///     `examples/export_png.rs` and vike-studio's `studio_shot`). Both are `required-features`-gated
///     and off by default, so NOTHING compiled them before this suite — not the roster lane, not
///     `app-check`, not any other feature lane. The drift that proves it: the two crates' pollster
///     pins diverged (1.0 vs 0.4) and a dependabot bump on exactly that dependency went green,
///     because CI never built either target (#1023 -> #1068). vike-ui-theme joined as the third
///     trigger with its component gallery (`crates/vike-ui-theme/examples/gallery.rs`): a change
///     under `examples/` selects only its own crate, and vike-ui-theme sits below the other two, so
///     no walk reaches it.
///   * `windows-cross` — the WINDOWS compile witness (`cargo check --target
///     x86_64-pc-windows-gnu`). Every workflow here runs on the self-hosted LINUX boxes, so this is
///     the only job in CI that compiles anything for Windows at all.
///
///     It began as the COMPLEMENT of `just windows-check`: that recipe builds `ci_crates`, which
///     excludes vike-desktop and vike-backfill (they are in [`super::roster::EXCLUDE_FROM_CI`], so the derived
///     roster cannot name them either), and the result was that
///     `crates/vike-backfill/src/vike_archive/readers.rs`'s `#[cfg(windows)]` `read_at` — the `seek_read`
///     arm that fixed a real `--jobs 4` read-corruption — was compiled by no CI job and no local
///     recipe, on any box. Mutation-proved: a bogus argument to `seek_read` is invisible to every
///     Linux check and fails E0425 under a Windows target. vike-desktop sits at the top of the dep
///     graph so this fires on nearly every code PR — the same shape as `app-check`, and affordable
///     for the same reason: measured 3.1s warm, 10.8s with the cross `target/` wiped but sccache
///     warm, ~67s only on a runner whose sccache has never seen it.
///
///     ⚠ vike-tradehub joins it as a DELIBERATE overlap with `just windows-check` (it IS in the
///     derived roster, so that recipe already checks it natively): `windows-check` runs on ONE box,
///     by hand, when somebody remembers, and vike-tradehub's `#[cfg(windows)]` background-hosting
///     surface (split-plane B10's console-ctrl stop, I13's stop-file arm) is Windows-only code in the
///     binary that signs real orders — the `vike_archive` hole, one manual step removed. It rides the
///     SAME invocation as the other two, so it shares their already compiled dependency tree.
///
///     ⚠ vike-cli is a FOURTH admission with a fourth argument: a RELEASE ASSET is cross-built from
///     it. `.github/workflows/release.yml`'s `windows` job builds `vike-cli.exe` beside the two GUI
///     assets, and a tag is the worst place to discover that a target does not build, so this lane
///     rehearses the release's own invocation. ⚠ **It must be a TRIGGER, not only a line in the
///     arm**: `vike-cli` is a LEAF binary (nothing depends on it, so reverse-dep closure never
///     reaches it) and it triggers only `light-consumers` here (and the `tradehub-all` job, which took over
///     the `multicall` suite's row for it in decision 0132), so
///     without this row a PR touching `crates/vike-cli/` alone would schedule every OTHER lane and
///     not the one added for it. It rides its own `cargo check` line inside the arm because that
///     line is deliberately not `--all-targets` (the crate's dev-deps are a second large tree the
///     release compiles none of).
///   * `studio-standalone` — the Studio crates' DEFAULT builds, ALONE (split-plane I7:
///     vike-studio/vike-studio-core take the DataFusion backend, `vike-store`, as a
///     [dev-dependencies] edge only, so a default Studio build is DataFusion-free). The roster lane can
///     never see that configuration — it builds the Studio crates in the same invocation as every
///     other crate, and resolver-2 unifies the other consumers' features into theirs (the
///     `light-consumers` failure class). The suite also asserts the structural property itself via
///     `cargo tree -e normal` — a compile-only check would go green again the day the edge moved back
///     onto a normal dependency. The trigger is the two crates themselves; anything BELOW them
///     (vike-data, vike-backtest, vike-ai …) lands in the affected set by reverse-dep closure and
///     fires the suite without being named here.
///   * `bridges-feeds` — the FEEDS-ONLY half of every venue bridge carrying a feeds/exec seam.
///     SEVEN crates, in two groups the arm checks for different reasons. Group 1, the EIP-712
///     bridges (split-plane Phase-5 hardening): vike-hyperliquid / vike-aster with default
///     features off, and vike-polymarket under its `feeds` feature alone (its default is EMPTY) —
///     the market-data surface without the EIP-712 order signer. Group 2, the HMAC CEX bridges
///     (ruling 8 of the datahub market-data wire design): vike-binance / vike-bybit / vike-okx
///     with default features off — the same seam, so a data daemon compiles their keyless
///     market-data plane and none of the order plane — and vike-deribit, which got the same seam as
///     a docs/decisions/0094 follow-up. The roster lane cannot see either group — every consumer
///     but vike-gatewayhub asks for their default-on `exec` (or polymarket's full `polymarket`), and
///     resolver-2 unifies it straight back in (`studio-standalone`'s argument wearing the signer
///     stack) — so a feed module that grew an exec-plane import would compile there. vike-gatewayhub
///     is the one exception (decision 0164: its `data` feature took the venue calls over from the datahub,
///     which took the same seven feeds-only): it takes all seven feeds-only, so the daemon's own build
///     (`ci.yml`'s `datahub-all` job, its `-p vike-gatewayhub --features data` lines) sees these
///     configurations too — but as part of the daemon; this suite compiles each one ALONE, on that
///     bridge's own change.
///     It also asserts structural properties via `cargo tree -e normal` — ⚠ but only GROUP 1 gets
///     the crate-name form (no EIP-712 signer crate in its three feeds trees). Group 2 gets NO
///     crate-name check, deliberately: those three sign with vike-bridge-core's HMAC signer, which
///     rides that crate's `full` feature that the feeds half needs anyway, so their default and feeds
///     trees are IDENTICAL and such a grep could never fail. Deribit's two trees carry the same crate
///     SET for its own reason — it signs nothing, and the one crate only its `exec` names
///     (tungstenite) rides that same `full`. Their one structural claim is a FEATURE-resolution grep
///     over vike-aster's feeds tree — it must not resolve vike-binance with `exec` — which is why
///     aster triggers this suite for group 2's sake as well as its own. The trigger is the seven
///     crates themselves; vike-bridge-core (whose `eip712` feature is the stack being kept out) sits
///     below all of them and lands in the affected set by reverse-dep closure.
///
/// ⚠ **HOW A KEY IS MATCHED (2026-10-06).** A suite fires when a crate in its trigger list OWNS a
/// changed file — the DIRECT set — and NOT when something it depends on changed, with one exception:
/// [`super::suite_rules::MANIFEST_CLOSURE_SUITES`] keep the walk for an edit to a MANIFEST or BUILD
/// SCRIPT only. Before that date every suite matched the full closure: an edit in a crate 43 of 70
/// crates depend on (vike-data) fired every lane, ibkr and png-export included, over a change they
/// cannot observe. (Two suites kept the whole walk, `multicall` and `workspace-bins`, the catch-alls
/// that compile the whole stack from its top; they are lines of `ci.yml`'s `tradehub-all` job since
/// decision 0132, and that job keeps their walk: `super::roster::TRADEHUB_ALL_CLOSURE_CRATES`.) A
/// global file ([`super::escalation::GLOBAL_PREFIXES`]) or an unresolvable diff base still fires every
/// suite; an edit of `scripts/ci_feature_suite.sh` fires the suite whose `case` arm it moved, or every
/// suite for an edit outside the arms (decision 0142, [`super::escalation::SUITE_SCRIPT`]). What covers the dropped cases is the `test` job over the full closure, the `tradehub-all`
/// job's closure, and the release, which runs EVERY suite once before anything can publish
/// (`release.yml`'s `features`). `xtask/tests/ci_plan_gate/lane_directness.rs` pins all of it.
pub const FEATURE_SUITES: &[(&str, &[&str])] = &[
    ("socks-proxy", &["vike-bridge-core"]),
    // The `vike-study` bin (`--features study-cli`, off by default so `studio-standalone`'s
    // DataFusion-free structural check keeps holding) — the only lane that type-checks it.
    ("study-cli", &["vike-studio-core"]),
    // ⚠ `vike-backtest` LEFT this row 2026-09-28 (docs/decisions/0094): MEASURED with
    // `cargo tree -p vike-backfill -e normal,dev --all-features --prefix none`, the vike-* crates
    // vike-backfill's whole tree reaches are vike-backfill, vike-bridge-core, vike-data, vike-exec,
    // vike-log, vike-marketdata, vike-model, vike-secrets and vike-venues — no vike-backtest (it likely left with
    // `poly-ch-backtest` and its two bins, 2026-09-20) — so naming it fires the suite on a PR this
    // crate cannot see change. ⚠ `vike-bridge-core` was named beside the crate itself until
    // 2026-10-11: it was the one OPTIONAL cross-crate dep the suite's four features pulled in
    // (databento/tardis/vikedata/vike-archive all named `dep:vike-bridge-core`). The vendor bins load
    // their key through `vike_secrets` now and the features pull no `vike-*` crate, so nothing is
    // optional here and a bridge-core PR no longer changes what this lane compiles.
    ("backfill", &["vike-backfill"]),
    // The test run of vike-backtesthub's store-backed and process-spawning tests (a post-merge suite, `suite_rules.rs`'s
    // `POST_MERGE_SUITES`). Its clippy half, once the `backtest-hist-replay` key, is two lines of `ci.yml`'s
    // `datahub-all` job since decision 0132; `xtask/tests/ci_plan_gate/hist_replay_split.rs` holds both.
    ("backtest-hist-replay-tests", &["vike-backtesthub"]),
    ("light-consumers", &["vike-cli"]),
    ("png-export", &["vike-chart", "vike-studio", "vike-ui-theme", "vike-panels"]),
    ("windows-cross", &["vike-desktop", "vike-backfill", "vike-tradehub", "vike-cli"]),
    ("studio-standalone", &["vike-studio", "vike-studio-core"]),
    (
        "bridges-feeds",
        &[
            "vike-hyperliquid",
            "vike-aster",
            "vike-polymarket",
            "vike-binance",
            "vike-bybit",
            "vike-okx",
            "vike-deribit",
        ],
    ),
];
