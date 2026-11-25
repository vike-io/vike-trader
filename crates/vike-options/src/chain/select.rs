//! The Options tool's SELECTORS over the chain grid (`BTreeMap<String, OptionChainSet>`, keyed by
//! underlying): which underlying and expiry to render ([`resolve_options_selection`]), the
//! underlying order the venue's listing gives ([`underlyings_by_listed_options`]), and the
//! front-expiry ATM focus-set a 100 ms ticker feed subscribes ([`front_expiry_focus_instruments`]).
//!
//! CONTRACT: pure functions of the grid and the caller's selection, no I/O, no clock; a selection
//! that no longer names a fetched underlying or expiry falls back to the grid's own defaults
//! instead of rendering an empty grid.

use std::collections::BTreeMap;

use vike_marketdata::OptionChainSet;

/// **The underlying selector's order, read from the venue's listing and never from a name** (owner,
/// 2026-11-13: no hardcoded symbols): every underlying `listed` names, once, the one the venue lists
/// the most options on first, a tie in name order. `listed` holds one underlying per listed option.
///
/// The venue's own server-side listing and a client's catalog both order by it, so the two agree.
/// MEASURED on Deribit 2026-11-13 it gives BTC (1,602 options), ETH (1,312), SOL (640), then HYPE,
/// XRP, AVAX and TRX: the first three the selector always showed first.
pub fn underlyings_by_listed_options<'a>(listed: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for underlying in listed {
        *counts.entry(underlying).or_default() += 1;
    }
    let mut order: Vec<(&str, usize)> = counts.into_iter().collect();
    // Stable, so a tie keeps the map's name order.
    order.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    order.into_iter().map(|(underlying, _)| underlying.to_string()).collect()
}

/// Resolve which underlying's bundle the Options tool should render, and reset a now-invalid expiry
/// selection (the BTC→ETH switch guard). Pure so the app's Options arm is unit-testable without
/// egui. Returns the selected `OptionChainSet` (or `None` when nothing has fetched yet).
///
/// - selected underlying = `underlying_selection` if it's still a fetched underlying, else the first of
///   `order` (the listing's first, [`underlyings_by_listed_options`]).
/// - if `expiry_selection` names an expiry that isn't in the selected underlying's chains, it's cleared
///   so the underlying's own `default_expiry` takes over (switching BTC→ETH never shows an empty
///   grid).
pub fn resolve_options_selection<'a>(
    by_underlying: &'a std::collections::BTreeMap<String, OptionChainSet>,
    order: &[String],
    underlying_selection: &Option<String>,
    expiry_selection: &mut Option<String>,
) -> Option<&'a OptionChainSet> {
    let active_u = underlying_selection
        .as_ref()
        .filter(|u| by_underlying.contains_key(*u))
        .cloned()
        .or_else(|| order.first().cloned())?;
    let bundle = by_underlying.get(&active_u)?;
    if let Some(selection) = expiry_selection.as_ref()
        && !bundle.chains.contains_key(selection)
    {
        *expiry_selection = None;
    }
    Some(bundle)
}

/// Collect the venue instrument ids for the FRONT (nearest) expiry's ATM ±`n` strikes of each named
/// underlying, read from the seeded grid — the fixed focus-set the `ticker.{inst}.100ms` bid/ask feed
/// subscribes. "ATM" is the strike band straddling the chain spot; ±n takes n strikes each side (the
/// one window [`crate::atm_strike_window`], which the grid shows too). BOTH the call and put id at
/// each in-window strike are collected (the grid renders both sides). An underlying that hasn't
/// fetched, has no front-expiry chain, or has no spot is skipped. Pure (grid in, ids out) so the
/// selection is unit-testable without a socket.
//
// TODO(dynamic-focus): MVP = a FIXED front-expiry ±n window. The next increment follows the user's
// live expiry/strike-window selection (`WindowView::options_expiry_selection` / `options_strike_window`) so the
// ticker set tracks whatever the user is actually viewing, plumbed UI→feed (re-subscribe on change).
pub fn front_expiry_focus_instruments(
    by_underlying: &std::collections::BTreeMap<String, OptionChainSet>,
    underlyings: &[&str],
    n: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    for &u in underlyings {
        let Some(bundle) = by_underlying.get(u) else { continue };
        // Front expiry = the bundle's default (nearest DTE).
        let Some(chain) = bundle.chains.get(&bundle.default_expiry) else { continue };
        let Some(spot) = chain.underlying_price else { continue };
        for sharpe in &chain.rows[crate::atm_strike_window(&chain.rows, spot, n)] {
            for q in [sharpe.call.as_ref(), sharpe.put.as_ref()].into_iter().flatten() {
                if let Some(name) = &q.instrument_name {
                    out.push(name.clone());
                }
            }
        }
    }
    out
}

/// The front-expiry ATM focus-set selection ([`front_expiry_focus_instruments`]) — the pure grid→ids
/// half of the ticker wiring. Pins the ±n ATM window (both call+put ids), the front-only scope, and
/// the skip-what-has-no-spot discipline.
#[cfg(test)]
mod focus_set_tests;

#[cfg(test)]
mod selection_tests;
