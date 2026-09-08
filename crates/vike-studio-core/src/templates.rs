//! Starter Rhai strategies for the Studio, each parameterized via `param()` so they drop straight
//! into the sweep grid. Rendered read-only into the editor from the templates dropdown.

/// `(name, source)` starters. Every entry is compile-checked + param-checked in tests.
pub fn templates() -> &'static [(&'static str, &'static str)] {
    &[("SMA cross", SMA_CROSS), ("RSI reversion", RSI_REVERSION), ("Donchian breakout", BREAKOUT)]
}

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
    use vike_backtest::SimBroker;
    use vike_script::{RhaiStrategy, discover_params};

    #[test]
    fn every_template_compiles_and_is_parameterized() {
        for (name, src) in templates() {
            assert!(
                RhaiStrategy::<SimBroker>::compile(src).is_ok(),
                "template {name} must compile"
            );
            assert!(
                !discover_params(src).unwrap().is_empty(),
                "template {name} must expose param()s"
            );
        }
    }
}
