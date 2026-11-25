//! The forward step of a two-state hidden Markov model whose states emit Gaussian log returns: the one
//! copy of the filter that the `hmm_regime` indicator row runs bar by bar and the Baum-Welch fit runs
//! over a whole series, so the row runs exactly the filter the fit estimated.
//!
//! Sources: James D. Hamilton, "A New Approach to the Economic Analysis of Nonstationary Time Series
//! and the Business Cycle", Econometrica 57 (1989) 357-384, <https://doi.org/10.2307/1912559> (the
//! filtered regime probability); Lawrence R. Rabiner, "A Tutorial on Hidden Markov Models and Selected
//! Applications in Speech Recognition", Proceedings of the IEEE 77 (1989) 257-286,
//! <https://doi.org/10.1109/5.18626> (the forward variable, section III.A, and its scaling, section
//! V.A, here normalised on every observation).
//!
//! State `i` emits `N(mean_i, sigma_i^2)` and stays with probability `stay_i`. The densities are taken
//! as logarithms and scaled by the larger before `exp`, so a return far out in both tails still
//! answers. Every logarithm and exponential is `libm`'s, never the platform's, so a value is the same
//! bits on every box (decision 0032).
//!
//! # The fit
//!
//! [`fit`] estimates, by Baum-Welch, the six
//! parameters the `hmm_regime` indicator row takes (`vike_indicators::indicators::HmmRegime`), and
//! [`viterbi`] decodes the most likely state path. A study fits on history here and hands the numbers
//! to the row, which runs the same forward filter in a backtest and live.
//!
//! Sources: Lawrence R. Rabiner, "A Tutorial on Hidden Markov Models and Selected Applications in
//! Speech Recognition", Proceedings of the IEEE 77 (1989) 257-286, <https://doi.org/10.1109/5.18626>
//! (the forward-backward procedure and its scaling, sections III.A and V.A; the re-estimation formulas
//! of section III.C with Gaussian emissions; the Viterbi algorithm, section III.B); L. E. Baum,
//! T. Petrie, G. Soules and N. Weiss, "A Maximization Technique Occurring in the Statistical Analysis
//! of Probabilistic Functions of Markov Chains", The Annals of Mathematical Statistics 41 (1970)
//! 164-171, <https://doi.org/10.1214/aoms/1177697196>.
//!
//! The model is the row's: state `i` emits a log return `N(mean_i, sigma_i^2)` and stays with
//! probability `stay_i`, and the chain starts at its stationary law (one half each when both stays
//! are 1), so the start is not a free parameter: the M-step re-estimates the means, the deviations and
//! the stays, and the start follows the stays. The forward step and the stationary law are
//! this module's, the one copy the row's `HmmCore` runs too, so [`HmmParameters::filtered`] is
//! the row's values bit for bit.
//!
//! After a fit, state 0 is the state with the smaller sigma (the calm one, as the row's defaults read
//! it), so two fits of one series name their states alike. Each sigma is held to the row's range
//! `[0.000001, 1]` and each mean to `[-1, 1]`, so a state that collapses onto one return cannot drive
//! its density to infinity. Every logarithm and exponential is `libm`'s. Returns must be finite: the
//! row skips a bar with no return, which is the same as leaving that return out of the series here.

use crate::statistics::mean_variance;

/// The chain's stationary probability of state 1, `(1 - stay_0) / (2 - stay_0 - stay_1)`: the belief
/// before the first return. One half when neither state is ever left (no stationary law singles one
/// out).
#[inline]
pub fn stationary(stay: [f64; 2]) -> f64 {
    let (leave_0, leave_1) = (1.0 - stay[0], 1.0 - stay[1]);
    if leave_0 + leave_1 > 0.0 { leave_0 / (leave_0 + leave_1) } else { 0.5 }
}

/// A model's six numbers in the form the step reads them: each state's mean, `ln(sigma)` (the
/// density's normalising term, up to the `sqrt(2 pi)` both share), `1 / (2 sigma^2)` and stay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoStateGaussian {
    mean: [f64; 2],
    log_sigma: [f64; 2],
    inverse_twice_variance: [f64; 2],
    stay: [f64; 2],
}

/// One return's step of the filter, with what a backward pass reuses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForwardStep {
    /// Each state's density of the return divided by the larger one's: `exp(l_i - top)`.
    pub scaled_density: [f64; 2],
    /// The larger log density (without the shared `-ln sqrt(2 pi)`): the scale of `scaled_density`.
    pub top: f64,
    /// `sum over states of predicted * scaled_density`: the scaling of this return. Zero only when the
    /// likelier state was already ruled out and the other's weight underflowed.
    pub total: f64,
    /// The filtered probability of state 1 after this return; the prediction itself when `total` is
    /// not above zero (a certain belief, which one return cannot move).
    pub filtered: f64,
}

impl TwoStateGaussian {
    /// State `i` emits `N(mean[i], sigma[i]^2)` and stays with probability `stay[i]`.
    pub fn new(mean: [f64; 2], sigma: [f64; 2], stay: [f64; 2]) -> Self {
        Self {
            mean,
            log_sigma: [libm::log(sigma[0]), libm::log(sigma[1])],
            inverse_twice_variance: [0.5 / (sigma[0] * sigma[0]), 0.5 / (sigma[1] * sigma[1])],
            stay,
        }
    }

    /// State `i`'s Gaussian log-density of the return `r`, without the `-ln sqrt(2 pi)` both states share:
    /// `-ln(sigma_i) - (r - mean_i)^2 / (2 sigma_i^2)`, as `-ln(sigma_i) - d * d * (0.5 / sigma_i^2)`. The one
    /// emission density: [`TwoStateGaussian::step`] weighs by it and [`viterbi`] scores with it.
    #[inline]
    pub fn log_density(&self, i: usize, r: f64) -> f64 {
        let d = r - self.mean[i];
        -self.log_sigma[i] - d * d * self.inverse_twice_variance[i]
    }

    /// The filter's step on return `r` from the belief `probability` in state 1: predict one step of
    /// the chain, `q1 = (1 - p) (1 - stay_0) + p stay_1`, then weigh by each state's density and
    /// normalise, `p' = q1 f1 / (q0 f0 + q1 f1)`.
    #[inline]
    pub fn step(&self, probability: f64, r: f64) -> ForwardStep {
        let p = probability;
        let predicted_1 = (1.0 - p) * (1.0 - self.stay[0]) + p * self.stay[1];
        let predicted = [1.0 - predicted_1, predicted_1];
        let (l0, l1) = (self.log_density(0, r), self.log_density(1, r));
        let top = l0.max(l1);
        let scaled_density = [libm::exp(l0 - top), libm::exp(l1 - top)];
        let weight_0 = predicted[0] * scaled_density[0];
        let weight_1 = predicted[1] * scaled_density[1];
        let total = weight_0 + weight_1;
        let filtered = if total > 0.0 { weight_1 / total } else { predicted_1 };
        ForwardStep { scaled_density, top, total, filtered }
    }
}

/// The row's smallest sigma.
const SIGMA_FLOOR: f64 = 0.000001;
/// Baum-Welch stops when the log-likelihood moves by less than this, relative to `1 + |ll|`.
const TOLERANCE: f64 = 1e-10;
/// Baum-Welch's budget.
const MAX_ITERATIONS: usize = 1_000;

/// The six numbers of a two-state Gaussian HMM, by state; the `hmm_regime` row takes them as
/// `mean[0], sigma[0], stay[0], mean[1], sigma[1], stay[1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HmmParameters {
    pub mean: [f64; 2],
    pub sigma: [f64; 2],
    pub stay: [f64; 2],
}

/// The belief in each state after a step (the scaled forward variable).
fn belief(step: &ForwardStep) -> [f64; 2] {
    [1.0 - step.filtered, step.filtered]
}

impl HmmParameters {
    /// The chain's stationary probability of state 1: the belief before the first return.
    pub fn stationary(&self) -> f64 {
        stationary(self.stay)
    }

    /// The forward filter over `returns` (the row's step, [`TwoStateGaussian::step`]), `None` when a bar
    /// leaves no weight on either state.
    fn forward(&self, returns: &[f64]) -> Option<Vec<ForwardStep>> {
        let model = TwoStateGaussian::new(self.mean, self.sigma, self.stay);
        let mut p = self.stationary();
        let mut steps = Vec::with_capacity(returns.len());
        for &r in returns {
            let step = model.step(p, r);
            if !(step.total > 0.0 && step.total.is_finite()) {
                return None;
            }
            p = step.filtered;
            steps.push(step);
        }
        Some(steps)
    }

    /// The filtered probability of state 1 after each return: the `hmm_regime` row's value on the bar
    /// of that return. `None` when a return is not finite or a bar leaves no weight on either state.
    pub fn filtered(&self, returns: &[f64]) -> Option<Vec<f64>> {
        if returns.iter().any(|r| !r.is_finite()) {
            return None;
        }
        Some(self.forward(returns)?.iter().map(|s| s.filtered).collect())
    }

    /// The log-likelihood of `returns` (the Gaussian densities' `ln sqrt(2 pi)` included); `-inf`
    /// when a bar leaves no weight on either state, `NaN` when a return is not finite.
    pub fn log_likelihood(&self, returns: &[f64]) -> f64 {
        if returns.iter().any(|r| !r.is_finite()) {
            return f64::NAN;
        }
        match self.forward(returns) {
            Some(steps) => log_likelihood_of(&steps),
            None => f64::NEG_INFINITY,
        }
    }

    /// A deterministic start for [`fit`]: both means the sample mean, the sigmas half and twice the
    /// population deviation ([`mean_variance`], compensated), both stays 0.9. `None` for no returns or a return that is not finite.
    pub fn starting(returns: &[f64]) -> Option<Self> {
        if returns.is_empty() || returns.iter().any(|r| !r.is_finite()) {
            return None;
        }
        let (mean, variance) = mean_variance(returns.iter().copied(), 0.0);
        let deviation = variance.sqrt();
        let sigma = |k: f64| (k * deviation).clamp(SIGMA_FLOOR, 1.0);
        Some(Self { mean: [mean; 2], sigma: [sigma(0.5), sigma(2.0)], stay: [0.9; 2] })
    }
}

fn log_likelihood_of(steps: &[ForwardStep]) -> f64 {
    let half_log_two_pi = 0.5 * libm::log(std::f64::consts::TAU);
    steps.iter().fold(0.0, |s, step| s + libm::log(step.total) + step.top - half_log_two_pi)
}

/// What [`fit`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct HmmFit {
    pub parameters: HmmParameters,
    /// The log-likelihood at `parameters`.
    pub log_likelihood: f64,
    /// Baum-Welch iterations run.
    pub iterations: usize,
    /// Whether the log-likelihood settled before the budget ran out.
    pub converged: bool,
}

/// [`fit_from`] from [`HmmParameters::starting`].
pub fn fit(returns: &[f64]) -> Option<HmmFit> {
    fit_from(returns, HmmParameters::starting(returns)?)
}

/// Fit the model to `returns` by Baum-Welch from `start`. `None` for fewer than 2 returns, a return
/// that is not finite, or a start under which a bar leaves no weight on either state.
pub fn fit_from(returns: &[f64], start: HmmParameters) -> Option<HmmFit> {
    if returns.len() < 2 || returns.iter().any(|r| !r.is_finite()) {
        return None;
    }
    let mut parameters = start;
    let mut steps = parameters.forward(returns)?;
    let mut log_likelihood = log_likelihood_of(&steps);
    let mut iterations = 0;
    let mut converged = false;
    while iterations < MAX_ITERATIONS {
        let next = maximisation(&parameters, returns, &steps);
        let Some(next_steps) = next.forward(returns) else {
            break;
        };
        let next_log_likelihood = log_likelihood_of(&next_steps);
        iterations += 1;
        let moved = (next_log_likelihood - log_likelihood).abs();
        (parameters, steps, log_likelihood) = (next, next_steps, next_log_likelihood);
        if moved <= TOLERANCE * (1.0 + log_likelihood.abs()) {
            converged = true;
            break;
        }
    }
    if parameters.sigma[0] > parameters.sigma[1] {
        // Swapping the labels moves nothing else: the stationary law and the filter are symmetric.
        parameters = HmmParameters {
            mean: [parameters.mean[1], parameters.mean[0]],
            sigma: [parameters.sigma[1], parameters.sigma[0]],
            stay: [parameters.stay[1], parameters.stay[0]],
        };
        log_likelihood = parameters.log_likelihood(returns);
    }
    Some(HmmFit { parameters, log_likelihood, iterations, converged })
}

/// One Baum-Welch re-estimation from the forward pass `steps` (Rabiner's scaled backward variable,
/// the state and transition posteriors, then the weighted means, deviations and stays).
fn maximisation(
    parameters: &HmmParameters,
    returns: &[f64],
    steps: &[ForwardStep],
) -> HmmParameters {
    let stay = parameters.stay;
    let transition = [[stay[0], 1.0 - stay[0]], [1.0 - stay[1], stay[1]]];
    let n = returns.len();
    // `posterior[t]` is gamma_t; `stayed` and `left_from` sum xi_t(i, i) and gamma_t(i) over t < n - 1.
    let mut posterior = vec![[0.0; 2]; n];
    let mut stayed = [0.0; 2];
    let mut left_from = [0.0; 2];
    let mut backward = [1.0, 1.0];
    posterior[n - 1] = belief(&steps[n - 1]);
    for t in (0..n - 1).rev() {
        let next = &steps[t + 1];
        let weighted = [
            next.scaled_density[0] * backward[0] / next.total,
            next.scaled_density[1] * backward[1] / next.total,
        ];
        let filtered = belief(&steps[t]);
        for i in 0..2 {
            stayed[i] += filtered[i] * transition[i][i] * weighted[i];
        }
        backward = [
            transition[0][0] * weighted[0] + transition[0][1] * weighted[1],
            transition[1][0] * weighted[0] + transition[1][1] * weighted[1],
        ];
        let gamma = [filtered[0] * backward[0], filtered[1] * backward[1]];
        let sum = gamma[0] + gamma[1];
        posterior[t] = [gamma[0] / sum, gamma[1] / sum];
        for i in 0..2 {
            left_from[i] += posterior[t][i];
        }
    }
    let mut next = *parameters;
    for i in 0..2 {
        let weight = posterior.iter().fold(0.0, |s, g| s + g[i]);
        if weight.is_nan() || weight <= 0.0 {
            // The state is never visited: nothing re-estimates it, so it keeps its numbers.
            continue;
        }
        let mean = posterior.iter().zip(returns).fold(0.0, |s, (g, &r)| s + g[i] * r) / weight;
        let variance = posterior
            .iter()
            .zip(returns)
            .fold(0.0, |s, (g, &r)| s + g[i] * (r - mean) * (r - mean))
            / weight;
        next.mean[i] = mean.clamp(-1.0, 1.0);
        next.sigma[i] = variance.sqrt().clamp(SIGMA_FLOOR, 1.0);
        if left_from[i] > 0.0 {
            next.stay[i] = (stayed[i] / left_from[i]).clamp(0.0, 1.0);
        }
    }
    next
}

/// The most likely state of each return (0 or 1) under `parameters`, by Viterbi in logarithms from
/// the chain's stationary law; a tie goes to state 0. `None` when a return is not finite.
pub fn viterbi(parameters: &HmmParameters, returns: &[f64]) -> Option<Vec<usize>> {
    if returns.iter().any(|r| !r.is_finite()) {
        return None;
    }
    let HmmParameters { mean, sigma, stay } = *parameters;
    let model = TwoStateGaussian::new(mean, sigma, stay);
    let log_transition = [
        [libm::log(stay[0]), libm::log(1.0 - stay[0])],
        [libm::log(1.0 - stay[1]), libm::log(stay[1])],
    ];
    let start = parameters.stationary();
    let mut score = [libm::log(1.0 - start), libm::log(start)];
    let mut came_from: Vec<[usize; 2]> = Vec::with_capacity(returns.len());
    for &r in returns {
        let mut next = [0.0; 2];
        let mut from = [0; 2];
        for j in 0..2 {
            let (via_0, via_1) = (score[0] + log_transition[0][j], score[1] + log_transition[1][j]);
            from[j] = usize::from(via_1 > via_0);
            next[j] = via_0.max(via_1) + model.log_density(j, r);
        }
        score = next;
        came_from.push(from);
    }
    let mut state = usize::from(score[1] > score[0]);
    let mut path = vec![0; returns.len()];
    for t in (0..returns.len()).rev() {
        path[t] = state;
        state = came_from[t][state];
    }
    Some(path)
}

#[cfg(test)]
#[path = "hidden_markov_tests.rs"]
mod tests;
