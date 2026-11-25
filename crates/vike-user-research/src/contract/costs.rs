//! The costs a study charges: THE cost type, `vike_model::Costs`, read off a recipe by the grammar a strategy
//! file's backtest reads its params by (`vike_fills::costs::costs_of`; the owner's word of 2026-11-19, "1 type and
//! unified"). A study keeps no cost formula of its own: it hands the `Costs` to the engine it runs, the
//! vectorized kernel (`vike_simulator::VectorBacktestEngine::run`) or the event engine (its fee through
//! `vike_model::Commission::engine_fee`), and each fill is charged as a backtest charges it.
//!
//! The recipe's keys are the backtest's, at its top level, each optional (absent is none): `commission` in
//! `commission_unit` (`percent` of the fill's value, the default, `per_contract`, `per_order` or
//! `probability_scaled`) and `slippage` in `slippage_unit` (`percent` of the price, the default, or `ticks` of
//! `slippage_tick`). A `[stated_slippage]` table is a slippage the recipe states, as a strategy file states one:
//! `impact_fit`'s `stated_slippage.toml`, pasted under it, charges the fitted market impact.

use vike_model::params::Params;
use vike_model::{Costs, ProfileEngineSlippage};

use super::StudyError;

/// The costs `recipe` states (the module doc).
///
/// # Errors
///
/// A cost key that is not what it takes, in the recipe's words, and a `[stated_slippage]` that is no slippage
/// table.
pub fn costs_of_recipe(recipe: &toml::Value) -> Result<Costs, StudyError> {
    let params: Params = recipe.clone().try_into().map_err(|e| {
        StudyError::Study(format!("the recipe is not a table of keys and values: {e}"))
    })?;
    let stated = match recipe.get("stated_slippage") {
        None => None,
        Some(table) => Some(table.clone().try_into::<ProfileEngineSlippage>().map_err(|e| {
            StudyError::Study(format!("the recipe's [stated_slippage] is no slippage table: {e}"))
        })?),
    };
    // shortcut: the grammar speaks a strategy file's words (the backtest profile's schema publishes them), so
    // a recipe reads them in its own by a rewrite; upgrade when `costs_of` takes the words' key prefix.
    vike_fills::costs::costs_of(&params, None, stated).map_err(|why| {
        StudyError::Study(
            why.to_string()
                .replace("strategy.params.", "recipe.")
                .replace("the strategy states", "the recipe's [stated_slippage] states"),
        )
    })
}
