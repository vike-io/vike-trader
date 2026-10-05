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
///   * `vike-desktop` — the egui/wgpu GUI binary: out of the shared nextest+clippy roster, because
///     it drags the whole eframe/wgpu/winit tree into the the CI box `test` job's SHARED cache, which no
///     other roster crate builds against. ⚠ This argument was re-authored when the crate's `fat`
///     feature was deleted: the exclusion used to rest on "its `fat` feature resolution matches no
///     other lane's cache", and the crate now has ONE configuration and no features at all, so the
///     cost is the dependency tree rather than a second resolution of it. NOT uncovered — `ci.yml`'s
///     `app-check` job checks it, lints it and runs its unit tests, gated off the `app`
///     output ([`APP_CHECK_CRATE`]).
///   * `vike-backfill` — not part of the default roster. The `ci.yml` lanes that NAME it are `hist`
///     and the `backfill` suite, which build and test it; `windows-cross`, which checks it for
///     Windows (its `#[cfg(windows)]` archive reader); and the `backfill-serve` FEATURE SUITE below,
///     which checks+lints the vike-datahub build for the reason that suite's own note argues — this
///     crate is one of ITS triggers too, not only the seven kline bridges. `multicall` compiles it
///     too without naming it, because its `full` turns on vike-datahub's `backfill-serve` CARGO
///     FEATURE, which depends on it. Unrelated to what it names: docs/decisions/0094 took
///     the last bridge crate off this crate's dependency tree (its rank dropped to 30, `engine`),
///     but that changed only what it may depend on, not which lane compiles it.
///
/// ⚠ `ibapi` was a row here (the in-tree IBKR API copy) until it was replaced by the published
/// crate: a registry dependency is not a workspace member, so it has no roster row to exclude, and
/// the `ibkr` feature suite is what compiles it.
///
/// A new crate that genuinely cannot build in the plain lane belongs here, with its reason.
///
/// ⚠ `xtask` is deliberately ABSENT: it is a workspace member and a CI crate like any other. The
/// alternative — a member excluded from the roster — is refused outright by
/// `crates/vike-ops/tests/feature_lane_coverage.rs`'s `every_workspace_member_is_built_by_some_lane`
/// unless some lane names it, and it would leave the tool that DERIVES the merge gate as the one
/// crate the merge gate never compiles.
pub const EXCLUDE_FROM_CI: &[&str] = &["vike-desktop", "vike-backfill"];

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
///   * `backfill` — ONE union-features lane over vike-backfill's four independent tool features
///     (`databento` + `tardis` + `vike-archive`; the merge
///     rationale is in `scripts/ci_feature_suite.sh`). The trigger is the union of the four lanes'
///     sets: `databento`/`tardis`/`vike-archive` pull
///     vike-bridge-core (the canonical credentials loader — vike-archive's own bin uses it for
///     `VIKE_ARCHIVE_API_KEY`). ⚠ `poly-reparse` was a fifth until docs/decisions/0094 deleted it —
///     no deployed process ever produced its input. `poly-ch-backtest` was a sixth until
///     2026-09-20; it pulled the vike-backtest harness onto a COLLECTOR, and both bins it gated
///     have left.
///   * ⚠ `backfill-ibkr` stood here, kept SEPARATE from `backfill` because the `ibkr` feature it
///     ran compiled the `ibapi` crate's tree — a large build no other backfill lane needed. It is
///     DELETED with that feature (docs/decisions/0094), measured unused: no `venue=ibkr` series
///     existed in any store.
///   * `polymarket-stack` — ONE lane for the polymarket mount stack (`vike-tradehub/polymarket`):
///     test+clippy over vike-tradehub with its feature, so its registry row for the bridge's
///     `PolymarketVenueMount`, its wired-market row, its moved feature-on tests and its LIVE
///     daemon arm compile while the shared tree builds once. ⚠ It named `vike-mount/polymarket` as
///     the innermost ring until the venue mount contract moved that crate's arm into the bridge
///     and the registry row to vike-tradehub (docs/decisions/0096, amended 2026-09-29), and
///     `vike-run/polymarket` until docs/decisions/0098 deleted that marker with the wired
///     markets' move; the key keeps its name although the lane builds one package now.
///   * `telegram` — vike-tradehub's opt-in TELEGRAM control channel. A remote order-origination
///     path reachable from a third-party chat service is compiled OUT of a default build
///     (`#[cfg(feature = "telegram")]` on the module, on the mount and on both call sites), so a
///     default `cargo test -p vike-tradehub` compiles NONE of it and `tests/telegram_control.rs` is
///     `#![cfg]`-ed away to an empty binary. Without this suite the whole channel would be
///     un-type-checked, un-clippy-gated and never run — the way the `benches/engines.rs` `--bench`
///     bug survived. Kept its own lane: it must stay provably DataFusion-free, and
///     this one must stay provably DataFusion-free. The trigger is the crate itself, because the
///     feature turns on no cross-crate optional dep — only the crate's own `dep:ureq`, already in
///     this graph transitively via vike-bridge-core.
///   * ⚠ `tradehub-sinks` is GONE. It was ONE lane for vike-tradehub's two opt-in DataFusion sink
///     arms (`record-feeds` + `materialize`), and both features were deleted by
///     `docs/decisions/0084-only-the-datahub-touches-the-store.md` — the daemon no longer writes
///     the store, so there is no sink arm left to compile.
///   * `backtest-hist-replay` — ⚠ KEEPS ITS NAME although the feature it was named for died in the
///     2026-09-27 feature collapse (`docs/decisions/0087`): `hist-replay` used to gate the whole
///     `harness/` tree (profile/run/sweep/euler), `hist_replay.rs` and the rayon sweep pool behind
///     an OPTIONAL feature; all of it is a DEFAULT `cargo test -p vike-backtest` build now, so the
///     roster lane's own nextest pass already runs it. What THIS suite still proves is the
///     DataFusion-free property that default build can no longer prove alone: a single-package
///     `cargo clippy -p vike-backtest` (trait-only, DataFusion-free — the decouple gate) plus
///     `--features datafusion-store` (the concrete `DataFusionHist` bins/tests). Without
///     it neither lane is clippy-gated with `vike-studio-core`'s dev-dependency unification kept
///     out of the picture, and the `datafusion-store` half is not run at all.
///   * ⚠ `alerting-standalone` is GONE (2026-09-28), and deleting it answers its comment rather
///     than silencing it. It built vike-alerting ALONE because the roster lane never compiled that
///     crate's vike-free build: vike-ops depended on it with a `core` feature (and `workspace-env`),
///     and resolver-2 unified both in. Both features are deleted and vike-ops' edge with them
///     (2026-09-23 and 2026-09-25), so the crate has ONE configuration and the roster lane builds,
///     lints and tests it — MEASURED on a 2026-09-28 verify-branch run, the roster's nextest ran
///     all 52 of the crate's tests and the lane's `cargo test` re-ran the same 52, compiling
///     nothing new. Its `cargo tree` half held the property the split exists for — no `vike-*`
///     crate among the crate's normal dependencies — and that is a manifest read now:
///     `crates/vike-ops/tests/layer_gate.rs`'s `VIKE_FREE_CRATES`, which is stricter (it counts
///     optional and target-specific edges too) and which `just test` runs, where the tree check
///     was a pipeline only CI could.
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
///     green, and `main` could not build `-p vike-cli` at all. The trigger set is the crates whose
///     feature gating or light-half dependency edges define the property.
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
///     `backfill-serve` Cargo feature was already compiled by [`HIST_CRATES`]'s `hist-datafusion`
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
///     excludes vike-desktop and vike-backfill (they are in [`EXCLUDE_FROM_CI`], so the derived
///     roster cannot name them either), and the result was that
///     `crates/vike-backfill/src/vike_archive.rs`'s `#[cfg(windows)]` `read_at` — the `seek_read`
///     arm that fixed a real `--jobs 4` read-corruption — was compiled by no CI job and no local
///     recipe, on any box. Mutation-proved: a bogus argument to `seek_read` is invisible to every
///     Linux check and fails E0425 under a Windows target. vike-desktop sits at the top of the dep
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
///     reaches it) and it triggered only `light-consumers` (and `multicall` since 2026-10-03) — so
///     without this row a PR touching
///     `crates/vike-cli/` alone would schedule every OTHER lane and not the one added for it,
///     leaving the release build to be first compiled at tag time. It rides its own `cargo check`
///     line inside the arm rather than the shared selection, because that line is deliberately not
///     `--all-targets` (the crate's dev-deps are a second large tree the release compiles none of).
///   * ⚠ vike-report's feature-OFF lane is GONE (2026-09-28), and deleting it answers its comment
///     rather than silencing it. It built vike-report with its default `journal` feature off —
///     the configuration no consumer and no other lane compiled — and asserted via `cargo tree`
///     that the feature-less normal tree carried none of the journal/exec/data closure, because a
///     renderer-only consumer (vike-cli's `backtest show --html`) took the crate that way. That
///     renderer half then moved into `vike-analytics` and the feature was deleted, so the
///     configuration no longer exists to be built. The property the `cargo tree` step held — a
///     light renderer carries no heavy closure — is held structurally now, by
///     `crates/vike-ops/tests/layer_gate.rs`'s tier-15 rule over vike-analytics.
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
///     of them through `backfill-serve` as well), deribit only through `backfill-serve`, which the
///     `hist-datafusion` job builds (`-p vike-datahub --features backfill-serve`). Those compile a
///     bridge's feeds half as part of the daemon; this suite compiles each one ALONE, on that
///     bridge's own change. The suite also asserts structural properties
///     via `cargo tree -e normal`, not just that the configuration compiles — ⚠ but only GROUP 1
///     gets the crate-name form (no EIP-712 signer crate in its three feeds trees). Group 2 gets
///     NO crate-name check and deliberately none: those three sign with vike-bridge-core's HMAC
///     signer, which rides that crate's `full` feature that the feeds half needs anyway, so their
///     default and feeds trees are IDENTICAL and such a grep could never fail. Deribit's two trees
///     carry the same crate SET for its own reason — it signs nothing, and the one crate only its
///     `exec` names (tungstenite) rides that same `full`. Their one structural claim is a
///     FEATURE-resolution grep over vike-aster's feeds tree — it must not resolve vike-binance with
///     `exec` — which is why aster triggers this suite for group 2's sake as well as its own. The
///     trigger is the seven crates themselves; vike-bridge-core (whose `eip712`
///     feature is the stack being kept out) sits below all of them and lands in the affected set by
///     reverse-dep closure.
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
///     and fails exactly where a release build fails; plus the members [`EXCLUDE_FROM_CI`] keeps
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
///     ⚠ It is deliberately NOT in [`SUITE_GROUPS`]: a cold `--workspace` compiles vike-desktop's
///     eframe/wgpu closure, this lane's cost has never been measured, and every set membership
///     rule below requires combined work well under the long pole.
pub const FEATURE_SUITES: &[(&str, &[&str])] = &[
    ("polymarket", &["vike-polymarket"]),
    // The fxcm chain the shipped daemon resolves: vike-tradehub/fxcm → dep:vike-fxcm +
    // vike-fxcm/fxcm. vike-tradehub is named for the DIRECTION reason: it sits ABOVE vike-fxcm, so
    // an edit in it never reaches vike-fxcm's row — and its `#[cfg(feature = "fxcm")]` sites (the
    // registry row and the wired-market row) are exactly what this lane compiles and nothing else
    // does. ⚠ vike-mount was named while its `("fxcm", _)` live arm compiled under its own `fxcm`
    // feature; the venue mount contract moved the arm into the bridge and the feature to
    // vike-tradehub, which holds the registry (docs/decisions/0096, amended 2026-09-29). ⚠ vike-run
    // was named while its `fxcm` MARKER gated its own `FXCM_MARKET` row and `build_node` call; that
    // marker went when the wired markets moved up to vike-tradehub (docs/decisions/0098), so every
    // site this lane compiles is vike-tradehub's alone, and a vike-mount change (vike-run merged into
    // it, 0098) still fires this lane because the affected set follows vike-tradehub's normal edge.
    ("fxcm", &["vike-fxcm", "vike-tradehub"]),
    // The ibkr chain: vike-tradehub/ibkr → dep:vike-ibkr + vike-ibkr/ibkr. vike-tradehub is named
    // for the DIRECTION reason `fxcm` records above: it sits ABOVE vike-ibkr, so an edit in it
    // never reaches vike-ibkr's row — and its `#[cfg(feature = "ibkr")]` sites run their tests in
    // this lane and nowhere else (the `multicall` lane compiles their library code under `full`).
    // ⚠ vike-mount was named while its `("ibkr", _)` make_engine arm and arming row compiled under
    // its own `ibkr` feature; the venue mount contract moved both into the bridge and the feature
    // to vike-tradehub, which holds the registry (docs/decisions/0096, amended 2026-09-29).
    // ⚠ vike-run was named while its `ibkr` MARKER gated its own `IBKR_MARKET` row and `build_node`
    // call; that marker went when the wired markets moved up to vike-tradehub (docs/decisions/0098),
    // and a vike-mount change (vike-run merged into it, 0098) still fires this lane because the
    // affected set follows vike-tradehub's normal edge.
    // The chain reaching vike-tradehub has its own history: that forward did not exist until
    // #1575, and without it the headless daemon fell through to paper in silence while the GUI
    // shell (`vike-app` then; `vike-desktop` declares no `[features]` table) could mount IBKR live.
    ("ibkr", &["vike-ibkr", "vike-tradehub"]),
    ("socks-proxy", &["vike-bridge-core"]),
    // The `vike-study` bin (`--features study-cli`, off by default so `studio-standalone`'s
    // DataFusion-free structural check keeps holding) — the only lane that type-checks it.
    // vike-studio-core alone: `affected` propagates to dependents, so a change in any crate the
    // bin builds against (vike-data, vike-ml, vike-user-research) reaches this trigger through it.
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
    // enables `dep:vike-recorder` (they forwarded `vike-recorder/polymarket` + `binance` until
    // decision 0092 deleted those features). So a change to that crate still changes what this
    // lane compiles, and the trigger has to exist wherever the EDGE does. The rule these rows
    // encode is "every crate this build compiles", not "every crate the dispatcher names".
    // ⚠ `vike-tradehub` and `vike-cli` were MISSING until 2026-10-03, and that was a hole, not a
    // judgment: `crates/vike/src/main.rs` calls `vike_tradehub::tradehub_cli::run`,
    // `venues_cli::run`, `catalog_cli::run` and `vike_cli::run`, every one behind an OPTIONAL dep
    // (no reverse edge in the default graph), and neither crate reaches any trigger above by a
    // normal edge — so a PR changing one of those signatures, confined to that crate, planned no
    // `multicall` leg — free to merge green and break that build (and the release, which builds
    // `--features full`) on `main`. It was masked whenever such a PR also touched a global file:
    // #2414 changed both crates together with a `scripts/` file and so ran the full matrix, which
    // narrowing `scripts/` would have turned into a plan with no `multicall` leg at all (replayed
    // through the real planner on a lane). `crates/vike-ops/tests/ci_plan_gate.rs` now
    // derives the set from the manifest: every optional `vike-*` dependency of `vike-backend` is a
    // trigger here unless a named edge from a trigger reaches it (vike-model and
    // vike-strategy-builder, the two it accepts — read the test for which edge each is).
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
    // ⚠ `vike-backtest` LEFT this row 2026-09-28 (docs/decisions/0094): MEASURED against
    // this tree (`cargo tree -p vike-backfill -e normal,dev --all-features --prefix none`), the
    // vike-* crates vike-backfill's whole tree — every feature on, dev edges included — reaches are
    // vike-backfill, vike-bridge-core, vike-data, vike-exec, vike-log, vike-marketdata, vike-model
    // and vike-secrets. `vike-backtest` is not among them — it likely left when `poly-ch-backtest`
    // and the two bins it gated did (2026-09-20), and this row was not re-measured then — so
    // keeping it fires the suite on a PR this crate cannot see change. Only vike-bridge-core is
    // named beside the crate itself: it is the one OPTIONAL cross-crate dep the suite's four
    // features pull in (databento/tardis/vikedata/vike-archive all name `dep:vike-bridge-core`), so
    // this table's own rule above requires naming it. Every other crate the measurement found is a
    // normal, non-optional edge (vike-backfill's own, or one of vike-bridge-core's), so a change to
    // any of them already reaches this row through the ordinary reverse-dep walk.
    ("backfill", &["vike-backfill", "vike-bridge-core"]),
    // The polymarket chain: vike-tradehub/polymarket → dep:vike-polymarket +
    // vike-polymarket/polymarket. vike-polymarket stays a trigger because it is an OPTIONAL
    // dependency of vike-tradehub, which the default-build reverse-dep closure does not follow.
    // ⚠ vike-mount was named while its `("polymarket", _)` make_engine arm and arming row compiled
    // under its own `polymarket` feature; the venue mount contract moved both into the bridge and
    // the registry row to vike-tradehub (docs/decisions/0096, amended 2026-09-29). ⚠ vike-run was
    // named while its `polymarket` MARKER gated its own `POLYMARKET_MARKET` row and `build_node`
    // call; that marker went when the wired markets moved up to vike-tradehub
    // (docs/decisions/0098), so every `#[cfg(feature = "polymarket")]` site this lane compiles is
    // vike-tradehub's, and a vike-mount change (vike-run merged into it, 0098) still fires this
    // lane because the affected set follows vike-tradehub's normal edge.
    ("polymarket-stack", &["vike-tradehub", "vike-polymarket"]),
    ("telegram", &["vike-tradehub"]),
    ("backtest-hist-replay", &["vike-backtest"]),
    ("recorder-venues", &["vike-recorder", "vike-datahub", "vike-polymarket", "vike-binance"]),
    ("light-consumers", &["vike-bridge-core", "vike-ops", "vike-cli", "vike-secrets"]),
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
    // The LIVE MARKET-DATA plane. ⚠ ALL SEVEN CRATES, and the six bridges are not decoration: this
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
        ],
    ),
    // The VENUE-CATALOG provider table
    // (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`). ⚠ The
    // seven crates of `live-feeds` above, for exactly the same reason — `catalog-serve` turns on
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
    // The set is the seven crates whose bins the deleted per-package release build used to prove.
    // It is a HEURISTIC, not a roster: the lane compiles the whole workspace whichever crate fired
    // it, and any enumeration of bin-owning crates here would be exactly the partial roster this
    // file exists to stop writing down. Everything below these seven reaches them by reverse-dep
    // closure. The two omissions are MEASURED, not forgotten — see this key's paragraph above.
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
///     member (6) was deleted — the vike-alerting GONE note on [`FEATURE_SUITES`] says why. ⚠ The
///     `fxcm` figure PREDATES that arm's widening to the vike-run/vike-tradehub feature chain and
///     is now an underestimate; it is left as the last thing actually measured rather than
///     replaced by a guess, and the set stays comfortably under the long pole even taking the
///     tradehub pair's cost as its ceiling.
///     ⚠ The `bridges-feeds` figure is stale for the same reason and by MORE: ruling 8 doubled
///     that arm's crate count (three EIP-712 bridges → those plus binance/bybit/okx, each with a
///     `cargo check`; the three tree greps stayed at four, since group 2 shares aster's), so read
///     15 as a floor, not a measurement of today's arm.
///     Re-measure before packing anything else in here;
///   * the polymarket-adjacent trio (~116s): polymarket (33) + socks-proxy (44) +
///     recorder-venues (39). ⚠ The `recorder-venues` figure PREDATES ruling 10 adding a
///     `-p vike-datahub --features record-*` build to that arm, AND decision 0092 removing the arm's
///     `-p vike-recorder --features …` pair, so it measures neither of today's arm's two steps — left
///     as the last thing actually MEASURED rather than replaced by a guess, exactly as the `fxcm`
///     and `ibkr` figures below. Re-measure before packing anything else into this trio;
///   * `ibkr` (46s measured) is UNGROUPED — a solo member is emitted alone anyway (see the packer
///     rule above), so it needs no set of its own. ⚠ It used to share a job with `backfill-ibkr`,
///     re-warming one shared ibapi tree; docs/decisions/0094 deleted that suite with the `ibkr`
///     feature it compiled (measured unused), so the pairing's reason went with it. The figure
///     PREDATES `ibkr`'s own widening to the vike-mount/vike-run/vike-tradehub feature chain and
///     is now an underestimate, exactly as the `fxcm` figure above is; it is left as the last
///     thing actually MEASURED rather than replaced by a guess. RE-MEASURE before packing it with
///     anything else;
///   * vike-tradehub's `telegram` used to be PACKED with a `tradehub-sinks` lane beside it (46s +
///     48s). That lane is GONE with the two sink features it existed for (0084), so telegram is
///     emitted alone now — which the packer already does for any unpaired member.
///
/// ⚠ A member named here that is not a [`FEATURE_SUITES`] key would never be affected, so its set
/// would silently degrade to singletons — `crates/vike-ops/tests/ci_plan_gate.rs`'s packing tests
/// pin membership validity and the packing behaviour itself.
pub const SUITE_GROUPS: &[&[&str]] = &[
    &["studio-standalone", "bridges-feeds", "fxcm"],
    &["polymarket", "socks-proxy", "recorder-venues"],
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
///   * `scripts/latency_contention.sh` — the gate's own VERDICT: it reads `/proc/pressure/cpu`
///     around each attempt and decides the job's exit code (pass, fail, or "unmeasurable"). Only
///     the latency job runs it, so an edit to it is tested by nothing else that can re-measure —
///     `crates/vike-ops/tests/latency_contention_gate.rs` runs its `--selftest` and pins its call
///     sites, but cannot run it against a real p99. It was the one script the latency job runs
///     that its trigger did not name. Edited once in its history (the PR that created it), so the
///     ~5-minute the latency box run it now buys per edit is rare.
///
/// EXCLUDED, with the reason (each was checked, not assumed):
///
///   * `rustfmt.toml` — formatting; cannot reach codegen.
///   * `deny.toml` — read by `cargo deny`, never by rustc.
///   * `justfile` — local recipes. CI derives its own plan and does not read it.
///   * every OTHER `scripts/` file — none of them is run by the latency job (`ci_feature_suite.sh`,
///     `ci_lockfile_gate.sh` and the rest drive other jobs). ⚠ This line read "`scripts/*` drive
///     OTHER jobs" while the latency job was running `latency_contention.sh` unnamed above.
///   * `.github/*` (other) — the other workflows (release/deny/jforex-bridge/ctrader-proto), plus
///     the `rust-ci-setup` composite action, which the `plan` job deliberately does NOT use.
///   * `docs/`, `CLAUDE.md`, `CODEOWNERS` — prose.
pub const LATENCY_GLOBAL_PREFIXES: &[&str] = &[
    "Cargo.lock",
    "rust-toolchain",
    ".cargo/",
    ".github/workflows/ci.yml",
    "scripts/latency_contention.sh",
    "xtask/",
];

/// Paths that invalidate the narrow selection entirely and escalate to the FULL crate matrix.
///
/// ⚠ `settings/` is here because it is a RUNTIME input to every test, not because it is
/// configuration. The repo root is the workspace root, so `vike_model::paths::state_path`'s
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

/// `(prefix, suffix)` pairs that sit UNDER a [`GLOBAL_PREFIXES`] entry and do not earn its
/// escalation. A file matching one is treated as ordinary: it selects crates the normal way.
///
/// ⚠ **The only safe exemption is one where something ELSE already gates the file**, and that is
/// exactly why `.md` under `scripts/` qualifies. `gate_crates_for` force-adds [`DOC_GATE_CRATE`] on
/// any `.md` path anywhere ([`DOC_GATE_INPUT_SUFFIXES`]), and it does so from the RAW file list,
/// independently of this narrowing — so `crates/vike-ops/tests/skills_gate.rs`,
/// `citation_gate.rs`, `publish_mirror_gate.rs` and `one_authority_gate.rs` still run. The gates
/// that actually care about these files are unaffected; only the 62-crate matrix goes away.
///
/// ⚠ These are NOT decorative files, and the exemption would be wrong if they were merely assumed
/// harmless. `scripts/skills/*.md` are the TEMPLATES rendered into `skills/*/SKILL.md`, which
/// `skills_gate` byte-compares; `scripts/mirror_readme.md` is rendered into the public mirror. They
/// are gated harder than most source — just not by compiling the workspace.
///
/// MEASURED over the 150 commits before this landed: 67 escalated to the full matrix, and 6 of them
/// were `scripts/` changes whose every file was `.md`. At 20-30 minutes each that is ~8% of all full
/// matrices, on a repository where 44% of commits trigger one.
///
/// ⚠ Deliberately a PAIR rather than a bare suffix. A bare `.md` exemption would also cover
/// `.github/**/*.md` — and a workflow's own README sits beside files that decide what CI runs, which
/// is not a claim this table is in a position to make.
///
/// ⚠ This is the ONLY pattern exemption, and it is not going to get a sibling. Everything else is
/// exempted BY NAME in [`GLOBAL_EXEMPT_FILES`]: a directory-wide pattern with a list of files it
/// must not cover is a carve-out whose failure mode is a NEW file — a script some CI job starts
/// running — inheriting the exemption without anybody deciding it should. Named, a new file stays
/// global until somebody adds it and the gate below has checked the job that runs it.
pub const GLOBAL_PREFIX_EXEMPT: &[(&str, &str)] = &[("scripts/", ".md")];

/// The files under [`GLOBAL_PREFIXES`] that do NOT escalate, BY NAME — matched by equality, so a
/// sibling, a subdirectory twin or a renamed copy is not covered. A file listed here selects crates
/// the normal way, and [`super::lane_crates_for`] adds the crates whose tests read it to the test
/// LANE. A file NOT listed (and not markdown under `scripts/`) stays global: that is the default,
/// and it is the safe one.
///
/// ⚠ **The only safe exemption is one where something ELSE already gates the file.** For every name
/// below that "something" is the following, and each mechanism is required:
///
///   1. **The gate crate joins the lane.** Every listed file force-adds [`EXEMPT_INPUT_GATE_CRATE`]
///      (vike-ops), the crate owning the gates that read or RUN these files —
///      `crates/vike-ops/tests/local_gate_mirrors_ci.rs` (the justfile as a mirror of
///      `scripts/ci_feature_suite.sh`), `lockfile_gate_mirror.rs`, `api_docs_gate.rs`,
///      `publish_mirror_gate.rs`, `shell_backtick_gate.rs`, `latency_contention_gate.rs`,
///      `ci_slowest_tests_gate.rs`, `deploy_tool_table_gate.rs`, `container_image_gate.rs`,
///      `packaging_gate.rs`, `smoke_guard_gate.rs` and more. LANE only, never `affected`: vike-ops
///      triggers `light-consumers`, which no script edit can move.
///   2. **A reader OUTSIDE vike-ops joins the lane too** — [`LANE_INPUT_READERS`], one row per file
///      another crate's test reads, plus [`DOCS_DATA_GATE_INPUTS`] for vike-docs. Lane only again:
///      `vike-fxcm` and `vike-dukascopy` trigger feature suites a script edit cannot move either.
///   3. **A script a WORKFLOW runs keeps the job that runs it.** `ci.yml`'s `plan` job runs
///      `scripts/ci_lockfile_gate.sh` and three selftests (`scripts/verify_branch.sh`,
///      `scripts/unit_drift.sh`, `scripts/assert_release_identity.sh`) on EVERY run — it has no
///      `if:` — which also covers the runs a pull request cannot reach: `unit_drift.sh` in
///      `deploy.yml`, and `assert_release_identity.sh` in a tag-only `release.yml` step; `scripts/ci_slowest_tests.sh`
///      runs in the `test` job, which mechanism 1 makes run; `scripts/build_api_docs.sh` and the two
///      files its build executes are [`DOCS_JOB_INPUTS`]; `scripts/latency_contention.sh`, which
///      only the latency job runs, is a [`LATENCY_GLOBAL_PREFIXES`] entry. A script only a tag-, dispatch- or
///      `workflow_run`-triggered workflow runs never ran under the full `ci.yml` matrix either, so
///      for those the gate crate of mechanism 1 is what tests the change, and each has one.
///   4. **A deleted or renamed-away file stays GLOBAL** ([`super::escalates`] checks the path still
///      exists). Citations of these paths live in every crate —
///      `crates/vike-data/tests/series_cadence_gate.rs` resolves the ones its table's prose names —
///      so a deletion can break a test no row selects, while an EDIT cannot move an existence check.
///      Rare (two deleted files in the 150 commits measured below, both in one PR), so it costs
///      nothing.
///
/// `crates/vike-ops/tests/ci_plan_gate.rs` holds this list BOTH WAYS: every name is a tracked file,
/// every tracked file under `scripts/` and `.github/` is either named here or in that file's list
/// of files that stay global (each with its reason — the feature-suite runner and the LightGBM
/// job's three scripts drive a job over the whole matrix; `ci.yml` and the setup action define
/// every job), and every script ANY workflow or action runs — directly,
/// through a `just` recipe, or through another script — is named here only if mechanism 3 holds for
/// the job that runs it or a named gate crate covers it. It also derives mechanism 2's reader set
/// from the tree both ways.
///
/// MEASURED (2026-10-03, the 150 first-parent commits on `main` before this landed, each job costed
/// at its median over 32 real push runs): 73 escalated, `scripts/` was the commonest trigger (34),
/// and exempting `scripts/` + `justfile` turned 22 of them narrow on their own, saving ~17,800
/// runner-seconds. The pure-script PRs are the ones that feel it — #2382, two deploy scripts, ran a
/// 773 s full matrix where a vike-ops-only run takes ~90 s. The justfile alone saves ~0: it never
/// decided a commit by itself, and is exempt because nothing in CI reads it but two tests.
///
/// ⚠ **`.github/` is named file by file too, and two of its files are NOT named.**
/// `.github/workflows/ci.yml` defines how every job runs (and `crates/vike-core/tests/common/latency_line.rs`
/// reads it), and `.github/actions/rust-ci-setup` is the setup every build job runs — an edit to
/// either must re-run what it changes. Every OTHER workflow is a separate workflow: path-triggered
/// on its own file (brand-assets, content, coverage, ctrader-proto, deny, feature-matrix,
/// fonts-vendor, fuzz, jforex-bridge, mutants) or run only on a tag, by hand, on a schedule or
/// after `release` (agent-eval, deploy, live-smokes, mcp-registry-publish, mirror-publish,
/// release, release-image, ureq-release-watch). The full `ci.yml` matrix never ran one of them, so
/// escalating on their edit bought nothing — while their READERS (mechanism 2: vike-dukascopy and
/// vike-strategy-builder read `release.yml`, vike-dukascopy reads `jforex-bridge.yml`, vike-docs
/// through [`DOCS_DATA_GATE_INPUTS`]) and vike-ops' workflow gates (mechanism 1) still run.
/// MEASURED over the same 150 commits: 4 commits were held global by such a workflow alone
/// (~5,300 runner-seconds, three of them single-workflow PRs costing over 1,000 each), and 4 more
/// needed both this and the `scripts/` names (~2,900).
///
/// Three names here were exemptions before the rest, and their measurements are why the rule above
/// reads the way it does:
///   * `scripts/build_api_docs.sh` (CI run 34875966216: one changed file emitted 62 `-p` flags and
///     all 14 suite legs; 164 s instead of 492 s once exempt) — and the one that found mechanism 3:
///     the `api-docs` job was the only thing that ever EXECUTED it, and nothing ran it once it
///     stopped escalating until [`DOCS_JOB_INPUTS`] named it.
///   * `scripts/verify_branch.sh` and its fixture harness (2026-09-22) — licensed by the `plan`
///     job's selftest step, and a measured warning about what happens without one: the fixture
///     suite had been RED on `main` while nothing invoked it. A fixture that is never run cannot
///     even go red.
pub const GLOBAL_EXEMPT_FILES: &[&str] = &[
    ".github/CODEOWNERS",
    ".github/dependabot.yml",
    ".github/workflows/agent-eval.yml",
    ".github/workflows/brand-assets.yml",
    ".github/workflows/content.yml",
    ".github/workflows/coverage.yml",
    ".github/workflows/ctrader-proto.yml",
    ".github/workflows/deny.yml",
    ".github/workflows/deploy.yml",
    ".github/workflows/feature-matrix.yml",
    ".github/workflows/fonts-vendor.yml",
    ".github/workflows/fuzz.yml",
    ".github/workflows/jforex-bridge.yml",
    ".github/workflows/live-smokes.yml",
    ".github/workflows/mcp-registry-publish.yml",
    ".github/workflows/mirror-publish.yml",
    ".github/workflows/mutants.yml",
    ".github/workflows/release-image.yml",
    ".github/workflows/release.yml",
    ".github/workflows/ureq-release-watch.yml",
    "justfile",
    "scripts/assert_release_identity.sh",
    "scripts/batch_prs.sh",
    "scripts/batch_prs_selftest.sh",
    "scripts/build_api_docs.sh",
    "scripts/changelog_release.sh",
    "scripts/changelog_release_selftest.sh",
    "scripts/ci_lockfile_gate.sh",
    "scripts/ci_slowest_tests.sh",
    "scripts/cli_mcp_smoke.sh",
    "scripts/creds_audit.sh",
    "scripts/deploy_sandbox.sh",
    "scripts/deploy_sandbox_selftest.sh",
    "scripts/fetch_release_tools.sh",
    "scripts/fonts_vendor_drift.sh",
    "scripts/forbidden_tokens.ere",
    "scripts/gen_skills.sh",
    "scripts/graphify_verify.py",
    "scripts/hooks/pre-commit",
    "scripts/latency_contention.sh",
    "scripts/marketing_shots.sh",
    "scripts/new_venue.sh",
    "scripts/nonfree_release_assets",
    "scripts/pr_freshness.sh",
    "scripts/pr_freshness_selftest.sh",
    "scripts/pr_merge.sh",
    "scripts/pr_merge_selftest.sh",
    "scripts/prod2_lane.sh",
    "scripts/prod2_lane_selftest.sh",
    "scripts/publish_mirror.sh",
    "scripts/publish_starter_data.sh",
    "scripts/qa_judge.sh",
    "scripts/qa_judge_selftest.sh",
    "scripts/qa_shots.sh",
    "scripts/refuse_box_paths.sh",
    "scripts/refuse_live_credentials.sh",
    "scripts/release_container_image.sh",
    "scripts/release_container_image_selftest.sh",
    "scripts/release_fxcm_artifact.sh",
    "scripts/run_mutations.sh",
    "scripts/run_wf.sh",
    "scripts/studio.ps1",
    "scripts/unit_drift.sh",
    "scripts/verify_branch.sh",
    "scripts/verify_branch_selftest.sh",
    "scripts/vike_tradehub_windows.ps1",
];

/// The crate a change to ANY exempted path force-adds to the test lane — the owner of the gates
/// that read these files (mechanism 1 on [`GLOBAL_EXEMPT_FILES`]).
pub const EXEMPT_INPUT_GATE_CRATE: &str = "vike-ops";

/// The crates OUTSIDE [`EXEMPT_INPUT_GATE_CRATE`] whose tests read a file that owns no crate,
/// keyed `(prefix, suffix)` the way [`GLOBAL_PREFIX_EXEMPT`] is (an exact path is a row whose
/// suffix is empty, and a directory row ends in `/`): a changed path matching a row
/// force-adds its crates to the test LANE ([`super::lane_crates_for`]) — never to `affected`, so no
/// feature suite fires to buy a test run.
///
/// ⚠ **This table is DERIVED, then written down, and the gate holds the two equal.**
/// `crates/vike-ops/tests/ci_plan_gate.rs` scans every crate's sources for the repo paths they
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
///     (vike-docs reads it too, through [`DOCS_DATA_GATE_INPUTS`].)
///   * `justfile` — `crates/bridges/fxcm/tests/fcsdk_packaging.rs` `include_str!`s it to hold the
///     `fxcm-package` recipe the runbook calls.
///   * `scripts/fetch_release_tools.sh` — `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` reads its
///     `jre` row, so the installed JDK and the pinned one cannot disagree.
///   * `scripts/qa_shots.sh` — `crates/vike-app-core/tests/qa_shot_account.rs` reads the fixture
///     store the contact sheet seeds.
///
/// ⚠ **The rows under `docs/`, `deploy/` and `skills/` close a hole that has nothing to do with the
/// global escalation**, and it is the same hole: those trees never escalated, they select only
/// [`DOC_GATE_CRATE`] (`docs/`, `deploy/`, any `.md`), so a test OUTSIDE vike-ops that reads one of
/// their files did not run on that file's own change — it ran, and failed, on the next unrelated
/// PR that happened to select its crate. MEASURED when these rows landed, with the reader scan the
/// gate runs: fifteen test files in nine crates, against the six an earlier grep had listed —
///   * `deploy/` units: `crates/vike-config/tests/removed.rs` reads EVERY `deploy/*.service` (no
///     unit may set a retired variable); `deploy/vike-tradehub.service` is read by
///     `crates/vike-cli/tests/bootstrap_daemon_cli.rs`, `crates/vike-tradehub/src/tradehub_cli_tests.rs`
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
///   * `skills/`: `crates/vike-cli/src/cmd/mcp_tests.rs` reads every `SKILL.md` and `data_tests.rs`
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
    ("docs/ops/kill-switches.md", "", &["vike-config"]),
    ("docs/ops/run-profile-live.toml", "", &["vike-config"]),
    ("docs/ops/tradehub-the CI box.md", "", &["vike-fxcm"]),
    ("justfile", "", &["vike-fxcm"]),
    ("scripts/fetch_release_tools.sh", "", &["vike-dukascopy"]),
    ("scripts/qa_shots.sh", "", &["vike-app-core"]),
    ("skills/", "", &["vike-agent-eval", "vike-cli"]),
];

/// Files owned by no crate whose READER compiles only under a non-default feature — so the test
/// lane, which builds every crate with its default features, never runs it, and putting its crate
/// in the lane would be a row that buys nothing. A changed path matching `(prefix, suffix)` FIRES
/// the named [`FEATURE_SUITES`] leg instead, the one build that compiles the reader.
///
/// `crates/vike-ops/tests/ci_plan_gate.rs` holds this table with [`LANE_INPUT_READERS`]: a reader
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
/// ([`super::docs_affected`]), and a shell script owns no crate — so without this table a change to
/// the script that BUILDS the API reference would not run the job that builds it. That was masked
/// until now: `docs` also fires whenever the global set is non-empty, so the script's own coverage
/// was an accident of the escalation [`GLOBAL_EXEMPT_FILES`] removed for it.
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

/// True when `f` is NAMED in [`GLOBAL_EXEMPT_FILES`] or matches a [`GLOBAL_PREFIX_EXEMPT`] pair. A
/// PATH question only: whether the file still EXISTS is [`super::escalates`]'s.
pub fn is_global_exempt(f: &str) -> bool {
    GLOBAL_EXEMPT_FILES.contains(&f)
        || GLOBAL_PREFIX_EXEMPT
            .iter()
            .any(|(prefix, suffix)| f.starts_with(prefix) && f.ends_with(suffix))
}

/// The crate owning the settings-registry gate (`crates/vike-ops/tests/settings_registry.rs`). That
/// gate walks EVERY `.rs` file in the workspace, so an undeclared `env::var` added ANYWHERE is its
/// business — which makes selecting it by reverse-dep closure unsound. See
/// [`super::gate_crates_for`].
///
/// ⚠ It also owns the STORE-KIND gate (`crates/vike-ops/tests/store_kind_gate.rs`), for the same
/// unsoundness: that gate checks each declared commit-key template verbatim against the PRODUCER
/// file that builds it, and those producers live in other crates —
/// `crates/vike-journal/src/materialize.rs` reaches no vike-data dependent at all, so a renamed
/// `vike_journal::materialize` commit key would merge green and redden the next unrelated vike-data
/// PR. It lived in vike-data's `tests/` until 2026-10-03, behind a `STORE_KIND_GATE_CRATE` rule that
/// force-added the whole of vike-data to the lane on every `.rs` change; under the roster build that
/// ran vike-data's DataFusion suites on every such PR, so the gate moved here and the rule was
/// deleted. The gate's module doc carries the measurement.
pub const SETTINGS_GATE_CRATE: &str = "vike-ops";

/// The crate owning the gates that read EVERY workspace member's `Cargo.toml` — selected by a change
/// to any crate's manifest, whichever crate owns it. Same unsoundness as [`SETTINGS_GATE_CRATE`],
/// on a third input: `crates/vike-ops/tests/layer_gate.rs` reads each manifest's
/// `[package.metadata.vike] layer` and its `vike-*` edges, and
/// `crates/vike-ops/tests/feature_lane_coverage.rs` reads each one's `[features]` and dependency
/// declarations. A manifest belongs to its own crate, so the reverse-dep closure selects that crate
/// and its dependents — never this one, which depends on almost nothing — and until this row a
/// manifest-only change (a new feature, a new `vike-*` edge, a layer number) skipped both gates.
///
/// It also removes the most plausible way to plan the one roster crate whose default build has no
/// test (`vike-polymarket`, measured 2026-10-03) ALONE — an edit to its own manifest — which the
/// `test` job's `--no-tests=fail` would turn into a red run over zero tests. The root `Cargo.toml`
/// and `xtask/Cargo.toml` are not this row's: both escalate to the full roster anyway.
pub const CRATE_MANIFEST_GATE_CRATE: &str = "vike-ops";

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
/// (`light-consumers`), with `hist`/`core`/`app` all still false. Those two are
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
/// ⚠ There was a third entry, `scripts/build_api_docs.sh`, the one script exempted before the rest:
/// `crates/vike-ops/tests/api_docs_gate.rs` reads it and rode the `scripts/` escalation until then.
/// It LEFT when [`GLOBAL_EXEMPT_FILES`] named every other script, because [`EXEMPT_INPUT_GATE_CRATE`]
/// now selects this same crate for every exempted path — through the LANE, so the `light-consumers`
/// suite this row used to fire as a side effect fires no longer. Two rows selecting one crate for
/// one file is a second spelling waiting to disagree with the first.
pub const DOC_GATE_INPUT_PREFIXES: &[&str] = &["docs/", "deploy/"];

/// The crate owning the MCP registry manifest's drift gate
/// (`crates/vike-cli/src/cmd/mcp_tests.rs`'s `the_registry_manifest_lists_every_tool_this_server_serves`,
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

/// The crate owning the DOCS-DATA gate (`crates/vike-docs/tests/docs_data_gate.rs`), which holds the
/// rendered release assets equal to the lists that ATTACH and MIRROR them, and two constants the
/// renderer copies equal to their authorities.
///
/// ⚠ Fourth instance of the same unsoundness as [`SETTINGS_GATE_CRATE`], and it did not exist while
/// the gate lived in `vike-ops`: that crate is force-added on every `.rs` change and on the prose
/// inputs, so the gate rode along. It moved to `vike-docs` on 2026-09-26 — a crate nothing depends
/// on and no force-add named — and three of the files it reads cannot select it through the
/// reverse-dep closure: `.github/workflows/release.yml` and `scripts/publish_mirror.sh` belong to no
/// crate, and `crates/vike-core/tests/runtime_latency.rs` is a TEST source of a crate this one does
/// not depend on. The last is the live hole: a change to the latency budget alone selects
/// `vike-core` plus [`SETTINGS_GATE_CRATE`] (any `.rs`) and never this crate, so the copy the
/// published `stats.json` carries would drift in silence. `docs/superpowers/DEFERRED-BACKLOG.md` predicted exactly this for the move.
///
/// Same cost shape as [`MANIFEST_GATE_CRATE`]: force-add the ONE crate that owns the gate, keyed on
/// the exact files it reads.
pub const DOCS_DATA_GATE_CRATE: &str = "vike-docs";

/// Every repo file [`DOCS_DATA_GATE_CRATE`]'s gate reads, as EXACT paths. Neither the workflow nor
/// the script entry escalates any more ([`GLOBAL_EXEMPT_FILES`] names both), so this row is now the
/// ONLY thing that runs that gate on their change — the case these entries were listed for in
/// advance.
/// `crates/vike-model/src/events.rs` is in the crate's own closure and is listed for the same
/// completeness: `crates/vike-ops/tests/ci_plan_gate.rs` holds this table equal to the set of files
/// that gate reads, both directions, so it cannot rot into a partial list. (That sentence was here
/// before the test was: it was written with the table and the test never landed, so nothing held it
/// until the `scripts/` narrowing made the row load-bearing.)
pub const DOCS_DATA_GATE_INPUTS: &[&str] = &[
    ".github/workflows/release.yml",
    "crates/vike-core/tests/runtime_latency.rs",
    "crates/vike-model/src/events.rs",
    "scripts/publish_mirror.sh",
];

/// Crates whose TESTS SPAWN another crate's shipped BINARY, and the crates those binaries are
/// built from — a DECLARATION, no longer a selection rule.
///
/// ⚠ **This row used to force-add the companions into the test LANE whenever the driver was
/// selected, and that force-add is GONE (2026-10-03).** It existed to get the binaries BUILT, and
/// the `test` job builds the whole roster on every run now. What the row still declares is the edge
/// cargo cannot see, and `crates/vike-ops/tests/ci_plan_gate.rs` holds the property the build
/// depends on: every companion is a roster member, so the roster build contains its bin.
///
/// # The measurement
///
/// CI run 34020320299 (PR #1653, a change confined to `crates/vike-agent-eval/`) planned
/// `-p vike-agent-eval -p vike-cli -p vike-data -p vike-ops`, and the `test` job went red:
///
/// ```text
/// FAIL [ 0.013s] vike-agent-eval::relative_work_dir a_relative_work_dir_still_stands_a_node_up
///   this test drives the SHIPPED binaries and one is not built in this tree.
///   vike-cli:       …/target/debug/vike-cli
///   vike-tradehub:  cannot find the `vike-tradehub` binary. Looked in: …
/// ```
///
/// `crates/vike-agent-eval/tests/scripted_pipeline.rs`'s
/// `every_case_passes_when_driven_by_its_own_scripted_plan` failed identically, and those two are
/// the whole CI-gateable half of that harness — the model half is nondeterministic and can never be
/// a merge gate.
///
/// ⚠ The suite had been green on every earlier run, and that was LUCK rather than coverage:
/// `.github/workflows/ci.yml`'s jobs check out with `clean: false` to keep `target/` warm on the
/// runner, so a `vike-tradehub` left behind by an unrelated run had been answering
/// `crates/vike-agent-eval/src/lib.rs`'s `locate_binary`. The PR that sees the defect is whichever
/// one lands on a box after that stale copy is evicted — the same "reddens the next unrelated
/// change" shape [`SETTINGS_GATE_CRATE`] exists against, wearing a build artifact instead of a
/// gate.
///
/// # Why no closure can find this edge
///
/// `crates/vike-agent-eval/Cargo.toml` declares NO `vike-*` dependency at all, normal or dev, and
/// says why: the harness reaches the system the way an operator does, by SPAWNING the shipped
/// binaries, so it cannot compile against an internal API the shipped surface does not expose. A
/// spawn is not a dependency — cargo models no edge for it and `cargo metadata` reports none — so
/// [`super::graph::affected_from`] has nothing to walk. It would still have nothing to walk if the
/// harness took the dev-dependency anyway: a lib edge builds a LIB, and what a spawn needs is an
/// uplifted `target/debug/<name>`.
///
/// # Why building a crate builds its binary (which is how the force-add once worked)
///
/// The lane WAS `cargo nextest run --profile ci --no-tests=pass ${crates}` — the `test` job in
/// `.github/workflows/ci.yml` — and cargo builds a package's BIN targets whenever it builds that
/// package's INTEGRATION tests — that is what `CARGO_BIN_EXE_<name>` names, and the binary it names
/// is the one uplifted to `target/debug/<name>`, the second path `locate_binary` tries. Both
/// companions qualify, and not incidentally: their own integration tests spawn their own binary
/// through exactly that variable (`crates/vike-tradehub/tests/daemon/help_and_log_dir.rs`,
/// `crates/vike-cli/tests/exit_codes.rs`), so a lane that runs them has already produced the
/// artifact this table is about. The failing run is the positive control as well as the negative
/// one: `vike-cli` WAS in that `-p` list and the panic reports it found at `target/debug/vike-cli`,
/// while `vike-tradehub` was not, and was not there.
///
/// # Why the force-add went: the roster build makes the binaries, on every plan
///
/// Since the `test` job builds the WHOLE roster (`super::Plan::roster`, the `roster=` output) and
/// selects what RUNS with a nextest filter (`tests=`), every roster crate's integration tests — and
/// therefore both companions' bins — are built on every run, whatever the plan selected. That
/// was argued from cargo's semantics when the roster build landed; it is MEASURED now. On a the latency box
/// lane (main `ec5e9d12b`), both bin sources touched, then `.github/workflows/ci.yml`'s own shape —
/// `cargo nextest run <the 68-crate roster> -E 'package(=vike-agent-eval)'`: nextest reported
/// "1 test and 725 binaries skipped", i.e. it BUILT all 735 test binaries and filtered at run
/// time; `vike-tradehub` and `vike-cli` were recompiled and re-uplifted (`target/debug/<name>`
/// mtimes past the touch); and the agent-eval suite's 74 tests passed against them.
///
/// The row had said it was kept "because a consumer that builds only `crates=` would need the
/// build half back". There is no such consumer, checked rather than assumed: every reader of the
/// plan is `.github/workflows/ci.yml`'s `test` job (all three cargo steps build `${ROSTER:?}`, pinned by
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
/// carries that argument on its own `binaries` fn, where it was paid for: it was the first shape,
/// and nextest KILLS a test that outruns the per-test budget `.config/nextest.toml` sets, so a cold
/// build of two crates turned "you have not built the binaries" into a killed test with no reason
/// attached. It is also non-hermetic — a test whose verdict depends on a compile — and it fights
/// the shared warm `target/` these runners exist to reuse: two test binaries here, each deciding to
/// build, against cargo's package lock and every other job on the box.
///
/// ⚠ A RENAME rots this table, and the failure is LOUD rather than silent — the driver's tests
/// panic naming the binary they could not find — so it carries no `compute`-time refusal the way
/// [`LIGHTGBM_CRATES`] does. `crates/vike-ops/tests/ci_plan_gate.rs` pins the names against the
/// real crate directories instead, and pins that no companion sits in [`EXCLUDE_FROM_CI`]: the
/// roster is `names - EXCLUDE_FROM_CI`, so an excluded companion is a binary no CI build makes.
pub const BINARY_DRIVER_COMPANIONS: &[(&str, &[&str])] =
    &[("vike-agent-eval", &["vike-cli", "vike-tradehub"])];

/// The GUI shell. Excluded from the test lane ([`EXCLUDE_FROM_CI`]), so its compile gate reads the
/// affected set directly.
pub const APP_CHECK_CRATE: &str = "vike-desktop";
