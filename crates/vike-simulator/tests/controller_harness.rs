//! position-executor STAGE 5 — the backtest-parity gate for the `ControllerHarness`.
//!
//! Design: `docs/superpowers/specs/2026-07-11-position-executor.md` (§3.4, PR5). The harness
//! (`vike_model::ControllerHarness`) IS a `Strategy<B>`, so it mounts UNCHANGED on both engines. These
//! tests prove that:
//!
//! 1. `harness_full_lifecycle_exact_outcome` — a full controller → executor → barrier-exit lifecycle
//!    runs through the backtest `StrategyEngine` (`run`, the closed-bar path) and the entry / exit /
//!    pnl come out EXACTLY as computed (zero fee/slippage), and the harness's own recorded
//!    `PositionExecutorOutcome` agrees with the engine's `Trade` — the executor's internal accounting is
//!    tied to the engine's ledger.
//! 2. `harness_tick_path_exact_outcome` — the same, driven off L1 QUOTE ticks (`run_ticks`, the
//!    `on_quote_tick → PositionExecutor::on_tick` barrier lane), reusing the `maker_backtest` pattern.
//! 3. `harness_backtest_equals_paper_core` — THE parity payoff: the SAME harness + reference
//!    controller produce BIT-IDENTICAL fills whether driven through the backtest engine or the live
//!    single-writer core in paper mode (mirrors the r7 gate). This is where "runs identically live and
//!    backtest" is proven for the executor framework, not just claimed.
//!
//! Everything is additive: the harness is composed from the existing `Strategy`/`OrderBroker` handlers +
//! the stage-1..4 `PositionExecutor`; no serde `Event`/wire change, no new `Command` verb.
//!
//! The bar and quote builders are the crate's shared ones, `vike_simulator::test_support::{bar, quote}`
//! (compiled once in the lib, behind `test-support`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, Mount, spawn_core};
use vike_execution::{Account, BalanceMode, Engine, RiskGate};
use vike_marketdata::Bar;
use vike_model::{ContractValue, Fill, RiskLimits, TripleBarrier, TripleBarrierKind};
use vike_paper::{ExecutionClientPaper, replay_bars};
use vike_simulator::{StrategyEngine, StrategyEngineConfig, TickEvent};
use vike_strategy::ControllerHarness;
use vike_strategy_lab::takers::momentum::MomentumController;

// ============================================================================================
// 1. Full lifecycle through the backtest engine (closed-bar path) — exact outcome.
// ============================================================================================

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    vike_simulator::test_support::bar(ts, o, h, l, c, 0.0)
}

/// An engine over ONE controller-driven long round-trip on `symbol`: a momentum controller (threshold 0
/// ⇒ opens long on the first up/flat move after it has a reference) with take-profit +5 (no stop, no
/// time), and a large cooldown ⇒ exactly ONE round-trip in this short series.
fn take_profit_round_trip(
    symbol: &str,
    params: StrategyEngineConfig,
) -> StrategyEngine<ControllerHarness<MomentumController>> {
    let controller =
        MomentumController::new(1.0, 0.0, TripleBarrier::new(Some(5.0), None, None, None));
    let harness = ControllerHarness::new(controller, "V", 10_000_000);

    // ts in ms (1-minute bars). The controller declines bar0 (no reference), opens long on bar1's
    // +1 close move; the entry market fills at bar2.open=100 (entry 100 → tp target 105); bar3 rallies
    // through 105 → tp fires → the close market fills at bar4.open=104 → realized (104−100)·1 = +4.
    let bars = vec![
        bar(60_000, 100.0, 100.0, 100.0, 100.0), // 0: prime the momentum reference (100)
        bar(120_000, 100.0, 100.0, 100.0, 101.0), // 1: +1 close move → open long (entry submitted)
        bar(180_000, 100.0, 100.0, 100.0, 100.0), // 2: entry fills @100 → Open; no barrier
        bar(240_000, 100.0, 106.0, 100.0, 105.0), // 3: high 106 ≥ tp 105 → TP fires (close submitted)
        bar(300_000, 104.0, 104.0, 104.0, 104.0), // 4: close fills @104 → Closed; cooldown armed
        bar(360_000, 104.0, 104.0, 104.0, 104.0), // 5: cooldown blocks re-open
        bar(420_000, 104.0, 104.0, 104.0, 104.0), // 6: cooldown blocks re-open
    ];
    StrategyEngine::new(vec![(symbol.to_string(), bars)], harness, params)
}

/// A single controller-driven long round-trip, driven bar-by-bar through the sim engine, with a
/// take-profit barrier. Every price is exact (zero fee/slippage), so the engine trade AND the
/// harness's own `PositionExecutorOutcome` are asserted to the cent.
#[test]
fn harness_full_lifecycle_exact_outcome() {
    const SYM: &str = "TOK";
    // A momentum controller: threshold 0 ⇒ opens long on the first up/flat move after it has a
    // reference; take-profit +5 (no stop / no time), so the position exits when price rallies +5.
    let mut engine = take_profit_round_trip(SYM, StrategyEngineConfig::default());
    let result = engine.run();

    // exactly ONE completed round-trip, exact prices/pnl
    assert_eq!(result.trades.len(), 1, "one controller-driven round-trip");
    let t = &result.trades[0];
    assert_eq!(t.entry_price, 100.0);
    assert_eq!(t.exit_price, 104.0);
    assert_eq!(t.size, 1.0);
    assert_eq!(t.pnl, 4.0, "(104−100)·1 long");
    assert!(t.is_long);
    assert_eq!(t.entry_timestamp, 180_000);
    assert_eq!(t.exit_timestamp, 300_000);

    // the harness's OWN executor outcome agrees with the engine's ledger
    let outcomes = engine.strategy.outcomes();
    assert_eq!(outcomes.len(), 1, "one executor outcome recorded");
    let o = outcomes[0];
    assert_eq!(o.barrier_hit, Some(TripleBarrierKind::TakeProfit));
    assert_eq!(o.entry_price, 100.0);
    assert_eq!(o.exit_price, 104.0);
    assert_eq!(o.entry_timestamp, 180_000);
    assert_eq!(o.exit_timestamp, 300_000);
    assert_eq!(o.realized_pnl, 4.0);

    // flat + no live executor at the end
    assert_eq!(engine.core.position_of(SYM).size, 0.0, "flat after the round-trip");
    assert_eq!(engine.strategy.active_count(), 0, "no live executor remains");
}

/// The executor values its PnL at the RUN's contract value (`OrderBroker::contract_value`), so on a
/// 50-point future its outcome is the engine's own trade, 50 × Δp × q, and not Δp × q (what it
/// reported while the harness built every executor at a multiplier of 1).
#[test]
fn harness_outcome_is_valued_at_the_contract_multiplier() {
    const SYM: &str = "TOK";
    let mut engine = take_profit_round_trip(
        SYM,
        StrategyEngineConfig { multiplier: 50.0, ..StrategyEngineConfig::default() },
    );
    let result = engine.run();

    assert_eq!(result.trades.len(), 1, "one controller-driven round-trip");
    assert_eq!(result.trades[0].pnl, 200.0, "the engine books 50·(104−100)·1");
    let outcomes = engine.strategy.outcomes();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].realized_pnl, 200.0, "the executor must value it at the same 50");
}

/// On an INVERSE contract the executor's PnL is the settle coin the engine's coin ledger books, bit
/// for bit: $4 on one $100 contract, at the exit price 104.
#[test]
fn harness_outcome_on_an_inverse_contract_is_the_engines_coin_pnl() {
    const SYM: &str = "TOK";
    let params = StrategyEngineConfig {
        contract_values: vec![(SYM.to_string(), ContractValue::inverse(100.0))],
        ..StrategyEngineConfig::default()
    };
    let mut engine = take_profit_round_trip(SYM, params);
    let result = engine.run();

    assert_eq!(result.trades.len(), 1, "one controller-driven round-trip");
    let t = &result.trades[0];
    assert_eq!(t.pnl, 4.0 / 104.0, "the coin ledger books $4 at the exit price");
    let outcomes = engine.strategy.outcomes();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].realized_pnl.to_bits(), t.pnl.to_bits(), "the executor books the coin");
}

// ============================================================================================
// 2. Full lifecycle through the TICK path (`run_ticks`) — exact outcome.
// ============================================================================================

/// One L1 quote at `ts` (ms) for `SYM`; the controller/executor drive off the mid = (bid+ask)/2.
fn q(ts: i64, bid: f64, ask: f64, symbol: &str) -> TickEvent {
    vike_simulator::test_support::quote(symbol, ts, bid, ask, 1.0, 1.0)
}

/// The controller/executor lifecycle over the QUOTE tick lane (`on_quote_tick` →
/// `PositionExecutor::on_tick`), mirroring `maker_backtest`'s `run_ticks` harnessing. Exact prices
/// (market fills at the next quote's mid).
#[test]
fn harness_tick_path_exact_outcome() {
    const SYM: &str = "TOK";
    // take-profit +2; large cooldown ⇒ one round-trip.
    let controller =
        MomentumController::new(1.0, 0.0, TripleBarrier::new(Some(2.0), None, None, None));
    let harness = ControllerHarness::new(controller, "V", 10_000_000);

    // mids: 100 (prime) → 101 (open long) → 101 (entry fills @101 → Open, tp target 103) →
    // 103 (tp fires) → 104 (close fills @104). Realized (104−101)·1 = +3.
    let ticks = vec![
        q(1, 99.5, 100.5, SYM),  // mid 100 — prime the reference
        q(2, 100.5, 101.5, SYM), // mid 101 — +1 move → open long (entry submitted)
        q(3, 100.5, 101.5, SYM), // mid 101 — entry fills @101 → Open; 101 < tp 103
        q(4, 102.5, 103.5, SYM), // mid 103 — tp fires (close submitted)
        q(5, 103.5, 104.5, SYM), // mid 104 — close fills @104 → Closed
    ];

    let mut engine = StrategyEngine::new(
        vec![(SYM.to_string(), Vec::new())],
        harness,
        StrategyEngineConfig::default(),
    );
    let result = engine.run_ticks(&[(SYM.to_string(), ticks)]);

    assert_eq!(result.trades.len(), 1, "one round-trip on the tick lane");
    let t = &result.trades[0];
    assert_eq!(t.entry_price, 101.0);
    assert_eq!(t.exit_price, 104.0);
    assert_eq!(t.pnl, 3.0);
    assert!(t.is_long);

    let outcomes = engine.strategy.outcomes();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].barrier_hit, Some(TripleBarrierKind::TakeProfit));
    assert_eq!(outcomes[0].entry_price, 101.0);
    assert_eq!(outcomes[0].exit_price, 104.0);
    assert_eq!(outcomes[0].realized_pnl, 3.0);

    assert_eq!(engine.core.position_of(SYM).size, 0.0, "flat after the tick-lane round-trip");
    assert_eq!(engine.strategy.active_count(), 0);
}

// ============================================================================================
// 3. THE parity payoff: backtest engine == live paper core, BIT-IDENTICAL fills.
// ============================================================================================

const VENUE: &str = "paper";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";

/// A momentum harness that trades a lot: threshold 0 (opens every eligible bar), a tight triple
/// barrier (tp/sl/time) so positions close quickly, and NO cooldown (re-opens immediately) — many
/// round-trips over the walk. Constructed fresh for each runtime so both start from identical state.
fn make_harness() -> ControllerHarness<MomentumController> {
    let controller = MomentumController::new(
        1.0,
        0.0,
        TripleBarrier::new(Some(1.5), Some(1.5), Some(180_000), None),
    );
    ControllerHarness::new(controller, VENUE, 0)
}

/// THE gate: the SAME `ControllerHarness` + reference controller, mounted on the R2 backtest engine
/// and on the live paper core, produce BIT-IDENTICAL fills. The strategy logic is the ONE harness
/// impl; the gate proves the two RUNTIMES agree when driving it (the "same Strategy live+backtest"
/// guarantee, extended to the position-executor framework).
#[test]
fn harness_backtest_equals_paper_core() {
    // Pre-tagged with the series symbol so BOTH runtimes key the harness on the same instrument (the
    // backtest engine re-stamps it anyway; the live replay preserves it: a real feed's bars are
    // symbol-tagged).
    let bars = vike_simulator::test_support::gen_bars(400, Some(SYMBOL));

    // ---- backtest (the R2 engine), fills captured via the ledger mirror ----
    let params =
        StrategyEngineConfig { cash: 100_000.0, mirror: true, ..StrategyEngineConfig::default() };
    let mut engine =
        StrategyEngine::new(vec![(SYMBOL.to_string(), bars.clone())], make_harness(), params);
    let result = engine.run();
    let engine_fills: Vec<Fill> = engine.core.mirror_fills.clone().expect("mirror enabled");
    let engine_position = engine.core.position_of(SYMBOL).size;
    assert!(engine_fills.len() > 10, "the gate needs real trading: {} fills", engine_fills.len());
    // the harness's own executor accounting ties to the engine ledger (zero fee/slippage ⇒ exact)
    let engine_realized: f64 = result.trades.iter().map(|t| t.pnl).fold(0.0, |a, b| a + b);
    let outcome_realized: f64 =
        engine.strategy.outcomes().iter().map(|o| o.realized_pnl).fold(0.0, |a, b| a + b);

    // ---- paper (the live core + ExecutionClientPaper + BacktestDataClient replay) ----
    let client = ExecutionClientPaper::new(VENUE, SYMBOL, 0.0, 0.0, 0.0);
    let paper_fills: Arc<Mutex<Vec<Fill>>> = Arc::clone(&client.fills);
    let exec = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        VENUE,
        SYMBOL,
    );
    let handle = spawn_core(
        exec,
        CoreConfig {
            seed_cash: 100_000.0,
            strategy: Some(Mount {
                order_client_id_code: None,
                account: None,
                symbols: Vec::new(),
                controller_id: None,
                underlying_symbol: None,
                venue: VENUE.to_string(),
                symbol: SYMBOL.to_string(),
                interval: INTERVAL.to_string(),
                strategy: Box::new(make_harness()),
            }),
            ..CoreConfig::default()
        },
    );
    let cell = handle.snapshot_cell();
    replay_bars(&handle.bar_sender(), VENUE, SYMBOL, INTERVAL, &bars);
    // the replay is lossless + ordered; wait for the core to drain it
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

    // final position agrees, and the harness's executor accounting == the engine ledger
    let paper_pos = snap.positions.iter().find(|p| p.symbol == SYMBOL).map_or(0.0, |p| p.size);
    assert_eq!(paper_pos.to_bits(), engine_position.to_bits(), "final position");
    assert_eq!(
        outcome_realized.to_bits(),
        engine_realized.to_bits(),
        "executor outcomes sum {outcome_realized} vs engine trades sum {engine_realized}",
    );
    assert!(snap.fault.is_none());
    println!(
        "controller-harness parity green: {} fills bit-identical; {} round-trips; realized {}",
        paper.len(),
        engine.strategy.outcomes().len(),
        engine_realized,
    );
}
