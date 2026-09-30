//! Per-symbol PRICE/SIZE GRID wiring — the mount-side producer for
//! `vike_exec::RiskLimits::grid_by_symbol`.
//!
//! ## The defect this closes
//!
//! Every live arm of [`crate::make_engine_with_legs`] builds its `RiskGate` with
//! `vike_exec::RiskLimits::from_properties(&f)` from **ONE** symbol's `SymbolProperties` — the
//! mounted `(venue, symbol)` pair. `vike_exec::RiskGate::check` then rounds EVERY order onto those
//! scalars, whatever instrument it names. That is exactly right for a single-symbol engine and
//! wrong the moment one admits a second symbol: a coarse mount lot destroys a valid finer-grid
//! order outright (`vike_exec::round_to(0.5, Some(1.0))` is `0.0`), and the gate then denies it
//! `"non-positive-size"` — a reason that names nothing about the real cause.
//!
//! `vike_exec::RiskLimits::grid_by_symbol` is the per-symbol override map the gate has consulted
//! since it was added (`vike_exec::RiskLimits::grid_for`, called once per `check`), with
//! field-by-field fallback to the scalars. Until this module it had **no producer at all** outside
//! unit tests: every mount left it empty, so the fix existed and was unreachable.
//!
//! ## The reference semantics — the BACKTEST twin
//!
//! `crates/vike-sim/src/engine/sim_broker.rs`'s `grid_for` already resolves a grid
//! PER SYMBOL (and point-in-time) and feeds each one through the same
//! `vike_exec::RiskLimits::from_properties`. The live side deliberately reuses that mapping —
//! `vike_exec::SymbolGrid::from_properties` is its per-leg twin, pinned equal to the scalar builder
//! by `a_symbol_grid_matches_the_scalar_builder_field_for_field` — rather than inventing a second
//! reading of the same venue payload. The ONE semantic the live side adds is the fallback: a
//! `SymbolGrid` field left `None` inherits the mount scalar, where the backtest's per-symbol
//! `RiskLimits` are independent. See `vike_exec::SymbolGrid::from_properties`' ⚠ for the residual
//! that creates.
//!
//! ## ⚠ Which arms are wired, and why the rest are NOT
//!
//! [`declared_grid_source`] is the STEP-1 capability declaration (CLAUDE.md's per-venue
//! capability-map playbook): one row per `vike_model::VENUES` roster venue, naming where THAT
//! arm's grid for a NON-mounted symbol would have to come from, cited to the arm that reads it.
//! Only [`DeclaredGridSource::InHand`] venues are wired here, and the reason is a hard rule rather
//! than a scheduling accident:
//!
//! **A mount must not gain a blocking network round trip it did not already have.** The crypto-CEX
//! / deribit / aster / alpaca / ibkr pre-fetches are all symbol-SCOPED requests (`?symbol=` on
//! `exchangeInfo`/`instruments-info`, a throwaway OAuth2 lifecycle per Alpaca call, a fresh
//! transient TWS socket per IBKR call), so a second symbol costs a second round trip — per leg,
//! serially, before the first feed is up. A degraded arm that SAYS so (`warn_ungridded_legs`,
//! one line at the mount naming the venue and the legs) beats a mount that hangs. Wiring one of
//! those arms is a one-line change here — `declared_symbol_grids` takes the arm's own fetch as a
//! CLOSURE — but it belongs to a PR that can weigh the startup cost per venue, not to this one.
//!
//! The arms that need no round trip are wired today: hyperliquid
//! (`crates/bridges/hyperliquid/src/mount.rs`'s `live_mount_for_account`) and cTrader (its
//! `SymbolsList` is resolved during the exec handshake — see
//! `crates/bridges/ctrader/src/symbols.rs`'s `risk_properties`, whose own doc says "Needs NO
//! network"). `declared_grid_source` is the authority for which, and
//! `every_roster_venue_declares_a_grid_source` is the partition — writing the list here a second
//! time is how it would rot.

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
        Some(crate::VenueRow::FeatureAbsent { .. }) => DeclaredGridSource::NoGrid,
        Some(crate::VenueRow::Legacy(_)) | None => legacy_grid_source(venue),
    }
}

/// ⚠ TRANSITIONAL: the legacy arms' grid-source rows — one per venue whose legacy arm still exists,
/// each citing the arm it was read from. A venue's row leaves as its port moves the declaration
/// into its bridge (`vike_bridge_core::venue_mount::VenueDeclaration::grid_source`).
pub(crate) fn legacy_grid_source(venue: &str) -> DeclaredGridSource {
    match venue {
        // `crates/bridges/hyperliquid/src/mount.rs`'s `live_mount_for_account` loads
        // `HyperliquidInstruments` for the WHOLE venue before it resolves the mounted symbol's
        // properties, so every other asset is already in the same response.
        "hyperliquid" => DeclaredGridSource::InHand,
        // The ctrader arm reads `handle.symbols.risk_properties(symbol)` off the exec handshake's
        // own `SymbolsList` — `crates/bridges/ctrader/src/symbols.rs`'s `risk_properties`.
        "ctrader" => DeclaredGridSource::InHand,
        // Symbol-scoped blocking pre-fetches, one round trip each:
        // `crates/bridges/binance/src/exec.rs`'s `fetch_binance_properties` (`?symbol=` on
        // exchangeInfo), `crates/bridges/bybit/src/exec.rs`'s `fetch_bybit_properties`,
        // `crates/bridges/okx/src/exec.rs`'s `fetch_okx_instrument`,
        // `crates/bridges/deribit/src/exec.rs`'s `fetch_deribit_properties`,
        // `crates/bridges/aster/src/exec.rs`'s `fetch_aster_properties`.
        "binance" | "bybit" | "okx" | "deribit" | "aster" => DeclaredGridSource::PerSymbolFetch,
        // `crates/bridges/alpaca/src/instruments.rs`'s `fetch_alpaca_properties` builds a THROWAWAY
        // OAuth2 client-credentials lifecycle per call — the most expensive per-symbol source on
        // the roster.
        "alpaca" => DeclaredGridSource::PerSymbolFetch,
        // `crates/bridges/vike-ibkr/src/properties.rs`'s `fetch_ibkr_properties` opens its OWN
        // transient socket connection to TWS/Gateway per call (IBKR publishes no keyless grid
        // endpoint), and is socket-backend-only.
        "ibkr" => DeclaredGridSource::PerSymbolFetch,
        // ig / oanda: exec-only arms with NO grid pre-fetch at all today — their mounted symbol
        // keeps `RiskLimits::new()`'s permissive `None`s, so a leg inherits nothing.
        // polymarket: shares are whole and price is a probability; the venue publishes no
        // instrument grid of this shape (the arm's own comment says so).
        // fxcm / dukascopy: no `make_engine` arm exists at all (see the crate module doc) — they
        // reach the paper `_` arm, whose limits are likewise all-`None`.
        _ => DeclaredGridSource::NoGrid,
    }
}

/// Build the per-symbol grid map for the EXTRA symbols a mount declared beyond its own, from that
/// arm's per-symbol properties source.
///
/// `properties_for` is the arm's OWN lookup, passed as a closure rather than resolved here: this
/// crate must not learn a second way to ask a venue for an instrument grid, and taking the closure
/// is also what makes the laziness below directly observable offline (call it with a counting
/// closure — `an_empty_declaration_never_touches_the_venue`), with no network and no credentials.
///
/// Rules, each load-bearing:
/// * **An empty `declared` never calls `properties_for` and returns an empty map.** That is the
///   BYTE-IDENTICAL property for every mount that exists today: `grid_by_symbol` stays empty,
///   `vike_exec::RiskLimits::grid_for` returns the scalars verbatim for every symbol, and the
///   serialized `RiskLimits` is unchanged (the field carries `skip_serializing_if`, so an empty map
///   does not even reach `vike_exec::engine_snapshot::state_hash`).
/// * **A leg naming the MOUNTED symbol gets no row.** The scalars already ARE that symbol's grid,
///   so a row could only ever be a redundant copy that drifts. Skipping it is also what makes a
///   caller that includes the primary in its list identical to one that does not.
/// * **A failed lookup gets no row, never a zeroed one.** `SymbolProperties::default()` is
///   all-`0.0`, which `vike_exec::SymbolGrid::from_properties` folds to all-`None` — a row that
///   inherits every scalar, i.e. exactly today's wrong answer wearing a declaration that it was
///   resolved. No row leaves the same wrong answer, but `warn_ungridded_legs` then SAYS so.
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
        // ⚠ The key is the RAW leg, not a trimmed copy. `vike_core`'s `resolve_intent_symbol`
        // matches `l.symbol == r` and stamps the raw declared string onto the `OrderRequest`, and
        // `RiskLimits::grid_for` then looks the map up by THAT string. A trimmed key would file a
        // row the gate can never hit for a leg declared with stray whitespace — the wrong-instrument
        // rounding this map exists to prevent, dressed as a resolved grid.
        // `trim` still DECIDES: an all-whitespace leg is a declaration error, not an instrument.
        if leg.trim().is_empty() || leg == primary || grids.contains_key(leg.as_str()) {
            continue;
        }
        if let Some(properties) = properties_for(leg) {
            grids.insert(leg.to_string(), SymbolGrid::from_properties(&properties));
        }
    }
    grids
}

/// Does this mount's `limits` carry a REAL instrument grid — i.e. did a venue pre-fetch land?
///
/// The distinction matters only for `warn_ungridded_legs`: with every scalar `None` (a paper
/// mount, a failed pre-fetch, or an arm that fetches no grid at all) an ungridded leg inherits
/// NOTHING, so there is no defect to report and a warning would be pure noise on the most common
/// mount in the tree.
fn carries_a_venue_grid(limits: &vike_exec::RiskLimits) -> bool {
    limits.tick_size.is_some()
        || limits.lot_size.is_some()
        || limits.min_qty.is_some()
        || limits.min_notional.is_some()
}

/// Say, ONCE per mount, that a declared leg is being judged on the MOUNTED symbol's grid.
///
/// This is the "a degraded arm that says so" half of the rule in this module's ⚠ section: the leg
/// still trades (it is refused nothing it was not refused before), but its tick/lot/floors are
/// another instrument's, and an operator has no other way to learn that. Emitted only when the
/// mount actually fetched a grid (`carries_a_venue_grid`) — with all-`None` scalars the leg
/// inherits nothing and there is nothing to warn about.
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
    // Raw here too, matching `declared_symbol_grids`' key — a trimmed lookup would report a leg as
    // ungridded that IS gridded, or the reverse, which is the same identity split one level up.
    let ungridded: Vec<&str> = declared
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty() && *s != primary && !grids.contains_key(*s))
        .collect();
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
