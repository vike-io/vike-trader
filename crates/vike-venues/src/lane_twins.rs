//! The lane-twin TABLE: a venue whose ONE account row trades TWO exec lanes (binance: spot, and USDⓈ-M
//! futures told apart by a perpetual's `.P` suffix or a dated contract's `_YYMMDD`, decision 0290) runs a SECOND
//! ENGINE for the lane its default engine does not trade, derived from the SAME row and the SAME keys. No second
//! `account` row, no second credential.
//!
//! This is the per-venue table only. The ROW it derives is the credential store's: `vike-secrets`'
//! `AccountRows::derive_lane_twins` (`crates/vike-secrets/src/db/accounts/lane_twins.rs`, whose module doc is the
//! derivation's argument) reads this table. The table moved here from that file on 2026-11-22 (the owner's
//! "do all 10", decision 0162's amendment of that day), so a reader that only asks "which venue has a twin"
//! links no store.
//!
//! # Why a second engine and not a second lane in one
//!
//! An engine has ONE ledger (`AccountRow::apply_account_state` sets its single `balance` from whichever
//! lane spoke last), ONE user-data stream per lane and ONE reconcile leg, and Binance keeps spot and
//! futures in two wallets. Two lanes in one engine would overwrite one wallet's cash with the other's.
//! Two engines are two books exactly as the venue keeps them.
//!
//! # Closed or open by DATA, like `vike_venues::LEVERAGE_VENUES`
//!
//! A tier a row does not list is closed: a twin starts a second signing engine on the same keys, and
//! on mainnet that is a decision with a risk budget, an exposure ceiling and a live lock per engine.
//! binance's `live` tier was opened by the owner (2026-10-19) by the one-line edit of [`LANE_TWINS`]
//! this paragraph used to promise; bybit, okx and every other venue have no row at all.

use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, label_of_route_key, route_key_of};

use crate::PERP_SUFFIX;

/// One row of [`LANE_TWINS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountRowLaneTwin {
    /// A `vike_model::VENUES` id.
    pub venue: &'static str,
    /// The twin's account label: the route key of its engine is `venue#label`. A valid
    /// `vike_model::accounts::account_keys::AccountLabel`, and RESERVED on this venue for the twin: a real
    /// row carrying it makes the twin stand down (`vike_secrets::AccountRows::with_lane_twins`).
    pub label: &'static str,
    /// **The suffix of the lane's perpetuals** (binance: `.P`, USD-M perpetuals): the symbol its parent states
    /// for it (`vike_secrets::AccountFact::LaneTwinSymbol`) ends in it, or is a dated contract's user name
    /// ([`crate::is_dated`]: decision 0290's `BTCUSDT_261225` carries no `.P` and trades on the same futures
    /// lane). The table names a lane and never a symbol.
    pub lane: &'static str,
    /// The tiers a twin is derived at. EMPTY = the code is there and the row is closed.
    pub tiers: &'static [AccountTier],
    /// What the GUI's venue · account picker calls the twin's row, starting with [`Self::label`] (the row's
    /// account stays the label: this is only what the menu shows). The twin is derived in memory, so no
    /// directory row names it, and this is where the trader learns that `PERP` is the perpetual lane of the
    /// account above it.
    pub picker_name: &'static str,
}

impl AccountRowLaneTwin {
    /// The route key of the twin's engine: `route_key_of`, the ONE spelling.
    #[must_use]
    pub fn route_key(&self) -> String {
        // `label` is a valid label (the table test pins it), so this is the `Named` `parse` would give.
        route_key_of(self.venue, &AccountLabel::Named(self.label.to_string()))
    }

    /// Whether `symbol` sits on the twin's lane ([`Self::lane`]): a perpetual's name, or a dated contract's.
    #[must_use]
    pub fn on_lane(&self, symbol: &str) -> bool {
        (symbol.len() > self.lane.len() && symbol.ends_with(self.lane)) || crate::is_dated(symbol)
    }

    /// **Whether an engine published under `key`, mounted on `symbol`, has this twin's SHAPE**: the table's label, a
    /// mount symbol on the twin's lane ([`Self::on_lane`]), at a tier the table opens. A block that does not say its
    /// tier is not judged to be one. It is all a frame can tell. The node's door asks more before it moves an order
    /// (`vike-tradehub`'s `is_lane_twin_engine`: at `live`, the account watcher's word that it derived this engine),
    /// and a real account can have the shape.
    #[must_use]
    pub fn is_shaped(&self, key: &str, symbol: &str, tier: Option<AccountTier>) -> bool {
        label_of_route_key(self.venue, key).as_ref().and_then(AccountLabel::text)
            == Some(self.label)
            && self.on_lane(symbol)
            && tier.is_some_and(|tier| self.tiers.contains(&tier))
    }
}

/// The venues whose default engine trades one lane and whose account row may also run the other.
///
/// binance is open at `demo` AND `live` (the owner opened `live` on 2026-10-19: "live has to do the
/// same as demo"). `live` is MAINNET, so a live twin is a second real-money engine on the same keys:
/// it exists only for a live default row that STATES a leverage (the owner states it, nothing here
/// ever does), and every ceiling of its parent applies to it unchanged.
pub const LANE_TWINS: &[AccountRowLaneTwin] = &[AccountRowLaneTwin {
    venue: "binance",
    label: "PERP",
    lane: PERP_SUFFIX,
    tiers: &[AccountTier::Demo, AccountTier::Live],
    picker_name: "PERP (perp lane)",
}];

/// The row for `venue`, whatever tiers it is open at.
#[must_use]
pub fn lane_twin(venue: &str) -> Option<&'static AccountRowLaneTwin> {
    LANE_TWINS.iter().find(|row| row.venue == venue)
}

/// Whether `label` is the label of `venue`'s lane twin, by the table alone (no row is consulted).
#[must_use]
pub fn is_lane_twin_label(venue: &str, label: &str) -> bool {
    lane_twin(venue).is_some_and(|row| row.label == label)
}

#[path = "lane_twins_tests.rs"]
#[cfg(test)]
mod lane_twins_tests;
