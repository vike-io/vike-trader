//! The RL environment's contract, proven with a tabular Q-learning agent on a bar tape whose runs it
//! can learn, and with quotes on a tick tape where the queue and latency models are opt-in.

use super::{RlAction, RlEnvironment, RlEvent, RlObservation, RlQuote};
use crate::test_support::{bar, trade};
use crate::{LatencyModelKind, QueueModelKind, StrategyEngineConfig, TickEvent};
use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind};

const SYMBOL: &str = "RUNS";

/// The agent's three actions: short, flat or long one unit.
const TARGETS: [f64; 3] = [-1.0, 0.0, 1.0];
/// Above every true action value here (about 1.2), so a greedy agent tries each action before it
/// settles: the exploration needs no random draw.
const OPTIMISTIC: f64 = 2.0;
const LEARNING_RATE: f64 = 0.1;
const DISCOUNT: f64 = 0.5;

/// A tape of runs: the close moves one unit per bar, five bars up then five down, each bar opening
/// at the close before it. Following the last bar's direction is right four times in five.
fn runs(n: usize) -> Vec<Bar> {
    let mut close = 100.0;
    (0..n)
        .map(|i| {
            let open = close;
            close += if (i / 5) % 2 == 0 { 1.0 } else { -1.0 };
            bar(i as i64 * 60_000, open, open.max(close), open.min(close), close, 1_000.0)
        })
        .collect()
}

fn with_fee() -> StrategyEngineConfig {
    StrategyEngineConfig { fee_rate: 0.0001, ..Default::default() }
}

/// The agent's state: did the last bar rise, and which target is held (`rose * 3 + held`).
fn state(observation: &RlObservation) -> usize {
    let RlEvent::Bar(last) = &observation.event else { panic!("a bar tape decides on bars") };
    let held = TARGETS
        .iter()
        .position(|t| *t == observation.position)
        .expect("the position is one of the targets");
    usize::from(last.close > last.open) * 3 + held
}

/// The first best action.
fn greedy(values: &[f64; 3]) -> usize {
    (0..3).fold(0, |best, a| if values[a] > values[best] { a } else { best })
}

/// Q-learning over `episodes` windows of 50 bars, one per seed: the table and each episode's return.
fn train(environment: &mut RlEnvironment, episodes: u64) -> (Vec<[f64; 3]>, Vec<f64>) {
    let mut values = vec![[OPTIMISTIC; 3]; 6];
    let mut returns = Vec::new();
    for seed in 0..episodes {
        let mut observation = environment.reset(seed, 50).expect("an episode");
        let mut total = 0.0;
        loop {
            let s = state(&observation);
            let a = greedy(&values[s]);
            let step = environment.step(RlAction::TargetPosition(TARGETS[a])).expect("a step");
            total += step.reward;
            let target = if step.done {
                step.reward
            } else {
                let next = values[state(&step.observation)];
                step.reward + DISCOUNT * next[greedy(&next)]
            };
            values[s][a] += LEARNING_RATE * (target - values[s][a]);
            if step.done {
                break;
            }
            observation = step.observation;
        }
        returns.push(total);
    }
    (values, returns)
}

/// The greedy policy over the whole tape: the first observation and every reward.
fn evaluate(environment: &mut RlEnvironment, values: &[[f64; 3]]) -> (RlObservation, Vec<f64>) {
    let first = environment.reset(0, usize::MAX).expect("the whole tape");
    let mut observation = first.clone();
    let mut rewards = Vec::new();
    loop {
        let action = RlAction::TargetPosition(TARGETS[greedy(&values[state(&observation)])]);
        let step = environment.step(action).expect("a step");
        rewards.push(step.reward);
        if step.done {
            return (first, rewards);
        }
        observation = step.observation;
    }
}

#[test]
fn a_tabular_agent_learns_to_ride_the_runs() {
    let mut environment = RlEnvironment::over_bars(SYMBOL, runs(400), with_fee);
    let (values, _) = train(&mut environment, 300);
    // Where the learned policy lives: long after a rise, short after a fall.
    assert_eq!(greedy(&values[3 + 2]), 2, "rose while long: stay long, {values:?}");
    assert_eq!(greedy(&values[0]), 0, "fell while short: stay short, {values:?}");

    let (first, rewards) = evaluate(&mut environment, &values);
    let total: f64 = rewards.iter().sum();
    // Riding every run earns 0.6 a bar less the fee (about 237 here); a flat agent earns 0.
    assert!(total > 150.0, "the greedy policy earned {total}");
    assert_eq!(rewards.len(), 400, "one step per bar of the whole tape");

    let result = environment.episode_result().expect("the episode's backtest result");
    let booked = result.final_equity - first.equity;
    assert!(
        (total - booked).abs() < 1e-9,
        "rewards {total} must sum to the equity gained {booked}"
    );
    assert!(!result.trades.is_empty(), "the policy trades");
    assert!(result.fees_paid > 0.0, "the opt-in fee reached the engine");
}

#[test]
fn the_same_seeds_and_actions_give_the_same_episodes() {
    let mut one = RlEnvironment::over_bars(SYMBOL, runs(200), with_fee);
    let mut two = RlEnvironment::over_bars(SYMBOL, runs(200), with_fee);
    assert_eq!(train(&mut one, 40), train(&mut two, 40), "training is deterministic");
}

#[test]
fn the_seed_picks_the_window_and_the_window_bounds_the_episode() {
    let mut environment =
        RlEnvironment::over_bars(SYMBOL, runs(100), StrategyEngineConfig::default);
    let first = environment.reset(7, 20).expect("an episode").timestamp;
    let again = environment.reset(7, 20).expect("an episode").timestamp;
    assert_eq!(first, again, "a seed names one window");
    let starts: std::collections::BTreeSet<i64> =
        (0..20).map(|seed| environment.reset(seed, 20).expect("an episode").timestamp).collect();
    assert!(starts.len() > 5, "seeds spread over the starts: {starts:?}");

    environment.reset(3, 20).expect("an episode");
    let mut steps = 0;
    loop {
        steps += 1;
        if environment.step(RlAction::TargetPosition(0.0)).expect("a step").done {
            break;
        }
    }
    assert_eq!(steps, 20, "one decision per bar of the window");
    assert!(environment.step(RlAction::TargetPosition(0.0)).is_err(), "an ended episode refuses");
    assert!(environment.episode_result().is_some());

    let whole = environment.reset(3, 1_000).expect("the whole tape");
    assert_eq!(whole.timestamp, 0, "a window longer than the tape is all of it");
    assert!(environment.step(RlAction::TargetPosition(f64::NAN)).is_err(), "NaN is refused");

    let mut empty = RlEnvironment::over_bars(SYMBOL, Vec::new(), StrategyEngineConfig::default);
    assert!(empty.reset(0, 10).is_err(), "no bar, no decision");
}

#[test]
fn reset_at_runs_the_window_at_a_chosen_start_and_reset_is_reset_at_its_seeded_start() {
    let mut environment =
        RlEnvironment::over_bars(SYMBOL, runs(100), StrategyEngineConfig::default);
    let at = environment.reset_at(30, 20).expect("an episode").timestamp;
    assert_eq!(at, 30 * 60_000, "the window starts at the bar it names");
    let start = environment.seeded_start(7, 20);
    let seeded = environment.reset(7, 20).expect("an episode").timestamp;
    assert_eq!(seeded, start as i64 * 60_000, "reset runs the window seeded_start names");
    assert!(environment.reset_at(81, 20).is_err(), "a window past the tape's end is refused");
    assert!(environment.reset_at(80, 20).is_ok(), "the last whole window runs");
}

/// Split by time at bar 60: no training episode holds a bar at or after the split, and no evaluation
/// episode starts within one episode of the last trained bar, so the agent is never scored on what it
/// trained on.
#[test]
fn training_and_evaluation_are_split_by_time_with_an_episode_of_embargo() {
    let window = 10;
    let mut environment =
        RlEnvironment::over_bars(SYMBOL, runs(100), StrategyEngineConfig::default)
            .with_training_end(60);
    let mut last_trained = 0;
    for seed in 0..64 {
        let first = environment.reset(seed, window).expect("a training episode").timestamp;
        last_trained = last_trained.max(first / 60_000 + window as i64 - 1);
    }
    assert!(last_trained <= 59, "every training episode ends before the split: {last_trained}");

    let whole = environment.reset(0, 1_000).expect("the training part").timestamp;
    assert_eq!(whole, 0, "a window longer than the training part is all of it");
    let mut steps = 0;
    loop {
        steps += 1;
        if environment.step(RlAction::TargetPosition(0.0)).expect("a step").done {
            break;
        }
    }
    assert_eq!(steps, 60, "and never a bar past the split");

    let refused = environment.reset_at(69, window).expect_err("an evaluation inside the embargo");
    assert!(refused.contains("starts at 70 or later"), "{refused}");
    let first_evaluated =
        environment.reset_at(70, window).expect("an evaluation episode").timestamp / 60_000;
    assert!(
        first_evaluated - last_trained > window as i64,
        "an episode lies between the last trained bar {last_trained} and the first evaluated \
         {first_evaluated}"
    );
}

/// A bid of 10 at 99 ahead of ours, a 0.5 print at 99, then a print at 100 that marks the position.
fn tape() -> Vec<TickEvent> {
    vec![
        TickEvent::Book(Box::new(L2BookUpdate {
            timestamp: 1,
            local_timestamp: 0,
            sequence: 1,
            kind: L2BookUpdateKind::Snapshot,
            tick_size: 0.01,
            bids: vec![L2BookLevel::new(99.0, 10.0)],
            asks: vec![L2BookLevel::new(101.0, 5.0)],
            symbol: SYMBOL.to_string(),
        })),
        trade(SYMBOL, 2, 99.0, 0.5),
        trade(SYMBOL, 3, 100.0, 0.1),
    ]
}

/// Bid 2 at 99 at every decision: the position seen after each step and the episode's return.
fn quote_the_bid(config: fn() -> StrategyEngineConfig) -> (Vec<f64>, f64) {
    let mut environment = RlEnvironment::over_ticks(SYMBOL, tape(), config);
    let first = environment.reset(0, 10).expect("an episode");
    assert!(matches!(first.event, RlEvent::Book(_)), "the snapshot is the first decision");
    let bid = RlAction::Quotes { bid: Some(RlQuote { price: 99.0, size: 2.0 }), ask: None };
    let (mut positions, mut total) = (Vec::new(), 0.0);
    loop {
        let step = environment.step(bid.clone()).expect("a step");
        positions.push(step.observation.position);
        total += step.reward;
        if step.done {
            return (positions, total);
        }
    }
}

#[test]
fn quotes_rest_as_limits_and_the_queue_and_latency_models_are_opt_in() {
    // No model: the touch fills the whole bid, and the print at 100 marks it up by 2.
    let (positions, total) = quote_the_bid(StrategyEngineConfig::default);
    assert_eq!(positions, vec![2.0, 2.0, 2.0]);
    assert!((total - 2.0).abs() < 1e-9, "bought 2 at 99, marked at 100: {total}");

    // The queue model: 10 ahead of the bid and 0.5 traded, so nothing fills.
    let (positions, total) = quote_the_bid(|| StrategyEngineConfig {
        queue_model: Some(QueueModelKind::RiskAverse),
        ..Default::default()
    });
    assert_eq!(positions, vec![0.0, 0.0, 0.0]);
    assert_eq!(total, 0.0);

    // The latency model: the bid reaches the book 5 ms after it was sent, after the tape ends.
    let (positions, _) = quote_the_bid(|| StrategyEngineConfig {
        latency_model: Some(LatencyModelKind::constant(5_000_000, 0)),
        ..Default::default()
    });
    assert_eq!(positions, vec![0.0, 0.0, 0.0]);
}
