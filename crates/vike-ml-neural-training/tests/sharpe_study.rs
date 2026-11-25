//! W24's consumer end to end: the committed study `sharpe_lstm` (in `vike-user-research`'s fixture
//! tree) fits a REAL LSTM on the Sharpe objective through `vike_ml_training::NeuralLearner`, which runs
//! the `vike-ml-neural-training` binary this crate builds, over a store of bars whose returns carry
//! momentum. The study's `model.onnx` is the network the trainer wrote (it loads in tract and answers
//! positions in [-1, 1]), the positions earn a positive Sharpe ratio on bars the network never saw,
//! and the study answers the same, bit for bit, across the study file's boundary, where the network
//! is HELD by the parent and crosses as the VALUE it declares (never a probability).

#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/sharpe_lstm/sharpe_lstm.rs"]
mod sharpe_lstm;

use std::path::PathBuf;
use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_marketdata::Bar;
use vike_ml_onnx_prediction::OnnxModel;
use vike_ml_training::NeuralLearner;
use vike_user_research::file::{guest, host};
use vike_user_research::{StudyContext, StudyOutcome};

use crate::common::momentum_returns;

const STEPS: usize = 8;
const BARS: i64 = 800;

/// `BARS` hourly bars whose returns are 1% times the momentum process's, and the window over them.
fn momentum_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    let store = HistoryStoreMemory::new();
    let mut close = 100.0;
    let returns = momentum_returns(51, BARS as usize);
    let bars: Vec<Bar> = (0..BARS)
        .zip(returns)
        .map(|(i, r)| {
            close *= 1.0 + 0.01 * f64::from(r);
            vike_marketdata::test_support::flat_bar_unit_volume_on("BTCUSDT", 3_600_000 * i, close)
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1h", &bars, Some("fixture")).expect("the bars");
    (Arc::new(store), TimestampRange::of(0, 3_600_000 * (BARS - 1)))
}

fn context() -> StudyContext {
    let spec = format!(
        "network = \"lstm\"\nhidden = [8]\nsteps = {STEPS}\nloss = \"sharpe\"\nturnover_cost = 0.0\n\
         epochs = 30\nbatch_size = 50\n"
    );
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_vike-ml-neural-training"));
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sharpe-study");
    let learner = NeuralLearner::new(&binary, &scratch, &spec).expect("the trainer is built");
    let (store, window) = momentum_store();
    StudyContext::new(store, window, scratch).with_learner(Arc::new(learner))
}

fn recipe() -> toml::Value {
    toml::from_str(&format!(
        "venue = \"binance\"\nsymbol = \"BTCUSDT\"\ninterval = \"1h\"\nseed = 3\nlearning_rate = 0.01\n\
         [learner]\nkind = \"neural\"\nsteps = {STEPS}\n"
    ))
    .expect("a recipe")
}

/// The study run across the study file's boundary, both halves in this process (as
/// `vike-user-research`'s pipeline test runs its fixtures).
fn across_the_boundary(ctx: StudyContext) -> StudyOutcome {
    let ctx: &'static StudyContext = Box::leak(Box::new(ctx));
    let start = host::start(ctx, &recipe()).expect("the start message");
    let parent = host::Parent::new(ctx, Arc::new(guest::serve));
    let host: guest::Host = Arc::new(move |request: &[u8]| parent.answer(request));
    host::finished(&guest::run(sharpe_lstm::run, &start, host)).expect("the study runs across")
}

#[test]
fn the_sharpe_study_trains_an_lstm_saves_its_onnx_and_earns_out_of_sample() {
    let out = sharpe_lstm::run(&context(), &recipe()).expect("the study runs");
    let artifact =
        |name: &str| out.artifacts().iter().find(|(n, _)| n == name).map(|(_, body)| body.clone());
    let text = artifact("model.onnx").expect("a network's text is saved as model.onnx");
    assert!(artifact("model.txt").is_none(), "a network is no tree model");
    let model = OnnxModel::from_model_text(&text, &[1, STEPS, 1]).expect("the saved network loads");
    let answer = model.predict(&[0.5; STEPS]).expect("the saved network answers")[0];
    assert!(answer.abs() <= 1.0, "it answers a position: {answer}");

    let metric = |name: &str| out.metric_value(name).unwrap_or(f64::NAN);
    assert!(metric("later_mean_abs_position") <= 1.0);
    assert!(
        metric("later_sharpe") > 0.1,
        "on bars it never saw the network earns a Sharpe ratio of {} per bar (holding: {})",
        metric("later_sharpe"),
        metric("later_sharpe_hold")
    );

    let across = across_the_boundary(context());
    let bits = |o: &StudyOutcome| -> Vec<(String, u64)> {
        o.metrics().iter().map(|(n, v)| (n.clone(), v.to_bits())).collect()
    };
    assert_eq!(
        bits(&out),
        bits(&across),
        "the metrics, bit for bit, the network held by the parent"
    );
    assert_eq!(out.artifacts(), across.artifacts(), "the artifacts, byte for byte");
}
