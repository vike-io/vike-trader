//! A trained [`Network`] as ONNX bytes, for a runtime that never links this crate
//! (`vike-ml-onnx-prediction`, which runs it with `tract`).
//!
//! `burn-onnx`'s exporter runs the forward pass ONCE on a capture device, records each tensor
//! operation and writes them as an opset-18 graph with the weights embedded. Its limits are this
//! function's:
//!
//! * **The shape is fixed.** The graph takes exactly `input_shape`, row count included; a live
//!   prediction is one row, so export with `rows = 1` for that. (A dynamic row axis exists upstream,
//!   but it refuses any slice of a tensor along that axis, which the LSTM's step loop and the TCN's
//!   last-step read both are.)
//! * **The output is what the loss trained it to mean**: a network fitted with
//!   [`Loss::BinaryCrossEntropy`] exports its scores through a sigmoid, so the graph answers the
//!   PROBABILITY of the positive class; one fitted with [`Loss::CrossEntropy`] exports through a
//!   softmax per row, so it answers one probability per class; one fitted with [`Loss::Sharpe`]
//!   exports them through a tanh, so it answers the POSITION in `[-1, 1]` the objective trained; one
//!   fitted with [`Loss::MeanSquaredError`] answers its raw value, and one fitted with [`Loss::Hedging`] its raw
//!   hedge. A reader of the graph never has to know which loss produced it. A network fitted with [`Loss::SinhArcsinhFlow`] answers its four
//!   raw parameters here; what a reader wants of a flow is its SAMPLER, [`export_flow_sampler`].
//! * **Upstream marks the exporter experimental**, and an operation it cannot lower is an
//!   `ExportError::UnsupportedOperation` naming it, never a wrong graph. `tests/onnx_export.rs` runs
//!   every network's export through `vike-ml-onnx-prediction` and compares it with [`predict`].
//!
//! [`predict`]: crate::training::predict

use burn::tensor::Tensor;
use burn::tensor::activation::{sigmoid, tanh};
use burn_onnx::export::{ExportError, OnnxExporter};

use crate::device;
use crate::flow::draw_next;
use crate::networks::Network;
use crate::training::Loss;

/// `network`, fitted with `loss`, as ONNX bytes whose one input is a float tensor of exactly
/// `input_shape` and whose one output is `[rows, outputs]`: probabilities for the binary loss, class
/// probabilities for the multiclass one, positions for the Sharpe one, raw values for the squared one
/// (module doc).
pub fn export_onnx<N: Network<D>, const D: usize>(
    network: &N,
    input_shape: [usize; D],
    loss: Loss,
) -> Result<Vec<u8>, ExportError> {
    let sample = Tensor::<D>::zeros(input_shape, &device(false));
    let model = OnnxExporter::new().export(&network.valid(), sample, |network: &N, input| {
        let scores = network.forward(input);
        match loss {
            Loss::BinaryCrossEntropy => sigmoid(scores),
            Loss::Sharpe { .. } => tanh(scores),
            Loss::MeanSquaredError | Loss::SinhArcsinhFlow | Loss::Hedging { .. } => scores,
            Loss::CrossEntropy { .. } => softmax(scores),
        }
    })?;
    Ok(model.into_bytes())
}

/// Each row's softmax, `exp(s - max) / sum(exp(s - max))`, from operations the exporter lowers
/// (`burn`'s own softmax takes a `max_dim`, which it does not; `max_dim_with_indices` is a `TopK`).
fn softmax(scores: Tensor<2>) -> Tensor<2> {
    let (largest, _) = scores.clone().max_dim_with_indices(1);
    let powers = (scores - largest).exp();
    powers.clone() / powers.sum_dim(1)
}

/// `network`, fitted with [`Loss::SinhArcsinhFlow`], as the ONNX bytes of its SAMPLER
/// (`crate::flow`'s "The exported sampler"): the one input is `[rows, columns + 1]`, each row the
/// network's row flat (`row_shape` without its first axis, `columns` numbers) and then one standard
/// normal draw, and the one output, `[rows, 1]`, is a draw of the target. `row_shape`'s first axis is
/// the graph's fixed row count, as in [`export_onnx`].
pub fn export_flow_sampler<N: Network<D>, const D: usize>(
    network: &N,
    row_shape: [usize; D],
) -> Result<Vec<u8>, ExportError> {
    let columns: usize = row_shape[1..].iter().product();
    let sample = Tensor::<2>::zeros([row_shape[0], columns + 1], &device(false));
    let model = OnnxExporter::new().export(&network.valid(), sample, |network: &N, input| {
        draw_next(network, input, row_shape)
    })?;
    Ok(model.into_bytes())
}
