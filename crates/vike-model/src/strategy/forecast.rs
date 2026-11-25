//! A strategy's FORECAST (book W35): what it expects of one symbol over a horizon, PUBLISHED beside its
//! orders through [`OrderBroker::publish_forecast`] and recorded by the broker running it, so many
//! strategies' forecasts can be measured and combined (the alpha pool, `vike_portfolio::alpha_pool`).
//!
//! A forecast is not an order: nothing is sent, no position or order is touched, and a strategy that
//! publishes none runs exactly as before. Its terms are held to ONE rule, [`Forecast::new`]'s, at every
//! door a strategy reaches (a Rust strategy file's call crosses the C ABI as the three numbers and is
//! rebuilt through it on the host side; a Rhai script's goes through it before it is recorded), so a
//! recorded forecast is always a finite value over a positive horizon.

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use super::OrderBroker;

/// What a strategy expects of one symbol: a VALUE over a HORIZON, and how sure it is when it says.
///
/// The value's unit is the strategy's own: the book's convention is the expected return over the
/// horizon (`0.002` for +0.2 %), signed, so forecasts of different strategies compare, but a score or a
/// probability is a forecast too, and the pool measures each against what followed. Built only by
/// [`Forecast::new`] (and read back from a document through the same rule), so a forecast in hand is
/// always one the rule accepts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ForecastTerms")]
pub struct Forecast {
    value: f64,
    horizon_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
}

/// [`Forecast`]'s fields as a document holds them, before [`Forecast::new`] judges them.
#[derive(Deserialize)]
struct ForecastTerms {
    value: f64,
    horizon_ms: i64,
    #[serde(default)]
    confidence: Option<f64>,
}

impl TryFrom<ForecastTerms> for Forecast {
    type Error = String;
    fn try_from(t: ForecastTerms) -> Result<Self, String> {
        Forecast::new(t.value, t.horizon_ms, t.confidence)
    }
}

impl Forecast {
    /// The forecast `value` over the next `horizon_ms` milliseconds, with an optional `confidence` in
    /// `[0, 1]`.
    ///
    /// # Errors
    /// The rule's words for a value that is not finite, a horizon that is not positive, or a
    /// confidence outside `[0, 1]` (a NaN included).
    pub fn new(value: f64, horizon_ms: i64, confidence: Option<f64>) -> Result<Self, String> {
        if !value.is_finite() {
            return Err(format!("a forecast's value must be a finite number, not {value}"));
        }
        if horizon_ms <= 0 {
            return Err(format!(
                "a forecast's horizon must be a positive number of milliseconds, not {horizon_ms}"
            ));
        }
        if let Some(c) = confidence
            && !(0.0..=1.0).contains(&c)
        {
            return Err(format!("a forecast's confidence must be within [0, 1], not {c}"));
        }
        Ok(Forecast { value, horizon_ms, confidence })
    }

    /// The forecast itself, in the strategy's unit.
    pub fn value(&self) -> f64 {
        self.value
    }

    /// How far ahead it looks, in milliseconds (positive).
    pub fn horizon_ms(&self) -> i64 {
        self.horizon_ms
    }

    /// How sure the strategy is, in `[0, 1]`; `None` when it did not say.
    pub fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}

/// One forecast as a broker recorded it: WHEN the strategy decided (epoch milliseconds: a bar step's
/// CLOSE, its bar's time plus the bar's length, decision 0271; any other hook's event time), so nothing
/// deciding before `timestamp` may read it; and on WHICH symbol (the name it resolves to: `""` is
/// recorded as the run's own).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForecastRow {
    pub timestamp: i64,
    pub symbol: String,
    pub forecast: Forecast,
}

#[cfg(test)]
#[path = "forecast_tests.rs"]
mod tests;
