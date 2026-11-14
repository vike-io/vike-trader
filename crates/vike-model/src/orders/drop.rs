//! An order a gate DROPPED, as a backtest records it: the symbol, the gate's reason, the size refused
//! and the sizer's weight. The simulator collects them (`vike_simulator::OrderBrokerSimulator::dropped`), the result
//! carries them (`vike_analytics::BacktestResult::dropped`) and a run record writes them
//! (`vike_runs::RunDiagnostics::dropped`), one type all the way, its JSON keys `symbol`, `reason`,
//! `size` and `weight`.

use serde::{Deserialize, Serialize};

/// One order the engine's gates DROPPED, named rather than a tuple because a run record is a
/// document a human opens.
///
/// `vike_analytics::zero_trade::aggregate_denials` turns a run's list into a ranked diagnosis. Both
/// halves are worth keeping: the aggregate is what a reader reads, the rows are what an
/// investigation needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrderDrop {
    /// Which instrument the order was for.
    pub symbol: String,
    /// The gate's own reason string — a `RiskGate` reason, an order-kind cash-gate drop,
    /// `"volume_cap"` or `"latency_reject"`.
    pub reason: String,
    /// The order size that was refused.
    pub size: f64,
    /// The sizer's weight for it.
    pub weight: f64,
}
