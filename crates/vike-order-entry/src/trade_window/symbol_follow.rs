//! Which accounts take an order on a symbol their engine was not started on: the Trade window
//! follows the symbol the operator PICKED (the chart's rule), not the one the account is mounted on.
//!
//! **The NODE says which accounts do, this module only reads it.** An engine whose adapter signs
//! each order on the order's own symbol and which adds that symbol on its first order publishes
//! `VenueBlock::admits_symbols` (`VenueBlockWire::admits_symbols` on the wire), derived on the node
//! from the same table its control door admits by
//! (`crates/vike-catalog/src/venue_symbol.rs`'s `SYMBOL_RULES`). A block without it,
//! from a venue the node does not open or from a node that predates the field, is one symbol only,
//! and the window says why ([`TradeWindowOneSymbolReason::why`]): an older node that is sent a foreign symbol Acks
//! and signs it on the MOUNTED symbol, so the desktop must not send one.
//!
//! * **Any symbol of the venue's catalog**: a DEMO or LIVE account whose block says
//!   `admits_symbols` (every venue `SYMBOL_RULES` admits symbols for, on a current node). (The window checks
//!   the catalog; this module knows nothing of it.)
//! * **One symbol**: a PAPER account (its paper exchange is one book, and no engine adds a symbol to
//!   it); a block that names no tier (an older node: never wider than the old ticket); any block
//!   that does not say `admits_symbols`: on a venue `SYMBOL_RULES` admits symbols for, that is a node
//!   older than the capability ([`TradeWindowOneSymbolReason::NodeDoesNotOffer`]); on any other row, or a
//!   venue the table does not know, the node does not add a second symbol to its engine
//!   ([`TradeWindowOneSymbolReason::EnginePending`]).
//!
//! ⚠ The door's table only WORDS the reason of a block that does not offer the capability; it never
//! grants it. For an account the node does not open it REFUSES an order on a symbol its
//! engine does not trade, and says so in words of its own. The window sends the order anyway and
//! shows that text on its strip (`vike_app_core::orders::node_refusals`): it never hides a refusal by
//! guessing the node's state.

use vike_catalog::{SYMBOL_RULES, VenueSymbolRule};
use vike_execution::VenueBlock;
use vike_model::AccountTier;

/// Why an account keeps to the one symbol its engine was started on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TradeWindowOneSymbolReason {
    /// A paper account: its exchange is one book.
    Paper,
    /// The node does not say the account's tier, so the window stays where the old ticket was.
    TierUnsaid,
    /// A venue the door's table does not admit symbols for (or does not know): its engine takes no
    /// second symbol on demand.
    EnginePending,
    /// A venue the door's table admits symbols for, on a node that does not publish the capability (a
    /// node older than the field, which would sign a foreign symbol on the mounted one).
    NodeDoesNotOffer,
}

impl TradeWindowOneSymbolReason {
    /// The sentence the window says, after what the account does trade.
    pub const fn why(self) -> &'static str {
        match self {
            TradeWindowOneSymbolReason::Paper => {
                "A paper account trades only the symbol it was started on."
            }
            TradeWindowOneSymbolReason::TierUnsaid => {
                "This node does not say what kind of account this is, so the window trades only \
                 the symbol it was started on."
            }
            TradeWindowOneSymbolReason::EnginePending => {
                "This exchange's account takes orders on the symbol it was started on only for \
                 now: the node does not yet add a second symbol to its engine."
            }
            TradeWindowOneSymbolReason::NodeDoesNotOffer => {
                "This node does not offer to trade another symbol on this account (a node older \
                 than that feature signs it on the symbol the account was started on), so the \
                 window trades only the symbol it was started on. Update the node."
            }
        }
    }
}

/// Whether the door's table (`SYMBOL_RULES`) admits symbols for `venue`, i.e. a CURRENT node's
/// engine on it publishes `admits_symbols`.
fn door_admits(venue: &str) -> bool {
    SYMBOL_RULES.iter().any(|(v, rule)| *v == venue && matches!(rule, VenueSymbolRule::Admits(_)))
}

/// Why the engine behind `vb` trades only the symbol it was started on, or `None` when the node
/// says it takes any symbol of its venue's catalog.
pub fn one_symbol(vb: &VenueBlock) -> Option<TradeWindowOneSymbolReason> {
    match vb.tier {
        Some(AccountTier::Demo | AccountTier::Live) => {}
        Some(AccountTier::Paper) => return Some(TradeWindowOneSymbolReason::Paper),
        None => return Some(TradeWindowOneSymbolReason::TierUnsaid),
    }
    if vb.admits_symbols {
        return None;
    }
    Some(if door_admits(&vb.venue) {
        TradeWindowOneSymbolReason::NodeDoesNotOffer
    } else {
        TradeWindowOneSymbolReason::EnginePending
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block as a CURRENT node publishes it: the capability for every venue the door admits for.
    fn block(venue: &str, tier: Option<AccountTier>) -> VenueBlock {
        VenueBlock {
            venue: venue.into(),
            tier,
            admits_symbols: door_admits(venue),
            ..Default::default()
        }
    }

    #[test]
    fn a_demo_or_live_account_the_node_opens_takes_any_symbol() {
        let admitting: Vec<&str> =
            SYMBOL_RULES.iter().map(|(v, _)| *v).filter(|v| door_admits(v)).collect();
        assert!(!admitting.is_empty(), "the door admits symbols for no venue: {SYMBOL_RULES:?}");
        for venue in admitting {
            for tier in [AccountTier::Demo, AccountTier::Live] {
                assert_eq!(one_symbol(&block(venue, Some(tier))), None, "{venue} {tier:?}");
            }
        }
    }

    /// The capability is the NODE's: a block that says it grants the window whatever the venue, and
    /// one that does not takes it from the window even for a venue the door admits for (an older
    /// node, which would sign the foreign symbol on the mounted one behind a plain Ack).
    #[test]
    fn the_nodes_capability_decides_not_the_venue_name() {
        let live = Some(AccountTier::Live);
        for venue in SYMBOL_RULES.iter().map(|(v, _)| *v).filter(|v| door_admits(v)) {
            let older = VenueBlock { admits_symbols: false, ..block(venue, live) };
            assert_eq!(
                one_symbol(&older),
                Some(TradeWindowOneSymbolReason::NodeDoesNotOffer),
                "{venue}"
            );
        }
        let opened = VenueBlock { admits_symbols: true, ..block("no-such-venue", live) };
        assert_eq!(one_symbol(&opened), None, "a venue the node opens later needs no GUI release");
        // The tier still comes first: the capability never lifts a paper or tier-less block.
        let paper =
            VenueBlock { admits_symbols: true, ..block("binance", Some(AccountTier::Paper)) };
        assert_eq!(one_symbol(&paper), Some(TradeWindowOneSymbolReason::Paper));
        let unsaid = VenueBlock { admits_symbols: true, ..block("binance", None) };
        assert_eq!(one_symbol(&unsaid), Some(TradeWindowOneSymbolReason::TierUnsaid));
    }

    #[test]
    fn every_other_account_keeps_one_symbol_and_says_why() {
        let live = Some(AccountTier::Live);
        assert_eq!(
            one_symbol(&block("binance", Some(AccountTier::Paper))),
            Some(TradeWindowOneSymbolReason::Paper)
        );
        assert_eq!(
            one_symbol(&block("binance", None)),
            Some(TradeWindowOneSymbolReason::TierUnsaid)
        );
        // A venue the door's table does not know: its engine takes no second symbol.
        assert_eq!(
            one_symbol(&block("no-such-venue", live)),
            Some(TradeWindowOneSymbolReason::EnginePending)
        );
        for why in [
            TradeWindowOneSymbolReason::Paper,
            TradeWindowOneSymbolReason::TierUnsaid,
            TradeWindowOneSymbolReason::EnginePending,
            TradeWindowOneSymbolReason::NodeDoesNotOffer,
        ] {
            assert!(!why.why().is_empty());
        }
    }
}
