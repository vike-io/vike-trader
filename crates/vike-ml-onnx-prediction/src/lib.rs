//! vike-ml-onnx-prediction — a trained NEURAL NETWORK, read from its ONNX bytes and asked for
//! predictions, in pure Rust (`tract`), and nothing else.
//!
//! # The contract
//!
//! * **One input, one shape, chosen by the caller.** [`OnnxModel::from_bytes`] takes the shape of the
//!   ONE float (`f32`) input the caller will feed and optimises the graph for exactly that shape. A
//!   dimension the model leaves symbolic (a batch axis exported as `N`) takes the caller's value; a
//!   dimension the model fixes must AGREE, or the load is refused: a network fed a shape it was not
//!   exported for answers a plausible number to the wrong question. A model with more than one input
//!   is refused.
//! * **The answer is the FIRST output**, flattened row-major ([`OnnxModel::predict`]). A model whose
//!   first output is not `f32` is refused at the call.
//! * **The one prediction trait.** [`OnnxModel::predictor`] declares what that output MEANS
//!   (`vike_ml_prediction::PredictionKind`: a probability, K class probabilities, a value) and answers
//!   `vike_ml_prediction::Predictor`, as a gradient-boosted model does, so a strategy treats every
//!   model kind the same way. A network whose output is none of those (a hidden state, a
//!   reconstruction) keeps [`OnnxModel::predict`].
//! * **The model reaches a strategy file as TEXT.** A file reads no file: its host hands it the
//!   run's model for a `type = "model"` key (`vike_strategy::file`'s format note), and a run's
//!   `model.onnx` crosses as the base64 (STANDARD alphabet, padded) of its bytes
//!   (`vike_runs::read_model_text`). [`OnnxModel::from_model_text`] decodes that.
//! * **No process spawn, no file read or write, no network, no thread.** The weights must be inside
//!   the ONNX bytes: a model whose tensors live in external data files is refused (the loader would
//!   have to open them). tract runs one call on the calling thread.
//!
//! # What it does NOT promise
//!
//! * **Bit-identity across machines.** tract picks its matrix kernels by the CPU's instruction set
//!   (FMA, AVX-512) and its own approximations for sigmoid and tanh, not the `libm` crate
//!   (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` governs the
//!   workspace's own maths, and a network is not). One machine answers the same bits every time; two
//!   machines agree to `f32` rounding. `tests/onnx_models.rs` holds the answers to a reference forward
//!   pass computed from the weights.
//! * **Freedom from the environment outside the jail.** tract reads `TRACT_*` variables (its kernel
//!   knobs). A strategy file runs in the jail, whose environment is EMPTY
//!   (`vike_sandbox::spawn_strategy_process`), so nothing configures it there; nothing in the
//!   workspace sets one, and no setting may (decision 0111).
//!
//! # Why a crate of its own
//!
//! `vike-ml-prediction` (the gradient-boosted trees and the `Predictor` trait) is in the normal
//! closure of the binary that carries the gateway daemon; this crate is not, and must not be: the
//! manifest says why and which gate holds it. It names `vike-ml-prediction` (for the trait), never
//! the other way round.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tract_onnx::prelude::*;
use vike_ml_prediction::{PredictionKind, Predictor};

/// A loaded ONNX network, optimised for one input shape and ready to predict. Cheap to clone (the
/// plan is shared), `Send + Sync`.
#[derive(Clone)]
pub struct OnnxModel {
    plan: Arc<TypedRunnableModel>,
    input_shape: Vec<usize>,
}

impl std::fmt::Debug for OnnxModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxModel").field("input_shape", &self.input_shape).finish_non_exhaustive()
    }
}

impl OnnxModel {
    /// The network in `bytes` (an ONNX `ModelProto`), optimised for an input of `input_shape`.
    pub fn from_bytes(bytes: &[u8], input_shape: &[usize]) -> Result<OnnxModel, String> {
        let refused = |e: TractError| format!("the ONNX model does not load: {e:#}");
        let model = tract_onnx::onnx().model_for_read(&mut &bytes[..]).map_err(refused)?;
        let inputs = model.input_outlets().map_err(refused)?.len();
        if inputs != 1 {
            return Err(format!("the ONNX model has {inputs} inputs; a model run here has ONE"));
        }
        let probe = Tensor::zero::<f32>(input_shape).map_err(refused)?;
        if !model.input_fact(0).map_err(refused)?.shape.matches(&probe, None).map_err(refused)? {
            return Err(format!(
                "the ONNX model declares its input as {:?}, which shape {input_shape:?} contradicts",
                model.input_fact(0).map_err(refused)?.shape
            ));
        }
        let plan = model
            .with_input_fact(0, f32::fact(input_shape).into())
            .and_then(|model| model.into_optimized())
            .and_then(|model| model.into_runnable())
            .map_err(|e| format!("the ONNX model does not run on shape {input_shape:?}: {e:#}"))?;
        Ok(OnnxModel { plan, input_shape: input_shape.to_vec() })
    }

    /// The network a host handed a strategy file as TEXT: the base64 of the ONNX bytes (see the crate
    /// doc), surrounding whitespace ignored.
    pub fn from_model_text(text: &str, input_shape: &[usize]) -> Result<OnnxModel, String> {
        let bytes = STANDARD.decode(text.trim()).map_err(|e| {
            format!("the model text is not the base64 of an ONNX model (a run's `model.onnx`): {e}")
        })?;
        OnnxModel::from_bytes(&bytes, input_shape)
    }

    /// The input shape the model was loaded for.
    pub fn input_shape(&self) -> &[usize] {
        &self.input_shape
    }

    /// The first output for `input`, both row-major; `input` holds exactly the input shape's element
    /// count.
    pub fn predict(&self, input: &[f32]) -> Result<Vec<f32>, String> {
        let tensor = Tensor::from_shape(&self.input_shape, input)
            .map_err(|e| format!("the input does not fit shape {:?}: {e:#}", self.input_shape))?;
        let outputs = self
            .plan
            .run(tvec!(tensor.into()))
            .map_err(|e| format!("the ONNX model failed to run: {e:#}"))?;
        let first = outputs.first().ok_or("the ONNX model returned no output")?;
        let view = first
            .to_plain_array_view::<f32>()
            .map_err(|e| format!("the ONNX model's first output is not f32: {e:#}"))?;
        Ok(view.iter().copied().collect())
    }

    /// This network as a [`Predictor`] whose first output MEANS `kind` (a probability, K class
    /// probabilities, or a value): the caller's word, because an ONNX graph does not say what its
    /// numbers mean. Checked once, on a zero input: the first output must hold exactly
    /// `kind.output_count()` numbers, so a network loaded for a batch, or a multi-output one (an LSTM's
    /// hidden state, an autoencoder), is refused here and stays with [`OnnxModel::predict`].
    pub fn predictor(self, kind: PredictionKind) -> Result<OnnxPredictor, String> {
        let zeros = vec![0.0; self.input_shape.iter().product()];
        let outputs = self.predict(&zeros)?.len();
        if outputs != kind.output_count() {
            return Err(format!(
                "the ONNX model's first output holds {outputs} numbers; a {kind:?} prediction is {}",
                kind.output_count()
            ));
        }
        Ok(OnnxPredictor { model: self, kind })
    }
}

/// An [`OnnxModel`] whose first output means a declared [`PredictionKind`]: the one prediction trait
/// every model kind answers through ([`Predictor`], beside `vike_ml_prediction::GbdtModel`'s), so a
/// strategy holds a `Box<dyn Predictor>` whatever trained it. A row is the network's whole input,
/// flattened row-major and rounded to `f32` (the network's own precision); the answer is widened back.
#[derive(Clone, Debug)]
pub struct OnnxPredictor {
    model: OnnxModel,
    kind: PredictionKind,
}

impl Predictor for OnnxPredictor {
    fn kind(&self) -> PredictionKind {
        self.kind
    }

    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        if out.len() != self.kind.output_count() {
            return Err(format!(
                "{} slots for a {:?} prediction of {} numbers",
                out.len(),
                self.kind,
                self.kind.output_count()
            ));
        }
        let input: Vec<f32> = row.iter().map(|&value| value as f32).collect();
        let answer = self.model.predict(&input)?;
        if answer.len() != out.len() {
            return Err(format!(
                "the network answered {} numbers, not {}",
                answer.len(),
                out.len()
            ));
        }
        out.iter_mut().zip(answer).for_each(|(slot, value)| *slot = f64::from(value));
        Ok(())
    }
}
