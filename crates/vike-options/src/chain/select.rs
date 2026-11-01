//! The Options tool's SELECTORS over the chain grid (`BTreeMap<String, UnderlyingChains>`, keyed by
//! underlying): which underlying and expiry to render ([`resolve_options_selection`]), the
//! canonical underlying order ([`underlying_rank`]), and the front-expiry ATM focus-set a 100 ms
//! ticker feed subscribes ([`front_expiry_focus_instruments`]).
//!
//! CONTRACT: pure functions of the grid and the caller's selection, no I/O, no clock; a selection
//! that no longer names a fetched underlying or expiry falls back to the grid's own defaults
//! instead of rendering an empty grid.

use vike_marketdata::UnderlyingChains;

/// Canonical rank for the underlying selector order: BTC, ETH, SOL, then anything else.
pub fn underlying_rank(u: &str) -> u8 {
    match u {
        "BTC" => 0,
        "ETH" => 1,
        "SOL" => 2,
        _ => 3,
    }
}

/// Resolve which underlying's bundle the Options tool should render, and reset a now-invalid expiry
/// selection (the BTC→ETH switch guard). Pure so the app's Options arm is unit-testable without
/// egui. Returns the selected `UnderlyingChains` (or `None` when nothing has fetched yet).
///
/// - selected underlying = `underlying_sel` if it's still a fetched underlying, else the first of
///   `order` (canonical: BTC).
/// - if `expiry_sel` names an expiry that isn't in the selected underlying's chains, it's cleared
///   so the underlying's own `default_expiry` takes over (switching BTC→ETH never shows an empty
///   grid).
pub fn resolve_options_selection<'a>(
    by_underlying: &'a std::collections::BTreeMap<String, UnderlyingChains>,
    order: &[String],
    underlying_sel: &Option<String>,
    expiry_sel: &mut Option<String>,
) -> Option<&'a UnderlyingChains> {
    let active_u = underlying_sel
        .as_ref()
        .filter(|u| by_underlying.contains_key(*u))
        .cloned()
        .or_else(|| order.first().cloned())?;
    let bundle = by_underlying.get(&active_u)?;
    if let Some(sel) = expiry_sel.as_ref()
        && !bundle.chains.contains_key(sel)
    {
        *expiry_sel = None;
    }
    Some(bundle)
}

/// Collect the venue instrument ids for the FRONT (nearest) expiry's ATM ±`n` strikes of each named
/// underlying, read from the seeded grid — the fixed focus-set the `ticker.{inst}.100ms` bid/ask feed
/// subscribes. "ATM" is the strike band straddling the chain spot; ±n takes n strikes each side (the
/// same window shape as [`crate::limit_strikes`]). BOTH the call and put id at each in-window
/// strike are collected (the grid renders both sides). An underlying that hasn't fetched, has no
/// front-expiry chain, or has no spot is skipped. Pure (grid in, ids out) so the selection is
/// unit-testable without a socket.
//
// TODO(dynamic-focus): MVP = a FIXED front-expiry ±n window. The next increment follows the user's
// live expiry/strike-window selection (`ToolView::opt_expiry_sel` / `opt_strike_window`) so the
// ticker set tracks whatever the user is actually viewing, plumbed UI→feed (re-subscribe on change).
pub fn front_expiry_focus_instruments(
    by_underlying: &std::collections::BTreeMap<String, UnderlyingChains>,
    underlyings: &[&str],
    n: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    for &u in underlyings {
        let Some(bundle) = by_underlying.get(u) else { continue };
        // Front expiry = the bundle's default (nearest DTE).
        let Some(chain) = bundle.chains.get(&bundle.default_expiry) else { continue };
        let Some(spot) = chain.underlying_price else { continue };
        // ATM split: first strike at/above spot; take n below + n at/above (limit_strikes' window).
        let split = chain.rows.iter().position(|r| r.strike >= spot).unwrap_or(chain.rows.len());
        let lo = split.saturating_sub(n);
        let hi = (split + n).min(chain.rows.len());
        for sr in &chain.rows[lo..hi] {
            for q in [sr.call.as_ref(), sr.put.as_ref()].into_iter().flatten() {
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
