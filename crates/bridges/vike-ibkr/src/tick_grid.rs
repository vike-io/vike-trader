//! **An order's price is held to the venue's REAL grid at that price** (decision 0220, N08).
//!
//! The facts answer ONE grid tick per stock: the increment of the band its price fell in when the facts were asked
//! (`crate::any_symbol`, "The grid"). The facts are kept ten minutes by the gatewayhub and an hour by the node, and a
//! market rule is a TABLE of bands, so a price that leaves its band afterwards (a stock crossing 100, a limit set far
//! from the mark) can sit off the grid of the band it is in: IB would refuse it with `[110] The price does not conform
//! to the minimum price variation`, and an engine that rounds to its one tick would never have said why.
//!
//! So the facts source notes every market rule it reads, by symbol, in a process-wide table ([`note`]; the gatewayhub
//! hosts the facts source and the exec of an account in one process, as it does for `crate::delayed_order`'s marks),
//! and the exec asks it before a price goes to the transport ([`refusal`]): a limit or trigger price that is on the grid
//! of NO rule the contract names, at that price, is refused by name with the venue's real increment, never sent and
//! never rounded. ⚠ **Fail-open**: a symbol this process never looked up has no table (the engine's own mount, a facts
//! source that failed), and the price is then the venue's to judge; a price on the grid of ANY one of the contract's
//! rules (one per listed exchange) is let through, because which exchange SMART routes to is not the adapter's to say.
//! A price AMENDMENT (`Modify`) is not judged here, as for `crate::delayed_order`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use vike_model::OrderRequest;

use crate::any_symbol::{PriceBand, increment_at};

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

/// The facts source read `rules` for `symbol` at `facts_price` (`None`: no price was known): keep them for the exec.
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

/// Why a price of `req` is refused for being off the venue's grid, or `None` (the module doc's rule).
pub(crate) fn refusal(req: &OrderRequest) -> Option<String> {
    let priced: Vec<f64> = [req.price, req.trigger_price].into_iter().flatten().collect();
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
