//! **The cost params' grammar: [`costs_of`] reads a run's commission and slippage into THE cost type,
//! `vike_model::Costs`** (the owner's ruling of 2026-11-18, decision 0269's addendum, and his word of
//! 2026-11-19: "1 type and unified"). A strategy file's backtest (`vike_backtest::run::costs::run_costs`),
//! the vectorized kernel (`vike_simulator::VectorBacktestEngine::run`) and a research study read the same
//! keys through it, so a key means one thing wherever it is written. The type, its commission
//! (`vike_model::Commission`, which a strategy file states too) and the schedule a commission is charged as
//! (`vike_model::Commission::schedule`) are vike-model's; the keys, their defaults and their refusals are here.
//!
//! - `commission` (a number >= 0) in `commission_unit`: `percent` of the fill's value (`0.05` is
//!   0.05%), `per_contract`, `per_order`, or `probability_scaled` (a prediction market's taker fee,
//!   `commission x p(1-p)` a share, `vike_model::FeeSchedule::ProbabilityScaled`).
//! - `slippage` (a number >= 0) in `slippage_unit`: `percent` of the price, or `ticks` of
//!   `slippage_tick` (the price of one tick), the shapes of `vike_model::ProfileEngineSlippage`.
//! - A key left out takes what the caller states (a strategy's `stated_fee`/`stated_slippage`): `commission =
//!   0.1` over a stated 0.05 per contract is 0.1 per contract. A stated shape no unit names (a schedule, a
//!   market impact model) is charged as stated until a unit is named. Nothing stated and nothing set is none.
//!
//! An engine is handed a `Costs` resolved: `vike_model::Commission::engine_fee` for the fee fields and
//! `crate::market_impact::slippage_terms` of its `slippage` for the slippage ones, the same two calls for
//! the event engine and the kernel. The refusals speak a strategy file's words (`strategy.params.…`), and each
//! is a [`CostRefusal`] variant, which the backtest profile's published schema lists
//! (`crate::refusals::CostRefusalId::ALL`).

use vike_model::params::{ParamValue, Params};
use vike_model::{
    Commission, CommissionUnit, Costs, FeeSchedule, ProfileEngineSlippage, Slippage, SlippageUnit,
};

use crate::refusals::CostRefusal;

/// The costs `params` set over what the caller states (the module doc): `stated_fee` and
/// `stated_slippage` are a strategy's own (`vike_model::Strategy::stated_fee`,
/// `vike_model::Strategy::stated_slippage`), `None` where nothing is stated.
///
/// # Errors
///
/// A cost param that is not what its key takes, in words naming the key.
pub fn costs_of(
    params: &Params,
    stated_fee: Option<FeeSchedule>,
    stated_slippage: Option<ProfileEngineSlippage>,
) -> Result<Costs, CostRefusal> {
    let (commission, commission_set) = commission_of(params, stated_fee)?;
    let (slippage, slippage_set) = slippage_of(params, stated_slippage)?;
    Ok(Costs { commission, commission_set, slippage, slippage_set })
}

/// A cost param that is a number: finite and not below zero, else refused in words.
fn amount(params: &Params, key: &str) -> Result<Option<f64>, CostRefusal> {
    let Some(v) = params.get(key) else { return Ok(None) };
    match v.as_f64() {
        Some(n) if n.is_finite() && n >= 0.0 => Ok(Some(n)),
        _ => Err(CostRefusal::NotANonNegativeNumber { key: key.to_string(), got: shown(v) }),
    }
}

/// A cost param that is a word.
fn word<'p>(params: &'p Params, key: &str) -> Result<Option<&'p str>, CostRefusal> {
    let Some(v) = params.get(key) else { return Ok(None) };
    v.as_str()
        .map(Some)
        .ok_or_else(|| CostRefusal::NotAWord { key: key.to_string(), got: shown(v) })
}

/// A param value in the words a refusal shows it: a number as itself, a word quoted, else its type.
fn shown(v: &ParamValue) -> String {
    match (v.as_f64(), v.as_str()) {
        (Some(n), _) => n.to_string(),
        (None, Some(s)) => format!("{s:?}"),
        (None, None) => format!("a {}", v.type_str()),
    }
}

fn commission_of(
    params: &Params,
    stated: Option<FeeSchedule>,
) -> Result<(Commission, bool), CostRefusal> {
    let [commission, commission_unit, ..] = vike_model::COST_PARAMS;
    let amount = amount(params, commission)?;
    let unit = match word(params, commission_unit)? {
        None => None,
        Some(w) => Some(
            CommissionUnit::of(w)
                .ok_or_else(|| CostRefusal::CommissionUnitUnknown { w: w.to_string() })?,
        ),
    };
    let stated = stated.filter(|s| !matches!(s, FeeSchedule::Free));
    if amount.is_none() && unit.is_none() {
        return Ok((stated.map_or(Commission::None, Commission::Schedule), false));
    }
    // A unit alone would read the strategy's own amount in another unit (0.05 a contract as 0.05%).
    if amount.is_none()
        && let (Some(unit), Some(s)) = (unit, &stated)
        && Commission::Schedule(*s).counted().map(|(_, own)| own) != Some(unit)
    {
        let own = Commission::Schedule(*s).words(|s| format!("{s:?}"));
        return Err(CostRefusal::CommissionUnitWithoutCommission { own });
    }
    let own = match &stated {
        None => None,
        Some(s) => match Commission::Schedule(*s).counted() {
            Some(counted) => Some(counted),
            None if unit.is_some() => None,
            None => {
                return Err(CostRefusal::CommissionStatedWithoutUnit { s: *s });
            }
        },
    };
    let unit = unit.or(own.map(|(_, u)| u)).unwrap_or(CommissionUnit::Percent);
    let amount = amount.or(own.map(|(a, _)| a)).unwrap_or(0.0);
    Ok((Commission::Counted { amount, unit }, true))
}

fn slippage_of(
    params: &Params,
    stated: Option<ProfileEngineSlippage>,
) -> Result<(Option<ProfileEngineSlippage>, bool), CostRefusal> {
    let [.., slippage, slippage_unit, _] = vike_model::COST_PARAMS;
    let amount = amount(params, slippage)?;
    let tick = amount_of_tick(params)?;
    let unit = match word(params, slippage_unit)? {
        None => None,
        Some(w) => Some(
            SlippageUnit::of(w)
                .ok_or_else(|| CostRefusal::SlippageUnitUnknown { w: w.to_string() })?,
        ),
    };
    if amount.is_none() && unit.is_none() && tick.is_none() {
        return Ok((stated, false));
    }
    let (own_amount, own_unit, own_tick) = match &stated {
        None => (0.0, None, None),
        Some(s) if s.model == "none" => (0.0, None, None),
        Some(s) if s.model == "percent" => {
            (s.value.unwrap_or(0.0) * 100.0, Some(SlippageUnit::Percent), None)
        }
        Some(s) if s.model == "ticks" => {
            (s.ticks.unwrap_or(0.0), Some(SlippageUnit::Ticks), s.tick)
        }
        Some(s) => {
            if unit.is_none() {
                return Err(CostRefusal::SlippageStatedWithoutUnit { model: s.model.clone() });
            }
            (0.0, None, None)
        }
    };
    // A unit alone would read the strategy's own amount in another unit (2 ticks as 2%).
    if amount.is_none()
        && let (Some(unit), Some(s)) = (unit, &stated)
        && s.model != "none"
        && own_unit != Some(unit)
    {
        let own = match own_unit {
            Some(SlippageUnit::Percent) => format!("{own_amount}% of the price"),
            Some(SlippageUnit::Ticks) => {
                format!("{own_amount} ticks of {}", own_tick.unwrap_or(0.0))
            }
            None => format!("the {} model", s.model),
        };
        return Err(CostRefusal::SlippageUnitWithoutSlippage { own });
    }
    let amount = amount.unwrap_or(own_amount);
    let slippage = match unit.or(own_unit).unwrap_or(SlippageUnit::Percent) {
        SlippageUnit::Percent => Slippage::percent(amount),
        SlippageUnit::Ticks => {
            let Some(tick) = tick.or(own_tick) else {
                return Err(CostRefusal::SlippageTicksWithoutTick);
            };
            Slippage::ticks(amount, tick)
        }
    };
    // The one mapping of a counted slippage to the engine's table (`vike_model::Slippage::table`).
    Ok((slippage.table(), true))
}

/// `slippage_tick`: a price, above zero.
fn amount_of_tick(params: &Params) -> Result<Option<f64>, CostRefusal> {
    let [.., slippage_tick] = vike_model::COST_PARAMS;
    match amount(params, slippage_tick)? {
        Some(0.0) => Err(CostRefusal::SlippageTickZero),
        t => Ok(t),
    }
}
