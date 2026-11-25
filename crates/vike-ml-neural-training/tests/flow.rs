//! The conditional normalising flow (`loss = "flow"`, `vike_ml_neural_training::flow`): its exported
//! sampler draws what `burn` draws, one seed is one network, the trainer PROCESS's held model draws
//! through the learner seam, and, end to end, a flow fitted on a market whose volatility clusters
//! GENERATES paths that keep that market's stylized facts where a Brownian motion with the same mean
//! and volatility loses them. The end-to-end half is `vike_analytics::synthetic`'s: the regime-switching
//! source, the next-step rows, `learned_bars` sampling the exported ONNX through the one ONNX runner,
//! and `stylized_facts`. Its comparison table prints with `--nocapture`.

use std::path::PathBuf;

use vike_analytics::synthetic::{
    StylizedFacts, SyntheticGrid, SyntheticModel, SyntheticRegime, block_bootstrap_bars,
    learned_bars, model_bars, next_step_rows, stylized_facts,
};
use vike_ml_neural_training::flow::{FLOW_PARAMETERS, sample_flow};
use vike_ml_neural_training::networks::{LstmNetworkConfig, MlpConfig};
use vike_ml_neural_training::onnx::export_flow_sampler;
use vike_ml_neural_training::trainer::{NetworkSpec, train_from_spec};
use vike_ml_neural_training::training::{Loss, TrainingSettings, train};
use vike_ml_onnx_prediction::OnnxModel;
use vike_ml_prediction::{PredictionKind, Predictor};
use vike_ml_training::train::matrix::Rows;
use vike_ml_training::{Capture, DEFAULT_POINT, GridPoint, Learner, NeuralLearner, TrainData};

use crate::common::uniform;

const WINDOW: usize = 6;
const ROWS: usize = 96;

/// `ROWS` windows of `WINDOW` values of an AR(1) (`x' = 0.6 x + shock`), and the value after each.
fn autoregressive() -> (Vec<f32>, Vec<f32>) {
    let shocks = uniform(5, ROWS + WINDOW);
    let mut series = vec![0.0_f32];
    for shock in &shocks[1..] {
        series.push(0.6 * series[series.len() - 1] + shock);
    }
    let windows = series.windows(WINDOW + 1).flat_map(|w| w[..WINDOW].to_vec()).collect();
    let labels = series.windows(WINDOW + 1).map(|w| w[WINDOW]).collect();
    (windows, labels)
}

/// Each `width`-wide row of `windows` with one draw appended, as the sampler reads it.
fn with_draws(windows: &[f32], width: usize) -> Vec<f32> {
    let draws = uniform(17, windows.len() / width);
    windows.chunks(width).zip(draws).flat_map(|(w, d)| w.iter().copied().chain([2.0 * d])).collect()
}

#[test]
fn the_exported_sampler_draws_what_burn_draws() {
    let (windows, labels) = autoregressive();
    let settings = TrainingSettings { seed: 3, epochs: 5, batch_size: 16, learning_rate: 0.01 };
    let flow = Loss::SinhArcsinhFlow;
    let mlp = MlpConfig { inputs: WINDOW, hidden: vec![8], outputs: FLOW_PARAMETERS }.init(3);
    let mlp =
        train(mlp, &windows, [ROWS, WINDOW], &labels, flow, &settings).expect("trains").network;
    let lstm = LstmNetworkConfig { inputs: 1, hidden: 4, outputs: FLOW_PARAMETERS }.init(3);
    let lstm =
        train(lstm, &windows, [ROWS, WINDOW, 1], &labels, flow, &settings).expect("trains").network;
    let rows = with_draws(&windows, WINDOW);
    let check = |onnx: Vec<u8>, drawn: Vec<f32>| {
        let sampler = OnnxModel::from_bytes(&onnx, &[1, WINDOW + 1]).expect("the sampler loads");
        assert_eq!(drawn.len(), ROWS);
        for (row, want) in rows.chunks(WINDOW + 1).zip(&drawn) {
            let got = sampler.predict(row).expect("the sampler answers")[0];
            assert!((got - want).abs() <= 1e-5 + 1e-5 * want.abs(), "{got} against {want}");
        }
    };
    check(
        export_flow_sampler(&mlp, [1, WINDOW]).expect("the MLP's sampler exports"),
        sample_flow(&mlp, &rows, [1, WINDOW]).expect("burn draws"),
    );
    check(
        export_flow_sampler(&lstm, [1, WINDOW, 1]).expect("the LSTM's sampler exports"),
        sample_flow(&lstm, &rows, [1, WINDOW, 1]).expect("burn draws"),
    );
}

fn flow_spec(epochs: usize) -> NetworkSpec {
    NetworkSpec::parse(&format!(
        "network = \"mlp\"\nhidden = [8]\nloss = \"flow\"\nepochs = {epochs}\nbatch_size = 16\n"
    ))
    .expect("the spec parses")
}

fn autoregressive_rows() -> Rows {
    let (features, labels) = autoregressive();
    Rows { features, labels, columns: WINDOW }
}

/// The sampler's answers to the seeded rows: what a flow IS, rather than its ONNX bytes, which name
/// each tensor by a process-wide counter and so differ between two exports of one network (MEASURED
/// 2026-11-17: `tensor_54` against `tensor_98`).
fn answers(model: &[u8]) -> Vec<f32> {
    let sampler = OnnxModel::from_bytes(model, &[1, WINDOW + 1]).expect("the sampler loads");
    let (windows, _) = autoregressive();
    with_draws(&windows, WINDOW)
        .chunks(WINDOW + 1)
        .map(|row| sampler.predict(row).expect("the sampler answers")[0])
        .collect()
}

#[test]
fn one_seed_is_one_flow_and_its_sampler_is_one_column_wider() {
    let rows = autoregressive_rows();
    let a = train_from_spec(&flow_spec(2), &rows, 4, 0.01, None).expect("the flow trains");
    let b = train_from_spec(&flow_spec(2), &rows, 4, 0.01, None).expect("the flow trains");
    let c = train_from_spec(&flow_spec(2), &rows, 5, 0.01, None).expect("the flow trains");
    assert_eq!((a.final_loss.to_bits(), &a.provenance), (b.final_loss.to_bits(), &b.provenance));
    assert_eq!(answers(&a.onnx), answers(&b.onnx));
    assert_ne!(answers(&a.onnx), answers(&c.onnx));
    assert_eq!(a.input_shape, vec![1, WINDOW + 1]);
    assert!(a.provenance.contains("loss=flow") && a.provenance.contains("input_shape=1,7"));
}

#[test]
fn a_flow_refuses_two_targets_per_row_and_a_misspelt_loss_is_named() {
    let (windows, labels) = autoregressive();
    let two: Vec<f32> = labels.iter().flat_map(|&y| [y, y]).collect();
    let mlp = MlpConfig { inputs: WINDOW, hidden: vec![4], outputs: FLOW_PARAMETERS }.init(0);
    let settings = TrainingSettings { seed: 0, epochs: 1, batch_size: 16, learning_rate: 0.01 };
    let error = train(mlp, &windows, [ROWS, WINDOW], &two, Loss::SinhArcsinhFlow, &settings)
        .expect_err("refused");
    assert!(error.0.contains("ONE target per row"), "{error}");
    let spec = NetworkSpec { loss: "flows".to_string(), ..flow_spec(1) };
    let error = train_from_spec(&spec, &autoregressive_rows(), 0, 0.01, None).expect_err("refused");
    assert!(error.0.contains("`flow`"), "{error}");
}

/// The trainer PROCESS, as a study host drives it: the held model answers a row of the window and a
/// draw with the bits its exported sampler gives, and refuses a row without the draw.
#[test]
fn a_flow_fitted_through_the_seam_draws_what_its_sampler_draws() {
    let rows = autoregressive_rows();
    let x: Vec<f64> = rows.features.iter().map(|&v| f64::from(v)).collect();
    let data = TrainData::new(&x, &rows.labels, WINDOW).expect("data");
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_vike-ml-neural-training"));
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("flow-seam");
    let spec = "network = \"mlp\"\nhidden = [8]\nloss = \"flow\"\nepochs = 3\nbatch_size = 16\n";
    let learner = NeuralLearner::new(&binary, &scratch, spec).expect("the trainer is built");
    let point = GridPoint { learning_rate: 0.01, ..DEFAULT_POINT };
    let want = Capture { importance: false, text: true };
    let (model, _, text) = learner.fit_captured(&data, &point, 7, want).expect("the trainer fits");
    assert_eq!(model.kind(), PredictionKind::Value);
    let sampler = OnnxModel::from_model_text(&text.expect("the model text"), &[1, WINDOW + 1])
        .expect("the captured text is the sampler");
    for row in with_draws(&rows.features, WINDOW).chunks(WINDOW + 1).take(20) {
        let wide: Vec<f64> = row.iter().map(|&v| f64::from(v)).collect();
        let held = model.predict_one(&wide).expect("the held model draws");
        assert_eq!(held, f64::from(sampler.predict(row).expect("the sampler draws")[0]));
    }
    let error = model.predict_one(&x[..WINDOW]).expect_err("a row without its draw");
    assert!(error.contains("takes 7 features"), "{error}");
}

const MINUTE: i64 = 60_000;
const BARS: usize = 6_000;
/// The flow's window of scaled log returns.
const FLOW_WINDOW: usize = 16;

fn grid() -> SyntheticGrid {
    SyntheticGrid { start_ms: 1_735_689_600_000, step_ms: MINUTE, bars: BARS }
}

/// A calm and a wild volatility, each persistent: a market whose volatility CLUSTERS and whose
/// returns have heavy tails, which a Brownian motion with its mean and volatility has neither of.
const MARKET: SyntheticModel = SyntheticModel::RegimeSwitching {
    regimes: [
        SyntheticRegime { drift: 0.0, volatility: 0.001 },
        SyntheticRegime { drift: 0.0, volatility: 0.005 },
    ],
    switch_probability: [0.01, 0.03],
};

fn table_row(name: &str, facts: &StylizedFacts) -> String {
    format!(
        "{name:<22} {:>9.6} {:>7.3} {:>8.3} {:>10.6} {:>10.6} {:>8.3} {:>8.3} {:>8.3}",
        facts.standard_deviation,
        facts.skewness,
        facts.excess_kurtosis,
        facts.quantiles[0],
        facts.quantiles[4],
        facts.autocorrelation[0],
        facts.absolute_autocorrelation[0],
        facts.absolute_autocorrelation[9],
    )
}

/// The book's chapter-5 claim, measured: a flow fitted on the market generates paths whose volatility
/// clusters and whose tails are heavy, as the market's do, and a Brownian motion fitted to the same
/// mean and volatility does not. Every path feeds a backtest the way `crates/vike-backtest/tests/bar_source.rs`
/// shows (`HistoryStoreSynthetic`), because each is a `Vec<Bar>` on its grid.
#[test]
fn a_flow_fitted_on_the_market_generates_its_volatility_clustering() {
    let source = model_bars(&MARKET, &grid(), 100.0, 1.0, 1, 21).expect("the market");
    let rows = next_step_rows(&source, FLOW_WINDOW).expect("the rows");
    let trainer_rows = Rows {
        features: rows.features.iter().map(|&v| v as f32).collect(),
        labels: rows.labels.iter().map(|&v| v as f32).collect(),
        columns: FLOW_WINDOW,
    };
    let spec = NetworkSpec::parse(
        "network = \"mlp\"\nhidden = [32, 32]\nloss = \"flow\"\nepochs = 30\nbatch_size = 128\n",
    )
    .expect("the spec parses");
    let model = train_from_spec(&spec, &trainer_rows, 3, 0.003, None).expect("the flow trains");
    let sampler =
        OnnxModel::from_bytes(&model.onnx, &model.input_shape).expect("the sampler loads");
    let (lowest, highest) = (rows.frame.lowest, rows.frame.highest);
    let clamped = std::cell::Cell::new(0_usize);
    let draw = |row: &[f64]| -> Result<f64, String> {
        let row: Vec<f32> = row.iter().map(|&v| v as f32).collect();
        let answer = f64::from(sampler.predict(&row)?[0]);
        clamped.set(clamped.get() + usize::from(!(lowest..=highest).contains(&answer)));
        Ok(answer)
    };

    let lags = 10;
    let market = stylized_facts(&source, lags);
    let (drift, volatility) =
        (rows.frame.center + rows.frame.scale * rows.frame.scale / 2.0, rows.frame.scale);
    let brownian = SyntheticModel::GeometricBrownianMotion { drift, volatility };
    let brownian = stylized_facts(&model_bars(&brownian, &grid(), 100.0, 1.0, 1, 5).unwrap(), lags);
    let bootstrap =
        stylized_facts(&block_bootstrap_bars(&source, 64, &grid(), 100.0, 5).unwrap(), lags);
    let fresh = stylized_facts(&model_bars(&MARKET, &grid(), 100.0, 1.0, 1, 5).unwrap(), lags);
    let paths = 3;
    let learned: Vec<StylizedFacts> = (0..paths)
        .map(|seed| {
            let path = learned_bars(draw, &rows.frame, &grid(), 100.0, 1.0, 1, seed as u64)
                .expect("the flow generates a path");
            stylized_facts(&path, lags)
        })
        .collect();

    println!(
        "flow: final mean negative log-likelihood {} (a standard normal law: 1.419); answers \
         clamped to the source's range: {} of {}",
        model.final_loss,
        clamped.get(),
        paths * BARS
    );
    println!(
        "{:<22} {:>9} {:>7} {:>8} {:>10} {:>10} {:>8} {:>8} {:>8}",
        "path", "std", "skew", "ex.kurt", "q01", "q99", "acf1", "|r|acf1", "|r|acf10"
    );
    println!("{}", table_row("market (source)", &market));
    println!("{}", table_row("market (fresh seed)", &fresh));
    println!("{}", table_row("block bootstrap (64)", &bootstrap));
    println!("{}", table_row("brownian, same moments", &brownian));
    for (seed, facts) in learned.iter().enumerate() {
        println!("{}", table_row(&format!("learned flow, seed {seed}"), facts));
    }

    // The flow learned the law: it beats the standard normal law of the scaled returns.
    assert!(model.final_loss < 1.35, "final loss {}", model.final_loss);
    // The source's range bounds a path, and it is a bound, not the path: under 1% of the steps.
    assert!(clamped.get() * 100 < paths * BARS, "{} steps clamped", clamped.get());
    assert!(market.absolute_autocorrelation[0] > 0.2, "{market:?}");
    assert!(brownian.absolute_autocorrelation[0].abs() < 0.05, "{brownian:?}");
    assert!(brownian.excess_kurtosis.abs() < 0.3, "{brownian:?}");
    for facts in &learned {
        let ratio = facts.standard_deviation / market.standard_deviation;
        assert!((0.5..2.0).contains(&ratio), "a learned path's volatility ran away: {facts:?}");
        assert!(facts.absolute_autocorrelation[0] > 0.1, "no volatility clustering: {facts:?}");
        assert!(facts.excess_kurtosis > 1.0, "no heavy tails: {facts:?}");
        assert!(facts.autocorrelation[0].abs() < 0.1, "a linear autocorrelation: {facts:?}");
    }
}
