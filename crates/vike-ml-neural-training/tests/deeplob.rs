//! DeepLOB on SYNTHETIC books: the Cont-Stoikov-Talreja queue model (`vike_analytics::synthetic`)
//! simulates a book event by event, `vike_analytics::book_export` turns it into DeepLOB's windows and
//! three-class labels, and a network trained on the first part of the stream predicts the rest better
//! than the classes' own frequencies do. In that model the mid's next move leans away from the thinner
//! best queue (Cont, Stoikov and Talreja, 2010, section 4), which is what there is to learn.

use vike_analytics::book_export::{
    BookExportParameters, BookLabel, BookSampling, BookWindows, book_windows,
    export_book_training_data,
};
use vike_analytics::synthetic::{SyntheticOrderBookModel, order_book_stream};
use vike_ml_neural_training::networks::DeepLobConfig;
use vike_ml_neural_training::training::{Loss, TrainingSettings, predict, train};
use vike_ml_training::calibration::{LOG_LOSS_CLIP, log_loss_of};

/// Levels a side, steps a window, and events to the label.
const LEVELS: usize = 3;
const STEPS: usize = 10;
const HORIZON: usize = 20;

/// Three hours of the paper's own Table 2 book, as DeepLOB's windows (about 45 events a minute).
fn synthetic_windows() -> BookWindows {
    let model = SyntheticOrderBookModel::cont_stoikov_talreja();
    let stream =
        order_book_stream(&model, 10_000.0, 0, 3 * 60 * 60_000, 11).expect("the model runs");
    let parameters = BookExportParameters {
        levels: LEVELS,
        sampling: BookSampling::EveryEvent,
        horizon_events: HORIZON,
        label: BookLabel::MidAfter,
        up_threshold: 0.0,
        down_threshold: 0.0,
    };
    let rows = export_book_training_data(&stream.updates, &parameters).expect("the book exports");
    book_windows(&rows, STEPS).expect("the windows build")
}

/// The mean log loss of class probabilities `probabilities` (`classes` a row) against `labels`, each
/// row the workspace's one log loss, clipped as a classifier's is.
fn log_loss(probabilities: &[f32], labels: &[usize], classes: usize) -> f64 {
    let sum: f64 = probabilities
        .chunks(classes)
        .zip(labels)
        .map(|(row, &label)| log_loss_of(f64::from(row[label]), LOG_LOSS_CLIP))
        .sum();
    sum / labels.len() as f64
}

/// Each row's softmax (the probabilities the exported model answers).
fn softmax(scores: &[f32], classes: usize) -> Vec<f32> {
    scores
        .chunks(classes)
        .flat_map(|row| {
            let largest = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let powers: Vec<f32> = row.iter().map(|&s| libm::expf(s - largest)).collect();
            let sum: f32 = powers.iter().sum();
            powers.into_iter().map(move |p| p / sum)
        })
        .collect()
}

#[test]
fn deeplob_learns_the_queue_imbalance_of_synthetic_books() {
    let windows = synthetic_windows();
    let rows = windows.rows();
    assert!(rows > 5_000, "too few windows: {rows}");
    let width = STEPS * 4 * LEVELS;
    let features: Vec<f32> = windows.features.iter().map(|&v| v as f32).collect();
    // DeepLOB's classes: 0 down, 1 flat, 2 up.
    let labels: Vec<usize> = windows.labels.iter().map(|&l| (l + 1) as usize).collect();
    let split = rows * 7 / 10;
    let classes = 3;

    let config = DeepLobConfig {
        levels: LEVELS,
        convolution_channels: 4,
        inception_channels: 4,
        hidden: 8,
        outputs: classes,
    };
    let targets: Vec<f32> = labels[..split].iter().map(|&l| l as f32).collect();
    let settings = TrainingSettings { seed: 3, epochs: 4, batch_size: 64, learning_rate: 0.003 };
    let trained = train(
        config.init(3),
        &features[..split * width],
        [split, 1, STEPS, 4 * LEVELS],
        &targets,
        Loss::CrossEntropy { classes },
        &settings,
    )
    .expect("DeepLOB trains");

    let test = rows - split;
    let scores =
        predict(&trained.network, &features[split * width..], [test, 1, STEPS, 4 * LEVELS])
            .expect("DeepLOB predicts");
    let network = log_loss(&softmax(&scores, classes), &labels[split..], classes);
    // The baseline: every test row given the TRAINING rows' class frequencies.
    let mut counts = [0usize; 3];
    labels[..split].iter().for_each(|&l| counts[l] += 1);
    let prior: Vec<f32> = counts.iter().map(|&c| c as f32 / split as f32).collect();
    let baseline = log_loss(&prior.repeat(test), &labels[split..], classes);
    println!(
        "DeepLOB on {split} synthetic windows: losses {:?}; held-out log loss {network:.4} against \
         {baseline:.4} for the class frequencies {counts:?}",
        trained.epoch_losses
    );
    assert!(
        network < baseline - 0.01,
        "the network must beat the class frequencies on held-out windows: {network} vs {baseline}"
    );
}

#[test]
fn a_multiclass_target_outside_its_classes_is_refused() {
    let config = DeepLobConfig {
        levels: 1,
        convolution_channels: 2,
        inception_channels: 2,
        hidden: 2,
        outputs: 3,
    };
    let inputs = vec![0.5_f32; 2 * 4 * 4];
    let settings = TrainingSettings { seed: 0, epochs: 1, batch_size: 2, learning_rate: 0.01 };
    for targets in [[0.0, 3.0], [0.5, 1.0], [-1.0, 0.0]] {
        let refused = train(
            config.init(0),
            &inputs,
            [2, 1, 4, 4],
            &targets,
            Loss::CrossEntropy { classes: 3 },
            &settings,
        );
        assert!(refused.is_err(), "{targets:?} must be refused");
    }
}
