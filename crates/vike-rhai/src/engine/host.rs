//! The host surface a strategy script calls: `build_engine`, the reads and `param` (its order verbs
//! are the `strategy` object's, `crate::tactics`).

use crate::context::SharedContext;
use crate::engine::SCRIPT_LOG_TARGET;
use crate::engine::builtin::register_indicators;
use crate::engine::installed::INSTALLED;
use crate::engine::user::register_user_indicators;

/// Point a script's `print`/`debug` at the `tracing` facade instead of the HOST PROCESS'S STDOUT.
///
/// ⚠ **This is a correctness line, not a tidy-up, and the host it protects signs real orders.**
/// `rhai::Engine::new()` wires both hooks to `println!` (rhai-1.25.1 `src/engine.rs`'s `new`:
/// `engine.print = Some(Box::new(|s| println!("{s}")))`), so on a default engine a user's
/// `print("hi")` writes to whatever stream the embedding binary owns. This crate is embedded in
/// binaries whose stdout is a PROTOCOL rather than a console:
///
/// * `vike-mcp` speaks newline-JSON-RPC over `io::stdout()`, and two of its tools
///   (`validate_strategy`, `discover_params`) compile a caller-supplied script in-process —
///   `crates/vike-mcp/src/offline_tools.rs`'s `tool_validate_strategy` reaches
///   [`crate::discover_params`], whose one-time top-level run is exactly where an author writes
///   `print`. A stray line there is a malformed frame, not noise.
/// * `vike-tradehub` mounts a `RhaiStrategy` on the LIVE core (`[strategy] rhai = …`) and keeps
///   its own stdout protocol-only for its newline-JSON control channel.
///
/// Nothing in this workspace READS a script's `print` off stdout — no tool, no test, no template
/// and no `.rhai` file in the tree emits one — so there is no consumer to break, only streams to
/// stop corrupting. (`vike-backtest`'s `backtesthub` bin is the third stream at risk and the mildest:
/// its stdout is a RESULT, not a protocol, so a stray line garbled a table rather than a frame.)
///
/// ⚠ **What this MOVES rather than removes is the VOLUME, and it moves it onto disk.** A `print`
/// inside `on_bar` fires once per bar either way; what changes is that a `tracing` event at INFO
/// reaches vike-log's FILE layer, which defaults to `trace` and rolls daily — so a chatty script
/// over a long backtest now writes into `<project>/settings/state/logs` instead of scrolling past a
/// terminal. That is the hazard the `preferences.log_file_level` row exists for
/// (`crates/vike-log/src/lib.rs`'s `file_level_directive` carries the measurements, one of them a
/// run that nearly filled a live trading node's disk), and a script's diagnostics are subject to
/// it like anything else. INFO rather than TRACE is deliberate — a diagnostic the author
/// explicitly asked for should be visible at the default console level — but neither stream makes
/// a per-bar `print` free, and this one leaves a file.
///
/// The study tier decided this first and is the spelling this one matches:
/// `crates/vike-study-runner/src/rhai_study/bind.rs`'s `build_engine`, whose module doc carries the
/// same argument. Both hooks are REDIRECTED rather than removed — a script's diagnostics are worth
/// keeping, just never on a stream somebody else owns.
///
/// `tier` distinguishes the two engines this crate builds (`build_engine` for a strategy,
/// `crates/vike-rhai/src/indicator.rs`'s `indicator_engine` for a user indicator) on one shared
/// target. It is a PARAMETER rather than two copies of these four lines precisely because missing
/// the second engine is the easy mistake here — `crates/vike-rhai/tests/module_import_refused.rs`
/// exists because the previous default-engine capability was closed in one of them first.
/// `crates/vike-rhai/tests/script_print_is_not_stdout.rs` gates both.
pub(crate) fn redirect_script_output(engine: &mut rhai::Engine, tier: &'static str) {
    engine.on_print(move |s| tracing::info!(target: SCRIPT_LOG_TARGET, tier, "{s}"));
    engine.on_debug(move |s, source, pos| {
        tracing::debug!(
            target: SCRIPT_LOG_TARGET,
            tier,
            source = source.unwrap_or(""),
            position = %pos,
            "{s}"
        );
    });
}

/// The resource ceilings of one tier's sandbox. A size of `0` is UNLIMITED, rhai's own convention.
///
/// A PARAMETER of [`sandboxed_engine`] rather than a constant inside it because the tiers differ on
/// purpose: a strategy hook and an indicator run per bar inside a hot fold ([`Self::SCRIPT`]), while
/// a study runs ONCE over a whole window and holds the arrays the host hands it out of the store
/// (`crates/vike-study-runner/src/rhai_study/bind.rs`'s `MAX_OPERATIONS` carries that argument).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxLimits {
    /// Operations one call may spend (`call_fn` gets a fresh counter).
    pub max_operations: u64,
    /// The recursion ceiling — a runaway recursion is a stack overflow no operation counter catches.
    pub max_call_levels: usize,
    /// The longest string, in bytes.
    pub max_string_size: usize,
    /// The longest array.
    pub max_array_size: usize,
    /// The largest object map.
    pub max_map_size: usize,
}

impl SandboxLimits {
    /// A strategy's and a user indicator's ceilings, PER HOOK INVOCATION, not cumulative across the
    /// strategy's lifetime.
    pub const SCRIPT: Self = Self {
        max_operations: 2_000_000,
        max_call_levels: 64,
        max_string_size: 8 * 1024,
        max_array_size: 4096,
        max_map_size: 4096,
    };
}

/// The sandbox EVERY Rhai engine in the workspace starts from — a strategy's (`build_engine`), a
/// user indicator's (`crates/vike-rhai/src/indicator.rs`'s `indicator_engine`) and a study's
/// (`crates/vike-study-runner/src/rhai_study/bind.rs`'s `build_engine`) alike: the module resolver
/// that refuses every `import`, the tier's [`SandboxLimits`] (opt-in on a plain
/// `rhai::Engine::new()` — default is UNLIMITED) so a runaway or malicious script cannot hang the
/// loop, and the script's `print`/`debug` redirected off stdout (`redirect_script_output`, under
/// `tier`). ONE function, so a third engine cannot be the one that misses a line.
///
/// ⚠ **`set_module_resolver` is the load-bearing line in this function, not a tidy-up.**
/// `rhai::Engine::new()` installs a `FileModuleResolver` rooted at the process's working directory
/// (rhai-1.25.1 `src/engine.rs`'s `new`), so `import "…" as m;` READS A FILE FROM DISK on a default
/// engine — the daemon's working directory being `<project>`, that is the operator's settings tree,
/// their credential store included. It is a capability NOBODY GRANTED: it appears in no
/// registration list because it is the interpreter's default rather than anything this host handed
/// over, which is exactly the kind of surface an allow-list cannot see.
/// `docs/decisions/0024-rhai-strategies-live.md` names "network, filesystem, process, or credential
/// access from a script" as a thing that would REOPEN the verdict that lets a script mount live,
/// and rests rail 4 on the live engine surface being exactly the backtest one — so closing this
/// keeps a premise that record already relies on rather than narrowing a decided grant.
/// `DummyModuleResolver` makes every `import` a refusal;
/// `crates/vike-rhai/tests/module_import_refused.rs`'s
/// `a_strategy_cannot_import_a_module_because_that_would_be_a_file_verb` is the gate.
pub fn sandboxed_engine(limits: SandboxLimits, tier: &'static str) -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    engine.set_max_operations(limits.max_operations);
    engine.set_max_call_levels(limits.max_call_levels);
    engine.set_max_string_size(limits.max_string_size);
    engine.set_max_array_size(limits.max_array_size);
    engine.set_max_map_size(limits.max_map_size);
    redirect_script_output(&mut engine, tier);
    engine
}

/// Builds a fresh `rhai::Engine` wired for one [`crate::RhaiStrategy`]: bounded resource limits
/// (opt-in on a plain `rhai::Engine::new()` — default is UNLIMITED) so a runaway or malicious
/// script cannot hang the strategy loop, plus the full read/indicator host-function surface and the
/// `strategy` object registered against `ctx`. The limits are PER-HOOK-INVOCATION (each `call_fn` gets a fresh
/// operation counter), not cumulative across the strategy's lifetime.
///
/// Called ONCE per mounted strategy (`RhaiStrategy::compile_with_params` and `discover_params`),
/// never per bar — `run_rule` reuses the stored engine. That is what makes registering the whole
/// bound indicator set here affordable: rhai's function map is an identity-hashed `u64` table with
/// a per-call-site resolution cache, so the registration count changes neither per-call lookup cost
/// nor anything on the bar path.
///
/// The sandbox itself is [`sandboxed_engine`]'s, shared with the user-indicator engine.
pub(crate) fn build_engine(ctx: &SharedContext) -> rhai::Engine {
    // Never the host's stdout (`redirect_script_output`): this daemon-hosted engine is the tier
    // where that matters most.
    let mut engine = sandboxed_engine(SandboxLimits::SCRIPT, "strategy");
    register_reads(&mut engine, ctx);
    crate::tactics::register(&mut engine);
    register_indicators(&mut engine, ctx);
    // The INSTALLED user indicators, if a binary installed any — see `install_user_indicators`.
    // Registered BEFORE `register_user_indicators`' explicit set so an explicitly-passed indicator
    // wins on a name collision (`register_fn` replaces), which is what makes a test able to
    // override the process-wide set.
    if let Some(installed) = INSTALLED.get() {
        register_user_indicators(&mut engine, ctx, installed);
    }
    // param(name, default): a sweepable knob. Records (name, default) for discovery (first-seen
    // wins) and returns the injected override if present, else the default. Called at top level
    // (`let fast = param("fast", 5.0);`) so compile's one-time top-level run bakes the value in.
    let c = ctx.clone();
    // ⚠ The default is `Dynamic`, not `f64`. Registered as `f64` it accepted `param("fast", 5.0)`
    // and REJECTED `param("fast", 5)` with a rhai "Function not found" naming an i64 — a knob that
    // works or does not depending on whether the author typed a decimal point. Every shipped
    // template happens to write `5.0`, which is why it went unnoticed.
    engine.register_fn(
        "param",
        move |name: &str, default: rhai::Dynamic| -> Result<f64, Box<rhai::EvalAltResult>> {
            let d = arg(&default).ok_or_else(|| bad_arg("param", 2, &default))?;
            let mut g = c.write().unwrap();
            g.params_seen.entry(name.to_string()).or_insert(d);
            Ok(g.overrides.get(name).copied().unwrap_or(d))
        },
    );
    engine
}

/// Every NON-indicator host function name `build_engine` registers — the reads
/// ([`register_reads`]) and `param`.
///
/// GLOBAL names only: the `strategy` object's verbs, builders and reads (`crate::tactics`'s
/// `register`) each take one of that module's own types first, so rhai hashes them apart from any
/// indicator of the same name and none can be shadowed; they are not listed here.
///
/// This exists for the indicator bridge, not for documentation: `rhai::Engine::register_fn`
/// REPLACES a previous registration of the same name and arity rather than erroring, so binding a
/// registry indicator that happened to be called `close` would silently shadow `close()` for every
/// script in the workspace. `exclusion` therefore refuses to bind such a name. Nothing collides in
/// today's registry (its names are all technical-analysis terms) — this rule is green now and
/// bites only a future registry addition, the same shape as a `deny.toml` ban.
///
/// ⚠ A name added to `register_reads` must be added HERE too;
/// `host_fn_names_are_all_callable` catches a stale entry (a name listed but no longer
/// registered), not an unlisted new one — rhai exposes no "what did I register" query without its
/// `metadata` feature, which this workspace does not enable.
///
/// ⚠ **It is `pub` because a SHELL now advertises it, and the sentence above is why that is safe.**
/// `crates/vike-cli/src/cmd/backtest/strategies.rs`'s `HOST_FUNCTIONS` is one describing row per name here, and its
/// own completeness gate holds the two sets equal in both directions — so the human listing follows
/// the BINDING rather than a second list somebody keeps in step by hand. That matters on a RELEASE
/// install for the same reason `vike-cli indicators` exists: there is no source tree, so "read
/// `register_reads`" names a file the user does not have, and a roster typed into prose
/// over-advertises the moment a name is added or withdrawn. A script calling a name the host does
/// not bind raises every bar and self-disables, which is the silent failure an over-advertising
/// roster causes.
///
/// The order functions `buy`, `sell`, `market` and `limit` left this list with the functions
/// (2026-11-10): a script that calls one is refused at compile (`crate::strategy`'s
/// `REMOVED_FUNCTIONS`).
pub const HOST_FN_NAMES: &[&str] = &[
    "close", "open", "high", "low", "volume", "position", "price", "equity", "index", "now",
    "param",
];

/// Registers the snapshot-read host functions onto `engine`: `close`/`open`/`high`/`low`/
/// `volume`/`position`/`price`/`equity`/`index`/`now`. Each is a pure getter over one
/// `ScriptContext` field via a cloned `Arc`. Every name registered here must also appear in
/// [`HOST_FN_NAMES`].
pub(crate) fn register_reads(engine: &mut rhai::Engine, ctx: &SharedContext) {
    macro_rules! read_f64 {
        ($name:literal, $field:expr) => {{
            let c = ctx.clone();
            engine.register_fn($name, move || -> f64 {
                let g = c.read().unwrap();
                $field(&g)
            });
        }};
    }
    read_f64!("close", |g: &crate::context::ScriptContext| g.current_bar.close);
    read_f64!("open", |g: &crate::context::ScriptContext| g.current_bar.open);
    read_f64!("high", |g: &crate::context::ScriptContext| g.current_bar.high);
    read_f64!("low", |g: &crate::context::ScriptContext| g.current_bar.low);
    read_f64!("volume", |g: &crate::context::ScriptContext| g.current_bar.volume);
    read_f64!("position", |g: &crate::context::ScriptContext| g.position);
    read_f64!("price", |g: &crate::context::ScriptContext| g.price);
    read_f64!("equity", |g: &crate::context::ScriptContext| g.equity);
    let c = ctx.clone();
    engine.register_fn("index", move || -> i64 { c.read().unwrap().index });
    let c = ctx.clone();
    engine.register_fn("now", move || -> i64 { c.read().unwrap().now });
}

/// Reads one Rhai argument as an indicator parameter.
///
/// Rhai dispatches on argument TYPE, so the indicator host functions take `rhai::Dynamic` and
/// convert here. That is ONE registration per arity instead of one per int/float permutation
/// (a 3-parameter indicator would otherwise need eight), and it is also the only shape under which
/// a MIXED call resolves: `alma(9, 0.85, 6)` is the natural spelling of an integer period, a float
/// offset and an integer sigma, and every shipped template writes periods as bare integers
/// (`crates/vike-rhai/src/templates.rs`'s `SMA_CROSS` calls `sma(fast.to_int())`).
///
/// A non-numeric argument yields `None`, which every caller turns into a raised
/// [`bad_arg`] — see that function for why it is an error rather than a value. It deliberately does
/// NOT fall through to the registry default: an OMITTED argument already means "take the default"
/// (`vike_indicators::coerce` maps a missing/NaN entry to `ParamSpec::default`), so `sma("20")`
/// silently becoming `sma(20)` would hand a script author a plausible number for a typo.
pub(crate) fn arg(v: &rhai::Dynamic) -> Option<f64> {
    v.as_float().ok().or_else(|| v.as_int().ok().map(|i| i as f64))
}

/// A wrong-TYPE indicator argument, as a Rhai runtime error rather than a value.
///
/// ⚠ Returning `NaN` for `sma("20")` was the first shape of this and it is the wrong one, because
/// NaN is exactly what an indicator returns while it is WARMING UP. Every shipped template opens
/// `if h.is_nan() { return; }`, so a typed-wrong argument would make a strategy return on every bar
/// — silently, forever, looking like a warm-up that never finishes. The previous three-name binding
/// took a concrete `i64` and so failed loudly (no registration matched, function-not-found); moving
/// to `Dynamic` to reach multi-arity and fractional params gave that up, and this hands it back.
///
/// An error is loud in the way this engine already handles: `RhaiStrategy` counts consecutive
/// failures and self-disables, so the author sees a stopped strategy rather than a silent one.
/// An OMITTED argument is still "take the registry default" — that is `coerce`'s job and is
/// unaffected.
pub(crate) fn bad_arg(
    name: &str,
    position: usize,
    got: &rhai::Dynamic,
) -> Box<rhai::EvalAltResult> {
    format!(
        "{name}: argument {position} must be a number, got {}. An indicator parameter is numeric; \
         omit it to take the registry default.",
        got.type_name()
    )
    .into()
}
