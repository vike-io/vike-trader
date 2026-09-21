//! The starter Rhai strategies — ONE source, read by every surface that offers a starting point.
//!
//! Each is parameterized via `param()` so it drops straight into a sweep grid, and each calls only
//! names in [`crate::RHAI_INDICATORS`], the set `engine.rs`'s `register_indicators` actually binds.
//!
//! # Why they live HERE, and not where either consumer keeps its own copy
//!
//! ⚠ **They were two byte-identical hand copies until 2026-09-18** — one in
//! `crates/vike-cli/src/cmd/mcp.rs`, one in `crates/vike-studio-core/src/templates.rs` — and the
//! CLI's copy carried the reason: importing `vike-studio-core` "would drag DataFusion into this
//! deliberately DataFusion-free CLI", so the rows were duplicated "with no cross-crate drift gate —
//! the two must be edited together".
//!
//! That reason is true about `vike-studio-core` and was never true about this crate. `vike-script`
//! is `layer = 30`, carries no DataFusion at all, and was ALREADY a normal dependency of both
//! consumers — `vike-cli` (65) and `vike-studio-core` (55) — before the move. So the duplication
//! bought nothing: the rows could always have lived in a crate both sides already link, and moving
//! them costs ZERO packages on either side. (`vike-studio-core`'s own manifest had reasoned to the
//! same place about the interpreted tier generally: "`vike-script` (30) would be the obvious home".)
//!
//! It is the disjunction `docs/decisions/0007-gates-not-prose.md` states — a claim that can rot is
//! either DERIVED or gated, never restated — settled on the DERIVED side. A drift gate is what you
//! write when two copies must merely AGREE and cannot be merged; here they had to be IDENTICAL,
//! which is a shared symbol's job rather than a comparison's.
//!
//! # What still lives with each consumer, deliberately
//!
//! Both execution gates stay where they are, because they prove different properties over these
//! same bytes: `crates/vike-cli/src/cmd/mcp.rs`'s `every_shipped_template_reaches_the_broker`
//! drives a `MockBroker` and asserts an order is submitted, while
//! `crates/vike-studio-core/tests/templates_execute.rs` runs the real `run_slice` pipeline over a
//! real store and asserts a CLOSED TRADE. Neither is redundant, and a compile check is not evidence
//! for either: rhai resolves a registered name when its line RUNS, and every call these templates
//! make sits inside `fn on_bar()`, which the compile path's one top-level pass never enters.

/// `(name, source)` starters, in the order every surface offers them.
pub const TEMPLATES: &[(&str, &str)] =
    &[("SMA cross", SMA_CROSS), ("RSI reversion", RSI_REVERSION), ("Donchian breakout", BREAKOUT)];

const SMA_CROSS: &str = r#"
let fast = param("fast", 5.0);
let slow = param("slow", 20.0);
fn on_bar() {
    let f = sma(fast.to_int()); let s = sma(slow.to_int());
    if s.is_nan() { return; }
    let target = if f > s { 1.0 } else { -1.0 };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

const RSI_REVERSION: &str = r#"
let len = param("len", 14.0);
let lo = param("lo", 30.0);
let hi = param("hi", 70.0);
fn on_bar() {
    let r = rsi(len.to_int());
    if r.is_nan() { return; }
    if r < lo && position() <= 0.0 { market(1, 1.0); }
    if r > hi && position() >= 0.0 { market(-1, 1.0); }
}
"#;

const BREAKOUT: &str = r#"
let len = param("len", 20.0);
fn on_bar() {
    let h = sma(len.to_int());   // symmetric proxy. donchian is NOT bound: it is multi-output
                                 // (upper/mid/lower) and the bridge binds single-output indicators
                                 // only, so `donchian(len)` is a function-not-found at the first
                                 // call rather than a silent upper-band read.
    if h.is_nan() { return; }
    if high() > h && position() <= 0.0 { market(1, 1.0); }
    if low() < h && position() >= 0.0 { market(-1, 1.0); }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RhaiStrategy, discover_params};
    use vike_model::strategy::MockBroker;

    /// Each starter compiles and declares tunable knobs. ⚠ This is the WEAK half of the contract
    /// and is kept here only because it is the half this crate can see: compiling proves nothing
    /// about whether a template trades (see the module doc). The properties that matter are gated
    /// by each consumer's own execution test.
    #[test]
    fn every_template_compiles_and_is_parameterized() {
        for (name, src) in TEMPLATES {
            assert!(
                RhaiStrategy::<MockBroker>::compile(src).is_ok(),
                "template {name} must compile"
            );
            assert!(
                !discover_params(src).unwrap().is_empty(),
                "template {name} must expose param()s"
            );
        }
    }
}
