//! **A PORTFOLIO's run** (`[[mounts]]`, decision 0300): `run_backtest`'s bar arm with each mount's strategy in
//! place of the one, over the simulator's mounted run (`vike_simulator::StrategyEngine::new_mounted`), the mounts
//! dispatched in list order as the live drain dispatches a node's mounts, on the run's one account. What it adds
//! to the record is each mount's row (`vike_analytics::results::TradingResultsMounts`), read off the simulator's
//! per-mount books (`vike_simulator::StrategyEngineMountReport`); the account's figures are the record's own.

use vike_analytics::TradingRecord;
use vike_analytics::results::{TradingResultsMount, TradingResultsMounts};
use vike_marketdata::Bar;
use vike_model::Strategy;
use vike_simulator::{
    OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, StrategyEngineMount,
    StrategyEngineMountReport, StrategyEngineMounts,
};

use crate::run::profile::{ProfileBacktest, ProfileMount};

/// `bars` (the run's slots) put in the order a portfolio's engine holds them, `ProfileBacktest::slot_series`' (by
/// first appearance in mount order), the order its equity log records and a rebuild reads. Every slot is some mount's
/// (`ProfileBacktest::mount_refusals`), so this only reorders.
pub(super) fn in_mount_order(profile: &ProfileBacktest, bars: &mut [(String, Vec<Bar>)]) {
    let order: Vec<String> = profile.slot_series().iter().map(|s| s.leg_name()).collect();
    bars.sort_by_key(|(slot, _)| order.iter().position(|s| s == slot));
}

/// The mounted engine of `profile`'s portfolio: each mount's strategy (`built`, in mount order) over its slots,
/// under its budget ([`budget_of`]).
pub(super) fn mounted_engine(
    profile: &ProfileBacktest,
    bars: Vec<(String, Vec<Bar>)>,
    built: Vec<Box<dyn Strategy<OrderBrokerSimulator>>>,
    params: StrategyEngineConfig,
) -> StrategyEngine<StrategyEngineMounts> {
    let mounts = profile
        .mounts
        .iter()
        .zip(built)
        .map(|(mount, strategy)| StrategyEngineMount {
            strategy,
            symbols: mount.symbols.clone(),
            budget: budget_of(profile, mount),
        })
        .collect();
    StrategyEngine::new_mounted(bars, mounts, params)
}

/// A mount's budget, the live type (`vike_execution::MountBudget`): its leverage cap (its own figure, else
/// `engine.max_mount_leverage`, every mount's default, which `mount_refusals` holds the own figure under) and its
/// loss latch (decision 0249, `vike_model::MountLossAction`'s default). `None` when it states neither.
fn budget_of(
    profile: &ProfileBacktest,
    mount: &ProfileMount,
) -> Option<vike_execution::MountBudget> {
    let max_leverage = mount.max_leverage.or(profile.engine.max_mount_leverage);
    (max_leverage.is_some() || mount.max_loss.is_some()).then(|| vike_execution::MountBudget {
        max_loss: mount.max_loss,
        max_notional: None,
        loss_action: vike_model::MountLossAction::default(),
        max_leverage,
    })
}

/// **Each mount's row of the report**, from the simulator's per-mount books (`reports`, in mount order): its file,
/// slots and figures (its closed-trade ledger's trades, win % and profit factor, net of commission; its PnL at the
/// last point of its curve; its leverage refusals and its latch), and what no mount holds: the account's PnL
/// (`record`'s final equity less `engine.cash`) less every mount's.
pub(super) fn mount_rows(
    profile: &ProfileBacktest,
    reports: Vec<StrategyEngineMountReport>,
    record: &TradingRecord,
) -> TradingResultsMounts {
    let rows: Vec<TradingResultsMount> = profile
        .mounts
        .iter()
        .zip(reports)
        .enumerate()
        .map(|(at, (mount, report))| {
            let all = report.closed_trades.as_ref().map(|c| &c.all);
            TradingResultsMount {
                mount: at,
                strategy_file: mount.strategy.file.clone(),
                strategy_sha256: mount.strategy.sha256().to_string(),
                strategy_sdk: mount.strategy.built_sdk(),
                symbols: mount.symbols.clone(),
                n_trades: all.map_or(0, |a| a.trades),
                win_rate: all.map_or(0.0, |a| a.percent_profitable),
                profit_factor: all.map_or(0.0, |a| a.profit_factor),
                pnl: report.pnl_curve.last().copied().unwrap_or(0.0),
                max_leverage: budget_of(profile, mount).and_then(|b| b.max_leverage),
                max_loss: mount.max_loss,
                leverage_refusals: report.leverage_refusals,
                latched_at: report.latched_at,
            }
        })
        .collect();
    let attributed = vike_math::summation::compensated_sum(rows.iter().map(|r| r.pnl));
    let unattributed_pnl = record.final_equity - profile.engine.cash - attributed;
    TradingResultsMounts { rows, unattributed_pnl }
}
