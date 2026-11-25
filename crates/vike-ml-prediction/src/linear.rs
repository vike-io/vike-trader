//! A fitted LINEAR model — `intercept + Σ coefficient_j · x_j`, through an identity or a logistic
//! link — read from its text and answered row by row.
//!
//! It is what `vike-ml-training`'s ordinary least squares, Ridge, LASSO, Elastic Net and logistic
//! regression all fit (`crates/vike-ml-training/src/linear.rs`): every one of them ends in an
//! intercept and one coefficient per feature, so ONE model kind and ONE text format serve them all.
//!
//! # The text, `vike-ml linear v1`
//!
//! ```text
//! vike-ml linear v1
//! link=identity                 (or logistic)
//! feature_count=2
//! intercept=1e0
//! coefficients=2e0 -5e-1
//! end of model
//! ```
//!
//! The envelope's rules are [`crate::text_format`]'s. The linear predictor is a naive left fold
//! from the intercept in feature order, so a model gives the same bits on every box; the logistic
//! link is the workspace's one `vike_math::special_functions::logistic` (`libm::exp`, decision 0032).

use vike_math::special_functions::logistic;

use crate::error::MlError;
use crate::predictor::{PredictionKind, Predictor};
use crate::text_format::{Reader, Writer};

/// The kind word of this model's text.
pub const KIND: &str = "linear";

/// What turns the linear predictor into the prediction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinearLink {
    /// The prediction IS the linear predictor: a regression ([`PredictionKind::Value`]).
    Identity,
    /// The prediction is `1 / (1 + exp(-η))`, the probability of the positive class
    /// ([`PredictionKind::Probability`]).
    Logistic,
}

impl LinearLink {
    fn word(self) -> &'static str {
        match self {
            LinearLink::Identity => "identity",
            LinearLink::Logistic => "logistic",
        }
    }
}

/// `intercept + Σ coefficients[j] · row[j]`, through [`LinearModel::link`].
#[derive(Clone, Debug, PartialEq)]
pub struct LinearModel {
    pub link: LinearLink,
    pub intercept: f64,
    /// One per feature, in the row's column order.
    pub coefficients: Vec<f64>,
}

impl LinearModel {
    /// The linear predictor `η = intercept + Σ coefficients[j] · row[j]`, before the link. A row
    /// of another width is refused: a missing feature would silently count as zero.
    pub fn linear_predictor(&self, row: &[f64]) -> Result<f64, MlError> {
        if row.len() != self.coefficients.len() {
            return Err(MlError::Shape(format!(
                "the row is {} wide; this linear model has {} coefficients",
                row.len(),
                self.coefficients.len()
            )));
        }
        let mut total = self.intercept;
        for (value, coefficient) in row.iter().zip(&self.coefficients) {
            total += value * coefficient;
        }
        Ok(total)
    }

    /// This model as its `vike-ml linear v1` text, which [`LinearModel::from_text`] reads back bit
    /// for bit. A non-finite number would be refused by the reader, so a caller writes only a
    /// model whose numbers are finite (every fit in `vike-ml-training` refuses the others).
    pub fn to_text(&self) -> String {
        let mut w = Writer::new(KIND);
        w.text("link", self.link.word());
        w.count("feature_count", self.coefficients.len());
        w.number("intercept", self.intercept);
        w.numbers("coefficients", &self.coefficients);
        w.finish()
    }

    /// Read a `vike-ml linear v1` text.
    pub fn from_text(text: &str) -> Result<Self, MlError> {
        let mut r = Reader::open(text, KIND)?;
        let at = r.line();
        let link = match r.text("link")? {
            "identity" => LinearLink::Identity,
            "logistic" => LinearLink::Logistic,
            other => {
                return Err(MlError::Parse {
                    line: at,
                    what: format!("`link={other}` is neither `identity` nor `logistic`"),
                });
            }
        };
        let feature_count = r.count("feature_count")?;
        let intercept = r.number("intercept")?;
        let coefficients = r.numbers("coefficients", feature_count)?;
        r.finish()?;
        Ok(Self { link, intercept, coefficients })
    }
}

impl Predictor for LinearModel {
    fn kind(&self) -> PredictionKind {
        match self.link {
            LinearLink::Identity => PredictionKind::Value,
            LinearLink::Logistic => PredictionKind::Probability,
        }
    }

    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        let [slot] = out else {
            return Err(format!(
                "a linear model predicts one number; the buffer holds {}",
                out.len()
            ));
        };
        let eta = self.linear_predictor(row).map_err(|e| e.to_string())?;
        *slot = match self.link {
            LinearLink::Identity => eta,
            LinearLink::Logistic => logistic(eta),
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(link: LinearLink) -> LinearModel {
        LinearModel { link, intercept: 1.0, coefficients: vec![2.0, -0.5] }
    }

    #[test]
    fn the_identity_link_answers_the_linear_predictor_by_hand() {
        // 1 + 2·3 − 0.5·4 = 5
        assert_eq!(model(LinearLink::Identity).predict_one(&[3.0, 4.0]), Ok(5.0));
        assert_eq!(model(LinearLink::Identity).kind(), PredictionKind::Value);
    }

    #[test]
    fn the_logistic_link_answers_the_probability_of_the_linear_predictor() {
        let m = model(LinearLink::Logistic);
        assert_eq!(m.kind(), PredictionKind::Probability);
        // η = 1 + 2·0 − 0.5·2 = 0, so exactly one half.
        assert_eq!(m.predict_one(&[0.0, 2.0]), Ok(0.5));
        let p = m.predict_one(&[1.0, 0.0]).unwrap(); // η = 3
        assert_eq!(p.to_bits(), (1.0 / (1.0 + libm::exp(-3.0))).to_bits());
    }

    #[test]
    fn a_row_of_another_width_is_refused_not_zero_filled() {
        let m = model(LinearLink::Identity);
        assert!(m.predict_one(&[1.0]).unwrap_err().contains("1 wide"));
        assert!(m.predict_one(&[1.0, 2.0, 3.0]).is_err());
        assert!(m.predict_into(&[1.0, 2.0], &mut [0.0; 2]).is_err(), "a two-slot buffer");
    }

    #[test]
    fn the_text_reads_back_bit_for_bit() {
        for link in [LinearLink::Identity, LinearLink::Logistic] {
            let m = LinearModel {
                link,
                intercept: -0.1,
                coefficients: vec![1.0 / 3.0, -0.0, f64::from_bits(1), f64::MAX],
            };
            let text = m.to_text();
            let back = LinearModel::from_text(&text).unwrap();
            assert_eq!(back.link, m.link);
            let bits = |v: &LinearModel| -> Vec<u64> {
                std::iter::once(v.intercept)
                    .chain(v.coefficients.clone())
                    .map(f64::to_bits)
                    .collect()
            };
            assert_eq!(bits(&back), bits(&m), "{text}");
        }
    }

    #[test]
    fn the_text_is_the_documented_one() {
        assert_eq!(
            model(LinearLink::Logistic).to_text(),
            "vike-ml linear v1\nlink=logistic\nfeature_count=2\nintercept=1e0\n\
             coefficients=2e0 -5e-1\nend of model\n"
        );
        let empty =
            LinearModel { link: LinearLink::Identity, intercept: 0.5, coefficients: vec![] };
        assert_eq!(LinearModel::from_text(&empty.to_text()), Ok(empty));
    }

    #[test]
    fn a_wrong_link_or_count_is_refused() {
        let text = model(LinearLink::Identity).to_text();
        assert!(LinearModel::from_text(&text.replace("identity", "probit")).is_err());
        assert!(
            LinearModel::from_text(&text.replace("feature_count=2", "feature_count=3")).is_err()
        );
        assert!(LinearModel::from_text(&text.replace("linear v1", "gaussian_process v1")).is_err());
    }
}
