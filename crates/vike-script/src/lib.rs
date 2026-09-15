//! Rhai-scripted strategies — run a Rhai script as a `vike_model::Strategy<B>` (Studio SP1).
//! Ports the Python studio's `importlib`-of-source bridge to Rust; the Rhai (ms-cadence) tier
//! behind the same `Strategy<B>` seam compiled Rust HFT strategies use.
//!
//! Script contract, two halves:
//!
//! 1. **What a script may call.** [`RHAI_INDICATORS`] is the host-bound indicator set, DERIVED
//!    from `vike_indicators::registry()` rather than hand-listed: almost the whole catalog, minus
//!    the entries [`unbound_reason`] rejects because a scalar binding of them would return a
//!    silently WRONG number (multi-output indicators, whose one reachable line is the `upper` band
//!    on `bollinger`/`donchian`/`keltner`; the future-reading `batch_only` three; and `var`, which
//!    a Rhai script cannot name at all). Each bound indicator has one form per argument count from
//!    zero (all registry defaults) up to its own parameter count. `unbound_reason` says why a
//!    missing one is missing.
//! 2. **How it must call it.** An indicator function must be called UNCONDITIONALLY on every
//!    `on_bar` invocation to keep its streaming state correct — a conditionally-referenced
//!    indicator (e.g. behind an `if`) silently skips whichever bars it isn't called on, desyncing
//!    it from the bar series (see `engine::indicator_value`'s fed-once-per-bar cache).
//!
//! Both halves are about CALLING an indicator. [`RhaiIndicator`] (`indicator.rs`) is the other
//! seam: DEFINING one. A user file under `user_data/indicators/<name>.rhai` compiles to a real
//! `vike_indicators::Indicator` — fed-once-per-bar state, `vectorize` parity by construction, and
//! no access to the order verbs — and [`RhaiStrategy::compile_with_indicators`] binds it beside
//! the built-ins. The two halves above then apply to it unchanged, INCLUDING multi-output: a file
//! declaring `fn outputs() { ["upper", "mid", "lower"] }` gets its lines through the same
//! [`line_fn_name`] the built-ins do ([`user_line_accessors`] is the [`line_accessors`] twin), under
//! the same namesake rule for whether the bare name binds at all. A file that declares no
//! `outputs()` is single-output and unchanged.
//!
//! the built-ins. The two halves above then apply to it unchanged.
//!
//! Because it IS a real `Indicator`, the same compiled prototype also PLOTS. [`user_meta`] wraps
//! one as a `&'static vike_indicators::IndicatorMeta` whose `factory` clones it, which is what
//! `vike_chart::indicators::install_user_studies` mounts into the chart's ƒx picker. Nothing about
//! the script contract changes for that — a file written for a strategy is already a chart study —
//! except one optional addition it had no use for before: `fn overlay()`, the author's answer to
//! "price pane, or my own?".

mod ctx;
mod engine;
mod indicator;
mod load;
mod strategy;

pub use engine::{
    RHAI_INDICATORS, install_user_indicators, installed_user_bare_call,
    installed_user_indicator_params, installed_user_indicators, installed_user_line_accessors,
    is_callable, line_accessors, line_fn_name, unbound_reason, user_indicator_conflict,
    user_line_accessors, user_line_conflict,
};
pub use indicator::{RhaiIndicator, compile_indicator, compile_indicator_with, user_meta};
pub use load::{
    IndicatorDiagnostic, IndicatorLoadReport, UserIndicator, load_and_install_user_indicators,
    load_user_indicators,
};
// Re-exported because a [`RhaiIndicator`]'s useful methods all come from this trait: a consumer
// holding one would otherwise need its own `vike-indicators` dependency to call `on_bar`.
pub use strategy::{RhaiStrategy, ScriptError, discover_params};
pub use vike_indicators::Indicator;

#[cfg(test)]
mod smoke {
    #[test]
    fn rhai_engine_evaluates() {
        let engine = rhai::Engine::new();
        let out: i64 = engine.eval("40 + 2").unwrap();
        assert_eq!(out, 42);
    }
}
