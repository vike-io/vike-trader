//! **Research-side neural-network training on the CPU, seeded, with an ONNX export.**
//!
//! Five small networks ([`networks`]): a multilayer perceptron, an LSTM over a window of steps, a
//! temporal convolutional network, an autoencoder whose code is the latent factors of its input, and
//! DeepLOB, which reads a window of order-book rows. [`training::train`] fits any of them with Adam on
//! mini-batches, against a squared error, a binary or a multiclass cross-entropy, or DIRECTLY a Sharpe
//! ratio of the positions it outputs ([`training::Loss::Sharpe`], W24); [`training::predict`] runs
//! one; [`onnx::export_onnx`] writes a trained one as an ONNX model, so a runtime that never links this
//! crate (`vike-ml-onnx-prediction`, `tract`) can evaluate it; all five export. [`compress`] makes a
//! trained network cheaper to ask (a distilled student, an int8 graph), W25.
//!
//! [`flow`] is the GENERATIVE half: any of the three predicting networks can learn the LAW of its
//! target (a conditional normalising flow, [`training::Loss::SinhArcsinhFlow`]) rather than one value
//! of it, and [`onnx::export_flow_sampler`] exports the sampler, which turns one standard normal draw
//! into a draw of the target. Sampled step by step over a window of returns it generates synthetic
//! price paths (`vike_analytics::synthetic::learned_bars`).
//!
//! [`trainer`] is the work of the binary `vike-ml-neural-training`, the separate process a study host drives
//! the way it drives LightGBM: the study asks its learner seam, the host's
//! `vike_ml_training::NeuralLearner` runs the binary OUTSIDE the jail, and the model comes back as ONNX
//! (decision 0292).
//!
//! # The contract
//!
//! * **Research only.** No strategy file and no binary that trades links this crate, and that is
//!   held by name, not by habit: `crates/vike-ops/tests/architecture/named_run_closure_gate/gate.rs`'s
//!   `the_neural_trainer_enters_no_strategy_file_and_no_order_signing_binary`
//!   (`docs/decisions/0292-neural-networks-train-research-side-on-burn-and-nothing-that-trades-links-the-trainer.md`).
//!   A strategy predicts from an exported model; it never trains. A study reaches training only through
//!   the separate trainer process, so `burn` is in no study build and no sandbox either.
//! * **Plain numbers in and out.** Inputs are row-major `f32` with an explicit shape whose first axis
//!   is the row (the sample); outputs are row-major `f32`, `[rows, outputs]`, RAW scores with no output
//!   activation (a binary classifier returns logits). The one type a caller meets from `burn` is the
//!   network itself, a `burn` module.
//! * **One seed, one network, on one box.** The seed fixes the initial weights and the order of every
//!   epoch's mini-batches. The backend runs on ONE thread (`flex` without `rayon`, root `Cargo.toml`),
//!   so a run repeats bit for bit on the same machine. It does NOT repeat bit for bit ACROSS platforms:
//!   `burn` computes `exp`/`tanh`/sigmoid with the platform's float methods, which decision 0032 keeps
//!   out of vike's OWN code (this crate calls no transcendental itself). The trained weights, not the
//!   run, are the artifact a prediction reads.
//! * **Two pieces of `burn` state are process-global, and this crate owns both.** The backend's random
//!   generator is one static per process, so `seeded` seeds it and initialises a network under one
//!   lock: two networks built at once on two threads still get their own seed's weights. And `burn`
//!   reads a runtime configuration on first use from a `burn.toml` in the working directory (or a
//!   parent) and from `BURN_*` environment variables; `device` pins it to the defaults before any
//!   tensor exists, so neither is read (decision 0111). Code in the same process that touches `burn`
//!   BEFORE this crate does can defeat the pin; nothing else here links `burn`.
//! * **Shapes are validated at the door.** A shape that disagrees with its data, an empty batch or a
//!   non-binary target for the binary loss is an [`InputError`], never a panic inside `burn`. A network
//!   CONFIG is code, not data: a zero width is a programmer error and asserts.
//!
//! The library reads no file, no environment and no clock, and nothing logs; the binary reads the files
//! its command line names.

pub mod compress;
pub mod flow;
pub mod networks;
pub mod onnx;
pub mod trainer;
pub mod training;
pub mod weights;

use std::sync::{Mutex, Once, PoisonError};

use burn::BurnConfig;
use burn::module::{Module, ModuleVisitor, Param};
use burn::tensor::{Device, Tensor};
use burn_std::config::RuntimeConfig;

/// The refusal of an input this crate cannot train on or run: the reason, in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError(pub String);

impl std::fmt::Display for InputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InputError {}

/// The CPU device every tensor here lives on — with gradients when `gradients` is true (training),
/// without them for prediction. The first call pins `burn`'s runtime configuration (module doc).
pub(crate) fn device(gradients: bool) -> Device {
    static PIN: Once = Once::new();
    PIN.call_once(|| {
        // `false` means something read or set it first; there is nothing left to pin then.
        let _ = BurnConfig::try_set(BurnConfig::default());
    });
    if gradients { Device::flex().autodiff() } else { Device::flex() }
}

/// Builds a network on the training device with the backend's generator seeded by `seed`, under the
/// one lock that makes the seed this network's own (module doc: the generator is process-global).
///
/// ⚠ `burn` initialises a parameter LAZILY, on its first read, so building the module draws nothing:
/// every parameter is read here, inside the lock. MEASURED 2026-11-16 without it: the same seed gave
/// different first-epoch losses while other tests built networks on other threads.
pub(crate) fn seeded<T: Module>(seed: u64, build: impl FnOnce(&Device) -> T) -> T {
    /// Reads every float parameter once, which runs its pending initialiser.
    struct Materialise;
    impl ModuleVisitor for Materialise {
        fn visit_float<const D: usize>(&mut self, parameter: &Param<Tensor<D>>) {
            let _initialised = parameter.val();
        }
    }
    static INITIALISATION: Mutex<()> = Mutex::new(());
    let _guard = INITIALISATION.lock().unwrap_or_else(PoisonError::into_inner);
    let device = device(true);
    device.seed(seed);
    let network = build(&device);
    network.visit(&mut Materialise);
    network
}
