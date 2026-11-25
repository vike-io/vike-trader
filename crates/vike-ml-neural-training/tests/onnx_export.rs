//! What `burn` exports, `tract` runs: each of the five networks is exported at a fixed shape, loaded by
//! `vike-ml-onnx-prediction` (the pure-Rust runtime a strategy file predicts with, W6) and its answers
//! are compared with this crate's own `predict` on the same rows.
//!
//! The tolerance is 1e-5 absolute plus 1e-5 relative: the two runtimes compute the same f32 graph
//! with different kernels (summation order, fused multiply-add, tract's own sigmoid and tanh), so
//! equality to the bit is not a property either promises, and a wrong graph misses by orders of
//! magnitude more.

use vike_math::special_functions::logistic;
use vike_ml_neural_training::networks::{
    AutoencoderConfig, DeepLobConfig, LstmNetworkConfig, MlpConfig, Network, TcnConfig,
};
use vike_ml_neural_training::onnx::export_onnx;
use vike_ml_neural_training::training::{Loss, predict};
use vike_ml_onnx_prediction::OnnxModel;

use crate::common::uniform;

const ROWS: usize = 3;

/// `network` exported at `shape` with `loss`, run by tract on seeded rows, against `predict` on the
/// same rows (through a sigmoid for the binary loss and a tanh for the Sharpe one, which the export
/// applies).
fn tract_agrees<N: Network<D>, const D: usize>(network: &N, shape: [usize; D], loss: Loss) {
    let bytes = export_onnx(network, shape, loss).expect("the network exports");
    tract_runs_it(&bytes, network, shape, loss);
}

/// `bytes`, run by tract on seeded rows, against `network`'s `predict` on the same rows through what
/// the export applies for `loss`.
fn tract_runs_it<N: Network<D>, const D: usize>(
    bytes: &[u8],
    network: &N,
    shape: [usize; D],
    loss: Loss,
) {
    let inputs = uniform(17, shape.iter().product());
    let scores = predict(network, &inputs, shape).expect("prediction runs");
    let ours: Vec<f32> = match loss {
        // A flow's network exports its raw parameters; its sampler is `tests/flow.rs`'s.
        Loss::MeanSquaredError | Loss::SinhArcsinhFlow | Loss::Hedging { .. } => scores,
        Loss::BinaryCrossEntropy => scores.iter().map(|&s| logistic(f64::from(s)) as f32).collect(),
        Loss::Sharpe { .. } => scores.iter().map(|&s| libm::tanhf(s)).collect(),
        Loss::CrossEntropy { classes } => scores
            .chunks(classes)
            .flat_map(|row| {
                let powers: Vec<f32> = row.iter().map(|&s| libm::expf(s)).collect();
                let sum: f32 = powers.iter().sum();
                powers.into_iter().map(move |p| p / sum)
            })
            .collect(),
    };
    let theirs = OnnxModel::from_bytes(bytes, &shape)
        .and_then(|model| model.predict(&inputs))
        .expect("tract loads and runs the exported graph");
    assert_eq!(theirs.len(), ours.len(), "tract and predict return different counts");
    for (index, (&a, &b)) in theirs.iter().zip(&ours).enumerate() {
        assert!((a - b).abs() <= 1e-5 + 1e-5 * b.abs(), "answer {index}: tract {a}, ours {b}");
    }
}

#[test]
fn tract_runs_the_exported_mlp() {
    let mlp = MlpConfig { inputs: 3, hidden: vec![8, 8], outputs: 2 }.init(1);
    tract_agrees(&mlp, [ROWS, 3], Loss::MeanSquaredError);
    tract_agrees(&mlp, [ROWS, 3], Loss::BinaryCrossEntropy);
}

#[test]
fn tract_runs_the_exported_lstm() {
    let lstm = LstmNetworkConfig { inputs: 2, hidden: 8, outputs: 1 }.init(1);
    tract_agrees(&lstm, [ROWS, 5, 2], Loss::MeanSquaredError);
    tract_agrees(&lstm, [1, 5, 2], Loss::BinaryCrossEntropy);
    // The Sharpe objective's network answers its position, tanh of the score.
    tract_agrees(&lstm, [1, 5, 2], Loss::Sharpe { turnover_cost: 0.0 });
}

#[test]
fn tract_runs_the_exported_tcn() {
    let tcn = TcnConfig { inputs: 2, channels: vec![4, 6], kernel_size: 3, outputs: 1 }.init(1);
    tract_agrees(&tcn, [ROWS, 2, 7], Loss::MeanSquaredError);
}

#[test]
fn tract_runs_the_exported_deeplob() {
    // Two levels a side, 12 steps: every block, the inception module and the LSTM run on a real shape.
    let deeplob = DEEPLOB.init(1);
    tract_agrees(&deeplob, [ROWS, 1, 12, 8], Loss::CrossEntropy { classes: 3 });
    tract_agrees(&deeplob, [1, 1, 12, 8], Loss::CrossEntropy { classes: 3 });
    tract_agrees(&deeplob, [ROWS, 1, 12, 8], Loss::MeanSquaredError);
}

/// A small DeepLOB: two levels a side, every block and branch present.
const DEEPLOB: DeepLobConfig = DeepLobConfig {
    levels: 2,
    convolution_channels: 4,
    inception_channels: 3,
    hidden: 5,
    outputs: 3,
};

/// The committed DeepLOB a jailed strategy file runs (`crates/vike-backend/tests/strategy_file_onnx.rs`'s
/// `a_strategy_file_runs_deeplob_in_the_jail_and_predicts_what_it_predicts_here`): [`DEEPLOB`] as seed
/// 5 draws it, untrained, exported for ONE row of 12 steps through the softmax. A route proof needs a
/// real graph, not a trained one.
fn deeplob_fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deeplob.onnx")
}

#[test]
fn the_committed_deeplob_fixture_answers_what_its_seeded_network_answers() {
    let bytes = std::fs::read(deeplob_fixture()).expect("the committed fixture");
    tract_runs_it(&bytes, &DEEPLOB.init(5), [1, 1, 12, 8], Loss::CrossEntropy { classes: 3 });
}

#[test]
#[ignore = "writes the committed fixture: run it by hand, then review and commit the file"]
fn capture_the_deeplob_fixture() {
    let loss = Loss::CrossEntropy { classes: 3 };
    let bytes = export_onnx(&DEEPLOB.init(5), [1, 1, 12, 8], loss).expect("DeepLOB exports");
    let path = deeplob_fixture();
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("the fixtures folder");
    std::fs::write(path, bytes).expect("write the fixture");
}

#[test]
fn tract_runs_the_exported_autoencoder() {
    let autoencoder = AutoencoderConfig { inputs: 4, hidden: vec![3], code: 2 }.init(1);
    tract_agrees(&autoencoder, [ROWS, 4], Loss::MeanSquaredError);
}
