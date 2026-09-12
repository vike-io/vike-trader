//! `RhaiStrategy<B>`: runs a compiled Rhai script as a `vike_model::Strategy<B>`. This is the
//! binding that plugs the `ScriptCtx`/`SharedCtx` state (`ctx.rs`) and the registered host
//! functions (`engine.rs`'s `register_reads`/`register_verbs`/`register_indicators`, wired
//! together by `build_engine`) into the real strategy seam every engine (backtest and live)
//! drives strategies through.
//!
//! Design note (SP1, revised — order-safety fix): a script's top-level statements (anything
//! outside a `fn`) run EXACTLY ONCE, in `compile`, via `Engine::run_ast_with_scope` — never again
//! per hook call. This matters because `Engine::call_fn`/`call_fn_with_options`'s default
//! `eval_ast: true` RE-EVALUATES the AST's top-level statements into the passed scope on every
//! single call before invoking the target function ("this allows a script to load necessary
//! modules", per its own doc) — harmless for a `const`, but if the top level is a bare order verb
//! like `buy(1.0);`, that default silently resubmits an order on every hook call, not just once
//! (the bug this revision fixes). `run_hook` therefore calls with
//! `CallFnOptions::default().eval_ast(false)`, so the top level is never touched again after
//! `compile`'s one-time run; any intents that one-time run itself recorded belong to no bar and
//! are discarded immediately (see `compile`).
//!
//! Constants use Rhai `const`, which still needs to be visible inside a script's `fn` bodies even
//! though the top level now runs only once. `compile`'s one-time `run_ast_with_scope` populates a
//! persisted `Scope` (`RhaiStrategy::scope`) with whatever the top level declared; `run_hook`
//! clones it fresh for every call rather than passing `&mut self.scope` straight through, so a
//! hook body's own `let`-declared locals live only for that one call (dropped with the clone) and
//! never pile up in `self.scope` across bars — while the top-level `const` is still visible
//! because it was already present in the scope BEFORE the clone was taken, not something the call
//! itself has to (re-)declare. This keeps the original invariant intact: scripts hold **no
//! cross-bar mutable state** — all state is host-side (the broker's `position()` + the indicator
//! caches in `ScriptCtx`). Script-managed persistent state is a follow-up.
//!
//! Historical pitfall (kept for anyone tempted to "simplify" back to plain `call_fn` with a
//! brand-new `Scope` per call): `rewind_scope`'s default `true` POPS a THIS-CALL top-level
//! declaration (e.g. `const K = 7.0;`) back off the scope before the target function is ever
//! invoked, so a bare reference to it inside the function body fails with
//! `ErrorVariableNotFound` (verified empirically against `rhai` 1.25.1 — this is not documented
//! behavior worth assuming). That pitfall cannot recur now: the top level runs only once (in
//! `compile`) and is never re-run inside a hook call's own `eval_ast` phase, so there is nothing
//! left for that call's own rewind to pop.
//!
//! Runtime errors FAIL SAFE: a hook that errors emits zero orders that bar (any intents recorded
//! before the error are discarded) and the strategy keeps running; after 10 CONSECUTIVE errors it
//! self-disables (every later hook call becomes a no-op) rather than risk a wedged script hammering
//! the engine forever. A successful hook call resets the consecutive-error counter.

use crate::ctx::{Intent, ScriptCtx, SharedCtx};
use crate::engine::build_engine;
use std::marker::PhantomData;
use vike_model::{Bar, Broker, Fill, Strategy};

/// A Rhai compile or setup error, surfaced from [`RhaiStrategy::compile`]. Wraps the underlying
/// `rhai` error's rendered message — callers that need to show a script author what went wrong
/// get a plain `Display`/`Error` string, not a `rhai`-specific type leaking through this crate's
/// public API.
#[derive(Debug)]
pub struct ScriptError(pub String);

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ScriptError {}

/// A compiled Rhai script mounted as a `Strategy<B>`. Holds its own resource-limited `rhai::Engine`
/// alongside the compiled `AST` and `SharedCtx` (never a borrowed `&mut B` — see `ctx.rs`'s module
/// doc), plus which of `on_start`/`on_bar`/`on_stop` the script actually defines (an undefined
/// hook is a no-op, never a call attempt) and the fail-safe consecutive-error counter.
///
/// `B` is carried only via `PhantomData<fn(&mut B)>`: nothing here stores a `B`-typed value, but
/// the `Strategy<B>` impl below needs the type parameter, and the `fn(&mut B)` marker (rather than
/// `PhantomData<B>`) keeps this struct's auto-trait bounds (`Send`/`Sync`) from depending on `B`'s.
pub struct RhaiStrategy<B: Broker> {
    engine: rhai::Engine,
    ast: rhai::AST,
    /// The AST's top level (anything outside a `fn`), evaluated EXACTLY ONCE by `compile` via
    /// `Engine::run_ast_with_scope` — holds whatever that one-time run declared (a top-level
    /// `const`, typically). `run_hook` clones this on every call rather than mutating it in
    /// place, so a hook body's own local variables never accumulate here across bars.
    scope: rhai::Scope<'static>,
    ctx: SharedCtx,
    has_on_start: bool,
    has_on_bar: bool,
    has_on_stop: bool,
    errors: u32,
    disabled: bool,
    _marker: PhantomData<fn(&mut B)>,
}

impl<B: Broker> RhaiStrategy<B> {
    /// Compiles `src` into a mounted strategy, with no `param()` overrides (every `param(name,
    /// default)` call resolves to its own `default`). Delegates to [`Self::compile_with_params`]
    /// with an empty override map.
    pub fn compile(src: &str) -> Result<Self, ScriptError> {
        Self::compile_with_params(src, indexmap::IndexMap::new())
    }

    /// Like [`Self::compile`], but injects `overrides` for `param(name, default)` calls BEFORE the
    /// one-time top-level run — so a swept value is baked into the persisted `scope` (and thus
    /// visible for the strategy's whole lifetime), not merely applied per hook call. Fails only on
    /// a Rhai parse/compile error; which hooks the script defines is detected via
    /// `AST::iter_functions` (a 0-arg `fn on_bar()` etc.) so a hook the script never wrote is
    /// skipped entirely at run time rather than attempted and failing.
    pub fn compile_with_params(
        src: &str,
        overrides: indexmap::IndexMap<String, f64>,
    ) -> Result<Self, ScriptError> {
        Self::compile_with_indicators(src, overrides, &[])
    }

    /// Like [`Self::compile_with_params`], but also binds `indicators` — USER-written indicators
    /// loaded from `user_data/indicators/` — as zero-argument host functions, so a script can call
    /// `my_thing()` beside the built-in `sma()`.
    ///
    /// Each is fed the current bar exactly once per bar through the same cache the built-ins use.
    /// A name that would shadow a built-in indicator or a host verb is SKIPPED here (see
    /// `engine::user_indicator_conflict`, which the loader is expected to have already surfaced to
    /// the author) — so a conflicting file yields a function-not-found at the call site rather than
    /// silently redefining `sma`.
    pub fn compile_with_indicators(
        src: &str,
        overrides: indexmap::IndexMap<String, f64>,
        indicators: &[crate::RhaiIndicator],
    ) -> Result<Self, ScriptError> {
        let ctx = ScriptCtx::new();
        ctx.write().unwrap().overrides = overrides;
        let mut engine = build_engine(&ctx);
        crate::engine::register_user_indicators(&mut engine, &ctx, indicators);
        let ast = engine.compile(src).map_err(|e| ScriptError(e.to_string()))?;
        let defines =
            |name: &str| ast.iter_functions().any(|f| f.name == name && f.params.is_empty());
        let has_on_start = defines("on_start");
        let has_on_bar = defines("on_bar");
        let has_on_stop = defines("on_stop");

        // Run the AST's top-level statements (anything outside a `fn`) EXACTLY ONCE, here at
        // compile time — never again per hook call (see the module doc: this is the fix for the
        // "a top-level `buy(1.0);` resubmits every bar" bug). `run_ast_with_scope` populates
        // `scope` with whatever the top level declared (typically a `const`), which is what
        // keeps a top-level `const` visible from inside a script `fn` despite `run_hook` never
        // re-running the top level again.
        let mut scope = rhai::Scope::new();
        engine.run_ast_with_scope(&mut scope, &ast).map_err(|e| ScriptError(e.to_string()))?;
        // Whatever that one-time run recorded belongs to no bar — discard ALL of it so
        // `compile()` itself can never submit an order or leave behind a bogus warm-up sample:
        // - `intents`: e.g. a bare top-level `buy(1.0);` outside any `fn`.
        // - `indicators`/`fed_this_bar`: a top-level reference to an indicator (e.g. `sma(3);`
        //   outside a hook) would otherwise lazily construct it here and feed it once with the
        //   zero-bar (`ctx::zero_bar`, since no real bar has arrived yet at compile time) via
        //   `engine::indicator_value` — a phantom sample the first REAL `on_bar` call would
        //   silently build on. Clearing both here ensures a script's first real bar is the
        //   first bar any indicator ever sees.
        {
            let mut g = ctx.write().unwrap();
            g.intents.clear();
            g.indicators.clear();
            // Same reasoning for the user-written ones, and it bites harder: a `RhaiIndicator`
            // keeps state the AUTHOR wrote, so a phantom zero-bar sample would corrupt their own
            // recurrence (a running sum would carry a spurious 0.0 forever), not merely shift a
            // warm-up by one.
            g.user_indicators.clear();
            g.fed_this_bar.clear();
        }

        Ok(RhaiStrategy {
            has_on_start,
            has_on_bar,
            has_on_stop,
            engine,
            ast,
            scope,
            ctx,
            errors: 0,
            disabled: false,
            _marker: PhantomData,
        })
    }

    /// Snapshot reads -> call the named 0-arg fn -> drain intents onto the broker. Fail-safe: a
    /// script error is logged and swallowed (no panic, no orders that bar); 10 consecutive errors
    /// self-disable the strategy for the rest of its life.
    ///
    /// Calls with `eval_ast(false)` so the AST's top level (already run exactly once in
    /// `compile`) never re-executes here — otherwise a top-level `buy(1.0);` would resubmit an
    /// order on every single hook call (see the module doc). The scope passed in is a fresh
    /// CLONE of the persisted `self.scope` (which holds whatever the one-time top-level run
    /// declared, e.g. a `const`), not `self.scope` itself: cloning means a hook body's own
    /// `let`-declared locals live only for this one call and are dropped with the clone, never
    /// accumulating in `self.scope` across bars, while the top-level `const` is still visible
    /// because it was already in the scope BEFORE this call's clone was taken. `rewind_scope`
    /// is left at `false` to mirror the reviewed guidance but is moot here either way:
    /// `call_scope` is discarded when this function returns regardless of that flag.
    fn run_hook(&mut self, name: &str, broker: &mut B) {
        if self.disabled {
            return;
        }
        let sym = self.sym();
        {
            let mut g = self.ctx.write().unwrap();
            // Only ask the broker for a per-symbol read when there IS a symbol. Before the
            // first bar arrives (e.g. inside `on_start`), `sym()` is `""` — a real
            // `SimBroker`/`LiveBroker` routes `position("")`/`price("")` through its symbol
            // index and PANICS on an unknown symbol (see `sym()`'s doc), which is not caught
            // by this strategy's own fail-safe (that only catches `EvalAltResult`, not a Rust
            // panic). Leave `position`/`price` at their prior value (the `ScriptCtx::new()`
            // default of `0.0` on the very first call) instead: a pre-trade `on_start`
            // legitimately has no position or price yet.
            if !sym.is_empty() {
                g.position = broker.position(&sym);
                g.price = broker.price(&sym);
            }
            g.equity = broker.equity();
            g.index = broker.index() as i64;
            g.now = broker.now();
            g.intents.clear();
        }
        let mut call_scope = self.scope.clone();
        let options = rhai::CallFnOptions::default().eval_ast(false).rewind_scope(false);
        let res =
            self.engine.call_fn_with_options::<()>(options, &mut call_scope, &self.ast, name, ());
        match res {
            Ok(()) => {
                self.errors = 0;
            }
            Err(e) => {
                self.errors += 1;
                tracing::warn!(
                    script_error = %e,
                    hook = name,
                    "rhai hook errored; emitting no orders this bar"
                );
                if self.errors >= 10 {
                    self.disabled = true;
                    tracing::error!("rhai strategy disabled after 10 consecutive errors");
                }
                self.ctx.write().unwrap().intents.clear();
                return;
            }
        }
        let intents: Vec<Intent> = std::mem::take(&mut self.ctx.write().unwrap().intents);
        for it in intents {
            match it {
                Intent::Market { side, qty } => broker.submit_market(&sym, side, qty),
                Intent::Limit { side, qty, price } => broker.submit_limit(&sym, side, qty, price),
            }
        }
    }

    /// single-symbol MVP: resolves the CURRENT bar's symbol tag from `ScriptCtx::cur_bar`, which
    /// `Strategy::on_bar` below sets fresh every bar — falling back to `""` before the first bar
    /// arrives (e.g. inside `on_start`). Deliberately does NOT call `broker.bars("")`: the mock
    /// broker in this crate's tests ignores the symbol argument and always returns its one bar
    /// series, which made that lookup look safe, but a real `SimBroker`/`LiveBroker` rejects the
    /// unknown symbol `""` and panics. The current bar already carries the mounted symbol's tag
    /// (`Bar::symbol`), so reading it directly needs no broker call at all. The SAME panic risk
    /// applies to `broker.position("")`/`broker.price("")` — `run_hook` guards both behind
    /// `!sym.is_empty()` for exactly this reason (a real Rust panic, unlike a script error,
    /// is not caught by this strategy's fail-safe).
    fn sym(&self) -> String {
        self.ctx.read().unwrap().cur_bar.symbol.clone().unwrap_or_default()
    }
}

impl<B: Broker> Strategy<B> for RhaiStrategy<B> {
    fn on_start(&mut self, broker: &mut B) {
        if self.has_on_start {
            self.run_hook("on_start", broker);
        }
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        if !self.has_on_bar {
            return;
        }
        {
            let mut g = self.ctx.write().unwrap();
            g.cur_bar = bar.clone();
            g.fed_this_bar.clear();
        }
        self.run_hook("on_bar", broker);
    }

    fn on_stop(&mut self, broker: &mut B) {
        if self.has_on_stop {
            self.run_hook("on_stop", broker);
        }
    }

    fn on_fill(&mut self, _broker: &mut B, _fill: &Fill) {
        // read-only in SP1: a script observes its resulting position via `position()`, not a
        // dedicated fill callback.
    }
}

/// Compile `src` with no overrides and return the params it requested, in declaration order,
/// as `(name, default)` — for the sweep UI. Broker-agnostic: runs the top level (where `param()`
/// is called) without mounting a `Strategy<B>`.
pub fn discover_params(src: &str) -> Result<Vec<(String, f64)>, ScriptError> {
    let ctx = ScriptCtx::new();
    let engine = build_engine(&ctx);
    let ast = engine.compile(src).map_err(|e| ScriptError(e.to_string()))?;
    let mut scope = rhai::Scope::new();
    engine.run_ast_with_scope(&mut scope, &ast).map_err(|e| ScriptError(e.to_string()))?;
    let g = ctx.read().unwrap();
    Ok(g.params_seen.iter().map(|(k, v)| (k.clone(), *v)).collect())
}
