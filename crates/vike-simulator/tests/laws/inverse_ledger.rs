//! A COIN LEDGER (`StrategyEngineConfig::contract_values` with an inverse value, decision 0258): the
//! simulator books an inverse contract's cash, PnL, fees and funding in its settle coin, by the venues'
//! own formulas (the inverse research, Part 1), and books the same coin as the live engine
//! (`vike_execution`'s coin ledger) for the same fills.
//!
//! Every number below is a venue's documented one or the research's worked example (deribit
//! `BTC-PERPETUAL`, a 0.1 BTC wallet, a 3.5 bp taker fee), never a figure read off this engine.

use indexmap::IndexMap;
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, EventHandler, RiskGate};
use vike_marketdata::Bar;
use vike_model::events::{Event, FillEvent, FundingEvent, PositionSide, TradeId};
use vike_model::{ContractValue, RiskLimits, Strategy};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "BTC-PERPETUAL";
const T0: i64 = 1_700_006_400_000;
const STEP: i64 = 3_600_000;

/// One bar per `(low, close)`: open = high = close, so a market order fills at the next close.
fn bars(rows: &[(f64, f64)]) -> Vec<Bar> {
    rows.iter()
        .enumerate()
        .map(|(i, &(low, close))| Bar {
            timestamp: T0 + i as i64 * STEP,
            open: close,
            high: close,
            low,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(SYM.into()),
        })
        .collect()
}

fn flat(closes: &[f64]) -> Vec<Bar> {
    bars(&closes.iter().map(|&c| (c, c)).collect::<Vec<_>>())
}

/// Opens `size` dollars on `side` at step `open_at` and closes the whole position at `close_at`; each
/// fills at the next bar's open.
struct OpenClose {
    side: i32,
    size: f64,
    open_at: usize,
    close_at: usize,
}

impl Strategy<OrderBrokerSimulator> for OpenClose {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        if ctx.index == self.open_at {
            ctx.submit(SYM, self.side, self.size, 0.0, true, None);
        } else if ctx.index == self.close_at {
            ctx.submit_close(SYM);
        }
    }
}

fn coin_params(cash: f64) -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash,
        contract_values: vec![(SYM.to_string(), ContractValue::inverse(1.0))],
        mirror: true,
        ..Default::default()
    }
}

fn close(got: f64, want: f64, tolerance: f64) -> bool {
    (got - want).abs() <= tolerance
}

/// The research's worked example, Part 2.3, which the venue's docs define: 0.1 BTC, buy 1,000 USD at
/// 60,000 (fee 0.0000058333 BTC), mark 66,000, sell at 66,000 (fee 0.0000053030 BTC), back to 60,000.
///
/// | step | the venue (coin) | in dollars |
/// |---|---|---|
/// | marked at 66,000 | 0.1 − 0.0000058333 + 0.0015151515 BTC | 6,699.62 |
/// | closed, at 60,000 | 0.1015040152 BTC | 6,090.24 |
#[test]
fn the_worked_example_books_the_coin_and_is_valued_at_the_contract_price() {
    let strat = OpenClose { side: 1, size: 1_000.0, open_at: 0, close_at: 2 };
    let params = StrategyEngineConfig { fee_rate: 0.000_35, ..coin_params(0.1) };
    let mut e = StrategyEngine::new(
        vec![(SYM.to_string(), flat(&[60_000.0, 60_000.0, 66_000.0, 66_000.0, 60_000.0]))],
        strat,
        params,
    );
    let r = e.run();
    let fee_open = 1_000.0 * 0.000_35 / 60_000.0;
    let fee_close = 1_000.0 * 0.000_35 / 66_000.0;
    let coin_pnl = 1_000.0 * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    assert!(close(r.fees_paid, fee_open + fee_close, 1e-18), "fees in BTC: {}", r.fees_paid);
    assert_eq!(r.trades.len(), 1);
    assert!(close(r.trades[0].pnl, coin_pnl, 1e-15), "PnL in BTC: {}", r.trades[0].pnl);
    // Marked at 66,000 while held: the venue's 6,699.62.
    let held = r.equity_curve[2];
    assert!(close(held, 0.1 - fee_open + coin_pnl, 1e-15), "{held}");
    assert!(close(held * 66_000.0, 6_699.62, 0.01), "{}", held * 66_000.0);
    // Closed, and the coin back at 60,000: the wallet is 0.1015040152 BTC, 6,090.24 USD.
    assert!(close(r.final_equity, 0.101_504_015_2, 1e-10), "{}", r.final_equity);
    assert_eq!(r.settle_price, Some(60_000.0), "valued at the contract's own last price");
    assert!(close(r.final_equity * 60_000.0, 6_090.24, 0.01), "{}", r.final_equity * 60_000.0);
    assert!(close(r.per_symbol_pnl[0].1, coin_pnl, 1e-15), "per symbol, in BTC");
}

/// The hedge, inverse's common use: 1 BTC held and 60,000 USD short at 60,000 is flat in DOLLARS at
/// every price (the research's Part 2.3 hedge case), while its coin count moves.
#[test]
fn the_hedge_is_flat_in_dollars_at_every_price() {
    let strat = OpenClose { side: -1, size: 60_000.0, open_at: 0, close_at: usize::MAX };
    let closes = [60_000.0, 60_000.0, 66_000.0, 54_000.0, 75_000.0];
    let mut e =
        StrategyEngine::new(vec![(SYM.to_string(), flat(&closes))], strat, coin_params(1.0));
    let r = e.run();
    for (i, (&eq, &px)) in r.equity_curve.iter().zip(&closes).enumerate().skip(1) {
        assert!(close(eq * px, 60_000.0, 1e-8), "step {i}: {eq} BTC at {px}");
    }
    assert!(close(r.equity_curve[2], 60_000.0 / 66_000.0, 1e-15), "0.909091 BTC at 66,000");
}

/// Funding on an inverse contract is paid in coin on the position's value at the mark (bybit: "position
/// value × funding rate", position value = quantity ÷ mark price), a long paying a positive rate.
#[test]
fn inverse_funding_is_paid_in_coin_on_the_position_value() {
    let strat = OpenClose { side: 1, size: 10_000.0, open_at: 0, close_at: usize::MAX };
    let mut rows = flat(&[50_000.0, 50_000.0, 50_000.0, 50_000.0]);
    rows[2].funding = Some(0.000_1);
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), rows)], strat, coin_params(1.0));
    let r = e.run();
    let paid = 10_000.0 / 50_000.0 * 0.000_1; // 0.00002 BTC
    assert!(close(r.funding_paid, -paid, 1e-18), "{}", r.funding_paid);
    assert!(close(r.final_equity, 1.0 - paid, 1e-15), "{}", r.final_equity);
}

/// The margin call liquidates an isolated-sized long where the venues' inverse liquidation price says:
/// `e·(1 + mm)/(1 + initial_margin)` (binance COIN-M's published formula with `cum = 0`; the research's Part 1.5).
/// 10,000 USD long at 50,000 on 10x (im 0.1, the margin 0.02 BTC as cash) with mm 0.5% liquidates
/// below 45,681.82, and not above it.
#[test]
fn the_margin_call_liquidates_at_the_venues_inverse_liquidation_price() {
    let (e0, initial_margin, mm) = (50_000.0, 0.1, 0.005);
    let lp = e0 * (1.0 + mm) / (1.0 + initial_margin);
    assert!(close(lp, 45_681.818_18, 1e-4), "{lp}");
    let run = |low: f64| {
        let strat = OpenClose { side: 1, size: 10_000.0, open_at: 0, close_at: usize::MAX };
        let rows = bars(&[(e0, e0), (e0, e0), (low, e0), (e0, e0)]);
        let params = StrategyEngineConfig {
            maint_margin: mm,
            liquidation_buffer: 0.0,
            ..coin_params(10_000.0 * initial_margin / e0)
        };
        let mut e = StrategyEngine::new(vec![(SYM.to_string(), rows)], strat, params);
        e.run();
        e.core.position_of(SYM).size
    };
    assert_eq!(run(lp + 5.0), 10_000.0, "held above the liquidation price");
    let after = run(lp - 5.0);
    assert!(after < 10_000.0, "reduced below it: {after}");
}

/// The percent verb sizes an inverse contract by the one sizing law (`ContractValue::units_worth`, decision
/// 0170 item 9) on the coin equity valued at the contract's price: 50% of 1 BTC at 60,000 is 30,000 dollar
/// contracts, as live's `order_target_percent` sizes it.
#[test]
fn the_percent_verb_sizes_a_coin_ledger_in_dollars_at_its_face() {
    struct Half;
    impl Strategy<OrderBrokerSimulator> for Half {
        fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
            if ctx.index == 0 {
                ctx.strategy_order_target_percent(SYM, 0.5);
            }
        }
    }
    let rows = flat(&[60_000.0, 60_000.0, 60_000.0]);
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), rows)], Half, coin_params(1.0));
    e.run();
    assert_eq!(e.core.position_of(SYM).size, 30_000.0);
    assert!(e.core.dropped.is_empty(), "{:?}", e.core.dropped);
}

/// A variation-margin sweep moves an inverse position's basis to the mark and its coin PnL into
/// realized, and the equity curve does not move a bit (the coin cash leg adds across marks).
#[test]
fn a_variation_margin_sweep_leaves_the_coin_equity_unchanged() {
    let run = |period: Option<i64>| {
        let strat = OpenClose { side: 1, size: 1_000.0, open_at: 0, close_at: 4 };
        let rows = flat(&[60_000.0, 60_000.0, 63_000.0, 66_000.0, 64_000.0, 64_000.0, 64_000.0]);
        let params = StrategyEngineConfig { settlement_period_ms: period, ..coin_params(0.1) };
        let mut e = StrategyEngine::new(vec![(SYM.to_string(), rows)], strat, params);
        let r = e.run();
        let settled: f64 = e.core.variation_settlements.iter().map(|v| v.amount).sum();
        (r, settled)
    };
    let (plain, none) = run(None);
    let (swept, settled) = run(Some(STEP));
    assert_eq!(none, 0.0);
    assert!(settled != 0.0, "the sweep settled something");
    let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&plain.equity_curve), bits(&swept.equity_curve), "equity unmoved");
    let coin_pnl = 1_000.0 * (1.0 / 60_000.0 - 1.0 / 64_000.0);
    let total: f64 = swept.trades.iter().map(|t| t.pnl).sum::<f64>() + settled;
    assert!(close(total, coin_pnl, 1e-15), "{total} vs {coin_pnl}");
}

/// **One law, two engines.** The same fills (open, add, reduce, flip, close, with fees and a funding
/// payment) folded by the simulator and by the live engine's coin ledger (decision 0258) book the
/// same coin: the realized PnL and the fees bit for bit (one division, `ContractValue::in_settle`),
/// the funding as stated, and the cash within a stated 1e-12 (the simulator's cash legs and the live
/// realized PnL are two spellings of one sum).
///
/// The live fees are stated as the paper book states them (`vike_fills::broker_sim::commission_of`, in
/// USD, which the coin ledger divides by the fill price), so this is the backtest = paper = live row
/// for an inverse contract.
#[test]
fn the_simulator_and_the_live_coin_ledger_book_the_same_coin() {
    struct Script(Vec<(usize, i32, f64)>);
    impl Strategy<OrderBrokerSimulator> for Script {
        fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
            for &(at, side, size) in &self.0 {
                if ctx.index == at {
                    ctx.submit(SYM, side, size, 0.0, true, None);
                }
            }
        }
    }
    let rate = 0.000_5;
    let script = Script(vec![
        (0, 1, 1_000.0),
        (1, 1, 2_000.0),
        (3, -1, 1_500.0),
        (4, -1, 3_000.0),
        (6, 1, 1_500.0),
    ]);
    let mut rows =
        flat(&[60_000.0, 61_000.0, 64_000.0, 58_500.0, 70_100.0, 66_600.0, 63_300.0, 65_000.0]);
    rows[3].funding = Some(0.000_3);
    let cash = 0.25;
    let params = StrategyEngineConfig { fee_rate: rate, ..coin_params(cash) };
    let mut sim = StrategyEngine::new(vec![(SYM.to_string(), rows)], script, params);
    let r = sim.run();
    assert_eq!(sim.core.position_of(SYM).size, 0.0, "the script ends flat");

    let value = ContractValue::inverse(1.0);
    let mut grid = IndexMap::new();
    grid.insert(SYM.to_string(), value);
    let mut live = Engine::new(
        Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        SYM,
    );
    let fills = sim.core.mirror_fills.clone().expect("mirror on");
    assert_eq!(fills.len(), 5);
    for (i, f) in fills.iter().enumerate() {
        let usd = vike_fills::broker_sim::commission(
            None,
            rate,
            rate,
            f.is_maker,
            f.size,
            f.price,
            value,
            None,
            &mut Default::default(),
        );
        let id = format!("f{i}");
        let event = FillEvent {
            trade_id: TradeId::new(&id).expect("not empty"),
            order_client_id: id,
            venue: "deribit".into(),
            symbol: SYM.into(),
            side: f.side,
            last_qty: f.size,
            last_price: f.price,
            commission: usd,
            commission_asset: "USD".into(),
            liquidity_side: "taker".into(),
            timestamp: f.timestamp,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        live.on_event(&Event::Fill(event), &mut EventBusOutbox::default());
        assert_eq!(f.fee.to_bits(), (usd / f.price).to_bits(), "fill {i}: one fee, in coin");
    }
    for m in sim.core.mirror_funding.clone().expect("mirror on") {
        live.account.apply_funding(&FundingEvent {
            venue: "deribit".into(),
            symbol: SYM.into(),
            position_side: PositionSide::Both,
            funding_rate: 0.000_3,
            amount: m.amount,
            mark_price: None,
            timestamp: m.timestamp,
            route_key: None,
            currency: None,
        });
    }

    let a = &live.account;
    let sim_realized: f64 = r.trades.iter().map(|t| t.pnl).sum();
    assert_eq!(a.realized_pnl.to_bits(), sim_realized.to_bits(), "realized coin, bit for bit");
    assert_eq!(a.fees_paid.to_bits(), r.fees_paid.to_bits(), "fees in coin, bit for bit");
    assert_eq!(a.funding_paid.to_bits(), r.funding_paid.to_bits(), "funding in coin, as stated");
    assert!(r.funding_paid < 0.0, "the long paid the positive rate");
    let live_cash = a.balance + a.realized_pnl;
    let sim_cash = r.final_equity - cash;
    assert!(close(live_cash, sim_cash, 1e-12), "live {live_cash} vs sim {sim_cash}");
}
