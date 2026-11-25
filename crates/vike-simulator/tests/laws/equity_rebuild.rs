//! A bar run's equity curve REBUILT from the log of its cash and position events and its bars, bit for bit
//! (`vike_simulator::rebuild_equity_curve`; `crates/vike-simulator/src/engine/sim_broker/ledger_events.rs`'s
//! module doc; decision 0078's amendment of 2026-11-19). The test build checks EVERY bar run of this crate's
//! suites the same way inside `StrategyEngine::run`; the cases here name the shapes a saved run meets, each
//! through the log's own bytes (`encode_ledger_events` and back), print one `CASE` line each, and measure the
//! log's size for a long run.

use vike_marketdata::Bar;
use vike_model::run_request::decide::DecideMode;
use vike_model::{AccountPerQuoteLeg, ContractValue, Strategy};
use vike_simulator::test_support::gen_bars;
use vike_simulator::{
    ConversionSeries, LedgerEvent, LedgerEventKind, LedgerStart, OrderBrokerSimulator,
    StrategyEngine, StrategyEngineConfig, decode_ledger_events, encode_ledger_events,
    rebuild_equity_curve,
};

const DAY: i64 = 86_400_000;

/// Market orders at set steps, `(step, symbol, signed size)`, each filling at the next bar's open.
struct Script {
    orders: Vec<(usize, &'static str, f64)>,
    done: Option<usize>,
}

impl Strategy<OrderBrokerSimulator> for Script {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        if self.done == Some(ctx.index) {
            return; // once per step, not once per symbol of the fan-out
        }
        self.done = Some(ctx.index);
        let now: Vec<_> = self.orders.iter().filter(|o| o.0 == ctx.index).copied().collect();
        for (_, symbol, qty) in now {
            ctx.submit(symbol, if qty > 0.0 { 1 } else { -1 }, qty.abs(), 0.0, true, None);
        }
    }
}

/// `n` bars of the deterministic walk for `symbol`, every price times `k`.
fn walk(n: usize, symbol: &str, k: f64) -> Vec<Bar> {
    gen_bars(n, Some(symbol))
        .into_iter()
        .map(|b| Bar {
            open: b.open * k,
            high: b.high * k,
            low: b.low * k,
            close: b.close * k,
            ..b
        })
        .collect()
}

struct Outcome {
    steps: usize,
    events: Vec<LedgerEvent>,
    /// The first step whose rebuilt equity's bits differ from the engine's.
    first_diff: Option<usize>,
    start: LedgerStart,
    curve: Vec<f64>,
}

/// Run `orders` over `series` under `params`, then rebuild the curve from the run's log, read back from its
/// bytes, and its own bars; the start is `LedgerStart::of` the run's terms, as a saved run's is.
fn run(
    series: Vec<(&str, Vec<Bar>)>,
    orders: Vec<(usize, &'static str, f64)>,
    params: StrategyEngineConfig,
) -> Outcome {
    let series: Vec<(String, Vec<Bar>)> =
        series.into_iter().map(|(s, b)| (s.to_string(), b)).collect();
    let symbols: Vec<String> = series.iter().map(|(s, _)| s.clone()).collect();
    let (cash, decide) = (params.cash, params.decide);
    let mut engine = StrategyEngine::new(series, Script { orders, done: None }, params);
    let engine_start = engine.core.start_ledger_log();
    let start = LedgerStart::of(cash, engine_start.values.clone(), &symbols, decide);
    assert_eq!(start, engine_start, "a saved run's start is the engine's");
    let record = engine.run();
    let logged = engine.core.ledger_events.clone().expect("the run logged");
    let events = decode_ledger_events(&encode_ledger_events(&logged)).expect("the log reads back");
    assert_eq!(events, logged, "the log's bytes read back to its events");
    let curve = record.equity_curve;
    let rebuilt = rebuild_equity_curve(&start, &events, curve.len(), |si, step| {
        engine.core.bars[si][step].close
    });
    let first_diff = (0..curve.len()).find(|&k| rebuilt[k].to_bits() != curve[k].to_bits());
    Outcome { steps: curve.len(), events, first_diff, start, curve }
}

fn report(case: &str, o: &Outcome) {
    let fills = o.events.iter().filter(|e| matches!(e.kind, LedgerEventKind::Fill { .. })).count();
    println!(
        "CASE {case}: steps={} events={} fills={} first_diff={:?}",
        o.steps,
        o.events.len(),
        fills,
        o.first_diff
    );
}

fn equal(case: &str, o: &Outcome) {
    report(case, o);
    assert!(
        o.events.iter().any(|e| matches!(e.kind, LedgerEventKind::Fill { .. })),
        "{case}: traded"
    );
    assert_eq!(o.first_diff, None, "{case}: the rebuilt curve differs");
}

fn costs() -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 100_000.0,
        fee_rate: 0.001,
        slippage: 0.0005,
        ..Default::default()
    }
}

#[test]
fn rebuild_long_only() {
    let orders = vec![
        (10, "A", 1.0),
        (50, "A", 0.5),
        (120, "A", -0.75),
        (200, "A", -0.75),
        (300, "A", 2.0),
        (400, "A", -2.0),
    ];
    equal("long_only", &run(vec![("A", walk(2_000, "A", 1.0))], orders, costs()));
}

#[test]
fn rebuild_long_and_short_with_flips() {
    let orders = vec![
        (10, "A", 1.0),
        (60, "A", -2.0),
        (100, "A", 0.4),
        (150, "A", 1.6),
        (220, "A", -3.0),
        (300, "A", 3.0),
        (900, "A", -1.0),
    ];
    equal("long_short", &run(vec![("A", walk(2_000, "A", 1.0))], orders, costs()));
}

/// Adds and partial exits in sizes binary floats cannot hold, ending on a dust close (`compute_fill`'s EPS).
#[test]
fn rebuild_partial_exits_and_adds() {
    let mut orders: Vec<(usize, &'static str, f64)> =
        (0..10).map(|i| (10 + 7 * i, "A", 0.1)).collect();
    orders.extend((0..7).map(|i| (100 + 11 * i, "A", -0.07)));
    orders.extend([
        (200, "A", -0.51),
        (250, "A", -0.3),
        (260, "A", 0.3),
        (300, "A", 0.2),
        (310, "A", 0.1),
    ]);
    equal("partial_adds", &run(vec![("A", walk(1_000, "A", 1.0))], orders, costs()));
}

/// Three symbols listed out of name order, once in list order and once `Simultaneous` (equity sums in name
/// order then), the shared-cash lane with it.
#[test]
fn rebuild_several_symbols() {
    let series = || {
        vec![
            ("C", walk(1_500, "C", 3.0)),
            ("A", walk(1_500, "A", 1.0)),
            ("B", walk(1_500, "B", 0.5)),
        ]
    };
    let orders = vec![
        (10, "A", 2.0),
        (12, "B", -5.0),
        (15, "C", 1.0),
        (80, "A", -1.0),
        (90, "C", -2.0),
        (200, "B", 8.0),
        (400, "A", -1.0),
        (700, "C", 1.0),
        (900, "B", -3.0),
    ];
    equal("several_symbols", &run(series(), orders.clone(), costs()));
    let simultaneous = StrategyEngineConfig { decide: DecideMode::Simultaneous, ..costs() };
    equal("several_symbols_simultaneous", &run(series(), orders, simultaneous));
}

#[test]
fn rebuild_contract_value_and_inverse() {
    let futures = StrategyEngineConfig {
        contract_values: vec![("ES".into(), ContractValue::linear(50.0))],
        ..costs()
    };
    let orders = vec![(10, "ES", 2.0), (100, "ES", -3.0), (300, "ES", 1.0)];
    equal("contract_value_x50", &run(vec![("ES", walk(1_000, "ES", 40.0))], orders, futures));

    // A coin ledger (decision 0258): 0.1 BTC, sizes in contracts of one dollar.
    let coin = StrategyEngineConfig {
        cash: 0.1,
        fee_rate: 0.000_35,
        contract_values: vec![("BTC-PERPETUAL".into(), ContractValue::inverse(1.0))],
        ..Default::default()
    };
    let orders = vec![
        (10, "BTC-PERPETUAL", 1_000.0),
        (60, "BTC-PERPETUAL", 500.0),
        (100, "BTC-PERPETUAL", -2_000.0),
        (400, "BTC-PERPETUAL", 500.0),
    ];
    equal(
        "inverse",
        &run(vec![("BTC-PERPETUAL", walk(1_000, "BTC-PERPETUAL", 300.0))], orders, coin),
    );
}

/// `Bar.funding` every eight hours on a held long, then a held short.
#[test]
fn rebuild_with_funding() {
    let bars: Vec<Bar> = walk(3_000, "P", 1.0)
        .into_iter()
        .enumerate()
        .map(|(i, b)| Bar {
            funding: (i % 480 == 0).then_some(0.0001 * (1.0 + (i % 7) as f64)),
            ..b
        })
        .collect();
    let orders = vec![(10, "P", 5.0), (1_200, "P", -10.0), (2_500, "P", 5.0)];
    let o = run(vec![("P", bars)], orders, costs());
    assert!(
        o.events.iter().any(|e| matches!(e.kind, LedgerEventKind::Funding { .. })),
        "funding charged"
    );
    equal("funding", &o);
}

/// A USDT-quoted symbol on the dollar ledger, rated by a daily USDT-USD series one-to-one where it has none
/// (decision 0243's automatic case): the rate enters the curve through the value and a held position's cash.
#[test]
fn rebuild_with_usdt_usd_conversion() {
    let days = 4;
    let rates: Vec<Bar> = (0..days)
        .map(|d| {
            let c = 0.9990 + 0.0004 * d as f64;
            vike_simulator::test_support::bar(d as i64 * DAY, c, c, c, c, 1.0)
        })
        .collect();
    let usdt = ConversionSeries {
        quote_currency: "USDT".into(),
        pair: "test:USDT-USD:1d".into(),
        leg: AccountPerQuoteLeg::QuoteIsBase,
        interval_ms: DAY,
        bars: rates.into(),
        symbols: vec!["BTCUSDT".into()],
        par_where_missing: true,
    };
    let params = StrategyEngineConfig {
        account_currency: Some("USD".into()),
        conversions: vec![usdt],
        ..costs()
    };
    let orders = vec![(10, "BTCUSDT", 2.0), (2_000, "BTCUSDT", -1.0), (4_000, "BTCUSDT", -3.0)];
    let o = run(vec![("BTCUSDT", walk(days * 1_440, "BTCUSDT", 600.0))], orders, params);
    let revalues =
        o.events.iter().filter(|e| matches!(e.kind, LedgerEventKind::Revalue { .. })).count();
    println!("CASE usdt_usd_conversion: revalue events={revalues}");
    assert!(revalues > 0, "the rate moved the curve");
    equal("usdt_usd_conversion", &o);
}

/// A 4-for-1 split at step 1,000 (decision 0289): the harness hands the engine the ADJUSTED series, so the
/// rebuild from the adjusted bars is exact, and the one from the raw stored bars is not (the manifest must
/// carry the split rows the run was adjusted by).
#[test]
fn rebuild_across_a_split() {
    let adjusted = walk(2_000, "AAPL", 1.0);
    let raw: Vec<f64> = adjusted
        .iter()
        .enumerate()
        .map(|(i, b)| if i < 1_000 { b.close * 4.0 } else { b.close })
        .collect();
    let orders = vec![(100, "AAPL", 10.0), (1_500, "AAPL", -10.0)];
    let o = run(vec![("AAPL", adjusted)], orders, costs());
    equal("split_adjusted_bars", &o);
    let from_raw = rebuild_equity_curve(&o.start, &o.events, o.steps, |_, step| raw[step]);
    let first = (0..o.steps).find(|&k| from_raw[k].to_bits() != o.curve[k].to_bits());
    println!("CASE split_raw_bars: first_diff={first:?}");
    assert!(first.is_some_and(|k| k < 1_000), "the raw bars differ while held before the split");
}

/// The precision_confluence-like case: 216 round trips over 263,521 one-minute bars, alone and with a daily
/// USDT-USD rate (184 days). Prints the log's size as saved (`encode_ledger_events`) against the 4,216,360-byte
/// `equity_curve.bin` the same run saved before (24 bytes of header, 16 a step).
#[test]
fn rebuild_size_of_a_long_run() {
    const STEPS: usize = 263_521;
    let orders: Vec<(usize, &'static str, f64)> = (0..216)
        .flat_map(|k| [(100 + k * 1_200, "BTCUSDT", 1.0), (700 + k * 1_200, "BTCUSDT", -1.0)])
        .collect();
    let bars = || walk(STEPS, "BTCUSDT", 1.0);
    for (case, params) in [
        ("size_plain", costs()),
        ("size_usdt_daily", {
            let rates: Vec<Bar> = (0..184)
                .map(|d| {
                    let c = 0.9990 + 0.0001 * (d % 20) as f64;
                    vike_simulator::test_support::bar(d as i64 * DAY, c, c, c, c, 1.0)
                })
                .collect();
            let usdt = ConversionSeries {
                quote_currency: "USDT".into(),
                pair: "test:USDT-USD:1d".into(),
                leg: AccountPerQuoteLeg::QuoteIsBase,
                interval_ms: DAY,
                bars: rates.into(),
                symbols: vec!["BTCUSDT".into()],
                par_where_missing: true,
            };
            StrategyEngineConfig { conversions: vec![usdt], ..costs() }
        }),
    ] {
        let o = run(vec![("BTCUSDT", bars())], orders.clone(), params);
        let log = encode_ledger_events(&o.events);
        println!(
            "CASE {case}: steps={} events={} log_bytes={} curve_bytes={}",
            o.steps,
            o.events.len(),
            log.len(),
            24 + 16 * o.steps
        );
        assert!(log.len() * 100 < 24 + 16 * o.steps, "the log is under a hundredth of the curve");
        equal(case, &o);
    }
}
