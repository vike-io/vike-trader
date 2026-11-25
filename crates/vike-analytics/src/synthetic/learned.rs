//! A LEARNED generator: bar paths sampled from a next-step model of the log return that a study
//! FITTED, so a backtest can run on paths with the dynamics a model learned from a market rather than
//! the ones a formula assumes. The book's model is the conditional normalising flow the network trainer
//! fits under `loss = "flow"` (`crates/vike-ml-neural-training/src/flow.rs`); any model that maps a
//! window of returns and one random draw to the next return can stand in its place.
//!
//! # The frame both halves share
//!
//! [`next_step_rows`] cuts a source of bars into training rows and [`learned_bars`] samples from the
//! model fitted on them. They agree through one [`NextStepFrame`]:
//! - a SCALED log return is `(ln(close / previous close) - center) / scale`, where `center` and `scale`
//!   are the mean and the sample standard deviation of the source's log returns, so the model sees
//!   numbers of order one whatever the market's units;
//! - a row's features are the `window` scaled log returns before its label, oldest first, and its label
//!   is the scaled log return that followed;
//! - the model a path is sampled from takes ONE MORE column than it was trained on: the window, then
//!   one standard normal draw. It answers the next scaled log return. That is the trainer's exported
//!   flow sampler; a test passes a closure.
//!
//! # A path
//!
//! [`learned_bars`] starts from the source's LAST window ([`NextStepFrame::context`]). Each sub-step
//! asks the model once, with a fresh draw from the seeded stream every generator here shares
//! (`super::Draws`), and slides the window by the answer. A bar is `intrabar_steps` consecutive model
//! steps, assembled as every model bar is (`super::stepped_bars`: the one path's first point,
//! maximum, minimum and last point). So ONE MODEL STEP IS ONE SOURCE BAR: a model fitted on minute bars
//! and sampled 60 steps per bar gives hourly bars whose highs and lows the learned minutes made, and
//! `intrabar_steps = 1` gives bars whose high and low are their open and close.
//!
//! # A path stays in the range the model was fitted on
//!
//! An answer outside the source's own range of scaled log returns ([`NextStepFrame::lowest`] to
//! [`NextStepFrame::highest`]) is CLAMPED to it before it enters the window. A model knows nothing
//! beyond what it saw, and sampled step by step it reads its own answers back: a network extrapolating
//! one outlier into a wider law draws a larger one, and the path diverges. MEASURED 2026-11-17 on the
//! first flow fitted (an MLP on a regime-switching minute market): without the clamp, a path left the
//! range of `f64` after 619 bars. The clamp bounds the generated tail by the source's largest move,
//! which is the honest limit of a model fitted on that source; a caller that wants to know how often
//! it bit counts the answers its `next_step` returns outside the range.
//!
//! The draws are seeded and every transcendental here is `libm` (decision 0032), so a path is a
//! function of the seed and of the model's answers. Whether the model answers the same numbers on
//! another box is the model's own contract: a network trained by `burn` repeats on one box only.

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;
use vike_marketdata::Bar;

use super::{Draws, SyntheticGrid, check_stepped, stepped_bars};
use vike_math::statistics::mean_variance;
use vike_math::time_series::log_ratio;

/// How a next-step model of log returns reads and answers (the module doc's "The frame both halves
/// share").
#[derive(Debug, Clone, PartialEq)]
pub struct NextStepFrame {
    /// Scaled log returns per row: the model's columns before the draw.
    pub window: usize,
    /// The mean of the source's log returns.
    pub center: f64,
    /// The sample standard deviation of the source's log returns, above zero.
    pub scale: f64,
    /// The source's smallest scaled log return: a lower answer is clamped to it (the module doc).
    pub lowest: f64,
    /// The source's largest scaled log return: a higher answer is clamped to it.
    pub highest: f64,
    /// The source's last `window` scaled log returns, oldest first: where a path starts.
    pub context: Vec<f64>,
}

/// A source cut into the rows a next-step model trains on, and the frame it was cut in.
#[derive(Debug, Clone, PartialEq)]
pub struct NextStepRows {
    /// The frame [`learned_bars`] samples the fitted model in.
    pub frame: NextStepFrame,
    /// `labels.len()` rows of `frame.window` features each, row-major.
    pub features: Vec<f64>,
    /// One per row: the scaled log return after the row's window.
    pub labels: Vec<f64>,
}

/// `source` cut into next-step rows of `window` scaled log returns each (the module doc).
///
/// # Errors
/// A sentence when `window` is zero, when a close is not finite and above zero, when the source is too
/// short for one row and a sample standard deviation (`window + 2` bars, and at least 3), or when its
/// log returns do not vary.
pub fn next_step_rows(source: &[Bar], window: usize) -> Result<NextStepRows, String> {
    if window == 0 {
        return Err("a next-step model needs a window of at least one return".to_string());
    }
    if let Some((i, bar)) =
        source.iter().enumerate().find(|(_, b)| !(b.close.is_finite() && b.close > 0.0))
    {
        return Err(format!(
            "source bar {i} (at {}) closes at {}, and a log return needs a finite close above zero",
            bar.timestamp, bar.close
        ));
    }
    let needed = (window + 2).max(3);
    if source.len() < needed {
        return Err(format!(
            "a window of {window} returns needs at least {needed} source bars, and the source holds {}",
            source.len()
        ));
    }
    let log_returns: Vec<f64> =
        source.windows(2).map(|pair| log_ratio(pair[1].close, pair[0].close)).collect();
    let (center, variance) = mean_variance(log_returns.iter().copied(), 1.0);
    let scale = variance.sqrt();
    if !(scale.is_finite() && scale > 0.0) {
        return Err(format!(
            "the source's log returns have the standard deviation {scale}; a model of their changes \
             needs them to vary"
        ));
    }
    let scaled: Vec<f64> = log_returns.iter().map(|r| (r - center) / scale).collect();
    let mut features = Vec::with_capacity((scaled.len() - window) * window);
    for row in scaled.windows(window + 1) {
        features.extend_from_slice(&row[..window]);
    }
    let lowest = scaled.iter().copied().fold(f64::INFINITY, f64::min);
    let highest = scaled.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Ok(NextStepRows {
        frame: NextStepFrame {
            window,
            center,
            scale,
            lowest,
            highest,
            context: scaled[scaled.len() - window..].to_vec(),
        },
        features,
        labels: scaled[window..].to_vec(),
    })
}

/// A path of `grid.bars` bars sampled from `next_step` in `frame`, starting from `first_price`, every
/// bar carrying `volume` and made of `intrabar_steps` model steps, the draws seeded by `seed` (the
/// module doc's "A path").
///
/// `next_step` is handed `frame.window + 1` numbers (the window, oldest first, then one standard
/// normal draw) and answers the next scaled log return, which is clamped to `frame.lowest ..=
/// frame.highest` (the module doc); a fitted study model is `|row| model.predict_one(row)`.
///
/// # Errors
/// A sentence naming the parameter refused (the grid, `first_price`, `volume` and `intrabar_steps` as
/// [`super::model_bars`] checks them, and a frame whose context is not `window` finite numbers, whose
/// scale is not above zero or whose range is not finite and ordered), the model's own refusal or a
/// non-finite answer with the bar and the step it happened at, or the bar whose price left the range
/// of `f64`.
pub fn learned_bars(
    mut next_step: impl FnMut(&[f64]) -> Result<f64, String>,
    frame: &NextStepFrame,
    grid: &SyntheticGrid,
    first_price: f64,
    volume: f64,
    intrabar_steps: usize,
    seed: u64,
) -> Result<Vec<Bar>, String> {
    check_stepped(grid, first_price, volume, intrabar_steps)?;
    let window = frame.window;
    if window == 0 || frame.context.len() != window || !frame.context.iter().all(|x| x.is_finite())
    {
        return Err(format!(
            "the frame's context must be its window of {window} finite scaled returns (at least one), \
             not {:?}",
            frame.context
        ));
    }
    if !(frame.center.is_finite() && frame.scale.is_finite() && frame.scale > 0.0) {
        return Err(format!(
            "the frame needs a finite center and a scale above zero, not {} and {}",
            frame.center, frame.scale
        ));
    }
    if !(frame.lowest.is_finite() && frame.highest.is_finite() && frame.lowest <= frame.highest) {
        return Err(format!(
            "the frame's range must be finite and ordered, not {} to {}",
            frame.lowest, frame.highest
        ));
    }
    let mut draws = Draws(ChaCha8Rng::seed_from_u64(seed));
    // The model's row: the window, oldest first, then the draw in the last column.
    let mut row = frame.context.clone();
    row.push(0.0);
    let hint = "the model's answers grew without bound; shorten the path";
    stepped_bars(grid, first_price, volume, intrabar_steps, hint, |i, step, log_price| {
        row[window] = draws.normal();
        let answer = next_step(&row).map_err(|e| format!("bar {i}, step {step}: {e}"))?;
        if !answer.is_finite() {
            return Err(format!("bar {i}, step {step}: the model answered {answer}"));
        }
        let answer = answer.clamp(frame.lowest, frame.highest);
        row.copy_within(1..window, 0);
        row[window - 1] = answer;
        Ok(log_price + frame.center + frame.scale * answer)
    })
}

#[cfg(test)]
mod tests {
    use super::super::{SyntheticModel, model_bars};
    use super::*;

    const MINUTE: i64 = 60_000;

    fn grid(bars: usize) -> SyntheticGrid {
        SyntheticGrid { start_ms: 1_735_689_600_000, step_ms: MINUTE, bars }
    }

    fn bar(close: f64) -> Bar {
        Bar::ohlcv(0, close, close, close, close, 1.0)
    }

    /// A frame whose range no test answer reaches, unless a test narrows it.
    fn frame(center: f64, scale: f64, context: Vec<f64>) -> NextStepFrame {
        let window = context.len();
        NextStepFrame { window, center, scale, lowest: -1e6, highest: 1e6, context }
    }

    #[test]
    fn a_source_is_cut_into_scaled_windows_and_the_return_after_each() {
        // Closes 1, e, e^3, e^2, e^4: log returns 1, 2, -1, 2 (mean 1, sample deviation sqrt 2).
        let e = std::f64::consts::E;
        let source: Vec<Bar> =
            [1.0, e, e * e * e, e * e, e * e * e * e].into_iter().map(bar).collect();
        let rows = next_step_rows(&source, 2).unwrap();
        let s = std::f64::consts::SQRT_2;
        let scaled = [0.0, 1.0 / s, -2.0 / s, 1.0 / s];
        assert_eq!(rows.frame.window, 2);
        assert!((rows.frame.center - 1.0).abs() < 1e-12, "{rows:?}");
        assert!((rows.frame.scale - s).abs() < 1e-12, "{rows:?}");
        let close = |got: &[f64], want: &[f64]| {
            got.len() == want.len() && got.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-12)
        };
        assert!(close(&rows.features, &[scaled[0], scaled[1], scaled[1], scaled[2]]), "{rows:?}");
        assert!(close(&rows.labels, &scaled[2..]), "{rows:?}");
        assert!(close(&rows.frame.context, &scaled[2..]), "{rows:?}");
    }

    #[test]
    fn a_short_flat_or_broken_source_is_refused() {
        let ramp: Vec<Bar> = (1..=4).map(|i| bar(f64::from(i))).collect();
        assert!(next_step_rows(&ramp, 0).unwrap_err().contains("at least one return"));
        assert!(next_step_rows(&ramp, 3).unwrap_err().contains("at least 5 source bars"));
        assert!(next_step_rows(&[bar(1.0), bar(2.0)], 1).unwrap_err().contains("at least 3"));
        let flat: Vec<Bar> = (0..6).map(|_| bar(5.0)).collect();
        assert!(next_step_rows(&flat, 2).unwrap_err().contains("to vary"));
        let broken = [bar(1.0), bar(0.0), bar(2.0), bar(3.0)];
        assert!(next_step_rows(&broken, 1).unwrap_err().contains("source bar 1"));
    }

    /// A model that answers its draw is a Brownian motion: the path is `model_bars`' GBM with the
    /// drift and volatility of the frame, draw for draw, whenever the two streams draw alike.
    #[test]
    fn a_model_that_answers_its_draw_walks_the_frames_brownian_motion() {
        let frame = frame(0.0001, 0.01, vec![0.0; 3]);
        let learned = learned_bars(|row| Ok(row[3]), &frame, &grid(400), 100.0, 2.0, 4, 9).unwrap();
        // GBM's per-sub-step mean is (drift - vol^2/2)/n and its deviation vol/sqrt(n).
        let (steps, volatility) = (4.0_f64, 0.01 * 2.0);
        let drift = 0.0001 * steps + volatility * volatility / 2.0;
        let gbm = SyntheticModel::GeometricBrownianMotion { drift, volatility };
        let reference = model_bars(&gbm, &grid(400), 100.0, 2.0, 4, 9).unwrap();
        for (a, b) in learned.iter().zip(&reference) {
            assert!((a.close / b.close - 1.0).abs() < 1e-9, "{a:?} {b:?}");
            assert!((a.high / b.high - 1.0).abs() < 1e-9, "{a:?} {b:?}");
            assert!((a.low / b.low - 1.0).abs() < 1e-9, "{a:?} {b:?}");
        }
        assert!(learned.iter().all(|b| b.volume == 2.0));
    }

    #[test]
    fn the_model_sees_the_window_slide_and_one_draw_per_step() {
        let frame = frame(0.0, 0.001, vec![7.0, 8.0]);
        let mut seen: Vec<Vec<f64>> = Vec::new();
        let bars = learned_bars(
            |row| {
                seen.push(row.to_vec());
                Ok(seen.len() as f64)
            },
            &frame,
            &grid(2),
            50.0,
            1.0,
            2,
            3,
        )
        .unwrap();
        assert_eq!(seen.len(), 4);
        let windows: Vec<[f64; 2]> = seen.iter().map(|r| [r[0], r[1]]).collect();
        assert_eq!(windows, [[7.0, 8.0], [8.0, 1.0], [1.0, 2.0], [2.0, 3.0]]);
        let draws: Vec<f64> = seen.iter().map(|r| r[2]).collect();
        assert!(draws.windows(2).all(|d| d[0] != d[1]), "{draws:?}");
        // Answers 1, 2 then 3, 4 scaled by 0.001: each bar's close is its open times exp(sum).
        assert!((bars[0].close - 50.0 * libm::exp(0.003)).abs() < 1e-9, "{bars:?}");
        assert!((bars[1].close - bars[0].close * libm::exp(0.007)).abs() < 1e-9, "{bars:?}");
        assert_eq!(bars[1].open, bars[0].close);
        assert_eq!(bars[1].low, bars[1].open);
    }

    #[test]
    fn one_seed_is_one_path() {
        let frame = frame(0.0, 0.01, vec![0.5]);
        let model = |row: &[f64]| Ok(0.3 * row[0] + row[1]);
        let a = learned_bars(model, &frame, &grid(50), 10.0, 1.0, 3, 1).unwrap();
        let b = learned_bars(model, &frame, &grid(50), 10.0, 1.0, 3, 1).unwrap();
        let c = learned_bars(model, &frame, &grid(50), 10.0, 1.0, 3, 2).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn a_bad_frame_a_refusing_model_or_a_runaway_answer_is_refused() {
        let frame = frame(0.0, 0.01, vec![0.0, 0.0]);
        let fine = |row: &[f64]| Ok(row[2]);
        let unordered = NextStepFrame { lowest: 1.0, highest: -1.0, ..frame.clone() };
        assert!(
            learned_bars(fine, &unordered, &grid(3), 1.0, 1.0, 1, 0).unwrap_err().contains("range")
        );
        let short = NextStepFrame { context: vec![0.0], ..frame.clone() };
        assert!(
            learned_bars(fine, &short, &grid(3), 1.0, 1.0, 1, 0).unwrap_err().contains("window")
        );
        let unscaled = NextStepFrame { scale: 0.0, ..frame.clone() };
        assert!(
            learned_bars(fine, &unscaled, &grid(3), 1.0, 1.0, 1, 0).unwrap_err().contains("scale")
        );
        assert!(learned_bars(fine, &frame, &grid(3), 1.0, 1.0, 0, 0).unwrap_err().contains("step"));
        let refusing = |_: &[f64]| Err("no".to_string());
        let why = learned_bars(refusing, &frame, &grid(3), 1.0, 1.0, 1, 0).unwrap_err();
        assert_eq!(why, "bar 0, step 0: no");
        let why = learned_bars(|_| Ok(f64::NAN), &frame, &grid(3), 1.0, 1.0, 1, 0).unwrap_err();
        assert!(why.contains("answered NaN"), "{why}");
        let runaway = NextStepFrame { scale: 1e300, ..frame };
        let why = learned_bars(|_| Ok(1.0), &runaway, &grid(3), 1.0, 1.0, 1, 0).unwrap_err();
        assert!(why.contains("left the range of f64"), "{why}");
    }

    /// An answer beyond the source's range enters the path and the window as the range's edge.
    #[test]
    fn an_answer_beyond_the_sources_range_is_clamped_to_it() {
        let narrow = NextStepFrame { lowest: -2.0, highest: 3.0, ..frame(0.0, 0.01, vec![0.0]) };
        let mut windows: Vec<f64> = Vec::new();
        let answers = [100.0, -100.0, 1.0];
        let bars = learned_bars(
            |row| {
                windows.push(row[0]);
                Ok(answers[windows.len() - 1])
            },
            &narrow,
            &grid(3),
            10.0,
            1.0,
            1,
            5,
        )
        .unwrap();
        assert_eq!(windows, [0.0, 3.0, -2.0]);
        let moves: Vec<f64> = bars.iter().map(|b| log_ratio(b.close, b.open)).collect();
        for (got, want) in moves.iter().zip([0.03, -0.02, 0.01]) {
            assert!((got - want).abs() < 1e-12, "{moves:?}");
        }
    }

    #[test]
    fn the_source_range_is_its_extreme_scaled_returns() {
        let e = std::f64::consts::E;
        let source: Vec<Bar> =
            [1.0, e, e * e * e, e * e, e * e * e * e].into_iter().map(bar).collect();
        let rows = next_step_rows(&source, 2).unwrap();
        let s = std::f64::consts::SQRT_2;
        assert!((rows.frame.lowest + 2.0 / s).abs() < 1e-12, "{rows:?}");
        assert!((rows.frame.highest - 1.0 / s).abs() < 1e-12, "{rows:?}");
    }
}
