//! The `venue_mounted` journal, the declared extra legs per venue, and the held engine order.

use std::collections::HashSet;

use vike_core::CoreConfig;

#[cfg(doc)]
use super::RuntimeAssembly;
use super::RuntimeMarket;

/// Journal what each venue was ASKED to be and what it BECAME: one `venue_mounted` record per
/// interesting venue, once per process start.
///
/// Both sides are needed: [`crate::venue_arming`] is the network-free PREDICTION (what the Data
/// Manager's arming screen renders), [`RuntimeAssembly::live_venues`] the OUTCOME. Together they answer *"I set
/// live, why does it not run?"*, including `vike_model::ArmingBlock::AdapterFailed` — the one block
/// known only AFTER the network: armed, permitted, credentialed, and not running because its
/// connect failed.
///
/// Recorded: accounts whose tier is `demo` or `live`, plus any that armed. ⚠ A `live` account that
/// reaches `live` still gets a line; an account at `paper`, or with no row stating a tier, is
/// exempt (nothing was asked of a venue). The record's `requested` field carries the account's
/// TIER, and its `effective` is absent for an account that does not run (decision 0156).
///
/// `state_dir` `None` (no project) writes NOTHING, as `vike_boot::journal_boot_settings` does and
/// `vike-tradehub`'s `a_journal_less_surface_writes_nothing` pins. `timestamp_ms` is stamped by the
/// composition root (`vike_change_journal` reads no clock). Returns one result per record
/// ATTEMPTED; the caller logs failures.
///
/// ⚠ **It takes the ROWS, not the vars map — a correctness property.** `vike-tradehub`'s
/// `live_runtime_with` STRIPS a `data_only` venue's exec credentials before mounting, so it holds two
/// maps and only one is right; `&[ArmingAccount]` (via [`crate::venue_arming`]) forces the caller to
/// choose. The daemon computes them after the withhold, before the map moves into the node config.
pub fn journal_venue_mounts(
    state_dir: Option<&std::path::Path>,
    arming: &[vike_config::ArmingAccount],
    live_venues: &HashSet<String>,
    version: &str,
    ts_ms: i64,
) -> Vec<Result<std::path::PathBuf, vike_change_journal::ChangeJournalError>> {
    use vike_change_journal::{Change, ChangeActor, ChangeJournal, ChangeOutcome, ChangeProcess};
    use vike_model::AccountTier;

    let Some(state_dir) = state_dir else { return Vec::new() };
    let journal = ChangeJournal::in_state_dir(state_dir, ChangeProcess::current(version));
    arming
        .iter()
        .filter_map(|row| {
            // ⚠ Test the ROUTE KEY, not the venue: `live_venues` is keyed per ACCOUNT, so `binance`
            // would answer for the DEFAULT account and record `binance#ALT` as failed whenever it armed.
            let armed = live_venues.contains(&row.route_key());
            // The tier asked of a VENUE: `demo`/`live` (or one that armed anyway, recorded at the
            // tier it reached). `paper` and no row asked nothing of a venue.
            let requested = row
                .tier
                .filter(|t| *t != AccountTier::Paper)
                .or(row.effective.filter(|_| armed))?;
            Some((row, armed, requested))
        })
        .map(|(row, armed, requested)| {
            // `live_venues` carries names, not tiers: an armed venue is recorded at the predicted
            // tier. Probe and mount read one row set (`venue_arming_under`), so only WHETHER differs.
            // An account that did not arm does NOT RUN: no effective tier (decision 0156).
            let effective = if armed { row.effective } else { None };
            let block = if effective == Some(requested) {
                None
            } else if !armed && row.effective.is_some_and(|t| t != AccountTier::Paper) {
                Some(vike_model::ArmingBlock::AdapterFailed.as_str())
            } else {
                Some(row.block.as_str())
            };
            journal.append(
                ts_ms,
                &Change::venue_mounted(
                    ChangeOutcome::Applied,
                    ChangeActor::Boot,
                    row.venue,
                    // `None` (DEFAULT account) SKIPS the field: one-account boxes write the same bytes.
                    row.label.text(),
                    requested,
                    effective,
                    block,
                ),
            )
        })
        .collect()
}

/// The EXTRA symbols this node's mounts declared for `venue`, beyond the one its DEFAULT account's engine
/// is mounted on (`default_symbol`, the symbol that account's row states; `None` when it states none):
/// the `declared_legs` argument of `crate::make_engine_with_legs`, the ONLY production producer of
/// `vike_model::RiskLimits::grid_by_symbol`.
///
/// `assemble_runtime` mounts each account of a wired market on its own symbol; only a `vike_core::Mount`'s
/// `symbols` (`vike_core::MountLeg`) says a strategy trades a second instrument, and it is already in
/// `RuntimeConfig::core_config` when the arms run (`crate::build_live_xemm_core` and
/// `crates/vike-tradehub-core/src/run/live.rs`'s `fold_strategy_mount` assign `core_config.strategy`
/// BEFORE `assemble_runtime`).
///
/// EMPTY for every mount whose strategy declares no `vike_model::Strategy::symbols` (the one source of
/// `crates/vike-tradehub-core/src/run/config.rs`'s `MountPlacement` `legs`, `MountSpec::declare_strategy_legs`), so
/// inert there; a declared leg is admitted into its venue's engine with its own grid. The xEMM `MountLeg::at`
/// may only name the taker's default account's symbol (else `XemmConfigError::HedgeSymbolNotAccepted`).
pub(super) fn declared_legs_for(
    core: &CoreConfig,
    venue: &str,
    default_symbol: Option<&str>,
) -> Vec<String> {
    legs_for_venue(
        core.strategy
            .iter()
            .chain(core.extra_mounts.iter())
            .map(|m| (m.venue.as_str(), m.symbols.as_slice())),
        venue,
        default_symbol,
    )
}

/// [`declared_legs_for`]'s pure core over `(mount venue, legs)` pairs, testable without building a
/// `Mount` (which owns a `Box<dyn Strategy>`).
///
/// * A leg's venue is `vike_core::MountLeg::venue` if set, else the MOUNT's — as `vike_core`'s
///   `resolve_intent_venue` routes it, so the engine that RECEIVES the order gets its grid.
/// * The default account's own symbol is excluded (its grid is the engine's scalars).
/// * Blanks drop and repeats collapse: one lookup (possibly a network call) per symbol.
/// * DECLARATION order is kept: `grid_by_symbol` is an `IndexMap`, so serialized limits are stable.
pub(super) fn legs_for_venue<'a>(
    mounts: impl Iterator<Item = (&'a str, &'a [vike_core::MountLeg])>,
    venue: &str,
    default_symbol: Option<&str>,
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
            if leg_venue != venue || symbol.trim().is_empty() || Some(symbol) == default_symbol {
                continue;
            }
            if !legs.iter().any(|s| s == symbol) {
                legs.push(symbol.to_string());
            }
        }
    }
    legs
}

/// The default engines in core order: ascending [`RuntimeMarket::engine_rank`], ties in mount order
/// (`sort_by_key` is stable). Pure, so testable without mounting.
pub(super) fn in_engine_order<T>(mut mounted: Vec<(RuntimeMarket, T)>) -> Vec<(RuntimeMarket, T)> {
    mounted.sort_by_key(|(m, _)| m.engine_rank);
    mounted
}
