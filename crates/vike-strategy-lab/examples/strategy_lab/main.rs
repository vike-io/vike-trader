//! Native vike strategies, each checked trade by trade against reference trade lists. A research harness, not product
//! code: nothing in the workspace names it.
//!
//! One row per strategy in `strategies/mod.rs`: a `STRATEGIES` row is a signal rule (`vike_tactics::signal`), market
//! orders of size 1 with no costs; a strategy that places real orders with costs and sizes is a row of `PLANS` instead, on
//! `desk.rs`, whose comparison also checks quantity and net result. A row's rule is the strategy gallery's
//! (its strategy file, `vike_strategy_lab::gallery`, decision 0269, W5) unless its file is in `strategies/`.
//!
//! The bars are Binance's own ETHUSDT USDⓈ-M perpetual klines, read through the datahub (`bars.rs`). The 15-minute ones run
//! from 2020-01-01. The 1-minute ones run from `strategies::FROM_2025_03_31` and are read only when a row that wants them
//! runs. Both stop where the lab's data stopped, on 2026-05-04.
//!
//! Every run, a signal strategy's and a desk row's, is a backtest profile's run through `vike_backtest` (`desk.rs`'s `//!`): the
//! bars are read once and served to each run by its profile's window (`desk::store_of`). A signal strategy's profile is
//! `Terms::PLAIN`'s with the simulator's in-bar fill rules off: market orders of size 1 at the next open, no costs.
//!
//! Every strategy's fills are also booked in `vike_trade_ledger`'s ledger and, when the reference's own lists are
//! in the data directory (`REFERENCE_LISTS`), checked against them (`reference_lists.rs`): an informational count, outside
//! RESULT.
//!
//! usage: cargo run -p vike-strategy-lab --example strategy_lab -- <dir with <name>.list_{a,b}.csv> [--dump DIR] [name...]
//! (`--dump`: `desk::DUMP_DIR`).

mod bars;
mod desk;
mod reference_lists;
mod strategies;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::rc::Rc;
use std::sync::Arc;

use desk::Terms;
use vike_data::HistoryStore;
use vike_marketdata::Bar;
use vike_model::{ContractValue, Fill, OrderEvent, Strategy, TradeClosedRow};
use vike_simulator::OrderBrokerSimulator;
use vike_tactics::{Rule, RuleRun, RuleRunOptions};
use vike_trade_ledger::{TradeClosedAccountTerms, TradeClosedLedger, TradeClosedLedgerOptions};

const SYMBOL: &str = "ETHUSDT";
const INTERVAL: &str = "15m";
const BAR_MS: i64 = 900_000;
/// The datahub's name for `SYMBOL`'s perpetual, and the span of its bars this lab reads. The first bar is 2020-01-01 00:00 UTC;
/// the last opens at 2026-05-04 15:00 (15 minutes) and 15:14 (1 minute).
const SERIES: &str = "ETHUSDT.P";
const FIRST_MS: i64 = 1_577_836_800_000;
const LAST_15M_MS: i64 = 1_777_906_800_000;
const LAST_1M_MS: i64 = 1_777_907_640_000;
/// The reference's own trade lists for the closed-trade check, in the data directory: research data, never in the
/// repository.
const REFERENCE_LISTS: &str = "reference_lists.csv";

/// A signal rule's run: the rule on the language's wrapper, its fills recorded as they come.
struct Runner {
    run: RuleRun<Box<dyn Rule + Send>>,
    fills: Rc<RefCell<Vec<Fill>>>,
}

impl Strategy<OrderBrokerSimulator> for Runner {
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        self.run.on_bar(broker, bar);
    }

    fn on_fill(&mut self, broker: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.borrow_mut().push(fill.clone());
        self.run.on_fill(broker, fill);
    }

    fn on_order_event(&mut self, broker: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.run.on_order_event(broker, event);
    }
}

#[derive(Clone, Debug)]
struct Trade {
    dir: i32,
    entry_ms: i64,
    entry_price: f64,
    exit_ms: i64,
    exit_price: f64,
}

impl Trade {
    fn pnl(&self) -> f64 {
        (self.exit_price - self.entry_price) * f64::from(self.dir)
    }
    fn same_as(&self, o: &Trade) -> bool {
        self.dir == o.dir
            && self.entry_ms == o.entry_ms
            && self.exit_ms == o.exit_ms
            && (self.entry_price - o.entry_price).abs() < 1e-9
            && (self.exit_price - o.exit_price).abs() < 1e-9
    }
}

/// Fills to round trips. A reversal fill closes one trade and opens the next at the same time and price. A position
/// still open on the last bar is booked at that bar's close, as the reference lists do.
fn trades_from_fills(fills: &[Fill], last: &Bar) -> Vec<Trade> {
    let mut out = Vec::new();
    let mut position = 0.0_f64;
    let mut open: Option<(i32, i64, f64)> = None;
    for f in fills {
        let next = position + f64::from(f.side) * f.size;
        if position != 0.0 && (next == 0.0 || next.signum() != position.signum()) {
            let (dir, entry_ms, entry_price) = open.take().expect("a position has an entry");
            out.push(Trade {
                dir,
                entry_ms,
                entry_price,
                exit_ms: f.timestamp,
                exit_price: f.price,
            });
        }
        if next != 0.0 && (position == 0.0 || next.signum() != position.signum()) {
            open = Some((if next > 0.0 { 1 } else { -1 }, f.timestamp, f.price));
        }
        position = next;
    }
    if let Some((dir, entry_ms, entry_price)) = open {
        out.push(Trade {
            dir,
            entry_ms,
            entry_price,
            exit_ms: last.timestamp,
            exit_price: last.close,
        });
    }
    out
}

fn read_list(path: &str) -> Vec<Trade> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .skip(1)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split(',').collect();
            Trade {
                dir: if c[0] == "long" { 1 } else { -1 },
                entry_ms: c[1].parse().unwrap_or_else(|e| panic!("{l}: {e}")),
                entry_price: c[2].parse().unwrap_or_else(|e| panic!("{l}: {e}")),
                exit_ms: c[3].parse().unwrap_or_else(|e| panic!("{l}: {e}")),
                exit_price: c[4].parse().unwrap_or_else(|e| panic!("{l}: {e}")),
            }
        })
        .collect()
}

/// One signal strategy's run: its round trips, and its fills booked in the closed-trade ledger with the lots still open on
/// the last bar booked at its close.
struct Ran {
    trades: Vec<Trade>,
    booked: (TradeClosedLedger, Vec<TradeClosedRow>),
}

/// `options` are the run options of the gallery file the rule is (`vike_strategy_lab::GalleryFile::run`), else the defaults.
fn run(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    rule: Box<dyn Rule + Send>,
    options: RuleRunOptions,
    warm_from_ms: i64,
    start_ms: i64,
    end_ms: i64,
) -> Ran {
    let terms = Terms { cash: params_cash(), ..Terms::PLAIN };
    let bars = desk::window_bars(store, &terms, warm_from_ms, end_ms);
    let fills = Rc::new(RefCell::new(Vec::new()));
    let run = RuleRun::new(rule, SYMBOL).trading(start_ms, end_ms).with(options);
    let strategy = Runner { run, fills: Rc::clone(&fills) };
    let last = bars.last().expect("bars").clone();
    let times: Vec<i64> = bars.iter().map(|b| b.timestamp).collect();
    let walks: Vec<([f64; 4], f64)> = bars.iter().map(|b| (b.path_points(), b.close)).collect();
    let profile = desk::profile_of(&terms, warm_from_ms, end_ms, false);
    let _result = desk::run_profile(&profile, store, Box::new(strategy), None);
    let collected = fills.borrow();
    // The same fills, booked one lot per entry, first in first out (these strategies pay no costs), each bar marked after its
    // fills (all at its open) for the trades' excursions.
    let unit = ContractValue::linear(1.0);
    let mut ledger = TradeClosedLedger::new(TradeClosedLedgerOptions::default())
        .with_account(TradeClosedAccountTerms::new(params_cash()));
    let mut next = 0;
    for (i, (walk, close)) in walks.iter().enumerate() {
        while let Some(f) = collected.get(next)
            && times.partition_point(|t| *t <= f.timestamp).saturating_sub(1) == i
        {
            ledger.apply(f, "", i, unit, 1.0);
            next += 1;
        }
        ledger.mark_bar(SYMBOL, i, walk);
        ledger.mark_close(times[i], |_| Some((*close, unit)));
    }
    assert_eq!(next, collected.len(), "every fill falls in a bar");
    let open = desk::open_rows(
        &ledger.metrics(|_| None).open_trades,
        &last,
        times.len() - 1,
        unit,
        |_| 0.0,
    );
    Ran { trades: trades_from_fills(&collected, &last), booked: (ledger, open) }
}

/// A signal strategy's account: a size of 1 never runs short of it.
fn params_cash() -> f64 {
    1_000_000.0
}

/// The closed-trade check of one strategy against the reference's own list for it: `None` when they agree.
fn reference_differs(
    lists: &HashMap<String, Vec<reference_lists::ListRow>>,
    name: &str,
    (ledger, open): &(TradeClosedLedger, Vec<TradeClosedRow>),
) -> (Option<String>, Option<String>) {
    let Some(list) = lists.get(name) else {
        return (Some("no reference list".to_string()), None);
    };
    let rows: Vec<TradeClosedRow> = ledger.rows().iter().chain(open).cloned().collect();
    // A list books a lot still open on its last bar as a closed row unless it marks it open: those lots are closed here at that
    // row's price and exit commission, so the ledger's statistics are over the rows the list counts closed.
    let mut closed = ledger.clone();
    let listed = list.get(ledger.rows().len()..).unwrap_or_default();
    for ((r, lot), l) in open.iter().zip(ledger.lots()).zip(listed) {
        if !l.open {
            let fill = Fill {
                side: -r.side,
                size: r.qty,
                price: r.exit_price,
                fee: r.commission - lot.entry_fee_left(),
                timestamp: r.exit_timestamp,
                is_maker: false,
                symbol: r.symbol.clone(),
                tag: None,
            };
            closed.apply(&fill, "", r.exit_bar, ContractValue::linear(1.0), 1.0);
        }
    }
    let metrics = closed.metrics(|_| None);
    (
        reference_lists::differs(&rows, &metrics, list),
        reference_lists::excursions_differ(&metrics.trades, list),
    )
}

/// How many trades are identical, position by position, and the first few that are not.
fn compare(mine: &[Trade], list: &[Trade]) -> (usize, Vec<String>) {
    let mut same = 0;
    let mut diffs = Vec::new();
    for i in 0..mine.len().max(list.len()) {
        match (mine.get(i), list.get(i)) {
            (Some(a), Some(b)) if a.same_as(b) => same += 1,
            (a, b) if diffs.len() < 3 => {
                diffs.push(format!("  #{i}: vike {a:?}\n       list {b:?}"))
            }
            _ => {}
        }
    }
    (same, diffs)
}

fn main() {
    desk::self_check();
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: strategy_lab <dir> [--dump DIR] [name...]");
    let mut only: Vec<String> = args.collect();
    if let Some(i) = only.iter().position(|a| a == "--dump") {
        let to = only.drain(i..(i + 2).min(only.len())).nth(1).expect("--dump takes a directory");
        fs::create_dir_all(&to).unwrap_or_else(|e| panic!("{to}: {e}"));
        desk::DUMP_DIR.set(to).expect("one dump directory");
    }
    let bars = bars::read("binance", SERIES, INTERVAL, Some(FIRST_MS), LAST_15M_MS);
    println!("bars read: {}", bars.len());
    // Over half a million bars, and no list trades before this: the others stay fast.
    let wanted = |e: &strategies::PlanEntry| only.is_empty() || only.iter().any(|s| s == e.name);
    let minutes = if strategies::PLANS.iter().any(|e| e.minutes && wanted(e)) {
        bars::read("binance", SERIES, "1m", Some(strategies::FROM_2025_03_31), LAST_1M_MS)
    } else {
        Vec::new()
    };
    println!("1-minute bars read: {}", minutes.len());
    // The perpetual is held under the name the lists and the rows trade (`desk::store_of`).
    let store = desk::store_of([(SYMBOL, INTERVAL, bars.as_slice()), (SYMBOL, "1m", &minutes)]);
    let minutes: Rc<[Bar]> = minutes.into();

    // The reference's own trade lists for the closed-trade check, when they are beside the bars (`reference_lists.rs`).
    let lists = reference_lists::read(&format!("{dir}/{REFERENCE_LISTS}"));
    let mut differing: Vec<(&str, String)> = Vec::new();
    // Of the strategies that agree, the ones whose trades' excursions do not.
    let mut excursions: Vec<(&str, String)> = Vec::new();
    let mut note = |name: &'static str, booked: &(TradeClosedLedger, Vec<TradeClosedRow>)| {
        let Some(l) = lists.as_ref() else { return };
        match reference_differs(l, name, booked) {
            (Some(why), _) => differing.push((name, why)),
            (None, Some(why)) => excursions.push((name, why)),
            (None, None) => {}
        }
    };
    let mut all_ok = true;
    let mut ran = 0;
    for entry in strategies::STRATEGIES {
        if !only.is_empty() && !only.iter().any(|s| s == entry.name) {
            continue;
        }
        ran += 1;
        let first = read_list(&format!("{dir}/{}.list_a.csv", entry.name));
        let second = read_list(&format!("{dir}/{}.list_b.csv", entry.name));
        // Trading starts on the signal bar of the list's first trade: its range start is not recorded per strategy.
        let start_ms = second[0].entry_ms - BAR_MS;
        let options = vike_strategy_lab::gallery_file(&desk::strategies_dir(), entry.name)
            .map_or(RuleRunOptions::OFF, |row| row.run);
        let Ran { trades: mine, booked } =
            run(&store, (entry.build)(), options, entry.warm_from_ms, start_ms, entry.end_ms);
        note(entry.name, &booked);
        let net = |t: &[Trade]| t.iter().map(Trade::pnl).sum::<f64>();
        // Where the two lists disagree they cannot both match: list-b is the reference, list-a is only shown.
        let (agreeing, _) = compare(&first, &second);
        let lists_agree = agreeing == first.len() && first.len() == second.len();
        for (list_name, list) in [("list-a", &first), ("list-b", &second)] {
            let (same, diffs) = compare(&mine, list);
            let ok = same == list.len() && mine.len() == list.len();
            let counts = lists_agree || list_name == "list-b";
            all_ok &= ok || !counts;
            println!(
                "{} {:<32} vs {list_name} vike={} list={} identical={same} net vike={:.2} list={:.2}",
                if ok {
                    "PASS"
                } else if counts {
                    "FAIL"
                } else {
                    "INFO"
                },
                entry.name,
                mine.len(),
                list.len(),
                net(&mine),
                net(list),
            );
            for d in diffs {
                println!("{d}");
            }
        }
    }
    for entry in strategies::PLANS {
        if only.is_empty() || only.iter().any(|s| s == entry.name) {
            ran += 1;
            let (ok, ledger, open) = desk::check(&dir, &store, &minutes, entry);
            all_ok &= ok;
            note(entry.name, &(ledger, open));
        }
    }
    assert!(
        ran > 0 && (only.is_empty() || ran == only.len()),
        "ran {ran} of the named strategies {only:?}"
    );
    // Informational: it changes no PASS line and not the RESULT.
    match &lists {
        None => println!("REFERENCE CHECK: skipped, no {REFERENCE_LISTS} beside the bars"),
        Some(_) => {
            for (name, why) in &differing {
                println!("REFERENCE DIFF {name:<40} {why}");
            }
            println!(
                "REFERENCE CHECK: {} of {ran} strategies agree with the reference's own trade lists (rows: count, net, \
                 commission, bars; trades analysis all/long/short; the closed-trade drawdown)",
                ran - differing.len()
            );
            for (name, why) in &excursions {
                println!("REFERENCE EXCURSION DIFF {name:<40} {why}");
            }
            println!(
                "REFERENCE EXCURSIONS: {} of those {} carry the list's run-up, drawdown, net % and cumulative net on every \
                 closed trade",
                ran - differing.len() - excursions.len(),
                ran - differing.len()
            );
        }
    }

    // The control: a wrong period must NOT match, or the comparison above proves nothing.
    let list = read_list(&format!("{dir}/{}.list_a.csv", strategies::CONTROL_NAME));
    let wrong = run(
        &store,
        strategies::control_wrong_period(),
        RuleRunOptions::OFF,
        strategies::FROM_START,
        list[0].entry_ms - BAR_MS,
        strategies::END_2026_05_01,
    )
    .trades;
    let (same, _) = compare(&wrong, &list);
    let control_ok = same < list.len();
    all_ok &= control_ok;
    println!(
        "{} control: SMA(151) against the SMA(152) list must differ: vike={} list={} identical={same}",
        if control_ok { "PASS" } else { "FAIL" },
        wrong.len(),
        list.len(),
    );
    all_ok &= desk::cost_control(&dir, &store, &strategies::COST_CONTROL);

    println!("{}", if all_ok { "RESULT: ALL IDENTICAL" } else { "RESULT: DIFFERENCES FOUND" });
    std::process::exit(i32::from(!all_ok));
}
