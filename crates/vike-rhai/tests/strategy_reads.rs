//! The `strategy` object reads the run beyond its own bars (decision 0271), on the real simulator:
//! an INPUT's closed bars as of each bar (declared in `fn inputs()`), and the closed trades as the
//! engine's ledger booked them.
//!
//! What a script read is what it printed: `print` reaches `tracing` (`crates/vike-rhai/src/engine/host.rs`'s
//! `redirect_script_output`), which `vike_log::capture::captured` holds. rhai prints a float in its
//! shortest round-trip form, so a printed money field parses back to the engine's bits. `()` prints
//! as nothing, so a "no value" is printed as its `type_of`.

use vike_model::{OrderBroker, Strategy, TradeClosedRow};
use vike_rhai::RhaiStrategy;
use vike_rhai::test_support::{OrderBrokerMock, drive, ohlcv};
use vike_simulator::{
    InputSeries, Inputs, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig,
};

const NAME: &str = "binance:BTCUSDT:1h";
const HALF_HOUR: i64 = 1_800_000;
const HOUR: i64 = 3_600_000;

/// The lines the script printed while `f` ran; a hook that errored fails the test with its error.
fn printed<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    let (r, events) = vike_log::capture::captured(f);
    let errors: Vec<_> = events.iter().filter_map(|e| e.field("script_error")).collect();
    assert!(errors.is_empty(), "a hook errored: {errors:?}");
    let lines = events.into_iter().filter(|e| e.target == "vike_rhai").map(|e| e.message);
    (r, lines.collect())
}

fn compile(src: &str) -> RhaiStrategy<OrderBrokerSimulator> {
    RhaiStrategy::compile(src).unwrap_or_else(|e| panic!("{e}"))
}

/// Half-hour bars against an hourly input, as `crates/vike-simulator/tests/laws/input_as_of.rs` lays them:
/// the decision at a bar's close sees the input bars closed by then, so the eight decisions see 0,
/// 1, 1, 2, 2, 3, 3 and 4 of them. Every field of input bar `k` names `k`.
#[test]
fn a_script_reads_an_inputs_last_closed_bar_and_its_bars_as_of_each_bar() {
    let hourly = (0..4)
        .map(|k| {
            let k = k as f64;
            ohlcv(k as i64 * HOUR, 1.0 + k, 1e9, 0.5, 10.0 * (k + 1.0), 100.0 + k)
        })
        .collect();
    let inputs = Inputs {
        bar_ms: HALF_HOUR,
        series: vec![InputSeries { name: NAME.into(), interval_ms: HOUR, bars: hourly }],
    };
    let src = r#"
fn inputs() { ["binance:BTCUSDT:1h"] }
fn on_bar(strategy) {
    let other = strategy.input("binance:ETHUSDT:1h");
    print(`undeclared ${type_of(other.close())} ${other.bars(9).len()}`);
    let h = strategy.input("binance:BTCUSDT:1h");
    if h.close() == () {
        print(`none ${type_of(h.close())} ${type_of(h.time())} ${h.bars(9).len()} ${h.since(0).len()}`);
        return;
    }
    print(`close ${h.close()} open ${h.open()} high ${h.high()} low ${h.low()} volume ${h.volume()} time ${h.time()}`);
    let last = h.bars(2);
    print(`bars ${last.len()} ${last[0].time} ${last[-1].close} since ${h.since(3600000).len()}`);
}
"#;
    let strat = compile(src);
    assert_eq!(
        Strategy::<OrderBrokerSimulator>::inputs(&strat),
        [NAME],
        "the run loads what fn inputs() names"
    );
    let bars = (0..8).map(|i| ohlcv(i * HALF_HOUR, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
    let params = StrategyEngineConfig { inputs, ..Default::default() };
    let mut engine = StrategyEngine::new(vec![("X".to_string(), bars)], strat, params);
    let (_, lines) = printed(|| engine.run());

    let mut want = Vec::new();
    for closed in [0_i64, 1, 1, 2, 2, 3, 3, 4] {
        want.push("undeclared () 0".to_string());
        if closed == 0 {
            want.push("none () () 0 0".to_string());
            continue;
        }
        let k = closed - 1;
        want.push(format!(
            "close {}.0 open {}.0 high 1000000000.0 low 0.5 volume {}.0 time {}",
            10 * (k + 1),
            1 + k,
            100 + k,
            k * HOUR
        ));
        let first = (closed - 2).max(0) * HOUR;
        want.push(format!("bars {} {first} {}.0 since {k}", closed.min(2), 10 * (k + 1)));
    }
    assert_eq!(lines, want);
}

/// `fn inputs()` is read once at compile and refuses a non-string entry by its place; it declares
/// what the strategy object reads, so it needs `fn on_bar(strategy)`.
#[test]
fn fn_inputs_is_the_strategys_inputs_and_refuses_what_is_not_a_name() {
    let strat = compile(r#"fn inputs() { ["1d", "binance:ETHUSDT:1h"] } fn on_bar(strategy) {}"#);
    assert_eq!(Strategy::<OrderBrokerSimulator>::inputs(&strat), ["1d", "binance:ETHUSDT:1h"]);
    assert!(
        Strategy::<OrderBrokerSimulator>::inputs(&compile("fn on_bar(strategy) {}")).is_empty()
    );

    let refused =
        |src: &str| RhaiStrategy::<OrderBrokerSimulator>::compile(src).err().expect("refused");
    let err = refused(r#"fn inputs() { ["1d", 5] } fn on_bar(strategy) {}"#);
    assert!(err.summary().starts_with("inputs: entry 2 is i64, not a string"), "{err}");
    let err = refused(r#"fn inputs() { ["1d"] } fn on_bar() {}"#);
    assert!(err.summary().contains("needs `fn on_bar(strategy)`"), "{err}");
}

/// A wave with a range inside each bar, so each round trip closes at a profit or a loss.
fn wave(n: i64) -> Vec<vike_marketdata::Bar> {
    let level = |i: f64| 100.0 + 8.0 * (i / 6.0).sin() + 0.05 * i;
    (0..n)
        .map(|i| {
            let (open, close) = (level(i as f64 - 1.0), level(i as f64));
            ohlcv(60_000 * (i + 1), open, open.max(close) + 0.6, open.min(close) - 0.6, close, 10.0)
        })
        .collect()
}

/// One printed row back as the row it was: `row long symbol entry_timestamp entry_bar exit_timestamp exit_bar qty
/// entry_price exit_price pnl commission net [tag]`.
fn parsed(line: &str) -> TradeClosedRow {
    let f: Vec<&str> = line.split(' ').collect();
    let number = |i: usize| f[i].parse::<f64>().unwrap_or_else(|e| panic!("{line}: {e}"));
    let int = |i: usize| f[i].parse::<i64>().unwrap_or_else(|e| panic!("{line}: {e}"));
    TradeClosedRow {
        side: if f[1] == "true" { 1 } else { -1 },
        symbol: f[2].to_string(),
        entry_timestamp: int(3),
        entry_bar: int(4) as usize,
        exit_timestamp: int(5),
        exit_bar: int(6) as usize,
        qty: number(7),
        entry_price: number(8),
        exit_price: number(9),
        pnl: number(10),
        commission: number(11),
        net: number(12),
        tag: f[13].trim_matches(['[', ']']).to_string(),
    }
}

/// The money fields' bits, which `TradeClosedRow`'s `==` does not compare (`0.0 == -0.0`).
fn bits(r: &TradeClosedRow) -> [u64; 6] {
    [r.qty, r.entry_price, r.exit_price, r.pnl, r.commission, r.net].map(f64::to_bits)
}

/// Five round trips, long then short, each closed in two halves: the rows the script reads in
/// `on_stop` are the engine's own ledger, row for row and bit for bit, and its net is theirs summed
/// in closing order. Before the first close it reads `[]` and `0.0`, never `()`.
#[test]
fn the_closed_trades_read_is_the_engines_ledger_row_for_row() {
    let src = r#"
fn on_bar(strategy) {
    let i = index();
    let side = if (i / 6) % 2 == 0 { OrderSide::Long } else { OrderSide::Short };
    if i < 30 && i % 6 == 0 { strategy.entry("E", side).size(2.0); }
    if i % 6 == 2 { strategy.close("E").size(1.0); }
    if i % 6 == 4 { strategy.close("E"); }
    print(`seen ${strategy.closed_trades().len()} ${strategy.closed_net()}`);
}
fn on_stop(strategy) {
    for r in strategy.closed_trades() {
        print(`row ${r.side == OrderSide::Long} ${r.symbol} ${r.entry_timestamp} ${r.entry_bar} ${r.exit_timestamp} ${r.exit_bar} ${r.qty} ${r.entry_price} ${r.exit_price} ${r.pnl} ${r.commission} ${r.net} [${r.tag}]`);
    }
    print(`net ${strategy.closed_net()}`);
}
"#;
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: 0.0002,
        maker_fee: Some(0.0002),
        taker_fee: Some(0.0007),
        ..StrategyEngineConfig::default()
    };
    let mut engine = StrategyEngine::new(vec![("X".to_string(), wave(36))], compile(src), params);
    let (_, lines) = printed(|| engine.run());
    let ledger = OrderBroker::closed_trades(&engine.core).expect("the simulator keeps a ledger");
    assert_eq!(ledger.len(), 10, "five round trips, two halves each: {ledger:?}");
    assert!(ledger.iter().any(|r| r.side < 0) && ledger.iter().all(|r| r.commission > 0.0));

    assert_eq!(lines[0], "seen 0 0.0", "an empty ledger is [] and 0.0");
    let rows: Vec<TradeClosedRow> =
        lines.iter().filter(|l| l.starts_with("row ")).map(|l| parsed(l)).collect();
    assert_eq!(rows, ledger);
    assert_eq!(
        rows.iter().map(bits).collect::<Vec<_>>(),
        ledger.iter().map(bits).collect::<Vec<_>>()
    );
    let net = lines.last().and_then(|l| l.strip_prefix("net ")).expect("the net is printed last");
    let summed = ledger.iter().fold(0.0, |net, row| net + row.net);
    assert_eq!(net.parse::<f64>().unwrap().to_bits(), summed.to_bits());
}

/// A broker that keeps no ledger (a test broker: the simulator and a paper or live mount keep one)
/// answers `()`, which is not "nothing closed"; an input it never loaded reads `()` too.
#[test]
fn a_broker_with_no_ledger_answers_unit_not_an_empty_list() {
    let src = r#"
fn inputs() { ["1d"] }
fn on_bar(strategy) {
    print(`${type_of(strategy.closed_trades())} ${type_of(strategy.closed_net())} ${type_of(strategy.input("1d").close())}`);
}
"#;
    let mut strat = RhaiStrategy::<OrderBrokerMock>::compile(src).unwrap_or_else(|e| panic!("{e}"));
    let mut broker = OrderBrokerMock::default();
    let ((), lines) = printed(|| drive(&mut strat, &mut broker, &[1.0]));
    assert_eq!(lines, ["() () ()"]);
}

/// What the script's hooks errored with while `f` ran.
fn script_errors<R>(f: impl FnOnce() -> R) -> Vec<String> {
    let (_, events) = vike_log::capture::captured(f);
    events.iter().filter_map(|e| e.field("script_error")).map(|e| e.to_string()).collect()
}

/// An answer longer than one array of a script holds (682 bars: the sandbox counts every field of every map
/// against its limit of 4096) is refused by name,
/// with the read to use instead, never as the sandbox's own size error: `bars(n)` past it, and `since`
/// over more bars than it holds.
#[test]
fn a_read_longer_than_a_scripts_array_is_refused_by_name() {
    let minutes: Vec<_> = (0..5000).map(|k| ohlcv(k * 60_000, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
    let run = |read: &str| {
        let src = format!(
            "fn inputs() {{ [\"{NAME}\"] }}\nfn on_bar(strategy) {{ strategy.input(\"{NAME}\").{read}; }}\n"
        );
        let inputs = Inputs {
            bar_ms: HOUR,
            series: vec![InputSeries {
                name: NAME.into(),
                interval_ms: 60_000,
                bars: minutes.clone(),
            }],
        };
        let params = StrategyEngineConfig { inputs, ..Default::default() };
        let bars = vec![ohlcv(4 * 24 * HOUR, 1.0, 1.0, 1.0, 1.0, 1.0)];
        let mut engine = StrategyEngine::new(vec![("X".to_string(), bars)], compile(&src), params);
        script_errors(|| engine.run())
    };
    let errors = run("bars(5000)");
    assert!(
        errors.iter().any(|e| e.contains("input.bars(5000)") && e.contains("ask for 682 or fewer")),
        "{errors:?}"
    );
    let errors = run("since(0)");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("holds 5000 bars") && e.contains("input.count() and input.bar(i)")),
        "{errors:?}"
    );
    let errors = run("bars(682)");
    assert!(errors.is_empty(), "682 bars fit one array: {errors:?}");
    let errors = run("bars(683)");
    assert!(
        errors.iter().any(|e| e.contains("input.bars(683)")),
        "one more is refused: {errors:?}"
    );
}

/// **The index reads walk any length inside the sandbox**, one map an answer, where an array of
/// 5,000 bars or 1,000 trades would be refused: `input.count()` and `input.bar(i)` over 5,000 closed
/// minutes (oldest first, `-1` the newest, `()` past the end), and `strategy.closed_count()` and
/// `strategy.closed_trade(i)` over 1,000 round trips, whose nets walked in order add to
/// `strategy.closed_net()` to the bit and are the engine's own ledger's.
#[test]
fn the_index_reads_walk_5000_bars_and_1000_closed_trades_in_the_sandbox() {
    let minutes: Vec<_> =
        (0..5000).map(|k| ohlcv(k * 60_000, 1.0, 1.0, 1.0, k as f64, 1.0)).collect();
    let src = r#"
fn inputs() { ["binance:BTCUSDT:1h"] }
fn on_bar(strategy) {
    if index() == 0 {
        let h = strategy.input("binance:BTCUSDT:1h");
        let n = h.count();
        let sum = 0.0;
        for k in 0..n { sum += h.bar(k).close; }
        let first = h.bar(0).time;
        let last = h.bar(-1).time;
        let past = type_of(h.bar(n));
        print(`bars ${n} ${sum} ${first} ${last} ${past}`);
    }
    if strategy.position() == 0.0 { strategy.entry("L", OrderSide::Long).size(1.0); } else { strategy.close("L"); }
}
fn on_stop(strategy) {
    let n = strategy.closed_count();
    let net = 0.0;
    for k in 0..n { net += strategy.closed_trade(k).net; }
    let total = strategy.closed_net();
    let past = type_of(strategy.closed_trade(n));
    print(`trades ${n} ${net} ${total} ${past}`);
}
"#;
    let inputs = Inputs {
        bar_ms: 60_000,
        series: vec![InputSeries { name: NAME.into(), interval_ms: 60_000, bars: minutes }],
    };
    // The run's own bars start once every minute has closed; 2,002 of them make 1,000 round trips.
    let bars: Vec<_> = wave(2002)
        .into_iter()
        .map(|b| vike_marketdata::Bar { timestamp: b.timestamp + 5000 * 60_000, ..b })
        .collect();
    let params = StrategyEngineConfig { inputs, ..Default::default() };
    let mut engine = StrategyEngine::new(vec![("X".to_string(), bars)], compile(src), params);
    let (_, lines) = printed(|| engine.run());
    let sum: f64 = (0..5000).map(|k| k as f64).sum();
    assert_eq!(lines[0], format!("bars 5000 {sum:?} 0 {} ()", 4999 * 60_000));
    let ledger = OrderBroker::closed_trades(&engine.core).expect("the simulator keeps a ledger");
    assert!(ledger.len() >= 1000, "{} round trips", ledger.len());
    let last = lines.last().expect("on_stop printed");
    let f: Vec<&str> = last.split(' ').collect();
    assert_eq!(
        (f[0], f[1].parse::<usize>().unwrap(), f[4]),
        ("trades", ledger.len(), "()"),
        "{last}"
    );
    let summed = ledger.iter().fold(0.0, |net, row| net + row.net);
    assert_eq!(f[2].parse::<f64>().unwrap().to_bits(), summed.to_bits(), "walked in order: {last}");
    assert_eq!(f[3].parse::<f64>().unwrap().to_bits(), summed.to_bits(), "closed_net: {last}");
}
