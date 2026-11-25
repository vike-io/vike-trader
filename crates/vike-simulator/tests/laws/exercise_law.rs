//! The exercise law, backtest side: [`vike_simulator::OrderBrokerSimulator::exercise`] closes a held AMERICAN option
//! IN CASH at its intrinsic value against its underlying's last price, fee-free, as its expiry
//! settles it (the owner's ruling, 2026-11-09: no delivery of the underlying). Pinned:
//!
//! 1. an exercise of part of a long position closes that part at the intrinsic value (a call
//!    `u − strike`, a put `strike − u`), recorded in `OrderBrokerSimulator::settlements` and returned to the
//!    caller, the rest held;
//! 2. at most the long position is exercised;
//! 3. a European option, an `Unknown` style, an underlying the run does not carry, a symbol no
//!    option spec names, a flat or short book, a quantity that is no finite number above zero and
//!    an expired option are refused in words, with nothing booked.

use vike_marketdata::{Bar, OptionKind};
use vike_model::{ContractOption, ContractOptionExerciseStyle, Margining};
use vike_simulator::{SettlementFill, StrategyEngine, StrategyEngineConfig};

use crate::combo_law::{Act, Script, at, qbar, refused, ts};

const OPT: &str = "OPT";
const UNDER: &str = "UNDER";

fn option(
    style: ContractOptionExerciseStyle,
    right: OptionKind,
    strike: f64,
    underlying: Option<&str>,
) -> ContractOption {
    ContractOption {
        right,
        strike,
        expiry_ms: ts(100),
        style,
        margining: Margining::Linear,
        settle: "USD".to_string(),
        value_currency: None,
        underlying: underlying.map(str::to_string),
    }
}

fn american_call(strike: f64) -> ContractOption {
    option(ContractOptionExerciseStyle::American, OptionKind::Call, strike, Some(UNDER))
}

/// Four steps: the option at 5 and the underlying at 120 throughout, no fee; `spec` is OPT's.
fn run(spec: Option<ContractOption>, acts: Vec<Act>) -> StrategyEngine<Script> {
    let source = spec.map(|spec| -> vike_simulator::OptionExpirySource {
        Box::new(move |s: &str| (s == OPT).then(|| spec.clone()))
    });
    let p = StrategyEngineConfig {
        cash: 1_000.0,
        maker_fee: Some(0.0),
        taker_fee: Some(0.0),
        option_specs: source,
        ..StrategyEngineConfig::default()
    };
    let bars = |px: f64| -> Vec<Bar> { (0..4).map(|i| qbar(i, px, None)).collect() };
    let symbols = vec![(OPT.to_string(), bars(5.0)), (UNDER.to_string(), bars(120.0))];
    let mut engine = StrategyEngine::new(symbols, Script::new(acts), p);
    engine.run();
    engine
}

/// Trade `qty` of OPT on `side` at step 0 (filled at step 1's open, 5), then exercise `exercise` at
/// step 2, asserting what comes back.
fn hold_then_exercise(
    side: i32,
    qty: f64,
    exercise: f64,
    expect: impl Fn(Option<vike_model::Fill>) + 'static,
) -> Vec<Act> {
    let mut acts = vec![at(2, move |b| expect(b.exercise(OPT, exercise)))];
    if qty > 0.0 {
        acts.insert(0, at(0, move |b| b.submit(OPT, side, qty, 0.0, true, None)));
    }
    acts
}

#[test]
fn an_american_call_exercised_in_part_closes_that_part_in_cash_at_its_intrinsic_value() {
    let acts = hold_then_exercise(1, 10.0, 4.0, |f| {
        let f = f.expect("exercised");
        assert_eq!((f.side, f.size, f.price, f.fee, f.timestamp), (-1, 4.0, 20.0, 0.0, ts(2)));
    });
    let engine = run(Some(american_call(100.0)), acts);
    assert_eq!(engine.core.position_of(OPT).size, 6.0, "the rest is held");
    let settled =
        SettlementFill { symbol: OPT.into(), payout: 20.0, qty: 4.0, side: -1, timestamp: ts(2) };
    assert_eq!(engine.core.settlements, vec![settled]);
    // 10 bought at 5, 4 exercised at 120 - 100
    assert_eq!(engine.core.cash, 1_000.0 - 50.0 + 80.0);
    assert_eq!(
        engine.strategy.fills.len(),
        1,
        "on_fill heard the buy; the exercise came back to its caller"
    );
    let trade = &engine.core.trades[0];
    assert_eq!(
        (trade.size, trade.entry_price, trade.exit_price, trade.pnl),
        (4.0, 5.0, 20.0, 60.0)
    );
}

#[test]
fn at_most_the_long_position_is_exercised_and_a_put_pays_strike_less_the_underlying() {
    let acts = hold_then_exercise(1, 10.0, 25.0, |f| {
        let f = f.expect("exercised");
        assert_eq!((f.size, f.price), (10.0, 10.0), "the whole long, at 130 - 120");
    });
    let put = option(ContractOptionExerciseStyle::American, OptionKind::Put, 130.0, Some(UNDER));
    let engine = run(Some(put), acts);
    assert_eq!(engine.core.position_of(OPT).size, 0.0);
    assert_eq!(engine.core.cash, 1_000.0 - 50.0 + 100.0);
}

#[test]
fn every_other_exercise_is_refused_in_words_with_nothing_booked() {
    let call = OptionKind::Call;
    let mut expired = american_call(100.0);
    expired.expiry_ms = ts(1);
    let cases: Vec<(Option<ContractOption>, i32, f64, f64, &str)> = vec![
        (
            Some(option(ContractOptionExerciseStyle::European, call, 100.0, Some(UNDER))),
            1,
            10.0,
            4.0,
            "OPT is a European option: only its expiry exercises it",
        ),
        (
            Some(option(ContractOptionExerciseStyle::Unknown, call, 100.0, Some(UNDER))),
            1,
            10.0,
            4.0,
            "OPT's exercise style is unknown",
        ),
        (
            Some(option(ContractOptionExerciseStyle::American, call, 100.0, None)),
            1,
            10.0,
            4.0,
            "OPT's underlying is not a symbol of this run",
        ),
        (
            Some(option(ContractOptionExerciseStyle::American, call, 100.0, Some("ELSEWHERE"))),
            1,
            10.0,
            4.0,
            "OPT's underlying is not a symbol of this run",
        ),
        (None, 1, 10.0, 4.0, "OPT is not an option this run's option specs name"),
        (
            Some(american_call(100.0)),
            1,
            0.0,
            4.0,
            "only a long option is exercised, and OPT's position is 0",
        ),
        (
            Some(american_call(100.0)),
            -1,
            10.0,
            4.0,
            "only a long option is exercised, and OPT's position is -10",
        ),
        (Some(american_call(100.0)), 1, 10.0, f64::NAN, "not a finite number above zero"),
        (Some(expired), 1, 10.0, 4.0, "OPT has expired"),
    ];
    for (spec, side, qty, exercise, words) in cases {
        let acts = hold_then_exercise(side, qty, exercise, |f| assert!(f.is_none(), "refused"));
        let engine = run(spec, acts);
        assert!(refused(&engine.core, words), "{words}: {:?}", engine.core.dropped);
        assert!(
            engine.core.settlements.iter().all(|s| s.timestamp != ts(2)),
            "{words}: no exercise booked"
        );
    }
}
