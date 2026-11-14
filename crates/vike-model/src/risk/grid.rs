//! The `Grid` family's words — where an instrument's price/size grid comes from:
//!
//! - [`GridSource`]: where a venue arm would get the grid of a symbol OTHER than the one it
//!   started on, as the venue's bridge DECLARES it. One enum for the bridge declaration
//!   (`vike_bridge_core::venue_adapter::VenueAdapterDeclaration::grid_source`), the node's fold
//!   (`vike_tradehub_core::symbol_grid`) and the gatewayhub's arming table; its serde tags
//!   (`"InHand"`, `"PerSymbolFetch"`, `"NoGrid"`) are the lease wire's, frozen in
//!   `fixtures/gatewayhub_wire/v1_responses.jsonl`.
//! - [`GridFetch`]: whether THIS mount's base grid was fetched from the venue, the flag
//!   [`crate::ProfileRisk::apply_to`] gates the profile's instrument fields on.

use serde::{Deserialize, Serialize};

/// **Where a venue arm would have to get the instrument grid for a symbol OTHER than the started
/// one** — the declaration behind which arms of `vike_tradehub_core::make_engine_with_legs` populate
/// [`crate::RiskLimits::grid_by_symbol`] (`crates/vike-tradehub-core/src/symbol_grid.rs`'s module
/// doc, its ⚠ section).
///
/// Declaring TODAY'S REALITY, per the capability-map playbook: the rows are read off the arms, no
/// arm's behaviour is inferred from a row, and
/// `crates/vike-gatewayhub/tests/trade_roster/mount_roster/symbol_grid.rs`'s
/// `every_roster_venue_declares_a_grid_source` iterates [`crate::VENUES`], so a new bridge crate
/// cannot join the roster without classifying itself.
///
/// ⚠ The variant NAMES are wire tags (the lease's arming table): a rename is a protocol break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridSource {
    /// The arm ALREADY holds a resolved instrument table covering every symbol of the venue, so a
    /// declared leg's grid costs no network at all. These are the arms wired today.
    InHand,
    /// The arm's only per-symbol source is a blocking, symbol-SCOPED network round trip — the very
    /// call it already makes once for the engine symbol. A declared leg would cost one more, at
    /// start time, before any feed is up. NOT wired: `crates/vike-tradehub-core/src/symbol_grid.rs`'s
    /// module doc, its ⚠ section.
    PerSymbolFetch,
    /// The arm fetches no instrument grid at all (or the venue publishes none of that shape), so
    /// there is nothing to read per symbol and nothing to wire. Its engine symbol has no grid
    /// either, which is why a leg here inherits nothing harmful.
    NoGrid,
}

/// Where the venue-owned portion of the `base` [`crate::RiskLimits`] passed to
/// [`crate::ProfileRisk::apply_to`] came from — the EXPLICIT flag the `[risk]` converter's exception
/// case is gated on (`crates/vike-model/src/risk/profile.rs`'s module doc). Never infer this from
/// "the field happened to be `None`": a fetch that legitimately returned zeroed/absent tick fields
/// (folded to `None` by `nz_step`) must still be treated as [`GridFetch::VenueFetched`], not
/// silently reopened for the profile to fill in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridFetch {
    /// `base` was built by `RiskLimits::from_properties` from a REAL venue-fetched
    /// instrument grid. The venue owns `tick_size`/`lot_size`/`min_qty`/`min_notional`; a profile
    /// that also sets one of them is a config error (see [`crate::ProfileRisk::apply_to`]).
    VenueFetched,
    /// No grid was fetched for this mount (`make_engine`'s permissive-default fallback after a
    /// failed fetch, or no fetch attempted at all — every backtest/paper mount). The ONLY case
    /// where the profile may supply the instrument-grid fields, as their sole source of truth.
    NoGridFetched,
}

impl GridFetch {
    /// **The grid fetch of a mount, from the TIER of the account it mounts** — the SOLE place
    /// that mapping is made (decision 0156 item 7: the run profile has no `mode`; where a forward
    /// run's orders go is the tier of the account each mount runs on).
    ///
    /// A `paper` account is the paper simulator: it is assembled without fetching an instrument
    /// grid, so [`GridFetch::NoGridFetched`] is the only sound value and the profile may supply the
    /// grid. A `demo` or `live` account mounts a real venue and fetches its grid, so
    /// [`GridFetch::VenueFetched`] is the only sound value and a profile that sets a venue-owned
    /// field is refused by [`crate::ProfileRisk::apply_to`].
    #[must_use]
    pub fn for_tier(tier: crate::AccountTier) -> Self {
        match tier {
            crate::AccountTier::Paper => GridFetch::NoGridFetched,
            crate::AccountTier::Demo | crate::AccountTier::Live => GridFetch::VenueFetched,
        }
    }
}
