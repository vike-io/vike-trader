//! `sport_taker` END-TO-END gate: the copy-trading strategy folded through the REAL
//! `StrategyEngine` over a two-stream tape (the copied wallet's SIGNAL series + the token's
//! public MARKET series), settled by the REAL binary-resolution source, with the REAL
//! order-latency gate standing in for copy-detection delay δ.
//!
//! What each test pins is a property the report of the divergence investigation depends on:
//!
//! * the crossing fires ONCE and buys the token — not the signal series;
//! * δ = 0 fills at the next MARKET print, and a larger δ fills LATER and therefore WORSE on a
//!   tape that reprices away from the copier;
//! * a δ that outruns the market's last print before resolution MISSES entirely (no trade), which
//!   is the one thing a "next print + haircut" SQL model cannot express;
//! * flat-$100 stake economics are recovered analytically from the booked `(entry, payout)` pair.

use vike_backtest::sport_taker::{
    flat_stake_pnl, market_symbol, signal_symbol, SportTaker, STAKE_USD,
};
use vike_backtest::{EngineParams, ResolutionSource, StrategyEngine, Tick};
use vike_model::TradeTick;

const WALLET: &str = "0x29b52d98ac9ef9414b04164246c95bc63d74cc6c";
const CID: &str = "0xcondition";
const OI: u8 = 1;
const T0: i64 = 1_750_000_000_000;

fn trade(symbol: &str, ts: i64, price: f64, size: f64, maker: bool) -> Tick {
    Tick::Trade(TradeTick {
        ts,
        local_ts: 0,
        price,
        size,
        is_buyer_maker: maker,
        symbol: symbol.to_string(),
    })
}

/// The token pays `payout` from `res_ts` onward; every other symbol (the signal series included)
/// answers `None` and is never settled or latched.
fn resolution(market: String, res_ts: i64, payout: f64) -> Option<ResolutionSource> {
    Some(Box::new(move |sym: &str, ts: i64| (sym == market && ts >= res_ts).then_some(payout)))
}

struct Fixture {
    sig: String,
    mkt: String,
    series: Vec<(String, Vec<Tick>)>,
    res_ts: i64,
}

/// Wallet buys 2000 @ 0.50 (= $1000, below the $2000 bar) then 3000 @ 0.50 (cum $2500 — CROSSES)
/// at T0+10s. The public tape reprices upward every 2 s afterwards (0.55, 0.60, 0.65, 0.70), so
/// each extra second of detection delay is directly visible as a worse fill.
fn fixture() -> Fixture {
    let sig = signal_symbol(WALLET, CID, OI);
    let mkt = market_symbol(CID, OI);
    let sig_ticks =
        vec![trade(&sig, T0, 0.50, 2000.0, false), trade(&sig, T0 + 10_000, 0.50, 3000.0, false)];
    // The wallet's own prints are on the public tape too (they ARE public taker fills), then the
    // market walks away.
    let mkt_ticks = vec![
        trade(&mkt, T0, 0.50, 2000.0, false),
        trade(&mkt, T0 + 10_000, 0.50, 3000.0, false),
        trade(&mkt, T0 + 12_000, 0.55, 100.0, false),
        trade(&mkt, T0 + 14_000, 0.60, 100.0, false),
        trade(&mkt, T0 + 16_000, 0.65, 100.0, false),
        trade(&mkt, T0 + 18_000, 0.70, 100.0, false),
    ];
    Fixture {
        sig: sig.clone(),
        mkt: mkt.clone(),
        // SIGNAL first: on a tie the engine's stable k-way merge takes the lower stream index, so
        // the crossing is detected before the same-block market print it could copy.
        series: vec![(sig, sig_ticks), (mkt, mkt_ticks)],
        res_ts: T0 + 30_000,
    }
}

fn run(delay_ms: i64, payout: f64) -> (vike_backtest::BacktestResult, SportTaker) {
    let f = fixture();
    let symbols: Vec<(String, Vec<vike_model::Bar>)> =
        vec![(f.sig.clone(), Vec::new()), (f.mkt.clone(), Vec::new())];
    let mut engine = StrategyEngine::new(
        symbols,
        SportTaker::default(),
        EngineParams {
            cash: 1_000.0,
            fee_rate: 0.0,
            slippage: 0.0,
            resolution: resolution(f.mkt.clone(), f.res_ts, payout),
            resolution_end_ts: Some(f.res_ts),
            latency_model: (delay_ms > 0).then(|| {
                vike_backtest::latency::LatencyModelKind::constant(delay_ms * 1_000_000, 0)
            }),
            ..Default::default()
        },
    );
    let res = engine.run_ticks(&f.series);
    (res, engine.strategy)
}

#[test]
fn crossing_fires_once_and_buys_the_market_token() {
    let (res, strat) = run(0, 1.0);
    assert_eq!(strat.fired.len(), 1, "one-shot: exactly one crossing");
    let fired = &strat.fired[0];
    assert_eq!(fired.ts, T0 + 10_000);
    assert_eq!(fired.market, market_symbol(CID, OI));
    assert_eq!(fired.signal.copied_wallet, WALLET);
    assert_eq!(fired.signal.segment, "esports");
    assert_eq!(fired.signal.trigger_buy_usd, 2500.0);
    // VWAP of 2000@0.50 + 3000@0.50 — the wallet's own average, not the copier's.
    assert_eq!(fired.signal.ideal_price, 0.5);
    assert_eq!(res.trades.len(), 1, "one settled round trip");
    assert!(res.trades[0].is_long);
}

#[test]
fn detection_delay_moves_the_fill_down_the_tape_monotonically() {
    // δ = 0 fills at the next market print after the crossing (the same-block print at T0+10s —
    // the unreachable instant-copy bound); every later δ fills at a strictly worse print.
    let entries: Vec<f64> = [0i64, 1_000, 3_000, 5_000, 7_000]
        .iter()
        .map(|&d| run(d, 1.0).0.trades[0].entry_price)
        .collect();
    assert_eq!(entries, vec![0.50, 0.55, 0.60, 0.65, 0.70]);
}

#[test]
fn a_delay_past_the_last_print_misses_the_bet_entirely() {
    // The crossing still FIRES (the signal is a detection, not an order outcome) but the order is
    // delivered after the tape's last print and before resolution, so it never fills — and the
    // resolution sweep cancels it. A next-print haircut would have booked a phantom bet here.
    let (res, strat) = run(9_000, 1.0);
    assert_eq!(strat.fired.len(), 1, "the crossing is still detected");
    assert!(res.trades.is_empty(), "no fill => no trade, not a fabricated one");
}

#[test]
fn settlement_pays_the_binary_payout_and_flat_stake_pnl_follows() {
    let (won, _) = run(0, 1.0);
    let (lost, _) = run(0, 0.0);
    assert_eq!(won.trades[0].exit_price, 1.0);
    assert_eq!(lost.trades[0].exit_price, 0.0);
    // $100 at 0.50 = 200 shares -> +$100 on a win, -$100 on a loss.
    let p = won.trades[0].entry_price;
    assert!((flat_stake_pnl(p, 1.0, STAKE_USD) - 100.0).abs() < 1e-9);
    assert_eq!(flat_stake_pnl(p, 0.0, STAKE_USD), -STAKE_USD);
}

#[test]
fn a_wallet_outside_the_allow_list_is_never_copied() {
    let f = fixture();
    let symbols: Vec<(String, Vec<vike_model::Bar>)> =
        vec![(f.sig.clone(), Vec::new()), (f.mkt.clone(), Vec::new())];
    let mut strat = SportTaker::default();
    strat.wallets = vec!["0xsomeoneelse".to_string()];
    let mut engine = StrategyEngine::new(
        symbols,
        strat,
        EngineParams {
            cash: 1_000.0,
            resolution: resolution(f.mkt.clone(), f.res_ts, 1.0),
            resolution_end_ts: Some(f.res_ts),
            ..Default::default()
        },
    );
    let res = engine.run_ticks(&f.series);
    assert!(engine.strategy.fired.is_empty());
    assert!(res.trades.is_empty());
}

#[test]
fn maker_fills_can_be_excluded_from_conviction() {
    // Same tape, but the crossing fill is flagged as the wallet resting as MAKER. With
    // `include_maker_fills = false` the conviction never reaches $2000 and nothing is copied —
    // the knob is material (maker fills are ~23 % of these wallets' BUYs), so it is pinned.
    let sig = signal_symbol(WALLET, CID, OI);
    let mkt = market_symbol(CID, OI);
    let series = vec![
        (
            sig.clone(),
            vec![
                trade(&sig, T0, 0.50, 2000.0, false),
                trade(&sig, T0 + 10_000, 0.50, 3000.0, true), // MAKER
            ],
        ),
        (mkt.clone(), vec![trade(&mkt, T0 + 12_000, 0.55, 100.0, false)]),
    ];
    let symbols: Vec<(String, Vec<vike_model::Bar>)> =
        vec![(sig, Vec::new()), (mkt.clone(), Vec::new())];
    let mut strat = SportTaker::default();
    strat.include_maker_fills = false;
    let mut engine = StrategyEngine::new(symbols, strat, EngineParams::default());
    engine.run_ticks(&series);
    assert!(engine.strategy.fired.is_empty());
}
