//! **`TierScope` — over WHICH `account` rows a venue's tier is judged** (decision 0140, item 1).
//!
//! An account trades at its own tier: the tier of its ACTIVE `account` rows
//! (`docs/decisions/0119-an-account-trades-at-its-own-tier.md`), read by the mount
//! (`crates/vike-tradehub-core/src/arming.rs`'s `account_tier`). For most venues "its own" means the rows
//! of ONE `(venue, label)`: two labelled accounts may sit at two tiers and both trade, each on its
//! own credentials and its own session ([`TierScope::Account`]).
//!
//! One venue cannot: **IBKR rides ONE session per box** — one product, one tier — because one IBKR
//! account admits one session and the two products evict each other
//! (`docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`). Its tier is therefore
//! judged over the rows of EVERY label ([`TierScope::Venue`]): an unlabelled ibkr row at `demo` and
//! a labelled one at `live` are the same `TierConflict` two rows of ONE label already were, and
//! every ibkr account mounts paper until one is deactivated. The label-blind read is
//! `vike_secrets::Accounts::active_tier_of_venue`; the session the venue-wide tier resolves to is
//! `crates/vike-venues/src/ibkr_session.rs`'s `ibkr_session`.
//!
//! The table follows `crates/vike-venues/CLAUDE.md`'s "Per-venue capability maps (the playbook)":
//! one NAMED row per roster venue, a verbatim matrix pin, a completeness test over
//! [`vike_model::VENUES`]. An unknown venue string is [`TierScope::Account`], 0119's rule.

/// Over which `account` rows a venue's tier is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TierScope {
    /// The rows of ONE `(venue, label)` (decision 0119): each account at its own tier.
    Account,
    /// The rows of EVERY label of the venue: one tier for the whole box (decision 0140). Two
    /// active non-paper tiers anywhere among them hold EVERY account of the venue at paper.
    Venue,
}

/// **The tier scope of `venue`.** Every roster venue is a NAMED row; an unknown string is
/// [`TierScope::Account`].
#[must_use]
pub fn venue_tier_scope(venue: &str) -> TierScope {
    named_row(venue).unwrap_or(TierScope::Account)
}

/// The NAMED rows; `None` is a venue this table has never heard of. Separate from
/// [`venue_tier_scope`] because a named `Account` row and the fallback answer the same word, and
/// only this split lets the completeness test tell "classified" from "forgotten".
fn named_row(venue: &str) -> Option<TierScope> {
    Some(match venue {
        // Each account of these venues is its own venue session (its own key set, its own
        // connection), so two labels at two tiers are two sessions that do not touch.
        "binance" | "bybit" | "okx" | "deribit" | "oanda" | "ig" | "fxcm" | "dukascopy"
        | "polymarket" | "ctrader" | "alpaca" | "aster" | "hyperliquid" => TierScope::Account,
        // ONE IB Gateway or Client Portal session per account, and the two products evict each
        // other (`docs/ops/ibkr-socket-gateway.md` §5): the box gets one tier for every label.
        "ibkr" => TierScope::Venue,
        // vike:new-venue:row // TODO(new-venue: {venue}): does one account of this venue hold a session that another
        // vike:new-venue:row // account at another tier would evict? No ⇒ keep `Account`; yes ⇒ `Venue`, with the reason.
        // vike:new-venue:row "{venue}" => TierScope::Account,
        _ => return None,
    })
}

#[path = "venue_tier_scope_tests.rs"]
#[cfg(test)]
mod venue_tier_scope_tests;
