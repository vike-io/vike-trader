//! `rl_episode` — episodes of the reinforcement-learning environment, run by a study FILE inside its
//! jail (the book's W27; decision 0291 lets the study template name `vike-simulator`).
//!
//! The consumer of `vike_simulator::RlEnvironment`: it reads the bars of one series through its context,
//! splits them by TIME (`RlEnvironment::with_training_end`) and plays one training episode, drawn by the
//! seed from the earlier bars, and one evaluation episode over the last `window` bars, with the window
//! before them left unplayed as an embargo, so the score is never taken on bars the agent trained on. The
//! agent is the simplest there is (hold `target` units at every decision). It reports the split
//! (`training_bars`, `embargo_bars`, `evaluation_start`), the training episode's summed reward, and the
//! evaluation episode's steps, summed reward and final equity, with each step of both as `episode.tsv`. A
//! real agent replaces the fixed target with its policy and learns over many training episodes; the loop
//! stays this one.
//!
//! The recipe may set `venue`, `symbol`, `interval` (default `binance`, `BTCUSDT`, `1m`), `target`
//! (default 1.0 unit), `seed` (default 0) and `window` (default a third of the bars read: training, the
//! embargo and the evaluation each need one).

use vike_simulator::{RlAction, RlEnvironment, RlObservation, StrategyEngineConfig};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

fn text<'a>(params: &'a toml::Value, key: &str, default: &'a str) -> &'a str {
    params.get(key).and_then(toml::Value::as_str).unwrap_or(default)
}

/// A float key; a whole number written without a point (`target = 2`) reads too.
fn number(params: &toml::Value, key: &str, default: f64) -> f64 {
    let value = params.get(key);
    value
        .and_then(toml::Value::as_float)
        .or_else(|| value.and_then(toml::Value::as_integer).map(|v| v as f64))
        .unwrap_or(default)
}

/// One episode from its `first` observation, holding `target` at every decision, each step a line of
/// `tsv` under `phase`: its steps, summed reward and final equity.
fn play(
    environment: &mut RlEnvironment,
    first: RlObservation,
    target: f64,
    phase: &str,
    tsv: &mut String,
) -> Result<(usize, f64, f64), StudyError> {
    tsv.push_str(&format!(
        "{phase}\t0\t{}\t{}\t{}\t0\n",
        first.timestamp, first.position, first.equity
    ));
    let (mut steps, mut total) = (0usize, 0.0);
    loop {
        let step = environment.step(RlAction::TargetPosition(target)).map_err(StudyError::Study)?;
        steps += 1;
        total += step.reward;
        let seen = &step.observation;
        tsv.push_str(&format!(
            "{phase}\t{steps}\t{}\t{}\t{}\t{}\n",
            seen.timestamp, seen.position, seen.equity, step.reward
        ));
        if step.done {
            return Ok((steps, total, seen.equity));
        }
    }
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = text(params, "venue", "binance");
    let symbol = text(params, "symbol", "BTCUSDT");
    let interval = text(params, "interval", "1m");
    let target = number(params, "target", 1.0);
    let seed = params.get("seed").and_then(toml::Value::as_integer).unwrap_or(0).max(0) as u64;
    let bars = ctx.bars(venue, symbol, interval, ctx.window())?;
    let held = bars.len();
    let window = params
        .get("window")
        .and_then(toml::Value::as_integer)
        .map_or(held / 3, |w| w.max(0) as usize);
    // Evaluation is the last window, the embargo the window before it, training every bar before that.
    if window == 0 || window.saturating_mul(3) > held {
        return Err(StudyError::Study(format!(
            "a window of {window} bars needs three of them, for training, the embargo and the \
             evaluation, and {held} bars were read"
        )));
    }
    let training_bars = held - 2 * window;
    let evaluation_start = training_bars + window;

    let mut environment = RlEnvironment::over_bars(symbol, bars, StrategyEngineConfig::default)
        .with_training_end(training_bars);
    let mut tsv = String::from("phase\tstep\ttimestamp\tposition\tequity\treward\n");
    let first = environment.reset(seed, window).map_err(StudyError::Study)?;
    let (_, training_reward, _) = play(&mut environment, first, target, "training", &mut tsv)?;
    let first = environment.reset_at(evaluation_start, window).map_err(StudyError::Study)?;
    let (steps, total, equity) = play(&mut environment, first, target, "evaluation", &mut tsv)?;

    let mut out = StudyOutcome::new();
    out.metric("training_bars", training_bars as f64)?;
    out.metric("embargo_bars", window as f64)?;
    out.metric("evaluation_start", evaluation_start as f64)?;
    out.metric("training_reward", training_reward)?;
    out.metric("steps", steps as f64)?;
    out.metric("total_reward", total)?;
    out.metric("final_equity", equity)?;
    out.artifact("episode.tsv", tsv)?;
    Ok(out)
}
