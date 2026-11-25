//! The starter Rhai strategies — ONE source, read by every surface that offers a starting point.
//!
//! Each is parameterized via `param()` so it drops straight into a sweep grid, and each calls only
//! names in [`crate::RHAI_INDICATORS`], the set `engine/builtin.rs`'s `register_indicators` actually binds.
//!
//! # Why they live HERE, and not where either consumer keeps its own copy
//!
//! ⚠ **They were two byte-identical hand copies until 2026-09-18** — one in
//! `crates/vike-mcp/src/lib.rs`, one in `crates/vike-studio-core/src/templates.rs` — and the
//! CLI's copy carried the reason: importing `vike-studio-core` "would drag DataFusion into this
//! deliberately DataFusion-free CLI", so the rows were duplicated "with no cross-crate drift gate —
//! the two must be edited together".
//!
//! That reason is true about `vike-studio-core` and was never true about this crate. `vike-rhai`
//! is `layer = 20` (30 when the rows moved), carries no DataFusion at all, and was ALREADY a normal
//! dependency of both consumers — `vike-cli` and `vike-studio-core` (65 and 55 then, 35 and 30
//! now) — before the move. So the duplication
//! bought nothing: the rows could always have lived in a crate both sides already link, and moving
//! them costs ZERO packages on either side. (`vike-studio-core`'s own manifest had reasoned to the
//! same place about the interpreted tier generally: "`vike-rhai` (30) would be the obvious home".)
//!
//! It is the disjunction `docs/decisions/0007-gates-not-prose.md` states — a claim that can rot is
//! either DERIVED or gated, never restated — settled on the DERIVED side. A drift gate is what you
//! write when two copies must merely AGREE and cannot be merged; here they had to be IDENTICAL,
//! which is a shared symbol's job rather than a comparison's.
//!
//! # What still lives with each consumer, deliberately
//!
//! Both execution gates stay where they are, because they prove different properties over these
//! same bytes: `crates/vike-mcp/src/tests/research_tools.rs`'s `every_shipped_template_reaches_the_broker`
//! drives a `OrderBrokerMock` and asserts an order is placed, while
//! `crates/vike-backtesthub/tests/templates_execute.rs` runs the real `run_slice` pipeline over a
//! real store and asserts a CLOSED TRADE. Neither is redundant, and a compile check is not evidence
//! for either: rhai resolves a registered name when its line RUNS, and every call these templates
//! make sits inside `fn on_bar(strategy)`, which the compile path's one top-level pass never enters.
//!
//! Every starter writes its orders through the `strategy` object (`crate::tactics`). The first three
//! were written with the order functions until 2026-11-10 and were rewritten to trade exactly as they
//! did: an always-in cross is one entry a side (an entry the other way adds the position it
//! closes), and a reversion or breakout that steps one unit at a time is a free order a side, one
//! name each, so two in one bar are two orders.
//!
//! ⚠ **The two SMA crosses are LONG-ONLY since 2026-11-12** (owner ruling: a shipped starter trades on
//! spot out of the box): above the cross a long, below it the long is closed and nothing is shorted.
//! A slice run judges a spot pair by the spot law (decision 0276), which refuses a sale past the
//! long, so the always-in reversal held its first buy and closed no trade on spot. The reversion and
//! the breakout sell only from a long and trade on spot as they are.
//!
//! ⚠ **Every starter sizes an entry as a SHARE OF THE EQUITY since 2026-11-16** (owner ruling): the
//! `equity_percent` knob, 10 by default, of `strategy.equity()` at the bar's close, never a fixed coin
//! quantity. One BTC an entry on a 10,000 spot account spent 96,000 a buy, and a step that closes a
//! position sells what it holds (`strategy.position()`). Pinned by `tests::every_template_sizes_by_a_share_of_the_equity`.

/// `(name, source)` starters, in the order every surface offers them.
pub const TEMPLATES: &[(&str, &str)] = &[
    ("SMA cross", SMA_CROSS),
    ("RSI reversion", RSI_REVERSION),
    ("Donchian breakout", BREAKOUT),
    ("SMA cross with stop and target", SMA_CROSS_EXITS),
];

const SMA_CROSS: &str = r#"
let fast = param("fast", 5.0);
let slow = param("slow", 20.0);
let equity_percent = param("equity_percent", 10.0);
fn on_bar(strategy) {
    let f = sma(fast.to_int()); let s = sma(slow.to_int());
    if s.is_nan() { return; }
    let size = strategy.equity() * equity_percent / 100.0 / close();
    if f > s { strategy.entry("Long", OrderSide::Long).size(size); } else { strategy.close("Long"); }
}
"#;

const RSI_REVERSION: &str = r#"
let len = param("len", 14.0);
let lo = param("lo", 30.0);
let hi = param("hi", 70.0);
let equity_percent = param("equity_percent", 10.0);
fn on_bar(strategy) {
    let r = rsi(len.to_int());
    if r.is_nan() { return; }
    let held = strategy.position();
    // a step closes what is held, else opens a share of the equity
    let size = if held != 0.0 { held.abs() } else { strategy.equity() * equity_percent / 100.0 / close() };
    if r < lo && held <= 0.0 { strategy.order("buy", OrderSide::Long).size(size); }
    if r > hi && held >= 0.0 { strategy.order("sell", OrderSide::Short).size(size); }
}
"#;

const BREAKOUT: &str = r#"
let len = param("len", 20.0);
let equity_percent = param("equity_percent", 10.0);
fn on_bar(strategy) {
    let h = sma(len.to_int());   // symmetric proxy. donchian is NOT bound: it is multi-output
                                 // (upper/mid/lower) and the bridge binds single-output indicators
                                 // only, so `donchian(len)` is a function-not-found at the first
                                 // call rather than a silent upper-band read.
    if h.is_nan() { return; }
    let held = strategy.position();
    // a step closes what is held, else opens a share of the equity
    let size = if held != 0.0 { held.abs() } else { strategy.equity() * equity_percent / 100.0 / close() };
    if high() > h && held <= 0.0 { strategy.order("buy", OrderSide::Long).size(size); }
    if low() < h && held >= 0.0 { strategy.order("sell", OrderSide::Short).size(size); }
}
"#;

/// The starter with exits: the long goes with a stop and a target that cancel each other, named
/// after the lots they protect; below the cross the long is closed.
const SMA_CROSS_EXITS: &str = r#"
let fast = param("fast", 5.0);
let slow = param("slow", 20.0);
let stop_pct = param("stop_pct", 2.0);
let take_pct = param("take_pct", 4.0);
let equity_percent = param("equity_percent", 10.0);
fn on_bar(strategy) {
    let f = sma(fast.to_int()); let s = sma(slow.to_int());
    if s.is_nan() { return; }
    if f > s && strategy.position() <= 0.0 {
        strategy.entry("Long", OrderSide::Long).size(strategy.equity() * equity_percent / 100.0 / close());
        strategy.exit("Long").stop_pct(stop_pct).take_pct(take_pct);
    }
    if f < s && strategy.position() > 0.0 { strategy.close("Long"); }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RhaiStrategy, discover_params};
    use vike_model::strategy::OrderBrokerMock;

    /// Each starter compiles and declares tunable knobs. ⚠ This is the WEAK half of the contract
    /// and is kept here only because it is the half this crate can see: compiling proves nothing
    /// about whether a template trades (see the module doc). The properties that matter are gated
    /// by each consumer's own execution test.
    #[test]
    fn every_template_compiles_and_is_parameterized() {
        for (name, src) in TEMPLATES {
            assert!(
                RhaiStrategy::<OrderBrokerMock>::compile(src).is_ok(),
                "template {name} must compile"
            );
            assert!(
                !discover_params(src).unwrap().is_empty(),
                "template {name} must expose param()s"
            );
        }
    }

    /// Every starter sizes what it opens as `equity_percent` (10 by default) of the equity at the bar's
    /// close, never a fixed quantity: on a flat inert book of 10,000 every order it places, every leg of a
    /// group and every amended quantity over a rising then falling wave is 1,000 worth at that close.
    #[test]
    fn every_template_sizes_by_a_share_of_the_equity() {
        use vike_marketdata::Bar;
        use vike_model::Strategy;
        for (name, src) in TEMPLATES {
            let mut strat = RhaiStrategy::<OrderBrokerMock>::compile(src).unwrap();
            let mut broker = OrderBrokerMock { equity: 10_000.0, ..OrderBrokerMock::default() };
            let mut sized = 0;
            for i in 0..80_i64 {
                let k = i % 40;
                let close = 100.0 + (if k < 20 { k } else { 40 - k }) as f64;
                let bar = Bar::ohlcv(60_000 * (i + 1), close, close + 1.0, close - 1.0, close, 1.0);
                broker.price = close;
                let seen = (broker.placed.len(), broker.groups.len(), broker.modified.len());
                strat.on_bar(&mut broker, &bar);
                // the script's own expression, so the figure is the same bits
                let want = 10_000.0 * 10.0 / 100.0 / close;
                let placed = broker.placed[seen.0..].iter().map(|p| p.3);
                let legs = broker.groups[seen.1..].iter().flatten().map(|(_, leg)| leg.qty);
                let amended = broker.modified[seen.2..].iter().filter_map(|m| m.1);
                for qty in placed.chain(legs).chain(amended) {
                    assert!(
                        (qty - want).abs() <= 1e-12 * want,
                        "{name} sized {qty} at {close}, not 10% of the equity ({want})"
                    );
                    sized += 1;
                }
            }
            assert!(sized > 0, "{name} placed nothing over the wave");
        }
    }
}
