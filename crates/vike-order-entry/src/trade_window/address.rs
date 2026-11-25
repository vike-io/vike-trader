//! Where a Trade window's intent goes: the window's own venue, account and symbol (spec §4.2).

use vike_model::accounts::account_keys::AccountLabel;

/// Where a Trade window's intent goes: the window's own venue, account and symbol (spec §4.2).
/// It is also the address a SENT order went to (vike-app-core's `orders::node_refusals`), which a
/// Trade window matches its own address against.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeWindowAddress {
    pub venue: String,
    /// `None` is the venue's default account. `Some(AccountLabel::Default)` is read as the same
    /// account everywhere in this module.
    pub account: Option<AccountLabel>,
    /// The venue-native symbol.
    pub symbol: String,
}

impl TradeWindowAddress {
    /// The address a window names with an account TEXT (`None` for the default account), or `None`
    /// when that text is not an account label.
    ///
    /// ⚠ **Fallible on purpose.** An unparseable label must stop the window from trading, never
    /// become `account: None`: `None` is the DEFAULT account, so an `.ok()` at the call site would
    /// turn a typo'd or corrupted label into an order on a different book. The text is read in the
    /// wire's spelling (`vike_model::accounts::account_keys::parse_wire_account`), so `DEFAULT` names the
    /// default account and is stored as `None`, the one spelling of it. Only the label is checked:
    /// an empty venue or symbol is the dispatcher's `NoRoutableMarket`.
    pub fn parse(venue: &str, account: Option<&str>, symbol: &str) -> Option<Self> {
        let account = match account {
            None => None,
            Some(text) => {
                let label = vike_model::accounts::account_keys::parse_wire_account(text).ok()?;
                (!label.is_default()).then_some(label)
            }
        };
        Some(TradeWindowAddress { venue: venue.to_string(), account, symbol: symbol.to_string() })
    }

    /// The engine this address routes to — `vike_model::accounts::account_keys::route_key_of`'s answer.
    pub fn route_key(&self) -> String {
        match &self.account {
            Some(l) => vike_model::accounts::account_keys::route_key_of(&self.venue, l),
            None => self.venue.clone(),
        }
    }
}
