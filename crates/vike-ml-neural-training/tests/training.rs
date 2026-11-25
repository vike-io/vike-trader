//! Each network learns its toy task, one seed gives one network bit for bit, and a bad input is an
//! `InputError` rather than a panic inside `burn`.

use vike_ml_neural_training::InputError;
use vike_ml_neural_training::networks::{
    AutoencoderConfig, LstmNetworkConfig, MlpConfig, Network, TcnConfig,
};
use vike_ml_neural_training::training::{Loss, Trained, TrainingSettings, encode, predict, train};

use crate::common::{momentum_returns, sharpe_of, uniform, windows_and_next};

const ROWS: usize = 96;
const STEPS: usize = 6;

/// `rows` windows of `steps` values and, per window, `1` when its sum is positive, else `0`.
fn sign_of_sum(rows: usize, steps: usize) -> (Vec<f32>, Vec<f32>) {
    let windows = uniform(11, rows * steps);
    let labels = windows
        .chunks(steps)
        .map(|window| if window.iter().sum::<f32>() > 0.0 { 1.0 } else { 0.0 })
        .collect();
    (windows, labels)
}

/// The share of `scores` (logits) on the right side of zero for `labels`.
fn accuracy(scores: &[f32], labels: &[f32]) -> f32 {
    let right =
        scores.iter().zip(labels).filter(|&(&score, &label)| (score > 0.0) == (label == 1.0));
    right.count() as f32 / labels.len() as f32
}

fn settings(seed: u64, epochs: usize) -> TrainingSettings {
    TrainingSettings { seed, epochs, batch_size: 16, learning_rate: 0.02 }
}

/// A nonlinear regression target over three features.
fn regression() -> (Vec<f32>, Vec<f32>) {
    let inputs = uniform(3, ROWS * 3);
    let targets = inputs.chunks(3).map(|row| 0.5 * row[0] - 0.3 * row[1] + row[2].abs()).collect();
    (inputs, targets)
}

/// Four features that are two latent factors mixed linearly: a two-wide code can carry them all.
fn two_factors() -> Vec<f32> {
    uniform(5, ROWS * 2)
        .chunks(2)
        .flat_map(|f| [f[0], f[1], f[0] + f[1], 0.5 * f[0] - f[1]])
        .collect()
}

fn mlp(seed: u64) -> Trained<impl Network<2>> {
    let (inputs, targets) = regression();
    let network = MlpConfig { inputs: 3, hidden: vec![16], outputs: 1 }.init(seed);
    train(network, &inputs, [ROWS, 3], &targets, Loss::MeanSquaredError, &settings(seed, 40))
        .expect("the regression trains")
}

fn lstm(seed: u64) -> Trained<impl Network<3>> {
    let (windows, labels) = sign_of_sum(ROWS, STEPS);
    let network = LstmNetworkConfig { inputs: 1, hidden: 8, outputs: 1 }.init(seed);
    let shape = [ROWS, STEPS, 1];
    train(network, &windows, shape, &labels, Loss::BinaryCrossEntropy, &settings(seed, 40))
        .expect("the LSTM trains")
}

fn tcn(seed: u64) -> Trained<impl Network<3>> {
    let (windows, labels) = sign_of_sum(ROWS, STEPS);
    let network =
        TcnConfig { inputs: 1, channels: vec![8, 8], kernel_size: 3, outputs: 1 }.init(seed);
    // One input channel, so `[rows, 1, steps]` holds the windows in the same order as `[rows, steps, 1]`.
    let shape = [ROWS, 1, STEPS];
    train(network, &windows, shape, &labels, Loss::BinaryCrossEntropy, &settings(seed, 40))
        .expect("the TCN trains")
}

fn autoencoder(seed: u64) -> Trained<vike_ml_neural_training::networks::Autoencoder> {
    let rows = two_factors();
    let network = AutoencoderConfig { inputs: 4, hidden: vec![], code: 2 }.init(seed);
    train(network, &rows, [ROWS, 4], &rows, Loss::MeanSquaredError, &settings(seed, 60))
        .expect("the autoencoder trains")
}

fn first_and_last(losses: &[f32]) -> (f32, f32) {
    (losses[0], *losses.last().expect("at least one epoch"))
}

#[test]
fn the_mlp_fits_a_nonlinear_regression() {
    let trained = mlp(1);
    let (first, last) = first_and_last(&trained.epoch_losses);
    assert!(last < first / 5.0, "the MLP's loss went {first} -> {last}");
}

#[test]
fn the_lstm_learns_the_sign_of_a_window_sum() {
    let trained = lstm(1);
    let (windows, labels) = sign_of_sum(ROWS, STEPS);
    let scores = predict(&trained.network, &windows, [ROWS, STEPS, 1]).expect("prediction runs");
    let (first, last) = first_and_last(&trained.epoch_losses);
    assert!(last < first, "the LSTM's loss went {first} -> {last}");
    assert!(
        accuracy(&scores, &labels) >= 0.8,
        "the LSTM is right on {}",
        accuracy(&scores, &labels)
    );
}

#[test]
fn the_tcn_learns_the_sign_of_a_window_sum() {
    let trained = tcn(1);
    let (windows, labels) = sign_of_sum(ROWS, STEPS);
    let scores = predict(&trained.network, &windows, [ROWS, 1, STEPS]).expect("prediction runs");
    let (first, last) = first_and_last(&trained.epoch_losses);
    assert!(last < first, "the TCN's loss went {first} -> {last}");
    assert!(
        accuracy(&scores, &labels) >= 0.8,
        "the TCN is right on {}",
        accuracy(&scores, &labels)
    );
}

#[test]
fn the_autoencoder_compresses_two_factors_into_a_two_wide_code() {
    let trained = autoencoder(1);
    let (first, last) = first_and_last(&trained.epoch_losses);
    assert!(last < first / 10.0, "the autoencoder's loss went {first} -> {last}");
    let code = encode(&trained.network, &two_factors(), [ROWS, 4]).expect("encoding runs");
    assert_eq!(code.len(), ROWS * 2, "one two-wide code per row");
}

/// The same pair of seeds gives the same losses and the same predictions, BIT for bit; another
/// seed gives other losses (the seed is not ignored).
#[test]
fn one_seed_gives_one_network_bit_for_bit() {
    let (inputs, _) = regression();
    let (windows, _) = sign_of_sum(ROWS, STEPS);
    let factors = two_factors();
    let bits = |values: Vec<f32>| values.into_iter().map(f32::to_bits).collect::<Vec<_>>();

    let (a, b) = (mlp(7), mlp(7));
    assert_eq!(bits(a.epoch_losses.clone()), bits(b.epoch_losses.clone()), "MLP losses");
    let predict_mlp = |t: &Trained<_>| predict(&t.network, &inputs, [ROWS, 3]).expect("runs");
    assert_eq!(bits(predict_mlp(&a)), bits(predict_mlp(&b)), "MLP predictions");
    assert_ne!(a.epoch_losses, mlp(8).epoch_losses, "the MLP ignored its seed");

    let (a, b) = (lstm(7), lstm(7));
    assert_eq!(bits(a.epoch_losses.clone()), bits(b.epoch_losses.clone()), "LSTM losses");
    let predict_lstm =
        |t: &Trained<_>| predict(&t.network, &windows, [ROWS, STEPS, 1]).expect("runs");
    assert_eq!(bits(predict_lstm(&a)), bits(predict_lstm(&b)), "LSTM predictions");
    assert_ne!(a.epoch_losses, lstm(8).epoch_losses, "the LSTM ignored its seed");

    let (a, b) = (tcn(7), tcn(7));
    assert_eq!(bits(a.epoch_losses.clone()), bits(b.epoch_losses.clone()), "TCN losses");
    let predict_tcn =
        |t: &Trained<_>| predict(&t.network, &windows, [ROWS, 1, STEPS]).expect("runs");
    assert_eq!(bits(predict_tcn(&a)), bits(predict_tcn(&b)), "TCN predictions");
    assert_ne!(a.epoch_losses, tcn(8).epoch_losses, "the TCN ignored its seed");

    let (a, b) = (autoencoder(7), autoencoder(7));
    assert_eq!(bits(a.epoch_losses.clone()), bits(b.epoch_losses.clone()), "autoencoder losses");
    let code = |t: &Trained<_>| encode(&t.network, &factors, [ROWS, 4]).expect("runs");
    assert_eq!(bits(code(&a)), bits(code(&b)), "autoencoder codes");
    assert_ne!(a.epoch_losses, autoencoder(8).epoch_losses, "the autoencoder ignored its seed");
}

/// The backend's generator is one static per process: networks initialised on several threads at
/// once must still each get their own seed's weights (the crate's initialisation lock).
#[test]
fn concurrent_initialisation_keeps_each_seed_its_own() {
    let (inputs, _) = regression();
    let config = MlpConfig { inputs: 3, hidden: vec![32, 32], outputs: 1 };
    let scores = |seed: u64| {
        let network = config.init(seed);
        predict(&network, &inputs, [ROWS, 3]).expect("prediction runs")
    };
    let alone: Vec<Vec<f32>> = (0..8).map(scores).collect();
    let together: Vec<Vec<f32>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8).map(|seed| scope.spawn(move || scores(seed))).collect();
        handles.into_iter().map(|handle| handle.join().expect("no thread panics")).collect()
    });
    assert_eq!(alone, together);
}

#[test]
fn a_bad_input_is_refused_before_burn_sees_it() {
    let (inputs, targets) = regression();
    let network = || MlpConfig { inputs: 3, hidden: vec![4], outputs: 1 }.init(0);
    let ok = settings(0, 1);
    let refused = |result: Result<Trained<_>, InputError>| result.expect_err("refused").0;

    let message =
        refused(train(network(), &inputs, [ROWS, 4], &targets, Loss::MeanSquaredError, &ok));
    assert!(message.contains("does not hold"), "{message}");
    let message =
        refused(train(network(), &inputs, [ROWS, 3], &targets[1..], Loss::MeanSquaredError, &ok));
    assert!(message.contains("do not split"), "{message}");
    let message =
        refused(train(network(), &inputs, [ROWS, 3], &targets, Loss::BinaryCrossEntropy, &ok));
    assert!(message.contains("0 or 1"), "{message}");
    let none = TrainingSettings { batch_size: 0, ..ok };
    let message =
        refused(train(network(), &inputs, [ROWS, 3], &targets, Loss::MeanSquaredError, &none));
    assert!(message.contains("at least 1"), "{message}");
    let flat = TrainingSettings { learning_rate: f64::NAN, ..ok };
    let message =
        refused(train(network(), &inputs, [ROWS, 3], &targets, Loss::MeanSquaredError, &flat));
    assert!(message.contains("not positive"), "{message}");
    assert!(predict(&network(), &[], [0, 3]).is_err(), "an empty batch is refused");
}

// ---- the Sharpe objective (W24) ---------------------------------------------------------------

/// Rows of the momentum process: `SHARPE_TRAIN` rows to fit, then `SHARPE_TEST` later rows.
const SHARPE_STEPS: usize = 8;
const SHARPE_TRAIN: usize = 400;
const SHARPE_TEST: usize = 200;

fn sharpe_rows() -> (Vec<f32>, Vec<f32>) {
    windows_and_next(&momentum_returns(21, SHARPE_TRAIN + SHARPE_TEST + SHARPE_STEPS), SHARPE_STEPS)
}

/// An LSTM fitted on the first rows with the Sharpe objective; `cost` per unit traded.
fn sharpe_lstm(seed: u64, cost: f32) -> Trained<impl Network<3>> {
    let (windows, next) = sharpe_rows();
    let network = LstmNetworkConfig { inputs: 1, hidden: 8, outputs: 1 }.init(seed);
    let shape = [SHARPE_TRAIN, SHARPE_STEPS, 1];
    let settings = TrainingSettings { seed, epochs: 30, batch_size: 50, learning_rate: 0.01 };
    let loss = Loss::Sharpe { turnover_cost: cost };
    train(
        network,
        &windows[..SHARPE_TRAIN * SHARPE_STEPS],
        shape,
        &next[..SHARPE_TRAIN],
        loss,
        &settings,
    )
    .expect("the Sharpe objective trains")
}

/// The positions (`tanh` of the scores) a network holds on the LATER rows, and their next returns.
fn later_positions<N: Network<3>>(network: &N) -> (Vec<f32>, Vec<f32>) {
    let (windows, next) = sharpe_rows();
    let scores =
        predict(network, &windows[SHARPE_TRAIN * SHARPE_STEPS..], [SHARPE_TEST, SHARPE_STEPS, 1])
            .expect("prediction runs");
    (scores.iter().map(|&s| libm::tanhf(s)).collect(), next[SHARPE_TRAIN..].to_vec())
}

/// The network finds the momentum: its loss (minus the Sharpe ratio) falls, and on rows it never saw
/// its positions earn a clearly positive Sharpe ratio. One seed repeats bit for bit.
#[test]
fn the_sharpe_objective_finds_the_momentum_and_one_seed_repeats() {
    let trained = sharpe_lstm(3, 0.0);
    let (first, last) = first_and_last(&trained.epoch_losses);
    assert!(last < first, "minus the Sharpe ratio went {first} -> {last}");
    let (positions, next) = later_positions(&trained.network);
    assert!(positions.iter().all(|p| p.abs() <= 1.0), "a position is in [-1, 1]");
    let sharpe = sharpe_of(&positions, &next);
    assert!(sharpe > 0.2, "out of sample the positions earn a Sharpe ratio of {sharpe} per period");

    let again = sharpe_lstm(3, 0.0);
    let bits = |values: &[f32]| values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&trained.epoch_losses), bits(&again.epoch_losses), "one seed, one run");
    assert_ne!(trained.epoch_losses, sharpe_lstm(4, 0.0).epoch_losses, "the seed is not ignored");
}

/// A turnover cost makes the network trade less: the summed change of position over the later rows
/// falls when each unit traded costs.
#[test]
fn a_turnover_cost_makes_the_network_trade_less() {
    let traded = |cost: f32| {
        let (positions, _) = later_positions(&sharpe_lstm(5, cost).network);
        positions.windows(2).map(|pair| (pair[1] - pair[0]).abs()).sum::<f32>()
    };
    let (free, costly) = (traded(0.0), traded(0.5));
    assert!(costly < free, "trading cost 0.5 per unit traded {costly}, free traded {free}");
}

#[test]
fn the_sharpe_objective_refuses_a_single_row_batch_and_a_negative_cost() {
    let (windows, next) = sharpe_rows();
    let network = || LstmNetworkConfig { inputs: 1, hidden: 2, outputs: 1 }.init(0);
    let shape = [SHARPE_TRAIN, SHARPE_STEPS, 1];
    let (inputs, targets) = (&windows[..SHARPE_TRAIN * SHARPE_STEPS], &next[..SHARPE_TRAIN]);
    let one = TrainingSettings { seed: 0, epochs: 1, batch_size: 1, learning_rate: 0.01 };
    let free = Loss::Sharpe { turnover_cost: 0.0 };
    let message = train(network(), inputs, shape, targets, free, &one).expect_err("refused").0;
    assert!(message.contains("batch_size of at least 2"), "{message}");
    let ok = TrainingSettings { batch_size: 8, ..one };
    let negative = Loss::Sharpe { turnover_cost: -0.1 };
    let message = train(network(), inputs, shape, targets, negative, &ok).expect_err("refused").0;
    assert!(message.contains("is not a cost"), "{message}");
}
