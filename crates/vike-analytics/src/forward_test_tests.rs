use super::*;

use vike_model::time::MS_PER_DAY;

/// A closed trade: a long when `is_long`, `size` 1 unless stated by the caller.
fn trade(is_long: bool, entry: (i64, f64), exit: (i64, f64), pnl: f64, fees: f64) -> Trade {
    Trade {
        entry_price: entry.1,
        exit_price: exit.1,
        size: 1.0,
        pnl,
        fees,
        entry_timestamp: entry.0,
        exit_timestamp: exit.0,
        symbol: String::new(),
        mae: 0.0,
        mfe: 0.0,
        is_long,
    }
}

fn close(a: f64, b: f64) {
    assert!((a - b).abs() <= 1e-9 * b.abs().max(1.0), "{a} vs {b}");
}

/// A long of 2 opened at 100 and closed in two pieces (110, then 120) is ONE round trip: size and money
/// summed, the exit price the size-weighted mean, the last exit time. Another position stays apart, and
/// the order is the order each position's first piece closed.
#[test]
fn the_pieces_of_one_position_are_one_round_trip() {
    let first = trade(true, (1_000, 100.0), (2_000, 110.0), 10.0, 0.1);
    let other = trade(false, (1_500, 105.0), (2_500, 100.0), 5.0, 0.2);
    let second = trade(true, (1_000, 100.0), (3_000, 120.0), 20.0, 0.1);
    let merged = round_trips(&[first, other.clone(), second]);
    assert_eq!(merged.len(), 2);
    let whole = &merged[0];
    assert_eq!((whole.size, whole.pnl, whole.exit_timestamp), (2.0, 30.0, 3_000));
    close(whole.fees, 0.2);
    close(whole.entry_price, 100.0);
    close(whole.exit_price, 115.0);
    assert_eq!(merged[1], other);
}

/// The net return is the round trip's money after fees over its entry notional, and a trade with no
/// notional has none.
#[test]
fn a_trades_net_return_is_after_fees_over_its_notional() {
    let linear = ContractValue::default();
    close(trade_net_return(&trade(true, (0, 100.0), (1, 110.0), 10.0, 0.3), linear), 0.097);
    let mut nothing = trade(true, (0, 100.0), (1, 110.0), 10.0, 0.0);
    nothing.size = 0.0;
    assert!(trade_net_return(&nothing, linear).is_nan());
}

/// The notional is the traded contract's (`ContractValue::gross_notional`): for the default linear 1.0 the
/// bits of `(pnl − fees) / |entry_price · size|`, a short and an odd size included; for an inverse perpetual
/// the dollars it put at risk. 1,000 USD of an inverse contract from 60,000 to 66,000 books
/// `(66,000 − 60,000) / 60,000 · 1,000` = 100 USD, a 10% return, where a linear reading of the same trade
/// is 100 / 60,000,000.
#[test]
fn a_trades_net_return_is_on_its_contracts_notional() {
    let mut odd = trade(false, (0, 101.37), (1, 99.5), 3.3, 0.07);
    odd.size = 2.75;
    let linear = (odd.pnl - odd.fees) / (odd.entry_price * odd.size).abs();
    assert_eq!(trade_net_return(&odd, ContractValue::default()).to_bits(), linear.to_bits());

    let mut inverse = trade(true, (0, 60_000.0), (1, 66_000.0), 100.0, 0.0);
    inverse.size = 1_000.0;
    close(trade_net_return(&inverse, ContractValue::inverse(1.0)), 0.1);
    close(trade_net_return(&inverse, ContractValue::default()), 100.0 / 60_000_000.0);
}

/// Slippage is signed so that POSITIVE is the forward paying more: a long that bought higher and sold
/// lower, a short that sold lower and bought back higher.
#[test]
fn slippage_is_positive_where_the_forward_paid_more() {
    let backtest = [
        trade(true, (1_000, 100.0), (5_000, 110.0), 10.0, 0.0),
        trade(false, (10_000, 100.0), (15_000, 90.0), 10.0, 0.0),
    ];
    let forward = [
        trade(true, (1_200, 100.5), (5_100, 109.5), 9.0, 0.0),
        trade(false, (10_300, 99.5), (15_200, 90.5), 9.0, 0.0),
    ];
    let m = match_trades(&backtest, &forward, 1_000, ContractValue::default());
    assert_eq!((m.backtest_only.len(), m.forward_only.len()), (0, 0));
    let (long, short) = (&m.pairs[0], &m.pairs[1]);
    assert_eq!((long.backtest, long.forward, long.entry_delay_ms), (0, 0, 200));
    close(long.entry_slippage_bps, 50.0);
    close(long.exit_slippage_bps, 0.5 / 110.0 * 10_000.0);
    assert_eq!((short.backtest, short.forward, short.entry_delay_ms), (1, 1, 300));
    close(short.entry_slippage_bps, 50.0);
    close(short.exit_slippage_bps, 0.5 / 90.0 * 10_000.0);
    close(short.slippage_bps(), short.entry_slippage_bps + short.exit_slippage_bps);
    close(long.backtest_return, 0.1);
    close(long.forward_return, 9.0 / 100.5);
}

/// A pair takes the nearest forward entry of the SAME direction inside the tolerance; a signal with
/// none is the backtest's alone, and a forward trade nobody claimed is the forward's alone. The input
/// order does not matter: indices name each slice as it was handed.
#[test]
fn trades_pair_by_direction_and_nearest_entry_inside_the_tolerance() {
    let backtest = [
        trade(true, (9_000, 1.0), (9_500, 1.0), 0.0, 0.0),
        trade(true, (1_000, 1.0), (2_000, 1.0), 0.0, 0.0),
        trade(false, (5_000, 1.0), (6_000, 1.0), 0.0, 0.0),
    ];
    let forward = [
        trade(false, (1_050, 1.0), (2_000, 1.0), 0.0, 0.0), // nearer, but the wrong direction
        trade(true, (1_300, 1.0), (2_000, 1.0), 0.0, 0.0),
        trade(true, (9_100, 1.0), (9_600, 1.0), 0.0, 0.0),
        trade(false, (20_000, 1.0), (21_000, 1.0), 0.0, 0.0),
    ];
    let m = match_trades(&backtest, &forward, 500, ContractValue::default());
    let pairs: Vec<(usize, usize, i64)> =
        m.pairs.iter().map(|p| (p.backtest, p.forward, p.entry_delay_ms)).collect();
    assert_eq!(pairs, [(1, 1, 300), (0, 2, 100)]);
    assert_eq!(m.backtest_only, [2], "no forward short within 500 ms of 5000");
    assert_eq!(m.forward_only, [0, 3]);
}

/// Greedy in time: the first backtest trade keeps the one forward trade both could take, and a tie in
/// distance goes to the earlier forward entry.
#[test]
fn the_first_signal_keeps_a_contested_forward_trade_and_ties_go_earlier() {
    let backtest = [
        trade(true, (1_000, 1.0), (2_000, 1.0), 0.0, 0.0),
        trade(true, (1_100, 1.0), (2_000, 1.0), 0.0, 0.0),
    ];
    let forward = [trade(true, (1_050, 1.0), (2_000, 1.0), 0.0, 0.0)];
    let m = match_trades(&backtest, &forward, 500, ContractValue::default());
    assert_eq!((m.pairs[0].backtest, m.backtest_only.as_slice()), (0, &[1][..]));

    let forward = [
        trade(true, (1_200, 1.0), (2_000, 1.0), 0.0, 0.0),
        trade(true, (800, 1.0), (2_000, 1.0), 0.0, 0.0),
    ];
    let m = match_trades(&backtest[..1], &forward, 500, ContractValue::default());
    assert_eq!(m.pairs[0].forward, 1, "800 and 1200 are both 200 away: the earlier wins");
}

/// Both records on the window's day grid: the capital at the opening, then every day's close; a trade
/// closing outside the window counts nothing, and a day without a close carries the day before.
#[test]
fn a_records_daily_equity_runs_over_the_whole_window() {
    let day = MS_PER_DAY;
    let trades = [
        trade(true, (day * 2, 1.0), (day * 2 + 5, 1.0), -5.0, 1.0),
        trade(true, (100, 1.0), (day / 2, 1.0), 10.0, 0.0),
        trade(true, (day * 2, 1.0), (day * 4, 1.0), 99.0, 0.0), // closes after the window
    ];
    let (equity, ts) = realized_daily_equity(1_000.0, &trades, 0, day * 3 - 1);
    assert_eq!(equity, [1_000.0, 1_010.0, 1_010.0, 1_004.0]);
    assert_eq!(ts, [0, day - 1, day * 2 - 1, day * 3 - 1]);

    let (quiet, quiet_ts) = realized_daily_equity(1_000.0, &[], 0, day * 3 - 1);
    assert_eq!(quiet, [1_000.0; 4]);
    assert_eq!(quiet_ts, ts, "two records on one window share one grid");
}
