//! The `venue_mounted` journal, the declared extra legs per venue, and the held engine order.

use std::collections::HashSet;

use vike_core::CoreConfig;

#[cfg(doc)]
use super::Node;
use super::WiredMarket;

/// The `block` for a venue the pre-mount probe said would arm and that then did NOT appear in
/// [`Node::live_venues`]: armed, permitted, credentialed, and still paper because its connect failed.
///
/// No [`vike_config::ArmingBlock`] variant carries it: those are all answerable BEFORE the network;
/// this one only AFTER. A string because [`vike_model::change_journal::VenueMountTarget::block`]
/// takes one.
pub const MOUNT_FAILED: &str = "mount failed";

/// Journal what each venue was ASKED to be and what it BECAME: one `venue_mounted` record per
/// interesting venue, once per process start.
///
/// Both sides are needed: [`crate::venue_arming`] is the network-free PREDICTION (what the Data
/// Manager's arming screen renders), [`Node::live_venues`] the OUTCOME. Together they answer *"I set
/// live, why did it trade paper?"*, including [`MOUNT_FAILED`].
///
/// Recorded: venues whose ceiling is above `paper`, plus any that armed. ⚠ A `live` venue that
/// reaches `live` still gets a line; only the all-paper DEFAULT case is exempt, because
/// `vike_boot`'s `boot_settings` anchor already brackets the ceilings.
///
/// `state_dir` `None` (no project) writes NOTHING, as `vike_boot::journal_boot_settings` does and
/// `vike-tradehub`'s `a_journal_less_surface_writes_nothing` pins. `ts_ms` is stamped by the
/// composition root (`vike_model::change_journal` reads no clock). Returns one result per record
/// ATTEMPTED; the caller logs failures.
///
/// ⚠ **It takes the ROWS, not the vars map — a correctness property.** `vike-tradehub`'s
/// `live_mount_with` STRIPS a `data_only` venue's exec credentials before mounting, so it holds two
/// maps and only one is right; `&[VenueArming]` (via [`crate::venue_arming`]) forces the caller to
/// choose. The daemon computes them after the withhold, before the map moves into the node config.
pub fn journal_venue_mounts(
    state_dir: Option<&std::path::Path>,
    arming: &[vike_config::VenueArming],
    live_venues: &HashSet<String>,
    version: &str,
    ts_ms: i64,
) -> Vec<Result<std::path::PathBuf, vike_model::change_journal::ChangeJournalError>> {
    use vike_config::VenueMode;
    use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

    let Some(state_dir) = state_dir else { return Vec::new() };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(version));
    arming
        .iter()
        .filter(|row| row.ceiling != VenueMode::Paper || live_venues.contains(&row.route_key()))
        .map(|row| {
            // ⚠ Test the ROUTE KEY, not the venue: `live_venues` is keyed per ACCOUNT, so `binance`
            // would answer for the DEFAULT account and record `binance#ALT` as failed whenever it armed.
            let armed = live_venues.contains(&row.route_key());
            // `live_venues` carries names, not tiers: an armed venue is recorded at the predicted
            // tier. Probe and mount read one row set (`venue_arming_under`), so only WHETHER differs.
            let effective = if armed { row.effective } else { VenueMode::Paper };
            let block = if effective == row.ceiling {
                None
            } else if !armed && row.effective != VenueMode::Paper {
                Some(MOUNT_FAILED)
            } else {
                Some(row.block.as_str())
            };
            journal.append(
                ts_ms,
                &Change::venue_mounted(
                    Outcome::Applied,
                    Actor::Boot,
                    row.venue,
                    // `None` (DEFAULT account) SKIPS the field: one-account boxes write the same bytes.
                    row.label.text(),
                    row.ceiling.as_str(),
                    effective.as_str(),
                    block,
                ),
            )
        })
        .collect()
}

/// The EXTRA symbols this node's mounts declared for `venue`, beyond the one its [`WiredMarket`]
/// row wires: the `declared_legs` argument of `crate::make_engine_with_legs`, the ONLY production
/// producer of `vike_model::RiskLimits::grid_by_symbol`.
///
/// `build_node` mounts one `(venue, symbol)` per wired market; only a `vike_core::StrategyMount`'s
/// `symbols` (`vike_core::MountLeg`) says a strategy trades a second instrument, and it is already in
/// `NodeConfig::core_config` when the arms run (`crate::build_live_xemm_core` and
/// `crates/vike-mount/src/run/live.rs`'s `build_live_maker_core` assign `core_config.strategy`
/// BEFORE `build_node`).
///
/// EMPTY for every mount today, so inert: `crates/vike-mount/src/run/config.rs`'s `MountSpec` leaves
/// `legs` empty, and the xEMM `MountLeg::at` may only name the taker's own wired symbol (else
/// `XemmConfigError::HedgeSymbolNotAccepted`: `make_engine` sets no `extra_symbols`).
pub(super) fn declared_legs_for(core: &CoreConfig, venue: &str, wired_symbol: &str) -> Vec<String> {
    legs_for_venue(
        core.strategy
            .iter()
            .chain(core.extra_mounts.iter())
            .map(|m| (m.venue.as_str(), m.symbols.as_slice())),
        venue,
        wired_symbol,
    )
}

/// [`declared_legs_for`]'s pure core over `(mount venue, legs)` pairs, testable without building a
/// `StrategyMount` (which owns a `Box<dyn Strategy>`).
///
/// * A leg's venue is `vike_core::MountLeg::venue` if set, else the MOUNT's — as `vike_core`'s
///   `resolve_intent_venue` routes it, so the engine that RECEIVES the order gets its grid.
/// * The venue's own wired symbol is excluded (its grid is the engine's scalars).
/// * Blanks drop and repeats collapse: one lookup (possibly a network call) per symbol.
/// * DECLARATION order is kept: `grid_by_symbol` is an `IndexMap`, so serialized limits are stable.
pub(super) fn legs_for_venue<'a>(
    mounts: impl Iterator<Item = (&'a str, &'a [vike_core::MountLeg])>,
    venue: &str,
    wired_symbol: &str,
) -> Vec<String> {
    let mut legs: Vec<String> = Vec::new();
    for (mount_venue, declared) in mounts {
        for leg in declared {
            let leg_venue = leg.venue.as_deref().unwrap_or(mount_venue);
            // ⚠ The RAW symbol travels; `trim()` only DECIDES. `vike-core`'s `resolve_intent_symbol`
            // matches `l.symbol == r` and stamps the raw string on the `OrderRequest`; a trimmed key
            // would file a row `RiskLimits::grid_for` never hits — silently back to wrong-instrument
            // rounding while the row claims the grid WAS resolved.
            let symbol = leg.symbol.as_str();
            if leg_venue != venue || symbol.trim().is_empty() || symbol == wired_symbol {
                continue;
            }
            if !legs.iter().any(|s| s == symbol) {
                legs.push(symbol.to_string());
            }
        }
    }
    legs
}

/// The default engines in core order: ascending [`WiredMarket::engine_rank`], ties in mount order
/// (`sort_by_key` is stable). Pure, so testable without mounting.
pub(super) fn in_engine_order<T>(mut mounted: Vec<(WiredMarket, T)>) -> Vec<(WiredMarket, T)> {
    mounted.sort_by_key(|(m, _)| m.engine_rank);
    mounted
}
