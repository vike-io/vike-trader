# vike-math

Pure numeric formulas for trading and research code: compensated summation, sample statistics, the
normal distribution, hypothesis tests, dense linear algebra, a derivative-free minimiser, stationarity
and change-detection tests, time-series measures, regressions, rolling-window kernels, smoothing
constants, and the GARCH(1,1) and two-state hidden Markov models. Pure Rust, one dependency (`libm`),
no `unsafe`, and the same bits on Windows and Linux.

It is the formula floor under [`vike-indicators`](https://crates.io/crates/vike-indicators): an
indicator there that needs a regression, a variance or a model step calls the one copy here.

```toml
[dependencies]
vike-math = "0.1"
```

## What is in it

Every item is reached through its module (`vike_math::statistics::mean_variance`); nothing is
re-exported at the crate root.

| Module | What it holds |
|---|---|
| `summation` | The compensated (Neumaier) sum and its single step; the ring-buffer and absolute-deviation folds the streaming indicators keep in a fixed order. |
| `powers` | `square`, `cube`, `fourth_power`, and the exact powers of ten `1e-22..=1e22`. |
| `price_transforms` | One bar's average, median and typical prices and weighted close, and the Heikin-Ashi candle's open and range. |
| `statistics` | Mean and variance (with a degrees-of-freedom divisor), covariance, bias-corrected skewness and excess kurtosis, Pearson correlation, doubled average ranks, the linear-interpolation and nearest-rank quantiles, medians, z-scores, sum-to-one normalisation, the correlation distance, equal-width bins. |
| `distributions` | The standard normal CDF (two forms), its quantile, and the normal density; the probability a lognormal spot closes a window above its open. |
| `special_functions` | `logistic`, `logit`, `softmax`. |
| `hypothesis_testing` | The two-sided normal p-value and the Benjamini-Hochberg adjustment. |
| `linear_algebra` | A dense row-major `Matrix`; least squares by Householder QR; Cholesky; the symmetric eigen-decomposition by cyclic Jacobi and the rebuild from its eigenpairs; covariance and correlation matrices; column means; dot products and squared distances. |
| `minimize` | Nelder-Mead. |
| `number_theory` | The greatest common divisor. |
| `stationarity_tests` | The augmented Dickey-Fuller statistic, its supremum over expanding windows, and the KPSS statistic. |
| `time_series` | Autocovariance and autocorrelation, the Bartlett long-run variance, the Newey-West t-statistic, rescaled range, entropy of bins, Lempel-Ziv complexity, fractional-difference weights, the Haar detail energy, the Goertzel power, guarded logarithms, the drawdown fraction from a peak; two series' cross-correlation at every lag with its lead-lag ratio. |
| `change_detection` | The two-sample Kolmogorov-Smirnov test, the population stability index, Page-Hinkley and CUSUM alarms, the symmetric CUSUM filter. |
| `resampling` | The moving-block bootstrap draw. |
| `random` | SplitMix64, the Fisher-Yates shuffle, Box-Muller, the inverse-CDF draw. |
| `regression` | Slope and half-life from moments, the least-squares slope, the slope t-value of an ordinary-least-squares fit, a Kalman-filter regression step. |
| `rolling` | Rolling mean, variance, standard deviation, rank, median and z-score over a `WindowSpec`. |
| `steps` | The decimals that print a step's multiples, the decimals an axis prints its 1/2/5 marks with, and the 1/2/5·10^k "nice" step for a raw one (rounded up or to the nearest). |
| `smoothing` | The exponential-moving-average alpha, the weighted-moving-average normaliser, half-life and exponential decay. |
| `garch` | The GARCH(1,1) variance step and its maximum-likelihood fit. |
| `hidden_markov` | The two-state Gaussian hidden Markov model: its forward step, the stationary law, the Baum-Welch fit and the Viterbi path. |

## Example

```rust
use vike_math::linear_algebra::{least_squares, Matrix};
use vike_math::statistics::mean_variance;

// y = 1 + 2x, fitted by least squares: the design is [1, x].
let design = Matrix::from_rows(&[vec![1.0, 0.0], vec![1.0, 1.0], vec![1.0, 2.0], vec![1.0, 3.0]])
    .expect("rows of one length");
let fit = least_squares(&design, &[1.0, 3.0, 5.0, 7.0]).expect("full rank");
assert!((fit.coefficients[1] - 2.0).abs() < 1e-12);

// The mean and the sample variance (one degree of freedom) of four values.
let (mean, variance) = mean_variance(&[1.0, 2.0, 3.0, 4.0], 1.0);
assert_eq!(mean, 2.5);
assert!((variance - 5.0 / 3.0).abs() < 1e-15);
```

## Determinism

Floating-point folds run in index order with no `mul_add`, and every logarithm, exponential and error
function comes from the `libm` crate rather than the platform's math library, so a value is the same
bits on every operating system. Two summation rules exist on purpose: the compensated sum and the plain
left fold. `None` (or `NaN` where a number is returned) means there is no answer: a shape that does not
fit, a non-finite input, a rank-deficient design.

## Compatibility

This is a `0.x` crate: the API can change between minor versions.

## License

FSL-1.1-ALv2 (Functional Source License, converting to Apache-2.0 two years after each release).
See the `license` field in `Cargo.toml` and the license file in the repository.
