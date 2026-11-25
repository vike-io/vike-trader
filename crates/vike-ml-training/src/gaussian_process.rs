//! A GAUSSIAN-PROCESS regressor with the RBF kernel, fitted in pure Rust (Rasmussen and Williams,
//! *Gaussian Processes for Machine Learning*, 2006, algorithm 2.1): the training targets are
//! centred on their mean, `K + σ_n² I` is factored by `vike_math::linear_algebra`'s Cholesky,
//! and the fit keeps what `vike-ml-prediction`'s [`GaussianProcessModel`] needs to answer a mean
//! and a variance with no solve: the weights `w = (K + σ_n² I)⁻¹ (y − m)` and the precision
//! `P = (K + σ_n² I)⁻¹`, made EXACTLY symmetric (its lower triangle copied up) so the model and
//! its `vike-ml gaussian_process v1` text predict the same bits.
//!
//! The hyperparameters `ℓ`, `σ_f²`, `σ_n²` are INPUTS ([`GaussianProcessParams`]). Each fit also
//! answers its log marginal likelihood (equation 2.30),
//! `−½ (y − m)ᵀ w − Σ log L_ii − (n/2) log 2π`, so a caller chooses them by comparing fits
//! (a grid, or `crate::search::tpe`). `O(n³)` time and `O(n²)` memory: a few thousand rows at most.
//!
//! The kernel is `vike-ml-prediction`'s one `rbf_kernel`; the one transcendental this file adds is
//! the likelihood's `libm::log` (decision 0032).

use vike_math::linear_algebra::{Matrix, cholesky};
use vike_ml_prediction::gaussian_process::{
    self as gaussian_model, GaussianProcessModel, rbf_kernel,
};

use crate::seam::fit_cache::{Transcript, data_digest};
use crate::{Capture, CapturedFit, GridPoint, Learner, TrainData};

/// The RBF kernel's hyperparameters and the noise the fit assumes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GaussianProcessParams {
    /// `ℓ > 0`, in the units of the features.
    pub length_scale: f64,
    /// `σ_f² > 0`, the prior variance of the latent function.
    pub signal_variance: f64,
    /// `σ_n² >= 0`, the observation noise; a small positive value is also the usual jitter that
    /// keeps `K` positive definite when two rows are close.
    pub noise_variance: f64,
}

/// A fitted model and the evidence for its hyperparameters.
#[derive(Clone, Debug, PartialEq)]
pub struct GaussianProcessFit {
    pub model: GaussianProcessModel,
    /// `log p(y | X, ℓ, σ_f², σ_n²)`, higher is better.
    pub log_marginal_likelihood: f64,
}

fn check(d: &TrainData<'_>, params: GaussianProcessParams) -> Result<(), String> {
    let GaussianProcessParams { length_scale, signal_variance, noise_variance } = params;
    let positive = |v: f64| v.is_finite() && v > 0.0;
    if !positive(length_scale)
        || !positive(signal_variance)
        || !noise_variance.is_finite()
        || noise_variance < 0.0
    {
        return Err(format!(
            "{params:?}: the length scale and the signal variance must be finite and positive, the \
             noise variance finite and not negative"
        ));
    }
    d.validate().map_err(|e| e.to_string())?;
    if d.n_rows == 0 {
        return Err("a Gaussian process needs at least one training row".into());
    }
    if !d.categorical.is_empty() {
        return Err(format!(
            "columns {:?} are declared categorical; the RBF kernel reads every column as a number",
            d.categorical
        ));
    }
    if d.x.iter().any(|v| !v.is_finite()) || d.y.iter().any(|v| !v.is_finite()) {
        return Err("a Gaussian process reads finite features and targets only".into());
    }
    Ok(())
}

/// Fit the process to `d` (features, regression targets) at `params`.
pub fn fit_gaussian_process(
    d: &TrainData<'_>,
    params: GaussianProcessParams,
) -> Result<GaussianProcessFit, String> {
    check(d, params)?;
    let n = d.n_rows;
    let GaussianProcessParams { length_scale, signal_variance, noise_variance } = params;
    let mean = vike_math::statistics::mean(d.y.iter().map(|&y| f64::from(y)));
    let centred: Vec<f64> = d.y.iter().map(|&y| f64::from(y) - mean).collect();
    let mut kernel = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let k = rbf_kernel(d.row(i), d.row(j), length_scale, signal_variance);
            kernel[i * n + j] = k;
            kernel[j * n + i] = k;
        }
        kernel[i * n + i] += noise_variance;
    }
    let kernel = Matrix::from_row_major(n, n, kernel).expect("the kernel is n × n");
    let factor = cholesky(&kernel).ok_or_else(|| {
        format!(
            "K + σ_n² I is not positive definite (two rows too close for the noise variance \
             {noise_variance}); raise the noise variance — about 1e-10 · σ_f² is the usual jitter"
        )
    })?;
    let weights = factor.solve(&centred).expect("the targets are n long");
    let mut precision = vec![0.0; n * n];
    let mut unit = vec![0.0; n];
    for j in 0..n {
        unit[j] = 1.0;
        let column = factor.solve(&unit).expect("the unit vector is n long");
        unit[j] = 0.0;
        // Column j's entries at and below the diagonal, mirrored: exactly symmetric.
        for i in j..n {
            precision[i * n + j] = column[i];
            precision[j * n + i] = column[i];
        }
    }
    let mut fit_term = 0.0;
    for (c, w) in centred.iter().zip(&weights) {
        fit_term += c * w;
    }
    let mut log_determinant_half = 0.0;
    for i in 0..n {
        log_determinant_half += libm::log(factor.lower().get(i, i));
    }
    let log_marginal_likelihood =
        -0.5 * fit_term - log_determinant_half - 0.5 * n as f64 * libm::log(std::f64::consts::TAU);
    let model = GaussianProcessModel {
        length_scale,
        signal_variance,
        noise_variance,
        mean,
        feature_count: d.n_cols,
        inputs: d.x.to_vec(),
        weights,
        precision,
    };
    Ok(GaussianProcessFit { model, log_marginal_likelihood })
}

/// A [`Learner`] over [`fit_gaussian_process`], its hyperparameters fixed at construction. Like
/// `crate::linear::LinearLearner`, it reads no [`GridPoint`] and no seed (the fit is
/// deterministic), so its [`Learner::fit_identity`] leaves both out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GaussianProcessLearner {
    pub params: GaussianProcessParams,
}

impl Learner for GaussianProcessLearner {
    type Model = GaussianProcessModel;

    fn fit(&self, d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Result<Self::Model, String> {
        Ok(fit_gaussian_process(d, self.params)?.model)
    }

    /// The tag `vike-ml gaussian_process fit-identity v2` (bumped when the fitting CODE changes
    /// what it answers; `v2` since the targets' mean became `vike_math::statistics::mean`'s
    /// compensated one), the model text's kind and version, the three hyperparameters' bits and the
    /// data's ([`data_digest`]). `None` for anything the fit would refuse up front.
    fn fit_identity(&self, d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Option<[u8; 32]> {
        check(d, self.params).ok()?;
        let GaussianProcessParams { length_scale, signal_variance, noise_variance } = self.params;
        let mut t = Transcript::new("vike-ml gaussian_process fit-identity v2");
        t.part(gaussian_model::KIND.as_bytes());
        t.part(vike_ml_prediction::text_format::FORMAT_VERSION.as_bytes());
        let mut numbers = Vec::with_capacity(24);
        for v in [length_scale, signal_variance, noise_variance] {
            numbers.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        t.part(&numbers);
        t.digest_part(data_digest(d));
        Some(t.finish())
    }

    /// The model's own `vike-ml gaussian_process v1` text when asked; no importance.
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<Self::Model>, String> {
        let model = self.fit(d, p, seed)?;
        let text = want.text.then(|| model.to_text());
        Ok((model, None, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::DEFAULT_POINT;
    use vike_ml_prediction::Predictor;

    const UNIT: GaussianProcessParams =
        GaussianProcessParams { length_scale: 1.0, signal_variance: 1.0, noise_variance: 0.0 };

    #[test]
    fn a_noiseless_fit_interpolates_with_no_variance_left_and_reverts_to_the_prior_far_away() {
        let x = [0.0, 0.7, 1.5, 3.0];
        let y = [1.0, -0.5, 2.0, 0.25];
        let d = TrainData::new(&x, &y, 1).unwrap();
        let fit = fit_gaussian_process(&d, UNIT).unwrap();
        for (xi, yi) in x.iter().zip(y) {
            let (mean, variance) = fit.model.predict_mean_and_variance(&[*xi]).unwrap();
            assert!((mean - f64::from(yi)).abs() < 1e-9, "x = {xi}: {mean}");
            assert!(variance < 1e-9, "x = {xi}: {variance}");
        }
        let (mean, variance) = fit.model.predict_mean_and_variance(&[100.0]).unwrap();
        assert_eq!((mean, variance), (fit.model.mean, 1.0));
        assert_eq!(fit.model.mean, 0.6875);
    }

    #[test]
    fn two_points_match_the_closed_form_mean_variance_and_likelihood() {
        // K = [[a, b], [b, a]] with a = σ_f² + σ_n², b = σ_f² exp(−Δ²/2ℓ²); centred targets (−h, h).
        let params =
            GaussianProcessParams { length_scale: 2.0, signal_variance: 1.5, noise_variance: 0.1 };
        let x = [0.0, 1.0];
        let y = [1.0, 2.0];
        let fit = fit_gaussian_process(&TrainData::new(&x, &y, 1).unwrap(), params).unwrap();
        let (a, b, h) = (1.6, 1.5 * libm::exp(-1.0 / 8.0), 0.5);
        // Midpoint: both kernel entries are c = σ_f² exp(−(1/2)² / 8); the mean is m (w sums to 0)
        // and the variance σ_f² − 2c² / (a + b).
        let c = 1.5 * libm::exp(-1.0 / 32.0);
        let (mean, variance) = fit.model.predict_mean_and_variance(&[0.5]).unwrap();
        assert!((mean - 1.5).abs() < 1e-12, "{mean}");
        assert!((variance - (1.5 - 2.0 * c * c / (a + b))).abs() < 1e-12, "{variance}");
        let fit_term = 2.0 * h * h / (a - b);
        let want =
            -0.5 * fit_term - 0.5 * libm::log(a * a - b * b) - libm::log(std::f64::consts::TAU);
        assert!(
            (fit.log_marginal_likelihood - want).abs() < 1e-12,
            "{}",
            fit.log_marginal_likelihood
        );
    }

    #[test]
    fn the_text_predicts_the_same_bits_as_the_fitted_model() {
        let x = [0.0, 0.1, 0.4, 0.4, 0.9, 1.0, 0.3, 0.2];
        let y = [0.5, 0.2, -0.1, 0.0];
        let d = TrainData::new(&x, &y, 2).unwrap();
        let learner = GaussianProcessLearner {
            params: GaussianProcessParams {
                length_scale: 0.5,
                signal_variance: 2.0,
                noise_variance: 1e-3,
            },
        };
        let want = Capture { importance: false, text: true };
        let (model, _, text) = learner.fit_captured(&d, &DEFAULT_POINT, 0, want).unwrap();
        let back = GaussianProcessModel::from_text(&text.unwrap()).unwrap();
        assert_eq!(back, model);
        for row in [[0.0, 0.0], [0.25, 0.3], [5.0, -1.0]] {
            let (m1, v1) = model.predict_mean_and_variance(&row).unwrap();
            let (m2, v2) = back.predict_mean_and_variance(&row).unwrap();
            assert_eq!((m1.to_bits(), v1.to_bits()), (m2.to_bits(), v2.to_bits()));
            assert_eq!(model.predict_one(&row).unwrap().to_bits(), m1.to_bits());
        }
        let identity = learner.fit_identity(&d, &DEFAULT_POINT, 0);
        assert!(identity.is_some());
        assert_eq!(learner.fit_identity(&d, &DEFAULT_POINT, 7), identity, "the seed is not read");
    }

    #[test]
    fn bad_hyperparameters_a_categorical_column_and_a_singular_kernel_are_refused() {
        let x = [0.0, 1.0];
        let y = [0.0, 1.0];
        let d = TrainData::new(&x, &y, 1).unwrap();
        for params in [
            GaussianProcessParams { length_scale: 0.0, ..UNIT },
            GaussianProcessParams { signal_variance: f64::NAN, ..UNIT },
            GaussianProcessParams { noise_variance: -1e-3, ..UNIT },
        ] {
            assert!(fit_gaussian_process(&d, params).is_err(), "{params:?}");
            assert_eq!(GaussianProcessLearner { params }.fit_identity(&d, &DEFAULT_POINT, 0), None);
        }
        assert!(fit_gaussian_process(&d.with_categorical(&[0]).unwrap(), UNIT).is_err());
        // Two identical rows and no noise: K is singular.
        let same = [1.0, 1.0];
        let error = fit_gaussian_process(&TrainData::new(&same, &y, 1).unwrap(), UNIT).unwrap_err();
        assert!(error.contains("noise variance"), "{error}");
    }
}
