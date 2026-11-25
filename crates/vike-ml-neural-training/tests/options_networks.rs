//! Networks for options (the book's options part).
//!
//! * Deep hedging (`Loss::Hedging`): the loss is the mean square of each path's hedged P&L, a network
//!   trained on it hedges a short call out of sample far better than no hedge and close to the
//!   Black–Scholes delta, the trainer process answers the hedge as a VALUE, and a bad path is refused.
//!   The paths are a seeded geometric Brownian motion laid out by the hedging environment's own
//!   `vike_simulator::hedging_training_rows`, so the rows are the features a policy later observes.
//! * The two options STUDY files run end to end with a real network (the second half of this file):
//!   the pricing network beats one flat volatility on a smiled chain, and the deep hedge, judged in the
//!   event simulator net of its fee, beats no hedge and nears the delta hedge.

use std::path::PathBuf;

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use vike_marketdata::{Bar, OptionKind};
use vike_ml_neural_training::networks::{Mlp, MlpConfig};
use vike_ml_neural_training::trainer::NetworkSpec;
use vike_ml_neural_training::training::{Loss, Trained, TrainingSettings, predict, train};
use vike_ml_onnx_prediction::OnnxModel;
use vike_ml_prediction::{PredictionKind, Predictor};
use vike_ml_training::train::matrix::Rows;
use vike_ml_training::{Capture, DEFAULT_POINT, GridPoint, Learner, NeuralLearner, TrainData};
use vike_simulator::{HEDGE_FEATURES, HEDGE_PRICE_COLUMN, HedgedOption, hedging_training_rows};

const DAY_MS: i64 = 86_400_000;
/// Eight daily closes a path: seven hedges, then the expiry.
const STEPS: usize = 8;
/// The annual volatility of the simulated underlying.
const VOLATILITY: f64 = 0.8;

/// The short at-the-money call every test hedges, sold at its Black–Scholes price.
fn short_call() -> HedgedOption {
    let years = (STEPS - 1) as f64 / 365.0;
    let premium =
        vike_options::black_scholes_price(1.0, 1.0, years, VOLATILITY, OptionKind::Call, 0.0)
            .expect("a price");
    HedgedOption {
        right: OptionKind::Call,
        strike_ratio: 1.0,
        quantity: -1.0,
        premium_ratio: premium,
    }
}

/// `count` flat bars `step_ms` apart closing on a driftless GBM at [`VOLATILITY`] from `seed`
/// (Box–Muller over a ChaCha stream). With `restart`, the price does not move into every `restart`-th
/// bar, so each run of `restart` bars is its own path.
fn gbm_bars(seed: u64, count: usize, step_ms: i64, restart: Option<usize>) -> Vec<Bar> {
    let mut stream = ChaCha8Rng::seed_from_u64(seed);
    let mut uniform = || (f64::from(stream.next_u32()) + 1.0) / 4_294_967_297.0;
    let step = VOLATILITY * (step_ms as f64 / (365.0 * DAY_MS as f64)).sqrt();
    let mut close = 100.0;
    (0..count)
        .map(|i| {
            if i > 0 && restart.is_none_or(|every| i % every != 0) {
                let normal = (-2.0 * libm::log(uniform())).sqrt()
                    * libm::cos(2.0 * std::f64::consts::PI * uniform());
                close *= libm::exp(-0.5 * step * step + step * normal);
            }
            Bar {
                timestamp: i as i64 * step_ms,
                open: close,
                high: close,
                low: close,
                close,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

/// `paths` independent paths of [`STEPS`] daily closes, as rows: `(features, labels)`, the features
/// narrowed to `f32` for the trainer.
fn gbm_rows(seed: u64, paths: usize) -> (Vec<f32>, Vec<f32>) {
    let bars = gbm_bars(seed, paths * STEPS, DAY_MS, Some(STEPS));
    let rows = hedging_training_rows(&bars, &short_call(), STEPS, STEPS).expect("rows");
    assert_eq!(rows.paths, paths);
    (rows.features.iter().map(|&v| v as f32).collect(), rows.labels)
}

/// The mean square of the hedged P&L of each path, the hedge on row `r` being `hedge(r, row)` (each
/// path's last row unused), with `fee_rate` on each trade's notional: the objective, in `f64`.
fn mean_square(
    features: &[f32],
    labels: &[f32],
    fee_rate: f64,
    hedge: impl Fn(usize, &[f32]) -> f64,
) -> f64 {
    let rows: Vec<&[f32]> = features.chunks(HEDGE_FEATURES).collect();
    let mut total = 0.0;
    let paths = rows.len() / STEPS;
    for path in 0..paths {
        let first = path * STEPS;
        let mut pnl = f64::from(labels[first]);
        let mut before = 0.0;
        for t in first..first + STEPS - 1 {
            let price = f64::from(rows[t][HEDGE_PRICE_COLUMN]);
            let next = f64::from(rows[t + 1][HEDGE_PRICE_COLUMN]);
            let held = hedge(t, rows[t]);
            pnl += held * (next - price) - fee_rate * (held - before).abs() * price;
            before = held;
        }
        total += pnl * pnl;
    }
    total / paths as f64
}

fn hedging(fee_rate: f32) -> Loss {
    Loss::Hedging { path_steps: STEPS, price_column: HEDGE_PRICE_COLUMN, fee_rate }
}

fn mlp(seed: u64) -> Mlp {
    MlpConfig { inputs: HEDGE_FEATURES, hidden: vec![16, 16], outputs: 1 }.init(seed)
}

#[test]
fn the_first_epochs_loss_is_the_mean_square_of_the_hedged_pnl_net_of_fees() {
    let (features, labels) = gbm_rows(3, 4);
    let rows = labels.len();
    let scores = predict(&mlp(5), &features, [rows, HEDGE_FEATURES]).expect("scores");
    let by_hand = mean_square(&features, &labels, 0.002, |index, _| f64::from(scores[index]));
    // One batch of every path: the first epoch's loss is measured before the optimiser's step.
    let settings = TrainingSettings { seed: 0, epochs: 1, batch_size: 4, learning_rate: 1e-3 };
    let trained =
        train(mlp(5), &features, [rows, HEDGE_FEATURES], &labels, hedging(0.002), &settings)
            .expect("trains");
    let first = f64::from(trained.epoch_losses[0]);
    assert!(
        (first - by_hand).abs() <= 1e-4 * by_hand.abs() + 1e-9,
        "loss {first}, by hand {by_hand}"
    );
}

#[test]
fn a_trained_hedge_beats_no_hedge_and_nears_the_delta_hedge_out_of_sample() {
    let (train_features, train_labels) = gbm_rows(11, 400);
    let (test_features, test_labels) = gbm_rows(12, 200);
    let settings = TrainingSettings { seed: 7, epochs: 150, batch_size: 32, learning_rate: 0.01 };
    let shape = [train_labels.len(), HEDGE_FEATURES];
    let trained: Trained<_> =
        train(mlp(7), &train_features, shape, &train_labels, hedging(0.0), &settings)
            .expect("trains");
    let losses = &trained.epoch_losses;
    assert!(losses[losses.len() - 1] < losses[0], "the loss falls: {losses:?}");

    let rows = test_labels.len();
    let hedges = predict(&trained.network, &test_features, [rows, HEDGE_FEATURES]).expect("hedges");
    let network =
        mean_square(&test_features, &test_labels, 0.0, |index, _| f64::from(hedges[index]));
    let none = mean_square(&test_features, &test_labels, 0.0, |_, _| 0.0);
    let delta = mean_square(&test_features, &test_labels, 0.0, |_, row| {
        let (price, years) = (f64::from(row[HEDGE_PRICE_COLUMN]), f64::from(row[2]));
        vike_options::black_scholes_greeks(price, 1.0, years, VOLATILITY, OptionKind::Call, 0.0)
            .map_or(0.0, |(delta, ..)| delta)
    });
    println!(
        "hedged P&L mean square, out of sample: none {none:.6e}, delta {delta:.6e}, network {network:.6e}"
    );
    assert!(network < 0.5 * none, "the network hedges: {network} vs unhedged {none}");
    assert!(network < 3.0 * delta, "the network nears the delta hedge: {network} vs {delta}");
}

#[test]
fn a_bad_path_or_spec_is_refused_before_burn_sees_it() {
    let (features, labels) = gbm_rows(3, 2);
    let rows = labels.len();
    let ok = TrainingSettings { seed: 0, epochs: 1, batch_size: 1, learning_rate: 1e-3 };
    let refused = |features: &[f32], labels: &[f32], loss: Loss| {
        train(mlp(0), features, [labels.len(), HEDGE_FEATURES], labels, loss, &ok)
            .expect_err("refused")
            .0
    };
    let message =
        refused(&features[..(rows - 1) * HEDGE_FEATURES], &labels[..rows - 1], hedging(0.0));
    assert!(message.contains("whole paths"), "{message}");
    let mut mixed = labels.clone();
    mixed[1] += 1.0;
    assert!(refused(&features, &mixed, hedging(0.0)).contains("different labels"));
    let mut broken = features.clone();
    broken[HEDGE_FEATURES + HEDGE_PRICE_COLUMN] = 0.0;
    assert!(refused(&broken, &labels, hedging(0.0)).contains("no positive price"));
    let outside = Loss::Hedging { path_steps: STEPS, price_column: HEDGE_FEATURES, fee_rate: 0.0 };
    assert!(refused(&features, &labels, outside).contains("outside"));
    assert!(refused(&features, &labels, hedging(-0.1)).contains("not a cost"));

    let base = "network = \"mlp\"\nhidden = [4]\nepochs = 1\nbatch_size = 1\n";
    let parse = |extra: &str| NetworkSpec::parse(&format!("{base}{extra}")).expect("parses");
    let spec = parse("loss = \"hedging\"\nprice_column = 0\n");
    let read = Rows { features: vec![1.0, 0.0, 0.1], labels: vec![1.0], columns: 3 };
    let error = vike_ml_neural_training::trainer::train_from_spec(&spec, &read, 0, 0.01, None)
        .expect_err("no path_steps");
    assert!(error.0.contains("path_steps"), "{}", error.0);
    let spec = parse("loss = \"regression\"\nfee_rate = 0.001\n");
    let error = vike_ml_neural_training::trainer::train_from_spec(&spec, &read, 0, 0.01, None)
        .expect_err("refused");
    assert!(error.0.contains("only to"), "{}", error.0);
}

#[test]
fn a_hedge_fitted_through_the_seam_answers_a_value_and_its_onnx() {
    let (features, labels) = gbm_rows(21, 40);
    let x: Vec<f64> = features.iter().map(|&v| f64::from(v)).collect();
    let data = TrainData::new(&x, &labels, HEDGE_FEATURES).expect("data");
    let spec = format!(
        "network = \"mlp\"\nhidden = [8]\nloss = \"hedging\"\npath_steps = {STEPS}\n\
         price_column = {HEDGE_PRICE_COLUMN}\nfee_rate = 0.0005\nepochs = 20\nbatch_size = 8\n"
    );
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_vike-ml-neural-training"));
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hedging-process");
    let learner = NeuralLearner::new(&binary, &scratch, &spec).expect("the trainer");
    let point = GridPoint { learning_rate: 0.01, ..DEFAULT_POINT };
    let want = Capture { importance: false, text: true };
    let (model, _, text) = learner.fit_captured(&data, &point, 7, want).expect("the trainer fits");
    assert_eq!(model.kind(), PredictionKind::Value, "a hedge is a value, not a probability");
    let onnx = OnnxModel::from_model_text(&text.expect("the model text"), &[1, HEDGE_FEATURES])
        .expect("the captured text is the ONNX the trainer wrote");
    for row in x.chunks(HEDGE_FEATURES).take(STEPS) {
        let held = model.predict_one(row).expect("the held model answers");
        let floats: Vec<f32> = row.iter().map(|&v| v as f32).collect();
        assert_eq!(held, f64::from(onnx.predict(&floats).expect("the ONNX answers")[0]));
    }
}

// ---------------------------------------------------------------------------------------------
// The book's two options STUDIES with a real network: each study file, compiled into this binary as
// `vike-user-research`'s pipeline compiles it, runs directly with the host's `NeuralLearner` driving
// this crate's trainer binary, over a seeded store.

#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/option_pricing_network/option_pricing_network.rs"]
mod option_pricing_network;

#[path = "../../vike-user-research/tests/fixture_user_data/research/studies/rust/deep_hedging/deep_hedging.rs"]
mod deep_hedging;

const HOUR_MS: i64 = 3_600_000;

/// A study context over `store` whose learner is the trainer process fitting `spec`.
fn real_network(
    store: vike_data::HistoryStoreMemory,
    spec: &str,
    tag: &str,
) -> vike_user_research::StudyContext {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_vike-ml-neural-training"));
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("options-{tag}"));
    let learner = NeuralLearner::new(&binary, &scratch, spec).expect("the trainer");
    vike_user_research::StudyContext::new(
        std::sync::Arc::new(store),
        vike_data::TimestampRange::all(),
        scratch,
    )
    .with_learner(std::sync::Arc::new(learner))
}

/// A month of hourly bars of a GBM future, and every fourth hour a chain snapshot of calls and puts at
/// nine strikes from 0.8 to 1.2 of the price, 30 and 90 days out, marked by Black–Scholes on a SKEWED
/// smile whose at-the-money level drifts day by day: what one flat volatility cannot price.
fn smile_store() -> vike_data::HistoryStoreMemory {
    use vike_data::HistoryStore;
    let store = vike_data::HistoryStoreMemory::new();
    let bars = gbm_bars(31, 30 * 24, HOUR_MS, None);
    store.append_bars("deribit", "U-PERP", "1h", &bars, Some("fixture")).expect("bars");
    for h in (1..bars.len()).step_by(4) {
        let (ts, spot) = (h as i64 * HOUR_MS + HOUR_MS - 1, bars[h - 1].close);
        let level = 0.45 + 0.05 * ((h / 24) % 5) as f64;
        let mut rows = Vec::new();
        for days in [30, 90] {
            let expiry_ms = ts + days * 24 * HOUR_MS;
            let years = days as f64 / 365.0;
            for step in 0..9 {
                let strike = spot * (0.8 + 0.05 * f64::from(step));
                let m = strike / spot - 1.0;
                let iv = level + 0.8 * m * m - 0.25 * m;
                for kind in [OptionKind::Call, OptionKind::Put] {
                    rows.push(vike_data::ChainRow {
                        ts,
                        underlying: "U".into(),
                        instrument: format!("U-{days}-{step}-{kind:?}"),
                        expiry_ms,
                        strike,
                        kind,
                        bid: None,
                        ask: None,
                        mark: vike_options::black_scholes_price(spot, strike, years, iv, kind, 0.0),
                        iv: Some(iv),
                        open_interest: None,
                        volume: None,
                        delta: None,
                        gamma: None,
                        theta: None,
                        vega: None,
                    });
                }
            }
        }
        store
            .append_chain_snapshot("deribit", "U", &rows, Some(&format!("chain-{h}")))
            .expect("a snapshot");
    }
    store
}

#[test]
fn the_pricing_network_learns_the_smile_that_one_flat_volatility_misses() {
    // Measured on the CI box (2026-11-17, out of sample, the flat price's RMSE 0.00416): this net 0.00194 in
    // 46 s; [64, 64] at 0.003 0.00044 in 101 s; [64, 64, 32] for 400 epochs 0.00035 in 220 s. The
    // cheapest that clearly learns the smile is kept here; the book's recipe takes the wider one.
    let spec = "network = \"mlp\"\nhidden = [32, 32]\nloss = \"regression\"\nepochs = 300\nbatch_size = 64\n";
    let recipe: toml::Value = toml::from_str(
        "venue = \"deribit\"\nchain = \"U\"\nunderlying_venue = \"deribit\"\nunderlying = \"U-PERP\"\n\
         interval = \"1h\"\ntrain_fraction = 0.7\nlearning_rate = 0.01\nseed = 3\n",
    )
    .expect("a recipe");
    let ctx = real_network(smile_store(), spec, "pricing");
    let out = option_pricing_network::run(&ctx, &recipe).expect("the study runs");
    let metric = |name: &str| out.metric_value(name).expect(name);
    println!(
        "pricing, out of sample over {} rows: rmse network {:.6}, black-scholes {:.6}; mae network {:.6}, black-scholes {:.6}",
        metric("rows_test"),
        metric("rmse_network"),
        metric("rmse_black_scholes"),
        metric("mae_network"),
        metric("mae_black_scholes"),
    );
    assert!(
        metric("rmse_network") < 0.6 * metric("rmse_black_scholes"),
        "the network prices the smile the flat volatility misses"
    );
    assert!(metric("mae_network") < metric("mae_black_scholes"), "and on the typical option too");
    assert!(out.artifacts().iter().any(|(name, _)| name == "model.onnx"), "the network is kept");
}

#[test]
fn the_deep_hedge_beats_no_hedge_and_nears_the_delta_hedge_in_the_simulator() {
    use vike_data::HistoryStore;
    let store = vike_data::HistoryStoreMemory::new();
    let bars = gbm_bars(41, 2_400, HOUR_MS, None);
    store.append_bars("deribit", "U-PERP", "1h", &bars, Some("fixture")).expect("bars");
    let learner = "path_steps = 25\nprice_column = 0\nfee_rate = 0.0005\n";
    let spec = format!(
        "network = \"mlp\"\nhidden = [16, 16]\nloss = \"hedging\"\n{learner}epochs = 30\nbatch_size = 64\n"
    );
    let recipe: toml::Value = toml::from_str(&format!(
        "venue = \"deribit\"\nsymbol = \"U-PERP\"\ninterval = \"1h\"\nvolatility = {VOLATILITY}\n\
         train_fraction = 0.75\nlearning_rate = 0.005\nseed = 5\ncommission = 0.05\n\
         [learner]\nkind = \"neural\"\n{learner}"
    ))
    .expect("a recipe");
    let ctx = real_network(store, &spec, "hedging");
    let out = deep_hedging::run(&ctx, &recipe).expect("the study runs");
    let metric = |name: &str| out.metric_value(name).expect(name);
    println!(
        "hedging over {} episodes (P&L / first close): std none {:.6}, delta {:.6}, network {:.6}; \
         mean none {:.6}, delta {:.6}, network {:.6}",
        metric("episodes"),
        metric("none_pnl_std"),
        metric("delta_pnl_std"),
        metric("network_pnl_std"),
        metric("none_pnl_mean"),
        metric("delta_pnl_mean"),
        metric("network_pnl_mean"),
    );
    assert!(metric("network_pnl_std") < 0.6 * metric("none_pnl_std"), "the network hedges");
    assert!(metric("network_pnl_std") < 2.0 * metric("delta_pnl_std"), "near the delta hedge");
}
