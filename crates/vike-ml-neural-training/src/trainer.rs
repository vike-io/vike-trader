//! The trainer PROCESS's work, as a library: what `vike-ml-neural-training train` reads and writes, so the
//! binary is argv and files only and a test can drive the same code.
//!
//! A study never links this crate (decision 0292): it asks its host's learner seam for a fit, and the
//! host's `vike_ml_training::NeuralLearner` runs `vike-ml-neural-training` OUTSIDE the jail, as
//! `vike_ml_training::GbdtLearner` runs LightGBM. The two files that cross:
//!
//! * **the data**: `vike_ml_training::TrainData::write_matrix`'s file, the fit's matrix as raw
//!   little-endian `f32` behind a header and a checksum, read by `vike_ml_training::train::matrix`'s
//!   `read_matrix` (whose module doc is the layout; one module holds both halves), which also takes
//!   the CSV an older host writes, so a rollback of the host alone keeps fitting. A missing value is
//!   refused: a network has no missing-value branch.
//! * **the network** ([`NetworkSpec`], TOML): which network, its widths, the loss and the epochs. The
//!   learning rate and the seed come on the command line, from the learner seam's point and seed.
//!   `loss = "sharpe"` trains the position itself (`crate::training::Loss::Sharpe`): the label is
//!   then each row's NEXT-period return, the rows must be in time order, and `turnover_cost` (that
//!   loss only, default 0) charges each unit of position traded.
//!   `loss = "hedging"` trains a HEDGE (`crate::training::Loss::Hedging`, deep hedging): the rows are
//!   paths of `path_steps` rows in time order, `price_column` names the hedge's price among the
//!   features, `fee_rate` (that loss only, default 0) charges each trade's notional, and `batch_size`
//!   counts PATHS. The model answers a value, the hedge to hold.
//!   `warm_start = true` (off when absent) with its own `warm_epochs`: each fit also hands back its
//!   WEIGHTS (`crate::weights`), and a fit handed a previous fit's weights starts from them and makes
//!   `warm_epochs` passes, so a walk-forward fold continues the fold before it instead of starting
//!   over. A network whose shape differs from those weights is refused, never half-loaded, and a fit
//!   whose trained weights are not finite (it diverged) is refused, so they start no next fold. Which
//!   fold continues which is the caller's (`vike_ml_training::NeuralLearner`'s chains); with the
//!   key absent nothing here changes.
//!
//! What comes back ([`train_from_spec`]): the ONNX bytes, exported at ONE row (`rows = 1`, the shape a
//! live prediction and the host's per-row predictor feed) through `crate::onnx::export_onnx`, and ONE
//! provenance line ([`TrainedModel::provenance`]). The provenance's `loss=` word (with `classes=` for
//! `multiclass`) is what the model's answer MEANS ([`prediction_kind`]): the binary process's
//! `predict` declares it, never assumes it.
//!
//! `loss = "flow"` trains a conditional normalising flow (`crate::flow`): the label is a real value
//! (standardised by the caller), the network computes its law from the row, and what comes back is the
//! flow's SAMPLER (`crate::onnx::export_flow_sampler`): the ONNX takes the row and ONE MORE column, a
//! standard normal draw, and answers a draw of the label. Its input shape is `[1, columns + 1]`
//! whatever the network, and the `predict` verb answers it as a `value`.
//!
//! A row is FLAT: `columns` numbers. The sequence networks read it with `steps` (from the spec):
//! the LSTM as `steps` consecutive groups of `columns / steps` features (step-major), the TCN as
//! `columns / steps` channels of `steps` values each (channel-major, a 1-D convolution's own layout,
//! because the exporter lowers no transpose). The caller lays the row out; nothing here reorders it.
//! DeepLOB (`network = "deeplob"`) reads it step-major like the LSTM, each step a book row of
//! `4 * levels` numbers (`crate::networks::DeepLobConfig`); `channels = [convolution, inception]` and
//! `hidden = [lstm]` are its widths.
//!
//! `loss = "multiclass"` with `classes = K` trains any network as a K-class classifier: the label is
//! the class index `0..K`, the network answers K scores, and the model answers one probability per
//! class (a softmax); its provenance line then carries `classes=K`.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use vike_ml_prediction::PredictionKind;
use vike_ml_training::train::matrix::Rows;

use crate::InputError;
use crate::flow::FLOW_PARAMETERS;
use crate::networks::{DeepLobConfig, LstmNetworkConfig, MlpConfig, Network, TcnConfig};
use crate::onnx::{export_flow_sampler, export_onnx};
use crate::training::{Loss, TrainingSettings, train};
use crate::weights::{finite_weights, weights_of, with_weights};

/// The network a fit trains, as the spec file spells it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    /// `mlp`, `lstm`, `tcn` or `deeplob`.
    pub network: String,
    /// The MLP's hidden widths, or the LSTM's or DeepLOB's one LSTM width (a one-element list).
    #[serde(default)]
    pub hidden: Vec<usize>,
    /// The TCN's block widths, or DeepLOB's `[convolution, inception]` channels.
    #[serde(default)]
    pub channels: Vec<usize>,
    /// The TCN's taps per convolution.
    #[serde(default)]
    pub kernel_size: Option<usize>,
    /// How many steps a row holds (the LSTM, the TCN and DeepLOB).
    #[serde(default)]
    pub steps: Option<usize>,
    /// `binary` (labels 0 or 1; the model answers a probability), `multiclass` (labels are class
    /// indices `0..classes`; the model answers one probability per class), `regression` (the model
    /// answers a value), `sharpe` (labels are next-period returns; the model answers a position in
    /// `[-1, 1]`), `flow` (the model answers a DRAW of the label from its learned law; module doc) or
    /// `hedging` (paths of rows; the model answers the hedge to hold).
    pub loss: String,
    /// The Sharpe objective's cost per unit of position traded, in the labels' return units
    /// (`loss = "sharpe"` only; absent is 0).
    #[serde(default)]
    pub turnover_cost: Option<f64>,
    /// The class count of `loss = "multiclass"` (that loss only).
    #[serde(default)]
    pub classes: Option<usize>,
    /// Passes over the data.
    pub epochs: usize,
    /// Rows per optimiser step; PATHS per step for `loss = "hedging"`.
    pub batch_size: usize,
    /// Rows per path (`loss = "hedging"` only).
    #[serde(default)]
    pub path_steps: Option<usize>,
    /// The feature column holding the hedge's price (`loss = "hedging"` only).
    #[serde(default)]
    pub price_column: Option<usize>,
    /// A trade's cost as a fraction of its notional (`loss = "hedging"` only; absent is 0).
    #[serde(default)]
    pub fee_rate: Option<f64>,
    /// Each fit keeps its weights, and a fit handed a previous fit's weights starts from them
    /// (module doc). Absent is off: every fit starts from the seed's weights, as before.
    #[serde(default)]
    pub warm_start: bool,
    /// Passes over the data of a fit that starts from previous weights (`warm_start` only).
    #[serde(default)]
    pub warm_epochs: Option<usize>,
}

impl NetworkSpec {
    /// The spec in `text` (TOML); an unknown key is refused, so a misspelt width is not a default.
    pub fn parse(text: &str) -> Result<NetworkSpec, InputError> {
        toml::from_str(text)
            .map_err(|e| InputError(format!("the network spec does not parse: {e}")))
    }

    fn loss(&self) -> Result<Loss, InputError> {
        let loss = self.loss.as_str();
        if self.turnover_cost.is_some() && loss != "sharpe" {
            return Err(InputError("`turnover_cost` applies only to `loss = \"sharpe\"`".into()));
        }
        if self.classes.is_some() && loss != "multiclass" {
            return Err(InputError("`classes` applies only to `loss = \"multiclass\"`".into()));
        }
        let hedging_keys =
            self.path_steps.is_some() || self.price_column.is_some() || self.fee_rate.is_some();
        if hedging_keys && loss != "hedging" {
            return Err(InputError(
                "`path_steps`, `price_column` and `fee_rate` apply only to `loss = \"hedging\"`"
                    .into(),
            ));
        }
        let needs = |key: &str| InputError(format!("`loss = \"hedging\"` needs `{key}`"));
        match (loss, self.classes) {
            ("sharpe", _) => {
                Ok(Loss::Sharpe { turnover_cost: self.turnover_cost.unwrap_or(0.0) as f32 })
            }
            ("multiclass", Some(classes)) if classes >= 2 => Ok(Loss::CrossEntropy { classes }),
            ("multiclass", _) => {
                Err(InputError("`loss = \"multiclass\"` needs `classes`, 2 or more".into()))
            }
            ("hedging", _) => Ok(Loss::Hedging {
                path_steps: self.path_steps.ok_or_else(|| needs("path_steps"))?,
                price_column: self.price_column.ok_or_else(|| needs("price_column"))?,
                fee_rate: self.fee_rate.unwrap_or(0.0) as f32,
            }),
            ("binary", _) => Ok(Loss::BinaryCrossEntropy),
            ("regression", _) => Ok(Loss::MeanSquaredError),
            ("flow", _) => Ok(Loss::SinhArcsinhFlow),
            (other, _) => Err(InputError(format!(
                "loss `{other}`: expected `binary`, `multiclass`, `regression`, `sharpe`, `flow` or \
                 `hedging`"
            ))),
        }
    }

    /// The passes a fit makes: `warm_epochs` when it starts from previous weights, else `epochs`.
    fn epochs(&self, warm: bool) -> Result<usize, InputError> {
        match (self.warm_start, self.warm_epochs, warm) {
            (false, Some(_), _) => {
                Err(InputError("`warm_epochs` applies only to `warm_start = true`".into()))
            }
            (true, None, _) => Err(InputError("`warm_start = true` needs `warm_epochs`".into())),
            (false, None, true) => Err(InputError(
                "a fit handed a previous fit's weights needs `warm_start = true` in its spec"
                    .into(),
            )),
            (true, Some(warm_epochs), true) => Ok(warm_epochs),
            _ => Ok(self.epochs),
        }
    }

    /// `columns` split into `(steps, features per step)`, refusing a split that does not divide.
    fn per_step(&self, columns: usize) -> Result<(usize, usize), InputError> {
        let steps = self
            .steps
            .ok_or_else(|| InputError(format!("a `{}` network needs `steps`", self.network)))?;
        if steps == 0 || !columns.is_multiple_of(steps) {
            return Err(InputError(format!("{columns} columns do not split into {steps} steps")));
        }
        Ok((steps, columns / steps))
    }
}

/// A trained network as the trainer process hands it back.
#[derive(Debug, Clone, PartialEq)]
pub struct TrainedModel {
    /// The ONNX model, ONE row in (shape [`TrainedModel::input_shape`]).
    pub onnx: Vec<u8>,
    /// The model's input shape, row count 1 first.
    pub input_shape: Vec<usize>,
    /// The last epoch's mean training loss.
    pub final_loss: f32,
    /// One line of `key=value` words: the trainer and its version, the network, the loss, the seed,
    /// the learning rate, the epochs, the input shape and the last epoch's mean loss, then `classes=K`
    /// for a multiclass loss, and with `warm_start` what the fit started from: `warm_start=cold` or
    /// `warm_start=<SHA-256 of the weights file>`.
    pub provenance: String,
    /// The trained network's weights (`crate::weights`), kept when the spec says `warm_start`.
    pub weights: Option<Vec<u8>>,
}

/// Trains the network `spec` names on `rows` with `seed` (the initial weights and the batch order)
/// and Adam's `learning_rate`, and exports it for one row. With `start` (a previous fit's weights
/// file, `warm_start` specs only) the seed's network is overwritten by those weights before
/// training, and the fit makes `warm_epochs` passes.
pub fn train_from_spec(
    spec: &NetworkSpec,
    rows: &Rows,
    seed: u64,
    learning_rate: f64,
    start: Option<&[u8]>,
) -> Result<TrainedModel, InputError> {
    let epochs = spec.epochs(start.is_some())?;
    let loss = spec.loss()?;
    let outputs = match loss {
        Loss::CrossEntropy { classes } => classes,
        Loss::SinhArcsinhFlow => FLOW_PARAMETERS,
        Loss::BinaryCrossEntropy
        | Loss::MeanSquaredError
        | Loss::Sharpe { .. }
        | Loss::Hedging { .. } => 1,
    };
    let count = rows.labels.len();
    let settings = TrainingSettings { seed, epochs, batch_size: spec.batch_size, learning_rate };
    let mut model = match spec.network.as_str() {
        "mlp" => {
            let network =
                MlpConfig { inputs: rows.columns, hidden: spec.hidden.clone(), outputs }.init(seed);
            fit(network, rows, [count, rows.columns], [1, rows.columns], loss, &settings, start)?
        }
        "lstm" => {
            let (steps, inputs) = spec.per_step(rows.columns)?;
            let &[hidden] = spec.hidden.as_slice() else {
                return Err(InputError("an `lstm` network takes ONE hidden width".into()));
            };
            let network = LstmNetworkConfig { inputs, hidden, outputs }.init(seed);
            fit(network, rows, [count, steps, inputs], [1, steps, inputs], loss, &settings, start)?
        }
        "deeplob" => {
            let (steps, width) = spec.per_step(rows.columns)?;
            let (&[hidden], &[convolution_channels, inception_channels]) =
                (spec.hidden.as_slice(), spec.channels.as_slice())
            else {
                return Err(InputError(
                    "a `deeplob` network takes ONE hidden width and `channels = [convolution, \
                     inception]`"
                        .into(),
                ));
            };
            if !width.is_multiple_of(4)
                || [hidden, convolution_channels, inception_channels].contains(&0)
            {
                return Err(InputError(format!(
                    "a `deeplob` step is 4 numbers a level ({width} given) and its widths are positive"
                )));
            }
            let levels = width / 4;
            let network =
                DeepLobConfig { levels, convolution_channels, inception_channels, hidden, outputs }
                    .init(seed);
            fit(
                network,
                rows,
                [count, 1, steps, width],
                [1, 1, steps, width],
                loss,
                &settings,
                start,
            )?
        }
        "tcn" => {
            let (steps, inputs) = spec.per_step(rows.columns)?;
            let kernel_size = spec
                .kernel_size
                .ok_or_else(|| InputError("a `tcn` network needs `kernel_size`".into()))?;
            if spec.channels.is_empty() || kernel_size < 2 {
                return Err(InputError(
                    "a `tcn` needs `channels` and a kernel of 2 or more".into(),
                ));
            }
            let network =
                TcnConfig { inputs, channels: spec.channels.clone(), kernel_size, outputs }
                    .init(seed);
            fit(network, rows, [count, inputs, steps], [1, inputs, steps], loss, &settings, start)?
        }
        other => {
            return Err(InputError(format!(
                "network `{other}`: expected `mlp`, `lstm`, `tcn` or `deeplob`"
            )));
        }
    };
    let shape_words: Vec<String> = model.input_shape.iter().map(usize::to_string).collect();
    model.provenance = format!(
        "trainer=vike-ml-neural-training version={} network={} loss={} seed={seed} learning_rate={learning_rate} \
         epochs={epochs} input_shape={} final_loss={}",
        env!("CARGO_PKG_VERSION"),
        spec.network,
        spec.loss,
        shape_words.join(","),
        model.final_loss,
    );
    if let Loss::Sharpe { turnover_cost } = loss {
        model.provenance.push_str(&format!(" turnover_cost={turnover_cost}"));
    }
    if let Loss::CrossEntropy { classes } = loss {
        model.provenance.push_str(&format!(" classes={classes}"));
    }
    if let Loss::Hedging { path_steps, price_column, fee_rate } = loss {
        model.provenance.push_str(&format!(
            " path_steps={path_steps} price_column={price_column} fee_rate={fee_rate}"
        ));
    }
    if spec.warm_start {
        if let Some(weights) = &model.weights {
            finite_weights(weights).map_err(|e| {
                InputError(format!(
                    "the trained network diverged, and a diverged network is no fold's warm start: \
                     {}",
                    e.0
                ))
            })?;
        }
        let from = start
            .map_or("cold".to_string(), |file| vike_utility::hex::hex_lower(&Sha256::digest(file)));
        model.provenance.push_str(&format!(" warm_start={from}"));
    } else {
        model.weights = None;
    }
    Ok(model)
}

/// What a model trained with the loss word `loss` (its provenance's `loss=`) answers: a
/// probability for `binary`; one probability per class for `multiclass` (its provenance's
/// `classes=`); a value for `regression`, for `sharpe` (a position in `[-1, 1]`), for `flow` (a
/// draw of the label) and for `hedging` (the hedge to hold).
/// The binary process's `predict` declares this kind to `OnnxModel::predictor`, which checks the
/// graph's output against it; an unknown word is refused, never read as a probability.
pub fn prediction_kind(loss: &str, classes: Option<usize>) -> Result<PredictionKind, InputError> {
    match (loss, classes) {
        ("binary", _) => Ok(PredictionKind::Probability),
        ("multiclass", Some(class_count)) if class_count >= 2 => {
            Ok(PredictionKind::ClassProbabilities { class_count })
        }
        ("multiclass", _) => {
            Err(InputError("a `multiclass` model needs `classes=` (2 or more)".into()))
        }
        ("regression" | "sharpe" | "flow" | "hedging", _) => Ok(PredictionKind::Value),
        (other, _) => Err(InputError(format!("loss `{other}` names no kind of answer"))),
    }
}

/// One network trained on `rows` shaped `shape`, then exported for `export_shape`: a flow as its
/// sampler, one column wider (module doc).
fn fit<N: Network<D>, const D: usize>(
    network: N,
    rows: &Rows,
    shape: [usize; D],
    export_shape: [usize; D],
    loss: Loss,
    settings: &TrainingSettings,
    start: Option<&[u8]>,
) -> Result<TrainedModel, InputError> {
    let network = match start {
        Some(file) => with_weights(network, file)?,
        None => network,
    };
    let trained = train(network, &rows.features, shape, &rows.labels, loss, settings)?;
    let (onnx, input_shape) = if loss == Loss::SinhArcsinhFlow {
        let sampler = export_flow_sampler(&trained.network, export_shape);
        (sampler, vec![1, rows.columns + 1])
    } else {
        (export_onnx(&trained.network, export_shape, loss), export_shape.to_vec())
    };
    let onnx =
        onnx.map_err(|e| InputError(format!("the trained network does not export: {e:?}")))?;
    Ok(TrainedModel {
        onnx,
        input_shape,
        final_loss: trained.epoch_losses.last().copied().unwrap_or(f32::NAN),
        provenance: String::new(),
        weights: Some(weights_of(&trained.network)),
    })
}
