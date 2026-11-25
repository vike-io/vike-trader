//! LINEAR MODELS, fitted in pure Rust: ordinary least squares, Ridge, LASSO, Elastic Net and
//! logistic regression (L2 or L1), each ending in `vike-ml-prediction`'s
//! [`LinearModel`] and its `vike-ml linear v1` text, so a strategy file runs the fitted model
//! exactly as it runs a LightGBM one.
//!
//! # The objectives (`n` rows, intercept `b₀` never penalised)
//!
//! - [`LinearSpec::OrdinaryLeastSquares`]: `min Σ (y − b₀ − x·b)²`.
//! - [`LinearSpec::Ridge`] `{ penalty: λ }`: `min (1/2n) Σ r² + (λ/2) ‖b‖²`.
//! - [`LinearSpec::ElasticNet`] `{ penalty: λ, l1_ratio: α }`:
//!   `min (1/2n) Σ r² + λ (α ‖b‖₁ + (1 − α)/2 ‖b‖²)` — glmnet's objective, and scikit-learn's
//!   `ElasticNet(alpha = λ, l1_ratio = α)`. LASSO is `α = 1`; `α = 0` is the Ridge above.
//! - [`LinearSpec::Logistic`] `{ penalty: λ, l1_ratio: α }`: labels `0`/`1`,
//!   `min −(1/n) Σ [y log p + (1 − y) log(1 − p)] + λ (α ‖b‖₁ + (1 − α)/2 ‖b‖²)`, `p` the logistic
//!   of `b₀ + x·b`: `α = 0` is the L2 penalty, `α = 1` the L1.
//!
//! Penalties apply to the coefficients on the scale the CALLER gives the features: standardise
//! them first when a penalty should treat them alike.
//!
//! # How each is solved
//!
//! - Ordinary least squares and Ridge: in closed form, by `vike_math::linear_algebra`'s
//!   `least_squares` (Householder QR) on the CENTRED design, with Ridge's `√(nλ) I` rows appended
//!   (the normal equations are never formed). A rank-deficient ordinary least squares is refused.
//! - Elastic Net (LASSO): cyclic coordinate descent with soft thresholding (Friedman, Hastie and
//!   Tibshirani, "Regularization paths for generalized linear models via coordinate descent",
//!   2010), on the centred columns, until a whole sweep moves no coefficient by more than the
//!   [`CONVERGENCE_TOLERANCE`] test.
//! - Logistic: Newton's method as iteratively reweighted least squares, each step a WEIGHTED
//!   Elastic Net solved by the same coordinate descent (glmnet's algorithm). The fitted
//!   probabilities come from the [`LinearModel`] itself, so the fit and the prediction share the
//!   ONE logistic function. Separable classes have no unpenalised optimum: such a fit runs out of
//!   Newton steps and is refused, which asks for a penalty above zero.
//!
//! Every fold is a naive left fold in index order and nothing calls a transcendental but the
//! logistic link (`libm::exp`, in `vike-ml-prediction`), so a fit is the same bits on every box.

use vike_math::linear_algebra::{Matrix, least_squares};
use vike_ml_prediction::Predictor;
use vike_ml_prediction::linear::{self as linear_model, LinearLink, LinearModel};

use crate::seam::fit_cache::{Transcript, data_digest};
use crate::{Capture, CapturedFit, GridPoint, Learner, TrainData};

/// Coordinate descent stops when a whole sweep moves no coefficient `j` by more than this, in the
/// sense `scale_j · Δb_j² <= tolerance² · Σ v_i z̃_i²` (glmnet's test, with the column's weighted
/// sum of squares `scale_j`): a change below one part in `10¹²` of the response's spread.
pub const CONVERGENCE_TOLERANCE: f64 = 1e-12;

/// The most coordinate-descent sweeps one fit may take before it is refused.
const MAXIMUM_SWEEPS: usize = 100_000;

/// The most Newton steps a logistic fit may take before it is refused.
const MAXIMUM_NEWTON_STEPS: usize = 100;

/// A logistic fit's probabilities are held in `[floor, 1 − floor]` for the working weights
/// `p (1 − p)`, so one confident row cannot zero its weight (glmnet's `pmin` rule).
const PROBABILITY_FLOOR: f64 = 1e-5;

/// Which linear model to fit, with its hyperparameters (the module doc states each objective).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinearSpec {
    OrdinaryLeastSquares,
    Ridge {
        penalty: f64,
    },
    /// `l1_ratio = 1` is LASSO.
    ElasticNet {
        penalty: f64,
        l1_ratio: f64,
    },
    /// `l1_ratio = 0` is the L2 penalty, `1` the L1.
    Logistic {
        penalty: f64,
        l1_ratio: f64,
    },
}

impl LinearSpec {
    fn check(self) -> Result<(), String> {
        let (penalty, l1_ratio) = match self {
            Self::OrdinaryLeastSquares => (0.0, 0.0),
            Self::Ridge { penalty } => (penalty, 0.0),
            Self::ElasticNet { penalty, l1_ratio } | Self::Logistic { penalty, l1_ratio } => {
                (penalty, l1_ratio)
            }
        };
        if !penalty.is_finite() || penalty < 0.0 {
            return Err(format!("the penalty {penalty} is not a finite number at or above zero"));
        }
        if !(0.0..=1.0).contains(&l1_ratio) {
            return Err(format!("the l1 ratio {l1_ratio} is not in [0, 1]"));
        }
        Ok(())
    }

    /// The bytes that tell two specs apart in a fit identity: a tag, then each number's bits.
    fn identity_bytes(self) -> Vec<u8> {
        let (tag, numbers): (u8, Vec<f64>) = match self {
            Self::OrdinaryLeastSquares => (0, Vec::new()),
            Self::Ridge { penalty } => (1, vec![penalty]),
            Self::ElasticNet { penalty, l1_ratio } => (2, vec![penalty, l1_ratio]),
            Self::Logistic { penalty, l1_ratio } => (3, vec![penalty, l1_ratio]),
        };
        let mut bytes = vec![tag];
        for number in numbers {
            bytes.extend_from_slice(&number.to_bits().to_le_bytes());
        }
        bytes
    }
}

/// Fit `spec` to `d`: every column a numeric feature, the labels the target (`0`/`1` for a
/// logistic fit). Refused: a categorical column (one-hot encode it first), a non-finite value, a
/// spec out of range, a rank-deficient ordinary least squares, a fit that does not converge.
pub fn fit_linear(d: &TrainData<'_>, spec: LinearSpec) -> Result<LinearModel, String> {
    check_data(d, spec)?;
    let y: Vec<f64> = d.y.iter().map(|&label| f64::from(label)).collect();
    match spec {
        LinearSpec::OrdinaryLeastSquares => ridge(d, &y, 0.0),
        LinearSpec::Ridge { penalty } => ridge(d, &y, penalty),
        LinearSpec::ElasticNet { penalty, l1_ratio } => {
            let weights = vec![1.0 / d.n_rows as f64; d.n_rows];
            let mut coefficients = vec![0.0; d.n_cols];
            let intercept =
                coordinate_descent(d, &y, &weights, penalty, l1_ratio, &mut coefficients)?;
            Ok(LinearModel { link: LinearLink::Identity, intercept, coefficients })
        }
        LinearSpec::Logistic { penalty, l1_ratio } => logistic(d, &y, penalty, l1_ratio),
    }
}

fn check_data(d: &TrainData<'_>, spec: LinearSpec) -> Result<(), String> {
    spec.check()?;
    d.validate().map_err(|e| e.to_string())?;
    if d.n_rows == 0 {
        return Err("a linear fit needs at least one row".into());
    }
    if !d.categorical.is_empty() {
        return Err(format!(
            "columns {:?} are declared categorical; a linear model reads every column as a number, \
             so one-hot encode a category first",
            d.categorical
        ));
    }
    if d.x.iter().any(|v| !v.is_finite()) || d.y.iter().any(|v| !v.is_finite()) {
        return Err(
            "a linear fit reads finite features and labels only (a NaN is not missing here)".into(),
        );
    }
    if matches!(spec, LinearSpec::Logistic { .. }) && d.y.iter().any(|&y| y != 0.0 && y != 1.0) {
        return Err("a logistic fit reads labels 0 and 1 only".into());
    }
    Ok(())
}

/// Each column's mean under `weights` (which need not sum to one) and the response's.
fn weighted_means(d: &TrainData<'_>, z: &[f64], weights: &[f64]) -> (Vec<f64>, f64) {
    let mut total = 0.0;
    let mut column_means = vec![0.0; d.n_cols];
    let mut response_mean = 0.0;
    for ((row, value), weight) in d.x.chunks_exact(d.n_cols).zip(z).zip(weights) {
        total += weight;
        for (mean, x) in column_means.iter_mut().zip(row) {
            *mean += weight * x;
        }
        response_mean += weight * value;
    }
    for mean in &mut column_means {
        *mean /= total;
    }
    (column_means, response_mean / total)
}

/// `b₀ = z̄ − Σ x̄_j b_j`: the intercept that the centring removed.
fn intercept_of(column_means: &[f64], response_mean: f64, coefficients: &[f64]) -> f64 {
    let mut intercept = response_mean;
    for (mean, coefficient) in column_means.iter().zip(coefficients) {
        intercept -= mean * coefficient;
    }
    intercept
}

/// Ordinary least squares (`penalty = 0`) or Ridge in closed form: QR of the centred design, with
/// `√(nλ) I` appended for Ridge.
fn ridge(d: &TrainData<'_>, y: &[f64], penalty: f64) -> Result<LinearModel, String> {
    let (n, p) = (d.n_rows, d.n_cols);
    let (column_means, response_mean) = weighted_means(d, y, &vec![1.0; n]);
    let extra = if penalty > 0.0 { p } else { 0 };
    let mut design = Vec::with_capacity((n + extra) * p);
    for row in d.x.chunks_exact(p) {
        design.extend(row.iter().zip(&column_means).map(|(x, mean)| x - mean));
    }
    let mut observations: Vec<f64> = y.iter().map(|v| v - response_mean).collect();
    if extra > 0 {
        let root = (n as f64 * penalty).sqrt();
        for j in 0..p {
            design.extend((0..p).map(|k| if k == j { root } else { 0.0 }));
            observations.push(0.0);
        }
    }
    let design =
        Matrix::from_row_major(n + extra, p, design).expect("the design is (n + extra) × p");
    let fit = least_squares(&design, &observations).ok_or_else(|| {
        format!(
            "ordinary least squares on {n} rows has no unique answer: a column is constant or a \
             combination of the others, or there are fewer than {} rows; Ridge (a penalty above \
             zero) has one",
            p + 1
        )
    })?;
    let intercept = intercept_of(&column_means, response_mean, &fit.coefficients);
    Ok(LinearModel { link: LinearLink::Identity, intercept, coefficients: fit.coefficients })
}

/// `S(z, γ) = sign(z) · max(|z| − γ, 0)`, the soft-thresholding operator.
fn soft_threshold(z: f64, gamma: f64) -> f64 {
    if z > gamma {
        z - gamma
    } else if z < -gamma {
        z + gamma
    } else {
        0.0
    }
}

/// Coordinate descent on `½ Σ v_i (z_i − b₀ − x_i·b)² + λ (α ‖b‖₁ + (1 − α)/2 ‖b‖²)` from the warm
/// start `coefficients`, which it leaves at the answer; returns the intercept `b₀`.
fn coordinate_descent(
    d: &TrainData<'_>,
    z: &[f64],
    weights: &[f64],
    penalty: f64,
    l1_ratio: f64,
    coefficients: &mut [f64],
) -> Result<f64, String> {
    let (n, p) = (d.n_rows, d.n_cols);
    let (column_means, response_mean) = weighted_means(d, z, weights);
    let mut centred = Vec::with_capacity(n * p);
    for row in d.x.chunks_exact(p) {
        centred.extend(row.iter().zip(&column_means).map(|(x, mean)| x - mean));
    }
    let mut scale = vec![0.0; p];
    let mut spread = 0.0;
    let mut residual = Vec::with_capacity(n);
    for ((row, value), weight) in centred.chunks_exact(p).zip(z).zip(weights) {
        let mut fitted = 0.0;
        for ((s, x), b) in scale.iter_mut().zip(row).zip(coefficients.iter()) {
            *s += weight * x * x;
            fitted += x * b;
        }
        let centred_value = value - response_mean;
        spread += weight * centred_value * centred_value;
        residual.push(centred_value - fitted);
    }
    let threshold = CONVERGENCE_TOLERANCE * CONVERGENCE_TOLERANCE * spread.max(f64::MIN_POSITIVE);
    let (l1, l2) = (penalty * l1_ratio, penalty * (1.0 - l1_ratio));
    for _ in 0..MAXIMUM_SWEEPS {
        let mut largest = 0.0f64;
        for j in 0..p {
            let denominator = scale[j] + l2;
            let new = if denominator > 0.0 {
                let mut rho = scale[j] * coefficients[j];
                for ((row, r), weight) in centred.chunks_exact(p).zip(&residual).zip(weights) {
                    rho += weight * row[j] * r;
                }
                soft_threshold(rho, l1) / denominator
            } else {
                0.0 // a constant column with no ridge part: its coefficient is free, so zero
            };
            let delta = new - coefficients[j];
            if delta != 0.0 {
                for (row, r) in centred.chunks_exact(p).zip(residual.iter_mut()) {
                    *r -= row[j] * delta;
                }
                coefficients[j] = new;
                largest = largest.max(scale[j] * delta * delta);
            }
        }
        if largest <= threshold {
            return Ok(intercept_of(&column_means, response_mean, coefficients));
        }
    }
    Err(format!("coordinate descent did not converge in {MAXIMUM_SWEEPS} sweeps"))
}

/// Penalised logistic regression by iteratively reweighted least squares.
fn logistic(
    d: &TrainData<'_>,
    y: &[f64],
    penalty: f64,
    l1_ratio: f64,
) -> Result<LinearModel, String> {
    let n = d.n_rows as f64;
    let mut model = LinearModel {
        link: LinearLink::Logistic,
        intercept: 0.0,
        coefficients: vec![0.0; d.n_cols],
    };
    let mut weights = Vec::with_capacity(d.n_rows);
    let mut working = Vec::with_capacity(d.n_rows);
    for _ in 0..MAXIMUM_NEWTON_STEPS {
        weights.clear();
        working.clear();
        for (i, label) in y.iter().enumerate() {
            let row = d.row(i);
            let eta = model.linear_predictor(row).map_err(|e| e.to_string())?;
            let p = model.predict_one(row)?.clamp(PROBABILITY_FLOOR, 1.0 - PROBABILITY_FLOOR);
            let w = p * (1.0 - p);
            weights.push(w / n);
            working.push(eta + (label - p) / w);
        }
        let mut coefficients = model.coefficients.clone();
        let intercept =
            coordinate_descent(d, &working, &weights, penalty, l1_ratio, &mut coefficients)?;
        let mut change = (intercept - model.intercept).abs();
        let mut size = intercept.abs();
        for (new, old) in coefficients.iter().zip(&model.coefficients) {
            change = change.max((new - old).abs());
            size = size.max(new.abs());
        }
        model.intercept = intercept;
        model.coefficients = coefficients;
        if change <= CONVERGENCE_TOLERANCE * 100.0 * (1.0 + size) {
            return Ok(model);
        }
    }
    Err(format!(
        "logistic regression did not converge in {MAXIMUM_NEWTON_STEPS} Newton steps: the classes \
         are separable (or nearly so), which has no unpenalised answer; fit with a penalty above zero"
    ))
}

/// A [`Learner`] over [`fit_linear`]: the hyperparameters are the SPEC, fixed at construction.
///
/// ⚠ **It reads no [`GridPoint`] and no seed.** The five grid axes are LightGBM's, and a linear fit
/// is deterministic, so [`Learner::fit_identity`] leaves both out too: every point and every seed is
/// the SAME fit and shares one cache entry, by construction. A search over a linear model's
/// hyperparameters builds one learner per spec.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearLearner {
    pub spec: LinearSpec,
}

impl Learner for LinearLearner {
    type Model = LinearModel;

    fn fit(&self, d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Result<LinearModel, String> {
        fit_linear(d, self.spec)
    }

    /// The tag `vike-ml linear fit-identity v1` (bump it when the fitting CODE changes what it
    /// answers), the model text's kind and version, the spec's bytes and the data's
    /// ([`data_digest`]). `None` for anything [`fit_linear`] would refuse up front.
    fn fit_identity(&self, d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Option<[u8; 32]> {
        check_data(d, self.spec).ok()?;
        let mut t = Transcript::new("vike-ml linear fit-identity v1");
        t.part(linear_model::KIND.as_bytes());
        t.part(vike_ml_prediction::text_format::FORMAT_VERSION.as_bytes());
        t.part(&self.spec.identity_bytes());
        t.digest_part(data_digest(d));
        Some(t.finish())
    }

    /// The model's own `vike-ml linear v1` text when asked; no importance (`None`: a linear model's
    /// coefficients are on the caller's feature scale, which is not an importance).
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<LinearModel>, String> {
        let model = self.fit(d, p, seed)?;
        let text = want.text.then(|| model.to_text());
        Ok((model, None, text))
    }
}

#[path = "linear_tests.rs"]
#[cfg(test)]
mod linear_tests;
