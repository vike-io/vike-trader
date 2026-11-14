//! The two mechanisms driven down one engine lane, and the one production-builder call site.

use std::path::{Path, PathBuf};

use vike_analytics::BacktestResult;
use vike_model::params::Params;
use vike_model::{OrderResting, Strategy};
use vike_rust_strategy::host::StrategyFileHost;
use vike_rust_strategy::loader;
use vike_simulator::{
    OrderBrokerSimulator, ScheduleDateRule, StrategyEngine, StrategyEngineConfig,
};
use vike_strategy_builder::render::{self, Profile, build_strategy_file};

use super::series::{engine_params, inputs_of, tick_series, universe};
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
/// there can never fill and never becomes a `Trade`. It does land in `OrderBrokerSimulator`'s per-symbol
/// working-order list, which outlives `run`, so reading it there is what turns `on_stop` from a
/// hook with a narrower witness into one the bit-for-bit comparison actually covers.
pub(super) struct Run {
    pub(super) result: BacktestResult,
    pub(super) pending: Vec<OrderResting>,
}

/// Drive one strategy down one lane. The SAME function for both mechanisms — a compiled-in
/// strategy and a `StrategyFileHost` are both just `S: Strategy<OrderBrokerSimulator>` here, which is the
/// property the whole comparison rests on.
pub(super) fn drive<S: Strategy<OrderBrokerSimulator>>(strategy: S, lane: Lane) -> Run {
    let bars = match lane {
        Lane::Bars => universe(),
        // `run_ticks` needs a symbol SLOT, not a bar series — the tape is the input. An empty
        // series is the shape `crates/vike-simulator/tests/fills/book_replay.rs` drives it with.
        Lane::Ticks => vec![(SYMBOL.to_string(), Vec::new())],
    };
    // The inputs the strategy DECLARES, read once before the run as the backtest's input planner
    // reads them: a mechanism that dropped the declaration runs with nothing loaded for it.
    let inputs = inputs_of(&strategy.inputs(), lane == Lane::Ticks);
    let mut engine =
        StrategyEngine::new(bars, strategy, StrategyEngineConfig { inputs, ..engine_params() });
    // Registered on the ENGINE, not by the strategy: `Schedule::on` is an inherent `OrderBrokerSimulator`
    // verb, so a portable `impl<B: OrderBroker> Strategy<B>` — which the fixture is, and must stay, to
    // be a real user strategy — cannot reach it. Both mechanisms get the identical registration
    // because both go through this one function.
    engine
        .core
        .schedule
        .on(ScheduleDateRule::every_n_bars(SCHEDULE_EVERY_N), ma_cross::REBALANCE_TAG);
    let result = match lane {
        Lane::Bars => engine.run(),
        Lane::Ticks => engine.run_ticks(&[(SYMBOL.to_string(), tick_series())]),
    };
    let pending = engine.core.symbol.iter().flat_map(|s| s.pending.clone()).collect();
    Run { result, pending }
}

/// PATH 1 — compiled in: a direct Rust call into a module of this binary.
pub(super) fn run_compiled(lane: Lane) -> Run {
    run_compiled_with(PARAMS_TOML, lane)
}

/// The same path under an explicit params document. Used by the differential probes below, which
/// need two runs of the COMPILED half that differ in exactly one params key — no cargo, no
/// strategy file, and therefore cheap enough to gate every PR.
pub(super) fn run_compiled_with(params_toml: &str, lane: Lane) -> Run {
    let params: Params = toml::from_str(params_toml).expect("fixture params must be valid TOML");
    // The entry contract returns `+ Send`; the engine's bound is the auto-trait-free object, and
    // dropping an auto trait from a trait object is an ordinary unsizing coercion.
    let strategy: Box<dyn Strategy<OrderBrokerSimulator>> =
        ma_cross::build::<OrderBrokerSimulator>(&params);
    drive(strategy, lane)
}

/// PATH 2 — the strategy file mechanism: the real builder, the real loader, the real host wrapper.
pub(super) fn run_strategy_file(so: &Path, lane: Lane) -> Run {
    let strategy =
        StrategyFileHost::<OrderBrokerSimulator>::new(load_or_panic(so).vtable, PARAMS_TOML, "{}");
    drive(strategy, lane)
}

pub(super) fn load_or_panic(so: &Path) -> loader::StrategyFileLoaded {
    loader::load(so).unwrap_or_else(|e| {
        panic!(
            "the loader refused the artifact the builder just produced at {}: {e}\n\
             (a FingerprintMismatch here means the host and the strategy file were built under \
             different profiles — see this file's module doc; a NoSymbol here means the template \
             did not export a dispatch symbol `loader::load` binds, which since ABI_VERSION 3 \
             is every hook in `vike_rust_strategy::host::WIRED_HOOKS`)",
            so.display()
        )
    })
}

/// Build the fixture into a real `.so` through the production builder. Returns the artifact path.
pub(super) fn build_fixture_strategy_file() -> PathBuf {
    build_through_the_production_builder(FIXTURE_SOURCE, "ma_cross")
}

/// Build [`FLOOR_SOURCE`] the same way, through the same builder, into the same output directory.
///
/// ⚠ **Called from inside the one test that already builds**, never from a `#[test]` of its own —
/// under `nextest` a second test is a second PROCESS, and two processes building at once
/// queue on the one shared build directory's lock. Same rule `assert_live_only_hooks_reach_the_strategy_file`
/// states at its own definition.
pub(super) fn build_floor_strategy_file() -> PathBuf {
    build_through_the_production_builder(FLOOR_SOURCE, "floor")
}

/// The one `build_strategy_file` call site: profile, output directory, checkout root and cargo, resolved
/// once so the fixture and the floor artifact are produced under byte-identical conditions and
/// their sizes are therefore comparable to each other as well as across runs. The tactics fixture
/// (`super::tactics`) builds through it too.
pub(super) fn build_through_the_production_builder(source: &str, name: &str) -> PathBuf {
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
    // What built the artifact (the ABI version, the toolchain fingerprint, the template, the profile
    // and the commit) is the builder's own build FOLDER inside this directory
    // (`render::build_folder`); before it was, a warm `CARGO_TARGET_TMPDIR` handed this test and
    // `tests/load_refusals.rs` the PREVIOUS ABI's artifact after the 2 -> 3 bump.
    let profile = Profile::of_this_binary();
    // Two levels up from this crate's manifest directory is the checkout root, which is what the
    // rendered scratch crate's `path` dependencies resolve against.
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    // ⚠ The directory is ALSO keyed on the SOURCES of the crates the strategy file computes with
    // ([`dependency_source_fingerprint`]). The folder says what built the artifact and the artifact
    // name carries the strategy's source, but neither says which version of `vike-indicators` the
    // strategy file was compiled against: a warm cache after an indicator's arithmetic changed by one bit
    // handed back a strategy file built with the OLD indicator, and this test compared it bit for bit with
    // the compiled half running the NEW one (measured: `rsi_scale` read a different last bit, 169
    // differences; a forced rebuild passed).
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "equivalence-strategy-file-{}",
        dependency_source_fingerprint(&workspace_root)
    ));
    // The exact cargo running this test, so the nested build uses the same toolchain rather than
    // whatever is first on PATH. `Layer::TestOnly`; the `(CARGO, vike-rust-strategy)` row in
    // `vike_config::settings::SETTINGS` already declares this crate's reads.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    // The SERVICE's own cleared environment for the nested cargo (`render::child_env`), so this
    // build runs under what a deployed build does rather than under the test runner's whole env.
    let child_env = render::child_env(&std::env::vars().collect());
    build_strategy_file(
        source,
        &format!("{name}.rs"),
        &out_dir,
        &workspace_root,
        &cargo,
        &child_env,
        profile,
        &render::CargoHomePin::disabled(),
    )
    .unwrap_or_else(|e| {
        panic!("the builder must produce a strategy file from the `{name}` source:\n{e}")
    })
}

/// A hash of the source files of the crates a strategy file computes with (`vike-indicators` and the
/// `vike-marketdata` it reads bars from; for the tactics fixture `vike-tactics` and the order-layer
/// arithmetic of `vike-model` it calls, the snaps and the trailing activation): every `.rs` under
/// their `src/` plus their `Cargo.toml`, in path order. A changed byte is a new directory, i.e. a
/// rebuild of the strategy file; `DefaultHasher`'s output is not stable across Rust versions, which is
/// harmless in the SAFE direction (a changed hash is a new directory, i.e. a rebuild). It reads the checkout,
/// never writes it.
fn dependency_source_fingerprint(workspace_root: &Path) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut files = Vec::new();
    for krate in ["vike-indicators", "vike-marketdata", "vike-model", "vike-tactics"] {
        let root = workspace_root.join("crates").join(krate);
        vike_data::test_support::text::rs_files(&root.join("src"), &mut files);
        files.push(root.join("Cargo.toml"));
    }
    files.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in &files {
        f.strip_prefix(workspace_root).unwrap_or(f).hash(&mut h);
        std::fs::read(f).unwrap_or_default().hash(&mut h);
    }
    format!("src{:016x}", h.finish())
}

/// A directory-name-safe tag for a profile. A `Debug` formatting of the enum would do, but
/// spelling it keeps the directory name stable if that derive ever changes.
pub(super) fn profile_tag(p: Profile) -> &'static str {
    match p {
        Profile::Debug => "debug",
        Profile::Release => "release",
    }
}
