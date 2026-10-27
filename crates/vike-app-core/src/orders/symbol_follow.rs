//! Which accounts take an order on a symbol their engine was not started on: the Trade window
//! follows the symbol the operator PICKED (the chart's rule), not the one the account is mounted on.
//!
//! **The NODE says which accounts do, this module only reads it.** An engine whose adapter signs
//! each order on the order's own symbol and which adds that symbol on its first order publishes
//! `VenueBlock::admits_symbols` (`WireVenueBlock::admits_symbols` on the wire), derived on the node
//! from the same table its control door admits by
//! (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `SYMBOL_RULES`). A block without it,
//! from a venue the node does not open or from a node that predates the field, is one symbol only,
//! and the window says why ([`OneSymbol::why`]): an older node that is sent a foreign symbol Acks
//! and signs it on the MOUNTED symbol, so the desktop must not send one.
//!
//! * **Any symbol of the venue's catalog**: a DEMO or LIVE account whose block says
//!   `admits_symbols` (binance, bybit, aster, okx and hyperliquid on a current node). (The window checks the catalog;
//!   this module knows nothing of it.)
//! * **One symbol**: a PAPER account (its paper exchange is one book, and no engine adds a symbol to
//!   it); a block that names no tier (an older node: never wider than the old ticket); any block
//!   that does not say `admits_symbols`: deribit (refused for now, its adapter is to be rewritten
//!   to sign each order on its own instrument), okx (the node does not add a
//!   second symbol to its engine yet), every venue whose prices the data server does not serve
//!   yet, and the venues above on a node that predates the capability.
//!
//! ⚠ The venue names below only WORD the reason of a block that does not offer the capability; none
//! of them grants it. For an account the node does not open it REFUSES an order on a symbol its
//! engine does not trade, and says so in words of its own. The window sends the order anyway and
//! shows that text on its strip (`crate::orders::node_refusals`): it never hides a refusal by
//! guessing the node's state.

use vike_exec::VenueBlock;
use vike_model::AccountTier;

/// Why an account keeps to the one symbol its engine was started on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneSymbol {
    /// A paper account: its exchange is one book.
    Paper,
    /// The node does not say the account's tier, so the window stays where the old ticket was.
    TierUnsaid,
    /// Deribit: refused for now.
    Deribit,
    /// OKX: its adapter cannot take a second symbol on demand yet.
    EnginePending,
    /// The venue's prices are not served by the data server yet.
    PricesNotServed,
    /// binance, bybit or aster on a node that does not publish the capability (a node older than
    /// the field, which would sign a foreign symbol on the mounted one).
    NodeDoesNotOffer,
}

impl OneSymbol {
    /// The sentence the window says, after what the account does trade.
    pub const fn why(self) -> &'static str {
        match self {
            OneSymbol::Paper => "A paper account trades only the symbol it was started on.",
            OneSymbol::TierUnsaid => {
                "This node does not say what kind of account this is, so the window trades only \
                 the symbol it was started on."
            }
            OneSymbol::Deribit => {
                "Deribit takes orders on one instrument per account for now: its adapter does not \
                 yet sign each order on its own instrument."
            }
            OneSymbol::EnginePending => {
                "This exchange's account takes orders on the symbol it was started on only for \
                 now: the node does not yet add a second symbol to its engine."
            }
            OneSymbol::PricesNotServed => {
                "This venue's prices are not served yet, so its account trades only the symbol it \
                 was started on."
            }
            OneSymbol::NodeDoesNotOffer => {
                "This node does not offer to trade another symbol on this account (a node older \
                 than that feature signs it on the symbol the account was started on), so the \
                 window trades only the symbol it was started on. Update the node."
            }
        }
    }
}

/// Why the engine behind `vb` trades only the symbol it was started on, or `None` when the node
/// says it takes any symbol of its venue's catalog.
pub fn one_symbol(vb: &VenueBlock) -> Option<OneSymbol> {
    match vb.tier {
        Some(AccountTier::Demo | AccountTier::Live) => {}
        Some(AccountTier::Paper) => return Some(OneSymbol::Paper),
        None => return Some(OneSymbol::TierUnsaid),
    }
    if vb.admits_symbols {
        return None;
    }
    Some(match vb.venue.as_str() {
        "binance" | "bybit" | "aster" | "hyperliquid" => OneSymbol::NodeDoesNotOffer,
        "okx" => OneSymbol::EnginePending,
        "deribit" => OneSymbol::Deribit,
        _ => OneSymbol::PricesNotServed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block as a CURRENT node publishes it: the capability for the venues it opens.
    fn block(venue: &str, tier: Option<AccountTier>) -> VenueBlock {
        let offers = matches!(venue, "binance" | "bybit" | "aster" | "hyperliquid");
        VenueBlock { venue: venue.into(), tier, admits_symbols: offers, ..Default::default() }
    }

    #[test]
    fn a_demo_or_live_account_the_node_opens_takes_any_symbol() {
        for venue in ["binance", "bybit", "aster", "hyperliquid"] {
            for tier in [AccountTier::Demo, AccountTier::Live] {
                assert_eq!(one_symbol(&block(venue, Some(tier))), None, "{venue} {tier:?}");
            }
        }
    }

    /// The capability is the NODE's: a block that says it grants the window whatever the venue, and
    /// one that does not takes it from the window even for a venue the table once named (an older
    /// node, which would sign the foreign symbol on the mounted one behind a plain Ack).
    #[test]
    fn the_nodes_capability_decides_not_the_venue_name() {
        let live = Some(AccountTier::Live);
        for venue in ["binance", "bybit", "aster", "hyperliquid"] {
            let older = VenueBlock { admits_symbols: false, ..block(venue, live) };
            assert_eq!(one_symbol(&older), Some(OneSymbol::NodeDoesNotOffer), "{venue}");
        }
        let opened = VenueBlock { admits_symbols: true, ..block("okx", live) };
        assert_eq!(one_symbol(&opened), None, "a venue the node opens later needs no GUI release");
        // The tier still comes first: the capability never lifts a paper or tier-less block.
        let paper =
            VenueBlock { admits_symbols: true, ..block("binance", Some(AccountTier::Paper)) };
        assert_eq!(one_symbol(&paper), Some(OneSymbol::Paper));
        let unsaid = VenueBlock { admits_symbols: true, ..block("binance", None) };
        assert_eq!(one_symbol(&unsaid), Some(OneSymbol::TierUnsaid));
    }

    #[test]
    fn every_other_account_keeps_one_symbol_and_says_why() {
        let live = Some(AccountTier::Live);
        assert_eq!(one_symbol(&block("binance", Some(AccountTier::Paper))), Some(OneSymbol::Paper));
        assert_eq!(one_symbol(&block("binance", None)), Some(OneSymbol::TierUnsaid));
        assert_eq!(one_symbol(&block("deribit", live)), Some(OneSymbol::Deribit));
        assert_eq!(one_symbol(&block("okx", live)), Some(OneSymbol::EnginePending));
        for venue in ["oanda", "ibkr", "alpaca", "polymarket", "no-such-venue"] {
            assert_eq!(
                one_symbol(&block(venue, live)),
                Some(OneSymbol::PricesNotServed),
                "{venue}"
            );
        }
        for why in [
            OneSymbol::Paper,
            OneSymbol::TierUnsaid,
            OneSymbol::Deribit,
            OneSymbol::EnginePending,
            OneSymbol::PricesNotServed,
            OneSymbol::NodeDoesNotOffer,
        ] {
            assert!(!why.why().is_empty());
        }
    }
}
