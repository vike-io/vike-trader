//! `OrderBroker::instrument_grid` on the simulator: a strategy reads the grid its orders are held to. With
//! a properties source that is the instrument data's point-in-time record at `now`, the one the gate
//! refresh and the fill lane read; without one, an armed gate's own `risk_limits`; with neither,
//! nothing.

use std::sync::Arc;

use vike_model::{Grid, OrderBroker, RiskLimits, Strategy, SymbolProperties};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "SYM0";

struct Idle;

impl Strategy<OrderBrokerSimulator> for Idle {}

fn engine(params: StrategyEngineConfig) -> StrategyEngine<Idle> {
    let bars = vike_simulator::test_support::flat_minute_bars(2, 100.0, 1.0);
    StrategyEngine::new(vec![(SYM.to_string(), bars)], Idle, params)
}

#[test]
fn a_strategy_reads_the_grid_its_orders_are_held_to() {
    let properties_value = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.001,
        min_qty: 0.002,
        min_notional: 5.0,
        ..Default::default()
    };
    let pit = StrategyEngineConfig {
        default_venue: Some("TEST".into()),
        properties: Some(Arc::new(move |_v: &str, _s: &str, ts: i64| {
            (ts >= 1_000).then_some(properties_value)
        })),
        ..Default::default()
    };
    let mut e = engine(pit);
    e.core.now = 0;
    assert_eq!(OrderBroker::instrument_grid(&e.core, ""), None, "no record at now: unconstrained");
    e.core.now = 2_000;
    let want = Grid {
        tick_size: Some(0.01),
        lot_size: Some(0.001),
        min_notional: Some(5.0),
        min_qty: Some(0.002),
    };
    assert_eq!(OrderBroker::instrument_grid(&e.core, ""), Some(want), "\"\" names the HFT symbol");
    assert_eq!(OrderBroker::instrument_grid(&e.core, SYM), Some(want));
    assert_eq!(OrderBroker::instrument_grid(&e.core, "OTHER"), None, "an unknown symbol");

    // No properties source: an armed gate's own floors are the grid.
    let gated = StrategyEngineConfig {
        risk_limits: Some(RiskLimits {
            tick_size: Some(0.5),
            min_notional: Some(10.0),
            ..RiskLimits::new()
        }),
        ..Default::default()
    };
    let grid = OrderBroker::instrument_grid(&engine(gated).core, "").expect("the gate's grid");
    assert_eq!((grid.tick_size, grid.min_notional, grid.min_qty), (Some(0.5), Some(10.0), None));

    assert_eq!(
        OrderBroker::instrument_grid(&engine(StrategyEngineConfig::default()).core, ""),
        None
    );
}

/// A record from `ts = 1_000` on, of a 0.01 tick and a 0.001 lot.
fn dated() -> StrategyEngineConfig {
    let record = SymbolProperties { tick_size: 0.01, step_size: 0.001, ..Default::default() };
    StrategyEngineConfig {
        default_venue: Some("TEST".into()),
        properties: Some(Arc::new(move |_v: &str, _s: &str, ts: i64| {
            (ts >= 1_000).then_some(record)
        })),
        ..Default::default()
    }
}

/// A `[risk]` that states a 0.5 tick and nothing else of the grid.
fn stated() -> Option<RiskLimits> {
    Some(RiskLimits { tick_size: Some(0.5), ..RiskLimits::new() })
}

/// The symbol's CURRENT kept row states a 0.1 tick.
fn kept() -> Vec<(String, f64)> {
    vec![(SYM.to_string(), 0.1)]
}

/// What a strategy reads at `now`.
fn grid_at(params: StrategyEngineConfig, now: i64) -> Option<Grid> {
    let mut e = engine(params);
    e.core.now = now;
    OrderBroker::instrument_grid(&e.core, "")
}

/// THE OWNER'S PRECEDENCE (2026-11-19): the tick `[risk]` states, then the dated properties record, then
/// the CURRENT kept row, then none. The kept row lends its TICK alone, never the record's other fields.
#[test]
fn a_strategy_reads_the_stated_tick_then_the_dated_record_then_the_kept_row_then_none() {
    let tick = |params, now| grid_at(params, now).and_then(|g| g.tick_size);
    // 4. nothing states a tick
    assert_eq!(grid_at(StrategyEngineConfig::default(), 2_000), None);
    // 3. the kept row alone: its tick and nothing else
    let alone = grid_at(StrategyEngineConfig { kept_ticks: kept(), ..Default::default() }, 0);
    let want = Grid { tick_size: Some(0.1), lot_size: None, min_notional: None, min_qty: None };
    assert_eq!(alone, Some(want), "the kept row lends its tick alone");
    // 2. a dated record beats it; where no record covers `now` the kept row answers, the record's lot gone with it
    let dated_and_kept = || StrategyEngineConfig { kept_ticks: kept(), ..dated() };
    let on_record = grid_at(dated_and_kept(), 2_000).expect("the record");
    assert_eq!((on_record.tick_size, on_record.lot_size), (Some(0.01), Some(0.001)));
    assert_eq!(grid_at(dated_and_kept(), 0), Some(want), "no record at now: the kept row");
    // 1. a stated tick beats both, the record keeping its other fields
    let all = || StrategyEngineConfig { risk_limits: stated(), kept_ticks: kept(), ..dated() };
    let over_record = grid_at(all(), 2_000).expect("the record's grid");
    assert_eq!((over_record.tick_size, over_record.lot_size), (Some(0.5), Some(0.001)));
    assert_eq!(tick(all(), 0), Some(0.5), "no record at now: still the stated tick");
    let gated =
        StrategyEngineConfig { risk_limits: stated(), kept_ticks: kept(), ..Default::default() };
    assert_eq!(tick(gated, 0), Some(0.5), "no properties source: the stated tick");
    // a kept tick that is no positive number is none, and another symbol's row lends nothing
    for nothing in [0.0, -0.1, f64::NAN] {
        let params =
            StrategyEngineConfig { kept_ticks: vec![(SYM.into(), nothing)], ..Default::default() };
        assert_eq!(grid_at(params, 0), None, "{nothing}");
    }
    let other =
        StrategyEngineConfig { kept_ticks: vec![("OTHER".into(), 0.1)], ..Default::default() };
    assert_eq!(grid_at(other, 0), None);
}

/// Reads the grid of its symbol on every bar, as the order language does on every pass.
struct Reader;

impl Strategy<OrderBrokerSimulator> for Reader {
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, _bar: &vike_marketdata::Bar) {
        let _ = OrderBroker::instrument_grid(broker, "");
    }
}

/// The lines of `params`' run of [`Reader`] (`read`) or of [`Idle`] that name the kept row's tick.
fn kept_lines(params: StrategyEngineConfig, read: bool) -> Vec<String> {
    let bars = vike_simulator::test_support::flat_minute_bars(3, 100.0, 1.0);
    let record = if read {
        StrategyEngine::new(vec![(SYM.to_string(), bars)], Reader, params).run()
    } else {
        StrategyEngine::new(vec![(SYM.to_string(), bars)], Idle, params).run()
    };
    record.data_warnings.into_iter().filter(|l| l.contains("current instrument list")).collect()
}

/// Reads, on every bar, the tick of the run's own symbol by `""` and by its name, and of the second
/// symbol by its name, as `(by "", by name, second)`.
#[derive(Default)]
struct ByBothNames {
    ticks: Vec<(Option<f64>, Option<f64>, Option<f64>)>,
}

impl Strategy<OrderBrokerSimulator> for ByBothNames {
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &vike_marketdata::Bar) {
        if bar.symbol.as_deref() != Some(SYM) {
            return;
        }
        let tick = |symbol: &str| OrderBroker::instrument_grid(&*broker, symbol)?.tick_size;
        self.ticks.push((tick(""), tick(SYM), tick("SYM1")));
    }
}

/// `""` names the run's FIRST symbol (the order language reads its own symbol so, where the simulator's
/// list opens with it): it resolves to that symbol before any lookup, so its kept-row tick is the one
/// its name reads, at every precedence, and a second symbol keeps its own.
#[test]
fn the_unnamed_own_symbol_reads_the_kept_rows_tick_its_name_reads() {
    let kept_both = vec![(SYM.to_string(), 0.1), ("SYM1".to_string(), 0.25)];
    for now in [0, 2_000] {
        let e = {
            let mut e = engine(StrategyEngineConfig { kept_ticks: kept_both.clone(), ..dated() });
            e.core.now = now;
            e
        };
        assert_eq!(
            OrderBroker::instrument_grid(&e.core, ""),
            OrderBroker::instrument_grid(&e.core, SYM),
            "at {now}"
        );
    }
    let bars = || vike_simulator::test_support::flat_minute_bars(3, 100.0, 1.0);
    let series = vec![(SYM.to_string(), bars()), ("SYM1".to_string(), bars())];
    let params = StrategyEngineConfig { kept_ticks: kept_both, ..Default::default() };
    let mut e = StrategyEngine::new(series, ByBothNames::default(), params);
    let record = e.run();
    assert_eq!(e.strategy.ticks, vec![(Some(0.1), Some(0.1), Some(0.25)); 3]);
    let lines: Vec<&String> =
        record.data_warnings.iter().filter(|l| l.contains("current instrument list")).collect();
    assert_eq!(
        lines.len(),
        2,
        "one line per symbol read, the unnamed read the own symbol's: {lines:?}"
    );
    let none = "the run reads no dated properties record";
    assert_eq!(lines, [&kept_line(SYM, "0.1", none), &kept_line("SYM1", "0.25", none)]);
}

/// The data-warnings line of a tick read from `symbol`'s kept row, for `reason`.
fn kept_line(symbol: &str, tick: &str, reason: &str) -> String {
    format!(
        "{symbol}: the instrument grid this run read carried tick {tick} from the current instrument \
         list ({reason})"
    )
}

/// A run whose strategy read the kept row's tick says so ONCE, whatever number of reads; a run whose
/// strategy never read it, or read a dated record's tick on every bar, says nothing.
#[test]
fn the_run_says_once_that_its_tick_came_from_the_kept_row() {
    let kept_only = || StrategyEngineConfig { kept_ticks: kept(), ..Default::default() };
    assert_eq!(
        kept_lines(kept_only(), true),
        [kept_line(SYM, "0.1", "the run reads no dated properties record")],
        "three bars, three reads, one line"
    );
    assert!(kept_lines(kept_only(), false).is_empty(), "no read: nothing said");
    // the bars are at 60_000.. on, inside the record: the kept row is never the answer
    assert!(kept_lines(StrategyEngineConfig { kept_ticks: kept(), ..dated() }, true).is_empty());
    let before_the_record = StrategyEngineConfig {
        kept_ticks: kept(),
        properties: Some(Arc::new(|_v: &str, _s: &str, ts: i64| {
            (ts >= 10_000_000).then_some(SymbolProperties { tick_size: 0.01, ..Default::default() })
        })),
        ..dated()
    };
    assert_eq!(
        kept_lines(before_the_record, true),
        [kept_line(SYM, "0.1", "no properties record for these dates")]
    );
    // a dated record that states a lot and no tick is a record: the line says it states no tick
    let tickless = |from: i64| StrategyEngineConfig {
        kept_ticks: kept(),
        properties: Some(Arc::new(move |_v: &str, _s: &str, ts: i64| {
            (ts >= from).then_some(SymbolProperties { step_size: 0.001, ..Default::default() })
        })),
        ..dated()
    };
    assert_eq!(
        kept_lines(tickless(0), true),
        [kept_line(SYM, "0.1", "the dated record states no tick")]
    );
    // the record starts inside the run: no record before it, a tickless one after
    let both = "no properties record for some of these dates, and the dated record states no tick \
                for the others";
    assert_eq!(kept_lines(tickless(150_000), true), [kept_line(SYM, "0.1", both)]);
}

/// Reads its own symbol's tick on every bar, as the order language does on every pass.
#[derive(Default)]
struct TickPerBar(Vec<Option<f64>>);

impl Strategy<OrderBrokerSimulator> for TickPerBar {
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, _bar: &vike_marketdata::Bar) {
        self.0.push(OrderBroker::instrument_grid(&*broker, "").and_then(|g| g.tick_size));
    }
}

/// A RUN THAT DOES NOT SNAP reads the dated record for its strategy's tick (`StrategyEngineConfig::dated_ticks`,
/// the owner's precedence of 2026-11-19) over the kept row, asking once per symbol per UTC day for the record
/// in force when the day began: a record observed at noon of day 0 answers from day 1 on, the kept row before
/// it, said once. A stated tick still wins, and then nothing is asked.
#[test]
fn a_run_that_does_not_snap_reads_the_dated_records_tick_once_a_day() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const HOUR: i64 = 3_600_000;
    let run = |params: StrategyEngineConfig| {
        // a minute past each hour, so hour 24 opens day 1
        let bar =
            |h: i64| vike_simulator::test_support::bar(h * HOUR + 60_000, 1.0, 1.0, 1.0, 1.0, 1.0);
        let bars = vec![(SYM.to_string(), (0..72).map(bar).collect())];
        let mut e = StrategyEngine::new(bars, TickPerBar::default(), params);
        let record = e.run();
        let lines: Vec<String> = record
            .data_warnings
            .into_iter()
            .filter(|l| l.contains("current instrument list"))
            .collect();
        (std::mem::take(&mut e.strategy.0), lines)
    };
    let dated = |asks: &Arc<AtomicUsize>| {
        let asks = Arc::clone(asks);
        StrategyEngineConfig {
            kept_ticks: kept(),
            dated_ticks: Some(Arc::new(move |symbol: &str, ts: i64| {
                assert_eq!(symbol, SYM, "asked under the run's slot name");
                asks.fetch_add(1, Ordering::Relaxed);
                (ts >= 12 * HOUR).then_some(0.05)
            })),
            ..Default::default()
        }
    };
    let asks = Arc::new(AtomicUsize::new(0));
    let (ticks, lines) = run(dated(&asks));
    let mut want = vec![Some(0.1); 24];
    want.extend([Some(0.05); 48]);
    assert_eq!(ticks, want, "day 0 the kept row's, days 1 and 2 the record's");
    assert_eq!(asks.load(Ordering::Relaxed), 3, "asked once a day, not per bar");
    let none = "no properties record in force when these UTC days began";
    assert_eq!(lines, [kept_line(SYM, "0.1", none)]);
    // a record that states no tick is a record: the kept row answers and the line says so
    let tickless = StrategyEngineConfig {
        dated_ticks: Some(Arc::new(|_: &str, _: i64| Some(0.0))),
        ..dated(&asks)
    };
    let (ticks, lines) = run(tickless);
    assert_eq!(ticks, vec![Some(0.1); 72]);
    assert_eq!(lines, [kept_line(SYM, "0.1", "the dated record states no tick")]);
    // `[risk]`'s tick wins, and the record is never asked
    let asks = Arc::new(AtomicUsize::new(0));
    let (ticks, lines) = run(StrategyEngineConfig { risk_limits: stated(), ..dated(&asks) });
    assert_eq!(ticks, vec![Some(0.5); 72]);
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(asks.load(Ordering::Relaxed), 0);
    // no kept row: the record's tick from day 1, none before it
    let (ticks, _) = run(StrategyEngineConfig { kept_ticks: Vec::new(), ..dated(&asks) });
    assert_eq!((ticks[23], ticks[24]), (None, Some(0.05)));
}
