//! User-written indicators bound beside the built-ins: name conflicts, line accessors, binding.

use crate::ctx::SharedCtx;
use crate::engine::bindable::{bare_name_binds, line_fn_name, sanitize_line, unbound_reason};
use crate::engine::builtin::{MAX_INDICATOR_ARITY, line_accessors};
use crate::engine::host::{HOST_FN_NAMES, arg, bad_arg};
use std::sync::LazyLock;

/// Every per-line accessor name the BUILT-IN bridge registers (`bollinger_mid`, `stochastic_k`, …),
/// built once per process from the same [`line_accessors`] derivation `register_indicators` binds
/// from.
///
/// ⚠ These are FUNCTION names a script resolves, not registry entries, so
/// `registry().iter().any(|m| m.name == n)` does not see them — which is how a user file called
/// `stochastic_k.rhai` could silently take over the built-in accessor of that name. Registration
/// order makes it certain rather than lucky: `build_engine` registers the user set AFTER
/// `register_indicators`, and `register_fn` REPLACES. So this set joins the refusals in
/// [`user_indicator_conflict`] and [`user_line_conflict`].
static BUILTIN_LINE_FNS: LazyLock<std::collections::HashSet<String>> = LazyLock::new(|| {
    vike_indicators::registry()
        .iter()
        .flat_map(|m| line_accessors(m.name).into_iter().map(|(_, f)| f))
        .collect()
});

/// Why a USER-written indicator (`user_data/indicators/<name>.rhai`) cannot be bound under that
/// name — `None` when the name is free.
///
/// The rules are [`exclusion`](crate::engine::bindable::exclusion)'s, asked about a user file instead of a registry entry, plus one
/// that only exists on this side: a user name may not take a BUILT-IN indicator's name. All four
/// are refusals of SILENT SHADOWING, which is the only failure mode here that a script author
/// could not diagnose from their own file.
///
/// ⚠ The built-in rule deliberately refuses even the names `exclusion` leaves unbound (`var`,
/// `bollinger`, …). Those are not free slots: they are names this engine has REASONS to withhold,
/// and letting a user file occupy `bollinger` would make `bollinger(20)` resolve to something
/// unrelated to Bollinger bands on that one machine — a wrong answer that travels with the script
/// and looks like a built-in to everyone reading it.
pub fn user_indicator_conflict(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("an indicator file needs a name".into());
    }
    if rhai::Engine::new_raw().compile(format!("{name}()")).is_err() {
        return Some(format!(
            "`{name}` is not callable from Rhai — the name is a reserved word or contains a \
             character the grammar reads as a symbol, so a script calling it would fail at PARSE \
             time. Rename the file."
        ));
    }
    if HOST_FN_NAMES.contains(&name) {
        return Some(format!(
            "`{name}` is a built-in host function (a read, an order verb, or `param`), and binding \
             it would SHADOW that function for every script. Rename the file."
        ));
    }
    if vike_indicators::registry().iter().any(|m| m.name == name) {
        let extra = unbound_reason(name)
            .map(|why| format!(" (that built-in is itself unbound from Rhai — {why})"))
            .unwrap_or_default();
        return Some(format!(
            "`{name}` is a built-in indicator{extra}. A user file may not take a built-in's name: \
             the same call would mean different things on different machines. Rename the file."
        ));
    }
    if BUILTIN_LINE_FNS.contains(name) {
        return Some(format!(
            "`{name}` is a built-in indicator's per-line accessor (see `vike-cli indicators \
             --json`), and the user set is registered LAST, so this file would silently answer \
             every `{name}()` in every strategy. Rename the file."
        ));
    }
    None
}

/// Why a USER indicator's declared output LINE cannot become a per-line accessor — `None` when the
/// generated name ([`line_fn_name`]) is free.
///
/// The line half of [`user_indicator_conflict`], and deliberately the SAME four refusals
/// `register_indicators` applies to a built-in's generated accessor, plus the one that only exists
/// on this side (a built-in accessor's own name). Applied at COMPILE, so an author is told which
/// line to rename instead of losing an accessor silently — the built-in side can only `continue`
/// past a bad name, because the registry is not the reader's to fix.
///
/// ⚠ **What a generated accessor can and cannot collide with.** [`line_fn_name`] joins the two
/// halves with a literal `_`, so the name this asks about ALWAYS carries one. Only a name that
/// contains a `_` is therefore reachable at all — which is why the host-read/verb refusal below
/// cannot fire today (no [`HOST_FN_NAMES`] entry has one) while the built-in-indicator refusal can
/// (`smi_ergodic`, `williams_fractal`, … are registry names a `<stem>_<line>` accessor spells
/// exactly). Both rules stay, because that is a property of the separator and of two lists, either
/// of which can change; `every_user_line_rule_is_reachable_or_provably_not` asserts which of them
/// is which so the unreachable one cannot go quietly reachable.
///
/// ⚠ **One residual, DECLARED rather than silently tolerated: collisions BETWEEN two user files.**
/// This sees one file, so `a.rhai` declaring a line `b` (accessor `a_b`) and a file `a_b.rhai` are
/// invisible to each other, and the later registration wins. Nothing here can answer it: the
/// question needs the whole set, which only the loader (`load.rs`, which already reports
/// `DuplicateName` for two files claiming one stem) and `register_user_indicators` ever hold. It is
/// narrower than the built-in collision this DOES refuse — both files are the reader's own, in one
/// directory they can list — which is why it is documented and not built.
pub fn user_line_conflict(indicator: &str, line: &str) -> Option<String> {
    if sanitize_line(line).is_empty() {
        return Some(format!(
            "a line named `{line}` carries no character a Rhai identifier can hold, so it spells no \
             accessor at all. Give it a name with letters or digits in it."
        ));
    }
    let fname = line_fn_name(indicator, line);
    if rhai::Engine::new_raw().compile(format!("{fname}()")).is_err() {
        return Some(format!(
            "the accessor it generates, `{fname}`, is not callable from Rhai — it is a reserved \
             word, or the file's own name carries a character the grammar reads as a symbol, so a \
             script calling it would fail at PARSE time. Rename the line (or the file)."
        ));
    }
    if HOST_FN_NAMES.contains(&fname.as_str()) {
        return Some(format!(
            "the accessor it generates, `{fname}`, is a host read/verb, and binding it would \
             SHADOW that function for every script. Rename the line."
        ));
    }
    if vike_indicators::registry().iter().any(|m| m.name == fname) {
        return Some(format!(
            "the accessor it generates, `{fname}`, is a built-in indicator's own name. Rename the \
             line."
        ));
    }
    if BUILTIN_LINE_FNS.contains(&fname) {
        return Some(format!(
            "the accessor it generates, `{fname}`, is a built-in indicator's per-line accessor. \
             Rename the line."
        ));
    }
    None
}

/// [`bare_name_binds`], asked about a user file's declared line list — the ONE rule, not a second
/// spelling of it. `my_bands()` handing back `upper` is the same wrong answer `bollinger()` was
/// refused for, and every line stays reachable through [`user_line_accessors`] either way.
pub(crate) fn user_bare_name_binds(name: &str, lines: &[String]) -> bool {
    bare_name_binds(name, lines.len(), lines.first().map(String::as_str))
}

/// Every CALLABLE per-line spelling of one USER indicator's outputs, as `(line name, function name)`
/// pairs in declaration order — the [`line_accessors`] twin for a file the user wrote.
///
/// Empty for a single-output indicator (its bare name already IS line 0), exactly as on the built-in
/// side. There is no filtering arm here and that is not an oversight: every rule
/// [`line_accessors`] applies at registration time is applied to a user file at COMPILE, by
/// [`user_line_conflict`], so a `RhaiIndicator` that exists at all has usable line names.
pub fn user_line_accessors(ind: &crate::RhaiIndicator) -> Vec<(String, String)> {
    let name = vike_indicators::Indicator::name(ind);
    let lines = ind.outputs();
    if lines.len() < 2 {
        return Vec::new();
    }
    lines.iter().map(|l| (l.clone(), line_fn_name(name, l))).collect()
}

/// Registers user-written indicators (`user_data/indicators/`) onto `engine` as host functions,
/// alongside the built-in bridge.
///
/// Each is fed the current bar EXACTLY once per bar through the same `ctx.indicators` /
/// `fed_this_bar` cache the built-ins use, so a script referencing `my_thing()` three times in one
/// bar streams it once — the property `sma_bridge_second_reference_same_bar_is_cached` gates for
/// the built-in side and `user_indicator_is_fed_once_per_bar` gates here.
///
/// TWO families of name, exactly as [`register_indicators`](crate::engine::builtin::register_indicators) generates for a built-in and from the
/// SAME [`line_fn_name`]:
///
/// - the **bare name**, `my_thing()` — bound for a single-output file, and for a multi-output one
///   whose line 0 is the namesake line. See [`user_bare_name_binds`].
/// - a **per-line accessor** for every line of a multi-output file, `my_bands_mid()`
///   ([`user_line_accessors`]). This is what makes a user-written band indicator's middle band
///   reachable at all, and it is why `on_bar` may now return an array.
///
/// ⚠ **All the lines of one user indicator share ONE streaming instance**, because
/// [`register_user_form`] keys the cache on `(indicator name, arguments)` — the LINE is captured by
/// the closure and never enters the key. Three accessor reads in one bar therefore feed the file's
/// `on_bar` once and index three entries of the one vector it returned. A line in the key would run
/// the author's recurrence three times over the same bars: same numbers at triple the cost, three
/// `fed_this_bar` slots where the contract says one, and — since a user `on_bar` is arbitrary code —
/// three independent copies of state that only agree while it stays deterministic.
///
/// One form per argument count, from zero up to the file's own `param()` count (capped by
/// [`MAX_INDICATOR_ARITY`]) — the same ladder the built-ins get, for the same reason: an argument
/// past the declared surface must be a function-not-found rather than a value silently discarded.
/// Every line accessor offers the same ladder as the bare name, so `my_bands_mid(50)` takes what
/// `my_bands(50)` would.
///
/// A name [`user_indicator_conflict`] rejects is SKIPPED rather than bound — the caller is expected
/// to have surfaced the reason at load time (`vike-cli init` and the Studio loader both do), and
/// binding it anyway would be the shadowing the rule exists to prevent. Its LINES need no such arm:
/// [`user_line_conflict`] refused a bad one at compile, where the author is told which to rename.
pub(crate) fn register_user_indicators(
    engine: &mut rhai::Engine,
    ctx: &SharedCtx,
    inds: &[crate::RhaiIndicator],
) {
    for ind in inds {
        let name = vike_indicators::Indicator::name(ind).to_string();
        if user_indicator_conflict(&name).is_some() {
            continue;
        }
        let arity = ind.params().len().min(MAX_INDICATOR_ARITY);
        if user_bare_name_binds(&name, ind.outputs()) {
            for argc in 0..=arity {
                register_user_form(engine, ctx, ind, &name, &name, 0, argc);
            }
        }
        for (line, (_, fname)) in user_line_accessors(ind).into_iter().enumerate() {
            for argc in 0..=arity {
                register_user_form(engine, ctx, ind, &name, &fname, line, argc);
            }
        }
    }
}

/// Registers ONE argument-count form of ONE spelling (the bare name, or one line accessor) of a user
/// indicator. `name` is the indicator's own name and is the CACHE IDENTITY; `fname` is the spelling
/// being registered; `line` is which entry of `on_bar`'s return this spelling reads.
///
/// ⚠ **The cache key carries the arguments and NOT the line**, `"user:<name>:<args>"`. Two halves,
/// both load-bearing:
///
/// - `my_mean(20)` and `my_mean(50)` are two streaming instances rather than one — exactly the
///   identity rule `indicator_value` applies to the built-ins. A key on the name alone would make
///   the SECOND call site in a script silently read the first one's lookback.
/// - `my_bands_upper(20)` and `my_bands_mid(20)` are ONE instance read at two indices. `fname` is
///   deliberately not in the key: it is the spelling, not the identity, and putting it there would
///   stream one file's recurrence once per line.
///
/// The instance is built by re-running the file's top level with the arguments bound to its
/// `param()` declarations (`RhaiIndicator::compile_with`), so a knob genuinely changes the
/// recurrence rather than being recorded and ignored.
fn register_user_form(
    engine: &mut rhai::Engine,
    ctx: &SharedCtx,
    ind: &crate::RhaiIndicator,
    name: &str,
    fname: &str,
    line: usize,
    argc: usize,
) {
    // The compiled indicator is the PROTOTYPE. It is re-instantiated per distinct argument tuple
    // and cached in `ctx.user_indicators`, so two strategies built from one loaded set — and two
    // call sites with different knobs — never share streaming state.
    //
    // ⚠ It is cached as a CONCRETE `RhaiIndicator`, in its own map, rather than boxed into
    // `ctx.indicators` beside the built-ins. That map is `Box<dyn Indicator>`, and reading the fault
    // back out of a trait object would mean either a downcast (`Indicator` exposes no `as_any`) or
    // widening the shared trait with a method that means nothing to the ~140 built-ins. The fault is
    // the whole reason this bridge can be loud; a concrete map keeps it reachable without either.
    let proto = ind.clone();
    let c = ctx.clone();
    let n = name.to_string();
    // The line list the ACCESSORS were generated from — the prototype's, i.e. the file read at its
    // `param()` defaults. Checked against each re-instantiation below.
    let declared: Vec<String> = ind.outputs().to_vec();

    // The bar path, shared by every form: resolve-or-build the instance for THESE arguments, feed it
    // at most once this bar, raise a fault, return the value.
    let body = move |args: &[f64]| -> Result<f64, Box<rhai::EvalAltResult>> {
        let key = format!("{n}:{args:?}");
        let mut g = c.write().unwrap();
        if !g.user_indicators.contains_key(&key) {
            // A re-instantiation can FAIL — the file's top level runs again with the caller's
            // values, and a script that divides by a knob will throw when that knob is 0. Raising
            // here is the same choice `bad_arg` makes: an error the author can see beats a NaN they
            // cannot distinguish from warm-up.
            let built = proto
                .compile_with(args)
                .map_err(|e| -> Box<rhai::EvalAltResult> { e.to_string().into() })?;
            // ⚠ `fn outputs()` runs AFTER the top level, so it can read a `param()` knob and return
            // a different list for a different call site — and the accessors were already
            // registered from the prototype's list. `my_bands_lower(9)` would then read index 2 of
            // a two-line vector: NaN, forever, with no fault. Raise instead, naming both lists.
            if built.outputs() != declared.as_slice() {
                return Err(format!(
                    "{n}: `fn outputs()` must not depend on a `param()` knob — the accessors were \
                     registered from [{}], but with argument(s) {args:?} this file declares [{}]. \
                     Return a fixed list.",
                    declared.join(", "),
                    built.outputs().join(", ")
                )
                .into());
            }
            g.user_indicators.insert(key.clone(), built);
        }
        // One `fed_this_bar` namespace shared with the built-ins. The `user:` prefix is what keeps
        // that safe: a built-in key is `"<name>:<coerced params>"`, which this would otherwise
        // collide with for a same-named paramless pair — `user_indicator_conflict` refuses that
        // name anyway, but relying on it from here would make this correct only by a rule enforced
        // somewhere else.
        let fed_key = format!("user:{key}");
        if g.fed_this_bar.insert(fed_key) {
            let bar = g.cur_bar.clone();
            vike_indicators::Indicator::on_bar(g.user_indicators.get_mut(&key).unwrap(), &bar);
        }
        let slot = g.user_indicators.get(&key).unwrap();
        // ⚠ The fault is RAISED, not passed on as the NaN it already is. `on_bar` had no error
        // channel (see `indicator.rs`), but this bridge does — and a warm-up-shaped NaN that
        // actually means "your script threw" is the exact failure `bad_arg` exists to prevent, one
        // layer down.
        if let Some(msg) = slot.fault() {
            return Err(msg.to_string().into());
        }
        // `line`, not `.first()`: the whole point of the accessors. Out of range is unreachable
        // (`read_values` pins `value().len()` to `outputs().len()`, which the accessors were
        // generated from) and reads as the warm-up NaN rather than panicking if that ever changes.
        Ok(vike_indicators::Indicator::value(slot).get(line).copied().unwrap_or(f64::NAN))
    };

    // `bad_arg` names the function the AUTHOR called — `my_bands_mid`, not `my_bands`: somebody who
    // wrote `my_bands_mid("20")` is not helped by an error about the indicator behind it.
    let label: &'static str = Box::leak(fname.to_string().into_boxed_str());
    match argc {
        0 => {
            engine.register_fn(label, move || body(&[]));
        }
        1 => {
            engine.register_fn(label, move |a: rhai::Dynamic| {
                let x = arg(&a).ok_or_else(|| bad_arg(label, 1, &a))?;
                body(&[x])
            });
        }
        2 => {
            engine.register_fn(label, move |a: rhai::Dynamic, b: rhai::Dynamic| {
                let x = arg(&a).ok_or_else(|| bad_arg(label, 1, &a))?;
                let y = arg(&b).ok_or_else(|| bad_arg(label, 2, &b))?;
                body(&[x, y])
            });
        }
        _ => {
            engine.register_fn(
                label,
                move |a: rhai::Dynamic, b: rhai::Dynamic, d: rhai::Dynamic| {
                    let x = arg(&a).ok_or_else(|| bad_arg(label, 1, &a))?;
                    let y = arg(&b).ok_or_else(|| bad_arg(label, 2, &b))?;
                    let z = arg(&d).ok_or_else(|| bad_arg(label, 3, &d))?;
                    body(&[x, y, z])
                },
            );
        }
    }
}
