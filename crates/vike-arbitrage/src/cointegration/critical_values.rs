//! The critical values of the two cointegration tests, from the published response surfaces. Nothing
//! here is simulated: the numbers are the authors' tables, copied, and the formulas are theirs.
//!
//! **Engle–Granger** ([`engle_granger_critical_values`]): MacKinnon, "Critical values for
//! cointegration tests" (Queen's University Working Paper 1227, 1990, reissued 2010), the appendix's
//! Table 2, the `c` rows: the cointegrating regression has an intercept and the Dickey–Fuller regression
//! on its residual has none. A critical value for `T` observations is `β∞ + β₁/T + β₂/T² + β₃/T³`
//! (the paper's equation A.1), the `β`s printed per number of variables `N = 2 ..= 12` and per level;
//! a `β₃` the paper omitted is `0`. `T` is the number of observations the Dickey–Fuller regression
//! used, i.e. the residual's length less one less its lags. The paper says its formula is strictly
//! valid for independent normal errors; with serially correlated ones the test is still run with lags
//! and these values are the standard reference.
//!
//! **Johansen** ([`johansen_critical_values`]): MacKinnon, Haug and Michelis, "Numerical Distribution
//! Functions of Likelihood Ratio Tests for Cointegration" (Journal of Applied Econometrics 14, 1999;
//! the September 1996 working paper's Tables 1–3), the asymptotic quantiles of the trace and
//! maximal-eigenvalue statistics for the three cases this crate runs, by `p − r` (the number of
//! variables less the rank under the null) from 1 to 12. They are ASYMPTOTIC: in a short sample the
//! tests over-reject, and the statistic in this crate is not corrected for it.

use super::JohansenCase;

/// A level at which a test's null (no cointegration) may be rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Significance {
    OnePercent,
    FivePercent,
    TenPercent,
}

/// The critical values of one test at the three levels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CriticalValues {
    pub one_percent: f64,
    pub five_percent: f64,
    pub ten_percent: f64,
}

impl CriticalValues {
    /// The value at `level`.
    pub fn at(&self, level: Significance) -> f64 {
        match level {
            Significance::OnePercent => self.one_percent,
            Significance::FivePercent => self.five_percent,
            Significance::TenPercent => self.ten_percent,
        }
    }
}

/// The two Johansen statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JohansenStatistic {
    /// `−T Σ_{i>r} ln(1 − λ_i)`: rank at most `r` against full rank.
    Trace,
    /// `−T ln(1 − λ_{r+1})`: rank `r` against rank `r + 1`.
    MaximumEigenvalue,
}

/// `[β∞, β₁, β₂, β₃]` at 1%, 5% and 10% for `N = 2 ..= 12` variables (index `N − 2`).
const ENGLE_GRANGER_CONSTANT: [[[f64; 4]; 3]; 11] = [
    [
        [-3.89644, -10.9519, -22.527, 0.0],
        [-3.33613, -6.1101, -6.823, 0.0],
        [-3.04445, -4.2412, -2.720, 0.0],
    ],
    [
        [-4.29374, -14.4354, -33.195, 47.433],
        [-3.74066, -8.5631, -10.852, 27.982],
        [-3.45218, -6.2143, -3.718, 0.0],
    ],
    [
        [-4.64332, -18.1031, -37.972, 0.0],
        [-4.09600, -11.2349, -11.175, 0.0],
        [-3.81020, -8.3931, -4.137, 0.0],
    ],
    [
        [-4.95756, -21.8883, -45.142, 0.0],
        [-4.41519, -14.0406, -12.575, 0.0],
        [-4.13157, -10.7417, -3.784, 0.0],
    ],
    [
        [-5.24568, -25.6688, -57.737, 88.639],
        [-4.70693, -16.9178, -17.492, 60.007],
        [-4.42501, -13.1875, -5.104, 27.877],
    ],
    [
        [-5.51233, -29.5760, -69.398, 164.295],
        [-4.97684, -19.9021, -22.045, 110.761],
        [-4.69648, -15.7315, -6.922, 67.721],
    ],
    [
        [-5.76202, -33.5258, -82.189, 256.289],
        [-5.22924, -23.0023, -24.646, 144.479],
        [-4.95007, -18.3959, -7.344, 94.872],
    ],
    [
        [-5.99742, -37.6572, -87.365, 248.316],
        [-5.46697, -26.2057, -26.627, 176.382],
        [-5.18897, -21.1377, -9.484, 172.704],
    ],
    [
        [-6.22103, -41.7154, -102.680, 389.330],
        [-5.69244, -29.4521, -30.994, 251.016],
        [-5.41533, -24.0006, -7.514, 163.049],
    ],
    [
        [-6.43377, -46.0084, -106.809, 352.752],
        [-5.90714, -32.8336, -30.275, 249.994],
        [-5.63086, -26.9693, -4.083, 151.427],
    ],
    [
        [-6.63790, -50.2095, -124.156, 579.622],
        [-6.11279, -36.2681, -32.505, 314.802],
        [-5.83724, -29.9864, -2.686, 184.116],
    ],
];

/// The Engle–Granger critical values for `variables` series (the dependent one and its regressors,
/// `2 ..= 12`) and `observations` in the Dickey–Fuller regression. The test rejects no cointegration
/// when its statistic is BELOW them.
///
/// `None` outside the table's variable counts or with no observation.
pub fn engle_granger_critical_values(
    variables: usize,
    observations: usize,
) -> Option<CriticalValues> {
    if !(2..=12).contains(&variables) || observations == 0 {
        return None;
    }
    let t = observations as f64;
    let at = |level: usize| {
        let b = ENGLE_GRANGER_CONSTANT[variables - 2][level];
        b[0] + b[1] / t + b[2] / (t * t) + b[3] / (t * t * t)
    };
    Some(CriticalValues { one_percent: at(0), five_percent: at(1), ten_percent: at(2) })
}

/// One row per `p − r = 1 ..= 12`: the maximal-eigenvalue quantiles at 1%, 2%, 5%, 10%, 20%, then the
/// trace quantiles at the same five levels, as the paper prints them (Table 1, Case 0).
const JOHANSEN_NO_CONSTANT: [[f64; 10]; 12] = [
    [6.94, 5.71, 4.13, 2.98, 1.88, 6.94, 5.71, 4.13, 2.98, 1.89],
    [15.09, 13.45, 11.22, 9.47, 7.62, 16.36, 14.65, 12.32, 10.47, 8.51],
    [22.25, 20.38, 17.80, 15.72, 13.45, 29.51, 27.33, 24.28, 21.78, 19.01],
    [29.06, 27.02, 24.16, 21.84, 19.28, 46.57, 43.92, 40.17, 37.03, 33.49],
    [35.72, 33.52, 30.44, 27.92, 25.10, 67.64, 64.53, 60.06, 56.28, 51.96],
    [42.23, 39.90, 36.63, 33.93, 30.91, 92.71, 89.13, 83.94, 79.53, 74.44],
    [48.66, 46.22, 42.77, 39.91, 36.70, 121.74, 117.70, 111.78, 106.74, 100.88],
    [55.04, 52.48, 48.88, 45.89, 42.52, 154.80, 150.27, 143.67, 138.00, 131.38],
    [61.35, 58.71, 54.97, 51.85, 48.32, 191.83, 186.81, 179.52, 173.23, 165.84],
    [67.65, 64.92, 61.03, 57.80, 54.14, 232.84, 227.41, 219.41, 212.47, 204.30],
    [73.89, 71.07, 67.08, 63.73, 59.94, 278.00, 272.01, 263.26, 255.68, 246.78],
    [80.12, 77.22, 73.09, 69.65, 65.73, 326.96, 320.57, 311.13, 302.90, 293.21],
];

/// Table 2, Case 1*: a constant restricted to the cointegrating relations.
const JOHANSEN_RESTRICTED_CONSTANT: [[f64; 10]; 12] = [
    [12.76, 11.23, 9.16, 7.56, 5.88, 12.76, 11.23, 9.16, 7.56, 5.88],
    [20.16, 18.37, 15.89, 13.91, 11.77, 25.08, 23.07, 20.26, 17.98, 15.48],
    [27.07, 25.07, 22.30, 20.05, 17.59, 41.20, 38.72, 35.19, 32.27, 28.99],
    [33.73, 31.59, 28.59, 26.12, 23.40, 61.27, 58.31, 54.08, 50.53, 46.46],
    [40.29, 38.01, 34.81, 32.17, 29.21, 85.34, 81.90, 76.97, 72.77, 67.94],
    [46.75, 44.35, 40.96, 38.16, 35.02, 113.42, 109.51, 103.84, 99.02, 93.42],
    [53.12, 50.61, 47.07, 44.13, 40.81, 145.40, 141.03, 134.68, 129.23, 122.85],
    [59.51, 56.89, 53.19, 50.11, 46.64, 181.51, 176.70, 169.61, 163.50, 156.36],
    [65.79, 63.07, 59.24, 56.05, 52.44, 221.45, 216.17, 208.45, 201.69, 193.80],
    [72.10, 69.26, 65.30, 61.99, 58.25, 265.53, 259.75, 251.27, 243.96, 235.28],
    [78.29, 75.42, 71.33, 67.93, 64.05, 313.75, 307.37, 298.17, 290.17, 280.73],
    [84.51, 81.56, 77.38, 73.85, 69.86, 365.64, 358.96, 348.99, 340.38, 330.19],
];

/// Table 3, Case 1: an unrestricted constant.
const JOHANSEN_UNRESTRICTED_CONSTANT: [[f64; 10]; 12] = [
    [6.63, 5.41, 3.84, 2.71, 1.64, 6.63, 5.41, 3.84, 2.71, 1.64],
    [18.52, 16.72, 14.26, 12.30, 10.19, 19.94, 18.07, 15.50, 13.43, 11.19],
    [25.86, 23.88, 21.13, 18.89, 16.44, 35.46, 33.12, 29.80, 27.07, 24.01],
    [32.71, 30.58, 27.58, 25.12, 22.41, 54.68, 51.87, 47.86, 44.49, 40.65],
    [39.37, 37.08, 33.88, 31.24, 28.30, 77.82, 74.54, 69.82, 65.82, 61.21],
    [45.87, 43.47, 40.08, 37.28, 34.15, 104.96, 101.20, 95.75, 91.11, 85.74],
    [52.31, 49.78, 46.23, 43.29, 39.98, 135.97, 131.76, 125.61, 120.37, 114.23],
    [58.67, 56.06, 52.36, 49.29, 45.82, 171.09, 166.38, 159.53, 153.63, 146.74],
    [64.99, 62.28, 58.43, 55.24, 51.63, 210.06, 204.93, 197.37, 190.88, 183.24],
    [71.26, 68.46, 64.51, 61.20, 57.45, 253.24, 247.54, 239.25, 232.11, 223.72],
    [77.49, 74.63, 70.53, 67.13, 63.26, 300.29, 294.13, 285.14, 277.38, 268.17],
    [83.70, 80.79, 76.58, 73.06, 69.08, 351.25, 344.66, 334.98, 326.53, 316.63],
];

/// The asymptotic Johansen critical values of `statistic` in `case` when `dimension = p − r` variables
/// remain under the null (`1 ..= 12`). The test rejects the null when the statistic is ABOVE them.
///
/// `None` outside the table's dimensions.
pub fn johansen_critical_values(
    case: JohansenCase,
    statistic: JohansenStatistic,
    dimension: usize,
) -> Option<CriticalValues> {
    if !(1..=12).contains(&dimension) {
        return None;
    }
    let table = match case {
        JohansenCase::NoConstant => &JOHANSEN_NO_CONSTANT,
        JohansenCase::RestrictedConstant => &JOHANSEN_RESTRICTED_CONSTANT,
        JohansenCase::UnrestrictedConstant => &JOHANSEN_UNRESTRICTED_CONSTANT,
    };
    let row = &table[dimension - 1];
    // Columns 0..5 are the maximal-eigenvalue quantiles, 5..10 the trace's: 1%, 2%, 5%, 10%, 20%.
    let first = match statistic {
        JohansenStatistic::MaximumEigenvalue => 0,
        JohansenStatistic::Trace => 5,
    };
    Some(CriticalValues {
        one_percent: row[first],
        five_percent: row[first + 2],
        ten_percent: row[first + 3],
    })
}
