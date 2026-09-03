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
//! - `stop verb`: `SimBroker::submit_stop` under `EngineParams::emulator_release_stops = true`
//!   == `LiveBroker::submit_stop` (always emulator-armed live: the fired conditional releases
//!   as a MARKET and fills the next bar's open). The DEFAULT (`false`) engine semantics —
//!   same-bar fill at the trigger — deliberately diverge from live; that divergence is pinned
//!   in `tests/laws/trigger_law.rs`, not here.
//! - `bracket`: `SimBroker` entry-with-protective-stop + resting TP limit ==
//!   `LiveBroker::submit_bracket`'s OTO/OCO triple through the paper book (both stops fill via
//!   the ONE trigger oracle; both sides OCO-cancel the surviving exit).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_backtest::paper::{replay_bars, PaperExecutionClient, PaperFill};
use vike_backtest::{EngineParams, SimBroker, StrategyEngine};
use vike_core::{spawn_core, CoreConfig, LiveBroker, StrategyMount};
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::{Bar, Broker, Fill, Strategy};

const VENUE: &str = "paper";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";
const QTY: f64 = 1.0;
const FAST: usize = 5;
const SLOW: usize = 20;
const SLIPPAGE: f64 = 0.0002;
const MAKER: f64 = 0.0002;
const TAKER: f64 = 0.0007;

/// Deterministic bar walk (LCG — no Date/rand).
fn gen_bars(n: usize) -> Vec<Bar> {
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut px = 100.0f64;
    let mut bars = Vec::with_capacity(n);
    for i in 0..n {
        let mut r = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 32) as f64 / (1u64 << 32) as f64 * 2.0 - 1.0 // [-1, 1)
        };
        let drift = r() * 0.8;
        let open = px;
        let close = (px + drift).max(1.0);
        let high = open.max(close) + r().abs() * 0.3;
        let low = (open.min(close) - r().abs() * 0.3).max(0.5);
        bars.push(Bar {
            ts: 60_000 * (i as i64 + 1),
            open,
            high,
            low,
            close,
            volume: 10.0 + r().abs() * 5.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        px = close;
    }
    bars
}

/// THE shared signal: SMA(FAST) vs SMA(SLOW) over closes → target sign (+1/-1/0=hold).
fn sma_signal(closes: &[f64]) -> i32 {
    if closes.len() < SLOW {
        return 0;
    }
    let mean = |w: &[f64]| w.iter().copied().fold(0.0, |a, b| a + b) / w.len() as f64;
    let fast = mean(&closes[closes.len() - FAST..]);
    let slow = mean(&closes[closes.len() - SLOW..]);
    if fast > slow {
        1
    } else {
        -1
    }
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

// --- THE strategy: written ONCE against the unified Broker seam, runs on BOTH stacks ---
// (backtest: Broker = the sim engine core, direct mutation; paper/live: Broker = LiveCtx,
// deferred submissions through mint → RiskGate → client). This single impl replacing the
// former GateSmaBacktest + GateSmaLive pair IS the write-once property the gate proves.

#[derive(Default)]
struct GateSma {
    fills: Vec<Fill>,
}

impl<B: Broker> Strategy<B> for GateSma {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        // backtest bars are symbol-tagged; the live broker is single-scoped and ignores it
        let sym = bar.symbol.clone().unwrap_or_default();
        let closes: Vec<f64> = broker.bars(&sym).iter().map(|b| b.close).collect();
        if let Some((side, qty)) = decide(sma_signal(&closes), broker.position(&sym)) {
            broker.submit_market(&sym, side, qty);
        }
    }

    /// fires in backtest only (the live side journals fills through the engine instead)
    fn on_fill(&mut self, _broker: &mut B, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[test]
fn backtest_equals_paper_core() {
    let bars = gen_bars(400);

    // ---- backtest (the R2 engine) ----
    let params = EngineParams {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..EngineParams::default()
    };
    let mut engine =
        StrategyEngine::new(vec![(SYMBOL.to_string(), bars.clone())], GateSma::default(), params);
    let result = engine.run();
    let engine_fills = engine.strategy.fills.clone();
    let engine_position = engine.core.position_of(SYMBOL).size;
    let engine_realized: f64 = result.trades.iter().map(|t| t.pnl).fold(0.0, |a, b| a + b);
    let engine_fees: f64 = engine_fills.iter().map(|f| f.fee).fold(0.0, |a, b| a + b);
    assert!(engine_fills.len() > 10, "the gate needs real trading: {} fills", engine_fills.len());

    // ---- paper (the live core + PaperExecutionClient + BacktestDataClient replay) ----
    let client = PaperExecutionClient::new(VENUE, SYMBOL, SLIPPAGE, MAKER, TAKER);
    let paper_fills: Arc<Mutex<Vec<PaperFill>>> = Arc::clone(&client.fills);
    let engine = ExecutionEngine::new(
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
            strategy: Some(StrategyMount {
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
    let paper: Vec<PaperFill> = paper_fills.lock().unwrap().clone();

    // ---- THE gate: bit-identical fills ----
    assert_eq!(paper.len(), engine_fills.len(), "fill count");
    for (i, (p, e)) in paper.iter().zip(&engine_fills).enumerate() {
        assert_eq!(p.ts, e.ts, "fill[{i}] ts");
        assert_eq!(p.side, e.side, "fill[{i}] side");
        assert_eq!(p.qty.to_bits(), e.size.to_bits(), "fill[{i}] qty");
        assert_eq!(p.px.to_bits(), e.price.to_bits(), "fill[{i}] px");
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
// The stop/bracket verbs have no portable `Broker` surface (live arms the emulator / lowers a
// bracket triple), so each row pairs two thin strategy impls sharing the same decision
// constants — the pre-unification gate shape, kept deliberately small.

/// Run the paper stack (core + PaperExecutionClient) over `bars` with `strategy`, waiting until
/// at least `expect_fills` paper fills landed (or timeout), and return the fill stream.
fn run_paper_stack(
    bars: &[Bar],
    strategy: Box<dyn Strategy<LiveBroker> + Send>,
    expect_fills: usize,
) -> Vec<PaperFill> {
    let client = PaperExecutionClient::new(VENUE, SYMBOL, SLIPPAGE, MAKER, TAKER);
    let paper_fills: Arc<Mutex<Vec<PaperFill>>> = Arc::clone(&client.fills);
    let engine = ExecutionEngine::new(
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
            strategy: Some(StrategyMount {
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
    let fills = paper_fills.lock().unwrap().clone();
    fills
}

/// Assert the two fill streams are bit-identical (the gate's comparison law).
fn assert_fills_identical(paper: &[PaperFill], engine_fills: &[Fill]) {
    assert_eq!(paper.len(), engine_fills.len(), "fill count");
    for (i, (p, e)) in paper.iter().zip(engine_fills).enumerate() {
        assert_eq!(p.ts, e.ts, "fill[{i}] ts");
        assert_eq!(p.side, e.side, "fill[{i}] side");
        assert_eq!(p.qty.to_bits(), e.size.to_bits(), "fill[{i}] qty");
        assert_eq!(p.px.to_bits(), e.price.to_bits(), "fill[{i}] px");
        assert_eq!(p.fee.to_bits(), e.fee.to_bits(), "fill[{i}] fee");
        assert_eq!(p.is_maker, e.is_maker, "fill[{i}] maker");
    }
}

// ---- row: the stop VERB (emulator_release_stops mirrors the live ConditionalBook) ----

const STOP_CYCLE: usize = 40;

/// shared decision constants: enter long on cycle bars when flat; once long, arm ONE
/// protective sell stop 0.5% under the arming bar's close (tight enough that the LCG tape
/// stops out every cycle — the row needs the RELEASE path to actually run, repeatedly).
fn stop_row_entry_due(index: usize) -> bool {
    index % STOP_CYCLE == 5
}
fn stop_row_stop_px(close: f64) -> f64 {
    close * 0.995
}

#[derive(Default)]
struct StopVerbSim {
    armed: bool,
    fills: Vec<Fill>,
}

impl Strategy<SimBroker> for StopVerbSim {
    fn on_bar(&mut self, ctx: &mut SimBroker, bar: &Bar) {
        let sym = bar.symbol.clone().unwrap_or_default();
        if ctx.position_of(&sym).size == 0.0 {
            self.armed = false;
            if stop_row_entry_due(ctx.index) {
                ctx.submit(&sym, 1, QTY, 0.0, false, None);
            }
        } else if !self.armed {
            self.armed = true;
            ctx.submit_stop(&sym, -1, QTY, stop_row_stop_px(bar.close), 0.0, false);
        }
    }
    fn on_fill(&mut self, _ctx: &mut SimBroker, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[derive(Default)]
struct StopVerbLive {
    armed: bool,
}

impl Strategy<LiveBroker> for StopVerbLive {
    fn on_bar(&mut self, ctx: &mut LiveBroker, bar: &Bar) {
        if Broker::position(ctx, SYMBOL) == 0.0 {
            self.armed = false;
            if stop_row_entry_due(ctx.index) {
                ctx.submit_market(SYMBOL, 1, QTY);
            }
        } else if !self.armed {
            self.armed = true;
            // live: ALWAYS emulator-armed — fires as a market through the one live path
            ctx.submit_stop(-1, QTY, stop_row_stop_px(bar.close));
        }
    }
}

#[test]
fn stop_verb_release_mode_equals_paper_core() {
    let bars = gen_bars(400);

    // engine, with the opt-in emulator mirror ON (the live-law mode)
    let params = EngineParams {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        emulator_release_stops: true,
        ..EngineParams::default()
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
fn bracket_row_stop_px(close: f64) -> f64 {
    close * 0.98
}
fn bracket_row_tp_px(close: f64) -> f64 {
    close * 1.02
}

#[derive(Default)]
struct BracketSim {
    fills: Vec<Fill>,
}

impl Strategy<SimBroker> for BracketSim {
    fn on_bar(&mut self, ctx: &mut SimBroker, bar: &Bar) {
        let sym = bar.symbol.clone().unwrap_or_default();
        if ctx.position_of(&sym).size == 0.0
            && ctx.pending_of(&sym).is_empty()
            && bracket_row_entry_due(ctx.index)
        {
            // entry + protective stop (the implicit kind-linked bracket) + resting TP limit
            ctx.submit(&sym, 1, QTY, 0.0, false, Some(bracket_row_stop_px(bar.close)));
            ctx.submit_limit(&sym, -1, QTY, bracket_row_tp_px(bar.close), 0.0, false, None);
        }
    }
    fn on_fill(&mut self, _ctx: &mut SimBroker, fill: &Fill) {
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
        if Broker::position(ctx, SYMBOL) == 0.0 && bracket_row_entry_due(ctx.index) {
            ctx.submit_bracket(
                1,
                QTY,
                None,
                bracket_row_stop_px(bar.close),
                bracket_row_tp_px(bar.close),
            );
        }
    }
}

#[test]
fn bracket_row_equals_paper_core() {
    let bars = gen_bars(400);

    let params = EngineParams {
        cash: 10_000.0,
        slippage: SLIPPAGE,
        maker_fee: Some(MAKER),
        taker_fee: Some(TAKER),
        ..EngineParams::default()
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
