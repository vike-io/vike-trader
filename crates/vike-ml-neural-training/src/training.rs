//! Fitting and running a [`Network`] on plain `f32` rows.
//!
//! [`train`] runs Adam over shuffled mini-batches for a fixed number of epochs and returns the
//! network with each epoch's mean loss. The seed in [`TrainingSettings`] orders the batches; the
//! network's own `init(seed)` fixed its starting weights, so a run is the pair of seeds (crate doc).
//! [`predict`] and [`encode`] run a network without gradients.
//!
//! # The Sharpe objective ([`Loss::Sharpe`])
//!
//! The network is trained DIRECTLY on what a trader is paid for, as in Deep Momentum Networks (Lim,
//! Zohren, Roberts, 2019): a row's position is `tanh(score)`, in `[-1, 1]`; its target is the return
//! the position earns over the NEXT period (volatility-scaled or not, as the caller laid it out); a
//! row's net return is `position * return - turnover_cost * |position - previous position|`, summed
//! over the outputs when a row holds several assets; and the loss is MINUS the Sharpe ratio of a
//! batch's net returns, `mean / sqrt(population variance + 1e-12)` (not annualised: a constant factor
//! moves no optimum).
//!
//! * **The rows must be in TIME order**, and a batch is a CONTIGUOUS run of `batch_size` rows (the
//!   last run of an epoch absorbs a remainder of one row, so no batch is a single row, whose variance
//!   is zero). The seed shuffles the ORDER of the runs per epoch, never the rows inside one, so the
//!   turnover term compares a position with the one just before it in time. A run's first row pays no
//!   turnover: its predecessor sits in another batch.
//! * Each epoch's loss is the row-weighted mean of its batches' minus-Sharpe ratios.
//!
//! # The hedging objective ([`Loss::Hedging`])
//!
//! Deep hedging (Buehler, Gonon, Teichmann and Wood, *Deep Hedging*, 2019) with the quadratic
//! criterion: the network's score on a row is the HEDGE held from that row's decision, and the loss
//! is the mean square of each path's terminal P&L,
//! `label + Σ_t δ_t (P_{t+1} − P_t) − fee_rate · Σ_t |δ_t − δ_{t−1}| · P_t` (`δ_{−1} = 0`), over
//! `t = 0 … path_steps − 2`.
//!
//! * **The rows are PATHS**: `path_steps` consecutive rows each, in time order; `P_t` is the feature
//!   at `price_column` (any scale a path shares, such as the price over its first one) and the label,
//!   the same on every row of a path, is the hedged position's own P&L in that scale (an option leg's
//!   payout less its premium). The last row's hedge trades nothing: no price follows it.
//! * A batch is `batch_size` WHOLE paths: the paths are cut into fixed groups of `batch_size` in order,
//!   and the seed shuffles the order of the groups per epoch, never the rows inside a path.
//! * The MLP alone: a path's rows are flat feature rows, and the policy sees no state of its own
//!   (its previous hedge). shortcut: a recurrent policy, when a chapter needs one.
//!
//! Every function validates its shapes BEFORE a tensor exists and answers an [`InputError`]: a
//! mismatch inside `burn` would be a panic.

use burn::module::Module;
use burn::nn::loss::{BinaryCrossEntropyLossConfig, CrossEntropyLossConfig, MseLoss, Reduction};
use burn::optim::{AdamConfig, GradientsParams};
use burn::tensor::activation::tanh;
use burn::tensor::{Device, Int, Tensor, TensorData};
use rand::seq::SliceRandom;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;

use crate::flow::negative_log_likelihood;
use crate::networks::{Autoencoder, Network};
use crate::{InputError, device};

/// What the network is fitted against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Loss {
    /// Mean squared error between the scores and real-valued targets (regression; an autoencoder
    /// against its own input).
    MeanSquaredError,
    /// Binary cross-entropy of the scores read as LOGITS against targets that are each exactly `0`
    /// or `1` (a binary classifier; one target column per score).
    BinaryCrossEntropy,
    /// The cross-entropy of a softmax over the `classes` scores of a row (read as LOGITS) against ONE
    /// target per row, the class index `0..classes` as a whole number (a multiclass classifier; the
    /// network must answer `classes` scores).
    CrossEntropy {
        /// How many classes, and so how many scores a row has.
        classes: usize,
    },
    /// Minus the Sharpe ratio of the positions `tanh(score)` held over each row's next-period return
    /// (the target), net of `turnover_cost` per unit of position traded (module doc). The rows must be
    /// in time order.
    Sharpe {
        /// The cost of trading one unit of position, in the targets' return units; `0` for none.
        turnover_cost: f32,
    },
    /// Minus the log-likelihood of ONE real target per row under the conditional normalising flow
    /// whose [`crate::flow::FLOW_PARAMETERS`] parameters the network's scores are
    /// ([`crate::flow`]): the network learns the LAW of the target, not one value of it.
    SinhArcsinhFlow,
    /// The mean square of each path's hedged P&L (module doc): the score is the hedge, the rows are
    /// paths of `path_steps` rows.
    Hedging {
        /// Rows per path, in time order (2 or more).
        path_steps: usize,
        /// The feature column holding the hedge instrument's price.
        price_column: usize,
        /// The cost of a trade as a fraction of its notional (`0` for none).
        fee_rate: f32,
    },
}

/// Under the Sharpe objective's square root, so a batch whose net returns are all equal (a network
/// that holds a constant position over equal returns) has a finite loss.
const SHARPE_VARIANCE_FLOOR: f32 = 1e-12;

/// How long and how fast to train, and the seed of the batch order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrainingSettings {
    /// Seeds every epoch's shuffle of the rows.
    pub seed: u64,
    /// Passes over the data.
    pub epochs: usize,
    /// Rows per optimiser step; the last batch of an epoch may be smaller.
    pub batch_size: usize,
    /// Adam's step size.
    pub learning_rate: f64,
}

/// A fitted network and the mean loss of each epoch, in order (row-weighted over its batches).
#[derive(Debug)]
pub struct Trained<N> {
    /// The network after the last epoch.
    pub network: N,
    /// One mean loss per epoch.
    pub epoch_losses: Vec<f32>,
}

/// Fits `network` to `targets` (`[rows, outputs]`, row-major) from `inputs` (row-major, shaped
/// `input_shape`, rows first).
pub fn train<N: Network<D>, const D: usize>(
    network: N,
    inputs: &[f32],
    input_shape: [usize; D],
    targets: &[f32],
    loss: Loss,
    settings: &TrainingSettings,
) -> Result<Trained<N>, InputError> {
    let rows = check_shape(inputs, input_shape)?;
    let outputs = targets_per_row(targets, rows)?;
    if loss == Loss::BinaryCrossEntropy && targets.iter().any(|&t| t != 0.0 && t != 1.0) {
        return Err(InputError("the binary cross-entropy needs every target to be 0 or 1".into()));
    }
    if let Loss::CrossEntropy { classes } = loss {
        let class_index = |t: f32| t >= 0.0 && t.fract() == 0.0 && (t as usize) < classes;
        if classes < 2 || outputs != 1 || !targets.iter().all(|&t| class_index(t)) {
            return Err(InputError(format!(
                "the cross-entropy over {classes} classes needs 2 or more classes and ONE target per \
                 row, a class index in 0..{classes}"
            )));
        }
    }
    if loss == Loss::SinhArcsinhFlow && outputs != 1 {
        return Err(InputError(format!("a flow models ONE target per row, not {outputs}")));
    }
    if settings.epochs == 0 || settings.batch_size == 0 {
        return Err(InputError("epochs and batch_size must both be at least 1".into()));
    }
    if let Loss::Sharpe { turnover_cost } = loss {
        if !(turnover_cost.is_finite() && turnover_cost >= 0.0) {
            return Err(InputError(format!("turnover_cost {turnover_cost} is not a cost")));
        }
        if rows < 2 || settings.batch_size < 2 {
            return Err(InputError(
                "the Sharpe objective needs at least 2 rows and a batch_size of at least 2 (a \
                 single return has no variance)"
                    .into(),
            ));
        }
    }
    if !(settings.learning_rate.is_finite() && settings.learning_rate > 0.0) {
        return Err(InputError(format!(
            "learning_rate {} is not positive",
            settings.learning_rate
        )));
    }
    // The Sharpe objective's batches are contiguous runs of rows; the hedging objective's, of whole paths.
    let mut runs = match loss {
        Loss::Hedging { path_steps, price_column, fee_rate } => {
            check_paths(inputs, &input_shape, targets, path_steps, price_column, fee_rate)?;
            contiguous_runs(rows, path_steps * settings.batch_size)
        }
        _ => contiguous_runs(rows, settings.batch_size),
    };

    let device = device(true);
    let inputs = tensor(inputs, input_shape, &device);
    let targets = tensor(targets, [rows, outputs], &device);
    let mut optimizer = AdamConfig::new().init();
    let mut batch_order = ChaCha8Rng::seed_from_u64(settings.seed);
    let mut order: Vec<i64> = (0..rows as i64).collect();
    let mut network = network;
    let mut epoch_losses = Vec::with_capacity(settings.epochs);
    for _ in 0..settings.epochs {
        let batches: Vec<&[i64]> = if matches!(loss, Loss::Sharpe { .. } | Loss::Hedging { .. }) {
            runs.shuffle(&mut batch_order);
            runs.iter().map(Vec::as_slice).collect()
        } else {
            order.shuffle(&mut batch_order);
            order.chunks(settings.batch_size).collect()
        };
        let mut loss_sum = 0.0_f64;
        for batch in batches {
            let index = Tensor::<1, Int>::from_data(
                TensorData::new(batch.to_vec(), [batch.len()]),
                &device,
            );
            let scores = network.forward(inputs.clone().select(0, index.clone()));
            let wanted = targets.clone().select(0, index.clone());
            let batch_loss = match loss {
                Loss::MeanSquaredError => MseLoss::new().forward(scores, wanted, Reduction::Mean),
                Loss::BinaryCrossEntropy => BinaryCrossEntropyLossConfig::new()
                    .with_logits(true)
                    .init(&device)
                    .forward(scores, wanted.int()),
                Loss::CrossEntropy { .. } => CrossEntropyLossConfig::new()
                    .init(&device)
                    .forward(scores, wanted.int().reshape([batch.len()])),
                Loss::Sharpe { turnover_cost } => negative_sharpe(scores, wanted, turnover_cost),
                Loss::SinhArcsinhFlow => negative_log_likelihood(scores, wanted),
                Loss::Hedging { path_steps, price_column, fee_rate } => {
                    let paths = batch.len() / path_steps;
                    let prices = inputs
                        .clone()
                        .select(0, index)
                        .reshape([batch.len(), input_shape[1]])
                        .narrow(1, price_column, 1)
                        .reshape([paths, path_steps]);
                    hedging_loss(
                        scores.reshape([paths, path_steps]),
                        prices,
                        wanted.reshape([paths, path_steps]),
                        fee_rate,
                    )
                }
            };
            loss_sum += f64::from(batch_loss.clone().into_scalar::<f32>()) * batch.len() as f64;
            let gradients = GradientsParams::from_grads(batch_loss.backward(), &network);
            network = optimizer.step(settings.learning_rate, network, gradients);
        }
        epoch_losses.push((loss_sum / rows as f64) as f32);
    }
    Ok(Trained { network, epoch_losses })
}

/// `0..rows` cut into contiguous runs of `size` rows, in time order; a last run of ONE row joins the
/// run before it (the Sharpe objective's batches, module doc; the hedging objective's too, over whole paths,
/// where every run is a multiple of `path_steps` >= 2 rows and so the merge never fires).
fn contiguous_runs(rows: usize, size: usize) -> Vec<Vec<i64>> {
    let all: Vec<i64> = (0..rows as i64).collect();
    let mut runs: Vec<Vec<i64>> = all.chunks(size).map(<[i64]>::to_vec).collect();
    if let [.., before, last] = runs.as_mut_slice()
        && last.len() == 1
    {
        before.append(last);
        runs.pop();
    }
    runs
}

/// Minus the Sharpe ratio of one contiguous batch (module doc): `scores` and `returns` are both
/// `[rows, outputs]`, `rows >= 2`, in time order.
fn negative_sharpe(scores: Tensor<2>, returns: Tensor<2>, turnover_cost: f32) -> Tensor<1> {
    let positions = tanh(scores);
    let [rows, _] = positions.dims();
    let mut net = (positions.clone() * returns).sum_dim(1);
    if turnover_cost > 0.0 {
        let traded = (positions.clone().narrow(0, 1, rows - 1)
            - positions.clone().narrow(0, 0, rows - 1))
        .abs()
        .sum_dim(1);
        let first = Tensor::<2>::zeros([1, 1], &positions.device());
        net = net - Tensor::cat(vec![first, traded], 0).mul_scalar(turnover_cost);
    }
    let (variance, mean) = net.var_mean_bias(0);
    (mean / variance.add_scalar(SHARPE_VARIANCE_FLOOR).sqrt()).neg().reshape([1_usize])
}

/// The hedging objective's checks (module doc), before any tensor exists: flat rows, one target per
/// row, whole paths of 2 rows or more, a price column inside a row holding positive finite prices,
/// one label per path, and a fee that is a cost.
fn check_paths<const D: usize>(
    inputs: &[f32],
    input_shape: &[usize; D],
    targets: &[f32],
    path_steps: usize,
    price_column: usize,
    fee_rate: f32,
) -> Result<(), InputError> {
    let &[rows, columns] = input_shape.as_slice() else {
        return Err(InputError(format!("the hedging loss takes flat rows, not {input_shape:?}")));
    };
    if targets.len() != rows {
        return Err(InputError("the hedging loss takes ONE label per row".into()));
    }
    if path_steps < 2 || !rows.is_multiple_of(path_steps) {
        return Err(InputError(format!(
            "{rows} rows are not whole paths of {path_steps} (2 or more)"
        )));
    }
    if price_column >= columns {
        return Err(InputError(format!(
            "price_column {price_column} is outside a {columns}-wide row"
        )));
    }
    if !(fee_rate.is_finite() && fee_rate >= 0.0) {
        return Err(InputError(format!("fee_rate {fee_rate} is not a cost")));
    }
    if let Some(row) = (0..rows).find(|row| {
        let price = inputs[row * columns + price_column];
        !(price.is_finite() && price > 0.0)
    }) {
        return Err(InputError(format!("row {row}: the price column holds no positive price")));
    }
    if let Some(path) =
        targets.chunks(path_steps).position(|path| path.iter().any(|l| *l != path[0]))
    {
        return Err(InputError(format!("path {path}: its rows carry different labels")));
    }
    Ok(())
}

/// The mean square of each path's hedged P&L (module doc). `hedges`, `prices` and `labels` are
/// `[paths, steps]`, `steps >= 2`, each path's label repeated on its rows.
pub(crate) fn hedging_loss(
    hedges: Tensor<2>,
    prices: Tensor<2>,
    labels: Tensor<2>,
    fee_rate: f32,
) -> Tensor<1> {
    let [paths, steps] = hedges.dims();
    let held = hedges.narrow(1, 0, steps - 1);
    let moves = prices.clone().narrow(1, 1, steps - 1) - prices.clone().narrow(1, 0, steps - 1);
    let mut pnl = labels.narrow(1, 0, 1) + (held.clone() * moves).sum_dim(1);
    if fee_rate > 0.0 {
        let none = Tensor::<2>::zeros([paths, 1], &held.device());
        let before = if steps > 2 {
            Tensor::cat(vec![none, held.clone().narrow(1, 0, steps - 2)], 1)
        } else {
            none
        };
        let traded = ((held - before).abs() * prices.narrow(1, 0, steps - 1)).sum_dim(1);
        pnl = pnl - traded.mul_scalar(fee_rate);
    }
    (pnl.clone() * pnl).mean()
}

/// The network's raw scores for `inputs`, row-major `[rows, outputs]`, computed without gradients.
pub fn predict<N: Network<D>, const D: usize>(
    network: &N,
    inputs: &[f32],
    input_shape: [usize; D],
) -> Result<Vec<f32>, InputError> {
    run(network, inputs, input_shape, N::forward)
}

/// The autoencoder's code (latent factors) for `inputs` (`[rows, inputs]`), row-major `[rows, code]`.
pub fn encode(
    network: &Autoencoder,
    inputs: &[f32],
    input_shape: [usize; 2],
) -> Result<Vec<f32>, InputError> {
    run(network, inputs, input_shape, Autoencoder::encode)
}

/// `forward` over `inputs` on the gradient-free device.
pub(crate) fn run<M: Module, const D: usize>(
    network: &M,
    inputs: &[f32],
    input_shape: [usize; D],
    forward: impl Fn(&M, Tensor<D>) -> Tensor<2>,
) -> Result<Vec<f32>, InputError> {
    check_shape(inputs, input_shape)?;
    let device = device(false);
    let scores = forward(&network.valid(), tensor(inputs, input_shape, &device));
    let data = scores.into_data_as::<f32>();
    let values = data.as_slice::<f32>().map_err(|error| InputError(format!("{error:?}")))?;
    Ok(values.to_vec())
}

/// The row count of `values` shaped `shape`, refusing an empty batch or a length that disagrees.
fn check_shape<const D: usize>(values: &[f32], shape: [usize; D]) -> Result<usize, InputError> {
    if D < 2 || shape.contains(&0) {
        return Err(InputError(format!("shape {shape:?} needs a row axis and no empty axis")));
    }
    if shape.iter().product::<usize>() != values.len() {
        return Err(InputError(format!("shape {shape:?} does not hold {} values", values.len())));
    }
    Ok(shape[0])
}

/// Targets per row, refusing a target count that is not a whole multiple of `rows`.
fn targets_per_row(targets: &[f32], rows: usize) -> Result<usize, InputError> {
    if targets.is_empty() || !targets.len().is_multiple_of(rows) {
        return Err(InputError(format!("{} targets do not split over {rows} rows", targets.len())));
    }
    Ok(targets.len() / rows)
}

/// `values` as a float tensor of `shape` on `device`.
fn tensor<const D: usize>(values: &[f32], shape: [usize; D], device: &Device) -> Tensor<D> {
    Tensor::from_data(TensorData::new(values.to_vec(), shape), device)
}
