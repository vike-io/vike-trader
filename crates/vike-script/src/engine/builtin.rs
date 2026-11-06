//! The built-in indicator bridge: registry indicators as Rhai host functions, fed once per bar.

use crate::ctx::SharedCtx;
use crate::engine::UNBOUND;
use crate::engine::bindable::{RHAI_INDICATORS, line_fn_name};
use crate::engine::host::{HOST_FN_NAMES, arg, bad_arg};
use vike_indicators::IndicatorMeta;

/// The highest argument count [`register_indicators`] registers a form for. Every bound indicator's
/// full parameter surface is reachable at this value today (`alma` is one of the deepest), and
/// `bound_indicator_arity_never_exceeds_the_registered_forms` FAILS if a future registry entry
/// needs one more, naming the arm to add. Without that gate a deeper indicator would bind quietly
/// with its tail parameters permanently pinned to the registry defaults — a knob that looks
/// present and is not.
pub(crate) const MAX_INDICATOR_ARITY: usize = 3;

/// Looks up (or lazily constructs via `IndicatorMeta::make_with`) the streaming indicator cached
/// under `"<name>:<coerced params>"` in `ctx.indicators`, feeds it `ctx.cur_bar` at most once per
/// bar (guarded by `ctx.fed_this_bar`), and returns its scalar output (`on_bar`/`value`'s index
/// `[0]`, which is the ONLY line — [`exclusion`](crate::engine::bindable::exclusion) refuses every multi-output indicator precisely so
/// that this `.first()` cannot be a truncation). A second reference to the same indicator with the
/// same parameters within one bar returns the cached `value()[0]` without re-advancing.
///
/// `meta` is the `&'static IndicatorMeta` captured by the host closure at registration, so the bar
/// path performs no registry lookup at all and an unknown name is unrepresentable here — a script
/// naming something unbound gets a Rhai function-not-found error, which is what
/// [`RHAI_INDICATORS`]' contract promises.
///
/// The key is built from the COERCED parameters (`vike_indicators::coerce`, the registry's one
/// value-coercion site) rather than from the raw arguments, so the cache key is the instance's real
/// identity: `sma()` and `sma(20)` are the same indicator and share one entry — and therefore one
/// `fed_this_bar` slot — instead of streaming two copies of it side by side.
fn indicator_value(ctx: &SharedCtx, meta: &'static IndicatorMeta, raw: &[f64], line: usize) -> f64 {
    let params = vike_indicators::coerce(meta.params, raw);
    let key = format!("{}:{params:?}", meta.name);
    let mut g = ctx.write().unwrap();
    if !g.indicators.contains_key(&key) {
        // `make_with` coerces again internally; `coerce` is idempotent (a coerced value is
        // non-NaN and already inside [min, max]), and the registry's own doc asks callers outside
        // it to coerce first, so passing the coerced slice is the documented call shape.
        g.indicators.insert(key.clone(), meta.build_with(&params));
    }
    if g.fed_this_bar.insert(key.clone()) {
        let bar = g.cur_bar.clone();
        let out = g.indicators.get_mut(&key).unwrap().on_bar(&bar);
        return out.get(line).copied().unwrap_or(f64::NAN);
    }
    // already fed this bar -> return cached value without re-advancing
    g.indicators.get(&key).unwrap().value().get(line).copied().unwrap_or(f64::NAN)
}

/// Every CALLABLE spelling of one registry indicator's output lines, as `(line name, function
/// name)` pairs in output order — `[("upper", "bollinger_upper"), ("mid", "bollinger_mid"), …]`.
///
/// Empty for a single-output indicator (its bare name already IS line 0), and empty for a
/// `batch_only` one (no line of a future-reading indicator is bound — see
/// [`register_indicators`]).
///
/// Exported because the advertising surfaces must not have to REDERIVE this. `RHAI_INDICATORS`
/// answers "which bare names bind", and after per-line accessors landed that stopped being the
/// whole callable set: `bollinger` is absent from it while `bollinger_mid` is callable. A surface
/// listing only the bare names would now be telling a user that a band indicator is unreachable,
/// which is exactly the drift `RHAI_INDICATORS`' own doc exists to prevent — so the second half of
/// the answer is exported beside the first, from the same derivation `register_indicators` binds
/// from.
pub fn line_accessors(indicator: &str) -> Vec<(&'static str, String)> {
    let Some(meta) = vike_indicators::registry().iter().find(|m| m.name == indicator) else {
        return Vec::new();
    };
    if meta.outputs.len() < 2 || meta.batch_only || meta.params.len() > MAX_INDICATOR_ARITY {
        return Vec::new();
    }
    meta.outputs
        .iter()
        .map(|o| (o.name, line_fn_name(meta.name, o.name)))
        .filter(|(_, f)| {
            !HOST_FN_NAMES.contains(&f.as_str())
                && !vike_indicators::registry().iter().any(|m| m.name == *f)
                && rhai::Engine::new_raw().compile(format!("{f}()")).is_ok()
        })
        .collect()
}

/// Whether a script can reach `indicator` by ANY spelling — its bare name, or at least one per-line
/// accessor.
///
/// ⚠ **This, not `RHAI_INDICATORS.contains(..)`, is the question every advertising surface means to
/// ask.** Those were the same question until per-line accessors landed and split them: `bollinger`
/// is absent from `RHAI_INDICATORS` (its bare name is refused — line 0 is the upper band) while
/// `bollinger_mid(20)` is perfectly callable. A surface still asking the old question reports a
/// band indicator as unreachable and offers the user no way in, which is worse than the silence it
/// replaced.
///
/// [`unbound_reason`](crate::engine::bindable::unbound_reason) remains the answer to the NARROWER question "why is the BARE name refused",
/// and it is still the right thing to print — its message names the accessor that works.
pub fn is_callable(indicator: &str) -> bool {
    RHAI_INDICATORS.contains(&indicator) || !line_accessors(indicator).is_empty()
}

/// Registers the indicator-bridge host functions onto `engine` — every `vike_indicators::registry()`
/// entry [`unbound_reason`](crate::engine::bindable::unbound_reason) does not reject, each backed by a streaming instance cached in
/// `ctx.indicators` and fed the current bar exactly once per bar (see [`indicator_value`]).
///
/// TWO families of name, both derived from the registry:
///
/// - the **bare name**, `sma(20)` — bound for every single-output indicator, and for the
///   multi-output ones whose line 0 is the namesake line (`macd`, `adx`, …). See [`exclusion`](crate::engine::bindable::exclusion)'s
///   rule 4 for why the others are refused a bare name.
/// - a **per-line accessor** for every line of every multi-output indicator,
///   [`line_fn_name`]`(indicator, line)` — `bollinger_mid(20)`, `macd_signal(12, 26, 9)`,
///   `stochastic_k(14)`. This is what makes a band indicator's middle band reachable at all.
///
/// ⚠ **All the lines of one indicator share ONE streaming instance**, because [`indicator_value`]
/// keys the cache on `(name, coerced params)` with NO line in the key. That is the whole point:
/// `bollinger_upper(20)` and `bollinger_mid(20)` in one bar feed the indicator ONCE and read two
/// entries of the same output vector. A line in the key would stream three independent copies of
/// bollinger side by side — same numbers, triple the work, and three `fed_this_bar` slots where the
/// contract says one.
///
/// One form per argument count, `0 ..= meta.params.len()` (capped by [`MAX_INDICATOR_ARITY`]):
///
/// - `name()` — every parameter at its registry default. This is the ONLY spelling for the
///   parameterless indicators (the candlestick patterns, `vwap`, `obv`, …), and the only way to
///   reach a FRACTIONAL default: `psar`'s `step` defaults to `0.02` with a `[0.001, 0.5]` range,
///   which no integer argument can express (`psar(0)` clamps to `0.001`, `psar(1)` to `0.5`).
/// - `name(p1)`, `name(p1, p2)`, `name(p1, p2, p3)` — up to that indicator's own parameter count.
///   Arguments beyond it are NOT registered, so `doji(5)` is a function-not-found rather than an
///   argument silently discarded, while parameters left off the end take their registry defaults
///   (`vike_indicators::coerce`'s documented contract, which is what makes the short forms safe).
///
/// Arguments are `rhai::Dynamic` so ints and floats both resolve — see [`arg`].
pub(crate) fn register_indicators(engine: &mut rhai::Engine, ctx: &SharedCtx) {
    for meta in vike_indicators::registry().iter() {
        // The BARE name, unless `exclusion` refused it. A refusal here is not a refusal of the
        // indicator: its per-line accessors below are registered regardless, which is how
        // `bollinger_mid` exists while `bollinger` does not.
        if !UNBOUND.contains_key(meta.name) {
            register_arity_forms(engine, ctx, meta, meta.name.to_string(), 0);
        }
        // The PER-LINE accessors. Single-output indicators get none — `sma_sma()` would be noise
        // beside `sma()`, and the bare name already IS line 0 for them.
        //
        // ⚠ Gated on the same `batch_only` rule as the bare name, deliberately: `ichimoku` and
        // `williams_fractal` are multi-output AND read future bars, so a per-line accessor would
        // hand back exactly the retroactively-revised value rule 3 exists to refuse. The rules
        // COMPOSE — lifting the multi-output refusal must not quietly lift that one too.
        // ⚠ The accessors obey the SAME rules the bare name does — `batch_only` (a future-reading
        // line is wrong however it is spelled) and the arity ceiling (`kst_signal(...)` could no
        // more express 9 parameters than `kst(...)` could). Lifting the multi-output refusal must
        // not become a side door around the other three.
        if meta.outputs.len() < 2 || meta.batch_only || meta.params.len() > MAX_INDICATOR_ARITY {
            continue;
        }
        for (idx, out) in meta.outputs.iter().enumerate() {
            let fname = line_fn_name(meta.name, out.name);
            // Same three refusals a bare name faces. None of them fires on today's registry
            // (`generated_line_names_are_bindable` proves it), so this is the future-proofing arm.
            if HOST_FN_NAMES.contains(&fname.as_str())
                || vike_indicators::registry().iter().any(|m| m.name == fname)
                || rhai::Engine::new_raw().compile(format!("{fname}()")).is_err()
            {
                continue;
            }
            register_arity_forms(engine, ctx, meta, fname, idx);
        }
    }
}

/// Registers `fname()` … `fname(p1, p2, p3)` for one indicator and one output line.
///
/// Factored out because the bare name and every per-line accessor must offer the SAME parameter
/// surface: `bollinger_mid(20, 2.0)` has to take the same arguments as `bollinger(20, 2.0)` would,
/// and two copies of this ladder would be two places for that to drift.
fn register_arity_forms(
    engine: &mut rhai::Engine,
    ctx: &SharedCtx,
    meta: &'static IndicatorMeta,
    fname: String,
    line: usize,
) {
    let arity = meta.params.len().min(MAX_INDICATOR_ARITY);
    // `bad_arg` names the function the AUTHOR called, not the registry entry behind it: someone who
    // wrote `bollinger_mid("20")` is not helped by an error about `bollinger`.
    let n0 = fname.clone();
    let c = ctx.clone();
    engine.register_fn(n0.as_str(), move || -> f64 { indicator_value(&c, meta, &[], line) });
    if arity >= 1 {
        let c = ctx.clone();
        let n: &'static str = Box::leak(fname.clone().into_boxed_str());
        engine.register_fn(n, move |a: rhai::Dynamic| -> Result<f64, Box<rhai::EvalAltResult>> {
            let x = arg(&a).ok_or_else(|| bad_arg(n, 1, &a))?;
            Ok(indicator_value(&c, meta, &[x], line))
        });
    }
    if arity >= 2 {
        let c = ctx.clone();
        let n: &'static str = Box::leak(fname.clone().into_boxed_str());
        engine.register_fn(
            n,
            move |a: rhai::Dynamic, b: rhai::Dynamic| -> Result<f64, Box<rhai::EvalAltResult>> {
                let x = arg(&a).ok_or_else(|| bad_arg(n, 1, &a))?;
                let y = arg(&b).ok_or_else(|| bad_arg(n, 2, &b))?;
                Ok(indicator_value(&c, meta, &[x, y], line))
            },
        );
    }
    if arity >= 3 {
        let c = ctx.clone();
        let n: &'static str = Box::leak(fname.into_boxed_str());
        engine.register_fn(
            n,
            move |a: rhai::Dynamic,
                  b: rhai::Dynamic,
                  d: rhai::Dynamic|
                  -> Result<f64, Box<rhai::EvalAltResult>> {
                let x = arg(&a).ok_or_else(|| bad_arg(n, 1, &a))?;
                let y = arg(&b).ok_or_else(|| bad_arg(n, 2, &b))?;
                let z = arg(&d).ok_or_else(|| bad_arg(n, 3, &d))?;
                Ok(indicator_value(&c, meta, &[x, y, z], line))
            },
        );
    }
}
