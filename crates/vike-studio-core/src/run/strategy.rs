//! Resolving a strategy spec (Rhai, native registry, or runtime plugin) into the boxed strategy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use vike_analytics::BacktestResult;
use vike_model::Strategy;
use vike_script::RhaiStrategy;
use vike_sim::SimBroker;
use vike_strategy_plugin::host::{PluginStrategy, PluginVTable};
use vike_strategy_plugin::loader;

use crate::spec::{StrategySpec, params_with_overrides};

/// Why a Run could not produce a result.
#[derive(Debug, Clone)]
pub enum RunError {
    /// The Rhai script failed to compile (parse/compile error, line/col in the message).
    Compile(String),
    /// The data slice could not be loaded, or yielded no bars/ticks.
    Data(String),
    /// A NATIVE strategy could not be built: an unknown registry name, or a params table the
    /// registry rejected. The native twin of [`RunError::Compile`] — kept a separate variant so
    /// the UI can say "no such strategy" instead of "compile error".
    Strategy(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Compile(msg) => write!(f, "compile error: {msg}"),
            RunError::Data(msg) => write!(f, "{msg}"),
            RunError::Strategy(msg) => write!(f, "strategy error: {msg}"),
        }
    }
}

impl std::error::Error for RunError {}

pub type RunOutcome = Result<BacktestResult, RunError>;

/// Where a [`StrategySpec::Plugin`] artifact is looked up: `<project>/user_data/plugins`, resolved
/// by `vike_model::paths::state_path`'s ordinary project walk from this process's working directory.
///
/// ⚠⚠ **THIS IS A SECOND PROJECT WALK, and `crates/vike-boot/tests/one_owner.rs`'s `WALK` table
/// classifies it as one. Read this before reusing the shape.** `vike-boot`'s whole contract is
/// that ONE walk decides: every project-relative path a root uses is derived from `Booted`, because
/// the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-BLIND and a second call answers with
/// whatever the working directory happens to sit above. This function performs exactly that second
/// call, so a daemon booted with `VIKE_SETTINGS_DIR` pointing somewhere other than its working
/// directory would load plugins from a DIFFERENT project than the one it booted into — silently,
/// with a `plugin artifact not found` at best and somebody else's artifact at worst.
///
/// ⚠ **`a_crate_that_boots_may_not_walk_again` does NOT catch it**, and that is worth knowing
/// rather than discovering: that rule's roster is the crates whose own source calls
/// `vike_boot::boot`. This is a LIBRARY, reached from a booted root (`vike-backend backtest`) one
/// crate down, so the walk is outside the rule by construction. The `WALK` row is what records it.
///
/// **The bound, MEASURED rather than hoped for.** On every SHIPPED unit the two answers cannot
/// disagree: `crates/vike-ops/tests/container_deploy/deploy_layout_gate/project_folder.rs`'s `every_daemon_unit_is_one_project_folder`
/// requires each daemon unit's `VIKE_SETTINGS_DIR` to be exactly `<WorkingDirectory>/settings`, so
/// the boot's project and this walk's project are the same directory by gate. **Two holes survive
/// it**: an `EnvironmentFile=` line BEATS `Environment=`, so an operator's `<root>/.env` can move
/// `VIKE_SETTINGS_DIR` where no gate can see; and a hand-run daemon (`cd elsewhere;
/// VIKE_SETTINGS_DIR=… vike-backend backtest --addr`) diverges outright.
///
/// **The cure, and why it is not here yet.** [`plugins_dir_from`] already takes the directory as a
/// parameter — answer (2) of that gate's own panic message — so what is missing is a caller holding
/// `Booted`'s answer. Threading it through `build_strategy` reaches four runners, three `*_local`
/// wire entries and `StudioRunTable`'s boxed-fn type in another crate; and the one composition root
/// that can name both halves (`crates/vike/src/main.rs`, layer 68) is FORBIDDEN to resolve a path
/// at all — `crates/vike-ops/tests/container_deploy/multicall_gate.rs`'s `the_dispatcher_starts_nothing` fails a PR
/// that so much as names `state_path::` there. The shape that fits is the one `study_run_fn`
/// already uses beside it: hand the dispatcher a FUNCTION ITEM and let `backtest_cli`'s `--addr`
/// arm — the rung that already walks for this daemon's settings — call it with the resolved root.
/// That is a new seam across three crates, which is a design change rather than a fix, so it is
/// named here rather than smuggled in.
///
/// What is true today: every shipped unit runs `WorkingDirectory=<project>`, so this resolves the
/// project the daemon was installed as — the same walk `state_path::user_runs_dir` and its
/// siblings already answer every other `user_data/` question with. It reads no environment
/// variable, so it adds no row to `vike_ops::settings::SETTINGS` and none to its `LIBRARY_PIN`
/// ratchet.
///
/// The fallback is the plain relative path, which is what the builder service's own out-dir
/// setting already defaults to: with no project marker above the working
/// directory there is nothing to resolve, and refusing outright would turn "no project" into a
/// different failure from the `LoadError::Missing` a caller is about to get anyway — with a worse
/// message.
#[must_use]
pub fn plugins_dir() -> PathBuf {
    plugins_dir_from(std::env::current_dir().ok().as_deref())
}

/// [`plugins_dir`] with the working directory supplied — `None` meaning "this process has none"
/// (a deleted or unreadable CWD, which `std::env::current_dir` reports as an error).
///
/// ⚠ **Split out so the FALLBACK branch can be proved rather than assumed.** The test that used to
/// cover it handed the resolver a temp directory and asserted `None` only *if* the resolver
/// happened to answer `None` — a conditional assertion, so the fallback was still reachable by
/// nothing. It cannot be made unconditional against a real filesystem either: the walk climbs to
/// `/`, and no test can promise that no ancestor of a temp directory carries a project marker.
/// Passing `None` here is the one input that reaches the fallback deterministically, on every box.
pub(super) fn plugins_dir_from(cwd: Option<&Path>) -> PathBuf {
    cwd.and_then(vike_model::paths::state_path::user_plugins_dir).unwrap_or_else(|| {
        PathBuf::from(vike_model::paths::state_path::PROJECT_USER_DATA_DIR)
            .join(vike_model::paths::state_path::PLUGINS_SUBDIR)
    })
}

/// The artifact a `(name, sha)` pair names, under `plugins_dir` — the design's
/// `user_data/plugins/<name>-<sha>.so`, and the exact shape the builder service writes
/// (`vike_strategy_builder::builder::artifact_strategy_name` parses it back).
fn plugin_artifact_path(plugins_dir: &Path, name: &str, sha: &str) -> PathBuf {
    plugins_dir.join(format!("{name}-{sha}.so"))
}

/// Every plugin artifact this process has already `dlopen`ed, keyed by its path.
///
/// **Why a cache and not a load per call.** `build_strategy` is called once per Run and once per
/// SWEEP POINT, and a sweep runs the SAME artifact with N different params tables. Without this,
/// an N-point sweep would issue N `dlopen`s of one file; with it, the artifact is opened once and
/// `create` is called N times — which is exactly the cost model
/// [`build_strategy_with`]'s own comment describes ("N constructions, not N compiles") and the
/// opposite of Rhai's per-point recompile.
///
/// **Why keeping the vtable past the `LoadedPlugin` is sound.** `vike_strategy_plugin::loader`
/// never calls `dlclose` — deliberately, and its module header says so — so the mapping the
/// function pointers point into lives for the life of the process whether or not anything holds
/// the handle. [`PluginVTable`] is `Copy` and holds only `extern "C"` function pointers, which are
/// `Send + Sync`.
///
/// The lock is held ACROSS the load on purpose: two sweep workers reaching a cold artifact at the
/// same moment should produce one `dlopen`, not two.
fn loaded_plugin_vtable(path: &Path) -> Result<PluginVTable, RunError> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, PluginVTable>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // A poisoned lock here means a previous caller panicked while holding it; the map is a plain
    // path -> POD table with no invariant a panic could have broken, so recovering is strictly
    // better than turning one unrelated panic into every later Run failing.
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(vt) = guard.get(path) {
        return Ok(*vt);
    }
    let loaded = loader::load(path).map_err(|e| RunError::Strategy(e.to_string()))?;
    guard.insert(path.to_path_buf(), loaded.vtable);
    Ok(loaded.vtable)
}

/// Resolve a [`StrategySpec`] into the boxed strategy every runner drives — the ONE place the
/// Rhai/native/plugin branch is taken. `dyn` dispatch is the harness registry's own shape and is
/// noise next to loading the slice (see `vike_backtest::harness::registry`'s module doc).
///
/// The plugin arm resolves its artifact under [`plugins_dir`]; [`build_strategy_in`] is the same
/// function with that directory supplied, which is what a caller that already knows where
/// `user_data` is should use.
pub fn build_strategy(spec: &StrategySpec) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    build_strategy_in(spec, &plugins_dir())
}

/// [`build_strategy`] with the plugin-artifact directory supplied rather than resolved.
pub fn build_strategy_in(
    spec: &StrategySpec,
    plugins_dir: &Path,
) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    match spec {
        StrategySpec::Rhai(src) => Ok(Box::new(
            RhaiStrategy::<SimBroker>::compile(src)
                .map_err(|e| RunError::Compile(e.to_string()))?,
        )),
        StrategySpec::Native { name, params } => {
            vike_backtest::harness::strategy_by_name(name, params)
                .map_err(|e| RunError::Strategy(e.to_string()))
        }
        // ⚠ **Fix-round-1 CRITICAL finding, and the PERMANENT half of the fix — read this before
        // deleting the empty-sha arm below when the loader lands.** Every caller that builds a
        // `StrategySpec::Plugin` from an absent sha (`StudioState::current_spec`,
        // `SavedStrategy::spec`) does so via `plugin_sha.clone().unwrap_or_default()` — an EMPTY
        // STRING, not a sentinel the type can distinguish from a real sha. This function is the
        // ONE place every Plugin spec is actually built, on EVERY path that reaches it: locally
        // (`run_slice`/`compare_all_slice`) and remotely (this same function runs again on the
        // compute daemon, reached through `to_strategy_spec`/`run_sweep_local`/
        // `run_walkforward_local`). So refusing an empty sha HERE, unconditionally, closes every
        // caller at once — today's four (Run/Sweep/Walk-Forward/Compare-all) and any future one —
        // rather than relying on each dispatcher to remember to ask `run_blocked_reason` first
        // (studio.rs now does too, as the belt; this is the buckle).
        //
        // ⚠ **This arm SURVIVED the loader wiring, which is what it was written to demand.** The
        // loader call is in the arm below, for a NON-empty sha; a real
        // `vike_strategy_plugin::loader::load` would itself fail to resolve `<name>-.so`, but that
        // is an ACCIDENT of a lookup failing, not a stated refusal — and relying on an accident is
        // exactly what this record was written to stop doing. It stays UNCONDITIONAL.
        StrategySpec::Plugin { name, sha, .. } if sha.trim().is_empty() => {
            Err(RunError::Strategy(format!(
                "plugin `{name}` has no sha — an empty sha names no artifact. Build it first \
                 (Studio: Run/Sweep/Walk-Forward are disabled until a Build returns one; \
                 Compare-all: this row reports this refusal as its result rather than a fabricated \
                 one)."
            )))
        }
        // THE JOIN — the design's Flow step 6. This arm used to return "the runtime loader is not
        // wired into this Run pipeline", which was the whole of what
        // `docs/decisions/0082-the-plugin-mechanism-lands-without-the-feature.md` recorded.
        //
        // It runs on BOTH sides of the wire and that is the point: locally through
        // `run_slice`/`compare_all_slice`, and on the compute daemon through
        // `wire_run::to_strategy_spec` → `run_slice_local`. The daemon therefore loads the
        // artifact WITHOUT ever knowing a builder exists — it is handed a sha and reads a file,
        // exactly as the design requires ("the backtest server never talks to the builder — they
        // hand off through the FILESYSTEM"). Nothing here polls for an artifact to appear: a
        // missing file is a refusal on the spot, naming the build, never a wait.
        //
        // ⚠ `unsafe` stays where it was quarantined. Everything this arm touches
        // (`loader::load`, `PluginStrategy`) is the safe face of `vike-strategy-plugin`; this
        // crate declares no `unsafe` of its own and inherits the workspace `forbid`.
        StrategySpec::Plugin { name, sha, params } => {
            let path = plugin_artifact_path(plugins_dir, name, sha);
            let vtable = loaded_plugin_vtable(&path)?;
            // Params cross the C-ABI as TEXT, never as a `toml::Value` — no Rust type crosses that
            // boundary by value, and the plugin parses the document on its own side. An empty
            // table serializes to an empty string, which is a valid empty TOML document (the one
            // `vike-strategy-plugin`'s own `a_template_built_plugin_accepts_a_real_params_document`
            // pins, after a `str::parse::<toml::Value>()` in the template made every params string
            // — the empty one included — produce a null handle and a strategy that traded nothing).
            let params_toml = toml::to_string(params).map_err(|e| {
                RunError::Strategy(format!(
                    "plugin `{name}`: serializing its params table to TOML failed: {e}"
                ))
            })?;
            Ok(Box::new(PluginStrategy::<SimBroker>::new(vtable, &params_toml)))
        }
    }
}

/// [`build_strategy`] with one sweep point's `(name, value)` overrides applied: Rhai gets them as
/// script `param()` bindings (`compile_with_params`), native gets them merged into its params table.
pub fn build_strategy_with(
    spec: &StrategySpec,
    overrides: &[(String, f64)],
) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    build_strategy_with_in(spec, overrides, &plugins_dir())
}

/// [`build_strategy_with`] with the plugin-artifact directory supplied rather than resolved — the
/// sweep twin of [`build_strategy_in`].
pub fn build_strategy_with_in(
    spec: &StrategySpec,
    overrides: &[(String, f64)],
    plugins_dir: &Path,
) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    match spec {
        StrategySpec::Rhai(src) => {
            let map: indexmap::IndexMap<String, f64> = overrides.iter().cloned().collect();
            Ok(Box::new(
                RhaiStrategy::<SimBroker>::compile_with_params(src, map)
                    .map_err(|e| RunError::Compile(e.to_string()))?,
            ))
        }
        StrategySpec::Native { name, params } => build_strategy_in(
            &StrategySpec::Native {
                name: name.clone(),
                params: params_with_overrides(params, overrides),
            },
            plugins_dir,
        ),
        // A plugin's overrides merge into its params table exactly as Native's do above — the
        // plugin's `create` takes params as TOML, so a sweep point's overrides land in the same
        // table an ordinary Run would build.
        //
        // Worth stating here because it is the OPPOSITE performance shape from Rhai, and it is now
        // a fact rather than a forecast: a sweep of N grid points `dlopen`s the `.so` ONCE — the
        // artifact is already built, and `loaded_plugin_vtable` caches the opened table by path —
        // and calls `create` N times, one per params table. Rhai recompiles the script per point
        // (`compile_with_params`); the plugin does not. The cost of a plugin sweep is N
        // constructions, not N compiles and not N loads.
        StrategySpec::Plugin { name, sha, params } => build_strategy_in(
            &StrategySpec::Plugin {
                name: name.clone(),
                sha: sha.clone(),
                params: params_with_overrides(params, overrides),
            },
            plugins_dir,
        ),
    }
}
