//! [`VenueMode`] — **the tier vocabulary of an account; `paper < demo < live`.**
//!
//! An account trades at its own tier: the `tier` of its ACTIVE row in the settings database's
//! `account` table (decision 0119), read by the mount (`crates/vike-mount/src/arming.rs`'s
//! `account_tier`). This module owns only the WORDS — the three tiers, their order and their
//! spelling — and the roster lookup venue-keyed readers share. It holds no per-venue map and
//! decides nothing: no row, an inactive row or a `paper` row all mean `paper`.
//!
//! # ⚠ The ORDER is part of the type
//!
//! [`VenueMode`] derives `PartialOrd`/`Ord` over variants declared in ascending risk order, so
//! `Paper < Demo < Live` is a property of the declaration rather than of a comment. Every consumer
//! that asks "is the tier the mount reached BELOW the account's own" leans on it
//! ([`crate::VenueArming::is_capped`], and the mount's interlock that refuses a bridge binding past
//! the account's tier).
//!
//! What `live` MEANS depends on the venue (decision 0095): for binance, bybit, okx and hyperliquid
//! it is MAINNET and `demo` is the demo network; elsewhere the venue's own mount picks among the
//! credentials it finds for the tier.

use std::fmt;

use serde::{Deserialize, Serialize};
use vike_model::VENUES;

/// One account's tier — `account.tier`'s vocabulary, and the tier a mount reached.
///
/// ⚠ The variants are declared in ASCENDING risk order and the `Ord` derive follows declaration
/// order, so `Paper < Demo < Live` is a property of this list rather than of a comment. Reordering
/// them silently inverts every comparison; there is deliberately no explicit discriminant to make
/// the ordering look like an arbitrary numbering that could be edited.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum VenueMode {
    /// Simulated fills against the local paper exchange. No credential is used, no order leaves
    /// the process. **The default**, because the safest answer is what an account no active row
    /// names must get.
    #[default]
    Paper,
    /// The venue's own demo/testnet/sandbox account — real wire, real rejections, no real funds.
    Demo,
    /// The venue's production account. Real money.
    Live,
}

impl VenueMode {
    /// Every mode, in ascending risk order — the order [`Ord`] agrees with, so a caller rendering a
    /// menu and a caller comparing two modes cannot disagree.
    pub const ALL: [VenueMode; 3] = [VenueMode::Paper, VenueMode::Demo, VenueMode::Live];

    /// The wire/row spelling, which is also what serde reads and writes (`rename_all` above) and
    /// what `account.tier`'s `CHECK` admits.
    ///
    /// Every message and every rendering goes through this rather than through a literal, so the
    /// legal set an error prints cannot drift from the set the parser accepts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VenueMode::Paper => "paper",
            VenueMode::Demo => "demo",
            VenueMode::Live => "live",
        }
    }
}

impl fmt::Display for VenueMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The legal modes, rendered for a refusal message — `paper / demo / live`.
///
/// Derived from [`VenueMode::ALL`] so the message and the parser cannot disagree about what is
/// legal; the same argument as `crates/vike-config/src/write.rs`'s `unknown_section_message`.
#[must_use]
pub fn legal_modes() -> String {
    VenueMode::ALL.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(" / ")
}

/// The canonical roster id equal to `name`, or `None` for a string
/// [`vike_model::VENUES`](VENUES) does not carry.
///
/// Returning the `&'static str` from the roster rather than a bool is what lets a caller key on
/// `&'static str` and be, by construction, incapable of holding a venue that does not exist.
#[must_use]
pub fn roster_id(name: &str) -> Option<&'static str> {
    VENUES.iter().copied().find(|v| *v == name)
}

#[path = "venue_mode_tests.rs"]
#[cfg(test)]
mod venue_mode_tests;
