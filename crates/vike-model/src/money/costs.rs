//! **THE cost type, [`Costs`], its commission, [`Commission`], and its counted slippage, [`Slippage`]** (the
//! owner's word of 2026-11-19: "1 type and unified"): what a strategy file STATES (`vike_tactics::RuleDefaults`'
//! `fee` and `slippage`), what a backtest
//! and a research study SET in params, and what the vectorized kernel is handed, one type for all of them.
//! Here, beside [`crate::FeeSchedule`] and [`crate::ProfileEngineSlippage`], because the order language
//! (`vike-tactics`, layer 15) states one and may name nothing above this crate.
//!
//! Data and its schedule only: the param grammar that reads a [`Costs`] off a run's keys
//! (`commission` in `commission_unit`, `slippage` in `slippage_unit`) and its refusals are
//! `vike_fills::costs::costs_of`'s. A commission reaches an engine as the [`crate::FeeSchedule`]
//! [`Commission::schedule`] builds, a slippage as the table [`Slippage::table`] builds: one mapping each (they
//! were `vike_tactics`' `RuleDefaultsFee::schedule` and `RuleDefaultsSlippage::table`, each with a copy in the
//! backtest's grammar).

use crate::{FeeSchedule, ProfileEngineSlippage};

/// **The parameters every strategy file has without declaring them** (the owner's ruling of
/// 2026-11-18, decision 0269's addendum): what a backtest charges each fill, a strategy PARAMETER like
/// any other. `commission` (a number) in `commission_unit` ([`CommissionUnit::WORDS`]), and `slippage`
/// (a number) in `slippage_unit` ([`SlippageUnit::WORDS`]), `slippage_tick` the price of one tick.
/// Here, beside [`Costs`], so the file header that refuses to declare one, the param grammar that
/// reads them and the surfaces that name them read one list. The order is published: keep it.
pub const COST_PARAMS: [&str; 5] =
    ["commission", "commission_unit", "slippage", "slippage_unit", "slippage_tick"];

/// What a counted commission is counted in (`commission_unit`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommissionUnit {
    /// Of the fill's value, in percent: `0.05` is 0.05%.
    Percent,
    /// Per contract (unit) filled.
    PerContract,
    /// Per order, whatever its size.
    PerOrder,
    /// A taker fill's `commission x p(1-p)` a share, `p` its price as a probability (a maker pays none).
    ProbabilityScaled,
}

impl CommissionUnit {
    /// Every `commission_unit` word with its unit, in the order a refusal or a help text lists them.
    pub const WORDS: [(&'static str, CommissionUnit); 4] = [
        ("percent", CommissionUnit::Percent),
        ("per_contract", CommissionUnit::PerContract),
        ("per_order", CommissionUnit::PerOrder),
        ("probability_scaled", CommissionUnit::ProbabilityScaled),
    ];

    /// The unit a `commission_unit` word names, `None` for a word that names none.
    pub fn of(word: &str) -> Option<Self> {
        Self::WORDS.iter().find(|(w, _)| *w == word).map(|(_, u)| *u)
    }

    /// The `commission_unit` word of this unit.
    pub fn word(self) -> &'static str {
        Self::WORDS.iter().find(|(_, u)| *u == self).map_or("percent", |(w, _)| w)
    }
}

/// What a counted slippage is counted in (`slippage_unit`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SlippageUnit {
    /// Of the price, in percent: `0.05` is 0.05%.
    Percent,
    /// Ticks of `slippage_tick`.
    Ticks,
}

impl SlippageUnit {
    /// Every `slippage_unit` word with its unit, in the order a refusal or a help text lists them.
    pub const WORDS: [(&'static str, SlippageUnit); 2] =
        [("percent", SlippageUnit::Percent), ("ticks", SlippageUnit::Ticks)];

    /// The unit a `slippage_unit` word names, `None` for a word that names none.
    pub fn of(word: &str) -> Option<Self> {
        Self::WORDS.iter().find(|(w, _)| *w == word).map(|(_, u)| *u)
    }

    /// The `slippage_unit` word of this unit.
    pub fn word(self) -> &'static str {
        Self::WORDS.iter().find(|(_, u)| *u == self).map_or("percent", |(w, _)| w)
    }
}

/// A slippage counted in a unit, against the trader on each fill: none, a percent of the price, or ticks of
/// a tick's price. What a strategy file states (`vike_tactics::RuleDefaults::slippage`) and what the param
/// grammar counts (`vike_fills::costs::costs_of`); an engine is handed the table [`Self::table`] builds,
/// which [`Costs::slippage`] holds beside the shapes no unit names (a market impact model).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Slippage {
    /// No slippage.
    None,
    /// `amount` in `unit`; `tick` is the price of one tick, which only [`SlippageUnit::Ticks`] reads.
    Counted { amount: f64, unit: SlippageUnit, tick: Option<f64> },
}

impl Slippage {
    /// `percent` of the fill's price, as a rule states it: `0.05` is 0.05%.
    pub const fn percent(percent: f64) -> Self {
        Slippage::Counted { amount: percent, unit: SlippageUnit::Percent, tick: None }
    }

    /// `ticks` ticks of `tick` (the price one tick is), on every fill no limit bounds.
    pub const fn ticks(ticks: f64, tick: f64) -> Self {
        Slippage::Counted { amount: ticks, unit: SlippageUnit::Ticks, tick: Some(tick) }
    }

    /// The slippage table an engine is handed for it (`crate::Strategy::stated_slippage`, [`Costs::slippage`]):
    /// the `percent` model (a fraction of the price) or the `ticks` model, the one mapping (it was
    /// `vike_tactics::RuleDefaultsSlippage::table` and a copy in the backtest's grammar); `None` states none.
    pub fn table(&self) -> Option<ProfileEngineSlippage> {
        let blank = |model: &str| ProfileEngineSlippage {
            model: model.to_string(),
            value: None,
            ticks: None,
            tick: None,
            base: None,
            exec_time: None,
            window: None,
            gamma: None,
            eta: None,
        };
        match *self {
            Slippage::None => None,
            Slippage::Counted { amount, unit: SlippageUnit::Percent, .. } => {
                Some(ProfileEngineSlippage { value: Some(amount / 100.0), ..blank("percent") })
            }
            Slippage::Counted { amount, unit: SlippageUnit::Ticks, tick } => {
                Some(ProfileEngineSlippage { ticks: Some(amount), tick, ..blank("ticks") })
            }
        }
    }
}

/// A commission: none, an amount in a unit, or a schedule charged as it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Commission {
    /// No commission.
    None,
    /// `amount` in `unit`.
    Counted { amount: f64, unit: CommissionUnit },
    /// A schedule charged as stated (a shape no unit names: a maker and taker split, a floor, a cap).
    Schedule(FeeSchedule),
}

impl Commission {
    /// `percent` of the fill's value, as a rule states it: `0.05` is 0.05%.
    pub const fn percent(percent: f64) -> Self {
        Commission::Counted { amount: percent, unit: CommissionUnit::Percent }
    }

    /// `amount` a contract (unit) filled.
    pub const fn per_contract(amount: f64) -> Self {
        Commission::Counted { amount, unit: CommissionUnit::PerContract }
    }

    /// `amount` an order, whatever its size.
    pub const fn per_order(amount: f64) -> Self {
        Commission::Counted { amount, unit: CommissionUnit::PerOrder }
    }

    /// The engine's schedule for it (`crate::Strategy::stated_fee`, `vike_simulator::StrategyEngineConfig::fee_schedule`);
    /// `None` charges nothing.
    pub fn schedule(&self) -> Option<FeeSchedule> {
        match *self {
            Commission::None => None,
            Commission::Counted { amount, unit } => Some(match unit {
                CommissionUnit::Percent => FeeSchedule::PercentMakerTaker {
                    maker_bps: amount * 100.0,
                    taker_bps: amount * 100.0,
                },
                CommissionUnit::PerContract => {
                    FeeSchedule::PerShareWithFloor { per_share: amount, min: 0.0, max_percent: 0.0 }
                }
                CommissionUnit::PerOrder => {
                    FeeSchedule::PerShareWithFloor { per_share: 0.0, min: amount, max_percent: 0.0 }
                }
                CommissionUnit::ProbabilityScaled => FeeSchedule::ProbabilityScaled {
                    taker_rate: amount,
                    maker_rate: 0.0,
                    maker_rebate_share: 0.0,
                },
            }),
            Commission::Schedule(s) => Some(s),
        }
    }

    /// The engine's two fee fields, `(fee_rate, fee_schedule)`: the flat rate is never used, so a
    /// commission is charged as the very schedule [`Self::schedule`] builds.
    pub fn engine_fee(&self) -> (f64, Option<FeeSchedule>) {
        (0.0, self.schedule())
    }

    /// The commission as an amount in a unit, where one names it: a stated schedule is read back
    /// ([`Self::schedule`]'s inverse), `None` for no commission and for a shape no unit names.
    pub fn counted(&self) -> Option<(f64, CommissionUnit)> {
        match *self {
            Commission::None => None,
            Commission::Counted { amount, unit } => Some((amount, unit)),
            Commission::Schedule(s) => counted_of(&s),
        }
    }

    /// Whether every fill is free.
    pub fn is_free(&self) -> bool {
        match self {
            Commission::None => true,
            Commission::Counted { amount, .. } => *amount == 0.0,
            Commission::Schedule(s) => matches!(s, FeeSchedule::Free),
        }
    }

    /// The commission in words: `none`, `0.05% of the fill's value`, `0.05 per contract`, …, or a stated
    /// schedule no unit names in `schedule_words`.
    pub fn words(&self, schedule_words: impl Fn(&FeeSchedule) -> String) -> String {
        if self.is_free() {
            return "none".to_string();
        }
        let Some(counted) = self.counted() else {
            return match self {
                Commission::Schedule(s) => schedule_words(s),
                _ => "none".to_string(),
            };
        };
        match counted {
            (amount, CommissionUnit::Percent) => format!("{amount}% of the fill's value"),
            (amount, CommissionUnit::PerContract) => format!("{amount} per contract"),
            (amount, CommissionUnit::PerOrder) => format!("{amount} per order"),
            (amount, CommissionUnit::ProbabilityScaled) => {
                format!("{amount} x p(1-p) a share, taker")
            }
        }
    }
}

/// A schedule read back as an amount in a unit, where one names it.
fn counted_of(stated: &FeeSchedule) -> Option<(f64, CommissionUnit)> {
    match *stated {
        FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } if maker_bps == taker_bps => {
            Some((taker_bps / 100.0, CommissionUnit::Percent))
        }
        FeeSchedule::PerShareWithFloor { per_share, min, max_percent }
            if min == 0.0 && max_percent == 0.0 =>
        {
            Some((per_share, CommissionUnit::PerContract))
        }
        FeeSchedule::PerShareWithFloor { per_share, min, max_percent }
            if per_share == 0.0 && max_percent == 0.0 =>
        {
            Some((min, CommissionUnit::PerOrder))
        }
        FeeSchedule::ProbabilityScaled { taker_rate, maker_rate, maker_rebate_share }
            if maker_rate == 0.0 && maker_rebate_share == 0.0 =>
        {
            Some((taker_rate, CommissionUnit::ProbabilityScaled))
        }
        _ => None,
    }
}

/// What a run charges each fill: THE cost type (the module doc).
#[derive(Debug, Clone, PartialEq)]
pub struct Costs {
    pub commission: Commission,
    /// Whether a `commission*` key set it (else the stated one, or none).
    pub commission_set: bool,
    /// The slippage as the `[engine.slippage]`-shaped table the engine is handed; `None` is none.
    pub slippage: Option<ProfileEngineSlippage>,
    /// Whether a `slippage*` key set it.
    pub slippage_set: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schedule of a counted commission reads back as that amount in that unit, and the four
    /// one-line constructors are the four counted shapes a rule states.
    #[test]
    fn a_counted_commission_round_trips_through_its_schedule() {
        for (c, unit) in [
            (Commission::percent(0.05), CommissionUnit::Percent),
            (Commission::per_contract(0.5), CommissionUnit::PerContract),
            (Commission::per_order(1.5), CommissionUnit::PerOrder),
        ] {
            let schedule = c.schedule().expect("a counted commission has a schedule");
            assert_eq!(Commission::Schedule(schedule).counted(), c.counted());
            assert_eq!(c.counted().map(|(_, u)| u), Some(unit));
            assert_eq!(CommissionUnit::of(unit.word()), Some(unit));
        }
        assert_eq!(Commission::None.schedule(), None);
    }

    /// The cost params and the two units' words, pinned in their published order, each word naming its
    /// unit and each unit its word.
    #[test]
    fn the_cost_params_and_the_unit_words_are_pinned() {
        assert_eq!(
            COST_PARAMS.join(","),
            "commission,commission_unit,slippage,slippage_unit,slippage_tick"
        );
        let commission: Vec<&str> = CommissionUnit::WORDS.iter().map(|(w, _)| *w).collect();
        assert_eq!(commission, ["percent", "per_contract", "per_order", "probability_scaled"]);
        let slippage: Vec<&str> = SlippageUnit::WORDS.iter().map(|(w, _)| *w).collect();
        assert_eq!(slippage, ["percent", "ticks"]);
        for (word, unit) in CommissionUnit::WORDS {
            assert_eq!((CommissionUnit::of(word), unit.word()), (Some(unit), word));
        }
        for (word, unit) in SlippageUnit::WORDS {
            assert_eq!((SlippageUnit::of(word), unit.word()), (Some(unit), word));
        }
        assert_eq!(SlippageUnit::of("pips"), None);
    }

    /// A stated slippage's table is the shape the profile's own check takes: a percent is the table's
    /// fraction of the price, ticks keep their tick, and none states none.
    #[test]
    fn a_counted_slippage_is_the_table_the_engine_takes() {
        let percent = Slippage::percent(0.05).table().expect("stated");
        assert_eq!((percent.model.as_str(), percent.value), ("percent", Some(0.05 / 100.0)));
        let ticks = Slippage::ticks(2.0, 0.25).table().expect("stated");
        assert_eq!(
            (ticks.model.as_str(), ticks.ticks, ticks.tick),
            ("ticks", Some(2.0), Some(0.25))
        );
        assert_eq!((percent.check(), ticks.check()), (Ok(()), Ok(())));
        assert_eq!(Slippage::None.table(), None);
    }
}
