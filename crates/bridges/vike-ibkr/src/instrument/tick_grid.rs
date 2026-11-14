//! **An order's price is held to the venue's REAL grid at that price** (decision 0220, N08).
//!
//! The facts answer ONE grid tick per stock: the increment of the band its price fell in when the facts were asked
//! (`crate::instrument::any_symbol`, "The grid"). The facts are kept ten minutes by the gatewayhub and an hour by the node, and a
//! market rule is a TABLE of bands, so a price that leaves its band afterwards (a stock crossing 100, a limit set far
//! from the mark) can sit off the grid of the band it is in: IB would refuse it with `[110] The price does not conform
//! to the minimum price variation`, and an engine that rounds to its one tick would never have said why.
//!
//! So the facts source notes every market rule it reads, by symbol, in a process-wide table ([`note`]; the gatewayhub
//! hosts the facts source and the exec of an account in one process, as it does for `crate::execution::delayed_order`'s marks),
//! and so does the engine's own MOUNT, whose grid pre-fetch reads the rules by the same rule (`crate::instrument::properties`'s
//! `on_the_band`, the audit's G5), and the exec asks it before a price goes to the transport ([`refusal`]): a limit or
//! trigger price that is on the grid of NO rule the contract names, at that price, is refused by name with the venue's
//! real increment, never sent and never rounded. ⚠ **Fail-open**: a symbol with no table is the venue's to judge, since
//! there is nothing to judge it against (a mount whose pre-fetch did not run, failed or read no rules, the cpapi
//! backend's mount, a facts source that failed, a forex pair or a future admitted by name, whose facts note nothing); a
//! price on the grid of ANY one of the contract's rules (one per listed exchange) is let through, because which exchange
//! SMART routes to is not the adapter's to say.
//! A price AMENDMENT (`Modify`) is not judged here, as for `crate::execution::delayed_order`.
//!
//! A DISTANCE is the one number this module ROUNDS rather than refuses: no price the operator read off a screen but a
//! difference of prices, put on the grid toward the passive side and refused only when a non-zero one rounds to
//! nothing ([`put_distances_on_grid`]). A pegged order's offset rounds toward the passive side in vike's sign
//! ([`peg_offset_on_grid`]); a discretionary amount, a trailing order's trail and limit offset, and an adjustment's
//! trail AMOUNT (a percent is no price) round DOWN, so the order never reaches, or trails, further than asked (the
//! coordinator's rulings of 2026-11-09). An adjustment's level and its target's stop and limit are PRICES, held to the grid like the order's own.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use vike_bridge_core::format::format_to_step_f;
use vike_model::{OrderRequest, OrderStopAdjustTo, snap_limit};

use crate::instrument::any_symbol::{PriceBand, increment_at};

/// How many symbols the table holds: the engine's own cap on admitted symbols is far below it, so a full table means a
/// runaway caller, and the symbol past the cap is simply not noted (its price is then the venue's to judge).
#[cfg(feature = "ibkr-socket")]
const MAX_GRIDS: usize = 256;

/// What the facts of one symbol read: the market rules and the price the grid tick was chosen for (`None`: none was
/// known).
type Noted = (Vec<Vec<PriceBand>>, Option<f64>);

/// The rules per symbol (the engine's spelling, upper case).
static GRIDS: LazyLock<Mutex<HashMap<String, Noted>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The facts source, or a mount's grid pre-fetch, read `rules` for `symbol` at `facts_price` (`None`: no price was
/// known): keep them for the exec.
#[cfg(feature = "ibkr-socket")]
pub(crate) fn note(symbol: &str, rules: &[Vec<PriceBand>], facts_price: Option<f64>) {
    let mut grids = GRIDS.lock().unwrap_or_else(PoisonError::into_inner);
    let noted: Noted = (rules.to_vec(), facts_price);
    if let Some(slot) = grids.get_mut(symbol) {
        *slot = noted;
    } else if grids.len() < MAX_GRIDS {
        grids.insert(symbol.to_string(), noted);
    }
}

/// Whether `price` is a whole number of `increment`s (float noise tolerated).
fn on_grid(price: f64, increment: f64) -> bool {
    let steps = price / increment;
    (steps - steps.round()).abs() <= 1e-6 * steps.abs().max(1.0)
}

/// The increments of `rules` at `price`, finest first and without repeats, when `price` is on the grid of NONE of them
/// (`None`: it is on at least one, or there is nothing to judge).
fn off_grid(rules: &[Vec<PriceBand>], price: f64) -> Option<Vec<f64>> {
    if rules.is_empty() || !price.is_finite() || price <= 0.0 {
        return None;
    }
    let mut at: Vec<f64> = rules.iter().map(|bands| increment_at(bands, price)).collect();
    if at.iter().any(|inc| on_grid(price, *inc)) {
        return None;
    }
    at.sort_by(f64::total_cmp);
    at.dedup();
    Some(at)
}

/// A pegged order's offset ON the venue's grid, or why it cannot be (the owner's ruling: the wire owes the grid
/// multiple). The step is the increment of the band the symbol's price fell in when the facts were read, the coarsest of
/// the contract's rules there (a multiple of it is on every rule's grid), and the offset is rounded to it toward the
/// PASSIVE side (vike's sign: a positive, aggressive offset down toward zero, a negative one further down) by
/// [`floor_on_grid`], the model's rule written as the wire writes it. `Ok(None)`: nothing to change (no peg, an
/// offset of 0, or fail-open as [`refusal`]: no table for the symbol, or no price to read the band off). `Err`: a
/// non-zero offset that rounds to nothing, refused in words.
pub(crate) fn peg_offset_on_grid(req: &OrderRequest) -> Result<Option<f64>, String> {
    let Some(offset) = req.peg.map(|p| p.offset).filter(|o| *o != 0.0 && o.is_finite()) else {
        return Ok(None);
    };
    let Some(step) = distance_step(&req.symbol) else { return Ok(None) };
    let gridded = floor_on_grid(offset, step);
    if gridded == 0.0 {
        return Err(format!(
            "ibkr: the peg offset {offset} of the order on {} rounds to nothing on the venue's price grid (IB's market \
             rule steps by {step} there): an offset is a whole number of steps, so send 0 or at least {step}; nothing \
             was sent",
            req.symbol
        ));
    }
    Ok(Some(gridded))
}

/// `distance` floored onto the `step` grid (DOWN in vike's sign, a negative one further from zero) by the ONE model rule
/// for a passive price (`vike_model::snap_limit` of a buy, decision 0269 row 27; the owner's D11 and G2): one a few ULP
/// under a grid point names it (`0.3 - 0.2` is 0.1, not 0.05), one within a ten-thousandth of a step above it stays on
/// it. Its magnitude is read back through `format_to_step_f`, the pinned Decimal wire site, so it is the grid multiple.
fn floor_on_grid(distance: f64, step: f64) -> f64 {
    let down = snap_limit(1, distance, Some(step));
    down.signum() * format_to_step_f(down.abs(), step).parse::<f64>().unwrap_or(0.0)
}

/// The step a DISTANCE of `symbol` goes on: the increment of the band the symbol's price fell in when the facts were
/// read, the coarsest of the contract's rules there (a multiple of it is on every rule's grid). `None`: fail-open (no
/// table for the symbol, or no price to read the band off).
fn distance_step(symbol: &str) -> Option<f64> {
    let grids = GRIDS.lock().unwrap_or_else(PoisonError::into_inner);
    let Some((rules, Some(at))) = grids.get(symbol) else { return None };
    let step = rules.iter().map(|bands| increment_at(bands, *at)).fold(0.0, f64::max);
    (step.is_finite() && step > 0.0).then_some(step)
}

/// `(peg offset, discretionary amount, trailing order's trail, its limit offset, adjustment's trail amount)` on the grid.
type Gridded = (Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>);

/// The distances of `req` on the venue's grid, each `None` where nothing changes (none stated, or fail-open). `Err`: the
/// first non-zero one that rounds to nothing, in words.
fn gridded_distances(req: &OrderRequest) -> Result<Gridded, String> {
    let peg = peg_offset_on_grid(req)?;
    let Some(step) = distance_step(&req.symbol) else { return Ok((peg, None, None, None, None)) };
    // Rounded DOWN: never more than asked (the module doc).
    let down = |what: &str, distance: Option<f64>| -> Result<Option<f64>, String> {
        let Some(d) = distance.filter(|d| d.is_finite() && *d > 0.0) else { return Ok(None) };
        match floor_on_grid(d, step) {
            gridded if gridded > 0.0 => Ok(Some(gridded)),
            _ => Err(format!(
                "ibkr: the {what} {d} of the order on {} rounds to nothing on the venue's price grid (IB's market \
                 rule steps by {step} there): it is a whole number of steps, so send at least {step}; nothing was \
                 sent",
                req.symbol
            )),
        }
    };
    let discretion =
        down("discretionary amount", req.attributes.and_then(|a| a.discretionary_amount))?;
    let trail = down("trail", req.trail.filter(|_| req.order_type.is_trailing()))?;
    // A `TRAIL LIMIT`'s / `TRAIL LIT`'s limit offset; one of 0 stays 0.
    let limit_offset =
        down("limit offset", req.limit_offset.filter(|_| req.order_type.is_trailing()))?;
    let adjust_trail = match req.adjust.map(|a| a.to) {
        Some(OrderStopAdjustTo::Trailing { trail, .. }) => down("adjustment's trail", trail)?,
        _ => None,
    };
    Ok((peg, discretion, trail, limit_offset, adjust_trail))
}

/// Put `req`'s distances on the venue's grid (the module doc); unchanged when nothing is to change or one is refused
/// (the refusal is [`refusal`]'s to say, before the order gets here).
pub(crate) fn put_distances_on_grid(req: &mut OrderRequest) {
    let Ok((peg, discretion, trail, limit_offset, adjust_trail)) = gridded_distances(req) else {
        return;
    };
    if let (Some(offset), Some(p)) = (peg, req.peg.as_mut()) {
        p.offset = offset;
    }
    if let (Some(d), Some(a)) = (discretion, req.attributes.as_mut()) {
        a.discretionary_amount = Some(d);
    }
    if trail.is_some() {
        req.trail = trail;
    }
    if limit_offset.is_some() {
        req.limit_offset = limit_offset;
    }
    if let (Some(t), Some(OrderStopAdjustTo::Trailing { trail, .. })) =
        (adjust_trail, req.adjust.as_mut().map(|a| &mut a.to))
    {
        *trail = Some(t);
    }
}

/// Why a price of `req` is refused for being off the venue's grid, or `None` (the module doc's rule): the order's own
/// prices and an adjustment's level and target prices. A distance that rounds to nothing is refused here too
/// ([`put_distances_on_grid`]).
pub(crate) fn refusal(req: &OrderRequest) -> Option<String> {
    if let Err(why) = gridded_distances(req) {
        return Some(why);
    }
    let adjusted = req.adjust.map(|a| {
        let limit = match a.to {
            OrderStopAdjustTo::StopLimit { limit, .. } => Some(limit),
            OrderStopAdjustTo::Stop { .. } | OrderStopAdjustTo::Trailing { .. } => None,
        };
        [Some(a.at), Some(a.to.stop()), limit]
    });
    let priced: Vec<f64> = [req.price, req.trigger_price]
        .into_iter()
        .chain(adjusted.into_iter().flatten())
        .flatten()
        .collect();
    if priced.is_empty() {
        return None;
    }
    let grids = GRIDS.lock().unwrap_or_else(PoisonError::into_inner);
    let (rules, facts_price) = grids.get(req.symbol.as_str())?;
    let (price, steps) = priced.iter().find_map(|p| off_grid(rules, *p).map(|s| (*p, s)))?;
    // `steps` is finest first: the last is the coarsest, a multiple of which is on every rule's grid at this price.
    let coarsest = steps.last().copied().unwrap_or_default();
    let steps: Vec<String> = steps.iter().map(f64::to_string).collect();
    Some(format!(
        "ibkr: the price {price} of the order on {} is off the venue's price grid: IB's market rule steps by {} at that \
         price, and {price} is not a multiple of it, so the order would be refused by the venue (a price that left the \
         band the node's grid tick was read for{}). Correct the price to a multiple of {}",
        req.symbol,
        steps.join(" or "),
        facts_price.map_or_else(
            || " with no price known".to_string(),
            |p| format!(": the facts were read at a price of {p}")
        ),
        coarsest
    ))
}

#[cfg(test)]
#[path = "tick_grid_tests.rs"]
mod tick_grid_tests;
