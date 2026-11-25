//! The conformal threshold by hand, and the coverage guarantee measured over many seeded draws.

use super::*;
use crate::linear::{LinearSpec, fit_linear};
use vike_ml_prediction::linear::{LinearLink, LinearModel};
use vike_ml_prediction::{Predictor, text_format};
use vike_model::test_support::corpus::lcg;

#[test]
fn the_threshold_is_the_conformal_rank_of_the_sorted_scores() {
    let scores = [5.0, 1.0, 4.0, 2.0, 3.0];
    // n = 5: α = 0.2 takes rank ⌈6 · 0.8⌉ = 5, α = 0.5 rank 3.
    assert_eq!(conformal_threshold(&scores, 0.2), Ok(5.0));
    assert_eq!(conformal_threshold(&scores, 0.5), Ok(3.0));
    // α = 0.1 needs rank ⌈5.4⌉ = 6 of 5 scores: no finite threshold covers.
    assert!(conformal_threshold(&scores, 0.1).unwrap_err().contains("rank"));
    assert!(conformal_threshold(&[], 0.5).is_err());
    assert!(conformal_threshold(&[1.0, f64::NAN], 0.5).is_err());
    for alpha in [0.0, 1.0, f64::NAN] {
        assert!(conformal_threshold(&scores, alpha).is_err(), "{alpha}");
    }
}

/// `rows` rows of one uniform feature in `[-1, 1)`, labelled by `label`.
fn draw(s: &mut u64, rows: usize, label: impl Fn(f64, &mut u64) -> f32) -> (Vec<f64>, Vec<f32>) {
    let mut x = Vec::with_capacity(rows);
    let mut y = Vec::with_capacity(rows);
    for _ in 0..rows {
        let v = 2.0 * lcg(s) - 1.0;
        y.push(label(v, s));
        x.push(v);
    }
    (x, y)
}

const MISCOVERAGE: f64 = 0.1;
const CALIBRATION_ROWS: usize = 99;
const TRIALS: usize = 300;
const TEST_ROWS: usize = 100;

/// The guarantee is `1 − α <= P(covered) <= 1 − α + 1/(n + 1)` for continuous scores, averaged over
/// calibration draws. Over `TRIALS` seeded trials the mean coverage has a standard deviation near
/// 0.0025, so a band 0.01 wider than the guarantee on each side is four of them.
fn assert_covered(covered: usize, what: &str) {
    let rate = covered as f64 / (TRIALS * TEST_ROWS) as f64;
    let (low, high) = (1.0 - MISCOVERAGE, 1.0 - MISCOVERAGE + 1.0 / (CALIBRATION_ROWS + 1) as f64);
    assert!(
        rate >= low - 0.01 && rate <= high + 0.01,
        "{what}: coverage {rate} outside [{low}, {high}]"
    );
}

#[test]
fn regression_intervals_cover_at_the_promised_rate() {
    let mut s = 17u64;
    let noisy = |x: f64, s: &mut u64| (1.0 + 2.0 * x + (2.0 * lcg(s) - 1.0)) as f32;
    let mut covered = 0;
    for _ in 0..TRIALS {
        let (tx, ty) = draw(&mut s, 50, noisy);
        let model =
            fit_linear(&TrainData::new(&tx, &ty, 1).unwrap(), LinearSpec::OrdinaryLeastSquares)
                .unwrap();
        let (cx, cy) = draw(&mut s, CALIBRATION_ROWS, noisy);
        let calibrated =
            calibrate_conformal(model.into(), &TrainData::new(&cx, &cy, 1).unwrap(), MISCOVERAGE)
                .unwrap();
        let (vx, vy) = draw(&mut s, TEST_ROWS, noisy);
        for (x, y) in vx.iter().zip(vy) {
            let (low, high) = calibrated.interval(&[*x]).unwrap();
            covered += usize::from(low <= f64::from(y) && f64::from(y) <= high);
        }
    }
    assert_covered(covered, "intervals");
}

#[test]
fn classification_sets_cover_at_the_promised_rate() {
    let truth = LinearModel { link: LinearLink::Logistic, intercept: 0.3, coefficients: vec![2.5] };
    let labelled =
        |x: f64, s: &mut u64| f32::from(u8::from(lcg(s) < truth.predict_one(&[x]).unwrap()));
    let mut s = 23u64;
    let mut covered = 0;
    for _ in 0..TRIALS {
        let (tx, ty) = draw(&mut s, 80, labelled);
        let spec = LinearSpec::Logistic { penalty: 0.01, l1_ratio: 0.0 };
        let model = fit_linear(&TrainData::new(&tx, &ty, 1).unwrap(), spec).unwrap();
        let (cx, cy) = draw(&mut s, CALIBRATION_ROWS, labelled);
        let calibrated =
            calibrate_conformal(model.into(), &TrainData::new(&cx, &cy, 1).unwrap(), MISCOVERAGE)
                .unwrap();
        let (vx, vy) = draw(&mut s, TEST_ROWS, labelled);
        for (x, y) in vx.iter().zip(vy) {
            covered +=
                usize::from(calibrated.prediction_set(&[*x]).unwrap().contains(&(y as usize)));
        }
    }
    assert_covered(covered, "prediction sets");
}

#[test]
fn a_calibrated_text_reads_back_with_its_threshold_and_bad_calibrations_are_refused() {
    let model = LinearModel { link: LinearLink::Identity, intercept: 0.0, coefficients: vec![1.0] };
    let x: Vec<f64> = (0..19).map(f64::from).collect();
    let y: Vec<f32> = (0..19).map(|i| i as f32 + if i % 2 == 0 { 0.5 } else { -1.5 }).collect();
    let d = TrainData::new(&x, &y, 1).unwrap();
    // Scores: ten 0.5s and nine 1.5s; n = 19, α = 0.5 takes rank 10: 0.5.
    let calibrated = calibrate_conformal(model.clone().into(), &d, 0.5).unwrap();
    assert_eq!(calibrated.threshold(), 0.5);
    let text = ConformalModel::text_from_parts(&model.to_text(), 0.5, calibrated.threshold());
    assert!(text.starts_with(text_format::HEADER_PREFIX));
    assert_eq!(AnyModel::from_text(&text).unwrap(), AnyModel::Conformal(calibrated));
    // Too few rows for α = 0.01, and a regression label is not a class.
    assert!(calibrate_conformal(model.clone().into(), &d, 0.01).is_err());
    let classifier = LinearModel { link: LinearLink::Logistic, ..model };
    assert!(
        calibrate_conformal(classifier.into(), &d, 0.5).unwrap_err().contains("calibration row")
    );
}
