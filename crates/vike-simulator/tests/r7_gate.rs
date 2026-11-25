//! THE R7 gate: same strategy + same bars → the R2 backtest engine and the live core in
//! paper mode produce BIT-IDENTICAL fills (ts/side/qty/px/fee/maker), realized PnL,
//! fees, and final position. The strategy logic is defined ONCE (a shared signal fn)
//! and mounted through each runtime's own trait — so the gate proves the RUNTIMES agree,
//! not that two strategy copies happen to.
//!
//! The cross-language chain: the r1/r4 fixtures prove Python backtest == Rust backtest; this
//! gate proves Rust backtest == Rust paper-core; therefore Python backtest == paper.
//!
//! CONDITIONAL-VERB ROWS (trigger-law wave 2): beyond the market-order SMA row, the gate now
//! proves the CONDITIONAL verbs agree too —
//! - `stop verb`: `OrderBrokerSimulator::submit_stop` under `StrategyEngineConfig::emulator_release_stops = true`
//!   == `LiveBroker::submit_stop` (always emulator-armed live: the fired conditional releases
//!   as a MARKET and fills the next bar's open). The DEFAULT (`false`) engine semantics —
//!   same-bar fill at the trigger — deliberately diverge from live; that divergence is pinned
//!   in `tests/laws/trigger_law.rs`, not here.
//! - `bracket`: `OrderBrokerSimulator` entry-with-protective-stop + resting TP limit ==
//!   `LiveBroker::submit_bracket`'s OTO/OCO triple through the paper book (both stops fill via
//!   the ONE trigger oracle; both sides OCO-cancel the surviving exit).
//! - `stop-limit` and `take-profit` (wave 1 of the new order kinds): `OrderBrokerSimulator::submit_stop_limit`
//!   / `submit_take_profit` == `LiveBroker::submit_stop_limit` / `submit_take_profit`, ORDERS the
//!   paper book rests by the same law, each with a taker exit on the trigger event and a maker exit
//!   after the conversion to the plain `Limit`.
//! - `order lists` (wave 4, decision 0264): `OrderBrokerSimulator::submit_oco` / `submit_ladder` ==
//!   `LiveBroker::submit_oco` / `submit_ladder` through the paper book: an OCO exit pair (a target
//!   limit and a stop, the first fill cancelling the other) and a bracket ladder (a market entry, a
//!   whole-size stop and two partial targets, OUO), both sides resolving the links by the ONE
//!   contingency book and the ONE OUO spend.
//! - `combos` (wave 6, decision 0266): `OrderBrokerSimulator::submit_combo` == `LiveBroker::submit_combo`
//!   through the paper book, over two symbols: a combo priced by its NET and one priced by a limit
//!   on EVERY leg, both filling all legs or none on the event where every leg has a fresh mark, by
//!   the ONE net law (`vike_model::combo_net_cross`) and the ONE per-leg law
//!   (`vike_model::combo_legs_cross`).
//! - `attributes` (the attributes wave, decision 0267): `OrderBrokerSimulator::submit_trailing_take_profit`,
//!   `submit_trailing_percent`, `submit_stop_adjustable` and `submit_limit_attributed` (all-or-none,
//!   and a discretionary amount) == the `LiveBroker` verbs of the same names, ORDERS the paper book
//!   rests by the same law, each as the exit of a long.
//! - `foreign quote` (decision 0243, phase 2): a symbol quoted in USDT on a DOLLAR ledger, converted
//!   by one USDT/USD pair on both sides (`StrategyEngineConfig::conversions` here, the
//!   journaled admission's rate there): the fills bit for bit in the quote currency, each fee the book
//!   states times its fill's rate is the simulator's, and the engine's realized PnL, fees and position
//!   are the simulator's.
//! - `two mounts` (the portfolio run, `StrategyEngine::new_mounted`): two strategies on one series of
//!   one account, the first under a leverage cap (`vike_execution::MountBudget::max_leverage`), here
//!   and as two mounts of the paper core: each mount hears the same fills bit for bit and the same
//!   `over-mount-leverage` denial, and books the same position and realized PnL; the capped mount's
//!   reversal (netted on its own row) and its close of its own short while the account is long (a covered
//!   reduce of its own row) pass on both; and over TWO symbols, a reduce-only order, an order list's OTO
//!   child (one priced above its first leg), an OCO exit, an order on the second symbol, an amend up and two
//!   orders of one callback in two verbs get the same verdict on both (each order judged alone).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, LiveBroker, Mount, MountLeg, spawn_core};
use vike_execution::{Account, BalanceMode, BarUpdate, Engine, RiskGate};
use vike_marketdata::Bar;
use vike_model::RiskLimits;
use vike_model::{
    FeeSchedule, Fill, LadderSpec, LadderTarget, OrderAttributes, OrderBroker, OrderCombo,
    OrderComboLeg, OrderListLeg, OrderPlacement, OrderStopAdjust, OrderStopAdjustTo, OrderType,
    Strategy, TimeInForce,
};
use vike_paper::{ExecutionClientPaper, replay_bars};
use vike_simulator::{
    OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, StrategyEngineMount,
};

const VENUE: &str = "paper";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";
const QTY: f64 = 1.0;
const FAST: usize = 5;
const SLOW: usize = 20;
const SLIPPAGE: f64 = 0.0002;
const MAKER: f64 = 0.0002;
const TAKER: f64 = 0.0007;

/// THE shared signal: SMA(FAST) vs SMA(SLOW) over closes → target sign (+1/-1/0=hold).
fn sma_signal(closes: &[f64]) -> i32 {
    if closes.len() < SLOW {
        return 0;
    }
    let mean = |w: &[f64]| w.iter().copied().fold(0.0, |a, b| a + b) / w.len() as f64;
    let fast = mean(&closes[closes.len() - FAST..]);
    let slow = mean(&closes[closes.len() - SLOW..]);
    if fast > slow { 1 } else { -1 }
}

/// Shared decision: given the signal + current signed position, the market delta order.
fn decide(signal: i32, position: f64) -> Option<(i32, f64)> {
    if signal == 0 {
        return None;
    }
    let target = signal as f64 * QTY;
    let delta = target - position;
    if delta.abs() <= 1e-12 {
        return None;
    }
    Some((if delta > 0.0 { 1 } else { -1 }, delta.abs()))
}

// --- THE strategy: written ONCE against the unified OrderBroker seam, runs on BOTH stacks ---
// (backtest: the sim engine core, direct mutation; paper/live: LiveContext, deferred submissions
// through mint → RiskGate → client). This single impl replacing the former GateSmaBacktest +
// GateSmaLive pair IS the write-once property the gate proves. Its order is the taker strategies'
// shape: an UNTAGGED market `place` (tag `""`), which both stacks route down the untagged lane.

#[derive(Default)]
struct GateSma {
    fills: Vec<Fill>,
}

impl<B: OrderBroker> Strategy<B> for GateSma {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        // backtest bars are symbol-tagged; the live broker is single-scoped and ignores it
        let symbol = bar.symbol.unwrap_or_default();
        let closes: Vec<f64> = broker.bars(&symbol).iter().map(|b| b.close).collect();
        if let Some((side, qty)) = decide(sma_signal(&closes), broker.position(&symbol)) {
            broker.place(&symbol, "", side, qty, OrderPlacement::Market, false);
        }
    }

    /// fires in backtest only (the live side journals fills through the engine instead)
    fn on_fill(&mut self, _broker: &mut B, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[test]
fn backtest_equals_paper_core() {
    let bars = vike_simulator::test_support::gen_bars(400, None);

    // ---- backtest (the R2 engine) ----
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    let mut engine =
        StrategyEngine::new(vec![(SYMBOL.to_string(), bars.clone())], GateSma::default(), params);
    let result = engine.run();
    let engine_fills = engine.strategy.fills.clone();
    let engine_position = engine.core.position_of(SYMBOL).size;
    let engine_realized: f64 = result.trades.iter().map(|t| t.pnl).fold(0.0, |a, b| a + b);
    let engine_fees: f64 = engine_fills.iter().map(|f| f.fee).fold(0.0, |a, b| a + b);
    assert!(engine_fills.len() > 10, "the gate needs real trading: {} fills", engine_fills.len());

    // ---- paper (the live core + ExecutionClientPaper + BacktestDataClient replay) ----
    let client = ExecutionClientPaper::new(VENUE, SYMBOL, SLIPPAGE, MAKER, TAKER);
    let paper_fills: Arc<Mutex<Vec<Fill>>> = Arc::clone(&client.fills);
    let engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        VENUE,
        SYMBOL,
    );
    let handle = spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(Mount {
                order_client_id_code: None,
                account: None,
                symbols: Vec::new(),
                controller_id: None,
                underlying_symbol: None,
                venue: VENUE.to_string(),
                symbol: SYMBOL.to_string(),
                interval: INTERVAL.to_string(),
                strategy: Box::new(GateSma::default()),
            }),
            ..CoreConfig::default()
        },
    );
    let cell = handle.snapshot_cell();
    replay_bars(&handle.bar_sender(), VENUE, SYMBOL, INTERVAL, &bars);
    // the replay is lossless+ordered; wait for the core to drain it
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let done = paper_fills.lock().unwrap().len() >= engine_fills.len();
        if done || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.shutdown_and_join();
    let snap = cell.load_full();
    let paper: Vec<Fill> = paper_fills.lock().unwrap().clone();

    // ---- THE gate: bit-identical fills ----
    assert_eq!(paper.len(), engine_fills.len(), "fill count");
    for (i, (p, e)) in paper.iter().zip(&engine_fills).enumerate() {
        assert_eq!(p.timestamp, e.timestamp, "fill[{i}] ts");
        assert_eq!(p.side, e.side, "fill[{i}] side");
        assert_eq!(p.size.to_bits(), e.size.to_bits(), "fill[{i}] qty");
        assert_eq!(p.price.to_bits(), e.price.to_bits(), "fill[{i}] px");
        assert_eq!(p.fee.to_bits(), e.fee.to_bits(), "fill[{i}] fee");
        assert_eq!(p.is_maker, e.is_maker, "fill[{i}] maker");
    }

    // realized / fees / position — identical folds over identical fills
    assert_eq!(
        snap.portfolio.realized_pnl.to_bits(),
        engine_realized.to_bits(),
        "realized: paper {} vs engine {}",
        snap.portfolio.realized_pnl,
        engine_realized
    );
    assert_eq!(
        snap.portfolio.fees_paid.to_bits(),
        engine_fees.to_bits(),
        "fees: paper {} vs engine {}",
        snap.portfolio.fees_paid,
        engine_fees
    );
    let paper_pos = snap.positions.iter().find(|p| p.symbol == SYMBOL).map_or(0.0, |p| p.size);
    assert_eq!(
        paper_pos.to_bits(),
        engine_position.to_bits(),
        "final position: paper {paper_pos} vs engine {engine_position}"
    );
    assert!(snap.fault.is_none());
    println!(
        "r7 gate green: {} fills bit-identical; realized {}; fees {}; position {}",
        paper.len(),
        snap.portfolio.realized_pnl,
        snap.portfolio.fees_paid,
        paper_pos
    );
}

// --- conditional-verb rows (trigger-law wave 2) -------------------------------------------
//
// The stop/bracket verbs have no portable `OrderBroker` surface (live arms the emulator / lowers a
// bracket triple), so each row pairs two thin strategy impls sharing the same decision
// constants — the pre-unification gate shape, kept deliberately small.

/// Run the paper stack (core + ExecutionClientPaper) over `bars` with `strategy`, waiting until
/// at least `expect_fills` paper fills landed (or timeout), and return the fill stream.
fn run_paper_stack(
    bars: &[Bar],
    strategy: Box<dyn Strategy<LiveBroker> + Send>,
    expect_fills: usize,
) -> Vec<Fill> {
    let client = ExecutionClientPaper::new(VENUE, SYMBOL, SLIPPAGE, MAKER, TAKER);
    run_paper_client(client, bars, strategy, expect_fills)
}

/// [`run_paper_stack`] over a client the row built itself.
fn run_paper_client(
    client: ExecutionClientPaper,
    bars: &[Bar],
    strategy: Box<dyn Strategy<LiveBroker> + Send>,
    expect_fills: usize,
) -> Vec<Fill> {
    let paper_fills: Arc<Mutex<Vec<Fill>>> = Arc::clone(&client.fills);
    let engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        VENUE,
        SYMBOL,
    );
    let handle = spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(Mount {
                order_client_id_code: None,
                account: None,
                symbols: Vec::new(),
                controller_id: None,
                underlying_symbol: None,
                venue: VENUE.to_string(),
                symbol: SYMBOL.to_string(),
                interval: INTERVAL.to_string(),
                strategy,
            }),
            ..CoreConfig::default()
        },
    );
    replay_bars(&handle.bar_sender(), VENUE, SYMBOL, INTERVAL, bars);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let done = paper_fills.lock().unwrap().len() >= expect_fills;
        if done || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.shutdown_and_join();

    paper_fills.lock().unwrap().clone()
}

/// Assert the two fill streams are bit-identical (the gate's comparison law).
fn assert_fills_identical(paper: &[Fill], engine_fills: &[Fill]) {
    assert_eq!(paper.len(), engine_fills.len(), "fill count");
    for (i, (p, e)) in paper.iter().zip(engine_fills).enumerate() {
        assert_eq!(p.timestamp, e.timestamp, "fill[{i}] ts");
        assert_eq!(p.side, e.side, "fill[{i}] side");
        assert_eq!(p.size.to_bits(), e.size.to_bits(), "fill[{i}] qty");
        assert_eq!(p.price.to_bits(), e.price.to_bits(), "fill[{i}] px");
        assert_eq!(p.fee.to_bits(), e.fee.to_bits(), "fill[{i}] fee");
        assert_eq!(p.is_maker, e.is_maker, "fill[{i}] maker");
    }
}

// ---- row: a fee SCHEDULE (the paper arm the mount uses, against the simulator's) ----

/// `ExecutionClientPaper::new` is the flat-rate arm, and a mount is built `with_fee_schedule`, so
/// the schedule arm was never held against the simulator. The floor is the exact path on both
/// sides (`OrderBrokerSimulator`'s `fee_curve`); the percent shape is the one the simulator flattens to rates,
/// so bit identity here also holds its "identical arithmetic" claim to the paper number.
#[test]
fn a_fee_schedule_charges_the_same_on_paper_and_in_the_engine() {
    let bars = vike_simulator::test_support::gen_bars(400, None);
    for schedule in [
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 0.005 },
        FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 7.0 },
    ] {
        let params = StrategyEngineConfig {
            cash: 10_000.0,
            slippage: SLIPPAGE,
            fee_schedule: Some(schedule),
            ..StrategyEngineConfig::default()
        };
        let mut engine = StrategyEngine::new(
            vec![(SYMBOL.to_string(), bars.clone())],
            GateSma::default(),
            params,
        );
        engine.run();
        let engine_fills = engine.strategy.fills.clone();
        assert!(engine_fills.len() > 10, "the row needs real trading: {}", engine_fills.len());
        assert!(engine_fills.iter().all(|f| f.fee > 0.0), "{schedule:?} must charge something");

        let client = ExecutionClientPaper::with_fee_schedule(VENUE, SYMBOL, SLIPPAGE, schedule);
        let paper =
            run_paper_client(client, &bars, Box::new(GateSma::default()), engine_fills.len());
        assert_fills_identical(&paper, &engine_fills);
        println!("r7 schedule row green: {schedule:?}, {} fills bit-identical", paper.len());
    }
}

// ---- row: a slippage table (the paper book takes the backtest's model) ----

/// A table of `model` with the keys `set` names, every other key absent.
fn slippage_table(
    model: &str,
    set: impl FnOnce(&mut vike_model::ProfileEngineSlippage),
) -> vike_model::ProfileEngineSlippage {
    let mut table = vike_model::ProfileEngineSlippage {
        model: model.to_string(),
        value: None,
        ticks: None,
        tick: None,
        base: None,
        exec_time: None,
        window: None,
        gamma: None,
        eta: None,
    };
    set(&mut table);
    table
}

/// **The paper book charges what the engine charges for one slippage table**, the
/// `market_impact` model included: both resolve the table through the ONE
/// `vike_fills::market_impact::slippage_terms`, the engine's bar lane and the paper book price a
/// taker by the ONE `ImpactModelAlmgrenChriss` over the bars that closed before the fill
/// (`window_stats_before`), and the `ticks` amount by the ONE `vike_fills::broker_sim::adverse_fill_amount`.
/// The fills are bit-identical, and the impact row is held to MOVE the price against the flat-only run,
/// so the identity is not two runs that both charged nothing.
#[test]
fn a_slippage_table_charges_the_same_on_paper_and_in_the_engine() {
    let bars = vike_simulator::test_support::gen_bars(400, None);
    let run_engine = |terms: &vike_fills::market_impact::ProfileEngineSlippageTerms| {
        let params = StrategyEngineConfig {
            cash: 10_000.0,
            slippage: terms.fraction,
            slippage_amount: terms.amount,
            impact: terms.impact.as_ref().map(|(model, _)| Arc::clone(model)),
            impact_window: terms
                .impact
                .as_ref()
                .map_or(vike_fills::market_impact::DEFAULT_IMPACT_WINDOW, |(_, w)| *w),
            maker_fee: Some(MAKER),
            taker_fee: Some(TAKER),
            ..StrategyEngineConfig::default()
        };
        let mut engine = StrategyEngine::new(
            vec![(SYMBOL.to_string(), bars.clone())],
            GateSma::default(),
            params,
        );
        engine.run();
        engine.strategy.fills.clone()
    };
    let flat = run_engine(&vike_fills::market_impact::ProfileEngineSlippageTerms {
        fraction: SLIPPAGE,
        amount: 0.0,
        impact: None,
    });
    for table in [
        slippage_table("market_impact", |t| {
            t.base = Some(SLIPPAGE);
            t.window = Some(10);
        }),
        slippage_table("market_impact", |t| {
            t.base = Some(SLIPPAGE);
            t.gamma = Some(0.9);
            t.eta = Some(0.05);
            t.exec_time = Some(3.0);
        }),
        slippage_table("ticks", |t| {
            t.ticks = Some(2.0);
            t.tick = Some(0.01);
        }),
        slippage_table("percent", |t| t.value = Some(0.0005)),
    ] {
        let terms = vike_fills::market_impact::slippage_terms(Some(&table)).expect("valid");
        let engine_fills = run_engine(&terms);
        assert!(engine_fills.len() > 10, "the row needs real trading: {}", engine_fills.len());
        let client = ExecutionClientPaper::new(VENUE, SYMBOL, SLIPPAGE, MAKER, TAKER)
            .with_slippage_terms(terms.clone());
        let paper =
            run_paper_client(client, &bars, Box::new(GateSma::default()), engine_fills.len());
        assert_fills_identical(&paper, &engine_fills);
        if table.model == "market_impact" {
            let moved = engine_fills.iter().zip(&flat).any(|(f, g)| f.price != g.price);
            assert!(
                moved,
                "{table:?}: the impact model charged no fill, so the row proves nothing"
            );
        }
        println!("r7 slippage row green: {table:?}, {} fills bit-identical", paper.len());
    }
}

// ---- row: the stop VERB (emulator_release_stops mirrors the live OrderEmulationBook) ----

const STOP_CYCLE: usize = 40;

/// shared decision constants: enter long on cycle bars when flat; once long, arm ONE
/// protective sell stop 0.5% under the arming bar's close (tight enough that the LCG tape
/// stops out every cycle — the row needs the RELEASE path to actually run, repeatedly).
fn stop_row_entry_due(index: usize) -> bool {
    index % STOP_CYCLE == 5
}
fn stop_row_stop_price(close: f64) -> f64 {
    close * 0.995
}

#[derive(Default)]
struct StopVerbSim {
    armed: bool,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for StopVerbSim {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol = bar.symbol.unwrap_or_default();
        if ctx.position_of(&symbol).size == 0.0 {
            self.armed = false;
            if stop_row_entry_due(ctx.index) {
                ctx.submit(&symbol, 1, QTY, 0.0, false, None);
            }
        } else if !self.armed {
            self.armed = true;
            ctx.submit_stop(&symbol, -1, QTY, stop_row_stop_price(bar.close), 0.0, false);
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[derive(Default)]
struct StopVerbLive {
    armed: bool,
}

impl Strategy<LiveBroker> for StopVerbLive {
    fn on_bar(&mut self, ctx: &mut LiveBroker, bar: &Bar) {
        if OrderBroker::position(ctx, SYMBOL) == 0.0 {
            self.armed = false;
            if stop_row_entry_due(ctx.index) {
                ctx.place(SYMBOL, "", 1, QTY, OrderPlacement::Market, false);
            }
        } else if !self.armed {
            self.armed = true;
            // live: ALWAYS emulator-armed — fires as a market through the one live path
            ctx.submit_stop(-1, QTY, stop_row_stop_price(bar.close));
        }
    }
}

#[test]
fn stop_verb_release_mode_equals_paper_core() {
    let bars = vike_simulator::test_support::gen_bars(400, None);

    // engine, with the opt-in emulator mirror ON (the live-law mode)
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        emulator_release_stops: true,
        ..StrategyEngineConfig::default()
    };
    let mut engine = StrategyEngine::new(
        vec![(SYMBOL.to_string(), bars.clone())],
        StopVerbSim::default(),
        params,
    );
    engine.run();
    let engine_fills = engine.strategy.fills.clone();
    assert!(engine_fills.len() >= 4, "the row needs real stop-outs: {}", engine_fills.len());

    let paper = run_paper_stack(&bars, Box::new(StopVerbLive::default()), engine_fills.len());
    assert_fills_identical(&paper, &engine_fills);
    println!("r7 stop-verb row green: {} fills bit-identical", paper.len());
}

// ---- row: the BRACKET (protective stop + TP vs the OTO/OCO triple) ----

const BRACKET_CYCLE: usize = 60;

fn bracket_row_entry_due(index: usize) -> bool {
    index % BRACKET_CYCLE == 5
}
fn bracket_row_stop_price(close: f64) -> f64 {
    close * 0.98
}
fn bracket_row_take_profit_price(close: f64) -> f64 {
    close * 1.02
}

#[derive(Default)]
struct BracketSim {
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for BracketSim {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol = bar.symbol.unwrap_or_default();
        if ctx.position_of(&symbol).size == 0.0
            && ctx.pending_of(&symbol).is_empty()
            && bracket_row_entry_due(ctx.index)
        {
            // entry + protective stop (the implicit kind-linked bracket) + resting TP limit
            ctx.submit(&symbol, 1, QTY, 0.0, false, Some(bracket_row_stop_price(bar.close)));
            ctx.submit_limit(
                &symbol,
                -1,
                QTY,
                bracket_row_take_profit_price(bar.close),
                0.0,
                false,
                None,
            );
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[derive(Default)]
struct BracketLive;

impl Strategy<LiveBroker> for BracketLive {
    fn on_bar(&mut self, ctx: &mut LiveBroker, bar: &Bar) {
        // the live side cannot see the paper book's resting orders; the cycle spacing
        // guarantees the prior bracket resolved — the guarantee the sim side's `pending_of`
        // check enforces explicitly
        if OrderBroker::position(ctx, SYMBOL) == 0.0 && bracket_row_entry_due(ctx.index) {
            ctx.submit_bracket(
                1,
                QTY,
                None,
                bracket_row_stop_price(bar.close),
                bracket_row_take_profit_price(bar.close),
            );
        }
    }
}

#[test]
fn bracket_row_equals_paper_core() {
    let bars = vike_simulator::test_support::gen_bars(400, None);

    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    let mut engine = StrategyEngine::new(
        vec![(SYMBOL.to_string(), bars.clone())],
        BracketSim::default(),
        params,
    );
    engine.run();
    let engine_fills = engine.strategy.fills.clone();
    assert!(engine_fills.len() >= 4, "the row needs entries + exits: {}", engine_fills.len());

    let paper = run_paper_stack(&bars, Box::new(BracketLive), engine_fills.len());
    assert_fills_identical(&paper, &engine_fills);
    println!("r7 bracket row green: {} fills bit-identical", paper.len());
}

// ---- rows: the STOP-LIMIT and TAKE-PROFIT verbs (wave 1 of the new order kinds) ----
//
// Both are ORDERS the paper book rests and fills by the one law (`vike_model::order_fill_price`,
// reached through `OrderResting::from_request`), so the engine runs its default mode: no emulator
// mirror. The limits sit where some trigger bars reach them and some do not, so each row books a
// taker exit on the trigger event AND a maker exit after the conversion to the plain `Limit`.

const KIND_CYCLE: usize = 40;

fn kind_row_entry_due(index: usize) -> bool {
    index % KIND_CYCLE == 5
}

/// A SELL stop-limit 0.5% under the arming close, its limit AT that close: a trigger bar that comes
/// back up to it fills there as a taker, one that does not rests it as the `Limit` there.
fn stop_limit_row_levels(close: f64) -> (f64, f64) {
    (close * 0.995, close)
}

/// A SELL take-profit 0.3% over the arming close: a market-if-touched on even cycles, a
/// limit-if-touched with its limit 0.6% over the close on odd ones.
fn take_profit_row_levels(index: usize, close: f64) -> (f64, Option<f64>) {
    (close * 1.003, ((index / KIND_CYCLE) % 2 == 1).then_some(close * 1.006))
}

#[derive(Debug, Clone, Copy)]
enum KindRow {
    StopLimit,
    TakeProfit,
}

struct KindRowSim {
    row: KindRow,
    armed: bool,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for KindRowSim {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol = bar.symbol.unwrap_or_default();
        if ctx.position_of(&symbol).size == 0.0 {
            self.armed = false;
            if kind_row_entry_due(ctx.index) {
                ctx.submit(&symbol, 1, QTY, 0.0, false, None);
            }
        } else if !self.armed {
            self.armed = true;
            match self.row {
                KindRow::StopLimit => {
                    let (trigger, limit) = stop_limit_row_levels(bar.close);
                    ctx.submit_stop_limit(&symbol, -1, QTY, trigger, limit, 0.0, false);
                }
                KindRow::TakeProfit => {
                    let (trigger, limit) = take_profit_row_levels(ctx.index, bar.close);
                    ctx.submit_take_profit(&symbol, -1, QTY, trigger, limit, 0.0, false);
                }
            }
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

struct KindRowLive {
    row: KindRow,
    armed: bool,
}

impl Strategy<LiveBroker> for KindRowLive {
    fn on_bar(&mut self, ctx: &mut LiveBroker, bar: &Bar) {
        if OrderBroker::position(ctx, SYMBOL) == 0.0 {
            self.armed = false;
            if kind_row_entry_due(ctx.index) {
                ctx.place(SYMBOL, "", 1, QTY, OrderPlacement::Market, false);
            }
        } else if !self.armed {
            self.armed = true;
            match self.row {
                KindRow::StopLimit => {
                    let (trigger, limit) = stop_limit_row_levels(bar.close);
                    ctx.submit_stop_limit(-1, QTY, trigger, limit);
                }
                KindRow::TakeProfit => {
                    let (trigger, limit) = take_profit_row_levels(ctx.index, bar.close);
                    ctx.submit_take_profit(-1, QTY, trigger, limit);
                }
            }
        }
    }
}

/// The engine's fills for `row`, then the paper stack's, bit for bit.
fn kind_row_equals_paper_core(row: KindRow) {
    let bars = vike_simulator::test_support::gen_bars(400, None);
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    let sim = KindRowSim { row, armed: false, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYMBOL.to_string(), bars.clone())], sim, params);
    engine.run();
    let engine_fills = engine.strategy.fills.clone();
    let exits =
        |maker: bool| engine_fills.iter().filter(|f| f.side < 0 && f.is_maker == maker).count();
    let (taker, maker) = (exits(false), exits(true));
    assert!(taker > 0 && maker > 0, "{row:?}: the row needs both fee classes: {engine_fills:?}");

    let live = KindRowLive { row, armed: false };
    let paper = run_paper_stack(&bars, Box::new(live), engine_fills.len());
    assert_fills_identical(&paper, &engine_fills);
    println!(
        "r7 {row:?} row green: {} fills bit-identical ({taker} taker, {maker} maker exits)",
        paper.len()
    );
}

#[test]
fn stop_limit_verb_equals_paper_core() {
    kind_row_equals_paper_core(KindRow::StopLimit);
}

#[test]
fn take_profit_verb_equals_paper_core() {
    kind_row_equals_paper_core(KindRow::TakeProfit);
}

// ---- rows: the ORDER LISTS (wave 4 of the new order kinds) ----
//
// Each pairs two thin impls over the same decision helpers, as the bracket row does. The OCO pair is
// armed over a long the way the kind rows arm their exit; the ladder is placed flat, as the bracket.

const LIST_CYCLE: usize = 60;

fn list_row_entry_due(index: usize) -> bool {
    index % LIST_CYCLE == 5
}

/// A reduce-only GTC SELL exit of `qty`: a limit at `price`, or a stop at `trigger_price`.
fn exit_leg(
    qty: f64,
    order_type: OrderType,
    price: Option<f64>,
    trigger_price: Option<f64>,
) -> OrderListLeg {
    OrderListLeg {
        side: -1,
        qty,
        order_type,
        price,
        trigger_price,
        trail: None,
        reduce_only: true,
        time_in_force: TimeInForce::Gtc,
        parent: None,
        contingency: None,
        linked: Vec::new(),
        order_client_id: None,
    }
}

/// The OCO row's exit pair over a long: a target limit 0.4% over the arming close and a stop 0.5%
/// under it (the verb links them).
fn oco_row_legs(close: f64) -> (OrderListLeg, OrderListLeg) {
    (
        exit_leg(QTY, OrderType::Limit, Some(close * 1.004), None),
        exit_leg(QTY, OrderType::Stop, None, Some(close * 0.995)),
    )
}

/// The ladder row's stop (1% under the placing close) and its two half-size targets (0.4% and 0.8% over).
fn ladder_row_levels(close: f64) -> (f64, Vec<LadderTarget>) {
    let half = QTY / 2.0;
    let targets = vec![
        LadderTarget { price: close * 1.004, qty: half },
        LadderTarget { price: close * 1.008, qty: half },
    ];
    (close * 0.99, targets)
}

#[derive(Debug, Clone, Copy)]
enum ListRow {
    Oco,
    Ladder,
}

struct ListRowSim {
    row: ListRow,
    armed: bool,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for ListRowSim {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol_name = bar.symbol.map(String::from).unwrap_or_default();
        let flat = ctx.position_of(&symbol_name).size == 0.0;
        match self.row {
            ListRow::Oco if flat => {
                self.armed = false;
                if list_row_entry_due(ctx.index) {
                    ctx.submit(&symbol_name, 1, QTY, 0.0, false, None);
                }
            }
            ListRow::Oco if !self.armed => {
                self.armed = true;
                let (target, stop) = oco_row_legs(bar.close);
                ctx.submit_oco(&symbol_name, target, stop);
            }
            ListRow::Oco => {}
            ListRow::Ladder => {
                if flat && ctx.pending_of(&symbol_name).is_empty() && list_row_entry_due(ctx.index)
                {
                    let (stop_loss, targets) = ladder_row_levels(bar.close);
                    ctx.submit_ladder(&LadderSpec {
                        venue: VENUE.to_string(),
                        symbol: symbol_name,
                        side: 1,
                        qty: QTY,
                        entry_price: None,
                        stop_loss,
                        targets,
                        account: None,
                    });
                }
            }
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

struct ListRowLive {
    row: ListRow,
    armed: bool,
}

impl Strategy<LiveBroker> for ListRowLive {
    fn on_bar(&mut self, ctx: &mut LiveBroker, bar: &Bar) {
        let flat = OrderBroker::position(ctx, SYMBOL) == 0.0;
        match self.row {
            ListRow::Oco if flat => {
                self.armed = false;
                if list_row_entry_due(ctx.index) {
                    ctx.place(SYMBOL, "", 1, QTY, OrderPlacement::Market, false);
                }
            }
            ListRow::Oco if !self.armed => {
                self.armed = true;
                let (target, stop) = oco_row_legs(bar.close);
                ctx.submit_oco(target, stop);
            }
            ListRow::Oco => {}
            // the cycle spacing guarantees the prior ladder resolved, as the bracket row's live side
            ListRow::Ladder => {
                if flat && list_row_entry_due(ctx.index) {
                    let (stop_loss, targets) = ladder_row_levels(bar.close);
                    ctx.submit_ladder(1, QTY, None, stop_loss, targets);
                }
            }
        }
    }
}

/// The engine's fills for `row`, then the paper stack's, bit for bit. Non-vacuous: the row books
/// exits of both fee classes (a target as a maker, a stop as a taker), and the ladder a half-size
/// target.
fn list_row_equals_paper_core(row: ListRow) {
    let bars = vike_simulator::test_support::gen_bars(400, None);
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    let sim = ListRowSim { row, armed: false, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYMBOL.to_string(), bars.clone())], sim, params);
    engine.run();
    let engine_fills = engine.strategy.fills.clone();
    let exits =
        |maker: bool| engine_fills.iter().filter(|f| f.side < 0 && f.is_maker == maker).count();
    let (taker, maker) = (exits(false), exits(true));
    assert!(taker > 0 && maker > 0, "{row:?}: the row needs both fee classes: {engine_fills:?}");
    if matches!(row, ListRow::Ladder) {
        assert!(
            engine_fills.iter().any(|f| f.side < 0 && f.size < QTY),
            "the ladder row needs a partial target: {engine_fills:?}"
        );
    }

    let live = ListRowLive { row, armed: false };
    let paper = run_paper_stack(&bars, Box::new(live), engine_fills.len());
    assert_fills_identical(&paper, &engine_fills);
    println!(
        "r7 {row:?} row green: {} fills bit-identical ({taker} taker, {maker} maker exits)",
        paper.len()
    );
}

#[test]
fn oco_exit_pair_equals_paper_core() {
    list_row_equals_paper_core(ListRow::Oco);
}

#[test]
fn bracket_ladder_equals_paper_core() {
    list_row_equals_paper_core(ListRow::Ladder);
}

// ---- rows: COMBOS (wave 6, decision 0266) ----
//
// Two symbols, NEAR then FAR in every step (the backtest's cross-section order, the replay's
// order). The desk decides on FAR's bar, the step's last, so both sides read the same positions:
// a combo the step filled is folded before either side's FAR dispatch. `deribit` is the one venue
// row that takes a combo; a combo priced by its legs goes out only on an engine whose STATED tier
// is paper (`lower_combo`).

const COMBO_VENUE: &str = "deribit";
const NEAR: &str = "NEAR";
const FAR: &str = "FAR";
const COMBO_CYCLE: i64 = 20;

/// NEAR is `gen_bars`' walk; FAR its continuation (bars 400..800 restamped onto NEAR's clock), so
/// the spread between them moves.
fn combo_bars() -> (Vec<Bar>, Vec<Bar>) {
    let near = vike_simulator::test_support::gen_bars(400, Some(NEAR));
    let far = vike_simulator::test_support::gen_bars(800, Some(FAR))[400..]
        .iter()
        .zip(&near)
        .map(|(b, n)| Bar { timestamp: n.timestamp, ..b.clone() })
        .collect();
    (near, far)
}

#[derive(Debug, Clone, Copy)]
enum ComboRow {
    /// `+1 NEAR -1 FAR` at a NET limit, this step's net
    Net,
    /// the same legs, each at a limit of its own, this step's close
    Legs,
}

/// THE shared decision. On FAR's bar of a cycle step, with no combo of its own still resting (the
/// NEAR position it was placed at has not moved), buy the spread when flat and sell it when long, at
/// this step's closes.
struct ComboDesk {
    row: ComboRow,
    resting_at: Option<f64>,
}

impl ComboDesk {
    fn decide<B: OrderBroker>(
        &mut self,
        b: &B,
        bar: &Bar,
    ) -> Option<(i32, Vec<OrderComboLeg>, Option<f64>)> {
        if bar.symbol.as_deref() != Some(FAR) {
            return None;
        }
        let pos = b.position(NEAR);
        if self.resting_at == Some(pos) {
            return None;
        }
        self.resting_at = None;
        if (bar.timestamp / 60_000) % COMBO_CYCLE != 5 {
            return None;
        }
        let near = b.bars(NEAR).last().map(|n| n.close)?;
        let side = vike_model::closing_side(pos);
        let leg = |symbol: &str, ratio, px: f64| OrderComboLeg {
            symbol: symbol.into(),
            ratio,
            limit: matches!(self.row, ComboRow::Legs).then_some(px),
        };
        let legs = vec![leg(NEAR, 1, near), leg(FAR, -1, bar.close)];
        let net = matches!(self.row, ComboRow::Net).then_some(near - bar.close);
        self.resting_at = Some(pos);
        Some((side, legs, net))
    }
}

struct ComboSim {
    desk: ComboDesk,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for ComboSim {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        if let Some((side, legs, net_limit)) = self.desk.decide(&*ctx, bar) {
            let spec = OrderCombo {
                venue: COMBO_VENUE.into(),
                side,
                qty: QTY,
                legs,
                net_limit,
                time_in_force: TimeInForce::Gtc,
                account: None,
                order_client_id: None,
            };
            assert!(ctx.submit_combo(&spec), "the combo rests: {:?}", ctx.dropped);
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

struct ComboLive {
    desk: ComboDesk,
}

impl Strategy<LiveBroker> for ComboLive {
    fn on_bar(&mut self, ctx: &mut LiveBroker, bar: &Bar) {
        if let Some((side, legs, net_limit)) = self.desk.decide(&*ctx, bar) {
            ctx.submit_combo(side, QTY, legs, net_limit);
        }
    }
}

/// The paper stack over both symbols: a `deribit` paper engine stated `paper`, the mount on NEAR
/// declaring FAR, the bars replayed NEAR then FAR per step.
fn run_combo_paper(row: ComboRow, near: &[Bar], far: &[Bar], expect_fills: usize) -> Vec<Fill> {
    let client = ExecutionClientPaper::new(COMBO_VENUE, NEAR, SLIPPAGE, MAKER, TAKER);
    let paper_fills: Arc<Mutex<Vec<Fill>>> = Arc::clone(&client.fills);
    let mut engine = Engine::new(
        Account::new(1.0, COMBO_VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        COMBO_VENUE,
        NEAR,
    );
    engine.extra_symbols = vec![FAR.into()];
    engine.set_tier(vike_model::AccountTier::Paper);
    let live = ComboLive { desk: ComboDesk { row, resting_at: None } };
    let handle = spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(Mount {
                order_client_id_code: None,
                account: None,
                symbols: vec![MountLeg::same_venue(FAR)],
                controller_id: None,
                underlying_symbol: None,
                venue: COMBO_VENUE.to_string(),
                symbol: NEAR.to_string(),
                interval: INTERVAL.to_string(),
                strategy: Box::new(live),
            }),
            ..CoreConfig::default()
        },
    );
    for (n, f) in near.iter().zip(far) {
        for (symbol, bar) in [(NEAR, n), (FAR, f)] {
            let update = BarUpdate {
                venue: COMBO_VENUE.into(),
                symbol: symbol.into(),
                interval: INTERVAL.into(),
                // live bars arrive symbol-less; the runtime stamps the series symbol
                bar: Bar { symbol: None, ..bar.clone() },
                sides: None,
            };
            handle.bar_sender().close(update).expect("core alive during replay");
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while paper_fills.lock().unwrap().len() < expect_fills && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.shutdown_and_join();
    paper_fills.lock().unwrap().clone()
}

/// The engine's combo fills for `row`, then the paper stack's, bit for bit. Non-vacuous: several
/// combos fill, both ways (a bought spread and a sold one), every one with its two legs at one ts,
/// and at least one rested past the step after it was placed.
fn combo_row_equals_paper_core(row: ComboRow) {
    let (near, far) = combo_bars();
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    let sim = ComboSim { desk: ComboDesk { row, resting_at: None }, fills: Vec::new() };
    let symbols = vec![(NEAR.to_string(), near.clone()), (FAR.to_string(), far.clone())];
    let mut engine = StrategyEngine::new(symbols, sim, params);
    engine.run();
    let engine_fills = engine.strategy.fills.clone();
    assert!(engine_fills.len() >= 6, "{row:?}: the row needs several combos: {engine_fills:?}");
    assert!(
        engine_fills.chunks(2).all(|p| p.len() == 2 && p[0].timestamp == p[1].timestamp),
        "{row:?}: legs pair"
    );
    assert!(engine_fills.iter().all(|f| f.is_maker), "{row:?}: a priced combo books maker");
    let sold = engine_fills.iter().step_by(2).filter(|f| f.side < 0).count();
    assert!(
        sold > 0 && sold < engine_fills.len() / 2,
        "{row:?}: bought and sold: {engine_fills:?}"
    );
    let late = engine_fills.iter().step_by(2).any(|f| (f.timestamp / 60_000) % COMBO_CYCLE != 6);
    assert!(late, "{row:?}: one combo must rest past the next step: {engine_fills:?}");

    let paper = run_combo_paper(row, &near, &far, engine_fills.len());
    assert_fills_identical(&paper, &engine_fills);
    println!("r7 combo {row:?} row green: {} leg fills bit-identical", paper.len());
}

#[test]
fn net_limit_combo_equals_paper_core() {
    combo_row_equals_paper_core(ComboRow::Net);
}

#[test]
fn leg_limit_combo_equals_paper_core() {
    combo_row_equals_paper_core(ComboRow::Legs);
}

// ---- rows: the ATTRIBUTES wave (decision 0267) ----
//
// Each is the SELL exit of a long, armed the way the kind rows arm theirs, and an ORDER the paper book
// rests by the one law (`vike_model::order_fill_price`), so the engine runs its default mode. The venue
// `paper` names no capability row, so the live core's preflight hands each to the paper book untouched.

#[derive(Debug, Clone, Copy)]
enum AttributeRow {
    TrailingTakeProfit,
    TrailPercent,
    AdjustableStop,
    AllOrNone,
    Discretionary,
}

/// A row's exit, in the terms both verbs take.
enum AttributeExit {
    TrailingTakeProfit { trail: f64, limit_offset: Option<f64> },
    TrailPercent { percent: f64 },
    AdjustableStop { stop: f64, adjust: OrderStopAdjust },
    Limit { price: f64, attributes: OrderAttributes },
}

/// The exit `row` arms over a long at the arming bar `index` closing at `close`:
/// - a trailing take-profit 0.3% of the close off the lowest price since it rests: a market-if-touched
///   on even cycles, a limit 0.1% of the close under its trigger on odd ones;
/// - a trailing stop 0.5% of its extreme;
/// - a stop 0.5% under the close that adjusts once the price is 0.3% over it: to a stop 0.1% over the
///   close on even cycles, to a stop-limit there with its limit at the close on odd ones;
/// - an all-or-none limit 0.3% over the close;
/// - a limit 0.4% over the close with a discretion of 0.2% of it, so it may sell down to 0.2% over.
fn attribute_row_exit(row: AttributeRow, index: usize, close: f64) -> AttributeExit {
    let odd = (index / KIND_CYCLE) % 2 == 1;
    match row {
        AttributeRow::TrailingTakeProfit => AttributeExit::TrailingTakeProfit {
            trail: close * 0.003,
            limit_offset: odd.then_some(close * 0.001),
        },
        AttributeRow::TrailPercent => AttributeExit::TrailPercent { percent: 0.5 },
        AttributeRow::AdjustableStop => {
            let stop = close * 1.001;
            let to = if odd {
                OrderStopAdjustTo::StopLimit { stop, limit: close }
            } else {
                OrderStopAdjustTo::Stop { stop }
            };
            AttributeExit::AdjustableStop {
                stop: close * 0.995,
                adjust: OrderStopAdjust { at: close * 1.003, to },
            }
        }
        AttributeRow::AllOrNone => AttributeExit::Limit {
            price: close * 1.003,
            attributes: OrderAttributes { all_or_none: true, ..OrderAttributes::default() },
        },
        AttributeRow::Discretionary => AttributeExit::Limit {
            price: close * 1.004,
            attributes: OrderAttributes {
                discretionary_amount: Some(close * 0.002),
                ..OrderAttributes::default()
            },
        },
    }
}

struct AttributeRowSim {
    row: AttributeRow,
    armed: bool,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for AttributeRowSim {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol = bar.symbol.unwrap_or_default();
        if ctx.position_of(&symbol).size == 0.0 {
            self.armed = false;
            if kind_row_entry_due(ctx.index) {
                ctx.submit(&symbol, 1, QTY, 0.0, false, None);
            }
        } else if !self.armed {
            self.armed = true;
            match attribute_row_exit(self.row, ctx.index, bar.close) {
                AttributeExit::TrailingTakeProfit { trail, limit_offset } => {
                    ctx.submit_trailing_take_profit(
                        &symbol,
                        -1,
                        QTY,
                        trail,
                        limit_offset,
                        0.0,
                        false,
                    );
                }
                AttributeExit::TrailPercent { percent } => {
                    let kind = OrderType::Trailing;
                    ctx.submit_trailing_percent(
                        &symbol, kind, -1, QTY, percent, None, None, 0.0, false,
                    );
                }
                AttributeExit::AdjustableStop { stop, adjust } => {
                    ctx.submit_stop_adjustable(&symbol, -1, QTY, stop, None, adjust, 0.0, false);
                }
                AttributeExit::Limit { price, attributes } => {
                    ctx.submit_limit_attributed(&symbol, -1, QTY, price, attributes, 0.0, false);
                }
            }
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

struct AttributeRowLive {
    row: AttributeRow,
    armed: bool,
}

impl Strategy<LiveBroker> for AttributeRowLive {
    fn on_bar(&mut self, ctx: &mut LiveBroker, bar: &Bar) {
        if OrderBroker::position(ctx, SYMBOL) == 0.0 {
            self.armed = false;
            if kind_row_entry_due(ctx.index) {
                ctx.place(SYMBOL, "", 1, QTY, OrderPlacement::Market, false);
            }
        } else if !self.armed {
            self.armed = true;
            match attribute_row_exit(self.row, ctx.index, bar.close) {
                AttributeExit::TrailingTakeProfit { trail, limit_offset } => {
                    ctx.submit_trailing_take_profit(-1, QTY, trail, limit_offset, false);
                }
                AttributeExit::TrailPercent { percent } => {
                    ctx.submit_trailing_percent(
                        OrderType::Trailing,
                        -1,
                        QTY,
                        percent,
                        None,
                        None,
                        false,
                    );
                }
                AttributeExit::AdjustableStop { stop, adjust } => {
                    ctx.submit_stop_adjustable(-1, QTY, stop, None, adjust, false);
                }
                AttributeExit::Limit { price, attributes } => {
                    ctx.submit_limit_attributed(-1, QTY, price, attributes);
                }
            }
        }
    }
}

/// The engine's fills for `row`, then the paper stack's, bit for bit. Non-vacuous: the row books
/// entries and at least two exits.
fn attribute_row_equals_paper_core(row: AttributeRow) {
    let bars = vike_simulator::test_support::gen_bars(400, None);
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    let sim = AttributeRowSim { row, armed: false, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYMBOL.to_string(), bars.clone())], sim, params);
    engine.run();
    let engine_fills = engine.strategy.fills.clone();
    let exits =
        |maker: bool| engine_fills.iter().filter(|f| f.side < 0 && f.is_maker == maker).count();
    let (taker, maker) = (exits(false), exits(true));
    assert!(taker + maker >= 2, "{row:?}: the row needs real exits: {engine_fills:?}");

    let live = AttributeRowLive { row, armed: false };
    let paper = run_paper_stack(&bars, Box::new(live), engine_fills.len());
    assert_fills_identical(&paper, &engine_fills);
    println!(
        "r7 {row:?} row green: {} fills bit-identical ({taker} taker, {maker} maker exits)",
        paper.len()
    );
}

#[test]
fn trailing_take_profit_verb_equals_paper_core() {
    attribute_row_equals_paper_core(AttributeRow::TrailingTakeProfit);
}

#[test]
fn percent_trailing_verb_equals_paper_core() {
    attribute_row_equals_paper_core(AttributeRow::TrailPercent);
}

#[test]
fn adjustable_stop_verb_equals_paper_core() {
    attribute_row_equals_paper_core(AttributeRow::AdjustableStop);
}

#[test]
fn all_or_none_limit_equals_paper_core() {
    attribute_row_equals_paper_core(AttributeRow::AllOrNone);
}

#[test]
fn discretionary_limit_equals_paper_core() {
    attribute_row_equals_paper_core(AttributeRow::Discretionary);
}

// ---- row: a symbol QUOTED in another currency than the ledger (decision 0243, phase 2) ----
//
// The ledger is the dollar and the symbol is quoted in USDT, converted by a USDT/USD rate path (the
// owner's amendment of decision 0243: a stablecoin quote converts like a fiat one when the run names its
// pair). The paper book fills in the symbol's own money by the one fill law, and its ENGINE books at the
// rate on the journaled admission (decision 0183: `ContractValue::in_account`, a fee stated in the quote
// currency times the rate); the simulator books at the same pair through `StrategyEngineConfig::conversions`.
// A USDT name is the gate's instrument because the paper BOOK refuses a name it can prove foreign (EURJPY):
// that fence is decision 0243's phase 3. `quote_conversion_law` works a EURJPY example on the simulator alone.

/// The ledger's currency and the symbol's quote currency.
const LEDGER: &str = "USD";
const QUOTE: &str = "USDT";
/// The pair's bar: 37 of the symbol's minutes, closing 30 s into one of them (a conversion bar's close
/// need not meet the series' own).
const PAIR_INTERVAL_MS: i64 = 37 * 60_000;

/// The USDT/USD pair, its close swinging far wider than the stablecoin moves, so a fill booked at a wrong
/// rate (the entry's, the next one's, par) cannot hide in a rounding. Each step is inside the engine's
/// jump cap (`vike_execution::MAX_QUOTE_RATE_STEP`). `QUOTE/LEDGER`: the rate is the close itself.
fn usdt_pair() -> vike_simulator::ConversionSeries {
    let closes = [1.0, 1.2, 0.9, 1.1, 0.8, 1.05, 0.95, 1.15, 0.85, 1.0, 1.25];
    let bars = closes.iter().enumerate().map(|(k, &close)| {
        let opened = k as i64 * PAIR_INTERVAL_MS + 30_000 - PAIR_INTERVAL_MS;
        vike_simulator::test_support::bar(opened, close, close, close, close, 1.0)
    });
    vike_simulator::ConversionSeries {
        quote_currency: QUOTE.into(),
        pair: "test:USDT-USD:37m".into(),
        leg: vike_model::AccountPerQuoteLeg::QuoteIsBase,
        interval_ms: PAIR_INTERVAL_MS,
        bars: std::sync::Arc::new(bars.collect()),
        symbols: vec![SYMBOL.into()],
        par_where_missing: false,
    }
}

/// The rate in force at `ts` (`ConversionSeries::rate_at`, the simulator's own rule).
fn rate_at(pair: &vike_simulator::ConversionSeries, ts: i64) -> vike_model::AccountPerQuote {
    pair.rate_at(ts).expect("a rate in force").rate
}

/// The paper book with its fills' fee stated in the symbol's QUOTE currency, as a venue that names its fee
/// currency states it (decision 0183, item 9). The book states none for a linear contract, which its
/// engine reads as the ledger's own; everything else is the book's, untouched.
struct PaperStatingFees(ExecutionClientPaper);

impl vike_execution::ExecutionClient for PaperStatingFees {
    fn submit(&mut self, request: &vike_model::OrderRequest) {
        self.0.submit(request);
    }
    fn cancel(&mut self, order_client_id: &str) {
        self.0.cancel(order_client_id);
    }
    fn on_bar(&mut self, bar: &Bar) {
        self.0.on_bar(bar);
    }
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        use vike_model::events::Event;
        let mut event = self.0.poll_events()?;
        match &mut event {
            Event::Fill(f) => f.commission_asset = QUOTE.into(),
            Event::OrderFilled(o) => o.fill.commission_asset = QUOTE.into(),
            Event::OrderPartiallyFilled(o) => o.fill.commission_asset = QUOTE.into(),
            _ => {}
        }
        Some(event)
    }
}

/// The mount's admission at `rate`, observed at `as_of_ms`, on the engine `route_key` names: what the node
/// sends to refresh a held rate (the engine judges a rate older than its two-minute window stale).
fn quote_admission(
    route_key: &str,
    rate: vike_model::AccountPerQuote,
    as_of_ms: i64,
) -> vike_execution::Command {
    vike_execution::Command::AdmitSymbol(Box::new(vike_execution::SymbolAdmission {
        route_key: route_key.into(),
        symbol: SYMBOL.into(),
        grid: vike_model::SymbolGrid::default(),
        contract_value: None,
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: Some(QUOTE.into()),
        quote_rate: Some(rate.get()),
        rate_as_of_ms: Some(as_of_ms),
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    }))
}

#[test]
fn a_symbol_quoted_in_another_currency_books_the_same_on_paper_and_in_the_engine() {
    let bars = vike_simulator::test_support::gen_bars(400, None);
    let pair = usdt_pair();

    // ---- backtest: the simulator converting by the pair ----
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        account_currency: Some(LEDGER.into()),
        conversions: vec![pair.clone()],
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    let mut engine =
        StrategyEngine::new(vec![(SYMBOL.to_string(), bars.clone())], GateSma::default(), params);
    let result = engine.run();
    let engine_fills = engine.strategy.fills.clone();
    let engine_realized: f64 = result.trades.iter().map(|t| t.pnl).fold(0.0, |a, b| a + b);
    let engine_fees: f64 = engine_fills.iter().map(|f| f.fee).fold(0.0, |a, b| a + b);
    let engine_position = engine.core.position_of(SYMBOL).size;
    let rates_used: std::collections::BTreeSet<u64> =
        engine_fills.iter().map(|f| rate_at(&pair, f.timestamp).get().to_bits()).collect();
    assert!(engine_fills.len() > 10, "the gate needs real trading: {}", engine_fills.len());
    assert!(rates_used.len() >= 5, "the fills must meet many rates: {}", rates_used.len());

    // ---- paper: the live core told its ledger, the mount declared quoted in USDT ----
    let client = ExecutionClientPaper::new(VENUE, SYMBOL, SLIPPAGE, MAKER, TAKER);
    let paper_fills: Arc<Mutex<Vec<Fill>>> = Arc::clone(&client.fills);
    let mut live = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        PaperStatingFees(client),
        VENUE,
        SYMBOL,
    );
    live.set_account_currency(LEDGER).expect("a currency code");
    live.declare_mount_quote_currency(QUOTE).expect("the mount is declared");
    let route_key = live.route_key.clone();
    // The core's clock is the time of the message it is about to fold (`on_dequeued` runs just before the
    // dispatch, on the core's own thread): a bar's open, an admission's stamp. A strategy's order is
    // judged at its bar's time too, so every rate is as fresh to the engine as the simulator's is to it.
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let (read, stamp) = (Arc::clone(&clock), Arc::clone(&clock));
    let handle = spawn_core(
        live,
        CoreConfig {
            seed_cash: 10_000.0,
            clock: Box::new(move || read.load(std::sync::atomic::Ordering::SeqCst)),
            on_dequeued: Some(Box::new(move |msg: &vike_execution::Ingest| {
                let at = match msg {
                    vike_execution::Ingest::BarClose(u) => u.bar.timestamp,
                    vike_execution::Ingest::Command(vike_execution::Command::AdmitSymbol(a)) => {
                        a.rate_as_of_ms.unwrap_or_default()
                    }
                    _ => return,
                };
                stamp.store(at, std::sync::atomic::Ordering::SeqCst);
            })),
            recent_events_cap: 4_096,
            strategy: Some(Mount {
                order_client_id_code: None,
                account: None,
                symbols: Vec::new(),
                controller_id: None,
                underlying_symbol: None,
                venue: VENUE.to_string(),
                symbol: SYMBOL.to_string(),
                interval: INTERVAL.to_string(),
                strategy: Box::new(GateSma::default()),
            }),
            ..CoreConfig::default()
        },
    );
    let cell = handle.snapshot_cell();
    // One ingest lane carries both, in order: before each bar, the rate in force at its time, stamped
    // with it (a new rate when a pair bar has closed, else the refresh of the standing one).
    let sender = handle.bar_sender();
    for bar in &bars {
        handle.send_command(quote_admission(
            &route_key,
            rate_at(&pair, bar.timestamp),
            bar.timestamp,
        ));
        sender
            .close(BarUpdate {
                venue: VENUE.to_string(),
                symbol: SYMBOL.to_string(),
                interval: INTERVAL.to_string(),
                bar: bar.clone(),
                sides: None,
            })
            .expect("core alive during replay");
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while paper_fills.lock().unwrap().len() < engine_fills.len() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.shutdown_and_join();
    let snap = cell.load_full();
    let paper: Vec<Fill> = paper_fills.lock().unwrap().clone();

    // ---- THE gate: the fill law's fills, bit for bit; the money at the rate of each fill ----
    assert_eq!(
        paper.len(),
        engine_fills.len(),
        "fill count; the core said {:#?}; its rates {:?}",
        snap.recent_events.iter().filter(|l| !l.starts_with("OrderDenied")).collect::<Vec<_>>(),
        snap.portfolio
            .venues
            .iter()
            .map(|v| (v.quote_rates_in.to_string(), format!("{:?}", v.quote_rates)))
            .collect::<Vec<_>>()
    );
    for (i, (p, e)) in paper.iter().zip(&engine_fills).enumerate() {
        assert_eq!(
            (p.timestamp, p.side, p.is_maker),
            (e.timestamp, e.side, e.is_maker),
            "fill[{i}]"
        );
        assert_eq!(p.size.to_bits(), e.size.to_bits(), "fill[{i}] qty");
        assert_eq!(p.price.to_bits(), e.price.to_bits(), "fill[{i}] px (the quote currency's)");
        // The book states its fee in USDT; the simulator books it in dollars at the fill's rate.
        let booked = rate_at(&pair, p.timestamp).to_account(p.fee);
        assert_eq!(booked.to_bits(), e.fee.to_bits(), "fill[{i}] fee: {} USDT", p.fee);
    }
    assert_eq!(
        snap.portfolio.realized_pnl.to_bits(),
        engine_realized.to_bits(),
        "realized: paper {} vs engine {}",
        snap.portfolio.realized_pnl,
        engine_realized
    );
    assert_eq!(
        snap.portfolio.fees_paid.to_bits(),
        engine_fees.to_bits(),
        "fees: paper {} vs engine {}",
        snap.portfolio.fees_paid,
        engine_fees
    );
    let paper_pos = snap.positions.iter().find(|p| p.symbol == SYMBOL).map_or(0.0, |p| p.size);
    assert_eq!(paper_pos.to_bits(), engine_position.to_bits(), "final position");
    assert!(snap.fault.is_none());
    println!(
        "r7 foreign-quote row green: {} fills over {} rates; realized {} {LEDGER}; fees {} {LEDGER}",
        paper.len(),
        rates_used.len(),
        engine_realized,
        engine_fees
    );
}

// ---- row: two mounts over one account (the portfolio run) ----

/// What one mount heard, shared out of the runtime that owns the strategy.
type Heard = Arc<Mutex<Vec<String>>>;

/// Tagged market orders on its own symbol at the bars `orders` name (`(bar, tag, side, qty)`), written
/// once for both stacks; it records each fill it hears bit for bit and each denial's tag, and the
/// denial's words apart (the simulator's tagged refusal says the gate refused it, live's names the cap:
/// the drop the simulator records names it).
struct Scheduled {
    orders: Vec<(i64, &'static str, i32, f64)>,
    heard: Heard,
    denials: Heard,
}

impl<B: OrderBroker> Strategy<B> for Scheduled {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        for &(at, tag, side, qty) in &self.orders {
            if bar.timestamp == 60_000 * (at + 1) {
                broker.place("", tag, side, qty, OrderPlacement::Market, false);
            }
        }
    }

    fn on_fill(&mut self, _broker: &mut B, f: &Fill) {
        let bits = (f.size.to_bits(), f.price.to_bits(), f.fee.to_bits());
        let line = format!("fill {:?} {} {} {bits:?} {}", f.tag, f.timestamp, f.side, f.is_maker);
        self.heard.lock().unwrap().push(line);
    }

    fn on_order_event(&mut self, _broker: &mut B, event: &vike_model::OrderEvent) {
        if let vike_model::OrderEventKind::Denied { reason } = &event.kind {
            self.heard.lock().unwrap().push(format!("denied {:?}", event.tag));
            self.denials.lock().unwrap().push(reason.clone());
        }
    }
}

/// The first mount (cap 1.5x the account's 10 000) buys 100 after its sibling (no cap) bought 100,
/// which passes on its own book; its next 80 would take its own book past 15 000 and is refused while
/// its sibling's 80 is not; its sell of 50 is a covered reduce, never judged. Once its sibling sold out,
/// its sell of 150 REVERSES its long 50 to short 100, netted on its row (10 000, passes; the old sum
/// 5 000 + 15 000 refused it); once its sibling bought 250 (the account long 150), its buy of 100 CLOSES
/// its own short, a covered reduce of its own row (the account's position read it as an add, refused).
fn two_mount_orders() -> [Vec<(i64, &'static str, i32, f64)>; 2] {
    let first = vec![
        (35, "a", 1, 100.0),
        (40, "b", 1, 80.0),
        (45, "c", -1, 50.0),
        (50, "e", -1, 150.0),
        (55, "g", 1, 100.0),
    ];
    let second =
        vec![(30, "a", 1, 100.0), (40, "b", 1, 80.0), (48, "d", -1, 180.0), (52, "f", 1, 250.0)];
    [first, second]
}

fn capped() -> vike_execution::MountBudget {
    vike_execution::MountBudget {
        max_loss: None,
        max_notional: None,
        loss_action: vike_model::MountLossAction::default(),
        max_leverage: Some(1.5),
    }
}

fn scheduled(orders: Vec<(i64, &'static str, i32, f64)>) -> (Scheduled, Heard, Heard) {
    let (heard, denials) = (Heard::default(), Heard::default());
    let strategy = Scheduled { orders, heard: Arc::clone(&heard), denials: Arc::clone(&denials) };
    (strategy, heard, denials)
}

#[test]
fn two_mounts_hear_the_same_fills_and_denial_as_two_mounts_of_the_paper_core() {
    let bars = vike_simulator::test_support::gen_bars(60, None);
    let [first_orders, second_orders] = two_mount_orders();

    // ---- backtest: one portfolio run ----
    let (first, sim_first, _) = scheduled(first_orders.clone());
    let (second, sim_second, _) = scheduled(second_orders.clone());
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    let mounts = vec![
        StrategyEngineMount {
            strategy: Box::new(first),
            symbols: vec![SYMBOL.to_string()],
            budget: Some(capped()),
        },
        StrategyEngineMount {
            strategy: Box::new(second),
            symbols: vec![SYMBOL.to_string()],
            budget: None,
        },
    ];
    let mut engine =
        StrategyEngine::new_mounted(vec![(SYMBOL.to_string(), bars.clone())], mounts, params);
    engine.run();
    let reports = engine.mount_reports();
    let dropped: Vec<String> = engine.core.dropped.iter().map(|d| d.reason.clone()).collect();
    let fills_of = |h: &Heard| h.lock().unwrap().iter().filter(|l| l.starts_with("fill")).count();
    let engine_fills = fills_of(&sim_first) + fills_of(&sim_second);
    assert_eq!(engine_fills, 8, "the row needs its eight fills (a reversal and a close)");

    // ---- paper: two mounts of the live core over one account ----
    let client = ExecutionClientPaper::new(VENUE, SYMBOL, SLIPPAGE, MAKER, TAKER);
    let paper_fills: Arc<Mutex<Vec<Fill>>> = Arc::clone(&client.fills);
    let engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        VENUE,
        SYMBOL,
    );
    let (first, paper_first, paper_denials) = scheduled(first_orders);
    let (second, paper_second, _) = scheduled(second_orders);
    let mount = |id: &str, strategy: Scheduled| Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: Some(id.to_string()),
        underlying_symbol: None,
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        interval: INTERVAL.to_string(),
        strategy: Box::new(strategy),
    };
    let handle = spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(mount("first", first)),
            extra_mounts: vec![mount("second", second)],
            mount_budgets: [("first".to_string(), capped())].into_iter().collect(),
            ..CoreConfig::default()
        },
    );
    let cell = handle.snapshot_cell();
    replay_bars(&handle.bar_sender(), VENUE, SYMBOL, INTERVAL, &bars);
    let deadline = Instant::now() + Duration::from_secs(30);
    while paper_fills.lock().unwrap().len() < engine_fills && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.shutdown_and_join();
    let snap = cell.load_full();

    // ---- THE row: each mount heard the same, and books the same ----
    let (sim, paper) = ([sim_first, sim_second], [paper_first, paper_second]);
    for (k, (s, p)) in sim.iter().zip(&paper).enumerate() {
        let (s, p) = (s.lock().unwrap().clone(), p.lock().unwrap().clone());
        assert_eq!(s, p, "mount {k}: what it heard, here and on the paper core");
    }
    assert!(sim[0].lock().unwrap().iter().any(|l| l == "denied Some(\"b\")"), "the cap refused");
    let refusal = vike_execution::risk::MOUNT_LEVERAGE_REFUSAL;
    assert!(dropped.len() == 1 && dropped[0].starts_with(refusal), "{dropped:?}");
    let live_denials = paper_denials.lock().unwrap().clone();
    assert!(live_denials.len() == 1 && live_denials[0].starts_with(refusal), "{live_denials:?}");
    for (report, id) in reports.iter().zip(["first", "second"]) {
        let view = snap.mounts.iter().find(|m| m.mount_id == id).expect("the mount's row");
        let row = &report.rows[0].1;
        assert_eq!(row.size.to_bits(), view.position.to_bits(), "{id}: position");
        assert_eq!(row.realized_net().to_bits(), view.realized_pnl.to_bits(), "{id}: realized");
    }
    assert!(snap.fault.is_none());
}

// ---- row: two mounts over TWO symbols, every kind of order the leverage cap judges ----

const SECOND: &str = "ETHUSDT";

/// One order a [`Kinds`] mount sends on its acting symbol's bar of a step.
#[derive(Clone, Copy)]
enum Kind {
    /// a tagged market order: `(symbol, tag, side, qty)`
    Market(&'static str, &'static str, i32, f64),
    /// a tagged REDUCE-ONLY market sell of `qty` on [`SYMBOL`]
    ReduceOnly(&'static str, f64),
    /// an order list on [`SYMBOL`]: a market buy of 1 (the first tag) arming a breakout buy of `qty` at
    /// `trigger` (the second): `(tags, qty, trigger)`
    Oto((&'static str, &'static str), f64, f64),
    /// an OCO pair on [`SYMBOL`], two orders of `side` · `qty` each as `orders` state them, reduce-only or
    /// not: `(tags, side, qty, orders, reduce_only)`
    Oco((&'static str, &'static str), i32, f64, (OrderPlacement, OrderPlacement), bool),
    /// a tagged resting limit buy on [`SYMBOL`]: `(tag, qty, price)`
    Rest(&'static str, f64, f64),
    /// an amend of a tag's quantity
    Amend(&'static str, f64),
}

/// Sends `orders` (`(step, kind)`) on `acts_on`'s bar of each step, written once for both stacks, and hears
/// as [`Scheduled`] does: each fill bit for bit, each denial's tag, the denial's words apart.
struct Kinds {
    acts_on: &'static str,
    orders: Vec<(i64, Kind)>,
    heard: Heard,
    denials: Heard,
}

impl<B: OrderBroker> Strategy<B> for Kinds {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        if bar.symbol.as_deref() != Some(self.acts_on) {
            return;
        }
        let leg = |side: i32, qty: f64, order: OrderPlacement, parent, reduce_only| {
            let (order_type, price, trigger_price, trail) = order.request_terms();
            OrderListLeg {
                side,
                qty,
                order_type,
                price,
                trigger_price,
                trail,
                reduce_only,
                time_in_force: TimeInForce::Gtc,
                parent,
                contingency: None,
                linked: Vec::new(),
                order_client_id: None,
            }
        };
        let oco = |leg: OrderListLeg, other: usize| OrderListLeg {
            contingency: Some(vike_model::ContingencyType::Oco),
            linked: vec![other],
            ..leg
        };
        for &(at, kind) in &self.orders {
            if bar.timestamp != 60_000 * (at + 1) {
                continue;
            }
            match kind {
                Kind::Market(symbol, tag, side, qty) => {
                    broker.place(symbol, tag, side, qty, OrderPlacement::Market, false);
                }
                Kind::ReduceOnly(tag, qty) => {
                    broker.place(SYMBOL, tag, -1, qty, OrderPlacement::Market, true);
                }
                Kind::Oto((e, c), qty, trigger) => {
                    let entry = leg(1, 1.0, OrderPlacement::Market, None, false);
                    let child = leg(1, qty, OrderPlacement::Stop { trigger }, Some(0), false);
                    broker.place_group(SYMBOL, vec![(e.into(), entry), (c.into(), child)]);
                }
                Kind::Oco((a, b), side, qty, (first, other), reduce_only) => {
                    let first = oco(leg(side, qty, first, None, reduce_only), 1);
                    let other = oco(leg(side, qty, other, None, reduce_only), 0);
                    broker.place_group(SYMBOL, vec![(a.into(), first), (b.into(), other)]);
                }
                Kind::Rest(tag, qty, price) => {
                    broker.place(SYMBOL, tag, 1, qty, OrderPlacement::Limit { price }, false);
                }
                Kind::Amend(tag, qty) => broker.amend(tag, Some(qty), None, None),
            }
        }
    }

    fn on_fill(&mut self, _broker: &mut B, f: &Fill) {
        let bits = (f.size.to_bits(), f.price.to_bits(), f.fee.to_bits());
        let line = format!("fill {:?} {} {} {bits:?} {}", f.tag, f.timestamp, f.side, f.is_maker);
        self.heard.lock().unwrap().push(line);
    }

    fn on_order_event(&mut self, _broker: &mut B, event: &vike_model::OrderEvent) {
        if let vike_model::OrderEventKind::Denied { reason } = &event.kind {
            self.heard.lock().unwrap().push(format!("denied {:?}", event.tag));
            self.denials.lock().unwrap().push(reason.clone());
        }
    }
}

/// `first` (cap 1.5 of the account's 10 000: 15 000; every price near 100) acts on the step's LAST bar,
/// `SECOND`'s, so an order on either symbol fills at the next step's open on both stacks; `second` (no cap)
/// acts on [`SYMBOL`]'s bar. Once `second` holds 200: `first`'s REDUCE-ONLY sell of 200 would book 200 short
/// on its own flat row (refused); its OTO list reaches 1 001 with its child (refused whole, each leg hearing
/// it); a second OTO list whose child of 100 is priced ABOVE its first leg, at 160, reaches 101 at 160 (16 160,
/// refused: valued at the first leg's price it was 10 100); it buys 70; an OCO EXIT pair of 70 over that long
/// is a covered reduce, its legs alternatives (it rests); a buy of 120 on the SECOND symbol takes its book past
/// the cap with the first symbol's 70 (refused), one of 30 does not; it rests a limit buy of 10 at 50 and
/// AMENDS it up to 200, a growth of 190 valued at the worse of 50 and the mark (refused, nothing heard). Last,
/// ONE callback sends an OCO entry pair of limit buys of 30 (an order list, which live's drain judges AFTER
/// the submissions) and THEN a market buy of 30 on the second symbol: each fits alone (about 13 000, a
/// resting buy below the market valued at the mark), the two together would not (about 16 000), and both go
/// out on both stacks, whichever each engine judges first.
fn two_symbol_orders() -> [Vec<(i64, Kind)>; 2] {
    let exits = (OrderPlacement::Limit { price: 200.0 }, OrderPlacement::Stop { trigger: 50.0 });
    let entries = (OrderPlacement::Limit { price: 60.0 }, OrderPlacement::Limit { price: 55.0 });
    let first = vec![
        (12, Kind::ReduceOnly("r", 200.0)),
        (15, Kind::Oto(("e", "c"), 1_000.0, 150.0)),
        (16, Kind::Oto(("p", "k"), 100.0, 160.0)),
        (20, Kind::Market(SYMBOL, "a", 1, 70.0)),
        (22, Kind::Oco(("x", "y"), -1, 70.0, exits, true)),
        (25, Kind::Market(SECOND, "b", 1, 120.0)),
        (26, Kind::Market(SECOND, "d", 1, 30.0)),
        (30, Kind::Rest("q", 10.0, 50.0)),
        (35, Kind::Amend("q", 200.0)),
        (40, Kind::Oco(("l", "m"), 1, 30.0, entries, false)),
        (40, Kind::Market(SECOND, "n", 1, 30.0)),
    ];
    let second = vec![(10, Kind::Market(SYMBOL, "s", 1, 200.0))];
    [first, second]
}

fn kinds(acts_on: &'static str, orders: Vec<(i64, Kind)>) -> (Kinds, Heard, Heard) {
    let (heard, denials) = (Heard::default(), Heard::default());
    let strategy =
        Kinds { acts_on, orders, heard: Arc::clone(&heard), denials: Arc::clone(&denials) };
    (strategy, heard, denials)
}

/// **Two mounts over two symbols agree on every kind of order the leverage cap judges** (rule v3): a
/// reduce-only order on its stated size, an order list's OTO child (one priced above its first leg), an OCO
/// exit over the mount's long, an order on a second symbol, an amend up, and two orders of one callback in
/// two verbs that each engine judges in its own order, here and on two mounts of the paper core (two paper
/// books behind one account). Each mount hears the same fills bit for bit and the same denials, the
/// simulator's refusals are the live ones and live's amend refusal is its ring line. Three cases are not in
/// the row, each engine's own suite pinning them: a TIMED order (the paper core's drain stamps `now` from
/// the wall clock, so a release at a bar's time, 1970, is "not in the future" there), an amend topping up a
/// PARTLY filled order (the paper book fills an order whole) and a market order on a NEVER-QUOTED leg (every
/// simulator slot has its bars, so its price).
#[test]
fn two_mounts_over_two_symbols_agree_on_every_kind_the_cap_judges() {
    let bars = |symbol| vike_simulator::test_support::gen_bars(60, Some(symbol));
    let (first_bars, second_bars) = (bars(SYMBOL), bars(SECOND));
    let [first_orders, second_orders] = two_symbol_orders();

    // ---- backtest: one portfolio run over both symbols ----
    let (first, sim_first, _) = kinds(SECOND, first_orders.clone());
    let (second, sim_second, _) = kinds(SYMBOL, second_orders.clone());
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    };
    let mounts = vec![
        StrategyEngineMount {
            strategy: Box::new(first),
            symbols: vec![SYMBOL.to_string(), SECOND.to_string()],
            budget: Some(capped()),
        },
        StrategyEngineMount {
            strategy: Box::new(second),
            symbols: vec![SYMBOL.to_string()],
            budget: None,
        },
    ];
    let series =
        vec![(SYMBOL.to_string(), first_bars.clone()), (SECOND.to_string(), second_bars.clone())];
    let mut engine = StrategyEngine::new_mounted(series, mounts, params);
    engine.run();
    let reports = engine.mount_reports();
    let refusal = vike_execution::risk::MOUNT_LEVERAGE_REFUSAL;
    let dropped: Vec<String> = engine.core.dropped.iter().map(|d| d.reason.clone()).collect();
    assert_eq!(dropped.iter().filter(|d| d.starts_with(refusal)).count(), 5, "{dropped:?}");
    let fills_of = |h: &Heard| h.lock().unwrap().iter().filter(|l| l.starts_with("fill")).count();
    let engine_fills = fills_of(&sim_first) + fills_of(&sim_second);
    assert_eq!(engine_fills, 4, "the row needs its four fills");
    let resting = engine.core.pending_of(SYMBOL).len();
    assert_eq!(resting, 5, "the OCO exit pair, the limit `q` and the OCO entry pair rest");

    // ---- paper: two mounts of the live core over one account, one paper book a symbol ----
    let books =
        [SYMBOL, SECOND].map(|s| ExecutionClientPaper::new(VENUE, s, SLIPPAGE, MAKER, TAKER));
    let paper_fills: Vec<Arc<Mutex<Vec<Fill>>>> =
        books.iter().map(|b| Arc::clone(&b.fills)).collect();
    let mut client = vike_paper::ExecutionClientPaperMulti::new();
    for book in books {
        client.add_book(book);
    }
    let mut engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        VENUE,
        SYMBOL,
    );
    engine.extra_symbols = vec![SECOND.into()];
    let (first, paper_first, paper_denials) = kinds(SECOND, first_orders);
    let (second, paper_second, _) = kinds(SYMBOL, second_orders);
    let mount = |id: &str, symbols: Vec<MountLeg>, strategy: Kinds| Mount {
        order_client_id_code: None,
        account: None,
        symbols,
        controller_id: Some(id.to_string()),
        underlying_symbol: None,
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        interval: INTERVAL.to_string(),
        strategy: Box::new(strategy),
    };
    let handle = spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(mount("first", vec![MountLeg::same_venue(SECOND)], first)),
            extra_mounts: vec![mount("second", Vec::new(), second)],
            mount_budgets: [("first".to_string(), capped())].into_iter().collect(),
            ..CoreConfig::default()
        },
    );
    let cell = handle.snapshot_cell();
    for (a, b) in first_bars.iter().zip(&second_bars) {
        for (symbol, bar) in [(SYMBOL, a), (SECOND, b)] {
            let update = BarUpdate {
                venue: VENUE.into(),
                symbol: symbol.into(),
                interval: INTERVAL.into(),
                // live bars arrive symbol-less; the runtime stamps the series symbol
                bar: Bar { symbol: None, ..bar.clone() },
                sides: None,
            };
            handle.bar_sender().close(update).expect("core alive during replay");
        }
    }
    let filled = || paper_fills.iter().map(|f| f.lock().unwrap().len()).sum::<usize>();
    let deadline = Instant::now() + Duration::from_secs(30);
    while filled() < engine_fills && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.shutdown_and_join();
    let snap = cell.load_full();

    // ---- THE row: each mount heard the same, refused the same, and books the same ----
    let (sim, paper) = ([sim_first, sim_second], [paper_first, paper_second]);
    for (k, (s, p)) in sim.iter().zip(&paper).enumerate() {
        let (s, p) = (s.lock().unwrap().clone(), p.lock().unwrap().clone());
        assert_eq!(s, p, "mount {k}: what it heard, here and on the paper core");
    }
    let denied: Vec<String> =
        ["r", "e", "c", "p", "k", "b"].iter().map(|t| format!("denied Some({t:?})")).collect();
    let heard_denials: Vec<String> =
        sim[0].lock().unwrap().iter().filter(|l| l.starts_with("denied")).cloned().collect();
    assert_eq!(heard_denials, denied, "the cap's six refusals the strategy hears");
    let live_denials = paper_denials.lock().unwrap().clone();
    assert!(live_denials.len() == 6 && live_denials.iter().all(|d| d.starts_with(refusal)));
    let amend = "REFUSED, its terms kept: over-mount-leverage";
    assert!(snap.recent_events.iter().any(|l| l.contains(amend)), "{:?}", snap.recent_events);
    for (report, id) in reports.iter().zip(["first", "second"]) {
        let view = snap.mounts.iter().find(|m| m.mount_id == id).expect("the mount's row");
        let row = &report.rows[0].1;
        assert_eq!(row.size.to_bits(), view.position.to_bits(), "{id}: position");
    }
    assert!(snap.fault.is_none());
}
