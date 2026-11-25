//! The `strategy` object TRADES as the order language does, on the real simulator.
//!
//! 1. **Each verb, from a script, is the same rule written in Rust.** Every case below is one rule
//!    spelled twice: a `vike_tactics::Rule` run in `vike_tactics::RuleRun`, and a Rhai script whose
//!    `on_bar` takes `strategy`. Both run over the same bars, and every fill (with the tag of the
//!    order that made it), every order end and every point of the equity curve must be the same,
//!    bit for bit. Each case also names what its log must hold, so a case whose orders never fill
//!    cannot pass by two empty logs agreeing.
//! 2. **The order functions' script is refused, with the spelling that replaced it.** The SMA cross
//!    the byte-parity gate wrote with `market(side, qty)` until the order functions were removed
//!    no longer compiles: it is refused at the `market` call's line, its zero-argument hook being
//!    no fault of its own. The same decision written through `strategy` keeps its bit-level proof
//!    against a Rust twin in that gate, `crates/vike-simulator/tests/parity/rhai_parity.rs`.
//! 3. **Every starter trades** through the same engine: each closes a trade over the wave.

use vike_marketdata::Bar;
use vike_model::OrderSide;
use vike_model::{Fill, OrderEvent, Strategy};
use vike_rhai::{RhaiStrategy, TEMPLATES};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};
use vike_tactics::{Rule, RuleDefaults, RuleRun, Tactics, TacticsTrail};

const SYMBOL: &str = "BTCUSDT";

/// A wave with a range inside each bar, so limits, stops and targets are reached and missed.
fn bars(n: usize) -> Vec<Bar> {
    let level = |i: f64| 100.0 + 8.0 * (i / 6.0).sin() + 0.05 * i;
    (0..n)
        .map(|i| {
            let (open, close) = (level(i as f64 - 1.0), level(i as f64));
            Bar {
                timestamp: 60_000 * (i as i64 + 1),
                open,
                high: open.max(close) + 0.6,
                low: open.min(close) - 0.6,
                close,
                volume: 10.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some(SYMBOL.into()),
            }
        })
        .collect()
}

fn params() -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 10_000.0,
        slippage: 0.0002,
        maker_fee: Some(0.0002),
        taker_fee: Some(0.0007),
        ..StrategyEngineConfig::default()
    }
}

/// What a strategy hears, in order: each fill and each order end.
struct Spy<S> {
    inner: S,
    log: Vec<String>,
}

impl<S: Strategy<OrderBrokerSimulator>> Strategy<OrderBrokerSimulator> for Spy<S> {
    fn warmup(&self) -> usize {
        self.inner.warmup()
    }
    fn on_start(&mut self, b: &mut OrderBrokerSimulator) {
        self.inner.on_start(b);
    }
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        self.inner.on_bar(b, bar);
    }
    fn on_fill(&mut self, b: &mut OrderBrokerSimulator, f: &Fill) {
        self.log.push(format!("fill {f:?}"));
        self.inner.on_fill(b, f);
    }
    fn on_order_event(&mut self, b: &mut OrderBrokerSimulator, e: &OrderEvent) {
        self.log.push(format!("end {e:?}"));
        self.inner.on_order_event(b, e);
    }
    fn on_stop(&mut self, b: &mut OrderBrokerSimulator) {
        self.inner.on_stop(b);
    }
}

/// What one run gave: the spy's log, the equity curve's bits, the trades.
#[derive(Debug, PartialEq)]
struct Outcome {
    log: Vec<String>,
    equity: Vec<u64>,
    trades: Vec<String>,
}

fn run<S: Strategy<OrderBrokerSimulator>>(inner: S) -> Outcome {
    let spy = Spy { inner, log: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYMBOL.to_string(), bars(160))], spy, params());
    let res = engine.run();
    Outcome {
        log: std::mem::take(&mut engine.strategy.log),
        equity: res.equity_curve.iter().map(|e| e.to_bits()).collect(),
        trades: res.trades.iter().map(|t| format!("{t:?}")).collect(),
    }
}

type Body = fn(&mut Tactics<'_>, i64, f64);
type After = fn(&mut Tactics<'_>);

/// A rule as a function of the bar's index and close: the Rust half of a case. The index is read
/// off the bar's time (`bars` stamps bar `i` at `60_000 * (i + 1)`), which is the simulator's
/// `index()` the script reads.
struct Written {
    header: RuleDefaults,
    body: Body,
    after: Option<After>,
}

impl Rule for Written {
    fn defaults(&self) -> RuleDefaults {
        self.header
    }
    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        (self.body)(strategy, bar.timestamp / 60_000 - 1, bar.close);
    }
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        if let Some(after) = self.after {
            after(strategy);
        }
    }
}

/// One rule, spelled in Rust and in Rhai, and what its log must hold.
struct Case {
    name: &'static str,
    header: RuleDefaults,
    rust: Body,
    after: Option<After>,
    /// The script: `{BODY}` sits inside `fn on_bar(strategy) { let i = index(); let c = close(); .. }`.
    script: &'static str,
    /// A header and an `after_fill` hook, written beside `on_bar` as they are.
    extra: &'static str,
    expect: &'static [&'static str],
}

fn script_of(case: &Case) -> String {
    format!(
        "{}\nfn on_bar(strategy) {{\n    let i = index();\n    let c = close();\n{}\n}}\n",
        case.extra, case.script
    )
}

const LONG: OrderSide = OrderSide::Long;
const SHORT: OrderSide = OrderSide::Short;

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "entry, and the opposite entry reverses",
            header: RuleDefaults::PLAIN,
            rust: |s, i, _| {
                if i == 3 {
                    s.entry("L", LONG);
                }
                if i == 20 {
                    s.entry("S", SHORT);
                }
            },
            after: None,
            script: r#"
    if i == 3 { strategy.entry("L", OrderSide::Long); }
    if i == 20 { strategy.entry("S", OrderSide::Short); }"#,
            extra: "",
            expect: &["L#", "S#"],
        },
        Case {
            name: "entry with size, stop, limit and group",
            header: RuleDefaults::PLAIN,
            rust: |s, i, c| {
                if i == 3 {
                    s.entry("L", LONG).size(2.0).stop(c + 1.0);
                }
                if i == 30 {
                    s.entry("A", SHORT).limit(c + 1.0).group("g");
                    s.entry("B", SHORT).stop(c - 1.5).limit(c - 2.0).group("g");
                }
            },
            after: None,
            script: r#"
    if i == 3 { strategy.entry("L", OrderSide::Long).size(2.0).stop(c + 1.0); }
    if i == 30 {
        strategy.entry("A", OrderSide::Short).limit(c + 1.0).group("g");
        strategy.entry("B", OrderSide::Short).stop(c - 1.5).limit(c - 2.0).group("g");
    }"#,
            extra: "",
            expect: &["fill", "L#"],
        },
        Case {
            name: "free orders",
            header: RuleDefaults::PLAIN,
            rust: |s, i, c| {
                if i == 3 {
                    s.order("F", LONG).size(1.5);
                }
                if i == 9 {
                    s.order("G", SHORT).limit(c + 0.5);
                }
                if i == 14 {
                    s.order("H", SHORT).stop(c - 0.5).size(0.5).group("h");
                }
            },
            after: None,
            script: r#"
    if i == 3 { strategy.order("F", OrderSide::Long).size(1.5); }
    if i == 9 { strategy.order("G", OrderSide::Short).limit(c + 0.5); }
    if i == 14 { strategy.order("H", OrderSide::Short).stop(c - 0.5).size(0.5).group("h"); }"#,
            extra: "",
            expect: &["F#", "G#"],
        },
        Case {
            name: "close a name, a part, at this close, and all",
            header: RuleDefaults::PLAIN,
            rust: |s, i, _| {
                if i == 3 {
                    s.entry("L", LONG).size(3.0);
                }
                if i == 10 {
                    s.close("L").size(1.0);
                }
                if i == 15 {
                    s.close("L").here();
                }
                if i == 20 {
                    s.entry("M", LONG);
                }
                if i == 30 {
                    s.close_all();
                }
            },
            after: None,
            script: r#"
    if i == 3 { strategy.entry("L", OrderSide::Long).size(3.0); }
    if i == 10 { strategy.close("L").size(1.0); }
    if i == 15 { strategy.close("L").here(); }
    if i == 20 { strategy.entry("M", OrderSide::Long); }
    if i == 30 { strategy.close_all(); }"#,
            extra: "",
            expect: &["L#", "M#"],
        },
        Case {
            name: "exits: stop and take, percents, from another's lots, a trail, a group",
            header: RuleDefaults::PLAIN,
            rust: |s, i, c| {
                if i == 3 {
                    s.entry("L", LONG).size(2.0);
                    s.exit("L").stop(c - 3.0).take(c + 4.0);
                }
                if i == 40 {
                    s.entry("S", SHORT);
                    s.exit("S").stop_pct(1.0).take_pct(2.0);
                    s.exit("P").from("S").size(0.5).take_pct(1.0).group("p");
                }
                if i == 80 {
                    s.entry("T", LONG);
                    s.exit("T").trail(TacticsTrail { after_ticks: 2.0, by_ticks: 1.0 });
                }
            },
            after: None,
            script: r#"
    if i == 3 {
        strategy.entry("L", OrderSide::Long).size(2.0);
        strategy.exit("L").stop(c - 3.0).take(c + 4.0);
    }
    if i == 40 {
        strategy.entry("S", OrderSide::Short);
        strategy.exit("S").stop_pct(1.0).take_pct(2.0);
        strategy.exit("P").from("S").size(0.5).take_pct(1.0).group("p");
    }
    if i == 80 {
        strategy.entry("T", OrderSide::Long);
        strategy.exit("T").trail(#{ after_ticks: 2.0, by_ticks: 1.0 });
    }"#,
            extra: "",
            expect: &["L#", "S#", "T#"],
        },
        Case {
            name: "cancel a name, and all",
            header: RuleDefaults::PLAIN,
            rust: |s, i, c| {
                if i == 3 {
                    s.order("F", LONG).limit(c - 20.0);
                    s.order("G", SHORT).limit(c + 20.0);
                    s.order("H", LONG).limit(c - 21.0);
                }
                if i == 6 {
                    s.cancel("F");
                }
                if i == 9 {
                    s.cancel_all();
                }
            },
            after: None,
            script: r#"
    if i == 3 {
        strategy.order("F", OrderSide::Long).limit(c - 20.0);
        strategy.order("G", OrderSide::Short).limit(c + 20.0);
        strategy.order("H", OrderSide::Long).limit(c - 21.0);
    }
    if i == 6 { strategy.cancel("F"); }
    if i == 9 { strategy.cancel_all(); }"#,
            extra: "",
            expect: &["Canceled", "F#", "G#", "H#"],
        },
        Case {
            name: "reads: position, avg_cost, equity, price, lots, snap_stop, snap_limit",
            header: RuleDefaults::PLAIN,
            rust: |s, i, c| {
                if s.position() == 0.0 && i % 9 == 0 {
                    let size = 1.0 + s.lots().len() as f64 + s.equity() / 1.0e6;
                    let stop = s.snap_stop(LONG, s.price() + 0.25);
                    s.entry("L", LONG).size(size).stop(stop);
                }
                if s.position() > 0.0 && s.lots()[0].side == LONG && c > s.avg_cost() + 1.0 {
                    let (held, limit) = (s.position(), s.snap_limit(SHORT, c + 0.2));
                    s.order("X", SHORT).size(held).limit(limit);
                }
            },
            after: None,
            script: r#"
    if strategy.position() == 0.0 && i % 9 == 0 {
        strategy.entry("L", OrderSide::Long)
            .size(1.0 + strategy.lots().len().to_float() + strategy.equity() / 1.0e6)
            .stop(strategy.snap_stop(OrderSide::Long, strategy.price() + 0.25));
    }
    if strategy.position() > 0.0 && strategy.lots()[0].side == OrderSide::Long && c > strategy.avg_cost() + 1.0 {
        strategy.order("X", OrderSide::Short).size(strategy.position()).limit(strategy.snap_limit(OrderSide::Short, c + 0.2));
    }"#,
            extra: "",
            expect: &["L#", "X#"],
        },
        Case {
            name: "the defaults' size and pyramiding",
            header: RuleDefaults { size: 2.0, pyramiding: 2, ..RuleDefaults::PLAIN },
            rust: |s, i, _| {
                if i % 3 == 0 && i < 12 {
                    s.entry("L", LONG);
                }
            },
            after: None,
            script: r#"
    if i % 3 == 0 && i < 12 { strategy.entry("L", OrderSide::Long); }"#,
            extra: "fn defaults() { #{ size: 2.0, pyramiding: 2 } }",
            expect: &["L#1", "L#2"],
        },
        Case {
            name: "after_fill: an exit issued on the entry's fill",
            header: RuleDefaults::PLAIN,
            rust: |s, i, c| {
                if i % 20 == 3 {
                    s.entry("L", LONG).limit(c - 0.3);
                }
            },
            after: Some(|s| {
                if s.position() > 0.0 {
                    let p = s.price();
                    s.exit("L").stop(p - 2.0);
                }
            }),
            script: r#"
    if i % 20 == 3 { strategy.entry("L", OrderSide::Long).limit(c - 0.3); }"#,
            extra: "fn after_fill(strategy) {
    if strategy.position() > 0.0 { strategy.exit(\"L\").stop(price() - 2.0); }
}",
            expect: &["L#"],
        },
    ]
}

#[test]
fn every_verb_from_a_script_trades_as_the_same_rule_written_in_rust() {
    for case in cases() {
        let written = Written { header: case.header, body: case.rust, after: case.after };
        let rust = run(RuleRun::new(written, SYMBOL));
        let src = script_of(&case);
        let script = RhaiStrategy::<OrderBrokerSimulator>::compile(&src)
            .unwrap_or_else(|e| panic!("{}: the script must compile: {e}\n{src}", case.name));
        let rhai = run(script);
        let log = rust.log.join("\n");
        for want in case.expect {
            assert!(log.contains(want), "{}: the run never shows {want:?}:\n{log}", case.name);
        }
        assert_eq!(rust, rhai, "{}: the script and the Rust rule part ways\n{src}", case.name);
    }
}

/// The byte-parity gate's script as the order functions wrote it.
const ORDER_FUNCTIONS: &str = r#"
fn on_bar() {
    let f = sma(5);
    let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { 1.0 } else { -1.0 };
    let delta = target - position();
    if abs(delta) > 1e-12 {
        market(if delta > 0.0 { 1 } else { -1 }, abs(delta));
    }
}
"#;

/// The order function is refused at its line with the spelling that replaced it, in its
/// zero-argument hook (which itself still runs) and beside `strategy` alike.
#[test]
fn the_order_functions_script_is_refused_with_the_spelling_that_replaced_it() {
    let refused =
        |src: &str| RhaiStrategy::<OrderBrokerSimulator>::compile(src).err().expect("refused");
    let beside = ORDER_FUNCTIONS.replace("fn on_bar()", "fn on_bar(strategy)");
    for src in [ORDER_FUNCTIONS, beside.as_str()] {
        let err = refused(src);
        assert!(
            err.summary().starts_with("market(side, qty) was removed: write strategy.order("),
            "{err}"
        );
        assert_eq!(err.line(), Some(9), "the market call's line: {err}");
    }
}

/// Every starter closes a trade on the simulator over the wave, so a starter rewritten onto the
/// order language is still one that trades, not one that merely compiles.
#[test]
fn every_starter_trades_on_the_simulator() {
    for (name, src) in TEMPLATES {
        let script = RhaiStrategy::<OrderBrokerSimulator>::compile(src)
            .unwrap_or_else(|e| panic!("{name} must compile: {e}"));
        let out = run(script);
        assert!(
            !out.trades.is_empty(),
            "{name} closed no trade over the wave:\n{}",
            out.log.join("\n")
        );
    }
}
