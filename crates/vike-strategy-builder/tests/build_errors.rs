//! `build_plugin`'s own tests. Task 5 (Track B).
//!
//! ⚠ **The tests that RUN cargo cannot run on the dev box and are slow, so those are
//! `#[ignore]`d.** This line said "they are all" until the unwired-hook refusal landed at the
//! bottom of this file: that check happens BEFORE cargo is invoked, costs nothing, and therefore
//! runs in the ordinary lane on every PR — which is the point of it, since the failure it guards
//! against is one that builds and loads cleanly and then produces plausible wrong numbers.
//! Run the ignored ones explicitly in a lane:
//!
//! ```text
//! LANE=lane2 MSYS_NO_PATHCONV=1 just the latency box <branch> cargo test -p vike-strategy-builder --test build_errors -- --ignored
//! ```
//!
//! ⚠ **`an_identical_source_is_not_rebuilt` used to be blocked on Track A's `guest::HostBroker`,
//! and that blocker is now resolved on this branch** — `crates/vike-strategy-plugin/src/guest.rs`
//! declares `HostBroker`, implementing both `Broker` and `HftBroker` (`ABI_VERSION` 2), exactly
//! the bound `template/lib.rs.in` instantiates `build::<B>` with. This test therefore now proves
//! what it always meant to: a rendered template genuinely COMPILES via the real `build_plugin`
//! pipeline. `a_syntax_error_returns_rustcs_diagnostics_verbatim` never depended on that type in
//! the first place, because a parse error in the spliced-in source aborts before any name
//! resolution happens at all.
//!
//! ⚠ **`a_cache_hit_never_invokes_cargo` is GONE, and its replacement is two tests rather than
//! one.** It planted GARBAGE bytes (not even a valid shared object) at the content-addressed path
//! and asserted `build_plugin` returned that path UNCHANGED — true of the bare `Path::exists()`
//! cache this crate shipped with, and exactly the defect this branch closes: a cache hit is now
//! `dlopen`ed and handshake-checked before being served, so the SAME garbage plant now correctly
//! triggers a rebuild instead of being handed back. `a_stale_cached_artifact_is_rebuilt_rather_than_served`
//! is that old test's planting code with the assertion flipped to match the fixed behaviour, and
//! `a_cache_hit_with_a_current_artifact_never_invokes_cargo` is the case the old test never
//! covered at all — a GENUINELY current artifact must still be served without a rebuild, proven by
//! timing and by an unchanged mtime rather than merely asserted.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use vike_strategy_builder::render::{BuildError, Profile, build_plugin, sha256_hex};
use vike_strategy_plugin::loader;

/// The checkout root, for `build_plugin`'s `workspace_root` parameter. `env!("CARGO_MANIFEST_DIR")`
/// used here only — this is a `tests/` file, one of the ~50 sites
/// `crates/vike-ops/tests/compile_time_path_gate.rs`'s own doc names as always correct ("a test
/// resolving a fixture from its own manifest directory… there is no deployment"; that gate never
/// walks `tests/` at all). Two levels up from this crate's own manifest dir
/// (`crates/vike-strategy-builder`) is the checkout root.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The `cargo` binary for `build_plugin`'s `cargo_bin` parameter — a plain `std::env::var("CARGO")`
/// read, fine in a `tests/` file (`Layer::TestOnly`, outside `LIBRARY_PIN`'s scope): `cargo test`
/// sets `CARGO` to the exact binary running this test, so the nested build uses the same
/// toolchain rather than guessing at `PATH`.
fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// These tests never LOAD what they build, so the profile is free — and `Debug` is the cheap one
/// (no LTO, no opt-level 3). A test that goes on to `dlopen` its artifact must instead build
/// under the profile its own binary was compiled with, or the fingerprint guard refuses it;
/// `crates/vike-strategy-plugin/tests/equivalence.rs` derives that from
/// `vike_strategy_plugin::fingerprint::FINGERPRINT` rather than guessing.
const TEST_PROFILE: Profile = Profile::Debug;

fn fixture_source() -> &'static str {
    r#"
use vike_model::{Bar, Broker, Strategy};

pub struct FixtureHold {
    qty: f64,
    entered: bool,
}

impl<B: Broker> Strategy<B> for FixtureHold {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        if self.entered {
            return;
        }
        let symbol = bar.symbol.clone().unwrap_or_default();
        if symbol.is_empty() {
            return;
        }
        broker.submit_market(&symbol, 1, self.qty);
        self.entered = true;
    }
}

pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let qty = params
        .get("qty")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(1.0);
    Box::new(FixtureHold { qty, entered: false })
}
"#
}

/// A compile failure must hand back rustc's OWN words. A generic "build failed" makes the user
/// guess at their own syntax error.
///
/// Independent of Track A: `-> { }` is a PARSE error (expected a type, found `{`), which the
/// compiler reports before it ever tries to resolve `guest::HostBroker` — so this is real,
/// green, lane-verifiable proof today regardless of what else is in the template.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_syntax_error_returns_rustcs_diagnostics_verbatim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = build_plugin(
        "pub fn build<B>(p: &toml::Value) -> { }",
        "broken",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        TEST_PROFILE,
    )
    .expect_err("a malformed entry must not build");
    match err {
        BuildError::Compile(msg) => {
            assert!(msg.contains("error"), "rustc's text must survive: {msg}");
            assert!(
                msg.contains("expected") || msg.contains("error["),
                "a diagnostic, not a summary: {msg}"
            );
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

/// The cache is the whole reason an unchanged strategy Runs instantly.
///
/// Requires a genuinely successful build of the rendered template, which imports
/// `vike_strategy_plugin::guest::HostBroker` — now real on this branch (see this file's header).
#[test]
#[ignore = "invokes cargo; lane-only"]
fn an_identical_source_is_not_rebuilt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = fixture_source();
    let root = workspace_root();
    let cargo = cargo_bin();
    let first =
        build_plugin(src, "cached", dir.path(), &root, &cargo, TEST_PROFILE).expect("builds");
    let stamp = std::fs::metadata(&first).unwrap().modified().unwrap();
    let second =
        build_plugin(src, "cached", dir.path(), &root, &cargo, TEST_PROFILE).expect("cache hit");
    assert_eq!(first, second, "the same source must resolve to the same artifact path");
    assert_eq!(
        stamp,
        std::fs::metadata(&second).unwrap().modified().unwrap(),
        "a cache hit must not rewrite the file"
    );
}

/// The residual this branch closes: a cached artifact that no longer matches this host's ABI
/// version or toolchain fingerprint (or is simply not a loadable shared object at all — the two
/// are indistinguishable to a cache that must decide "serve or rebuild") must be REBUILT, never
/// handed back for the loader to refuse later.
///
/// ⚠ **Supersedes the old `a_cache_hit_never_invokes_cargo`, whose premise this branch inverts.**
/// That test planted GARBAGE bytes at the content-addressed path and asserted `build_plugin`
/// returned the planted path UNCHANGED — correct for the bare `Path::exists()` cache this crate
/// shipped with, and exactly the defect described at `render::build_plugin`'s own cache-check
/// comment (an operator upgrading this service's ABI/toolchain on a warm cache got `Ok(sha)` for a
/// stale artifact and a refused Run, with no way to force a rebuild but deleting the file by
/// hand). The SAME plant now proves the opposite: this exercises the PRODUCTION `build_plugin`
/// path end to end, through a genuine `cargo` invocation, which is why it stays `#[ignore]`d
/// rather than joining the free lane below.
///
/// **This crate cannot BUILD a wrong-ABI artifact**, so the plant here is non-ELF bytes and what
/// it exercises is `plat::open`'s own failure — the arm `verify_handshake` (and therefore this
/// cache check) must treat exactly the way it treats an `AbiMismatch`/`FingerprintMismatch`:
/// "not what would be built now, rebuild", never a reason to error out or to serve garbage.
///
/// ⚠ **The two COMPARISONS are witnessed next door, not here, and not by inference.**
/// `vike-strategy-plugin`'s `tests/load_refusals.rs` already builds `bad_abi_version` and
/// `bad_fingerprint` — real cdylibs that `dlopen` PERFECTLY WELL and answer the wrong contract,
/// which is the artifact this residual was always about and the one a non-ELF plant cannot
/// stand in for. Its `verify_handshake_refuses_exactly_what_load_refuses` runs the cache check's
/// own entry point against exactly those. Read the two files as one pair of witnesses: this test
/// proves `build_plugin` REBUILDS on a failed handshake, that one proves the handshake FAILS on
/// the drift the cache exists to catch.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_stale_cached_artifact_is_rebuilt_rather_than_served() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = Profile::of_this_binary();
    let src = fixture_source();
    let sha = sha256_hex(src.as_bytes());
    let artifact_path = dir.path().join(format!("stale_cache-{sha}.so"));
    std::fs::write(
        &artifact_path,
        b"not a real cdylib -- simulates an artifact built under a different ABI/toolchain",
    )
    .expect("plant fake stale artifact");
    let stamp_before = std::fs::metadata(&artifact_path).unwrap().modified().unwrap();

    // Sanity: the planted bytes really do fail to load, or a "rebuild happened" verdict below is
    // vacuously true.
    assert!(
        loader::load(&artifact_path).is_err(),
        "the planted bytes must not already be a loadable plugin, or this test proves nothing"
    );

    let rebuilt: PathBuf =
        build_plugin(src, "stale_cache", dir.path(), &workspace_root(), &cargo_bin(), profile)
            .expect("a stale cache entry must be rebuilt, not returned as an error");
    assert_eq!(rebuilt, artifact_path, "the rebuild must still land at the wire-contract name");

    let stamp_after = std::fs::metadata(&rebuilt).unwrap().modified().unwrap();
    assert_ne!(
        stamp_before, stamp_after,
        "the file must have been rewritten -- a served-stale artifact would leave the mtime alone, \
         which is exactly what the old `a_cache_hit_never_invokes_cargo` asserted"
    );

    // The DISCRIMINATING observable: the rebuilt artifact must genuinely load, which the planted
    // garbage provably could not (asserted above). A `build_plugin` that merely re-touched the
    // stale file's mtime without really rebuilding would pass the mtime check and fail this one.
    loader::load(&rebuilt).expect(
        "the rebuilt artifact must load cleanly -- the whole point of rebuilding on a stale cache",
    );
}

/// The other half of the same discrimination: a cache hit against a CURRENT, already-loadable
/// artifact must still be served from cache — no rebuild, no rewrite — even though the check now
/// opens the file to decide that.
///
/// ⚠ **Proven by an observable, not by asserting "no rebuild happened".** Two independent signals,
/// matching this branch's own non-negotiable: the served path's mtime is byte-identical to the
/// first build's (a rebuild would rewrite the file, exactly as
/// `a_stale_cached_artifact_is_rebuilt_rather_than_served` shows above), and the second call's
/// wall-clock time stays far under what a real `cargo build` of this template costs (whole
/// seconds even warm, because the rendered crate links `vike-model`/`vike-strategy`/
/// `vike-indicators`) — a verified cache hit costs one `dlopen` plus two `dlsym`s, not a compiler
/// invocation. The threshold is generous on purpose: this proves "cargo did not run", not "the
/// dlopen was fast".
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_cache_hit_with_a_current_artifact_never_invokes_cargo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = Profile::of_this_binary();
    let src = fixture_source();
    let root = workspace_root();
    let cargo = cargo_bin();

    let first = build_plugin(src, "warm_cache", dir.path(), &root, &cargo, profile)
        .expect("first build must succeed");
    // The served artifact must already handshake — otherwise the "no rebuild" verdict below would
    // be indistinguishable from a bug that always serves the first thing it finds.
    loader::load(&first).expect("the freshly built artifact must load cleanly");
    let stamp_before = std::fs::metadata(&first).unwrap().modified().unwrap();

    let started = Instant::now();
    let second: PathBuf = build_plugin(src, "warm_cache", dir.path(), &root, &cargo, profile)
        .expect("a current cache entry must be served, not refused");
    let elapsed = started.elapsed();

    assert_eq!(first, second, "the same source must resolve to the same artifact path");
    assert_eq!(
        stamp_before,
        std::fs::metadata(&second).unwrap().modified().unwrap(),
        "a cache hit must not rewrite the file"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a cache hit took {elapsed:?} -- long enough to suggest cargo ran rather than a dlopen \
         handshake check"
    );
    loader::load(&second).expect("a served cache hit must still load cleanly");
}

/// Not a correctness test — a MEASUREMENT, run because this branch's own review demanded one
/// ("Measure it, do not estimate — a hit is the path that makes an unchanged strategy Run
/// instantly, and that property is why the cache exists"). Times raw `Path::exists()` (what a
/// cache hit cost BEFORE this branch) against raw `loader::verify_handshake()` (what it costs
/// AFTER) over the SAME real, current artifact, so the two numbers are directly comparable rather
/// than reasoned about. Printed rather than asserted on: wall-clock timings vary by box and load,
/// and the property worth gating (`a_cache_hit_with_a_current_artifact_never_invokes_cargo`,
/// above) is "orders of magnitude below a cargo invocation", not a specific nanosecond figure.
#[test]
#[ignore = "invokes cargo to produce a real artifact to measure against; lane-only"]
fn measured_cache_hit_cost_before_and_after() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = Profile::of_this_binary();
    let src = fixture_source();
    let artifact =
        build_plugin(src, "cost_probe", dir.path(), &workspace_root(), &cargo_bin(), profile)
            .expect("build once to have a real, current artifact to measure against");

    // ⚠ **COLD and WARM are timed separately, because they are different numbers and only one of
    // them is what a loop measures.** `verify_handshake` never `dlclose`s (this crate's posture —
    // see its module header), so the SECOND and every later `dlopen` of the same path finds the
    // library already mapped and does nothing but bump a refcount: a 1000-iteration average is
    // the WARM figure, and reporting it as "the cost of a cache hit" would be measuring the
    // wrong thing. The cost production actually pays the first time a given artifact is served
    // from cache is the COLD one — mapping and relocating the object — so that is timed on its
    // own, once, before the loop that can no longer reproduce it.
    let started = Instant::now();
    loader::verify_handshake(&artifact).expect("the artifact just built must still handshake");
    let verify_cold = started.elapsed();

    const ITERS: u32 = 1000;

    let started = Instant::now();
    for _ in 0..ITERS {
        assert!(artifact.exists());
    }
    let exists_total = started.elapsed();

    let started = Instant::now();
    for _ in 0..ITERS {
        loader::verify_handshake(&artifact).expect("the artifact just built must still handshake");
    }
    let verify_total = started.elapsed();

    eprintln!(
        "MEASURED cache-hit cost against a real artifact at {}: \
         BEFORE Path::exists() over {ITERS} iterations total={exists_total:?} avg={:?} -- \
         AFTER loader::verify_handshake COLD (first open, what a hit on a not-yet-mapped \
         artifact costs) ={verify_cold:?}; WARM (already-mapped, refcount only) over {ITERS} \
         iterations total={verify_total:?} avg={:?}",
        artifact.display(),
        exists_total / ITERS,
        verify_total / ITERS,
    );
}

// ── the unwired-hook refusal ─────────────────────────────────────────────────────────────────
//
// ⚠ NOT `#[ignore]`d, unlike everything above: the refusal happens BEFORE cargo is invoked, so
// these cost nothing and gate every PR. That matters more than usual here — the failure they
// guard against is one that builds, loads and handshakes cleanly and then produces plausible
// wrong numbers, which is the one shape nobody catches by reading a diff.

/// A source overriding a hook `PluginVTable` does not carry is refused, and the refusal NAMES the
/// hook. Loaded as a plugin that strategy would never receive `save_state`, so it would behave
/// differently than the identical file compiled into the binary, silently.
///
/// ⚠ **This test named `on_fill` until `ABI_VERSION` 3, and it had to move because `on_fill` is
/// WIRED now.** That is the healthy direction for this test to rot in — it went red on a PR that
/// delivered the hook rather than on one that broke something — and the replacement is chosen
/// from the three `vike_strategy_plugin::host::UNWIRED_HOOKS` still names, not from a hook that
/// merely happens to be unpopular.
#[test]
fn a_strategy_overriding_an_unwired_hook_is_refused_by_name() {
    let src = r#"
use vike_model::{Bar, Broker, Strategy};

pub struct WantsState {
    seen: usize,
}

impl<B: Broker> Strategy<B> for WantsState {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {
        self.seen += 1;
    }
    fn save_state(&self) -> Option<serde_json::Value> {
        None
    }
}

pub fn build<B: vike_model::HftBroker + 'static>(
    _params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(WantsState { seen: 0 })
}
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let err =
        build_plugin(src, "wants_state", dir.path(), &workspace_root(), &cargo_bin(), TEST_PROFILE)
            .expect_err("a source overriding an unwired hook must be refused");
    match &err {
        BuildError::UnwiredHook(hooks) => assert_eq!(
            hooks,
            &["save_state".to_string()],
            "the offending hook must be named exactly"
        ),
        other => panic!("expected UnwiredHook, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("save_state"), "the message must name the hook: {msg}");
    // A refusal that does not say what to do is a wall. Both exits must be in the text.
    assert!(msg.contains("build-time tier"), "the message must name the tier that works: {msg}");

    // ...and nothing was built. The refusal precedes cargo, so the output directory is untouched.
    let produced: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert!(produced.is_empty(), "the refusal must precede any build: {produced:?}");
}

/// The mirror: a source that overrides only WIRED hooks passes the scan. Without this the test
/// above is satisfied by a function that refuses everything.
#[test]
fn a_strategy_overriding_only_wired_hooks_passes_the_scan() {
    assert!(
        vike_strategy_builder::render::unwired_hook_overrides(fixture_source()).is_empty(),
        "the fixture overrides only `on_bar`, which IS wired"
    );
}

/// An inherent helper sharing a hook's NAME is not an override, and refusing it would be a false
/// refusal over a private identifier the host never calls. The scan is scoped to the `Strategy`
/// impl body for exactly this case.
#[test]
fn an_inherent_method_named_like_a_hook_is_not_an_override() {
    let src = r#"
use vike_model::{Bar, Broker, Strategy};

pub struct S;

impl S {
    fn on_fill(&self) -> usize { 0 }
    fn load_state(&self) -> usize { 1 }
    fn params(&self) -> usize { 2 }
}

impl<B: Broker> Strategy<B> for S {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}
"#;
    assert!(
        vike_strategy_builder::render::unwired_hook_overrides(src).is_empty(),
        "only an override inside the `Strategy` impl is a hook the host would have called"
    );
}

/// Several unwired overrides are ALL reported, not just the first — an author who fixes the one
/// name a refusal happened to mention and rebuilds should not meet a second refusal.
///
/// ⚠ This used to plant `on_start`/`on_stop`, both WIRED since `ABI_VERSION` 3. It plants
/// `load_state`/`params` now — two of the three [`vike_strategy_plugin::host::UNWIRED_HOOKS`]
/// still names, with a wired hook between them so the scan is shown skipping one rather than
/// collecting every `fn` it sees.
#[test]
fn every_unwired_override_is_reported_at_once() {
    let src = r#"
use vike_model::{Bar, Broker, Strategy};
pub struct S;
impl<B: Broker> Strategy<B> for S {
    fn load_state(&mut self, _state: &serde_json::Value) {}
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
    fn params(&self) -> Option<vike_model::StrategyParams> { None }
}
"#;
    assert_eq!(
        vike_strategy_builder::render::unwired_hook_overrides(src),
        vec!["load_state".to_string(), "params".to_string()]
    );
}

/// `artifact_dir_tag` must actually VARY with the contract it names — otherwise every caller
/// keying an output directory on it shares one directory again and the whole point is lost,
/// silently, which is exactly how `vike-studio-core`'s `plugin_run.rs` went red in CI.
///
/// Checked against its two inputs by NAME rather than by re-deriving the value, so it cannot pass
/// by computing the same wrong thing twice.
#[test]
fn the_artifact_dir_tag_names_the_abi_version_and_moves_with_the_fingerprint() {
    let tag = vike_strategy_builder::render::artifact_dir_tag();
    assert!(
        tag.contains(&format!("abi{}", vike_strategy_plugin::abi::ABI_VERSION)),
        "the tag must name the ABI version `loader::load` compares: {tag}"
    );
    assert!(
        tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "the tag keys a DIRECTORY NAME, so it must carry no separator or exotic byte: {tag}"
    );
    // The fingerprint half, shown to be load-bearing by hashing a DIFFERENT string the same way:
    // if that hash were constant — or simply omitted, as the first cut of every one of these tags
    // omitted it — these two would agree, and a toolchain change would reuse a stale directory
    // exactly as it did before the tag existed.
    use std::hash::{Hash as _, Hasher as _};
    let other = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{}-not-this-toolchain", vike_strategy_plugin::fingerprint::FINGERPRINT)
            .hash(&mut h);
        format!("abi{}-fp{:016x}", vike_strategy_plugin::abi::ABI_VERSION, h.finish())
    };
    assert_ne!(tag, other, "a different fingerprint must produce a different tag");
}

/// ...and the wired half of the same claim, stated positively: a source overriding EVERY hook the
/// vtable now carries passes the scan whole. Without this, the list could shrink to nothing and
/// the tests above would still read green — they only ever prove that SOMETHING is refused.
///
/// It asserts against `WIRED_HOOKS` rather than a list typed here, so a hook wired without being
/// declared (or declared without being wired) fails at the source of truth instead of at a copy.
#[test]
fn a_source_overriding_every_wired_hook_passes_the_scan() {
    let mut body = String::new();
    for hook in vike_strategy_plugin::host::WIRED_HOOKS {
        // Signatures are irrelevant to the scan — it reads `fn <name>(` at the start of a trimmed
        // line inside a `Strategy` impl body — and spelling fifteen real ones here would be a
        // second copy of the trait for no gain. This source is never compiled.
        body.push_str(&format!("    fn {hook}(&mut self) {{}}\n"));
    }
    let src = format!("impl<B: Broker> Strategy<B> for S {{\n{body}}}\n");
    assert!(
        vike_strategy_builder::render::unwired_hook_overrides(&src).is_empty(),
        "every WIRED_HOOKS entry must pass the scan; the refusal is for UNWIRED_HOOKS alone"
    );
}

// The version-skew guard, all three branches — see `render::verify_source_version`'s own doc for
// the whole argument. Cheap and un-ignored: every case here returns before `cargo` is ever
// invoked, so none needs `#[ignore]` the way the real-build tests above do.

/// The precondition every OTHER test in this file quietly relies on: `workspace_root()` (the real
/// checkout) carries no stamp, so every build above exercises the ABSENT-stamp path rather than a
/// coincidental match. Named directly rather than left implicit, because a stray `SOURCE_GIT_SHA`
/// left at the repo root by a manual experiment would silently move every other test in this file
/// onto a different branch of the check.
#[test]
fn the_real_checkout_carries_no_version_stamp() {
    let stamp = workspace_root().join(vike_strategy_builder::render::SOURCE_VERSION_STAMP_FILE);
    assert!(
        !stamp.exists(),
        "{} exists — every build_plugin call in this file that expects the ABSENT-stamp path to \
         run now exercises the MATCH or MISMATCH path instead, silently.",
        stamp.display()
    );
}

/// A `workspace_root` with NO stamp at all must not be refused by the version check — this is the
/// STATUS QUO the fix preserves, not a new allowance, and it is what makes shipping the check cost
/// every existing `build_plugin` caller (this file included) nothing.
#[test]
fn an_absent_stamp_is_not_refused_by_the_version_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    // No SOURCE_GIT_SHA written at all. `verify_source_version` must return `Ok` and let the
    // UNWIRED_HOOKS check run next, which is what actually rejects this bogus source+root pair —
    // proving the version check specifically did not.
    let out = tempfile::tempdir().expect("tempdir");
    let err = build_plugin(
        "impl<B: Broker> Strategy<B> for S { fn on_start(&mut self) {} }",
        "absent_stamp_probe",
        out.path(),
        dir.path(),
        &cargo_bin(),
        TEST_PROFILE,
    )
    .expect_err("a bogus workspace_root with none of the real crates cannot possibly succeed");
    assert!(
        !matches!(err, BuildError::SourceVersionMismatch(_)),
        "an ABSENT stamp must not be refused by the version check; got {err:?}"
    );
}

/// A `workspace_root` stamped with THIS binary's own commit must not be refused either — the
/// MATCH path, proven the same way as the absent one: a bogus root fails LATER, on something else.
#[test]
fn a_stamp_matching_the_binarys_own_commit_is_not_refused_by_the_version_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(vike_strategy_builder::render::SOURCE_VERSION_STAMP_FILE),
        vike_buildinfo::GIT_SHA,
    )
    .expect("write stamp");
    let out = tempfile::tempdir().expect("tempdir");
    let err = build_plugin(
        "fn not_even_valid_rust(",
        "match_stamp_probe",
        out.path(),
        dir.path(),
        &cargo_bin(),
        TEST_PROFILE,
    )
    .expect_err("a bogus workspace_root with none of the real crates cannot possibly succeed");
    assert!(
        !matches!(err, BuildError::SourceVersionMismatch(_)),
        "a MATCHING stamp must not be refused by the version check; got {err:?}"
    );
}

/// The one case this whole mechanism exists for: a PRESENT stamp naming a DIFFERENT commit is
/// refused BEFORE cargo runs, with both values named in the message.
#[test]
fn a_stamp_naming_a_different_commit_is_refused_before_cargo_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A syntactically SHA-shaped value that is (astronomically) never the real running binary's
    // own commit.
    let bogus_sha = "0000000";
    assert_ne!(
        bogus_sha,
        vike_buildinfo::GIT_SHA,
        "the planted mismatch collided with the real value — the test proves nothing"
    );
    std::fs::write(
        dir.path().join(vike_strategy_builder::render::SOURCE_VERSION_STAMP_FILE),
        bogus_sha,
    )
    .expect("write stamp");
    let out = tempfile::tempdir().expect("tempdir");
    let err = build_plugin(
        "fn x() {}",
        "mismatch_stamp_probe",
        out.path(),
        dir.path(),
        &cargo_bin(),
        TEST_PROFILE,
    )
    .expect_err("a stamp naming a different commit must refuse before cargo ever runs");
    match err {
        BuildError::SourceVersionMismatch(msg) => {
            assert!(msg.contains(bogus_sha), "message must name the source's stamped value: {msg}");
            assert!(
                msg.contains(vike_buildinfo::GIT_SHA),
                "message must name the binary's own commit: {msg}"
            );
        }
        other => panic!("expected SourceVersionMismatch, got {other:?}"),
    }
}

/// The two sides of this mechanism live in two different LANGUAGES (a GitHub Actions YAML step, a
/// Rust constant), so nothing can derive one from the other — they simply have to agree, the same
/// shape `deploy_layout_gate.rs`'s `PLUGINS_REL`/`STORE_REL` literals already carry for a fact
/// duplicated across languages.
#[test]
fn the_release_workflow_stamps_the_spelling_this_crate_reads() {
    let release_yml_path = workspace_root().join(".github").join("workflows").join("release.yml");
    let release_yml = std::fs::read_to_string(&release_yml_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", release_yml_path.display()));
    let stamp_name = vike_strategy_builder::render::SOURCE_VERSION_STAMP_FILE;
    assert!(
        release_yml.contains(stamp_name),
        "{} no longer names `{stamp_name}` — the release packaging step and \
         `render::verify_source_version` must spell the same stamp filename, and nothing derives \
         one from the other across a YAML step and a Rust constant.",
        release_yml_path.display()
    );
}
