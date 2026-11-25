//! **What a backtest charges each fill: the strategy's commission and slippage, two strategy
//! PARAMETERS** (the owner's ruling of 2026-11-18, decision 0269's addendum). Every strategy file
//! has them without declaring them (`vike_model::COST_PARAMS`), the user sets them as any
//! other param (`[strategy.params]`, `--set strategy.params.commission=…`, a Studio params row, a
//! `[paramscan]` axis), and each defaults to what the file itself states (a rule's `RuleDefaults`,
//! crossing as `vike_model::Strategy::stated_fee` and `vike_model::Strategy::stated_slippage`), else
//! **none**: no venue table is applied to a strategy file's backtest.
//!
//! What the keys mean is `vike_fills::costs` (`commission` in `commission_unit`, `slippage` in
//! `slippage_unit` with `slippage_tick`), the grammar a research study and the vectorized kernel read too,
//! and the one type a run's costs are is `vike_model::Costs`. This module reads them off a profile and its strategy, and says
//! in a stamp whose they were.
//!
//! The run-level keys this replaced are REFUSED, never read (the owner's word: one path, no alias):
//! `[engine.fee]`, `engine.fee_rate`, `[engine.slippage]` and the older `[engine.impact]`
//! ([`refuse_run_level_costs`]), each naming the param that replaces it.

use vike_fills::costs::costs_of;
use vike_model::Costs;
use vike_model::params::Params;

use super::profile::ProfileBacktest;
use crate::HarnessError;
use crate::run::refusals::ProfileRefusal;

/// The run's costs: the strategy's params, else what the strategy states, else none (the module
/// doc). The strategy is built to read what it states, as its inputs are read; one that does not
/// build states nothing here (the run is refused for that first). A run that has built its strategy
/// reads the same off it instead ([`costs_of_built`]).
/// A portfolio's are its first mount's, which every mount must charge
/// (`ProfileBacktest::mount_costs` refuses a run whose mounts differ).
pub fn run_costs(profile: &ProfileBacktest) -> Result<Costs, HarnessError> {
    let Some(table) = profile.law_strategy() else {
        return costs_of(&Params::new(), None, None).map_err(|refusal| {
            HarnessError::from(ProfileRefusal::StrategyCostRefused { refusal })
        });
    };
    match table.build() {
        Ok(strategy) => costs_of_table(table, strategy.as_ref()),
        Err(_) => costs_of(&table.params, None, None)
            .map_err(|refusal| HarnessError::from(ProfileRefusal::StrategyCostRefused { refusal })),
    }
}

/// [`run_costs`] off a strategy the run already built: what the engine's two construction sites
/// read, so a builder that hands out one strategy (the strategy labs') is never asked for a second.
pub fn costs_of_built(
    profile: &ProfileBacktest,
    strategy: &dyn vike_model::Strategy<vike_simulator::OrderBrokerSimulator>,
) -> Result<Costs, HarnessError> {
    match profile.law_strategy() {
        Some(table) => costs_of_table(table, strategy),
        None => costs_of(&Params::new(), strategy.stated_fee(), strategy.stated_slippage())
            .map_err(|refusal| HarnessError::from(ProfileRefusal::StrategyCostRefused { refusal })),
    }
}

/// [`costs_of_built`] of one strategy table (a portfolio's mount) and the strategy built from it.
pub(crate) fn costs_of_table(
    table: &super::profile::ProfileStrategy,
    strategy: &dyn vike_model::Strategy<vike_simulator::OrderBrokerSimulator>,
) -> Result<Costs, HarnessError> {
    costs_of(&table.params, strategy.stated_fee(), strategy.stated_slippage())
        .map_err(|refusal| HarnessError::from(ProfileRefusal::StrategyCostRefused { refusal }))
}

/// Whose a cost is: a param the run set, or the strategy's own default.
pub fn whose(set: bool) -> &'static str {
    if set { "strategy.params" } else { "the strategy's own" }
}

/// **The cost params a grid varies**, as `(commission's, slippage's)`: the keys of `axes` (a profile's
/// `[paramscan]` table, a Studio sweep's axes) that are cost params (`vike_model::COST_PARAMS`).
/// A search over one charged no one cost, each point its own, so its stamp says so
/// ([`varies_by_point`]) and never that the search was free.
pub fn swept_cost_params<'a>(
    axes: impl IntoIterator<Item = &'a str>,
) -> (Vec<&'a str>, Vec<&'a str>) {
    let swept: Vec<&str> =
        axes.into_iter().filter(|k| vike_model::COST_PARAMS.contains(k)).collect();
    swept.into_iter().partition(|k| k.starts_with("commission"))
}

/// A swept cost in words: `varies by point (axis: commission)`.
pub fn varies_by_point(axes: &[&str]) -> String {
    format!("varies by point (axis: {})", axes.join(", "))
}

/// **Refuse the run-level cost keys** this module replaced, read off the profile's raw table before
/// it is typed (the module doc): each refusal names the strategy param that replaces it. Nothing is
/// mapped or migrated (the owner's standing rule, and his word of 2026-11-18: one path, no alias).
pub(crate) fn refuse_run_level_costs(doc: &toml::Table) -> Result<(), HarnessError> {
    let Some(engine) = doc.get("engine").and_then(toml::Value::as_table) else { return Ok(()) };
    if let Some(rate) = engine.get("fee_rate") {
        return Err(HarnessError::from(ProfileRefusal::EngineFeeRateNoLongerRead {
            rate: rate.clone(),
        }));
    }
    if engine.contains_key("fee") {
        return Err(HarnessError::from(ProfileRefusal::EngineFeeNoLongerRead));
    }
    if engine.contains_key("slippage") || engine.contains_key("impact") {
        let key = if engine.contains_key("impact") { "[engine.impact]" } else { "engine.slippage" };
        return Err(HarnessError::from(ProfileRefusal::EngineSlippageNoLongerRead {
            key: key.to_string(),
        }));
    }
    Ok(())
}

/// What a refused run-level commission key is told: the param that replaced it.
pub const COMMISSION_NOW: &str = "commission is a strategy parameter now (the owner's ruling of \
     2026-11-18): set [strategy.params] commission = <amount> with commission_unit = \"percent\" \
     (of the fill's value, 0.05 is 0.05%), \"per_contract\", \"per_order\" or \
     \"probability_scaled\", or --set strategy.params.commission=<amount>; left out, the strategy's \
     own commission applies, else none";

/// What a refused run-level slippage key is told: the param that replaced it.
pub const SLIPPAGE_NOW: &str = "slippage is a strategy parameter now (the owner's ruling of \
     2026-11-18): set [strategy.params] slippage = <amount> with slippage_unit = \"percent\" (of \
     the price) or \"ticks\" with slippage_tick = <the price of one tick>, or \
     --set strategy.params.slippage=<amount>; left out, the strategy's own slippage applies, else \
     none";
