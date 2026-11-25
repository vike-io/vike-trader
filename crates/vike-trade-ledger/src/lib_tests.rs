use super::*;

const PLAIN: TradeClosedLedgerOptions = TradeClosedLedgerOptions { whole_entry_fee: false };

fn fill(side: i32, size: f64, price: f64, fee: f64, ts: i64) -> Fill {
    Fill {
        side,
        size,
        price,
        fee,
        timestamp: ts,
        is_maker: false,
        symbol: "ETHUSDT".to_string(),
        tag: None,
    }
}

/// One fill at par: a linear unit worth its price, the fee in the account's money.
fn book(ledger: &mut TradeClosedLedger, f: &Fill, tag: &str, bar: usize) -> Vec<TradeClosedRow> {
    ledger.apply(f, tag, bar, ContractValue::linear(1.0), 1.0).to_vec()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn pyramided_entries_close_oldest_first_one_row_per_lot() {
    let mut l = TradeClosedLedger::new(PLAIN);
    book(&mut l, &fill(1, 1.0, 10.0, 0.0, 1), "A", 1);
    book(&mut l, &fill(1, 1.0, 11.0, 0.0, 2), "B", 2);
    book(&mut l, &fill(1, 1.0, 12.0, 0.0, 3), "C", 3);
    let rows = book(&mut l, &fill(-1, 2.0, 13.0, 0.0, 4), "", 5);
    let got: Vec<(&str, f64, f64)> =
        rows.iter().map(|r| (r.tag.as_str(), r.entry_price, r.net)).collect();
    assert_eq!(got, [("A", 10.0, 3.0), ("B", 11.0, 2.0)], "first in, first out, a row per lot");
    assert_eq!(l.lots().len(), 1);
    assert_eq!((l.lots()[0].tag.as_str(), l.lots()[0].qty), ("C", 1.0), "the youngest stays open");
    assert_eq!(rows[0].bars(), 4, "bars from the entry's to the exit's");
}

#[test]
fn a_partial_exit_splits_a_lot_and_both_fees_by_quantity() {
    let mut l = TradeClosedLedger::new(PLAIN);
    // 0.4 entry fee on 4 units (0.1 a unit), 0.2 exit fee on 1 unit, then 0.6 on the last 3.
    book(&mut l, &fill(1, 4.0, 100.0, 0.4, 1), "A", 0);
    let first = book(&mut l, &fill(-1, 1.0, 110.0, 0.2, 2), "", 1);
    assert_eq!(first.len(), 1);
    assert!(close(first[0].pnl, 10.0));
    assert!(close(first[0].commission, 0.1 + 0.2), "{}", first[0].commission);
    assert!(close(first[0].net, 10.0 - 0.1 - 0.2), "{}", first[0].net);
    assert!(close(l.lots()[0].entry_fee_left(), 0.3), "three units' share stays on the lot");
    let rest = book(&mut l, &fill(-1, 3.0, 90.0, 0.6, 3), "", 2);
    assert!(close(rest[0].net, -30.0 - 0.3 - 0.6), "{}", rest[0].net);
    assert!(l.lots().is_empty());
}

#[test]
fn the_whole_entry_fee_goes_to_the_first_row_that_closes_a_lot() {
    let mut l = TradeClosedLedger::new(TradeClosedLedgerOptions { whole_entry_fee: true });
    book(&mut l, &fill(1, 4.0, 100.0, 0.4, 1), "A", 0);
    let first = book(&mut l, &fill(-1, 1.0, 110.0, 0.2, 2), "", 1);
    assert!(close(first[0].net, 10.0 - 0.4 - 0.2), "{}", first[0].net);
    assert_eq!(l.lots()[0].entry_fee_left(), 0.0, "nothing left to charge");
    let rest = book(&mut l, &fill(-1, 3.0, 90.0, 0.6, 3), "", 2);
    assert!(close(rest[0].net, -30.0 - 0.6), "{}", rest[0].net);
    // Once the lot is closed both readings sum to the same net.
    let mut shared = TradeClosedLedger::new(PLAIN);
    for (f, tag) in [(fill(1, 4.0, 100.0, 0.4, 1), "A"), (fill(-1, 1.0, 110.0, 0.2, 2), "")] {
        book(&mut shared, &f, tag, 0);
    }
    book(&mut shared, &fill(-1, 3.0, 90.0, 0.6, 3), "", 0);
    let sum = |l: &TradeClosedLedger| compensated_sum(l.rows().iter().map(|r| r.net));
    assert!(close(sum(&l), sum(&shared)));
}

#[test]
fn a_flip_closes_then_opens_with_the_rest_of_the_fill_and_its_fee_share() {
    let mut l = TradeClosedLedger::new(PLAIN);
    book(&mut l, &fill(1, 1.0, 10.0, 0.1, 1), "A", 0);
    book(&mut l, &fill(1, 1.0, 11.0, 0.1, 2), "B", 1);
    // 3 units sold at 12, 0.3 commission: closes A and B, opens 1 short with 0.1 of the fee.
    let rows = book(&mut l, &fill(-1, 3.0, 12.0, 0.3, 3), "S", 2);
    let nets: Vec<f64> = rows.iter().map(|r| r.net).collect();
    assert_eq!(nets.len(), 2);
    assert!(close(nets[0], 2.0 - 0.1 - 0.1) && close(nets[1], 1.0 - 0.1 - 0.1), "{nets:?}");
    let lot = &l.lots()[0];
    assert_eq!((lot.side, lot.qty, lot.tag.as_str()), (-1, 1.0, "S"), "a reversal opens a lot");
    assert!(close(lot.entry_fee_left(), 0.1));
    let short = book(&mut l, &fill(1, 1.0, 11.5, 0.05, 4), "", 3);
    assert_eq!(short[0].side, -1);
    assert!(close(short[0].net, 0.5 - 0.1 - 0.05), "{}", short[0].net);
}

#[test]
fn lots_of_another_symbol_are_not_closed() {
    let mut l = TradeClosedLedger::new(PLAIN);
    book(&mut l, &fill(1, 1.0, 10.0, 0.0, 1), "", 0);
    let btc = Fill { symbol: "BTCUSDT".to_string(), ..fill(-1, 1.0, 50.0, 0.0, 2) };
    assert!(book(&mut l, &btc, "", 1).is_empty(), "a short of another symbol opens");
    assert_eq!(l.lots().len(), 2);
}

#[test]
fn a_fill_of_no_quantity_books_nothing() {
    let mut l = TradeClosedLedger::new(PLAIN);
    book(&mut l, &fill(1, 1.0, 10.0, 0.0, 1), "", 0);
    assert!(book(&mut l, &fill(-1, 0.0, 11.0, 0.0, 2), "", 1).is_empty());
    assert!(book(&mut l, &fill(-1, f64::NAN, 11.0, 0.0, 2), "", 1).is_empty());
    assert_eq!(l.lots().len(), 1);
}

#[test]
fn the_account_rate_converts_the_price_pnl_and_the_fee_rate_the_commission() {
    let mut l = TradeClosedLedger::new(PLAIN);
    let half = ContractValue::linear(1.0).in_account(0.5).expect("a rate");
    l.apply(&fill(1, 1.0, 10.0, 0.2, 1), "", 0, half, 0.5);
    let rows = l.apply(&fill(-1, 1.0, 12.0, 0.2, 2), "", 1, half, 1.0).to_vec();
    // (12 - 10) at 0.5, less the entry fee at its own 0.5 and the exit fee at 1.0.
    assert!(close(rows[0].net, 1.0 - 0.1 - 0.2), "{}", rows[0].net);
}

#[test]
fn the_statistics_count_rows_net_split_long_and_short_and_keep_open_lots_apart() {
    let mut l = TradeClosedLedger::new(PLAIN);
    // Long 2 at 100 in two lots, closed at 110 (bar 4): two winners of 10 less 1 commission each.
    book(&mut l, &fill(1, 1.0, 100.0, 0.5, 1), "", 0);
    book(&mut l, &fill(1, 1.0, 100.0, 0.5, 2), "", 2);
    book(&mut l, &fill(-1, 2.0, 110.0, 1.0, 3), "", 4);
    // A short of 1 at 100 closed at 105 (bar 6): a loser of 5 and 0.5 + 0.5.
    book(&mut l, &fill(-1, 1.0, 100.0, 0.5, 4), "", 5);
    book(&mut l, &fill(1, 1.0, 105.0, 0.5, 5), "", 6);
    // A free round trip at one price: even.
    book(&mut l, &fill(1, 1.0, 100.0, 0.0, 6), "", 7);
    book(&mut l, &fill(-1, 1.0, 100.0, 0.0, 7), "", 8);
    // Still open: long 3 at 100, marked at 104.
    book(&mut l, &fill(1, 3.0, 100.0, 0.3, 8), "", 9);
    let m = l.metrics(|_| Some((104.0, ContractValue::linear(1.0))));
    let all = &m.all;
    assert_eq!((all.trades, all.winners, all.losers, all.even), (4, 2, 1, 1));
    assert!(close(all.percent_profitable, 0.5), "even rows stay in the denominator");
    assert!(close(all.gross_profit, 18.0) && close(all.gross_loss, 6.0));
    assert!(close(all.net_profit, 12.0) && close(all.expected_payoff, 3.0));
    assert!(close(all.profit_factor, 3.0));
    assert!(
        close(all.average_win, 9.0)
            && close(all.average_loss, -6.0)
            && close(all.win_loss_ratio, 1.5)
    );
    assert!(close(all.largest_win, 9.0) && close(all.largest_loss, -6.0));
    assert!(close(all.largest_win_percent, 0.5) && close(all.largest_loss_percent, 1.0));
    // Bars: 4 and 2 (the longs), 1 (the short), 1 (the even one).
    assert!(
        close(all.average_bars, 2.0)
            && close(all.average_bars_win, 3.0)
            && close(all.average_bars_loss, 1.0)
    );
    assert!(close(all.commission, 3.0), "{}", all.commission);
    assert_eq!(all.open_trades, 1);
    assert!(close(all.open_pnl, 12.0), "the open lot's price P&L at the mark");
    assert!(
        close(all.max_held, 3.0) && close(m.long.max_held, 3.0) && close(m.short.max_held, 1.0)
    );
    assert_eq!((m.long.trades, m.long.winners, m.long.even, m.long.open_trades), (3, 2, 1, 1));
    assert_eq!((m.short.trades, m.short.losers, m.short.open_trades), (1, 1, 0));
    assert!(close(m.short.net_profit, -6.0) && m.short.profit_factor == 0.0);
    assert_eq!(m.long.profit_factor, f64::INFINITY, "nothing lost, something won");
    assert_eq!(m.value_of("closed_net_profit"), Some(all.net_profit));
    assert_eq!(m.value_of("net_profit"), None, "only the closed ids");
    // ...and the open lot is LISTED, with every fact a reader needs to book it its own way.
    assert_eq!(m.trades.len(), 4, "the open lot is no closed row");
    assert_eq!(
        m.open_trades,
        vec![LotOpenEntry {
            symbol: "ETHUSDT".into(),
            tag: String::new(),
            side: 1,
            entry_timestamp: 8,
            entry_bar: 9,
            entry_price: 100.0,
            qty: 3.0,
            entry_fee: 0.3,
            price: Some(104.0),
            pnl: all.open_pnl,
        }]
    );
}

/// The trades still open at the end are listed oldest entry first, each with what is left of it and
/// the entry commission it still carries (none once `whole_entry_fee` booked it), and they survive
/// the wire. A lot the mark cannot price is listed with no price and no P&L.
#[test]
fn the_trades_still_open_are_listed_with_what_is_left_of_them() {
    for (options, entry_fee) in
        [(PLAIN, 1.0), (TradeClosedLedgerOptions { whole_entry_fee: true }, 0.0)]
    {
        let mut l = TradeClosedLedger::new(options);
        // Long 4 at 100 (fee 2.0), half of it closed at 110; then a short lot of 1 at 120.
        book(&mut l, &fill(1, 4.0, 100.0, 2.0, 1), "Long", 0);
        book(&mut l, &fill(-1, 2.0, 110.0, 0.0, 2), "", 3);
        let mut btc_short = fill(-1, 1.0, 120.0, 0.5, 3);
        btc_short.symbol = "BTCUSDT".into();
        book(&mut l, &btc_short, "Short", 4);
        let m = l.metrics(|s| (s == "ETHUSDT").then_some((105.0, ContractValue::linear(1.0))));
        let open: Vec<(&str, f64, f64, Option<f64>)> =
            m.open_trades.iter().map(|o| (o.tag.as_str(), o.qty, o.entry_fee, o.price)).collect();
        assert_eq!(
            open,
            vec![("Long", 2.0, entry_fee, Some(105.0)), ("Short", 1.0, 0.5, None)],
            "{options:?}"
        );
        assert!(close(m.open_trades[0].pnl, 10.0) && m.open_trades[1].pnl == 0.0);
        assert_eq!(m.all.open_trades, m.open_trades.len());
        let back: TradeClosedMetrics =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back.open_trades, m.open_trades);
    }
}

#[test]
fn a_walk_range_runs_from_the_first_reach_of_one_price_to_the_next_reach_of_the_other() {
    let walk = [103.0, 100.0, 109.0, 106.0];
    assert_eq!(walk_range(&walk, None, None), (100.0, 109.0));
    assert_eq!(walk_range(&walk, None, Some(101.5)), (101.5, 103.0), "out on the way down");
    assert_eq!(walk_range(&walk, None, Some(103.0)), (103.0, 103.0), "out at the open");
    assert_eq!(walk_range(&walk, Some(104.0), None), (104.0, 109.0), "in on the way up");
    assert_eq!(
        walk_range(&walk, Some(101.0), Some(108.0)),
        (100.0, 108.0),
        "in and out in one bar"
    );
    assert_eq!(walk_range(&walk, None, Some(120.0)), (100.0, 109.0), "a price the walk never met");
}

const DAY: i64 = 86_400_000;

/// Daily bars of one symbol: a long of 2 bought at 102 (fee 0.2) on bar 1's open and stopped out at
/// 101.5 (fee 0.2) on the way down of bar 3, then a short of 1 sold at 104 on the way up of bar 4
/// and bought back at bar 5's open at 103. Capital 1,000.
fn two_trades() -> TradeClosedLedger {
    two_trades_on(TradeClosedAccountTerms::new(1000.0))
}

fn two_trades_on(terms: TradeClosedAccountTerms) -> TradeClosedLedger {
    let mut l = TradeClosedLedger::new(PLAIN).with_account(terms);
    let walks = [
        [100.0, 99.0, 101.0, 100.0],
        [102.0, 101.0, 105.0, 104.0],
        [104.0, 103.5, 106.0, 105.0],
        [103.0, 100.0, 109.0, 106.0],
        [102.0, 101.0, 106.0, 105.0],
        [103.0, 102.0, 104.0, 103.5],
    ];
    let fills = [
        (1, fill(1, 2.0, 102.0, 0.2, DAY)),
        (3, fill(-1, 2.0, 101.5, 0.2, 3 * DAY)),
        (4, fill(-1, 1.0, 104.0, 0.0, 4 * DAY)),
        (5, fill(1, 1.0, 103.0, 0.0, 5 * DAY)),
    ];
    for (bar, walk) in walks.iter().enumerate() {
        for (_, f) in fills.iter().filter(|(b, _)| *b == bar) {
            book(&mut l, f, "", bar);
        }
        l.mark_bar("ETHUSDT", bar, walk);
        l.mark_close(bar as i64 * DAY, |_| Some((walk[3], ContractValue::linear(1.0))));
    }
    l
}

#[test]
fn a_trade_meets_the_bars_it_was_held_through_and_its_exit_bar_up_to_the_exit() {
    let m = two_trades().metrics(|_| Some((103.5, ContractValue::linear(1.0))));
    let [long, short] = &m.trades[..] else { panic!("{:?}", m.trades) };
    // Met 101 .. 106 (bar 3's 100 and 109 came after the stop): (106 - 102) * 2 and (101 - 102) * 2,
    // less the entry commission.
    assert!(close(long.runup, 7.8) && close(long.drawdown, -2.2), "{long:?}");
    assert!(
        close(long.net, -1.4) && close(long.net_percent, -1.4 / 204.0) && close(long.cum_net, -1.4)
    );
    assert_eq!(long.bars, 2);
    // In at 104 on the way up of bar 4, so not its 101: 104 .. 106, and bar 5's open alone.
    assert!(close(short.runup, 1.0) && close(short.drawdown, -2.0), "{short:?}");
    assert!(close(short.cum_net, -0.4) && short.side == -1);
}

#[test]
fn the_account_figures_of_two_trades() {
    let a = two_trades().metrics(|_| Some((103.5, ContractValue::linear(1.0)))).account;
    // Intrabar, from the closed-trade equity's peak and trough (the capital until the long closes): to the short at
    // bar 4's high, -1.4 - 2, and to the long at bar 2's high, 7.8. The equity at the closes (5.8 at bar 2) is no peak.
    assert!(
        close(a.drawdown, 3.4)
            && close(a.drawdown_percent, 0.0034)
            && close(a.drawdown_init_percent, 0.0034),
        "{a:?}"
    );
    assert!(
        close(a.runup, 7.8)
            && close(a.runup_percent, 0.0078)
            && close(a.runup_init_percent, 0.0078)
    );
    assert!(
        close(a.total_pnl, -0.4)
            && close(a.total_return, -0.0004)
            && close(a.net_profit_percent, -0.0004)
    );
    // The closed-trade equity: 998.6 after the long, 999.6 after the short; it never falls.
    assert!(close(a.drawdown_close, 0.0) && close(a.drawdown_close_percent, 0.0));
    assert!(close(a.runup_close, 1.0) && close(a.runup_close_percent, 1.0 / 998.6));
    assert!(close(a.average_drawdown_percent, 0.0) && close(a.average_drawdown_days, 0.0));
    assert!(close(a.average_runup_percent, 1.0 / 998.6) && close(a.average_runup_days, 2.0));
    assert!(close(a.runup_now_percent, 1.0 / 998.6));
    // One month (-0.04%): no deviation for the Sharpe; its whole shortfall below 2%/12 for the Sortino.
    assert_eq!(a.sharpe, 0.0);
    assert!(close(a.sortino, -1.0), "{}", a.sortino);
    assert!(close(a.cagr, libm::pow(0.9996, 365.0 / 5.0) - 1.0));
    assert!(close(a.buy_hold_percent, 1.5 / 102.0) && close(a.buy_hold, 1000.0 * 1.5 / 102.0));
    assert!(close(a.outperformance, -0.0004 - 1.5 / 102.0));
    let m = two_trades().metrics(|_| None);
    assert_eq!(m.value_of("closed_drawdown"), Some(m.account.drawdown));
    // Buy and hold from the first entry (102) at each close after it: 104, 105, 106, 105, 103.5.
    let line: Vec<(i64, f64)> = m.buy_hold_curve.clone();
    assert_eq!(line.len(), 5, "{line:?}");
    assert!(line[0].0 == DAY && close(line[0].1, 1000.0 * 104.0 / 102.0));
    assert!(line[4].0 == 5 * DAY && close(line[4].1, 1000.0 * 103.5 / 102.0));
}

/// The margin at each close from the first entry's bar (1): 2 x 104 and 2 x 105 at half, nothing,
/// 1 x 105 at all of it, nothing; five bars. Buy and hold where the closed-trade equity ends: the
/// short's exit at 103, against the last close's 103.5.
#[test]
fn the_margin_used_and_buy_and_hold_where_the_equity_ends() {
    let terms = TradeClosedAccountTerms {
        margin_long: 0.5,
        margin_short: 1.0,
        ..TradeClosedAccountTerms::new(1000.0)
    };
    let a = two_trades_on(terms).metrics(|_| Some((103.5, ContractValue::linear(1.0)))).account;
    assert!(close(a.average_margin, (104.0 + 105.0 + 105.0) / 5.0), "{}", a.average_margin);
    assert!(close(a.margin_eff, -0.4 / a.average_margin));
    assert!(close(a.buy_hold_chart, 1000.0 * (103.0 / 102.0 - 1.0)), "{}", a.buy_hold_chart);
    assert!(close(a.buy_hold, 1000.0 * (103.5 / 102.0 - 1.0)));
    let none = two_trades().metrics(|_| Some((103.5, ContractValue::linear(1.0)))).account;
    assert_eq!((none.average_margin, none.margin_eff), (0.0, 0.0), "no margin stated, none used");
}

/// Ten round trips of one unit at 100 on 1,000: +1 +1 +1 -1 +2 0 -1 -1 +30 -1. Net percents mean
/// 3.1%, sample deviation 9.5155%: only the +30% lies past two of them.
#[test]
fn expectancy_outliers_their_averages_and_the_streaks() {
    let mut l = TradeClosedLedger::new(PLAIN).with_account(TradeClosedAccountTerms::new(1000.0));
    let nets = [1.0, 1.0, 1.0, -1.0, 2.0, 0.0, -1.0, -1.0, 30.0, -1.0];
    for (i, net) in nets.iter().enumerate() {
        let t = 2 * i as i64;
        book(&mut l, &fill(1, 1.0, 100.0, 0.0, t), "", 0);
        book(&mut l, &fill(-1, 1.0, 100.0 + net, 0.0, t + 1), "", 0);
    }
    let all = l.metrics(|_| None).all;
    assert!(close(all.expectancy_percent, 0.031), "{}", all.expectancy_percent);
    assert_eq!(all.outliers, 1);
    assert!(close(all.outliers_pnl, 30.0) && close(all.outliers_pnl_percent, 0.03));
    assert!(
        close(all.average_win_percent, 0.0125) && close(all.average_loss_percent, -0.01),
        "{all:?}"
    );
    // Winning runs 3, 1, 1 (the even row ends one); losing runs 1, 2, 1.
    assert_eq!((all.win_streak, all.loss_streak), (3, 2));
    assert!(close(all.average_win_streak, 5.0 / 3.0) && close(all.average_loss_streak, 4.0 / 3.0));
    assert_eq!(outliers(&[0.5]), vec![false], "one row has no deviation");
}

#[test]
fn a_ledger_with_no_account_reports_no_account_figures() {
    let mut l = TradeClosedLedger::new(PLAIN);
    book(&mut l, &fill(1, 1.0, 10.0, 0.0, 1), "", 0);
    book(&mut l, &fill(-1, 1.0, 12.0, 0.0, 2), "", 1);
    let m = l.metrics(|_| None);
    assert_eq!(m.account, TradeClosedAccountStatistics::default());
    assert_eq!(m.trades.len(), 1);
    assert!(
        close(m.trades[0].runup, 2.0) && close(m.trades[0].drawdown, 0.0),
        "entry and exit prices only"
    );
}

/// **A ledger resumed from another's rows and lots books on as that one would**: after a pyramid and a partial
/// close, a ledger built from the rows and the lots alone (each lot's per-unit fee and value read back) holds
/// the same rows and lots, and a close across both lots and a flip then make the same rows and lots, bit for
/// bit, as on the ledger that never stopped.
#[test]
fn a_resumed_ledger_books_on_as_the_ledger_it_was_taken_from() {
    let mut whole = TradeClosedLedger::new(PLAIN);
    let value = ContractValue::linear(0.5);
    let fills = [
        (fill(1, 2.0, 100.0, 0.3, 1), "A", 0),
        (fill(1, 1.0, 102.0, 0.17, 2), "B", 1),
        (fill(-1, 0.5, 103.0, 0.07, 3), "", 2),
    ];
    for (f, tag, bar) in &fills {
        whole.apply(f, tag, *bar, value, 1.0);
    }
    let lots = whole.lots().iter().map(|l| OpenLot::resumed(&l.state())).collect();
    let mut resumed = TradeClosedLedger::resumed(whole.rows().to_vec(), lots);
    assert_eq!(resumed.rows(), whole.rows());
    assert_eq!(resumed.lots(), whole.lots());
    for (f, bar) in [(fill(-1, 2.0, 104.0, 0.11, 4), 3), (fill(-1, 1.5, 101.0, 0.09, 5), 4)] {
        let a = whole.apply(&f, "", bar, value, 1.0).to_vec();
        let b = resumed.apply(&f, "", bar, value, 1.0).to_vec();
        assert_eq!(a, b, "the same rows at bar {bar}");
    }
    assert_eq!(resumed.rows(), whole.rows());
    assert_eq!(resumed.lots(), whole.lots());
}

/// **A lot's persisted state is what it shows, and a lot resumed from it persists the same state**: each
/// of the nine fields read off the lot, the per-unit fee and the value included, after a pyramid and a
/// partial close left two lots of different sizes open.
#[test]
fn a_lots_state_is_its_fields_and_survives_a_resume() {
    let mut l = TradeClosedLedger::new(PLAIN);
    let value = ContractValue::linear(0.5);
    l.apply(&fill(1, 2.0, 100.0, 0.3, 1), "A", 0, value, 1.0);
    l.apply(&fill(1, 1.0, 102.0, 0.17, 2), "", 1, value, 1.0);
    l.apply(&fill(-1, 0.5, 103.0, 0.07, 3), "", 2, value, 1.0);
    assert_eq!(l.lots().len(), 2);
    for lot in l.lots() {
        let s = lot.state();
        assert_eq!(
            (s.symbol.as_str(), s.tag.as_str(), s.side, s.entry_timestamp, s.entry_bar),
            (lot.symbol.as_str(), lot.tag.as_str(), lot.side, lot.entry_timestamp, lot.entry_bar)
        );
        assert_eq!((s.price.to_bits(), s.qty.to_bits()), (lot.price.to_bits(), lot.qty.to_bits()));
        assert_eq!(s.fee_per_unit.to_bits(), lot.entry_fee_per_unit().to_bits());
        assert_eq!(s.value, lot.value());
        assert_eq!(OpenLot::resumed(&s).state(), s);
    }
}

#[test]
fn an_infinite_profit_factor_survives_the_wire_as_null() {
    let m = TradeClosedMetrics {
        all: TradeClosedStatistics { profit_factor: f64::INFINITY, ..Default::default() },
        ..Default::default()
    };
    let json = serde_json::to_string(&m).expect("serializes");
    assert!(json.contains("\"profit_factor\":null"), "{json}");
    let back: TradeClosedMetrics = serde_json::from_str(&json).expect("reads back");
    assert_eq!(back, m);
}

/// `TradeClosedLedger::mark_close` as it was before it read each lot's mark once: four sums, each
/// asking `mark` for every lot again.
fn mark_close_asking_four_times(
    l: &mut TradeClosedLedger,
    ts: i64,
    mark: impl Fn(&str) -> Option<(f64, ContractValue)>,
) {
    let open_equity = |l: &TradeClosedLedger, price: fn(&OpenLot) -> f64| {
        compensated_sum(l.lots.iter().map(|lot| {
            mark(&lot.symbol).map_or(0.0, |(last, value)| {
                let px = Some(price(lot)).filter(|p| !p.is_nan()).unwrap_or(last);
                value.in_settle(
                    value.unrealized_pnl(lot.price, px, f64::from(lot.side) * lot.qty),
                    px,
                ) - lot.entry_fee_left()
            })
        }))
    };
    let close = l.cum + open_equity(l, |_| f64::NAN);
    let worst = l.cum + open_equity(l, |l| if l.side > 0 { l.bar_met.0 } else { l.bar_met.1 });
    let best = l.cum + open_equity(l, |l| if l.side > 0 { l.bar_met.1 } else { l.bar_met.0 });
    let i = &mut l.intrabar;
    if i.peak - worst > i.drawdown {
        (i.drawdown, i.drawdown_from) = (i.peak - worst, i.peak);
    }
    if best - i.trough > i.runup {
        (i.runup, i.runup_from) = (best - i.trough, i.trough);
    }
    (i.peak, i.trough) = (i.peak.max(l.cum), i.trough.min(l.cum));
    l.closes.push((ts, close));
    if let Some(r) = l.buy_hold_at(&mark) {
        l.buy_hold.push((ts, r));
    }
    if l.first_entry.is_some() {
        let (long, short) = l.account.map_or((0.0, 0.0), |a| (a.margin_long, a.margin_short));
        let used = compensated_sum(l.lots.iter().map(|lot| {
            let rate = if lot.side > 0 { long } else { short };
            mark(&lot.symbol).map_or(0.0, |(px, value)| {
                value.in_settle(value.gross_notional(lot.qty, px), px) * rate
            })
        }));
        l.margin = (l.margin.0 + used, l.margin.1 + 1);
    }
}

/// Reading each lot's mark once changes no bit: over 60 bars of three symbols (a linear one, an
/// inverse one and one `mark` cannot price), with pyramided longs and shorts, partial closes and flips,
/// every close leaves the ledger exactly as the four separate sums did (its curve, intrabar extremes,
/// buy and hold and margin), and `mark` is asked once per lot plus once for buy and hold.
#[test]
fn mark_close_reads_each_lot_once_and_samples_what_four_reads_did() {
    let terms = TradeClosedAccountTerms {
        margin_long: 0.5,
        margin_short: 0.25,
        ..TradeClosedAccountTerms::new(10_000.0)
    };
    let mut l = TradeClosedLedger::new(PLAIN).with_account(terms);
    let on = |symbol: &str, side, size, price, fee, ts| Fill {
        symbol: symbol.to_string(),
        ..fill(side, size, price, fee, ts)
    };
    let inverse = ContractValue::inverse(100.0);
    let (mut seed, mut prices) = (0x5EED_u64, [100.0, 30_000.0, 0.5]);
    let mut widest = 0;
    for bar in 0..60_usize {
        let ts = bar as i64 * DAY;
        let mut walks = [[0.0; 4]; 3];
        for (walk, price) in walks.iter_mut().zip(&mut prices) {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let step = ((seed >> 33) % 401) as f64 / 10_000.0 - 0.02;
            let open = *price;
            *price *= 1.0 + step;
            *walk = [open, open.min(*price) * 0.997, open.max(*price) * 1.004, *price];
        }
        let [eth, btc, xrp] = walks.map(|w| w[0]);
        if bar % 7 == 0 {
            book(&mut l, &on("ETHUSDT", 1, 1.0, eth, 0.05, ts), "E", bar);
        }
        if bar % 11 == 3 {
            book(&mut l, &on("ETHUSDT", -1, 2.5, eth, 0.07, ts), "F", bar);
        }
        if bar % 5 == 1 {
            l.apply(&on("BTCUSDT", -1, 3.0, btc, 0.0001, ts), "S", bar, inverse, 1.0);
        }
        if bar % 9 == 4 {
            l.apply(&on("BTCUSDT", 1, 4.0, btc, 0.0001, ts), "B", bar, inverse, 1.0);
        }
        if bar == 2 || bar == 30 {
            book(&mut l, &on("XRPUSDT", 1, 100.0, xrp, 0.01, ts), "X", bar);
        }
        for (symbol, walk) in ["ETHUSDT", "BTCUSDT", "XRPUSDT"].iter().zip(&walks) {
            l.mark_bar(symbol, bar, walk);
        }
        let mark = |symbol: &str| match symbol {
            "ETHUSDT" => Some((walks[0][3], ContractValue::linear(1.0))),
            "BTCUSDT" => Some((walks[1][3], inverse)),
            _ => None,
        };
        let mut four_reads = l.clone();
        let asked = std::cell::Cell::new(0);
        l.mark_close(ts, |symbol| {
            asked.set(asked.get() + 1);
            mark(symbol)
        });
        mark_close_asking_four_times(&mut four_reads, ts, mark);
        assert_eq!(format!("{l:?}"), format!("{four_reads:?}"), "bar {bar}");
        assert_eq!(asked.get(), l.lots.len() + usize::from(l.first_entry.is_some()), "bar {bar}");
        widest = widest.max(l.lots.len());
    }
    assert!(widest >= 4 && l.margin.1 > 50, "{widest} lots at most, {} samples", l.margin.1);
    assert!(l.lots.iter().any(|lot| lot.symbol == "XRPUSDT"), "an unpriced lot stays open");
}
