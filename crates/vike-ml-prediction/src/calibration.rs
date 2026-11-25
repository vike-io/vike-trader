//! PROBABILITY CALIBRATION, the applying half: a fitted map from a forecast probability to a
//! calibrated one, so that among the events forecast at `p`, a share close to `p` happens.
//! `vike-ml-training` fits it and scores forecasts (`crates/vike-ml-training/src/calibration.rs`);
//! this is how a fitted calibration is read and applied.
//!
//! - **Platt** (Platt, "Probabilistic outputs for support vector machines", 1999), on the LOG-ODDS
//!   of the forecast: `p' = 1 / (1 + exp(−(slope · log_odds(p) + intercept)))`. Slope 1 and
//!   intercept 0 leave a forecast as it is; a slope below 1 pulls over-confident forecasts toward
//!   one half, a slope above 1 pushes timid ones out (Cox, "Two further applications of a model for
//!   binary regression", 1958, reads the same two numbers as a calibration test).
//! - **Isotonic** (Zadrozny and Elkan, "Transforming classifier scores into accurate multiclass
//!   probability estimates", 2002): a non-decreasing STEP function. Step `k` starts at the forecast
//!   `starts[k]` and answers `values[k]` up to the next step's start; a forecast below the first
//!   start answers the first value.
//!
//! A [`Calibration`] is a model in its own right: its feature row is the ONE forecast it calibrates
//! (`[p]`), and it predicts a [`PredictionKind::Probability`]. So it applies alone, to a forecast that
//! came from anywhere (`predict_one(&[p])`), or on top of a model as a STACKED ensemble whose single
//! member is that model and whose meta model is the calibration
//! (`crate::ensemble::Ensemble::text_from_parts(&[model_text], Some(&calibration.to_text()))`): the
//! one text a strategy is handed then answers calibrated probabilities.
//!
//! # The text, `vike-ml calibration v1`
//!
//! ```text
//! vike-ml calibration v1
//! method=platt
//! slope=<number>
//! intercept=<number>
//! end of model
//! ```
//!
//! ```text
//! vike-ml calibration v1
//! method=isotonic
//! step_count=<K, at least 1>
//! starts=<K forecasts in [0, 1], strictly ascending>
//! values=<K probabilities in [0, 1], non-decreasing>
//! end of model
//! ```
//!
//! The envelope's rules are [`crate::text_format`]'s. Both transcendentals are the workspace's one
//! logistic pair, `vike_math::special_functions::logistic` and `vike_math::special_functions::logit` (`libm`, decision
//! 0032); [`log_odds`] adds only its clamp.

use vike_math::special_functions::{logistic, logit};

use crate::error::MlError;
use crate::predictor::{PredictionKind, Predictor};
use crate::text_format::{Reader, Writer};

/// The kind word of this model's text.
pub const KIND: &str = "calibration";

/// [`log_odds`] reads a forecast below this as this, and one above `1 −` this as `1 −` this, so a
/// forecast of exactly 0 or 1 keeps its sign at about ±36 instead of becoming infinite.
pub const FORECAST_FLOOR: f64 = f64::EPSILON;

/// The logit `ln(p / (1 − p))` (`vike_math::special_functions::logit`), with `p` first held in
/// `[FORECAST_FLOOR, 1 − FORECAST_FLOOR]`: the coordinate Platt calibration is linear in. The fit
/// (`vike-ml-training`) and the apply read this ONE clamp.
pub fn log_odds(probability: f64) -> f64 {
    logit(probability.clamp(FORECAST_FLOOR, 1.0 - FORECAST_FLOOR))
}

/// A fitted calibration of forecast probabilities (the module doc defines both methods).
///
/// Every fit in `vike-ml-training` builds a valid one, and [`Calibration::from_text`] refuses an
/// invalid one; a caller building one by hand keeps the invariants [`Calibration::check`] states.
#[derive(Clone, Debug, PartialEq)]
pub enum Calibration {
    Platt {
        slope: f64,
        intercept: f64,
    },
    Isotonic {
        /// Each step's lowest forecast, strictly ascending, in `[0, 1]`.
        starts: Vec<f64>,
        /// Each step's probability, non-decreasing, in `[0, 1]`; as many as `starts`.
        values: Vec<f64>,
    },
}

impl Calibration {
    /// The calibrated probability of the forecast `probability`, which must be in `[0, 1]`.
    pub fn apply(&self, probability: f64) -> Result<f64, MlError> {
        if !(0.0..=1.0).contains(&probability) {
            return Err(MlError::Shape(format!(
                "the forecast {probability} is not a probability in [0, 1]"
            )));
        }
        Ok(match self {
            Self::Platt { slope, intercept } => logistic(slope * log_odds(probability) + intercept),
            Self::Isotonic { starts, values } => {
                let step = starts.partition_point(|&start| start <= probability);
                values[step.saturating_sub(1)]
            }
        })
    }

    /// Platt: both numbers finite. Isotonic: at least one step, the starts strictly ascending in
    /// `[0, 1]`, the values non-decreasing in `[0, 1]`, one value per start.
    pub fn check(&self) -> Result<(), MlError> {
        match self {
            Self::Platt { slope, intercept } => {
                if !slope.is_finite() || !intercept.is_finite() {
                    return Err(MlError::Shape(format!(
                        "a Platt calibration needs a finite slope and intercept, not {slope} and {intercept}"
                    )));
                }
            }
            Self::Isotonic { starts, values } => {
                let probability = |v: &f64| (0.0..=1.0).contains(v);
                if starts.is_empty() || starts.len() != values.len() {
                    return Err(MlError::Shape(format!(
                        "an isotonic calibration needs one value per step and at least one step; \
                         it has {} start(s) and {} value(s)",
                        starts.len(),
                        values.len()
                    )));
                }
                if !starts.iter().all(probability) || starts.windows(2).any(|w| w[0] >= w[1]) {
                    return Err(MlError::Shape(
                        "an isotonic calibration's starts must rise strictly within [0, 1]".into(),
                    ));
                }
                if !values.iter().all(probability) || values.windows(2).any(|w| w[0] > w[1]) {
                    return Err(MlError::Shape(
                        "an isotonic calibration's values must not fall and must lie in [0, 1]"
                            .into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// This calibration as its `vike-ml calibration v1` text, which [`Calibration::from_text`]
    /// reads back bit for bit.
    pub fn to_text(&self) -> String {
        let mut w = Writer::new(KIND);
        match self {
            Self::Platt { slope, intercept } => {
                w.text("method", "platt");
                w.number("slope", *slope);
                w.number("intercept", *intercept);
            }
            Self::Isotonic { starts, values } => {
                w.text("method", "isotonic");
                w.count("step_count", starts.len());
                w.numbers("starts", starts);
                w.numbers("values", values);
            }
        }
        w.finish()
    }

    /// Read a `vike-ml calibration v1` text; an invalid calibration ([`Calibration::check`]) is
    /// refused.
    pub fn from_text(text: &str) -> Result<Self, MlError> {
        let mut r = Reader::open(text, KIND)?;
        let at = r.line();
        let calibration = match r.text("method")? {
            "platt" => Self::Platt { slope: r.number("slope")?, intercept: r.number("intercept")? },
            "isotonic" => {
                let count = r.count("step_count")?;
                Self::Isotonic {
                    starts: r.numbers("starts", count)?,
                    values: r.numbers("values", count)?,
                }
            }
            other => {
                return Err(MlError::Parse {
                    line: at,
                    what: format!("`method={other}` is neither `platt` nor `isotonic`"),
                });
            }
        };
        r.finish()?;
        calibration.check()?;
        Ok(calibration)
    }
}

impl Predictor for Calibration {
    fn kind(&self) -> PredictionKind {
        PredictionKind::Probability
    }

    /// `row` is the one forecast to calibrate.
    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        let [forecast] = row else {
            return Err(format!("a calibration reads one forecast; the row holds {}", row.len()));
        };
        let [slot] = out else {
            return Err(format!(
                "a calibration answers one probability; the buffer holds {}",
                out.len()
            ));
        };
        *slot = self.apply(*forecast).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::any_model::AnyModel;
    use crate::ensemble::Ensemble;
    use crate::linear::{LinearLink, LinearModel};

    fn isotonic() -> Calibration {
        Calibration::Isotonic { starts: vec![0.1, 0.2, 0.4], values: vec![0.0, 0.5, 1.0] }
    }

    #[test]
    fn platt_by_hand() {
        // Slope 2 squares the odds: p = 3/4 is odds 3, odds 9 is 9/10.
        let sharpen = Calibration::Platt { slope: 2.0, intercept: 0.0 };
        assert!((sharpen.apply(0.75).unwrap() - 0.9).abs() < 1e-15);
        assert_eq!(sharpen.apply(0.5), Ok(0.5));
        // Intercept ln 3 multiplies the odds by 3: p = 1/2 becomes 3/4.
        let shift = Calibration::Platt { slope: 1.0, intercept: libm::log(3.0) };
        assert!((shift.apply(0.5).unwrap() - 0.75).abs() < 1e-15);
        // Slope 1, intercept 0 leaves a forecast as it is; 0 and 1 stay finite and on their side.
        let identity = Calibration::Platt { slope: 1.0, intercept: 0.0 };
        for p in [0.01, 0.3, 0.5, 0.97] {
            assert!((identity.apply(p).unwrap() - p).abs() < 1e-15, "{p}");
        }
        assert!(identity.apply(0.0).unwrap() < 1e-15 && identity.apply(1.0).unwrap() > 1.0 - 1e-15);
        assert_eq!(log_odds(0.0), -log_odds(1.0));
        assert!(log_odds(0.0).is_finite());
    }

    #[test]
    fn isotonic_answers_the_step_a_forecast_falls_in() {
        let c = isotonic();
        for (p, want) in [(0.0, 0.0), (0.1, 0.0), (0.15, 0.0), (0.2, 0.5), (0.35, 0.5), (0.4, 1.0)]
        {
            assert_eq!(c.apply(p), Ok(want), "{p}");
        }
        assert_eq!(c.apply(1.0), Ok(1.0));
    }

    #[test]
    fn a_forecast_outside_zero_to_one_or_a_row_of_another_width_is_refused() {
        let c = isotonic();
        for p in [-0.1, 1.1, f64::NAN] {
            assert!(c.apply(p).is_err(), "{p}");
            assert!(c.predict_one(&[p]).is_err(), "{p}");
        }
        assert!(c.predict_one(&[0.5, 0.5]).is_err());
        assert!(c.predict_into(&[0.5], &mut [0.0; 2]).is_err());
        assert_eq!(c.kind(), PredictionKind::Probability);
        assert_eq!(c.predict_one(&[0.3]), Ok(0.5));
    }

    #[test]
    fn the_text_reads_back_bit_for_bit_and_refuses_an_invalid_calibration() {
        for c in [Calibration::Platt { slope: 1.0 / 3.0, intercept: -0.0 }, isotonic()] {
            let text = c.to_text();
            assert_eq!(Calibration::from_text(&text), Ok(c.clone()), "{text}");
            assert_eq!(AnyModel::from_text(&text), Ok(AnyModel::Calibration(c)));
        }
        assert_eq!(
            isotonic().to_text(),
            "vike-ml calibration v1\nmethod=isotonic\nstep_count=3\nstarts=1e-1 2e-1 4e-1\n\
             values=0e0 5e-1 1e0\nend of model\n"
        );
        let text = isotonic().to_text();
        for bad in [
            text.replace("starts=1e-1 2e-1", "starts=2e-1 1e-1"),
            text.replace("starts=1e-1", "starts=-1e-1"),
            text.replace("values=0e0 5e-1", "values=6e-1 5e-1"),
            text.replace("1e0\nend", "2e0\nend"),
            text.replace("method=isotonic", "method=beta"),
            text.replace(
                "step_count=3\nstarts=1e-1 2e-1 4e-1\nvalues=0e0 5e-1 1e0",
                "step_count=0\nstarts=\nvalues=",
            ),
        ] {
            assert!(Calibration::from_text(&bad).is_err(), "accepted: {bad:?}");
        }
    }

    #[test]
    fn stacked_over_a_model_it_calibrates_that_models_probability() {
        let model =
            LinearModel { link: LinearLink::Logistic, intercept: 0.0, coefficients: vec![1.0] };
        let calibration = Calibration::Platt { slope: 0.5, intercept: 0.25 };
        let text = Ensemble::text_from_parts(&[&model.to_text()], Some(&calibration.to_text()));
        let stacked = AnyModel::from_text(&text).unwrap();
        assert_eq!(stacked.kind(), PredictionKind::Probability);
        for x in [-3.0, 0.0, 1.5] {
            let raw = model.predict_one(&[x]).unwrap();
            let want = calibration.apply(raw).unwrap();
            assert_eq!(stacked.predict_one(&[x]).unwrap().to_bits(), want.to_bits(), "{x}");
        }
    }
}
