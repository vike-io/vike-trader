//! The table of venues whose account leverage the platform can make real, the tiers each one is open
//! at, and the pure rules over it (decisions 0181 and 0182).
//!
//! ⚠ **One table, three readers, and a closed row is closed for all of them.** `vike-cli`'s
//! `account set-leverage` refuses a figure the table does not honour ([`leverage_refusal`]); the
//! tradehub's engine ignores a figure planted on a row the table does not honour; the gatewayhub's lane
//! makes no venue call for one. A `live` tier is therefore closed by DATA here, not by habit: opening
//! one is a one-line edit of [`LEVERAGE_VENUES`], per venue, after a demo drill on that venue.

use vike_model::AccountTier;

use super::AccountLeverage;

/// How a venue's stated account leverage is made real.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeverageMeans {
    /// The venue HOLDS a leverage the account set itself and the adapter sends none (hyperliquid,
    /// decision 0181): the venue's number is a CAP the stated figure may not exceed.
    Capped,
    /// The bridge's exec thread POSTS a leverage at its start (bybit, okx, binance, aster). The
    /// per-account figure rides that one existing write, and the engine applies it only when the
    /// venue is READ BACK holding exactly that number (decision 0182).
    Posted,
}

/// One row of [`LEVERAGE_VENUES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeverageVenue {
    /// A `vike_model::VENUES` id.
    pub venue: &'static str,
    /// The tiers an account's figure is honoured at. A row of any other tier is refused by
    /// `account set-leverage`, ignored by the engine and never read from the venue by the lane. EMPTY
    /// = the code is there and the row is closed.
    pub tiers: &'static [AccountTier],
    /// The venue takes the leverage as a whole number (binance and aster send `leverage as i64`), so
    /// a fractional figure is refused at the CLI and floored by [`AccountLeverage::candidate`].
    pub whole_only: bool,
    /// How the figure is made real.
    pub means: LeverageMeans,
}

impl LeverageVenue {
    /// Whether a stated figure on an account of `tier` is honoured at all.
    #[must_use]
    pub fn allows(&self, tier: AccountTier) -> bool {
        self.tiers.contains(&tier)
    }
}

/// The venues whose account leverage the platform can make real. Hyperliquid is decision 0181's and
/// stays EXACTLY as shipped: every tier (a `paper` row may state a figure before it is switched to
/// `demo`/`live`; the paper engine never reads it, only the shared tail of a `demo`/`live` start
/// does); bybit and okx are decision 0182's, **demo only**; binance is decision 0182's at `demo` and was
/// opened at `live` by the owner on 2026-10-19 (for its lane twin, `LANE_TWINS`); aster is coded and CLOSED
/// (only a `live` pair is stored for it, so no demo account can state a figure). deribit is never listed:
/// it has no leverage number ([`leverage_refusal`]).
pub const LEVERAGE_VENUES: &[LeverageVenue] = &[
    LeverageVenue {
        venue: "hyperliquid",
        tiers: &[AccountTier::Paper, AccountTier::Demo, AccountTier::Live],
        whole_only: false,
        means: LeverageMeans::Capped,
    },
    LeverageVenue {
        venue: "bybit",
        tiers: &[AccountTier::Demo],
        whole_only: true,
        means: LeverageMeans::Posted,
    },
    LeverageVenue {
        venue: "okx",
        tiers: &[AccountTier::Demo],
        whole_only: true,
        means: LeverageMeans::Posted,
    },
    // binance is open at `live` too (the owner, 2026-10-19: the lane twin `binance#PERP` has to do the
    // same on mainnet as on demo). bybit and okx stay demo-only: only the binance twin was ruled.
    LeverageVenue {
        venue: "binance",
        tiers: &[AccountTier::Demo, AccountTier::Live],
        whole_only: true,
        means: LeverageMeans::Posted,
    },
    LeverageVenue { venue: "aster", tiers: &[], whole_only: true, means: LeverageMeans::Posted },
];

/// The row for `venue`, whatever tiers it is open at.
#[must_use]
pub fn leverage_venue(venue: &str) -> Option<&'static LeverageVenue> {
    LEVERAGE_VENUES.iter().find(|row| row.venue == venue)
}

/// The row for `venue` when it honours an account of `tier`; `None` is "ignore the stated figure".
#[must_use]
pub fn leverage_venue_at(venue: &str, tier: AccountTier) -> Option<&'static LeverageVenue> {
    leverage_venue(venue).filter(|row| row.allows(tier))
}

/// What `account set-leverage` says for deribit, the one venue with a different answer.
const DERIBIT_HAS_NO_LEVERAGE: &str = "deribit has no leverage number to state: its margin is a \
     model (standard or portfolio margin) set for the whole account, not a figure per account or \
     per instrument";

/// **Why a stated `figure` on an account of `venue` at `tier` cannot be honoured**, in the operator's
/// words, or `None` when it can. PURE; the CLI refuses with it (clearing a figure is never refused).
#[must_use]
pub fn leverage_refusal(venue: &str, tier: AccountTier, figure: f64) -> Option<String> {
    if venue == "deribit" {
        return Some(DERIBIT_HAS_NO_LEVERAGE.to_string());
    }
    let Some(row) = leverage_venue(venue) else {
        let listed: Vec<&str> =
            LEVERAGE_VENUES.iter().filter(|r| !r.tiers.is_empty()).map(|r| r.venue).collect();
        return Some(format!(
            "{venue} is not a venue whose account leverage the platform can make real (only {} \
             are)",
            listed.join(", ")
        ));
    };
    if !row.allows(tier) {
        let open: Vec<&str> = row.tiers.iter().map(|t| t.as_str()).collect();
        let open = if open.is_empty() {
            format!("{venue} is not open at any tier yet")
        } else {
            format!("{venue} takes a stated leverage at tier {} only", open.join(" or "))
        };
        return Some(format!(
            "{open} in this release, and this account is `{}`: a live tier opens by one edit of \
             the table, after a demo drill on that venue",
            tier.as_str()
        ));
    }
    if row.whole_only && figure.fract() != 0.0 {
        return Some(format!(
            "{venue} takes the leverage as a whole number, so {figure} cannot be honoured: state \
             a whole number"
        ));
    }
    None
}

/// [`leverage_refusal`] for a ROW: the table's answer for `(venue, tier, figure)` plus the label rule. **At
/// `live` the owner opened the lane TWIN only** (2026-10-19): on a venue whose lane twin is open at `live` a
/// stated figure is honoured on the DEFAULT (unlabelled) row, where it starts the perpetual twin, and on no
/// labelled live row, whose figure the views drop (`Accounts::derive_lane_twins_ignoring`) and nothing asks
/// of the venue. `label` is the row's label text, `None` for the default account. PURE; the CLI refuses with it.
#[must_use]
pub fn row_leverage_refusal(
    venue: &str,
    tier: AccountTier,
    label: Option<&str>,
    figure: f64,
) -> Option<String> {
    if let Some(why) = leverage_refusal(venue, tier, figure) {
        return Some(why);
    }
    let twin = super::lane_twin(venue).filter(|t| t.tiers.contains(&AccountTier::Live))?;
    let label = label?;
    (tier == AccountTier::Live).then(|| {
        format!(
            "at `live` {venue} takes a stated leverage on the DEFAULT (unlabelled) account only, where it \
             starts the perpetual lane twin `{venue}#{}`; this account is labelled `{label}`, so a figure on \
             it would be ignored and nothing would be written to the venue",
            twin.label
        )
    })
}

impl AccountLeverage {
    /// **The ONE figure a venue that POSTS a whole number is asked to hold**: `floor(min(this
    /// stated figure, policy.max_leverage, the run profile's max_leverage))`, never below 1. The
    /// NODE computes it (the gatewayhub does not know `policy.max_leverage`), so it can only be
    /// LOWER than any of the three inputs, never higher, and it is integral.
    #[must_use]
    pub fn candidate(self, policy_max: Option<f64>, profile_max: Option<f64>) -> f64 {
        // `f64::min` ignores a NaN operand, so a nonsense ceiling can never loosen the figure.
        let lowest = [policy_max, profile_max].into_iter().flatten().fold(self.get(), f64::min);
        lowest.floor().max(1.0)
    }
}

#[path = "leverage_venues_tests.rs"]
#[cfg(test)]
mod leverage_venues_tests;
