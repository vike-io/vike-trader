//! The `strategy` object's own rules: what a script on the order language sends, what it is refused,
//! and what its defaults and trail maps accept. How its verbs TRADE, against the same rule written in
//! Rust, is `tests/strategy_object.rs` (on the simulator).

use super::*;
use crate::{RhaiStrategy, TEMPLATES, discover_params};
use rhai::Dynamic;
use vike_marketdata::{Bar, TickQuote};
use vike_model::strategy::OrderBrokerMock;
use vike_model::{OrderPlacement, Strategy};

/// Mounts `src` and runs it over `n` rising bars on vike-model's recording mock.
fn drive(src: &str, n: i64) -> OrderBrokerMock {
    let mut strat = RhaiStrategy::<OrderBrokerMock>::compile(src).expect("compiles");
    drive_on(&mut strat, n)
}

/// [`drive`]'s bars, on a strategy the test keeps.
fn drive_on(strat: &mut RhaiStrategy<OrderBrokerMock>, n: i64) -> OrderBrokerMock {
    let mut broker = OrderBrokerMock::default();
    for i in 0..n {
        let c = 100.0 + i as f64;
        let bar = Bar::ohlcv(60_000 * (i + 1), c, c + 1.0, c - 1.0, c, 1.0);
        broker.price = c;
        strat.on_bar(&mut broker, &bar);
    }
    broker
}

#[test]
fn a_verb_reaches_the_seam_with_its_options() {
    let src =
        r#"fn on_bar(strategy) { strategy.order("o", OrderSide::Short).size(2.5).limit(101.0); }"#;
    let b = drive(src, 1);
    assert_eq!(b.placed.len(), 1, "{:?}", b.placed);
    let (_, tag, side, qty, order, reduce_only) = &b.placed[0];
    assert!(tag.starts_with("o#"), "a tag is <name>#<n>: {tag}");
    assert_eq!(
        (*side, *qty, *order, *reduce_only),
        (-1, 2.5, OrderPlacement::Limit { price: 101.0 }, false)
    );
}

#[test]
fn chase_pegs_an_order_and_iceberg_shows_a_slice_and_a_slice_of_zero_is_refused_by_name() {
    let src = r#"fn on_bar(strategy) {
        strategy.entry("c", OrderSide::Long).chase();
        strategy.order("i", OrderSide::Short).size(3.0).limit(101.0).iceberg(1.0);
        strategy.order("h", OrderSide::Short).limit(101.0).iceberg(0.0);
    }"#;
    let b = drive(src, 1);
    let kinds: Vec<_> = b.placed.iter().map(|p| (p.2, p.3, p.4)).collect();
    let best = vike_model::OrderPeg { anchor: vike_model::OrderPegAnchor::Best, offset: 0.0 };
    assert_eq!(
        kinds,
        [
            (1, 1.0, OrderPlacement::Pegged { peg: best }),
            (-1, 3.0, OrderPlacement::Iceberg { price: 101.0, display: 1.0 })
        ]
    );
    // the hidden order is the language's refusal, the same one a Rust rule meets: sent nowhere,
    // denied to the broker in words, the rest of the hook's orders untouched
    assert_eq!(b.denied.len(), 1, "{:?}", b.denied);
    let (tag, why) = &b.denied[0];
    assert!(
        tag.starts_with("h#") && why.contains(".iceberg(0)") && why.contains("above zero"),
        "{why}"
    );
}

/// An order function beside the `strategy` object is refused at compile, at its line, where it
/// used to fail the hook on every bar and send nothing.
#[test]
fn an_order_function_beside_strategy_is_refused_at_compile() {
    let src =
        "fn on_bar(strategy) {\n    strategy.order(\"o\", OrderSide::Long);\n    buy(1.0);\n}";
    let err = RhaiStrategy::<OrderBrokerMock>::compile(src).err().expect("refused");
    assert!(err.summary().starts_with("buy(qty) was removed"), "{err}");
    assert_eq!(err.line(), Some(3), "{err}");
    // the control: the same verb alone is sent
    let alone = drive(r#"fn on_bar(strategy) { strategy.order("o", OrderSide::Long); }"#, 1);
    assert_eq!(alone.placed.len(), 1);
}

#[test]
fn an_option_chained_after_a_later_verb_is_an_error_not_a_quiet_edit() {
    let src = r#"fn on_bar(strategy) {
        let e = strategy.order("a", OrderSide::Long);
        strategy.order("b", OrderSide::Long);
        e.size(2.0);
    }"#;
    assert!(drive(src, 1).placed.is_empty(), "the hook errored, so it sends nothing");
}

#[test]
fn a_bare_exit_is_an_error_not_rhais_exit() {
    let src = r#"fn on_bar(strategy) { strategy.order("o", OrderSide::Long); strategy.exit(); }"#;
    assert!(drive(src, 1).placed.is_empty(), "rhai's own exit() would have ended the hook quietly");
}

#[test]
fn on_bar_is_defined_once() {
    let err = RhaiStrategy::<OrderBrokerMock>::compile("fn on_bar() {} fn on_bar(strategy) {}")
        .err()
        .expect("refused");
    assert!(err.summary().contains("on_bar once"), "{err}");
}

/// Only a parameter NAMED `strategy` puts a script on the language, so a script with some other
/// one-parameter `on_bar` (never run before this wave) behaves as it did: its hook is not run, so
/// the order it would place through that parameter, were it handed the object, never lands.
#[test]
fn a_one_parameter_on_bar_of_another_name_is_not_the_language() {
    let b = drive(r#"fn on_bar(bar) { bar.order("o", OrderSide::Long); }"#, 2);
    assert!(b.placed.is_empty(), "the hook is not run, as before");
}

#[test]
fn the_defaults_state_size_and_pyramiding_and_nothing_else() {
    let mut map = rhai::Map::new();
    map.insert("size".into(), Dynamic::from_int(2));
    map.insert("pyramiding".into(), Dynamic::from_int(3));
    let h = defaults_of(&map).unwrap();
    assert_eq!((h.size, h.pyramiding), (2.0, 3));
    map.insert("pyramiding".into(), Dynamic::from_float(1.5));
    assert!(defaults_of(&map).is_err(), "a fractional pyramiding");
    map.remove("pyramiding");
    map.insert("slippage".into(), Dynamic::from_float(1.0));
    assert_eq!(
        defaults_of(&map).unwrap_err(),
        "defaults: `slippage` is not one of its fields (size, pyramiding)"
    );

    let src = r#"fn defaults() { #{ size: 2.0 } }
                 fn on_bar(strategy) { strategy.order("o", OrderSide::Long); }"#;
    assert_eq!(drive(src, 1).placed[0].3, 2.0, "the defaults' size is the order's");
    let bad = r#"fn defaults() { #{ sise: 2.0 } } fn on_bar(strategy) {}"#;
    assert!(
        RhaiStrategy::<OrderBrokerMock>::compile(bad).is_err(),
        "a misspelt field is a compile error"
    );
}

/// The defaults hook's OLD name is refused at compile with the new one, never read and never
/// ignored (owner 2026-11-13). The old name is built from parts: plan 2b's word script rewrites
/// the joined spelling in every string it reads.
#[test]
fn the_old_defaults_hook_is_refused_naming_the_new_one() {
    let old = "header";
    let src = format!(
        "fn {old}() {{ #{{ size: 2.0, pyramiding: 2 }} }}\n\
         fn on_bar(strategy) {{ strategy.order(\"o\", OrderSide::Long); }}"
    );
    let Err(e) = RhaiStrategy::<OrderBrokerMock>::compile(&src) else {
        panic!("a script defining the old hook must not compile");
    };
    assert_eq!(
        e.summary(),
        format!(
            "`fn {old}()` is now `fn defaults()`: the same map (size, pyramiding) under its new name"
        )
    );
    let dot = discover_params(&src).expect_err("the editor's compile dot refuses it too");
    assert_eq!(dot.summary(), e.summary());
    // the CONTROL: the same script under the new name compiles and keeps its size
    let renamed = src.replacen(&format!("fn {old}()"), "fn defaults()", 1);
    assert_eq!(drive(&renamed, 1).placed[0].3, 2.0, "the size the renamed hook states");
    assert!(discover_params(&renamed).is_ok(), "and its compile dot is clean");
}

#[test]
fn a_trail_names_both_distances() {
    let mut map = rhai::Map::new();
    map.insert("after_ticks".into(), Dynamic::from_int(4));
    assert!(trail_of(&map).is_err());
    map.insert("by_ticks".into(), Dynamic::from_float(2.0));
    assert_eq!(trail_of(&map).unwrap(), TacticsTrail { after_ticks: 4.0, by_ticks: 2.0 });
    let bad = r#"fn on_bar(strategy) {
        strategy.order("o", OrderSide::Long);
        strategy.exit("L").trail(#{ after_ticks: 4 });
    }"#;
    assert!(drive(bad, 1).placed.is_empty(), "a half-stated trail errors the hook");
}

/// The editor's compile dot and the sweep UI read a script with no broker (`discover_params`).
#[test]
fn discovery_reads_the_starter_on_the_language_without_a_broker() {
    let (_, starter) = TEMPLATES
        .iter()
        .find(|(name, _)| *name == "SMA cross with stop and target")
        .expect("the starter with exits");
    let names: Vec<String> = discover_params(starter).unwrap().into_iter().map(|p| p.0).collect();
    assert_eq!(names, ["fast", "slow", "stop_pct", "take_pct"]);
    let b = drive(starter, 40);
    assert!(!b.groups.is_empty() || !b.placed.is_empty(), "rising bars cross, so it trades");
}

/// Every [`STRATEGY_FN_NAMES`] entry is bound: one hook calls each, and a stale entry would be a
/// function-not-found error, so the hook would send nothing at all.
#[test]
fn every_strategy_fn_name_is_callable() {
    // short chains: a debug build's rhai caps an expression's depth inside a function (16)
    let src = r#"fn on_bar(strategy) {
        strategy.position(); strategy.avg_cost(); strategy.equity(); strategy.price();
        strategy.lots(); strategy.snap_stop(OrderSide::Long, 99.0); strategy.snap_limit(OrderSide::Long, 99.0);
        let i = strategy.input("1d");
        i.close(); i.open(); i.high(); i.low(); i.volume(); i.time(); i.bars(2); i.since(0);
        i.count(); i.bar(0);
        strategy.closed_trades(); strategy.closed_net(); strategy.closed_count(); strategy.closed_trade(0);
        strategy.entry("e", OrderSide::Long).size(1.0).stop(99.0);
        strategy.entry("f", OrderSide::Long).limit(98.0).group("g");
        strategy.order("o", OrderSide::Short).size(1.0).stop(101.0);
        strategy.order("p", OrderSide::Short).limit(102.0).group("h");
        strategy.entry("c", OrderSide::Long).chase();
        strategy.order("q", OrderSide::Short).limit(102.0).iceberg(0.5);
        strategy.exit("x").from("e").size(1.0);
        strategy.exit("w").stop(90.0).take(110.0);
        strategy.exit("y").stop_pct(1.0);
        strategy.exit("z").take_pct(2.0).group("g");
        strategy.exit("t").trail(#{ after_ticks: 1.0, by_ticks: 1.0 });
        strategy.close("e").size(1.0).here();
        strategy.close_all();
        strategy.cancel("o");
        strategy.cancel_all();
        strategy.position_of(""); strategy.price_of("");
        strategy.on("").order("last", OrderSide::Long).size(1.0);
    }"#;
    for name in STRATEGY_FN_NAMES {
        assert!(src.contains(&format!(".{name}(")), "the probe does not call `{name}`");
    }
    let b = drive(src, 1);
    assert!(b.placed.iter().any(|p| p.1.starts_with("last#")), "a call errored: {:?}", b.placed);
}

/// `fn trades()` declares the symbols a script trades beside its own: `strategy.on(symbol)` sends
/// the verbs after it on that symbol, tagged `<symbol>/<name>#<n>`, until the hook ends. A symbol
/// it does not declare is an error of the hook, by name, as is a `fn trades()` it cannot read.
#[test]
fn on_trades_a_symbol_fn_trades_declares_and_refuses_one_it_does_not() {
    let src = r#"fn trades() { ["Y"] }
                 fn on_bar(strategy) {
                     strategy.order("a", OrderSide::Long);
                     strategy.on("Y").order("b", OrderSide::Short).limit(101.0);
                     strategy.order("c", OrderSide::Long);
                 }"#;
    let placed: Vec<_> = drive(src, 1).placed.into_iter().map(|p| (p.0, p.1, p.4)).collect();
    let on = |symbol: &str, tag: &str, order| (symbol.to_string(), tag.to_string(), order);
    assert_eq!(
        placed,
        [
            on("", "a#1", OrderPlacement::Market),
            on("Y", "Y/b#1", OrderPlacement::Limit { price: 101.0 }),
            on("Y", "Y/c#2", OrderPlacement::Market),
        ],
        "the run's own symbol is the first bar's (none here); Y's orders are numbered on Y"
    );

    let bad = r#"fn on_bar(strategy) { strategy.order("a", OrderSide::Long); strategy.on("Z"); }"#;
    assert!(drive(bad, 1).placed.is_empty(), "the hook errored, so it sends nothing");
    let ctx = crate::context::ScriptContext::new();
    let engine = crate::engine::host::build_engine(&ctx);
    let ast = engine.compile(r#"fn f(s) { s.on("Z") } fn g(s) { s.price_of("Z") }"#).unwrap();
    for name in ["f", "g"] {
        let handle = Handle(ctx.clone());
        let err = engine.call_fn::<Dynamic>(&mut rhai::Scope::new(), &ast, name, (handle,));
        let err = err.unwrap_err().to_string();
        assert!(err.contains("(\"Z\")") && err.contains("fn trades()"), "{err}");
    }

    for (src, words) in [
        ("fn trades() { [\"Y\"] } fn on_bar() {}", "`fn trades()` needs `fn on_bar(strategy)`"),
        ("fn trades() { [1] } fn on_bar(strategy) {}", "trades: entry 1 is"),
    ] {
        let err = RhaiStrategy::<OrderBrokerMock>::compile(src).err().expect("refused");
        assert!(err.to_string().contains(words), "{err}");
    }
}

/// **A script mounted on a symbol trades it as its own, whichever bar comes first** (one broker plan,
/// step 12's leftover): on a live mount that declares legs the first bar may be a leg's, and the run took
/// that bar's symbol as its own, so its own orders went to the leg. `RhaiStrategy::mounted_on` names the
/// mount's; without it (a backtest) the first bar's symbol is still the run's own.
#[test]
fn a_mounted_script_trades_the_mounts_symbol_whichever_bar_comes_first() {
    let src =
        r#"fn trades() { ["Y"] } fn on_bar(strategy) { strategy.order("a", OrderSide::Long); }"#;
    let on_a_legs_bar = |mut strat: RhaiStrategy<OrderBrokerMock>| {
        let mut broker = OrderBrokerMock { price: 100.0, ..OrderBrokerMock::default() };
        let bar =
            Bar { symbol: Some("Y".into()), ..Bar::ohlcv(60_000, 100.0, 101.0, 99.0, 100.0, 1.0) };
        strat.on_bar(&mut broker, &bar);
        broker.placed.into_iter().map(|p| (p.0, p.1)).collect::<Vec<_>>()
    };
    let compile = || RhaiStrategy::<OrderBrokerMock>::compile(src).expect("compiles");
    assert_eq!(
        on_a_legs_bar(compile().mounted_on("X")),
        [("X".to_string(), "a#1".to_string())],
        "the mount's symbol is the run's own, its orders untagged by a symbol"
    );
    assert_eq!(
        on_a_legs_bar(compile()),
        [("Y".to_string(), "a#1".to_string())],
        "unmounted, the first bar's symbol is the run's own, as before"
    );
}

/// The snapshot follows a history that grows at its end, and copies one that does not whole.
#[test]
fn follow_appends_what_is_new_and_copies_a_history_that_moved_whole() {
    let mut held: Vec<i32> = Vec::new();
    assert!(!follow(&mut held, &[]), "nothing new");
    assert!(follow(&mut held, &[1, 2]));
    assert!(!follow(&mut held, &[1, 2]), "the same history");
    assert!(follow(&mut held, &[1, 2, 3]));
    assert_eq!(held, [1, 2, 3]);
    assert!(follow(&mut held, &[7, 8, 9, 10]), "not this one extended");
    assert_eq!(held, [7, 8, 9, 10]);
    assert!(follow(&mut held, &[7]), "shorter");
    assert_eq!(held, [7]);
}

fn quote(ts: i64) -> TickQuote {
    let symbol = String::new();
    TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid: 99.0,
        ask: 101.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol,
    }
}

/// `on_start` is refused the object by name: before the first bar the run has no symbol to trade
/// and no price.
#[test]
fn on_start_is_refused_the_strategy_object_by_name() {
    let src = "fn on_bar(strategy) {} fn on_start(strategy) {}";
    let err = RhaiStrategy::<OrderBrokerMock>::compile(src).err().expect("refused");
    assert!(err.summary().starts_with("on_start takes no `strategy`"), "{err}");
}

/// `on_tick` and `on_stop` take the object only beside `on_bar(strategy)`, and are defined one way.
#[test]
fn on_tick_and_on_stop_take_it_once_and_beside_on_bar_strategy() {
    for hook in ["on_tick", "on_stop"] {
        let alone = format!("fn on_bar() {{}} fn {hook}(strategy) {{}}");
        let err = RhaiStrategy::<OrderBrokerMock>::compile(&alone).err().expect("refused");
        assert!(err.summary().contains("needs `fn on_bar(strategy)`"), "{err}");
        let twice = format!("fn on_bar(strategy) {{}} fn {hook}() {{}} fn {hook}(strategy) {{}}");
        let err = RhaiStrategy::<OrderBrokerMock>::compile(&twice).err().expect("refused");
        assert!(err.summary().contains(&format!("defines {hook} once")), "{err}");
    }
}

/// `on_tick(strategy)` runs from the run's first bar on, sampled as `on_tick()` is, and its verbs
/// reach the seam at the quote. A quote before the first bar runs nothing and spends no sample.
#[test]
fn on_tick_writes_through_the_seam_from_the_first_bar() {
    let src = r#"fn on_bar(strategy) {}
                 fn on_tick(strategy) { strategy.order("t", OrderSide::Long).size(1.5); }"#;
    let mut strat = RhaiStrategy::<OrderBrokerMock>::compile(src).unwrap();
    let mut b = OrderBrokerMock::default();
    strat.on_quote_tick(&mut b, &quote(59_990));
    assert!(b.placed.is_empty(), "no bar yet: the run has no symbol, nothing runs");
    strat.on_bar(&mut b, &Bar::ohlcv(60_000, 100.0, 101.0, 99.0, 100.0, 1.0));
    assert!(b.placed.is_empty(), "the bar hook sends nothing");
    strat.on_quote_tick(&mut b, &quote(60_000));
    strat.on_quote_tick(&mut b, &quote(60_000 + crate::TICK_HOOK_MIN_MS - 1));
    assert_eq!(b.placed.len(), 1, "one run: the second quote is inside the bound: {:?}", b.placed);
    let (_, tag, side, qty, order, reduce_only) = &b.placed[0];
    assert!(tag.starts_with("t#"), "{tag}");
    assert_eq!((*side, *qty, *order, *reduce_only), (1, 1.5, OrderPlacement::Market, false));
}

/// In `on_stop` a verb is refused by its own name, and records nothing; a read still answers.
#[test]
fn on_stop_refuses_a_verb_by_its_name_and_reads() {
    let ctx = crate::context::ScriptContext::new();
    {
        let mut g = ctx.write().unwrap();
        g.reads_only = true;
        g.reads.symbols[0].1.position = 2.0;
    }
    let engine = crate::engine::host::build_engine(&ctx);
    let ast = engine.compile("fn f(s) { s.position() } fn g(s) { s.close_all() }").unwrap();
    let call = |name: &str| {
        let handle = Handle(ctx.clone());
        engine.call_fn::<Dynamic>(&mut rhai::Scope::new(), &ast, name, (handle,))
    };
    assert_eq!(call("f").unwrap().as_float(), Ok(2.0), "a read answers in on_stop");
    let err = call("g").unwrap_err().to_string();
    assert!(err.contains("strategy.close_all is refused in on_stop"), "{err}");
    assert!(ctx.read().unwrap().calls.is_empty(), "the refused verb recorded nothing");

    let src =
        r#"fn on_bar(strategy) {} fn on_stop(strategy) { strategy.entry("L", OrderSide::Long); }"#;
    let mut strat = RhaiStrategy::<OrderBrokerMock>::compile(src).unwrap();
    let mut b = drive_on(&mut strat, 3);
    strat.on_stop(&mut b);
    assert!(b.placed.is_empty() && b.groups.is_empty(), "{:?} {:?}", b.placed, b.groups);
}
