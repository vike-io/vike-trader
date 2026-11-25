//! The five networks. Each is a `burn` module built from its config by `init(seed)` (the seed fixes
//! its initial weights, crate doc) and each is a [`Network`]: one forward pass from a batch, rows
//! first, to `[rows, outputs]` raw scores.
//!
//! | network | input shape | what it is |
//! |---|---|---|
//! | [`Mlp`] | `[rows, inputs]` | fully connected layers, ReLU between them |
//! | [`LstmNetwork`] | `[rows, steps, inputs]` | an LSTM step (written here, so it exports) over the window, its last hidden state into a linear head |
//! | [`Tcn`] | `[rows, inputs, steps]` | causal dilated 1-D convolutions (dilation 1, 2, 4, ...) with residuals, the last step into a linear head |
//! | [`Autoencoder`] | `[rows, inputs]` | an encoder to a narrow linear code and a mirrored decoder back; [`Autoencoder::encode`] is the code |
//! | [`DeepLob`] | `[rows, 1, steps, 4 * levels]` | DeepLOB's convolution blocks and inception module over a window of order-book rows, then the LSTM above |
//!
//! ⚠ The TCN's input is CHANNELS FIRST (`[rows, inputs, steps]`, a 1-D convolution's own layout) while
//! the LSTM's is steps first: the exporter lowers no transpose, so the layout is the caller's, never a
//! `swap_dims` inside a forward pass. DeepLOB's window is one image channel whose rows are the steps,
//! so a step-major flat row IS its layout.

use burn::module::Module;
use burn::nn::conv::{Conv1d, Conv1dConfig, Conv2d, Conv2dConfig};
use burn::nn::pool::{MaxPool2d, MaxPool2dConfig};
use burn::nn::{Linear, LinearConfig, PaddingConfig1d, PaddingConfig2d};
use burn::tensor::activation::{relu, sigmoid, tanh};
use burn::tensor::{Device, Tensor};

use crate::seeded;

/// A network the trainer can fit and the exporter can write: one forward pass from a rank-`D`
/// batch whose first axis is the row to `[rows, outputs]` raw scores, with no output activation.
pub trait Network<const D: usize>: Module {
    /// The forward pass.
    fn forward(&self, input: Tensor<D>) -> Tensor<2>;
}

/// A multilayer perceptron: `inputs`, then each `hidden` width with a ReLU after it, then `outputs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MlpConfig {
    /// Features per row.
    pub inputs: usize,
    /// The hidden layers' widths, in order; empty is a linear model.
    pub hidden: Vec<usize>,
    /// Scores per row.
    pub outputs: usize,
}

/// The network [`MlpConfig`] builds.
#[derive(Module, Debug)]
pub struct Mlp {
    layers: Vec<Linear>,
}

impl MlpConfig {
    /// The network with its initial weights drawn from `seed`.
    pub fn init(&self, seed: u64) -> Mlp {
        let widths = layer_widths(self.inputs, &self.hidden, self.outputs);
        seeded(seed, |device| Mlp { layers: linear_stack(&widths, device) })
    }
}

impl Network<2> for Mlp {
    fn forward(&self, input: Tensor<2>) -> Tensor<2> {
        through(&self.layers, input)
    }
}

/// An LSTM over a window of `steps` (any length), `inputs` features per step, whose LAST hidden
/// state (`hidden` wide) feeds a linear head of `outputs` scores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LstmNetworkConfig {
    /// Features per step.
    pub inputs: usize,
    /// The hidden state's width.
    pub hidden: usize,
    /// Scores per row.
    pub outputs: usize,
}

/// The network [`LstmNetworkConfig`] builds.
///
/// ⚠ The LSTM step is written HERE rather than taken from `burn-nn`'s `Lstm`: that module builds its
/// output sequence with `Tensor::empty` and `slice_assign`, which the ONNX exporter does not lower
/// (MEASURED 2026-11-16 on burn-onnx 0.22), and this network keeps only the LAST hidden state anyway.
/// The step uses linear layers, slices, sigmoid, tanh, products and sums, all of which export.
#[derive(Module, Debug)]
pub struct LstmNetwork {
    /// The four gates' input transforms in one layer, `inputs -> 4 * hidden`, in the order input,
    /// forget, candidate, output.
    input: Linear,
    /// The same four gates' hidden-state transforms, `hidden -> 4 * hidden`.
    recurrent: Linear,
    head: Linear,
    hidden: usize,
}

impl LstmNetworkConfig {
    /// The network with its initial weights drawn from `seed`.
    pub fn init(&self, seed: u64) -> LstmNetwork {
        seeded(seed, |device| self.build(device))
    }

    /// The layers on `device`, drawn from whatever seed the caller's [`seeded`] set (one lock: a
    /// network that holds this one builds it inside its own `seeded`).
    fn build(&self, device: &Device) -> LstmNetwork {
        layer_widths(self.inputs, &[self.hidden], self.outputs);
        let gates = 4 * self.hidden;
        LstmNetwork {
            input: LinearConfig::new(self.inputs, gates).init(device),
            recurrent: LinearConfig::new(self.hidden, gates).with_bias(false).init(device),
            head: LinearConfig::new(self.hidden, self.outputs).init(device),
            hidden: self.hidden,
        }
    }
}

impl Network<3> for LstmNetwork {
    fn forward(&self, input: Tensor<3>) -> Tensor<2> {
        let [rows, steps, inputs] = input.dims();
        let device = input.device();
        self.over_steps(rows, steps, &device, |step| {
            input.clone().narrow(1, step, 1).reshape([rows, inputs])
        })
    }
}

impl LstmNetwork {
    /// The head's scores from the last hidden state after `steps` steps, `step(t)` being step `t`'s
    /// `[rows, inputs]`: the ONE LSTM step, whichever axis of the caller's tensor holds the steps.
    fn over_steps(
        &self,
        rows: usize,
        steps: usize,
        device: &Device,
        step: impl Fn(usize) -> Tensor<2>,
    ) -> Tensor<2> {
        let width = self.hidden;
        let mut hidden = Tensor::<2>::zeros([rows, width], device);
        let mut cell = Tensor::<2>::zeros([rows, width], device);
        for position in 0..steps {
            let gates = self.input.forward(step(position)) + self.recurrent.forward(hidden);
            let gate = |index: usize| gates.clone().narrow(1, index * width, width);
            let (input_gate, forget_gate) = (sigmoid(gate(0)), sigmoid(gate(1)));
            let (candidate, output_gate) = (tanh(gate(2)), sigmoid(gate(3)));
            cell = forget_gate * cell + input_gate * candidate;
            hidden = output_gate * tanh(cell.clone());
        }
        self.head.forward(hidden)
    }
}

/// A temporal convolutional network: one causal block per `channels` entry, block `i` dilated by
/// `2^i` with `kernel_size` taps, so the receptive field is `1 + (kernel_size - 1) * (2^blocks - 1)`
/// steps; the last step's channels feed a linear head of `outputs` scores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcnConfig {
    /// Features per step (the input's channels).
    pub inputs: usize,
    /// Each block's output channels, in order; at least one.
    pub channels: Vec<usize>,
    /// Taps per convolution; at least 2.
    pub kernel_size: usize,
    /// Scores per row.
    pub outputs: usize,
}

/// The network [`TcnConfig`] builds.
#[derive(Module, Debug)]
pub struct Tcn {
    blocks: Vec<TcnBlock>,
    head: Linear,
}

/// One causal block: a dilated convolution padded on the LEFT only (no step sees a later one), a
/// ReLU, and a residual connection (a 1x1 convolution when the channel count changes).
#[derive(Module, Debug)]
struct TcnBlock {
    convolution: Conv1d,
    residual: Option<Conv1d>,
}

impl TcnConfig {
    /// The network with its initial weights drawn from `seed`.
    pub fn init(&self, seed: u64) -> Tcn {
        let widths = layer_widths(self.inputs, &self.channels, self.outputs);
        assert!(!self.channels.is_empty(), "a TCN needs at least one block");
        assert!(self.kernel_size >= 2, "a TCN's kernel needs at least two taps");
        seeded(seed, |device| {
            let blocks = widths[..widths.len() - 1]
                .windows(2)
                .enumerate()
                .map(|(index, pair)| {
                    let dilation = 1 << index;
                    let left = (self.kernel_size - 1) * dilation;
                    TcnBlock {
                        convolution: Conv1dConfig::new(pair[0], pair[1], self.kernel_size)
                            .with_dilation(dilation)
                            .with_padding(PaddingConfig1d::Explicit(left, 0))
                            .init(device),
                        residual: (pair[0] != pair[1])
                            .then(|| Conv1dConfig::new(pair[0], pair[1], 1).init(device)),
                    }
                })
                .collect();
            let last = widths[widths.len() - 2];
            Tcn { blocks, head: LinearConfig::new(last, self.outputs).init(device) }
        })
    }
}

impl Network<3> for Tcn {
    fn forward(&self, input: Tensor<3>) -> Tensor<2> {
        let features = self.blocks.iter().fold(input, |x, block| {
            let convolved = relu(block.convolution.forward(x.clone()));
            match &block.residual {
                Some(projection) => convolved + projection.forward(x),
                None => convolved + x,
            }
        });
        let [rows, channels, steps] = features.dims();
        self.head.forward(features.narrow(2, steps - 1, 1).reshape([rows, channels]))
    }
}

/// An autoencoder: `inputs`, each `hidden` width with a ReLU, then a LINEAR `code` (no activation,
/// so with no hidden layer it spans the same subspace as a principal-component projection), and the
/// mirror image back to `inputs`. Trained with the mean-squared loss against its own input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoencoderConfig {
    /// Features per row, in and out.
    pub inputs: usize,
    /// The encoder's hidden widths, in order; the decoder mirrors them.
    pub hidden: Vec<usize>,
    /// The code's width: the number of latent factors.
    pub code: usize,
}

/// The network [`AutoencoderConfig`] builds.
#[derive(Module, Debug)]
pub struct Autoencoder {
    encoder: Vec<Linear>,
    decoder: Vec<Linear>,
}

impl AutoencoderConfig {
    /// The network with its initial weights drawn from `seed`.
    pub fn init(&self, seed: u64) -> Autoencoder {
        let encoder = layer_widths(self.inputs, &self.hidden, self.code);
        let decoder: Vec<usize> = encoder.iter().rev().copied().collect();
        seeded(seed, |device| Autoencoder {
            encoder: linear_stack(&encoder, device),
            decoder: linear_stack(&decoder, device),
        })
    }
}

impl Autoencoder {
    /// The code (latent factors) of a `[rows, inputs]` batch: `[rows, code]`.
    pub fn encode(&self, input: Tensor<2>) -> Tensor<2> {
        through(&self.encoder, input)
    }
}

impl Network<2> for Autoencoder {
    /// The reconstruction, `[rows, inputs]`.
    fn forward(&self, input: Tensor<2>) -> Tensor<2> {
        through(&self.decoder, self.encode(input))
    }
}

/// A DeepLOB-style network over a window of order-book rows (Zhang, Zohren and Roberts, "DeepLOB: Deep
/// Convolutional Neural Networks for Limit Order Books", IEEE Transactions on Signal Processing 67(11),
/// 2019, its Figure 3). The input is ONE image channel, `[rows, 1, steps, 4 * levels]`: each step a book
/// row of ask price, ask quantity, bid price and bid quantity per level, best level first (the column
/// order of `vike_analytics::book_export`).
///
/// * Three convolution blocks, each a convolution ACROSS the row and then two `(4, 1)` convolutions
///   ALONG the steps (padded as Keras's `same`: one step before, two after), every one followed by a
///   leaky ReLU of slope 0.01. The first block's `(1, 2)` stride-2 kernel pairs each price with its
///   quantity, the second's pairs each level's ask with its bid, and the third's `(1, levels)` folds
///   the levels into one column.
/// * The inception module: `1x1` then `(3, 1)`; `1x1` then `(5, 1)`; and a `(3, 1)` max pool then
///   `1x1`, each `inception_channels` wide, concatenated.
/// * The LSTM above ([`LstmNetwork`]'s own step, `hidden` wide) over the steps, its last hidden state
///   into a linear head of `outputs` scores (three in the paper: down, flat, up).
///
/// Departures, each for the export: the leaky ReLU is `relu(x) - 0.01 * relu(-x)` (the ONNX exporter
/// lowers no masked select), and the paper's dropout before the LSTM is left out (a training-time
/// regulariser; nothing exports it). Every convolution keeps the step count, so the LSTM sees all
/// `steps` steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepLobConfig {
    /// Price levels a side in each row; a row is `4 * levels` numbers.
    pub levels: usize,
    /// Every convolution block's channels (32 in the paper).
    pub convolution_channels: usize,
    /// Each inception branch's channels (64 in the paper).
    pub inception_channels: usize,
    /// The LSTM's hidden width (64 in the paper).
    pub hidden: usize,
    /// Scores per row.
    pub outputs: usize,
}

/// The network [`DeepLobConfig`] builds.
#[derive(Module, Debug)]
pub struct DeepLob {
    blocks: Vec<ConvolutionBlock>,
    inception: Inception,
    lstm: LstmNetwork,
}

/// One convolution across the row, then two along the steps.
#[derive(Module, Debug)]
struct ConvolutionBlock {
    across: Conv2d,
    along: Vec<Conv2d>,
}

/// DeepLOB's inception module: three branches of different reach over the steps.
#[derive(Module, Debug)]
struct Inception {
    near_in: Conv2d,
    near: Conv2d,
    far_in: Conv2d,
    far: Conv2d,
    pool: MaxPool2d,
    pooled: Conv2d,
}

/// The leaky ReLU's slope below zero (the paper's 0.01).
const LEAKY_SLOPE: f64 = 0.01;

impl DeepLobConfig {
    /// The network with its initial weights drawn from `seed`.
    pub fn init(&self, seed: u64) -> DeepLob {
        let (channels, branch) = (self.convolution_channels, self.inception_channels);
        layer_widths(4 * self.levels, &[channels, branch, self.hidden], self.outputs);
        let lstm =
            LstmNetworkConfig { inputs: 3 * branch, hidden: self.hidden, outputs: self.outputs };
        seeded(seed, |device| {
            let convolution =
                |from: usize, kernel: [usize; 2], stride: [usize; 2], pad: [usize; 2]| {
                    Conv2dConfig::new([from, channels], kernel)
                        .with_stride(stride)
                        .with_padding(PaddingConfig2d::Explicit(pad[0], 0, pad[1], 0))
                        .init(device)
                };
            let block = |from: usize, across: [usize; 2]| ConvolutionBlock {
                across: convolution(from, across, across, [0, 0]),
                along: vec![
                    convolution(channels, [4, 1], [1, 1], [1, 2]),
                    convolution(channels, [4, 1], [1, 1], [1, 2]),
                ],
            };
            let branch_of = |from: usize, height: usize| {
                let pad = height / 2;
                Conv2dConfig::new([from, branch], [height, 1])
                    .with_padding(PaddingConfig2d::Explicit(pad, 0, pad, 0))
                    .init(device)
            };
            DeepLob {
                blocks: vec![
                    block(1, [1, 2]),
                    block(channels, [1, 2]),
                    block(channels, [1, self.levels]),
                ],
                inception: Inception {
                    near_in: branch_of(channels, 1),
                    near: branch_of(branch, 3),
                    far_in: branch_of(channels, 1),
                    far: branch_of(branch, 5),
                    pool: MaxPool2dConfig::new([3, 1])
                        .with_strides([1, 1])
                        .with_padding(PaddingConfig2d::Explicit(1, 0, 1, 0))
                        .init(),
                    pooled: branch_of(channels, 1),
                },
                lstm: lstm.build(device),
            }
        })
    }
}

impl Network<4> for DeepLob {
    fn forward(&self, input: Tensor<4>) -> Tensor<2> {
        let features = self.blocks.iter().fold(input, |x, block| {
            let across = leaky_relu(block.across.forward(x));
            block.along.iter().fold(across, |x, convolution| leaky_relu(convolution.forward(x)))
        });
        let inception = &self.inception;
        let near = leaky_relu(
            inception.near.forward(leaky_relu(inception.near_in.forward(features.clone()))),
        );
        let far = leaky_relu(
            inception.far.forward(leaky_relu(inception.far_in.forward(features.clone()))),
        );
        let pooled = leaky_relu(inception.pooled.forward(inception.pool.forward(features)));
        let mixed = Tensor::cat(vec![near, far, pooled], 1);
        let [rows, channels, steps, _] = mixed.dims();
        let device = mixed.device();
        let sequence = mixed.reshape([rows, channels, steps]);
        self.lstm.over_steps(rows, steps, &device, |step| {
            sequence.clone().narrow(2, step, 1).reshape([rows, channels])
        })
    }
}

/// DeepLOB's activation, `max(x, 0.01 x)`, as `relu(x) - 0.01 relu(-x)`: the exporter lowers these
/// operations and not `burn`'s own leaky ReLU, which is a masked select.
fn leaky_relu<const D: usize>(x: Tensor<D>) -> Tensor<D> {
    relu(x.clone()) - relu(x.neg()).mul_scalar(LEAKY_SLOPE)
}

/// `first`, `middle..`, `last`, refusing a zero width (a config is code: a zero is a bug, not data).
fn layer_widths(first: usize, middle: &[usize], last: usize) -> Vec<usize> {
    let widths: Vec<usize> =
        std::iter::once(first).chain(middle.iter().copied()).chain([last]).collect();
    assert!(widths.iter().all(|&width| width > 0), "a layer width of zero in {widths:?}");
    widths
}

/// One linear layer per consecutive pair of `widths`.
fn linear_stack(widths: &[usize], device: &Device) -> Vec<Linear> {
    widths.windows(2).map(|pair| LinearConfig::new(pair[0], pair[1]).init(device)).collect()
}

/// `input` through `layers`, a ReLU after every layer but the last.
fn through(layers: &[Linear], input: Tensor<2>) -> Tensor<2> {
    let last = layers.len() - 1;
    layers.iter().enumerate().fold(input, |x, (index, layer)| {
        let output = layer.forward(x);
        if index < last { relu(output) } else { output }
    })
}
