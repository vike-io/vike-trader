//! A live mount's published forecasts — the `kind=exec_forecast` series (book W35), on the account plane beside
//! the exec trade log ([`crate::exec_log`]), NOT market data.
//!
//! A strategy publishes a forecast through `vike_model::OrderBroker::publish_forecast`; a live mount's core
//! journals it (`vike_journal::JournalRecord::Forecast`) and `vike_datahub::execution_stream` turns each streamed record into a
//! row here, keyed `venue=<the mount's venue>`/`symbol=<the forecast's symbol>`, so the forecasts of every live
//! strategy are a queryable series beside its fills (what a drift check measures a model's later predictions by).
//! A backtest keeps its own in the run's `forecasts.json` (`vike_runs::FORECASTS_FILE`), never here.
//!
//! Pure data, no I/O: the DataFusion+Parquet codec lives with the other series codecs in `vike-store`'s
//! `datafusion_hist::codec`, as [`crate::ExecFillRow`]'s does.

use serde::{Deserialize, Serialize};

/// One published forecast, persisted as the `kind=exec_forecast` series. Identity `(venue, symbol)` is the
/// PARTITION path, never a stored column, as for every account kind; `mount_id` is the column that tells two
/// mounts' forecasts for one symbol apart (the core's mount key, `vike_core::strategy_state::mount_id_with`).
/// The three terms are a `vike_model::Forecast`'s, already held to its rule when they were published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecForecastRow {
    /// When the strategy DECIDED, epoch milliseconds (UTC): a bar step's bar CLOSE (the bar's own time is its
    /// open; decision 0271), any other hook's event time. So `StudyContext::forecasts_as_of`'s
    /// `ts <= decision` never shows a forecast built from a bar that had not closed by the decision. Named `ts`
    /// for the universal `ts` column every series codec and the range read key off.
    pub ts: i64,
    /// WHICH mount published it.
    pub mount_id: String,
    /// The forecast, in the strategy's own unit (`vike_model::Forecast::value`).
    pub value: f64,
    /// How far ahead it looks, in milliseconds.
    pub horizon_ms: i64,
    /// How sure the strategy said it was, in `[0, 1]`; `None` when it did not say.
    pub confidence: Option<f64>,
}
