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
///     The trigger is the union of the four lanes' sets: `databento`/`tardis`/`vike-archive` pull
///     vike-bridge-core (the canonical credentials loader — vike-archive's own bin uses it for
///     `VIKE_ARCHIVE_API_KEY`). ⚠ Deleted: `poly-reparse` (docs/decisions/0094: no deployed process ever
///     produced its input) and `poly-ch-backtest` (2026-09-20: it pulled the vike-backtest harness onto a COLLECTOR).
///   * ⚠ `backfill-ibkr` is DELETED with vike-backfill's `ibkr` feature (docs/decisions/0094), measured
///     unused: no `venue=ibkr` series existed in any store.
///   * `polymarket-stack` — ONE lane for the polymarket mount stack (`vike-tradehub/polymarket`):
///     test+clippy over vike-tradehub with its feature, so its registry row for the bridge's
///     `PolymarketVenueMount`, its wired-market row, its moved feature-on tests and its LIVE
///     daemon arm compile while the shared tree builds once. ⚠ The key keeps its name although the
///     lane builds one package now: its `vike-mount/polymarket` and `vike-run/polymarket` rings went
///     with docs/decisions/0096 (amended 2026-09-29) and docs/decisions/0098.
///   * `telegram` — vike-tradehub's opt-in TELEGRAM control channel. A remote order-origination
///     path reachable from a third-party chat service is compiled OUT of a default build
///     (`#[cfg(feature = "telegram")]` on the module, on the mount and on both call sites), so a
///     default `cargo test -p vike-tradehub` compiles NONE of it and `tests/telegram_control.rs` is
///     `#![cfg]`-ed away to an empty binary. Without this suite the whole channel would be
///     un-type-checked, un-clippy-gated and never run — the way the `benches/engines.rs` `--bench`
///     bug survived. Kept its own lane: it must stay provably DataFusion-free. The trigger is the
///     crate itself, because the feature turns on no cross-crate optional dep — only the crate's own
///     `dep:ureq`, already in this graph transitively via vike-bridge-core.
///   * ⚠ `tradehub-sinks` is GONE with vike-tradehub's `record-feeds`/`materialize` sink features:
///     `docs/decisions/0084-only-the-datahub-touches-the-store.md` — the daemon no longer writes the store.
///   * `backtest-hist-replay` — ⚠ KEEPS ITS NAME although its `hist-replay` feature died in the
///     2026-09-27 feature collapse (`docs/decisions/0087`): what it gated is a DEFAULT
///     `cargo test -p vike-backtest` build now, which the roster lane's own nextest pass runs. What
///     THIS suite still proves is the DataFusion-free property that default build can no longer
///     prove alone: a single-package `cargo clippy -p vike-backtest` (trait-only, DataFusion-free —
///     the decouple gate) plus `--features datafusion-store` (the concrete `DataFusionHist`
///     bins/tests). Without it neither lane is clippy-gated with `vike-studio-core`'s dev-dependency
///     unification kept out of the picture, and the `datafusion-store` half is not run at all.
///   * ⚠ `alerting-standalone` is GONE (2026-09-28). It built vike-alerting ALONE because the roster
///     lane never compiled its vike-free build: vike-ops depended on it with a `core` feature (and
///     `workspace-env`), and resolver-2 unified both in. Both features and vike-ops' edge went
///     (2026-09-23 and 2026-09-25), so the crate has ONE configuration and the roster lane builds, lints and tests it — MEASURED on a
///     2026-09-28 verify-branch run, the roster's nextest ran all 52 of its tests and the lane re-ran
///     the same 52, compiling nothing new. Its `cargo tree` property (no `vike-*` crate among its
///     normal dependencies) is `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s `VIKE_FREE_CRATES`
///     now, which is stricter (it counts optional and target-specific edges too) and which
///     `just test` runs, where the tree check was a pipeline only CI could.
///   * `recorder-venues` — the recorder's venue rows, and the daemon that mounts them:
///     `-p vike-datahub --features record-polymarket,record-binance`. Ruling 10 merged the recorder
///     into the data server and decision 0092 moved its venue table there
///     (`crates/vike-datahub/src/recording.rs`), so the mount, the tick loop, the teardown and the
///     two venue rows compile under nothing else, since the roster lane takes that crate's DEFAULT
///     features. ⚠ It also ran `-p vike-recorder --features polymarket,binance` until 0092 deleted
///     those features: vike-recorder names no bridge now, so the roster lane compiles all of it.
///     vike-recorder stays in the trigger set because this build is still the only one that wires
///     its resolvers and `narrow` to a real venue row, and vike-polymarket/vike-binance because each
///     row is an ASSEMBLY over those crates' `RollingFamily`/catalog and their broker client.
///   * `light-consumers` — the default-features-OFF half of vike-bridge-core, plus the light crates
///     that take it that way. A WORKSPACE build cannot see this configuration: resolver-2 unifies
///     features, every other consumer takes `full`, so a module that forgets its
///     `#[cfg(feature = "full")]` compiles everywhere else CI looks. That let #1046 land an ungated
///     `leverage` module naming `vike_exec` one PR after #1042 made vike-exec optional — both
///     green, and `main` could not build `-p vike-cli` at all. The trigger set is the two crates
///     the arm BUILDS — vike-bridge-core with its defaults off, and vike-cli — and the lane is in
///     [`super::suite_rules::MANIFEST_CLOSURE_SUITES`]: the property is the shape of vike-cli's NORMAL dependency tree
///     (no DataFusion, no transport), so what moves it is a `Cargo.toml` edit anywhere below.
///     ⚠ `vike-ops` and `vike-secrets` were triggers until 2026-10-06 and the arm builds neither:
///     the `cargo check -p vike-ops --no-default-features` that justified the first went on
///     2026-09-26, so a source or test edit to either fired a lane that could not see it (#2543,
///     #2544). Their MANIFESTS still do, through vike-cli, which depends on both.
///   * `venue-catalog` — the `CatalogProvider` entries `vike_datahub::catalog::
///     real_catalog_table` names, behind `catalog-serve`. ⚠ It is a NARROW lane by design and the
///     narrowness is the interesting part: the verb, its bucket, its TTL memo, its credential fence
///     and its whole wire test are FEATURE-FREE and ride the roster lane on every PR — because the
///     verb writes no store, so `crates/vike-datahub/tests/venue_catalog.rs` needs neither
///     DataFusion nor a `#![cfg]`. What reaches no other lane is exactly the bridge-naming closures
///     and the `serve-datafusion` wiring that mounts them. Trigger set as `live-feeds`' PLUS
///     vike-dukascopy and vike-fxcm, whose bundled-static providers this table names and which no
///     other lane's `catalog-serve` build can reach — all of them optional edges contributing no
///     reverse edge.
///     `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`.
///   * `backfill-serve` — closes the one hole the backfill-on-demand verb's own feature left open
///     (split-plane REQ-9; `docs/decisions/0094-backfill-names-no-venue.md`). vike-datahub's
///     `backfill-serve` Cargo feature was already compiled by [`super::roster::HIST_CRATES`]'s `hist-datafusion`
///     job and by `multicall`'s `full`, and BOTH trigger sets are blind to the seven kline bridges
///     that feature turns on as optional deps — so renaming a bridge's own collector type (its
///     `BybitKlines`, its `BinanceFunding`, `vike_dukascopy::fetch_quotes_range`) passed PR CI and
///     broke `main`. This suite is a `check`+`clippy` compile witness ONLY, deliberately not a
///     second `cargo test` of the same DataFusion tree: the two lanes above already RUN it on every
///     vike-data/vike-backfill/vike-report/vike-datahub/multicall-tree PR, and a bridge-only PR
///     needs nothing more than the compile-time proof that its renamed symbol still resolves.
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
///     `crates/vike-backfill/src/vike_archive.rs`'s `#[cfg(windows)]` `read_at` — the `seek_read`
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
///     reaches it) and it triggered only `light-consumers` (and `multicall` since 2026-10-03), so
///     without this row a PR touching `crates/vike-cli/` alone would schedule every OTHER lane and
///     not the one added for it. It rides its own `cargo check` line inside the arm because that
///     line is deliberately not `--all-targets` (the crate's dev-deps are a second large tree the
///     release compiles none of).
///   * ⚠ vike-report's feature-OFF lane is GONE (2026-09-28) with its `journal` feature (the renderer
///     half moved into `vike-analytics`); its `cargo tree` property — a light renderer carries no heavy
///     closure — is `crates/vike-ops/tests/architecture/layer_gate.rs`'s tier-15 rule over vike-analytics now.
///   * `studio-standalone` — the Studio crates' DEFAULT builds, ALONE (split-plane I7:
///     vike-studio/vike-studio-core moved their `vike-data/hist-datafusion` enables to
///     [dev-dependencies], so a default Studio build is DataFusion-free). The roster lane can never
///     see that configuration — it builds the Studio crates in the same invocation as
///     vike-recorder, whose NORMAL dep enables the feature, and resolver-2 unifies it straight back
///     on (the `light-consumers` failure class wearing a different feature). The suite also asserts
///     the structural property itself via `cargo tree -e normal` — a compile-only check would go green
///     again the day the enable moved back onto the normal dep. The trigger is the two crates
///     themselves; anything BELOW them (vike-data, vike-backtest, vike-ai …) lands in the affected
///     set by reverse-dep closure and fires the suite without being named here.
///   * `bridges-feeds` — the FEEDS-ONLY half of every venue bridge carrying a feeds/exec seam.
///     SEVEN crates, in two groups the arm checks for different reasons. Group 1, the EIP-712
///     bridges (split-plane Phase-5 hardening): vike-hyperliquid / vike-aster with default
///     features off, and vike-polymarket under its `feeds` feature alone (its default is EMPTY) —
///     the market-data surface without the EIP-712 order signer. Group 2, the HMAC CEX bridges
///     (ruling 8 of the datahub market-data wire design): vike-binance / vike-bybit / vike-okx
///     with default features off — the same seam, so a data daemon compiles their keyless
///     market-data plane and none of the order plane — and vike-deribit, which got the same seam as
///     a docs/decisions/0094 follow-up. The roster lane cannot see either group — every consumer
///     but vike-datahub asks for their default-on `exec` (or polymarket's full `polymarket`), and
///     resolver-2 unifies it straight back in (`studio-standalone`'s argument wearing the signer
///     stack) — so a feed module that grew an exec-plane import would compile there. vike-datahub
///     is the one exception: it takes all seven feeds-only, so the daemon's own builds see these
///     configurations too — the other six through its `live-feeds` and `venue-catalog` lanes (five
///     of them through `backfill-serve` as well), deribit through `live-feeds` and `backfill-serve`,
///     the latter what the `hist-datafusion` job builds (`-p vike-datahub --features backfill-serve`)
///     — but as part of the daemon; this suite compiles each one ALONE, on that bridge's own change.
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
///   * `workspace-bins` — `cargo check --workspace --bins`: every `[[bin]]` compiled the way a
///     RELEASE compiles it, rather than the way `cargo test` does. It exists because
///     `.github/workflows/release.yml` used to do this incidentally — its host job compiled seven
///     per-tool binaries and threw six of them away on every tag — and the 3-binary release cut
///     deleted that build.
///
///     ⚠ It is NARROWER than it sounds and the arm says so at length: a `[[bin]]` defaults to
///     `test = true`, so the roster lane's `cargo nextest run` already compiles every ungated bin
///     of every roster crate. What only THIS lane sees is the non-test profile — no `cfg(test)`,
///     no dev-dependencies — where a bin leaning on a dev-dep compiles green under `cargo test`
///     and fails exactly where a release build fails; plus the members [`super::roster::EXCLUDE_FROM_CI`] keeps
///     out of the roster lane. ⚠ And `--bins` SKIPS a bin whose `required-features` are off, so it
///     covers neither `backtest`/`cheap_np_*` nor `vike-study` (the `backtest-hist-replay` and
///     `study-cli` arms do) nor ANY vike-backfill bin (all gated; the `backfill` arms do).
///
///     The trigger set is the seven crates whose bins the deleted release build proved, and it is
///     a heuristic rather than a roster — the lane compiles the whole workspace whichever crate
///     fired it, and anything below those seven reaches them by reverse-dep closure, the same
///     mechanism `studio-standalone`'s note argues from. vike-desktop and vike-backfill are
///     deliberately NOT triggers: vike-desktop's one bin is already compiled non-test by
///     `app-check`'s `cargo check -p vike-desktop --all-targets` on the same affected-set signal,
///     and every vike-backfill bin is `required-features`-gated so this lane compiles none of them.
///     ⚠ It is deliberately NOT in [`super::suite_rules::SUITE_GROUPS`]: a cold `--workspace` compiles vike-desktop's
///     eframe/wgpu closure, this lane's cost has never been measured, and every set membership
///     rule below requires combined work well under the long pole.
///
/// ⚠ **HOW A KEY IS MATCHED (2026-10-06).** A suite fires when a crate in its trigger list OWNS a
/// changed file — the DIRECT set — and NOT when something it depends on changed, with two
/// exceptions: [`super::suite_rules::CLOSURE_SUITES`] keep the whole reverse-dependency walk, and
/// [`super::suite_rules::MANIFEST_CLOSURE_SUITES`] keep it for an edit to a MANIFEST or BUILD SCRIPT only. Before that
/// date every suite matched the full closure: an edit in a crate 43 of 70 crates depend on
/// (vike-data) fired every lane, ibkr and png-export included, over a change they cannot observe.
/// A global file ([`super::escalation::GLOBAL_PREFIXES`]) or an unresolvable diff base still fires every suite. What
/// covers the dropped cases is the `test` job over the full closure, the closure suites, and the
/// release, which runs EVERY suite once before anything can publish (`release.yml`'s `features`).
/// `xtask/tests/ci_plan_gate/lane_directness.rs` pins all of it.
pub const FEATURE_SUITES: &[(&str, &[&str])] = &[
    ("polymarket", &["vike-polymarket"]),
    // The fxcm chain the shipped daemon resolves: vike-tradehub/fxcm → dep:vike-fxcm +
    // vike-fxcm/fxcm. vike-tradehub is named for the DIRECTION reason: it sits ABOVE vike-fxcm, so
    // an edit in it never reaches vike-fxcm's row — and its `#[cfg(feature = "fxcm")]` sites (the
    // registry row and the wired-market row) are exactly what this lane compiles and nothing else
    // does. ⚠ vike-mount and vike-run are no longer named: vike-mount's `fxcm` arm moved into the
    // bridge (docs/decisions/0096, amended 2026-09-29) and vike-run's `fxcm` marker went when the
    // wired markets moved up to vike-tradehub (docs/decisions/0098).
    ("fxcm", &["vike-fxcm", "vike-tradehub"]),
    // The ibkr chain: vike-tradehub/ibkr → dep:vike-ibkr + vike-ibkr/ibkr. vike-tradehub is named
    // for the DIRECTION reason `fxcm` records above: it sits ABOVE vike-ibkr, so an edit in it
    // never reaches vike-ibkr's row — and its `#[cfg(feature = "ibkr")]` sites run their tests in
    // this lane and nowhere else (the `multicall` lane compiles their library code under `full`).
    // ⚠ vike-mount and vike-run are no longer named: vike-mount's `ibkr` arm and arming row moved
    // into the bridge (docs/decisions/0096, amended 2026-09-29) and vike-run's `ibkr` marker went
    // when the wired markets moved up to vike-tradehub (docs/decisions/0098).
    // The vike-tradehub forward arrived in #1575; without it the headless daemon fell through to
    // paper in silence while the GUI shell (`vike-app` then; `vike-desktop` declares no `[features]`
    // table) could mount IBKR live.
    ("ibkr", &["vike-ibkr", "vike-tradehub"]),
    ("socks-proxy", &["vike-bridge-core"]),
    // The `vike-study` bin (`--features study-cli`, off by default so `studio-standalone`'s
    // DataFusion-free structural check keeps holding) — the only lane that type-checks it.
    ("study-cli", &["vike-studio-core"]),
    // The `vike-backend` multicall dispatcher (`--features full`): every tool row at once, the
    // configuration the release builds. Triggered by the dispatcher itself AND by every crate it
    // dispatches into — a change to any tool's `run` signature breaks this build and nothing else.
    // ⚠ `vike-backend` is a PACKAGE name (these rows are matched against cargo-metadata member
    // names); its directory is still `crates/vike/`, which is what the gates key on.
    // ⚠ `vike-recorder` IS STILL HERE, and dropping it was a real hole for one revision. Ruling 10
    // retired that crate's `[[bin]]`, so the multicall no longer dispatches into it DIRECTLY — but
    // `full` still reaches it, one edge further out: `vike/Cargo.toml`'s `vike-datahub` feature
    // forwards `vike-datahub/record-polymarket` + `record-binance`, and both imply `record`, which
    // enables `dep:vike-recorder`. So a change to that crate still changes what this lane compiles,
    // and the trigger has to exist wherever the EDGE does. The rule these rows encode is "every
    // crate this build compiles", not "every crate the dispatcher names".
    // ⚠ `vike-tradehub` and `vike-cli` were MISSING until 2026-10-03, a hole rather than a
    // judgment: `crates/vike/src/main.rs` calls `vike_tradehub::tradehub_cli::run`,
    // `venues_cli::run`, `catalog_cli::run` and `vike_cli::run`, each behind an OPTIONAL dep (no
    // reverse edge in the default graph), and neither crate reaches any trigger above by a normal
    // edge — so a PR changing one of those signatures, confined to that crate, planned no
    // `multicall` leg — free to merge green and break that build (and the release's
    // `--features full`) on `main`.
    // It was masked whenever such a PR also touched a global file: #2414 changed both crates with a
    // `scripts/` file and so ran the full matrix, which narrowing `scripts/` would have turned into
    // a plan with no `multicall` leg (replayed through the real planner on a lane).
    // `xtask/tests/ci_plan_gate.rs` now derives the set from the manifest: every
    // optional `vike-*` dependency of `vike-backend` is a trigger here unless a named edge from a
    // trigger reaches it (vike-model and vike-strategy-builder, the two it accepts — read the test
    // for which edge each is).
    (
        "multicall",
        &[
            "vike-backend",
            "vike-studio-core",
            "vike-report",
            "vike-backtest",
            "vike-datahub",
            "vike-recorder",
            "vike-tradehub",
            "vike-cli",
        ],
    ),
    // ⚠ `vike-backtest` LEFT this row 2026-09-28 (docs/decisions/0094): MEASURED with
    // `cargo tree -p vike-backfill -e normal,dev --all-features --prefix none`, the vike-* crates
    // vike-backfill's whole tree reaches are vike-backfill, vike-bridge-core, vike-data, vike-exec,
    // vike-log, vike-marketdata, vike-model and vike-secrets — no vike-backtest (it likely left with
    // `poly-ch-backtest` and its two bins, 2026-09-20) — so naming it fires the suite on a PR this
    // crate cannot see change. Only vike-bridge-core is named beside the crate itself: it is the one
    // OPTIONAL cross-crate dep the suite's four features pull in (databento/tardis/vikedata/
    // vike-archive all name `dep:vike-bridge-core`), so this table's own rule above requires naming it.
    ("backfill", &["vike-backfill", "vike-bridge-core"]),
    // The polymarket chain: vike-tradehub/polymarket → dep:vike-polymarket +
    // vike-polymarket/polymarket. vike-polymarket stays a trigger because it is an OPTIONAL
    // dependency of vike-tradehub, which the default-build reverse-dep closure does not follow.
    // ⚠ vike-mount and vike-run are no longer named: vike-mount's `polymarket` arm and arming row
    // moved into the bridge (docs/decisions/0096, amended 2026-09-29) and vike-run's `polymarket`
    // marker went when the wired markets moved up to vike-tradehub (docs/decisions/0098), so every
    // `#[cfg(feature = "polymarket")]` site this lane compiles is vike-tradehub's.
    ("polymarket-stack", &["vike-tradehub", "vike-polymarket"]),
    ("telegram", &["vike-tradehub"]),
    ("backtest-hist-replay", &["vike-backtest"]),
    ("recorder-venues", &["vike-recorder", "vike-datahub", "vike-polymarket", "vike-binance"]),
    ("light-consumers", &["vike-bridge-core", "vike-cli"]),
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
    // The LIVE MARKET-DATA plane. ⚠ EVERY CRATE this plane links, and the bridges are not
    // decoration (vike-deribit joined 2026-10-04): this
    // suite turns on OPTIONAL cross-crate deps, and this table's own rule above says such a suite
    // must list BOTH sides, because `xtask::ci::graph` builds from `cargo metadata`'s
    // DEFAULT-feature `resolve.nodes[].deps` and an off-by-default optional dep contributes NO
    // reverse edge. Without the bridge rows a PR touching `crates/bridges/bybit/` alone would
    // schedule every lane except the one that compiles its feeds half into this daemon.
    (
        "live-feeds",
        &[
            "vike-datahub",
            "vike-binance",
            "vike-bybit",
            "vike-okx",
            "vike-aster",
            "vike-hyperliquid",
            "vike-polymarket",
            "vike-deribit",
        ],
    ),
    // The VENUE-CATALOG provider table
    // (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`). ⚠ The
    // crates of `live-feeds` above (all but vike-deribit, which has no catalog provider in this table),
    // for exactly the same reason — `catalog-serve` turns on
    // the SAME off-by-default optional bridge edges that plane does, and an optional dep
    // contributes no reverse edge to `xtask::ci::graph`. Without the bridge rows a PR touching
    // `crates/bridges/okx/src/catalog.rs` alone — the single likeliest edit to break this lane —
    // would schedule every lane except the one that compiles that provider.
    //
    // ⚠ PLUS TWO the market-data plane does not carry: vike-dukascopy and vike-fxcm, whose bundled
    // static catalogs joined the table without joining any feed. They matter MORE here than the
    // six, not less: those six also trigger `live-feeds`, so a PR touching one of them compiles
    // SOMETHING either way, while an edit to `crates/bridges/dukascopy/src/catalog.rs` reaches a
    // `catalog-serve` build through this row and no other.
    (
        "venue-catalog",
        &[
            "vike-datahub",
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
    // The BACKFILL-ON-DEMAND COLLECTOR TABLE's bridge side (split-plane REQ-9;
    // docs/decisions/0094-backfill-names-no-venue.md). Same rule as `live-feeds`/`venue-catalog`
    // above, on the SAME crate's feature: `backfill-serve` turns on vike-backfill plus seven kline
    // bridges as OPTIONAL deps (`crates/vike-datahub/Cargo.toml`), so — absent this row — a PR
    // touching only one of them is invisible to `xtask::ci::graph`'s DEFAULT-feature closure.
    // vike-data/vike-backfill/vike-datahub are named too for the same "every crate this build
    // compiles" reason `multicall`'s own note gives, not because their trigger would otherwise be
    // missed (they already fire `hist-datafusion` via [`HIST_CRATES`]). vike-dukascopy also sits in
    // `venue-catalog`'s trigger set above, for an unrelated feature (its bundled static catalog);
    // here it is one of the seven kline sources instead. vike-oanda joined with the CREDENTIALED
    // lane (docs/decisions/0097): the one hand-written `BackfillLane::CredentialedKlines` row names
    // `vike_oanda::OandaKlines` and its token-provider types, which nothing else in this lane's
    // trigger set can see renamed.
    (
        "backfill-serve",
        &[
            "vike-datahub",
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
    // ⚠ `workspace-bins` is LAST deliberately: this array's ORDER is the matrix order (see the
    // note at the top of this doc), so a key appended here cannot renumber an existing leg.
    // The set is a HEURISTIC, not a roster (any enumeration of bin-owning crates here would be
    // exactly the partial roster this file exists to stop writing down); it and its two MEASURED
    // omissions are argued in this key's paragraph above.
    (
        "workspace-bins",
        &[
            "vike-tradehub",
            "vike-cli",
            "vike-backtest",
            "vike-datahub",
            "vike-studio-core",
            "vike-report",
        ],
    ),
];
