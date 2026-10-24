//! Rhai engine wiring: `build_engine` constructs a resource-limited `rhai::Engine` and registers
//! the order-verb host functions (`buy`/`sell`/`limit`/`market`) that a strategy script calls.
//! Each closure captures a cloned `SharedCtx` (never a borrowed `&mut Broker`) and records an
//! `Intent` into `ScriptCtx::intents` — no broker/strategy wiring happens here; `strategy.rs`
//! drains the recorded intents into real orders each bar.
//!
//! It also owns the INDICATOR BRIDGE and, with it, the answer to "which of
//! `vike_indicators::registry()` can a script actually call". That answer is DERIVED from the
//! registry, never hand-listed: [`RHAI_INDICATORS`](crate::engine::bindable::RHAI_INDICATORS) is `registry()` minus [`unbound_reason`](crate::engine::bindable::unbound_reason)'s
//! exclusions, and [`register_indicators`](crate::engine::builtin::register_indicators) iterates the same predicate, so the advertised set and
//! the bound set cannot drift apart. The exclusions exist because a bound name must be
//! CORRECT, not merely constructible — see `exclusion` for the four rules and why each one is a
//! silent-wrong-answer hazard rather than a matter of taste.

pub(crate) mod bindable;
pub(crate) mod builtin;
pub(crate) mod host;
pub(crate) mod installed;
pub(crate) mod user;

use crate::engine::bindable::exclusion;
use std::sync::LazyLock;

#[cfg(test)]
use crate::engine::bindable::{line_fn_name, sanitize_line};
#[cfg(test)]
use crate::engine::builtin::line_accessors;
#[cfg(test)]
use crate::engine::user::user_indicator_conflict;

/// The `tracing` target every script diagnostic lands on, whichever of this crate's two engines
/// produced it. One name so an operator can turn the whole surface up or down with a single
/// `RUST_LOG=vike_script=debug`, and so a `target:` typo cannot silently orphan one tier.
pub(crate) const SCRIPT_LOG_TARGET: &str = "vike_script";

/// The registry entries the Rhai host deliberately leaves unbound, name -> reason. Built ONCE per
/// process (this is a `LazyLock`, not per-`build_engine` work), because the callable-in-Rhai half of
/// [`exclusion`] is answered by compiling `<name>()` on a throwaway `rhai::Engine::new_raw()` — a
/// raw engine parses identically to the one `build_engine` returns (reserved words are a tokenizer
/// property; `build_engine` disables no symbols and defines no custom keywords) and skips the
/// standard packages, and rhai's default `OptimizationLevel::Simple` never eagerly evaluates a call,
/// so the probe measures the GRAMMAR and nothing else.
static UNBOUND: LazyLock<indexmap::IndexMap<&'static str, &'static str>> = LazyLock::new(|| {
    let probe = rhai::Engine::new_raw();
    vike_indicators::registry()
        .iter()
        .filter_map(|m| {
            let callable = probe.compile(format!("{}()", m.name)).is_ok();
            let line0 = m.outputs.first().map(|o| o.name);
            exclusion(m.name, m.batch_only, line0, m.outputs.len(), m.params.len(), callable)
                .map(|why| (m.name, why))
        })
        .collect()
});

/// A `(stem, line)` pair whose generated accessor IS a built-in indicator's own name — DERIVED from
/// the registry so it cannot rot when an indicator is renamed, and PROVEN rather than assumed (the
/// reconstruction is asserted by the callers that use it).
///
/// Every registry name containing a `_` is a candidate, because [`line_fn_name`] joins with exactly
/// that separator: `smi_ergodic` is what a file `smi.rhai` declaring a line `ergodic` would spell.
/// The one taken is the first whose STEM is itself a free user-indicator name, so a LOADER test
/// reaches `compile_indicator` instead of being refused for the file's own name first.
#[cfg(test)]
pub(crate) fn registry_name_a_user_line_could_spell() -> (&'static str, &'static str) {
    vike_indicators::registry()
        .iter()
        .filter_map(|m| m.name.split_once('_'))
        .find(|&(stem, line)| {
            // The accessor this pair GENERATES must be the registry name it was split out of —
            // sanitisation is the identity on a registry name's tail, and this checks that rather
            // than trusting it, so the witness cannot silently stop being one.
            vike_indicators::get(&line_fn_name(stem, line)).is_some()
                && user_indicator_conflict(stem).is_none()
        })
        .expect("a registry name with a `_`, whose stem is a free user-indicator name")
}

/// The first BUILT-IN per-line accessor this build registers, as `(indicator, line, accessor)` —
/// derived from [`line_accessors`] over the whole registry, so no test has to name one.
///
/// ⚠ Lines that sanitise to NOTHING are skipped. [`line_accessors`] does not filter them (they
/// still spell a callable `<name>_`), but [`user_line_conflict`] answers such a line under its FIRST
/// rule, and a caller asking this for a per-line-accessor witness would get the wrong refusal.
#[cfg(test)]
pub(crate) fn a_builtin_line_accessor() -> (&'static str, &'static str, String) {
    vike_indicators::registry()
        .iter()
        .find_map(|m| {
            line_accessors(m.name)
                .into_iter()
                .find(|(l, _)| !sanitize_line(l).is_empty())
                .map(|(l, f)| (m.name, l, f))
        })
        .expect("this build registers at least one per-line accessor")
}

#[cfg(test)]
mod tests;
