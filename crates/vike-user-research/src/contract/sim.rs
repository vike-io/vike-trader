//! The simulator seam, erased: [`StudySim`] runs one backtest and hands back [`SimOutcome`].

use std::sync::Arc;

use vike_data::HistStore;

/// What one backtest hands back: the three numbers a sweep aggregates, and nothing else.
///
/// ⚠ **Deliberately NOT the simulator's own result type**: naming it would put `vike-backtest`
/// (layer 30) in this crate's `[dependencies]`, which
/// `crates/vike-ops/tests/architecture/layer_gate.rs` refuses, and a vectorised signal Sharpe and
/// an event-driven one must not silently become one number. A study gets a RESULT, never the
/// simulator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimOutcome {
    /// Fractional return over the run.
    pub total_return: f64,
    /// How many trades the run closed.
    pub n_trades: u64,
    /// Fraction of closed trades that won, in `[0, 1]`.
    pub win_rate: f64,
}

/// ONE event-driven backtest, as a study may ask for it.
///
/// The profile is `toml::Value` for the reason [`SimOutcome`] is local: the typed profile belongs
/// to a crate this one may not name, so the host owns the deserialization and its `Err`.
///
/// ⚠ The store is passed IN, never built: a study can NAME a `HistStore` but cannot construct one,
/// and the seam keeps that by handing the caller's store straight through.
pub trait StudySim: Send + Sync {
    /// Run one backtest and report the three numbers a sweep needs.
    ///
    /// `Err` is the HOST's refusal — an unparseable profile, a strategy it does not carry, a run
    /// that failed — carried as text so this crate names no error type it cannot see.
    fn run_one(
        &self,
        profile: &toml::Value,
        store: Arc<dyn HistStore + Send + Sync>,
    ) -> Result<SimOutcome, String>;
}
