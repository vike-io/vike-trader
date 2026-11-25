//! The strategy's commission and slippage through a run (the owner's ruling of 2026-11-18, decision 0269's
//! addendum): two strategy PARAMETERS, each defaulting to what the strategy states, else none; no venue table;
//! the run-level keys they replaced refused in words.

use std::sync::Arc;

use vike_data::HistoryStoreMemory;
use vike_marketdata::Bar;
use vike_model::{FeeSchedule, OrderBroker, OrderPlacement, ProfileEngineSlippage, Strategy};
use vike_simulator::OrderBrokerSimulator;

use super::*;
use crate::run::strategy_builder::StrategyBuilder;

/// A UTC midnight.
const S: i64 = 19_675 * 86_400_000;
const MINUTE: i64 = 60_000;

/// One contract in on the first bar and out on the third, stating `fee` and `slippage`.
struct RoundTrip {
    fee: Option<FeeSchedule>,
    slippage: Option<ProfileEngineSlippage>,
}

impl Strategy<OrderBrokerSimulator> for RoundTrip {
    fn stated_fee(&self) -> Option<FeeSchedule> {
        self.fee
    }
    fn stated_slippage(&self) -> Option<ProfileEngineSlippage> {
        self.slippage.clone()
    }
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        let side = match (bar.timestamp - S) / MINUTE {
            0 => 1,
            2 => -1,
            _ => return,
        };
        broker.place("", "", side, 1.0, OrderPlacement::Market, false);
    }
}

fn store() -> Arc<HistoryStoreMemory> {
    let s = HistoryStoreMemory::new();
    let bar = |i: i64| Bar {
        timestamp: S + i * MINUTE,
        open: 100.0,
        high: 100.0,
        low: 100.0,
        close: 100.0,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    let bars: Vec<Bar> = (0..6).map(bar).collect();
    s.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    Arc::new(s)
}

/// The profile text of a six-minute run, `[engine]` ending in `engine` and `[strategy.params]` holding
/// `params`.
fn profile_text(engine: &str, params: &str) -> String {
    format!(
        "[data]\nkind = \"bar\"\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\ninterval = \"1m\"\n\
         from = \"{S}\"\nto = \"{}\"\n[engine]\ncash = 10000.0\nattach_funding = false\n{engine}\n\n\
         [strategy]\nfile = \"other/round_trip.rs\"\n[strategy.params]\n{params}\n",
        S + 6 * MINUTE - 1
    )
}

/// A six-minute run of [`RoundTrip`] stating `fee` and `slippage`, its `[strategy.params]` holding `params`.
fn profile(
    params: &str,
    fee: Option<FeeSchedule>,
    slippage: Option<ProfileEngineSlippage>,
) -> ProfileBacktest {
    let toml = profile_text("", params);
    let mut p = ProfileBacktest::from_toml_str(&toml)
        .unwrap_or_else(|e| panic!("{e}\n{toml}"))
        .with_stand_in(vike_strategy_lab::stand_in);
    p.strategy.as_mut().unwrap().builder = Some(StrategyBuilder::new(move |_| {
        Ok(Box::new(RoundTrip { fee, slippage: slippage.clone() }))
    }));
    p
}

fn run(p: &ProfileBacktest) -> TradingRecord {
    run_backtest(p, store()).expect("the round trip runs")
}

/// The stamp's words for what the run charged: `(fees, slippage)`.
fn charged(p: &ProfileBacktest) -> (String, String) {
    let stamp = crate::run::report::realism_stamp(p);
    let c = stamp.charged.expect("a stamp says what it charged");
    (c.fees, c.slippage)
}

/// `per_contract(0.05)` as a rule states it (`vike_model::Commission::schedule`).
const PER_CONTRACT: FeeSchedule =
    FeeSchedule::PerShareWithFloor { per_share: 0.05, min: 0.0, max_percent: 0.0 };

/// Two ticks of 0.25, as a rule states them (`vike_model::Slippage::table`).
fn two_ticks() -> ProfileEngineSlippage {
    ProfileEngineSlippage {
        model: "ticks".to_string(),
        value: None,
        ticks: Some(2.0),
        tick: Some(0.25),
        base: None,
        exec_time: None,
        window: None,
        gamma: None,
        eta: None,
    }
}

/// **Nothing stated, nothing set: no commission and no slippage**, and no venue table: the round trip at
/// 100 in and 100 out ends where it began, and the stamp says every fill was free.
#[test]
fn a_strategy_stating_no_costs_pays_none() {
    let p = profile("", None, None);
    let result = run(&p);
    assert_eq!(result.fees_paid, 0.0, "no venue table");
    assert_eq!(result.final_equity, 10_000.0);
    assert_eq!(charged(&p), ("none".to_string(), "none".to_string()));
    let stamp = crate::run::report::realism_stamp(&p);
    assert!(stamp.charged_nothing.is_some(), "every fill was free");
}

/// **What the strategy states is its params' default**: 0.05 a contract is 0.10 a round trip, two ticks of
/// 0.25 a fill are one point (100.50 in, 99.50 out), and the stamp says whose.
#[test]
fn a_stated_commission_and_slippage_are_charged() {
    let fee = profile("", Some(PER_CONTRACT), None);
    let paid = run(&fee).fees_paid;
    assert!((paid - 0.10).abs() < 1e-12, "a round trip paid {paid}");
    assert_eq!(charged(&fee).0, "0.05 per contract (the strategy's own)");

    let slipped = profile("", None, Some(two_ticks()));
    assert_eq!(run(&slipped).final_equity, 9_999.0, "in at 100.50, out at 99.50");
    assert_eq!(charged(&slipped).1, "2 ticks of 0.25 a fill (the strategy's own)");
}

/// **A param the user sets wins**, and a key he leaves out keeps the strategy's: zero is zero; an amount
/// alone keeps the stated unit (0.1 a contract, one tick of 0.25); a unit beside it is that unit (0.05% of
/// a 100 fill is 0.05).
#[test]
fn a_cost_param_the_user_sets_wins_over_the_stated_one() {
    let zero = profile("commission = 0\nslippage = 0", Some(PER_CONTRACT), Some(two_ticks()));
    let free = run(&zero);
    assert_eq!((free.fees_paid, free.final_equity), (0.0, 10_000.0), "zero wins");

    let amount = profile("commission = 0.1\nslippage = 1", Some(PER_CONTRACT), Some(two_ticks()));
    let paid = run(&amount);
    assert!((paid.fees_paid - 0.20).abs() < 1e-12, "0.1 a contract: {}", paid.fees_paid);
    assert!((paid.final_equity + paid.fees_paid - 9_999.5).abs() < 1e-9, "one tick a fill");
    assert_eq!(
        charged(&amount),
        (
            "0.1 per contract (strategy.params)".to_string(),
            "1 ticks of 0.25 a fill (strategy.params)".to_string()
        )
    );

    let percent = profile(
        "commission = 0.05\ncommission_unit = \"percent\"\nslippage = 0.5\nslippage_unit = \"percent\"",
        None,
        None,
    );
    let paid = run(&percent);
    assert!((paid.fees_paid - 0.10).abs() < 1e-9, "0.05% of 100.5 and of 99.5: {}", paid.fees_paid);
    assert!((paid.final_equity + paid.fees_paid - 9_999.0).abs() < 1e-9, "0.5% of 100 a fill");
}

/// A cost param the run cannot read is refused in words naming it.
#[test]
fn a_cost_param_of_the_wrong_kind_is_refused() {
    for (params, words) in [
        ("commission = -1", "strategy.params.commission must be a number >= 0"),
        ("commission_unit = \"bps\"", "strategy.params.commission_unit = \"bps\" is not a unit"),
        ("slippage_unit = \"ticks\"\nslippage = 2", "needs strategy.params.slippage_tick"),
        ("slippage = \"two\"", "strategy.params.slippage must be a number >= 0"),
    ] {
        let err = run_backtest(&profile(params, None, None), store())
            .expect_err("a bad cost param is refused")
            .to_string();
        assert!(err.contains(words), "{params}: {err}");
    }
}

/// Every cost sentence offers the model's unit words (`CommissionUnit::WORDS`, `SlippageUnit::WORDS`),
/// in order. Each sentence says what every unit word MEANS beside it, so it cannot be rendered from the
/// roster without rewording: this holds the words to the roster instead.
#[test]
fn every_cost_sentence_names_the_models_unit_words() {
    fn names_in_order(text: &str, words: &[String]) -> bool {
        let mut at = 0;
        words.iter().all(|w| match text[at..].find(w.as_str()) {
            Some(i) => {
                at += i + w.len();
                true
            }
            None => false,
        })
    }
    let commission: Vec<&str> = vike_model::CommissionUnit::WORDS.iter().map(|(w, _)| *w).collect();
    let slippage: Vec<&str> = vike_model::SlippageUnit::WORDS.iter().map(|(w, _)| *w).collect();
    let quoted = |words: &[&str]| words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>();
    let bare = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
    let refusal = |params: &str| {
        run_backtest(&profile(params, None, None), store()).expect_err("refused").to_string()
    };
    for (text, words) in [
        (crate::run::costs::COMMISSION_NOW.to_string(), quoted(&commission)),
        (crate::run::costs::SLIPPAGE_NOW.to_string(), quoted(&slippage)),
        (refusal("commission_unit = \"bps\""), bare(&commission)),
        (refusal("slippage_unit = \"pips\""), bare(&slippage)),
    ] {
        assert!(names_in_order(&text, &words), "{words:?} not named in order: {text}");
    }
}

/// **A unit set alone never reads the strategy's amount in another unit**: `slippage_unit = "percent"` on
/// a file stating 2 ticks would charge 2%, and `commission_unit = "percent"` on one stating 0.05 a
/// contract 0.05%, so each is refused naming both keys. The stated unit set alone keeps the stated
/// amount, and a unit beside its amount runs.
#[test]
fn a_unit_set_alone_that_changes_the_stated_unit_is_refused() {
    for (params, words) in [
        (
            "commission_unit = \"percent\"",
            "Set strategy.params.commission beside strategy.params.commission_unit",
        ),
        (
            "slippage_unit = \"percent\"",
            "Set strategy.params.slippage beside strategy.params.slippage_unit",
        ),
    ] {
        let err = run_backtest(&profile(params, Some(PER_CONTRACT), Some(two_ticks())), store())
            .expect_err("a unit alone is refused")
            .to_string();
        assert!(err.contains(words) && err.contains("is set without"), "{params}: {err}");
    }
    let same = profile(
        "commission_unit = \"per_contract\"\nslippage_unit = \"ticks\"",
        Some(PER_CONTRACT),
        Some(two_ticks()),
    );
    let paid = run(&same);
    assert!((paid.fees_paid - 0.10).abs() < 1e-12, "the stated amount: {}", paid.fees_paid);
    run(&profile("slippage = 0.5\nslippage_unit = \"percent\"", None, Some(two_ticks())));
}

/// **A sweep over a cost param is no one cost**: a grid over `commission`, with no base commission and a
/// strategy stating none, is stamped `varies by point (axis: commission)` in the key and in words, and
/// never `charged_nothing`; the slippage it does not sweep keeps its own word.
#[test]
fn a_sweep_over_commission_is_not_stamped_free() {
    let p = profile("\n[paramscan]\ncommission = [0, 0.05, 0.1]", None, None);
    let stamp = crate::run::report::realism_stamp(&p);
    let varies = "varies by point (axis: commission)";
    assert_eq!(stamp.get("strategy.params.commission"), Some(varies));
    assert_eq!(charged(&p), (varies.to_string(), "none".to_string()));
    assert!(stamp.charged_nothing.is_none(), "a swept commission is never free: {stamp:?}");
}

/// **The run-level cost keys are refused in words naming the param** (the owner's ruling of 2026-11-18:
/// one path, no alias, nothing mapped).
#[test]
fn the_run_level_cost_keys_are_refused_naming_the_param() {
    for engine in [
        "fee_rate = 0.001",
        "fee = { kind = \"free\" }",
        "[engine.fee]\nkind = \"venue\"\nvenue = \"binance\"",
        "slippage = 0.0005",
        "[engine.slippage]\nmodel = \"ticks\"\nticks = 1.0\ntick = 0.25",
        "[engine.impact]\nmodel = \"almgren\"",
    ] {
        let err = ProfileBacktest::from_toml_str(&profile_text(engine, ""))
            .expect_err("a run-level cost key is refused")
            .to_string();
        let names = if engine.contains("slippage") || engine.contains("impact") {
            "slippage is a strategy parameter now"
        } else {
            "commission is a strategy parameter now"
        };
        assert!(err.contains(names) && err.contains("--set strategy.params."), "{engine}: {err}");
    }
}

/// A Studio run's profile (`ProfileBacktest::for_slice`, what `vike-backtesthub`'s `slice_profile` builds),
/// [`RoundTrip`] stating `fee` and `slippage` over the six minutes, its params `params`.
fn slice(
    costs: Option<&vike_backtesthub_client::wire_studio::StrategyEngineConfigWire>,
    params: vike_model::params::Params,
    fee: Option<FeeSchedule>,
    slippage: Option<ProfileEngineSlippage>,
) -> Result<ProfileBacktest, HarnessError> {
    let range = vike_data::TimestampRange { start: Some(S), end: Some(S + 6 * MINUTE - 1) };
    let mut strategy = crate::run::profile::ProfileStrategy::new("other/round_trip.rs", params);
    strategy.builder = Some(StrategyBuilder::new(move |_| {
        Ok(Box::new(RoundTrip { fee, slippage: slippage.clone() }))
    }));
    let symbols = ["BTCUSDT".to_string()];
    let kind = vike_model::run_request::replay::ReplayKind::Bar;
    ProfileBacktest::for_slice("binance", &symbols, "1m", range, kind, costs, strategy)
}

/// **A Studio run carries no cost of its own**: its profile holds none, so the strategy's stated costs are
/// charged; the params panel's `commission` and `slippage` rows ride in the params and win; an older
/// Studio's fee or slippage field is refused in words, never read.
#[test]
fn a_studio_slice_charges_the_strategy_params() {
    use vike_backtesthub_client::wire_studio::StrategyEngineConfigWire;
    let none = vike_model::params::Params::new();
    let stated = slice(None, none.clone(), Some(PER_CONTRACT), Some(two_ticks())).expect("a slice");
    let text = ProfileBacktest::slice_text(
        "binance",
        &["BTCUSDT".to_string()],
        "1m",
        vike_data::TimestampRange { start: Some(S), end: None },
        vike_model::run_request::replay::ReplayKind::Bar,
        None,
        stated.strategy.as_ref().unwrap(),
    )
    .expect("the slice's text");
    assert!(!text.contains("fee") && !text.contains("slippage"), "no cost in the run: {text}");
    let paid = run(&stated);
    assert!((paid.fees_paid - 0.10).abs() < 1e-12, "the strategy's fee: {}", paid.fees_paid);
    assert!((paid.final_equity + paid.fees_paid - 9_999.0).abs() < 1e-9, "its two ticks");

    let mut rows = vike_model::params::Params::new();
    rows.insert("commission", vike_model::params::ParamValue::Float(0.0));
    rows.insert("slippage", vike_model::params::ParamValue::Float(0.0));
    let theirs = run(&slice(None, rows, Some(PER_CONTRACT), Some(two_ticks())).expect("a slice"));
    assert_eq!((theirs.fees_paid, theirs.final_equity), (0.0, 10_000.0), "the user's rows win");

    for old in [
        StrategyEngineConfigWire { fee_rate: Some(0.001), ..StrategyEngineConfigWire::default() },
        StrategyEngineConfigWire { slippage: Some(0.0005), ..StrategyEngineConfigWire::default() },
    ] {
        let err = slice(Some(&old), none.clone(), None, None).expect_err("refused").to_string();
        assert!(err.contains("is a strategy parameter now"), "{err}");
    }
    let cash =
        StrategyEngineConfigWire { cash: Some(5_000.0), ..StrategyEngineConfigWire::default() };
    assert!(slice(Some(&cash), none, None, None).is_ok(), "cash alone is the run's");
}
