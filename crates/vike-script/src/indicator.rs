//! `RhaiIndicator`: a USER-WRITTEN indicator, as a real [`vike_indicators::Indicator`].
//!
//! This is the seam the indicator binding (`engine.rs`) deliberately left unbuilt. That work made
//! every registry indicator CALLABLE from a script; it did not let anyone DEFINE one, because a
//! plain Rhai function has nowhere to keep state between bars and no guarantee about how often it
//! is called. An indicator is exactly the thing that needs both.
//!
//! The contract given to the author is therefore state-first. A file under
//! `user_data/indicators/<name>.rhai` defines:
//!
//! ```text
//! fn init() { #{ prev: () } }        // OPTIONAL — the state you keep. Default: an empty map.
//! fn on_bar(bar) { ... }             // REQUIRED — called EXACTLY once per bar, in order.
//! fn warmup() { 20 }                 // OPTIONAL — bars before the value means anything.
//! fn outputs() { ["up", "lo"] }      // OPTIONAL — the output LINES. Default: one, named for it.
//! fn overlay() { true }              // OPTIONAL — draw on the PRICE pane? Default: false.
//! ```
//!
//! Inside `on_bar`, `this` IS that state and mutations to it persist (rhai's `bind_this_ptr`), so
//! an author writes a recurrence the way they would think of one. `bar` is a map of
//! `ts`/`open`/`high`/`low`/`close`/`volume`. Returning `()` means "warming up" and reads as NaN.
//!
//! ## Multi-output: `fn outputs()`, and why it is a function rather than new syntax
//!
//! A built-in declares `IndicatorMeta::outputs` — an ordered `OutSpec` slice — and `engine.rs`
//! generates one host function per line from it ([`crate::line_fn_name`]: `bollinger_mid`). A user
//! file declares the same ordered list from `fn outputs()`, and gets the same accessors under the
//! same rule, INCLUDING the namesake rule that decides whether the bare name binds at all.
//!
//! It is an optional 0-arg function because that is the shape this contract already has: `init` and
//! `warmup` are read through the identical `AST::iter_functions` probe, so an author learns no new
//! concept and this file grows no new parsing. The alternative shape — a `param()`-style host call
//! collecting names in first-seen order — buys nothing here: `param`'s call ORDER has to be
//! discovered by RUNNING because a call site maps arguments onto it positionally, while an output
//! list is written out in one place and read once.
//!
//! ⚠ **Names only, no `OutSpec::style`.** A built-in's style drives the CHART, which reads
//! `IndicatorMeta`; a `RhaiIndicator` has no `IndicatorMeta` and is reachable only from a script, so
//! a declared style would be a knob nothing reads — worse than an unimplemented feature, per this
//! workspace's settings rule. Adding one later is additive (a map element beside the string).
//!
//! ⚠ **A file that declares no `outputs()` is UNCHANGED, to the value.** It has one line named after
//! itself, `on_bar` returns one number, and an ARRAY return is still refused — see [`read_values`],
//! whose refusal now names the declaration that would have made it legal instead of ending the
//! conversation. Every indicator written before this exists is in that state.
//!
//! ## Three properties this shape buys, none of them incidental
//!
//! **[`Indicator::vectorize`] is the fold of `on_bar` over a FRESH instance, so streaming/batch
//! parity holds by CONSTRUCTION.** For the built-ins that equality is a hard-won property gated
//! per indicator in `crates/vike-indicators/tests/parity.rs` — two separately-written code paths
//! that must agree bit-for-bit. Here there is only one path, so there is nothing for a user to get
//! wrong and nothing for this crate to have to check. (It is still gated, in `tests/user.rs` — the
//! claim is that the definition makes it true, and a test is how that claim stays true.)
//!
//! **Streaming is O(1) per bar, by construction.** The author holds their own state, so cost per
//! bar cannot depend on history length. Contrast `hist_indicator!`, which streams ~150 of the
//! built-ins by re-running a batch kernel over retained history: that was quadratic until #1177
//! bounded it, which is precisely why binding the registry waited for that fix. A user indicator
//! has no such trap available to it.
//!
//! **An indicator cannot trade.** [`indicator_engine`] registers NO order verbs, NO ctx reads and
//! NO other indicators — it is `rhai::Engine::new()` plus limits, MINUS the file-backed module
//! resolver that constructor installs by default. So a user indicator is a pure
//! function of the bars it was fed and its own state. That is a safety boundary (a file in an
//! `indicators/` folder submitting an order would be an unpleasant surprise), and it is also what
//! makes the fold above SOUND: a `vectorize` that replays side effects would not be a reference
//! implementation of anything.
//!
//! ## Faults are held, not silently NaN'd
//!
//! [`Indicator::on_bar`] returns `Vec<f64>` and has no error channel, so a script that throws on
//! bar N can only be NaN there. NaN is the warm-up value, and a strategy that reads a permanently
//! warming-up indicator does nothing forever without saying why — the exact failure this crate's
//! `bad_arg` was written to avoid. So the fault is RECORDED ([`RhaiIndicator::fault`]) rather than
//! dropped, and the strategy-side bridge (`crates/vike-script/src/engine.rs`'s
//! `register_user_indicators`) turns it into a raised Rhai error, where `RhaiStrategy`'s
//! consecutive-failure cap can stop the strategy.

use std::sync::Arc;
use vike_indicators::Indicator;
use vike_model::Bar;

/// The per-bar entry point. 1 parameter (the bar map), `this` bound to the state.
const ON_BAR: &str = "on_bar";
/// Optional 0-arg state constructor. Absent -> an empty map.
const INIT: &str = "init";
/// Optional 0-arg warm-up depth, in bars.
const WARMUP: &str = "warmup";
/// Optional 0-arg output-LINE declaration. Absent -> one line, named after the indicator.
const OUTPUTS: &str = "outputs";
/// Optional 0-arg chart placement: `true` to draw on the price pane, `false` (the default) for the
/// indicator's own pane. See [`RhaiIndicator::is_overlay`].
const OVERLAY: &str = "overlay";

/// A user-written Rhai indicator, mounted as a real [`Indicator`].
///
/// `Clone` is part of the [`Indicator`] contract (via `BoxedClone`, so a caller can evaluate a
/// speculative forming bar on a throwaway copy). The engine and AST are shared behind `Arc` — they
/// are immutable after compile — while `state` and `last` are the per-instance streaming state and
/// are genuinely copied, so a clone advances independently of its original.
#[derive(Clone)]
pub struct RhaiIndicator {
    name: String,
    engine: Arc<rhai::Engine>,
    ast: Arc<rhai::AST>,
    /// The scope the AST's top level produced, run ONCE at compile. Cloned per call so a hook's
    /// own locals never accumulate across bars — the same discipline as `RhaiStrategy::run_hook`,
    /// and what makes a top-level `let lookback = 50;` visible from inside `on_bar`.
    scope: rhai::Scope<'static>,
    /// `this` inside `on_bar`. Mutations persist across bars; that is the whole point.
    state: rhai::Dynamic,
    /// The value `init()` produced, kept for [`Indicator::reset`]. Held rather than re-run: reset
    /// must return the indicator to CONSTRUCTION, and re-running a user's `init` would diverge
    /// from construction for any `init` that is not pure.
    initial: rhai::Dynamic,
    last: Vec<f64>,
    warmup: usize,
    /// The lines `fn outputs()` declared, in order — `None` when the file declares none.
    ///
    /// ⚠ `None` is NOT the same as `Some(vec![name])`, and the difference is the whole
    /// compatibility guarantee: [`RhaiIndicator::outputs`] reports one line either way, but
    /// [`read_values`] uses `None` to keep the pre-existing single-output contract EXACTLY —
    /// including refusing an array return of ANY length, which a `Some` of length one would start
    /// accepting as a second, undeclared spelling of the same number.
    declared_outputs: Option<Vec<String>>,
    /// The file's own `overlay()`, or `false` when it declares none — see
    /// [`RhaiIndicator::is_overlay`]. Chart placement only; nothing in the fold reads it.
    overlay: bool,
    fault: Option<String>,
    /// The `param(name, default)` calls this file makes, in FIRST-SEEN order — the equivalent of a
    /// built-in's `IndicatorMeta::params`, and what [`RhaiIndicator::compile_with`] maps a call
    /// site's positional arguments onto.
    params: Vec<(String, f64)>,
    /// The file's source, kept ONLY so [`RhaiIndicator::compile_with`] can re-run its top level
    /// with a call site's arguments bound. An `Arc<str>` because every instance of one indicator
    /// shares it and a clone is on the bar path.
    src: Arc<str>,
}

impl std::fmt::Debug for RhaiIndicator {
    /// Hand-written because `rhai::Engine` is not `Debug`. Prints the identity and the streaming
    /// position, not the user's state map.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhaiIndicator")
            .field("name", &self.name)
            .field("warmup", &self.warmup)
            .field("outputs", &self.outputs())
            .field("last", &self.last)
            .field("fault", &self.fault)
            .finish()
    }
}

/// The engine a user indicator runs in: computation only.
///
/// Deliberately NOT `engine.rs`'s `build_engine`. That one registers the order verbs, the ctx
/// reads and the whole bound indicator set, all of which are wrong here — see the module doc's
/// "an indicator cannot trade". The limits mirror `build_engine`'s so that one runaway script is
/// bounded the same way whichever seam it was loaded through.
///
/// ⚠ So does the module resolver, and for the same reason the strategy side carries it: a default
/// `rhai::Engine` resolves `import "…" as m;` off the FILESYSTEM, so without this line a user
/// indicator — which is loaded from `user_data/` and is therefore the LEAST reviewed script this
/// crate compiles — would reach a file verb nobody granted it. See
/// `crates/vike-script/src/engine.rs`'s `build_engine` for the full argument; the gate covering
/// both seams is `crates/vike-script/tests/module_import_refused.rs`.
fn indicator_engine(knobs: &Knobs) -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    engine.set_max_operations(2_000_000);
    engine.set_max_call_levels(64);
    engine.set_max_string_size(8 * 1024);
    engine.set_max_array_size(4096);
    engine.set_max_map_size(4096);
    // ...and so does the print/debug redirect, for the same "a default engine chose a stream its
    // embedder owns" reason: `crates/vike-script/src/engine.rs`'s `redirect_script_output` is the
    // argument, and this is the SECOND of the two engines it has to reach.
    crate::engine::redirect_script_output(&mut engine, "indicator");
    // `param(name, default)` — the ONLY host function an indicator engine registers, and the same
    // spelling a STRATEGY already uses (`crates/vike-script/src/engine.rs`'s `build_engine`). That
    // is the whole design: an author who has written a strategy already knows this, so a
    // parameterised indicator needs no new syntax and no declaration block.
    //
    // It records every (name, default) in first-seen order — that ordered list IS the indicator's
    // parameter surface, the equivalent of a built-in's `IndicatorMeta::params` — and returns the
    // injected override when the caller supplied one.
    let k = knobs.clone();
    // ⚠ `Dynamic`, not `f64`, for the same reason the strategy side takes one: registered as `f64`
    // this accepted `param("lookback", 20.0)` and rejected `param("lookback", 20)`, which is the
    // spelling anyone writes for a bar count.
    engine.register_fn(
        "param",
        move |name: &str, default: rhai::Dynamic| -> Result<f64, Box<rhai::EvalAltResult>> {
            let d = default.as_float().or_else(|_| default.as_int().map(|i| i as f64)).map_err(
                |_| -> Box<rhai::EvalAltResult> {
                    format!(
                        "param(\"{name}\", ...): the default must be a number, got {}",
                        default.type_name()
                    )
                    .into()
                },
            )?;
            let mut g = k.write().unwrap();
            g.seen.entry(name.to_string()).or_insert(d);
            Ok(g.overrides.get(name).copied().unwrap_or(d))
        },
    );
    engine
}

/// The `param()` recording cell shared between one engine and the `compile` that built it.
///
/// `seen` is the declared surface (first-seen wins, matching the strategy side); `overrides` is what
/// a call site supplied. Both live behind one lock because the host closure needs `'static` capture.
#[derive(Default)]
pub(crate) struct KnobCell {
    seen: indexmap::IndexMap<String, f64>,
    overrides: indexmap::IndexMap<String, f64>,
}

type Knobs = Arc<std::sync::RwLock<KnobCell>>;

/// The bar handed to `on_bar`, as a Rhai map.
///
/// Six fields, all always present. `Bar`'s three `Option` fields (`bid`/`ask`/`funding`) and its
/// `symbol` are deliberately omitted: an indicator is a function of the price series, and a field
/// that is `()` on most series is a trap dressed as a feature. Adding one later is additive.
fn bar_map(bar: &Bar) -> rhai::Map {
    let mut m = rhai::Map::new();
    m.insert("ts".into(), rhai::Dynamic::from_int(bar.ts));
    m.insert("open".into(), rhai::Dynamic::from_float(bar.open));
    m.insert("high".into(), rhai::Dynamic::from_float(bar.high));
    m.insert("low".into(), rhai::Dynamic::from_float(bar.low));
    m.insert("close".into(), rhai::Dynamic::from_float(bar.close));
    m.insert("volume".into(), rhai::Dynamic::from_float(bar.volume));
    m
}

/// Reads ONE number out of a Rhai value: `()` is warm-up -> NaN (what every built-in returns before
/// it is warm), and ints coerce so a counting indicator can `return this.n` without a float cast.
///
/// Used for `on_bar`'s whole return on a single-output indicator AND for each element of the array
/// a multi-output one returns, so a line is warm-up-able on its own (`[upper, (), lower]`) exactly
/// as the whole indicator is.
fn read_scalar(v: &rhai::Dynamic) -> Option<f64> {
    if v.is_unit() {
        return Some(f64::NAN);
    }
    v.as_float().ok().or_else(|| v.as_int().ok().map(|i| i as f64))
}

/// Reads `on_bar`'s return as this bar's value(s).
///
/// `declared` is `None` for a file with no `fn outputs()`, and that arm is the pre-existing
/// single-output contract PRESERVED RATHER THAN REIMPLEMENTED: one number out, `()` for warm-up,
/// and an ARRAY refused whatever its length. The refusal is why this seam existed — `engine.rs`'s
/// `exclusion` refuses a bare multi-output call precisely because the bridge would hand back line 0,
/// the UPPER band on every band indicator, to an author who read it as the middle — but it is now a
/// SIGNPOST: the array becomes legal the moment the file says what its lines are, which is exactly
/// what makes them separately reachable.
///
/// ⚠ A one-element array is refused on that arm too, deliberately. Accepting it would be a second,
/// undeclared spelling of a plain number, and "a file that declares no `outputs()` behaves exactly
/// as it did" is a claim about EVERY input, not about the three-element one somebody happened to
/// test.
///
/// With `declared`, the length must MATCH: padding would invent a number for a line the author
/// never computed, and truncating would hide one they did — both silent, both on the value path a
/// strategy trades from. So it is an error naming both counts.
fn read_values(
    name: &str,
    declared: Option<&[String]>,
    v: &rhai::Dynamic,
) -> Result<Vec<f64>, String> {
    let Some(lines) = declared else {
        if let Some(x) = read_scalar(v) {
            return Ok(vec![x]);
        }
        if v.is_array() {
            return Err(format!(
                "{name}: `on_bar` returned an array. A user indicator is single-output until it \
                 says otherwise — this file declares no `fn outputs()`, so the bridge reads one \
                 scalar and would silently hand back the first (on a band indicator that is the \
                 UPPER band, not the middle). Declare the lines — `fn outputs() {{ [\"upper\", \
                 \"mid\", \"lower\"] }}` — and each gets its own accessor (`{name}_mid()`), or \
                 return one number."
            ));
        }
        return Err(format!(
            "{name}: `on_bar` must return a number, or () while warming up — got {}",
            v.type_name()
        ));
    };

    // `()` is warm-up on EVERY line at once — the spelling a band indicator uses before its window
    // is full, and the reason a multi-output author never has to write `[(), (), ()]`.
    if v.is_unit() {
        return Ok(vec![f64::NAN; lines.len()]);
    }
    let got = |n: usize| {
        format!(
            "{name}: `fn outputs()` declares {} line(s) ({}) but `on_bar` returned {n} value(s). \
             Padding would invent a number and truncating would hide one, so it is an error \
             instead — return one value per declared line, in that order, or () while warming up \
             (which reads as NaN on every line).",
            lines.len(),
            lines.join(", ")
        )
    };
    if let Some(x) = read_scalar(v) {
        if lines.len() == 1 {
            return Ok(vec![x]);
        }
        return Err(got(1));
    }
    let Ok(arr) = v.clone().into_array() else {
        return Err(format!(
            "{name}: `on_bar` must return an array of {} number(s) — one per declared output line \
             — or () while warming up. Got {}.",
            lines.len(),
            v.type_name()
        ));
    };
    if arr.len() != lines.len() {
        return Err(got(arr.len()));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, e) in arr.iter().enumerate() {
        let Some(x) = read_scalar(e) else {
            return Err(format!(
                "{name}: `on_bar` returned {} on output line {i} (`{}`) — every line is a number, \
                 or () while that one line is warming up.",
                e.type_name(),
                lines[i]
            ));
        };
        out.push(x);
    }
    Ok(out)
}

/// Reads `fn outputs()`'s return as the ordered output-line list.
///
/// Every rejection here is a COMPILE error rather than a per-bar fault, which is what makes it
/// reportable against the file (`load.rs`'s `CompileFailed`) instead of surfacing as a
/// function-not-found at some strategy's call site.
///
/// The name rules are [`crate::user_line_conflict`]'s, and they are applied to EVERY declared line
/// even when there is only one and no accessor is generated from it: a rule that starts biting when
/// an author adds a second line would be refusing a file for a reason that was true all along.
fn read_outputs(name: &str, v: &rhai::Dynamic) -> Result<Vec<String>, String> {
    let Ok(arr) = v.clone().into_array() else {
        return Err(format!(
            "`{OUTPUTS}()` must return an array of line names, e.g. `[\"upper\", \"mid\", \
             \"lower\"]` — got {}",
            v.type_name()
        ));
    };
    if arr.is_empty() {
        return Err(format!(
            "`{OUTPUTS}()` returned an empty array. An indicator has at least one line; DELETE the \
             function to keep the single-output default"
        ));
    }
    let mut lines: Vec<String> = Vec::with_capacity(arr.len());
    for (i, e) in arr.into_iter().enumerate() {
        let Ok(s) = e.clone().into_string() else {
            return Err(format!(
                "`{OUTPUTS}()` element {i} is {}, not a line NAME — every entry is a string",
                e.type_name()
            ));
        };
        let s = s.trim().to_string();
        if s.is_empty() {
            return Err(format!("`{OUTPUTS}()` element {i} is empty — every line needs a name"));
        }
        if let Some(why) = crate::user_line_conflict(name, &s) {
            return Err(format!("`{OUTPUTS}()` element {i} (`{s}`): {why}"));
        }
        lines.push(s);
    }
    // ⚠ Two names that SANITISE alike (`%K` and `K`) would generate one accessor with two
    // meanings — silently, since the second registration replaces the first. That is the same
    // hazard `engine.rs`'s `generated_line_names_are_unambiguous` gates for the registry, asked of
    // one user file, where the author can actually fix it.
    for (i, a) in lines.iter().enumerate() {
        for b in &lines[i + 1..] {
            let spelling = crate::line_fn_name(name, a);
            if spelling == crate::line_fn_name(name, b) {
                return Err(format!(
                    "`{OUTPUTS}()` lines `{a}` and `{b}` both spell `{spelling}` — one accessor \
                     cannot mean two lines. Rename one."
                ));
            }
        }
    }
    Ok(lines)
}

/// Compiles a user indicator from source.
///
/// Runs the AST's top level exactly ONCE (so a top-level `let lookback = 50;` is a real knob) and
/// calls `init()`/`warmup()` if present. Fails when `on_bar` is missing or has the wrong arity —
/// which is a compile-time answer to what would otherwise be a per-bar function-not-found on every
/// single bar.
pub fn compile_indicator(name: &str, src: &str) -> Result<RhaiIndicator, crate::ScriptError> {
    compile_indicator_with(name, src, &[])
}

/// [`compile_indicator`] with a call site's positional arguments bound to the file's own
/// `param(name, default)` declarations, in first-seen order.
///
/// `my_mean(50)` sets the FIRST declared knob to 50 and leaves the rest at their defaults — exactly
/// how a built-in's short forms work (`vike_indicators::coerce`'s contract). An argument past the
/// declared count is an error rather than being discarded: a knob that looks accepted and does
/// nothing is the failure this whole seam keeps refusing.
pub fn compile_indicator_with(
    name: &str,
    src: &str,
    args: &[f64],
) -> Result<RhaiIndicator, crate::ScriptError> {
    let err = |e: String| crate::ScriptError(format!("{name}: {e}"));
    let knobs: Knobs = Arc::new(std::sync::RwLock::new(KnobCell::default()));
    let engine = indicator_engine(&knobs);
    let ast = engine.compile(src).map_err(|e| err(e.to_string()))?;

    // ⚠ TWO passes, and the first is why. A call site supplies arguments POSITIONALLY, but the
    // positions are defined by the order the file's `param()` calls run — which is only knowable by
    // running the top level. So pass 1 discovers the surface with NO overrides, pass 2 re-runs it
    // with the arguments mapped onto the names pass 1 found. Running once and hoping the caller
    // knew the names would make `my_mean(50)` unwritable.
    if !args.is_empty() {
        let mut probe = rhai::Scope::new();
        engine.run_ast_with_scope(&mut probe, &ast).map_err(|e| err(e.to_string()))?;
        let declared: Vec<String> = knobs.read().unwrap().seen.keys().cloned().collect();
        if args.len() > declared.len() {
            return Err(err(format!(
                "called with {} argument(s) but the file declares {} `param(...)` knob(s){}. An                  argument with nowhere to go would be silently discarded, so it is refused instead",
                args.len(),
                declared.len(),
                if declared.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", declared.join(", "))
                }
            )));
        }
        let mut g = knobs.write().unwrap();
        for (i, v) in args.iter().enumerate() {
            g.overrides.insert(declared[i].clone(), *v);
        }
        // The discovery pass populated `seen` and may have left state behind in a `this`-less
        // scope; the real run below starts from a fresh scope, so only `overrides` carries over.
        g.seen.clear();
    }

    let takes =
        |f: &str, n: usize| ast.iter_functions().any(|d| d.name == f && d.params.len() == n);
    if !takes(ON_BAR, 1) {
        return Err(err(format!(
            "no `fn {ON_BAR}(bar)`. A user indicator is defined by that one function: it is called \
             exactly once per bar with the bar as a map, and `this` is your state."
        )));
    }

    // The top level runs once, here — never per bar. Same discipline as `RhaiStrategy::compile`.
    let mut scope = rhai::Scope::new();
    engine.run_ast_with_scope(&mut scope, &ast).map_err(|e| err(e.to_string()))?;

    let initial: rhai::Dynamic = if takes(INIT, 0) {
        let mut s = scope.clone();
        engine.call_fn(&mut s, &ast, INIT, ()).map_err(|e| err(e.to_string()))?
    } else {
        rhai::Map::new().into()
    };

    let warmup = if takes(WARMUP, 0) {
        let mut s = scope.clone();
        let w: rhai::Dynamic =
            engine.call_fn(&mut s, &ast, WARMUP, ()).map_err(|e| err(e.to_string()))?;
        // ⚠ Read as `Dynamic`, not as `i64`. `param()` returns an f64, so the natural spelling of a
        // parameterised warm-up — `fn warmup() { lookback - 1 }` — produces a FLOAT, and an `i64`
        // read rejected it with "Output type incorrect: f64 (expecting i64)". That is a rhai type
        // error surfacing at the author as a compile failure of their whole indicator, for writing
        // the only thing that could have worked.
        let w = w.as_int().map(|i| i as f64).or_else(|_| w.as_float()).map_err(|_| {
            err(format!("`{WARMUP}()` must return a number, got {}", w.type_name()))
        })?;
        // A negative or non-finite warm-up is meaningless; `lookback` is a usize and the trait reads
        // it as a conservative LOWER bound, so clamping to 0 is the honest reading of "no warm-up".
        if w.is_finite() {
            w.max(0.0).round() as usize
        } else {
            0
        }
    } else {
        0
    };

    // The output LINES. Read AFTER the top level has run, so `fn outputs()` can name lines the file
    // computed there — and read through the same `takes(.., 0)` probe as `init`/`warmup`, which is
    // what keeps this an optional member of that family rather than new syntax.
    let declared_outputs: Option<Vec<String>> = if takes(OUTPUTS, 0) {
        let mut s = scope.clone();
        let v: rhai::Dynamic =
            engine.call_fn(&mut s, &ast, OUTPUTS, ()).map_err(|e| err(e.to_string()))?;
        Some(read_outputs(name, &v).map_err(err)?)
    } else {
        None
    };

    // ⚠ Default FALSE — its own pane, not the price pane. A user indicator returns one unlabelled
    // number and this crate cannot know its scale; a z-score or a bar count plotted on the price
    // axis silently wrecks the price pane's autofit, taking the CANDLES with it. Its own pane is
    // wrong-looking at worst, so the safe answer is the default and the author opts out.
    let overlay = if takes(OVERLAY, 0) {
        let mut s = scope.clone();
        let v: rhai::Dynamic =
            engine.call_fn(&mut s, &ast, OVERLAY, ()).map_err(|e| err(e.to_string()))?;
        v.as_bool()
            .map_err(|_| err(format!("`{OVERLAY}()` must return a bool, got {}", v.type_name())))?
    } else {
        false
    };

    let declared_params: Vec<(String, f64)> =
        knobs.read().unwrap().seen.iter().map(|(k, v)| (k.clone(), *v)).collect();

    let lines = declared_outputs.as_ref().map_or(1, Vec::len);
    Ok(RhaiIndicator {
        name: name.to_string(),
        engine: Arc::new(engine),
        ast: Arc::new(ast),
        scope,
        state: initial.clone(),
        initial,
        last: vec![f64::NAN; lines],
        warmup,
        declared_outputs,
        overlay,
        fault: None,
        params: declared_params,
        src: Arc::from(src),
    })
}

/// One `param(name, default)` declaration as a chart-facing [`ParamSpec`].
///
/// ⚠ A script declares a NAME and a DEFAULT and nothing else — the contract has no range — so
/// `min`/`max` are opened as wide as `f64` goes rather than guessed. That leaves
/// `vike_indicators::coerce` with only the part worth keeping — a NaN or a missing entry falls
/// back to the default — while its clamp reaches nothing a user could mean (only ±∞ moves, onto
/// ±`f64::MAX`). It is the honest answer: clamping to a range nobody declared would silently plot
/// a different number than the author asked for.
///
/// `step` IS inferred, because it is the one field with a visible consequence — the settings
/// dialog derives its decimal places from it (`step >= 1.0` renders 0 dp, so a fractional knob
/// under a step of 1 would be uneditable). A whole-number default is a bar count and steps by 1;
/// a fractional one is a multiplier and steps by 0.001.
fn param_spec(name: &str, default: f64) -> vike_indicators::ParamSpec {
    vike_indicators::ParamSpec {
        name: Box::leak(name.to_string().into_boxed_str()),
        default,
        min: f64::MIN,
        max: f64::MAX,
        step: if default.fract() == 0.0 { 1.0 } else { 0.001 },
    }
}

/// A compiled user indicator as a chart-mountable `&'static IndicatorMeta`.
///
/// This is the whole seam between "a user wrote a file" and "the chart can plot it". The chart
/// needs a DESCRIPTOR (name, placement, knobs) plus a CONSTRUCTOR, and the constructor is the hard
/// half: it must clone THIS compiled prototype — engine, AST and initial state — which is captured
/// state a bare `fn` pointer cannot reach. `IndicatorMeta::user`'s `factory` parameter is the slot
/// that exists for exactly that, and this is the only thing in the tree that fills it.
///
/// ⚠ **LEAKS, so call it at startup, once per indicator** — `IndicatorMeta::user`'s doc is the
/// authority on why that is the right lifetime rather than a shortcut.
///
/// The `pretty` label is the name verbatim: it is the file stem, it is what a strategy calls the
/// indicator, and inventing a title-cased variant would make the picker disagree with the file.
pub fn user_meta(proto: &RhaiIndicator) -> &'static vike_indicators::IndicatorMeta {
    let params: Vec<vike_indicators::ParamSpec> =
        proto.params().iter().map(|(n, d)| param_spec(n, *d)).collect();
    let kind = if proto.is_overlay() {
        vike_indicators::RenderKind::Overlay
    } else {
        vike_indicators::RenderKind::Oscillator
    };
    let name = Indicator::name(proto).to_string();
    let proto = proto.clone();
    let factory: vike_indicators::UserFactory =
        Box::leak(Box::new(move |raw: &[f64]| -> Box<dyn Indicator> {
            match proto.compile_with(raw) {
                Ok(ind) => Box::new(ind),
                // ⚠ `compile_with` re-RUNS the file's top level with the call site's arguments
                // bound, so a script whose top level errors only at certain values — `let k =
                // param("k", 3); let x = 1 / (k.to_int() - 5);` — fails HERE and nowhere earlier.
                // (`k.to_int()` is not decoration: `param` hands back an f64, and rhai's FLOAT
                // `/` is plain IEEE division, so the same line without it yields `inf` and never
                // reaches this arm. `tests::a_param_value_that_breaks_the_top_level_...` pins
                // both halves.) There is
                // no error channel on a constructor, and the two silent options are both bad: an
                // all-NaN series is indistinguishable from warming up (the trap this module's
                // `fault` exists to avoid), and fabricating a value is worse. So it is LOGGED at
                // error and falls back to the prototype's declared defaults — real values of the
                // author's own script, one line in the log saying which knobs did not take.
                // Same fail-safe shape as `RhaiStrategy::run_hook`.
                Err(e) => {
                    tracing::error!(
                        indicator = %Indicator::name(&proto),
                        params = ?raw,
                        error = %e,
                        "user indicator could not be built at these parameters; plotting it at its \
                         declared defaults instead"
                    );
                    Box::new(proto.clone())
                }
            }
        }));
    vike_indicators::IndicatorMeta::user(&name, &name, kind, params, factory)
}

impl RhaiIndicator {
    /// The most recent script fault, if any — see the module doc on why this is held rather than
    /// collapsed into the NaN that `on_bar` had to return.
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    /// A NEW instance of this indicator with `args` bound to its `param()` declarations.
    ///
    /// Re-runs the file's top level, so a knob genuinely changes the recurrence rather than being
    /// recorded and ignored. The source is kept for exactly this — a call site's arguments are not
    /// knowable at load time, and `my_mean(20)` beside `my_mean(50)` needs two real instances.
    pub fn compile_with(&self, args: &[f64]) -> Result<RhaiIndicator, crate::ScriptError> {
        if args.is_empty() {
            return Ok(self.clone());
        }
        compile_indicator_with(&self.name, &self.src, args)
    }

    /// This file's `param(name, default)` declarations, in first-seen order.
    ///
    /// The equivalent of a built-in's `IndicatorMeta::params`, and what decides how many argument
    /// forms `register_user_indicators` binds. Empty for a file that declares none — which is every
    /// indicator written before this existed, so those keep exactly their zero-argument shape.
    pub fn params(&self) -> &[(String, f64)] {
        &self.params
    }

    /// This indicator's output LINES, in order — the equivalent of a built-in's
    /// `IndicatorMeta::outputs`, and what `engine.rs`'s `register_user_indicators` generates the
    /// per-line accessors from through the shared [`crate::line_fn_name`].
    ///
    /// NEVER empty. A file with no `fn outputs()` reports ONE line named after the indicator, which
    /// is the truth about it and always was — the bare name is that line, and `line_accessors`'
    /// `< 2` rule then generates no accessor for it, exactly as it does for a single-output
    /// built-in.
    pub fn outputs(&self) -> &[String] {
        // `from_ref` rather than a stored `vec![name]` so the two states stay distinguishable
        // internally — see `declared_outputs`, where that distinction IS the compatibility rule.
        self.declared_outputs.as_deref().unwrap_or(std::slice::from_ref(&self.name))
    }

    /// Where this indicator wants to be DRAWN: `true` = on the price pane, `false` = its own.
    ///
    /// The file's optional `fn overlay()`, defaulting to `false`. Chart placement only — it is
    /// read by [`user_meta`] to pick a `RenderKind` and by nothing on the compute path, so a
    /// STRATEGY calling this indicator is byte-identical either way.
    pub fn is_overlay(&self) -> bool {
        self.overlay
    }
}

impl Indicator for RhaiIndicator {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        // `bind_this_ptr` needs `&mut Dynamic` while `self.engine`/`self.ast` are borrowed, so the
        // state is moved out for the call and put back after. UNIT is a placeholder that is never
        // observable: nothing can read `self.state` between these two lines.
        let mut this = std::mem::replace(&mut self.state, rhai::Dynamic::UNIT);
        let mut call_scope = self.scope.clone();
        let options =
            rhai::CallFnOptions::new().eval_ast(false).rewind_scope(true).bind_this_ptr(&mut this);
        let res = self.engine.call_fn_with_options::<rhai::Dynamic>(
            options,
            &mut call_scope,
            &self.ast,
            ON_BAR,
            (bar_map(bar),),
        );
        self.state = this;

        let read = res
            .map_err(|e| e.to_string())
            .and_then(|v| read_values(&self.name, self.declared_outputs.as_deref(), &v));
        let lines = self.outputs().len();
        let values = match read {
            Ok(v) => {
                self.fault = None;
                v
            }
            Err(e) => {
                self.fault = Some(e);
                // A fault is NaN on every line, not on line 0 alone: `value()` is indexed by the
                // per-line accessors, and a short vector there would read as NaN anyway while
                // making `value().len()` disagree with `outputs().len()` for one bar.
                vec![f64::NAN; lines]
            }
        };
        self.last = values;
        self.last.clone()
    }

    /// The fold of [`Indicator::on_bar`] over a fresh instance — see the module doc. Streaming and
    /// batch are therefore the same code, not two implementations that have to be proven equal.
    ///
    /// LINE-MAJOR, like every built-in's: the outer `Vec` is one entry per output line, each as long
    /// as `bars`. That was already the shape (a single-output indicator returned `vec![series]`);
    /// what is new is that there can be more than one row.
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        let mut c = self.clone();
        c.reset();
        let mut lines: Vec<Vec<f64>> = vec![Vec::with_capacity(bars.len()); self.outputs().len()];
        for b in bars {
            let out = c.on_bar(b);
            for (i, line) in lines.iter_mut().enumerate() {
                line.push(out.get(i).copied().unwrap_or(f64::NAN));
            }
        }
        lines
    }

    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }

    fn reset(&mut self) {
        let lines = self.outputs().len();
        self.state = self.initial.clone();
        self.last = vec![f64::NAN; lines];
        self.fault = None;
    }

    fn name(&self) -> &str {
        &self.name
    }

    /// The script's own `warmup()`, or the trait's conservative `0` when it declares none.
    fn lookback(&self) -> usize {
        self.warmup
    }

    /// Deliberately left at the trait default (`false`) even when the script DOES declare
    /// `warmup()`. `lookback_exact` promises the value is the exact first-non-NaN index; a user's
    /// `warmup()` is their estimate of their own recurrence, which this crate cannot verify, and
    /// over-claiming exactness would let a caller size seed history too short.
    fn lookback_exact(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_indicators::BoxedClone;

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar {
                ts: i as i64 * 60_000,
                open: c,
                high: c + 1.0,
                low: c - 1.0,
                close: c,
                volume: 100.0 + i as f64,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            })
            .collect()
    }

    const RUNNING_MEAN: &str = r#"
        let lookback = 3;
        fn init() { #{ buf: [] } }
        fn warmup() { 2 }
        fn on_bar(bar) {
            this.buf.push(bar.close);
            if this.buf.len() > lookback { this.buf.remove(0); }
            if this.buf.len() < lookback { return (); }
            let s = 0.0;
            for v in this.buf { s += v; }
            s / lookback
        }
    "#;

    #[test]
    fn state_persists_across_bars_and_the_recurrence_is_the_authors_own() {
        let mut ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
        let out: Vec<f64> = bars(&[1.0, 2.0, 3.0, 4.0]).iter().map(|b| ind.on_bar(b)[0]).collect();
        assert!(out[0].is_nan() && out[1].is_nan(), "warm-up reads as NaN: {out:?}");
        assert_eq!(out[2], 2.0);
        assert_eq!(out[3], 3.0);
    }

    /// The property the whole design rests on. It is true BY CONSTRUCTION (`vectorize` folds
    /// `on_bar`), and this is what keeps that construction from being quietly replaced by a second
    /// implementation later — which is exactly how the built-ins' parity gate earns its keep.
    #[test]
    fn vectorize_is_bit_identical_to_the_streaming_fold() {
        let ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
        let bs = bars(&[1.0, 2.5, 3.25, 4.125, 9.0, 11.5, 2.0]);
        let batch = &ind.vectorize(&bs)[0];
        let mut s = ind.clone();
        for (i, b) in bs.iter().enumerate() {
            let v = s.on_bar(b)[0];
            assert_eq!(v.to_bits(), batch[i].to_bits(), "bar {i}: {v} != {}", batch[i]);
        }
    }

    /// `vectorize` takes `&self` and must not advance the receiver — a caller drawing a chart from
    /// a live indicator would otherwise double-feed it.
    #[test]
    fn vectorize_does_not_advance_the_instance_it_was_called_on() {
        let mut ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
        let bs = bars(&[1.0, 2.0, 3.0, 4.0]);
        for b in &bs[..3] {
            ind.on_bar(b);
        }
        let before = ind.value();
        let _ = ind.vectorize(&bs);
        assert_eq!(ind.value()[0].to_bits(), before[0].to_bits());
        // ...and the NEXT streamed bar continues from bar 3, not from a replayed history.
        assert_eq!(ind.on_bar(&bs[3])[0], 3.0);
    }

    #[test]
    fn reset_returns_the_state_to_construction() {
        let mut ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
        let bs = bars(&[1.0, 2.0, 3.0, 4.0]);
        for b in &bs {
            ind.on_bar(b);
        }
        ind.reset();
        assert!(ind.value()[0].is_nan());
        let after: Vec<f64> = bs.iter().map(|b| ind.on_bar(b)[0]).collect();
        assert!(after[0].is_nan() && after[1].is_nan());
        assert_eq!(after[2], 2.0, "a reset instance must warm up again from scratch");
    }

    /// A clone must stream independently — `BoxedClone` exists so a caller can evaluate the live
    /// FORMING bar on a throwaway copy, and that is only safe if the copy is truly detached.
    #[test]
    fn a_clone_advances_without_touching_its_original() {
        let mut ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
        let bs = bars(&[1.0, 2.0, 3.0, 4.0, 100.0]);
        for b in &bs[..3] {
            ind.on_bar(b);
        }
        let mut speculative = ind.clone_box();
        speculative.on_bar(&bs[4]);
        assert_eq!(ind.value()[0], 2.0, "the original must not have advanced");
        assert_eq!(ind.on_bar(&bs[3])[0], 3.0, "and it continues from where it was");
    }

    #[test]
    fn a_top_level_let_is_visible_inside_on_bar_and_runs_once() {
        // `runs` counts top-level executions. If the top level re-ran per bar the counter would
        // reset and the recurrence below could never reach 3.
        let src = r#"
            let scale = 10.0;
            fn init() { #{ n: 0 } }
            fn on_bar(bar) { this.n += 1; this.n * scale }
        "#;
        let mut ind = compile_indicator("scaled", src).unwrap();
        let out: Vec<f64> = bars(&[1.0, 2.0, 3.0]).iter().map(|b| ind.on_bar(b)[0]).collect();
        assert_eq!(out, vec![10.0, 20.0, 30.0]);
    }

    #[test]
    fn every_bar_field_reaches_the_script() {
        let src = "fn on_bar(bar) { bar.ts + bar.open + bar.high + bar.low + bar.close + \
                   bar.volume }";
        let mut ind = compile_indicator("sum", src).unwrap();
        let b = &bars(&[5.0])[0];
        // ts 0 + open 5 + high 6 + low 4 + close 5 + volume 100
        assert_eq!(ind.on_bar(b)[0], 120.0);
    }

    #[test]
    fn init_is_optional_and_defaults_to_an_empty_map() {
        let src = "fn on_bar(bar) { this.n = if this.n == () { 1 } else { this.n + 1 }; this.n }";
        let mut ind = compile_indicator("count", src).unwrap();
        let out: Vec<f64> = bars(&[1.0, 2.0, 3.0]).iter().map(|b| ind.on_bar(b)[0]).collect();
        assert_eq!(out, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn warmup_is_optional_and_absent_means_the_conservative_zero() {
        let plain = compile_indicator("p", "fn on_bar(bar) { bar.close }").unwrap();
        assert_eq!(plain.lookback(), 0);
        assert_eq!(plain.lookback_full(), 0);
        let declared = compile_indicator("d", RUNNING_MEAN).unwrap();
        assert_eq!(declared.lookback(), 2);
        assert_eq!(declared.lookback_full(), 2, "the trait default forwards to lookback");
    }

    /// Over-claiming here would let a caller size seed history too short — see the method's doc.
    #[test]
    fn a_declared_warmup_is_never_reported_as_exact() {
        assert!(!compile_indicator("d", RUNNING_MEAN).unwrap().lookback_exact());
    }

    #[test]
    fn a_negative_warmup_clamps_to_zero_rather_than_wrapping() {
        let src = "fn warmup() { -5 } fn on_bar(bar) { bar.close }";
        assert_eq!(compile_indicator("neg", src).unwrap().lookback(), 0);
    }

    #[test]
    fn a_missing_on_bar_is_a_compile_error_naming_the_function() {
        let e = compile_indicator("bad", "fn init() { #{} }").unwrap_err().to_string();
        assert!(e.contains("on_bar"), "{e}");
        assert!(e.contains("bad"), "the error names the indicator: {e}");
    }

    /// A `fn on_bar()` with no parameter is the natural typo (it is the STRATEGY hook's shape), so
    /// it must be caught at compile rather than as a per-bar function-not-found on every bar.
    #[test]
    fn an_on_bar_with_the_wrong_arity_is_rejected_at_compile() {
        assert!(compile_indicator("bad", "fn on_bar() { 1.0 }").is_err());
        assert!(compile_indicator("bad", "fn on_bar(a, b) { 1.0 }").is_err());
    }

    #[test]
    fn a_syntax_error_is_a_compile_error_not_a_panic() {
        assert!(compile_indicator("bad", "fn on_bar(bar) { this. }").is_err());
    }

    /// ⚠ The NaN trap, from the other side. A runtime fault CANNOT be distinguished from warm-up
    /// by its value, so it must be distinguishable some other way — that is what `fault` is for.
    #[test]
    fn a_runtime_fault_is_recorded_rather_than_passing_as_warm_up() {
        let src = "fn on_bar(bar) { if bar.close > 2.0 { throw \"boom\" } bar.close }";
        let mut ind = compile_indicator("boom", src).unwrap();
        let bs = bars(&[1.0, 3.0, 1.5]);
        assert_eq!(ind.on_bar(&bs[0])[0], 1.0);
        assert!(ind.fault().is_none());

        assert!(ind.on_bar(&bs[1])[0].is_nan(), "the trait has no error channel, so NaN");
        assert!(ind.fault().is_some_and(|f| f.contains("boom")), "...but the fault is HELD");

        // ...and a later good bar clears it, so the flag means "the last bar faulted", not "this
        // indicator faulted once". A sticky flag would disable a strategy for a transient.
        assert_eq!(ind.on_bar(&bs[2])[0], 1.5);
        assert!(ind.fault().is_none());
    }

    #[test]
    fn a_non_numeric_return_is_a_fault_naming_the_type() {
        let src = "fn on_bar(bar) { \"twenty\" }";
        let mut ind = compile_indicator("s", src).unwrap();
        assert!(ind.on_bar(&bars(&[1.0])[0])[0].is_nan());
        assert!(ind.fault().is_some_and(|f| f.contains("must return a number")));
    }

    /// See [`read_values`]: silently taking line 0 is the exact hazard `exclusion` refuses for the
    /// built-in band indicators, so creating one from this side — WITHOUT declaring the lines — is
    /// refused too. The refusal now names the declaration that makes it legal, which is the only
    /// part of this that changed.
    #[test]
    fn a_multi_output_return_is_refused_rather_than_truncated_to_line_zero() {
        let mut ind = compile_indicator("bands", "fn on_bar(bar) { [1.0, 2.0, 3.0] }").unwrap();
        let v = ind.on_bar(&bars(&[1.0])[0])[0];
        assert!(v.is_nan(), "must NOT return 1.0, the first line");
        assert!(ind.fault().is_some_and(|f| f.contains("single-output")), "{:?}", ind.fault());
        assert!(
            ind.fault().is_some_and(|f| f.contains("outputs()")),
            "the refusal must name the way IN, not just the limitation: {:?}",
            ind.fault()
        );
    }

    /// A band indicator: three lines, a `param()` knob, and a real warm-up bar.
    const BANDS: &str = r#"
        let width = param("width", 2.0);
        fn outputs() { ["upper", "mid", "lower"] }
        fn init() { #{ prev: () } }
        fn warmup() { 1 }
        fn on_bar(bar) {
            if this.prev == () { this.prev = bar.close; return (); }
            this.prev = bar.close;
            [bar.close + width, bar.close, bar.close - width]
        }
    "#;

    /// ⚠ **The compatibility gate.** Every indicator written before `outputs()` existed declares
    /// none, and for those NOTHING may change: one line named after the file, one value out of
    /// `on_bar`, one row out of `vectorize`, and an ARRAY still refused — at EVERY length, including
    /// the one-element array that would otherwise be a second, undeclared spelling of a number.
    ///
    /// Non-vacuous because the last two assertions are the SAME source with `fn outputs()` added:
    /// the array is accepted there, so this test measures the declaration and not the array shape.
    /// An implementation that simply started accepting arrays would fail on the refusals; one that
    /// never accepted them would fail on the acceptance.
    #[test]
    fn a_file_that_declares_no_outputs_is_exactly_single_output_as_before() {
        let mut plain = compile_indicator("plain", "fn on_bar(bar) { bar.close }").unwrap();
        assert_eq!(plain.outputs(), ["plain".to_string()], "one line, named after the file");
        let bs = bars(&[1.0, 2.0]);
        assert_eq!(plain.on_bar(&bs[0]).len(), 1);
        assert_eq!(plain.value().len(), 1);
        assert_eq!(plain.vectorize(&bs).len(), 1, "vectorize is one row per line");

        for body in ["[bar.close]", "[bar.close, bar.close]"] {
            let mut ind =
                compile_indicator("arr", &format!("fn on_bar(bar) {{ {body} }}")).unwrap();
            assert!(ind.on_bar(&bs[0])[0].is_nan(), "{body} must not resolve to a value");
            assert!(
                ind.fault().is_some_and(|f| f.contains("single-output")),
                "{body}: {:?}",
                ind.fault()
            );
        }
        // ...and the identical bodies with the lines DECLARED are fine, so the refusals above are
        // about the missing declaration.
        for (decl, body) in
            [(r#"["a"]"#, "[bar.close]"), (r#"["a", "b"]"#, "[bar.close, bar.close]")]
        {
            let src = format!("fn outputs() {{ {decl} }} fn on_bar(bar) {{ {body} }}");
            let mut ind = compile_indicator("arr", &src).unwrap();
            assert!(ind.on_bar(&bs[0])[0].is_finite(), "{decl}: {:?}", ind.fault());
            assert!(ind.fault().is_none(), "{decl}: {:?}", ind.fault());
        }
    }

    #[test]
    fn a_declared_line_list_is_reported_in_order_and_drives_on_bars_width() {
        let mut ind = compile_indicator("bands", BANDS).unwrap();
        assert_eq!(ind.outputs(), ["upper".to_string(), "mid".into(), "lower".into()]);
        let bs = bars(&[10.0, 20.0]);
        let warming = ind.on_bar(&bs[0]);
        assert_eq!(warming.len(), 3, "a `()` return is warm-up on EVERY line, not just line 0");
        assert!(warming.iter().all(|v| v.is_nan()), "{warming:?}");
        assert_eq!(ind.on_bar(&bs[1]), vec![22.0, 20.0, 18.0]);
        assert_eq!(ind.value(), vec![22.0, 20.0, 18.0]);
    }

    /// ⚠ A mismatch is an ERROR naming BOTH counts — never a pad (which invents a number for a line
    /// the author never computed) and never a truncation (which hides one they did). Both would be
    /// silent, on the value path a strategy trades from.
    #[test]
    fn a_line_count_mismatch_is_an_error_naming_both_counts() {
        let short = "fn outputs() { [\"a\", \"b\", \"c\"] } fn on_bar(bar) { [1.0, 2.0] }";
        let mut ind = compile_indicator("short", short).unwrap();
        let out = ind.on_bar(&bars(&[1.0])[0]);
        assert_eq!(out.len(), 3, "the value vector still matches the DECLARATION");
        assert!(out.iter().all(|v| v.is_nan()), "and every line is the fault NaN: {out:?}");
        let f = ind.fault().unwrap_or_default().to_string();
        assert!(f.contains("declares 3"), "{f}");
        assert!(f.contains("returned 2"), "{f}");
        assert!(f.contains("a, b, c"), "the message names the lines: {f}");

        // ...and the same for a SCALAR return, which is the natural mistake when adding a line.
        let scalar = "fn outputs() { [\"a\", \"b\"] } fn on_bar(bar) { bar.close }";
        let mut ind = compile_indicator("scalar", scalar).unwrap();
        ind.on_bar(&bars(&[1.0])[0]);
        let f = ind.fault().unwrap_or_default().to_string();
        assert!(f.contains("declares 2") && f.contains("returned 1"), "{f}");
    }

    /// One line may warm up on its own — `[upper, (), lower]` — which is what lets a signal line
    /// that needs more history than its own source say so.
    #[test]
    fn a_single_line_may_warm_up_while_the_others_are_real() {
        let src = "fn outputs() { [\"v\", \"sig\"] } fn on_bar(bar) { [bar.close, ()] }";
        let mut ind = compile_indicator("half", src).unwrap();
        let out = ind.on_bar(&bars(&[7.0])[0]);
        assert_eq!(out[0], 7.0);
        assert!(out[1].is_nan());
        assert!(ind.fault().is_none(), "a `()` LINE is warm-up, not a fault: {:?}", ind.fault());
    }

    /// The `vectorize`-is-the-fold property, now on every line at once. Line-major, and bit-for-bit
    /// against the streaming path — an implementation that transposed the result, or that folded
    /// only line 0, fails here rather than in a chart nobody diffed.
    #[test]
    fn vectorize_is_line_major_and_bit_identical_to_the_streaming_fold_on_every_line() {
        let ind = compile_indicator("bands", BANDS).unwrap();
        let bs = bars(&[1.0, 2.5, 3.25, 4.125, 9.0]);
        let batch = ind.vectorize(&bs);
        assert_eq!(batch.len(), 3, "one row per declared line");
        assert!(batch.iter().all(|l| l.len() == bs.len()), "each row is one value per bar");
        let mut s = ind.clone();
        for (i, b) in bs.iter().enumerate() {
            let streamed = s.on_bar(b);
            for (l, v) in streamed.iter().enumerate() {
                assert_eq!(
                    v.to_bits(),
                    batch[l][i].to_bits(),
                    "line {l} bar {i}: {v} != {}",
                    batch[l][i]
                );
            }
        }
    }

    /// A knob must reach every line, not only the one somebody tested.
    #[test]
    fn a_call_site_knob_moves_every_line() {
        let mut wide = compile_indicator_with("bands", BANDS, &[5.0]).unwrap();
        let bs = bars(&[10.0, 20.0]);
        wide.on_bar(&bs[0]);
        assert_eq!(wide.on_bar(&bs[1]), vec![25.0, 20.0, 15.0], "width 5, not the default 2");
    }

    #[test]
    fn outputs_must_be_a_non_empty_array_of_named_lines() {
        for (src, needle) in [
            ("fn outputs() { 3 } fn on_bar(bar) { bar.close }", "array of line names"),
            ("fn outputs() { [] } fn on_bar(bar) { bar.close }", "empty array"),
            ("fn outputs() { [\"a\", 7] } fn on_bar(bar) { bar.close }", "not a line NAME"),
            ("fn outputs() { [\"a\", \"  \"] } fn on_bar(bar) { bar.close }", "is empty"),
        ] {
            let e = compile_indicator("bad", src).unwrap_err().to_string();
            assert!(e.contains(needle), "expected {needle:?} in: {e}");
        }
    }

    /// ⚠ Two names that SANITISE alike would generate ONE accessor with two meanings, silently —
    /// `register_fn` replaces. That is the registry's `generated_line_names_are_unambiguous` hazard
    /// asked of one user file, where the author can act on it, so it is a COMPILE error.
    #[test]
    fn two_lines_that_spell_one_accessor_are_refused_at_compile() {
        let src = "fn outputs() { [\"%K\", \"K\"] } fn on_bar(bar) { [1.0, 2.0] }";
        let e = compile_indicator("stoch_ish", src).unwrap_err().to_string();
        assert!(e.contains("both spell"), "{e}");
        assert!(e.contains("stoch_ish_k"), "the message names the collision: {e}");
        // ...and the same two lines under names that do NOT collide compile fine, so the rule is
        // about the collision rather than about `%`.
        let ok = "fn outputs() { [\"%K\", \"%D\"] } fn on_bar(bar) { [1.0, 2.0] }";
        assert!(compile_indicator("stoch_ish", ok).is_ok());
    }

    /// A line whose accessor would shadow a BUILT-IN is refused where the author can fix it — at
    /// COMPILE, naming the accessor — rather than by silently losing that one accessor to the
    /// built-in at registration time.
    ///
    /// ⚠ The witness is DERIVED, and it has to be. [`crate::line_fn_name`] joins the two halves
    /// with a literal `_`, so the obvious hand-written witness — `pos` + `ition` for the host read
    /// `position` — spells `pos_ition` and collides with nothing; this test was written that way
    /// and failed. Only a name CONTAINING a `_` is reachable, and every one of those is a registry
    /// entry, so the pair comes from the registry.
    #[test]
    fn a_line_whose_accessor_would_take_a_builtins_name_is_refused_at_compile() {
        let (stem, line) = crate::engine::registry_name_a_user_line_could_spell();
        let taken = crate::line_fn_name(stem, line);
        let body = "fn on_bar(bar) { [1.0, 2.0] }";
        let e = compile_indicator(stem, &format!("fn outputs() {{ [\"{line}\", \"x\"] }} {body}"))
            .unwrap_err()
            .to_string();
        assert!(e.contains(&taken), "the refusal must name the accessor it would take: {e}");
        assert!(e.contains("built-in indicator's own name"), "{e}");
        // ...and the SAME file with that one line renamed compiles, so the refusal is about the
        // collision and not about declaring outputs at all.
        let ok = format!("fn outputs() {{ [\"{line}_of_mine\", \"x\"] }} {body}");
        assert!(compile_indicator(stem, &ok).is_ok(), "{:?}", compile_indicator(stem, &ok).err());
    }

    /// An integer return is the natural spelling of a counting indicator; requiring `.to_float()`
    /// would be a papercut with no upside.
    #[test]
    fn an_integer_return_coerces() {
        let mut ind = compile_indicator("i", "fn on_bar(bar) { 42 }").unwrap();
        assert_eq!(ind.on_bar(&bars(&[1.0])[0])[0], 42.0);
    }

    /// The safety boundary from the module doc, asserted rather than asserted-in-prose: the
    /// indicator engine registers no verbs, so an indicator that tries to trade fails to resolve.
    #[test]
    fn an_indicator_cannot_place_an_order_or_read_the_broker() {
        for verb in ["buy(1.0)", "sell(1.0)", "position()", "equity()", "sma(20)"] {
            let src = format!("fn on_bar(bar) {{ {verb}; bar.close }}");
            let mut ind = compile_indicator("rogue", &src).unwrap();
            assert!(ind.on_bar(&bars(&[1.0])[0])[0].is_nan(), "{verb} must not resolve");
            assert!(ind.fault().is_some(), "{verb} must fault");
        }
    }

    /// The runaway bound. `set_max_operations` mirrors `build_engine`'s, so a user indicator
    /// cannot hang the bar loop where a strategy could not.
    #[test]
    fn a_runaway_script_is_bounded_by_the_operation_limit() {
        let src = "fn on_bar(bar) { let i = 0; loop { i += 1; } }";
        let mut ind = compile_indicator("spin", src).unwrap();
        assert!(ind.on_bar(&bars(&[1.0])[0])[0].is_nan());
        assert!(ind.fault().is_some(), "the operation limit must surface as a fault");
    }

    // ----------------------------------------------------- the chart-mounting seam

    /// The DEFAULT is the safe one: an undeclared placement is the indicator's own pane.
    ///
    /// Non-vacuous: the opposite default is one word away in `compile_indicator_with`, and it is
    /// the one that silently ruins the price pane's autofit for any indicator whose scale is not a
    /// price — which is most of them.
    #[test]
    fn overlay_is_optional_and_absent_means_its_own_pane() {
        assert!(!compile_indicator("p", "fn on_bar(bar) { bar.close }").unwrap().is_overlay());
        let src = "fn overlay() { true } fn on_bar(bar) { bar.close }";
        assert!(compile_indicator("o", src).unwrap().is_overlay());
        let src = "fn overlay() { false } fn on_bar(bar) { bar.close }";
        assert!(!compile_indicator("f", src).unwrap().is_overlay());
    }

    /// A non-bool `overlay()` is a COMPILE error naming the hook, not a silent `false`.
    ///
    /// Non-vacuous: `as_bool().unwrap_or(false)` is the tempting spelling and would accept
    /// `fn overlay() { 1 }` — the natural way to write it for anyone coming from C — by quietly
    /// meaning the opposite of what was written.
    #[test]
    fn a_non_bool_overlay_is_rejected_at_compile_naming_the_hook() {
        let e = compile_indicator("o", "fn overlay() { 1 } fn on_bar(bar) { bar.close }")
            .unwrap_err()
            .to_string();
        assert!(e.contains("overlay"), "{e}");
    }

    /// [`user_meta`] carries the placement the script declared.
    ///
    /// Non-vacuous: a hard-coded `RenderKind` (either one) passes half of this and fails the other.
    #[test]
    fn user_meta_maps_the_overlay_hook_onto_the_render_kind() {
        let osc = user_meta(&compile_indicator("um_osc", "fn on_bar(bar) { bar.close }").unwrap());
        assert_eq!(osc.kind, vike_indicators::RenderKind::Oscillator);
        let src = "fn overlay() { true } fn on_bar(bar) { bar.close }";
        let ovl = user_meta(&compile_indicator("um_ovl", src).unwrap());
        assert_eq!(ovl.kind, vike_indicators::RenderKind::Overlay);
        assert!(ovl.is_user() && osc.is_user());
        assert_eq!(ovl.name, "um_ovl");
        assert_eq!(ovl.pretty, "um_ovl", "the label is the file stem verbatim");
    }

    /// The declared `param()` knobs become the meta's parameter surface, IN ORDER — the order a
    /// call site's positional arguments and the settings dialog's rows both depend on.
    ///
    /// Non-vacuous: `RhaiIndicator::params` is an `IndexMap`-backed first-seen order, and any
    /// re-collection through a `HashMap` would still produce two rows with the right names and
    /// defaults while scrambling which one `my_thing(50)` sets.
    #[test]
    fn user_meta_carries_the_declared_knobs_in_first_seen_order() {
        let src = "let a = param(\"slow\", 50); let b = param(\"fast\", 0.25); \
                   fn on_bar(bar) { bar.close }";
        let m = user_meta(&compile_indicator("um_params", src).unwrap());
        assert_eq!(m.params.len(), 2);
        assert_eq!(m.params[0].name, "slow");
        assert_eq!(m.params[0].default, 50.0);
        assert_eq!(m.params[0].step, 1.0, "a whole-number default is a bar count");
        assert_eq!(m.params[1].name, "fast");
        assert_eq!(m.params[1].default, 0.25);
        assert_eq!(m.params[1].step, 0.001, "a fractional default must stay editable to 3 dp");
    }

    /// The point of the whole seam: the meta's `factory` builds an indicator that computes the
    /// USER's recurrence at the CALL SITE's parameters.
    ///
    /// Non-vacuous twice: `build_with` reaching the built-in `make_with` slot would panic
    /// (`vike_indicators`' `unbuilt`), and a factory that ignored `raw` and cloned the prototype
    /// unchanged would return 30.0 rather than 50.0 on the second assert.
    #[test]
    fn the_meta_factory_builds_the_users_recurrence_at_the_call_sites_params() {
        let src = "let k = param(\"k\", 3.0); fn on_bar(bar) { bar.close * k }";
        let m = user_meta(&compile_indicator("um_factory", src).unwrap());
        let b = &bars(&[10.0])[0];
        assert_eq!(m.build().on_bar(b)[0], 30.0, "no params -> the declared default");
        assert_eq!(m.build_with(&[5.0]).on_bar(b)[0], 50.0, "the call site's k reaches the script");
    }

    /// A knob whose value breaks the file's TOP LEVEL falls back to the declared defaults and
    /// LOGS, rather than panicking or plotting a fabricated number — the residual `user_meta`'s
    /// factory documents.
    ///
    /// Non-vacuous: `compile_with(...).unwrap()` (the shorter spelling) turns this exact script
    /// into a panic inside a chart repaint, which is the failure this arm exists to prevent.
    ///
    /// ⚠ **`k.to_int()` is the whole reason this test tests anything.** `param()` hands back an
    /// f64, and rhai resolves a mixed `INT / FLOAT` through its FLOAT `/`, which is plain IEEE
    /// division with no zero check (in the rhai crate's own source: `Divide => impl_op!(FLOAT =>
    /// $xx / $yy)` in `src/func/builtin.rs`, while only the INT `divide` in
    /// `src/packages/arithmetic.rs` raises `Division by zero`). So the
    /// obvious spelling — `100 / (k - 5)` — quietly evaluates to `inf` at k=5, compiles fine, and
    /// leaves this `Err` arm with ZERO coverage while looking covered. Forcing the divisor to an
    /// INT is what makes the failure real. `the_premise` below asserts it directly, so a future
    /// rhai that changes either rule fails HERE, naming the reason, instead of failing on an
    /// unexplained number three lines down.
    #[test]
    fn a_param_value_that_breaks_the_top_level_falls_back_to_the_defaults() {
        // `k == 5` is INTEGER division by zero at the top level — an error rhai does raise, and
        // one only the RE-run at that value hits.
        let src = "let k = param(\"k\", 3); let scale = 100 / (k.to_int() - 5); \
                   fn on_bar(bar) { bar.close * scale }";
        let proto = compile_indicator("um_bad", src).unwrap();

        // the_premise: k=5 must genuinely fail to compile, and k=4 must genuinely succeed.
        assert!(
            proto.compile_with(&[5.0]).is_err(),
            "premise: k=5 must break the file's top level, or the fallback below proves nothing"
        );
        assert!(proto.compile_with(&[4.0]).is_ok(), "premise: a good override still compiles");

        let m = user_meta(&proto);
        let b = &bars(&[1.0])[0];
        // defaults: k=3 -> scale = 100/(3-5) = -50
        assert_eq!(m.build().on_bar(b)[0], -50.0);
        // k=4 -> scale = 100/(4-5) = -100: a good override genuinely takes effect...
        assert_eq!(m.build_with(&[4.0]).on_bar(b)[0], -100.0);
        // ...and k=5 (division by zero at the top level) degrades to the defaults, not a panic.
        assert_eq!(m.build_with(&[5.0]).on_bar(b)[0], -50.0);
    }

    /// The rhai semantics the test above depends on, asserted on rhai itself rather than inferred.
    ///
    /// This is the claim that was WRONG when the fallback test was first written: it used
    /// `100 / (k - 5)` with a float `k`, believed that raised `Division by zero`, and so never
    /// exercised the `Err` arm it existed to cover at all. Pinning the language rule separately
    /// means the next person who reaches for the shorter spelling is told why it does not work.
    #[test]
    fn rhai_raises_on_integer_division_by_zero_but_not_on_float() {
        let float_div = "let k = param(\"k\", 3); let scale = 100 / (k - 5); \
                         fn on_bar(bar) { scale }";
        let mut ind = compile_indicator("f", float_div)
            .unwrap()
            .compile_with(&[5.0])
            .expect("float division by zero is NOT an error in rhai — it is `inf`");
        assert!(ind.on_bar(&bars(&[1.0])[0])[0].is_infinite(), "...and the value is infinite");

        let int_div = "let k = param(\"k\", 3); let scale = 100 / (k.to_int() - 5); \
                       fn on_bar(bar) { scale }";
        let e = compile_indicator("i", int_div).unwrap().compile_with(&[5.0]).unwrap_err();
        assert!(e.to_string().contains("Division by zero"), "{e}");
    }

    /// A paramless user indicator is a real one — `coerce` over an empty spec list must not turn
    /// its zero-argument construction into an error or a NaN.
    #[test]
    fn a_paramless_user_indicator_builds_from_an_empty_slice() {
        let m = user_meta(&compile_indicator("um_plain", "fn on_bar(bar) { bar.close }").unwrap());
        assert!(m.params.is_empty());
        assert_eq!(m.build().on_bar(&bars(&[7.5])[0])[0], 7.5);
        assert_eq!(m.build_with(&[]).on_bar(&bars(&[7.5])[0])[0], 7.5);
    }
}
