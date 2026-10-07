//! Per-symbol PRICE/SIZE GRID wiring — the mount-side producer for
//! `vike_exec::RiskLimits::grid_by_symbol`.
//!
//! **The defect.** A live mount ([`crate::make_engine_with_legs`]) builds its `RiskGate` with
//! `vike_exec::RiskLimits::from_properties(&f)` from ONE symbol's `SymbolProperties`, and
//! `vike_exec::RiskGate::check` rounds EVERY order onto those scalars. With a second symbol, a
//! coarse mount lot destroys a valid finer-grid order (`vike_exec::round_to(0.5, Some(1.0))` is
//! `0.0`) and the gate denies it `"non-positive-size"`, naming nothing of the cause. The gate
//! consults the override via `vike_exec::RiskLimits::grid_for` (field-by-field scalar fallback).
//!
//! **Reference semantics: the backtest twin.** `crates/vike-sim/src/engine/sim_broker.rs`'s
//! `grid_for` resolves a grid PER SYMBOL through the same `vike_exec::RiskLimits::from_properties`;
//! the live side reuses that mapping via `vike_exec::SymbolGrid::from_properties` (pinned equal by
//! `a_symbol_grid_matches_the_scalar_builder_field_for_field`). The ONE added semantic: a `None`
//! `SymbolGrid` field inherits the mount scalar (the residual is
//! `vike_exec::SymbolGrid::from_properties`' ⚠).
//!
//! ## ⚠ Which mounts are wired, and why the rest are NOT
//!
//! [`declared_grid_source`] reads each bridge's STEP-1 declaration
//! (`VenueDeclaration::grid_source`, CLAUDE.md's per-venue capability-map playbook). Only
//! [`DeclaredGridSource::InHand`] venues are wired, by a hard rule: **a mount must not gain a
//! blocking network round trip it did not already have.** The crypto-CEX / deribit / aster /
//! alpaca / ibkr pre-fetches are symbol-SCOPED (`?symbol=`, a throwaway Alpaca OAuth2 lifecycle, a
//! fresh IBKR TWS socket per call), so each extra leg costs a serial round trip before the first
//! feed is up; a degraded mount that SAYS so (`warn_ungridded_legs`) beats one that hangs. Wiring
//! one is a change to its bridge's mount (`LiveExec::leg_grids`, the lookup CLOSURE the fold hands
//! `declared_symbol_grids`), in a PR that weighs the startup cost per venue. Wired today:
//! hyperliquid (`crates/bridges/hyperliquid/src/mount.rs`'s `live_mount_for_account`) and cTrader
//! (`crates/bridges/ctrader/src/symbols.rs`'s `risk_properties`, "Needs NO network"). The
//! partition over the real registry is `crates/vike-tradehub/tests/mount_roster/symbol_grid.rs`'s
//! `every_roster_venue_declares_a_grid_source`; do not list it a second time here.

use indexmap::IndexMap;

use vike_bridge_core::venue_mount::DeclaredGridSource;
use vike_exec::SymbolGrid;

/// The grid-source declaration for `venue`, from its registry row.
#[must_use]
pub fn declared_grid_source(
    registry: &'static [crate::VenueRow],
    venue: &str,
) -> DeclaredGridSource {
    match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => row.declaration().grid_source,
        Some(crate::VenueRow::FeatureAbsent { .. }) | None => DeclaredGridSource::NoGrid,
    }
}

/// The per-symbol grid map for the EXTRA symbols a mount declared beyond its own.
///
/// `properties_for` is the mount's OWN lookup, as a closure: this crate must not learn a second way
/// to ask a venue for a grid, and the laziness is observable offline
/// (`an_empty_declaration_never_touches_the_venue`). Rules, each load-bearing:
/// * **An empty `declared` never calls `properties_for` and returns an empty map**: byte-identical
///   for every existing mount (`grid_by_symbol` empty, `grid_for` returns the scalars, and the
///   field's `skip_serializing_if` keeps it out of `vike_exec::state_hash`).
/// * **A leg naming the MOUNTED symbol gets no row**: the scalars ARE its grid; a row would be a
///   copy that drifts, and a caller listing the primary equals one that does not.
/// * **A failed lookup gets no row, never a zeroed one**: `SymbolProperties::default()` folds to an
///   all-`None` row inheriting every scalar — the wrong answer dressed as resolved. No row leaves
///   the same answer, but `warn_ungridded_legs` then SAYS so.
pub(crate) fn declared_symbol_grids<F>(
    primary: &str,
    declared: &[String],
    properties_for: F,
) -> IndexMap<String, SymbolGrid>
where
    F: Fn(&str) -> Option<vike_model::SymbolProperties>,
{
    let mut grids = IndexMap::new();
    for leg in declared {
        // ⚠ Key by the RAW leg, never a trimmed copy: `vike_core`'s `resolve_intent_symbol` stamps
        // the raw string onto the `OrderRequest` and `RiskLimits::grid_for` looks it up by that, so
        // a trimmed key is a row the gate never hits. `trim` only rejects an all-whitespace leg.
        if leg.trim().is_empty() || leg == primary || grids.contains_key(leg.as_str()) {
            continue;
        }
        if let Some(properties) = properties_for(leg) {
            grids.insert(leg.to_string(), SymbolGrid::from_properties(&properties));
        }
    }
    grids
}

/// Did a venue grid pre-fetch land in `limits`? With every scalar `None` (paper, a failed
/// pre-fetch, an arm that fetches none) an ungridded leg inherits nothing, so
/// `warn_ungridded_legs` stays silent.
fn carries_a_venue_grid(limits: &vike_exec::RiskLimits) -> bool {
    limits.tick_size.is_some()
        || limits.lot_size.is_some()
        || limits.min_qty.is_some()
        || limits.min_notional.is_some()
}

/// The declared legs that got no grid row and are not the mounted symbol, in declaration order. The
/// lookup is by the RAW leg, matching `declared_symbol_grids`' key (a trimmed lookup would misreport
/// gridded legs), but an all-whitespace leg is skipped by the same `trim` test that function uses: it
/// was never going to get a row, so reporting it would be noise.
fn ungridded_legs<'a>(
    primary: &str,
    declared: &'a [String],
    grids: &IndexMap<String, SymbolGrid>,
) -> Vec<&'a str> {
    declared
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty() && *s != primary && !grids.contains_key(*s))
        .collect()
}

/// Say, ONCE per mount, that a declared leg is judged on the MOUNTED symbol's grid — the "degraded
/// mount that says so" half of this module's ⚠ rule. The leg still trades, on another instrument's
/// tick/lot/floors. Emitted only when `carries_a_venue_grid`.
pub(crate) fn warn_ungridded_legs(
    venue: &str,
    grid_source: DeclaredGridSource,
    primary: &str,
    declared: &[String],
    grids: &IndexMap<String, SymbolGrid>,
    limits: &vike_exec::RiskLimits,
) {
    if declared.is_empty() || !carries_a_venue_grid(limits) {
        return;
    }
    let ungridded = ungridded_legs(primary, declared, grids);
    if ungridded.is_empty() {
        return;
    }
    let why = match grid_source {
        DeclaredGridSource::InHand => {
            "this venue's resolved instrument table does not know the symbol (misspelled, or not \
             listed)"
        }
        DeclaredGridSource::PerSymbolFetch => {
            "this venue's only per-symbol grid source is a blocking, symbol-scoped network \
             round-trip, which a mount deliberately does not add per declared leg (see \
             vike_mount::symbol_grid's module doc)"
        }
        DeclaredGridSource::NoGrid => "this venue arm fetches no instrument grid at all",
    };
    tracing::warn!(
        venue,
        mounted = primary,
        legs = ?ungridded,
        "declared leg(s) have NO per-symbol risk grid → they are rounded onto `{primary}`'s tick \
         and lot and judged against its floors, which is the wrong instrument's grid: {why}"
    );
}

#[path = "symbol_grid_tests.rs"]
#[cfg(test)]
mod symbol_grid_tests;

#[path = "symbol_identity_tests.rs"]
#[cfg(test)]
mod symbol_identity_tests;
