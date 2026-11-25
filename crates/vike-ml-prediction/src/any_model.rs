//! [`AnyModel`]: every model kind this crate predicts with, behind ONE text reader — what a
//! strategy file or a study hands a model text to when it does not know (or care) which kind it is.
//!
//! [`AnyModel::from_text`] reads the first line: a `vike-ml <kind> v1` text
//! ([`crate::text_format`]) goes to its kind's reader, anything else to LightGBM's
//! ([`crate::parse_model_text`]). A kind word this crate does not know is a refusal.

use crate::calibration::{self, Calibration};
use crate::conformal::{self, ConformalModel};
use crate::ensemble::{self, Ensemble};
use crate::error::MlError;
use crate::gaussian_process::{self, GaussianProcessModel};
use crate::linear::{self, LinearModel};
use crate::model::GbdtModel;
use crate::model::parse::parse_model_text;
use crate::predictor::{PredictionKind, Predictor};
use crate::text_format::kind_of;

/// One parsed model of any kind this crate implements.
#[derive(Clone, Debug, PartialEq)]
pub enum AnyModel {
    /// LightGBM's gradient-boosted trees ([`crate::model`]).
    Gbdt(GbdtModel),
    /// A linear or logistic regression ([`crate::linear`]).
    Linear(LinearModel),
    /// A Gaussian-process regressor ([`crate::gaussian_process`]).
    GaussianProcess(GaussianProcessModel),
    /// A bagged mean or a stacked ensemble of other models ([`crate::ensemble`]).
    Ensemble(Ensemble),
    /// A model with its split-conformal calibration ([`crate::conformal`]).
    Conformal(ConformalModel),
    /// A calibration of forecast probabilities, a model of the one forecast it reads
    /// ([`crate::calibration`]).
    Calibration(Calibration),
}

impl AnyModel {
    /// Read any model text this crate implements.
    ///
    /// shortcut: a nested text (an ensemble member, a conformal wrapper's model) recurses with no
    /// depth bound, so a text nested thousands of levels deep exhausts the stack; the texts are
    /// the user's own runs. Add a depth argument if a model text ever arrives from elsewhere.
    pub fn from_text(text: &str) -> Result<Self, MlError> {
        match kind_of(text) {
            None => parse_model_text(text).map(Self::Gbdt),
            Some(linear::KIND) => LinearModel::from_text(text).map(Self::Linear),
            Some(gaussian_process::KIND) => {
                GaussianProcessModel::from_text(text).map(Self::GaussianProcess)
            }
            Some(ensemble::KIND) => Ensemble::from_text(text).map(Self::Ensemble),
            Some(conformal::KIND) => ConformalModel::from_text(text).map(Self::Conformal),
            Some(calibration::KIND) => Calibration::from_text(text).map(Self::Calibration),
            Some(other) => Err(MlError::Unsupported(format!(
                "model kind `{other}`: this build reads `{}`, `{}`, `{}`, `{}`, `{}` and LightGBM's text",
                linear::KIND,
                gaussian_process::KIND,
                ensemble::KIND,
                conformal::KIND,
                calibration::KIND
            ))),
        }
    }

    fn predictor(&self) -> &dyn Predictor {
        match self {
            Self::Gbdt(m) => m,
            Self::Linear(m) => m,
            Self::GaussianProcess(m) => m,
            Self::Ensemble(m) => m,
            Self::Conformal(m) => m,
            Self::Calibration(m) => m,
        }
    }
}

impl Predictor for AnyModel {
    fn kind(&self) -> PredictionKind {
        self.predictor().kind()
    }

    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        self.predictor().predict_into(row, out)
    }
}

impl From<GbdtModel> for AnyModel {
    fn from(m: GbdtModel) -> Self {
        Self::Gbdt(m)
    }
}

impl From<LinearModel> for AnyModel {
    fn from(m: LinearModel) -> Self {
        Self::Linear(m)
    }
}

impl From<GaussianProcessModel> for AnyModel {
    fn from(m: GaussianProcessModel) -> Self {
        Self::GaussianProcess(m)
    }
}

impl From<Ensemble> for AnyModel {
    fn from(m: Ensemble) -> Self {
        Self::Ensemble(m)
    }
}

impl From<ConformalModel> for AnyModel {
    fn from(m: ConformalModel) -> Self {
        Self::Conformal(m)
    }
}

impl From<Calibration> for AnyModel {
    fn from(m: Calibration) -> Self {
        Self::Calibration(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::LinearLink;

    #[test]
    fn each_text_goes_to_its_own_kinds_reader() {
        let linear =
            LinearModel { link: LinearLink::Identity, intercept: 1.0, coefficients: vec![2.0] };
        let any = AnyModel::from_text(&linear.to_text()).unwrap();
        assert_eq!(any, AnyModel::Linear(linear));
        assert_eq!(any.predict_one(&[3.0]), Ok(7.0));

        let gbdt = include_str!("../tests/fixtures/two_trees_binary_v4.txt");
        let any = AnyModel::from_text(gbdt).unwrap();
        assert_eq!(any, AnyModel::Gbdt(parse_model_text(gbdt).unwrap()));
        assert_eq!(any.kind(), PredictionKind::Probability);
    }

    #[test]
    fn an_unknown_kind_is_refused_by_name() {
        let error = AnyModel::from_text("vike-ml forest v1\nend of model\n").unwrap_err();
        assert!(error.to_string().contains("`forest`"), "{error}");
    }
}
