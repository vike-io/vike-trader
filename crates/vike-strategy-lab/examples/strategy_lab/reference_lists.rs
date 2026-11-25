//! The closed-trade check: what `vike_trade_ledger` booked for a set-A strategy against the reference's own trade
//! list for it, row by row and in the trades analysis.
//!
//! The lists are the local research database's (tape `tv`), exported to `reference_lists.csv` beside the bars: never in the
//! repository, never shipped. A strategy agrees when its rows are the list's (the count, each row's net and commission to the
//! cent beyond the list's own rounding, its bars exactly) and the ledger's statistics over its closed rows, all, long and short,
//! equal the same statistics summed off the list's closed rows: counts exactly, money to the cent beyond the rounding of the
//! rows summed, ratios to the reference report's own precision. The closed-trade drawdown (the deepest fall of the closed rows'
//! cumulative net from the first) is part of that agreement. When the file carries the list's excursion columns, each closed row's run-up and
//! drawdown (favourable and adverse excursion), net percent and cumulative net are checked apart (`excursions_differ`).

use std::collections::HashMap;
use std::fs;

use vike_model::TradeClosedRow;
use vike_trade_ledger::{TradeClosedEntry, TradeClosedMetrics, TradeClosedStatistics};

/// One row of a reference list.
pub struct ListRow {
    side: i32,
    quantity: f64,
    net: f64,
    /// Half a unit of the last decimal the list printed `net` to.
    net_round: f64,
    commission: Option<(f64, f64)>,
    bars: Option<usize>,
    /// The list marks it open on its last bar.
    pub open: bool,
    entry_ms: i64,
    exit_ms: i64,
    /// Run-up, drawdown, net percent and cumulative net, each with half a unit of its last printed decimal, when the file
    /// carries them.
    excursion: Option<[(f64, f64); 4]>,
}

/// Half a unit of the last decimal of a number as written.
fn half_unit(text: &str) -> f64 {
    let decimals = text.split_once('.').map_or(0, |(_, frac)| frac.len());
    0.5 * 10f64.powi(-(decimals as i32))
}

/// Every strategy's list by its lab name, `None` when the file is not there.
pub fn read(path: &str) -> Option<HashMap<String, Vec<ListRow>>> {
    let text = fs::read_to_string(path).ok()?;
    let mut lists: HashMap<String, Vec<ListRow>> = HashMap::new();
    for l in text.lines().skip(1).filter(|l| !l.is_empty()) {
        let c: Vec<&str> = l.split(',').collect();
        let n = |i: usize| c[i].parse::<f64>().unwrap_or_else(|e| panic!("{l}: {e}"));
        let int = |i: usize| c[i].parse::<i64>().unwrap_or_else(|e| panic!("{l}: {e}"));
        let row = ListRow {
            side: if c[1] == "long" { 1 } else { -1 },
            entry_ms: int(2),
            exit_ms: int(4),
            quantity: n(6),
            net: n(7),
            net_round: half_unit(c[7]),
            commission: (!c[8].is_empty()).then(|| (n(8), half_unit(c[8]))),
            bars: (!c[9].is_empty()).then(|| int(9) as usize),
            open: c[10] == "1",
            excursion: (c.len() >= 15 && c[11..15].iter().all(|v| !v.is_empty()))
                .then(|| [11, 12, 13, 14].map(|i| (n(i), half_unit(c[i])))),
        };
        lists.entry(c[0].to_string()).or_default().push(row);
    }
    Some(lists)
}

/// To the cent, beyond the rounding `round` the reference's figure already carries.
fn cent(mine: f64, list: f64, round: f64) -> bool {
    (mine - list).abs() <= 0.005 + round + 1e-9
}

/// `None` when the run's rows are the list's, else the first difference. `rows` are the ledger's closed rows and then its
/// lots still open on the last bar, booked at that bar's close; `metrics` its statistics over the closed rows.
pub fn differs(
    rows: &[TradeClosedRow],
    metrics: &TradeClosedMetrics,
    list: &[ListRow],
) -> Option<String> {
    if rows.len() != list.len() {
        return Some(format!("{} rows against the list's {}", rows.len(), list.len()));
    }
    for (i, (r, l)) in rows.iter().zip(list).enumerate() {
        if !cent(r.net, l.net, l.net_round) {
            return Some(format!("row {i}: net {} against {}", r.net, l.net));
        }
        if let Some((c, round)) = l.commission
            && !cent(r.commission, c, round)
        {
            return Some(format!("row {i}: commission {} against {c}", r.commission));
        }
        if l.bars.is_some_and(|b| b != r.bars()) {
            return Some(format!("row {i}: {} bars against {:?}", r.bars(), l.bars));
        }
    }
    let closed: Vec<&ListRow> = list.iter().filter(|l| !l.open).collect();
    // From the first closed row on, as the ledger's close-to-close drawdown is.
    let (mut cum, mut peak, mut theirs) = (0.0_f64, f64::NEG_INFINITY, 0.0_f64);
    for l in &closed {
        cum += l.net;
        peak = peak.max(cum);
        theirs = theirs.max(peak - cum);
    }
    let round: f64 = closed.iter().map(|l| l.net_round).sum();
    if !cent(metrics.account.drawdown_close, theirs, round) {
        return Some(format!(
            "closed-trade drawdown {} against {theirs}",
            metrics.account.drawdown_close
        ));
    }
    for (side, mine) in [(None, &metrics.all), (Some(1), &metrics.long), (Some(-1), &metrics.short)]
    {
        let theirs: Vec<&ListRow> =
            closed.iter().copied().filter(|l| side.is_none_or(|s| s == l.side)).collect();
        if let Some(why) = stats_differ(mine, &theirs) {
            let column = match side {
                None => "all",
                Some(1) => "long",
                _ => "short",
            };
            return Some(format!("{column}: {why}"));
        }
    }
    None
}

/// The trades analysis summed off the list, plainly, against the ledger's.
fn stats_differ(mine: &TradeClosedStatistics, list: &[&ListRow]) -> Option<String> {
    let nets: Vec<f64> = list.iter().map(|l| l.net).collect();
    let round = |keep: &dyn Fn(f64) -> bool| -> f64 {
        list.iter().filter(|l| keep(l.net)).map(|l| l.net_round).sum()
    };
    let sum =
        |keep: &dyn Fn(f64) -> bool| -> f64 { nets.iter().copied().filter(|n| keep(*n)).sum() };
    let count = |keep: &dyn Fn(f64) -> bool| nets.iter().filter(|n| keep(**n)).count();
    let (wins, losses) = (count(&|n| n > 0.0), count(&|n| n < 0.0));
    let counts = [
        ("trades", mine.trades, list.len()),
        ("winners", mine.winners, wins),
        ("losers", mine.losers, losses),
        ("even", mine.even, list.len() - wins - losses),
    ];
    for (what, a, b) in counts {
        if a != b {
            return Some(format!("{what} {a} against {b}"));
        }
    }
    let n = list.len().max(1) as f64;
    let (gross_profit, gross_loss) = (sum(&|n| n > 0.0), -sum(&|n| n < 0.0));
    let commission: f64 = list.iter().filter_map(|l| l.commission.map(|(c, _)| c)).sum();
    let commission_round: f64 = list.iter().filter_map(|l| l.commission.map(|(_, r)| r)).sum();
    let largest_win = nets.iter().copied().fold(0.0, f64::max);
    let largest_loss = nets.iter().copied().fold(0.0, f64::min);
    let money = [
        ("net profit", mine.net_profit, sum(&|_| true), round(&|_| true)),
        ("gross profit", mine.gross_profit, gross_profit, round(&|n| n > 0.0)),
        ("gross loss", mine.gross_loss, gross_loss, round(&|n| n < 0.0)),
        ("expected payoff", mine.expected_payoff, sum(&|_| true) / n, round(&|_| true) / n),
        ("average win", mine.average_win, gross_profit / wins.max(1) as f64, round(&|n| n > 0.0)),
        (
            "average loss",
            mine.average_loss,
            -gross_loss / losses.max(1) as f64,
            round(&|n| n < 0.0),
        ),
        ("largest win", mine.largest_win, largest_win, round(&|n| n > 0.0)),
        ("largest loss", mine.largest_loss, largest_loss, round(&|n| n < 0.0)),
    ];
    for (what, a, b, rounding) in money {
        if !cent(a, b, rounding) {
            return Some(format!("{what} {a} against {b}"));
        }
    }
    if list.iter().all(|l| l.commission.is_some())
        && !cent(mine.commission, commission, commission_round)
    {
        return Some(format!("commission {} against {commission}", mine.commission));
    }
    let ratio = |a: f64, b: f64| if b > 0.0 { a / b } else { 0.0 };
    let profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else if gross_profit > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };
    let (average_win, average_loss) =
        (ratio(gross_profit, wins as f64), ratio(gross_loss, losses as f64));
    let ratios = [
        ("percent profitable", mine.percent_profitable, wins as f64 / n),
        ("profit factor", mine.profit_factor, profit_factor),
        ("win/loss ratio", mine.win_loss_ratio, ratio(average_win, average_loss)),
        ("largest win share", mine.largest_win_percent, ratio(largest_win, gross_profit)),
        ("largest loss share", mine.largest_loss_percent, ratio(-largest_loss, gross_loss)),
    ];
    for (what, a, b) in ratios {
        // The reference report prints these to 3 decimals (a ratio) or 2 of a percent.
        let same = if a.is_finite() && b.is_finite() {
            (a - b).abs() <= 5e-4 * b.abs().max(1.0)
        } else {
            a == b
        };
        if !same {
            return Some(format!("{what} {a} against {b}"));
        }
    }
    if list.iter().all(|l| l.bars.is_some()) {
        let bars = |keep: &dyn Fn(f64) -> bool| -> f64 {
            let held: Vec<f64> =
                list.iter().filter(|l| keep(l.net)).map(|l| l.bars.unwrap_or(0) as f64).collect();
            if held.is_empty() { 0.0 } else { held.iter().sum::<f64>() / held.len() as f64 }
        };
        let averages = [
            ("average bars", mine.average_bars, bars(&|_| true)),
            ("average bars of a win", mine.average_bars_win, bars(&|n| n > 0.0)),
            ("average bars of a loss", mine.average_bars_loss, bars(&|n| n < 0.0)),
        ];
        for (what, a, b) in averages {
            if (a - b).abs() > 1e-9 {
                return Some(format!("{what} {a} against {b}"));
            }
        }
    }
    let held = max_held(list);
    if (mine.max_held - held).abs() > 1e-3 * held.max(1.0) {
        return Some(format!("max held {} against {held}", mine.max_held));
    }
    None
}

/// The most the list held at once: its rows' quantities from entry to exit, an exit before an entry at one time unless it
/// is that entry's own (a round trip inside one bar).
fn max_held(list: &[&ListRow]) -> f64 {
    let exit_rank = |l: &ListRow| if l.exit_ms == l.entry_ms { 2 } else { 0 };
    let mut moves: Vec<(i64, i32, f64)> = list
        .iter()
        .flat_map(|l| [(l.entry_ms, 1, l.quantity), (l.exit_ms, exit_rank(l), -l.quantity)])
        .collect();
    moves.sort_by_key(|m| (m.0, m.1));
    let (mut now, mut most) = (0.0_f64, 0.0_f64);
    for (_, _, q) in moves {
        now += q;
        most = most.max(now);
    }
    most
}

/// `None` when every closed trade carries the list's run-up, drawdown, net percent and cumulative net (to the cent beyond the
/// list's rounding, the percent to its own two decimals), else the first difference; `None` too when the file has no such
/// columns. `trades` are the ledger's closed trades, in the order of the list's rows not marked open.
pub fn excursions_differ(trades: &[TradeClosedEntry], list: &[ListRow]) -> Option<String> {
    let closed: Vec<&ListRow> = list.iter().filter(|l| !l.open).collect();
    if closed.iter().any(|l| l.excursion.is_none()) {
        return None;
    }
    if trades.len() != closed.len() {
        return Some(format!("{} closed trades against the list's {}", trades.len(), closed.len()));
    }
    for (i, (t, l)) in trades.iter().zip(&closed).enumerate() {
        let [run_up, drawdown, pct, cum] = l.excursion.expect("checked above");
        let checks = [
            ("run-up", t.runup, run_up, 0.005),
            ("drawdown", t.drawdown, drawdown, 0.005),
            ("net %", t.net_percent * 100.0, pct, 0.0),
            ("cumulative net", t.cum_net, cum, 0.005),
        ];
        for (what, mine, (theirs, round), cents) in checks {
            if (mine - theirs).abs() > cents + round + 1e-9 {
                return Some(format!("row {i}: {what} {mine} against {theirs}"));
            }
        }
    }
    None
}
