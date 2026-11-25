//! `build_strategy_file`'s own tests. Task 5 (Track B).
//!
//! ⚠ **The tests that RUN cargo cannot run on the dev box and are slow, so those are
//! `#[ignore]`d.** This line said "they are all" until the unwired-hook refusal landed at the
//! bottom of this file: that check happens BEFORE cargo is invoked, costs nothing, and therefore
//! runs in the ordinary lane on every PR — which is the point of it, since the failure it guards
//! against is one that builds and loads cleanly and then produces plausible wrong numbers.
//! Run the ignored ones explicitly in a lane:
//!
//! ```text
//! LANE=the build runner MSYS_NO_PATHCONV=1 just the latency box <branch> cargo test -p vike-strategy-builder --test integration -- --ignored build_errors::
//! ```
//!
//! ⚠ **`an_identical_source_is_not_rebuilt` used to be blocked on Track A's `guest::HostBroker`,
//! and that blocker is now resolved on this branch** — `crates/vike-rust-strategy/src/guest.rs`
//! declares `HostBroker`, implementing `OrderBroker` (since `ABI_VERSION` 8; `HftBroker` from 2 to
//! 7), exactly the bound `template/lib.rs.in` instantiates `build::<B>` with. This test therefore now proves
//! what it always meant to: a rendered template genuinely COMPILES via the real `build_strategy_file`
//! pipeline. `a_syntax_error_returns_rustcs_diagnostics_verbatim` never depended on that type in
//! the first place, because a parse error in the spliced-in source aborts before any name
//! resolution happens at all.
//!
//! ⚠ **`a_cache_hit_never_invokes_cargo` is GONE, and its replacement is two tests rather than
//! one.** It planted GARBAGE bytes (not even a valid shared object) at the content-addressed path
//! and asserted `build_strategy_file` returned that path UNCHANGED — true of the bare `Path::exists()`
//! cache this crate shipped with, and exactly the defect this branch closes: a cache hit is now
//! `dlopen`ed and handshake-checked before being served, so the SAME garbage plant now correctly
//! triggers a rebuild instead of being handed back. `a_stale_cached_artifact_is_rebuilt_rather_than_served`
//! is that old test's planting code with the assertion flipped to match the fixed behaviour, and
//! `a_cache_hit_with_a_current_artifact_never_invokes_cargo` is the case the old test never
//! covered at all — a GENUINELY current artifact must still be served without a rebuild, proven by
//! timing and by an unchanged mtime rather than merely asserted.

use std::assert_matches;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vike_hub_protocol::hash::sha256_hex;
use vike_model::host_build::row_symbol::symbol_edits;
use vike_model::host_build::source_edits::{SourceEdits, apply_edits};
use vike_model::host_build::state_signature::state_edits;
use vike_rust_strategy::loader;
use vike_rust_strategy::sdk::TemplateKind;
use vike_strategy_builder::render::{
    self, BuildError, Hooks, Presence, Profile, build_strategy_file,
};

use crate::checkout_shape;

/// Where `build_strategy_file` puts the artifact of a strategy file `text` built from the checkout into
/// `builds` under `profile`: `<builds>/sdk-dev/<folder>/<sha256>.so` (`render::build_folder`).
fn artifact_of(builds: &Path, text: &str, profile: Profile) -> PathBuf {
    let (sdk, folder) = render::build_folder(&workspace_root(), TemplateKind::Strategy, profile)
        .expect("the checkout is a source tree");
    vike_rust_strategy::artifact::artifact_path(builds, sdk, &folder, &sha256_hex(text.as_bytes()))
}

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-ops/tests/architecture/parity_fold_gate.rs`'s `repo_root`, do not.
/// The checkout root, for `build_strategy_file`'s `workspace_root` parameter. `env!("CARGO_MANIFEST_DIR")`
/// used here only — this is a `tests/` file, one of the ~50 sites
/// `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs`'s own doc names as always correct ("a test
/// resolving a fixture from its own manifest directory… there is no deployment"; that gate never
/// walks `tests/` at all). Two levels up from this crate's own manifest dir
/// (`crates/vike-strategy-builder`) is the checkout root.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The `cargo` binary for `build_strategy_file`'s `cargo_bin` parameter — a plain `std::env::var("CARGO")`
/// read, fine in a `tests/` file (`Layer::TestOnly`, outside `LIBRARY_PIN`'s scope): `cargo test`
/// sets `CARGO` to the exact binary running this test, so the nested build uses the same
/// toolchain rather than guessing at `PATH`.
fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// The environment the nested `cargo` gets — the SERVICE's own allowlist
/// (`render::child_env`) applied to this test's environment, so a build here runs under the same
/// cleared environment a deployed build does. A sweep names no variable, so it adds no read site.
///
/// `CARGO_HOME` is ensured (the build jail needs it, as a deployed unit always sets it): the real
/// one if set, else `$HOME/.cargo`. A build here runs `--offline` against it, so it must be the WARM
/// cargo home the lane already built the workspace with — the fallback is that home, never a fresh
/// scratch cargo would have to fill over the network the jail has closed.
fn child_env() -> Vec<(String, String)> {
    let mut vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    if !vars.get("CARGO_HOME").is_some_and(|v| !v.trim().is_empty())
        && let Some(home) = vars.get("HOME").filter(|h| !h.trim().is_empty())
    {
        vars.insert("CARGO_HOME".to_string(), format!("{home}/.cargo"));
    }
    render::child_env(&vars)
}

/// This file's own scratch `CARGO_HOME` (`workspace_root`'s sibling setup) is not what
/// `CargoHomePin` exists to police — that guard's own tests live beside it in `render.rs` and
/// exercise `CargoHomePin::capture`/`check` directly. `disabled()` documents which one this is.
fn no_pin() -> render::CargoHomePin {
    render::CargoHomePin::disabled()
}

/// These tests never LOAD what they build, so the profile is free — and `Debug` is the cheap one
/// (no opt-level 3). A test that goes on to `dlopen` its artifact must instead build
/// under the profile its own binary was compiled with, or the fingerprint guard refuses it;
/// `crates/vike-rust-strategy/tests/equivalence.rs` derives that from
/// `vike_rust_strategy::fingerprint::FINGERPRINT` rather than guessing.
const TEST_PROFILE: Profile = Profile::Debug;

fn fixture_source() -> &'static str {
    r#"
use vike_model::{OrderBroker, Strategy, OrderPlacement};
use vike_marketdata::Bar;

pub struct FixtureHold {
    qty: f64,
    entered: bool,
}

impl<B: OrderBroker> Strategy<B> for FixtureHold {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        if self.entered {
            return;
        }
        let symbol = bar.symbol.clone().unwrap_or_default();
        if symbol.is_empty() {
            return;
        }
        broker.place(&symbol, "", 1, self.qty, OrderPlacement::Market, false);
        self.entered = true;
    }
}

pub fn build<B: vike_model::OrderBroker + 'static>(
    params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let qty = params
        .get("qty")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(1.0);
    Box::new(FixtureHold { qty, entered: false })
}
"#
}

/// A source overriding `save_state` and `load_state`, the restart-state pair a strategy file could
/// not carry before `ABI_VERSION` 8 (`build_strategy_file` refused it, before anything touched the disk or
/// spawned `cargo`). Both cross now, so the hook scan passes it.
const SAVE_STATE_SOURCE: &str = r#"
use vike_model::{OrderBroker, Strategy};
use vike_marketdata::Bar;

pub struct WantsState {
    seen: usize,
}

impl<B: OrderBroker> Strategy<B> for WantsState {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {
        self.seen += 1;
    }
    fn save_state(&self) -> Option<vike_model::StrategyState> {
        vike_model::StrategyState::of(&self.seen).ok()
    }
    fn load_state(&mut self, state: &vike_model::StrategyState) {
        self.seen = state.read().unwrap_or(0);
    }
}

pub fn build<B: vike_model::OrderBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(WantsState { seen: 0 })
}
"#;

/// A source on the JSON state signature of before `ABI_VERSION` 15, in the shapes the two shipped makers
/// had (`serde_json::to_value(&dto).ok()`, `serde_json::from_value(state.clone())`), plus the `json!`
/// one and an unused parameter: it no longer builds, and its migration must.
const JSON_STATE_SOURCE: &str = r#"
use serde::{Deserialize, Serialize};
use vike_marketdata::Bar;
use vike_model::{OrderBroker, Strategy};

#[derive(Serialize, Deserialize)]
struct Saved {
    v: u32,
    seen: usize,
}

pub struct WantsState {
    seen: usize,
}

impl<B: OrderBroker> Strategy<B> for WantsState {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {
        self.seen += 1;
    }
    fn save_state(&self) -> Option<serde_json::Value> {
        let dto = Saved { v: 1, seen: self.seen };
        serde_json::to_value(&dto).ok()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        let dto: Saved = match serde_json::from_value(state.clone()) {
            Ok(dto) => dto,
            Err(err) => {
                tracing::warn!(%err, "unparsable saved state, keeping fresh");
                return;
            }
        };
        self.seen = dto.seen;
    }
}

pub struct Counter {
    pub n: u64,
}

impl<B: OrderBroker> Strategy<B> for Counter {
    fn save_state(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "n": self.n }))
    }
    fn load_state(&mut self, _state: &serde_json::Value) {}
}

pub fn build<B: vike_model::OrderBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(WantsState { seen: 0 })
}
"#;

/// A source whose `build` still bounds `B: vike_model::HftBroker` (`ABI_VERSION` 7 and before): it
/// no longer compiles, since the strategy file's broker implements `OrderBroker` alone, and the
/// failure must carry the one-line fix after rustc's text.
const HFT_BROKER_SOURCE: &str = r#"
use vike_model::{HftBroker, Strategy};
use vike_marketdata::Bar;

pub struct Old;

impl<B: HftBroker> Strategy<B> for Old {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}

pub fn build<B: vike_model::HftBroker + 'static>(
    _params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    Box::new(Old)
}
"#;

/// A `CargoHomePin` captured over a scratch `CARGO_HOME` whose `config.toml` is PRESENT, and then
/// DRIFTED by an edit, so `build_strategy_file` refuses with `CargoHomeTampered { was_present: true,
/// now_present: true }` at the first check AFTER the cache — before it creates a
/// directory or spawns `cargo`. Two uses: a test that must prove an EARLIER check PASSED asserts that
/// exact refusal; a test that expects an earlier refusal carries it anyway, so a regression in its
/// check is refused here too instead of running a nested build. (The absent-then-present drift is
/// `a_build_is_refused_once_cargo_home_config_drifts_from_its_pin`'s own case.) The `TempDir` is
/// returned so it outlives the call.
fn drifted_pin() -> (tempfile::TempDir, render::CargoHomePin) {
    let home = tempfile::tempdir().expect("tempdir");
    let config = home.path().join("config.toml");
    std::fs::write(&config, "# the operator's config, as the pin captured it\n").expect("config");
    let pin = render::CargoHomePin::capture(home.path());
    std::fs::write(&config, "# edited after the capture\n").expect("drift");
    (home, pin)
}

/// The exact refusal [`drifted_pin`] produces: present at capture, present (and different) now.
fn is_drifted_pin_refusal(err: &BuildError) -> bool {
    matches!(
        err,
        BuildError::CargoHomeTampered { was_present: Presence(true), now_present: Presence(true) }
    )
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
    let err = build_strategy_file(
        "pub fn build<B>(p: &vike_model::params::Params) -> { }",
        "broken.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        TEST_PROFILE,
        &no_pin(),
    )
    .expect_err("a malformed entry must not build");
    match err {
        BuildError::Compile { diagnostics: msg } => {
            assert!(msg.contains("error"), "rustc's text must survive: {msg}");
            assert!(
                msg.contains("expected") || msg.contains("error["),
                "a diagnostic, not a summary: {msg}"
            );
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

/// A source that still imports a market-data name through `vike_model` and fails to compile gets
/// the moved-name hint AFTER rustc's own text, through the real `build_strategy_file` pipeline. The
/// deliberate type error makes the build fail whether or not the old import still resolves; the
/// old spelling is built from parts because the market-data codemod rewrites this file's sources.
/// The no-hint side (the new spelling) is `render.rs`'s own unit test, which needs no cargo.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_failed_build_of_an_old_market_data_import_carries_the_moved_name_hint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = format!(
        "use {}::{{Bar, Broker, Strategy}};\npub fn broken() -> u8 {{ \"not a u8\" }}\n",
        "vike_model"
    );
    let err = build_strategy_file(
        &src,
        "moved_name.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        TEST_PROFILE,
        &no_pin(),
    )
    .expect_err("a type error must not build");
    match err {
        BuildError::Compile { diagnostics: msg } => {
            assert!(msg.contains("error"), "rustc's text must survive: {msg}");
            assert!(
                msg.contains(
                    "\nhint: `Bar` moved from `vike_model` to `vike_marketdata`: write \
                     `use vike_marketdata::Bar;`"
                ),
                "the hint follows rustc's text: {msg}"
            );
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

/// **A file on the JSON state signature fails with the migrate hint, and its migration BUILDS**: the
/// builder prints the state hint after rustc's text, and the text `vike-cli strategy migrate` writes
/// for it (`vike_model::host_build::state_signature`'s edits) builds through the real pipeline; a
/// second pass over it finds nothing.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_file_on_the_json_state_signature_draws_the_migrate_hint_and_its_migration_builds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let build = |src: &str| {
        build_strategy_file(
            src,
            "json_state.rs",
            dir.path(),
            &workspace_root(),
            &cargo_bin(),
            &child_env(),
            TEST_PROFILE,
            &no_pin(),
        )
    };
    match build(JSON_STATE_SOURCE).expect_err("the JSON signature no longer builds") {
        BuildError::Compile { diagnostics: msg } => {
            assert!(msg.contains("error"), "rustc's text must survive: {msg}");
            assert!(
                msg.contains(
                    "\nhint: `save_state` and `load_state` carry the strategy state as \
                     `vike_model::StrategyState` (msgpack) since ABI 15"
                ) && msg.contains("`vike-cli strategy migrate <file>`"),
                "the state hint follows rustc's text: {msg}"
            );
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let found = state_edits(JSON_STATE_SOURCE);
    assert!(found.hints.is_empty(), "every hook is a shape migrate rewrites: {:?}", found.hints);
    let migrated = apply_edits(JSON_STATE_SOURCE, &found.edits);
    build(&migrated).expect("the migrated file builds");
    assert_eq!(state_edits(&migrated), SourceEdits::default(), "a second pass finds nothing");
}

/// A strategy written against the `String` rows (before the SDK of 2026-11-20): it reads a bar's and a
/// quote's symbol back as a `String` and writes one into a row, the shapes the shipped files had.
const STRING_SYMBOL_SOURCE: &str = r#"
use vike_marketdata::{Bar, TickQuote, TickTrade};
use vike_model::{OrderBroker, Strategy};

fn symbol_key(instrument: &Option<String>) -> String {
    instrument.clone().unwrap_or_default()
}

pub struct ReadsSymbols {
    pub symbol: Option<String>,
    pub last: Option<TickTrade>,
}

impl<B: OrderBroker> Strategy<B> for ReadsSymbols {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let symbol: String = self.symbol.clone().or_else(|| bar.symbol.clone()).unwrap_or_default();
        let key = symbol_key(&bar.symbol);
        if !symbol.is_empty() && key == symbol {
            let _ = broker.position(&symbol);
        }
    }
    fn on_quote_tick(&mut self, _broker: &mut B, q: &TickQuote) {
        let symbol: String = self.symbol.clone().unwrap_or_else(|| q.symbol.clone());
        self.last = Some(TickTrade {
            timestamp: q.timestamp,
            local_timestamp: 0,
            price: q.mid(),
            size: 1.0,
            is_buyer_maker: false,
            symbol,
        });
    }
}

pub fn build<B: OrderBroker + 'static>(_params: &vike_model::params::Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(ReadsSymbols { symbol: None, last: None })
}
"#;

/// **A file that reads a row's symbol as a `String` fails with the hint, and its migration BUILDS**: the
/// builder prints the symbol hint after rustc's text, and the text `vike-cli strategy migrate` writes for it
/// (`vike_model::host_build::row_symbol`'s edits, M1 to M3) builds through the real pipeline on this
/// checkout's `Symbol` rows; a second pass over it finds nothing.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_file_reading_a_row_symbol_as_a_string_draws_the_hint_and_its_migration_builds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let build = |src: &str| {
        build_strategy_file(
            src,
            "string_symbol.rs",
            dir.path(),
            &workspace_root(),
            &cargo_bin(),
            &child_env(),
            TEST_PROFILE,
            &no_pin(),
        )
    };
    match build(STRING_SYMBOL_SOURCE).expect_err("a `String` read of a `Symbol` no longer builds") {
        BuildError::Compile { diagnostics: msg } => {
            assert!(msg.contains("error"), "rustc's text must survive: {msg}");
            assert!(
                msg.contains("\nhint: a bar's `symbol` is an `Option<vike_marketdata::Symbol>`")
                    && msg.contains("`vike-cli strategy migrate <file>`"),
                "the symbol hint follows rustc's text: {msg}"
            );
        }
        other => panic!("wrong variant: {other:?}"),
    }
    let found = symbol_edits(STRING_SYMBOL_SOURCE);
    assert!(found.hints.is_empty(), "every read is a shape migrate rewrites: {:?}", found.hints);
    assert_eq!(found.edits.len(), 4, "M1 twice, M2 and M3: {:?}", found.edits);
    let migrated = apply_edits(STRING_SYMBOL_SOURCE, &found.edits);
    build(&migrated).expect("the migrated file builds");
    assert_eq!(symbol_edits(&migrated), SourceEdits::default(), "a second pass finds nothing");
}

/// The cache is the whole reason an unchanged strategy Runs instantly.
///
/// Requires a genuinely successful build of the rendered template, which imports
/// `vike_rust_strategy::guest::HostBroker` — now real on this branch (see this file's header).
#[test]
#[ignore = "invokes cargo; lane-only"]
fn an_identical_source_is_not_rebuilt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = fixture_source();
    let root = workspace_root();
    let cargo = cargo_bin();
    let first = build_strategy_file(
        src,
        "cached.rs",
        dir.path(),
        &root,
        &cargo,
        &child_env(),
        TEST_PROFILE,
        &no_pin(),
    )
    .expect("builds");
    let stamp = std::fs::metadata(&first).unwrap().modified().unwrap();
    let second = build_strategy_file(
        src,
        "cached.rs",
        dir.path(),
        &root,
        &cargo,
        &child_env(),
        TEST_PROFILE,
        &no_pin(),
    )
    .expect("cache hit");
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
/// That test planted GARBAGE bytes at the content-addressed path and asserted `build_strategy_file`
/// returned the planted path UNCHANGED — correct for the bare `Path::exists()` cache this crate
/// shipped with, and exactly the defect described at `render::build_strategy_file`'s own cache-check
/// comment (an operator upgrading this service's ABI/toolchain on a warm cache got `Ok(sha)` for a
/// stale artifact and a refused Run, with no way to force a rebuild but deleting the file by
/// hand). The SAME plant now proves the opposite: this exercises the PRODUCTION `build_strategy_file`
/// path end to end, through a genuine `cargo` invocation, which is why it stays `#[ignore]`d
/// rather than joining the free lane below.
///
/// **This crate cannot BUILD a wrong-ABI artifact**, so the plant here is non-ELF bytes and what
/// it exercises is the record reader's own refusal — the arm `loader::check_identity` (and therefore
/// this cache check) must treat exactly the way it treats an `AbiMismatch`/`FingerprintMismatch`:
/// "not what would be built now, rebuild", never a reason to error out or to serve garbage.
///
/// ⚠ **The two COMPARISONS are witnessed next door, not here, and not by inference.**
/// `vike-rust-strategy`'s `tests/load_refusals.rs` already builds `bad_abi_version` and
/// `bad_fingerprint` — real cdylibs whose records read PERFECTLY WELL and state the wrong contract,
/// which is the artifact this residual was always about and the one a non-ELF plant cannot
/// stand in for. Its `the_cache_check_refuses_exactly_what_load_refuses` runs the cache check's
/// own entry point against exactly those. Read the two files as one pair of witnesses: this test
/// proves `build_strategy_file` REBUILDS on a failed record check, that one proves the check FAILS on
/// the drift the cache exists to catch.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_stale_cached_artifact_is_rebuilt_rather_than_served() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = Profile::of_this_binary();
    let src = fixture_source();
    let artifact_path = artifact_of(dir.path(), src, profile);
    std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("the build folder");
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
        "the planted bytes must not already be a loadable strategy file, or this test proves nothing"
    );

    let rebuilt: PathBuf = build_strategy_file(
        src,
        "stale_cache.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        profile,
        &no_pin(),
    )
    .expect("a stale cache entry must be rebuilt, not returned as an error");
    assert_eq!(rebuilt, artifact_path, "the rebuild must still land at its folder and sha");

    let stamp_after = std::fs::metadata(&rebuilt).unwrap().modified().unwrap();
    assert_ne!(
        stamp_before, stamp_after,
        "the file must have been rewritten -- a served-stale artifact would leave the mtime alone, \
         which is exactly what the old `a_cache_hit_never_invokes_cargo` asserted"
    );

    // The DISCRIMINATING observable: the rebuilt artifact must genuinely load, which the planted
    // garbage provably could not (asserted above). A `build_strategy_file` that merely re-touched the
    // stale file's mtime without really rebuilding would pass the mtime check and fail this one.
    loader::load(&rebuilt).expect(
        "the rebuilt artifact must load cleanly -- the whole point of rebuilding on a stale cache",
    );
}

/// The other half of the same discrimination: a cache hit against a CURRENT, already-loadable
/// artifact must still be served from cache — no rebuild, no rewrite — even though the check now
/// reads the file's record to decide that.
///
/// ⚠ **Proven by an observable, not by asserting "no rebuild happened".** Two independent signals,
/// matching this branch's own non-negotiable: the served path's mtime is byte-identical to the
/// first build's (a rebuild would rewrite the file, exactly as
/// `a_stale_cached_artifact_is_rebuilt_rather_than_served` shows above), and the second call's
/// wall-clock time stays far under what a real `cargo build` of this template costs (whole
/// seconds even warm, because the rendered crate links `vike-model`/`vike-strategy`/
/// `vike-indicators`) — a verified cache hit costs one bounded read of the file's record, not a
/// compiler invocation. The threshold is generous on purpose: this proves "cargo did not run", not
/// "the read was fast".
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_cache_hit_with_a_current_artifact_never_invokes_cargo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = Profile::of_this_binary();
    let src = fixture_source();
    let root = workspace_root();
    let cargo = cargo_bin();

    let first = build_strategy_file(
        src,
        "warm_cache.rs",
        dir.path(),
        &root,
        &cargo,
        &child_env(),
        profile,
        &no_pin(),
    )
    .expect("first build must succeed");
    // The served artifact must already handshake — otherwise the "no rebuild" verdict below would
    // be indistinguishable from a bug that always serves the first thing it finds.
    loader::load(&first).expect("the freshly built artifact must load cleanly");
    let stamp_before = std::fs::metadata(&first).unwrap().modified().unwrap();
    // The use stamp: backdate the access time, so the hit below must be what moves it forward.
    let long_ago = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(&first)
        .and_then(|f| f.set_times(std::fs::FileTimes::new().set_accessed(long_ago)))
        .expect("backdate the access time");
    let asked_at = std::time::SystemTime::now();

    let started = Instant::now();
    let second: PathBuf = build_strategy_file(
        src,
        "warm_cache.rs",
        dir.path(),
        &root,
        &cargo,
        &child_env(),
        profile,
        &no_pin(),
    )
    .expect("a current cache entry must be served, not refused");
    let elapsed = started.elapsed();

    assert_eq!(first, second, "the same source must resolve to the same artifact path");
    assert_eq!(
        stamp_before,
        std::fs::metadata(&second).unwrap().modified().unwrap(),
        "a cache hit must not rewrite the file"
    );
    // Retention evicts by last use (`builder::prune`), so a hit must record one.
    let used = std::fs::metadata(&second).unwrap().accessed().unwrap();
    assert!(
        used >= asked_at - Duration::from_secs(1),
        "a cache hit must stamp the artifact's access time: {used:?}, asked at {asked_at:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a cache hit took {elapsed:?} -- long enough to suggest cargo ran rather than a record \
         check"
    );
    loader::load(&second).expect("a served cache hit must still load cleanly");
}

/// Not a correctness test — a MEASUREMENT, run because this branch's own review demanded one
/// ("Measure it, do not estimate — a hit is the path that makes an unchanged strategy Run
/// instantly, and that property is why the cache exists"). Times raw `Path::exists()` (what a
/// cache hit cost BEFORE the check) against raw `loader::check_identity()` (what it costs now, the
/// record read) over the SAME real, current artifact, so the two numbers are directly comparable rather
/// than reasoned about. Printed rather than asserted on: wall-clock timings vary by box and load,
/// and the property worth gating (`a_cache_hit_with_a_current_artifact_never_invokes_cargo`,
/// above) is "orders of magnitude below a cargo invocation", not a specific nanosecond figure.
#[test]
#[ignore = "invokes cargo to produce a real artifact to measure against; lane-only"]
fn measured_cache_hit_cost_before_and_after() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = Profile::of_this_binary();
    let src = fixture_source();
    let artifact = build_strategy_file(
        src,
        "cost_probe.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        profile,
        &no_pin(),
    )
    .expect("build once to have a real, current artifact to measure against");

    // ⚠ **COLD and WARM are timed separately, because they are different numbers and only one of
    // them is what a loop measures.** The first read of a file's record may reach the disk; every
    // later one finds its pages cached, so a 1000-iteration average is the WARM figure. The first
    // is timed on its own, once, before the loop that can no longer reproduce it.
    let started = Instant::now();
    loader::check_identity(&artifact).expect("the artifact just built must still match");
    let verify_cold = started.elapsed();

    const ITERS: u32 = 1000;

    let started = Instant::now();
    for _ in 0..ITERS {
        assert!(artifact.exists());
    }
    let exists_total = started.elapsed();

    let started = Instant::now();
    for _ in 0..ITERS {
        loader::check_identity(&artifact).expect("the artifact just built must still match");
    }
    let verify_total = started.elapsed();

    eprintln!(
        "MEASURED cache-hit cost against a real artifact at {}: \
         BEFORE Path::exists() over {ITERS} iterations total={exists_total:?} avg={:?} -- \
         AFTER loader::check_identity COLD (first read) ={verify_cold:?}; WARM (pages cached) \
         over {ITERS} iterations total={verify_total:?} avg={:?}",
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
//
// ⚠ `UNWIRED_HOOKS` names `failures` alone (the host's own count, never the file's), so the scan's own
// properties are held over a list given here (`render::hook_overrides`), the end-to-end test below holds
// the other direction, that the hooks 8 wired are no longer refused, and one source overriding
// `failures` holds the real list.

/// A source overriding `save_state` and `load_state`, the pair a strategy file could not carry
/// before `ABI_VERSION` 8, is NOT refused: the hook scan passes it and the build reaches the next
/// check (here the drifted cargo pin, so no nested build of the real checkout runs).
#[test]
fn a_strategy_overriding_the_restart_state_hooks_is_no_longer_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_home, pin) = drifted_pin();
    let err = build_strategy_file(
        SAVE_STATE_SOURCE,
        "wants_state.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        TEST_PROFILE,
        &pin,
    )
    .expect_err("the drifted pin refuses the build after the hook scan");
    assert!(
        is_drifted_pin_refusal(&err),
        "the hook scan must pass the restart-state pair and leave the refusal to the pin: {err:?}"
    );
    assert!(vike_strategy_builder::render::unwired_hook_overrides(SAVE_STATE_SOURCE).is_empty());
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

/// A source overriding `failures` is refused by the real list: what a strategy's hooks failed with is
/// its host's count (`vike_rust_strategy::host::StrategyFileHost`), and a file's own answer would only
/// lie in its report. An inherent `failures` helper is the source's own and passes.
#[test]
fn a_strategy_overriding_failures_is_refused_by_the_scan() {
    let src = r#"
impl S {
    fn failures(&self) -> usize { 0 }
}

impl<B: OrderBroker> Strategy<B> for S {
    fn failures(&self) -> vike_model::StrategyFailures { Default::default() }
}
"#;
    assert_eq!(vike_strategy_builder::render::unwired_hook_overrides(src), ["failures"]);
    let inherent = "impl S {\n    fn failures(&self) -> usize { 0 }\n}\n";
    assert!(vike_strategy_builder::render::unwired_hook_overrides(inherent).is_empty());
}

/// An inherent helper sharing a hook's NAME is not an override, and refusing it would be a false
/// refusal over a private identifier the host never calls. The scan is scoped to the `Strategy`
/// impl body for exactly this case.
#[test]
fn an_inherent_method_named_like_a_hook_is_not_an_override() {
    let src = r#"
use vike_model::{OrderBroker, Strategy};
use vike_marketdata::Bar;

pub struct S;

impl S {
    fn on_fill(&self) -> usize { 0 }
    fn load_state(&self) -> usize { 1 }
    fn params(&self) -> usize { 2 }
}

impl<B: OrderBroker> Strategy<B> for S {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}
"#;
    assert!(
        vike_strategy_builder::render::hook_overrides(src, &["on_fill", "load_state", "params"])
            .is_empty(),
        "only an override inside the `Strategy` impl is a hook the host would have called"
    );
}

/// Several listed overrides are ALL reported, not just the first — an author who fixes the one
/// name a refusal happened to mention and rebuilds should not meet a second refusal. A hook the
/// list does not name, between them, is skipped rather than collected.
#[test]
fn every_listed_override_is_reported_at_once() {
    let src = r#"
use vike_model::{OrderBroker, Strategy};
use vike_marketdata::Bar;
pub struct S;
impl<B: OrderBroker> Strategy<B> for S {
    fn load_state(&mut self, _state: &vike_model::StrategyState) {}
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
    fn params(&self) -> Option<vike_model::StrategyParams> { None }
}
"#;
    assert_eq!(
        vike_strategy_builder::render::hook_overrides(src, &["params", "load_state"]),
        vec!["load_state".to_string(), "params".to_string()]
    );
}

/// The refusal's words, which a user reads when a Build is refused: they name every hook, say both
/// ways forward and name no internal mechanism. No hook is unwired today, so it is held on the
/// variant itself.
#[test]
fn the_unwired_hook_refusal_names_the_hooks_and_both_ways_forward() {
    let msg = BuildError::UnwiredHook { hooks: Hooks(vec!["save_state".to_string()]) }.to_string();
    assert!(msg.contains("save_state"), "the message must name the hook: {msg}");
    assert!(msg.contains("build-time tier"), "the message must name the tier that works: {msg}");
    assert!(!msg.to_lowercase().contains("plugin"), "a user reads this, no internal name: {msg}");
}

/// A source whose `build` still bounds `B: vike_model::HftBroker` fails to compile against a
/// strategy file's broker since `ABI_VERSION` 8, and the failure carries the one-line fix after
/// rustc's own text, through the real `build_strategy_file` pipeline.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_failed_build_of_an_hft_broker_source_carries_the_renamed_name_hint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = build_strategy_file(
        HFT_BROKER_SOURCE,
        "hft_broker.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        TEST_PROFILE,
        &no_pin(),
    )
    .expect_err("a `build` bounding `HftBroker` must not build against the one broker interface");
    match err {
        BuildError::Compile { diagnostics: msg } => {
            assert!(msg.contains("error"), "rustc's text must survive: {msg}");
            assert!(
                msg.contains(
                    "\nhint: `HftBroker` is now `OrderBroker` in `vike_model`: write the new name \
                     wherever the file names the old one\n"
                ),
                "the hint follows rustc's text: {msg}"
            );
        }
        other => panic!("wrong variant: {other:?}"),
    }
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
    for hook in vike_rust_strategy::host::WIRED_HOOKS {
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

// The source tree a build compiles against (decision 0296: an installed SDK or a git checkout, it
// replaced the version stamp) — see `render::source_tree_refusal`. Cheap and un-ignored: every case
// here returns before `cargo` is ever invoked.

/// A tree that is neither an installed SDK (no marker) nor a checkout (no `.git`) is refused before
/// cargo runs, in words naming the marker and the `--sdk-root` fix.
#[test]
fn a_tree_that_is_neither_an_sdk_nor_a_checkout_is_refused_before_cargo_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = tempfile::tempdir().expect("tempdir");
    // A drifted pin, though the refusal expected here comes EARLIER: a regression in the tree check
    // is then refused at the pin rather than going on to a nested `cargo build`.
    let (_home, pin) = drifted_pin();
    let err = build_strategy_file(
        "fn x() {}",
        "unmarked_tree_probe.rs",
        out.path(),
        dir.path(),
        &cargo_bin(),
        &child_env(),
        TEST_PROFILE,
        &pin,
    )
    .expect_err("a tree that is neither must refuse before cargo ever runs");
    match err {
        BuildError::Sdk { why: msg } => {
            assert!(msg.contains(vike_rust_strategy::sdk::SDK_MARKER_FILE), "{msg}");
            assert!(msg.contains("--sdk-root"), "the fix is named: {msg}");
        }
        other => panic!("expected the source-tree refusal, got {other:?}"),
    }
    assert!(!out.path().join("sdk-dev").exists(), "nothing was laid out for a refused tree");
}

/// A checkout's shape passes the tree check — proven by the EXACT next refusal, without running
/// `cargo`: the drifted pin stops the call at the check right after the cache, so
/// `CargoHomeTampered` is the proof that the tree was accepted (as SDK dev), and nothing builds.
#[test]
fn a_checkout_is_accepted_as_sdk_dev() {
    let dir = tempfile::tempdir().expect("tempdir");
    checkout_shape::plant_checkout_shape(dir.path());
    let out = tempfile::tempdir().expect("tempdir");
    let (_home, pin) = drifted_pin();
    let err = build_strategy_file(
        "impl<B: Broker> Strategy<B> for S { fn on_start(&mut self) {} }",
        "checkout_probe.rs",
        out.path(),
        dir.path(),
        &cargo_bin(),
        &child_env(),
        TEST_PROFILE,
        &pin,
    )
    .expect_err("a drifted cargo-home pin refuses whatever the tree check decided");
    assert!(
        is_drifted_pin_refusal(&err),
        "a checkout must pass the tree check and stop at the drifted pin; got {err:?}"
    );
}

/// Install a miniature SDK `number` under `sdk_root`: this checkout's strategy boundary and templates
/// (so its boundary is this host's and it is usable here), then its marker. It holds no crate to
/// compile, so a build of it is served from a planted artifact only.
fn install_miniature_sdk(sdk_root: &Path, number: u32) -> PathBuf {
    let dir = vike_rust_strategy::sdk::snapshot_dir(sdk_root, number);
    let crate_dir = vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR;
    let (from, to) = (workspace_root().join(crate_dir), dir.join(crate_dir));
    fn copy(from: &Path, to: &Path) {
        if from.is_dir() {
            for entry in std::fs::read_dir(from).expect("a boundary folder").flatten() {
                copy(&entry.path(), &to.join(entry.file_name()));
            }
        } else if from.is_file() {
            std::fs::create_dir_all(to.parent().expect("a parent")).expect("the folder");
            std::fs::copy(from, to).expect("copy a boundary file");
        }
    }
    // The boundary (`crates/vike-rust-strategy/build/source_hash.rs`'s `BOUNDARY_FILES` and `BOUNDARY_FOLDERS`).
    for part in [
        "src/abi.rs",
        "src/guest.rs",
        "src/host.rs",
        "src/study.rs",
        "src/guest",
        "src/host",
        "template",
        "tests/boundary",
    ] {
        copy(&from.join(part), &to.join(part));
    }
    vike_rust_strategy::sdk::SdkMarker::of_snapshot(&dir, number, "0123456789", 15)
        .write(&dir)
        .expect("the marker");
    dir
}

/// **An SDK override builds against the SDK it names** (decision 0296: `Request::Build`'s `sdk`, a backtest
/// profile's `[strategy] sdk`), over the newest one, and the artifact it answers names that SDK and its
/// folder, which the daemon's `BuildOk` reads off it (`builder::sdk_and_folder`). The miniature SDK holds
/// no crate to compile, so its build is planted from this checkout's build of the same text and served
/// by the cache after its handshake, as any build is.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn an_sdk_override_builds_against_the_named_sdk_and_its_answer_names_it() {
    use vike_rust_strategy::sdk::Sdk;
    use vike_strategy_builder::builder::{BuildSources, sdk_and_folder};
    let sdk_root = tempfile::tempdir().expect("the SDK root");
    let (seven, eight) =
        (install_miniature_sdk(sdk_root.path(), 7), install_miniature_sdk(sdk_root.path(), 8));
    let sources = BuildSources::Installed(
        render::InstalledSdks::open(sdk_root.path()).expect("the installed SDKs"),
    );
    let src = fixture_source();
    assert_eq!(sources.tree_for(Some(src), None).ok(), Some(eight), "the newest, unasked");
    let tree = sources.tree_for(Some(src), Some(7)).expect("the override");
    assert_eq!(tree, seven, "the SDK the override names");

    let (builds, profile) = (tempfile::tempdir().expect("builds"), Profile::of_this_binary());
    let build = |root: &Path| {
        build_strategy_file(
            src,
            "override_probe.rs",
            builds.path(),
            root,
            &cargo_bin(),
            &child_env(),
            profile,
            &no_pin(),
        )
    };
    let from_the_checkout = build(&workspace_root()).expect("this checkout builds the text");
    let (sdk, folder) =
        render::build_folder(&tree, TemplateKind::Strategy, profile).expect("SDK 7's folder");
    assert_eq!(sdk, Sdk::Number(7));
    let planted = vike_rust_strategy::artifact::artifact_path(
        builds.path(),
        sdk,
        &folder,
        &sha256_hex(src.as_bytes()),
    );
    std::fs::create_dir_all(planted.parent().expect("its folder")).expect("SDK 7's folder");
    std::fs::copy(&from_the_checkout, &planted).expect("plant SDK 7's build");

    let built = build(&tree).expect("served from SDK 7's folder");
    assert_eq!(built, planted, "the override's artifact lies in SDK 7's builds");
    assert_eq!(sdk_and_folder(&built), Some((Some(7), folder)), "what BuildOk says");
}

/// **A strategy file path that is not a plain relative `.rs` path is refused, never rewritten.** The
/// path is a wire peer's string (`Request::Build`); the artifact is named by the text's sha, so no
/// peer string reaches a path, and the path check keeps a refusal or a journal line from carrying one
/// that climbs out of `strategies/`. Cheap: the refusal fires before the hook scan and long before
/// `cargo`, and both halves carry a drifted pin, so nothing can compile whatever the check does: the
/// `ok` half asserts the pin's EXACT refusal (the last check before anything is built, which a path
/// only reaches by passing the path check), and a build that lost its path guard fails the `bad` half
/// at the same pin.
#[test]
fn a_path_that_is_not_a_plain_relative_rs_path_is_refused_before_cargo_ever_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_home, pin) = drifted_pin();
    for bad in ["../escaped.rs", "/abs.rs", "a\\b.rs", "trend/../x.rs", "x", "", "C:x.rs"] {
        let err = build_strategy_file(
            "pub fn build<B>(_: &vike_model::params::Params) -> Box<dyn vike_model::Strategy<B>> { unreachable!() }",
            bad,
            dir.path(),
            &workspace_root(),
            &cargo_bin(),
            &child_env(),
            TEST_PROFILE,
            &pin,
        )
        .expect_err(&format!("{bad:?} must be refused"));
        assert_matches!(
            err, BuildError::InvalidPath { path: ref n } if n == bad,
            "{bad:?} must refuse as InvalidPath, got {err:?}"
        );
    }
    for ok in ["fixture_hold.rs", "trend/my-strategy.rs", "grid_dca/strat123.rs"] {
        let err = build_strategy_file(
            fixture_source(),
            ok,
            dir.path(),
            &workspace_root(),
            &cargo_bin(),
            &child_env(),
            TEST_PROFILE,
            &pin,
        )
        .expect_err("the drifted pin refuses");
        assert!(
            is_drifted_pin_refusal(&err),
            "{ok:?} must pass the path check and stop at the drifted pin, got {err:?}"
        );
    }
    // ...and nothing reached the builds directory: every refusal above precedes the build.
    let produced: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert!(produced.is_empty(), "no refusal may leave anything in the builds dir: {produced:?}");
}

/// A file exporting BOTH entries is refused by name before anything is built: which one would
/// run is not a guess the builder makes.
#[test]
fn a_file_exporting_both_entries_is_refused_before_cargo_ever_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_home, pin) = drifted_pin();
    let text = "pub fn build<B>(_: &P) {}\npub fn rule(_params: &P) {}\n";
    let err = build_strategy_file(
        text,
        "two.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        TEST_PROFILE,
        &pin,
    )
    .expect_err("two entries");
    assert_eq!(err, BuildError::TwoEntries);
    assert!(err.to_string().contains("`pub fn build` and `pub fn rule`"), "{err}");
}

/// **A Build is refused, not silently compiled, once `$CARGO_HOME/config.toml` drifts from what
/// this pin captured.** Cheap: the refusal in `build_strategy_file` fires after the cache
/// but before `fs::create_dir_all(out_dir)` or `cargo` is ever spawned, so this test invokes no
/// compiler despite exercising a real `build_strategy_file` call.
#[test]
fn a_build_is_refused_once_cargo_home_config_drifts_from_its_pin() {
    let cargo_home = tempfile::tempdir().expect("tempdir");
    let out_dir = tempfile::tempdir().expect("tempdir");
    let pin = render::CargoHomePin::capture(cargo_home.path());

    // A caller's own build.rs writing this file directly — no traversal, its own environment
    // already names the path — is exactly what this guard exists to catch on the NEXT request.
    std::fs::write(
        cargo_home.path().join("config.toml"),
        "[registry]\ncredential-provider = [\"/tmp/evil\"]\n",
    )
    .unwrap();

    let err = build_strategy_file(
        "irrelevant: refused before cargo would ever see this source",
        "cargo_home_probe.rs",
        out_dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        TEST_PROFILE,
        &pin,
    )
    .expect_err("drifted cargo-home config must refuse the build");
    match err {
        BuildError::CargoHomeTampered { was_present, now_present } => {
            assert!(!was_present.0, "the pin captured an absent file");
            assert!(now_present.0, "the file exists now");
        }
        other => panic!("expected CargoHomeTampered, got {other:?}"),
    }
}

/// **A symlink planted at the exact path a Build would land on gets refused, not followed.**
/// End-to-end proof of the placement hardening: a real build succeeds and its bytes land at
/// `artifact_path`, even though that path already held a symlink pointing at a file elsewhere —
/// which the OLD `fs::copy` fallback would have overwritten THROUGH. Needs a real `cargo`
/// (`#[ignore]`, lane-only) because the vulnerable code path only runs after a genuine compile.
#[cfg(unix)]
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_preexisting_symlink_at_the_artifact_path_is_refused_not_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target_dir = tempfile::tempdir().expect("tempdir");
    let sentinel = target_dir.path().join("do_not_touch.txt");
    std::fs::write(&sentinel, "sentinel content, never to be overwritten").unwrap();

    let src = "pub fn build<B: vike_model::OrderBroker + 'static>(\
               _: &vike_model::params::Params) -> \
               Box<dyn vike_model::Strategy<B> + Send> { unimplemented!() }";
    let artifact_path = artifact_of(dir.path(), src, TEST_PROFILE);
    std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("the build folder");
    std::os::unix::fs::symlink(&sentinel, &artifact_path).expect("plant the symlink");

    let built = build_strategy_file(
        src,
        "symlink_probe.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        TEST_PROFILE,
        &no_pin(),
    )
    .expect("a real build must still succeed despite the planted symlink");
    assert_eq!(built, artifact_path, "the artifact must land at the documented path");
    assert!(!built.is_symlink(), "the symlink must have been replaced, not written through");
    assert_eq!(
        std::fs::read_to_string(&sentinel).unwrap(),
        "sentinel content, never to be overwritten",
        "the symlink's TARGET must survive untouched"
    );
}

/// The two sides of this mechanism live in two different LANGUAGES (a shell script the release and
/// the commit deploy run, a Rust constant), so nothing can derive one from the other — they simply
/// have to agree, the same shape `deploy_layout_gate.rs`'s `STRATEGY_FILES_RELATIVE`/`STORE_REL`
/// literals already carry for a fact duplicated across languages.
///
/// Re-pinned with the versioned strategy SDK (decision 0296): the `SOURCE_GIT_SHA` stamp is gone,
/// and what the builder reads off a snapshot is its MARKER, `sdk-N/SDK`
/// (`vike_rust_strategy::sdk::SDK_MARKER_FILE`). The packer writes it through the one writer, the
/// `write_sdk_marker` bin over `SdkMarker::write` (so the TOML has one spelling), and refuses an
/// archive that does not hold it at that name.
#[test]
fn the_release_workflow_stamps_the_spelling_this_crate_reads() {
    // The step in `.github/workflows/release.yml` is one line calling this script since 2026-11-06.
    let packager_path = workspace_root().join("scripts").join("package_strategy_source.sh");
    let packager = std::fs::read_to_string(&packager_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", packager_path.display()));
    let marker = vike_rust_strategy::sdk::SDK_MARKER_FILE;
    for needle in [
        "cargo run -q -p vike-rust-strategy --bin write_sdk_marker -- \"$snapshot\"".to_string(),
        format!("grep -qx \"sdk-$sdk/{marker}\" \"$LISTING\""),
    ] {
        assert!(
            packager.contains(&needle),
            "{} no longer carries `{needle}` — the packer must write the snapshot's marker through \
             the one writer and at the name the builder reads (`{marker}`), and nothing derives one \
             from the other across a shell script and a Rust constant.",
            packager_path.display()
        );
    }
}

/// A source naming a renamed word fails WITH its `RENAMED_NAMES` hint after rustc's text, through
/// the real pipeline, and the failure is RECORDED: the same text is answered from `<sha>.err`
/// beside where its artifact would be, byte for byte and without `cargo`.
///
/// The case is the order word `TaggedOrder` (now `OrderPlacement`) reached through `vike_model`, a
/// `RENAMED_NAMES` row today. The expected hint is the production function's own answer
/// (`vike_model::host_build::moved_name::source_hints`), so a row added to that table (plan 2b's
/// order-language words) needs only one more source in this list, never a second copy of the text.
/// The second source is one: the order side, `vike_tactics`' `Side` (`OrderSide::Long` now, in
/// `vike_model`). Each source also fails on its own `broken`, so the case holds on a tree the
/// scripted rename has not reached yet; every old word is built from parts, since that script
/// rewrites a joined one wherever it finds it.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn an_old_order_word_fails_with_its_hint_and_the_failure_is_answered_from_its_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sources = [
        format!(
            "use {}::{{OrderBroker, Strategy, TaggedOrder}};\npub fn broken() -> u8 {{ \"not a u8\" }}\n",
            "vike_model"
        ),
        format!(
            "use {}::{{Rule, {side}, Tactics}};\npub fn long() -> {side} {{ {side}::Long }}\n\
             pub fn broken() -> u8 {{ \"not a u8\" }}\n",
            "vike_tactics",
            side = "Side"
        ),
    ];
    let side_hint = "`Side` is now `vike_model::OrderSide` in `vike_tactics`";
    assert!(
        sources.iter().any(|src| {
            vike_model::host_build::moved_name::source_hints(src)
                .iter()
                .any(|h| h.contains(side_hint))
        }),
        "the order side's source draws its hint: {side_hint}"
    );
    for src in &sources {
        let hints = vike_model::host_build::moved_name::source_hints(src);
        assert!(!hints.is_empty(), "the case must draw a hint, or it proves nothing: {src}");
        let build = || {
            build_strategy_file(
                src,
                "trend/old_words.rs",
                dir.path(),
                &workspace_root(),
                &cargo_bin(),
                &child_env(),
                TEST_PROFILE,
                &no_pin(),
            )
        };
        let Err(BuildError::Compile { diagnostics: first }) = build() else {
            panic!("must fail to compile")
        };
        for hint in &hints {
            assert!(
                first.contains(&format!("\nhint: {hint}\n")),
                "the hint follows rustc: {first}"
            );
        }
        let record = artifact_of(dir.path(), src, TEST_PROFILE).with_extension("err");
        assert_eq!(std::fs::read_to_string(&record).expect("the failure is recorded"), first);
        let started = Instant::now();
        let Err(BuildError::Compile { diagnostics: again }) = build() else {
            panic!("must fail again")
        };
        let elapsed = started.elapsed();
        assert_eq!(again, first, "the record answers byte for byte");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}: cargo ran for a recorded failure");
    }
}

/// A `build` that takes the mount facts reads them, and the optional `refuse` and `echo` exports
/// answer over the params the strategy was created with (`ABI_VERSION` 9); a file without them
/// answers none.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn the_facts_the_refusal_and_the_echo_cross_into_a_built_file() {
    use vike_model::Strategy as _;
    use vike_model::strategy::OrderBrokerMock;
    use vike_rust_strategy::host::{StrategyFileHost, refusal};

    let dir = tempfile::tempdir().expect("tempdir");
    let so = build_strategy_file(
        FACTS_SOURCE,
        "other/facts.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        Profile::of_this_binary(),
        &no_pin(),
    )
    .unwrap_or_else(|e| panic!("builds:\n{e}"));
    let vt = loader::load(&so).expect("loads").vtable;
    let facts = r#"{"symbol":"BTCUSDT","tick_size":0.5}"#;
    let host = StrategyFileHost::<OrderBrokerMock>::new(vt, "rungs = 0\n", facts);
    assert!(host.created());
    assert_eq!(host.warmup(), 705, "the facts reached `build`");
    assert_eq!(refusal(&vt, "rungs = 0\n").as_deref(), Some("rungs 0 < 1"));
    assert_eq!(host.echo(), Some(vec![("rungs".to_string(), "0".to_string())]));
    let host = StrategyFileHost::<OrderBrokerMock>::new(vt, "rungs = 4\n", "{}");
    assert_eq!((host.warmup(), refusal(&vt, "rungs = 4\n")), (0, None), "no facts, no refusal");
    let host = StrategyFileHost::<OrderBrokerMock>::new(vt, "", r#"{"unknown":1}"#);
    assert!(!host.created(), "a facts document that is not one creates nothing");

    // The one-argument form, and no `refuse`/`echo`: both answer none.
    let so = build_strategy_file(
        fixture_source(),
        "other/plain.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        Profile::of_this_binary(),
        &no_pin(),
    )
    .unwrap_or_else(|e| panic!("builds:\n{e}"));
    let vt = loader::load(&so).expect("loads").vtable;
    let host = StrategyFileHost::<OrderBrokerMock>::new(vt, "qty = 1.0\n", "{}");
    assert!(host.created());
    assert_eq!((refusal(&vt, "qty = 1.0\n"), host.echo()), (None, None));
}

/// A `build(params, facts)` file with a `refuse` and an `echo`: its warmup is the facts' symbol
/// length times 100 plus ten tick sizes, so the test reads what crossed.
const FACTS_SOURCE: &str = r#"
use vike_marketdata::Bar;
use vike_model::params::{MountFacts, Params};
use vike_model::{OrderBroker, Strategy};

struct Facts(usize);

impl<B: OrderBroker> Strategy<B> for Facts {
    fn warmup(&self) -> usize {
        self.0
    }
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}

pub fn build<B: OrderBroker + 'static>(
    _params: &Params,
    facts: &MountFacts,
) -> Box<dyn Strategy<B> + Send> {
    Box::new(Facts(facts.symbol.len() * 100 + facts.tick_size.map_or(0, |t| (t * 10.0) as usize)))
}

pub fn refuse(params: &Params) -> Option<String> {
    let rungs = params.get("rungs").and_then(|v| v.as_integer())?;
    (rungs < 1).then(|| format!("rungs {rungs} < 1"))
}

pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    vec![("rungs", params.get("rungs").map(|v| v.to_string()).unwrap_or_default())]
}
"#;

/// A gallery rule as a strategy FILE (`pub fn rule(params)`, the order language) builds, loads and
/// is created on the facts' symbol; with no symbol it is not created at all.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_rule_file_runs_on_the_facts_symbol_and_is_refused_without_one() {
    use vike_model::strategy::OrderBrokerMock;
    use vike_rust_strategy::host::StrategyFileHost;

    let dir = tempfile::tempdir().expect("tempdir");
    let so = build_strategy_file(
        GALLERY_RULE_FILE,
        "mean_reversion/bollinger_bands_ema_9.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        Profile::of_this_binary(),
        &no_pin(),
    )
    .unwrap_or_else(|e| panic!("the gallery rule builds as a file:\n{e}"));
    let vt = loader::load(&so).expect("loads").vtable;
    let on = StrategyFileHost::<OrderBrokerMock>::new(vt, "", r#"{"symbol":"BTCUSDT"}"#);
    assert!(on.created(), "a rule file with a symbol is created");
    let off = StrategyFileHost::<OrderBrokerMock>::new(vt, "", "{}");
    assert!(!off.created(), "a rule file with no symbol is not created");
}

/// One of the gallery's rules, as plan 2b turns each into a file (`tests/fixtures/`'s copy keeps its
/// licence lines).
const GALLERY_RULE_FILE: &str = include_str!("fixtures/gallery_bollinger_bands_ema_9.rs.in");

/// MEASURED, not asserted (plan 2b section 3 asks F3 to measure them): a COLD build (the first in
/// this source tree's build folder, its dependency closure compiled into the shared target), a WARM
/// build of another file (only its own crate and its link), the gallery rule file, and a cache hit;
/// then the shared target's size. Run it FIRST on sources no lane built yet, alone, or the "first
/// build" line times a warm one (it says which it found).
#[test]
#[ignore = "invokes cargo; a measurement; lane-only"]
fn measured_cold_warm_and_gallery_build_times() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = Profile::of_this_binary();
    let (_, folder) = render::build_folder(&workspace_root(), TemplateKind::Strategy, profile)
        .expect("the checkout is a source tree");
    let work =
        render::work_directory_of(dir.path(), &workspace_root(), TemplateKind::Strategy, profile)
            .expect("the checkout is a source tree");
    let warm_before = work.join("target").exists();
    let build = |text: &str, path: &str| {
        let started = Instant::now();
        let so = build_strategy_file(
            text,
            path,
            dir.path(),
            &workspace_root(),
            &cargo_bin(),
            &child_env(),
            profile,
            &no_pin(),
        )
        .unwrap_or_else(|e| panic!("{path} builds:\n{e}"));
        (started.elapsed(), std::fs::metadata(&so).expect("artifact").len())
    };
    let (cold, cold_size) = build(fixture_source(), "measure/first.rs");
    let second = format!("{}\n// another file\n", fixture_source());
    let (warm, _) = build(&second, "measure/second.rs");
    let (gallery, gallery_size) = build(GALLERY_RULE_FILE, "mean_reversion/bollinger.rs");
    let (hit, _) = build(&second, "measure/second.rs");
    let target_bytes = dir_bytes(&work);
    println!(
        "MEASURED profile {profile:?}, folder {folder} (its shared build dir existed before: \
         {warm_before}): first build {cold:?} ({cold_size} B), another file {warm:?}, the gallery \
         rule {gallery:?} ({gallery_size} B), a cache hit {hit:?}; shared build dir {} MiB",
        target_bytes / (1024 * 1024)
    );
}

/// The bytes under `dir`, recursively (symlinks not followed); 0 for a missing directory.
fn dir_bytes(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// The shipped example file (`strategies/other/example.rs`, its header in `//!` lines) builds as it
/// is and its strategy is created: the file is the rendered crate's module `strategy`, so its
/// `//!` header leads that module as rustc requires. Read at run time, skipped loudly where the
/// checkout has no `strategies/` folder.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn the_shipped_example_file_builds_with_its_doc_header() {
    use vike_model::strategy::OrderBrokerMock;
    use vike_rust_strategy::host::StrategyFileHost;

    let file = workspace_root().join("strategies").join("other").join("example.rs");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("SKIPPED: no {} in this checkout", file.display());
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let so = build_strategy_file(
        &text,
        "other/example.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        Profile::of_this_binary(),
        &no_pin(),
    )
    .unwrap_or_else(|e| panic!("the example builds:\n{e}"));
    let host = StrategyFileHost::<OrderBrokerMock>::new(
        loader::load(&so).expect("loads").vtable,
        "qty = 2.0\n",
        r#"{"symbol":"BTCUSDT"}"#,
    );
    assert!(host.created());
}

/// The shipped files that reach crates beyond the first template (plan 2b: the makers name
/// `vike-market-making`, `serde` and `tracing`, the funding carry `vike-venues`) build through the real builder
/// and load; each is created on a live mount's facts, and what `create` answered is printed. Read
/// at run time; a file this checkout does not hold is skipped loudly.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn the_shipped_files_that_need_the_wider_template_build_and_load() {
    use vike_model::strategy::OrderBrokerMock;
    use vike_rust_strategy::host::{StrategyFileHost, refusal};

    let dir = tempfile::tempdir().expect("tempdir");
    for name in [
        "market_making/spread_maker.rs",
        "market_making/cross_exchange_maker.rs",
        "funding/funding_carry.rs",
    ] {
        let file = workspace_root().join("strategies").join(name);
        let Ok(text) = std::fs::read_to_string(&file) else {
            eprintln!("SKIPPED: no {} in this checkout", file.display());
            continue;
        };
        let started = Instant::now();
        let so = build_strategy_file(
            &text,
            name,
            dir.path(),
            &workspace_root(),
            &cargo_bin(),
            &child_env(),
            Profile::of_this_binary(),
            &no_pin(),
        )
        .unwrap_or_else(|e| panic!("{name} builds:\n{e}"));
        let built = started.elapsed();
        let vt = loader::load(&so).unwrap_or_else(|e| panic!("{name} loads: {e}"));
        let facts = r#"{"symbol":"BTCUSDT","venue":"bybit","tick_size":0.1}"#;
        let refused = refusal(&vt.vtable, "");
        let host = StrategyFileHost::<OrderBrokerMock>::new(vt.vtable, "", facts);
        println!(
            "MEASURED {name}: built in {built:?}, {} B; on empty params: refusal {refused:?}, \
             created {}",
            std::fs::metadata(&so).map(|m| m.len()).unwrap_or(0),
            host.created(),
        );
    }
}

/// A strategy file's own `tracing::warn!` reaches the HOST's subscriber (`ABI_VERSION` 9's log slot):
/// the file links its own `tracing`, and the loader hands it `host::host_log` at load, which
/// re-emits the event under the target `strategy_file`. The host's subscriber here is the
/// workspace's capture (`vike_log::capture::captured`), so the test reads what the host logged.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_files_own_warning_is_logged_by_the_host() {
    use vike_model::strategy::OrderBrokerMock;
    use vike_rust_strategy::host::StrategyFileHost;

    const SOURCE: &str = r#"
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_model::{OrderBroker, Strategy};

struct Quiet;

impl<B: OrderBroker> Strategy<B> for Quiet {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}

pub fn build<B: OrderBroker + 'static>(_params: &Params) -> Box<dyn Strategy<B> + Send> {
    tracing::warn!(rungs = 3, "f3-log-probe: a file's own warning");
    Box::new(Quiet)
}
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let so = build_strategy_file(
        SOURCE,
        "other/log_probe.rs",
        dir.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        Profile::of_this_binary(),
        &no_pin(),
    )
    .unwrap_or_else(|e| panic!("builds:\n{e}"));
    let ((), logged) = vike_log::capture::captured(|| {
        let vt = loader::load(&so).expect("loads").vtable;
        let host = StrategyFileHost::<OrderBrokerMock>::new(vt, "", "{}");
        assert!(host.created());
    });
    println!("MEASURED the host logged: {logged:?}");
    assert!(
        logged.iter().any(|e| e.level.as_str() == "WARN"
            && e.target == "strategy_file"
            && e.message.contains("f3-log-probe: a file's own warning")
            && e.message.contains("rungs=3")
            && e.field("file") == Some("strategy_file::strategy")),
        "the file's warning must reach the host's subscriber as a WARN under `strategy_file`: \
         {logged:?}"
    );
}
