//! A fitted GAUSSIAN-PROCESS regressor with the RBF kernel: the posterior mean AND its variance at
//! a new row, from what the fit kept (Rasmussen and Williams, *Gaussian Processes for Machine
//! Learning*, 2006, algorithm 2.1).
//!
//! - The kernel is [`rbf_kernel`], `k(a, b) = σ_f² · exp(−‖a − b‖² / (2ℓ²))`, the one copy the
//!   fit (`crates/vike-ml-training/src/gaussian_process.rs`) builds its matrix with.
//! - The mean is `m + Σ_i w_i · k(x, x_i)`, with `w = (K + σ_n² I)⁻¹ (y − m)` and `m` the training
//!   targets' mean, both kept by the fit.
//! - The variance of the latent function is `k(x, x) − kᵀ P k` with `P = (K + σ_n² I)⁻¹` kept by
//!   the fit, so a prediction is two dot products and no solve: this crate links no linear algebra
//!   (it names no `vike-*` crate). A rounding below zero answers zero. The variance of a new
//!   OBSERVATION adds [`GaussianProcessModel::noise_variance`].
//!
//! # The text, `vike-ml gaussian_process v1`
//!
//! ```text
//! vike-ml gaussian_process v1
//! kernel=rbf
//! feature_count=<d>
//! training_count=<n>
//! length_scale=…
//! signal_variance=…
//! noise_variance=…
//! mean=…
//! inputs=<n · d numbers, row after row>
//! weights=<n numbers>
//! precision=<n (n + 1) / 2 numbers: P's lower triangle, row after row>
//! end of model
//! ```
//!
//! The envelope's rules are [`crate::text_format`]'s. Every fold is a naive left fold in index
//! order and the one transcendental is `libm::exp` (decision 0032), so a prediction is the same
//! bits on every box. The text grows as `n²`: a GP is for a few thousand training rows at most.

use crate::error::MlError;
use crate::predictor::{PredictionKind, Predictor};
use crate::text_format::{Reader, Writer};

/// The kind word of this model's text.
pub const KIND: &str = "gaussian_process";

/// The squared-exponential (RBF) kernel `σ_f² · exp(−‖a − b‖² / (2ℓ²))` of two rows of equal width.
pub fn rbf_kernel(a: &[f64], b: &[f64], length_scale: f64, signal_variance: f64) -> f64 {
    let squared_distance = vike_math::linear_algebra::squared_distance(a, b);
    signal_variance * libm::exp(-squared_distance / (2.0 * length_scale * length_scale))
}

/// What a Gaussian-process fit keeps to predict.
#[derive(Clone, Debug, PartialEq)]
pub struct GaussianProcessModel {
    /// `ℓ`, in the units of the features.
    pub length_scale: f64,
    /// `σ_f²`, the prior variance of the latent function.
    pub signal_variance: f64,
    /// `σ_n²`, the variance of the observation noise the fit assumed.
    pub noise_variance: f64,
    /// `m`, the training targets' mean: the prior mean the fit centred on.
    pub mean: f64,
    pub feature_count: usize,
    /// The training rows, `training_count · feature_count` values, row-major.
    pub inputs: Vec<f64>,
    /// `w = (K + σ_n² I)⁻¹ (y − m)`, one per training row.
    pub weights: Vec<f64>,
    /// `P = (K + σ_n² I)⁻¹`, `training_count²` values, row-major and symmetric.
    pub precision: Vec<f64>,
}

impl GaussianProcessModel {
    /// How many training rows the model carries.
    pub fn training_count(&self) -> usize {
        self.weights.len()
    }

    /// The posterior mean and the latent function's posterior variance at `row`. A row of another
    /// width is refused.
    pub fn predict_mean_and_variance(&self, row: &[f64]) -> Result<(f64, f64), MlError> {
        if row.len() != self.feature_count || self.feature_count == 0 {
            return Err(MlError::Shape(format!(
                "the row is {} wide; this Gaussian process reads {} features",
                row.len(),
                self.feature_count
            )));
        }
        let kernel: Vec<f64> = self
            .inputs
            .chunks_exact(self.feature_count)
            .map(|x| rbf_kernel(row, x, self.length_scale, self.signal_variance))
            .collect();
        let mut mean = self.mean;
        for (weight, k) in self.weights.iter().zip(&kernel) {
            mean += weight * k;
        }
        let n = kernel.len();
        let mut explained = 0.0;
        for (i, k_i) in kernel.iter().enumerate() {
            let mut row_total = 0.0;
            for (p, k_j) in self.precision[i * n..(i + 1) * n].iter().zip(&kernel) {
                row_total += p * k_j;
            }
            explained += k_i * row_total;
        }
        Ok((mean, (self.signal_variance - explained).max(0.0)))
    }

    /// This model as its `vike-ml gaussian_process v1` text, which
    /// [`GaussianProcessModel::from_text`] reads back bit for bit (the precision through its lower
    /// triangle, which the fit leaves exactly symmetric).
    pub fn to_text(&self) -> String {
        let n = self.training_count();
        let mut lower = Vec::with_capacity(n * (n + 1) / 2);
        for i in 0..n {
            lower.extend_from_slice(&self.precision[i * n..=i * n + i]);
        }
        let mut w = Writer::new(KIND);
        w.text("kernel", "rbf");
        w.count("feature_count", self.feature_count);
        w.count("training_count", n);
        w.number("length_scale", self.length_scale);
        w.number("signal_variance", self.signal_variance);
        w.number("noise_variance", self.noise_variance);
        w.number("mean", self.mean);
        w.numbers("inputs", &self.inputs);
        w.numbers("weights", &self.weights);
        w.numbers("precision", &lower);
        w.finish()
    }

    /// Read a `vike-ml gaussian_process v1` text.
    pub fn from_text(text: &str) -> Result<Self, MlError> {
        let mut r = Reader::open(text, KIND)?;
        let at = r.line();
        let kernel = r.text("kernel")?;
        if kernel != "rbf" {
            return Err(MlError::Parse {
                line: at,
                what: format!("`kernel={kernel}`: only `rbf` is implemented"),
            });
        }
        let feature_count = r.count("feature_count")?;
        let n = r.count("training_count")?;
        let at = r.line();
        let length_scale = r.number("length_scale")?;
        let signal_variance = r.number("signal_variance")?;
        let noise_variance = r.number("noise_variance")?;
        if length_scale <= 0.0 || signal_variance <= 0.0 || noise_variance < 0.0 {
            return Err(MlError::Parse {
                line: at,
                what: "the length scale and the signal variance must be positive and the noise \
                       variance not negative"
                    .into(),
            });
        }
        let mean = r.number("mean")?;
        let too_large = || MlError::Parse { line: at, what: "the counts overflow".into() };
        let inputs = r.numbers("inputs", n.checked_mul(feature_count).ok_or_else(too_large)?)?;
        let weights = r.numbers("weights", n)?;
        let triangle = n.checked_add(1).and_then(|m| m.checked_mul(n)).ok_or_else(too_large)? / 2;
        let lower = r.numbers("precision", triangle)?;
        r.finish()?;
        let mut precision = vec![0.0; n * n];
        let mut next = lower.iter();
        for i in 0..n {
            for j in 0..=i {
                let value = *next.next().expect("the reader checked the length");
                precision[i * n + j] = value;
                precision[j * n + i] = value;
            }
        }
        Ok(Self {
            length_scale,
            signal_variance,
            noise_variance,
            mean,
            feature_count,
            inputs,
            weights,
            precision,
        })
    }
}

impl Predictor for GaussianProcessModel {
    fn kind(&self) -> PredictionKind {
        PredictionKind::Value
    }

    /// The posterior MEAN; [`GaussianProcessModel::predict_mean_and_variance`] adds its variance.
    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        let [slot] = out else {
            return Err(format!(
                "a Gaussian process predicts one number; the buffer holds {}",
                out.len()
            ));
        };
        *slot = self.predict_mean_and_variance(row).map_err(|e| e.to_string())?.0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two training points at 0 and 1 on one feature, ℓ = 1, σ_f² = 1, σ_n² = 0. By hand:
    /// `K = [[1, e], [e, 1]]` with `e = exp(−1/2)`, `P = K⁻¹ = [[1, −e], [−e, 1]] / (1 − e²)`.
    fn two_points(targets: [f64; 2]) -> GaussianProcessModel {
        let e = libm::exp(-0.5);
        let det = 1.0 - e * e;
        let precision = vec![1.0 / det, -e / det, -e / det, 1.0 / det];
        let mean = (targets[0] + targets[1]) / 2.0;
        let centred = [targets[0] - mean, targets[1] - mean];
        let weights = vec![
            precision[0] * centred[0] + precision[1] * centred[1],
            precision[2] * centred[0] + precision[3] * centred[1],
        ];
        GaussianProcessModel {
            length_scale: 1.0,
            signal_variance: 1.0,
            noise_variance: 0.0,
            mean,
            feature_count: 1,
            inputs: vec![0.0, 1.0],
            weights,
            precision,
        }
    }

    #[test]
    fn the_kernel_is_the_squared_exponential_by_hand() {
        assert_eq!(rbf_kernel(&[1.0, 2.0], &[1.0, 2.0], 3.0, 2.0), 2.0);
        // ‖a − b‖² = 9 + 16 = 25, ℓ = 5: exp(−25 / 50) = exp(−1/2), times σ_f² = 2.
        let k = rbf_kernel(&[0.0, 0.0], &[3.0, 4.0], 5.0, 2.0);
        assert_eq!(k.to_bits(), (2.0 * libm::exp(-0.5)).to_bits());
    }

    #[test]
    fn a_noiseless_process_interpolates_its_training_points_with_no_variance_left() {
        let m = two_points([1.0, 3.0]);
        for (x, y) in [(0.0, 1.0), (1.0, 3.0)] {
            let (mean, variance) = m.predict_mean_and_variance(&[x]).unwrap();
            assert!((mean - y).abs() < 1e-12, "x = {x}: {mean}");
            assert!(variance < 1e-12, "x = {x}: {variance}");
        }
        // Far from the data the posterior falls back to the prior: the mean and σ_f².
        let (mean, variance) = m.predict_mean_and_variance(&[50.0]).unwrap();
        assert_eq!((mean, variance), (2.0, 1.0));
    }

    #[test]
    fn the_midpoint_matches_the_closed_form() {
        // At x = 1/2 both kernel entries are c = exp(−1/8), so the mean is m + c (w₀ + w₁) and the
        // variance 1 − 2c² (1 − e) / (1 − e²) = 1 − 2c² / (1 + e).
        let m = two_points([1.0, 3.0]);
        let (e, c) = (libm::exp(-0.5), libm::exp(-0.125));
        let (mean, variance) = m.predict_mean_and_variance(&[0.5]).unwrap();
        assert!((mean - 2.0).abs() < 1e-12, "symmetric targets around the mean: {mean}");
        assert!((variance - (1.0 - 2.0 * c * c / (1.0 + e))).abs() < 1e-12, "{variance}");
        assert_eq!(m.predict_one(&[0.5]).unwrap().to_bits(), mean.to_bits());
        assert!(m.predict_one(&[0.5, 1.0]).is_err(), "a row of another width");
    }

    #[test]
    fn the_text_reads_back_bit_for_bit() {
        let m = two_points([-0.25, 7.0]);
        let back = GaussianProcessModel::from_text(&m.to_text()).unwrap();
        assert_eq!(back, m);
        let bits = |v: &GaussianProcessModel| -> Vec<u64> {
            v.precision.iter().chain(&v.weights).map(|x| x.to_bits()).collect()
        };
        assert_eq!(bits(&back), bits(&m));
        let text = m.to_text();
        assert!(
            GaussianProcessModel::from_text(&text.replace("kernel=rbf", "kernel=matern")).is_err()
        );
        assert!(
            GaussianProcessModel::from_text(&text.replace("length_scale=1e0", "length_scale=0e0"))
                .is_err()
        );
        assert!(
            GaussianProcessModel::from_text(&text.replace("training_count=2", "training_count=3"))
                .is_err()
        );
    }
}
