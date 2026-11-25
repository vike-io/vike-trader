//! The cost params' refusals (`crate::costs::costs_of`'s grammar), one variant each, in a strategy file's words
//! (`strategy.params.…`): the backtest profile's schema publishes them through [`CostRefusalId::ALL`].

use vike_model::FeeSchedule;

vike_utility::refusal_enum! {
    /// A refused cost param (`crate::costs::costs_of`'s grammar), one variant per refusal.
    #[derive(Debug, Clone, PartialEq)]
    pub enum CostRefusal with ids CostRefusalId {
        NotANonNegativeNumber { key: String, got: String } =>
            "profile.strategy.params.not_a_non_negative_number":
            "strategy.params.{key} must be a number >= 0, got {got}",
        NotAWord { key: String, got: String } => "profile.strategy.params.not_a_word":
            "strategy.params.{key} must be a word, got {got}",
        CommissionUnitUnknown { w: String } => "profile.strategy.params.commission_unit_unknown":
            "strategy.params.commission_unit = {w:?} is not a unit: percent (of the fill's \
             value, 0.05 is 0.05%), per_contract, per_order or probability_scaled",
        /// A unit with no amount over a strategy that states its commission in another unit.
        CommissionUnitWithoutCommission { own: String } =>
            "profile.strategy.params.commission_unit_without_commission":
            "strategy.params.commission_unit is set without strategy.params.commission, and the \
             strategy states its commission as {own}: the unit alone would read that amount in \
             another unit. Set strategy.params.commission beside strategy.params.commission_unit",
        CommissionStatedWithoutUnit { s: FeeSchedule } =>
            "profile.strategy.params.commission_stated_without_unit":
            "strategy.params.commission is set, and the strategy states a commission no unit \
             names ({s:?}): name strategy.params.commission_unit (percent, per_contract, \
             per_order or probability_scaled) beside it",
        SlippageUnitUnknown { w: String } => "profile.strategy.params.slippage_unit_unknown":
            "strategy.params.slippage_unit = {w:?} is not a unit: percent (of the price, 0.05 is \
             0.05%) or ticks (of strategy.params.slippage_tick, the price of one tick)",
        SlippageStatedWithoutUnit { model: String } =>
            "profile.strategy.params.slippage_stated_without_unit":
            "strategy.params.slippage is set, and the strategy states the {model} slippage model, \
             which no unit names: name strategy.params.slippage_unit (percent or ticks) beside it",
        /// A unit with no amount over a strategy that states its slippage in another unit.
        SlippageUnitWithoutSlippage { own: String } =>
            "profile.strategy.params.slippage_unit_without_slippage":
            "strategy.params.slippage_unit is set without strategy.params.slippage, and the \
             strategy states its slippage as {own}: the unit alone would read that amount in \
             another unit. Set strategy.params.slippage beside strategy.params.slippage_unit",
        SlippageTicksWithoutTick => "profile.strategy.params.slippage_ticks_without_tick":
            "strategy.params.slippage_unit = \"ticks\" needs strategy.params.slippage_tick, \
             the price of one tick (0.25)",
        SlippageTickZero => "profile.strategy.params.slippage_tick_zero":
            "strategy.params.slippage_tick must be the price of one tick, above zero, got 0",
    }
}
