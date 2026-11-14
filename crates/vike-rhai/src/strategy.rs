//! `RhaiStrategy<B>`: runs a compiled Rhai script as a `vike_model::Strategy<B>`. This is the
//! binding that plugs the `ScriptContext`/`SharedContext` state (`context.rs`) and the registered host
//! functions (`engine/`'s `register_reads`/`register_indicators` and `crate::tactics`'s `register`,
//! wired together by `build_engine`) into the real strategy seam every engine (backtest and live)
//! drives strategies through.
//!
//! Design note (SP1, revised — order-safety fix): a script's top-level statements (anything
//! outside a `fn`) run EXACTLY ONCE, in `compile`, via `Engine::run_ast_with_scope` — never again
//! per hook call. This matters because `Engine::call_fn`/`call_fn_with_options`'s default
//! `eval_ast: true` RE-EVALUATES the AST's top-level statements into the passed scope on every
//! single call before invoking the target function ("this allows a script to load necessary
//! modules", per its own doc) — harmless for a `const`, but a top level with a side effect repeats
//! it on every hook call, not just once (a bare top-level order verb resubmitted an order every
//! bar: the bug this revision fixed). `run_hook` and `run_rule` therefore call with
//! `CallFnOptions::default().eval_ast(false)`, so the top level is never touched again after
//! `compile`'s one-time run; anything that one-time run itself recorded belongs to no bar and is
//! discarded immediately (see `compile`).
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
//! caches in `ScriptContext`). Script-managed persistent state is a follow-up.
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
//! Runtime errors FAIL SAFE: a hook that errors emits zero orders that bar (anything it recorded
//! before the error is discarded) and the strategy keeps running; after 10 CONSECUTIVE errors it
//! self-disables (every later hook call becomes a no-op) rather than risk a wedged script hammering
//! the engine forever. A successful hook call resets the consecutive-error counter.
//!
//! **Two kinds of hook, told apart by their parameter.** The zero-argument hooks (`fn on_start()`,
//! `fn on_bar()`, `fn on_tick()`, `fn on_stop()`) run as they always did (`run_hook`): the global
//! reads, the indicators, `print`, the fail-safe. They place no order: the order functions they
//! used (`buy`, `sell`, `market`, `limit`) were REMOVED on 2026-11-10, with no fallback, and a
//! script that still calls one is refused at compile, by name, with the spelling that replaced it
//! ([`REMOVED_FUNCTIONS`]): rhai resolves a call only when its line runs, so a removed function
//! left to the run would fail every bar and self-disable the strategy, silently.
//! `fn on_bar(strategy)` writes orders in the order language (`crate::tactics`), a script's one
//! door for an order: from its first bar the script is a `vike_tactics::Rule` inside
//! `vike_tactics::RuleRun`, the one wrapper the lab's rules run in, which hears the fills and order
//! ends this type hands it.

use crate::context::{ScriptContext, SharedContext};
use crate::engine::host::build_engine;
use std::marker::PhantomData;
use vike_marketdata::{Bar, TickQuote};
use vike_model::{Fill, OrderBroker, OrderEvent, Strategy};
use vike_tactics::{Rule, RuleDefaults, RuleRun, Tactics};

/// The shortest event-time gap between two runs of a script's `on_tick` hook, in milliseconds
/// (decision 0245): the hook is SAMPLED, not run per market message. A venue's top of book moves hundreds of
/// times a second and the hook runs on the core's fold thread, so an unbounded script would be a stall the
/// operator cannot see; a strategy that must act on every tick is a native `Strategy` (`on_quote_tick`), not
/// a script. The first quote after a gap longer than this always runs it.
pub const TICK_HOOK_MIN_MS: i64 = 100;

/// The order functions the order language replaced, each refused at compile with its new spelling
/// (the module doc). A function the script defines itself under one of these names is its own.
pub(crate) const REMOVED_FUNCTIONS: [(&str, &str); 4] = [
    (
        "buy",
        "buy(qty) was removed: write strategy.order(\"buy\", OrderSide::Long).size(qty) inside fn \
         on_bar(strategy)",
    ),
    (
        "sell",
        "sell(qty) was removed: write strategy.order(\"sell\", OrderSide::Short).size(qty) inside fn \
         on_bar(strategy)",
    ),
    (
        "market",
        "market(side, qty) was removed: write strategy.order(name, OrderSide::Long).size(qty) \
         (OrderSide::Short for a side of -1) inside fn on_bar(strategy); strategy.entry(name, side) also \
         closes an opposite position",
    ),
    (
        "limit",
        "limit(side, qty, price) was removed: write strategy.order(name, OrderSide::Long).size(qty)\
         .limit(price) (OrderSide::Short for a side of -1) inside fn on_bar(strategy)",
    ),
];

/// The zero-argument hooks the order language renamed, `(old, new)`: a script still defining the old
/// one is refused at compile with the new name. No fallback read and no conversion (owner
/// 2026-11-13): an old hook read silently, or not at all, would trade at a size the script never
/// stated. The defaults hook is `vike_tactics::Rule::defaults`'s twin, its one name in both languages.
pub(crate) const RENAMED_HOOKS: [(&str, &str); 1] = [("header", "defaults")];

/// Refuses a script that defines a renamed hook ([`RENAMED_HOOKS`]) with the new name: at compile and
/// in the editor's compile dot ([`discover_params`]) alike.
fn refuse_renamed_hooks(ast: &rhai::AST) -> Result<(), ScriptError> {
    let defines = |name: &str| ast.iter_functions().any(|f| f.name == name && f.params.is_empty());
    match RENAMED_HOOKS.iter().find(|(old, _)| defines(old)) {
        Some((old, new)) => Err(ScriptError::plain(format!(
            "`fn {old}()` is now `fn {new}()`: the same map (size, pyramiding) under its new name"
        ))),
        None => Ok(()),
    }
}

/// The Rhai modules the order language renamed, `(old, new)`, each refused at compile with the new
/// name, so an old script fails with its fix instead of at its first bar with "module not found".
/// The Rust spelling of the same rename is a `vike_model::host_build::moved_name::RENAMED_NAMES` row.
pub(crate) const RENAMED_MODULES: [(&str, &str); 1] = [("Side", "OrderSide")];

/// Refuses a script that names a renamed module ([`RENAMED_MODULES`]), with the new name and the line.
fn refuse_renamed_modules(ast: &rhai::AST) -> Result<(), ScriptError> {
    let mut found = None;
    ast.walk(&mut |path: &[rhai::ASTNode]| {
        if let Some(rhai::ASTNode::Expr(rhai::Expr::Variable(v, _, pos))) = path.last()
            && !v.2.is_empty()
            && let Some((old, new)) = RENAMED_MODULES.iter().find(|(old, _)| v.2.root() == *old)
        {
            let why = format!(
                "`{old}` is now `{new}`: write {new}::{} (the order side's module was renamed)",
                v.1
            );
            found = Some(ScriptError::at(&why, *pos));
            return false;
        }
        true
    });
    found.map_or(Ok(()), Err)
}

/// Refuses a script that calls a removed order function, by name, with the new spelling
/// ([`REMOVED_FUNCTIONS`]) and the call's line. A method call (`.limit(price)` on an order) and a
/// qualified one are not the global function, and are not looked at; nor is a call of the script's
/// own `fn` of that name and arity.
fn refuse_removed(ast: &rhai::AST) -> Result<(), ScriptError> {
    let defines = |name: &str, arity: usize| {
        ast.iter_functions().any(|f| f.name == name && f.params.len() == arity)
    };
    let mut found = None;
    ast.walk(&mut |path: &[rhai::ASTNode]| {
        // a call that is its own statement (`buy(1.0);`) is a statement node, not an expression
        let call = match path.last() {
            Some(rhai::ASTNode::Stmt(rhai::Stmt::FnCall(call, pos)))
            | Some(rhai::ASTNode::Expr(rhai::Expr::FnCall(call, pos))) => Some((call, *pos)),
            _ => None,
        };
        if let Some((call, pos)) = call
            && !call.is_qualified()
            && !defines(&call.name, call.args.len())
            && let Some((_, why)) = REMOVED_FUNCTIONS.iter().find(|(name, _)| call.name == *name)
        {
            found = Some(ScriptError::at(why, pos));
            return false;
        }
        true
    });
    found.map_or(Ok(()), Err)
}

/// A Rhai compile or setup error, surfaced from [`RhaiStrategy::compile`]. `Display` is the
/// underlying `rhai` error's rendered message; the failing LINE is carried TYPED ([`Self::line`]),
/// read off rhai's own `Position`, so a caller that points an author at the line never parses the
/// rendered text — and no `rhai` type leaks through this crate's public API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    /// rhai's full rendering, position suffix included.
    message: String,
    /// The 1-based line of the innermost failing position, when rhai knows one.
    line: Option<u32>,
    /// The innermost error's own words WITHOUT its `(line N, position M)` suffix.
    summary: String,
}

impl ScriptError {
    /// An error with no source position (a setup failure this crate words itself).
    pub(crate) fn plain(message: String) -> Self {
        Self { summary: message.clone(), message, line: None }
    }

    /// An error this crate words itself, at a place in the script, rendered as rhai renders its own.
    fn at(summary: &str, pos: rhai::Position) -> Self {
        Self { message: format!("{summary} ({pos})"), line: line_of(pos), summary: summary.into() }
    }

    /// The 1-based line the script failed on, when rhai reported one.
    pub fn line(&self) -> Option<u32> {
        self.line
    }

    /// The failure in the script's own words, without the position suffix [`Self::line`] carries.
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// rhai's 1-based line, as the `u32` an editor counts in.
fn line_of(pos: rhai::Position) -> Option<u32> {
    pos.line().and_then(|l| u32::try_from(l).ok())
}

impl From<rhai::ParseError> for ScriptError {
    fn from(e: rhai::ParseError) -> Self {
        Self {
            message: e.to_string(),
            line: line_of(e.position()),
            summary: e.err_type().to_string(),
        }
    }
}

impl From<Box<rhai::EvalAltResult>> for ScriptError {
    /// The INNERMOST error is the one the author must fix (a failure inside a function the script
    /// called renders first, then the call site), so its position and words are the ones carried.
    fn from(e: Box<rhai::EvalAltResult>) -> Self {
        let inner = e.unwrap_inner();
        let pos = if inner.position().is_none() { e.position() } else { inner.position() };
        let text = inner.to_string();
        let summary = match text.strip_suffix(&format!(" ({})", inner.position())) {
            Some(bare) if !inner.position().is_none() => bare.to_string(),
            _ => text,
        };
        Self { message: e.to_string(), line: line_of(pos), summary }
    }
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ScriptError {}

/// A compiled Rhai script mounted as a `Strategy<B>`. Holds its own resource-limited `rhai::Engine`
/// alongside the compiled `AST` and `SharedContext` (never a borrowed `&mut B` — see `context.rs`'s module
/// doc), plus which of `on_start`/`on_bar`/`on_tick`/`on_stop` the script actually defines (an undefined
/// hook is a no-op, never a call attempt) and the fail-safe consecutive-error counter.
///
/// `B` is carried only via `PhantomData<fn(&mut B)>`: nothing here stores a `B`-typed value, but
/// the `Strategy<B>` impl below needs the type parameter, and the `fn(&mut B)` marker (rather than
/// `PhantomData<B>`) keeps this struct's auto-trait bounds (`Send`/`Sync`) from depending on `B`'s.
///
/// `B` is the order seam, `OrderBroker`, for both kinds of hook: a script whose `on_bar` takes
/// `strategy` (the module doc) is a `Strategy<B>` through `vike_tactics::RuleRun`, and the
/// zero-argument hooks only read; every broker a script mounts on (the simulator's, the live
/// core's, the test doubles) implements that seam.
pub struct RhaiStrategy<B: OrderBroker> {
    /// The script, until one whose `on_bar` takes `strategy` meets its first bar (the bar names the
    /// symbol `RuleRun` trades): from then on `run` holds it.
    script: Option<Script>,
    run: Option<RuleRun<Script>>,
    /// `fn inputs()`'s names (`crate::tactics::inputs_of`), read once at compile: `Strategy::inputs`.
    inputs: Vec<String>,
    /// `fn trades()`'s symbols (`crate::tactics::trades_of`), read once at compile: `RuleRun::trades`.
    trades: Vec<String>,
    /// The mount's own symbol ([`Self::mounted_on`]), the one `RuleRun` trades as its own; `None` takes
    /// the first bar's.
    own: Option<String>,
    _marker: PhantomData<fn(&mut B)>,
}

/// The compiled script, which of its hooks it defines, and the fail-safe.
struct Script {
    engine: rhai::Engine,
    ast: rhai::AST,
    /// The AST's top level (anything outside a `fn`), evaluated EXACTLY ONCE by `compile` via
    /// `Engine::run_ast_with_scope` — holds whatever that one-time run declared (a top-level
    /// `const`, typically). `run_hook` clones this on every call rather than mutating it in
    /// place, so a hook body's own local variables never accumulate here across bars.
    scope: rhai::Scope<'static>,
    ctx: SharedContext,
    has_on_start: bool,
    has_on_bar: bool,
    has_on_stop: bool,
    /// The script defines `fn on_tick()`: the quote lane reaches it ([`TICK_HOOK_MIN_MS`]); a script without it
    /// pays one bool on the quote the mount already hears.
    has_on_tick: bool,
    /// The event time of the quote the last `on_tick` ran on ([`TICK_HOOK_MIN_MS`]).
    last_tick_at: Option<i64>,
    errors: u32,
    disabled: bool,
    /// `fn on_bar(strategy)`: the order language's bar hook (`vike_tactics::Rule::on_bar`).
    takes_strategy: bool,
    /// `fn on_tick(strategy)` (only beside `takes_strategy`): a sampled quote's pass of the language.
    tick_takes_strategy: bool,
    /// `fn on_stop(strategy)` (only beside `takes_strategy`): the language's reads, its verbs refused.
    stop_takes_strategy: bool,
    /// `fn after_fill(strategy)` (`vike_tactics::Rule::after_fill`), only beside `takes_strategy`.
    has_after_fill: bool,
    /// `fn defaults()`'s map (`vike_tactics::Rule::defaults`), `RuleDefaults::PLAIN` without one.
    header: RuleDefaults,
}

impl<B: OrderBroker> RhaiStrategy<B> {
    /// Compiles `src` into a mounted strategy, with no `param()` overrides (every `param(name,
    /// default)` call resolves to its own `default`). Delegates to [`Self::compile_with_params`]
    /// with an empty override map.
    pub fn compile(src: &str) -> Result<Self, ScriptError> {
        Self::compile_with_params(src, indexmap::IndexMap::new())
    }

    /// Like [`Self::compile`], but injects `overrides` for `param(name, default)` calls BEFORE the
    /// one-time top-level run — so a swept value is baked into the persisted `scope` (and thus
    /// visible for the strategy's whole lifetime), not merely applied per hook call. Fails on a
    /// Rhai parse/compile error and on a removed order function (the module doc); which hooks the
    /// script defines is detected via
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
    /// `engine::user::user_indicator_conflict`, which the loader is expected to have already surfaced to
    /// the author) — so a conflicting file yields a function-not-found at the call site rather than
    /// silently redefining `sma`.
    pub fn compile_with_indicators(
        src: &str,
        overrides: indexmap::IndexMap<String, f64>,
        indicators: &[crate::RhaiIndicator],
    ) -> Result<Self, ScriptError> {
        let ctx = ScriptContext::new();
        ctx.write().unwrap().overrides = overrides;
        let mut engine = build_engine(&ctx);
        crate::engine::user::register_user_indicators(&mut engine, &ctx, indicators);
        let ast = engine.compile(src).map_err(ScriptError::from)?;
        refuse_renamed_modules(&ast)?;
        refuse_removed(&ast)?;
        let defines = |name: &str, params: &[&str]| {
            ast.iter_functions().any(|f| f.name == name && f.params == params)
        };
        refuse_renamed_hooks(&ast)?;
        let has_on_start = defines("on_start", &[]);
        let has_on_bar = defines("on_bar", &[]);
        let has_on_stop = defines("on_stop", &[]);
        let has_on_tick = defines("on_tick", &[]);
        let takes_strategy = defines("on_bar", &["strategy"]);
        if has_on_bar && takes_strategy {
            return Err(ScriptError::plain(
                "a script defines on_bar once: `fn on_bar()`, or `fn on_bar(strategy)` with the \
                 strategy object"
                    .to_string(),
            ));
        }
        let has_after_fill = takes_strategy && defines("after_fill", &["strategy"]);
        let tick_takes_strategy = defines("on_tick", &["strategy"]);
        let stop_takes_strategy = defines("on_stop", &["strategy"]);
        let declares_inputs = defines("inputs", &[]);
        if declares_inputs && !takes_strategy {
            return Err(ScriptError::plain(
                "`fn inputs()` needs `fn on_bar(strategy)`: a script reads its inputs through the \
                 strategy object, strategy.input(name)"
                    .to_string(),
            ));
        }
        let declares_trades = defines("trades", &[]);
        if declares_trades && !takes_strategy {
            return Err(ScriptError::plain(
                "`fn trades()` needs `fn on_bar(strategy)`: a script trades another symbol through \
                 the strategy object, strategy.on(symbol)"
                    .to_string(),
            ));
        }
        if defines("on_start", &["strategy"]) {
            return Err(ScriptError::plain(
                "on_start takes no `strategy`: the strategy object starts at the run's first bar, \
                 which names the symbol it trades and its price. Place the first orders from \
                 on_bar(strategy)"
                    .to_string(),
            ));
        }
        for (hook, plain, takes) in [
            ("on_tick", has_on_tick, tick_takes_strategy),
            ("on_stop", has_on_stop, stop_takes_strategy),
        ] {
            if plain && takes {
                return Err(ScriptError::plain(format!(
                    "a script defines {hook} once: `fn {hook}()` or `fn {hook}(strategy)`"
                )));
            }
            if takes && !takes_strategy {
                return Err(ScriptError::plain(format!(
                    "`fn {hook}(strategy)` needs `fn on_bar(strategy)`: the strategy object is the \
                     order language's, which a script enters through its bar hook"
                )));
            }
        }

        // Run the AST's top-level statements (anything outside a `fn`) EXACTLY ONCE, here at
        // compile time — never again per hook call (see the module doc: this is the fix for the
        // "a top-level order verb resubmits every bar" bug). `run_ast_with_scope` populates
        // `scope` with whatever the top level declared (typically a `const`), which is what
        // keeps a top-level `const` visible from inside a script `fn` despite `run_hook` never
        // re-running the top level again.
        let mut scope = rhai::Scope::new();
        engine.run_ast_with_scope(&mut scope, &ast).map_err(ScriptError::from)?;
        // A script on the order language declares its defaults ONCE (`vike_tactics::Rule::defaults`),
        // read here so a malformed one is a compile error, not a run's surprise.
        let options = || rhai::CallFnOptions::default().eval_ast(false).rewind_scope(false);
        let header = if takes_strategy && defines("defaults", &[]) {
            let map = engine
                .call_fn_with_options::<rhai::Map>(
                    options(),
                    &mut scope.clone(),
                    &ast,
                    "defaults",
                    (),
                )
                .map_err(ScriptError::from)?;
            crate::tactics::defaults_of(&map).map_err(ScriptError::plain)?
        } else {
            RuleDefaults::PLAIN
        };
        // And the inputs it reads (`vike_model::Strategy::inputs`), which the run loads before it starts.
        let inputs = if declares_inputs {
            let names = engine
                .call_fn_with_options::<rhai::Array>(
                    options(),
                    &mut scope.clone(),
                    &ast,
                    "inputs",
                    (),
                )
                .map_err(ScriptError::from)?;
            crate::tactics::inputs_of(names).map_err(ScriptError::plain)?
        } else {
            Vec::new()
        };
        ctx.write().unwrap().reads.inputs =
            inputs.iter().map(|n| (n.clone(), Vec::new())).collect();
        // And the symbols it trades beside its own (`vike_tactics::RuleRun::trades`), each read per hook.
        let trades = if declares_trades {
            let names = engine
                .call_fn_with_options::<rhai::Array>(
                    options(),
                    &mut scope.clone(),
                    &ast,
                    "trades",
                    (),
                )
                .map_err(ScriptError::from)?;
            crate::tactics::trades_of(names).map_err(ScriptError::plain)?
        } else {
            Vec::new()
        };
        ctx.write().unwrap().reads.trading(&trades);
        // Whatever that one-time run recorded belongs to no bar — discard ALL of it so
        // `compile()` itself can never leave behind a bogus warm-up sample:
        // - `indicators`/`fed_this_bar`: a top-level reference to an indicator (e.g. `sma(3);`
        //   outside a hook) would otherwise lazily construct it here and feed it once with the
        //   all-zero bar `ctx` starts from (no real bar has arrived yet at compile time) via
        //   `engine::builtin::indicator_value` — a phantom sample the first REAL `on_bar` call would
        //   silently build on. Clearing both here ensures a script's first real bar is the
        //   first bar any indicator ever sees.
        {
            let mut g = ctx.write().unwrap();
            g.indicators.clear();
            // Same reasoning for the user-written ones, and it bites harder: a `RhaiIndicator`
            // keeps state the AUTHOR wrote, so a phantom zero-bar sample would corrupt their own
            // recurrence (a running sum would carry a spurious 0.0 forever), not merely shift a
            // warm-up by one.
            g.user_indicators.clear();
            g.fed_this_bar.clear();
        }

        let script = Script {
            has_on_start,
            has_on_bar,
            has_on_stop,
            has_on_tick,
            last_tick_at: None,
            engine,
            ast,
            scope,
            ctx,
            errors: 0,
            disabled: false,
            takes_strategy,
            tick_takes_strategy,
            stop_takes_strategy,
            has_after_fill,
            header,
        };
        Ok(RhaiStrategy {
            script: Some(script),
            run: None,
            inputs,
            trades,
            own: None,
            _marker: PhantomData,
        })
    }

    /// The symbol the MOUNT trades, which a script whose `on_bar` takes `strategy` trades as its own
    /// from its first bar on, whichever symbol that bar is: on a mount that declares legs
    /// (`fn trades()`) the first bar may be a leg's. A live mount states it; without it (a backtest)
    /// the run's own symbol is its first bar's.
    #[must_use]
    pub fn mounted_on(mut self, symbol: &str) -> Self {
        self.own = Some(symbol.to_string());
        self
    }

    /// The script, wherever it is held.
    fn script(&mut self) -> &mut Script {
        match (&mut self.run, &mut self.script) {
            (Some(run), _) => run.rule_mut(),
            (None, Some(script)) => script,
            (None, None) => unreachable!("a RhaiStrategy holds its script, or the run holding it"),
        }
    }
}

impl Script {
    /// The global reads a hook sees (`position()`, `price()` and the rest), taken from the broker
    /// before it runs.
    fn snapshot<B: OrderBroker>(&self, broker: &B) {
        let symbol = self.symbol();
        let mut g = self.ctx.write().unwrap();
        // Only ask the broker for a per-symbol read when there IS a symbol. Before the
        // first bar arrives (e.g. inside `on_start`), `symbol()` is `""` — a real
        // `OrderBrokerSimulator`/`LiveBroker` routes `position("")`/`price("")` through its symbol
        // index and PANICS on an unknown symbol (see `symbol()`'s doc), which is not caught
        // by this strategy's own fail-safe (that only catches `EvalAltResult`, not a Rust
        // panic). Leave `position`/`price` at their prior value (the `ScriptContext::new()`
        // default of `0.0` on the very first call) instead: a pre-trade `on_start`
        // legitimately has no position or price yet.
        if !symbol.is_empty() {
            g.position = broker.position(&symbol);
            g.price = broker.price(&symbol);
        }
        g.equity = broker.equity();
        g.index = broker.index() as i64;
        g.now = broker.now();
    }

    /// The bar a hook reads (`close()` and the rest), and a fresh indicator feed for it.
    fn enter_bar(&self, bar: &Bar) {
        let mut g = self.ctx.write().unwrap();
        g.current_bar = bar.clone();
        g.fed_this_bar.clear();
    }

    /// A hook's error: counted, logged, its orders dropped, and the script disabled after 10 in a row.
    fn fail(&mut self, name: &str, e: &rhai::EvalAltResult) {
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
        self.ctx.write().unwrap().calls.clear();
    }

    /// Runs the hook `name(strategy)` of a script on the order language: the reads from `strategy`,
    /// the call, then what it recorded replayed onto `strategy` (`crate::tactics`). Fail-safe as
    /// [`Script::run_hook`]: an error issues nothing.
    fn run_rule(&mut self, name: &str, strategy: &mut Tactics<'_>) {
        if self.disabled {
            return;
        }
        {
            let mut g = self.ctx.write().unwrap();
            crate::tactics::snapshot(&mut g.reads, strategy);
            g.calls.clear();
            g.reads_only = name == "on_stop";
        }
        let mut call_scope = self.scope.clone();
        let options = rhai::CallFnOptions::default().eval_ast(false).rewind_scope(false);
        let handle = crate::tactics::Handle(self.ctx.clone());
        // `Dynamic`, not `()`: a hook may end on a builder chain, whose value is that builder
        let res = self.engine.call_fn_with_options::<rhai::Dynamic>(
            options,
            &mut call_scope,
            &self.ast,
            name,
            (handle,),
        );
        if let Err(e) = res {
            self.fail(name, &e);
            return;
        }
        self.errors = 0;
        let calls = std::mem::take(&mut self.ctx.write().unwrap().calls);
        crate::tactics::replay(strategy, calls);
    }

    /// Snapshot reads -> call the named 0-arg fn. It places no order (the order functions were
    /// removed: the module doc). Fail-safe: a script error is logged and swallowed (no panic);
    /// 10 consecutive errors self-disable the strategy for the rest of its life.
    ///
    /// Calls with `eval_ast(false)` so the AST's top level (already run exactly once in
    /// `compile`) never re-executes here (see the module doc). The scope passed in is a fresh
    /// CLONE of the persisted `self.scope` (which holds whatever the one-time top-level run
    /// declared, e.g. a `const`), not `self.scope` itself: cloning means a hook body's own
    /// `let`-declared locals live only for this one call and are dropped with the clone, never
    /// accumulating in `self.scope` across bars, while the top-level `const` is still visible
    /// because it was already in the scope BEFORE this call's clone was taken. `rewind_scope`
    /// is left at `false` to mirror the reviewed guidance but is moot here either way:
    /// `call_scope` is discarded when this function returns regardless of that flag.
    fn run_hook<B: OrderBroker>(&mut self, name: &str, broker: &mut B) {
        if self.disabled {
            return;
        }
        self.snapshot(broker);
        let mut call_scope = self.scope.clone();
        let options = rhai::CallFnOptions::default().eval_ast(false).rewind_scope(false);
        let res =
            self.engine.call_fn_with_options::<()>(options, &mut call_scope, &self.ast, name, ());
        if let Err(e) = res {
            self.fail(name, &e);
            return;
        }
        self.errors = 0;
    }

    /// single-symbol MVP: resolves the CURRENT bar's symbol tag from `ScriptContext::current_bar`, which
    /// `Strategy::on_bar` below sets fresh every bar — falling back to `""` before the first bar
    /// arrives (e.g. inside `on_start`). Deliberately does NOT call `broker.bars("")`: the mock
    /// broker in this crate's tests ignores the symbol argument and always returns its one bar
    /// series, which made that lookup look safe, but a real `OrderBrokerSimulator`/`LiveBroker` rejects the
    /// unknown symbol `""` and panics. The current bar already carries the mounted symbol's tag
    /// (`Bar::symbol`), so reading it directly needs no broker call at all. The SAME panic risk
    /// applies to `broker.position("")`/`broker.price("")` — `run_hook` guards both behind
    /// `!symbol.is_empty()` for exactly this reason (a real Rust panic, unlike a script error,
    /// is not caught by this strategy's fail-safe).
    fn symbol(&self) -> String {
        self.ctx.read().unwrap().current_bar.symbol.clone().unwrap_or_default()
    }
}

/// The order language's view of a script whose `on_bar` takes `strategy`.
impl Rule for Script {
    fn defaults(&self) -> RuleDefaults {
        self.header
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        self.run_rule("on_bar", strategy);
    }

    /// `_bar` is what `close()` and the rest already read: the run hands `RuleRun::on_fill`'s last
    /// closed bar, the one `RhaiStrategy::on_bar` last entered.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        if self.has_after_fill {
            self.run_rule("after_fill", strategy);
        }
    }
}

impl<B: OrderBroker> Strategy<B> for RhaiStrategy<B> {
    /// The names the script's `fn inputs()` declares (decision 0271); none without one.
    fn inputs(&self) -> Vec<String> {
        self.inputs.clone()
    }

    /// The symbols the script's `fn trades()` declares (`vike_tactics::RuleRun::trades`); none without one.
    fn symbols(&self) -> Vec<String> {
        self.trades.clone()
    }

    fn on_start(&mut self, broker: &mut B) {
        let script = self.script();
        if script.has_on_start {
            script.run_hook("on_start", broker);
        }
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let script = self.script();
        if !script.has_on_bar && !script.takes_strategy {
            return;
        }
        script.enter_bar(bar);
        if script.has_on_bar {
            script.run_hook("on_bar", broker);
            return;
        }
        // `fn on_bar(strategy)`: inside the language's one wrapper from the first bar, trading the
        // mount's symbol, else the one that bar names
        if let Some(script) = self.script.take() {
            let own = self.own.as_deref().or(bar.symbol.as_deref()).unwrap_or_default();
            let run = RuleRun::new(script, own);
            self.run = Some(run.trades(&self.trades));
        }
        let Some(run) = self.run.as_mut() else { return };
        run.rule_mut().snapshot(broker);
        Strategy::<B>::on_bar(run, broker, bar);
    }

    /// **A script works by candles AND by ticks** (decision 0245): a script that defines `fn on_tick()` is run on
    /// the mount's quote lane, sampled at most once per [`TICK_HOOK_MIN_MS`] of event time, with `price()` the
    /// mark the engine prices against at that quote (the board already holds it), and the same reads as
    /// `on_bar()`; like it, it places no order. A one-sided or crossed quote runs nothing. ⚠ Indicators stream once per BAR: call
    /// them from `on_bar`, and read nothing but `price()`, `position()`, `equity()` and `now()` here (an
    /// indicator first called from `on_tick` is fed the last bar `on_bar` saw, or an empty one before any).
    /// A script that does not define the hook returns here on one bool.
    ///
    /// `fn on_tick(strategy)` (beside `fn on_bar(strategy)`) is the same hook in the order language:
    /// sampled the same way, from the run's first bar on (before it a quote runs nothing and spends
    /// no sample), its verbs lowered in a pass of `vike_tactics::RuleRun::pass` at the quote's time.
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        if self.run.is_none() && self.script.as_ref().is_some_and(|s| s.tick_takes_strategy) {
            return;
        }
        let script = self.script();
        if !(script.has_on_tick || script.tick_takes_strategy) || script.disabled {
            return;
        }
        if !(q.bid.is_finite() && q.ask.is_finite() && q.bid > 0.0 && q.ask >= q.bid) {
            return;
        }
        let at = if q.timestamp > 0 { q.timestamp } else { broker.now() };
        if script
            .last_tick_at
            .is_some_and(|last| (0..TICK_HOOK_MIN_MS).contains(&at.saturating_sub(last)))
        {
            return;
        }
        script.last_tick_at = Some(at);
        if !script.tick_takes_strategy {
            if !q.symbol.is_empty() {
                // Before the first bar the script knows no symbol, and an order or a read needs one.
                let mut g = script.ctx.write().unwrap();
                if g.current_bar.symbol.is_none() {
                    g.current_bar.symbol = Some(q.symbol.clone());
                }
            }
            script.run_hook("on_tick", broker);
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        run.rule_mut().snapshot(broker);
        run.pass(broker, at, |script, strategy| script.run_rule("on_tick", strategy));
    }

    /// `fn on_stop(strategy)` READS the language (its verbs are refused, by name: the strategy is
    /// stopping); a run that never met a bar has nothing to read and runs nothing.
    fn on_stop(&mut self, broker: &mut B) {
        let script = self.script();
        if script.has_on_stop {
            script.run_hook("on_stop", broker);
            return;
        }
        if !script.stop_takes_strategy {
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        run.rule_mut().snapshot(broker);
        let at = broker.now();
        run.pass(broker, at, |script, strategy| script.run_rule("on_stop", strategy));
    }

    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        // A zero-argument hook observes fills through `position()`, with no fill callback (SP1). A
        // script whose `on_bar` takes `strategy` hands each to the language's book, and to its
        // `after_fill`, whose `position()` and the rest read after the fill.
        let Some(run) = self.run.as_mut() else { return };
        if run.rule().has_after_fill {
            run.rule_mut().snapshot(broker);
        }
        Strategy::<B>::on_fill(run, broker, fill);
    }

    /// The end of an order the language placed (only a script whose `on_bar` takes `strategy`
    /// places one) takes it off the language's book.
    fn on_order_event(&mut self, broker: &mut B, event: &OrderEvent) {
        if let Some(run) = self.run.as_mut() {
            Strategy::<B>::on_order_event(run, broker, event);
        }
    }
}

/// Compile `src` with no overrides and return the params it requested, in declaration order,
/// as `(name, default)` — for the sweep UI. Broker-agnostic: runs the top level (where `param()`
/// is called) without mounting a `Strategy<B>`. A removed order function and a renamed hook are
/// refused here as [`RhaiStrategy::compile`] refuses them, so the editor's compile dot says so.
pub fn discover_params(src: &str) -> Result<Vec<(String, f64)>, ScriptError> {
    let ctx = ScriptContext::new();
    let engine = build_engine(&ctx);
    let ast = engine.compile(src).map_err(ScriptError::from)?;
    refuse_renamed_modules(&ast)?;
    refuse_removed(&ast)?;
    refuse_renamed_hooks(&ast)?;
    let mut scope = rhai::Scope::new();
    engine.run_ast_with_scope(&mut scope, &ast).map_err(ScriptError::from)?;
    let g = ctx.read().unwrap();
    Ok(g.params_seen.iter().map(|(k, v)| (k.clone(), *v)).collect())
}

#[path = "strategy_tests.rs"]
#[cfg(test)]
mod strategy_tests;
