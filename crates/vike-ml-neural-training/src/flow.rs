//! The conditional NORMALISING FLOW a network learns under [`Loss::SinhArcsinhFlow`]: the law of the
//! next value given a row (a window of returns, say) as ONE sinh-arcsinh layer (M. C. Jones and A.
//! Pewsey, "Sinh-arcsinh distributions", Biometrika 96(4), 2009) whose four parameters the network
//! computes from the row. Sampled step by step it is a GENERATOR of paths
//! (`vike_analytics::synthetic::learned_bars`).
//!
//! # The law
//!
//! The network's four scores per row, `(a, b, c, d)`, become four parameters, each bounded by a scaled
//! tanh so none can run away while the network is untrained:
//! - the location `mu = a`;
//! - the scale `sigma = exp(4 tanh(b / 4))`;
//! - the skew `epsilon = 2 tanh(c / 2)`;
//! - the tail weight `delta = exp(tanh(d))`: below 1 heavier tails than a normal law, above 1 lighter.
//!
//! A standard normal draw `z` becomes the value `y = mu + sigma * sinh((asinh(z) + epsilon) / delta)`
//! ([`sample`], the generating direction). The inverse is closed form too: with
//! `u = (y - mu) / sigma`, `z = sinh(delta * asinh(u) - epsilon)`, so the log-likelihood is exact,
//! `ln phi(z) + ln delta + ln(1 + z^2) / 2 - ln(1 + u^2) / 2 - ln sigma`, and its negative averaged over
//! a batch is the loss ([`negative_log_likelihood`]). Four zero scores are the standard normal law,
//! which is where an untrained network starts: hand it STANDARDISED targets (the caller's frame,
//! `vike_analytics::synthetic::next_step_rows`).
//!
//! # Why a flow, not a GAN
//!
//! A flow is fitted by maximum likelihood: one network, one optimiser and a loss that falls as the
//! model improves and compares across seeds and architectures. A GAN (TimeGAN's four networks
//! included) is a two-player game with no such loss, prone to mode collapse, and its convergence is
//! judged by eye. The flow's sampler is the same network with the draw added, exported once.
//!
//! # The two directions are written apart
//!
//! Training uses `burn`'s own `asinh` and `sinh`, accurate on any input. The SAMPLER is exported to
//! ONNX, and the exporter lowers neither (burn-onnx 0.22 lowers `Exp`, `Log`, `Sqrt`, `Tanh` and the
//! arithmetic), so [`sample`] spells them in those: `asinh(z) = ln(z + sqrt(z^2 + 1))` and
//! `sinh(t) = (e^t - e^-t) / 2`. shortcut: that `asinh` loses relative precision for a large NEGATIVE
//! argument (about `1e-5` at `z = -9` in `f32`), harmless for a normal draw; use the odd-symmetric form
//! when the exporter lowers `Sign`.
//!
//! # The exported sampler
//!
//! `crate::onnx::export_flow_sampler` writes the network and [`sample`] as ONE graph whose input row is
//! the network's row, flat, and then ONE MORE column, the draw; it answers `y`. A held model then
//! generates a path with nothing but a normal draw per step. [`sample_flow`] is the same computation in
//! `burn`, for a check.

use burn::tensor::Tensor;
use burn::tensor::activation::tanh;

use crate::InputError;
use crate::networks::Network;
#[cfg(doc)]
use crate::training::Loss;
use crate::training::run;

/// The scores per row a flow's network computes: its four parameters before their bounds.
pub const FLOW_PARAMETERS: usize = 4;

/// `ln sigma` stays within plus or minus this (`sigma` from about 0.018 to 55 standardised units).
const LOG_SCALE_BOUND: f32 = 4.0;
/// `epsilon` stays within plus or minus this.
const SKEW_BOUND: f32 = 2.0;
/// `ln delta` stays within plus or minus this (`delta` from about 0.37 to 2.7).
const LOG_TAIL_BOUND: f32 = 1.0;
/// `ln(2 pi) / 2`.
const HALF_LOG_TWO_PI: f32 = 0.918_938_5;

/// The four parameters of each row's law, `[rows, 1]` each, the scale and the tail weight as logs.
struct FlowParameters {
    location: Tensor<2>,
    log_scale: Tensor<2>,
    skew: Tensor<2>,
    log_tail: Tensor<2>,
}

/// `scores` (`[rows, 4]`) as the law's parameters (the module doc's bounds).
fn parameters(scores: Tensor<2>) -> FlowParameters {
    let [_, width] = scores.dims();
    assert_eq!(
        width, FLOW_PARAMETERS,
        "a flow's network computes {FLOW_PARAMETERS} scores per row"
    );
    let column = |index: usize| scores.clone().narrow(1, index, 1);
    let bounded = |x: Tensor<2>, bound: f32| tanh(x.div_scalar(bound)).mul_scalar(bound);
    FlowParameters {
        location: column(0),
        log_scale: bounded(column(1), LOG_SCALE_BOUND),
        skew: bounded(column(2), SKEW_BOUND),
        log_tail: bounded(column(3), LOG_TAIL_BOUND),
    }
}

/// The mean negative log-likelihood of `targets` (`[rows, 1]`) under the laws `scores` (`[rows, 4]`)
/// parameterise: the training loss (module doc).
pub fn negative_log_likelihood(scores: Tensor<2>, targets: Tensor<2>) -> Tensor<1> {
    let law = parameters(scores);
    let u = (targets - law.location) / law.log_scale.clone().exp();
    let z = (law.log_tail.clone().exp() * u.clone().asinh() - law.skew).sinh();
    let per_row = law.log_scale - law.log_tail - (z.clone() * z.clone()).log1p().mul_scalar(0.5)
        + (u.clone() * u).log1p().mul_scalar(0.5)
        + (z.clone() * z).mul_scalar(0.5).add_scalar(HALF_LOG_TWO_PI);
    per_row.mean()
}

/// One value per row drawn from the laws `scores` (`[rows, 4]`) parameterise, the row's standard normal
/// draw in `draws` (`[rows, 1]`): the generating direction, in the operations the ONNX exporter
/// lowers (module doc).
pub fn sample(scores: Tensor<2>, draws: Tensor<2>) -> Tensor<2> {
    let law = parameters(scores);
    let asinh = (draws.clone() + (draws.clone() * draws).add_scalar(1.0).sqrt()).log();
    let t = (asinh + law.skew) / law.log_tail.exp();
    let sinh = (t.clone().exp() - t.neg().exp()).mul_scalar(0.5);
    law.location + law.log_scale.exp() * sinh
}

/// The sampler's forward pass: `input` is `[rows, columns + 1]`, each row the network's row flat and
/// then its draw. The row is read in `row_shape` (the network's input shape; its first axis, the row
/// count, is taken from `input`). Answers `[rows, 1]`.
pub(crate) fn draw_next<N: Network<D>, const D: usize>(
    network: &N,
    input: Tensor<2>,
    row_shape: [usize; D],
) -> Tensor<2> {
    let [rows, width] = input.dims();
    let mut shape = row_shape;
    shape[0] = rows;
    let window = input.clone().narrow(1, 0, width - 1).reshape(shape);
    sample(network.forward(window), input.narrow(1, width - 1, 1))
}

/// One draw per row of `inputs` from a network trained under [`Loss::SinhArcsinhFlow`]: each row is
/// the network's row flat (`row_shape` without its first axis) and then one standard normal draw;
/// row-major `[rows, 1]` back. The exported sampler's computation, run in `burn` without gradients.
pub fn sample_flow<N: Network<D>, const D: usize>(
    network: &N,
    inputs: &[f32],
    row_shape: [usize; D],
) -> Result<Vec<f32>, InputError> {
    let columns: usize = row_shape[1..].iter().product::<usize>() + 1;
    if columns == 1 || !inputs.len().is_multiple_of(columns) || inputs.is_empty() {
        return Err(InputError(format!(
            "{} values are not rows of {columns} (the network's row and a draw)",
            inputs.len()
        )));
    }
    let rows = inputs.len() / columns;
    run(network, inputs, [rows, columns], |network, input| draw_next(network, input, row_shape))
}
