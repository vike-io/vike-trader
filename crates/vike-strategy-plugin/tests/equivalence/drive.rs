//! The two mechanisms driven down one engine lane, and the one production-builder call site.

use std::path::{Path, PathBuf};

use vike_analytics::BacktestResult;
use vike_model::{Strategy, WorkingOrder};
use vike_sim::{DateRule, SimBroker, StrategyEngine};
use vike_strategy_builder::render::{self, Profile, artifact_dir_tag, build_plugin};
use vike_strategy_plugin::host::PluginStrategy;
use vike_strategy_plugin::loader;

use super::series::{engine_params, tick_series, universe};
use super::{FIXTURE_SOURCE, FLOOR_SOURCE, PARAMS_TOML, SCHEDULE_EVERY_N, SYMBOL, ma_cross};

/// Which engine lane a comparison is run down. Both drive the SAME fixture instance type through
/// the SAME `StrategyEngine`; they differ in which `Strategy` hooks the engine emits, which is
/// exactly what makes two lanes worth having.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lane {
    /// `StrategyEngine::run` — emits `warmup`, `on_start`, `on_bar`, `on_schedule`, `on_fill`,
    /// `on_stop`.
    Bars,
    /// `StrategyEngine::run_ticks` — emits `warmup`, `on_start`, `on_quote_tick`, `on_trade_tick`,
    /// `on_order_book`, `on_fill`, `on_stop`.
    Ticks,
}

impl Lane {
    pub(super) fn label(self) -> &'static str {
        match self {
            Lane::Bars => "bar lane (run)",
            Lane::Ticks => "tick lane (run_ticks)",
        }
    }
}

/// What one run produced: the ordinary result, PLUS the engine's still-working orders.
///
/// ⚠ **`pending` is here because `on_stop` is otherwise unobservable.** The engine calls it, but a
/// `BacktestResult` carries no trace of anything it did: the run is over, so an order submitted
/// there can never fill and never becomes a `Trade`. It does land in `SimBroker`'s per-symbol
/// working-order list, which outlives `run`, so reading it there is what turns `on_stop` from a
/// hook with a narrower witness into one the bit-for-bit comparison actually covers.
pub(super) struct Run {
    pub(super) result: BacktestResult,
    pub(super) pending: Vec<WorkingOrder>,
}

/// Drive one strategy down one lane. The SAME function for both mechanisms — a compiled-in
/// strategy and a `PluginStrategy` are both just `S: Strategy<SimBroker>` here, which is the
/// property the whole comparison rests on.
pub(super) fn drive<S: Strategy<SimBroker>>(strategy: S, lane: Lane) -> Run {
    let bars = match lane {
        Lane::Bars => universe(),
        // `run_ticks` needs a symbol SLOT, not a bar series — the tape is the input. An empty
        // series is the shape `crates/vike-sim/tests/book_replay.rs` drives it with.
        Lane::Ticks => vec![(SYMBOL.to_string(), Vec::new())],
    };
    let mut engine = StrategyEngine::new(bars, strategy, engine_params());
    // Registered on the ENGINE, not by the strategy: `Schedule::on` is an inherent `SimBroker`
    // verb, so a portable `impl<B: Broker> Strategy<B>` — which the fixture is, and must stay, to
    // be a real user strategy — cannot reach it. Both mechanisms get the identical registration
    // because both go through this one function.
    engine.core.schedule.on(DateRule::every_n_bars(SCHEDULE_EVERY_N), ma_cross::REBALANCE_TAG);
    let result = match lane {
        Lane::Bars => engine.run(),
        Lane::Ticks => engine.run_ticks(&[(SYMBOL.to_string(), tick_series())]),
    };
    let pending = engine.core.sym.iter().flat_map(|s| s.pending.clone()).collect();
    Run { result, pending }
}

/// PATH 1 — the build-time tier's mechanism: a direct Rust call into a module of this binary.
pub(super) fn run_compiled(lane: Lane) -> Run {
    run_compiled_with(PARAMS_TOML, lane)
}

/// The same path under an explicit params document. Used by the differential probes below, which
/// need two runs of the COMPILED half that differ in exactly one params key — no cargo, no
/// plugin, and therefore cheap enough to gate every PR.
pub(super) fn run_compiled_with(params_toml: &str, lane: Lane) -> Run {
    let params: toml::Value =
        toml::from_str(params_toml).expect("fixture params must be valid TOML");
    // The entry contract returns `+ Send`; the engine's bound is the auto-trait-free object, and
    // dropping an auto trait from a trait object is an ordinary unsizing coercion.
    let strategy: Box<dyn Strategy<SimBroker>> = ma_cross::build::<SimBroker>(&params);
    drive(strategy, lane)
}

/// PATH 2 — the plugin mechanism: the real builder, the real loader, the real host wrapper.
pub(super) fn run_plugin(so: &Path, lane: Lane) -> Run {
    let strategy = PluginStrategy::<SimBroker>::new(load_or_panic(so).vtable, PARAMS_TOML);
    drive(strategy, lane)
}

pub(super) fn load_or_panic(so: &Path) -> loader::LoadedPlugin {
    loader::load(so).unwrap_or_else(|e| {
        panic!(
            "the loader refused the artifact the builder just produced at {}: {e}\n\
             (a FingerprintMismatch here means the host and the plugin were built under \
             different profiles — see this file's module doc; a NoSymbol here means the template \
             did not export a dispatch symbol `loader::load` binds, which at ABI_VERSION 3 is \
             every hook in `vike_strategy_plugin::host::WIRED_HOOKS`)",
            so.display()
        )
    })
}

/// Build the fixture into a real `.so` through the production builder. Returns the artifact path.
pub(super) fn build_fixture_plugin() -> PathBuf {
    build_through_the_production_builder(FIXTURE_SOURCE, "ma_cross")
}

/// Build [`FLOOR_SOURCE`] the same way, through the same builder, into the same output directory.
///
/// ⚠ **Called from inside the one test that already builds**, never from a `#[test]` of its own —
/// under `nextest` a second test is a second PROCESS, and two processes racing a cold
/// `build_plugin` share one scratch target directory. Same rule `assert_live_only_hooks_reach_the_plugin`
/// states at its own definition.
pub(super) fn build_floor_plugin() -> PathBuf {
    build_through_the_production_builder(FLOOR_SOURCE, "floor")
}

/// The one `build_plugin` call site: profile, output directory, checkout root and cargo, resolved
/// once so the fixture and the floor artifact are produced under byte-identical conditions and
/// their sizes are therefore comparable to each other as well as across runs.
fn build_through_the_production_builder(source: &str, name: &str) -> PathBuf {
    // `CARGO_TARGET_TMPDIR` is Cargo's own scratch directory under this checkout's `target/`, the
    // same one `tests/load_refusals.rs` caches its fixtures in. It is not the shared system
    // scratch directory, and nothing in this file builds a path under that one.
    //
    // ⚠ That last sentence is worded the way it is ON PURPOSE. It used to name the std accessor
    // outright, and `crates/vike-ops/tests/hygiene/temp_path_gate.rs` matches that accessor as RAW TEXT —
    // comments included — and then reads the next `.join("literal")` it finds up to the following
    // `;`. So a comment saying "this is NOT that directory" made the gate read the line below as
    // a fixed-name scratch directory under it and reject it. Measured on this branch's first gate
    // run. Do not reintroduce the name here.
    //
    // The directory is keyed on `render::artifact_dir_tag()` — the ABI version and the whole
    // toolchain fingerprint, i.e. exactly what `loader::load` compares — because `build_plugin`'s
    // artifact name carries the source sha and NEITHER. Without it a run after a bump (of the
    // ABI, of rustc, of the profile, of the target) is handed the PREVIOUS artifact and refused
    // on `AbiMismatch`/`FingerprintMismatch`.
    //
    // ⚠ Not hypothetical and not caught by reasoning: it is what a warm `CARGO_TARGET_TMPDIR` did
    // to `tests/load_refusals.rs`'s two `build_plugin` tests when `ABI_VERSION` went 2 -> 3, and
    // then again to `crates/vike-studio-core/tests/plugin_run.rs` in CI. This test escaped the
    // first time only because the fixture SOURCE changed in the same commit, so its sha changed
    // too — i.e. by luck, not by design. The rule is a LIBRARY function now, so there is ONE
    // spelling rather than one per crate; `render::artifact_dir_tag`'s own doc carries the
    // finding and says why production deliberately does not use it.
    let profile = Profile::of_this_binary();
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("equivalence-plugin-{}", artifact_dir_tag()));
    // Two levels up from this crate's manifest directory is the checkout root, which is what the
    // rendered scratch crate's `path` dependencies resolve against.
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    // The exact cargo running this test, so the nested build uses the same toolchain rather than
    // whatever is first on PATH. `Layer::TestOnly`; the `(CARGO, vike-strategy-plugin)` row in
    // `vike_ops::settings::SETTINGS` already declares this crate's reads.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    // The SERVICE's own cleared environment for the nested cargo (`render::child_env`), so this
    // build runs under what a deployed build does rather than under the test runner's whole env.
    let child_env = render::child_env(&std::env::vars().collect());
    build_plugin(
        source,
        name,
        &out_dir,
        &workspace_root,
        &cargo,
        &child_env,
        profile,
        &render::CargoHomePin::disabled(),
    )
    .unwrap_or_else(|e| panic!("the builder must produce a plugin from the `{name}` source:\n{e}"))
}

/// A directory-name-safe tag for a profile. A `Debug` formatting of the enum would do, but
/// spelling it keeps the directory name stable if that derive ever changes.
pub(super) fn profile_tag(p: Profile) -> &'static str {
    match p {
        Profile::Debug => "debug",
        Profile::Release => "release",
    }
}
