use super::*;
use crate::test_support::{OrderBrokerMock, drive};

fn compile_err(src: &str) -> ScriptError {
    RhaiStrategy::<OrderBrokerMock>::compile(src).err().expect("the script must not compile")
}

/// A parse error carries its line TYPED, and its words without the position suffix, so an editor
/// never parses the rendered text; `Display` is still rhai's full rendering.
#[test]
fn a_parse_error_carries_its_line_and_its_bare_words() {
    let e = compile_err("fn on_bar() {\n  let x = ;\n}");
    assert_eq!(e.line(), Some(2), "{e}");
    assert!(!e.summary().contains("(line"), "{}", e.summary());
    assert!(e.to_string().starts_with(e.summary()), "{e}");
    assert!(e.to_string().contains("(line 2,"), "{e}");
}

/// A top-level RUNTIME failure (the compile runs the top level once) carries the line too.
#[test]
fn a_top_level_runtime_error_carries_its_line() {
    let e = compile_err("let a = 1;\nlet b = no_such_fn(a);\nfn on_bar() {}");
    assert_eq!(e.line(), Some(2), "{e}");
    assert!(!e.summary().contains("(line"), "{}", e.summary());
}

/// A failure this crate words itself has no position, and says so rather than inventing one.
#[test]
fn a_plain_error_has_no_line() {
    let e = ScriptError::plain("boom".to_string());
    assert_eq!((e.line(), e.summary(), e.to_string().as_str()), (None, "boom", "boom"));
}

/// The zero-argument hooks run as they always did, each at its own event (a throw is the witness:
/// it counts as that hook's error), on the global reads of the bar; and they place nothing, since
/// the order functions are gone.
#[test]
fn the_zero_argument_hooks_still_run_each_at_its_event_and_place_nothing() {
    let src = r#"fn on_start() { throw "start"; }
                 fn on_bar() { if close() == 100.0 && index() == 0 { throw "bar"; } }
                 fn on_tick() { throw "tick"; }
                 fn on_stop() { throw "stop"; }"#;
    let mut s =
        RhaiStrategy::<OrderBrokerMock>::compile(src).expect("the zero-argument hooks compile");
    let errors =
        |s: &RhaiStrategy<OrderBrokerMock>| s.script.as_ref().expect("no run: no strategy").errors;
    let mut b = OrderBrokerMock::default();
    s.on_start(&mut b);
    assert_eq!(errors(&s), 1, "on_start ran");
    drive(&mut s, &mut b, &[100.0]);
    assert_eq!(errors(&s), 2, "on_bar ran, and read this bar's close and index");
    let q = vike_marketdata::TickQuote {
        timestamp: 1_000,
        local_timestamp: 0,
        bid: 99.0,
        ask: 101.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    s.on_quote_tick(&mut b, &q);
    assert_eq!(errors(&s), 3, "on_tick ran on the quote");
    s.on_stop(&mut b);
    assert_eq!(errors(&s), 4, "on_stop ran");
    assert!(b.seam.is_empty(), "{:?}", b.seam);

    // a hook that returns normally resets the count: the fail-safe is the one it always was
    let mut quiet =
        RhaiStrategy::<OrderBrokerMock>::compile("fn on_bar() { let x = sma(3); }").unwrap();
    drive(&mut quiet, &mut b, &[100.0, 101.0]);
    assert_eq!(errors(&quiet), 0, "a zero-argument on_bar that computes runs clean");
}

/// Each removed order function is refused at compile by its name, AT THE CALL'S LINE, wherever it
/// sits: in a zero-argument hook (where every old script called it), beside `strategy`, in a
/// function the script wrote, or at the top level (which `compile` would otherwise run). `discover_params`, the editor's compile dot, refuses it alike.
#[test]
fn a_removed_function_is_refused_by_name_at_its_line() {
    let calls = [
        ("buy", "buy(1.0)", "buy(qty) was removed"),
        ("sell", "sell(1.0)", "sell(qty) was removed"),
        ("market", "market(1, 1.0)", "market(side, qty) was removed"),
        ("limit", "limit(1, 1.0, 99.0)", "limit(side, qty, price) was removed"),
    ];
    for (name, call, words) in calls {
        let (_, why) = REMOVED_FUNCTIONS.iter().find(|(n, _)| *n == name).expect("listed");
        let places = [
            format!("fn on_bar() {{\n    let x = 1;\n    {call};\n}}"),
            format!("fn on_bar(strategy) {{\n    let x = 1;\n    {call};\n}}"),
            format!(
                "fn helper() {{\n    let x = close();\n    if x > 0.0 {{ {call}; }}\n}}\n\
                 fn on_bar(strategy) {{ helper(); }}"
            ),
            format!("let x = 1;\nlet y = 2;\n{call};\nfn on_bar(strategy) {{}}"),
        ];
        for src in places {
            let e = compile_err(&src);
            assert!(e.summary().starts_with(words), "{name}: {e}");
            assert_eq!((e.summary(), e.line()), (*why, Some(3)), "{name}: {e}\n{src}");
            assert!(e.to_string().starts_with(*why) && e.to_string().contains("(line 3,"), "{e}");
            assert_eq!(discover_params(&src).unwrap_err(), e, "discovery refuses {name} alike");
        }
    }
}

/// The refusal reads the GLOBAL function only: a builder's `.limit(price)` is the order language's
/// own method, and a function the script defines under a removed name is the script's own. Both
/// compile and trade.
#[test]
fn a_builders_limit_and_a_function_the_script_defines_are_not_refused() {
    let place = |src: &str| {
        let mut s = RhaiStrategy::<OrderBrokerMock>::compile(src)
            .unwrap_or_else(|e| panic!("must compile: {e}\n{src}"));
        let mut b = OrderBrokerMock::default();
        drive(&mut s, &mut b, &[100.0]);
        b.orders()
    };
    let limit = r#"fn on_bar(strategy) { strategy.order("o", OrderSide::Long).limit(99.0); }"#;
    assert_eq!(place(limit), [(1, 1.0)], "a builder's .limit is not limit()");
    let own = r#"fn buy(q) { q * 2.0 }
                 fn on_bar(strategy) { strategy.order("o", OrderSide::Long).size(buy(1.5)); }"#;
    assert_eq!(place(own), [(1, 3.0)], "the script's own fn buy runs");
}

/// `on_stop(strategy)` RUNS, on the language's reads as the run ends (a throw is the witness: it
/// counts as the hook's error), and a verb there is an error that sends nothing. A run that never
/// met a bar has nothing to read, and its `on_stop` runs nothing.
#[test]
fn on_stop_with_strategy_runs_on_the_languages_reads() {
    let run = |body: &str| {
        let src = format!("fn on_bar(strategy) {{}} fn on_stop(strategy) {{ {body} }}");
        let mut s = RhaiStrategy::<OrderBrokerMock>::compile(&src).unwrap();
        let mut b = OrderBrokerMock::default();
        drive(&mut s, &mut b, &[100.0, 101.0]);
        s.on_stop(&mut b);
        (s.run.as_ref().expect("the run starts at the first bar").rule().errors, b.seam.len())
    };
    let reads = "if strategy.price() != 101.0 || strategy.equity() != 10000.0 { throw \"stale\"; }";
    assert_eq!(run(reads), (0, 0), "the reads are the broker's as the run ends");
    assert_eq!(run("throw \"ran\";"), (1, 0), "the hook runs: its error is counted");
    assert_eq!(run("strategy.close_all();"), (1, 0), "a verb is refused and sends nothing");

    let src = "fn on_bar(strategy) {} fn on_stop(strategy) { throw \"ran\"; }";
    let mut never = RhaiStrategy::<OrderBrokerMock>::compile(src).unwrap();
    never.on_stop(&mut OrderBrokerMock::default());
    assert!(never.run.is_none() && never.script.as_ref().is_some_and(|s| s.errors == 0));
}
