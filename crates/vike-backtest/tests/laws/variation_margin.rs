//! Gate for the opt-in `EngineParams::settlement_period_ms` variation-margin settlement (the LEAN
//! `FutureSettlementModel` analog): a multi-day held position marks to market on each cadence
//! boundary — the daily profit/loss crosses from unrealized into `realized`/`settled_profit` and the
//! cost basis resets to the mark — WITHOUT closing the position.
//!
//! The two load-bearing properties proved here:
//! 1. **Same total, different timing.** A settled run and an unsettled run of the SAME tape end
//!    with bit-identical `final_equity`, cash and equity curve; only the realized/unrealized split
//!    and the recorded trade legs differ.
//! 2. **Default off is byte-identical.** `settlement_period_ms: None` (and a non-positive period)
//!    never computes a bucket, never visits a symbol, and leaves `variation_settlements` empty.
//!
//! Mirrors the run-construction pattern in `resolution_settlement.rs`.

use vike_backtest::{EngineParams, SimBroker, StrategyEngine, Tick};
use vike_model::{Bar, QuoteTick, Strategy};

const SYM: &str = "BTC-PERP";
const T0: i64 = 1_700_006_400_000; // 2023-11-15T00:00:00Z — an exact UTC day boundary
const DAY_MS: i64 = 86_400_000;

/// Daily bars whose ts land exactly on successive UTC day boundaries.
fn mk_daily_bars(opens: &[f64]) -> Vec<Bar> {
    opens
        .iter()
        .enumerate()
        .map(|(i, &o)| Bar {
            ts: T0 + i as i64 * DAY_MS,
            open: o,
            high: o + 1.0,
            low: o - 1.0,
            close: o,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(SYM.to_string()),
        })
        .collect()
}

/// Buys `size` at step 0 (raw market → fills at bar 1's open) and holds until `close_at`, where it
/// closes the whole position. `close_at: None` holds to the end of the run.
struct BuyAndHold {
    size: f64,
    close_at: Option<usize>,
}

impl Strategy<SimBroker> for BuyAndHold {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == 0 {
            ctx.submit(SYM, 1, self.size, 0.0, true, None);
        } else if Some(ctx.index) == self.close_at {
            ctx.submit(SYM, -1, self.size, 0.0, true, None);
        }
    }
}

fn run_with(opens: &[f64], close_at: Option<usize>, period: Option<i64>) -> RunOut {
    let params =
        EngineParams { cash: 100_000.0, settlement_period_ms: period, ..Default::default() };
    let strat = BuyAndHold { size: 2.0, close_at };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), mk_daily_bars(opens))], strat, params);
    let r = e.run();
    RunOut {
        final_equity: r.final_equity,
        equity_curve: r.equity_curve.clone(),
        per_symbol_pnl: r.per_symbol_pnl.clone(),
        cash: e.core.cash,
        realized: e.core.sym[0].realized,
        settled_profit: e.core.sym[0].settled_profit,
        avg_price: e.core.position_of(SYM).avg_price,
        pos_size: e.core.position_of(SYM).size,
        settlements: e.core.variation_settlements.clone(),
        trades: r.trades.clone(),
    }
}

struct RunOut {
    final_equity: f64,
    equity_curve: Vec<f64>,
    per_symbol_pnl: Vec<(String, f64)>,
    cash: f64,
    realized: f64,
    settled_profit: f64,
    avg_price: f64,
    pos_size: f64,
    settlements: Vec<vike_backtest::VariationSettlement>,
    trades: Vec<vike_model::Trade>,
}

/// A multi-day HELD position settles once per day, at each day's close, and the accumulated
/// `settled_profit` equals the whole open PnL — while equity and cash are untouched.
#[test]
fn held_position_settles_daily() {
    // bar0 anchors the bucket (submit); the market fills at bar1's open = 101.
    let opens = [100.0, 101.0, 104.0, 103.0, 108.0];
    let out = run_with(&opens, None, Some(DAY_MS));

    // Bars 1..4 each open a new day-bucket → four settlement points. Bar 1 settles the position
    // opened in that very step at its own close (101), which is exactly its entry: dpl == 0.0, so
    // nothing is recorded. Bars 2/3/4 each record one.
    assert_eq!(out.settlements.len(), 3, "one settlement per day with a non-flat mark move");
    let amounts: Vec<f64> = out.settlements.iter().map(|s| s.amount).collect();
    // 2 contracts, basis walks 101 → 104 → 103 → 108
    assert_eq!(amounts, vec![(104.0 - 101.0) * 2.0, (103.0 - 104.0) * 2.0, (108.0 - 103.0) * 2.0]);
    assert_eq!(out.settlements[0].ts, T0 + 2 * DAY_MS);
    assert_eq!(out.settlements[2].ts, T0 + 4 * DAY_MS);
    assert_eq!(out.settlements[2].mark, 108.0);
    // the record carries the running accumulator
    assert_eq!(out.settlements[2].settled_profit, out.settled_profit);

    // basis reset to the last mark; the position itself never changed size
    assert_eq!(out.avg_price, 108.0);
    assert_eq!(out.pos_size, 2.0);

    // the ENTIRE open PnL has crossed into realized — nothing unrealized is left
    assert_eq!(out.settled_profit, (108.0 - 101.0) * 2.0);
    assert_eq!(out.realized, out.settled_profit);
}

/// Property (1): settling changes only the TIMING of the realized/unrealized split — the settled
/// and unsettled runs of the same tape are bit-identical in equity, cash and per-symbol PnL.
#[test]
fn settled_and_unsettled_agree_on_total_pnl() {
    // six bars so a close submitted at bar 4 still has a bar 5 open to fill at
    let opens = [100.0, 101.0, 104.0, 103.0, 108.0, 115.0];

    for close_at in [None, Some(4)] {
        let off = run_with(&opens, close_at, None);
        let on = run_with(&opens, close_at, Some(DAY_MS));

        assert_eq!(on.final_equity, off.final_equity, "total PnL is settlement-invariant");
        assert_eq!(on.cash, off.cash, "cash never moves on a settlement (see the field doc)");
        assert_eq!(on.equity_curve, off.equity_curve, "the equity curve is invariant, sample-wise");
        assert_eq!(on.per_symbol_pnl, off.per_symbol_pnl, "realized + unrealized is invariant");
    }
}

/// ...and when the position IS closed, the settled run's trade legs plus its settled profit sum to
/// the unsettled run's single trade pnl (the documented basis-reset caveat, pinned).
#[test]
fn closing_trade_plus_settled_equals_unsettled_trade() {
    // the close submitted at bar 4 fills at bar 5's open (115) — BEFORE bar 5's settlement point,
    // so the last basis the exit sees is bar 4's settlement mark (108).
    let opens = [100.0, 101.0, 104.0, 103.0, 108.0, 115.0];
    let off = run_with(&opens, Some(4), None);
    let on = run_with(&opens, Some(4), Some(DAY_MS));

    assert_eq!(off.trades.len(), 1);
    assert_eq!(on.trades.len(), 1);
    // unsettled: one trade 101 → 115. settled: the close exits against the LAST settlement basis,
    // and everything before it lives in settled_profit.
    assert_eq!(off.trades[0].entry_price, 101.0);
    assert_eq!(off.trades[0].exit_price, 115.0);
    assert_eq!(on.trades[0].entry_price, 108.0, "basis reset to the final settlement mark");
    assert_eq!(on.settled_profit, (108.0 - 101.0) * 2.0);
    assert_eq!(on.trades[0].pnl + on.settled_profit, off.trades[0].pnl);
    assert_eq!(on.realized, off.realized, "total realized still agrees once flat");
}

/// Property (2): default OFF (and a non-positive period) is byte-identical — no bucket is computed,
/// no settlement is recorded, no basis moves.
#[test]
fn default_off_is_byte_identical() {
    let opens = [100.0, 101.0, 104.0, 103.0, 108.0];
    let base = run_with(&opens, None, None);

    assert!(base.settlements.is_empty());
    assert_eq!(base.settled_profit, 0.0);
    assert_eq!(base.avg_price, 101.0, "unsettled: the ORIGINAL entry basis survives");
    assert_eq!(base.realized, 0.0, "unsettled: the whole PnL is still unrealized");

    // a non-positive period is treated exactly like None (the guard returns before bucketing)
    for period in [Some(0i64), Some(-1i64)] {
        let z = run_with(&opens, None, period);
        assert!(z.settlements.is_empty());
        assert_eq!(z.settled_profit, 0.0);
        assert_eq!(z.avg_price, base.avg_price);
        assert_eq!(z.realized, base.realized);
        assert_eq!(z.final_equity, base.final_equity);
        assert_eq!(z.equity_curve, base.equity_curve);
    }
}

/// A cadence coarser than the tape settles only on the real boundaries: with 1-day bars and a
/// 2-day period, only every other bar opens a new bucket.
#[test]
fn coarser_cadence_settles_less_often() {
    let opens = [100.0, 101.0, 104.0, 103.0, 108.0, 110.0, 112.0];
    let out = run_with(&opens, None, Some(2 * DAY_MS));
    // buckets by bar index (T0 is an even day boundary): 0,0,1,1,2,2,3 → boundaries at bars 2,4,6
    let tss: Vec<i64> = out.settlements.iter().map(|s| s.ts).collect();
    assert_eq!(tss, vec![T0 + 2 * DAY_MS, T0 + 4 * DAY_MS, T0 + 6 * DAY_MS]);
    // still the whole open PnL, just in fewer chunks
    assert_eq!(out.settled_profit, (112.0 - 101.0) * 2.0);
}

// --- the tick path -------------------------------------------------------------------------

fn quote(ts: i64, px: f64) -> Tick {
    Tick::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid: px,
        ask: px,
        bid_size: 100.0,
        ask_size: 100.0,
        symbol: SYM.to_string(),
    })
}

/// Buys once on the first quote and holds — the tick-path twin of `BuyAndHold`.
struct BuyOnceTick {
    done: bool,
}

impl Strategy<SimBroker> for BuyOnceTick {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, _q: &QuoteTick) {
        if !self.done {
            self.done = true;
            ctx.submit(SYM, 1, 2.0, 0.0, true, None);
        }
    }
}

/// `run_ticks` honors the same cadence: crossing a day boundary settles the open position at its
/// last-seen price, and OFF stays byte-identical.
#[test]
fn tick_path_settles_on_the_cadence() {
    // three quotes: two inside day 0, one in day 1
    let ticks = vec![(
        SYM.to_string(),
        vec![quote(T0, 100.0), quote(T0 + 3_600_000, 105.0), quote(T0 + DAY_MS, 111.0)],
    )];

    let mk = |period: Option<i64>| {
        let params =
            EngineParams { cash: 100_000.0, settlement_period_ms: period, ..Default::default() };
        let mut e = StrategyEngine::new(
            vec![(SYM.to_string(), mk_daily_bars(&[100.0]))],
            BuyOnceTick { done: false },
            params,
        );
        let r = e.run_ticks(&ticks);
        (r.final_equity, e.core.variation_settlements.clone(), e.core.sym[0].realized)
    };

    let (eq_off, s_off, realized_off) = mk(None);
    let (eq_on, s_on, realized_on) = mk(Some(DAY_MS));

    assert!(s_off.is_empty(), "tick path OFF records nothing");
    assert_eq!(realized_off, 0.0);
    assert_eq!(s_on.len(), 1, "exactly one day boundary was crossed");
    assert_eq!(s_on[0].ts, T0 + DAY_MS);
    assert_eq!(s_on[0].mark, 111.0);
    assert_eq!(realized_on, s_on[0].amount, "the crossed PnL became realized");
    assert_eq!(eq_on, eq_off, "equity is settlement-invariant on the tick path too");
}
