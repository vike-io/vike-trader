//! The `[engine.impact]` table: `ImpactCfg`, the opt-in market-impact model.

use std::sync::Arc;

use serde::Deserialize;

use crate::harness::HarnessError;

/// TOML shape of the opt-in market-impact model, e.g.
///
/// ```toml
/// [engine.impact]
/// model = "almgren_chriss"
/// exec_time = 1.0
/// window = 21
/// # gamma = 0.314   # published; the LEVEL knob, and the only one an L2 run can move
/// # eta   = 0.142   # published; the temporary half, which a book walk already pays
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCfg {
    /// which model — currently only `"almgren_chriss"`
    pub model: String,
    /// Execution horizon, in whatever period this run's context is measured over — bar-periods
    /// in bar mode, TRADE PRINTS in tick mode (see [`vike_sim::AlmgrenChriss::exec_time`]
    /// and [`vike_sim::TickWindow`]). The default `1.0` means "worked inside one period".
    ///
    /// ⚠ **It scales the TEMPORARY term only.** The permanent term is horizon-independent in the
    /// paper and `exec_time` cancels out of it exactly, so this knob is PROVABLY INERT on the L2
    /// lane, which is charged the permanent term and nothing else. Reach for [`Self::gamma`]
    /// there. Do not read this field as the answer to the tick lane's unit mismatch either — an
    /// earlier revision of the module docs said it was, and it moves only the smaller addend.
    #[serde(default = "default_exec_time")]
    pub exec_time: f64,
    /// Rolling sigma/volume lookback — in BARS in bar mode, in TRADE PRINTS in tick mode. The
    /// default is one trading month of daily bars; on a liquid tape it is a fraction of a
    /// second, so a tick profile should set it rather than inherit it. Lengthening it averages
    /// away sampling noise; it does NOT change the UNIT the context is measured in.
    #[serde(default = "default_impact_window")]
    pub window: usize,
    /// Permanent-impact coefficient, defaulting to the published [`vike_sim::AC_GAMMA`].
    ///
    /// **The one lever that reaches an L2-lane charge**, and a linear one: that lane pays
    /// `gamma * sigma * (qty/avg_volume)^alpha` and nothing else. It exists because the published
    /// calibration is fitted on DAILY US-equity context, and a run that measures its context per
    /// TRADE PRINT is spending the coefficient in a unit it was not fitted in — an operator who
    /// has measured that mismatch restates the LEVEL here rather than being told to turn a knob
    /// wired to nothing. The exponents are deliberately not exposed: they are the SHAPE (monotone,
    /// concave), and refitting them is a different model.
    #[serde(default = "default_ac_gamma")]
    pub gamma: f64,
    /// Temporary-impact coefficient, defaulting to the published [`vike_sim::AC_ETA`]. The
    /// twin of [`Self::gamma`] for the half a book walk already pays — so it moves the bar and
    /// L1-tick lanes and is inert on an L2 fill that walked a book. Same rationale, same fence
    /// around the exponents.
    #[serde(default = "default_ac_eta")]
    pub eta: f64,
}

fn default_exec_time() -> f64 {
    1.0
}

fn default_impact_window() -> usize {
    vike_sim::DEFAULT_IMPACT_WINDOW
}

fn default_ac_gamma() -> f64 {
    vike_sim::AC_GAMMA
}

fn default_ac_eta() -> f64 {
    vike_sim::AC_ETA
}

impl ImpactCfg {
    /// Resolve to a live model, or `Err` on an unknown `model` name / nonsensical numbers — a
    /// typo in a profile must fail loudly, not silently price fills at zero impact.
    pub fn build(&self) -> Result<Arc<dyn vike_sim::ImpactModel>, HarnessError> {
        if self.exec_time <= 0.0 || !self.exec_time.is_finite() {
            return Err(HarnessError::Validation(format!(
                "engine.impact.exec_time must be > 0, got {}",
                self.exec_time
            )));
        }
        if self.window < 3 {
            return Err(HarnessError::Validation(format!(
                "engine.impact.window must be >= 3 (two returns), got {}",
                self.window
            )));
        }
        // Both coefficients must be > 0 and finite. A NEGATIVE one would make the cost fall with
        // size and eventually go negative — a model that PAYS a large order — and the trait's
        // contract (finite, non-negative, non-decreasing in `qty`) is the thing the fill site
        // relies on when it declines to bound the estimate from above. Zero is refused too: it
        // spells "this half is switched off", which the profile can already say by not naming the
        // knob, and which would otherwise silently disarm the L2 lane's only charge.
        for (name, v) in [("gamma", self.gamma), ("eta", self.eta)] {
            if v <= 0.0 || !v.is_finite() {
                return Err(HarnessError::Validation(format!(
                    "engine.impact.{name} must be > 0 and finite, got {v}"
                )));
            }
        }
        match self.model.as_str() {
            "almgren_chriss" => Ok(Arc::new(vike_sim::AlmgrenChriss::with_coefficients(
                self.gamma,
                self.eta,
                self.exec_time,
            ))),
            other => Err(HarnessError::Validation(format!(
                "unknown engine.impact.model {other:?} (known: \"almgren_chriss\")"
            ))),
        }
    }
}
