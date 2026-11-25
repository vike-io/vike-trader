//! The hedging environment's contract: the option leg's cash, the hedge's fill and fee, and the
//! training rows laying exactly the features the environment observes.

use super::{
    HEDGE_FEATURES, HEDGE_PRICE_COLUMN, HedgedOption, HedgingEnvironment, hedging_training_rows,
};
use crate::StrategyEngineConfig;
use crate::test_support::bar;
use vike_marketdata::{Bar, OptionKind};

const SYMBOL: &str = "HEDGE";
const HOUR: i64 = 3_600_000;

/// Each bar opens at the close before it, so a market order sent at a decision fills at that close.
fn tape(closes: &[f64]) -> Vec<Bar> {
    let mut open = closes[0];
    closes
        .iter()
        .enumerate()
        .map(|(i, &close)| {
            let made = bar(i as i64 * HOUR, open, open.max(close), open.min(close), close, 1_000.0);
            open = close;
            made
        })
        .collect()
}

fn short_call() -> HedgedOption {
    HedgedOption { right: OptionKind::Call, strike_ratio: 1.0, quantity: -1.0, premium_ratio: 0.02 }
}

/// Every reward of one episode holding `hedge` from the first decision on, and how many steps it took.
fn play(environment: &mut HedgingEnvironment, hedge: f64) -> (Vec<f64>, usize) {
    let mut rewards = Vec::new();
    loop {
        let step = environment.step(hedge).expect("a step");
        rewards.push(step.reward);
        if step.done {
            assert_eq!(
                step.observation.years_to_expiry(),
                0.0,
                "the terminal decision is the expiry"
            );
            return (rewards.clone(), rewards.len());
        }
    }
}

#[test]
fn an_unhedged_short_call_earns_its_premium_less_its_payout() {
    let bars = tape(&[100.0, 102.0, 101.0, 104.0, 103.0]);
    let mut environment =
        HedgingEnvironment::over_bars(SYMBOL, bars, short_call(), StrategyEngineConfig::default)
            .expect("an environment");
    let first = environment.reset_at(0, 5).expect("an episode");
    assert_eq!((first.start_price, first.strike), (100.0, 100.0));
    assert_eq!(first.expiry_ms, 4 * HOUR);
    let (rewards, steps) = play(&mut environment, 0.0);
    assert_eq!(steps, 5, "one decision per bar");
    assert_eq!(rewards[0], 2.0, "the premium is received at the first step");
    assert_eq!(rewards[4], -3.0, "the call settles at its intrinsic value, 103 - 100");
    assert_eq!(rewards.iter().sum::<f64>(), -1.0);
}

#[test]
fn the_hedge_fills_at_the_next_open_pays_its_fee_and_offsets_the_payout() {
    let bars = tape(&[100.0, 102.0, 101.0, 104.0, 103.0]);
    let fee = || StrategyEngineConfig { fee_rate: 0.001, ..Default::default() };
    let mut environment =
        HedgingEnvironment::over_bars(SYMBOL, bars, short_call(), fee).expect("an environment");
    environment.reset_at(0, 5).expect("an episode");
    let (rewards, _) = play(&mut environment, 1.0);
    // One unit bought at the second bar's open (100), marked at the last close (103): +3, less a fee of
    // 0.1; the premium 2 in, the payout 3 out.
    let total: f64 = rewards.iter().sum();
    assert!((total - 1.9).abs() < 1e-9, "the hedged book's P&L: {total}");
    let result = environment.episode_result().expect("the hedge's run");
    assert_eq!(result.simulator().unwrap().taker_fills, 1, "one market order filled");
    assert!((result.fees_paid - 0.1).abs() < 1e-12, "the fee: {}", result.fees_paid);
}

#[test]
fn the_training_rows_are_the_features_the_environment_observes() {
    let closes = [100.0, 103.0, 99.0, 104.0, 108.0, 101.0, 97.0, 110.0];
    let option = HedgedOption {
        right: OptionKind::Put,
        strike_ratio: 0.98,
        quantity: 2.0,
        premium_ratio: 0.03,
    };
    let window = 4;
    let rows = hedging_training_rows(&tape(&closes), &option, window, 1).expect("rows");
    assert_eq!(rows.paths, closes.len() - window + 1);
    assert_eq!(rows.labels.len(), rows.paths * window);
    assert_eq!(rows.features.len(), rows.labels.len() * HEDGE_FEATURES);

    let mut environment =
        HedgingEnvironment::over_bars(SYMBOL, tape(&closes), option, StrategyEngineConfig::default)
            .expect("an environment");
    for (path, &first_close) in closes.iter().enumerate().take(rows.paths) {
        let mut seen = vec![environment.reset_at(path, window).expect("an episode")];
        let mut total = 0.0;
        loop {
            let step = environment.step(0.0).expect("a step");
            total += step.reward;
            if step.done {
                break;
            }
            seen.push(step.observation);
        }
        let laid =
            &rows.features[path * window * HEDGE_FEATURES..(path + 1) * window * HEDGE_FEATURES];
        let observed: Vec<f64> = seen.iter().flat_map(|o| o.features()).collect();
        assert_eq!(observed, laid, "path {path}: the same features, bit for bit");
        assert_eq!(laid[HEDGE_PRICE_COLUMN], 1.0, "a path's price ratio starts at 1");
        let label = rows.labels[path * window];
        assert!(rows.labels[path * window..(path + 1) * window].iter().all(|l| *l == label));
        // Unhedged, the book's P&L is the option leg's, the label in units of the first close.
        let in_first_close = total / first_close;
        assert!(
            (in_first_close - f64::from(label)).abs() < 1e-6,
            "path {path}: {in_first_close} vs {label}"
        );
    }
}

#[test]
fn a_bad_option_window_stride_or_price_is_refused() {
    let bars = tape(&[100.0, 101.0, 102.0]);
    let zero = HedgedOption { quantity: 0.0, ..short_call() };
    assert!(hedging_training_rows(&bars, &zero, 2, 1).is_err());
    assert!(
        HedgingEnvironment::over_bars(SYMBOL, bars.clone(), zero, StrategyEngineConfig::default)
            .is_err()
    );
    assert!(hedging_training_rows(&bars, &short_call(), 1, 1).is_err(), "a path of one bar");
    assert!(hedging_training_rows(&bars, &short_call(), 2, 0).is_err(), "a zero stride");
    let mut broken = bars.clone();
    broken[1].close = 0.0;
    assert!(hedging_training_rows(&broken, &short_call(), 2, 1).is_err(), "a zero close");
    let rows = hedging_training_rows(&bars, &short_call(), 2, 2).expect("rows");
    assert_eq!(rows.paths, 1, "a stride of 2 over 3 bars starts one path of 2");
    let mut environment =
        HedgingEnvironment::over_bars(SYMBOL, bars, short_call(), StrategyEngineConfig::default)
            .expect("an environment");
    assert!(environment.step(0.0).is_err(), "no episode yet");
}

/// REVIEW-LIVE-MONEY L9: the option leg's cash is per underlying unit in the quote currency, added to the
/// equity change by hand, so a hedge valued otherwise (a coin ledger's inverse perpetual, a multiplier of 50)
/// is refused in words; a listed linear 1.0 value is the default and is taken.
#[test]
fn a_hedge_not_valued_linear_at_one_is_refused() {
    let bars = tape(&[100.0, 101.0, 102.0]);
    let open = |config: fn() -> StrategyEngineConfig| {
        HedgingEnvironment::over_bars(SYMBOL, bars.clone(), short_call(), config).map(|_| ())
    };
    let inverse = || StrategyEngineConfig {
        contract_values: vec![(SYMBOL.to_string(), vike_model::ContractValue::inverse(10.0))],
        ..Default::default()
    };
    let fifty = || StrategyEngineConfig { multiplier: 50.0, ..Default::default() };
    let listed = || StrategyEngineConfig {
        multipliers: vec![(SYMBOL.to_string(), 50.0)],
        contract_values: vec![(SYMBOL.to_string(), vike_model::ContractValue::linear(1.0))],
        ..Default::default()
    };
    for refused in [open(inverse), open(fifty)] {
        assert!(refused.unwrap_err().contains("not linear at 1.0 a unit"));
    }
    open(listed).expect("a listed linear 1.0 value is the default");
}
